//! P2.1 — camera-side analytics ingestion (ONVIF PullPoint events).
//!
//! Modern cameras run their own detection (Dahua/Amcrest IVS, Hikvision Smart
//! Events, Reolink person/vehicle) on the camera's chip and publish results as
//! ONVIF events. This worker subscribes to each opted-in camera's pull point,
//! normalizes what arrives into first-class `camera_*` events (motion,
//! tripwire, intrusion, person, vehicle), and routes them through the normal
//! snapshot + MQTT + Alarm Manager machinery — detection at **zero server GPU
//! cost**, the Blue Iris "ONVIF triggers" / Axis Camera Station model. It
//! complements (never replaces) server-side YOLO: pair the two with the
//! cross-modal `confirm_label` for AND-precision.
//!
//! Best-effort by design: vendors dialect ONVIF events heavily, so anything we
//! can't classify still lands in the per-camera **inspector** ring (surfaced at
//! `GET /api/onvif/inspect`) — the Blue Iris-style debug view that lets a user
//! see exactly what their camera emits before writing rules against it.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::db::Db;
use crate::ptz::{extract_between, parse_source, soap_call, CamTarget};

/// Recent raw notifications kept per camera for the inspector endpoint.
const INSPECT_KEEP: usize = 50;
/// PullMessages server-side wait; short so one camera can't stall the loop.
const PULL_TIMEOUT: &str = "PT2S";
/// Subscription lease; renewed opportunistically, recreated on any failure.
const SUB_TERMINATION: &str = "PT120S";

/// One normalized camera-side notification.
#[derive(Clone, Debug, serde::Serialize)]
pub struct CamNotify {
    /// Normalized topic (namespace prefixes stripped), e.g.
    /// "RuleEngine/CellMotionDetector/Motion".
    pub topic: String,
    /// The boolean payload when the event carries one (IsMotion/State/…);
    /// `None` = a momentary (pulse) event.
    pub active: Option<bool>,
    /// Vendor object classification when present ("Human", "Vehicle", …).
    pub object_class: Option<String>,
}

/// Shared inspector state: camera id → most recent notifications (newest last).
pub type InspectorBoard = Arc<Mutex<HashMap<i64, VecDeque<(i64, CamNotify)>>>>;

/// Map a normalized notification onto a Cammy event label. `None` = a topic we
/// don't ingest (still shown in the inspector). Object classifications win over
/// the topic: a Dahua IVS "Human" tripwire is a person sighting first.
pub(crate) fn label_for(topic: &str, object_class: Option<&str>) -> Option<&'static str> {
    if let Some(c) = object_class {
        let c = c.to_ascii_lowercase();
        if c.contains("human") || c.contains("person") || c.contains("people") {
            return Some("camera_person");
        }
        if c.contains("vehicle") || c.contains("car") || c.contains("truck") {
            return Some("camera_vehicle");
        }
    }
    let t = topic.to_ascii_lowercase();
    if t.contains("crossline") || t.contains("tripwire") || t.contains("linedetector") {
        return Some("camera_tripwire");
    }
    if t.contains("intrusion") || t.contains("fielddetector") || t.contains("objectsinside") {
        return Some("camera_intrusion");
    }
    if t.contains("motion") {
        return Some("camera_motion");
    }
    None
}

/// Pull the notification messages out of a PullMessagesResponse. Hand-rolled
/// substring parsing like the rest of the ONVIF client — the shapes we need
/// are shallow and vendors are sloppier than any strict parser tolerates.
pub(crate) fn parse_notifications(xml: &str) -> Vec<CamNotify> {
    let mut out = Vec::new();
    // Each message: <wsnt:NotificationMessage> … </wsnt:NotificationMessage>
    for chunk in xml.split("NotificationMessage>").skip(1).step_by(2) {
        // Topic element text, e.g. ">tns1:RuleEngine/CellMotionDetector/Motion</".
        let Some(raw_topic) = extract_between(chunk, "Topic", "</")
            .and_then(|t| t.split_once('>').map(|(_, v)| v))
        else {
            continue;
        };
        // Strip per-segment namespace prefixes: "tns1:RuleEngine/…" → "RuleEngine/…".
        let topic: String = raw_topic
            .trim()
            .split('/')
            .map(|seg| seg.rsplit(':').next().unwrap_or(seg))
            .collect::<Vec<_>>()
            .join("/");
        if topic.is_empty() {
            continue;
        }
        // Boolean payload: the first state-ish SimpleItem in the Data section.
        let mut active = None;
        let mut object_class = None;
        for item in chunk.split("<tt:SimpleItem").skip(1) {
            let name = extract_between(item, "Name=\"", "\"").unwrap_or("");
            let value = extract_between(item, "Value=\"", "\"").unwrap_or("");
            match name {
                "IsMotion" | "State" | "IsInside" | "LogicalState" | "IsTamper" => {
                    active = Some(value.eq_ignore_ascii_case("true") || value == "1");
                }
                "ObjectClass" | "ClassTypes" | "Type" if !value.is_empty() => {
                    object_class = Some(value.to_string());
                }
                _ => {}
            }
        }
        out.push(CamNotify {
            topic,
            active,
            object_class,
        });
    }
    out
}

/// A camera's live subscription: the pull-point address plus edge-trigger
/// memory (topic → last boolean state) so a held-true state fires once.
struct Sub {
    pull_url: String,
    last_state: HashMap<String, bool>,
}

/// Events-service subscription: GetCapabilities → Events XAddr →
/// CreatePullPointSubscription → the SubscriptionReference address.
fn subscribe(t: &CamTarget) -> anyhow::Result<String> {
    let device_service = format!("http://{}/onvif/device_service", t.host);
    let caps = soap_call(
        &device_service,
        t,
        r#"<GetCapabilities xmlns="http://www.onvif.org/ver10/device/wsdl">
             <Category>Events</Category>
           </GetCapabilities>"#,
    )?;
    let events_xaddr = extract_between(&caps, "XAddr>", "</")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| format!("http://{}/onvif/event_service", t.host));
    let resp = soap_call(
        &events_xaddr,
        t,
        &format!(
            r#"<CreatePullPointSubscription xmlns="http://www.onvif.org/ver10/events/wsdl">
                 <InitialTerminationTime>{SUB_TERMINATION}</InitialTerminationTime>
               </CreatePullPointSubscription>"#
        ),
    )?;
    let addr = extract_between(&resp, "Address>", "</")
        .map(str::trim)
        .filter(|a| a.starts_with("http"))
        .ok_or_else(|| anyhow::anyhow!("no SubscriptionReference address"))?;
    Ok(addr.to_string())
}

fn pull_messages(t: &CamTarget, pull_url: &str) -> anyhow::Result<String> {
    soap_call(
        pull_url,
        t,
        &format!(
            r#"<PullMessages xmlns="http://www.onvif.org/ver10/events/wsdl">
                 <Timeout>{PULL_TIMEOUT}</Timeout>
                 <MessageLimit>32</MessageLimit>
               </PullMessages>"#
        ),
    )
}

fn renew(t: &CamTarget, pull_url: &str) {
    let _ = soap_call(
        pull_url,
        t,
        &format!(
            r#"<Renew xmlns="http://docs.oasis-open.org/wsn/b-2">
                 <TerminationTime>{SUB_TERMINATION}</TerminationTime>
               </Renew>"#
        ),
    );
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    db: Db,
    api_base: String,
    snapshots_dir: std::path::PathBuf,
    inspector: InspectorBoard,
    throttle: crate::notify::AlarmThrottle,
    mqtt_tx: std::sync::mpsc::Sender<crate::mqtt::EventMsg>,
    shutdown: Arc<AtomicBool>,
) {
    let mut subs: HashMap<i64, Sub> = HashMap::new();
    // Per-(camera, label) event cooldown, mirroring the detection pipeline's.
    let mut last_event: HashMap<(i64, String), i64> = HashMap::new();
    let mut last_renew = std::time::Instant::now();
    while !shutdown.load(Ordering::Relaxed) {
        let cameras = db.list_cameras().unwrap_or_default();
        let settings = db.settings();
        let wanted: Vec<_> = cameras
            .iter()
            .filter(|c| c.enabled && c.detect_config.onvif_events)
            .collect();
        subs.retain(|id, _| wanted.iter().any(|c| c.id == *id));
        {
            let mut board = inspector.lock().expect("onvif inspector poisoned");
            board.retain(|id, _| wanted.iter().any(|c| c.id == *id));
        }
        if wanted.is_empty() {
            crate::util::sleep_interruptible(Duration::from_secs(5), &shutdown);
            continue;
        }
        let do_renew = last_renew.elapsed() > Duration::from_secs(60);
        if do_renew {
            last_renew = std::time::Instant::now();
        }
        for cam in &wanted {
            if shutdown.load(Ordering::Relaxed) {
                return;
            }
            let Some(target) = parse_source(&cam.source) else {
                continue; // no ONVIF credentials in the source URL
            };
            let sub = match subs.entry(cam.id) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(slot) => match subscribe(&target) {
                    Ok(pull_url) => {
                        tracing::info!(camera = %cam.name, "onvif events: subscribed");
                        slot.insert(Sub {
                            pull_url,
                            last_state: HashMap::new(),
                        })
                    }
                    Err(e) => {
                        tracing::debug!(camera = %cam.name, "onvif subscribe failed: {e:#}");
                        continue;
                    }
                },
            };
            if do_renew {
                renew(&target, &sub.pull_url);
            }
            let xml = match pull_messages(&target, &sub.pull_url) {
                Ok(x) => x,
                Err(e) => {
                    // Stale/expired subscription — drop it and resubscribe next tick.
                    tracing::debug!(camera = %cam.name, "onvif pull failed (resubscribing): {e:#}");
                    subs.remove(&cam.id);
                    continue;
                }
            };
            let now = chrono::Local::now().timestamp();
            for n in parse_notifications(&xml) {
                // Inspector ring first — the user sees EVERYTHING the camera
                // says, classified or not.
                {
                    let mut board = inspector.lock().expect("onvif inspector poisoned");
                    let ring = board.entry(cam.id).or_default();
                    ring.push_back((now, n.clone()));
                    while ring.len() > INSPECT_KEEP {
                        ring.pop_front();
                    }
                }
                // Edge trigger: boolean topics fire on false→true only; pulse
                // topics (no boolean) fire on every message (cooldown-gated).
                let fire = match n.active {
                    Some(state) => {
                        let prev = sub.last_state.insert(n.topic.clone(), state);
                        state && prev != Some(true)
                    }
                    None => true,
                };
                if !fire {
                    continue;
                }
                let Some(label) = label_for(&n.topic, n.object_class.as_deref()) else {
                    continue;
                };
                let cooldown = settings.event_cooldown_secs.max(1);
                let key = (cam.id, label.to_string());
                if last_event.get(&key).is_some_and(|t| now - t < cooldown) {
                    continue;
                }
                last_event.insert(key, now);
                emit(
                    &db, &settings, &throttle, &mqtt_tx, &api_base, &snapshots_dir, cam, label,
                    &n.topic, now,
                );
            }
        }
        crate::util::sleep_interruptible(Duration::from_millis(250), &shutdown);
    }
}

/// Record the camera-side event and route it through snapshot + MQTT + alarms,
/// mirroring the audio worker's shape (degenerate bbox, masked frame grab).
#[allow(clippy::too_many_arguments)]
fn emit(
    db: &Db,
    settings: &crate::db::Settings,
    throttle: &crate::notify::AlarmThrottle,
    mqtt_tx: &std::sync::mpsc::Sender<crate::mqtt::EventMsg>,
    api_base: &str,
    snapshots_dir: &std::path::Path,
    cam: &crate::db::Camera,
    label: &str,
    topic: &str,
    now: i64,
) {
    let snap_rel = format!("{}-{}-onvif.jpg", cam.name, now);
    let masks = &cam.detect_config.privacy_masks;
    let snapshot = fetch_frame(api_base, &cam.name)
        .and_then(|bytes| {
            std::fs::create_dir_all(snapshots_dir).ok();
            let path = snapshots_dir.join(&snap_rel);
            if masks.is_empty() {
                std::fs::write(path, bytes).ok()
            } else {
                let mut img = image::load_from_memory(&bytes).ok()?;
                crate::pipeline::apply_privacy_masks(&mut img, masks);
                img.save(path).ok()
            }
        })
        .map(|_| snap_rel.clone());
    let id = match db.add_event(
        cam.id,
        now,
        label,
        1.0,
        [0.0; 4],
        snapshot.as_deref(),
        None,
        None,
        None,
        Some(topic),
    ) {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("onvif event insert failed: {e:#}");
            return;
        }
    };
    tracing::info!(camera = %cam.name, label, topic, event = id, "camera-side event");
    let snap_url = snapshot
        .as_ref()
        .map(|s| format!("/api/snapshots/{s}"))
        .unwrap_or_default();
    let _ = mqtt_tx.send(crate::mqtt::EventMsg {
        event_id: id,
        camera: cam.name.clone(),
        label: label.to_string(),
        score: 1.0,
        ts: now,
        snapshot: snap_url.clone(),
        topic: None,
    });
    let alarms = db.list_alarms().unwrap_or_default();
    let snap_abs = snapshot.as_ref().map(|_| snapshots_dir.join(&snap_rel));
    let alarm_ev = crate::notify::AlarmEvent {
        event_id: id,
        camera: &cam.name,
        label,
        score: 1.0,
        ts: now,
        snapshot_url: &snap_url,
        snapshot_path: snap_abs.as_deref(),
        face: None,
        plate: None,
        gesture: None,
        transcript: None,
        speed: None,
        base_url: &settings.public_base_url,
        webhook_template: &settings.webhook_template,
        smtp: crate::notify::smtp_cfg(settings),
        duress: false,
        severity: crate::severity::severity_for(label, None, None),
        min_push_severity: settings.notify_min_severity,
        caption: None,
    };
    for rule in alarms.iter().filter(|r| {
        r.matches(cam.id, label, 1.0, None, None, None, None)
            && r.zone_ok(Some(topic))
            && r.confirm_ok(db, cam.id, now)
            && crate::notify::armed_in_mode(&r.modes, &settings.arm_mode)
            && crate::notify::ready(r, throttle, now)
    }) {
        let suppressed = crate::notify::take_suppressed(throttle, rule.id);
        crate::notify::fire(rule, &alarm_ev, mqtt_tx, suppressed);
    }
}

fn fetch_frame(api_base: &str, camera: &str) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let url = format!("{api_base}/api/frame.jpeg?src={camera}");
    let resp = ureq::get(&url)
        .timeout(Duration::from_secs(5))
        .call()
        .ok()?;
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(32 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_dahua_style_motion_notification() {
        let xml = r#"<tev:PullMessagesResponse>
          <wsnt:NotificationMessage>
            <wsnt:Topic Dialect="http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet">tns1:RuleEngine/CellMotionDetector/Motion</wsnt:Topic>
            <wsnt:Message><tt:Message UtcTime="2026-07-02T18:00:00Z" PropertyOperation="Changed">
              <tt:Source><tt:SimpleItem Name="VideoSourceConfigurationToken" Value="V0"/></tt:Source>
              <tt:Data><tt:SimpleItem Name="IsMotion" Value="true"/></tt:Data>
            </tt:Message></wsnt:Message>
          </wsnt:NotificationMessage>
          <wsnt:NotificationMessage>
            <wsnt:Topic Dialect="...">tns1:RuleEngine/MyRuleDetector/TripwireDetector</wsnt:Topic>
            <wsnt:Message><tt:Message>
              <tt:Data>
                <tt:SimpleItem Name="State" Value="false"/>
                <tt:SimpleItem Name="ObjectClass" Value="Human"/>
              </tt:Data>
            </tt:Message></wsnt:Message>
          </wsnt:NotificationMessage>
        </tev:PullMessagesResponse>"#;
        let ns = parse_notifications(xml);
        assert_eq!(ns.len(), 2);
        assert_eq!(ns[0].topic, "RuleEngine/CellMotionDetector/Motion");
        assert_eq!(ns[0].active, Some(true));
        assert!(ns[0].object_class.is_none());
        assert_eq!(ns[1].topic, "RuleEngine/MyRuleDetector/TripwireDetector");
        assert_eq!(ns[1].active, Some(false));
        assert_eq!(ns[1].object_class.as_deref(), Some("Human"));
    }

    #[test]
    fn label_mapping_prefers_object_class_and_covers_the_families() {
        // Object classification wins over the topic family.
        assert_eq!(
            label_for("RuleEngine/MyRuleDetector/TripwireDetector", Some("Human")),
            Some("camera_person")
        );
        assert_eq!(
            label_for("RuleEngine/FieldDetector/ObjectsInside", Some("Vehicle")),
            Some("camera_vehicle")
        );
        // Topic families.
        assert_eq!(
            label_for("RuleEngine/CellMotionDetector/Motion", None),
            Some("camera_motion")
        );
        assert_eq!(
            label_for("VideoSource/MotionAlarm", None),
            Some("camera_motion")
        );
        assert_eq!(
            label_for("RuleEngine/CrossLineDetector/Crossed", None),
            Some("camera_tripwire")
        );
        assert_eq!(
            label_for("RuleEngine/FieldDetector/ObjectsInside", None),
            Some("camera_intrusion")
        );
        // Unknown topics are inspector-only.
        assert_eq!(label_for("Device/Trigger/DigitalInput", None), None);
    }
}
