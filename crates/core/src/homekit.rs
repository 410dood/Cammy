//! P3.4 HomeKit v1a — the in-process "Cammy Sensors" HAP bridge.
//!
//! go2rtc's HAP server (v0) exposes each opted-in camera as a live-view-only
//! accessory: its accessory is exactly [AccessoryInformation,
//! CameraRTPStreamManagement, Microphone] with no sensor services and no way to
//! push an event, so HomeKit *automations* ("when motion, turn on the porch
//! light") are impossible through it. This worker runs a SECOND, Cammy-owned
//! HAP bridge (hap-rs) that exposes one MotionSensor accessory per
//! `homekit_expose` camera. The owner pairs it once in the Home app — an
//! honest, clearly-labeled second pairing beside the per-camera v0 pairings.
//!
//! Event source: a subscription to the same `broadcast::Sender<EventMsg>` tap
//! that feeds the SSE feed (every event flows through the MQTT worker's choke
//! point even with MQTT off). A motion-ish label on an exposed camera sets
//! MotionDetected=true; a per-camera timer clears it ~45s after the last event.
//!
//! Default-off invariants:
//!   - `Settings.homekit_enabled == false` (or no exposed camera) ⇒ this worker
//!     never constructs the HAP server, so NO listener binds and NO mDNS is
//!     announced.
//!   - Nothing here touches the go2rtc config or the v0 KV identities; the v0
//!     camera pairings are byte-for-byte unaffected.
//!
//! Identity/pairing state persists via hap-rs `FileStorage` under
//! `<data_dir>/homekit-bridge/` so the bridge pairing survives restarts. The
//! bridge PIN lives in the KV (`homekit.bridge_pin`, separate from the v0
//! camera PIN) and is re-asserted into the stored config on every start.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use hap::{
    accessory::{
        bridge::BridgeAccessory, motion_sensor::MotionSensorAccessory,
        stateless_programmable_switch::StatelessProgrammableSwitchAccessory, AccessoryCategory,
        AccessoryInformation, HapAccessory,
    },
    server::{IpServer, Server},
    storage::{FileStorage, Storage},
    HapType, Pin,
};

/// What `Server::add_accessory` returns (`hap::pointer::Accessory`, but that
/// module is private in 0.1.0-pre.15).
type AccessoryPtr = Arc<hap::futures::lock::Mutex<Box<dyn HapAccessory>>>;
use tokio::sync::broadcast;

use crate::{db::Db, mqtt::EventMsg};

/// TCP port the sensor bridge serves HAP on (fixed so the firewall note in
/// DEPLOYMENT.md can name it; mDNS advertises it either way).
pub const BRIDGE_PORT: u16 = 32180;

/// Seconds of quiet after the last motion-ish event before MotionDetected
/// auto-clears (HomeKit motion sensors are level-, not edge-, based).
const MOTION_CLEAR_SECS: u64 = 45;

/// aid offset for v1b doorbell-button accessories, so a camera's motion sensor
/// (aid = id + 1) and its doorbell button never collide across any realistic
/// camera-id range.
const DOORBELL_AID_OFFSET: u64 = 1 << 20;

/// Whether an event label means "the doorbell rang": the YAMNet audio class
/// ("Doorbell") or a soft trigger the owner labels "doorbell".
pub fn is_ring_label(label: &str) -> bool {
    label.eq_ignore_ascii_case("doorbell")
}

/// Labels that mean "something is moving in frame" for a HomeKit motion
/// sensor. Covers the YOLO moving-object classes plus the tracker/analytics
/// events that are inherently motion (crossing/loitering/zone entry) and the
/// generic motion/manual soft-trigger labels. Deliberately excludes
/// state-change events (zone_open, absence, audio labels).
pub fn is_motion_label(label: &str) -> bool {
    matches!(
        label,
        "person"
            | "car"
            | "truck"
            | "bus"
            | "motorcycle"
            | "bicycle"
            | "cat"
            | "dog"
            | "bird"
            | "horse"
            | "sheep"
            | "cow"
            | "bear"
            | "crossing"
            | "loiter"
            | "zone_enter"
            | "motion"
            | "manual"
    )
}

/// Get-or-create the sensor bridge's own 8-digit pairing PIN (KV
/// `homekit.bridge_pin`) — deliberately separate from the v0 per-camera go2rtc
/// PIN so rotating one never unpairs the other.
pub fn bridge_pin(db: &Db) -> String {
    if let Some(p) = db.get_kv("homekit.bridge_pin") {
        if p.len() == 8 && p.bytes().all(|c| c.is_ascii_digit()) {
            return p;
        }
    }
    let p = crate::go2rtc::gen_hap_pin();
    let _ = db.set_kv("homekit.bridge_pin", &p);
    p
}

fn hap_pin(digits: &str) -> Option<Pin> {
    let b: Vec<u8> = digits.bytes().map(|c| c.wrapping_sub(b'0')).collect();
    let arr: [u8; 8] = b.try_into().ok()?;
    Pin::new(arr).ok()
}

/// The (enabled, sorted exposed cameras) signature; the server is rebuilt when
/// it changes and torn down when it goes inactive.
fn config_sig(db: &Db) -> (bool, String, Vec<(i64, String, bool)>) {
    let s = db.settings();
    let mut cams: Vec<(i64, String, bool)> = db
        .list_cameras()
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c.enabled && c.detect_config.homekit_expose)
        .map(|c| (c.id, c.name, c.detect_config.homekit_doorbell))
        .collect();
    cams.sort();
    // v1c: the reset marker rides the signature so a serving generation tears
    // down promptly when a reset is requested (the worker loop then wipes).
    let reset = db.get_kv(BRIDGE_RESET_KEY).unwrap_or_default();
    (s.homekit_enabled && !cams.is_empty(), reset, cams)
}

/// KV marker an Admin sets to "pending" (POST /api/homekit/reset) to ask this
/// worker to factory-reset the sensor bridge. The WORKER performs the wipe —
/// it owns the storage directory's lifecycle, so there is no race with a
/// still-serving generation.
pub const BRIDGE_RESET_KEY: &str = "homekit.bridge_reset";

/// Worker entry point (own thread). Each configuration generation runs on its
/// OWN throwaway current-thread tokio runtime: hap-rs `tokio::spawn`s a task
/// per accepted controller connection, and only dropping the runtime reliably
/// kills those — otherwise a paired hub's long-lived event session would
/// survive a rebuild (still subscribed to the OLD accessories, so motion
/// notifications silently stop) or keep serving after the owner disables the
/// bridge. (Adversarial-review finding.)
pub fn run(
    db: Db,
    data_dir: PathBuf,
    events_tx: broadcast::Sender<EventMsg>,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::Relaxed) {
        // Requested reset: wipe the bridge's identity, pairings, and PIN. Runs
        // here — between generations — so no HAP server holds the files.
        if db.get_kv(BRIDGE_RESET_KEY).as_deref() == Some("pending") {
            let _ = std::fs::remove_dir_all(data_dir.join("homekit-bridge"));
            let _ = db.delete_kv("homekit.bridge_pin");
            let _ = db.delete_kv(BRIDGE_RESET_KEY);
            tracing::info!("homekit sensor bridge reset: identity, pairings and PIN wiped");
        }
        let sig = config_sig(&db);
        let (active, cams) = (sig.0, sig.2.clone());
        if !active {
            crate::util::sleep_interruptible(Duration::from_secs(5), &shutdown);
            continue;
        }
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                tracing::error!("homekit bridge: tokio runtime failed: {e:#}");
                return;
            }
        };
        // hap-rs panics (`expect`) on mDNS responder setup failure; contain it
        // so a transient UDP-5353 bind error degrades into the backoff below
        // instead of silently killing this worker thread for good.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            rt.block_on(serve(&db, &data_dir, &events_tx, &shutdown, sig, cams))
        }));
        // Dropping the runtime aborts every task hap spawned (accept loop,
        // per-connection sessions, mDNS) — a full teardown per generation.
        drop(rt);
        match outcome {
            Ok(Ok(())) => {} // clean teardown: config change / disable / shutdown
            Ok(Err(e)) => {
                tracing::warn!("homekit sensor bridge stopped: {e:#}");
                crate::util::sleep_interruptible(Duration::from_secs(30), &shutdown);
            }
            Err(_) => {
                tracing::warn!(
                    "homekit sensor bridge panicked (likely mDNS UDP 5353 setup); retrying"
                );
                crate::util::sleep_interruptible(Duration::from_secs(30), &shutdown);
            }
        }
    }
}

/// Build + run the HAP server for one configuration generation. Returns Ok(())
/// on a clean teardown (config change / disable / shutdown), Err on failure.
async fn serve(
    db: &Db,
    data_dir: &std::path::Path,
    events_tx: &broadcast::Sender<EventMsg>,
    shutdown: &Arc<AtomicBool>,
    sig: (bool, String, Vec<(i64, String, bool)>),
    cams: Vec<(i64, String, bool)>,
) -> anyhow::Result<()> {
    use anyhow::Context;

    let dir = data_dir.join("homekit-bridge");
    let mut storage = FileStorage::new(&dir)
        .await
        .map_err(|e| anyhow::anyhow!("bridge storage at {}: {e}", dir.display()))?;
    // The stored config carries pairing secrets (plaintext PIN + the bridge's
    // long-term ed25519 identity key); hap-rs writes them with the default
    // umask, so lock the directory down like tls.rs / evidence.rs do.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }

    let pin = hap_pin(&bridge_pin(db)).context("bridge PIN invalid")?;
    // Load the persisted bridge identity (device_id + ed25519 keypair MUST stay
    // stable across restarts or the Home app drops the pairing); re-assert the
    // KV PIN, name, and current LAN IP on every start.
    let mut config = match storage.load_config().await {
        Ok(mut c) => {
            c.pin = pin;
            c.name = "Cammy Sensors".into();
            c.port = BRIDGE_PORT;
            c.redetermine_local_ip();
            let _ = storage.save_config(&c).await;
            c
        }
        Err(_) => {
            let c = hap::Config {
                pin,
                name: "Cammy Sensors".into(),
                port: BRIDGE_PORT,
                category: AccessoryCategory::Bridge,
                ..Default::default()
            };
            storage
                .save_config(&c)
                .await
                .map_err(|e| anyhow::anyhow!("persisting bridge identity: {e}"))?;
            c
        }
    };

    // Reconcile the persisted aid cache with the CURRENT camera set. hap-rs
    // only bumps the HAP configuration number (`c#`) when an aid it has never
    // seen is added — it has no idea a camera was un-exposed/deleted — so
    // without this prune a removed sensor sits in the Home app as a permanent
    // "No Response" accessory (paired controllers only re-fetch /accessories
    // on a c# change), and a reused SQLite camera id would silently inherit
    // the dead accessory's identity/automations. (Adversarial-review finding.)
    let desired_aids: Vec<u64> = std::iter::once(1u64)
        .chain(cams.iter().map(|(id, _, _)| *id as u64 + 1))
        .chain(
            cams.iter()
                .filter(|(_, _, doorbell)| *doorbell)
                .map(|(id, _, _)| *id as u64 + 1 + DOORBELL_AID_OFFSET),
        )
        .collect();
    if let Ok(cache) = storage.load_aid_cache().await {
        let pruned: Vec<u64> = cache
            .iter()
            .copied()
            .filter(|a| desired_aids.contains(a))
            .collect();
        if pruned.len() != cache.len() {
            storage
                .save_aid_cache(&pruned)
                .await
                .map_err(|e| anyhow::anyhow!("pruning stale HomeKit accessories: {e}"))?;
            config.configuration_number += 1;
            storage
                .save_config(&config)
                .await
                .map_err(|e| anyhow::anyhow!("bumping HomeKit config number: {e}"))?;
        }
    }

    let server = IpServer::new(config, storage)
        .await
        .map_err(|e| anyhow::anyhow!("starting HAP server: {e}"))?;
    server
        .add_accessory(
            BridgeAccessory::new(
                1,
                AccessoryInformation {
                    name: "Cammy Sensors".into(),
                    manufacturer: "Cammy".into(),
                    model: "Cammy NVR sensor bridge".into(),
                    serial_number: "cammy-sensors-1".into(),
                    ..Default::default()
                },
            )
            .map_err(|e| anyhow::anyhow!("bridge accessory: {e}"))?,
        )
        .await
        .map_err(|e| anyhow::anyhow!("adding bridge accessory: {e}"))?;

    // One MotionSensor per exposed camera; aid = camera DB id + 1 (bridge = 1)
    // so an accessory keeps its identity (and Home-app room/automations) across
    // restarts and camera renames.
    let mut sensors: HashMap<String, AccessoryPtr> = HashMap::new();
    // v1b: doorbell BUTTON accessories (stateless programmable switch — the
    // Home app rejects a Doorbell service on an accessory with no camera
    // stream, and only go2rtc's sensor-less HAP accessory has one).
    let mut doorbells: HashMap<String, AccessoryPtr> = HashMap::new();
    for (id, name, doorbell) in &cams {
        let aid = (*id as u64) + 1;
        let acc = MotionSensorAccessory::new(
            aid,
            AccessoryInformation {
                name: format!("{name} Motion"),
                manufacturer: "Cammy".into(),
                model: "Cammy camera motion".into(),
                serial_number: format!("cammy-motion-{id}"),
                ..Default::default()
            },
        )
        .map_err(|e| anyhow::anyhow!("sensor accessory {name}: {e}"))?;
        let ptr = server
            .add_accessory(acc)
            .await
            .map_err(|e| anyhow::anyhow!("adding sensor {name}: {e}"))?;
        sensors.insert(name.clone(), ptr);
        if *doorbell {
            let acc = StatelessProgrammableSwitchAccessory::new(
                aid + DOORBELL_AID_OFFSET,
                AccessoryInformation {
                    name: format!("{name} Doorbell"),
                    manufacturer: "Cammy".into(),
                    model: "Cammy doorbell button".into(),
                    serial_number: format!("cammy-doorbell-{id}"),
                    ..Default::default()
                },
            )
            .map_err(|e| anyhow::anyhow!("doorbell accessory {name}: {e}"))?;
            let ptr = server
                .add_accessory(acc)
                .await
                .map_err(|e| anyhow::anyhow!("adding doorbell {name}: {e}"))?;
            doorbells.insert(name.clone(), ptr);
        }
    }

    // run_handle() borrows the server, so drive it inside the select loop
    // (dropping the future on any teardown path stops serving + mDNS).
    let server_fut = server.run_handle();
    tokio::pin!(server_fut);
    tracing::info!(
        port = BRIDGE_PORT,
        sensors = sensors.len(),
        "HomeKit sensor bridge up (pair \"Cammy Sensors\" in the Home app)"
    );

    let mut rx = events_tx.subscribe();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Camera name -> last motion-ish event (drives the auto-clear).
    let mut last_motion: HashMap<String, tokio::time::Instant> = HashMap::new();

    let result = loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) => {
                    // `topic: Some(..)` events are alarm-rule MQTT copies of an
                    // event already broadcast with topic None — skip them so a
                    // rule can't double-count (same filter as the SSE feed).
                    if ev.topic.is_none() && is_motion_label(&ev.label) {
                        if let Some(ptr) = sensors.get(&ev.camera) {
                            let fresh = last_motion
                                .insert(ev.camera.clone(), tokio::time::Instant::now())
                                .is_none();
                            if fresh {
                                set_motion(ptr, true).await;
                                tracing::debug!(camera = ev.camera, label = ev.label,
                                    "homekit motion ON");
                            }
                        }
                    }
                    if ev.topic.is_none() && is_ring_label(&ev.label) {
                        if let Some(ptr) = doorbells.get(&ev.camera) {
                            ring_doorbell(ptr).await;
                            tracing::debug!(camera = ev.camera, "homekit doorbell ring");
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!("homekit bridge lagged {n} events");
                }
                Err(broadcast::error::RecvError::Closed) => break Ok(()),
            },
            _ = tick.tick() => {
                if shutdown.load(Ordering::Relaxed) {
                    break Ok(());
                }
                // Auto-clear sensors that have been quiet long enough.
                let expired: Vec<String> = last_motion
                    .iter()
                    .filter(|(_, t)| t.elapsed() >= Duration::from_secs(MOTION_CLEAR_SECS))
                    .map(|(c, _)| c.clone())
                    .collect();
                for cam in expired {
                    last_motion.remove(&cam);
                    if let Some(ptr) = sensors.get(&cam) {
                        set_motion(ptr, false).await;
                        tracing::debug!(camera = cam, "homekit motion cleared");
                    }
                }
                // Config change (toggle off / expose list changed) => rebuild.
                if config_sig(db) != sig {
                    break Ok(());
                }
            },
            served = &mut server_fut => {
                break match served {
                    Ok(()) => Err(anyhow::anyhow!("HAP server exited")),
                    Err(e) => Err(anyhow::anyhow!("HAP server error: {e}")),
                };
            }
        }
    };
    result
}

/// Set the MotionDetected characteristic on a sensor accessory (notifies any
/// subscribed paired controller via hap-rs's event emitter).
async fn set_motion(ptr: &AccessoryPtr, on: bool) {
    let mut acc = ptr.lock().await;
    if let Some(ch) = acc
        .get_mut_service(HapType::MotionSensor)
        .and_then(|s| s.get_mut_characteristic(HapType::MotionDetected))
    {
        if let Err(e) = ch.set_value(serde_json::json!(on)).await {
            tracing::warn!("homekit set MotionDetected={on}: {e}");
        }
    }
}

/// How many controllers are paired to the sensor bridge (files in hap-rs's
/// pairings/ store; 0 when the bridge has never run or was reset).
pub fn bridge_pairing_count(data_dir: &std::path::Path) -> usize {
    std::fs::read_dir(data_dir.join("homekit-bridge").join("pairings"))
        .map(|d| d.filter_map(|e| e.ok()).count())
        .unwrap_or(0)
}

/// Emit a "single press" (value 0) on a doorbell button accessory.
async fn ring_doorbell(ptr: &AccessoryPtr) {
    let mut acc = ptr.lock().await;
    if let Some(ch) = acc
        .get_mut_service(HapType::StatelessProgrammableSwitch)
        .and_then(|s| s.get_mut_characteristic(HapType::ProgrammableSwitchEvent))
    {
        if let Err(e) = ch.set_value(serde_json::json!(0)).await {
            tracing::warn!("homekit doorbell press: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motion_labels_gate_correctly() {
        assert!(is_motion_label("person"));
        assert!(is_motion_label("crossing"));
        // State/audio events must NOT flip a motion sensor.
        assert!(!is_motion_label("zone_open"));
        assert!(!is_motion_label("Doorbell"));
        assert!(!is_motion_label(""));
    }
}
