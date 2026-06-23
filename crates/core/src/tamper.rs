//! Camera tamper / defocus / scene-change detection — the optical-integrity
//! watchdog every commercial NVR ships (Axis "camera tampering", Bosch/Hanwha
//! tamper, Hikvision "video tampering"). It catches three ways a camera stops
//! doing its job that ordinary motion/object detection misses entirely:
//!
//!   - **Blackout** — the lens is covered, bagged, spray-painted, or the scene
//!     went fully dark/saturated (near-uniform, very low contrast).
//!   - **Defocus** — the lens was turned/smeared so the image lost its edges
//!     (high-frequency energy collapses well below the camera's own baseline).
//!   - **Scene change** — the camera was physically moved or redirected (a large,
//!     sustained, whole-frame change, distinct from an object crossing the view).
//!
//! All three run on a tiny grayscale thumbnail of the sampled frame, so the cost
//! is negligible and it piggybacks on the frames the pipeline already pulls. The
//! gate is stateful with hysteresis (must persist to fire, must recover to
//! clear) and learns each camera's own sharpness/scene baseline **only while
//! healthy**, so a tampered frame never poisons the reference.

use image::{imageops::FilterType, DynamicImage};

/// Thumbnail edge for the analysis (small = ~free, big enough to judge focus).
pub const THUMB: usize = 32;

/// What kind of tamper the gate detected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TamperKind {
    /// Lens covered / scene dark or saturated — near-uniform, no contrast.
    Blackout,
    /// Image lost its edges — defocused or smeared.
    Defocus,
    /// Camera physically moved / redirected — large sustained whole-frame change.
    SceneChange,
}

impl TamperKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TamperKind::Blackout => "blackout",
            TamperKind::Defocus => "defocus",
            TamperKind::SceneChange => "scene_change",
        }
    }
    /// Severity order for picking the dominant instantaneous kind.
    fn rank(self) -> u8 {
        match self {
            TamperKind::Blackout => 3,
            TamperKind::Defocus => 2,
            TamperKind::SceneChange => 1,
        }
    }
}

/// A state transition worth surfacing: tamper started (`entered`) or recovered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TamperEvent {
    pub kind: TamperKind,
    /// true = tamper just started; false = the camera just recovered.
    pub entered: bool,
}

/// Tunable thresholds. Defaults are conservative (favor missing a marginal
/// tamper over false alarms — a covered/defocused lens is unambiguous).
#[derive(Clone, Copy, Debug)]
pub struct TamperConfig {
    /// Consecutive abnormal frames before a tamper state is declared.
    pub enter_frames: u32,
    /// Consecutive healthy frames before the tamper state clears.
    pub clear_frames: u32,
    /// Mean luma (0..1) below which (with low contrast) it's a dark blackout.
    pub blackout_dark: f32,
    /// Mean luma above which (with low contrast) it's a saturated/whiteout cover.
    pub blackout_bright: f32,
    /// Luma variance (contrast) below which the frame is "uniform".
    pub uniform_var: f32,
    /// Defocus fires when sharpness drops below this fraction of the baseline.
    pub defocus_frac: f32,
    /// Mean abs thumbnail diff vs the learned reference for a scene change.
    pub scene_diff: f32,
    /// Frame-to-frame diff below which a *re-aimed* camera counts as "settled",
    /// letting a latched SceneChange re-baseline onto the new framing and clear.
    pub scene_stable_diff: f32,
    /// EMA weight for learning the sharpness/scene baseline (only while healthy).
    pub baseline_alpha: f32,
    /// Healthy frames to observe before defocus/scene-change logic activates
    /// (so a cold start doesn't fire before a baseline exists).
    pub warmup_frames: u32,
}

impl Default for TamperConfig {
    fn default() -> Self {
        Self {
            enter_frames: 4,
            clear_frames: 3,
            blackout_dark: 0.06,
            blackout_bright: 0.95,
            uniform_var: 0.0016,
            defocus_frac: 0.35,
            scene_diff: 0.20,
            scene_stable_diff: 0.05,
            baseline_alpha: 0.05,
            warmup_frames: 5,
        }
    }
}

/// Per-camera stateful tamper gate.
pub struct TamperGate {
    cfg: TamperConfig,
    sharp_baseline: Option<f32>,
    reference: Option<Vec<f32>>,
    /// Previous frame's thumbnail, for frame-to-frame "has it settled?" checks.
    last_thumb: Option<Vec<f32>>,
    healthy_seen: u32,
    abnormal_streak: u32,
    healthy_streak: u32,
    /// Consecutive "settled" frames while latched in SceneChange (re-aim recovery).
    scene_stable_streak: u32,
    /// The dominant kind observed across the current abnormal streak.
    pending_kind: Option<TamperKind>,
    /// The currently-declared tamper (None = healthy).
    active: Option<TamperKind>,
}

impl TamperGate {
    pub fn new(cfg: TamperConfig) -> Self {
        Self {
            cfg,
            sharp_baseline: None,
            reference: None,
            last_thumb: None,
            healthy_seen: 0,
            abnormal_streak: 0,
            healthy_streak: 0,
            scene_stable_streak: 0,
            pending_kind: None,
            active: None,
        }
    }

    /// The current declared tamper state, if any.
    pub fn state(&self) -> Option<TamperKind> {
        self.active
    }

    /// Feed the next frame's grayscale thumbnail (length `THUMB*THUMB`, values
    /// 0..1, row-major). Returns a [`TamperEvent`] only on a state transition.
    pub fn update(&mut self, thumb: &[f32]) -> Option<TamperEvent> {
        debug_assert_eq!(thumb.len(), THUMB * THUMB);
        let mean = thumb.iter().sum::<f32>() / thumb.len() as f32;
        let var = thumb.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / thumb.len() as f32;
        let sharp = laplacian_var(thumb);
        let diff = self
            .reference
            .as_ref()
            .map(|r| mean_abs_diff(thumb, r))
            .unwrap_or(0.0);

        // Classify this frame. Blackout is checked first (most severe and most
        // unambiguous); defocus/scene-change need an established baseline.
        let warm = self.healthy_seen >= self.cfg.warmup_frames;
        let blackout = var < self.cfg.uniform_var
            && (mean <= self.cfg.blackout_dark || mean >= self.cfg.blackout_bright);
        let defocus = warm
            && !blackout
            && self
                .sharp_baseline
                .is_some_and(|b| sharp < b * self.cfg.defocus_frac);
        let scene = warm && !blackout && diff > self.cfg.scene_diff;

        let kind = if blackout {
            Some(TamperKind::Blackout)
        } else if defocus {
            Some(TamperKind::Defocus)
        } else if scene {
            Some(TamperKind::SceneChange)
        } else {
            None
        };

        // Frame-to-frame delta vs the previous frame, for "has a re-aimed camera
        // settled?" detection (kept current every frame).
        let step = self
            .last_thumb
            .as_ref()
            .map(|lt| mean_abs_diff(thumb, lt))
            .unwrap_or(1.0);
        self.last_thumb = Some(thumb.to_vec());

        // SceneChange recovery: a physically re-aimed camera differs from the
        // (frozen) reference forever, so a latched SceneChange would never clear
        // or re-baseline. If we're latched in SceneChange and the view now looks
        // healthy (not blackout/defocus) and has STOPPED moving for `clear_frames`,
        // accept the new framing as the baseline and clear.
        if self.active == Some(TamperKind::SceneChange) && !blackout && !defocus {
            if step < self.cfg.scene_stable_diff {
                self.scene_stable_streak += 1;
            } else {
                self.scene_stable_streak = 0;
            }
            if self.scene_stable_streak >= self.cfg.clear_frames {
                self.reference = Some(thumb.to_vec());
                self.sharp_baseline = Some(sharp);
                self.active = None;
                self.abnormal_streak = 0;
                self.healthy_streak = 0;
                self.scene_stable_streak = 0;
                self.pending_kind = None;
                return Some(TamperEvent {
                    kind: TamperKind::SceneChange,
                    entered: false,
                });
            }
        } else {
            self.scene_stable_streak = 0;
        }

        if let Some(k) = kind {
            // Abnormal frame: grow the streak, remember the worst kind, reset the
            // healthy streak. Do NOT update baselines (a tampered frame must not
            // poison the reference the recovery check relies on).
            self.abnormal_streak += 1;
            self.healthy_streak = 0;
            self.pending_kind = Some(match self.pending_kind {
                Some(p) if p.rank() >= k.rank() => p,
                _ => k,
            });
        } else {
            // Healthy frame: learn the baseline/reference slowly, grow the
            // healthy streak, decay the abnormal streak.
            self.healthy_seen = self.healthy_seen.saturating_add(1);
            self.healthy_streak += 1;
            self.abnormal_streak = 0;
            let a = self.cfg.baseline_alpha;
            self.sharp_baseline = Some(match self.sharp_baseline {
                Some(b) => b * (1.0 - a) + sharp * a,
                None => sharp,
            });
            match &mut self.reference {
                Some(r) => {
                    for (rv, tv) in r.iter_mut().zip(thumb) {
                        *rv = *rv * (1.0 - a) + *tv * a;
                    }
                }
                None => self.reference = Some(thumb.to_vec()),
            }
        }

        // State machine with hysteresis.
        match self.active {
            None => {
                if self.abnormal_streak >= self.cfg.enter_frames {
                    let kind = self.pending_kind.unwrap_or(TamperKind::Blackout);
                    self.active = Some(kind);
                    self.abnormal_streak = 0;
                    self.pending_kind = None;
                    return Some(TamperEvent {
                        kind,
                        entered: true,
                    });
                }
            }
            Some(kind) => {
                if self.healthy_streak >= self.cfg.clear_frames {
                    self.active = None;
                    self.healthy_streak = 0;
                    return Some(TamperEvent {
                        kind,
                        entered: false,
                    });
                }
            }
        }
        None
    }
}

/// Downscale any frame to a `THUMB`×`THUMB` grayscale thumbnail in 0..1.
pub fn thumb_of(img: &DynamicImage) -> Vec<f32> {
    let g = img
        .resize_exact(THUMB as u32, THUMB as u32, FilterType::Triangle)
        .to_luma8();
    g.pixels().map(|p| p.0[0] as f32 / 255.0).collect()
}

/// Variance of the discrete Laplacian over the interior — a focus measure
/// (sharp images have lots of high-frequency energy, blurred ones very little).
fn laplacian_var(thumb: &[f32]) -> f32 {
    let n = THUMB;
    let at = |x: usize, y: usize| thumb[y * n + x];
    let mut vals = Vec::with_capacity((n - 2) * (n - 2));
    for y in 1..n - 1 {
        for x in 1..n - 1 {
            let l = 4.0 * at(x, y) - at(x - 1, y) - at(x + 1, y) - at(x, y - 1) - at(x, y + 1);
            vals.push(l);
        }
    }
    if vals.is_empty() {
        return 0.0;
    }
    let mean = vals.iter().sum::<f32>() / vals.len() as f32;
    vals.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / vals.len() as f32
}

fn mean_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: usize = THUMB;

    /// A sharp checkerboard thumbnail (lots of edges → high Laplacian variance).
    fn checkerboard() -> Vec<f32> {
        (0..N * N)
            .map(|i| {
                let (x, y) = (i % N, i / N);
                if (x + y) % 2 == 0 {
                    0.85
                } else {
                    0.15
                }
            })
            .collect()
    }
    /// A soft gradient (few edges → low Laplacian variance) = "blurred".
    fn blurred() -> Vec<f32> {
        (0..N * N)
            .map(|i| 0.3 + 0.4 * (i % N) as f32 / N as f32)
            .collect()
    }
    fn uniform(v: f32) -> Vec<f32> {
        vec![v; N * N]
    }
    /// A different sharp scene (checkerboard shifted by one) for scene-change.
    fn other_scene() -> Vec<f32> {
        (0..N * N)
            .map(|i| {
                let (x, y) = (i % N, i / N);
                if (x + y) % 2 == 1 {
                    0.9
                } else {
                    0.1
                }
            })
            .collect()
    }

    fn warm(gate: &mut TamperGate, frames: u32) {
        let cb = checkerboard();
        for _ in 0..frames {
            assert_eq!(gate.update(&cb), None);
        }
    }

    #[test]
    fn healthy_scene_never_fires() {
        let mut g = TamperGate::new(TamperConfig::default());
        for _ in 0..30 {
            assert_eq!(g.update(&checkerboard()), None);
        }
        assert_eq!(g.state(), None);
    }

    #[test]
    fn blackout_enters_after_hysteresis_and_clears_on_recovery() {
        let cfg = TamperConfig::default();
        let mut g = TamperGate::new(cfg);
        warm(&mut g, 8);
        let black = uniform(0.0);
        // Must persist enter_frames before it declares.
        for _ in 0..cfg.enter_frames - 1 {
            assert_eq!(g.update(&black), None, "fired too early");
        }
        let ev = g.update(&black).expect("blackout should fire");
        assert_eq!(
            ev,
            TamperEvent {
                kind: TamperKind::Blackout,
                entered: true
            }
        );
        assert_eq!(g.state(), Some(TamperKind::Blackout));
        // A bright/saturated cover is also a blackout.
        // Recover: healthy frames clear after clear_frames.
        for _ in 0..cfg.clear_frames - 1 {
            assert_eq!(g.update(&checkerboard()), None, "cleared too early");
        }
        let ev = g.update(&checkerboard()).expect("should clear");
        assert!(!ev.entered && ev.kind == TamperKind::Blackout);
        assert_eq!(g.state(), None);
    }

    #[test]
    fn whiteout_is_a_blackout() {
        let cfg = TamperConfig::default();
        let mut g = TamperGate::new(cfg);
        warm(&mut g, 8);
        let white = uniform(1.0);
        let mut fired = None;
        for _ in 0..cfg.enter_frames {
            fired = g.update(&white);
        }
        assert_eq!(fired.map(|e| e.kind), Some(TamperKind::Blackout));
    }

    #[test]
    fn defocus_fires_when_edges_collapse_vs_baseline() {
        let cfg = TamperConfig::default();
        let mut g = TamperGate::new(cfg);
        warm(&mut g, 10); // learn the sharp baseline
        let soft = blurred();
        let mut ev = None;
        for _ in 0..cfg.enter_frames {
            ev = g.update(&soft);
        }
        let ev = ev.expect("defocus should fire");
        assert_eq!(ev.kind, TamperKind::Defocus);
        assert!(ev.entered);
    }

    #[test]
    fn scene_change_fires_on_large_sustained_global_change() {
        let cfg = TamperConfig::default();
        let mut g = TamperGate::new(cfg);
        warm(&mut g, 10); // reference learns the checkerboard
        let other = other_scene(); // inverted: every pixel flips → huge diff
        let mut ev = None;
        for _ in 0..cfg.enter_frames {
            ev = g.update(&other);
        }
        let ev = ev.expect("scene change should fire");
        assert_eq!(ev.kind, TamperKind::SceneChange);
    }

    #[test]
    fn scene_change_recovers_after_a_re_aimed_camera_settles() {
        let cfg = TamperConfig::default();
        let mut g = TamperGate::new(cfg);
        warm(&mut g, 10);
        let other = other_scene();
        // Drive the latch.
        let mut ev = None;
        for _ in 0..cfg.enter_frames {
            ev = g.update(&other);
        }
        assert_eq!(ev.map(|e| e.kind), Some(TamperKind::SceneChange));
        assert_eq!(g.state(), Some(TamperKind::SceneChange));
        // The new framing is now stable (identical frames): after clear_frames of
        // a tiny frame-to-frame delta, the gate re-baselines and clears.
        let mut cleared = None;
        for _ in 0..cfg.clear_frames + 1 {
            if let Some(e) = g.update(&other) {
                cleared = Some(e);
            }
        }
        let cleared = cleared.expect("should emit a cleared event after settling");
        assert!(!cleared.entered && cleared.kind == TamperKind::SceneChange);
        assert_eq!(g.state(), None, "must re-baseline onto the new framing");
        // And a subsequent move to yet another scene can fire again.
        let mut ev2 = None;
        for _ in 0..cfg.enter_frames {
            ev2 = g.update(&checkerboard());
        }
        assert_eq!(ev2.map(|e| e.kind), Some(TamperKind::SceneChange));
    }

    #[test]
    fn transient_change_below_hysteresis_does_not_fire() {
        let cfg = TamperConfig::default();
        let mut g = TamperGate::new(cfg);
        warm(&mut g, 10);
        // A couple of black frames (an object briefly filling the view), then back.
        for _ in 0..cfg.enter_frames - 1 {
            assert_eq!(g.update(&uniform(0.0)), None);
        }
        assert_eq!(g.update(&checkerboard()), None);
        assert_eq!(g.state(), None, "a transient blip must not declare tamper");
    }

    #[test]
    fn thumb_of_produces_right_size_and_range() {
        let img = DynamicImage::new_rgb8(80, 60);
        let t = thumb_of(&img);
        assert_eq!(t.len(), THUMB * THUMB);
        assert!(t.iter().all(|v| (0.0..=1.0).contains(v)));
    }

    #[test]
    fn laplacian_var_higher_for_sharp_than_blurred() {
        assert!(laplacian_var(&checkerboard()) > laplacian_var(&blurred()) * 10.0);
        assert_eq!(laplacian_var(&uniform(0.5)), 0.0);
    }
}
