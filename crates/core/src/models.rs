//! Does this model actually LOAD?
//!
//! `/api/capabilities` proved file PRESENCE — `Path::exists` — and the UI drew a
//! green tick from it. A truncated download, a half-written `.part` renamed by a
//! crash, an `.onnx` from an incompatible export, or a text file saved where a
//! model should be all read as "installed", and the feature then silently
//! no-ops: exactly the class of failure the presence check was added to prevent.
//!
//! The same handler already refuses to assert twice over — `openvino_available()`
//! really asks the linked runtime, and `ask` is gated on a configured endpoint.
//! The model flags were the one thing left on that page that only asserted.
//!
//! Probing is not free (a real ONNX session build costs ~0.2-2 s per model), so
//! results are cached under a key derived from one `stat`: path + size + mtime +
//! accelerator. Steady state is a handful of `stat` calls and zero session
//! builds; the cost is paid once per process and once per model replacement — and
//! a file that APPEARS invalidates the cached negative just as naturally, so
//! finishing a download never needs a restart.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// What kind of file this is, and therefore what "loadable" means for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// An ONNX graph — proved by really building a session.
    Onnx,
    /// A JSON sidecar (the CLIP tokenizer) — proved by parsing it.
    Json,
    /// A whisper.cpp ggml model — proved by its magic bytes. (Loading one costs
    /// hundreds of MB of RAM, which is not a reasonable thing to do on a
    /// capabilities request.)
    Ggml,
    /// A text/CSV sidecar (class maps, plate dictionary) — proved by being
    /// readable and non-empty.
    Text,
}

/// One file's verdict.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Probe {
    /// The file exists.
    pub present: bool,
    /// It was opened and understood, not merely found.
    pub loadable: bool,
    /// Why it could not be loaded, in the underlying library's words.
    pub error: Option<String>,
    /// How long the check took — the cheap proof-of-work that makes the badge
    /// believable, the same instinct as the hwaccel probe's real test-encode.
    pub probed_ms: u64,
}

/// Cache identity. A replaced model changes size or mtime (a download's rename
/// carries the download's mtime forward), and a different accelerator asks a
/// different question, so all four belong in the key.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    len: u64,
    mtime_ns: i128,
    accel: String,
}

fn cache() -> &'static Mutex<HashMap<String, (Key, Probe)>> {
    static C: OnceLock<Mutex<HashMap<String, (Key, Probe)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Current identity of `path`, or `None` when it does not exist.
fn key_for(path: &str, accel: &str) -> Option<Key> {
    let md = std::fs::metadata(path).ok()?;
    if !md.is_file() {
        return None;
    }
    let mtime_ns = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0);
    Some(Key {
        len: md.len(),
        mtime_ns,
        accel: accel.to_string(),
    })
}

/// Really open the file. BLOCKING and potentially seconds long — callers on the
/// async runtime must go through `spawn_blocking`.
fn load_check(path: &str, kind: Kind, accel: &str) -> Result<(), String> {
    match kind {
        Kind::Onnx => {
            // Building the session is the whole proof. Note what it does NOT
            // prove: ONNX Runtime registers execution providers with
            // `error_on_failure = false`, so a GPU EP that cannot load quietly
            // degrades to CPU — a successful build means the MODEL is good, not
            // that the accelerator engaged. The UI must not overclaim.
            detector::build_ort_session(path, accel)
                .map(|_| ())
                .map_err(|e| format!("{e:#}"))
        }
        Kind::Json => {
            let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            serde_json::from_str::<serde_json::Value>(&text)
                .map(|_| ())
                .map_err(|e| format!("not valid JSON: {e}"))
        }
        Kind::Ggml => {
            let mut buf = [0u8; 4];
            {
                use std::io::Read as _;
                let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
                f.read_exact(&mut buf)
                    .map_err(|_| "file is too small to be a model".to_string())?;
            }
            // whisper.cpp writes "ggml" (older) or "lmgg" (GGUF-era little-endian).
            if &buf == b"ggml" || &buf == b"lmgg" {
                Ok(())
            } else {
                Err("this is not a whisper model file (wrong header)".into())
            }
        }
        Kind::Text => {
            let md = std::fs::metadata(path).map_err(|e| e.to_string())?;
            if md.len() == 0 {
                return Err("the file is empty".into());
            }
            std::fs::read_to_string(path)
                .map(|_| ())
                .map_err(|e| format!("could not be read as text: {e}"))
        }
    }
}

/// Probe one model, using the cache when nothing about the file has changed.
///
/// An empty `path` means "not configured", which is reported as absent rather
/// than as an error.
pub fn probe(path: &str, kind: Kind, accel: &str) -> Probe {
    let path = path.trim();
    if path.is_empty() {
        return Probe {
            present: false,
            loadable: false,
            error: None,
            probed_ms: 0,
        };
    }
    let cache_id = format!("{path}|{accel}");
    let key = key_for(path, accel);
    if let Some(k) = &key {
        let hit = cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&cache_id)
            .filter(|(cached, _)| cached == k)
            .map(|(_, p)| p.clone());
        if let Some(p) = hit {
            return p;
        }
    }
    let Some(k) = key else {
        // Absent. Cache nothing: `key_for` has no identity to key on, and the
        // stat is cheap, so a file that appears is picked up on the next call.
        return Probe {
            present: false,
            loadable: false,
            error: None,
            probed_ms: 0,
        };
    };
    let started = Instant::now();
    let outcome = load_check(path, kind, accel);
    let probed_ms = started.elapsed().as_millis() as u64;
    let probe = Probe {
        present: true,
        loadable: outcome.is_ok(),
        error: outcome.err(),
        probed_ms,
    };
    if let Some(err) = &probe.error {
        tracing::warn!(
            model = path,
            "model file is present but will not load: {err}"
        );
    }
    cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(cache_id, (k, probe.clone()));
    probe
}

/// Combine several files' verdicts into one feature's verdict: present only if
/// every file is there, loadable only if every one loads, and the first real
/// error explains why.
pub fn combine(parts: &[(String, Probe)]) -> (bool, bool, Option<String>, u64) {
    let present = !parts.is_empty() && parts.iter().all(|(_, p)| p.present);
    let loadable = present && parts.iter().all(|(_, p)| p.loadable);
    let error = parts
        .iter()
        .find(|(_, p)| p.present && p.error.is_some())
        .map(|(name, p)| format!("{name}: {}", p.error.clone().unwrap_or_default()));
    let probed_ms = parts.iter().map(|(_, p)| p.probed_ms).sum();
    (present, loadable, error, probed_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, bytes: &[u8]) -> String {
        let dir = std::env::temp_dir().join(format!("cammy-models-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p.to_string_lossy().to_string()
    }

    /// The bug this module exists for: a file that is PRESENT but garbage used to
    /// read as a working feature.
    #[test]
    fn a_present_but_invalid_file_is_not_loadable() {
        let p = tmp("tokenizer.json", b"{ this is not json");
        let r = probe(&p, Kind::Json, "cpu");
        assert!(r.present, "the file is there…");
        assert!(!r.loadable, "…but it must not read as working");
        assert!(r.error.is_some());

        let ok = tmp("good.json", br#"{"a":1}"#);
        let r = probe(&ok, Kind::Json, "cpu");
        assert!(r.present && r.loadable && r.error.is_none());
    }

    #[test]
    fn a_truncated_whisper_model_is_caught_by_its_header() {
        // The exact failure mode of an interrupted download: real name, real
        // extension, plausible size, wrong contents.
        let bad = tmp("ggml-base.en.bin", b"<!DOCTYPE html><html>404");
        let r = probe(&bad, Kind::Ggml, "cpu");
        assert!(r.present && !r.loadable);
        assert!(r.error.unwrap().contains("not a whisper model"));
        // Too small to even read a header is an error, not a crash.
        let stub = tmp("stub.bin", b"gg");
        assert!(!probe(&stub, Kind::Ggml, "cpu").loadable);
        // A real header passes.
        let good = tmp("good.bin", b"ggml\x00\x00\x00\x00");
        assert!(probe(&good, Kind::Ggml, "cpu").loadable);
    }

    #[test]
    fn absent_and_unconfigured_are_reported_as_absent_not_broken() {
        let missing = probe("definitely-not-here.onnx", Kind::Onnx, "cpu");
        assert!(!missing.present && !missing.loadable && missing.error.is_none());
        // An empty setting is "not configured", which is not a fault either.
        let unset = probe("   ", Kind::Onnx, "cpu");
        assert!(!unset.present && unset.error.is_none());
    }

    /// A rewritten file must be re-probed rather than served from the cache —
    /// otherwise replacing a broken model would still show red until a restart.
    #[test]
    fn the_cache_follows_the_file() {
        let p = tmp("swap.json", b"nonsense");
        assert!(!probe(&p, Kind::Json, "cpu").loadable);
        // Same path, different contents (and a different length, so the key moves
        // even on a filesystem with coarse mtime).
        std::fs::write(&p, br#"{"now":"valid"}"#).unwrap();
        assert!(
            probe(&p, Kind::Json, "cpu").loadable,
            "a replaced file must be re-probed, not served stale"
        );
    }

    #[test]
    fn combine_needs_every_part() {
        let ok = Probe {
            present: true,
            loadable: true,
            error: None,
            probed_ms: 3,
        };
        let missing = Probe {
            present: false,
            loadable: false,
            error: None,
            probed_ms: 0,
        };
        let broken = Probe {
            present: true,
            loadable: false,
            error: Some("INVALID_PROTOBUF".into()),
            probed_ms: 7,
        };
        let (p, l, e, ms) = combine(&[("a".into(), ok.clone()), ("b".into(), ok.clone())]);
        assert!(p && l && e.is_none() && ms == 6);
        // One missing sidecar makes the whole feature unavailable — this is why
        // the per-feature booleans had to become per-file.
        let (p, l, _, _) = combine(&[("a".into(), ok.clone()), ("b".into(), missing)]);
        assert!(!p && !l);
        // A present-but-broken part is present, not loadable, and NAMES itself.
        let (p, l, e, _) = combine(&[("a".into(), ok), ("b".into(), broken)]);
        assert!(p && !l);
        assert!(e.unwrap().starts_with("b: INVALID_PROTOBUF"));
    }
}
