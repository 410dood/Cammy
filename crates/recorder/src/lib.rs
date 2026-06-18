//! Continuous recording: copy packets from go2rtc's RTSP restream to disk
//! WITHOUT decoding (Moonfire's model — cheap and lossless), as fixed-length
//! MP4 segments named by wall-clock start time, plus retention pruning.
//!
//! We drive `ffmpeg -c copy -f segment` as a child process rather than writing
//! our own muxer ("reuse, don't rebuild"). The segment files on disk are the
//! source of truth; the caller (core) indexes completed segments into SQLite.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use chrono::{Local, NaiveDateTime, TimeZone};

/// Segment filenames are strftime-stamped so they sort chronologically and the
/// start time survives without a database.
const SEGMENT_PATTERN: &str = "%Y%m%d-%H%M%S.mp4";

/// Keep spawned ffmpeg children from popping a console window on Windows (the
/// packaged desktop app is a GUI process with no console of its own). No-op
/// elsewhere.
trait NoConsole {
    fn no_console(&mut self) -> &mut Self;
}
impl NoConsole for Command {
    #[cfg(windows)]
    fn no_console(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(0x0800_0000) // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    fn no_console(&mut self) -> &mut Self {
        self
    }
}

/// A completed recording segment discovered on disk.
#[derive(Clone, Debug)]
pub struct Segment {
    pub path: PathBuf,
    /// Unix timestamp (seconds, local wall clock) parsed from the filename.
    pub start_ts: i64,
    pub bytes: u64,
}

/// A running ffmpeg recording process for one camera.
pub struct Recording {
    child: Child,
    camera: String,
}

impl Recording {
    /// Start recording `rtsp_url` into `out_dir` as `segment_seconds`-long parts.
    /// Video is always packet-copied; `audio` transcodes the camera's audio to
    /// AAC (RTSP audio is often PCM/G.711, which MP4 can't carry verbatim).
    pub fn start(
        ffmpeg_bin: &Path,
        camera: &str,
        rtsp_url: &str,
        out_dir: &Path,
        segment_seconds: u32,
        audio: bool,
    ) -> Result<Self> {
        std::fs::create_dir_all(out_dir)
            .with_context(|| format!("creating {}", out_dir.display()))?;
        let pattern = out_dir.join(SEGMENT_PATTERN);

        let codec_args: &[&str] = if audio {
            &["-c:v", "copy", "-c:a", "aac", "-b:a", "96k"]
        } else {
            &["-c", "copy", "-an"]
        };
        let child = Command::new(ffmpeg_bin)
            .args(["-loglevel", "error", "-rtsp_transport", "tcp", "-i"])
            .arg(rtsp_url)
            .args(codec_args)
            .args(["-f", "segment"])
            .args(["-segment_time", &segment_seconds.to_string()])
            .args(["-segment_format", "mp4"])
            // faststart puts the moov atom up front so browsers can play
            // segments progressively.
            .args(["-segment_format_options", "movflags=+faststart"])
            .args(["-reset_timestamps", "1", "-strftime", "1"])
            .arg(&pattern)
            .stdin(Stdio::piped()) // lets us send 'q' for a clean finalize
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .no_console()
            .spawn()
            .with_context(|| format!("spawning ffmpeg for camera {camera}"))?;

        tracing::info!(camera, pid = child.id(), dir = %out_dir.display(), "recording started");
        Ok(Self {
            child,
            camera: camera.to_string(),
        })
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Ask ffmpeg to finish the current segment and exit; kill if it won't.
    pub fn stop(mut self) {
        if let Some(stdin) = self.child.stdin.take() {
            let mut stdin = stdin;
            let _ = stdin.write_all(b"q");
        }
        for _ in 0..40 {
            if !self.is_alive() {
                tracing::info!(camera = %self.camera, "recording stopped cleanly");
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        tracing::warn!(camera = %self.camera, "ffmpeg did not exit; killing");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Locate ffmpeg: explicit path, then a vendored ./bin copy, then PATH.
pub fn locate_ffmpeg(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        anyhow::ensure!(p.exists(), "ffmpeg not found at {}", p.display());
        return Ok(p.to_path_buf());
    }
    let exe = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    let vendored = PathBuf::from("bin").join(exe);
    if vendored.exists() {
        return Ok(vendored);
    }
    which::which("ffmpeg").context(
        "ffmpeg not found. Install it (e.g. winget install Gyan.FFmpeg), drop it \
         at ./bin/, or set FFMPEG_BIN.",
    )
}

/// List COMPLETED segments in a camera's recording dir, oldest first. A file
/// still being written by ffmpeg is excluded by requiring its mtime to be at
/// least `min_quiet_secs` in the past.
pub fn scan_segments(dir: &Path, min_quiet_secs: u64) -> Result<Vec<Segment>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(out), // dir not created yet -> no segments
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(start_ts) = parse_segment_start(name) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else { continue };
        let quiet = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .unwrap_or_default();
        if quiet.as_secs() < min_quiet_secs {
            continue; // probably ffmpeg's open segment
        }
        if meta.len() == 0 {
            continue; // created but not yet flushed (ffmpeg buffers before first write)
        }
        out.push(Segment {
            path,
            start_ts,
            bytes: meta.len(),
        });
    }
    out.sort_by_key(|s| s.start_ts);
    Ok(out)
}

/// Parse `YYYYmmdd-HHMMSS.mp4` into a local-time unix timestamp.
fn parse_segment_start(file_name: &str) -> Option<i64> {
    let stem = file_name.strip_suffix(".mp4")?;
    let naive = NaiveDateTime::parse_from_str(stem, "%Y%m%d-%H%M%S").ok()?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|dt| dt.timestamp())
}

/// Re-encode a segment to space-saving quality (720p max, CRF 30, veryfast)
/// and atomically replace the original. Returns the new size in bytes.
/// UniFi-style "enhanced retention": old footage keeps existing at a fraction
/// of the storage cost.
/// Video-codec args for the chosen encoder. Unknown names fall back to CPU.
fn codec_args(hwaccel: &str) -> &'static [&'static str] {
    match hwaccel {
        "nvenc" => &["-c:v", "h264_nvenc", "-preset", "p4", "-cq", "30"],
        "qsv" => &["-c:v", "h264_qsv", "-global_quality", "30"],
        "videotoolbox" => &["-c:v", "h264_videotoolbox", "-q:v", "55"],
        _ => &["-c:v", "libx264", "-preset", "veryfast", "-crf", "30"],
    }
}

/// Run one re-encode attempt with the given encoder. Returns whether it
/// succeeded (a clean exit producing the temp file).
fn run_reencode(ffmpeg: &Path, path: &Path, tmp: &Path, hwaccel: &str) -> bool {
    let status = Command::new(ffmpeg)
        .args(["-loglevel", "error", "-y", "-i"])
        .arg(path)
        .args(["-vf", "scale='min(1280,iw)':-2"])
        .args(codec_args(hwaccel))
        .args(["-c:a", "aac", "-b:a", "64k", "-movflags", "+faststart"])
        .arg(tmp)
        .no_console()
        .status();
    matches!(status, Ok(s) if s.success())
}

pub fn reencode_segment(ffmpeg: &Path, path: &Path, hwaccel: &str) -> Result<u64> {
    let tmp = path.with_extension("tmp.mp4");
    // Try the requested hardware encoder, then fall back to libx264 so a
    // misconfigured accelerator never silently stops enhanced retention.
    let mut ok = run_reencode(ffmpeg, path, &tmp, hwaccel);
    if !ok && !matches!(hwaccel, "" | "cpu" | "libx264") {
        tracing::warn!("hwaccel '{hwaccel}' re-encode failed; falling back to CPU (libx264)");
        ok = run_reencode(ffmpeg, path, &tmp, "cpu");
    }
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        anyhow::bail!("re-encode failed for {}", path.display());
    }
    let new_bytes = std::fs::metadata(&tmp)?.len();
    let old_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(u64::MAX);
    if new_bytes >= old_bytes {
        // Not worth it (already small/efficient); keep the original.
        let _ = std::fs::remove_file(&tmp);
        anyhow::bail!("re-encode did not shrink {}", path.display());
    }
    std::fs::remove_file(path).context("removing original segment")?;
    std::fs::rename(&tmp, path).context("installing re-encoded segment")?;
    Ok(new_bytes)
}

/// Delete the oldest completed segments until the directory tree fits both
/// limits. Returns the deleted paths so the caller can drop their index rows.
pub fn prune(
    dirs: &[PathBuf],
    max_age_days: Option<u32>,
    max_total_bytes: Option<u64>,
) -> Result<Vec<PathBuf>> {
    let mut all: Vec<Segment> = Vec::new();
    for dir in dirs {
        all.extend(scan_segments(dir, 10)?);
    }
    all.sort_by_key(|s| s.start_ts);

    let mut deleted = Vec::new();
    let now = Local::now().timestamp();

    if let Some(days) = max_age_days {
        let cutoff = now - i64::from(days) * 86_400;
        for seg in all.iter().filter(|s| s.start_ts < cutoff) {
            if std::fs::remove_file(&seg.path).is_ok() {
                deleted.push(seg.path.clone());
            }
        }
        all.retain(|s| s.start_ts >= cutoff);
    }

    if let Some(max_bytes) = max_total_bytes {
        let mut total: u64 = all.iter().map(|s| s.bytes).sum();
        for seg in &all {
            if total <= max_bytes {
                break;
            }
            if std::fs::remove_file(&seg.path).is_ok() {
                total -= seg.bytes;
                deleted.push(seg.path.clone());
            }
        }
    }

    if !deleted.is_empty() {
        tracing::info!(count = deleted.len(), "retention pruned segments");
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_segment_filenames() {
        let ts = parse_segment_start("20260609-153045.mp4").unwrap();
        let dt = Local.timestamp_opt(ts, 0).unwrap();
        assert_eq!(dt.format("%Y%m%d-%H%M%S").to_string(), "20260609-153045");
        assert!(parse_segment_start("not-a-segment.mp4").is_none());
        assert!(parse_segment_start("20260609-153045.mkv").is_none());
    }

    #[test]
    fn scan_skips_fresh_files_and_sorts() {
        let dir = std::env::temp_dir().join(format!("rec-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("20260101-000200.mp4"), b"bb").unwrap();
        std::fs::write(dir.join("20260101-000100.mp4"), b"aa").unwrap();
        std::fs::write(dir.join("garbage.txt"), b"x").unwrap();

        // min_quiet_secs = 0 -> both count, sorted oldest first.
        let segs = scan_segments(&dir, 0).unwrap();
        assert_eq!(segs.len(), 2);
        assert!(segs[0].start_ts < segs[1].start_ts);

        // A huge quiet window excludes the just-written files.
        assert!(scan_segments(&dir, 3600).unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prune_by_size_deletes_oldest_first() {
        let dir = std::env::temp_dir().join(format!("prune-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("20260101-000100.mp4"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.join("20260101-000200.mp4"), vec![0u8; 100]).unwrap();
        // scan inside prune uses min_quiet_secs=10; backdate mtimes via wait is
        // overkill — instead prune with no limits is a no-op, then by size 150.
        let untouched = prune(std::slice::from_ref(&dir), None, None).unwrap();
        assert!(untouched.is_empty());

        // Backdate both files so the quiet-window filter sees them.
        let old = std::time::SystemTime::now() - Duration::from_secs(60);
        for f in ["20260101-000100.mp4", "20260101-000200.mp4"] {
            let p = dir.join(f);
            let file = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
            file.set_modified(old).unwrap();
        }

        let deleted = prune(std::slice::from_ref(&dir), None, Some(150)).unwrap();
        assert_eq!(deleted.len(), 1);
        assert!(deleted[0].ends_with("20260101-000100.mp4"));
        assert!(dir.join("20260101-000200.mp4").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
