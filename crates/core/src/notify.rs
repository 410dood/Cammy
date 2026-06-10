//! Alarm action dispatch, shared by the video pipeline and the audio worker.
//! Actions: webhook (JSON POST), mqtt (custom topic), ntfy (phone push with
//! the snapshot attached — the self-hoster standard; works with ntfy.sh or a
//! private ntfy server, no account required).

use std::path::Path;
use std::time::Duration;

use crate::db::AlarmRule;
use crate::mqtt::EventMsg;

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
        "ntfy" => ntfy(&rule.target, &rule.name, ev),
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
    });
    if let Err(e) = ureq::post(url)
        .timeout(Duration::from_secs(3))
        .send_json(payload)
    {
        tracing::debug!("alarm webhook failed: {e}");
    }
}

/// ntfy push: PUT with the snapshot attached when available, plain POST
/// otherwise. Title/extras travel as headers per the ntfy protocol.
fn ntfy(url: &str, rule_name: &str, ev: &AlarmEvent) {
    let mut detail = format!("{} ({:.0}%) on {}", ev.label, ev.score * 100.0, ev.camera);
    if let Some(f) = ev.face {
        detail.push_str(&format!(" — {f}"));
    }
    if let Some(p) = ev.plate {
        detail.push_str(&format!(" — plate {p}"));
    }

    let snapshot = ev.snapshot_path.and_then(|p| std::fs::read(p).ok());
    let result = match snapshot {
        Some(bytes) => ureq::put(url)
            .timeout(Duration::from_secs(10))
            .set("X-Title", rule_name)
            .set("X-Message", &detail)
            .set("X-Tags", "rotating_light")
            .set("Filename", "snapshot.jpg")
            .send_bytes(&bytes),
        None => ureq::post(url)
            .timeout(Duration::from_secs(10))
            .set("X-Title", rule_name)
            .set("X-Tags", "rotating_light")
            .send_string(&detail),
    };
    if let Err(e) = result {
        tracing::debug!("ntfy push failed: {e}");
    }
}
