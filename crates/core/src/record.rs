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

/// How long a live ffmpeg may go without completing a segment before we stop
/// calling it "recording". A healthy recorder finishes one every
/// `segment_seconds`, so this is a generous multiple-plus-slack: it must never
/// fire on a healthy camera (that would re-create the false "recording stopped"
/// alerts), only on one that has genuinely stopped producing footage.
fn stall_after(segment_seconds: u32) -> Duration {
    Duration::from_secs(
        u64::from(segment_seconds)
            .saturating_mul(2)
            .saturating_add(60),
    )
}

/// Whether this camera can produce events at all — i.e. whether "no event
/// covers this segment" is meaningful evidence that nothing happened.
///
/// The event-only and detection-triggered retention modes delete every segment
/// no event vouches for. On a camera that emits no events they therefore delete
/// EVERYTHING, within about a minute, silently. Detection runs only when
/// `detect` is on (`pipeline.rs` filters `c.enabled && c.detect`); the other
/// three are the remaining always-on background emitters. Deliberately
/// restrictive: anything not listed here means "keep the footage".
fn can_emit_events(c: &crate::db::Camera) -> bool {
    c.detect
        || c.detect_config.audio_detect
        || c.detect_config.pose_detect
        || c.detect_config.onvif_events
}

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

    // Keyed by (camera_id, stream) so a camera can run TWO recorders — the
    // full-res "main" stream (always, for every recordable camera) and, when it
    // opts into P3.7 dual-stream, the low-res "sub" stream. A camera that does
    // NOT opt in has exactly one ("main") entry, so its running-set, segments and
    // retention are byte-for-byte identical to before this feature. The value
    // carries the audio flag + output dir the recording was started with, so
    // flipping either setting restarts the affected recorder(s).
    let mut running: HashMap<(i64, &'static str), (bool, PathBuf, Recording)> = HashMap::new();
    let mut last_retention = Instant::now() - RETENTION_EVERY;
    // Segment-progress watchdog. `is_alive()` only asks whether the ffmpeg
    // process has exited — an ffmpeg that is up but writing nothing (a dead or
    // stalled RTSP source it keeps retrying) reported `recording: true` forever
    // while no footage reached disk. These track the newest completed segment
    // seen per recorder and when we last saw a NEW one; both are reset whenever
    // a recorder starts, so a fresh process gets a full grace period.
    let mut newest_seen: HashMap<(i64, &'static str), i64> = HashMap::new();
    let mut last_progress: HashMap<(i64, &'static str), Instant> = HashMap::new();

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
        //
        // The value is (go2rtc stream name, output dir): for "main" that's the
        // camera name + `<rec>/<name>`; for "sub" (P3.7) the `{name}_sub`
        // restream + `<rec>/<name>__sub`. The DOUBLE underscore is deliberate —
        // a real camera literally named "front_sub" records to `<rec>/front_sub`
        // (single underscore), so `<rec>/front__sub` can never collide with it.
        let mut desired: HashMap<(i64, &'static str), (String, PathBuf)> = HashMap::new();
        for c in cameras.iter().filter(|c| {
            c.enabled
                && c.record
                && c.detect_config
                    .record_schedule
                    .as_ref()
                    .map(|s| s.active_now())
                    .unwrap_or(true)
        }) {
            // Main stream: always, exactly as before — one entry per camera.
            desired.insert(
                (c.id, "main"),
                (c.name.clone(), recordings_dir.join(&c.name)),
            );
            // Sub stream: only when opted in AND a detect sub-stream exists to
            // record (no `detect_source` = no `{name}_sub` restream). Fail-safe:
            // a missing sub source just means no sub recorder — logged, no crash.
            if c.detect_config.record_substream {
                if c.detect_source.as_deref().is_some_and(|s| !s.is_empty()) {
                    desired.insert(
                        (c.id, "sub"),
                        (
                            format!("{}_sub", c.name),
                            recordings_dir.join(format!("{}__sub", c.name)),
                        ),
                    );
                } else {
                    tracing::debug!(
                        camera = %c.name,
                        "record_substream is on but the camera has no detect sub-stream; \
                         not recording a sub copy"
                    );
                }
            }
        }

        let stop_keys: Vec<(i64, &'static str)> = running
            .keys()
            .filter(|k| !desired.contains_key(k))
            .copied()
            .collect();
        for key in stop_keys {
            if let Some((_, _, rec)) = running.remove(&key) {
                rec.stop();
            }
            newest_seen.remove(&key);
            last_progress.remove(&key);
        }

        for (key, (stream_name, dir)) in &desired {
            let healthy = running
                .get_mut(key)
                .map(|(audio, d, r)| r.is_alive() && *audio == settings.record_audio && d == dir)
                .unwrap_or(false);
            if !healthy {
                if let Some((_, _, dead)) = running.remove(key) {
                    dead.stop();
                }
                match Recording::start(
                    &ffmpeg,
                    stream_name,
                    &go2rtc.rtsp_url(stream_name),
                    dir,
                    settings.segment_seconds,
                    settings.record_audio,
                ) {
                    Ok(rec) => {
                        running.insert(*key, (settings.record_audio, dir.clone(), rec));
                        // A fresh process gets a full grace period before the
                        // progress watchdog can judge it — its first segment
                        // cannot complete for `segment_seconds` yet.
                        newest_seen.remove(key);
                        last_progress.insert(*key, Instant::now());
                    }
                    Err(e) => {
                        tracing::warn!(camera = %stream_name, "failed to start recording: {e:#}")
                    }
                }
            }
        }

        // --- index completed segments ------------------------------------
        // Index the main dir (always) and the sub dir (P3.7), tagging each with
        // its stream. The sub dir is scanned UNCONDITIONALLY — even for a camera
        // that has since turned dual-stream off — so any leftover sub segments
        // stay indexed and therefore stay subject to retention. `scan_segments`
        // returns empty for a dir that doesn't exist, so this is a cheap no-op
        // for the common (main-only) camera.
        for cam in cameras.iter().filter(|c| c.record) {
            for (dir, stream) in [
                (recordings_dir.join(&cam.name), "main"),
                (recordings_dir.join(format!("{}__sub", cam.name)), "sub"),
            ] {
                if let Ok(segments) = recorder::scan_segments(&dir, SEGMENT_QUIET_SECS) {
                    let mut newest = 0i64;
                    for seg in segments {
                        newest = newest.max(seg.start_ts);
                        let path = seg.path.to_string_lossy().to_string();
                        if let Err(e) =
                            db.upsert_segment(cam.id, seg.start_ts, &path, seg.bytes, stream)
                        {
                            tracing::warn!("segment index failed: {e:#}");
                        }
                    }
                    // A newly completed segment is the only proof footage is
                    // really reaching disk. Comparing against the newest we had
                    // already seen (rather than a wall clock) keeps this immune
                    // to clock skew and to re-scanning the same old files.
                    let key = (cam.id, stream);
                    if newest > 0 && newest > newest_seen.get(&key).copied().unwrap_or(0) {
                        newest_seen.insert(key, newest);
                        last_progress.insert(key, Instant::now());
                    }
                }
            }
        }

        // Publish recorder liveness (the MAIN recorder drives the camera's
        // "recording" indicator — the sub copy is a background scrub aid) + drop
        // status for deleted cameras.
        let stall = stall_after(settings.segment_seconds);
        for cam in &cameras {
            let key = (cam.id, "main");
            // "Recording" must mean footage is being SAVED, not merely that an
            // ffmpeg process object exists. A recorder attached to a dead or
            // stalled source stays alive and retries forever, so `is_alive()`
            // alone reported healthy while nothing reached disk — the exact
            // silent failure the health guardian is supposed to catch.
            let alive = running.contains_key(&key);
            let progressing = last_progress.get(&key).is_some_and(|t| t.elapsed() < stall);
            let recording = alive && progressing;
            if alive && !progressing {
                tracing::warn!(
                    camera = %cam.name,
                    "recorder is running but has completed no segment recently — \
                     reporting it as not recording"
                );
            }
            // Separate "parked by its own schedule" from "should be recording
            // and isn't". Both look identical on the wire otherwise, and the
            // second one is the failure this product exists to catch.
            let paused = !recording
                && cam.enabled
                && cam.record
                && cam
                    .detect_config
                    .record_schedule
                    .as_ref()
                    .is_some_and(|s| !s.active_now());
            status.set_recording(cam.id, recording, paused);
        }
        status.retain(&cameras.iter().map(|c| c.id).collect::<Vec<_>>());

        // --- retention ----------------------------------------------------
        if last_retention.elapsed() >= RETENTION_EVERY {
            last_retention = Instant::now();
            // The global byte-cap pool spans BOTH streams' dirs of every camera,
            // so sub-stream bytes count toward the disk cap and get pruned too.
            // A non-existent sub dir scans empty, so this is harmless for
            // main-only cameras.
            let dirs: Vec<PathBuf> = cameras
                .iter()
                .flat_map(|c| {
                    [
                        recordings_dir.join(&c.name),
                        recordings_dir.join(format!("{}__sub", c.name)),
                    ]
                })
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

            // P2.14 bookmark protection covers EVERY deletion pass below, not just
            // age/byte-cap: the event-only and detection-triggered passes use a
            // window down to `pre_roll` (default 10s) — TIGHTER than the flagged
            // coverage slack (segment_seconds + 15s) — so a bookmarked segment in
            // that tail could otherwise be deleted here before the age/byte-cap
            // pass's protected check ran. Build the covering-segment set ONCE and
            // honor it in all passes. Fail-SAFE: on lookup error, skip every
            // deletion pass this tick (keep footage) — retention retries next cycle.
            let seg_span = i64::from(settings.segment_seconds) + 15;
            let (protected, do_deletes) = match db.flagged_segment_paths(seg_span) {
                Ok(p) => (p, true),
                Err(e) => {
                    tracing::warn!(
                        "bookmark-protection lookup failed; skipping deletion-based \
                         retention this tick (keeping footage): {e:#}"
                    );
                    (std::collections::HashSet::new(), false)
                }
            };

            // Event-only recording (Frigate retain mode): for cameras with
            // the flag, drop segments that have no event within one segment
            // span of them once they age past a 15-minute review grace.
            for cam in cameras.iter().filter(|c| {
                do_deletes && c.record && c.detect_config.event_only_recording && can_emit_events(c)
            }) {
                let older_than = chrono::Local::now().timestamp() - 15 * 60;
                let span = i64::from(settings.segment_seconds);
                // (span, span) reproduces the original symmetric window exactly.
                match db.eventless_segments(cam.id, older_than, span, span, span) {
                    Ok(mut paths) if !paths.is_empty() => {
                        // Never drop a bookmarked event's covering segment.
                        paths.retain(|p| !protected.contains(p));
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

            // Detection-triggered recording (P3.8): a TIGHTER, ASYMMETRIC
            // variant of event-only retention. Continuous packet-copy segmenting
            // is untouched (the segmenter never stops, so the pre-roll footage is
            // real) — this only prunes HARDER. Keep a short pre-roll and a longer
            // post-roll around each detection and delete everything else fast, so
            // a quiet camera sheds disk in about a minute. Flagged/bookmarked
            // events are ordinary event rows (never pruned), so their segment is
            // always kept by the same nearby-event check. Fail-SAFE: any DB error
            // skips pruning this tick — on doubt, footage is kept.
            for cam in cameras.iter().filter(|c| {
                do_deletes && c.record && c.detect_config.trigger_recording && can_emit_events(c)
            }) {
                let span = i64::from(settings.segment_seconds);
                let pre = i64::from(cam.detect_config.trigger_pre_roll_secs.unwrap_or(10));
                let post = i64::from(cam.detect_config.trigger_post_roll_secs.unwrap_or(30));
                // Settle grace: only prune a segment once its END (start_ts +
                // span) is older than now - (max(pre,post) + 30s). A segment still
                // being written or still inside a possible post-roll window is
                // NEVER eligible; the `pre` term also holds a segment long enough
                // that a LATER detection whose pre-roll reaches back to it (up to
                // `pre` seconds) will already exist and protect it via the
                // NOT-EXISTS check before it becomes eligible — so a large pre-roll
                // can't lose earlier footage. `eventless_segments` bounds
                // `start_ts`, so subtract the span too.
                let grace = post.max(pre) + 30;
                let older_than = chrono::Local::now().timestamp() - span - grace;
                match db.eventless_segments(cam.id, older_than, span, pre, post) {
                    Ok(mut paths) if !paths.is_empty() => {
                        // Never drop a bookmarked event's covering segment — this
                        // pass's window (down to pre_roll) is tighter than the
                        // flagged slack, so the protected check is load-bearing here.
                        paths.retain(|p| !protected.contains(p));
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
                            pre_roll = pre,
                            post_roll = post,
                            "detection-triggered retention: dropped un-triggered segments"
                        );
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(
                        "detection-triggered retention failed (keeping footage): {e:#}"
                    ),
                }
            }

            // Retention in two passes:
            //  1. Per-camera AGE prune — each camera keeps its own number of
            //     days (its override, else the global default). Age-only here so
            //     a camera's window is honored regardless of the others.
            //  2. ONE global pooled BYTE-cap pass — the disk-bound safety net,
            //     deleting oldest-across-all until under the global GB cap. This
            //     keeps total disk bounded even if per-camera overrides sum high.
            // P2.14 footage safety: a flagged (bookmarked) event's row + snapshot
            // already survive event retention, but the underlying recording
            // segment could still be deleted here — silently losing the footage
            // the user saved. `protected` (built once at the top of the retention
            // block) is honored in BOTH prune passes; `do_deletes` is false when
            // that lookup failed, so the whole deletion prune is skipped this tick.
            if do_deletes {
                let mut pruned: Vec<PathBuf> = Vec::new();
                for cam in &cameras {
                    let days = cam
                        .detect_config
                        .retention_days
                        .unwrap_or(settings.retention_days);
                    if days == 0 {
                        continue; // 0 = keep indefinitely (byte cap still applies below)
                    }
                    // Age out BOTH streams of this camera together (a sub
                    // segment ages on the same clock as its main). The sub
                    // dir scans empty for a main-only camera → no-op.
                    let cam_dirs = [
                        recordings_dir.join(&cam.name),
                        recordings_dir.join(format!("{}__sub", cam.name)),
                    ];
                    match recorder::prune(&cam_dirs, Some(days), None, &protected) {
                        Ok(deleted) => pruned.extend(deleted),
                        Err(e) => {
                            tracing::warn!(camera = %cam.name, "age retention failed: {e:#}")
                        }
                    }
                }
                let max_bytes = u64::from(settings.retention_gb) * 1_000_000_000;
                if max_bytes > 0 {
                    match recorder::prune(&dirs, None, Some(max_bytes), &protected) {
                        Ok(deleted) => pruned.extend(deleted),
                        Err(e) => tracing::warn!("byte-cap retention failed: {e:#}"),
                    }
                }
                for path in pruned {
                    let _ = db.delete_segment_by_path(&path.to_string_lossy());
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Camera, DetectConfig};

    fn cam(detect: bool, dc: DetectConfig) -> Camera {
        Camera {
            id: 1,
            name: "gate".into(),
            source: "rtsp://x".into(),
            detect_source: None,
            enabled: true,
            detect,
            record: true,
            created_ts: 0,
            detect_config: dc,
            group: None,
        }
    }

    #[test]
    fn event_only_retention_is_skipped_on_a_camera_that_emits_no_events() {
        // Event-only / detection-triggered retention deletes every segment no
        // event vouches for. With detection off and no other emitter, a camera
        // produces no events at all, so that rule would delete 100% of its
        // footage within about a minute. Nothing in the UI or in
        // `PATCH /api/cameras` prevents the combination.
        assert!(!can_emit_events(&cam(false, DetectConfig::default())));

        // Any always-on emitter makes "no event here" meaningful again.
        assert!(can_emit_events(&cam(true, DetectConfig::default())));
        for dc in [
            DetectConfig {
                audio_detect: true,
                ..Default::default()
            },
            DetectConfig {
                pose_detect: true,
                ..Default::default()
            },
            DetectConfig {
                onvif_events: true,
                ..Default::default()
            },
        ] {
            assert!(can_emit_events(&cam(false, dc)));
        }
    }

    #[test]
    fn stall_window_is_generous_enough_never_to_flag_a_healthy_recorder() {
        // A healthy recorder completes a segment every `segment_seconds`, so the
        // window must comfortably exceed that or we resurrect the false
        // "recording stopped" alerts this release just removed.
        for secs in [10u32, 60, 300] {
            assert!(
                stall_after(secs) > Duration::from_secs(u64::from(secs) * 2),
                "stall window must allow at least two missed segments"
            );
        }
        assert_eq!(stall_after(60), Duration::from_secs(180));
        // Absurd operator input must not overflow into a tiny window.
        assert!(stall_after(u32::MAX) > Duration::from_secs(60));
    }
}
