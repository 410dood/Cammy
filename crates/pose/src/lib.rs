//! Body-posture support: a posture taxonomy plus a pure geometric classifier
//! over the 17-keypoint COCO layout (the output of a YOLOv8-pose / MoveNet model).
//!
//! Like the [`gesture`](../gesture) crate, this is the server's shared, testable
//! brain. The big difference from the hand path: body pose for **safety** (fall,
//! crib rollover, climb-out, covered-face) must run **24/7 headless on the
//! server**, not in an open browser tab — so the keypoints come from a server-side
//! ONNX YOLOv8-pose model in the core pipeline, and this crate turns them into a
//! posture. It is pure (no model, no I/O) so the geometry is unit-tested in
//! isolation.
//!
//! Coordinates are frame fractions (0..1, top-left origin), so the classifier is
//! resolution-independent and shares the zone/tracker coordinate space.
//!
//! ## SAFETY (read `docs/05-residential-analytics-suite.md`)
//! Every output is an **assistive, best-effort hint** — never a guarantee, never a
//! medical device. 2D keypoints can't reliably separate prone from supine under a
//! blanket, and overhead/odd framing degrades all of it. The pipeline + UI disclaim
//! these and they must never be presented as SIDS / suffocation / fall certainty.

use serde::{Deserialize, Serialize};

/// COCO 17-keypoint indices (the standard pose-model output order).
pub mod kp {
    pub const NOSE: usize = 0;
    pub const LEFT_EYE: usize = 1;
    pub const RIGHT_EYE: usize = 2;
    pub const LEFT_EAR: usize = 3;
    pub const RIGHT_EAR: usize = 4;
    pub const LEFT_SHOULDER: usize = 5;
    pub const RIGHT_SHOULDER: usize = 6;
    pub const LEFT_ELBOW: usize = 7;
    pub const RIGHT_ELBOW: usize = 8;
    pub const LEFT_WRIST: usize = 9;
    pub const RIGHT_WRIST: usize = 10;
    pub const LEFT_HIP: usize = 11;
    pub const RIGHT_HIP: usize = 12;
    pub const LEFT_KNEE: usize = 13;
    pub const RIGHT_KNEE: usize = 14;
    pub const LEFT_ANKLE: usize = 15;
    pub const RIGHT_ANKLE: usize = 16;
}

pub const NUM_KEYPOINTS: usize = 17;

/// Minimum per-keypoint confidence to treat a joint as observed.
const MIN_KP_CONF: f32 = 0.3;

/// One body keypoint in frame fractions with the model's confidence.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Keypoint {
    pub x: f32,
    pub y: f32,
    pub conf: f32,
}

impl Keypoint {
    fn valid(&self) -> bool {
        self.conf >= MIN_KP_CONF && self.x.is_finite() && self.y.is_finite()
    }
}

/// A detected person's 17 keypoints.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Pose {
    pub kpts: [Keypoint; NUM_KEYPOINTS],
}

/// Canonical posture labels (the snake_case strings used by events + alarm rules).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Posture {
    Standing,
    Sitting,
    Lying,
    Unknown,
}

impl Posture {
    pub fn as_str(self) -> &'static str {
        match self {
            Posture::Standing => "standing",
            Posture::Sitting => "sitting",
            Posture::Lying => "lying",
            Posture::Unknown => "unknown",
        }
    }
}

/// The result of classifying a pose: the coarse posture, whether the face is
/// observable (for the covered-face hint), and a 0..1 confidence (the fraction of
/// keypoints the model actually saw — low = treat the posture as unreliable).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Assessment {
    pub posture: Posture,
    pub face_visible: bool,
    pub confidence: f32,
}

impl Pose {
    /// Midpoint of two keypoints, falling back to whichever one is valid; `None`
    /// if neither is observed.
    fn mid(&self, a: usize, b: usize) -> Option<(f32, f32)> {
        let (ka, kb) = (self.kpts[a], self.kpts[b]);
        match (ka.valid(), kb.valid()) {
            (true, true) => Some(((ka.x + kb.x) * 0.5, (ka.y + kb.y) * 0.5)),
            (true, false) => Some((ka.x, ka.y)),
            (false, true) => Some((kb.x, kb.y)),
            (false, false) => None,
        }
    }

    /// Is a face observable? Nose, or both eyes — the basis for the covered-face
    /// hint (body present but face not visible for a while).
    pub fn face_visible(&self) -> bool {
        self.kpts[kp::NOSE].valid()
            || (self.kpts[kp::LEFT_EYE].valid() && self.kpts[kp::RIGHT_EYE].valid())
    }

    /// Fraction of the 17 keypoints the model actually observed.
    pub fn confidence(&self) -> f32 {
        let seen = self.kpts.iter().filter(|k| k.valid()).count();
        seen as f32 / NUM_KEYPOINTS as f32
    }

    /// Classify the coarse posture from torso orientation + leg geometry. Tuned
    /// for frontal/oblique views (the standard fall / elderly framing); overhead
    /// crib framing is degraded and should be treated as a hint only.
    pub fn classify(&self) -> Assessment {
        let face_visible = self.face_visible();
        let confidence = self.confidence();

        let (Some(sh), Some(hip)) = (
            self.mid(kp::LEFT_SHOULDER, kp::RIGHT_SHOULDER),
            self.mid(kp::LEFT_HIP, kp::RIGHT_HIP),
        ) else {
            return Assessment {
                posture: Posture::Unknown,
                face_visible,
                confidence,
            };
        };

        let (dx, dy) = (hip.0 - sh.0, hip.1 - sh.1);
        let torso = (dx * dx + dy * dy).sqrt();
        if torso < 1e-4 {
            return Assessment {
                posture: Posture::Unknown,
                face_visible,
                confidence,
            };
        }

        // Verticality: how vertical the torso is (1 = upright, 0 = flat). A torso
        // more horizontal than vertical = lying down.
        let verticality = dy.abs() / torso;
        let posture = if verticality < 0.5 {
            Posture::Lying
        } else {
            // Upright torso: standing vs sitting from how far the legs extend below
            // the hips (relative to torso length).
            match self.mid(kp::LEFT_ANKLE, kp::RIGHT_ANKLE) {
                Some(ankle) => {
                    let leg_drop = ankle.1 - hip.1;
                    if leg_drop > torso * 0.8 {
                        Posture::Standing
                    } else if leg_drop < torso * 0.5 {
                        Posture::Sitting
                    } else {
                        // Ambiguous: knees drawn up above the hips => sitting.
                        match self.mid(kp::LEFT_KNEE, kp::RIGHT_KNEE) {
                            Some(knee) if knee.1 < hip.1 => Posture::Sitting,
                            _ => Posture::Standing,
                        }
                    }
                }
                // Upright torso, legs not visible: assume standing.
                None => Posture::Standing,
            }
        };

        Assessment {
            posture,
            face_visible,
            confidence,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a pose from an array of (index, x, y) at full confidence; all other
    /// keypoints are absent (conf 0).
    fn pose(points: &[(usize, f32, f32)]) -> Pose {
        let mut kpts = [Keypoint::default(); NUM_KEYPOINTS];
        for &(i, x, y) in points {
            kpts[i] = Keypoint { x, y, conf: 1.0 };
        }
        Pose { kpts }
    }

    #[test]
    fn standing_person() {
        // Shoulders high, hips mid, ankles low, all roughly vertical.
        let p = pose(&[
            (kp::LEFT_SHOULDER, 0.48, 0.2),
            (kp::RIGHT_SHOULDER, 0.52, 0.2),
            (kp::LEFT_HIP, 0.48, 0.5),
            (kp::RIGHT_HIP, 0.52, 0.5),
            (kp::LEFT_ANKLE, 0.48, 0.9),
            (kp::RIGHT_ANKLE, 0.52, 0.9),
        ]);
        assert_eq!(p.classify().posture, Posture::Standing);
    }

    #[test]
    fn sitting_person() {
        // Upright torso but legs barely extend below the hips (folded).
        let p = pose(&[
            (kp::LEFT_SHOULDER, 0.48, 0.3),
            (kp::RIGHT_SHOULDER, 0.52, 0.3),
            (kp::LEFT_HIP, 0.48, 0.55),
            (kp::RIGHT_HIP, 0.52, 0.55),
            (kp::LEFT_ANKLE, 0.48, 0.6),
            (kp::RIGHT_ANKLE, 0.52, 0.6),
        ]);
        assert_eq!(p.classify().posture, Posture::Sitting);
    }

    #[test]
    fn lying_person() {
        // Torso is horizontal (shoulders and hips at the same height).
        let p = pose(&[
            (kp::LEFT_SHOULDER, 0.3, 0.5),
            (kp::RIGHT_SHOULDER, 0.3, 0.54),
            (kp::LEFT_HIP, 0.6, 0.5),
            (kp::RIGHT_HIP, 0.6, 0.54),
        ]);
        assert_eq!(p.classify().posture, Posture::Lying);
    }

    #[test]
    fn unknown_without_torso() {
        // Only a face, no shoulders/hips -> can't judge posture.
        let p = pose(&[(kp::NOSE, 0.5, 0.5)]);
        assert_eq!(p.classify().posture, Posture::Unknown);
    }

    #[test]
    fn face_visibility() {
        let with_face = pose(&[(kp::NOSE, 0.5, 0.3)]);
        assert!(with_face.face_visible());
        let eyes = pose(&[(kp::LEFT_EYE, 0.48, 0.3), (kp::RIGHT_EYE, 0.52, 0.3)]);
        assert!(eyes.face_visible());
        // Body but no face keypoints -> covered-face candidate.
        let no_face = pose(&[
            (kp::LEFT_SHOULDER, 0.4, 0.5),
            (kp::RIGHT_SHOULDER, 0.6, 0.5),
            (kp::LEFT_HIP, 0.4, 0.7),
            (kp::RIGHT_HIP, 0.6, 0.7),
        ]);
        assert!(!no_face.face_visible());
    }

    #[test]
    fn confidence_tracks_observed_keypoints() {
        let p = pose(&[(kp::NOSE, 0.5, 0.3), (kp::LEFT_HIP, 0.5, 0.6)]);
        // 2 of 17 observed.
        assert!((p.confidence() - 2.0 / 17.0).abs() < 1e-6);
    }
}
