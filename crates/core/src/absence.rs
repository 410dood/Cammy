//! Absence / inactivity watch (Verkada-style "inactivity detection", the
//! aging-in-place & pet primitive): a camera with `DetectConfig.absence_hours`
//! set raises a notification + phone push when it has seen NO person/pet event
//! for that long — "nobody has moved in the kitchen since last night".
//!
//! Edge-triggered like the health/tamper/offsite latches: one alert per quiet
//! spell, cleared (with a recovery notification) by the next sighting, so a
//! long absence can't spam the bell every tick. ASSISTIVE only — the absence
//! of detections is not proof of the absence of activity (angle, lighting,
//! model misses); framed accordingly in every message.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::db::Db;

const TICK: Duration = Duration::from_secs(300);
/// Floor so a typo like 0.001 can't turn the watch into a notification loop.
const MIN_HOURS: f32 = 0.25;

/// Pure decision for one camera at one tick: currently-quiet given the last
/// presence timestamp (None = no event ever recorded → treat the camera's
/// creation as the spell start via `since_fallback`).
fn is_quiet(now: i64, last_presence: Option<i64>, since_fallback: i64, hours: f32) -> bool {
    let hours = hours.max(MIN_HOURS);
    let since = last_presence.unwrap_or(since_fallback);
    now - since >= (hours * 3600.0) as i64
}

pub fn run(db: Db, shutdown: Arc<AtomicBool>) {
    // Cameras currently latched "quiet" — in-memory like the alarm throttle;
    // a restart just re-notifies once if the spell is still ongoing.
    let mut latched: HashSet<i64> = HashSet::new();
    while !shutdown.load(Ordering::Relaxed) {
        if let Err(e) = tick(&db, &mut latched) {
            tracing::debug!("absence watch: {e:#}");
        }
        crate::util::sleep_interruptible(TICK, &shutdown);
    }
}

fn tick(db: &Db, latched: &mut HashSet<i64>) -> anyhow::Result<()> {
    let now = chrono::Local::now().timestamp();
    let cameras = db.list_cameras()?;
    let settings = db.settings();
    // Drop latches for cameras that were deleted or had the watch turned off.
    latched.retain(|id| {
        cameras
            .iter()
            .any(|c| c.id == *id && c.enabled && c.detect_config.absence_hours.is_some())
    });
    for cam in cameras.iter().filter(|c| c.enabled) {
        let Some(hours) = cam.detect_config.absence_hours.filter(|h| *h > 0.0) else {
            continue;
        };
        let last = db.last_presence_ts(cam.id)?;
        let quiet = is_quiet(now, last, cam.created_ts, hours);
        if quiet && !latched.contains(&cam.id) {
            latched.insert(cam.id);
            let title = format!("No activity on {}", cam.name);
            let body = format!(
                "No person or pet has been detected on {} for over {:.1} hours. \
                 Assistive check only — verify directly if this is unexpected.",
                cam.name, hours
            );
            let _ = db.add_notification(now, "absence", &title, Some(&body), None);
            let url = settings.health_ntfy_url.trim();
            if !url.is_empty() {
                crate::notify::ntfy_text(url, &title, &body, "hourglass_flowing_sand");
            }
            tracing::info!(camera = %cam.name, hours, "absence watch: quiet spell alerted");
        } else if !quiet && latched.remove(&cam.id) {
            let title = format!("Activity again on {}", cam.name);
            let _ = db.add_notification(
                now,
                "absence_cleared",
                &title,
                Some("A person or pet was seen — the inactivity alert is cleared."),
                None,
            );
            tracing::info!(camera = %cam.name, "absence watch: cleared");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_decision_edges() {
        let h = 12.0;
        // Seen 1 hour ago → not quiet.
        assert!(!is_quiet(100_000, Some(100_000 - 3600), 0, h));
        // Seen just past the threshold → quiet.
        assert!(is_quiet(100_000, Some(100_000 - 13 * 3600), 0, h));
        // Never seen anything: falls back to camera creation time.
        assert!(is_quiet(100_000, None, 100_000 - 13 * 3600, h));
        assert!(!is_quiet(100_000, None, 100_000 - 3600, h));
        // Absurdly small hours are floored so it can't alert-loop.
        assert!(!is_quiet(100_000, Some(100_000 - 60), 0, 0.001));
    }
}
