//! Small shared helpers consolidated so individual features don't each re-roll
//! their own copy: lowercase hex encoding (SigV4 / TOTP / auth tokens / PTZ) and
//! the interruptible worker sleep every periodic background thread uses.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Lowercase hex-encode `bytes` into one pre-allocated `String` (no per-byte
/// allocation, unlike the `.map(|b| format!("{b:02x}")).collect()` it replaces).
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Humanize a raw event label (`"camera_tripwire"`, `"still_water"`) for
/// display in backend-generated text — daily digests, the anomaly-alert title,
/// and the push/email that carry them. The stored label is never changed
/// (alarm rules and the API match on it verbatim); only rendering. Mirrors the
/// web `prettyLabel` overrides so the same event reads identically everywhere.
pub fn pretty_label(label: &str) -> String {
    match label {
        "crossing" => "line crossing".to_string(),
        "loiter" => "loitering".to_string(),
        "occupancy" => "occupancy limit".to_string(),
        "still_water" => "motionless in water".to_string(),
        "zone_open" => "zone opened".to_string(),
        "zone_closed" => "zone closed".to_string(),
        other => other.replace('_', " "),
    }
}

/// Sleep up to `dur`, waking within ~200 ms once `shutdown` is set so a periodic
/// background worker tears down promptly instead of blocking a full tick.
pub fn sleep_interruptible(dur: Duration, shutdown: &Arc<AtomicBool>) {
    let start = Instant::now();
    while start.elapsed() < dur && !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_lowercase_zero_padded() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff, 0xab]), "000fffab");
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x01, 0x23, 0x45, 0x67, 0x89]), "0123456789");
    }

    #[test]
    fn pretty_label_humanizes() {
        // Generic underscore→space.
        assert_eq!(pretty_label("camera_tripwire"), "camera tripwire");
        assert_eq!(pretty_label("package_removed"), "package removed");
        assert_eq!(pretty_label("person"), "person");
        // Curated overrides where a bare swap reads wrong.
        assert_eq!(pretty_label("crossing"), "line crossing");
        assert_eq!(pretty_label("still_water"), "motionless in water");
        assert_eq!(pretty_label("zone_open"), "zone opened");
    }
}
