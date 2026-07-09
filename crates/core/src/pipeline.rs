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

/// Gait (#64): bounded per-track body-sample buffer and how long a track lingers
/// in the gait map after the tracker stops reporting it.
const GAIT_SAMPLE_CAP: usize = 64;
const GAIT_RETIRE_MS: i64 = 30_000;

/// Stationary-object suppression: minimum IoU between a detection box and a
/// confirmed track's box to treat them as the same physical object.
const STATIONARY_MATCH_IOU: f32 = 0.3;
/// Stationary-object suppression: how far an already-alerted object's ground
/// anchor must move (frame-fraction Euclidean distance) before it re-alerts —
/// above per-frame box jitter, below a real repositioning.
const STATIONARY_MOVE_FRAC: f32 = 0.05;
/// Stationary-object suppression: a confirmed track that went unobserved for at
/// least this many frames before re-matching is treated as a NEW object, not a
/// continuation. The tracker keeps a vacated track alive (up to `max_age`) and
/// can re-associate a *different* object that later occupies the same spot to the
/// old id; without this guard that new arrival would inherit the old track's
/// stale alert anchor and be wrongly suppressed. A 1-frame detector flicker on a
/// continuously-present object stays below this, so it doesn't re-fire.
const STATIONARY_REACQUIRE_GAP: u32 = 2;

/// Has an alerted object's anchor moved at least `thresh` (frame fractions) from
/// where it last fired? `None` (never alerted) always counts as moved, so a
/// newly-matched track fires once. Pure so it can be unit-tested.
fn moved_enough(prev: Option<(f32, f32)>, cur: (f32, f32), thresh: f32) -> bool {
    match prev {
        None => true,
        Some((px, py)) => {
            let (dx, dy) = (cur.0 - px, cur.1 - py);
            (dx * dx + dy * dy).sqrt() >= thresh
        }
    }
}

/// Stationary-suppression decision for a detection matched to confirmed track
/// `id` at ground anchor `cur`. Returns whether to KEEP (fire) the event and
/// records the new anchor on a keep. `reacquired` means the track had a
/// continuity gap before this frame (its prior occupant likely left and a
/// different object took its place), so its stale anchor is ignored and it fires
/// as new. Pure (mutates only the passed map) so it can be unit-tested.
fn stationary_keep(
    alerted: &mut HashMap<u64, (f32, f32)>,
    id: u64,
    cur: (f32, f32),
    reacquired: bool,
    move_thresh: f32,
) -> bool {
    let prev = if reacquired {
        None
    } else {
        alerted.get(&id).copied()
    };
    if moved_enough(prev, cur, move_thresh) {
        alerted.insert(id, cur);
        true
    } else {
        false
    }
}

/// Whether a detection box `dbox` (frame fractions) of class `label` should fire
/// an event under stationary suppression: match it to its best-IoU confirmed
/// track of the same label in this frame's `sup_tracks` snapshot
/// `(id, label, box, reacquired)`, then defer to [`stationary_keep`]. A detection
/// with no confirmed-track match (brand-new / tentative object) fires (fail-open).
/// This is the actual per-detection decision the pipeline applies — factored out
/// so it can be integration-tested against a real [`tracker::Tracker`].
fn stationary_should_fire(
    dbox: tracker::BBox,
    label: &str,
    sup_tracks: &[(u64, String, tracker::BBox, bool)],
    alerted: &mut HashMap<u64, (f32, f32)>,
) -> bool {
    let best = sup_tracks
        .iter()
        .filter(|(_, lbl, _, _)| lbl.as_str() == label)
        .map(|(id, _, bb, reacq)| (*id, bb.iou(&dbox), *reacq))
        .filter(|(_, iou, _)| *iou >= STATIONARY_MATCH_IOU)
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    match best {
        None => true,
        Some((id, _, reacquired)) => {
            stationary_keep(alerted, id, dbox.anchor(), reacquired, STATIONARY_MOVE_FRAC)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    db: Db,
    go2rtc: Arc<Go2Rtc>,
    snapshots_dir: PathBuf,
    status: StatusBoard,
    mqtt_tx: std::sync::mpsc::Sender<crate::mqtt::EventMsg>,
    throttle: crate::notify::AlarmThrottle,
    genai_tx: std::sync::mpsc::Sender<crate::genai::Job>,
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
    // P2.2 prompt-rule text embeddings, cached per rule (re-embedded only when
    // the prompt text changes) so a standing rule costs one CLIP text run ever.
    let mut prompt_embs: HashMap<i64, (String, Vec<f32>)> = HashMap::new();
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
    // Per-camera residential analytics memory (zone-enter, child/adult, fall,
    // still-in-water). Same lifecycle as `analytics`.
    let mut residential: HashMap<i64, crate::residential::ResidentialState> = HashMap::new();
    // Per-camera tamper / defocus / scene-change gate (#63), on cameras opted
    // into `tamper_detect`.
    let mut tamper_gates: HashMap<i64, crate::tamper::TamperGate> = HashMap::new();
    // Per-camera gait accumulation (#64), on cameras opted into `gait_identify`.
    let mut gait_states: HashMap<i64, crate::gait::GaitState> = HashMap::new();
    // Per-camera parcel presence (package-delivered / -removed monitoring).
    let mut packages: HashMap<i64, crate::parcel::PackageState> = HashMap::new();
    // Stationary-object suppression (`suppress_stationary`): per camera, the
    // ground-anchor (frame fractions) at which each confirmed track last fired an
    // event. A track already in here that hasn't moved past `STATIONARY_MOVE_FRAC`
    // is suppressed (a parked car re-tripping the gate via ambient motion).
    let mut alerted_tracks: HashMap<i64, HashMap<u64, (f32, f32)>> = HashMap::new();

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
        // Prune per-camera analytics state for cameras that were deleted (ids are
        // never reused), so a long-running process doesn't hold a tracker +
        // trajectory buffers for a camera that's gone.
        {
            let live: std::collections::HashSet<i64> = cameras.iter().map(|c| c.id).collect();
            trackers.retain(|k, _| live.contains(k));
            analytics.retain(|k, _| live.contains(k));
            residential.retain(|k, _| live.contains(k));
            tamper_gates.retain(|k, _| live.contains(k));
            gait_states.retain(|k, _| live.contains(k));
            alerted_tracks.retain(|k, _| live.contains(k));
            // Drop parcel state for cameras that are gone OR have package
            // detection off, so toggling it back on starts from a clean slate
            // (a stale `present` state would otherwise fire a spurious
            // `package_removed` on the first absent frame after re-enabling).
            packages.retain(|k, _| {
                cameras
                    .iter()
                    .any(|c| c.id == *k && c.detect_config.package_detect)
            });
        }
        // Gait (#64): load enrolled profiles once per tick (not per camera/frame)
        // when any camera uses gait, parsed to fixed-length signatures for matching.
        let gait_profiles: Vec<(String, crate::gait::GaitSignature)> = if cameras
            .iter()
            .any(|c| c.enabled && c.detect && c.detect_config.gait_identify)
        {
            db.gait_profile_sigs()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(n, v)| {
                    <[f32; crate::gait::GAIT_DIMS]>::try_from(v)
                        .ok()
                        .map(|s| (n, s))
                })
                .collect()
        } else {
            Vec::new()
        };
        let gait_prof_sigs: Vec<crate::gait::GaitSignature> =
            gait_profiles.iter().map(|(_, s)| *s).collect();
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

            // Tamper / defocus / scene-change watchdog (#63). Runs on the RAW
            // frame (before privacy masks, so a large mask can't read as a
            // blackout) and regardless of motion (a covered lens produces no
            // motion). On a state transition it fires a `tamper` event + an
            // in-app/phone notification.
            if cam.detect_config.tamper_detect {
                let thumb = crate::tamper::thumb_of(&frame);
                let gate = tamper_gates
                    .entry(cam.id)
                    .or_insert_with(|| crate::tamper::TamperGate::new(Default::default()));
                if let Some(ev) = gate.update(&thumb) {
                    // The gate analyzes the RAW frame, but the saved snapshot must
                    // honor privacy masks — so mask a clone for the snapshot when
                    // any mask is set (transitions are rare, so the clone is cheap).
                    let now_ts = chrono::Local::now().timestamp();
                    if cam.detect_config.privacy_masks.is_empty() {
                        handle_tamper_event(
                            &db,
                            &settings,
                            &alarms,
                            &throttle,
                            &mqtt_tx,
                            &snapshots_dir,
                            &frame,
                            cam,
                            ev,
                            now_ts,
                        );
                    } else {
                        let mut masked = frame.clone();
                        apply_privacy_masks(&mut masked, &cam.detect_config.privacy_masks);
                        handle_tamper_event(
                            &db,
                            &settings,
                            &alarms,
                            &throttle,
                            &mqtt_tx,
                            &snapshots_dir,
                            &masked,
                            cam,
                            ev,
                            now_ts,
                        );
                    }
                }
                // Publish the live tamper state so /api/status + the UI can show it.
                status.set_tamper(cam.id, gate.state().map(|k| k.as_str().to_string()));
            } else if tamper_gates.remove(&cam.id).is_some() {
                // Tamper just turned off: forget stale state so re-enabling
                // re-learns the baseline, and clear the status indicator.
                status.set_tamper(cam.id, None);
            }
            // Gait (#64): forget a camera's accumulated walking state the moment
            // gait is turned off (mirrors the tamper disable-cleanup above; the
            // per-tick deletion-retain only covers removed cameras).
            if !cam.detect_config.gait_identify {
                gait_states.remove(&cam.id);
            }
            // Same for stationary suppression: drop a camera's last-alerted
            // anchors when the feature is off, so re-enabling starts clean.
            if !cam.detect_config.suppress_stationary {
                alerted_tracks.remove(&cam.id);
            }

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
            // Capture WHERE the motion was (for the snapshot highlight) right
            // after the diff, while the gate's mask is fresh — an owned Vec so we
            // can release the gate borrow. Only when a snapshot will actually be
            // drawn (motion frame + the global highlight setting on).
            let motion_boxes: Vec<[f32; 4]> = if settings.highlight_motion && verdict.is_motion() {
                gate.motion_regions()
            } else {
                Vec::new()
            };
            // Tracker-driven analytics (line-crossing / loitering) must keep
            // advancing even on motionless frames: a person standing still
            // produces no pixel change yet is exactly what a loiter alert
            // targets. So on cameras that configure a tripwire or a dwell zone,
            // run the detector even when the motion gate sees no motion (regular
            // per-object event emission is still suppressed below).
            let analytics_on = !cam.detect_config.tripwires.is_empty()
                || cam.detect_config.fall_detect
                || cam.detect_config.zones.iter().any(|z| {
                    z.dwell_secs.unwrap_or(0) > 0
                        || z.occupancy_max.unwrap_or(0) > 0
                        // Residential zone flags also need the tracker running, even
                        // on motionless frames (a child standing still in a danger
                        // zone, a person lying motionless after a fall).
                        || z.alert_enter
                        || z.child_watch
                        || z.supervise
                        || z.water
                });
            // Gait identification (#64) also needs the tracker running for person
            // tracks (even across motionless frames, to keep the trajectory).
            let gait_on = cam.detect_config.gait_identify;
            // Stationary-object suppression also drives the tracker: it needs
            // persistent IDs to tell a re-detected parked car from a new arrival,
            // and the tracker must keep advancing on motionless frames so a
            // still object's track stays alive (and `misses` reset) between the
            // ambient-motion frames that would otherwise re-fire it.
            let suppress_stationary = cam.detect_config.suppress_stationary;
            let tracker_on = analytics_on || gait_on || suppress_stationary;
            // Parcel monitoring must also keep sampling motionless frames: a
            // package sits perfectly still, and we still need to notice it appear
            // (delivered) and later vanish (removed).
            let package_on = cam.detect_config.package_detect;
            if !verdict.is_motion() && !tracker_on && !package_on {
                continue;
            }
            tracing::debug!(camera = %cam.name, ?verdict, "running detector");

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
            let mut wanted: Vec<_> = dets
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
            // --- tracker-driven analytics + gait ----------------------------
            // Runs on the full (label/score/zone-filtered) detection set, NOT
            // the cooldown-throttled `wanted`, so an object trips a tripwire even
            // while its label is in event cooldown. Active on cameras that
            // configure a tripwire / dwell zone OR opt into gait.
            // Confirmed person tracks with a computed gait signature, as
            // `(frame-fraction box, identity, signature_json)`, for attributing
            // the gait identity to this frame's person events below.
            let mut gait_attr: Vec<(tracker::BBox, String, Option<String>)> = Vec::new();
            // Stationary suppression: owned snapshot of this frame's confirmed
            // tracks `(id, label, box, reacquired)` plus every live track id,
            // captured inside the tracker block (where the borrow is held) for the
            // `wanted` filter applied once we know an event would fire. `reacquired`
            // marks a track that had a continuity gap before this frame, so a new
            // object inheriting a vacated track's id isn't wrongly suppressed.
            // Empty unless opted in.
            let mut sup_tracks: Vec<(u64, String, tracker::BBox, bool)> = Vec::new();
            let mut sup_live: std::collections::HashSet<u64> = std::collections::HashSet::new();
            if tracker_on {
                let tdets: Vec<tracker::Det> = dets
                    .iter()
                    .filter(|d| labels.is_empty() || labels.iter().any(|l| l == d.label))
                    .filter(|d| d.score >= min_score)
                    // size-only gate (NOT passes_zones_and_size): zone/tripwire
                    // membership is handled inside analytics, so a Required/Ignore
                    // dwell zone must not drop objects from the tracker feed.
                    .filter(|d| passes_size(d, &cam.detect_config, fw, fh))
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
                // Feed the tracker millisecond timestamps so speed estimation has
                // a continuous time base (whole-second `now` would quantise dt and
                // skew km/h at ~1 fps). Track lifecycle is hit/miss-count based, so
                // the unit is irrelevant to confirmation/retirement; only history
                // (consumed by `track_speed_kmh`) cares, and it wants millis.
                let now_ms = chrono::Local::now().timestamp_millis();
                // Capture each existing track's consecutive-miss count BEFORE the
                // update re-matches it (update resets misses to 0 on a match), so
                // stationary suppression can tell a continuously-present object
                // from a vacated track that a different object just re-occupied.
                let prev_misses: HashMap<u64, u32> = if suppress_stationary {
                    trk.tracks().iter().map(|t| (t.id, t.misses)).collect()
                } else {
                    HashMap::new()
                };
                trk.update(&tdets, now_ms);
                let confirmed: Vec<&tracker::Track> = trk.confirmed().collect();

                // Snapshot the tracker state for stationary suppression before
                // its borrow ends (matching set = confirmed tracks; pruning set =
                // every live id so an occluded-but-alive track keeps its anchor).
                // `reacquired` = the track had a gap of >= STATIONARY_REACQUIRE_GAP
                // frames before this frame re-matched it → treat as a new object.
                if suppress_stationary {
                    sup_tracks = confirmed
                        .iter()
                        .map(|t| {
                            let reacquired = prev_misses.get(&t.id).copied().unwrap_or(0)
                                >= STATIONARY_REACQUIRE_GAP;
                            (t.id, t.label.clone(), t.bbox, reacquired)
                        })
                        .collect();
                    sup_live = trk.tracks().iter().map(|t| t.id).collect();
                }

                // Gait identification (#64): accumulate body samples for each
                // confirmed person track and attribute an identity (an enrolled
                // name, or the `?` unknown sentinel) once enough walking is seen.
                if gait_on {
                    let gst = gait_states.entry(cam.id).or_default();
                    for t in &confirmed {
                        if t.label == "person" {
                            gst.observe(
                                t.id,
                                [t.bbox.x1, t.bbox.y1, t.bbox.x2, t.bbox.y2],
                                now_ms,
                                GAIT_SAMPLE_CAP,
                            );
                        }
                    }
                    gst.retire_stale(now_ms, GAIT_RETIRE_MS);
                    let params = crate::gait::GaitParams::default();
                    for t in &confirmed {
                        if t.label != "person" {
                            continue;
                        }
                        let Some(buf) = gst.get(t.id) else { continue };
                        let Some(sig) = crate::gait::signature(&buf.samples, &params) else {
                            continue;
                        };
                        let name = match crate::gait::best_match(
                            &sig,
                            &gait_prof_sigs,
                            params.match_threshold,
                        ) {
                            Some((i, _)) => gait_profiles[i].0.clone(),
                            None => crate::db::UNKNOWN_GAIT.to_string(),
                        };
                        if let Some(b) = gst.get_mut(t.id) {
                            b.identity = Some(name.clone());
                        }
                        let sig_json = serde_json::to_string(&sig.to_vec()).ok();
                        gait_attr.push((t.bbox, name, sig_json));
                    }
                }

                // Analytics emits (line-crossing / loiter / occupancy) only when
                // the camera actually configures them; the tracker above may have
                // run solely for gait.
                if analytics_on {
                    // Build the ground-plane homography from the camera's optional
                    // calibration (cheap 8x8 solve; rebuilt each processed frame).
                    let homography = cam.detect_config.ground_calib.as_ref().and_then(|c| {
                        tracker::Homography::from_quad(
                            [
                                (c.points[0][0], c.points[0][1]),
                                (c.points[1][0], c.points[1][1]),
                                (c.points[2][0], c.points[2][1]),
                                (c.points[3][0], c.points[3][1]),
                            ],
                            c.width_m,
                            c.height_m,
                        )
                    });
                    let astate = analytics.entry(cam.id).or_default();
                    let (crossings, loiters, occupancy) = astate.tick(
                        &confirmed,
                        &cam.detect_config.tripwires,
                        &cam.detect_config.zones,
                        homography.as_ref(),
                        now,
                    );
                    for c in &crossings {
                        let label = if c.wrong_way { "wrong_way" } else { "crossing" };
                        emit_analytics_event(
                            &db,
                            &settings,
                            &alarms,
                            &throttle,
                            &mqtt_tx,
                            &snapshots_dir,
                            &frame,
                            cam,
                            label,
                            c.anchor,
                            Some(&c.tripwire),
                            Some(c.dir.as_str()),
                            c.speed_kmh,
                            now,
                        );
                    }
                    for l in &loiters {
                        emit_analytics_event(
                            &db,
                            &settings,
                            &alarms,
                            &throttle,
                            &mqtt_tx,
                            &snapshots_dir,
                            &frame,
                            cam,
                            "loiter",
                            l.anchor,
                            Some(&l.zone),
                            None,
                            None,
                            now,
                        );
                    }
                    // Publish the live occupancy gauge to the status board (cleared to
                    // empty when the camera has no zones) and fire an edge-triggered
                    // `occupancy` event for any zone that just exceeded its limit.
                    // Zone names aren't unique, so SUM same-named zones rather than
                    // letting one silently overwrite another, and skip unnamed zones.
                    let mut gauge: std::collections::HashMap<String, u32> =
                        std::collections::HashMap::new();
                    for o in &occupancy {
                        if !o.zone.is_empty() {
                            *gauge.entry(o.zone.clone()).or_insert(0) += o.count;
                        }
                    }
                    status.set_occupancy(cam.id, gauge);
                    for (zo, zone) in occupancy.iter().zip(cam.detect_config.zones.iter()) {
                        if zo.over {
                            // Use the zone's vertex centroid as the snapshot marker.
                            let anchor = if zone.points.is_empty() {
                                (0.5, 0.5)
                            } else {
                                let n = zone.points.len() as f32;
                                let (sx, sy) = zone
                                    .points
                                    .iter()
                                    .fold((0.0f32, 0.0f32), |(ax, ay), p| (ax + p[0], ay + p[1]));
                                (sx / n, sy / n)
                            };
                            emit_analytics_event(
                                &db,
                                &settings,
                                &alarms,
                                &throttle,
                                &mqtt_tx,
                                &snapshots_dir,
                                &frame,
                                cam,
                                "occupancy",
                                anchor,
                                Some(&zo.zone),
                                None,
                                None,
                                now,
                            );
                        }
                    }

                    // --- residential analytics (zone-enter, child/adult, fall,
                    // still-in-water). Each ResEvent rides the same emit path, so its
                    // label + zone flow through Alarm Manager (zone_like), webhook and
                    // MQTT exactly like a crossing/loiter/occupancy event.
                    let rstate = residential.entry(cam.id).or_default();
                    for ev in rstate.tick(
                        &confirmed,
                        &cam.detect_config.zones,
                        cam.detect_config.child_height_frac,
                        cam.detect_config.fall_detect,
                        now,
                    ) {
                        emit_analytics_event(
                            &db,
                            &settings,
                            &alarms,
                            &throttle,
                            &mqtt_tx,
                            &snapshots_dir,
                            &frame,
                            cam,
                            &ev.label,
                            ev.anchor,
                            ev.zone.as_deref(),
                            None,
                            None,
                            now,
                        );
                    }
                } // if analytics_on
            } // if tracker_on

            // --- parcel monitoring: package delivered / removed -------------
            // Runs on the full detection set (independent of motion + cooldown):
            // a parcel-like object that persists in the zone fires `package`, and
            // one that then disappears fires `package_removed`.
            if package_on {
                let cfg = &cam.detect_config;
                // A degenerate polygon (<3 points) can't contain anything, so
                // treat it as "no zone" (whole frame) rather than silently
                // disabling detection.
                let zone = cfg.package_zone.as_deref().filter(|z| z.len() >= 3);
                let mut anchor: Option<(f32, f32)> = None;
                let in_zone = dets.iter().any(|d| {
                    if !crate::parcel::matches_package(d.label, &cfg.package_labels)
                        || d.score < min_score
                    {
                        return false;
                    }
                    let c = ((d.x1 + d.x2) / 2.0 / fw, (d.y1 + d.y2) / 2.0 / fh);
                    let inside = zone.is_none_or(|z| crate::db::point_in_polygon(z, c.0, c.1));
                    if inside && anchor.is_none() {
                        anchor = Some(c);
                    }
                    inside
                });
                let pstate = packages.entry(cam.id).or_default();
                if let Some(ev) = pstate.update(
                    in_zone,
                    now,
                    crate::parcel::CONFIRM_SECS,
                    crate::parcel::GONE_SECS,
                ) {
                    let label = match ev {
                        crate::parcel::PackageEvent::Delivered => "package",
                        crate::parcel::PackageEvent::Removed => "package_removed",
                    };
                    // Mark the parcel (delivered) or, since it's already gone on
                    // removal, the zone centroid.
                    let mark = anchor.unwrap_or_else(|| zone_centroid(zone));
                    emit_analytics_event(
                        &db,
                        &settings,
                        &alarms,
                        &throttle,
                        &mqtt_tx,
                        &snapshots_dir,
                        &frame,
                        cam,
                        label,
                        mark,
                        None,
                        None,
                        None,
                        now,
                    );
                }
            }

            // A motionless frame ran the tracker/analytics above, but the motion
            // gate's job is to suppress regular per-object detection events — so
            // skip the rest of the event path when there was no motion.
            if !verdict.is_motion() {
                continue;
            }
            if wanted.is_empty() {
                continue;
            }

            // Stationary-object suppression: now that an event WOULD fire, drop
            // any detection that's an already-alerted, non-moving object. Match
            // each detection to its best-IoU confirmed track of the same label;
            // a track we've alerted before that hasn't moved past the threshold is
            // suppressed. A new/tentative object (no confirmed track yet) fires
            // (fail-open), preserving first-arrival latency. The per-label
            // cooldown still rate-limits the genuinely-moving objects that pass.
            if suppress_stationary {
                let alerted = alerted_tracks.entry(cam.id).or_default();
                alerted.retain(|id, _| sup_live.contains(id));
                wanted.retain(|d| {
                    let dbox = tracker::BBox::new(d.x1 / fw, d.y1 / fh, d.x2 / fw, d.y2 / fh);
                    stationary_should_fire(dbox, d.label, &sup_tracks, alerted)
                });
                if wanted.is_empty() {
                    continue;
                }
            }

            // One annotated snapshot per frame, shared by its events.
            let snap_rel = format!("{}-{}.jpg", cam.name, now);
            let snap_abs = snapshots_dir.join(&snap_rel);
            if let Err(e) = save_snapshot(&frame, &wanted, &motion_boxes, &snap_abs) {
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
            // Per new event: id + pixel box (to embed its object crop for
            // cross-camera Re-ID) + the event context the prompt-rule pass
            // (P2.2) needs to gate/fire on the same crop embedding.
            let mut crop_jobs: Vec<CropJob> = Vec::new();
            for (i, d) in wanted.iter().enumerate() {
                last_event.insert((cam.id, d.label), now);
                // The (required) zone this detection sits in, computed once and
                // reused for both the event record and the alarm `zone_like` gate.
                let ev_zone = zone_for(d, &cam.detect_config, fw, fh);
                match db.add_event(
                    cam.id,
                    now,
                    d.label,
                    d.score,
                    // Store the box as 0..1 frame fractions (like zones, masks and
                    // analytics events) so it survives resolution/sub-stream
                    // changes and is directly comparable across the events table.
                    [d.x1 / fw, d.y1 / fh, d.x2 / fw, d.y2 / fh],
                    Some(&snap_rel),
                    face_names[i].as_deref(),
                    plates[i].as_deref(),
                    None,
                    ev_zone.as_deref(),
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
                        let severity = crate::severity::severity_for(
                            d.label,
                            face_names[i].as_deref(),
                            None,
                        );
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
                            speed: None,
                            base_url: &settings.public_base_url,
                            webhook_template: &settings.webhook_template,
                            smtp: crate::notify::smtp_cfg(&settings),
                            duress: false,
                            severity,
                            min_push_severity: settings.notify_min_severity,
                            caption: None,
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
                            ) && r.zone_ok(ev_zone.as_deref())
                                && r.confirm_ok(&db, cam.id, now)
                                && crate::notify::armed_in_mode(&r.modes, &settings.arm_mode)
                                && crate::notify::ready(r, &throttle, now)
                        }) {
                            // Deferred fires: a rule with a vlm_prompt is verified
                            // by the vision model OFF this detection thread (the
                            // call is multi-second), and a `describe` rule is
                            // captioned there first so the description rides in
                            // the push. The GenAI worker fires either iff/when
                            // ready (VLM fails OPEN). All the cheap gates (incl.
                            // cooldown via `ready` above) already passed. Plain
                            // rules fire inline as before.
                            let suppressed = crate::notify::take_suppressed(&throttle, rule.id);
                            let deferred = rule
                                .vlm_prompt
                                .as_deref()
                                .is_some_and(|p| !p.trim().is_empty())
                                || (rule.describe && settings.genai_enabled);
                            if deferred {
                                let _ = genai_tx.send(crate::genai::Job::VlmGate(Box::new(
                                    crate::genai::VlmGateJob {
                                        rule: rule.clone(),
                                        event_id: id,
                                        camera: cam.name.clone(),
                                        label: d.label.to_string(),
                                        score: d.score,
                                        ts: now,
                                        snapshot_url: format!("/api/snapshots/{snap_rel}"),
                                        snapshot_path: snap_abs.clone(),
                                        face: face_names[i].clone(),
                                        plate: plates[i].clone(),
                                        severity,
                                        suppressed,
                                    },
                                )));
                            } else {
                                crate::notify::fire(rule, &alarm_ev, &mqtt_tx, suppressed);
                            }
                        }
                        // Gait (#64): attribute an identity to this person event
                        // from the best-overlapping confirmed person track.
                        if gait_on && d.label == "person" && !gait_attr.is_empty() {
                            let dbox =
                                tracker::BBox::new(d.x1 / fw, d.y1 / fh, d.x2 / fw, d.y2 / fh);
                            let best = gait_attr
                                .iter()
                                .map(|(b, name, sj)| (b.iou(&dbox), name, sj))
                                .filter(|(iou, _, _)| *iou > 0.3)
                                .max_by(|a, b| {
                                    a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
                                });
                            if let Some((_, name, sj)) = best {
                                let _ = db.set_event_gait(id, Some(name), sj.as_deref());
                            }
                        }
                        new_event_ids.push(id);
                        crop_jobs.push(CropJob {
                            event_id: id,
                            bbox_px: [d.x1, d.y1, d.x2, d.y2],
                            score: d.score,
                            label: d.label.to_string(),
                            face: face_names[i].clone(),
                            plate: plates[i].clone(),
                            zone: ev_zone.clone(),
                        });
                    }
                    Err(e) => tracing::warn!("event insert failed: {e:#}"),
                }
            }

            // Activity signal for the Live grid's sort — stamped once per
            // event-bearing frame.
            if !new_event_ids.is_empty() {
                status.detection(cam.id, now);
            }

            // GenAI captioning (opt-in): one job per event-frame, captioned
            // off-thread so the LLM call never stalls detection.
            if settings.genai_enabled {
                if let Some(&first) = new_event_ids.first() {
                    let _ = genai_tx.send(crate::genai::Job::Caption(crate::genai::CaptionJob {
                        event_id: first,
                        snapshot_path: snap_abs.clone(),
                        label: wanted[0].label.to_string(),
                        camera: cam.name.clone(),
                    }));
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

            // Smart search + appearance search: one CLIP embedding of the event
            // frame (shared, makes snapshots text-searchable) PLUS one of each
            // object's crop (cross-camera Re-ID — find the same person/vehicle on
            // other cameras). Both reuse the single loaded CLIP session.
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
                        Ok(frame_emb) => {
                            // The frame embedding (one CLIP run, text search) is
                            // written for every event. The per-object crop
                            // embedding (Re-ID) costs an extra CLIP run EACH, so
                            // cap it per frame: a crowded scene must never block
                            // the single shared detection thread with unbounded
                            // inferences. `crop_jobs` is in detector (≈score)
                            // order, so the cap keeps the most confident objects.
                            const MAX_CROPS_PER_FRAME: usize = 6;
                            if crop_jobs.len() > MAX_CROPS_PER_FRAME {
                                tracing::debug!(
                                    camera = %cam.name,
                                    objects = crop_jobs.len(),
                                    "capping Re-ID crop embeddings to {MAX_CROPS_PER_FRAME}"
                                );
                            }
                            // Fire each prompt rule at most once per frame,
                            // even when several crops match it.
                            let mut fired_prompt_rules: std::collections::HashSet<i64> =
                                std::collections::HashSet::new();
                            for (i, job) in crop_jobs.iter().enumerate() {
                                let crop_emb = if i < MAX_CROPS_PER_FRAME {
                                    embed_crop(embedder, &frame, &job.bbox_px)
                                } else {
                                    None
                                };
                                let _ = db.set_event_embeddings(
                                    job.event_id,
                                    &frame_emb,
                                    crop_emb.as_deref(),
                                );
                                // P2.2 prompt-based standing rules: does this
                                // object *look like* any rule's description?
                                if let Some(emb) = &crop_emb {
                                    fire_prompt_alarms(
                                        &db,
                                        &settings,
                                        &alarms,
                                        &throttle,
                                        &mqtt_tx,
                                        &mut prompt_embs,
                                        &mut fired_prompt_rules,
                                        cam,
                                        job,
                                        emb,
                                        &snap_rel,
                                        &snap_abs,
                                        now,
                                    );
                                }
                            }
                        }
                        Err(e) => tracing::debug!("clip embed failed: {e:#}"),
                    }
                }
            }
        }

        let elapsed = tick.elapsed();
        // Reuse this tick's already-fetched `settings` (line ~172) instead of a
        // second full Settings deserialize + KV read under the global DB mutex
        // every tick; a mid-tick poll_ms change simply applies one tick later.
        let budget = Duration::from_millis(settings.poll_ms);
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
/// Vertex centroid of a polygon (0..1 fractions), defaulting to frame center.
fn zone_centroid(poly: Option<&[[f32; 2]]>) -> (f32, f32) {
    match poly {
        Some(p) if !p.is_empty() => {
            let n = p.len() as f32;
            let (sx, sy) = p
                .iter()
                .fold((0.0f32, 0.0f32), |(ax, ay), q| (ax + q[0], ay + q[1]));
            (sx / n, sy / n)
        }
        _ => (0.5, 0.5),
    }
}

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
    speed: Option<f32>,
    now: i64,
) -> Option<i64> {
    let (ax, ay) = anchor;
    // A small marker box around the ground-contact point for the thumbnail.
    let bbox = [
        (ax - 0.03).clamp(0.0, 1.0),
        (ay - 0.08).clamp(0.0, 1.0),
        (ax + 0.03).clamp(0.0, 1.0),
        ay.clamp(0.0, 1.0),
    ];
    // Privacy/dignity: a no-clip camera (nursery/bedroom/bathroom) fires the alert
    // without writing any image — no snapshot on disk, in MQTT or in email.
    let snap_rel = format!("{}-{}-{}.jpg", cam.name, now, label);
    let snap_abs = snapshots_dir.join(&snap_rel);
    let snapshot: Option<String> = if cam.detect_config.no_clip {
        None
    } else {
        match frame.save(&snap_abs) {
            Ok(()) => Some(snap_rel.clone()),
            Err(e) => {
                tracing::warn!("analytics snapshot save failed: {e:#}");
                None
            }
        }
    };
    let id = match db.add_event_dir(
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
        direction,
        speed,
    ) {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("analytics event insert failed: {e:#}");
            return None;
        }
    };
    tracing::info!(
        camera = %cam.name, kind = label, zone = zone.unwrap_or("-"),
        dir = direction.unwrap_or("-"),
        speed = speed.map(|s| format!("{s:.0}km/h")).unwrap_or_else(|| "-".into()),
        event = id, "analytics event"
    );
    let snap_url = snapshot
        .as_ref()
        .map(|s| format!("/api/snapshots/{s}"))
        .unwrap_or_default();
    if !settings.webhook_url.is_empty() {
        let payload = serde_json::json!({
            "type": label,
            "event_id": id,
            "camera": cam.name,
            "label": label,
            "zone": zone,
            "direction": direction,
            "speed": speed,
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
        snapshot_path: snapshot.as_ref().map(|_| snap_abs.as_path()),
        face: None,
        plate: None,
        gesture: None,
        transcript: None,
        speed,
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
        crate::notify::fire(rule, &alarm_ev, mqtt_tx, suppressed);
    }
    Some(id)
}

/// Handle a camera-tamper state transition (#63): on entry, create a `tamper`
/// event (riding the alarm/webhook/MQTT machinery) and a notification + phone
/// push; on recovery, just notify. The `tamper` kind is carried in the event's
/// zone field and the notification text.
#[allow(clippy::too_many_arguments)]
fn handle_tamper_event(
    db: &Db,
    settings: &crate::db::Settings,
    alarms: &[crate::db::AlarmRule],
    throttle: &crate::notify::AlarmThrottle,
    mqtt_tx: &std::sync::mpsc::Sender<crate::mqtt::EventMsg>,
    snapshots_dir: &std::path::Path,
    frame: &DynamicImage,
    cam: &crate::db::Camera,
    ev: crate::tamper::TamperEvent,
    now: i64,
) {
    let kind = ev.kind.as_str();
    let kind_label = match ev.kind {
        crate::tamper::TamperKind::Blackout => "lens covered / blacked out",
        crate::tamper::TamperKind::Defocus => "image defocused / smeared",
        crate::tamper::TamperKind::SceneChange => "camera moved / redirected",
    };
    if ev.entered {
        tracing::warn!(camera = %cam.name, kind, "camera tamper detected");
        // Event + alarm/webhook/MQTT, with the kind in the zone slot.
        let event_id = emit_analytics_event(
            db,
            settings,
            alarms,
            throttle,
            mqtt_tx,
            snapshots_dir,
            frame,
            cam,
            "tamper",
            (0.5, 0.5),
            Some(kind),
            None,
            None,
            now,
        );
        let title = "Camera tampering";
        let msg = format!("{}: {kind_label}", cam.name);
        let _ = db.add_notification(now, "tamper", title, Some(&msg), event_id);
        let url = settings.health_ntfy_url.trim();
        if !url.is_empty() {
            crate::notify::ntfy_text(url, title, &msg, "rotating_light");
        }
    } else {
        tracing::info!(camera = %cam.name, kind, "camera recovered from tamper");
        let title = "Camera tampering cleared";
        let msg = format!("{} recovered ({kind_label})", cam.name);
        let _ = db.add_notification(now, "tamper_cleared", title, Some(&msg), None);
        let url = settings.health_ntfy_url.trim();
        if !url.is_empty() {
            crate::notify::ntfy_text(url, title, &msg, "white_check_mark");
        }
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
            speed: None,
            base_url: "",
            webhook_template: template,
            smtp: None,
            duress: false,
            severity: crate::severity::severity_for(d.label, None, None),
            min_push_severity: 1,
            caption: None,
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

/// Object-size gate only (fraction of frame area) — used to feed the tracker the
/// real objects WITHOUT the Required/Ignore zone-membership filter. Zone and
/// tripwire membership is an analytics concern handled inside
/// `AnalyticsState::tick` (via `contains`), so a dwell zone drawn as `Ignore`
/// (or a `Required` zone elsewhere) must not silently drop objects from tracking.
fn passes_size(d: &detector::Detection, cfg: &crate::db::DetectConfig, fw: f32, fh: f32) -> bool {
    if cfg.min_area.is_none() && cfg.max_area.is_none() {
        return true;
    }
    let area = ((d.x2 - d.x1).max(0.0) * (d.y2 - d.y1).max(0.0)) / (fw * fh).max(1.0);
    !(cfg.min_area.is_some_and(|m| area < m) || cfg.max_area.is_some_and(|m| area > m))
}

/// Human label for the execution provider a detector is using on this OS.
/// Embed an object's crop from the frame for cross-camera appearance search.
/// `b` is a pixel box `[x1, y1, x2, y2]`. Returns `None` when the box is too
/// small to carry meaningful appearance (a few pixels upscaled to CLIP's 224 is
/// just noise) or falls outside the frame.
fn embed_crop(
    embedder: &mut crate::smart::ImageEmbedder,
    frame: &DynamicImage,
    b: &[f32; 4],
) -> Option<Vec<f32>> {
    let x = b[0].max(0.0) as u32;
    let y = b[1].max(0.0) as u32;
    let w = (b[2] - b[0]).max(0.0) as u32;
    let h = (b[3] - b[1]).max(0.0) as u32;
    if w < 24 || h < 24 {
        return None;
    }
    let w = w.min(frame.width().saturating_sub(x));
    let h = h.min(frame.height().saturating_sub(y));
    if w < 24 || h < 24 {
        return None;
    }
    embedder.embed(&frame.crop_imm(x, y, w, h)).ok()
}

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

/// One new event's crop-embedding work order: the pixel box for the CLIP crop
/// plus the event context the prompt-rule pass needs to gate and fire.
struct CropJob {
    event_id: i64,
    bbox_px: [f32; 4],
    score: f32,
    label: String,
    face: Option<String>,
    plate: Option<String>,
    zone: Option<String>,
}

/// Cosine similarity at/above which an object crop "looks like" a prompt.
/// CLIP text↔image cosines for a true match typically land 0.28–0.35 while
/// unrelated pairs sit ≤0.2, so 0.27 is deliberately conservative (alerts must
/// not cry wolf). Near-misses are logged at debug for tuning.
const PROMPT_FIRE_COSINE: f32 = 0.27;

/// P2.2 — prompt-based standing NL rules (Reolink "Prompt-Based Alerts"): fire
/// every prompt rule whose CLIP text embedding is cosine-similar to this
/// detection's crop embedding. Runs on the detection thread right where the
/// crop embedding is produced (no extra CLIP image run); each rule's text is
/// embedded once and cached. All the normal gates (camera/label/zone scope,
/// schedule, modes, cooldown, cross-modal confirm) still apply.
#[allow(clippy::too_many_arguments)]
fn fire_prompt_alarms(
    db: &Db,
    settings: &crate::db::Settings,
    alarms: &[crate::db::AlarmRule],
    throttle: &crate::notify::AlarmThrottle,
    mqtt_tx: &std::sync::mpsc::Sender<crate::mqtt::EventMsg>,
    prompt_embs: &mut HashMap<i64, (String, Vec<f32>)>,
    fired_this_frame: &mut std::collections::HashSet<i64>,
    cam: &crate::db::Camera,
    job: &CropJob,
    crop_emb: &[f32],
    snap_rel: &str,
    snap_abs: &std::path::Path,
    now: i64,
) {
    for rule in alarms.iter().filter(|r| r.is_prompt_rule()) {
        if fired_this_frame.contains(&rule.id) {
            continue;
        }
        // Cheap gates first — don't spend a CLIP text run on a rule that could
        // never fire for this event anyway.
        if !rule.matches_prompt(
            cam.id,
            &job.label,
            job.score,
            job.face.as_deref(),
            job.plate.as_deref(),
        ) || !rule.zone_ok(job.zone.as_deref())
            || !crate::notify::armed_in_mode(&rule.modes, &settings.arm_mode)
        {
            continue;
        }
        let prompt = rule.prompt_like.as_deref().unwrap_or("").trim().to_string();
        // (Re-)embed the prompt only when its text changed since the cache.
        if prompt_embs
            .get(&rule.id)
            .map(|(p, _)| p != &prompt)
            .unwrap_or(true)
        {
            match crate::smart::embed_text(&prompt) {
                Ok(emb) => {
                    prompt_embs.insert(rule.id, (prompt.clone(), emb));
                }
                Err(e) => {
                    tracing::debug!(rule = %rule.name, "prompt embed failed: {e:#}");
                    continue;
                }
            }
        }
        let Some((_, prompt_emb)) = prompt_embs.get(&rule.id) else {
            continue;
        };
        let sim = crate::smart::cosine(prompt_emb, crop_emb);
        if sim < PROMPT_FIRE_COSINE {
            if sim > PROMPT_FIRE_COSINE - 0.05 {
                tracing::debug!(
                    rule = %rule.name, event = job.event_id, sim = format!("{sim:.3}"),
                    "prompt rule near-miss (below {PROMPT_FIRE_COSINE})"
                );
            }
            continue;
        }
        if !rule.confirm_ok(db, cam.id, now) || !crate::notify::ready(rule, throttle, now) {
            continue;
        }
        fired_this_frame.insert(rule.id);
        let suppressed = crate::notify::take_suppressed(throttle, rule.id);
        let matched = format!("Matched \"{prompt}\" ({:.0}% similar)", sim * 100.0);
        let ev = crate::notify::AlarmEvent {
            event_id: job.event_id,
            camera: &cam.name,
            label: &job.label,
            score: job.score,
            ts: now,
            snapshot_url: &format!("/api/snapshots/{snap_rel}"),
            snapshot_path: Some(snap_abs),
            face: job.face.as_deref(),
            plate: job.plate.as_deref(),
            gesture: None,
            transcript: None,
            speed: None,
            base_url: &settings.public_base_url,
            webhook_template: &settings.webhook_template,
            smtp: crate::notify::smtp_cfg(settings),
            duress: false,
            severity: crate::severity::severity_for(&job.label, job.face.as_deref(), None)
                .max(3),
            min_push_severity: settings.notify_min_severity,
            caption: Some(&matched),
        };
        tracing::info!(
            rule = %rule.name, event = job.event_id, sim = format!("{sim:.3}"),
            "prompt rule fired"
        );
        crate::notify::fire(rule, &ev, mqtt_tx, suppressed);
    }
}

/// Black out the privacy-mask polygons (frame-fraction coordinates) in place.
/// Shared with the side-channel snapshot paths (gesture / soft-trigger / audio
/// frame grabs), which fetch raw frames from go2rtc and would otherwise leak
/// masked regions into pushes/webhooks.
pub(crate) fn apply_privacy_masks(frame: &mut DynamicImage, masks: &[Vec<[f32; 2]>]) {
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

/// Object detection box color (red).
const DETECT_COLOR: Rgb<u8> = Rgb([255, 40, 40]);
/// Motion-region highlight color (amber) — visually distinct from the red
/// detection boxes so a viewer can tell "what moved" from "what was detected".
const MOTION_COLOR: Rgb<u8> = Rgb([255, 176, 0]);

/// Save the frame with the motion region(s) that tripped the gate burned in
/// (amber, drawn first) and the matched detection boxes on top (red).
fn save_snapshot(
    frame: &DynamicImage,
    dets: &[&detector::Detection],
    motion_boxes: &[[f32; 4]],
    path: &std::path::Path,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut img = frame.to_rgb8();
    let (w, h) = (img.width() as f32, img.height() as f32);
    // Motion boxes are 0..1 fractions; scale to pixels and draw under the
    // detection boxes (thinner, so an overlapping detection stays legible).
    for b in motion_boxes {
        draw_rect(
            &mut img,
            (b[0] * w) as i64,
            (b[1] * h) as i64,
            (b[2] * w) as i64,
            (b[3] * h) as i64,
            MOTION_COLOR,
            2,
        );
    }
    for d in dets {
        draw_rect(
            &mut img,
            d.x1 as i64,
            d.y1 as i64,
            d.x2 as i64,
            d.y2 as i64,
            DETECT_COLOR,
            3,
        );
    }
    img.save(path)
        .with_context(|| format!("writing {}", path.display()))
}

fn draw_rect(
    img: &mut image::RgbImage,
    x1: i64,
    y1: i64,
    x2: i64,
    y2: i64,
    color: Rgb<u8>,
    thickness: i64,
) {
    let (w, h) = (img.width() as i64, img.height() as i64);
    let mut put = |x: i64, y: i64| {
        if x >= 0 && x < w && y >= 0 && y < h {
            img.put_pixel(x as u32, y as u32, color);
        }
    };
    for t in 0..thickness {
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
    use super::{
        moved_enough, passes_zones_and_size, stationary_keep, stationary_should_fire,
        STATIONARY_MOVE_FRAC, STATIONARY_REACQUIRE_GAP,
    };
    use crate::db::{DetectConfig, PolyZone, ZoneKind};
    use detector::Detection;
    use std::collections::HashMap;

    /// Drive a real tracker one frame exactly like the pipeline does, then apply
    /// the production stationary decision to each detection. Returns the per-
    /// detection fire/suppress result. Mirrors the pipeline's prev-miss capture →
    /// update → confirmed snapshot (with the re-acquisition flag) → prune → filter.
    fn run_frame(
        trk: &mut tracker::Tracker,
        alerted: &mut HashMap<u64, (f32, f32)>,
        dets: &[(&'static str, tracker::BBox)],
        ts: i64,
    ) -> Vec<bool> {
        let prev_misses: HashMap<u64, u32> =
            trk.tracks().iter().map(|t| (t.id, t.misses)).collect();
        let tdets: Vec<tracker::Det> = dets
            .iter()
            .map(|(l, b)| tracker::Det {
                label: l,
                score: 0.9,
                bbox: *b,
            })
            .collect();
        trk.update(&tdets, ts);
        let sup_tracks: Vec<(u64, String, tracker::BBox, bool)> = trk
            .confirmed()
            .map(|t| {
                let reacq =
                    prev_misses.get(&t.id).copied().unwrap_or(0) >= STATIONARY_REACQUIRE_GAP;
                (t.id, t.label.clone(), t.bbox, reacq)
            })
            .collect();
        let live: std::collections::HashSet<u64> = trk.tracks().iter().map(|t| t.id).collect();
        alerted.retain(|id, _| live.contains(id));
        dets.iter()
            .map(|(l, b)| stationary_should_fire(*b, l, &sup_tracks, alerted))
            .collect()
    }

    #[test]
    fn integration_parked_car_suppresses_then_new_arrival_fires() {
        // End-to-end through a REAL tracker: reproduce "8 identical parked-car
        // events" and the adversarial-review re-acquisition hole in one go.
        let mut trk = tracker::Tracker::new(tracker::TrackerConfig::default());
        let mut alerted: HashMap<u64, (f32, f32)> = HashMap::new();
        let car = tracker::BBox::new(0.55, 0.30, 0.80, 0.62); // the parked SUV box
        let dets = [("car", car)];

        // Frames 0..15: the car sits parked (same box every frame, ambient motion
        // re-running detection each tick). It must NOT spam an event per frame.
        let mut fires = 0;
        for f in 0..15i64 {
            let fired = run_frame(&mut trk, &mut alerted, &dets, f * 1000);
            if fired[0] {
                fires += 1;
            }
            if f >= 5 {
                // Well past confirmation: steady-state suppression, zero events.
                assert!(!fired[0], "parked car must be suppressed at frame {f}");
            }
        }
        // Only the brief pre-confirmation transient fired (fail-open until the
        // track confirms, then the first confirmed alert) — not one per frame.
        assert!(
            fires <= 3,
            "parked car fired {fires} times; expected the bounded arrival transient"
        );

        // The car leaves: 5 frames with no detection. The track lingers (misses
        // grow but stay < max_age = 30), so its id survives — the exact condition
        // for the inheritance bug.
        for f in 15..20i64 {
            run_frame(&mut trk, &mut alerted, &[], f * 1000);
        }

        // A DIFFERENT car pulls into the SAME spot. It re-associates to the old
        // id, but the re-acquisition guard treats it as new → it MUST fire, not be
        // suppressed by the previous occupant's stale anchor.
        let fired = run_frame(&mut trk, &mut alerted, &dets, 20 * 1000);
        assert!(
            fired[0],
            "a new arrival in a just-vacated spot must fire (re-acquisition guard)"
        );
    }

    #[test]
    fn moved_enough_first_alert_always_fires() {
        // No prior alert (a freshly-matched track) → fires.
        assert!(moved_enough(None, (0.5, 0.5), STATIONARY_MOVE_FRAC));
    }

    #[test]
    fn moved_enough_stationary_object_is_suppressed() {
        // Same spot (within jitter) → suppressed.
        assert!(!moved_enough(
            Some((0.50, 0.50)),
            (0.51, 0.49),
            STATIONARY_MOVE_FRAC
        ));
    }

    #[test]
    fn moved_enough_real_move_re_alerts() {
        // Object slid well past the threshold → fires again.
        assert!(moved_enough(
            Some((0.20, 0.20)),
            (0.40, 0.40),
            STATIONARY_MOVE_FRAC
        ));
        // Exactly at the threshold distance also fires (>=).
        assert!(moved_enough(
            Some((0.0, 0.0)),
            (STATIONARY_MOVE_FRAC, 0.0),
            STATIONARY_MOVE_FRAC
        ));
    }

    #[test]
    fn stationary_keep_fires_new_track_then_suppresses_repeat() {
        let mut alerted: HashMap<u64, (f32, f32)> = HashMap::new();
        // First sighting of track 7: no prior anchor → fires and records.
        assert!(stationary_keep(
            &mut alerted,
            7,
            (0.30, 0.60),
            false,
            STATIONARY_MOVE_FRAC
        ));
        assert_eq!(alerted.get(&7), Some(&(0.30, 0.60)));
        // Same track, still parked → suppressed.
        assert!(!stationary_keep(
            &mut alerted,
            7,
            (0.31, 0.59),
            false,
            STATIONARY_MOVE_FRAC
        ));
        // It then drives off (moves a lot) → re-fires and the anchor advances.
        assert!(stationary_keep(
            &mut alerted,
            7,
            (0.70, 0.60),
            false,
            STATIONARY_MOVE_FRAC
        ));
        assert_eq!(alerted.get(&7), Some(&(0.70, 0.60)));
    }

    #[test]
    fn stationary_keep_reacquired_track_is_treated_as_new() {
        // Regression for the track-id-inheritance bug: a vacated track id is
        // re-associated to a DIFFERENT object that took the same spot. Even though
        // the stored anchor matches the spot, `reacquired` must force a fire so the
        // genuinely-new arrival isn't dropped.
        let mut alerted: HashMap<u64, (f32, f32)> = HashMap::new();
        alerted.insert(9, (0.40, 0.80)); // prior occupant alerted here
                                         // Same id, same spot, but reacquired after a gap → fires (not suppressed).
        assert!(stationary_keep(
            &mut alerted,
            9,
            (0.40, 0.80),
            true,
            STATIONARY_MOVE_FRAC
        ));
        // And the new occupant's anchor is recorded, so its own repeats suppress.
        assert!(!stationary_keep(
            &mut alerted,
            9,
            (0.40, 0.80),
            false,
            STATIONARY_MOVE_FRAC
        ));
    }

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
    fn required_zone_keeps_only_inside_for_that_label() {
        let cfg = DetectConfig {
            zones: vec![PolyZone {
                name: "driveway".into(),
                points: vec![[0.0, 0.0], [0.5, 0.0], [0.5, 1.0], [0.0, 1.0]],
                kind: ZoneKind::Required,
                labels: vec!["person".into()],
                dwell_secs: None,
                occupancy_max: None,
                ..Default::default()
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
                occupancy_max: None,
                ..Default::default()
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
