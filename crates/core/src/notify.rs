//! Alarm action dispatch, shared by the video pipeline and the audio worker.
//! Actions: webhook (JSON POST), mqtt (custom topic), ntfy (phone push with
//! the snapshot attached — the self-hoster standard; works with ntfy.sh or a
//! private ntfy server, no account required).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::db::{Action, AlarmRule};
use crate::mqtt::EventMsg;

/// Shared per-rule last-fired clock (rule id → unix seconds). Lives in memory
/// and is consulted by every dispatch site (video pipeline, audio worker, the
/// gesture endpoint) so a rule's cooldown holds across cameras, detections and
/// ticks without a DB round-trip per event.
pub type AlarmThrottle = Arc<Mutex<HashMap<i64, i64>>>;

/// SMTP config for the "email" alarm action, borrowed from Settings at the
/// dispatch site. `to` is the default recipient(s) (comma-separated); an action
/// can override it with its own `target`.
pub struct SmtpConfig<'a> {
    pub url: &'a str,
    pub user: &'a str,
    pub pass: &'a str,
    pub from: &'a str,
    pub to: &'a str,
}

/// Borrow an SmtpConfig from Settings when SMTP is configured (URL set), for the
/// `smtp` field of an AlarmEvent at each dispatch site. `None` = email off.
pub fn smtp_cfg(s: &crate::db::Settings) -> Option<SmtpConfig<'_>> {
    (!s.smtp_url.trim().is_empty()).then(|| SmtpConfig {
        url: &s.smtp_url,
        user: &s.smtp_user,
        pass: &s.smtp_pass,
        from: &s.smtp_from,
        to: &s.smtp_to,
    })
}

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
    /// Speech-to-text transcript (for spoken-keyword alarms) — carried in the
    /// webhook payload and shown in the push so the receiver sees what was said.
    pub transcript: Option<&'a str>,
    /// Estimated ground speed (km/h) for a calibrated traffic-analytics event;
    /// `None` for events without a ground calibration. Exposed as `{{speed}}`.
    pub speed: Option<f32>,
    /// Public base URL (e.g. "https://nvr.example.com"); when set, pushes carry
    /// tap-through "View clip" / "Snapshot" action links. Empty = no links.
    pub base_url: &'a str,
    /// Optional webhook body template ({{placeholder}} form). Empty = default
    /// detection JSON.
    pub webhook_template: &'a str,
    /// SMTP config for an "email" action; `None` = email not configured.
    pub smtp: Option<SmtpConfig<'a>>,
    /// Duress/panic event: force max push urgency and a distinct alarm tag.
    pub duress: bool,
}

/// JSON-escape a value so substituting it into a JSON template stays valid.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Render a webhook body template, substituting `{{key}}` placeholders with the
/// event's fields (JSON-escaped). Unknown placeholders are left untouched.
pub fn render_template(tpl: &str, ev: &AlarmEvent) -> String {
    let fields: [(&str, String); 11] = [
        ("event_id", ev.event_id.to_string()),
        ("camera", json_escape(ev.camera)),
        ("label", json_escape(ev.label)),
        ("score", format!("{:.3}", ev.score)),
        ("ts", ev.ts.to_string()),
        ("snapshot", json_escape(ev.snapshot_url)),
        ("face", json_escape(ev.face.unwrap_or(""))),
        ("plate", json_escape(ev.plate.unwrap_or(""))),
        ("gesture", json_escape(ev.gesture.unwrap_or(""))),
        ("transcript", json_escape(ev.transcript.unwrap_or(""))),
        (
            "speed",
            ev.speed.map(|s| format!("{s:.0}")).unwrap_or_default(),
        ),
    ];
    let mut out = tpl.to_string();
    for (k, v) in &fields {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
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

/// Whether a rule is armed in the current system security mode (UniFi-style
/// Home/Away/Disarmed). An empty `modes` list means "armed in every *armed*
/// mode" (home + away) but suppressed while the system is "disarmed". A rule
/// that explicitly lists "disarmed" still fires while disarmed — a panic rule.
/// Callers OR this with the per-event `duress` flag so a panic always fires.
pub fn armed_in_mode(modes: &[String], arm_mode: &str) -> bool {
    if arm_mode == "disarmed" {
        modes.iter().any(|m| m == "disarmed")
    } else {
        modes.is_empty() || modes.iter().any(|m| m == arm_mode)
    }
}

/// Fire a matched rule's actions — a "scene" can be several at once (push AND
/// webhook AND …). Failures are logged and swallowed; notification problems
/// must never stall detection. `effective_actions` falls back to the legacy
/// single action for pre-scenes rules.
pub fn fire(rule: &AlarmRule, ev: &AlarmEvent, mqtt_tx: &std::sync::mpsc::Sender<EventMsg>) {
    tracing::info!(rule = %rule.name, event = ev.event_id, "alarm triggered");
    for action in rule.effective_actions() {
        fire_action(&action, &rule.name, ev, mqtt_tx);
    }
}

fn fire_action(
    action: &Action,
    rule_name: &str,
    ev: &AlarmEvent,
    mqtt_tx: &std::sync::mpsc::Sender<EventMsg>,
) {
    match action.kind.as_str() {
        "webhook" => webhook(&action.target, ev),
        "mqtt" => {
            let _ = mqtt_tx.send(EventMsg {
                event_id: ev.event_id,
                camera: ev.camera.to_string(),
                label: ev.label.to_string(),
                score: ev.score,
                ts: ev.ts,
                snapshot: ev.snapshot_url.to_string(),
                topic: Some(format!("alarms/{}", action.target)),
            });
        }
        "ntfy" => ntfy(&action.target, rule_name, action.priority, ev),
        "email" => email(&action.target, rule_name, ev),
        other => tracing::warn!("unknown alarm action {other:?}"),
    }
}

/// Email (SMTP) action: send the alarm detail with the snapshot attached.
/// Best-effort and log-and-swallow like every other channel. The recipient is
/// the action's `target` if set, else the configured default `smtp.to`.
fn email(target: &str, rule_name: &str, ev: &AlarmEvent) {
    use lettre::message::{header::ContentType, Attachment, MultiPart, SinglePart};
    use lettre::{Message, Transport};

    let Some(cfg) = &ev.smtp else {
        tracing::warn!("email action skipped: SMTP not configured in Settings");
        return;
    };
    let to_raw = if target.trim().is_empty() {
        cfg.to
    } else {
        target
    };
    if cfg.from.trim().is_empty() || to_raw.trim().is_empty() {
        tracing::warn!("email action skipped: missing from/to address");
        return;
    }
    let from = match cfg.from.trim().parse() {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("email skipped: bad from address {:?}: {e}", cfg.from);
            return;
        }
    };
    let subject = if ev.duress {
        format!("🚨 DURESS — {rule_name}")
    } else {
        format!("Alarm: {rule_name}")
    };
    let mut body = format!("{} ({:.0}%) on {}", ev.label, ev.score * 100.0, ev.camera);
    if let Some(f) = ev.face {
        body.push_str(&format!("\nFace: {f}"));
    }
    if let Some(p) = ev.plate {
        body.push_str(&format!("\nPlate: {p}"));
    }
    if let Some(t) = ev.transcript {
        body.push_str(&format!("\nHeard: \"{t}\""));
    }
    if !ev.base_url.is_empty() {
        let base = ev.base_url.trim_end_matches('/');
        body.push_str(&format!("\n\nClip: {base}/api/events/{}/clip", ev.event_id));
    }

    let mut builder = Message::builder().from(from).subject(subject);
    let mut any_to = false;
    for addr in to_raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match addr.parse() {
            Ok(a) => {
                builder = builder.to(a);
                any_to = true;
            }
            Err(e) => tracing::warn!("email: bad recipient {addr:?}: {e}"),
        }
    }
    if !any_to {
        return;
    }

    let text = SinglePart::plain(body);
    let msg = match ev.snapshot_path.and_then(|p| std::fs::read(p).ok()) {
        Some(bytes) => {
            let att = Attachment::new("snapshot.jpg".to_string())
                .body(bytes, ContentType::parse("image/jpeg").unwrap());
            builder.multipart(MultiPart::mixed().singlepart(text).singlepart(att))
        }
        None => builder.singlepart(text),
    };
    let msg = match msg {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("email build failed: {e}");
            return;
        }
    };
    match build_smtp(cfg) {
        Ok(mailer) => {
            if let Err(e) = mailer.send(&msg) {
                tracing::debug!("email send failed: {e}");
            }
        }
        Err(e) => tracing::warn!("email transport failed: {e}"),
    }
}

/// Build a blocking SMTP transport from the config. URL forms: "smtps://host:465"
/// (implicit TLS), "smtp://host:587" (STARTTLS), or bare "host[:port]" (implicit
/// TLS). Any user:pass@ in the URL is ignored in favor of the explicit creds.
fn build_smtp(cfg: &SmtpConfig) -> Result<lettre::SmtpTransport, lettre::transport::smtp::Error> {
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::SmtpTransport;

    let raw = cfg.url.trim();
    let (starttls, rest) = if let Some(r) = raw.strip_prefix("smtps://") {
        (false, r)
    } else if let Some(r) = raw.strip_prefix("smtp://") {
        (true, r)
    } else {
        (false, raw)
    };
    let hostport = rest.rsplit('@').next().unwrap_or(rest);
    let (host, port) = match hostport.split_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()),
        None => (hostport, None),
    };
    let mut builder = if starttls {
        SmtpTransport::starttls_relay(host)?
    } else {
        SmtpTransport::relay(host)?
    };
    // Bound the send: this runs inline on the detection/audio worker threads, so
    // a hung SMTP server must not stall detection (lettre defaults to 60s).
    builder = builder.timeout(Some(Duration::from_secs(10)));
    if let Some(p) = port {
        builder = builder.port(p);
    }
    if !cfg.user.is_empty() {
        builder = builder.credentials(Credentials::new(cfg.user.to_string(), cfg.pass.to_string()));
    }
    Ok(builder.build())
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
    let result = if ev.webhook_template.is_empty() {
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
            "transcript": ev.transcript,
        });
        ureq::post(url)
            .timeout(Duration::from_secs(3))
            .send_json(payload)
    } else {
        let body = render_template(ev.webhook_template, ev);
        ureq::post(url)
            .timeout(Duration::from_secs(3))
            .set("Content-Type", "application/json")
            .send_string(&body)
    };
    if let Err(e) = result {
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
    if let Some(t) = ev.transcript {
        detail.push_str(&format!(" — 🎙️ \"{t}\""));
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

    // Duress overrides: max urgency, a distinct siren tag, and a flagged title.
    let title = if ev.duress {
        format!("🚨 DURESS — {rule_name}")
    } else {
        rule_name.to_string()
    };
    let (tags, eff_priority) = if ev.duress {
        ("warning,rotating_light,sos", 5)
    } else {
        ("rotating_light", priority)
    };

    let apply = |req: ureq::Request| {
        let mut req = req.set("X-Title", &title).set("X-Tags", tags);
        if (1..=5).contains(&eff_priority) {
            req = req.set("X-Priority", &eff_priority.to_string());
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
            transcript_like: None,
            face_unknown: false,
            zone_like: None,
            confirm_label: None,
            confirm_within_secs: None,
            vlm_prompt: None,
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
            modes: vec![],
            actions: vec![],
        }
    }

    #[test]
    fn arm_modes_gate_dispatch() {
        // Back-compat guard: a legacy empty-modes rule MUST still fire in the
        // default arm mode after an upgrade — i.e. the default is an *armed*
        // mode. If someone changes the default to "disarmed", this fails loudly
        // instead of silently muting every existing rule.
        assert!(armed_in_mode(&[], &crate::db::Settings::default().arm_mode));
        // Empty modes: armed in home + away, suppressed when disarmed.
        assert!(armed_in_mode(&[], "home"));
        assert!(armed_in_mode(&[], "away"));
        assert!(!armed_in_mode(&[], "disarmed"));
        // Opted into "away" only.
        let away = vec!["away".to_string()];
        assert!(armed_in_mode(&away, "away"));
        assert!(!armed_in_mode(&away, "home"));
        assert!(!armed_in_mode(&away, "disarmed"));
        // A panic rule opts into "disarmed": fires even while disarmed.
        let panic = vec![
            "disarmed".to_string(),
            "home".to_string(),
            "away".to_string(),
        ];
        assert!(armed_in_mode(&panic, "disarmed"));
        assert!(armed_in_mode(&panic, "home"));
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

    #[test]
    fn template_renders_and_escapes() {
        let ev = AlarmEvent {
            event_id: 7,
            camera: "front-door",
            label: "person",
            score: 0.9123,
            ts: 1000,
            snapshot_url: "/api/snapshots/x.jpg",
            snapshot_path: None,
            face: Some("Bob \"the\" Builder"),
            plate: None,
            gesture: None,
            // Embeds a control char (vertical tab) to prove it's \u-escaped.
            transcript: Some("help\u{000b}me"),
            speed: None,
            base_url: "",
            webhook_template: "",
            smtp: None,
            duress: false,
        };
        let out = render_template(
            r#"{"cam":"{{camera}}","obj":"{{label}}","who":"{{face}}","p":{{score}},"said":"{{transcript}}","miss":"{{nope}}"}"#,
            &ev,
        );
        // Valid JSON after substitution (quotes + control chars are escaped).
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["cam"], "front-door");
        assert_eq!(v["obj"], "person");
        assert_eq!(v["who"], "Bob \"the\" Builder");
        assert_eq!(v["p"], 0.912);
        assert_eq!(v["said"], "help\u{000b}me");
        // Unknown placeholder is left as-is.
        assert_eq!(v["miss"], "{{nope}}");
    }
}
