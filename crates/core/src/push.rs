//! WebPush + per-user email fan-out worker (#68 / P2.11). Watches the
//! `notifications` table (the single sink every alert source already writes to —
//! alarms, camera offline/online, anomaly, digest) and, for each new row,
//! delivers it to the right people:
//!
//! - **PUSH** to each subscribed browser. Anonymous/legacy subscriptions (no
//!   `user_id`) stay unrestricted (today's behaviour); a subscription owned by a
//!   named user delivers only when that user's notify pref allows this rule's
//!   channel AND (for an alarm-tagged notification) the user may see the camera.
//! - **EMAIL** to each user with a notification email set, under the same
//!   per-user pref + camera-visibility gate.
//!
//! Doing both channels here keeps ALL network I/O (SMTP included) OFF the hot
//! detection thread — `notify::fire` only writes the row. A single `last` cursor
//! processes each notification exactly once for both channels.
//!
//! Self-gating: it costs one COUNT (+ one email presence check) per tick and does
//! nothing until a browser subscribes or a user sets an email. The VAPID public
//! key handed to browsers is stable (persisted).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
        crate::util::sleep_interruptible(TICK, &shutdown);
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        // No push subscribers AND no email recipients → keep the cursor at the tip
        // and skip the work (matches the pre-P2.11 self-gating on subscriptions).
        let have_push = db.count_push_subscriptions() > 0;
        let have_email = db.any_user_email();
        if !have_push && !have_email {
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
        let subs = if have_push {
            db.list_push_subscriptions().unwrap_or_default()
        } else {
            Vec::new()
        };
        let email_users = if have_email {
            db.users_with_email().unwrap_or_default()
        } else {
            Vec::new()
        };
        // SMTP config + severity gate (read once per batch); email delivery is
        // skipped when SMTP isn't configured. `smtp` borrows `settings`, so keep
        // both alive here.
        let settings = db.settings();
        let smtp = crate::notify::smtp_cfg(&settings);
        let min_sev = settings.notify_min_severity;

        'batch: for n in &news {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            last = last.max(n.id);
            // rule_id 0 (or NULL for a system notification) resolves against the
            // user's default pref row.
            let rule_id = n.rule_id.unwrap_or(0);
            // `notify_min_severity` quiets the HUMAN channels (push + email) below
            // the bar. A system notification has no severity (NULL ⇒ always
            // delivered); an alarm's duress rides severity 4 ⇒ always passes. The
            // in-app notification ROW is written regardless — only DELIVERY is
            // gated here.
            let human_ok = severity_allows(n.severity, min_sev);

            // --- PUSH ---------------------------------------------------------
            if human_ok {
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
                    // Anonymous/legacy sub → unrestricted (preserves today's
                    // fan-out-to-everyone; correct only for the genuine
                    // loopback/single-admin, which has no camera scope to leak).
                    // Owned sub → per-user pref + camera gate.
                    let deliver = match sub.user_id {
                        None => true,
                        Some(uid) => {
                            // A system notification (rule_id NULL) isn't governed
                            // by any per-rule/Default pref — the matrix only lists
                            // rules — so it always pushes (still camera-gated,
                            // though system rows carry no camera_id). An alarm is
                            // gated by the user's pref for that rule (or Default).
                            (n.rule_id.is_none() || db.pref_enabled(uid, rule_id, "push"))
                                && n.camera_id
                                    .is_none_or(|cid| db.user_can_see_camera(uid, cid))
                        }
                    };
                    if !deliver {
                        continue;
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

            // --- EMAIL --------------------------------------------------------
            // v0 cut: per-user email is delivered ONLY for alarm-tagged
            // notifications (rule_id set). System notifications (offline/online/
            // anomaly/tamper/digest/backup, rule_id NULL) are NOT emailed per
            // user — that would storm every mailbox on routine activity and can't
            // carry a per-rule pref. They still push (above) and show in the bell.
            if human_ok && n.rule_id.is_some() {
                if let Some(cfg) = &smtp {
                    let subject = clamp(&n.title, 200);
                    let body = n
                        .body
                        .as_deref()
                        .map(|b| clamp(b, 2000))
                        .unwrap_or_else(|| subject.clone());
                    for (uid, addr) in &email_users {
                        if shutdown.load(Ordering::Relaxed) {
                            break 'batch;
                        }
                        let deliver = db.pref_enabled(*uid, rule_id, "email")
                            && n.camera_id
                                .is_none_or(|cid| db.user_can_see_camera(*uid, cid));
                        if !deliver {
                            continue;
                        }
                        crate::notify::email_simple(cfg, addr, &subject, &body);
                    }
                }
            }
        }
    }
}

/// Whether a HUMAN channel (push/email) may deliver a notification under the
/// global `notify_min_severity` gate. A system notification has no severity
/// (NULL ⇒ always delivered); an alarm's severity (1..4, duress = 4) must be at
/// or above the bar. `min` 0/1 = no gate (severities are ≥ 1).
fn severity_allows(severity: Option<i64>, min: u8) -> bool {
    match severity {
        Some(s) => s >= i64::from(min),
        None => true,
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

#[cfg(test)]
mod tests {
    use super::severity_allows;

    #[test]
    fn severity_gate_quiets_human_channels_below_the_bar() {
        // Gate off (min 0/1): every alarm severity delivers.
        assert!(severity_allows(Some(1), 1));
        assert!(severity_allows(Some(2), 0));
        // Below the bar → quiet; at/above → delivers.
        assert!(!severity_allows(Some(2), 3));
        assert!(severity_allows(Some(3), 3));
        assert!(severity_allows(Some(4), 3)); // duress rides severity 4
                                              // System notifications carry no severity ⇒ always delivered (offline /
                                              // anomaly / digest are never quieted by this alarm gate).
        assert!(severity_allows(None, 4));
    }
}
