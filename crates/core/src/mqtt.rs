//! MQTT publisher (Frigate/Home Assistant style): detection events go to
//! `{prefix}/events` (full JSON) and `{prefix}/{camera}/{label}` (score),
//! with `{prefix}/available` as a retained availability topic backed by a
//! last-will so subscribers see "offline" if the NVR dies.
//!
//! Runs on its own thread like the other workers; the detection pipeline
//! hands events over a channel and never blocks on the network. Connection
//! settings are re-read every loop, so changing the broker URL in Settings
//! applies within seconds.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use rumqttc::{Client, LastWill, MqttOptions, QoS};

use crate::db::Db;

/// What the pipeline sends per detection.
#[derive(Clone, Debug)]
pub struct EventMsg {
    pub event_id: i64,
    pub camera: String,
    pub label: String,
    pub score: f32,
    pub ts: i64,
    pub snapshot: String,
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
                    let _ = client.publish(
                        format!("{prefix}/events"),
                        QoS::AtLeastOnce,
                        false,
                        payload,
                    );
                    let _ = client.publish(
                        format!("{prefix}/{}/{}", ev.camera, ev.label),
                        QoS::AtLeastOnce,
                        false,
                        format!("{:.2}", ev.score),
                    );
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Re-read settings so URL changes take effect.
                    if db.settings().mqtt_url != url_at_connect {
                        break;
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
