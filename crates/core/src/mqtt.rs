//! MQTT publisher (Frigate/Home Assistant style): detection events go to
//! `{prefix}/events` (full JSON) and `{prefix}/{camera}/{label}` (score),
//! with `{prefix}/available` as a retained availability topic backed by a
//! last-will so subscribers see "offline" if the NVR dies.
//!
//! Runs on its own thread like the other workers; the detection pipeline
//! hands events over a channel and never blocks on the network. Connection
//! settings are re-read every loop, so changing the broker URL in Settings
//! applies within seconds.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rumqttc::{Client, Event, LastWill, MqttOptions, Packet, QoS};
use tokio::sync::broadcast;

use crate::db::Db;
use crate::go2rtc::Go2Rtc;
use crate::notify::AlarmThrottle;

/// MQTT-safe identifier: letters, digits, '_' kept; everything else (spaces in
/// labels like "traffic light", etc.) becomes '_'.
fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Map a detection label to a Home Assistant binary_sensor device_class.
fn device_class(label: &str) -> &'static str {
    match label {
        "person" => "occupancy",
        "car" | "truck" | "bus" | "motorcycle" | "bicycle" => "moving",
        "dog" | "cat" | "bird" => "presence",
        _ => "motion",
    }
}

/// HA device block so all of a camera's entities group under one device.
fn ha_device(camera: &str) -> serde_json::Value {
    serde_json::json!({
        "identifiers": [format!("zoomy_{}", slug(camera))],
        "name": format!("Cammy {camera}"),
        "manufacturer": "Cammy",
        "model": "NVR camera",
    })
}

/// Home Assistant MQTT-discovery config topics + retained payloads: a
/// binary_sensor per (camera, label) and a last-detection sensor per camera.
/// Publishing these makes HA auto-create entities with no YAML.
fn discovery_configs(
    ha_prefix: &str,
    prefix: &str,
    cameras: &[String],
    labels: &[String],
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for cam in cameras {
        let cs = slug(cam);
        let dev = ha_device(cam);
        for label in labels {
            let ls = slug(label);
            let topic = format!("{ha_prefix}/binary_sensor/zoomy_{cs}_{ls}/config");
            let payload = serde_json::json!({
                "name": label,
                "unique_id": format!("zoomy_{cs}_{ls}"),
                "state_topic": format!("{prefix}/{cam}/{ls}/state"),
                "payload_on": "ON",
                "payload_off": "OFF",
                "device_class": device_class(label),
                "availability_topic": format!("{prefix}/available"),
                "payload_available": "online",
                "payload_not_available": "offline",
                "device": dev,
            });
            out.push((topic, payload.to_string()));
        }
        // Per-camera "last detection" sensor with the full event as attributes.
        let topic = format!("{ha_prefix}/sensor/zoomy_{cs}_event/config");
        let payload = serde_json::json!({
            "name": "Last detection",
            "unique_id": format!("zoomy_{cs}_event"),
            "state_topic": format!("{prefix}/{cam}/event"),
            "value_template": "{{ value_json.label }}",
            "json_attributes_topic": format!("{prefix}/{cam}/event"),
            "availability_topic": format!("{prefix}/available"),
            "payload_available": "online",
            "payload_not_available": "offline",
            "icon": "mdi:cctv",
            "device": dev,
        });
        out.push((topic, payload.to_string()));
    }
    out
}

/// What the pipeline sends per detection. `topic` overrides the standard
/// topics — used by Alarm Manager rules with an mqtt action.
#[derive(Clone, Debug)]
pub struct EventMsg {
    pub event_id: i64,
    pub camera: String,
    pub label: String,
    pub score: f32,
    pub ts: i64,
    pub snapshot: String,
    pub topic: Option<String>,
}

type Credentials = Option<(String, String)>;

/// Parse "mqtt://user:pass@host:1883", "host:1883" or "host" forms.
fn parse_url(url: &str) -> Option<(String, u16, Credentials)> {
    let rest = url.strip_prefix("mqtt://").unwrap_or(url).trim();
    if rest.is_empty() {
        return None;
    }
    let (creds, hostport) = match rest.split_once('@') {
        Some((u, h)) => (u.split_once(':').map(|(a, b)| (a.into(), b.into())), h),
        None => (None, rest),
    };
    let (host, port) = match hostport.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()?),
        None => (hostport.to_string(), 1883),
    };
    Some((host, port, creds))
}

/// The MQTT worker. Besides the outbound publish + HA discovery it has always
/// done, it now (P3.3):
///   - taps EVERY consumed [`EventMsg`] into `broadcast_tx` for the live SSE feed
///     (`GET /api/events/stream`), including while outbound MQTT is off; and
///   - optionally (Settings.mqtt_commands_enabled, default OFF) subscribes to
///     `<prefix>/cmd/#` and dispatches inbound arm/trigger commands, re-using the
///     soft-trigger + arm-mode helpers.
///
/// `event_tx` is a clone of the same mpsc `Sender` that feeds `rx`, so an inbound
/// trigger command can inject a real event back through the normal path.
#[allow(clippy::too_many_arguments)]
pub fn run(
    db: Db,
    go2rtc: Arc<Go2Rtc>,
    snapshots_dir: PathBuf,
    rx: Receiver<EventMsg>,
    event_tx: Sender<EventMsg>,
    broadcast_tx: broadcast::Sender<EventMsg>,
    alarm_throttle: AlarmThrottle,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::Relaxed) {
        let settings = db.settings();
        let Some((host, port, creds)) = parse_url(&settings.mqtt_url) else {
            // MQTT off: still tap events to the SSE feed, then drop them so the
            // channel never backs up.
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(ev) => {
                    let _ = broadcast_tx.send(ev);
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };
        let prefix = if settings.mqtt_prefix.trim().is_empty() {
            "zoomy".to_string()
        } else {
            settings.mqtt_prefix.trim().to_string()
        };

        let mut opts = MqttOptions::new("zoomy-nvr", &host, port);
        opts.set_keep_alive(Duration::from_secs(15));
        opts.set_last_will(LastWill::new(
            format!("{prefix}/available"),
            "offline",
            QoS::AtLeastOnce,
            true,
        ));
        if let Some((u, p)) = creds {
            opts.set_credentials(u, p);
        }

        let (client, mut connection) = Client::new(opts, 64);
        // Inbound-command context (P3.3): built once per connection, only when the
        // opt-in toggle is on. A malformed/unknown command is ignored, never a
        // panic; every accepted command is audited.
        let commands_on = settings.mqtt_commands_enabled;
        let cmd_ctx = commands_on.then(|| CommandCtx {
            db: db.clone(),
            prefix: prefix.clone(),
            go2rtc: go2rtc.clone(),
            snapshots_dir: snapshots_dir.clone(),
            event_tx: event_tx.clone(),
            alarm_throttle: alarm_throttle.clone(),
        });
        // Drive the network event loop on a helper thread; it flags death so
        // the publisher loop below can reconnect. Inbound command publishes arrive
        // here — each is dispatched on its own short-lived thread so a slow command
        // (a trigger's snapshot fetch) can't stall MQTT keep-alives.
        let alive = Arc::new(AtomicBool::new(true));
        let driver = std::thread::spawn({
            let alive = alive.clone();
            move || {
                for ev in connection.iter() {
                    match ev {
                        Ok(Event::Incoming(Packet::Publish(p))) => {
                            if let Some(ctx) = &cmd_ctx {
                                if let Some(cmd) = parse_command(&ctx.prefix, &p.topic, &p.payload)
                                {
                                    let ctx = ctx.clone();
                                    std::thread::spawn(move || ctx.dispatch(cmd));
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!("mqtt connection error: {e}");
                            break;
                        }
                    }
                }
                alive.store(false, Ordering::Relaxed);
            }
        });

        let _ = client.publish(
            format!("{prefix}/available"),
            QoS::AtLeastOnce,
            true,
            "online",
        );
        if commands_on {
            // Subscribe so the broker forwards command publishes to the driver.
            let _ = client.subscribe(format!("{prefix}/cmd/#"), QoS::AtLeastOnce);
            tracing::info!(
                prefix,
                "mqtt inbound commands ENABLED (subscribed to {prefix}/cmd/#)"
            );
        }
        tracing::info!(broker = format!("{host}:{port}"), prefix, "mqtt connected");

        // Home Assistant discovery: publish (retained) configs and remember the
        // (cameras × labels) signature so we re-publish when it changes.
        let label_set = || {
            let mut l = settings.detect_labels.clone();
            if l.is_empty() {
                l = vec!["person".into()];
            }
            l
        };
        let cam_set = |db: &Db| -> Vec<String> {
            db.list_cameras()
                .unwrap_or_default()
                .into_iter()
                .filter(|c| c.enabled)
                .map(|c| c.name)
                .collect()
        };
        let mut disco_sig = String::new();
        let publish_discovery = |client: &Client, cams: &[String], labels: &[String]| {
            for (topic, payload) in
                discovery_configs(&settings.mqtt_ha_prefix, &prefix, cams, labels)
            {
                let _ = client.publish(topic, QoS::AtLeastOnce, true, payload);
            }
        };
        if settings.mqtt_ha_discovery {
            let (cams, labels) = (cam_set(&db), label_set());
            disco_sig = format!("{cams:?}|{labels:?}");
            publish_discovery(&client, &cams, &labels);
            tracing::info!("published Home Assistant MQTT discovery configs");
        }

        // Track ON binary_sensors so they can be auto-cleared to OFF.
        let mut last_on: HashMap<(String, String), Instant> = HashMap::new();
        let state_timeout = Duration::from_secs(settings.mqtt_state_timeout_secs.max(1));

        let url_at_connect = settings.mqtt_url.clone();
        while alive.load(Ordering::Relaxed) && !shutdown.load(Ordering::Relaxed) {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(ev) => {
                    // Tap into the live SSE feed before publishing (the SSE handler
                    // keeps only the base `topic: None` events; see api::events_stream).
                    let _ = broadcast_tx.send(ev.clone());
                    let payload = serde_json::json!({
                        "type": "detection",
                        "event_id": ev.event_id,
                        "camera": ev.camera,
                        "label": ev.label,
                        "score": ev.score,
                        "ts": ev.ts,
                        "snapshot": ev.snapshot,
                    })
                    .to_string();
                    match &ev.topic {
                        // System arm-mode state: publish RETAINED as a bare mode
                        // string ("home"/"away"/"disarmed") so a (re)connecting
                        // HA / keypad automation always reads the current mode from
                        // the broker, not just live changes.
                        Some(t) if t == "mode" => {
                            let _ = client.publish(
                                format!("{prefix}/mode"),
                                QoS::AtLeastOnce,
                                true,
                                ev.label.clone(),
                            );
                        }
                        // Alarm rule with a custom topic: publish there only.
                        Some(t) => {
                            let _ = client.publish(
                                format!("{prefix}/{t}"),
                                QoS::AtLeastOnce,
                                false,
                                payload,
                            );
                        }
                        None => {
                            let ls = slug(&ev.label);
                            let _ = client.publish(
                                format!("{prefix}/events"),
                                QoS::AtLeastOnce,
                                false,
                                payload.clone(),
                            );
                            let _ = client.publish(
                                format!("{prefix}/{}/{}", ev.camera, ev.label),
                                QoS::AtLeastOnce,
                                false,
                                format!("{:.2}", ev.score),
                            );
                            // HA binary_sensor ON + per-camera last-event sensor.
                            let _ = client.publish(
                                format!("{prefix}/{}/{ls}/state", ev.camera),
                                QoS::AtLeastOnce,
                                false,
                                "ON",
                            );
                            let _ = client.publish(
                                format!("{prefix}/{}/event", ev.camera),
                                QoS::AtLeastOnce,
                                true,
                                payload,
                            );
                            last_on.insert((ev.camera.clone(), ls), Instant::now());
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Auto-clear binary_sensors whose detection has gone stale.
                    last_on.retain(|(cam, ls), since| {
                        if since.elapsed() >= state_timeout {
                            let _ = client.publish(
                                format!("{prefix}/{cam}/{ls}/state"),
                                QoS::AtLeastOnce,
                                false,
                                "OFF",
                            );
                            false
                        } else {
                            true
                        }
                    });
                    // Re-read settings so URL / command-toggle changes take effect
                    // (reconnect re-runs subscribe/unsubscribe cleanly).
                    let now_settings = db.settings();
                    if now_settings.mqtt_url != url_at_connect
                        || now_settings.mqtt_commands_enabled != commands_on
                    {
                        break;
                    }
                    // Re-publish discovery when the camera/label set changes.
                    if now_settings.mqtt_ha_discovery {
                        let (cams, labels) = (cam_set(&db), label_set());
                        let sig = format!("{cams:?}|{labels:?}");
                        if sig != disco_sig {
                            disco_sig = sig;
                            publish_discovery(&client, &cams, &labels);
                        }
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        let _ = client.publish(
            format!("{prefix}/available"),
            QoS::AtLeastOnce,
            true,
            "offline",
        );
        let _ = client.disconnect();
        let _ = driver.join();
        if !shutdown.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(2)); // reconnect backoff
        }
    }
}

// --- inbound MQTT commands (P3.3, opt-in) ------------------------------------

/// A parsed inbound command. Deliberately tiny for v0 — arm/disarm + trigger only.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    /// Set the system security mode: "home" | "away" | "disarmed".
    Arm(String),
    /// Trigger a camera by numeric id or exact name — a soft-trigger event.
    Trigger(String),
}

/// Map an inbound `<prefix>/cmd/...` publish to a [`Command`]. PURE — no side
/// effects, so it is fully unit-testable. Anything unrecognized (wrong topic,
/// bad mode, empty/oversized/controly payload) returns `None` and is ignored.
fn parse_command(prefix: &str, topic: &str, payload: &[u8]) -> Option<Command> {
    let sub = topic
        .strip_prefix(prefix)
        .and_then(|t| t.strip_prefix("/cmd/"))?;
    let body = std::str::from_utf8(payload).ok()?.trim();
    match sub {
        "arm" => {
            let mode = body.to_ascii_lowercase();
            matches!(mode.as_str(), "home" | "away" | "disarmed").then_some(Command::Arm(mode))
        }
        "trigger" => (!body.is_empty()
            && body.chars().count() <= 64
            && !body.chars().any(|c| c.is_control()))
        .then(|| Command::Trigger(body.to_string())),
        _ => None,
    }
}

/// Everything a command dispatch needs. Cloned per command onto a short-lived
/// thread. Cheap to clone (handles/`Arc`s/paths).
#[derive(Clone)]
struct CommandCtx {
    db: Db,
    prefix: String,
    go2rtc: Arc<Go2Rtc>,
    snapshots_dir: PathBuf,
    event_tx: Sender<EventMsg>,
    alarm_throttle: AlarmThrottle,
}

impl CommandCtx {
    /// Execute a validated command by re-using the same code paths the HTTP API
    /// does (arm-mode KV + audit + notify + state publish; soft-trigger event +
    /// alarm dispatch). Every accepted command is audited. Fail-soft throughout.
    fn dispatch(&self, cmd: Command) {
        let now = chrono::Local::now().timestamp();
        match cmd {
            Command::Arm(mode) => {
                if self.db.settings().arm_mode == mode {
                    tracing::debug!(%mode, "mqtt arm command: already in mode, no-op");
                    return;
                }
                if let Err(e) = self.db.set_kv("arm_mode", &mode) {
                    tracing::warn!("mqtt arm command: set_kv failed: {e:#}");
                    return;
                }
                // Audit (no client IP — the broker is the source), notify, and
                // re-publish the retained mode state for other HA/keypad clients.
                self.db
                    .add_audit(now, None, "mqtt_arm_command", Some(&mode));
                let _ = self.db.add_notification(
                    now,
                    "mode",
                    &format!("System {}", crate::api::mode_phrase(&mode)),
                    Some("Set via inbound MQTT command"),
                    None,
                );
                let _ = self.event_tx.send(EventMsg {
                    event_id: 0,
                    camera: String::new(),
                    label: mode.clone(),
                    score: 0.0,
                    ts: now,
                    snapshot: String::new(),
                    topic: Some("mode".to_string()),
                });
                tracing::info!(%mode, "mqtt command: security mode changed");
            }
            Command::Trigger(cam_ref) => {
                let Some(cam) = resolve_camera(&self.db, &cam_ref) else {
                    tracing::warn!(camera = %cam_ref, "mqtt trigger command: unknown camera, ignored");
                    return;
                };
                self.db
                    .add_audit(now, None, "mqtt_trigger_command", Some(&cam.name));
                let ctx = crate::trigger::TriggerCtx {
                    db: self.db.clone(),
                    go2rtc: self.go2rtc.clone(),
                    snapshots_dir: self.snapshots_dir.clone(),
                    mqtt_tx: self.event_tx.clone(),
                    alarm_throttle: self.alarm_throttle.clone(),
                };
                match crate::trigger::record_event(&ctx, &cam, "manual", now) {
                    Ok(rec) => crate::trigger::fire_alarms(&ctx, &cam, "manual", now, &rec),
                    Err(e) => tracing::warn!("mqtt trigger command: {e:#}"),
                }
            }
        }
    }
}

/// Resolve a trigger command's camera reference (numeric id or exact name) to a
/// registered camera. Returns `None` for an unknown camera so the command is
/// rejected rather than acting on the wrong one.
fn resolve_camera(db: &Db, cam_ref: &str) -> Option<crate::db::Camera> {
    let cams = db.list_cameras().ok()?;
    if let Ok(id) = cam_ref.parse::<i64>() {
        if let Some(c) = cams.iter().find(|c| c.id == id) {
            return Some(c.clone());
        }
    }
    cams.into_iter().find(|c| c.name == cam_ref)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_broker_urls() {
        assert_eq!(
            parse_url("mqtt://10.0.0.5:1884"),
            Some(("10.0.0.5".into(), 1884, None))
        );
        assert_eq!(
            parse_url("broker.local"),
            Some(("broker.local".into(), 1883, None))
        );
        let (h, p, c) = parse_url("mqtt://bob:pw@hass:1883").unwrap();
        assert_eq!((h.as_str(), p), ("hass", 1883));
        assert_eq!(c, Some(("bob".into(), "pw".into())));
        assert_eq!(parse_url(""), None);
        assert_eq!(parse_url("host:notaport"), None);
    }

    #[test]
    fn parses_arm_commands() {
        assert_eq!(
            parse_command("zoomy", "zoomy/cmd/arm", b"home"),
            Some(Command::Arm("home".into()))
        );
        // Case-insensitive + whitespace-trimmed payload.
        assert_eq!(
            parse_command("zoomy", "zoomy/cmd/arm", b" AWAY \n"),
            Some(Command::Arm("away".into()))
        );
        assert_eq!(
            parse_command("zoomy", "zoomy/cmd/arm", b"disarmed"),
            Some(Command::Arm("disarmed".into()))
        );
        // A non-default prefix is honored exactly.
        assert_eq!(
            parse_command("home/nvr", "home/nvr/cmd/arm", b"home"),
            Some(Command::Arm("home".into()))
        );
        // Unknown mode → ignored.
        assert_eq!(parse_command("zoomy", "zoomy/cmd/arm", b"panic"), None);
        assert_eq!(parse_command("zoomy", "zoomy/cmd/arm", b""), None);
    }

    #[test]
    fn parses_trigger_commands() {
        assert_eq!(
            parse_command("zoomy", "zoomy/cmd/trigger", b"3"),
            Some(Command::Trigger("3".into()))
        );
        assert_eq!(
            parse_command("zoomy", "zoomy/cmd/trigger", b"Front Door"),
            Some(Command::Trigger("Front Door".into()))
        );
        // Empty / control-char / oversized payloads are rejected.
        assert_eq!(parse_command("zoomy", "zoomy/cmd/trigger", b"  "), None);
        assert_eq!(
            parse_command("zoomy", "zoomy/cmd/trigger", b"bad\nname"),
            None
        );
        let too_long = "x".repeat(65);
        assert_eq!(
            parse_command("zoomy", "zoomy/cmd/trigger", too_long.as_bytes()),
            None
        );
        // Non-UTF-8 payload → ignored, never a panic.
        assert_eq!(
            parse_command("zoomy", "zoomy/cmd/trigger", &[0xff, 0xfe]),
            None
        );
    }

    #[test]
    fn ignores_unknown_and_foreign_topics() {
        // Unknown subcommand.
        assert_eq!(parse_command("zoomy", "zoomy/cmd/reboot", b"now"), None);
        // Command topic under the wrong prefix.
        assert_eq!(parse_command("zoomy", "other/cmd/arm", b"home"), None);
        // Our own OUTBOUND topics must never parse as commands.
        assert_eq!(parse_command("zoomy", "zoomy/events", b"{}"), None);
        assert_eq!(parse_command("zoomy", "zoomy/mode", b"home"), None);
        // Prefix present but not a /cmd/ topic.
        assert_eq!(parse_command("zoomy", "zoomy/cmd", b"home"), None);
        assert_eq!(parse_command("zoomy", "zoomycmd/arm", b"home"), None);
    }
}
