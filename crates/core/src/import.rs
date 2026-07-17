//! P3.10 — offline footage import ("virtual camera").
//!
//! Import a phone / dashcam / offline video file into the searchable archive by
//! running the SAME detection stack the live pipeline uses over the file's frames,
//! producing ordinary event rows + annotated snapshots.
//!
//! A "virtual camera" is just a normal [`Camera`] row whose `source` is
//! `virtual:<slug>` and `enabled = false`. Because every live loop filters on
//! `enabled` — go2rtc `desired_streams` (`.filter(|c| c.enabled)`), the recorder's
//! `desired` set (`c.enabled && c.record`), the detection pipeline
//! (`c.enabled && c.detect`), the health watcher (`if !cam.enabled { continue }`)
//! and the MQTT HA-discovery publisher (`.filter(|c| c.enabled)`) — a virtual
//! camera is skipped by ALL of them automatically, exactly like any paused
//! camera, with zero changes to those files.
//!
//! v0 is **events + annotated snapshots only** (no remux into the recordings
//! layout). Rationale: keeping segments out of the recordings dir avoids all the
//! retention / segment-indexing interactions (the recorder indexes + prunes every
//! `c.record` camera's dir), so the import stays a self-contained, correct read
//! path. Imported events flow through the normal events list / filters / search
//! like any other event; their clips resolve as "snapshot only" (the same honest
//! state the UI already shows for any event whose covering footage was pruned).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::db::{Camera, Db, DetectConfig};
use crate::proc::NoConsole as _;

/// Camera-source scheme marking a virtual (imported-footage) camera. A `virtual:`
/// source is never a real stream — the camera is created `enabled = false` so
/// go2rtc / recorder / pipeline all skip it — so this prefix is purely a tag plus
/// the guard that an import never clobbers a REAL camera row of the same name.
pub const VIRTUAL_SCHEME: &str = "virtual:";

/// One analyzed frame per this many seconds of footage (matches the live
/// pipeline's ~1 fps sampling).
const SAMPLE_FPS: u32 = 1;

/// Hard cap on frames analyzed in one import — guards a huge file from running for
/// hours and filling the disk with snapshots. ~2 h of footage at 1 fps.
const MAX_FRAMES: u32 = 7200;

/// Whether a camera source string denotes a virtual (imported) camera.
pub fn is_virtual_source(source: &str) -> bool {
    source.starts_with(VIRTUAL_SCHEME)
}

/// Summary returned to the caller after an import run.
#[derive(Debug, Clone, Serialize)]
pub struct ImportSummary {
    pub camera_id: i64,
    pub camera: String,
    pub frames_scanned: usize,
    pub events_created: usize,
}

/// Tunables for an import run.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Wall-clock timestamp (unix secs) for frame 0; frame `k` is stamped
    /// `base + k/fps`. `None` = now(), so imported events appear at import time
    /// (and stay clear of the global event-retention prune). A `base_ts` far in
    /// the past is honored, but such events may be pruned by `event_retention_days`.
    pub base_ts: Option<i64>,
}

/// Turn free-form text into a valid camera name (`valid_name`: 1–32 chars of
/// `a-z 0-9 - _`). Non-conforming characters collapse to `-`; leading/trailing
/// `-` are trimmed; an empty result falls back to `"import"`.
pub fn slugify(name: &str) -> String {
    let mut s: String = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if s.len() > 32 {
        s.truncate(32);
    }
    let trimmed = s.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "import".to_string()
    } else {
        trimmed
    }
}

/// Import a server-local video `path` as events on a virtual camera named after
/// `camera_name`. Creates (or reuses) the `virtual:<slug>` camera row, drives
/// ffmpeg to sample ~1 frame/sec, runs the same detector + label/min-score/zone
/// filters as the live pipeline on each sampled frame, and emits an event (with an
/// annotated snapshot) per wanted detection. Timestamps are `base + offset` so the
/// footage lands on a real timeline. Bounded by [`MAX_FRAMES`]; logs progress.
///
/// Runs synchronously and is CPU/ffmpeg-heavy — call it from `spawn_blocking`.
pub fn import_file(
    db: &Db,
    ffmpeg_bin: Option<&Path>,
    snapshots_dir: &Path,
    path: &str,
    camera_name: &str,
    opts: &ImportOptions,
) -> Result<ImportSummary> {
    // --- 1. Validate + canonicalize the (Admin-supplied) server path ---------
    // Reject control chars up front (defence in depth), then canonicalize — which
    // resolves `..` and verifies the file exists — and require a regular file.
    let raw = path.trim();
    if raw.is_empty() || raw.chars().any(char::is_control) {
        bail!("path must be a non-empty, control-character-free file path");
    }
    let full = std::fs::canonicalize(raw)
        .with_context(|| format!("file not found or unreadable: {raw}"))?;
    if !full.is_file() {
        bail!("not a regular file: {}", full.display());
    }

    // --- 2. Resolve the slug + guard against clobbering a real camera --------
    let slug = slugify(camera_name);
    if !valid_slug(&slug) {
        bail!("could not derive a valid camera name from {camera_name:?}");
    }
    let existing = db.list_cameras()?.into_iter().find(|c| c.name == slug);
    if let Some(c) = &existing {
        if !is_virtual_source(&c.source) {
            bail!(
                "a camera named {slug:?} already exists and is not a virtual import \
                 camera — choose a different name so a real camera is never overwritten"
            );
        }
    }

    // --- 3. Build the detector exactly like the live pipeline's global path ---
    let settings = db.settings();
    let ffmpeg = recorder::locate_ffmpeg(ffmpeg_bin)
        .context("ffmpeg not found (needed to decode the imported file)")?;
    let accel = detector::effective_accelerator(&settings.accelerator, settings.force_cpu);
    let mut det = detector::Detector::new(
        &settings.model_path,
        accel,
        settings.confidence,
        settings.nms_iou,
    )
    .with_context(|| {
        format!(
            "loading detection model {} (needed to analyze the import)",
            settings.model_path
        )
    })?;

    // --- 4. Extract ~1 fps of frames to a scratch dir, then always clean up ---
    let tmp = std::env::temp_dir().join(format!(
        "cammy-import-{}-{}",
        std::process::id(),
        chrono::Local::now().timestamp_millis()
    ));
    std::fs::create_dir_all(&tmp)
        .with_context(|| format!("creating scratch dir {}", tmp.display()))?;
    let run = import_frames(
        db,
        &ffmpeg,
        &full,
        &tmp,
        snapshots_dir,
        &slug,
        existing,
        &mut det,
        &settings,
        opts,
    );
    let _ = std::fs::remove_dir_all(&tmp);
    run
}

/// The body of an import, run with the scratch dir already created; the caller
/// removes the scratch dir on both success and failure.
#[allow(clippy::too_many_arguments)]
fn import_frames(
    db: &Db,
    ffmpeg: &Path,
    full: &Path,
    tmp: &Path,
    snapshots_dir: &Path,
    slug: &str,
    existing: Option<Camera>,
    det: &mut detector::Detector,
    settings: &crate::db::Settings,
    opts: &ImportOptions,
) -> Result<ImportSummary> {
    // ffmpeg: sample the video to a bounded set of 1 fps JPEG keyframes. `fps=1`
    // resamples to one frame per output second, so frame N (1-indexed) is roughly
    // input second N-1 — good enough to place events on a timeline.
    let pattern = tmp.join("f-%06d.jpg");
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-y", "-i"])
        .arg(full)
        .args(["-vf", "fps=1", "-frames:v"])
        .arg(MAX_FRAMES.to_string())
        .args(["-q:v", "3"])
        .arg(&pattern)
        .no_console()
        .output()
        .context("running ffmpeg to extract frames")?;

    let mut frames: Vec<PathBuf> = std::fs::read_dir(tmp)
        .with_context(|| format!("reading scratch dir {}", tmp.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "jpg"))
        .collect();
    frames.sort();

    if frames.is_empty() {
        // ffmpeg produced nothing decodable — surface its error so the user knows
        // whether the path pointed at a non-video / corrupt file.
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: String = stderr.lines().rev().take(4).collect::<Vec<_>>().join(" | ");
        bail!(
            "no video frames could be read from {} — is it a video file? {}",
            full.display(),
            tail.trim()
        );
    }

    // --- Create or reuse the virtual camera (only now that we have frames) ----
    let cam = match existing {
        Some(c) => c,
        None => create_virtual_camera(db, slug)?,
    };

    let base = opts
        .base_ts
        .unwrap_or_else(|| chrono::Local::now().timestamp());
    tracing::info!(
        camera = %cam.name,
        frames = frames.len(),
        file = %full.display(),
        "footage import started"
    );

    // Reuse the live pipeline's label / min-score / zone filtering verbatim.
    let labels = cam
        .detect_config
        .labels
        .as_ref()
        .unwrap_or(&settings.detect_labels);
    let min_score = cam.detect_config.min_score.unwrap_or(0.0);
    // Per-label cooldown (as the live loop applies) so a stationary object in the
    // footage doesn't produce one near-identical event per sampled second.
    let mut last_event: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();

    let mut frames_scanned = 0usize;
    let mut events_created = 0usize;
    for (i, fpath) in frames.iter().enumerate() {
        let ts = base + (i as i64) / i64::from(SAMPLE_FPS);
        let frame = match image::open(fpath) {
            Ok(f) => f,
            Err(e) => {
                tracing::debug!(frame = %fpath.display(), "skipping unreadable frame: {e:#}");
                continue;
            }
        };
        frames_scanned += 1;
        if frames_scanned.is_multiple_of(300) {
            tracing::info!(camera = %cam.name, scanned = frames_scanned, events = events_created, "import progress");
        }

        let dets = match det.detect(&frame) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(frame = %fpath.display(), "inference failed: {e:#}");
                continue;
            }
        };
        let (fw, fh) = (frame.width() as f32, frame.height() as f32);
        let wanted: Vec<&detector::Detection> = dets
            .iter()
            .filter(|d| labels.is_empty() || labels.iter().any(|l| l == d.label))
            .filter(|d| d.score >= min_score)
            .filter(|d| crate::pipeline::passes_zones_and_size(d, &cam.detect_config, fw, fh))
            .filter(|d| {
                last_event
                    .get(d.label)
                    .map(|t| ts - t >= settings.event_cooldown_secs)
                    .unwrap_or(true)
            })
            .collect();
        if wanted.is_empty() {
            continue;
        }

        // One annotated snapshot per frame, shared by that frame's events.
        let snap_rel = format!("{}-{}.jpg", cam.name, ts);
        let snap_abs = snapshots_dir.join(&snap_rel);
        if let Err(e) = crate::pipeline::save_snapshot(&frame, &wanted, &[], &snap_abs) {
            tracing::warn!("import snapshot save failed: {e:#}");
        }

        for d in &wanted {
            last_event.insert(d.label, ts);
            let zone = crate::pipeline::zone_for(d, &cam.detect_config, fw, fh);
            match db.add_event(
                cam.id,
                ts,
                d.label,
                d.score,
                // Store 0..1 fractions like every other event row.
                [d.x1 / fw, d.y1 / fh, d.x2 / fw, d.y2 / fh],
                Some(&snap_rel),
                None,
                None,
                None,
                zone.as_deref(),
            ) {
                Ok(_) => events_created += 1,
                Err(e) => tracing::warn!("import add_event failed: {e:#}"),
            }
        }
    }

    tracing::info!(
        camera = %cam.name,
        frames_scanned,
        events_created,
        "footage import complete"
    );
    Ok(ImportSummary {
        camera_id: cam.id,
        camera: cam.name.clone(),
        frames_scanned,
        events_created,
    })
}

/// Create the `virtual:<slug>` camera row: `enabled = false` (so every live loop
/// skips it), `detect = false` + `record = false` (nothing to stream/record), and
/// `retention_days = Some(0)` on its detect-config (never age-pruned) for the day
/// a future version remuxes segments.
fn create_virtual_camera(db: &Db, slug: &str) -> Result<Camera> {
    // add_camera inserts enabled=true; flip it off (+ set the import detect-config)
    // in one follow-up update before anything reads the registry.
    let mut cam = db.add_camera(slug, &format!("{VIRTUAL_SCHEME}{slug}"), None, false, false)?;
    cam.enabled = false;
    cam.detect_config = DetectConfig {
        retention_days: Some(0),
        ..Default::default()
    };
    db.update_camera(&cam)?;
    Ok(cam)
}

/// A slug is valid iff it satisfies the camera-name rules (`valid_name` in the API
/// layer): 1–32 chars of `a-z 0-9 - _`.
fn valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_db() -> Db {
        let dir = std::env::temp_dir().join(format!("cammy-import-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join(format!("t-{:?}.db", std::time::Instant::now()))).unwrap()
    }

    #[test]
    fn slugify_produces_valid_names() {
        assert_eq!(slugify("My Phone Clip!"), "my-phone-clip");
        assert_eq!(slugify("dashcam_2026-07-16"), "dashcam_2026-07-16");
        assert_eq!(slugify("  Front Door  "), "front-door");
        // All-invalid / empty falls back to a usable name.
        assert_eq!(slugify(""), "import");
        assert_eq!(slugify("***"), "import");
        // Over-length is truncated to <=32 and stays valid.
        let long = slugify(&"a".repeat(50));
        assert!(long.len() <= 32 && valid_slug(&long));
        for s in ["my-phone-clip", "front-door", "import", &long] {
            assert!(valid_slug(s), "slug {s:?} must satisfy valid_name");
        }
    }

    #[test]
    fn virtual_scheme_round_trips() {
        assert!(is_virtual_source("virtual:my-clip"));
        assert!(!is_virtual_source("rtsp://cam/stream"));
        assert!(!is_virtual_source("exec:ffmpeg -i x -f rtsp {output}"));
    }

    #[test]
    fn rejects_bad_paths_before_touching_ffmpeg() {
        let db = mem_db();
        let snaps = std::env::temp_dir();
        // Control character in the path → rejected up front.
        let bad = import_file(
            &db,
            None,
            &snaps,
            "some\nfile.mp4",
            "clip",
            &ImportOptions::default(),
        );
        assert!(bad.is_err());
        assert!(bad
            .unwrap_err()
            .to_string()
            .contains("control-character-free"));
        // Empty path → rejected.
        assert!(import_file(&db, None, &snaps, "   ", "clip", &ImportOptions::default()).is_err());
        // Non-existent path → canonicalize fails ("not found or unreadable").
        let missing = import_file(
            &db,
            None,
            &snaps,
            "Z:/definitely/not/here/nope.mp4",
            "clip",
            &ImportOptions::default(),
        );
        assert!(missing.is_err());
        assert!(missing
            .unwrap_err()
            .to_string()
            .contains("not found or unreadable"));
        // None of that should have created a camera.
        assert!(db.list_cameras().unwrap().is_empty());
    }

    #[test]
    fn refuses_to_clobber_a_real_camera() {
        let db = mem_db();
        // A real (non-virtual) camera named like our slug.
        db.add_camera("front-door", "rtsp://cam/stream", None, true, true)
            .unwrap();
        // A real (non-video) file so path validation passes and we reach the
        // collision guard, which fires before ffmpeg is ever run.
        let f = std::env::temp_dir().join(format!(
            "cammy-import-clobber-{}-{:?}.bin",
            std::process::id(),
            std::time::Instant::now()
        ));
        std::fs::write(&f, b"not a video").unwrap();
        let path = f.to_string_lossy().to_string();
        let err = import_file(
            &db,
            None,
            &std::env::temp_dir(),
            &path,
            "Front Door",
            &ImportOptions::default(),
        )
        .expect_err("must refuse to overwrite a real camera");
        let _ = std::fs::remove_file(&f);
        assert!(err.to_string().contains("already exists"), "got: {err}");
    }

    /// Full offline import over the repo's real `sample.mp4` with the real YOLO
    /// model + ffmpeg — the end-to-end validation the running NVR can't give us
    /// without a restart. Ignored by default (those assets aren't in CI):
    ///   cargo test -p zoomy -- --ignored import_e2e_sample_video --nocapture
    #[test]
    #[ignore = "needs workspace-root yolov8n.onnx + sample.mp4 + ffmpeg"]
    fn import_e2e_sample_video() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let sample = root.join("sample.mp4");
        let model = root.join("yolov8n.onnx");
        if !sample.exists() || !model.exists() {
            eprintln!("skipping E2E: sample.mp4 / yolov8n.onnx not present");
            return;
        }
        let db = mem_db();
        let mut s = db.settings();
        s.model_path = model.to_string_lossy().to_string();
        db.save_settings(&s).unwrap();

        let snaps = std::env::temp_dir().join(format!("cammy-import-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&snaps).unwrap();
        let summary = import_file(
            &db,
            None, // ffmpeg from PATH
            &snaps,
            &sample.to_string_lossy(),
            "Sample Import",
            &ImportOptions::default(),
        )
        .expect("import should succeed");
        eprintln!(
            "E2E: scanned {} frames, created {} events on camera {}",
            summary.frames_scanned, summary.events_created, summary.camera
        );
        assert!(summary.frames_scanned > 0, "should scan at least one frame");

        // The created camera must be a DISABLED virtual camera so every live loop
        // (go2rtc/record/pipeline/health/mqtt) skips it.
        let cam = db
            .list_cameras()
            .unwrap()
            .into_iter()
            .find(|c| c.id == summary.camera_id)
            .unwrap();
        assert!(!cam.enabled, "virtual camera must be disabled");
        assert!(is_virtual_source(&cam.source));
        assert_eq!(cam.detect_config.retention_days, Some(0));

        // Its events flow through the normal events list.
        let evs = db
            .list_events(Some(cam.id), None, None, None, None, None, false, 10_000)
            .unwrap();
        assert_eq!(evs.len(), summary.events_created);
        let _ = std::fs::remove_dir_all(&snaps);
    }
}
