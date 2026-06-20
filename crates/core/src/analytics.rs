//! Tracker-driven video analytics: **line-crossing** (virtual tripwires with
//! direction) and **loitering** (dwell-time in a zone). These are the flagship
//! commercial analytics that a per-frame detector cannot express — they need a
//! persistent per-object identity across frames, which the [`tracker`] crate
//! provides. This module is pure logic over confirmed tracks + per-camera
//! config; the pipeline drives [`AnalyticsState::tick`] once per sampled frame
//! and emits the resulting crossing/loiter events.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use tracker::{side_of_line, Track};

use crate::db::PolyZone;

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
    start_ts: i64,
    last_inside_ts: i64,
    fired: bool,
}

/// Per-camera analytics memory across frames: each track's last side of every
/// tripwire (to detect a crossing) and its dwell progress in every zone.
pub struct AnalyticsState {
    /// (track_id, tripwire_name) -> last signed side of the line.
    last_side: HashMap<(u64, String), f32>,
    /// (track_id, zone_name) -> dwell progress.
    dwell: HashMap<(u64, String), DwellState>,
    /// A track briefly lost to occlusion shouldn't reset its dwell timer; only
    /// reset after it's been continuously outside for this many seconds.
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
        now: i64,
    ) -> (Vec<Crossing>, Vec<Loiter>) {
        let mut crossings = Vec::new();
        let mut loiters = Vec::new();
        let live: HashSet<u64> = tracks.iter().map(|t| t.id).collect();

        for t in tracks {
            let anchor = t.anchor();

            // --- tripwires ---------------------------------------------------
            for tw in tripwires {
                if !tw.applies_to(&t.label) {
                    continue;
                }
                let side = side_of_line((tw.a[0], tw.a[1]), (tw.b[0], tw.b[1]), anchor);
                let key = (t.id, tw.name.clone());
                if let Some(&prev) = self.last_side.get(&key) {
                    // A sign flip between consecutive points = a crossing. Guard
                    // against exact-zero (on the line) readings so we don't fire
                    // twice as a point grazes the line.
                    if prev != 0.0 && side != 0.0 && (prev > 0.0) != (side > 0.0) {
                        let dir = if prev > 0.0 { Dir::AToB } else { Dir::BToA };
                        if dir.allowed_by(tw.direction) {
                            crossings.push(Crossing {
                                tripwire: tw.name.clone(),
                                track_id: t.id,
                                label: t.label.clone(),
                                dir,
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
            for z in zones {
                let Some(threshold) = dwell_threshold(z) else {
                    continue;
                };
                if !z.applies_to(&t.label) {
                    continue;
                }
                let key = (t.id, z.name.clone());
                let inside = z.contains(anchor.0, anchor.1);
                if inside {
                    let st = self.dwell.entry(key).or_insert(DwellState {
                        start_ts: now,
                        last_inside_ts: now,
                        fired: false,
                    });
                    st.last_inside_ts = now;
                    if !st.fired && now - st.start_ts >= threshold as i64 {
                        st.fired = true;
                        loiters.push(Loiter {
                            zone: z.name.clone(),
                            track_id: t.id,
                            label: t.label.clone(),
                            dwell_secs: now - st.start_ts,
                            anchor,
                        });
                    }
                } else if let Some(st) = self.dwell.get(&key) {
                    // Outside: only forget the dwell once it's been gone past the
                    // grace window (so a momentary occlusion doesn't reset it).
                    if now - st.last_inside_ts > self.grace_secs {
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
            let (cr, _) = st.tick(&[&t], &tw, &[], i as i64);
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
            let (cr, _) = st.tick(&[&t], &tw, &[], i as i64);
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
            let (cr, _) = st.tick(&[&t], &tw, &[], i as i64);
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
            let (cr, _) = st.tick(&[&t], &tw, &[], i as i64);
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
            let (cr, _) = st.tick(&[&t], &tws, &[], i as i64);
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
            let (_, lo) = st.tick(&[&t], &[], &zones, now);
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
            let (_, lo) = st.tick(&[&t], &[], &zones, now);
            total.extend(lo);
        }
        // Track leaves (no longer present) for the rest.
        for now in 2..8 {
            let (_, lo) = st.tick(&[], &[], &zones, now);
            total.extend(lo);
        }
        assert!(total.is_empty(), "a quick pass-through never loiters");
    }

    #[test]
    fn occlusion_within_grace_does_not_reset_dwell() {
        let mut st = AnalyticsState::default(); // grace = 3s
        let zones = vec![dwell_zone("entry", 5)];
        let mut total = Vec::new();
        // Inside for 3s.
        for now in 0..3 {
            let t = track_at(1, "person", 0.5, 0.5);
            let (_, lo) = st.tick(&[&t], &[], &zones, now);
            total.extend(lo);
        }
        // Briefly outside the polygon for 2s (within grace) — dwell must persist.
        for now in 3..5 {
            let t = track_at(1, "person", 0.1, 0.1); // outside the zone
            let (_, lo) = st.tick(&[&t], &[], &zones, now);
            total.extend(lo);
        }
        // Back inside: cumulative dwell from the original start still passes 5s.
        for now in 5..7 {
            let t = track_at(1, "person", 0.5, 0.5);
            let (_, lo) = st.tick(&[&t], &[], &zones, now);
            total.extend(lo);
        }
        assert_eq!(
            total.len(),
            1,
            "dwell survived the brief occlusion and fired"
        );
    }

    #[test]
    fn state_is_gc_d_for_gone_tracks() {
        let mut st = AnalyticsState::default();
        let tw = vec![tripwire("door", CrossDir::Both)];
        let t = track_at(1, "person", 0.3, 0.5);
        st.tick(&[&t], &tw, &[], 0);
        assert!(!st.last_side.is_empty());
        // Track gone next frame -> its side state is dropped.
        st.tick(&[], &tw, &[], 1);
        assert!(st.last_side.is_empty(), "state GC'd for retired track");
    }
}
