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
        self.write_config(db, &cameras)?;

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
        self.write_config(db, &cameras)?;

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

    fn write_config(&self, db: &Db, cameras: &[Camera]) -> Result<()> {
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
        // P3.4 HomeKit (HAP) bridge. Emit a `homekit:` section ONLY when the
        // bridge is on AND at least one enabled camera opts in — so the default
        // (off, or on-but-nothing-exposed) leaves the config byte-for-byte
        // unchanged from before this feature. go2rtc's HAP config schema (v1.9.14)
        // is a top-level map of stream-name -> { pin, name, device_id,
        // device_private, pairings }; the accessory category (17 = IP Camera) is
        // auto-derived by go2rtc. See `write_homekit`.
        let settings = db.settings();
        if settings.homekit_enabled {
            let exposed: Vec<&Camera> = cameras
                .iter()
                .filter(|c| c.enabled && c.detect_config.homekit_expose)
                .collect();
            if !exposed.is_empty() {
                // Preserve any controller pairing records go2rtc wrote into the
                // existing config so pairings survive a Cammy-driven regeneration.
                let existing = std::fs::read_to_string(&self.config_path).unwrap_or_default();
                let pairings = parse_homekit_pairings(&existing);
                let pin = homekit_pin(db);
                yaml.push_str("homekit:\n");
                for cam in exposed {
                    let name = clean(&cam.name);
                    let (dev_id, dev_priv) = homekit_identity(db, &name);
                    let saved = pairings.get(&name).map(String::as_str).unwrap_or("[]");
                    yaml.push_str(&format!(
                        "  {name}:\n    pin: \"{pin}\"\n    name: \"Cammy {name}\"\n    \
                         device_id: \"{dev_id}\"\n    device_private: \"{dev_priv}\"\n    \
                         pairings: {saved}\n"
                    ));
                }
            }
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

// --- P3.4 HomeKit (HAP) identity helpers -----------------------------------
//
// go2rtc owns the live HAP pairing; Cammy owns a STABLE accessory identity so a
// paired Apple Home controller keeps trusting the accessory across restarts.
// The PIN + per-stream device_id/device_private are generated once and persisted
// in the settings KV table (regenerating them would unpair Apple Home), then
// emitted into every generated go2rtc.yaml.

/// Fill `n` random bytes via ring's system CSPRNG, with a time-seeded fallback so
/// config generation never fails on the vanishingly rare RNG error.
fn rand_bytes(n: usize) -> Vec<u8> {
    use ring::rand::SecureRandom;
    let mut buf = vec![0u8; n];
    if ring::rand::SystemRandom::new().fill(&mut buf).is_err() {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        for (i, b) in buf.iter_mut().enumerate() {
            *b = ((t >> ((i % 16) * 8)) as u8) ^ (i as u8);
        }
    }
    buf
}

/// Generate an 8-digit HAP setup code, avoiding the HAP-invalid trivial codes
/// (all-same-digit and the reserved sequentials) so a real device can pair.
fn gen_pin() -> String {
    const INVALID: [&str; 12] = [
        "00000000", "11111111", "22222222", "33333333", "44444444", "55555555",
        "66666666", "77777777", "88888888", "99999999", "12345678", "87654321",
    ];
    loop {
        let s: String = rand_bytes(8)
            .iter()
            .map(|x| char::from(b'0' + (x % 10)))
            .collect();
        if !INVALID.contains(&s.as_str()) {
            return s;
        }
    }
}

/// Get-or-create the persisted 8-digit HAP PIN (KV `homekit.pin`).
pub fn homekit_pin(db: &Db) -> String {
    if let Some(p) = db.get_kv("homekit.pin") {
        if p.len() == 8 && p.bytes().all(|c| c.is_ascii_digit()) {
            return p;
        }
    }
    let p = gen_pin();
    let _ = db.set_kv("homekit.pin", &p);
    p
}

/// Format an 8-digit HAP setup code as `XXX-XX-XXX` for manual entry in the
/// Apple Home app (which accepts either form).
pub fn format_homekit_pin(pin: &str) -> String {
    if pin.len() == 8 && pin.bytes().all(|c| c.is_ascii_digit()) {
        format!("{}-{}-{}", &pin[0..3], &pin[3..5], &pin[5..8])
    } else {
        pin.to_string()
    }
}

/// Get-or-create a stream's stable HAP identity: `(device_id, device_private)`.
/// `device_id` is a MAC-style id (`AA:BB:CC:DD:EE:FF`); `device_private` is a
/// 32-byte ed25519 seed hex. Persisted per-stream in the settings KV so pairings
/// survive restarts (go2rtc accepts both verbatim — validated live).
fn homekit_identity(db: &Db, stream: &str) -> (String, String) {
    let id_key = format!("homekit.device_id.{stream}");
    let pk_key = format!("homekit.device_private.{stream}");
    let dev_id = db
        .get_kv(&id_key)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let id = rand_bytes(6)
                .iter()
                .map(|x| format!("{x:02X}"))
                .collect::<Vec<_>>()
                .join(":");
            let _ = db.set_kv(&id_key, &id);
            id
        });
    let dev_priv = db
        .get_kv(&pk_key)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let pk = crate::util::hex(&rand_bytes(32));
            let _ = db.set_kv(&pk_key, &pk);
            pk
        });
    (dev_id, dev_priv)
}

/// Best-effort extraction of each stream's `pairings` list from an existing
/// go2rtc.yaml so controller pairing records survive a Cammy config
/// regeneration. Returns stream-name -> the YAML list text to re-emit (e.g.
/// `[abc, def]`). Handles both inline flow lists and `- item` block lists (the
/// form go2rtc's yaml.v3 marshaller writes). v0: best-effort — an unrecognized
/// layout simply yields no pairings (the owner re-pairs), never an error.
fn parse_homekit_pairings(yaml: &str) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;
    fn commit(out: &mut HashMap<String, String>, cur: &Option<String>, block: &mut Vec<String>) {
        if let Some(s) = cur {
            if !block.is_empty() {
                out.insert(s.clone(), format!("[{}]", block.join(", ")));
            }
        }
        block.clear();
    }
    let mut out: HashMap<String, String> = HashMap::new();
    let mut in_hk = false;
    let mut cur: Option<String> = None;
    let mut block: Vec<String> = Vec::new();
    let mut in_block = false;
    for line in yaml.lines() {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();
        // A non-indented, non-blank line starts a new top-level section.
        if !line.starts_with(' ') && !trimmed.is_empty() {
            if in_block {
                commit(&mut out, &cur, &mut block);
                in_block = false;
            }
            cur = None;
            in_hk = trimmed.starts_with("homekit:");
            continue;
        }
        if !in_hk {
            continue;
        }
        if in_block {
            if let Some(item) = trimmed.strip_prefix("- ") {
                block.push(item.trim().trim_matches('"').to_string());
                continue;
            }
            commit(&mut out, &cur, &mut block);
            in_block = false;
        }
        // A new stream key at 2-space indent (`front:`).
        if indent == 2 && trimmed.ends_with(':') && !trimmed.starts_with('-') {
            cur = Some(trimmed.trim_end_matches(':').trim().trim_matches('"').to_string());
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("pairings:") {
            let rest = rest.trim();
            if rest.is_empty() {
                in_block = true; // a block list follows
            } else if rest != "[]" {
                if let Some(s) = &cur {
                    out.insert(s.clone(), rest.to_string());
                }
            }
        }
    }
    if in_block {
        commit(&mut out, &cur, &mut block);
    }
    out
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
    fn homekit_pin_formats_and_gen_is_valid() {
        // Display form is XXX-XX-XXX; non-8-digit input passes through untouched.
        assert_eq!(format_homekit_pin("19550224"), "195-50-224");
        assert_eq!(format_homekit_pin("195-50-224"), "195-50-224");
        // A generated PIN is exactly 8 digits and never a trivial/invalid code.
        let p = gen_pin();
        assert_eq!(p.len(), 8);
        assert!(p.bytes().all(|c| c.is_ascii_digit()));
        assert_ne!(p, "12345678");
        // Block-list pairings round-trip into an inline list re-emission.
        let y = "homekit:\n  front:\n    pin: \"1\"\n    pairings:\n      - abc\n      - def\n";
        let got = parse_homekit_pairings(y);
        assert_eq!(got.get("front").map(String::as_str), Some("[abc, def]"));
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
