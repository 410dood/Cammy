//! Gait analysis & identification — a walking-biometric identity signal that
//! works at a distance and when the face isn't visible (Verkada/Watrix-class
//! "identify by how they walk"). It builds on the object tracker (#53): from a
//! confirmed person track's bounding-box trajectory we derive a compact,
//! scale-invariant **gait signature**, then match it against enrolled gait
//! profiles to attribute the person — or flag an unknown walker for enrollment,
//! exactly like the face flow.
//!
//! **Honest scope.** The detection pipeline samples ~1 fps, far below a true
//! step cadence (~2 Hz), so this is NOT forensic gait recognition. What survives
//! at this rate is a *body-and-motion* signature — build/posture (aspect ratio),
//! vertical bob and lateral sway relative to body size, apparent-height
//! variation, normalized pace, and path directness — which is a genuine
//! cross-camera identification *aid* (a coarse biometric / re-ID by walk), not a
//! court-grade match. Every feature is scale- and position-invariant so the same
//! person reads alike near and far.

use std::collections::HashMap;

/// Number of dimensions in a gait signature.
pub const GAIT_DIMS: usize = 7;

/// A compact gait/body signature (see module docs for the dimensions).
pub type GaitSignature = [f32; GAIT_DIMS];

/// Per-dimension spreads used to normalize distances so every dimension
/// contributes comparably (a difference of one spread ≈ 1.0 in each axis).
const SCALE: [f32; GAIT_DIMS] = [0.15, 0.05, 0.08, 0.10, 0.08, 1.0, 0.30];

/// One observation of a tracked person: a timestamp and a frame-fraction box.
#[derive(Clone, Copy, Debug)]
pub struct GaitSample {
    pub ts_ms: i64,
    /// `[x1, y1, x2, y2]` in 0..1 frame fractions.
    pub bbox: [f32; 4],
}

/// Tunables for signature extraction and matching.
#[derive(Clone, Copy, Debug)]
pub struct GaitParams {
    /// Minimum observations before a signature is computable.
    pub min_samples: usize,
    /// Minimum track lifetime (ms) for a usable signature.
    pub min_span_ms: i64,
    /// Minimum net travel (frame fractions) — a stationary person isn't walking.
    pub min_travel: f32,
    /// Max normalized distance for two signatures to count as the same person.
    pub match_threshold: f32,
}

impl Default for GaitParams {
    fn default() -> Self {
        Self {
            min_samples: 5,
            min_span_ms: 1500,
            min_travel: 0.05,
            match_threshold: 0.85,
        }
    }
}

fn mean(xs: &[f32]) -> f32 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().sum::<f32>() / xs.len() as f32
}

fn std(xs: &[f32]) -> f32 {
    if xs.len() < 2 {
        return 0.0;
    }
    let m = mean(xs);
    (xs.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / xs.len() as f32).sqrt()
}

/// Std of the residual after removing the best-fit line of `ys` against `ts`
/// (so a person walking diagonally across the frame doesn't read as "sway" —
/// only the wobble *around* their smooth path counts).
fn detrended_std(ts: &[f32], ys: &[f32]) -> f32 {
    let n = ys.len();
    if n < 3 {
        return 0.0;
    }
    let (mt, my) = (mean(ts), mean(ys));
    let mut sxx = 0.0;
    let mut sxy = 0.0;
    for i in 0..n {
        let dt = ts[i] - mt;
        sxx += dt * dt;
        sxy += dt * (ys[i] - my);
    }
    let slope = if sxx.abs() < 1e-9 { 0.0 } else { sxy / sxx };
    let intercept = my - slope * mt;
    let resid: Vec<f32> = (0..n)
        .map(|i| ys[i] - (slope * ts[i] + intercept))
        .collect();
    std(&resid)
}

/// Compute a gait signature from a track's samples, or `None` if the track
/// doesn't represent enough walking motion to be meaningful.
pub fn signature(samples: &[GaitSample], p: &GaitParams) -> Option<GaitSignature> {
    if samples.len() < p.min_samples {
        return None;
    }
    let span = samples.last().unwrap().ts_ms - samples.first().unwrap().ts_ms;
    if span < p.min_span_ms {
        return None;
    }
    let ts: Vec<f32> = samples
        .iter()
        .map(|s| (s.ts_ms - samples[0].ts_ms) as f32 / 1000.0)
        .collect();
    let w: Vec<f32> = samples
        .iter()
        .map(|s| (s.bbox[2] - s.bbox[0]).abs())
        .collect();
    let h: Vec<f32> = samples
        .iter()
        .map(|s| (s.bbox[3] - s.bbox[1]).abs())
        .collect();
    let cx: Vec<f32> = samples
        .iter()
        .map(|s| (s.bbox[0] + s.bbox[2]) * 0.5)
        .collect();
    let cy: Vec<f32> = samples
        .iter()
        .map(|s| (s.bbox[1] + s.bbox[3]) * 0.5)
        .collect();
    // Ground-contact (feet) point drives travel/path so perspective height
    // change doesn't masquerade as movement.
    let ax: Vec<f32> = cx.clone();
    let ay: Vec<f32> = samples.iter().map(|s| s.bbox[3]).collect();

    let mean_h = mean(&h);
    let mean_w = mean(&w);
    if mean_h < 1e-4 || mean_w < 1e-4 {
        return None;
    }

    // Net travel + path length (feet trajectory).
    let net = ((ax[ax.len() - 1] - ax[0]).powi(2) + (ay[ay.len() - 1] - ay[0]).powi(2)).sqrt();
    if net < p.min_travel {
        return None; // stationary — not a gait
    }
    let mut path = 0.0;
    for i in 1..ax.len() {
        path += ((ax[i] - ax[i - 1]).powi(2) + (ay[i] - ay[i - 1]).powi(2)).sqrt();
    }
    let straightness = if path > 1e-5 {
        (net / path).clamp(0.0, 1.0)
    } else {
        0.0
    };

    let aspect: Vec<f32> = w.iter().zip(&h).map(|(w, h)| w / h.max(1e-4)).collect();
    let span_s = (span as f32 / 1000.0).max(1e-3);

    let sig: GaitSignature = [
        mean(&aspect),                    // build / posture
        std(&aspect),                     // posture variation while walking
        detrended_std(&ts, &cy) / mean_h, // vertical bob, body-relative
        detrended_std(&ts, &cx) / mean_w, // lateral sway, body-relative
        std(&h) / mean_h,                 // apparent-height variation
        (path / span_s) / mean_h,         // pace in body-lengths/sec
        straightness,                     // path directness
    ];
    if sig.iter().any(|v| !v.is_finite()) {
        return None;
    }
    Some(sig)
}

/// Normalized distance between two signatures (RMS over spread-scaled dims).
pub fn distance(a: &GaitSignature, b: &GaitSignature) -> f32 {
    let mut acc = 0.0;
    for i in 0..GAIT_DIMS {
        let d = (a[i] - b[i]) / SCALE[i];
        acc += d * d;
    }
    (acc / GAIT_DIMS as f32).sqrt()
}

/// Nearest profile within `threshold`: `(index, distance)`, or `None`.
pub fn best_match(
    sig: &GaitSignature,
    profiles: &[GaitSignature],
    threshold: f32,
) -> Option<(usize, f32)> {
    let mut best: Option<(usize, f32)> = None;
    for (i, prof) in profiles.iter().enumerate() {
        let d = distance(sig, prof);
        if d <= threshold && best.map(|(_, bd)| d < bd).unwrap_or(true) {
            best = Some((i, d));
        }
    }
    best
}

// --- pipeline-side per-track accumulation -----------------------------------

/// Accumulated samples + decided identity for one tracked person.
#[derive(Clone, Debug, Default)]
pub struct GaitTrackBuf {
    pub samples: Vec<GaitSample>,
    pub last_ms: i64,
    /// Resolved identity once decided: a name, or `"?"` for a confident unknown.
    pub identity: Option<String>,
}

/// Per-camera gait accumulation across the tracker's confirmed tracks.
#[derive(Default)]
pub struct GaitState {
    tracks: HashMap<u64, GaitTrackBuf>,
}

impl GaitState {
    /// Record one observation of a confirmed track, bounding the buffer length.
    pub fn observe(&mut self, track_id: u64, bbox: [f32; 4], ts_ms: i64, cap: usize) {
        let buf = self.tracks.entry(track_id).or_default();
        buf.samples.push(GaitSample { ts_ms, bbox });
        if buf.samples.len() > cap {
            let overflow = buf.samples.len() - cap;
            buf.samples.drain(0..overflow);
        }
        buf.last_ms = ts_ms;
    }

    pub fn get(&self, track_id: u64) -> Option<&GaitTrackBuf> {
        self.tracks.get(&track_id)
    }
    pub fn get_mut(&mut self, track_id: u64) -> Option<&mut GaitTrackBuf> {
        self.tracks.get_mut(&track_id)
    }

    /// Drop tracks not observed within `after_ms` (retired by the tracker), so
    /// the map can't grow without bound.
    pub fn retire_stale(&mut self, now_ms: i64, after_ms: i64) {
        self.tracks.retain(|_, b| now_ms - b.last_ms <= after_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthesize a walking track: a person of given build crossing the frame
    /// with a given vertical bob + lateral sway, sampled at `fps` for `secs`.
    #[allow(clippy::too_many_arguments)]
    fn walk(
        aspect: f32,
        height: f32,
        bob: f32,
        sway: f32,
        speed: f32,
        seed: f32,
        secs: f32,
        fps: f32,
    ) -> Vec<GaitSample> {
        let n = (secs * fps) as usize;
        let w = height * aspect;
        (0..n)
            .map(|i| {
                let t = i as f32 / fps;
                let cx = 0.1 + speed * t + sway * ((t * 6.0 + seed).sin());
                let cy = 0.5 + bob * ((t * 5.0 + seed).cos());
                let ts_ms = (t * 1000.0) as i64;
                GaitSample {
                    ts_ms,
                    bbox: [
                        cx - w / 2.0,
                        cy - height / 2.0,
                        cx + w / 2.0,
                        cy + height / 2.0,
                    ],
                }
            })
            .collect()
    }

    #[test]
    fn stationary_or_short_tracks_have_no_signature() {
        let p = GaitParams::default();
        // Too few samples.
        let few = walk(0.4, 0.4, 0.02, 0.02, 0.1, 0.0, 0.5, 4.0);
        assert!(signature(&few, &p).is_none());
        // Long but not moving (speed 0, no sway) -> below min_travel.
        let still: Vec<GaitSample> = (0..20)
            .map(|i| GaitSample {
                ts_ms: i * 1000,
                bbox: [0.4, 0.3, 0.6, 0.9],
            })
            .collect();
        assert!(signature(&still, &p).is_none());
    }

    #[test]
    fn signature_is_scale_invariant() {
        let p = GaitParams::default();
        // Same person/gait, sampled near (tall box) and far (small box): the box
        // height differs but the normalized signature should be close.
        let near = walk(0.4, 0.5, 0.03, 0.03, 0.06, 1.0, 6.0, 2.0);
        let far = walk(0.4, 0.2, 0.012, 0.012, 0.024, 1.0, 6.0, 2.0); // 0.4x scale
        let sn = signature(&near, &p).unwrap();
        let sf = signature(&far, &p).unwrap();
        // Aspect + straightness identical; bob/sway/pace body-relative so close.
        assert!(
            distance(&sn, &sf) < p.match_threshold,
            "d={}",
            distance(&sn, &sf)
        );
    }

    #[test]
    fn same_person_matches_and_different_person_does_not() {
        let p = GaitParams::default();
        // Two walks of the SAME build/gait with different phase + a little noise.
        let a = walk(0.38, 0.45, 0.035, 0.030, 0.05, 0.0, 7.0, 2.0);
        let b = walk(0.40, 0.45, 0.030, 0.032, 0.05, 2.3, 7.0, 2.0);
        // A clearly DIFFERENT person: much wider build, big sway, little bob, faster.
        let c = walk(0.80, 0.45, 0.008, 0.10, 0.12, 1.1, 7.0, 2.0);
        let (sa, sb, sc) = (
            signature(&a, &p).unwrap(),
            signature(&b, &p).unwrap(),
            signature(&c, &p).unwrap(),
        );
        assert!(
            distance(&sa, &sb) < p.match_threshold,
            "same d={}",
            distance(&sa, &sb)
        );
        assert!(
            distance(&sa, &sc) > p.match_threshold,
            "diff d={}",
            distance(&sa, &sc)
        );

        // best_match picks the enrolled same-person profile, rejects when only
        // the different profile is enrolled.
        assert_eq!(
            best_match(&sa, &[sb], p.match_threshold).map(|(i, _)| i),
            Some(0)
        );
        assert_eq!(best_match(&sa, &[sc], p.match_threshold), None);
        // With both enrolled, the closer (same person) wins.
        assert_eq!(
            best_match(&sa, &[sc, sb], p.match_threshold).map(|(i, _)| i),
            Some(1)
        );
    }

    #[test]
    fn gait_state_caps_buffer_and_retires_stale() {
        let mut st = GaitState::default();
        for i in 0..10 {
            st.observe(7, [0.4, 0.3, 0.6, 0.9], i * 1000, 5);
        }
        assert_eq!(st.get(7).unwrap().samples.len(), 5, "buffer must be capped");
        st.observe(8, [0.1, 0.1, 0.2, 0.4], 20_000, 5);
        st.retire_stale(20_000, 5_000); // track 7 last seen at 9000 -> stale
        assert!(st.get(7).is_none());
        assert!(st.get(8).is_some());
    }
}
