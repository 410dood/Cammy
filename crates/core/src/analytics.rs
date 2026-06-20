//! Tracker-driven video analytics: **line-crossing** (virtual tripwires with
//! direction) and **loitering** (dwell-time in a zone). These are the flagship
//! commercial analytics that a per-frame detector cannot express — they need a
//! persistent per-object identity across frames, which the [`tracker`] crate
//! provides. This module is pure logic over confirmed tracks + per-camera
//! config; the pipeline drives [`AnalyticsState::tick`] once per sampled frame
//! and emits the resulting crossing/loiter events.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use tracker::{side_of_line, Homography, Track};

use crate::db::PolyZone;

/// Trajectory window (secs) over which a track's speed is averaged.
const SPEED_WINDOW_SECS: i64 = 3;

/// Estimate a track's ground speed (km/h) from its recent trajectory, warped to
/// the ground plane by `h`. Sums ground distance over the points within the last
/// [`SPEED_WINDOW_SECS`] and divides by the elapsed time (so a curved path isn't
/// underestimated). Returns `None` for a too-short/young track or a point behind
/// the horizon; capped at a plausibility ceiling to reject calibration/IoU noise.
pub fn track_speed_kmh(t: &Track, h: &Homography) -> Option<f32> {
    let last_ts = t.history.back()?.0;
    let pts: Vec<(i64, f32, f32)> = t
        .history
        .iter()
        .filter(|(ts, _, _)| last_ts - *ts <= SPEED_WINDOW_SECS)
        .filter_map(|(ts, ax, ay)| h.project((*ax, *ay)).map(|(gx, gy)| (*ts, gx, gy)))
        .collect();
    if pts.len() < 2 {
        return None;
    }
    let dt = (pts.last()?.0 - pts.first()?.0) as f32;
    if dt <= 0.0 {
        return None;
    }
    let dist: f32 = pts
        .windows(2)
        .map(|w| ((w[1].1 - w[0].1).powi(2) + (w[1].2 - w[0].2).powi(2)).sqrt())
        .sum();
    let kmh = (dist / dt) * 3.6;
    (kmh.is_finite() && kmh <= 320.0).then_some(kmh)
}

/// Which way across a tripwire counts. The line is directed `a -> b`; a crossing
/// is classified by the side transition of the object's ground-contact point.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CrossDir {
    /// Fire on a crossing in either direction.
    #[default]
    Both,
    /// Only when crossing from the A-side to the B-side (side `+ -> -`).
    AToB,
    /// Only when crossing from the B-side to the A-side (side `- -> +`).
    BToA,
}

/// A directed virtual line ("tripwire") in frame-fraction coordinates (0..1).
/// Crossing it produces a `crossing` event — the canonical perimeter / in-out
/// counting primitive (people through a doorway, vehicles past a gate, one-way
/// enforcement).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Tripwire {
    pub name: String,
    /// Endpoints `[x, y]` as frame fractions.
    pub a: [f32; 2],
    pub b: [f32; 2],
    /// Which crossing direction fires (default: both).
    pub direction: CrossDir,
    /// Object labels this tripwire applies to; empty = any object.
    pub labels: Vec<String>,
    /// One-way enforcement: when set on a directional tripwire, a crossing in the
    /// *forbidden* direction fires a `wrong_way` event (instead of being silently
    /// suppressed). No effect on a `Both`-direction tripwire.
    #[serde(default)]
    pub alert_wrong_way: bool,
}

impl Tripwire {
    fn applies_to(&self, label: &str) -> bool {
        self.labels.is_empty() || self.labels.iter().any(|l| l == label)
    }
}

/// The concrete direction an object crossed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    AToB,
    BToA,
}

impl Dir {
    /// Stable string stored on the event (`"a_to_b"` / `"b_to_a"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Dir::AToB => "a_to_b",
            Dir::BToA => "b_to_a",
        }
    }
    fn allowed_by(self, cfg: CrossDir) -> bool {
        matches!(
            (cfg, self),
            (CrossDir::Both, _) | (CrossDir::AToB, Dir::AToB) | (CrossDir::BToA, Dir::BToA)
        )
    }
}

/// A line-crossing produced this tick.
#[derive(Clone, Debug, PartialEq)]
pub struct Crossing {
    pub tripwire: String,
    pub track_id: u64,
    pub label: String,
    pub dir: Dir,
    /// True if this crossing was against a one-way tripwire's allowed direction
    /// (a `wrong_way` event) rather than a normal `crossing`.
    pub wrong_way: bool,
    /// Estimated ground speed at the crossing (km/h), when the camera has a
    /// ground-plane calibration; `None` otherwise.
    pub speed_kmh: Option<f32>,
    /// Ground-contact point at the crossing (frame fractions).
    pub anchor: (f32, f32),
}

/// A loiter (dwell threshold reached) produced this tick.
#[derive(Clone, Debug, PartialEq)]
pub struct Loiter {
    pub zone: String,
    pub track_id: u64,
    pub label: String,
    pub dwell_secs: i64,
    pub anchor: (f32, f32),
}

#[derive(Clone, Copy, Debug)]
struct DwellState {
    /// Accumulated *contiguous* time the object has been inside the zone (secs).
    /// Only inside-time counts: stepping out pauses accrual (and never credits
    /// time spent outside), but within the grace window it doesn't reset.
    inside_secs: i64,
    /// Timestamp of the previous tick this (track, zone) pair was processed.
    last_ts: i64,
    /// Was the immediately-previous observation inside? Breaks the contiguous
    /// run so the gap across an out-excursion isn't mistakenly credited.
    contiguous: bool,
    /// Last tick the anchor was actually inside — drives grace-window expiry.
    last_inside_ts: i64,
    fired: bool,
}

/// Per-camera analytics memory across frames: each track's last side of every
/// tripwire (to detect a crossing) and its dwell progress in every zone.
pub struct AnalyticsState {
    /// (track_id, tripwire **index**) -> last signed side of the line. Keyed by
    /// index, not the user-editable name, so two tripwires that share a name
    /// can't collide into one cell (which would drop a real crossing).
    last_side: HashMap<(u64, usize), f32>,
    /// (track_id, zone **index**) -> dwell progress (same index-keying reason).
    dwell: HashMap<(u64, usize), DwellState>,
    /// When a track briefly steps outside, inside-time accrual pauses but the
    /// dwell state is kept alive for this many seconds before it's forgotten.
    grace_secs: i64,
}

impl Default for AnalyticsState {
    fn default() -> Self {
        Self {
            last_side: HashMap::new(),
            dwell: HashMap::new(),
            grace_secs: 3,
        }
    }
}

impl AnalyticsState {
    /// Advance analytics for one frame. `tracks` is the camera's *confirmed*
    /// tracks; `now` is the frame timestamp (unix secs). Returns the crossing
    /// and loiter events triggered this frame.
    pub fn tick(
        &mut self,
        tracks: &[&Track],
        tripwires: &[Tripwire],
        zones: &[PolyZone],
        homography: Option<&Homography>,
        now: i64,
    ) -> (Vec<Crossing>, Vec<Loiter>) {
        let mut crossings = Vec::new();
        let mut loiters = Vec::new();
        let live: HashSet<u64> = tracks.iter().map(|t| t.id).collect();

        for t in tracks {
            let anchor = t.anchor();

            // --- tripwires ---------------------------------------------------
            for (ti, tw) in tripwires.iter().enumerate() {
                if !tw.applies_to(&t.label) {
                    continue;
                }
                let side = side_of_line((tw.a[0], tw.a[1]), (tw.b[0], tw.b[1]), anchor);
                let key = (t.id, ti);
                if let Some(&prev) = self.last_side.get(&key) {
                    // A sign flip between consecutive points = a crossing. Guard
                    // against exact-zero (on the line) readings so we don't fire
                    // twice as a point grazes the line.
                    if prev != 0.0 && side != 0.0 && (prev > 0.0) != (side > 0.0) {
                        let dir = if prev > 0.0 { Dir::AToB } else { Dir::BToA };
                        let allowed = dir.allowed_by(tw.direction);
                        // Allowed direction -> a normal crossing. The forbidden
                        // direction on a one-way tripwire with alert_wrong_way ->
                        // a wrong_way crossing. (Both-direction tripwires have no
                        // forbidden direction, so nothing to alert.)
                        if allowed || (tw.alert_wrong_way && tw.direction != CrossDir::Both) {
                            let speed_kmh = homography.and_then(|h| track_speed_kmh(t, h));
                            crossings.push(Crossing {
                                tripwire: tw.name.clone(),
                                track_id: t.id,
                                label: t.label.clone(),
                                dir,
                                wrong_way: !allowed,
                                speed_kmh,
                                anchor,
                            });
                        }
                    }
                }
                if side != 0.0 {
                    self.last_side.insert(key, side);
                }
            }

            // --- loitering / dwell ------------------------------------------
            for (zi, z) in zones.iter().enumerate() {
                let Some(threshold) = dwell_threshold(z) else {
                    continue;
                };
                if !z.applies_to(&t.label) {
                    continue;
                }
                let key = (t.id, zi);
                if z.contains(anchor.0, anchor.1) {
                    let st = self.dwell.entry(key).or_insert(DwellState {
                        inside_secs: 0,
                        last_ts: now,
                        contiguous: false,
                        last_inside_ts: now,
                        fired: false,
                    });
                    // Credit only contiguous inside-time: add the gap since the
                    // previous tick *iff* that tick was also inside.
                    if st.contiguous {
                        st.inside_secs += (now - st.last_ts).max(0);
                    }
                    st.contiguous = true;
                    st.last_ts = now;
                    st.last_inside_ts = now;
                    if !st.fired && st.inside_secs >= threshold as i64 {
                        st.fired = true;
                        loiters.push(Loiter {
                            zone: z.name.clone(),
                            track_id: t.id,
                            label: t.label.clone(),
                            dwell_secs: st.inside_secs,
                            anchor,
                        });
                    }
                } else {
                    // Outside: break the contiguous run (so the out-excursion gap
                    // isn't credited), and forget the dwell only once it's been
                    // gone past the grace window.
                    let expired = match self.dwell.get_mut(&key) {
                        Some(st) => {
                            st.contiguous = false;
                            now - st.last_inside_ts > self.grace_secs
                        }
                        None => false,
                    };
                    if expired {
                        self.dwell.remove(&key);
                    }
                }
            }
        }

        // Garbage-collect state for tracks that no longer exist.
        self.last_side.retain(|(id, _), _| live.contains(id));
        self.dwell.retain(|(id, _), _| live.contains(id));

        (crossings, loiters)
    }
}

/// A zone participates in loiter detection when it carries a positive
/// `dwell_secs`. Stored on `PolyZone` (added field); `None`/0 = not a dwell zone.
fn dwell_threshold(z: &PolyZone) -> Option<u32> {
    z.dwell_secs.filter(|s| *s > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use tracker::BBox;

    /// Build a confirmed track whose ground-contact anchor is at `(px, py)`.
    fn track_at(id: u64, label: &str, px: f32, py: f32) -> Track {
        // anchor = bottom-center, so center x = px and bottom y = py.
        let bbox = BBox::new(px - 0.02, py - 0.1, px + 0.02, py);
        Track {
            id,
            label: label.to_string(),
            bbox,
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

    fn tripwire(name: &str, dir: CrossDir) -> Tripwire {
        // Vertical line at x = 0.5.
        Tripwire {
            name: name.to_string(),
            a: [0.5, 0.0],
            b: [0.5, 1.0],
            direction: dir,
            labels: vec![],
            alert_wrong_way: false,
        }
    }

    fn dwell_zone(name: &str, secs: u32) -> PolyZone {
        PolyZone {
            name: name.to_string(),
            points: vec![[0.3, 0.3], [0.7, 0.3], [0.7, 0.7], [0.3, 0.7]],
            kind: crate::db::ZoneKind::Required,
            labels: vec![],
            dwell_secs: Some(secs),
        }
    }

    #[test]
    fn crossing_fires_once_with_direction() {
        let mut st = AnalyticsState::default();
        let tw = vec![tripwire("door", CrossDir::Both)];
        // Move a person left -> right across x=0.5 over three frames.
        let xs = [0.3_f32, 0.45, 0.55];
        let mut total = Vec::new();
        for (i, x) in xs.iter().enumerate() {
            let t = track_at(1, "person", *x, 0.5);
            let (cr, _) = st.tick(&[&t], &tw, &[], None, i as i64);
            total.extend(cr);
        }
        assert_eq!(total.len(), 1, "exactly one crossing for one pass");
        assert_eq!(total[0].dir, Dir::AToB, "left->right is A->B (side + -> -)");
        assert_eq!(total[0].tripwire, "door");
    }

    #[test]
    fn reverse_crossing_is_other_direction() {
        let mut st = AnalyticsState::default();
        let tw = vec![tripwire("door", CrossDir::Both)];
        let xs = [0.7_f32, 0.55, 0.45]; // right -> left
        let mut total = Vec::new();
        for (i, x) in xs.iter().enumerate() {
            let t = track_at(1, "person", *x, 0.5);
            let (cr, _) = st.tick(&[&t], &tw, &[], None, i as i64);
            total.extend(cr);
        }
        assert_eq!(total.len(), 1);
        assert_eq!(total[0].dir, Dir::BToA, "right->left is B->A (side - -> +)");
    }

    #[test]
    fn direction_filter_suppresses_wrong_way() {
        let mut st = AnalyticsState::default();
        let tw = vec![tripwire("oneway", CrossDir::AToB)];
        // Cross right->left (B->A): should NOT fire on an A->B-only tripwire.
        let xs = [0.7_f32, 0.55, 0.45];
        let mut total = Vec::new();
        for (i, x) in xs.iter().enumerate() {
            let t = track_at(1, "person", *x, 0.5);
            let (cr, _) = st.tick(&[&t], &tw, &[], None, i as i64);
            total.extend(cr);
        }
        assert!(total.is_empty(), "wrong-way crossing suppressed");
    }

    #[test]
    fn no_crossing_when_staying_on_one_side() {
        let mut st = AnalyticsState::default();
        let tw = vec![tripwire("door", CrossDir::Both)];
        let xs = [0.2_f32, 0.25, 0.3, 0.25];
        let mut total = Vec::new();
        for (i, x) in xs.iter().enumerate() {
            let t = track_at(1, "person", *x, 0.5);
            let (cr, _) = st.tick(&[&t], &tw, &[], None, i as i64);
            total.extend(cr);
        }
        assert!(total.is_empty());
    }

    #[test]
    fn label_filter_on_tripwire() {
        let mut st = AnalyticsState::default();
        let mut tw = tripwire("vehgate", CrossDir::Both);
        tw.labels = vec!["car".into()];
        let tws = vec![tw];
        // A person crosses — should be ignored (tripwire is car-only).
        for (i, x) in [0.3_f32, 0.55].iter().enumerate() {
            let t = track_at(1, "person", *x, 0.5);
            let (cr, _) = st.tick(&[&t], &tws, &[], None, i as i64);
            assert!(cr.is_empty(), "person ignored by car-only tripwire");
        }
    }

    #[test]
    fn loiter_fires_after_threshold_once() {
        let mut st = AnalyticsState::default();
        let zones = vec![dwell_zone("entry", 5)];
        // Person sits at (0.5,0.5) inside the zone for 7 seconds.
        let mut total = Vec::new();
        for now in 0..8 {
            let t = track_at(1, "person", 0.5, 0.5);
            let (_, lo) = st.tick(&[&t], &[], &zones, None, now);
            total.extend(lo);
        }
        assert_eq!(total.len(), 1, "loiter fires exactly once at the threshold");
        assert!(total[0].dwell_secs >= 5);
        assert_eq!(total[0].zone, "entry");
    }

    #[test]
    fn passing_through_does_not_loiter() {
        let mut st = AnalyticsState::default();
        let zones = vec![dwell_zone("entry", 5)];
        // Inside only briefly (2s) then gone.
        let mut total = Vec::new();
        for now in 0..2 {
            let t = track_at(1, "person", 0.5, 0.5);
            let (_, lo) = st.tick(&[&t], &[], &zones, None, now);
            total.extend(lo);
        }
        // Track leaves (no longer present) for the rest.
        for now in 2..8 {
            let (_, lo) = st.tick(&[], &[], &zones, None, now);
            total.extend(lo);
        }
        assert!(total.is_empty(), "a quick pass-through never loiters");
    }

    #[test]
    fn brief_outside_excursion_pauses_but_does_not_reset_dwell() {
        // Inside-time accrues; a short step outside (within grace) PAUSES the
        // accumulator but doesn't reset it, so the accrued inside-time survives.
        let mut st = AnalyticsState::default(); // grace = 3s
        let zones = vec![dwell_zone("entry", 5)];
        let mut total = Vec::new();
        // Inside 0..5 -> accrues ~4s of inside-time (first sample doesn't credit).
        for now in 0..5 {
            let t = track_at(1, "person", 0.5, 0.5);
            total.extend(st.tick(&[&t], &[], &zones, None, now).1);
        }
        // Outside for 2s (within grace) — accrual pauses, state persists.
        for now in 5..7 {
            let t = track_at(1, "person", 0.1, 0.1);
            total.extend(st.tick(&[&t], &[], &zones, None, now).1);
        }
        // Back inside: a couple more inside-seconds push the accumulator past 5
        // (only possible because the prior 4s wasn't reset by the excursion).
        for now in 7..10 {
            let t = track_at(1, "person", 0.5, 0.5);
            total.extend(st.tick(&[&t], &[], &zones, None, now).1);
        }
        assert_eq!(
            total.len(),
            1,
            "dwell survived the excursion and fired once"
        );
    }

    #[test]
    fn intermittent_presence_does_not_over_count_loiter() {
        // An object mostly OUTSIDE (in/out faster than it accrues) must not trip
        // a loiter — wall-clock-since-entry would have falsely fired here.
        let mut st = AnalyticsState::default();
        let zones = vec![dwell_zone("entry", 5)];
        let mut total = Vec::new();
        // Inside only at t = 0,3,6,9; outside (within grace) in between.
        for now in 0..12 {
            let inside = now % 3 == 0;
            let (px, py) = if inside { (0.5, 0.5) } else { (0.1, 0.1) };
            let t = track_at(1, "person", px, py);
            total.extend(st.tick(&[&t], &[], &zones, None, now).1);
        }
        assert!(
            total.is_empty(),
            "time spent outside the zone is not credited toward dwell"
        );
    }

    #[test]
    fn state_is_gc_d_for_gone_tracks() {
        let mut st = AnalyticsState::default();
        let tw = vec![tripwire("door", CrossDir::Both)];
        let t = track_at(1, "person", 0.3, 0.5);
        st.tick(&[&t], &tw, &[], None, 0);
        assert!(!st.last_side.is_empty());
        // Track gone next frame -> its side state is dropped.
        st.tick(&[], &tw, &[], None, 1);
        assert!(st.last_side.is_empty(), "state GC'd for retired track");
    }

    #[test]
    fn wrong_way_alert_on_forbidden_direction() {
        let mut st = AnalyticsState::default();
        let mut tw = tripwire("oneway", CrossDir::AToB);
        tw.alert_wrong_way = true;
        let tws = vec![tw];
        // Cross the FORBIDDEN direction (b_to_a): right -> left.
        let mut total = Vec::new();
        for (i, x) in [0.7_f32, 0.55, 0.45].iter().enumerate() {
            let t = track_at(1, "person", *x, 0.5);
            total.extend(st.tick(&[&t], &tws, &[], None, i as i64).0);
        }
        assert_eq!(
            total.len(),
            1,
            "wrong-way crossing fires when alerting is on"
        );
        assert!(total[0].wrong_way, "marked as a wrong-way crossing");
        assert_eq!(total[0].dir, Dir::BToA);
    }

    #[test]
    fn allowed_direction_is_not_wrong_way() {
        let mut st = AnalyticsState::default();
        let mut tw = tripwire("oneway", CrossDir::AToB);
        tw.alert_wrong_way = true;
        let tws = vec![tw];
        // Cross the ALLOWED direction (a_to_b): left -> right.
        let mut total = Vec::new();
        for (i, x) in [0.3_f32, 0.45, 0.55].iter().enumerate() {
            let t = track_at(1, "person", *x, 0.5);
            total.extend(st.tick(&[&t], &tws, &[], None, i as i64).0);
        }
        assert_eq!(total.len(), 1);
        assert!(
            !total[0].wrong_way,
            "the allowed direction is a normal crossing"
        );
    }

    #[test]
    fn speed_from_calibrated_track() {
        use std::collections::VecDeque;
        use tracker::BBox;
        // Image square maps to a 20 m x 10 m ground rectangle (axis-aligned ->
        // affine, so distances are linear and easy to reason about).
        let h = Homography::from_quad([(0.2, 0.2), (0.8, 0.2), (0.8, 0.8), (0.2, 0.8)], 20.0, 10.0)
            .unwrap();
        // Anchor moves image x 0.35 -> 0.65 over 2 s: ground 5 m -> 15 m = 10 m
        // in 2 s = 5 m/s = 18 km/h.
        let mut history = VecDeque::new();
        history.push_back((0i64, 0.35f32, 0.5f32));
        history.push_back((2i64, 0.65f32, 0.5f32));
        let t = Track {
            id: 1,
            label: "car".into(),
            bbox: BBox::new(0.63, 0.4, 0.67, 0.5),
            vx: 0.0,
            vy: 0.0,
            history,
            hits: 5,
            misses: 0,
            confirmed: true,
            start_ts: 0,
            last_ts: 2,
        };
        let kmh = track_speed_kmh(&t, &h).unwrap();
        assert!((kmh - 18.0).abs() < 0.5, "got {kmh} km/h, expected ~18");
    }
}
