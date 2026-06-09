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
    recordings_dir: PathBuf,
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

    // Value carries the audio flag the recording was started with, so a
    // settings flip restarts recorders with the new codec args.
    let mut running: HashMap<i64, (bool, Recording)> = HashMap::new();
    let mut last_retention = Instant::now() - RETENTION_EVERY;

    while !shutdown.load(Ordering::Relaxed) {
        let settings = db.settings();
        let cameras = db.list_cameras().unwrap_or_default();

        // --- reconcile: stop unwanted, start missing/dead ----------------
        let desired: HashMap<i64, String> = cameras
            .iter()
            .filter(|c| c.enabled && c.record)
            .map(|c| (c.id, c.name.clone()))
            .collect();

        let stop_ids: Vec<i64> = running
            .keys()
            .filter(|id| !desired.contains_key(id))
            .copied()
            .collect();
        for id in stop_ids {
            if let Some((_, rec)) = running.remove(&id) {
                rec.stop();
            }
        }

        for (id, name) in &desired {
            let healthy = running
                .get_mut(id)
                .map(|(audio, r)| r.is_alive() && *audio == settings.record_audio)
                .unwrap_or(false);
            if !healthy {
                if let Some((_, dead)) = running.remove(id) {
                    dead.stop();
                }
                let dir = recordings_dir.join(name);
                match Recording::start(
                    &ffmpeg,
                    name,
                    &go2rtc.rtsp_url(name),
                    &dir,
                    settings.segment_seconds,
                    settings.record_audio,
                ) {
                    Ok(rec) => {
                        running.insert(*id, (settings.record_audio, rec));
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
            let max_bytes = u64::from(settings.retention_gb) * 1_000_000_000;
            match recorder::prune(&dirs, Some(settings.retention_days), Some(max_bytes)) {
                Ok(deleted) => {
                    for path in deleted {
                        let _ = db.delete_segment_by_path(&path.to_string_lossy());
                    }
                }
                Err(e) => tracing::warn!("retention failed: {e:#}"),
            }
        }

        // Sleep in small steps so shutdown is responsive.
        let waited = Instant::now();
        while waited.elapsed() < RECONCILE_EVERY && !shutdown.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    tracing::info!("stopping {} recording(s)", running.len());
    for (_, (_, rec)) in running.drain() {
        rec.stop();
    }
}
