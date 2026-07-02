//! Event severity tiers (Wyze "No Big Deal" / Reolink L1–L4 style): every event
//! gets a 1..4 severity so the UI can badge what matters and the push path can
//! be gated with ONE knob (`Settings.notify_min_severity`) instead of per-rule
//! tuning. Pure and deterministic — computed from what the event *is* at emit
//! time (label / identity / gesture), not from history (the anomaly worker keeps
//! its own orthogonal 0..1 unusualness score).
//!
//! Tiers:
//! - 4 critical — life-safety / security-integrity: fall, still water, child
//!   alone, camera tamper, duress, glass break / gunshot / scream / smoke alarm.
//! - 3 high     — action-likely: stranger (unknown face), gesture signals,
//!   loitering, wrong-way, capacity breach, package, watched-zone entry,
//!   siren / car alarm / baby cry.
//! - 2 normal   — routine signal: person, vehicles, crossings, doorbell, bark,
//!   speech, and any label we don't recognize (fail toward notifying).
//! - 1 low      — ambient: animals/wildlife.

/// Severity for an event, from its label plus the identity/gesture slots.
/// `face` is the recognized-face slot (the `?` sentinel = stranger); `gesture`
/// is the recognized hand signal, if any. Duress is a dispatch-time property
/// (a *configured* duress gesture), so callers overlay it themselves — see
/// [`crate::notify::AlarmEvent::severity`].
pub fn severity_for(label: &str, face: Option<&str>, gesture: Option<&str>) -> u8 {
    let l = label.to_ascii_lowercase();

    // Critical: assistive-safety + optical-integrity events, and the audio
    // classes that mean "something broke / someone screamed / fire".
    const CRITICAL: [&str; 4] = ["fall", "still_water", "child_alone", "tamper"];
    const CRITICAL_AUDIO: [&str; 6] = [
        "glass",
        "shatter",
        "gunshot",
        "scream",
        "smoke detector",
        "fire alarm",
    ];
    if CRITICAL.contains(&l.as_str()) || CRITICAL_AUDIO.iter().any(|k| l.contains(k)) {
        return 4;
    }

    // High: an unfamiliar face is the marquee "worth looking at" signal; a hand
    // signal is always deliberate; plus the perimeter/watch analytics and the
    // urgent-but-not-catastrophic audio classes.
    if face == Some(crate::db::UNKNOWN_FACE) || gesture.is_some() {
        return 3;
    }
    const HIGH: [&str; 7] = [
        "wrong_way",
        "loiter",
        "occupancy",
        "package",
        "child_watch",
        "covered_face",
        "zone_enter",
    ];
    const HIGH_AUDIO: [&str; 3] = ["siren", "car alarm", "baby cry"];
    if HIGH.contains(&l.as_str()) || HIGH_AUDIO.iter().any(|k| l.contains(k)) {
        return 3;
    }

    // Low: wildlife/pets seen in passing. (A pet in a watched zone still fires
    // as `zone_enter` = 3 above, so this only demotes ambient sightings.)
    const LOW: [&str; 10] = [
        "cat", "dog", "bird", "horse", "sheep", "cow", "bear", "elephant", "zebra", "giraffe",
    ];
    if LOW.contains(&l.as_str()) {
        return 1;
    }

    // Everything else — person, vehicles, crossing, doorbell, speech, unknown
    // labels — is a normal event. Unknown fails toward 2 (notifying), never 1.
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_and_integrity_are_critical() {
        for l in ["fall", "still_water", "child_alone", "tamper"] {
            assert_eq!(severity_for(l, None, None), 4, "{l}");
        }
        // YAMNet class names (comma-form) hit via substring.
        assert_eq!(severity_for("Smoke detector, smoke alarm", None, None), 4);
        assert_eq!(severity_for("Gunshot, gunfire", None, None), 4);
        assert_eq!(severity_for("Glass", None, None), 4);
        assert_eq!(severity_for("Screaming", None, None), 4);
    }

    #[test]
    fn stranger_and_gesture_are_high() {
        assert_eq!(severity_for("person", Some(crate::db::UNKNOWN_FACE), None), 3);
        assert_eq!(severity_for("person", None, Some("open_palm")), 3);
        // A *recognized* face stays a normal person event.
        assert_eq!(severity_for("person", Some("alice"), None), 2);
    }

    #[test]
    fn analytics_and_urgent_audio_are_high() {
        for l in ["wrong_way", "loiter", "occupancy", "package", "zone_enter"] {
            assert_eq!(severity_for(l, None, None), 3, "{l}");
        }
        assert_eq!(severity_for("Car alarm", None, None), 3);
        assert_eq!(severity_for("Baby cry, infant cry", None, None), 3);
        assert_eq!(severity_for("Siren", None, None), 3);
    }

    #[test]
    fn routine_is_normal_and_wildlife_is_low() {
        for l in ["person", "car", "truck", "crossing", "Doorbell", "Speech"] {
            assert_eq!(severity_for(l, None, None), 2, "{l}");
        }
        // Unknown labels fail toward notifying, not silence.
        assert_eq!(severity_for("some-future-label", None, None), 2);
        for l in ["cat", "dog", "bird", "deer-like-cow"] {
            let want = if l == "deer-like-cow" { 2 } else { 1 }; // exact match only
            assert_eq!(severity_for(l, None, None), want, "{l}");
        }
    }
}
