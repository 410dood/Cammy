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
}
