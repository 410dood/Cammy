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

pub fn run(
    db: Db,
    go2rtc: Arc<Go2Rtc>,
    snapshots_dir: PathBuf,
    status: StatusBoard,
    mqtt_tx: std::sync::mpsc::Sender<crate::mqtt::EventMsg>,
    shutdown: Arc<AtomicBool>,
) {
    let mut detector: Option<Detector> = None;
    let mut detector_key = String::new();
    let mut face_engine: Option<facerec::FaceEngine> = None;
    let mut face_key = String::new();
    let mut clip: Option<crate::smart::ImageEmbedder> = None;
    // Throttle unknown-face crops: one per camera per 30s, or enrollment
    // would drown in near-duplicates.
    let mut last_unknown_save: HashMap<i64, i64> = HashMap::new();
    // Per-camera motion gate, keyed with the threshold it was built for so a
    // settings or per-camera-config change rebuilds it.
    let mut gates: HashMap<i64, (f32, MotionGate)> = HashMap::new();
    let mut last_event: HashMap<(i64, &'static str), i64> = HashMap::new();

    while !shutdown.load(Ordering::Relaxed) {
        let tick = Instant::now();
        let settings = db.settings();

        // Rebuild the ONNX session if model/EP/thresholds changed.
        let key = format!(
            "{}|{}|{}|{}",
            settings.model_path, settings.force_cpu, settings.confidence, settings.nms_iou
        );
        if detector.is_none() || key != detector_key {
            match Detector::new(
                &settings.model_path,
                settings.force_cpu,
                settings.confidence,
                settings.nms_iou,
            ) {
                Ok(d) => {
                    tracing::info!(model = %settings.model_path, "detector ready");
                    detector = Some(d);
                    detector_key = key;
                }
                Err(e) => {
                    tracing::error!("detector unavailable (retrying in 30s): {e:#}");
                    sleep_responsive(Duration::from_secs(30), &shutdown);
                    continue;
                }
            }
        }

        let cameras = db.list_cameras().unwrap_or_default();
        for cam in cameras.iter().filter(|c| c.enabled && c.detect) {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            // Sample the low-res sub-stream when one is configured.
            let stream_key = match cam.detect_source.as_deref().filter(|s| !s.is_empty()) {
                Some(_) => format!("{}_sub", cam.name),
                None => cam.name.clone(),
            };
            let frame = match fetch_frame(&go2rtc.api_base(), &stream_key) {
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

            let dets = match detector
                .as_mut()
                .expect("detector built above")
                .detect(&frame)
            {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(camera = %cam.name, "inference failed: {e:#}");
                    continue;
                }
            };

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
                .filter(|d| {
                    !in_ignore_zone(
                        &cam.detect_config.ignore_zones,
                        (d.x1 + d.x2) / 2.0 / fw,
                        (d.y1 + d.y2) / 2.0 / fh,
                    )
                })
                .filter(|d| {
                    last_event
                        .get(&(cam.id, d.label))
                        .map(|t| now - t >= settings.event_cooldown_secs)
                        .unwrap_or(true)
                })
                .collect();
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
            if settings.face_recognition && wanted.iter().any(|d| d.label == "person") {
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
                ) {
                    Ok(id) => {
                        tracing::info!(
                            camera = %cam.name,
                            label = d.label,
                            score = format!("{:.0}%", d.score * 100.0),
                            face = face_names[i].as_deref().unwrap_or("-"),
                            event = id,
                            "event recorded"
                        );
                        if !settings.webhook_url.is_empty() {
                            post_webhook(&settings.webhook_url, &cam.name, id, d, now, &snap_rel);
                        }
                        let _ = mqtt_tx.send(crate::mqtt::EventMsg {
                            event_id: id,
                            camera: cam.name.clone(),
                            label: d.label.to_string(),
                            score: d.score,
                            ts: now,
                            snapshot: format!("/api/snapshots/{snap_rel}"),
                        });
                        new_event_ids.push(id);
                    }
                    Err(e) => tracing::warn!("event insert failed: {e:#}"),
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
        if let Some(name) = &name {
            for (i, d) in wanted.iter().enumerate() {
                if d.label == "person" && fcx >= d.x1 && fcx <= d.x2 && fcy >= d.y1 && fcy <= d.y2 {
                    face_names[i] = Some(name.clone());
                }
            }
        } else if face.score >= 0.6 {
            // Save for enrollment, at most one crop per camera per 30s.
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

/// Fire-and-forget event notification (Blue Iris alarm-server style). Runs on
/// the pipeline thread with a short timeout; a dead listener must never stall
/// detection, so failures are logged at debug and dropped.
fn post_webhook(
    url: &str,
    camera: &str,
    event_id: i64,
    d: &detector::Detection,
    ts: i64,
    snapshot: &str,
) {
    let payload = serde_json::json!({
        "type": "detection",
        "event_id": event_id,
        "camera": camera,
        "label": d.label,
        "score": d.score,
        "box": [d.x1, d.y1, d.x2, d.y2],
        "ts": ts,
        "snapshot": format!("/api/snapshots/{snapshot}"),
    });
    if let Err(e) = ureq::post(url)
        .timeout(Duration::from_secs(3))
        .send_json(payload)
    {
        tracing::debug!("webhook delivery failed: {e}");
    }
}

/// True when a detection's box center (frame fractions) lands inside any
/// ignore zone — e.g. the busy street at the edge of a driveway camera.
fn in_ignore_zone(zones: &[crate::db::Zone], cx: f32, cy: f32) -> bool {
    zones.iter().any(|z| z.contains(cx, cy))
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
    use super::in_ignore_zone;
    use crate::db::Zone;

    #[test]
    fn ignore_zone_drops_center_hits_only() {
        let zones = vec![Zone {
            x: 0.8,
            y: 0.0,
            w: 0.2,
            h: 1.0,
        }];
        assert!(in_ignore_zone(&zones, 0.9, 0.5)); // street strip on the right
        assert!(!in_ignore_zone(&zones, 0.5, 0.5)); // driveway center
        assert!(!in_ignore_zone(&[], 0.9, 0.5)); // no zones -> nothing ignored
    }
}
