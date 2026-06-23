//! Residential / family-safety analytics — the consumer-camera tier (baby, pet,
//! pool, kid, aging-in-place). These are cross-track or temporal rules that
//! neither the per-frame detector nor the commercial [`crate::analytics`] engine
//! can express: "a person/pet entered this zone", "a small person is here with
//! no adult", "someone went down and isn't getting up", "a swimmer has gone
//! motionless in the water". Pure logic over confirmed tracks + per-camera
//! config, driven once per sampled frame beside [`crate::analytics::AnalyticsState::tick`].
//!
//! ## SAFETY / LIABILITY (read `docs/05-residential-analytics-suite.md`)
//! Every output here is an **assistive hint, never a guarantee**. The child/adult
//! split is a fragile bbox-height heuristic that needs per-camera calibration;
//! fall and motionless-in-water are best-effort at ~1 fps sampling and miss
//! occluded / soft / slow events. The UI must disclaim these and they must never
//! be presented as a medical device, drowning detection, or SIDS prevention.

use std::collections::{HashMap, HashSet};

use tracker::Track;

use crate::db::PolyZone;

/// Anchor displacement (frame fractions) between ticks below which a track counts
/// as "motionless" — the shared basis for the fall and still-in-water timers.
const MOTION_EPS: f32 = 0.03;
/// A track whose ground-contact anchor sits at/below this frame fraction is "on
/// the floor band" — the region a fallen person occupies (lower part of frame).
const FALL_LOWER_BAND: f32 = 0.6;
/// A person seen above `FALL_LOWER_BAND - UPRIGHT_MARGIN` is "upright" — recorded
/// so a fall requires a prior standing observation (cuts furniture false-positives).
const UPRIGHT_MARGIN: f32 = 0.05;
/// Seconds a person must lie motionless in the floor band before a `fall` fires.
/// Conservative on purpose; assistive only. Tunable later per camera.
const FALL_STILL_SECS: i64 = 6;
/// Seconds a person must be motionless inside a water zone before an EXPERIMENTAL
/// `still_water` hint fires. NOT drowning detection — see module docs.
const WATER_STILL_SECS: i64 = 8;

/// What a residential rule detected this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResKind {
    /// A (label-scoped) track entered a zone flagged `alert_enter`.
    Enter,
    /// A child-classified person entered a zone flagged `child_watch`.
    ChildInZone,
    /// A child is present in a `supervise` zone with no adult present.
    ChildAlone,
    /// A person went motionless in the lower frame band (assistive fall hint).
    Fall,
    /// A person went motionless inside a `water` zone (EXPERIMENTAL hint).
    StillWater,
}

/// A residential event produced this tick. `label` is the event label the
/// pipeline stores/alerts on (the object class for [`ResKind::Enter`], otherwise
/// a fixed residential label); `zone` ties it to the zone that fired it.
#[derive(Clone, Debug, PartialEq)]
pub struct ResEvent {
    pub kind: ResKind,
    pub label: String,
    pub zone: Option<String>,
    pub track_id: u64,
    pub anchor: (f32, f32),
}

/// Per-track motion / fall / water-stillness memory across frames.
#[derive(Clone, Copy, Debug)]
struct Motion {
    last_ts: i64,
    last_anchor: (f32, f32),
    /// Contiguous motionless seconds (reset whenever the anchor moves).
    still_secs: i64,
    /// Contiguous motionless seconds while inside a water zone.
    water_still_secs: i64,
    /// Has this person ever been observed upright? Gates fall (a fall needs a
    /// stand-then-down transition, not just "already on the ground").
    saw_upright: bool,
    fall_fired: bool,
    water_fired: bool,
    /// False until the first observation, so the first tick's `dt` is 0.
    seeded: bool,
}

/// Per-camera residential analytics memory across frames.
#[derive(Default)]
pub struct ResidentialState {
    /// (track_id, zone index) -> was the track inside last tick. Drives the
    /// rising-edge `Enter` / `ChildInZone` events. Index-keyed; reset on layout change.
    inside: HashMap<(u64, usize), bool>,
    /// supervise-zone index -> was a child alone in it last tick (edge latch, so
    /// `ChildAlone` fires once per breach and re-arms when an adult appears).
    alone_over: HashMap<usize, bool>,
    /// track_id -> motion/fall/water memory (keyed by identity, survives layout changes).
    motion: HashMap<u64, Motion>,
    /// Zone-name fingerprint the index-keyed maps were built against.
    shape: Vec<String>,
}

/// Is this person track classified as a child? Heuristic only: its normalized
/// bbox height is at/below the per-camera `child_height_frac`. Returns false when
/// the camera has no calibration (`None`) — child features are then off, by design.
fn is_child(t: &Track, child_height_frac: Option<f32>) -> bool {
    if t.label != "person" {
        return false;
    }
    let h = t.bbox.h();
    child_height_frac.is_some_and(|f| h > 0.0 && h <= f)
}

fn is_adult(t: &Track, child_height_frac: Option<f32>) -> bool {
    t.label == "person" && child_height_frac.is_some_and(|f| t.bbox.h() > f)
}

fn zone_centroid(z: &PolyZone) -> (f32, f32) {
    if z.points.is_empty() {
        return (0.5, 0.5);
    }
    let n = z.points.len() as f32;
    let (sx, sy) = z
        .points
        .iter()
        .fold((0.0f32, 0.0f32), |(ax, ay), p| (ax + p[0], ay + p[1]));
    (sx / n, sy / n)
}

impl ResidentialState {
    /// Advance residential analytics for one frame. `tracks` is the camera's
    /// confirmed tracks; `child_height_frac` calibrates the child/adult split
    /// (`None` = child features off); `fall_detect` arms the assistive fall hint;
    /// `now` is the frame timestamp (unix secs). Returns this frame's events.
    pub fn tick(
        &mut self,
        tracks: &[&Track],
        zones: &[PolyZone],
        child_height_frac: Option<f32>,
        fall_detect: bool,
        now: i64,
    ) -> Vec<ResEvent> {
        let mut out = Vec::new();

        // Reset index-keyed state if the zone layout changed (an index now points
        // at a different zone) — same hazard the commercial engine guards against.
        let shape: Vec<String> = zones.iter().map(|z| z.name.clone()).collect();
        if self.shape != shape {
            self.inside.clear();
            self.alone_over.clear();
            self.shape = shape;
        }

        let live: HashSet<u64> = tracks.iter().map(|t| t.id).collect();

        // Per supervise-zone child/adult tallies + a child's anchor for the marker.
        let mut child_cnt = vec![0u32; zones.len()];
        let mut adult_cnt = vec![0u32; zones.len()];
        let mut child_anchor: Vec<Option<(f32, f32)>> = vec![None; zones.len()];

        for t in tracks {
            let a = t.anchor();
            let person = t.label == "person";
            let child = is_child(t, child_height_frac);
            let adult = is_adult(t, child_height_frac);
            let in_water = zones
                .iter()
                .any(|z| z.water && z.applies_to(&t.label) && z.contains(a.0, a.1));

            // --- motion / fall / still-in-water (per track) -------------------
            let m = self.motion.entry(t.id).or_insert(Motion {
                last_ts: now,
                last_anchor: a,
                still_secs: 0,
                water_still_secs: 0,
                saw_upright: false,
                fall_fired: false,
                water_fired: false,
                seeded: false,
            });
            let dt = if m.seeded {
                (now - m.last_ts).max(0)
            } else {
                0
            };
            let disp = ((a.0 - m.last_anchor.0).powi(2) + (a.1 - m.last_anchor.1).powi(2)).sqrt();
            let moving = disp > MOTION_EPS;
            m.still_secs = if moving { 0 } else { m.still_secs + dt };
            m.water_still_secs = if moving || !in_water {
                0
            } else {
                m.water_still_secs + dt
            };
            m.last_ts = now;
            m.last_anchor = a;
            m.seeded = true;
            if person && a.1 < FALL_LOWER_BAND - UPRIGHT_MARGIN {
                m.saw_upright = true;
            }
            let in_band = a.1 >= FALL_LOWER_BAND;
            if fall_detect
                && person
                && in_band
                && m.saw_upright
                && m.still_secs >= FALL_STILL_SECS
                && !m.fall_fired
            {
                m.fall_fired = true;
                out.push(ResEvent {
                    kind: ResKind::Fall,
                    label: "fall".into(),
                    zone: None,
                    track_id: t.id,
                    anchor: a,
                });
            }
            if moving {
                m.fall_fired = false; // re-arm once the person recovers/moves again
            }
            if in_water && m.water_still_secs >= WATER_STILL_SECS && !m.water_fired {
                m.water_fired = true;
                let zname = zones
                    .iter()
                    .find(|z| z.water && z.applies_to(&t.label) && z.contains(a.0, a.1))
                    .map(|z| z.name.clone());
                out.push(ResEvent {
                    kind: ResKind::StillWater,
                    label: "still_water".into(),
                    zone: zname,
                    track_id: t.id,
                    anchor: a,
                });
            }
            if moving || !in_water {
                m.water_fired = false;
            }

            // --- zone membership: enter edges + supervise tallies -------------
            for (zi, z) in zones.iter().enumerate() {
                let inside_now = z.applies_to(&t.label) && z.contains(a.0, a.1);
                if z.supervise && inside_now {
                    if child {
                        child_cnt[zi] += 1;
                        child_anchor[zi].get_or_insert(a);
                    }
                    if adult {
                        adult_cnt[zi] += 1;
                    }
                }
                if z.alert_enter || z.child_watch {
                    let key = (t.id, zi);
                    let was = self.inside.get(&key).copied().unwrap_or(false);
                    if inside_now && !was {
                        if z.alert_enter {
                            out.push(ResEvent {
                                kind: ResKind::Enter,
                                label: t.label.clone(),
                                zone: Some(z.name.clone()),
                                track_id: t.id,
                                anchor: a,
                            });
                        }
                        if z.child_watch && child {
                            out.push(ResEvent {
                                kind: ResKind::ChildInZone,
                                label: "child".into(),
                                zone: Some(z.name.clone()),
                                track_id: t.id,
                                anchor: a,
                            });
                        }
                    }
                    self.inside.insert(key, inside_now);
                }
            }
        }

        // --- child-alone (no adult present) edge-trigger per supervise zone ---
        for (zi, z) in zones.iter().enumerate() {
            if !z.supervise {
                self.alone_over.remove(&zi);
                continue;
            }
            let alone = child_cnt[zi] >= 1 && adult_cnt[zi] == 0;
            let was = self.alone_over.get(&zi).copied().unwrap_or(false);
            self.alone_over.insert(zi, alone);
            if alone && !was {
                out.push(ResEvent {
                    kind: ResKind::ChildAlone,
                    label: "child_alone".into(),
                    zone: Some(z.name.clone()),
                    track_id: 0,
                    anchor: child_anchor[zi].unwrap_or_else(|| zone_centroid(z)),
                });
            }
        }

        // Garbage-collect state for tracks that no longer exist.
        self.inside.retain(|(id, _), _| live.contains(id));
        self.motion.retain(|id, _| live.contains(id));

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use tracker::BBox;

    /// Build a confirmed track: ground-contact anchor at `(cx, bottom)`, box
    /// `height` tall (frame fractions). Height drives the child/adult split.
    fn trk(id: u64, label: &str, cx: f32, bottom: f32, height: f32) -> Track {
        Track {
            id,
            label: label.to_string(),
            bbox: BBox::new(cx - 0.02, bottom - height, cx + 0.02, bottom),
            vx: 0.0,
            vy: 0.0,
            history: VecDeque::new(),
            hits: 5,
            misses: 0,
            confirmed: true,
            start_ts: 0,
            last_ts: 0,
        }
    }

    fn zone(name: &str) -> PolyZone {
        PolyZone {
            name: name.to_string(),
            points: vec![[0.3, 0.3], [0.7, 0.3], [0.7, 0.7], [0.3, 0.7]],
            ..Default::default()
        }
    }

    fn count(out: &[ResEvent], k: ResKind) -> usize {
        out.iter().filter(|e| e.kind == k).count()
    }

    #[test]
    fn enter_fires_once_per_entry() {
        let mut st = ResidentialState::default();
        let z = vec![PolyZone {
            alert_enter: true,
            ..zone("pool")
        }];
        let mut total = Vec::new();
        // Outside, then inside for three frames, then leaves, then re-enters.
        let path = [(0.1, 0.1), (0.5, 0.5), (0.5, 0.5), (0.1, 0.1), (0.5, 0.5)];
        for (i, (x, y)) in path.iter().enumerate() {
            let t = trk(1, "person", *x, *y, 0.3);
            total.extend(st.tick(&[&t], &z, None, false, i as i64));
        }
        assert_eq!(count(&total, ResKind::Enter), 2, "one Enter per entry");
        assert_eq!(total[0].zone.as_deref(), Some("pool"));
        assert_eq!(total[0].label, "person");
    }

    #[test]
    fn child_in_zone_requires_child_height() {
        let z = vec![PolyZone {
            child_watch: true,
            ..zone("stairs")
        }];
        // A child (short) enters -> ChildInZone; an adult (tall) does not.
        let mut st = ResidentialState::default();
        let child = trk(1, "person", 0.5, 0.5, 0.2);
        let out = st.tick(&[&child], &z, Some(0.3), false, 0);
        assert_eq!(count(&out, ResKind::ChildInZone), 1);

        let mut st2 = ResidentialState::default();
        let adult = trk(2, "person", 0.5, 0.5, 0.5);
        let out2 = st2.tick(&[&adult], &z, Some(0.3), false, 0);
        assert_eq!(
            count(&out2, ResKind::ChildInZone),
            0,
            "adult is not a child"
        );
    }

    #[test]
    fn child_features_off_without_calibration() {
        let z = vec![PolyZone {
            child_watch: true,
            ..zone("stairs")
        }];
        let mut st = ResidentialState::default();
        let child = trk(1, "person", 0.5, 0.5, 0.1);
        // child_height_frac = None -> child classification disabled entirely.
        let out = st.tick(&[&child], &z, None, false, 0);
        assert_eq!(count(&out, ResKind::ChildInZone), 0);
    }

    #[test]
    fn child_alone_fires_without_adult_and_suppressed_with_one() {
        let z = vec![PolyZone {
            supervise: true,
            ..zone("poolyard")
        }];
        let mut st = ResidentialState::default();
        let child = trk(1, "person", 0.5, 0.5, 0.2);
        let adult = trk(2, "person", 0.45, 0.55, 0.5);

        // Child alone -> fires.
        let o0 = st.tick(&[&child], &z, Some(0.3), false, 0);
        assert_eq!(count(&o0, ResKind::ChildAlone), 1);
        // Adult joins -> latched, no new fire.
        let o1 = st.tick(&[&child, &adult], &z, Some(0.3), false, 1);
        assert_eq!(count(&o1, ResKind::ChildAlone), 0);
        // Adult leaves, child still alone -> re-fires.
        let o2 = st.tick(&[&child], &z, Some(0.3), false, 2);
        assert_eq!(count(&o2, ResKind::ChildAlone), 1);
    }

    #[test]
    fn fall_fires_after_motionless_in_floor_band() {
        let mut st = ResidentialState::default();
        let mut total = Vec::new();
        for now in 0..10 {
            // Upright at t=0, then down in the floor band and motionless.
            let (x, y) = if now == 0 { (0.5, 0.3) } else { (0.5, 0.85) };
            let t = trk(1, "person", x, y, 0.1);
            total.extend(st.tick(&[&t], &[], None, true, now));
        }
        assert_eq!(
            count(&total, ResKind::Fall),
            1,
            "one fall after the still period"
        );
    }

    #[test]
    fn no_fall_when_upright_and_still() {
        let mut st = ResidentialState::default();
        let mut total = Vec::new();
        for now in 0..12 {
            let t = trk(1, "person", 0.5, 0.3, 0.1); // stays high in frame
            total.extend(st.tick(&[&t], &[], None, true, now));
        }
        assert_eq!(
            count(&total, ResKind::Fall),
            0,
            "standing still is not a fall"
        );
    }

    #[test]
    fn still_water_fires_after_motionless_in_water_zone() {
        let z = vec![PolyZone {
            water: true,
            ..zone("pool")
        }];
        let mut st = ResidentialState::default();
        let mut total = Vec::new();
        for now in 0..11 {
            let t = trk(1, "person", 0.5, 0.5, 0.1);
            total.extend(st.tick(&[&t], &z, None, false, now));
        }
        assert_eq!(count(&total, ResKind::StillWater), 1);
        assert_eq!(total.last().unwrap().zone.as_deref(), Some("pool"));
    }

    #[test]
    fn state_gc_for_gone_tracks() {
        let z = vec![PolyZone {
            alert_enter: true,
            ..zone("pool")
        }];
        let mut st = ResidentialState::default();
        let t = trk(1, "person", 0.5, 0.5, 0.3);
        st.tick(&[&t], &z, None, true, 0);
        assert!(!st.motion.is_empty() && !st.inside.is_empty());
        st.tick(&[], &z, None, true, 1);
        assert!(st.motion.is_empty(), "motion state GC'd for retired track");
        assert!(
            st.inside.is_empty(),
            "membership state GC'd for retired track"
        );
    }
}
