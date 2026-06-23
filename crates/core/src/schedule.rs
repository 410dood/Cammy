//! Auto-arm / disarm scheduler (Ring/Arlo-style "modes" automation). Flips the
//! system `arm_mode` on a recurring day + time schedule — e.g. **Away** at 08:00
//! on weekdays, **Home** at 18:00, **Disarmed** all weekend. As well as the
//! convenience, it's a false-alarm reducer: don't push every family member
//! walking around during the day, but arm fully overnight.
//!
//! Opt-in by adding entries to `Settings.arm_schedule` (empty = the worker idles).
//! Config is re-read every tick, so edits take effect without a restart. The mode
//! is written to the authoritative `arm_mode` KV row (the same one the Settings
//! page and `/api/arm` write), so there's no read-modify-write race with a manual
//! change.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{Datelike, Local, Timelike};

use crate::db::Db;

/// Wake often enough to never miss the target minute, but cheaply (a string
/// compare over a handful of entries).
const TICK: Duration = Duration::from_secs(20);

pub fn run(db: Db, shutdown: Arc<AtomicBool>) {
    // The minute-of-epoch we last applied a change in, so a multi-tick minute
    // can't fight a manual mode change the user makes seconds later.
    let mut applied_minute: i64 = -1;

    while !shutdown.load(Ordering::Relaxed) {
        let s = db.settings();
        if !s.arm_schedule.is_empty() {
            let now = Local::now();
            let minute = now.timestamp() / 60;
            let weekday = now.weekday().num_days_from_sunday() as u8; // 0 = Sunday
            let hhmm = format!("{:02}:{:02}", now.hour(), now.minute());

            if applied_minute != minute {
                for e in &s.arm_schedule {
                    if entry_due(e, weekday, &hhmm) {
                        // Idempotent: only act (and notify) when it actually changes
                        // the live mode, so re-reading the same schedule is a no-op.
                        if s.arm_mode != e.mode && db.set_kv("arm_mode", &e.mode).is_ok() {
                            tracing::info!(mode = %e.mode, at = %hhmm, "auto-arm schedule applied");
                            let _ = db.add_notification(
                                now.timestamp(),
                                "schedule",
                                &format!("System set to {}", mode_label(&e.mode)),
                                Some(&format!("Scheduled mode change at {hhmm}")),
                                None,
                            );
                        }
                        applied_minute = minute;
                        break;
                    }
                }
            }
        }
        crate::util::sleep_interruptible(TICK, &shutdown);
    }
}

/// Is this schedule entry due right now? A valid mode, the weekday is in the
/// entry's day set (empty = every day), and the local time matches `HH:MM`.
fn entry_due(e: &crate::db::ArmScheduleEntry, weekday: u8, hhmm: &str) -> bool {
    matches!(e.mode.as_str(), "home" | "away" | "disarmed")
        && (e.days.is_empty() || e.days.contains(&weekday))
        && e.hhmm == hhmm
}

fn mode_label(mode: &str) -> &str {
    match mode {
        "home" => "Home",
        "away" => "Away (armed)",
        "disarmed" => "Disarmed",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::ArmScheduleEntry;

    fn entry(days: Vec<u8>, hhmm: &str, mode: &str) -> ArmScheduleEntry {
        ArmScheduleEntry {
            days,
            hhmm: hhmm.into(),
            mode: mode.into(),
        }
    }

    #[test]
    fn due_on_matching_day_and_time() {
        let e = entry(vec![1, 2, 3, 4, 5], "08:00", "away");
        assert!(entry_due(&e, 3, "08:00")); // Wednesday 08:00
        assert!(!entry_due(&e, 0, "08:00")); // Sunday excluded
        assert!(!entry_due(&e, 3, "08:01")); // wrong minute
    }

    #[test]
    fn empty_days_means_every_day() {
        let e = entry(vec![], "22:00", "home");
        assert!(entry_due(&e, 0, "22:00"));
        assert!(entry_due(&e, 6, "22:00"));
    }

    #[test]
    fn invalid_mode_never_due() {
        let e = entry(vec![], "08:00", "bogus");
        assert!(!entry_due(&e, 1, "08:00"));
    }
}
