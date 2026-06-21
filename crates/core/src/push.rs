//! WebPush fan-out worker (#68). Watches the `notifications` table (the single
//! sink every alert source already writes to — alarms, camera offline/online,
//! anomaly, digest) and pushes each new row to every subscribed browser via
//! `webpush`. Decoupling network I/O from `add_notification` this way means no
//! caller has to know about push, and an expired endpoint is pruned on the spot.
//!
//! Self-gating: it costs one COUNT per tick and does nothing until a browser
//! subscribes. The VAPID public key handed to browsers is stable (persisted).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::db::Db;
use crate::webpush::{self, SendError};

const TICK: Duration = Duration::from_secs(5);
const TTL: u32 = 24 * 3600;
const BATCH: u32 = 50;

pub fn run(db: Db, shutdown: Arc<AtomicBool>) {
    // Start at the current tip so a freshly-subscribed browser isn't flooded
    // with the entire backlog.
    let mut last = db.max_notification_id();
    let keys = match webpush::vapid_keys(&db) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!("WebPush disabled (no VAPID key): {e:#}");
            return;
        }
    };

    while !shutdown.load(Ordering::Relaxed) {
        sleep_interruptible(TICK, &shutdown);
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        // No subscribers → keep the cursor at the tip and skip the work.
        if db.count_push_subscriptions() == 0 {
            last = db.max_notification_id();
            continue;
        }
        let news = match db.notifications_after(last, BATCH) {
            Ok(n) => n,
            Err(e) => {
                tracing::debug!("push: read notifications: {e:#}");
                continue;
            }
        };
        if news.is_empty() {
            continue;
        }
        let subs = db.list_push_subscriptions().unwrap_or_default();
        'batch: for n in &news {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            last = last.max(n.id);
            // Clamp the human-readable text so a long body (e.g. a GenAI anomaly
            // caption) can't exceed the single-record aes128gcm size budget.
            let payload = json!({
                "title": clamp(&n.title, 200),
                "body": n.body.as_deref().map(|b| clamp(b, 600)),
                "kind": n.kind,
                "event_id": n.event_id,
                "id": n.id,
                "ts": n.ts,
            })
            .to_string();
            for sub in &subs {
                // Stay responsive to shutdown even mid-fan-out (each send can
                // block up to its timeout).
                if shutdown.load(Ordering::Relaxed) {
                    break 'batch;
                }
                match webpush::send(&keys, sub, payload.as_bytes(), TTL) {
                    Ok(()) => {}
                    Err(SendError::Gone) => {
                        let _ = db.delete_push_subscription(&sub.endpoint);
                        tracing::debug!("push: pruned expired subscription");
                    }
                    Err(SendError::Other(msg)) => {
                        tracing::debug!("push: send failed: {msg}");
                    }
                }
            }
        }
    }
}

fn sleep_interruptible(dur: Duration, shutdown: &Arc<AtomicBool>) {
    let start = Instant::now();
    while start.elapsed() < dur && !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Truncate to at most `max` chars (not bytes — never split a UTF-8 boundary),
/// appending an ellipsis when shortened.
fn clamp(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}
