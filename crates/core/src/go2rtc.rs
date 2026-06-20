//! go2rtc child-process supervisor. go2rtc does all camera-protocol ingest and
//! WebRTC signalling; we generate its config from the camera registry, keep it
//! running, and restart it when the registry changes.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::api::urlencode;
use crate::db::{Camera, Db};
use crate::proc::NoConsole as _;

/// Strip control characters from an interpolated value so a malicious/legacy
/// source can never break out of its YAML line (or REST query) and inject an
/// extra stream. The API layer also rejects such input on the way in.
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// Whether go2rtc will accept this source over its REST `PUT /api/streams` API.
/// Local-process sources (`exec:` / `ffmpeg:`) are config-file only — the API
/// rejects them (they're an RCE vector, and the `{output}` placeholder isn't
/// resolvable there), so a stream using one must be applied via a config
/// rewrite + restart rather than a live reconcile.
fn api_addable(src: &str) -> bool {
    let s = src.trim_start();
    !(s.starts_with("exec:") || s.starts_with("ffmpeg:"))
}

/// The go2rtc stream `name -> source` map the registry implies: every enabled
/// camera, plus its optional low-res detect sub-stream under `{name}_sub`.
/// Single source of truth shared by the config writer and the live REST sync.
fn desired_streams(cameras: &[Camera]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for cam in cameras.iter().filter(|c| c.enabled) {
        out.push((cam.name.clone(), clean(&cam.source)));
        if let Some(sub) = cam.detect_source.as_deref().filter(|s| !s.is_empty()) {
            out.push((format!("{}_sub", cam.name), clean(sub)));
        }
    }
    out
}

pub struct Go2Rtc {
    binary: PathBuf,
    config_path: PathBuf,
    api_port: u16,
    child: Mutex<Option<Child>>,
}

impl Go2Rtc {
    pub fn new(explicit_bin: Option<&Path>, config_path: PathBuf, api_port: u16) -> Result<Self> {
        Ok(Self {
            binary: locate(explicit_bin)?,
            config_path,
            api_port,
            child: Mutex::new(None),
        })
    }

    pub fn api_base(&self) -> String {
        format!("http://127.0.0.1:{}", self.api_port)
    }

    /// RTSP restream URL for a camera — what the recorder consumes.
    pub fn rtsp_url(&self, camera: &str) -> String {
        format!("rtsp://127.0.0.1:8554/{camera}")
    }

    /// (Re)generate config from the registry and (re)start the child.
    pub fn restart_with(&self, db: &Db) -> Result<()> {
        let cameras = db.list_cameras()?;
        self.write_config(&cameras)?;

        let mut guard = self.child.lock().expect("go2rtc mutex poisoned");
        if let Some(mut old) = guard.take() {
            let _ = old.kill();
            let _ = old.wait();
        }
        let child = Command::new(&self.binary)
            .arg("-config")
            .arg(&self.config_path)
            .no_console()
            .spawn()
            .with_context(|| format!("spawning {}", self.binary.display()))?;
        tracing::info!(
            pid = child.id(),
            cameras = cameras.len(),
            "go2rtc (re)started"
        );
        *guard = Some(child);
        Ok(())
    }

    /// Reconcile the running go2rtc's streams with the registry **without
    /// restarting** the process, so live viewers of unaffected cameras don't
    /// blip. Handles the common interactive edits — adding a camera, deleting
    /// one, enabling/disabling, and renaming (an old-name delete + new-name
    /// add) — via go2rtc's REST API. The config file is always rewritten first
    /// so any later restart (watchdog, crash, next boot) is still correct.
    ///
    /// Returns `Ok(true)` if the live reconcile succeeded (no restart needed),
    /// `Ok(false)` if a restart was performed instead. Falls back to a full
    /// restart when go2rtc isn't running yet or its API is unreachable, and
    /// when `force_restart` is set (the caller knows a same-name source edit
    /// happened, which the name-only reconcile can't propagate).
    pub fn sync_streams(&self, db: &Db, force_restart: bool) -> Result<bool> {
        let cameras = db.list_cameras()?;
        self.write_config(&cameras)?;

        if force_restart {
            self.restart_with(db)?;
            return Ok(false);
        }
        // A reconcile only makes sense against a live process; otherwise start.
        let running = {
            let mut guard = self.child.lock().expect("go2rtc mutex poisoned");
            match guard.as_mut() {
                Some(child) => matches!(child.try_wait(), Ok(None)),
                None => false,
            }
        };
        if !running {
            self.restart_with(db)?;
            return Ok(false);
        }
        match self.reconcile_via_api(&cameras) {
            Ok(true) => Ok(true),
            // A new stream uses a config-only source (exec:/ffmpeg:) that go2rtc
            // won't accept over its API — an expected restart, not a failure.
            Ok(false) => {
                tracing::info!("go2rtc has a config-only source to apply; restarting");
                self.restart_with(db)?;
                Ok(false)
            }
            Err(e) => {
                tracing::warn!("go2rtc stream sync failed ({e:#}); falling back to restart");
                self.restart_with(db)?;
                Ok(false)
            }
        }
    }

    /// Diff the live stream set against the registry and apply the delta with
    /// `PUT`/`DELETE /api/streams`. Names in both sets are left untouched (so
    /// unrelated live streams never drop); a rename surfaces as a delete of the
    /// old name plus an add of the new. Streams added out-of-band (e.g. ONVIF
    /// probe streams) that aren't in the registry are removed to converge on
    /// the config. Any failed `PUT` aborts so the caller can restart instead.
    /// Returns `Ok(true)` if the live set was fully reconciled, or `Ok(false)`
    /// if a new stream uses a config-only source (see [`api_addable`]) that the
    /// caller must apply with a restart instead.
    fn reconcile_via_api(&self, cameras: &[Camera]) -> Result<bool> {
        let base = self.api_base();
        let live: serde_json::Value = ureq::get(&format!("{base}/api/streams"))
            .timeout(Duration::from_secs(4))
            .call()
            .context("listing go2rtc streams")?
            .into_json()
            .context("parsing go2rtc streams")?;
        let live_names: HashSet<String> = live
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        let desired = desired_streams(cameras);
        let desired_names: HashSet<&str> = desired.iter().map(|(n, _)| n.as_str()).collect();

        // Remove streams that are no longer wanted (best-effort: a failed
        // delete just leaves a stale stream until the next restart).
        for name in &live_names {
            if !desired_names.contains(name.as_str()) {
                let _ = ureq::delete(&format!("{base}/api/streams?src={}", urlencode(name)))
                    .timeout(Duration::from_secs(4))
                    .call();
                tracing::info!(stream = %name, "go2rtc stream removed (no restart)");
            }
        }
        // Add streams that are new. A failing PUT is fatal to the reconcile so
        // the caller falls back to a restart and the camera still comes up.
        let mut needs_restart = false;
        for (name, src) in &desired {
            if !live_names.contains(name) {
                if !api_addable(src) {
                    // exec:/ffmpeg: are config-only — go2rtc's API rejects them.
                    // Defer to a restart rather than a doomed PUT (400).
                    needs_restart = true;
                    continue;
                }
                ureq::put(&format!(
                    "{base}/api/streams?name={}&src={}",
                    urlencode(name),
                    urlencode(src)
                ))
                .timeout(Duration::from_secs(4))
                .call()
                .with_context(|| format!("adding go2rtc stream {name}"))?;
                tracing::info!(stream = %name, "go2rtc stream added (no restart)");
            }
        }
        Ok(!needs_restart)
    }

    /// Restart the child if it died (call from a watchdog loop).
    pub fn ensure_alive(&self, db: &Db) -> Result<()> {
        let needs_restart = {
            let mut guard = self.child.lock().expect("go2rtc mutex poisoned");
            match guard.as_mut() {
                None => true,
                Some(child) => !matches!(child.try_wait(), Ok(None)),
            }
        };
        if needs_restart {
            tracing::warn!("go2rtc not running; restarting");
            self.restart_with(db)?;
        }
        Ok(())
    }

    pub fn stop(&self) {
        if let Some(mut child) = self.child.lock().expect("go2rtc mutex poisoned").take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn write_config(&self, cameras: &[Camera]) -> Result<()> {
        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        // The API binds loopback only — the recorder, detector and web UI all
        // reach go2rtc via 127.0.0.1, and keeping it off the LAN shrinks the
        // attack surface. The browser never connects here directly: the live
        // player's WebSocket is reverse-proxied through zoomy's own origin
        // (`/api/ws`), so go2rtc keeps its default same-origin protection (no
        // `origin: "*"` needed). (RTSP/WebRTC keep their own listeners for the
        // recorder and browser media transport.)
        let mut yaml = format!(
            "# AUTO-GENERATED by zoomy from the camera registry. Do not edit.\n\
             api:\n  listen: \"127.0.0.1:{}\"\n\
             rtsp:\n  listen: \":8554\"\n\
             webrtc:\n  listen: \":8555\"\n\
             log:\n  level: warn\n\
             streams:\n",
            self.api_port
        );
        // `desired_streams` already strips control chars (defense in depth: the
        // API rejects such input, but legacy rows predate that check). The
        // low-res `{name}_sub` keys are go2rtc's Frigate-style "detect role" —
        // the pipeline decodes those instead of the full-res main stream.
        for (name, src) in desired_streams(cameras) {
            yaml.push_str(&format!("  {name}:\n    - {src}\n"));
        }
        let mut f = std::fs::File::create(&self.config_path)
            .with_context(|| format!("creating {}", self.config_path.display()))?;
        f.write_all(yaml.as_bytes())?;
        Ok(())
    }
}

impl Drop for Go2Rtc {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Find go2rtc: explicit flag/env first, then ./bin, then PATH.
fn locate(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if p.exists() {
            return Ok(p.to_path_buf());
        }
        bail!("go2rtc binary not found at {}", p.display());
    }
    let exe = if cfg!(windows) {
        "go2rtc.exe"
    } else {
        "go2rtc"
    };
    let vendored = PathBuf::from("bin").join(exe);
    if vendored.exists() {
        return Ok(vendored);
    }
    if let Ok(found) = which::which("go2rtc") {
        return Ok(found);
    }
    bail!(
        "go2rtc not found. Download it from https://github.com/AlexxIT/go2rtc/releases \
         and put it at ./bin/{exe}, on PATH, or set GO2RTC_BIN."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Camera, DetectConfig};

    fn cam(name: &str, source: &str, detect_source: Option<&str>, enabled: bool) -> Camera {
        Camera {
            id: 0,
            name: name.into(),
            source: source.into(),
            detect_source: detect_source.map(Into::into),
            enabled,
            detect: false,
            record: false,
            created_ts: 0,
            detect_config: DetectConfig::default(),
            group: None,
        }
    }

    #[test]
    fn desired_streams_includes_sub_and_skips_disabled() {
        let cams = vec![
            cam("front", "rtsp://a", Some("rtsp://a/sub"), true),
            cam("back", "rtsp://b", None, true),
            cam("garage", "rtsp://c", Some("rtsp://c/sub"), false), // disabled → omitted
        ];
        let got = desired_streams(&cams);
        assert_eq!(
            got,
            vec![
                ("front".to_string(), "rtsp://a".to_string()),
                ("front_sub".to_string(), "rtsp://a/sub".to_string()),
                ("back".to_string(), "rtsp://b".to_string()),
            ]
        );
    }

    #[test]
    fn api_addable_rejects_config_only_sources() {
        // Network sources can be PUT over go2rtc's API.
        assert!(api_addable("rtsp://cam/stream"));
        assert!(api_addable("onvif://user:pass@host"));
        assert!(api_addable("http://host/stream.m3u8"));
        // Local-process sources are config-only (API returns 400) -> need restart.
        assert!(!api_addable("exec:ffmpeg -i x -f rtsp {output}"));
        assert!(!api_addable("ffmpeg:device?video=0"));
        assert!(!api_addable("  exec:foo")); // leading whitespace tolerated
    }

    #[test]
    fn desired_streams_strips_control_chars() {
        // A newline in a source must never survive into a stream value (it could
        // otherwise inject an extra go2rtc stream / `exec:` producer).
        let cams = vec![cam("c", "rtsp://x\n  evil:\n    - exec:rm", None, true)];
        let got = desired_streams(&cams);
        assert_eq!(
            got,
            vec![("c".to_string(), "rtsp://x  evil:    - exec:rm".to_string())]
        );
    }
}
