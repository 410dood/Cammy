//! B3 — proactive anomaly detection. Periodically scores recent events by how
//! unusual the (camera, label, hour-of-day) combination is relative to each
//! camera's own history, writes the score back onto the event, and raises a
//! notification for clearly-unusual activity. Pure statistics over data already
//! in the database; opt-in, no extra ML.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{Local, TimeZone, Timelike};

use crate::db::Db;

const TICK: Duration = Duration::from_secs(120);
const HISTORY_SECS: i64 = 30 * 86_400; // 30-day baseline
const RECENT_SECS: i64 = 600; // only score events from the last ~10 min
const MIN_HISTORY: usize = 20; // need a baseline before judging anything
const NOTIFY_THRESHOLD: f32 = 0.8;

pub fn run(db: Db, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::Relaxed) {
        if db.settings().anomaly_detection {
            if let Err(e) = score_recent(&db) {
                tracing::debug!("anomaly scoring: {e:#}");
            }
        }
        crate::util::sleep_interruptible(TICK, &shutdown);
    }
}

fn score_recent(db: &Db) -> anyhow::Result<()> {
    let now = Local::now().timestamp();
    let hist = db.list_events(
        None,
        None,
        None,
        None,
        Some(now - HISTORY_SECS),
        None,
        false,
        50_000,
    )?;
    if hist.len() < MIN_HISTORY {
        return Ok(());
    }

    // Per (camera_id, label) hour-of-day histogram across the baseline window.
    let mut bins: HashMap<(i64, String), [u32; 24]> = HashMap::new();
    for e in &hist {
        if let Some(dt) = Local.timestamp_opt(e.ts, 0).single() {
            bins.entry((e.camera_id, e.label.clone()))
                .or_insert([0u32; 24])[dt.hour() as usize] += 1;
        }
    }

    for e in &hist {
        if e.anomaly_score.is_some() || now - e.ts > RECENT_SECS {
            continue;
        }
        let dt = match Local.timestamp_opt(e.ts, 0).single() {
            Some(d) => d,
            None => continue,
        };
        let hour = dt.hour() as usize;
        let score = match bins.get(&(e.camera_id, e.label.clone())) {
            None => 0.7, // never seen this object on this camera
            Some(b) => {
                let total: u32 = b.iter().sum();
                let avg = total as f32 / 24.0;
                if total < 8 {
                    0.55 // rarely-seen combination overall
                } else {
                    // An unusually quiet hour for this camera/label means activity
                    // now is surprising. (The event counts itself once, so a truly
                    // novel hour still scores high.)
                    (1.0 - b[hour] as f32 / (avg + 1.0)).clamp(0.0, 0.95)
                }
            }
        };
        db.set_event_anomaly(e.id, score)?;
        if score >= NOTIFY_THRESHOLD {
            let title = format!("Unusual activity: {} at {}", e.label, e.camera);
            let _ = db.add_notification(now, "anomaly", &title, e.caption.as_deref(), Some(e.id));
        }
    }
    Ok(())
}
