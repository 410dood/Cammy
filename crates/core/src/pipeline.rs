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
    shutdown: Arc<AtomicBool>,
) {
    let mut detector: Option<Detector> = None;
    let mut detector_key = String::new();
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
            let frame = match fetch_frame(&go2rtc.api_base(), &cam.name) {
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

            for d in &wanted {
                last_event.insert((cam.id, d.label), now);
                match db.add_event(
                    cam.id,
                    now,
                    d.label,
                    d.score,
                    [d.x1, d.y1, d.x2, d.y2],
                    Some(&snap_rel),
                ) {
                    Ok(id) => {
                        tracing::info!(
                            camera = %cam.name,
                            label = d.label,
                            score = format!("{:.0}%", d.score * 100.0),
                            event = id,
                            "event recorded"
                        );
                        if !settings.webhook_url.is_empty() {
                            post_webhook(&settings.webhook_url, &cam.name, id, d, now, &snap_rel);
                        }
                    }
                    Err(e) => tracing::warn!("event insert failed: {e:#}"),
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
