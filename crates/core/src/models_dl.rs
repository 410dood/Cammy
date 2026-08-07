//! docs/10 P3 — server-side downloads for the optional AI models.
//!
//! The Models card used to point at a README table and leave the user to fetch
//! files by hand into the right directory with the right names. Now the server
//! does it: a fixed catalog (the exact sources the README documents — no
//! caller-supplied URLs, so this is not an SSRF/write-anywhere surface) maps
//! each optional feature to its files, and a background thread streams them to
//! `<name>.part` beside the executable's working directory (where every
//! model-presence check already looks) and renames into place when complete.
//! Progress is polled via a small in-process job board.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Mutex, OnceLock};

/// One downloadable file: the exact filename the feature's presence check
/// looks for, and where it comes from.
pub struct Item {
    pub target: &'static str,
    pub url: String,
}

fn hf(repo: &str, path: &str) -> String {
    format!("https://huggingface.co/{repo}/resolve/main/{path}")
}

/// The fixed feature → files catalog (mirrors README "optional AI models").
/// `None` = unknown feature or one that can't be downloaded (pose needs a
/// local `yolo export`; detection ships with the install).
pub fn catalog(feature: &str) -> Option<Vec<Item>> {
    let items = |v: Vec<Item>| Some(v);
    match feature {
        "smart_search" => items(vec![
            Item {
                target: crate::smart::VISION_MODEL,
                url: hf(
                    "Xenova/clip-vit-base-patch32",
                    "onnx/vision_model_quantized.onnx",
                ),
            },
            Item {
                target: crate::smart::TEXT_MODEL,
                url: hf(
                    "Xenova/clip-vit-base-patch32",
                    "onnx/text_model_quantized.onnx",
                ),
            },
            Item {
                target: crate::smart::TOKENIZER,
                url: hf("Xenova/clip-vit-base-patch32", "tokenizer.json"),
            },
        ]),
        "audio" => items(vec![
            Item {
                target: crate::audio::MODEL,
                url: hf("jafet21/yamnetonnx", "yamnet.onnx"),
            },
            Item {
                target: crate::audio::CLASS_MAP,
                url: hf("jafet21/yamnetonnx", "yamnet_class_map.csv"),
            },
        ]),
        "lpr" => items(vec![
            Item {
                target: crate::lpr::DET_MODEL,
                url: hf(
                    "onnx-community/yolos-small-finetuned-license-plate-detection-ONNX",
                    "onnx/model_quantized.onnx",
                ),
            },
            Item {
                target: crate::lpr::REC_MODEL,
                url: hf("monkt/paddleocr-onnx", "languages/english/rec.onnx"),
            },
            Item {
                target: crate::lpr::DICT_FILE,
                url: hf("monkt/paddleocr-onnx", "languages/english/dict.txt"),
            },
        ]),
        "face" => items(vec![
            Item {
                target: "det_10g.onnx",
                url: hf("immich-app/buffalo_l", "detection/model.onnx"),
            },
            Item {
                target: "w600k_r50.onnx",
                url: hf("immich-app/buffalo_l", "recognition/model.onnx"),
            },
        ]),
        // Two quality tiers for speech-to-text; the UI offers both.
        "transcription" => items(vec![Item {
            target: "ggml-tiny.en.bin",
            url: hf("ggerganov/whisper.cpp", "ggml-tiny.en.bin"),
        }]),
        "transcription_base" => items(vec![Item {
            target: "ggml-base.en.bin",
            url: hf("ggerganov/whisper.cpp", "ggml-base.en.bin"),
        }]),
        _ => None,
    }
}

/// Live job state, one per feature key.
#[derive(Clone, serde::Serialize)]
pub struct Status {
    /// "running" | "done" | "failed"
    pub state: String,
    /// The file currently (or last) transferred.
    pub file: String,
    pub done_files: usize,
    pub total_files: usize,
    /// Bytes received of the current file (no reliable total — HF redirects).
    pub bytes: u64,
    pub error: Option<String>,
}

fn jobs() -> &'static Mutex<HashMap<String, Status>> {
    static JOBS: OnceLock<Mutex<HashMap<String, Status>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn all_status() -> HashMap<String, Status> {
    jobs().lock().expect("model dl jobs poisoned").clone()
}

fn set(feature: &str, st: Status) {
    jobs()
        .lock()
        .expect("model dl jobs poisoned")
        .insert(feature.to_string(), st);
}

/// Kick off a background download of every file the feature needs. Errors if
/// the feature is unknown or already downloading. Files that already exist are
/// skipped (a re-download after a partial failure only fetches what's missing).
pub fn start(feature: &str) -> Result<(), String> {
    let items = catalog(feature).ok_or_else(|| format!("unknown feature '{feature}'"))?;
    {
        let map = jobs().lock().expect("model dl jobs poisoned");
        if map.get(feature).is_some_and(|s| s.state == "running") {
            return Err("that download is already running".into());
        }
    }
    let key = feature.to_string();
    let total = items.len();
    set(
        &key,
        Status {
            state: "running".into(),
            file: items[0].target.to_string(),
            done_files: 0,
            total_files: total,
            bytes: 0,
            error: None,
        },
    );
    std::thread::Builder::new()
        .name("model-download".into())
        .spawn(move || {
            for (i, item) in items.iter().enumerate() {
                if std::path::Path::new(item.target).exists() {
                    set(
                        &key,
                        Status {
                            state: "running".into(),
                            file: item.target.to_string(),
                            done_files: i + 1,
                            total_files: total,
                            bytes: 0,
                            error: None,
                        },
                    );
                    continue;
                }
                match fetch_one(&key, item, i, total) {
                    Ok(()) => {}
                    Err(e) => {
                        tracing::warn!(feature = %key, file = item.target, "model download failed: {e}");
                        set(
                            &key,
                            Status {
                                state: "failed".into(),
                                file: item.target.to_string(),
                                done_files: i,
                                total_files: total,
                                bytes: 0,
                                error: Some(e),
                            },
                        );
                        return;
                    }
                }
            }
            tracing::info!(feature = %key, files = total, "model download complete");
            set(
                &key,
                Status {
                    state: "done".into(),
                    file: String::new(),
                    done_files: total,
                    total_files: total,
                    bytes: 0,
                    error: None,
                },
            );
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Stream one file to `<target>.part`, then rename into place. Progress is
/// published every ~4 MB so the UI can show life without lock churn.
fn fetch_one(key: &str, item: &Item, done_files: usize, total: usize) -> Result<(), String> {
    let resp = ureq::get(&item.url)
        .timeout(std::time::Duration::from_secs(60 * 60))
        .call()
        .map_err(|e| format!("GET {}: {e}", item.url))?;
    let part = format!("{}.part", item.target);
    let mut out = std::fs::File::create(&part).map_err(|e| format!("create {part}: {e}"))?;
    let mut reader = resp.into_reader();
    let mut buf = [0u8; 64 * 1024];
    let mut total_bytes: u64 = 0;
    let mut last_pub: u64 = 0;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])
            .map_err(|e| format!("write: {e}"))?;
        total_bytes += n as u64;
        if total_bytes - last_pub > 4 * 1024 * 1024 {
            last_pub = total_bytes;
            set(
                key,
                Status {
                    state: "running".into(),
                    file: item.target.to_string(),
                    done_files,
                    total_files: total,
                    bytes: total_bytes,
                    error: None,
                },
            );
        }
    }
    out.flush().map_err(|e| e.to_string())?;
    drop(out);
    // An empty or absurdly small "model" is an error page, not a model.
    if total_bytes < 1024 {
        let _ = std::fs::remove_file(&part);
        return Err(format!(
            "{} came back suspiciously small ({total_bytes} B)",
            item.target
        ));
    }
    std::fs::rename(&part, item.target).map_err(|e| format!("rename {part}: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_targets_match_presence_checks() {
        // The downloaded filenames must be exactly what the feature presence
        // checks look for — a drifted name downloads a file nothing ever finds.
        let clip: Vec<&str> = catalog("smart_search")
            .unwrap()
            .iter()
            .map(|i| i.target)
            .collect();
        assert_eq!(
            clip,
            vec![
                crate::smart::VISION_MODEL,
                crate::smart::TEXT_MODEL,
                crate::smart::TOKENIZER
            ]
        );
        assert_eq!(catalog("audio").unwrap()[0].target, crate::audio::MODEL);
        assert_eq!(catalog("lpr").unwrap()[0].target, crate::lpr::DET_MODEL);
        assert!(catalog("pose").is_none());
        assert!(catalog("detection").is_none());
        // Every URL is https and pinned to huggingface (fixed catalog — the
        // endpoint must never fetch a caller-supplied URL).
        for f in [
            "smart_search",
            "audio",
            "lpr",
            "face",
            "transcription",
            "transcription_base",
        ] {
            for item in catalog(f).unwrap() {
                assert!(
                    item.url.starts_with("https://huggingface.co/"),
                    "{}",
                    item.url
                );
            }
        }
    }
}
