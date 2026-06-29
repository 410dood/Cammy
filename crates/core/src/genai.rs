//! Optional GenAI event captioner. Runs on its own worker thread (like the
//! MQTT publisher) so a multi-second LLM call never stalls detection. The
//! pipeline hands over (event id, snapshot) and the worker writes a one-line
//! natural-language description back onto the event for review + search.
//!
//! Local-first: the default endpoint is a localhost Ollama vision model, so by
//! default nothing leaves the machine. The whole feature is gated behind an
//! explicit opt-in (`genai_enabled`), and a snapshot is only ever sent once the
//! user points the URL somewhere — cloud use is a deliberate configuration.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;

use crate::db::Db;

/// A request to caption one event's snapshot.
#[derive(Clone, Debug)]
pub struct CaptionJob {
    pub event_id: i64,
    pub snapshot_path: PathBuf,
    pub label: String,
    pub camera: String,
}

/// The captioning prompt for a detection.
fn prompt_for(label: &str, camera: &str) -> String {
    format!(
        "You are a security camera assistant. In one short, factual sentence, \
         describe what is happening in this image from the '{camera}' camera \
         (a '{label}' was detected). Do not speculate beyond what is visible."
    )
}

/// Build the Ollama /api/generate request body.
fn build_request(model: &str, prompt: &str, image_b64: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "prompt": prompt,
        "images": [image_b64],
        "stream": false,
    })
}

/// Pull the caption text out of an Ollama (`response`) or OpenAI-compatible
/// (`choices[0].message.content`) reply, trimmed to a single tidy line.
fn parse_response(body: &serde_json::Value) -> Option<String> {
    let text = body.get("response").and_then(|v| v.as_str()).or_else(|| {
        body.pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
    })?;
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = one_line.trim().trim_matches('"').trim();
    (!trimmed.is_empty()).then(|| {
        // Keep captions compact for the UI / push.
        if trimmed.len() > 280 {
            format!(
                "{}…",
                &trimmed[..trimmed.char_indices().nth(279).unwrap().0]
            )
        } else {
            trimmed.to_string()
        }
    })
}

/// Result of one caption attempt, so the worker can surface a *reachability*
/// failure to the user instead of swallowing it at debug (the silent-failure gap).
enum Outcome {
    /// The model was reached (a caption was saved, or it replied with none).
    Reached,
    /// Disabled / no snapshot — nothing to do, not a failure.
    Skipped,
    /// The model could not be reached (network/HTTP/parse) — the user can't tell
    /// their Ollama/endpoint is down without this.
    Failed(String),
}

fn caption_one(db: &Db, job: &CaptionJob) -> Outcome {
    let s = db.settings();
    if !s.genai_enabled || s.genai_url.trim().is_empty() {
        return Outcome::Skipped;
    }
    let Ok(bytes) = std::fs::read(&job.snapshot_path) else {
        return Outcome::Skipped;
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let req = build_request(&s.genai_model, &prompt_for(&job.label, &job.camera), &b64);

    let mut call = ureq::post(s.genai_url.trim()).timeout(Duration::from_secs(60));
    if !s.genai_api_key.trim().is_empty() {
        call = call.set(
            "Authorization",
            &format!("Bearer {}", s.genai_api_key.trim()),
        );
    }
    match call.send_json(req) {
        Ok(resp) => match resp.into_json::<serde_json::Value>() {
            Ok(body) => {
                if let Some(caption) = parse_response(&body) {
                    if let Err(e) = db.set_event_caption(job.event_id, &caption) {
                        // A DB write failure is a local problem, not "model down" —
                        // don't trip the reachability notification for it.
                        tracing::debug!("caption save failed: {e}");
                    } else {
                        tracing::info!(event = job.event_id, "genai caption: {caption}");
                    }
                }
                Outcome::Reached
            }
            Err(e) => Outcome::Failed(format!("response not JSON: {e}")),
        },
        Err(e) => Outcome::Failed(format!("request failed: {e}")),
    }
}

/// Decide the in-app notification (if any) for a caption outcome, given whether
/// we've already notified about an ongoing failure. Returns
/// `(new_notified_state, title, body)` when a notification should fire — edge-
/// triggered like the offsite/health latches so a flapping endpoint can't spam
/// the bell. Pure → unit-tested.
fn err_transition(outcome: &Outcome, notified: bool) -> Option<(bool, &'static str, String)> {
    match outcome {
        Outcome::Failed(msg) if !notified => Some((
            true,
            "AI captions unavailable",
            format!(
                "The captioning model could not be reached ({}). Captions are paused \
                 until it recovers; check the GenAI endpoint in Settings.",
                msg.chars().take(200).collect::<String>()
            ),
        )),
        Outcome::Reached if notified => Some((
            false,
            "AI captions recovered",
            "The captioning model is reachable again.".to_string(),
        )),
        _ => None,
    }
}

pub fn run(db: Db, rx: Receiver<CaptionJob>, shutdown: Arc<AtomicBool>) {
    // Edge-triggered failure surface: notify once when the endpoint goes
    // unreachable, once when it recovers.
    let mut err_notified = false;
    while !shutdown.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(job) => {
                let outcome = caption_one(&db, &job);
                if let Some((new_state, title, body)) = err_transition(&outcome, err_notified) {
                    let now = chrono::Utc::now().timestamp();
                    let _ = db.add_notification(now, "genai_error", title, Some(&body), None);
                    err_notified = new_state;
                    if new_state {
                        tracing::warn!("genai captioner endpoint unreachable: {title}");
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_has_model_prompt_and_image() {
        let r = build_request("llava", "describe", "QUJD");
        assert_eq!(r["model"], "llava");
        assert_eq!(r["images"][0], "QUJD");
        assert_eq!(r["stream"], false);
    }

    #[test]
    fn parses_ollama_and_openai_shapes() {
        let ollama = serde_json::json!({ "response": "  A person at the door.\n" });
        assert_eq!(
            parse_response(&ollama).as_deref(),
            Some("A person at the door.")
        );
        let openai = serde_json::json!({
            "choices": [ { "message": { "content": "\"A red car in the driveway.\"" } } ]
        });
        assert_eq!(
            parse_response(&openai).as_deref(),
            Some("A red car in the driveway.")
        );
        // Empty / missing → None.
        assert!(parse_response(&serde_json::json!({ "response": "   " })).is_none());
        assert!(parse_response(&serde_json::json!({ "x": 1 })).is_none());
    }

    #[test]
    fn err_transition_is_edge_triggered() {
        // First failure → notify + latch on.
        let (state, title, _) =
            err_transition(&Outcome::Failed("conn refused".into()), false).unwrap();
        assert!(state);
        assert_eq!(title, "AI captions unavailable");
        // Repeat failure while latched → no spam.
        assert!(err_transition(&Outcome::Failed("conn refused".into()), true).is_none());
        // Recovery while latched → notify + latch off.
        let (state, title, _) = err_transition(&Outcome::Reached, true).unwrap();
        assert!(!state);
        assert_eq!(title, "AI captions recovered");
        // Success while not latched, and skips, are silent.
        assert!(err_transition(&Outcome::Reached, false).is_none());
        assert!(err_transition(&Outcome::Skipped, true).is_none());
        assert!(err_transition(&Outcome::Skipped, false).is_none());
    }
}
