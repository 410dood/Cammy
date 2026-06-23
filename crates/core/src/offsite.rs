//! Matrix #70 — offsite / cloud backup of recordings to S3-compatible object
//! storage (MinIO, AWS S3, Backblaze B2, Wasabi, Cloudflare R2, a NAS, …).
//!
//! Self-hosted NVRs are local-only by default (Blue Iris does FTP, Frigate
//! leans on external rclone glue, UniFi/Scrypted keep everything on the box) —
//! so if the recorder is stolen or its disk dies, the footage is gone. This is
//! the disaster-recovery / evidence-preservation answer: a self-contained worker
//! that mirrors sealed recording segments to a bring-your-own bucket.
//!
//! Design: a background thread (joined cleanly at shutdown, off by default,
//! gated on `Settings.offsite_backup_enabled`) walks the `segments` index for
//! anything not yet uploaded, reads each sealed MP4, signs a `PUT` with
//! hand-rolled AWS SigV4 (see [`crate::sigv4`]), and ships it over the same
//! `ureq` client every other worker uses. Upload state lives in
//! `offsite_uploads` so a restart resumes idempotently; failures retry with
//! capped exponential backoff; per-tick work is bounded so a big backlog can't
//! starve the detection/recorder threads. Backup health surfaces via
//! `GET /api/offsite/status`, `zoomy_backup_*` Prometheus gauges, and a
//! stale-backup notification.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{Datelike, Local, TimeZone, Utc};

use crate::db::Db;
use crate::sigv4;

const TICK: Duration = Duration::from_secs(30);
/// Upper bound on segments shipped per wake so the worker can't monopolise
/// upstream bandwidth / disk IO or starve detection.
const MAX_PER_TICK: usize = 25;
/// How many candidate rows to consider per tick (more than [`MAX_PER_TICK`] so
/// rows still inside their backoff window don't block fresh ones behind them).
const CANDIDATE_FETCH: u32 = 400;
const BASE_BACKOFF_SECS: i64 = 30;
const MAX_BACKOFF_SECS: i64 = 3600;
/// Hard ceiling on a single segment we'll buffer + upload. A normal minutes-long
/// MP4 is tens of MB; anything past this is degenerate, so we refuse it rather
/// than read it into RAM and risk OOMing the in-process worker.
const MAX_SEGMENT_BYTES: u64 = 1_024 * 1_024 * 1_024; // 1 GiB
/// Fire a "backup stalled" notification once we've actually failed and nothing
/// has succeeded in this long (and there is a backlog).
const STALE_AFTER_SECS: i64 = 3600;
/// Per-upload HTTP timeout. Generous: a 60 s 4K segment can be ~100 MB.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(180);

/// A resolved, validated backup target built from `Settings`. `None` from
/// [`Target::from_settings`] means "disabled or not usable" and the worker idles.
struct Target {
    /// Normalised origin, no trailing slash, e.g. `http://localhost:9000`.
    endpoint: String,
    /// Authority as it appears in the wire `Host` header (default ports
    /// stripped), e.g. `localhost:9000` or `s3.us-east-1.amazonaws.com`.
    host: String,
    region: String,
    bucket: String,
    /// Key prefix, no leading/trailing slash; `""` for none.
    prefix: String,
    access_key: String,
    secret_key: String,
}

impl Target {
    fn from_settings(s: &crate::db::Settings) -> Option<Target> {
        if !s.offsite_backup_enabled {
            return None;
        }
        let endpoint = s.offsite_endpoint.trim();
        let bucket = s.offsite_bucket.trim();
        let access_key = s.offsite_access_key.trim();
        let secret_key = s.offsite_secret_key.trim();
        // Need a full target to do anything; missing creds = idle (not an error).
        if endpoint.is_empty()
            || bucket.is_empty()
            || access_key.is_empty()
            || secret_key.is_empty()
        {
            return None;
        }
        let (scheme, rest) = if let Some(r) = endpoint.strip_prefix("https://") {
            ("https", r)
        } else if let Some(r) = endpoint.strip_prefix("http://") {
            ("http", r)
        } else {
            tracing::warn!("offsite endpoint must start with http(s)://; backup idle");
            return None;
        };
        // Keep only the authority (drop any path the user pasted), drop any
        // userinfo (user:pass@), and strip the default port so the signed Host
        // matches exactly what ureq puts on the wire.
        let authority = rest.split('/').next().unwrap_or(rest);
        let authority = authority.rsplit('@').next().unwrap_or(authority);
        if authority.is_empty() || authority.chars().any(char::is_control) {
            return None;
        }
        let host = match scheme {
            "https" => authority.strip_suffix(":443").unwrap_or(authority),
            _ => authority.strip_suffix(":80").unwrap_or(authority),
        };
        let region = {
            let r = s.offsite_region.trim();
            if r.is_empty() {
                "us-east-1"
            } else {
                r
            }
        };
        let prefix = s.offsite_prefix.trim().trim_matches('/');
        Some(Target {
            endpoint: format!("{scheme}://{host}"),
            host: host.to_string(),
            region: region.to_string(),
            bucket: bucket.to_string(),
            prefix: prefix.to_string(),
            access_key: access_key.to_string(),
            secret_key: secret_key.to_string(),
        })
    }
}

/// Worker entry point. Spawned + joined in `lib.rs` like the other background
/// workers; re-reads live `Settings` each tick.
pub fn run(db: Db, shutdown: Arc<AtomicBool>) {
    let mut stale_notified = false;
    while !shutdown.load(Ordering::Relaxed) {
        match Target::from_settings(&db.settings()) {
            Some(target) => {
                run_once(&db, &target, &shutdown);
                update_stale_notification(&db, &mut stale_notified);
            }
            None => {
                // Disabled / unconfigured: reset the latch so re-enabling later
                // starts from a clean "healthy" state.
                stale_notified = false;
            }
        }
        crate::util::sleep_interruptible(TICK, &shutdown);
    }
}

/// Upload up to [`MAX_PER_TICK`] eligible segments.
fn run_once(db: &Db, target: &Target, shutdown: &Arc<AtomicBool>) {
    let now = Utc::now().timestamp();
    let candidates = match db.pending_offsite(CANDIDATE_FETCH) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(error = %e, "offsite: listing pending failed");
            return;
        }
    };
    let mut sent = 0usize;
    for c in candidates {
        if sent >= MAX_PER_TICK || shutdown.load(Ordering::Relaxed) {
            break;
        }
        // Respect backoff for rows that already failed.
        if c.attempts > 0 {
            let exp = (c.attempts - 1).clamp(0, 7) as u32;
            let wait = (BASE_BACKOFF_SECS * (1i64 << exp)).min(MAX_BACKOFF_SECS);
            if now < c.last_ts + wait {
                continue;
            }
        }
        // The local file may have been pruned by retention before we got to it.
        if !Path::new(&c.path).exists() {
            let _ = db.mark_offsite_skipped(&c.path, &c.camera, now);
            continue;
        }
        // Refuse to buffer a degenerate / huge segment into RAM (#70 review):
        // terminal give-up — it won't get smaller, so don't retry it.
        if c.bytes > MAX_SEGMENT_BYTES {
            let _ = db.mark_offsite_gaveup(
                &c.path,
                &c.camera,
                &format!("segment too large to back up ({} bytes)", c.bytes),
                now,
            );
            tracing::warn!(path = %c.path, bytes = c.bytes, "offsite: segment too large, skipped");
            continue;
        }
        let data = match std::fs::read(&c.path) {
            Ok(d) => d,
            Err(e) => {
                let _ =
                    db.mark_offsite_failed(&c.path, &c.camera, "", 0, &format!("read: {e}"), now);
                sent += 1;
                continue;
            }
        };
        let bytes = data.len() as u64;
        let key = object_key(&target.prefix, &c.camera, c.start_ts, &c.path);
        match put_object(target, &key, &data) {
            Ok(()) => {
                let _ = db.mark_offsite_done(&c.path, &c.camera, &key, bytes, now);
                tracing::debug!(key, bytes, "offsite upload ok");
            }
            Err(e) => {
                let msg = format!("{e:#}");
                let _ = db.mark_offsite_failed(&c.path, &c.camera, &key, bytes, &msg, now);
                tracing::debug!(key, error = %msg, "offsite upload failed");
            }
        }
        sent += 1;
    }
    if sent > 0 {
        tracing::info!(uploaded = sent, "offsite backup tick");
    }
}

/// `[<prefix>/]<camera>/<YYYY>/<MM>/<DD>/<filename>`. Every component (prefix
/// segments included) is sanitised to a safe charset, keeping `/` only as the
/// separator, so the whole key is `[A-Za-z0-9._/-]` — it never needs
/// percent-encoding and no camera/prefix value can smuggle path traversal or
/// control chars into a key.
fn object_key(prefix: &str, camera: &str, start_ts: i64, path: &str) -> String {
    let filename = Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("segment.mp4");
    let (y, m, d) = Local
        .timestamp_opt(start_ts, 0)
        .single()
        .map(|t| (t.year(), t.month(), t.day()))
        .unwrap_or((1970, 1, 1));
    let cam = sanitize(camera);
    let mut key = String::new();
    // Sanitise each prefix path segment, preserving '/' as the folder separator.
    let clean_prefix = prefix
        .split('/')
        .filter(|seg| !seg.is_empty())
        .map(sanitize)
        .collect::<Vec<_>>()
        .join("/");
    if !clean_prefix.is_empty() {
        key.push_str(&clean_prefix);
        key.push('/');
    }
    key.push_str(&format!(
        "{cam}/{y:04}/{m:02}/{d:02}/{}",
        sanitize(filename)
    ));
    key
}

fn sanitize(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        "_".to_string()
    } else {
        out
    }
}

/// PUT one object. Buffers the (one) segment in memory so we can send a real
/// `Content-Length` + a true payload SHA-256 (S3 verifies it) without wading
/// into aws-chunked streaming — segments are minutes-long MP4s (tens of MB), and
/// only [`MAX_PER_TICK`] go per tick, one at a time, so peak memory is bounded.
fn put_object(target: &Target, key: &str, data: &[u8]) -> anyhow::Result<()> {
    // Sanitised keys contain only `[A-Za-z0-9._/-]`, so encoding is effectively
    // identity here — but we still route through the signer's encoder so the
    // URL path and the canonical URI are guaranteed byte-identical.
    let encoded_path = sigv4::encode_path(&format!("/{}/{}", target.bucket, key));
    let url = format!("{}{}", target.endpoint, encoded_path);

    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let datestamp = now.format("%Y%m%d").to_string();
    let payload_hash = sigv4::sha256_hex(data);

    let headers = vec![
        ("host".to_string(), target.host.clone()),
        ("x-amz-content-sha256".to_string(), payload_hash.clone()),
        ("x-amz-date".to_string(), amz_date.clone()),
    ];
    let authz = sigv4::authorization(
        &sigv4::Request {
            method: "PUT",
            canonical_uri: &encoded_path,
            canonical_query: "",
            headers: &headers,
            payload_hash_hex: &payload_hash,
        },
        &sigv4::Credentials {
            access_key: &target.access_key,
            secret_key: &target.secret_key,
            region: &target.region,
            service: "s3",
        },
        &amz_date,
        &datestamp,
    );

    let resp = ureq::put(&url)
        .timeout(UPLOAD_TIMEOUT)
        .set("x-amz-date", &amz_date)
        .set("x-amz-content-sha256", &payload_hash)
        .set("Authorization", &authz)
        .set("Content-Type", "video/mp4")
        .send_bytes(data);
    match resp {
        // Only a 2xx is a real success. ureq returns 3xx as Ok (it doesn't
        // auto-follow a redirect on PUT) — e.g. S3 answers a wrong-region PUT
        // with 301 PermanentRedirect — so an un-checked Ok would mark a segment
        // "done" that never actually stored. Require 2xx explicitly.
        Ok(r) if (200..300).contains(&r.status()) => Ok(()),
        Ok(r) => {
            let code = r.status();
            let body = redact(&r.into_string().unwrap_or_default(), &target.access_key);
            anyhow::bail!("S3 PUT unexpected status {code}: {}", truncate(&body, 300))
        }
        // ureq treats >=400 as Err(Status(..)); surface S3's error body (it
        // explains SignatureDoesNotMatch etc.) but bound its length.
        Err(ureq::Error::Status(code, r)) => {
            let body = redact(&r.into_string().unwrap_or_default(), &target.access_key);
            anyhow::bail!("S3 PUT {code}: {}", truncate(&body, 300))
        }
        Err(e) => Err(anyhow::anyhow!("S3 PUT transport error: {e}")),
    }
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() > max {
        s.chars().take(max).collect::<String>() + "…"
    } else {
        s.to_string()
    }
}

/// Scrub the configured Access Key ID out of an S3 error body before it is
/// persisted to `last_error` — that field is surfaced by the Viewer-reachable
/// `GET /api/offsite/status` (and the stale-backup notification), which both
/// deliberately omit the access key. S3/MinIO echo `<AWSAccessKeyId>…</…>` in
/// the common wrong-credential 403, so without this the id leaks straight back.
/// The secret is never in the body; this guards the id (an identifier) anyway.
fn redact(body: &str, access_key: &str) -> String {
    if access_key.is_empty() {
        body.to_string()
    } else {
        body.replace(access_key, "[REDACTED-KEY]")
    }
}

/// Edge-triggered backup-health notification (mirrors the camera online/offline
/// latch in `health.rs`): fire once on transition into "stalled", once on
/// recovery, never repeatedly.
fn update_stale_notification(db: &Db, notified: &mut bool) {
    let stats = match db.offsite_stats() {
        Ok(s) => s,
        Err(_) => return,
    };
    let now = Utc::now().timestamp();
    // Stalled = there's a backlog, we've actually failed at least once, and
    // nothing has succeeded recently. The failure guard avoids a false alarm
    // when the worker simply hasn't reached a fresh backlog yet.
    let stale = stats.backlog > 0
        && stats.last_error.is_some()
        && stats
            .last_success_ts
            .is_none_or(|t| now - t > STALE_AFTER_SECS);
    if stale && !*notified {
        let detail = match &stats.last_error {
            Some(e) => format!(
                "{} segment(s) still not backed up offsite. Last error: {}",
                stats.backlog,
                truncate(e, 200)
            ),
            None => format!("{} segment(s) still not backed up offsite.", stats.backlog),
        };
        let _ = db.add_notification(now, "backup", "Offsite backup stalled", Some(&detail), None);
        *notified = true;
        tracing::warn!(backlog = stats.backlog, "offsite backup stalled");
    } else if !stale && *notified {
        let _ = db.add_notification(
            now,
            "backup",
            "Offsite backup recovered",
            Some("Recordings are syncing offsite again."),
            None,
        );
        *notified = false;
        tracing::info!("offsite backup recovered");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_layout_and_sanitization() {
        // 2021-01-01T00:00:00Z = 1609459200; date component is local-tz, so just
        // assert the structural pieces that don't depend on the test box's zone.
        let key = object_key(
            "cammy",
            "Front Door",
            1_609_459_200,
            "/rec/Front Door/20210101-000000.mp4",
        );
        assert!(key.starts_with("cammy/Front_Door/"));
        assert!(key.ends_with("/20210101-000000.mp4"));
        assert!(!key.contains(' '));
    }

    #[test]
    fn empty_prefix_has_no_leading_slash() {
        let key = object_key("", "cam", 1_609_459_200, "/rec/cam/seg.mp4");
        assert!(key.starts_with("cam/"));
        assert!(!key.starts_with('/'));
    }

    #[test]
    fn messy_prefix_is_sanitized_per_segment() {
        // A prefix with spaces / extra slashes keeps '/' as the folder separator
        // but every segment is sanitised, so the key stays [A-Za-z0-9._/-].
        let key = object_key(
            "my backups//site a/",
            "cam",
            1_609_459_200,
            "/rec/cam/seg.mp4",
        );
        assert!(key.starts_with("my_backups/site_a/cam/"));
        assert!(!key.contains(' '));
        assert!(!key.contains("//"));
        assert!(key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/')));
    }

    #[test]
    fn sanitize_blocks_traversal_and_controls() {
        // '.' is intentionally kept (file extensions); the danger char '/' that
        // would forge key hierarchy is replaced, so no separator can be smuggled.
        assert_eq!(sanitize("../etc"), ".._etc");
        assert_eq!(sanitize("a/b"), "a_b");
        assert!(!sanitize("a/b/c").contains('/'));
        assert_eq!(sanitize(""), "_");
        assert_eq!(sanitize("ok-name_1.2"), "ok-name_1.2");
    }
}
