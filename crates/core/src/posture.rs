//! Server-side body-pose worker — the 24/7 headless half of the residential
//! safety tier. Unlike the browser-only hand-gesture path, baby/fall safety can't
//! depend on someone having a tab open, so this worker runs a YOLOv8-pose model
//! (via ONNX Runtime, the same EP machinery as YOLO/YAMNet) on its own thread and
//! turns each person's 17 keypoints into a posture (via the pure [`pose`] crate),
//! then emits assistive safety events through the normal Alarm Manager path:
//!
//! - **fall** — a person is lying low in the frame, held for a few seconds.
//! - **standing** — a person is standing inside a named zone (crib climb-out /
//!   baby standing up). Zone-scoped so it isn't noise.
//! - **covered_face** — a person is present in a named zone but no face is visible
//!   for a while (rollover face-down / blanket over the face). Zone-scoped.
//!
//! Opt-in per camera (`DetectConfig.pose_detect`) and only runs when the pose
//! model file exists, so it costs nothing until a user sets up a nursery/elder cam.
//!
//! ## SAFETY / LIABILITY (read `docs/05`)
//! Every event here is **assistive, best-effort, NOT a medical device**. It misses
//! occluded/odd-framed people, can't truly tell prone from supine, and stops the
//! moment the camera or model does. The UI disclaims it; never auto-dial emergency
//! services off one of these and never present it as SIDS / suffocation / fall
//! certainty.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use detector::PoseEstimator;
use image::DynamicImage;
use pose::{Keypoint, Posture};

use crate::db::Db;
use crate::go2rtc::Go2Rtc;

/// Person-score threshold for a pose detection, and NMS IoU.
const POSE_CONF: f32 = 0.4;
const POSE_IOU: f32 = 0.45;
/// A person whose ground-contact anchor sits at/below this frame fraction is "on
/// the floor band" — the fall region.
const FALL_BAND: f32 = 0.6;
/// Hold times (secs) before a sustained condition fires, debouncing momentary poses.
const FALL_HOLD_SECS: i64 = 3;
const COVERED_HOLD_SECS: i64 = 5;
/// Per-(camera, label) re-fire cooldown so a held condition doesn't spam.
const COOLDOWN_SECS: i64 = 60;

/// Per-camera debounce/latch state across frames.
#[derive(Default, Clone, Copy)]
struct CamState {
    fall_since: Option<i64>,
    fall_fired: bool,
    covered_since: Option<i64>,
    covered_fired: bool,
    standing_on: bool,
}

pub fn models_present(pose_model: &str) -> bool {
    std::path::Path::new(pose_model).exists()
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    db: Db,
    go2rtc: Arc<Go2Rtc>,
    snapshots_dir: PathBuf,
    mqtt_tx: std::sync::mpsc::Sender<crate::mqtt::EventMsg>,
    throttle: crate::notify::AlarmThrottle,
    shutdown: Arc<AtomicBool>,
) {
    let mut estimator: Option<(String, PoseEstimator)> = None;
    let mut state: HashMap<i64, CamState> = HashMap::new();
    let mut last_fire: HashMap<(i64, &'static str), i64> = HashMap::new();

    while !shutdown.load(Ordering::Relaxed) {
        let settings = db.settings();
        let cameras = db.list_cameras().unwrap_or_default();
        let targets: Vec<_> = cameras
            .iter()
            .filter(|c| c.enabled && c.detect_config.pose_detect)
            .collect();

        if targets.is_empty() || !models_present(&settings.pose_model) {
            estimator = None;
            sleep_responsive(Duration::from_secs(3), &shutdown);
            continue;
        }

        // (Re)build the estimator when first needed or the model path changed.
        if estimator
            .as_ref()
            .map(|(p, _)| p != &settings.pose_model)
            .unwrap_or(true)
        {
            match PoseEstimator::new(
                &settings.pose_model,
                settings.force_cpu,
                POSE_CONF,
                POSE_IOU,
            ) {
                Ok(e) => {
                    tracing::info!(model = %settings.pose_model, "pose worker ready");
                    estimator = Some((settings.pose_model.clone(), e));
                }
                Err(e) => {
                    tracing::warn!("pose model unavailable: {e:#}");
                    sleep_responsive(Duration::from_secs(30), &shutdown);
                    continue;
                }
            }
        }
        let est = &mut estimator.as_mut().expect("built above").1;
        let alarms = db.list_alarms().unwrap_or_default();
        let live: std::collections::HashSet<i64> = cameras.iter().map(|c| c.id).collect();
        state.retain(|k, _| live.contains(k));

        for cam in &targets {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            let Some(bytes) = fetch_frame(&go2rtc.api_base(), &cam.name) else {
                continue;
            };
            let Ok(frame) = image::load_from_memory(&bytes) else {
                continue;
            };
            let (fw, fh) = (frame.width() as f32, frame.height() as f32);
            if fw < 1.0 || fh < 1.0 {
                continue;
            }
            let people = match est.estimate(&frame) {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!(camera = %cam.name, "pose estimate: {e:#}");
                    continue;
                }
            };

            // Evaluate the per-camera safety conditions across all detected people.
            let mut any_fall: Option<(f32, f32)> = None; // anchor of a fallen person
            let mut standing_zone: Option<(String, (f32, f32))> = None;
            let mut covered_zone: Option<(String, (f32, f32))> = None;
            for p in &people {
                let kpts = normalize_keypoints(p, fw, fh);
                let pose = pose::Pose { kpts };
                let assess = pose.classify();
                let anchor = (((p.x1 + p.x2) * 0.5) / fw, p.y2 / fh);
                let zone = cam
                    .detect_config
                    .zones
                    .iter()
                    .find(|z| z.contains(anchor.0, anchor.1))
                    .map(|z| z.name.clone());

                if assess.posture == Posture::Lying && anchor.1 >= FALL_BAND {
                    any_fall.get_or_insert(anchor);
                }
                if let Some(z) = &zone {
                    if assess.posture == Posture::Standing {
                        standing_zone.get_or_insert((z.clone(), anchor));
                    }
                    // Body present in the zone but no face visible -> covered /
                    // rolled face-down. (Confidence floor avoids a partial pose.)
                    if !assess.face_visible && assess.confidence >= 0.25 {
                        covered_zone.get_or_insert((z.clone(), anchor));
                    }
                }
            }

            let now = chrono::Local::now().timestamp();
            let st = state.entry(cam.id).or_default();

            // --- fall: held lying-in-floor-band ---
            if let Some(anchor) = any_fall {
                let since = *st.fall_since.get_or_insert(now);
                if !st.fall_fired && now - since >= FALL_HOLD_SECS {
                    st.fall_fired = true;
                    emit(
                        &db,
                        &settings,
                        &alarms,
                        &throttle,
                        &mqtt_tx,
                        &snapshots_dir,
                        &frame,
                        cam,
                        "fall",
                        anchor,
                        None,
                        now,
                        &mut last_fire,
                    );
                }
            } else {
                st.fall_since = None;
                st.fall_fired = false;
            }

            // --- covered_face: held body-present-no-face in a zone ---
            if let Some((zone, anchor)) = covered_zone {
                let since = *st.covered_since.get_or_insert(now);
                if !st.covered_fired && now - since >= COVERED_HOLD_SECS {
                    st.covered_fired = true;
                    emit(
                        &db,
                        &settings,
                        &alarms,
                        &throttle,
                        &mqtt_tx,
                        &snapshots_dir,
                        &frame,
                        cam,
                        "covered_face",
                        anchor,
                        Some(&zone),
                        now,
                        &mut last_fire,
                    );
                }
            } else {
                st.covered_since = None;
                st.covered_fired = false;
            }

            // --- standing: edge-triggered standing-in-zone (crib climb-out) ---
            match standing_zone {
                Some((zone, anchor)) => {
                    if !st.standing_on {
                        st.standing_on = true;
                        emit(
                            &db,
                            &settings,
                            &alarms,
                            &throttle,
                            &mqtt_tx,
                            &snapshots_dir,
                            &frame,
                            cam,
                            "standing",
                            anchor,
                            Some(&zone),
                            now,
                            &mut last_fire,
                        );
                    }
                }
                None => st.standing_on = false,
            }
        }

        sleep_responsive(Duration::from_secs(1), &shutdown);
    }
}

/// Normalize a person's pixel keypoints to frame fractions for the classifier.
fn normalize_keypoints(
    p: &detector::PersonPose,
    fw: f32,
    fh: f32,
) -> [Keypoint; pose::NUM_KEYPOINTS] {
    let mut kpts = [Keypoint::default(); pose::NUM_KEYPOINTS];
    for (i, k) in p.keypoints.iter().enumerate() {
        kpts[i] = Keypoint {
            x: k[0] / fw,
            y: k[1] / fh,
            conf: k[2],
        };
    }
    kpts
}

#[allow(clippy::too_many_arguments)]
fn emit(
    db: &Db,
    settings: &crate::db::Settings,
    alarms: &[crate::db::AlarmRule],
    throttle: &crate::notify::AlarmThrottle,
    mqtt_tx: &std::sync::mpsc::Sender<crate::mqtt::EventMsg>,
    snapshots_dir: &std::path::Path,
    frame: &DynamicImage,
    cam: &crate::db::Camera,
    label: &str,
    anchor: (f32, f32),
    zone: Option<&str>,
    now: i64,
    last_fire: &mut HashMap<(i64, &'static str), i64>,
) {
    // Per-(camera, label) cooldown so a sustained pose doesn't re-fire each second.
    let key_label: &'static str = match label {
        "fall" => "fall",
        "standing" => "standing",
        "covered_face" => "covered_face",
        _ => "pose",
    };
    if last_fire
        .get(&(cam.id, key_label))
        .map(|t| now - t < COOLDOWN_SECS)
        .unwrap_or(false)
    {
        return;
    }
    last_fire.insert((cam.id, key_label), now);

    let (ax, ay) = anchor;
    let bbox = [
        (ax - 0.04).clamp(0.0, 1.0),
        (ay - 0.1).clamp(0.0, 1.0),
        (ax + 0.04).clamp(0.0, 1.0),
        ay.clamp(0.0, 1.0),
    ];
    // Privacy/dignity: on a no-clip camera (nursery/bedroom/bathroom) fire the
    // alert WITHOUT writing any image to disk.
    let snap_rel = format!("{}-{}-{}.jpg", cam.name, now, label);
    let snapshot = if cam.detect_config.no_clip {
        None
    } else {
        std::fs::create_dir_all(snapshots_dir).ok();
        frame
            .save(snapshots_dir.join(&snap_rel))
            .ok()
            .map(|_| snap_rel.clone())
    };

    let id = match db.add_event(
        cam.id,
        now,
        label,
        1.0,
        bbox,
        snapshot.as_deref(),
        None,
        None,
        None,
        zone,
    ) {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("pose event insert failed: {e:#}");
            return;
        }
    };
    tracing::info!(camera = %cam.name, posture = label, zone = zone.unwrap_or("-"), event = id, "pose event");

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
    let snap_abs = snapshots_dir.join(&snap_rel);
    let alarm_ev = crate::notify::AlarmEvent {
        event_id: id,
        camera: &cam.name,
        label,
        score: 1.0,
        ts: now,
        snapshot_url: &snap_url,
        snapshot_path: snapshot.is_some().then_some(snap_abs.as_path()),
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
            && r.zone_ok(zone)
            && r.confirm_ok(db, cam.id, now)
            && crate::notify::armed_in_mode(&r.modes, &settings.arm_mode)
            && crate::notify::ready(r, throttle, now)
    }) {
        let suppressed = crate::notify::take_suppressed(throttle, rule.id);
        crate::notify::fire(rule, &alarm_ev, mqtt_tx, suppressed, db);
    }
}

fn fetch_frame(api_base: &str, camera: &str) -> Option<Vec<u8>> {
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

fn sleep_responsive(total: Duration, shutdown: &AtomicBool) {
    let start = std::time::Instant::now();
    while start.elapsed() < total && !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
    }
}
