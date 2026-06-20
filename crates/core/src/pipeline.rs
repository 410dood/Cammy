//! Two-stage detection pipeline: sample each camera's decoded frame from
//! go2rtc, run the cheap motion gate, and only when pixels actually changed
//! hand the frame to YOLO. Matching detections become events with annotated
//! snapshots.
//!
//! One thread + one ONNX session serves all cameras: at ~1 fps sampling and
//! <10 ms GPU inference, a single session comfortably covers a home's worth of
//! cameras, and the GPU never sees a still frame.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use detector::Detector;
use image::{DynamicImage, Rgb};
use motion::MotionGate;

use crate::db::Db;
use crate::go2rtc::Go2Rtc;
use crate::status::StatusBoard;

/// Cap on a fetched JPEG frame (sanity guard, not a real limit).
const MAX_FRAME_BYTES: u64 = 32 * 1024 * 1024;

#[allow(clippy::too_many_arguments)]
pub fn run(
    db: Db,
    go2rtc: Arc<Go2Rtc>,
    snapshots_dir: PathBuf,
    status: StatusBoard,
    mqtt_tx: std::sync::mpsc::Sender<crate::mqtt::EventMsg>,
    throttle: crate::notify::AlarmThrottle,
    genai_tx: std::sync::mpsc::Sender<crate::genai::CaptionJob>,
    shutdown: Arc<AtomicBool>,
) {
    // One detector session per (model, force_cpu, conf, iou) combination, so
    // cameras can be assigned different models or accelerators.
    let mut detectors: HashMap<String, Detector> = HashMap::new();
    let mut global_detect_key = String::new();
    // Per-camera sample-interval cap (FPS governance).
    let mut last_poll: HashMap<i64, Instant> = HashMap::new();
    let mut face_engine: Option<facerec::FaceEngine> = None;
    let mut face_key = String::new();
    let mut clip: Option<crate::smart::ImageEmbedder> = None;
    let mut lpr: Option<crate::lpr::PlateEngine> = None;
    // Autotrack state: PTZ capability cache + per-camera move cooldown.
    let mut ptz_capable: HashMap<i64, bool> = HashMap::new();
    let mut last_autotrack: HashMap<i64, Instant> = HashMap::new();
    // Throttle unknown-face crops: one per camera per 30s, or enrollment
    // would drown in near-duplicates.
    let mut last_unknown_save: HashMap<i64, i64> = HashMap::new();
    // Per-camera motion gate, keyed with the threshold it was built for so a
    // settings or per-camera-config change rebuilds it.
    let mut gates: HashMap<i64, (f32, MotionGate)> = HashMap::new();
    let mut last_event: HashMap<(i64, &'static str), i64> = HashMap::new();
    // Per-camera object tracker + analytics memory (line-crossing, loitering).
    // Only used on cameras that configure a tripwire or a dwell zone.
    let mut trackers: HashMap<i64, tracker::Tracker> = HashMap::new();
    let mut analytics: HashMap<i64, crate::analytics::AnalyticsState> = HashMap::new();

    while !shutdown.load(Ordering::Relaxed) {
        let tick = Instant::now();
        let settings = db.settings();

        // Drop cached sessions when global model/EP/threshold settings change
        // (per-camera overrides get their own cache keys below).
        let gkey = format!(
            "{}|{}|{}|{}",
            settings.model_path, settings.force_cpu, settings.confidence, settings.nms_iou
        );
        if gkey != global_detect_key {
            detectors.clear();
            global_detect_key = gkey;
        }

        let cameras = db.list_cameras().unwrap_or_default();
        let alarms = db.list_alarms().unwrap_or_default();
        for cam in cameras.iter().filter(|c| c.enabled && c.detect) {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            // Per-camera FPS cap: skip until this camera's interval elapses.
            if let Some(ms) = cam.detect_config.poll_ms {
                if last_poll
                    .get(&cam.id)
                    .is_some_and(|t| t.elapsed() < Duration::from_millis(ms))
                {
                    continue;
                }
                last_poll.insert(cam.id, Instant::now());
            }

            // Resolve this camera's model + accelerator (per-camera override or
            // global), and build/fetch the matching detector session.
            let model = cam
                .detect_config
                .model
                .clone()
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| settings.model_path.clone());
            let force_cpu = cam.detect_config.force_cpu.unwrap_or(settings.force_cpu);
            let dkey = format!(
                "{model}|{force_cpu}|{}|{}",
                settings.confidence, settings.nms_iou
            );
            if !detectors.contains_key(&dkey) {
                match Detector::new(&model, force_cpu, settings.confidence, settings.nms_iou) {
                    Ok(d) => {
                        tracing::info!(camera = %cam.name, model = %model, force_cpu, "detector ready");
                        detectors.insert(dkey.clone(), d);
                    }
                    Err(e) => {
                        tracing::debug!(camera = %cam.name, "detector unavailable: {e:#}");
                        continue;
                    }
                }
            }
            let accelerator = accel_label(force_cpu);

            // Sample the low-res sub-stream when one is configured.
            let stream_key = match cam.detect_source.as_deref().filter(|s| !s.is_empty()) {
                Some(_) => format!("{}_sub", cam.name),
                None => cam.name.clone(),
            };
            let mut frame = match fetch_frame(&go2rtc.api_base(), &stream_key) {
                Ok(f) => {
                    status.frame_ok(cam.id, chrono::Local::now().timestamp());
                    f
                }
                Err(e) => {
                    status.frame_err(cam.id, format!("{e:#}"));
                    tracing::debug!(camera = %cam.name, "no frame: {e:#}");
                    continue;
                }
            };

            // Privacy masks: black out the polygons before anything looks at the
            // frame — motion gate, detector and snapshot all see the masked view.
            if !cam.detect_config.privacy_masks.is_empty() {
                apply_privacy_masks(&mut frame, &cam.detect_config.privacy_masks);
            }

            let threshold = cam
                .detect_config
                .motion_threshold
                .unwrap_or(settings.motion_threshold);
            let gate = match gates.get_mut(&cam.id) {
                Some((t, g)) if *t == threshold => g,
                _ => {
                    &mut gates
                        .entry(cam.id)
                        .insert_entry((threshold, MotionGate::new(threshold)))
                        .into_mut()
                        .1
                }
            };
            let verdict = gate.update(&frame);
            if !verdict.is_motion() {
                continue;
            }
            tracing::debug!(camera = %cam.name, ?verdict, "motion -> running detector");

            let infer_start = Instant::now();
            let dets = match detectors
                .get_mut(&dkey)
                .expect("detector built above")
                .detect(&frame)
            {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(camera = %cam.name, "inference failed: {e:#}");
                    continue;
                }
            };
            status.infer(
                cam.id,
                infer_start.elapsed().as_secs_f32() * 1000.0,
                accelerator,
                &model,
            );

            let now = chrono::Local::now().timestamp();
            let labels = cam
                .detect_config
                .labels
                .as_ref()
                .unwrap_or(&settings.detect_labels);
            let min_score = cam.detect_config.min_score.unwrap_or(0.0);
            let (fw, fh) = (frame.width() as f32, frame.height() as f32);
            let wanted: Vec<_> = dets
                .iter()
                .filter(|d| labels.is_empty() || labels.iter().any(|l| l == d.label))
                .filter(|d| d.score >= min_score)
                .filter(|d| passes_zones_and_size(d, &cam.detect_config, fw, fh))
                .filter(|d| {
                    last_event
                        .get(&(cam.id, d.label))
                        .map(|t| now - t >= settings.event_cooldown_secs)
                        .unwrap_or(true)
                })
                .collect();
            // --- tracker-driven analytics: line-crossing + loitering --------
            // Runs on the full (label/score/zone-filtered) detection set, NOT
            // the cooldown-throttled `wanted`, so an object trips a tripwire even
            // while its label is in event cooldown. Active only on cameras that
            // configure a tripwire or a dwell zone (otherwise the tracker is idle).
            let analytics_on = !cam.detect_config.tripwires.is_empty()
                || cam
                    .detect_config
                    .zones
                    .iter()
                    .any(|z| z.dwell_secs.unwrap_or(0) > 0);
            if analytics_on {
                let tdets: Vec<tracker::Det> = dets
                    .iter()
                    .filter(|d| labels.is_empty() || labels.iter().any(|l| l == d.label))
                    .filter(|d| d.score >= min_score)
                    .filter(|d| passes_zones_and_size(d, &cam.detect_config, fw, fh))
                    .map(|d| tracker::Det {
                        label: d.label,
                        score: d.score,
                        // Normalize pixel boxes to frame fractions (tracks share
                        // the zone/tripwire coordinate space).
                        bbox: tracker::BBox::new(d.x1 / fw, d.y1 / fh, d.x2 / fw, d.y2 / fh),
                    })
                    .collect();
                let trk = trackers
                    .entry(cam.id)
                    .or_insert_with(|| tracker::Tracker::new(tracker::TrackerConfig::default()));
                trk.update(&tdets, now);
                let confirmed: Vec<&tracker::Track> = trk.confirmed().collect();
                let astate = analytics.entry(cam.id).or_default();
                let (crossings, loiters) = astate.tick(
                    &confirmed,
                    &cam.detect_config.tripwires,
                    &cam.detect_config.zones,
                    now,
                );
                for c in &crossings {
                    emit_analytics_event(
                        &db, &settings, &alarms, &throttle, &mqtt_tx, &snapshots_dir, &frame, cam,
                        "crossing", c.anchor, Some(&c.tripwire), Some(c.dir.as_str()), now,
                    );
                }
                for l in &loiters {
                    emit_analytics_event(
                        &db, &settings, &alarms, &throttle, &mqtt_tx, &snapshots_dir, &frame, cam,
                        "loiter", l.anchor, Some(&l.zone), None, now,
                    );
                }
            }

            if wanted.is_empty() {
                continue;
            }

            // One annotated snapshot per frame, shared by its events.
            let snap_rel = format!("{}-{}.jpg", cam.name, now);
            let snap_abs = snapshots_dir.join(&snap_rel);
            if let Err(e) = save_snapshot(&frame, &wanted, &snap_abs) {
                tracing::warn!("snapshot save failed: {e:#}");
            }

            // --- face recognition on person detections -------------------
            let mut face_names: Vec<Option<String>> = vec![None; wanted.len()];
            let face_on = cam
                .detect_config
                .face_recognize
                .unwrap_or(settings.face_recognition);
            if face_on && wanted.iter().any(|d| d.label == "person") {
                let fkey = format!(
                    "{}|{}|{}",
                    settings.face_det_model, settings.face_rec_model, settings.force_cpu
                );
                if (face_engine.is_none() || fkey != face_key)
                    && std::path::Path::new(&settings.face_det_model).exists()
                    && std::path::Path::new(&settings.face_rec_model).exists()
                {
                    match facerec::FaceEngine::new(
                        &settings.face_det_model,
                        &settings.face_rec_model,
                        settings.force_cpu,
                    ) {
                        Ok(e) => {
                            tracing::info!("face recognition ready");
                            face_engine = Some(e);
                            face_key = fkey;
                        }
                        Err(e) => tracing::warn!("face engine unavailable: {e:#}"),
                    }
                }
                if let Some(engine) = face_engine.as_mut() {
                    match run_faces(
                        engine,
                        &db,
                        &frame,
                        &wanted,
                        &mut face_names,
                        settings.face_match_threshold,
                        &snapshots_dir,
                        cam,
                        now,
                        &mut last_unknown_save,
                    ) {
                        Ok(()) => {}
                        Err(e) => tracing::debug!(camera = %cam.name, "face stage: {e:#}"),
                    }
                }
            }

            // --- license plate recognition on vehicle detections ----------
            let mut plates: Vec<Option<String>> = vec![None; wanted.len()];
            const VEHICLES: [&str; 4] = ["car", "truck", "bus", "motorcycle"];
            if crate::lpr::models_present() && wanted.iter().any(|d| VEHICLES.contains(&d.label)) {
                if lpr.is_none() {
                    match crate::lpr::PlateEngine::try_new() {
                        Ok(e) => {
                            tracing::info!("license plate recognition ready");
                            lpr = Some(e);
                        }
                        Err(e) => tracing::warn!("LPR unavailable: {e:#}"),
                    }
                }
                if let Some(engine) = lpr.as_mut() {
                    // Plates need pixels: when detecting on a low-res
                    // sub-stream, OCR the matching full-res frame instead.
                    let hires = if cam.detect_source.is_some() {
                        fetch_frame(&go2rtc.api_base(), &cam.name).ok()
                    } else {
                        None
                    };
                    let src = hires.as_ref().unwrap_or(&frame);
                    let (sx, sy) = (
                        src.width() as f32 / frame.width() as f32,
                        src.height() as f32 / frame.height() as f32,
                    );
                    // Full-frame plate pass, shared as a fallback: small
                    // vehicle crops can starve the detector of context.
                    let frame_plate = engine.detect(src, 0.5).ok().flatten();
                    for (i, d) in wanted.iter().enumerate() {
                        if !VEHICLES.contains(&d.label) {
                            continue;
                        }
                        let x = (d.x1 * sx).max(0.0) as u32;
                        let y = (d.y1 * sy).max(0.0) as u32;
                        let w = (((d.x2 - d.x1) * sx) as u32).min(src.width() - x);
                        let h = (((d.y2 - d.y1) * sy) as u32).min(src.height() - y);
                        if w < 48 || h < 48 {
                            continue;
                        }
                        let vehicle = src.crop_imm(x, y, w, h);
                        let read = match engine.detect(&vehicle, 0.5) {
                            Ok(Some(p)) => engine.read(&vehicle, &p).ok(),
                            _ => None,
                        };
                        // Fallback: a full-frame plate whose center lies in
                        // this vehicle's box.
                        let read = read.or_else(|| {
                            frame_plate.as_ref().and_then(|p| {
                                let (pcx, pcy) = ((p.x1 + p.x2) / 2.0, (p.y1 + p.y2) / 2.0);
                                let inside = pcx >= d.x1 * sx
                                    && pcx <= d.x2 * sx
                                    && pcy >= d.y1 * sy
                                    && pcy <= d.y2 * sy;
                                inside.then(|| engine.read(src, p).ok()).flatten()
                            })
                        });
                        if let Some(text) = read.filter(|t| t.len() >= 3) {
                            // Vehicle of interest gets a guaranteed high-priority
                            // push (independent of any alarm rule). The plate
                            // library wins: a "watch" entry alerts with its name;
                            // otherwise fall back to the legacy deny-list.
                            let lib = db
                                .plate_by_text(&crate::db::normalize_plate(&text))
                                .ok()
                                .flatten();
                            let interest = match &lib {
                                Some(p) => p.category == "watch",
                                None => {
                                    crate::lpr::plate_status(
                                        &text,
                                        &settings.plate_allowlist,
                                        &settings.plate_denylist,
                                    ) == crate::lpr::PlateStatus::Deny
                                }
                            };
                            if interest && !settings.health_ntfy_url.is_empty() {
                                let who = lib
                                    .as_ref()
                                    .map(|p| format!("{} — plate {text}", p.name))
                                    .unwrap_or_else(|| format!("Plate {text} (deny-list)"));
                                crate::notify::ntfy_text(
                                    &settings.health_ntfy_url,
                                    &format!("🚗 Vehicle of interest on {}", cam.name),
                                    &format!("{who} seen on {}", cam.name),
                                    "warning,oncoming_automobile",
                                );
                            }
                            plates[i] = Some(text);
                        }
                    }
                }
            }

            let mut new_event_ids: Vec<i64> = Vec::new();
            for (i, d) in wanted.iter().enumerate() {
                last_event.insert((cam.id, d.label), now);
                match db.add_event(
                    cam.id,
                    now,
                    d.label,
                    d.score,
                    [d.x1, d.y1, d.x2, d.y2],
                    Some(&snap_rel),
                    face_names[i].as_deref(),
                    plates[i].as_deref(),
                    None,
                    zone_for(d, &cam.detect_config, fw, fh).as_deref(),
                ) {
                    Ok(id) => {
                        tracing::info!(
                            camera = %cam.name,
                            label = d.label,
                            score = format!("{:.0}%", d.score * 100.0),
                            face = face_names[i].as_deref().unwrap_or("-"),
                            plate = plates[i].as_deref().unwrap_or("-"),
                            event = id,
                            "event recorded"
                        );
                        if !settings.webhook_url.is_empty() {
                            post_webhook(
                                &settings.webhook_url,
                                &settings.webhook_template,
                                &cam.name,
                                id,
                                d,
                                now,
                                &snap_rel,
                            );
                        }
                        let _ = mqtt_tx.send(crate::mqtt::EventMsg {
                            event_id: id,
                            camera: cam.name.clone(),
                            label: d.label.to_string(),
                            score: d.score,
                            ts: now,
                            snapshot: format!("/api/snapshots/{snap_rel}"),
                            topic: None,
                        });
                        // Alarm Manager: fire every matching rule's action.
                        let alarm_ev = crate::notify::AlarmEvent {
                            event_id: id,
                            camera: &cam.name,
                            label: d.label,
                            score: d.score,
                            ts: now,
                            snapshot_url: &format!("/api/snapshots/{snap_rel}"),
                            snapshot_path: Some(&snap_abs),
                            face: face_names[i].as_deref(),
                            plate: plates[i].as_deref(),
                            gesture: None,
                            transcript: None,
                            base_url: &settings.public_base_url,
                            webhook_template: &settings.webhook_template,
                            smtp: crate::notify::smtp_cfg(&settings),
                            duress: false,
                        };
                        for rule in alarms.iter().filter(|r| {
                            r.matches(
                                cam.id,
                                d.label,
                                d.score,
                                face_names[i].as_deref(),
                                plates[i].as_deref(),
                                None,
                                None,
                            ) && crate::notify::armed_in_mode(&r.modes, &settings.arm_mode)
                                && crate::notify::ready(r, &throttle, now)
                        }) {
                            crate::notify::fire(rule, &alarm_ev, &mqtt_tx);
                        }
                        new_event_ids.push(id);
                    }
                    Err(e) => tracing::warn!("event insert failed: {e:#}"),
                }
            }

            // GenAI captioning (opt-in): one job per event-frame, captioned
            // off-thread so the LLM call never stalls detection.
            if settings.genai_enabled {
                if let Some(&first) = new_event_ids.first() {
                    let _ = genai_tx.send(crate::genai::CaptionJob {
                        event_id: first,
                        snapshot_path: snap_abs.clone(),
                        label: wanted[0].label.to_string(),
                        camera: cam.name.clone(),
                    });
                }
            }

            // PTZ autotracking: steer toward the strongest detection to keep
            // it centered (Frigate-style). Runs on the raw detections so the
            // camera follows even between cooldown-throttled events.
            if cam.detect_config.autotrack {
                let capable = *ptz_capable.entry(cam.id).or_insert_with(|| {
                    crate::ptz::parse_source(&cam.source)
                        .map(|t| crate::ptz::supports_ptz(&t))
                        .unwrap_or(false)
                });
                let cooled = last_autotrack
                    .get(&cam.id)
                    .map(|t| t.elapsed() >= Duration::from_millis(1500))
                    .unwrap_or(true);
                if capable && cooled {
                    if let Some(best) = wanted.iter().filter(|d| d.score >= 0.5).max_by(|a, b| {
                        a.score
                            .partial_cmp(&b.score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    }) {
                        // Offset of the object center from frame center, -1..1.
                        let dx = ((best.x1 + best.x2) / 2.0 - fw / 2.0) / (fw / 2.0);
                        let dy = ((best.y1 + best.y2) / 2.0 - fh / 2.0) / (fh / 2.0);
                        if dx.abs() > 0.15 || dy.abs() > 0.15 {
                            if let Some(target) = crate::ptz::parse_source(&cam.source) {
                                last_autotrack.insert(cam.id, Instant::now());
                                // Velocity proportional to offset, but with a
                                // floor: real PTZ motors ignore tiny velocities
                                // over short bursts (validated on the Amcrest —
                                // 0.23 for 350 ms produced zero movement). The
                                // burst length scales with how far off-center
                                // the object is. Tilt axis is inverted
                                // (positive tilt looks up).
                                let boost = |v: f32| {
                                    if v == 0.0 {
                                        0.0
                                    } else {
                                        v.signum() * v.abs().max(0.4)
                                    }
                                };
                                let pan = if dx.abs() > 0.15 {
                                    boost(dx * 0.6)
                                } else {
                                    0.0
                                };
                                let tilt = if dy.abs() > 0.15 {
                                    boost(-dy * 0.6)
                                } else {
                                    0.0
                                };
                                let (pan, tilt) = (pan.clamp(-0.6, 0.6), tilt.clamp(-0.6, 0.6));
                                let burst = 300 + (dx.abs().max(dy.abs()) * 500.0) as u64;
                                tracing::info!(
                                    camera = %cam.name,
                                    label = best.label,
                                    pan = format!("{pan:.2}"),
                                    tilt = format!("{tilt:.2}"),
                                    burst_ms = burst,
                                    "autotrack: centering object"
                                );
                                let _ = crate::ptz::continuous_move(&target, pan, tilt, 0.0);
                                std::thread::sleep(Duration::from_millis(burst));
                                let _ = crate::ptz::stop(&target);
                            }
                        }
                    }
                }
            }

            // Smart search: one CLIP embedding per event frame (shared by its
            // events) so snapshots become text-searchable.
            if !new_event_ids.is_empty() && crate::smart::models_present() {
                if clip.is_none() {
                    match crate::smart::ImageEmbedder::try_new() {
                        Ok(e) => {
                            tracing::info!("smart search (CLIP) ready");
                            clip = Some(e);
                        }
                        Err(e) => tracing::warn!("smart search unavailable: {e:#}"),
                    }
                }
                if let Some(embedder) = clip.as_mut() {
                    match embedder.embed(&frame) {
                        Ok(emb) => {
                            for id in &new_event_ids {
                                let _ = db.set_event_embedding(*id, &emb);
                            }
                        }
                        Err(e) => tracing::debug!("clip embed failed: {e:#}"),
                    }
                }
            }
        }

        let elapsed = tick.elapsed();
        let budget = Duration::from_millis(db.settings().poll_ms);
        if elapsed < budget {
            sleep_responsive(budget - elapsed, &shutdown);
        }
    }
}

/// Detect + embed faces in the frame, match against enrolled identities, and
/// fill `face_names` for person detections whose box contains a face center.
/// Confident-but-unknown faces are saved (crop + embedding sidecar) for the
/// enrollment UI, throttled per camera.
#[allow(clippy::too_many_arguments)]
fn run_faces(
    engine: &mut facerec::FaceEngine,
    db: &Db,
    frame: &DynamicImage,
    wanted: &[&detector::Detection],
    face_names: &mut [Option<String>],
    threshold: f32,
    snapshots_dir: &std::path::Path,
    cam: &crate::db::Camera,
    now: i64,
    last_unknown_save: &mut HashMap<i64, i64>,
) -> Result<()> {
    let faces = engine.detect(frame, 0.5)?;
    if faces.is_empty() {
        return Ok(());
    }
    let enrolled = db.list_faces()?;

    for face in &faces {
        let emb = engine.embed(frame, face)?;
        let best = enrolled
            .iter()
            .map(|f| (facerec::cosine(&emb, &f.embedding), f))
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let name = match best {
            Some((sim, f)) if sim >= threshold => Some(f.name.clone()),
            _ => None,
        };

        let (fcx, fcy) = ((face.x1 + face.x2) / 2.0, (face.y1 + face.y2) / 2.0);
        let in_person = |d: &detector::Detection| {
            d.label == "person" && fcx >= d.x1 && fcx <= d.x2 && fcy >= d.y1 && fcy <= d.y2
        };
        if let Some(name) = &name {
            for (i, d) in wanted.iter().enumerate() {
                if in_person(d) {
                    face_names[i] = Some(name.clone());
                }
            }
        } else if face.score >= 0.6 {
            // A confident face that matched no enrolled identity → mark the
            // containing person as a "stranger" (unless already recognized by
            // another face in the same box — a real identity wins). Only when
            // at least one identity is enrolled: with none, *everyone* would be
            // "unknown", which is noise, not a stranger alert.
            if !enrolled.is_empty() {
                for (i, d) in wanted.iter().enumerate() {
                    if in_person(d) && face_names[i].is_none() {
                        face_names[i] = Some(crate::db::UNKNOWN_FACE.to_string());
                    }
                }
            }
            // Also save the crop for enrollment, at most one per camera per 30s.
            let due = last_unknown_save
                .get(&cam.id)
                .map(|t| now - t >= 30)
                .unwrap_or(true);
            if due {
                last_unknown_save.insert(cam.id, now);
                save_unknown_face(frame, face, &emb, snapshots_dir, &cam.name, now)?;
            }
        }
    }
    Ok(())
}

/// Crop the face (with margin) into data/faces/unknown plus an embedding
/// sidecar the enrollment endpoint can ingest without re-running the model.
fn save_unknown_face(
    frame: &DynamicImage,
    face: &facerec::Face,
    emb: &[f32],
    snapshots_dir: &std::path::Path,
    camera: &str,
    now: i64,
) -> Result<()> {
    let dir = snapshots_dir
        .parent()
        .unwrap_or(snapshots_dir)
        .join("faces")
        .join("unknown");
    std::fs::create_dir_all(&dir).ok();
    let (fw, fh) = (face.x2 - face.x1, face.y2 - face.y1);
    let margin = fw.max(fh) * 0.3;
    let x = (face.x1 - margin).max(0.0) as u32;
    let y = (face.y1 - margin).max(0.0) as u32;
    let w = ((fw + margin * 2.0) as u32).min(frame.width().saturating_sub(x));
    let h = ((fh + margin * 2.0) as u32).min(frame.height().saturating_sub(y));
    if w < 8 || h < 8 {
        return Ok(());
    }
    let name = format!("{camera}-{now}.jpg");
    frame.crop_imm(x, y, w, h).save(dir.join(&name))?;
    std::fs::write(
        dir.join(format!("{name}.json")),
        serde_json::to_string(emb)?,
    )?;
    tracing::info!(camera, file = name, "unknown face saved for enrollment");
    Ok(())
}

/// Emit a tracker-driven analytics event (`crossing` / `loiter`): snapshot the
/// frame, insert the event (with optional crossing `direction`), and dispatch
/// the global webhook + MQTT + matching alarm rules — the same notification
/// machinery a detection event uses, so analytics rides existing alerting (an
/// alarm rule with `label = "crossing"` fires on any crossing).
#[allow(clippy::too_many_arguments)]
fn emit_analytics_event(
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
    direction: Option<&str>,
    now: i64,
) {
    let (ax, ay) = anchor;
    // A small marker box around the ground-contact point for the thumbnail.
    let bbox = [
        (ax - 0.03).clamp(0.0, 1.0),
        (ay - 0.08).clamp(0.0, 1.0),
        (ax + 0.03).clamp(0.0, 1.0),
        ay.clamp(0.0, 1.0),
    ];
    let snap_rel = format!("{}-{}-{}.jpg", cam.name, now, label);
    let snap_abs = snapshots_dir.join(&snap_rel);
    if let Err(e) = frame.save(&snap_abs) {
        tracing::warn!("analytics snapshot save failed: {e:#}");
    }
    let id = match db.add_event_dir(
        cam.id, now, label, 1.0, bbox, Some(&snap_rel), None, None, None, zone, direction,
    ) {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("analytics event insert failed: {e:#}");
            return;
        }
    };
    tracing::info!(
        camera = %cam.name, kind = label, zone = zone.unwrap_or("-"),
        dir = direction.unwrap_or("-"), event = id, "analytics event"
    );
    let snap_url = format!("/api/snapshots/{snap_rel}");
    if !settings.webhook_url.is_empty() {
        let payload = serde_json::json!({
            "type": label,
            "event_id": id,
            "camera": cam.name,
            "label": label,
            "zone": zone,
            "direction": direction,
            "ts": now,
            "snapshot": snap_url,
        });
        let _ = ureq::post(&settings.webhook_url)
            .timeout(Duration::from_secs(3))
            .send_json(payload);
    }
    let _ = mqtt_tx.send(crate::mqtt::EventMsg {
        event_id: id,
        camera: cam.name.clone(),
        label: label.to_string(),
        score: 1.0,
        ts: now,
        snapshot: snap_url.clone(),
        topic: None,
    });
    let alarm_ev = crate::notify::AlarmEvent {
        event_id: id,
        camera: &cam.name,
        label,
        score: 1.0,
        ts: now,
        snapshot_url: &snap_url,
        snapshot_path: Some(&snap_abs),
        face: None,
        plate: None,
        gesture: None,
        transcript: None,
        base_url: &settings.public_base_url,
        webhook_template: &settings.webhook_template,
        smtp: crate::notify::smtp_cfg(settings),
        duress: false,
    };
    for rule in alarms.iter().filter(|r| {
        r.matches(cam.id, label, 1.0, None, None, None, None)
            && crate::notify::armed_in_mode(&r.modes, &settings.arm_mode)
            && crate::notify::ready(r, throttle, now)
    }) {
        crate::notify::fire(rule, &alarm_ev, mqtt_tx);
    }
}

/// Fire-and-forget event notification (Blue Iris alarm-server style). Runs on
/// the pipeline thread with a short timeout; a dead listener must never stall
/// detection, so failures are logged at debug and dropped.
#[allow(clippy::too_many_arguments)]
fn post_webhook(
    url: &str,
    template: &str,
    camera: &str,
    event_id: i64,
    d: &detector::Detection,
    ts: i64,
    snapshot: &str,
) {
    let snapshot_url = format!("/api/snapshots/{snapshot}");
    let result = if template.is_empty() {
        let payload = serde_json::json!({
            "type": "detection",
            "event_id": event_id,
            "camera": camera,
            "label": d.label,
            "score": d.score,
            "box": [d.x1, d.y1, d.x2, d.y2],
            "ts": ts,
            "snapshot": snapshot_url,
        });
        ureq::post(url)
            .timeout(Duration::from_secs(3))
            .send_json(payload)
    } else {
        let ev = crate::notify::AlarmEvent {
            event_id,
            camera,
            label: d.label,
            score: d.score,
            ts,
            snapshot_url: &snapshot_url,
            snapshot_path: None,
            face: None,
            plate: None,
            gesture: None,
            transcript: None,
            base_url: "",
            webhook_template: template,
            smtp: None,
            duress: false,
        };
        ureq::post(url)
            .timeout(Duration::from_secs(3))
            .set("Content-Type", "application/json")
            .send_string(&crate::notify::render_template(template, &ev))
    };
    if let Err(e) = result {
        tracing::debug!("webhook delivery failed: {e}");
    }
}

/// Apply per-camera zone and object-size gating to one detection. Returns true
/// to keep it. The anchor is the box-center in frame fractions, matching the
/// long-standing ignore-zone semantics.
///
/// Order: object-size bounds, legacy rectangle ignore zones, then polygon zones
/// — a polygon `Ignore` zone drops the detection, and if any `Required` zone
/// applies to its label the anchor must fall inside one of them.
fn passes_zones_and_size(
    d: &detector::Detection,
    cfg: &crate::db::DetectConfig,
    fw: f32,
    fh: f32,
) -> bool {
    let cx = (d.x1 + d.x2) / 2.0 / fw;
    let cy = (d.y1 + d.y2) / 2.0 / fh;

    // Object-size gate (fraction of frame area).
    if cfg.min_area.is_some() || cfg.max_area.is_some() {
        let area = ((d.x2 - d.x1).max(0.0) * (d.y2 - d.y1).max(0.0)) / (fw * fh).max(1.0);
        if cfg.min_area.is_some_and(|m| area < m) || cfg.max_area.is_some_and(|m| area > m) {
            return false;
        }
    }

    // Legacy rectangle ignore zones.
    if cfg.ignore_zones.iter().any(|z| z.contains(cx, cy)) {
        return false;
    }

    // Polygon ignore zones that apply to this label.
    if cfg
        .zones
        .iter()
        .filter(|z| z.kind == crate::db::ZoneKind::Required)
        .any(|z| z.applies_to(d.label))
    {
        // Required zones exist for this label → the anchor must be in one.
        let inside_required = cfg
            .zones
            .iter()
            .filter(|z| z.kind == crate::db::ZoneKind::Required && z.applies_to(d.label))
            .any(|z| z.contains(cx, cy));
        if !inside_required {
            return false;
        }
    }
    if cfg.zones.iter().any(|z| {
        z.kind == crate::db::ZoneKind::Ignore && z.applies_to(d.label) && z.contains(cx, cy)
    }) {
        return false;
    }
    true
}

/// Human label for the execution provider a detector is using on this OS.
fn accel_label(force_cpu: bool) -> &'static str {
    if force_cpu {
        "CPU"
    } else if cfg!(target_os = "windows") {
        "DirectML"
    } else if cfg!(target_os = "macos") {
        "CoreML"
    } else if cfg!(target_os = "linux") {
        "CUDA"
    } else {
        "GPU"
    }
}

/// The name of the (required) zone a detection's anchor falls in, for tagging
/// the event so review can filter by zone. `None` when not in a named zone.
fn zone_for(
    d: &detector::Detection,
    cfg: &crate::db::DetectConfig,
    fw: f32,
    fh: f32,
) -> Option<String> {
    let cx = (d.x1 + d.x2) / 2.0 / fw;
    let cy = (d.y1 + d.y2) / 2.0 / fh;
    cfg.zones
        .iter()
        .find(|z| {
            z.kind == crate::db::ZoneKind::Required && z.applies_to(d.label) && z.contains(cx, cy)
        })
        .map(|z| z.name.clone())
}

/// Black out the privacy-mask polygons (frame-fraction coordinates) in place.
fn apply_privacy_masks(frame: &mut DynamicImage, masks: &[Vec<[f32; 2]>]) {
    let mut img = frame.to_rgb8();
    let (w, h) = (img.width(), img.height());
    for mask in masks {
        if mask.len() < 3 {
            continue;
        }
        // Only scan each polygon's bounding box.
        let xs = mask.iter().map(|p| p[0]);
        let ys = mask.iter().map(|p| p[1]);
        let x0 = (xs.clone().fold(1.0f32, f32::min) * w as f32)
            .floor()
            .max(0.0) as u32;
        let x1 = (xs.fold(0.0f32, f32::max) * w as f32).ceil().min(w as f32) as u32;
        let y0 = (ys.clone().fold(1.0f32, f32::min) * h as f32)
            .floor()
            .max(0.0) as u32;
        let y1 = (ys.fold(0.0f32, f32::max) * h as f32).ceil().min(h as f32) as u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                if crate::db::point_in_polygon(mask, fx, fy) {
                    img.put_pixel(x, y, Rgb([0, 0, 0]));
                }
            }
        }
    }
    *frame = DynamicImage::ImageRgb8(img);
}

fn sleep_responsive(total: Duration, shutdown: &AtomicBool) {
    let start = Instant::now();
    while start.elapsed() < total && !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Pull one decoded keyframe from go2rtc as JPEG. go2rtc only decodes when
/// asked, so sampling at ~1 fps is far cheaper than decoding the full stream.
fn fetch_frame(api_base: &str, camera: &str) -> Result<DynamicImage> {
    let url = format!("{api_base}/api/frame.jpeg?src={camera}");
    let resp = ureq::get(&url)
        .timeout(Duration::from_secs(5))
        .call()
        .with_context(|| format!("fetching frame for {camera}"))?;
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(MAX_FRAME_BYTES)
        .read_to_end(&mut bytes)
        .context("reading frame body")?;
    image::load_from_memory(&bytes).context("decoding frame JPEG")
}

/// Save the frame with red detection boxes burned in.
fn save_snapshot(
    frame: &DynamicImage,
    dets: &[&detector::Detection],
    path: &std::path::Path,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut img = frame.to_rgb8();
    for d in dets {
        draw_rect(&mut img, d.x1 as i64, d.y1 as i64, d.x2 as i64, d.y2 as i64);
    }
    img.save(path)
        .with_context(|| format!("writing {}", path.display()))
}

fn draw_rect(img: &mut image::RgbImage, x1: i64, y1: i64, x2: i64, y2: i64) {
    const COLOR: Rgb<u8> = Rgb([255, 40, 40]);
    const THICKNESS: i64 = 3;
    let (w, h) = (img.width() as i64, img.height() as i64);
    let mut put = |x: i64, y: i64| {
        if x >= 0 && x < w && y >= 0 && y < h {
            img.put_pixel(x as u32, y as u32, COLOR);
        }
    };
    for t in 0..THICKNESS {
        for x in x1..=x2 {
            put(x, y1 + t);
            put(x, y2 - t);
        }
        for y in y1..=y2 {
            put(x1 + t, y);
            put(x2 - t, y);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::passes_zones_and_size;
    use crate::db::{DetectConfig, PolyZone, Zone, ZoneKind};
    use detector::Detection;

    /// A detection whose box center is (cx, cy) in a 100x100 frame, sized w×h.
    fn det_at(label: &'static str, cx: f32, cy: f32, w: f32, h: f32) -> Detection {
        Detection {
            label,
            class: 0,
            score: 0.9,
            x1: cx - w / 2.0,
            y1: cy - h / 2.0,
            x2: cx + w / 2.0,
            y2: cy + h / 2.0,
        }
    }

    #[test]
    fn legacy_rect_ignore_zone_drops_center_hits_only() {
        let cfg = DetectConfig {
            ignore_zones: vec![Zone {
                x: 0.8,
                y: 0.0,
                w: 0.2,
                h: 1.0,
            }],
            ..Default::default()
        };
        assert!(!passes_zones_and_size(
            &det_at("person", 90.0, 50.0, 4.0, 4.0),
            &cfg,
            100.0,
            100.0
        ));
        assert!(passes_zones_and_size(
            &det_at("person", 50.0, 50.0, 4.0, 4.0),
            &cfg,
            100.0,
            100.0
        ));
        assert!(passes_zones_and_size(
            &det_at("person", 90.0, 50.0, 4.0, 4.0),
            &DetectConfig::default(),
            100.0,
            100.0
        ));
    }

    #[test]
    fn required_zone_keeps_only_inside_for_that_label() {
        let cfg = DetectConfig {
            zones: vec![PolyZone {
                name: "driveway".into(),
                points: vec![[0.0, 0.0], [0.5, 0.0], [0.5, 1.0], [0.0, 1.0]],
                kind: ZoneKind::Required,
                labels: vec!["person".into()],
                dwell_secs: None,
            }],
            ..Default::default()
        };
        // Person inside the left-half required zone: kept; outside: dropped.
        assert!(passes_zones_and_size(
            &det_at("person", 25.0, 50.0, 4.0, 4.0),
            &cfg,
            100.0,
            100.0
        ));
        assert!(!passes_zones_and_size(
            &det_at("person", 75.0, 50.0, 4.0, 4.0),
            &cfg,
            100.0,
            100.0
        ));
        // A car is unconstrained by a person-only required zone.
        assert!(passes_zones_and_size(
            &det_at("car", 75.0, 50.0, 4.0, 4.0),
            &cfg,
            100.0,
            100.0
        ));
    }

    #[test]
    fn polygon_ignore_zone_drops_inside() {
        let cfg = DetectConfig {
            zones: vec![PolyZone {
                name: "sidewalk".into(),
                points: vec![[0.5, 0.0], [1.0, 0.0], [1.0, 1.0], [0.5, 1.0]],
                kind: ZoneKind::Ignore,
                labels: vec![],
                dwell_secs: None,
            }],
            ..Default::default()
        };
        assert!(!passes_zones_and_size(
            &det_at("person", 75.0, 50.0, 4.0, 4.0),
            &cfg,
            100.0,
            100.0
        ));
        assert!(passes_zones_and_size(
            &det_at("person", 25.0, 50.0, 4.0, 4.0),
            &cfg,
            100.0,
            100.0
        ));
    }

    #[test]
    fn object_size_gate() {
        let cfg = DetectConfig {
            min_area: Some(0.01), // ≥ 1% of frame
            max_area: Some(0.5),  // ≤ 50% of frame
            ..Default::default()
        };
        // 4x4 in 100x100 = 0.0016 -> too small.
        assert!(!passes_zones_and_size(
            &det_at("person", 50.0, 50.0, 4.0, 4.0),
            &cfg,
            100.0,
            100.0
        ));
        // 20x20 = 0.04 -> ok.
        assert!(passes_zones_and_size(
            &det_at("person", 50.0, 50.0, 20.0, 20.0),
            &cfg,
            100.0,
            100.0
        ));
        // 90x90 = 0.81 -> too big.
        assert!(!passes_zones_and_size(
            &det_at("person", 50.0, 50.0, 90.0, 90.0),
            &cfg,
            100.0,
            100.0
        ));
    }
}
