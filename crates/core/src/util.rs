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

/// Which configured limit actually caps how far back footage reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionLimit {
    /// `retention_days` binds — footage ages out before the disk budget fills.
    Age,
    /// `retention_gb` binds — the byte cap recycles footage first.
    Disk,
    /// Nothing is capping it yet (no cap set, or no measurable write rate).
    Unbounded,
}

/// How far back recorded footage ACTUALLY reaches, in days.
///
/// `retention_days` is only a ceiling. The byte cap is what usually binds, and
/// on a multi-camera 4K install it binds by orders of magnitude: measured on
/// this repo's own NVR, a configured "7 days" against a 20 GB cap and a
/// 78.8 GB/day write rate is really about **6 hours**. Reporting the configured
/// number — which the weekly health heartbeat did — overstates the truth by
/// ~28x and tells the owner they can go back and find footage that was recycled
/// the same afternoon.
///
/// Returns `None` when nothing bounds it (or the write rate isn't measurable
/// yet), together with which limit is doing the capping.
pub fn retention_horizon(
    retention_days: u32,
    retention_gb: u32,
    write_bytes_per_day: u64,
) -> (Option<f64>, RetentionLimit) {
    let by_days = (retention_days > 0).then_some(retention_days as f64);
    let by_disk = (retention_gb > 0 && write_bytes_per_day > 0)
        .then(|| retention_gb as f64 * 1e9 / write_bytes_per_day as f64);
    match (by_days, by_disk) {
        (Some(a), Some(b)) if b < a => (Some(b), RetentionLimit::Disk),
        (Some(a), Some(_)) => (Some(a), RetentionLimit::Age),
        (Some(a), None) => (Some(a), RetentionLimit::Age),
        (None, Some(b)) => (Some(b), RetentionLimit::Disk),
        (None, None) => (None, RetentionLimit::Unbounded),
    }
}

/// A plain-language span for a retention horizon: "about 6 hours",
/// "about 3 days". Sub-day spans must not round to "0 days" — that is exactly
/// the case the owner most needs stated clearly.
pub fn humanize_days(days: f64) -> String {
    if !days.is_finite() || days <= 0.0 {
        return "no footage".into();
    }
    let unit = |n: f64, word: &str| {
        format!(
            "about {n:.0} {word}{}",
            if (n - 1.0).abs() < 0.5 { "" } else { "s" }
        )
    };
    if days < 1.0 / 24.0 {
        return unit((days * 1440.0).round().max(1.0), "minute");
    }
    if days < 1.0 {
        return unit(days * 24.0, "hour");
    }
    unit(days, "day")
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

    #[test]
    fn retention_horizon_reports_whichever_limit_binds() {
        use super::{humanize_days, retention_horizon, RetentionLimit};
        // The real numbers from this repo's NVR: "7 days" configured, but a
        // 20 GB cap against 78.8 GB/day of 4K is really about six hours.
        let (d, lim) = retention_horizon(7, 20, 78_825_248_126);
        assert_eq!(lim, RetentionLimit::Disk);
        assert_eq!(humanize_days(d.unwrap()), "about 6 hours");

        // A modest write rate lets the age limit bind instead.
        let (d, lim) = retention_horizon(7, 500, 10_000_000_000);
        assert_eq!(lim, RetentionLimit::Age);
        assert_eq!(d.unwrap(), 7.0);

        // Only one cap set.
        assert_eq!(
            retention_horizon(0, 20, 20_000_000_000).1,
            RetentionLimit::Disk
        );
        assert_eq!(
            retention_horizon(3, 0, 20_000_000_000).1,
            RetentionLimit::Age
        );
        // No caps, or no measurable write rate yet.
        assert_eq!(retention_horizon(0, 0, 0).1, RetentionLimit::Unbounded);
        assert!(retention_horizon(0, 0, 0).0.is_none());
        assert_eq!(retention_horizon(0, 20, 0).1, RetentionLimit::Unbounded);
    }

    #[test]
    fn humanize_days_never_rounds_a_short_span_to_zero() {
        use super::humanize_days;
        // The whole point: a sub-day horizon must never read as "0 days".
        assert_eq!(humanize_days(0.25), "about 6 hours");
        // Just under an hour falls to the minutes branch rather than "0 hours".
        assert_eq!(humanize_days(0.04), "about 58 minutes");
        assert_eq!(humanize_days(0.002), "about 3 minutes");
        assert_eq!(humanize_days(2.4), "about 2 days");
        // Singulars read as singulars.
        assert_eq!(humanize_days(1.0), "about 1 day");
        assert_eq!(humanize_days(1.0 / 24.0), "about 1 hour");
        assert_eq!(humanize_days(0.0), "no footage");
        assert_eq!(humanize_days(f64::NAN), "no footage");
    }
}
