//! Recording manager: keeps one ffmpeg packet-copy process per recordable
//! camera, indexes completed segments into SQLite, and applies retention.
//! Runs on a plain thread with a poll loop — reconciliation (desired vs
//! running) makes it self-healing after go2rtc restarts or ffmpeg crashes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use recorder::Recording;

use crate::db::Db;
use crate::go2rtc::Go2Rtc;
use crate::status::StatusBoard;

/// Completed-segment quiet window: a file untouched this long is closed.
const SEGMENT_QUIET_SECS: u64 = 5;
const RECONCILE_EVERY: Duration = Duration::from_secs(3);
const RETENTION_EVERY: Duration = Duration::from_secs(60);

pub fn run(
    db: Db,
    go2rtc: Arc<Go2Rtc>,
    default_recordings_dir: PathBuf,
    snapshots_dir: PathBuf,
    ffmpeg_bin: Option<PathBuf>,
    status: StatusBoard,
    shutdown: Arc<AtomicBool>,
) {
    let ffmpeg = match recorder::locate_ffmpeg(ffmpeg_bin.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("recording disabled: {e:#}");
            return;
        }
    };

    // Value carries the audio flag + output dir the recording was started
    // with, so flipping either setting restarts recorders accordingly.
    let mut running: HashMap<i64, (bool, PathBuf, Recording)> = HashMap::new();
    let mut last_retention = Instant::now() - RETENTION_EVERY;

    while !shutdown.load(Ordering::Relaxed) {
        let settings = db.settings();
        let cameras = db.list_cameras().unwrap_or_default();
        // Storage option: a custom recordings root (other drive / NAS share)
        // applies to new segments; old ones play from their indexed paths.
        let recordings_dir = if settings.recordings_dir.trim().is_empty() {
            default_recordings_dir.clone()
        } else {
            PathBuf::from(settings.recordings_dir.trim())
        };

        // --- reconcile: stop unwanted, start missing/dead ----------------
        // A per-camera recording schedule (#67) gates continuous recording by
        // day/time: outside its window the camera drops out of `desired`, so the
        // stop logic below finalizes its current segment and parks the recorder
        // until the window reopens. `None` = always record.
        let desired: HashMap<i64, String> = cameras
            .iter()
            .filter(|c| c.enabled && c.record)
            .filter(|c| {
                c.detect_config
                    .record_schedule
                    .as_ref()
                    .map(|s| s.active_now())
                    .unwrap_or(true)
            })
            .map(|c| (c.id, c.name.clone()))
            .collect();

        let stop_ids: Vec<i64> = running
            .keys()
            .filter(|id| !desired.contains_key(id))
            .copied()
            .collect();
        for id in stop_ids {
            if let Some((_, _, rec)) = running.remove(&id) {
                rec.stop();
            }
        }

        for (id, name) in &desired {
            let dir = recordings_dir.join(name);
            let healthy = running
                .get_mut(id)
                .map(|(audio, d, r)| r.is_alive() && *audio == settings.record_audio && *d == dir)
                .unwrap_or(false);
            if !healthy {
                if let Some((_, _, dead)) = running.remove(id) {
                    dead.stop();
                }
                match Recording::start(
                    &ffmpeg,
                    name,
                    &go2rtc.rtsp_url(name),
                    &dir,
                    settings.segment_seconds,
                    settings.record_audio,
                ) {
                    Ok(rec) => {
                        running.insert(*id, (settings.record_audio, dir.clone(), rec));
                    }
                    Err(e) => tracing::warn!(camera = %name, "failed to start recording: {e:#}"),
                }
            }
        }

        // Publish recorder liveness + drop status for deleted cameras.
        for cam in &cameras {
            status.set_recording(cam.id, running.contains_key(&cam.id));
        }
        status.retain(&cameras.iter().map(|c| c.id).collect::<Vec<_>>());

        // --- index completed segments ------------------------------------
        for cam in cameras.iter().filter(|c| c.record) {
            let dir = recordings_dir.join(&cam.name);
            if let Ok(segments) = recorder::scan_segments(&dir, SEGMENT_QUIET_SECS) {
                for seg in segments {
                    let path = seg.path.to_string_lossy().to_string();
                    if let Err(e) = db.upsert_segment(cam.id, seg.start_ts, &path, seg.bytes) {
                        tracing::warn!("segment index failed: {e:#}");
                    }
                }
            }
        }

        // --- retention ----------------------------------------------------
        if last_retention.elapsed() >= RETENTION_EVERY {
            last_retention = Instant::now();
            let dirs: Vec<PathBuf> = cameras
                .iter()
                .map(|c| recordings_dir.join(&c.name))
                .collect();
            // Enhanced retention (UniFi-style) runs BEFORE deletion-based
            // pruning: shrinking old footage is the alternative to losing it
            // when the size cap bites. Bounded per cycle so a backlog cannot
            // starve the recorder loop.
            if settings.enhanced_retention_days > 0 {
                let cutoff = chrono::Local::now().timestamp()
                    - i64::from(settings.enhanced_retention_days) * 86_400;
                match db.reduction_candidates(cutoff, 3) {
                    Ok(candidates) => {
                        for (path, _ts) in candidates {
                            let p = PathBuf::from(&path);
                            if !p.exists() {
                                let _ = db.delete_segment_by_path(&path);
                                continue;
                            }
                            match recorder::reencode_segment(&ffmpeg, &p, &settings.hwaccel) {
                                Ok(new_bytes) => {
                                    let _ = db.mark_segment_reduced(&path, new_bytes);
                                    tracing::info!(
                                        segment = %p.display(),
                                        new_mb = format!("{:.1}", new_bytes as f64 / 1e6),
                                        "enhanced retention: segment reduced"
                                    );
                                }
                                Err(e) => {
                                    // Mark anyway so a stubborn file is not
                                    // retried forever.
                                    let _ = db.mark_segment_reduced(
                                        &path,
                                        p.metadata().map(|m| m.len()).unwrap_or(0),
                                    );
                                    tracing::debug!("enhanced retention skip: {e:#}");
                                }
                            }
                        }
                    }
                    Err(e) => tracing::warn!("enhanced retention query failed: {e:#}"),
                }
            }

            // Event-only recording (Frigate retain mode): for cameras with
            // the flag, drop segments that have no event within one segment
            // span of them once they age past a 15-minute review grace.
            for cam in cameras
                .iter()
                .filter(|c| c.record && c.detect_config.event_only_recording)
            {
                let older_than = chrono::Local::now().timestamp() - 15 * 60;
                let span = i64::from(settings.segment_seconds);
                match db.eventless_segments(cam.id, older_than, span, span) {
                    Ok(paths) if !paths.is_empty() => {
                        let mut dropped = 0u32;
                        for path in paths {
                            let p = PathBuf::from(&path);
                            if !p.exists() || std::fs::remove_file(&p).is_ok() {
                                let _ = db.delete_segment_by_path(&path);
                                dropped += 1;
                            }
                        }
                        tracing::info!(
                            camera = %cam.name,
                            count = dropped,
                            "event-only retention: dropped eventless segments"
                        );
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("event-only retention failed: {e:#}"),
                }
            }

            // Retention in two passes:
            //  1. Per-camera AGE prune — each camera keeps its own number of
            //     days (its override, else the global default). Age-only here so
            //     a camera's window is honored regardless of the others.
            //  2. ONE global pooled BYTE-cap pass — the disk-bound safety net,
            //     deleting oldest-across-all until under the global GB cap. This
            //     keeps total disk bounded even if per-camera overrides sum high.
            let mut pruned: Vec<PathBuf> = Vec::new();
            for cam in &cameras {
                let days = cam
                    .detect_config
                    .retention_days
                    .unwrap_or(settings.retention_days);
                if days == 0 {
                    continue; // 0 = keep indefinitely (byte cap still applies below)
                }
                let dir = recordings_dir.join(&cam.name);
                match recorder::prune(&[dir], Some(days), None) {
                    Ok(deleted) => pruned.extend(deleted),
                    Err(e) => tracing::warn!(camera = %cam.name, "age retention failed: {e:#}"),
                }
            }
            let max_bytes = u64::from(settings.retention_gb) * 1_000_000_000;
            if max_bytes > 0 {
                match recorder::prune(&dirs, None, Some(max_bytes)) {
                    Ok(deleted) => pruned.extend(deleted),
                    Err(e) => tracing::warn!("byte-cap retention failed: {e:#}"),
                }
            }
            for path in pruned {
                let _ = db.delete_segment_by_path(&path.to_string_lossy());
            }

            // Event retention: expire old events and their snapshot files
            // (snapshots otherwise grow without bound).
            if settings.event_retention_days > 0 {
                let cutoff = chrono::Local::now().timestamp()
                    - i64::from(settings.event_retention_days) * 86_400;
                match db.prune_events_before(cutoff) {
                    Ok(snapshots) if !snapshots.is_empty() => {
                        let snap_dir = snapshots_dir.clone();
                        for s in &snapshots {
                            let _ = std::fs::remove_file(snap_dir.join(s));
                        }
                        tracing::info!(count = snapshots.len(), "event retention pruned");
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("event retention failed: {e:#}"),
                }
            }
        }

        // Sleep in small steps so shutdown is responsive.
        let waited = Instant::now();
        while waited.elapsed() < RECONCILE_EVERY && !shutdown.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    tracing::info!("stopping {} recording(s)", running.len());
    for (_, (_, _, rec)) in running.drain() {
        rec.stop();
    }
}
