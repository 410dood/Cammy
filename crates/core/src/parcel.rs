//! Package / parcel monitoring (#69) — the "porch piracy" alert: tell me when a
//! parcel is **delivered** to (and later **taken from**) a watched zone.
//!
//! No new model: a parcel is any detection whose label is in the configured
//! package set (default the COCO carry-item classes `suitcase`/`backpack`/
//! `handbag`, the same proxy Frigate users adopt — and forward-compatible with a
//! future dedicated `package` class, just add the label). The pure
//! [`PackageState`] turns the per-frame "is a parcel sitting in the zone?" signal
//! into exactly two edge events: a parcel that **persists** for `confirm_secs`
//! fires `package` (delivered); a confirmed parcel that then **stays gone** for
//! `gone_secs` fires `package_removed` (taken). Brief detector gaps are tolerated
//! so a flicker neither falsely confirms nor falsely clears.

/// COCO carry-item classes used as the default parcel proxy.
pub const DEFAULT_PACKAGE_LABELS: &[&str] = &["suitcase", "backpack", "handbag"];
/// A parcel must persist this long in the zone before "delivered" fires.
pub const CONFIRM_SECS: i64 = 15;
/// A confirmed parcel must be absent this long before "removed" fires (also the
/// gap we tolerate before giving up on an unconfirmed parcel).
pub const GONE_SECS: i64 = 15;

/// Does `label` count as a parcel? Empty config = the built-in default set.
pub fn matches_package(label: &str, configured: &[String]) -> bool {
    if configured.is_empty() {
        DEFAULT_PACKAGE_LABELS.contains(&label)
    } else {
        configured.iter().any(|l| l == label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageEvent {
    Delivered,
    Removed,
}

/// Per-camera parcel presence tracker. Drive [`update`] once per processed frame
/// with whether a parcel is currently in the zone; it returns an edge event on
/// the delivered/removed transitions and `None` otherwise.
#[derive(Default, Debug)]
pub struct PackageState {
    present: bool,
    /// Start of the current pre-confirm presence streak (None when not building one).
    first_seen: Option<i64>,
    /// Last timestamp a parcel was actually seen in the zone.
    last_seen: Option<i64>,
}

impl PackageState {
    pub fn update(
        &mut self,
        in_zone: bool,
        ts: i64,
        confirm_secs: i64,
        gone_secs: i64,
    ) -> Option<PackageEvent> {
        if in_zone {
            // Require OBSERVED continuity, not merely elapsed wall-clock time: if
            // the last time we actually saw the parcel was more than `gone_secs`
            // ago (e.g. the pipeline skipped this camera on a fetch/inference
            // error, so `update` was never called with in_zone=false), the streak
            // is broken — restart it rather than confirm across an unobserved gap.
            if let Some(prev) = self.last_seen {
                if ts - prev >= gone_secs {
                    self.first_seen = None;
                }
            }
            self.last_seen = Some(ts);
        }
        if !self.present {
            if in_zone {
                let first = *self.first_seen.get_or_insert(ts);
                if ts - first >= confirm_secs {
                    self.present = true;
                    self.first_seen = None;
                    return Some(PackageEvent::Delivered);
                }
            } else if let Some(last) = self.last_seen {
                // Tolerate brief detector gaps while waiting to confirm; only give
                // up on the streak after a genuine absence.
                if ts - last >= gone_secs {
                    self.first_seen = None;
                }
            }
            None
        } else if !in_zone {
            // Confirmed parcel no longer seen: a sustained absence = "taken".
            if let Some(last) = self.last_seen {
                if ts - last >= gone_secs {
                    self.present = false;
                    self.first_seen = None;
                    self.last_seen = None;
                    return Some(PackageEvent::Removed);
                }
            }
            None
        } else {
            None
        }
    }

    #[cfg(test)]
    pub fn is_present(&self) -> bool {
        self.present
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_default_and_configured() {
        assert!(matches_package("suitcase", &[]));
        assert!(matches_package("backpack", &[]));
        assert!(!matches_package("person", &[]));
        // Configured set overrides the default (e.g. a model with a real class).
        let cfg = vec!["package".to_string()];
        assert!(matches_package("package", &cfg));
        assert!(!matches_package("suitcase", &cfg));
    }

    #[test]
    fn delivered_after_continuous_presence() {
        let mut s = PackageState::default();
        // Seen every second; nothing fires until confirm_secs elapse.
        for t in 0..15 {
            assert_eq!(s.update(true, t, 15, 15), None, "t={t}");
        }
        assert_eq!(s.update(true, 15, 15, 15), Some(PackageEvent::Delivered));
        assert!(s.is_present());
        // Doesn't re-fire while it stays present.
        assert_eq!(s.update(true, 16, 15, 15), None);
    }

    #[test]
    fn brief_gap_does_not_reset_confirm() {
        let mut s = PackageState::default();
        for t in 0..=10 {
            s.update(true, t, 15, 15);
        }
        // A 3-second detector gap (< gone_secs) must NOT restart the streak.
        for t in 11..=13 {
            assert_eq!(s.update(false, t, 15, 15), None);
        }
        // Reappears; original first_seen=0 still counts -> delivered at t=15.
        assert_eq!(s.update(true, 14, 15, 15), None);
        assert_eq!(s.update(true, 15, 15, 15), Some(PackageEvent::Delivered));
    }

    #[test]
    fn unobserved_gap_does_not_falsely_confirm() {
        // The pipeline may skip a camera entirely (poll throttle / fetch /
        // inference error), so `update` isn't called with in_zone=false during a
        // gap. A glimpse at t=0 then another at t=30 must NOT confirm by elapsed
        // time alone — only OBSERVED persistence counts.
        let mut s = PackageState::default();
        assert_eq!(s.update(true, 0, 15, 15), None);
        // Big jump with no intervening calls at all:
        assert_eq!(s.update(true, 30, 15, 15), None, "stale streak must reset");
        // A fresh, continuous 15s streak still confirms.
        for t in 31..=45 {
            let r = s.update(true, t, 15, 15);
            if t < 45 {
                assert_eq!(r, None, "t={t}");
            } else {
                assert_eq!(r, Some(PackageEvent::Delivered));
            }
        }
    }

    #[test]
    fn long_gap_before_confirm_gives_up() {
        let mut s = PackageState::default();
        for t in 0..=5 {
            s.update(true, t, 15, 15);
        }
        // Absent for >= gone_secs before confirming -> streak abandoned.
        for t in 6..=21 {
            assert_eq!(s.update(false, t, 15, 15), None);
        }
        // A fresh presence must build a brand-new 15s streak.
        for t in 22..=36 {
            assert_eq!(s.update(true, t, 15, 15), None, "t={t}");
        }
        assert_eq!(s.update(true, 37, 15, 15), Some(PackageEvent::Delivered));
    }

    #[test]
    fn removed_after_sustained_absence() {
        let mut s = PackageState::default();
        for t in 0..=15 {
            s.update(true, t, 15, 15);
        }
        assert!(s.is_present());
        // Brief absence does NOT clear it.
        for t in 16..=29 {
            assert_eq!(s.update(false, t, 15, 15), None, "t={t}");
        }
        // last_seen=15; at t=30, 30-15>=15 -> removed.
        assert_eq!(s.update(false, 30, 15, 15), Some(PackageEvent::Removed));
        assert!(!s.is_present());
    }

    #[test]
    fn flicker_after_delivery_does_not_remove() {
        let mut s = PackageState::default();
        for t in 0..=15 {
            s.update(true, t, 15, 15);
        }
        // Missed a few frames, then seen again — stays present, no removal.
        assert_eq!(s.update(false, 16, 15, 15), None);
        assert_eq!(s.update(false, 17, 15, 15), None);
        assert_eq!(s.update(true, 18, 15, 15), None);
        for t in 19..=32 {
            assert_eq!(s.update(false, t, 15, 15), None, "t={t}");
        }
        // last_seen was refreshed to 18, so removal waits until 18+15=33.
        assert_eq!(s.update(false, 33, 15, 15), Some(PackageEvent::Removed));
    }
}
