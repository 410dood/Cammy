//! P3.9 — pull-based two-box archive (v0, disaster recovery).
//!
//! A SECONDARY Cammy PULLS selected cameras' recording segments FROM a PRIMARY
//! Cammy over the primary's ordinary HTTP API, authenticated with an api_tokens
//! Bearer token (#48) created on the primary. This is the "second box in another
//! building" answer: if the primary is stolen or its disk dies, a copy of the
//! footage already lives on the secondary and is browsable/playable there.
//!
//! Design (structurally a clone of `offsite.rs`, but pulling FROM an HTTP API
//! instead of pushing to S3): a background thread, joined at shutdown, off by
//! default, gated on `Settings.archive_pull_enabled`. Each tick it:
//!   1. GET `{primary}/api/cameras` (Bearer) — the cameras the token may see.
//!   2. For each selected remote camera, ensures a LOCAL placeholder camera
//!      exists (`enabled=false, record=false, detect=false`, `group="archive"`)
//!      so NONE of the live loops (go2rtc config / recorder / detection /
//!      health) ever touch it — they all filter on `enabled` (go2rtc.rs,
//!      record.rs `enabled && record`, pipeline.rs `enabled && detect`).
//!   3. Reads a per-camera forward cursor (last pulled start_ts) from the KV
//!      store, GETs `{primary}/api/recordings?camera=<id>&since=<cursor>`
//!      (ascending), downloads each new segment's bytes via the existing
//!      `GET /api/recordings/{id}/video`, writes it under the placeholder
//!      camera's local recordings folder, `upsert_segment`s it locally, and
//!      advances the cursor.
//!
//! Bounded (N segments/tick) so a big backlog can't starve the box; capped
//! download size; timeouts; fail-safe — any error backs off + retries and never
//! advances a cursor past a segment it didn't fully store, so the local index is
//! never corrupted. v0 is SEGMENTS-ONLY: events, faces, plates etc. are NOT
//! mirrored yet (deferred to v1). Status surfaces via `GET /api/archive/status`.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::db::Db;

const TICK: Duration = Duration::from_secs(60);
/// Upper bound on segments downloaded per wake so a fresh secondary catching up
/// on a big backlog can't monopolise disk/network for hours in one tick.
const MAX_PULL_PER_TICK: usize = 20;
/// Remote segment-list page size requested per camera per tick.
const SEGMENTS_FETCH: u32 = 100;
/// Hard ceiling on a single segment we'll download (a normal minutes-long MP4 is
/// tens of MB; anything past this is degenerate). Refuse rather than fill disk.
const MAX_SEGMENT_BYTES: u64 = 2 * 1_024 * 1_024 * 1_024; // 2 GiB
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// KV keys.
const KV_LAST_PULL: &str = "archive_last_pull_ts";
const KV_LAST_ERROR: &str = "archive_last_error";

/// A resolved, validated pull target built from `Settings`. `None` = disabled or
/// not usable (the worker idles).
struct Target {
    /// Primary origin, no trailing slash, e.g. `https://nvr.example:8080`.
    base: String,
    token: String,
    /// Remote camera names to mirror; `None` = every camera the token can see.
    cameras: Option<HashSet<String>>,
}

impl Target {
    fn from_settings(s: &crate::db::Settings) -> Option<Target> {
        if !s.archive_pull_enabled {
            return None;
        }
        let url = s.archive_primary_url.trim();
        let token = s.archive_token.trim();
        if url.is_empty() || token.is_empty() {
            return None;
        }
        // SSRF note: the primary URL is Admin-configured (trusted), but still
        // require an http(s):// scheme and reject control chars / junk.
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            tracing::warn!("archive primary url must start with http(s)://; pull idle");
            return None;
        }
        if url.chars().any(char::is_control) || token.chars().any(char::is_control) {
            return None;
        }
        let base = url.trim_end_matches('/').to_string();
        let cams: HashSet<String> = s
            .archive_cameras
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect();
        let cameras = if cams.is_empty() { None } else { Some(cams) };
        Some(Target {
            base,
            token: token.to_string(),
            cameras,
        })
    }
}

/// Worker entry point. Spawned + joined in `lib.rs` like the other workers;
/// re-reads live `Settings` each tick.
pub fn run(db: Db, default_recordings_dir: PathBuf, shutdown: Arc<AtomicBool>) {
    // In-memory exponential backoff multiplier for a persistently-unreachable
    // primary, so we don't hammer it every 60 s while it's down.
    let mut backoff_mult: u32 = 1;
    while !shutdown.load(Ordering::Relaxed) {
        let mut wait = TICK;
        if let Some(target) = Target::from_settings(&db.settings()) {
            match run_once(&db, &target, &default_recordings_dir, &shutdown) {
                Ok(()) => backoff_mult = 1,
                Err(e) => {
                    let msg = format!("{e:#}");
                    let _ = db.set_kv(KV_LAST_ERROR, &truncate(&msg, 300));
                    tracing::debug!(error = %msg, "archive pull tick failed");
                    backoff_mult = (backoff_mult * 2).min(30); // up to ~30 min
                    wait = TICK * backoff_mult;
                }
            }
        }
        crate::util::sleep_interruptible(wait, &shutdown);
    }
}

fn run_once(
    db: &Db,
    target: &Target,
    default_recordings_dir: &Path,
    shutdown: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    // A top-level cameras-list failure aborts the tick (→ backoff). Per-camera /
    // per-segment failures are recorded but non-fatal so one bad camera can't
    // stall the rest.
    let cams = api_get_json(&target.base, "/api/cameras", &target.token)
        .map_err(|e| anyhow::anyhow!("listing primary cameras: {e:#}"))?;
    let cams = cams
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("primary /api/cameras did not return a list"))?;

    let settings = db.settings();
    let rec_root = if settings.recordings_dir.trim().is_empty() {
        default_recordings_dir.to_path_buf()
    } else {
        PathBuf::from(settings.recordings_dir.trim())
    };

    let mut pulled = 0usize;
    let mut had_error = false;
    for cam in cams {
        if pulled >= MAX_PULL_PER_TICK || shutdown.load(Ordering::Relaxed) {
            break;
        }
        let remote_id = cam.get("id").and_then(|v| v.as_i64());
        let remote_name = cam.get("name").and_then(|v| v.as_str());
        let (remote_id, remote_name) = match (remote_id, remote_name) {
            (Some(i), Some(n)) => (i, n),
            _ => continue,
        };
        // Camera filter (empty = all offered).
        if let Some(sel) = &target.cameras {
            if !sel.contains(remote_name) {
                continue;
            }
        }
        if let Err(e) = pull_camera(db, target, remote_id, remote_name, &rec_root, &mut pulled, shutdown) {
            had_error = true;
            let msg = format!("{remote_name}: {e:#}");
            let _ = db.set_kv(KV_LAST_ERROR, &truncate(&msg, 300));
            tracing::debug!(camera = %remote_name, error = %format!("{e:#}"), "archive pull: camera failed");
        }
    }

    let now = chrono::Utc::now().timestamp();
    let _ = db.set_kv(KV_LAST_PULL, &now.to_string());
    if !had_error {
        // Clear the sticky error once a full clean tick completes.
        let _ = db.set_kv(KV_LAST_ERROR, "");
    }
    if pulled > 0 {
        tracing::info!(pulled, "archive pull tick");
    }
    Ok(())
}

fn pull_camera(
    db: &Db,
    target: &Target,
    remote_id: i64,
    remote_name: &str,
    rec_root: &Path,
    pulled: &mut usize,
    shutdown: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let local = ensure_placeholder(db, remote_id, remote_name)?;
    let cursor_key = format!("archive_cursor_{}", local.id);
    let cursor: i64 = db
        .get_kv(&cursor_key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let path = format!(
        "/api/recordings?camera={remote_id}&since={cursor}&limit={SEGMENTS_FETCH}"
    );
    let segs = api_get_json(&target.base, &path, &target.token)?;
    let segs = segs
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("recordings list was not an array"))?;

    let dest_dir = rec_root.join(&local.name);
    for seg in segs {
        if *pulled >= MAX_PULL_PER_TICK || shutdown.load(Ordering::Relaxed) {
            break;
        }
        let seg_id = seg.get("id").and_then(|v| v.as_i64());
        let start_ts = seg.get("start_ts").and_then(|v| v.as_i64());
        let remote_path = seg.get("path").and_then(|v| v.as_str());
        let (seg_id, start_ts, remote_path) = match (seg_id, start_ts, remote_path) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => continue,
        };
        let filename = sanitize_filename(remote_path);
        let dest = dest_dir.join(&filename);
        let dest_str = dest.to_string_lossy().to_string();

        if dest.exists() {
            // Already have the bytes (e.g. cursor was reset). Make sure it's
            // indexed locally, then advance past it.
            let bytes = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
            let _ = db.upsert_segment(local.id, start_ts, &dest_str, bytes, "main");
        } else {
            std::fs::create_dir_all(&dest_dir)?;
            let bytes = download_segment(&target.base, seg_id, &target.token, &dest)?;
            db.upsert_segment(local.id, start_ts, &dest_str, bytes, "main")?;
            *pulled += 1;
        }
        // Advance the cursor only AFTER the segment is safely stored + indexed,
        // so an interrupted download is simply retried from the same point next
        // tick (fail-safe; never skips footage).
        db.set_kv(&cursor_key, &(start_ts + 1).to_string())?;
    }
    Ok(())
}

/// Find-or-create the local placeholder camera that mirrors a remote camera.
/// Mapping (remote id → local camera id) lives in the KV store so a rename on
/// the primary doesn't fork a second placeholder.
fn ensure_placeholder(db: &Db, remote_id: i64, remote_name: &str) -> anyhow::Result<crate::db::Camera> {
    let key = format!("archive_cam_{remote_id}");
    if let Some(v) = db.get_kv(&key) {
        if let Ok(local_id) = v.parse::<i64>() {
            if let Some(cam) = db.get_camera(local_id)? {
                return Ok(cam);
            }
        }
    }
    let base = placeholder_name(remote_name);
    let name = unique_name(db, &base)?;
    // enabled=false, record=false, detect=false → every live loop skips it (it's
    // a pure archive sink). The "source" is a non-stream placeholder string; it's
    // never used because the camera is disabled.
    let mut cam = db.add_camera(&name, "archive-mirror", None, false, false)?;
    cam.enabled = false;
    cam.group = Some("archive".to_string());
    db.update_camera(&cam)?;
    db.set_kv(&key, &cam.id.to_string())?;
    tracing::info!(remote = %remote_name, local = %cam.name, "archive: created placeholder camera");
    Ok(cam)
}

/// A deterministic, [a-z0-9_-]-only base name for a placeholder camera, prefixed
/// so it's obviously a mirror. Bounded to leave room for a de-dup suffix.
fn placeholder_name(remote: &str) -> String {
    let s: String = remote
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches('_');
    let base = if s.is_empty() { "cam" } else { s };
    let mut name = format!("arc-{base}");
    name.truncate(28); // leave room for "-NN"; valid_name cap is 32
    name
}

fn unique_name(db: &Db, base: &str) -> anyhow::Result<String> {
    let existing: HashSet<String> = db.list_cameras()?.into_iter().map(|c| c.name).collect();
    if !existing.contains(base) {
        return Ok(base.to_string());
    }
    for n in 2..10_000 {
        let cand = format!("{base}-{n}");
        if !existing.contains(&cand) {
            return Ok(cand);
        }
    }
    anyhow::bail!("could not find a free placeholder camera name for {base}")
}

/// Last path component, sanitised to `[A-Za-z0-9._-]` so a remote path can't
/// smuggle a separator / traversal into the local file name.
fn sanitize_filename(remote_path: &str) -> String {
    let raw = remote_path
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("segment.mp4");
    let out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() || out == "." || out == ".." {
        "segment.mp4".to_string()
    } else {
        out
    }
}

fn api_get_json(base: &str, path: &str, token: &str) -> anyhow::Result<serde_json::Value> {
    let url = format!("{base}{path}");
    let resp = ureq::get(&url)
        .timeout(LIST_TIMEOUT)
        .set("Authorization", &format!("Bearer {token}"))
        .call()?;
    Ok(resp.into_json()?)
}

/// Download one segment's bytes to `dest` via a temp file + atomic rename, with a
/// hard size cap so a degenerate/huge (or hostile) response can't fill the disk.
fn download_segment(base: &str, seg_id: i64, token: &str, dest: &Path) -> anyhow::Result<u64> {
    let url = format!("{base}/api/recordings/{seg_id}/video");
    let resp = ureq::get(&url)
        .timeout(DOWNLOAD_TIMEOUT)
        .set("Authorization", &format!("Bearer {token}"))
        .call()?;
    let mut reader = resp.into_reader().take(MAX_SEGMENT_BYTES + 1);
    let tmp = dest.with_file_name(format!(
        "{}.part",
        dest.file_name().and_then(|s| s.to_str()).unwrap_or("segment.mp4")
    ));
    let n = {
        let mut f = std::fs::File::create(&tmp)?;
        std::io::copy(&mut reader, &mut f)?
    };
    if n > MAX_SEGMENT_BYTES {
        let _ = std::fs::remove_file(&tmp);
        anyhow::bail!("segment {seg_id} exceeds size cap ({} bytes)", MAX_SEGMENT_BYTES);
    }
    std::fs::rename(&tmp, dest)?;
    Ok(n)
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() > max {
        s.chars().take(max).collect::<String>() + "…"
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_name_is_safe_and_prefixed() {
        assert_eq!(placeholder_name("Front Door"), "arc-front_door");
        assert!(placeholder_name("../etc").starts_with("arc-"));
        assert_eq!(placeholder_name(""), "arc-cam");
        assert!(placeholder_name(&"x".repeat(200)).len() <= 28);
    }

    #[test]
    fn sanitize_filename_blocks_traversal() {
        assert_eq!(sanitize_filename("/rec/cam/20260101-000000.mp4"), "20260101-000000.mp4");
        assert_eq!(sanitize_filename("../../evil"), "evil");
        assert!(!sanitize_filename("a/b/c").contains('/'));
        assert_eq!(sanitize_filename("/rec/cam/"), "segment.mp4");
    }
}
