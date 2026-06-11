//! Alarm action dispatch, shared by the video pipeline and the audio worker.
//! Actions: webhook (JSON POST), mqtt (custom topic), ntfy (phone push with
//! the snapshot attached — the self-hoster standard; works with ntfy.sh or a
//! private ntfy server, no account required).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::db::AlarmRule;
use crate::mqtt::EventMsg;

/// Shared per-rule last-fired clock (rule id → unix seconds). Lives in memory
/// and is consulted by every dispatch site (video pipeline, audio worker, the
/// gesture endpoint) so a rule's cooldown holds across cameras, detections and
/// ticks without a DB round-trip per event.
pub type AlarmThrottle = Arc<Mutex<HashMap<i64, i64>>>;

pub struct AlarmEvent<'a> {
    pub event_id: i64,
    pub camera: &'a str,
    pub label: &'a str,
    pub score: f32,
    pub ts: i64,
    /// Web path, e.g. "/api/snapshots/x.jpg" (for payload consumers).
    pub snapshot_url: &'a str,
    /// Local file, for attaching the image to push notifications.
    pub snapshot_path: Option<&'a Path>,
    pub face: Option<&'a str>,
    pub plate: Option<&'a str>,
    pub gesture: Option<&'a str>,
    /// Public base URL (e.g. "https://nvr.example.com"); when set, pushes carry
    /// tap-through "View clip" / "Snapshot" action links. Empty = no links.
    pub base_url: &'a str,
}

/// Is the rule clear to fire right now? False when snoozed or still inside its
/// per-rule cooldown. On a `true` result the rule is stamped as fired `now`, so
/// callers should fire exactly when this returns true (no double-firing).
pub fn ready(rule: &AlarmRule, throttle: &AlarmThrottle, now: i64) -> bool {
    if rule.snooze_until > now {
        return false;
    }
    let mut map = throttle.lock().expect("alarm throttle poisoned");
    if rule.cooldown_secs > 0 {
        if let Some(&last) = map.get(&rule.id) {
            if now - last < rule.cooldown_secs {
                return false;
            }
        }
    }
    map.insert(rule.id, now);
    true
}

/// Fire one matched rule's action. Failures are logged and swallowed —
/// notification problems must never stall detection.
pub fn fire(rule: &AlarmRule, ev: &AlarmEvent, mqtt_tx: &std::sync::mpsc::Sender<EventMsg>) {
    tracing::info!(rule = %rule.name, event = ev.event_id, "alarm triggered");
    match rule.action.as_str() {
        "webhook" => webhook(&rule.target, ev),
        "mqtt" => {
            let _ = mqtt_tx.send(EventMsg {
                event_id: ev.event_id,
                camera: ev.camera.to_string(),
                label: ev.label.to_string(),
                score: ev.score,
                ts: ev.ts,
                snapshot: ev.snapshot_url.to_string(),
                topic: Some(format!("alarms/{}", rule.target)),
            });
        }
        "ntfy" => ntfy(&rule.target, &rule.name, rule.priority, ev),
        other => tracing::warn!("unknown alarm action {other:?}"),
    }
}

/// Plain-text ntfy push (no attachment) — used for camera health alerts.
pub fn ntfy_text(url: &str, title: &str, message: &str, tags: &str) {
    if let Err(e) = ureq::post(url)
        .timeout(Duration::from_secs(10))
        .set("X-Title", title)
        .set("X-Tags", tags)
        .send_string(message)
    {
        tracing::debug!("ntfy push failed: {e}");
    }
}

fn webhook(url: &str, ev: &AlarmEvent) {
    let payload = serde_json::json!({
        "type": "alarm",
        "event_id": ev.event_id,
        "camera": ev.camera,
        "label": ev.label,
        "score": ev.score,
        "ts": ev.ts,
        "snapshot": ev.snapshot_url,
        "face": ev.face,
        "plate": ev.plate,
        "gesture": ev.gesture,
    });
    if let Err(e) = ureq::post(url)
        .timeout(Duration::from_secs(3))
        .send_json(payload)
    {
        tracing::debug!("alarm webhook failed: {e}");
    }
}

/// ntfy push: PUT with the snapshot attached when available, plain POST
/// otherwise. Title/extras travel as headers per the ntfy protocol. When a
/// public base URL is known the push carries tap-through "View clip" /
/// "Snapshot" actions, and `priority` (1..5) maps to ntfy's X-Priority.
fn ntfy(url: &str, rule_name: &str, priority: u8, ev: &AlarmEvent) {
    let mut detail = format!("{} ({:.0}%) on {}", ev.label, ev.score * 100.0, ev.camera);
    if let Some(f) = ev.face {
        detail.push_str(&format!(" — {f}"));
    }
    if let Some(p) = ev.plate {
        detail.push_str(&format!(" — plate {p}"));
    }
    if let Some(g) = ev.gesture {
        detail.push_str(&format!(" — ✋ {g}"));
    }

    // Tap-through actions when we can build absolute links.
    let actions = if ev.base_url.is_empty() {
        None
    } else {
        let base = ev.base_url.trim_end_matches('/');
        Some(format!(
            "view, View clip, {base}/api/events/{}/clip; view, Snapshot, {base}{}",
            ev.event_id, ev.snapshot_url
        ))
    };

    let apply = |req: ureq::Request| {
        let mut req = req
            .set("X-Title", rule_name)
            .set("X-Tags", "rotating_light");
        if (1..=5).contains(&priority) {
            req = req.set("X-Priority", &priority.to_string());
        }
        if let Some(a) = &actions {
            req = req.set("X-Actions", a);
        }
        req
    };

    let snapshot = ev.snapshot_path.and_then(|p| std::fs::read(p).ok());
    let result = match snapshot {
        Some(bytes) => apply(ureq::put(url).timeout(Duration::from_secs(10)))
            .set("X-Message", &detail)
            .set("Filename", "snapshot.jpg")
            .send_bytes(&bytes),
        None => apply(ureq::post(url).timeout(Duration::from_secs(10))).send_string(&detail),
    };
    if let Err(e) = result {
        tracing::debug!("ntfy push failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: i64, cooldown: i64, snooze: i64) -> AlarmRule {
        AlarmRule {
            id,
            name: "r".into(),
            enabled: true,
            camera_id: None,
            label: None,
            face_like: None,
            plate_like: None,
            gesture_like: None,
            min_score: 0.0,
            action: "ntfy".into(),
            target: "t".into(),
            days: vec![],
            start_hhmm: None,
            end_hhmm: None,
            cooldown_secs: cooldown,
            priority: 0,
            snooze_until: snooze,
            created_ts: 0,
        }
    }

    #[test]
    fn cooldown_suppresses_within_window() {
        let throttle: AlarmThrottle = Default::default();
        let r = rule(1, 60, 0);
        assert!(ready(&r, &throttle, 1000)); // first fire
        assert!(!ready(&r, &throttle, 1030)); // 30s < 60s cooldown
        assert!(ready(&r, &throttle, 1061)); // 61s ≥ cooldown
    }

    #[test]
    fn no_cooldown_always_ready() {
        let throttle: AlarmThrottle = Default::default();
        let r = rule(2, 0, 0);
        assert!(ready(&r, &throttle, 100));
        assert!(ready(&r, &throttle, 100));
    }

    #[test]
    fn snooze_blocks_until_expiry() {
        let throttle: AlarmThrottle = Default::default();
        let r = rule(3, 0, 5000);
        assert!(!ready(&r, &throttle, 4999)); // still snoozed
        assert!(ready(&r, &throttle, 5001)); // snooze elapsed
    }
}
