//! Lightweight multi-object tracker (SORT-lite): associates per-frame object
//! detections into persistent **tracks** with stable IDs and centroid
//! trajectories, so the analytics layer (line-crossing, loitering, counting,
//! occupancy, speed, heatmaps) can reason about *one physical object across
//! frames* instead of independent per-frame boxes.
//!
//! Pure geometry — no Kalman/FFI/GPU/allocations-per-pixel. The recipe:
//!   1. **Predict** each existing track's box forward by its constant velocity.
//!   2. **Associate** current detections to predicted tracks by IoU, greedily,
//!      ByteTrack-style in two passes: confident detections first, then a
//!      recovery pass that lets leftover *low*-confidence detections re-attach
//!      to still-unmatched tracks (rescues partial occlusions). Only same-label
//!      boxes match.
//!   3. **Manage lifecycle** with hit/miss hysteresis: a new detection becomes a
//!      *confirmed* track after `min_hits` consecutive frames; a track survives
//!      `max_age` missed frames (occlusion) before it is retired.
//!
//! All coordinates are **frame fractions** (0..1, top-left origin) so tracks
//! live in the same coordinate space as zones and tripwires.

use std::collections::VecDeque;

use serde::Serialize;

/// An axis-aligned box in frame-fraction coordinates (0..1).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BBox {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl BBox {
    pub fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }
    pub fn cx(&self) -> f32 {
        (self.x1 + self.x2) * 0.5
    }
    pub fn cy(&self) -> f32 {
        (self.y1 + self.y2) * 0.5
    }
    pub fn w(&self) -> f32 {
        (self.x2 - self.x1).max(0.0)
    }
    pub fn h(&self) -> f32 {
        (self.y2 - self.y1).max(0.0)
    }
    pub fn area(&self) -> f32 {
        self.w() * self.h()
    }
    /// Bottom-center of the box — the foot/wheel ground-contact point, which is
    /// the right reference for zone containment and line crossing (it cuts the
    /// perspective error a centroid would carry for tall objects).
    pub fn anchor(&self) -> (f32, f32) {
        (self.cx(), self.y2)
    }
    /// Intersection-over-union with another box.
    pub fn iou(&self, o: &BBox) -> f32 {
        let ix1 = self.x1.max(o.x1);
        let iy1 = self.y1.max(o.y1);
        let ix2 = self.x2.min(o.x2);
        let iy2 = self.y2.min(o.y2);
        let iw = (ix2 - ix1).max(0.0);
        let ih = (iy2 - iy1).max(0.0);
        let inter = iw * ih;
        let union = self.area() + o.area() - inter;
        if union <= 0.0 {
            0.0
        } else {
            inter / union
        }
    }
    fn shifted(&self, dx: f32, dy: f32) -> BBox {
        BBox {
            x1: self.x1 + dx,
            y1: self.y1 + dy,
            x2: self.x2 + dx,
            y2: self.y2 + dy,
        }
    }
}

/// One detection handed to the tracker for the current frame.
#[derive(Clone, Copy, Debug)]
pub struct Det<'a> {
    pub label: &'a str,
    pub score: f32,
    pub bbox: BBox,
}

/// A tracked object: a stable identity plus its recent trajectory.
#[derive(Clone, Debug, Serialize)]
pub struct Track {
    pub id: u64,
    pub label: String,
    /// Last observed box (frame fractions).
    pub bbox: BBox,
    /// Center velocity in fractions per frame (used for prediction).
    pub vx: f32,
    pub vy: f32,
    /// Bounded trajectory of `(ts, anchor_x, anchor_y)` ground-contact points.
    pub history: VecDeque<(i64, f32, f32)>,
    pub hits: u32,
    pub misses: u32,
    pub confirmed: bool,
    pub start_ts: i64,
    pub last_ts: i64,
}

impl Track {
    pub fn anchor(&self) -> (f32, f32) {
        self.bbox.anchor()
    }
    /// Predicted box for the next frame (constant-velocity).
    fn predicted(&self) -> BBox {
        self.bbox.shifted(self.vx, self.vy)
    }
}

/// Tunable association/lifecycle thresholds.
#[derive(Clone, Copy, Debug)]
pub struct TrackerConfig {
    /// Minimum IoU (against the *predicted* box) to associate a detection.
    pub iou_threshold: f32,
    /// Missed frames a track tolerates before retirement (occlusion budget).
    pub max_age: u32,
    /// Consecutive hits before a tentative track is `confirmed`.
    pub min_hits: u32,
    /// Score at/above which a detection is "high confidence" (first pass).
    pub high_score: f32,
    /// Bounded trajectory length kept per track.
    pub history_len: usize,
    /// EMA factor for velocity smoothing (0..1; higher = more responsive).
    pub vel_smooth: f32,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            iou_threshold: 0.2,
            max_age: 30,
            min_hits: 3,
            high_score: 0.5,
            history_len: 120,
            vel_smooth: 0.5,
        }
    }
}

/// Per-camera multi-object tracker. Call [`Tracker::update`] once per sampled
/// frame with that frame's detections.
#[derive(Debug)]
pub struct Tracker {
    cfg: TrackerConfig,
    tracks: Vec<Track>,
    next_id: u64,
}

impl Tracker {
    pub fn new(cfg: TrackerConfig) -> Self {
        Self {
            cfg,
            tracks: Vec::new(),
            next_id: 1,
        }
    }

    /// All live tracks (including tentative, not-yet-confirmed ones).
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// Only confirmed tracks — what analytics should act on.
    pub fn confirmed(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| t.confirmed)
    }

    /// Ingest one frame's detections and advance every track. Returns the IDs of
    /// the tracks that are confirmed *and* were updated this frame.
    pub fn update(&mut self, dets: &[Det], ts: i64) -> Vec<u64> {
        let n_tracks = self.tracks.len();
        // Predict every track's box once.
        let predicted: Vec<BBox> = self.tracks.iter().map(|t| t.predicted()).collect();

        let mut track_taken = vec![false; n_tracks];
        let mut det_taken = vec![false; dets.len()];
        // det index -> matched track index
        let mut matched: Vec<Option<usize>> = vec![None; dets.len()];

        // Pass 1: high-confidence detections.
        self.greedy_match(
            dets,
            &predicted,
            &mut track_taken,
            &mut det_taken,
            &mut matched,
            |d| d.score >= self.cfg.high_score,
        );
        // Pass 2 (ByteTrack recovery): leftover low-confidence detections re-
        // attach to still-unmatched tracks, rescuing partial occlusions.
        self.greedy_match(
            dets,
            &predicted,
            &mut track_taken,
            &mut det_taken,
            &mut matched,
            |d| d.score < self.cfg.high_score,
        );

        // Apply matches: update each matched track from its detection.
        for (di, m) in matched.iter().enumerate() {
            if let Some(ti) = *m {
                let d = &dets[di];
                let t = &mut self.tracks[ti];
                let (old_cx, old_cy) = (t.bbox.cx(), t.bbox.cy());
                let (new_cx, new_cy) = (d.bbox.cx(), d.bbox.cy());
                let s = self.cfg.vel_smooth;
                t.vx = (1.0 - s) * t.vx + s * (new_cx - old_cx);
                t.vy = (1.0 - s) * t.vy + s * (new_cy - old_cy);
                t.bbox = d.bbox;
                t.hits += 1;
                t.misses = 0;
                t.last_ts = ts;
                if t.hits >= self.cfg.min_hits {
                    t.confirmed = true;
                }
                let (ax, ay) = d.bbox.anchor();
                t.history.push_back((ts, ax, ay));
                while t.history.len() > self.cfg.history_len {
                    t.history.pop_front();
                }
            }
        }

        // Unmatched tracks age; spawn new tentative tracks for unmatched high dets.
        for (ti, taken) in track_taken.iter().enumerate() {
            if !taken {
                self.tracks[ti].misses += 1;
            }
        }
        for (di, taken) in det_taken.iter().enumerate() {
            if !taken && dets[di].score >= self.cfg.high_score {
                let d = &dets[di];
                let (ax, ay) = d.bbox.anchor();
                let mut history = VecDeque::with_capacity(self.cfg.history_len.min(16));
                history.push_back((ts, ax, ay));
                self.tracks.push(Track {
                    id: self.next_id,
                    label: d.label.to_string(),
                    bbox: d.bbox,
                    vx: 0.0,
                    vy: 0.0,
                    history,
                    hits: 1,
                    misses: 0,
                    confirmed: self.cfg.min_hits <= 1,
                    start_ts: ts,
                    last_ts: ts,
                });
                self.next_id += 1;
            }
        }

        // Retire tracks that have been lost too long.
        let max_age = self.cfg.max_age;
        self.tracks.retain(|t| t.misses <= max_age);

        self.tracks
            .iter()
            .filter(|t| t.confirmed && t.last_ts == ts)
            .map(|t| t.id)
            .collect()
    }

    /// Greedy IoU assignment over the detections passing `want`, against tracks
    /// not yet taken, highest-IoU pairs first. Same-label only.
    fn greedy_match(
        &self,
        dets: &[Det],
        predicted: &[BBox],
        track_taken: &mut [bool],
        det_taken: &mut [bool],
        matched: &mut [Option<usize>],
        want: impl Fn(&Det) -> bool,
    ) {
        // Build candidate (iou, track_idx, det_idx) above threshold + same label.
        let mut cands: Vec<(f32, usize, usize)> = Vec::new();
        for (ti, t) in self.tracks.iter().enumerate() {
            if track_taken[ti] {
                continue;
            }
            for (di, d) in dets.iter().enumerate() {
                if det_taken[di] || !want(d) || d.label != t.label {
                    continue;
                }
                let iou = predicted[ti].iou(&d.bbox);
                if iou >= self.cfg.iou_threshold {
                    cands.push((iou, ti, di));
                }
            }
        }
        // Highest IoU first; ties broken deterministically by indices.
        cands.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.cmp(&b.1))
                .then(a.2.cmp(&b.2))
        });
        for (_iou, ti, di) in cands {
            if track_taken[ti] || det_taken[di] {
                continue;
            }
            track_taken[ti] = true;
            det_taken[di] = true;
            matched[di] = Some(ti);
        }
    }
}

/// Signed side of the directed line `a -> b` that point `p` lies on, via the 2D
/// cross product `(b-a) x (p-a)`. `> 0` is the left side, `< 0` the right, `0`
/// on the line. A sign change of this value between two trajectory points means
/// the object crossed the line; the new sign gives the crossing direction.
pub fn side_of_line(a: (f32, f32), b: (f32, f32), p: (f32, f32)) -> f32 {
    (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(x1: f32, y1: f32, x2: f32, y2: f32) -> BBox {
        BBox::new(x1, y1, x2, y2)
    }
    fn det<'a>(label: &'a str, bb: BBox) -> Det<'a> {
        Det {
            label,
            score: 0.9,
            bbox: bb,
        }
    }
    // Confirm tracks quickly in tests.
    fn cfg() -> TrackerConfig {
        TrackerConfig {
            min_hits: 2,
            max_age: 3,
            ..Default::default()
        }
    }

    #[test]
    fn iou_and_anchor() {
        let a = b(0.0, 0.0, 0.2, 0.2);
        assert!((a.iou(&a) - 1.0).abs() < 1e-6);
        assert_eq!(a.iou(&b(0.5, 0.5, 0.6, 0.6)), 0.0);
        // Anchor is bottom-center.
        let (ax, ay) = b(0.2, 0.4, 0.4, 0.8).anchor();
        assert!((ax - 0.3).abs() < 1e-6 && (ay - 0.8).abs() < 1e-6);
    }

    #[test]
    fn single_object_keeps_stable_id() {
        let mut tr = Tracker::new(cfg());
        let mut id = None;
        // Move a person box rightward across 6 frames.
        for (i, x) in [0.10, 0.16, 0.22, 0.28, 0.34, 0.40].iter().enumerate() {
            let bb = b(*x, 0.4, x + 0.1, 0.7);
            tr.update(&[det("person", bb)], i as i64);
            let confirmed: Vec<_> = tr.confirmed().collect();
            if i >= 1 {
                assert_eq!(confirmed.len(), 1, "exactly one confirmed track");
                let cur = confirmed[0].id;
                match id {
                    None => id = Some(cur),
                    Some(prev) => assert_eq!(prev, cur, "id must stay stable"),
                }
            }
        }
    }

    #[test]
    fn two_objects_crossing_do_not_swap_ids() {
        let mut tr = Tracker::new(cfg());
        // A moves left->right, B moves right->left; they cross in the middle.
        // Constant-velocity prediction should keep their identities separated.
        let mut a_id = None;
        let mut b_id = None;
        for i in 0..8 {
            let t = i as f32;
            let ax = 0.10 + 0.05 * t; // 0.10 -> 0.45
            let bx = 0.45 - 0.05 * t; // 0.45 -> 0.10
            let dets = [
                det("person", b(ax, 0.4, ax + 0.08, 0.7)),
                det("person", b(bx, 0.4, bx + 0.08, 0.7)),
            ];
            tr.update(&dets, i);
            if i == 1 {
                let mut ids: Vec<_> = tr.confirmed().map(|t| (t.bbox.cx(), t.id)).collect();
                ids.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());
                // leftmost = A's track at this early frame
                a_id = Some(ids[0].1);
                b_id = Some(ids[1].1);
            }
            if i == 7 {
                // After crossing, A is now on the right, B on the left.
                let mut ids: Vec<_> = tr.confirmed().map(|t| (t.bbox.cx(), t.id)).collect();
                ids.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());
                let left_id = ids[0].1; // B ended on the left
                let right_id = ids[1].1; // A ended on the right
                assert_eq!(Some(right_id), a_id, "A kept its id through the crossing");
                assert_eq!(Some(left_id), b_id, "B kept its id through the crossing");
            }
        }
    }

    #[test]
    fn survives_brief_occlusion_then_reassociates() {
        let mut tr = Tracker::new(cfg());
        let mk = |x: f32| b(x, 0.4, x + 0.1, 0.7);
        tr.update(&[det("person", mk(0.10))], 0);
        tr.update(&[det("person", mk(0.16))], 1);
        let id = tr.confirmed().next().unwrap().id;
        // One frame with no detection (occlusion) — track must persist.
        tr.update(&[], 2);
        assert_eq!(
            tr.tracks().len(),
            1,
            "track survives a miss within max_age"
        );
        // Reappears near the predicted position -> same id.
        tr.update(&[det("person", mk(0.28))], 3);
        let again = tr.confirmed().next().unwrap().id;
        assert_eq!(id, again, "re-associates to the same id after occlusion");
    }

    #[test]
    fn retires_after_max_age() {
        let mut tr = Tracker::new(cfg());
        let mk = |x: f32| b(x, 0.4, x + 0.1, 0.7);
        tr.update(&[det("person", mk(0.10))], 0);
        tr.update(&[det("person", mk(0.16))], 1);
        assert_eq!(tr.tracks().len(), 1);
        // max_age = 3 misses tolerated; the 4th miss retires it.
        for t in 2..=5 {
            tr.update(&[], t);
        }
        assert_eq!(tr.tracks().len(), 0, "track retired after exceeding max_age");
    }

    #[test]
    fn confirms_only_after_min_hits() {
        let mut tr = Tracker::new(cfg()); // min_hits = 2
        let mk = |x: f32| b(x, 0.4, x + 0.1, 0.7);
        tr.update(&[det("person", mk(0.10))], 0);
        assert_eq!(tr.confirmed().count(), 0, "one hit is still tentative");
        tr.update(&[det("person", mk(0.16))], 1);
        assert_eq!(tr.confirmed().count(), 1, "confirmed on the second hit");
    }

    #[test]
    fn different_labels_do_not_associate() {
        let mut tr = Tracker::new(cfg());
        let bb = b(0.10, 0.4, 0.2, 0.7);
        tr.update(&[det("person", bb)], 0);
        // Same place, different label -> a separate track, not a re-association.
        tr.update(&[det("car", bb)], 1);
        assert_eq!(tr.tracks().len(), 2, "person and car are distinct tracks");
    }

    #[test]
    fn side_of_line_sign() {
        // Vertical line from (0.5,0) to (0.5,1): left side is x<0.5.
        let a = (0.5, 0.0);
        let bb = (0.5, 1.0);
        assert!(side_of_line(a, bb, (0.2, 0.5)) > 0.0, "left of upward line");
        assert!(side_of_line(a, bb, (0.8, 0.5)) < 0.0, "right of upward line");
        assert!(side_of_line(a, bb, (0.5, 0.5)).abs() < 1e-6, "on the line");
    }
}
