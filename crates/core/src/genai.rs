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

/// A rule fire deferred to this worker — either VLM-verified before firing
/// (`vlm_prompt`) and/or captioned so the description rides IN the push
/// (`describe`), both off the detection thread. Carries owned event data; the
/// settings-derived fields (base_url, webhook template, SMTP) are rebuilt from
/// the DB at fire time so the job stays small.
#[derive(Clone, Debug)]
pub struct VlmGateJob {
    pub rule: crate::db::AlarmRule,
    pub event_id: i64,
    pub camera: String,
    /// Camera id (P2.8b: the per-camera feedback-suppression lookup key).
    pub camera_id: i64,
    pub label: String,
    pub score: f32,
    pub ts: i64,
    pub snapshot_url: String,
    pub snapshot_path: PathBuf,
    pub face: Option<String>,
    pub plate: Option<String>,
    /// Event severity at dispatch (the emit site computed it).
    pub severity: u8,
    /// The rule's drained burst counter (`notify::take_suppressed`), carried so
    /// the deferred push still reads "+N more during cooldown".
    pub suppressed: u32,
}

/// Work for the single GenAI worker thread (captioning + VLM alarm verification
/// share the one loaded-model lifecycle and the off-detection-thread guarantee).
pub enum Job {
    Caption(CaptionJob),
    // Boxed: a VlmGateJob carries a full AlarmRule, so box it to keep the enum
    // (and the channel) small.
    VlmGate(Box<VlmGateJob>),
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

/// One GenAI vision call → the model's cleaned text reply. `Ok(Some)` = a reply,
/// `Ok(None)` = reached but empty, `Err` = transport/parse failure (endpoint
/// unreachable). Shared by the captioner and the VLM gate.
fn call_vision(
    url: &str,
    api_key: &str,
    body: serde_json::Value,
) -> Result<Option<String>, String> {
    let mut call = ureq::post(url.trim()).timeout(Duration::from_secs(60));
    if !api_key.trim().is_empty() {
        call = call.set("Authorization", &format!("Bearer {}", api_key.trim()));
    }
    match call.send_json(body) {
        Ok(resp) => match resp.into_json::<serde_json::Value>() {
            Ok(body) => Ok(parse_response(&body)),
            Err(e) => Err(format!("response not JSON: {e}")),
        },
        Err(e) => Err(format!("request failed: {e}")),
    }
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
    match call_vision(&s.genai_url, &s.genai_api_key, req) {
        Ok(caption) => {
            if let Some(caption) = caption {
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
        Err(e) => Outcome::Failed(e),
    }
}

/// Interpret a model's yes/no answer. `Some(true)`/`Some(false)` only on a clear
/// answer (we ask for a one-word reply); `None` when it can't be read — callers
/// FAIL OPEN on `None`. Unit-tested.
fn interpret_yes_no(text: &str) -> Option<bool> {
    let t = text.trim().to_lowercase();
    // Whole-word tokens (so "not"/"nobody"/"yesterday" never count as no/yes).
    let words: Vec<&str> = t
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .collect();
    let is_yes = |w: &str| matches!(w, "yes" | "yep" | "yeah" | "true" | "y");
    let is_no = |w: &str| matches!(w, "no" | "nope" | "false" | "n");
    // The leading token is the reliable signal (we asked for a one-word answer).
    match words.first().copied().unwrap_or("") {
        w if is_yes(w) => Some(true),
        w if is_no(w) => Some(false),
        // Verbose reply: a single clear polarity word elsewhere wins, else give up
        // (ambiguous → None → the gate fails OPEN).
        _ => match (
            words.iter().any(|w| is_yes(w)),
            words.iter().any(|w| is_no(w)),
        ) {
            (true, false) => Some(true),
            (false, true) => Some(false),
            _ => None,
        },
    }
}

/// Ask the GenAI vision model a yes/no question about an image. `Some(true)` =
/// confirmed, `Some(false)` = denied, `None` = couldn't tell (error/timeout/
/// ambiguous). The VLM gate fails OPEN on `None`. Reuses the captioner's model +
/// endpoint; appends a one-word-answer instruction to the rule's prompt.
fn vlm_confirm(s: &crate::db::Settings, prompt: &str, image_b64: &str) -> Option<bool> {
    let full = format!("{}\nAnswer with only one word: yes or no.", prompt.trim());
    let req = build_request(&s.genai_model, &full, image_b64);
    match call_vision(&s.genai_url, &s.genai_api_key, req) {
        Ok(Some(text)) => interpret_yes_no(&text),
        _ => None,
    }
}

/// VLM-verify a matched alarm and fire it if confirmed. Runs in the worker (off
/// the detection thread). **Fails OPEN**: fires unless the model gives a clear
/// "no", so a missing/unreachable model or an ambiguous reply never silently
/// suppresses a real alert.
fn vlm_gate(db: &Db, j: &VlmGateJob, mqtt_tx: &std::sync::mpsc::Sender<crate::mqtt::EventMsg>) {
    let s = db.settings();
    let verdict = if s.genai_enabled && !s.genai_url.trim().is_empty() {
        match (
            std::fs::read(&j.snapshot_path),
            j.rule.vlm_prompt.as_deref(),
        ) {
            (Ok(bytes), Some(prompt)) if !prompt.trim().is_empty() => {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                vlm_confirm(&s, prompt, &b64)
            }
            // No snapshot / no prompt → can't verify → fail open.
            _ => None,
        }
    } else {
        None // captioner/model disabled → can't verify → fail open
    };
    if verdict == Some(false) {
        tracing::info!(rule = %j.rule.name, event = j.event_id, "vlm gate: suppressed (model said no)");
        return;
    }
    // P2.8b feedback learning: quiet this AI-verified fire if the event's object
    // crop looks like one the user thumbs-downed on this camera + label. The crop
    // embedding was produced by the detection pipeline's second pass and has
    // usually landed in the DB by now. **Fails OPEN** on any error / missing
    // embedding: a lookup failure or an event with no crop never suppresses.
    if let Ok(Some(crop)) = db.crop_embedding_for(j.event_id) {
        let sup = db
            .feedback_embeddings_for_camera(j.camera_id, &j.label)
            .unwrap_or_default();
        if crate::smart::any_similar(&crop, &sup, crate::smart::FEEDBACK_SUPPRESS_COSINE) {
            tracing::debug!(
                rule = %j.rule.name, event = j.event_id,
                "vlm gate: suppressed by feedback (crop matches a thumbs-down)"
            );
            return;
        }
    }
    // Describe-in-notification: reuse the caption the Caption job may have
    // already written, else generate one now (fail open — a model error just
    // fires a normal caption-less alert). Saved onto the event either way so
    // the UI shows what the push said.
    let caption = (j.rule.describe && s.genai_enabled && !s.genai_url.trim().is_empty())
        .then(|| match db.event_caption(j.event_id) {
            Ok(Some(c)) => Some(c),
            _ => std::fs::read(&j.snapshot_path).ok().and_then(|bytes| {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                let req = build_request(&s.genai_model, &prompt_for(&j.label, &j.camera), &b64);
                let caption = call_vision(&s.genai_url, &s.genai_api_key, req)
                    .ok()
                    .flatten()?;
                let _ = db.set_event_caption(j.event_id, &caption);
                Some(caption)
            }),
        })
        .flatten();
    let smtp = crate::notify::smtp_cfg(&s);
    let ev = crate::notify::AlarmEvent {
        event_id: j.event_id,
        camera: &j.camera,
        label: &j.label,
        score: j.score,
        ts: j.ts,
        snapshot_url: &j.snapshot_url,
        snapshot_path: Some(j.snapshot_path.as_path()),
        face: j.face.as_deref(),
        plate: j.plate.as_deref(),
        gesture: None,
        transcript: None,
        speed: None,
        base_url: &s.public_base_url,
        webhook_template: &s.webhook_template,
        smtp,
        duress: false,
        severity: j.severity,
        min_push_severity: s.notify_min_severity,
        caption: caption.as_deref(),
    };
    tracing::info!(
        rule = %j.rule.name, event = j.event_id, confirmed = ?verdict,
        described = caption.is_some(), "deferred alarm: firing"
    );
    crate::notify::fire(&j.rule, &ev, mqtt_tx, j.suppressed, db);
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

pub fn run(
    db: Db,
    rx: Receiver<Job>,
    mqtt_tx: std::sync::mpsc::Sender<crate::mqtt::EventMsg>,
    shutdown: Arc<AtomicBool>,
) {
    // Edge-triggered failure surface: notify once when the endpoint goes
    // unreachable, once when it recovers.
    let mut err_notified = false;
    while !shutdown.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Job::Caption(job)) => {
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
            Ok(Job::VlmGate(j)) => vlm_gate(&db, &j, &mqtt_tx),
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
    fn yes_no_interpretation() {
        assert_eq!(interpret_yes_no("Yes"), Some(true));
        assert_eq!(interpret_yes_no("  no.\n"), Some(false));
        assert_eq!(interpret_yes_no("YES, a person is at the door"), Some(true));
        assert_eq!(interpret_yes_no("No, there is nobody."), Some(false));
        assert_eq!(interpret_yes_no("yep"), Some(true));
        // "not"/"nobody" must NOT count as a "no" (whole-word matching).
        assert_eq!(interpret_yes_no("I'm not sure, maybe"), None);
        assert_eq!(interpret_yes_no(""), None);
        // Leading token wins when both appear: "yes and no" answers yes.
        assert_eq!(interpret_yes_no("yes and no"), Some(true));
        // A mid-sentence lone polarity with no leading answer word.
        assert_eq!(interpret_yes_no("definitely false"), Some(false));
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
