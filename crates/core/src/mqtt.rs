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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rumqttc::{Client, LastWill, MqttOptions, QoS};

use crate::db::Db;

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

pub fn run(db: Db, rx: Receiver<EventMsg>, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::Relaxed) {
        let settings = db.settings();
        let Some((host, port, creds)) = parse_url(&settings.mqtt_url) else {
            // MQTT off: drop incoming events so the channel never backs up.
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(_) | Err(RecvTimeoutError::Timeout) => continue,
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
        // Drive the network event loop on a helper thread; it flags death so
        // the publisher loop below can reconnect.
        let alive = Arc::new(AtomicBool::new(true));
        let driver = std::thread::spawn({
            let alive = alive.clone();
            move || {
                for ev in connection.iter() {
                    if let Err(e) = ev {
                        tracing::warn!("mqtt connection error: {e}");
                        break;
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
                    // Re-read settings so URL changes take effect.
                    let now_settings = db.settings();
                    if now_settings.mqtt_url != url_at_connect {
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
}
