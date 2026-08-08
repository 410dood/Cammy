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

impl Job {
    fn kind(&self) -> &'static str {
        match self {
            Job::Caption(_) => "caption",
            Job::VlmGate(_) => "vlm_gate",
        }
    }
    /// Whether losing this job loses an ALERT. A `VlmGate` job is a rule that
    /// has already matched and already had its cooldown stamped + burst counter
    /// drained (`notify::ready` / `take_suppressed` run at the dispatch site,
    /// BEFORE the hand-off) — so nothing retries it and no later event repeats
    /// it. A `Caption` job is a sentence of description.
    fn is_alarm(&self) -> bool {
        matches!(self, Job::VlmGate(_))
    }
}

/// Queued captions allowed before new ones are shed. Deliberately far below
/// [`ALARM_CAP`] so a camera producing captions faster than the model can answer
/// can never crowd out an alarm fire.
const CAPTION_CAP: usize = 64;
/// Hard ceiling on the whole queue. Reaching it means the vision endpoint has
/// been unresponsive for a very long time; the alternative to shedding is
/// unbounded RAM growth and alerts that arrive hours late, which is not a
/// better outcome — but it is loud (warn! + an in-app notification), never silent.
const ALARM_CAP: usize = 512;

/// Whether a job may be queued at the current depth. Pure → unit-tested.
fn admits(is_alarm: bool, depth: usize) -> bool {
    depth < if is_alarm { ALARM_CAP } else { CAPTION_CAP }
}

/// Live queue counters, shared with `/api/metrics` (`AppState`) so a backlog is
/// observable from outside the process instead of only inferable from late alerts.
#[derive(Clone, Default)]
pub struct QueueStats {
    depth: Arc<std::sync::atomic::AtomicUsize>,
    shed: Arc<std::sync::atomic::AtomicUsize>,
}

impl QueueStats {
    /// Jobs waiting for the worker right now.
    pub fn depth(&self) -> usize {
        self.depth.load(Ordering::Relaxed)
    }
    /// Jobs dropped since startup because the queue was saturated.
    pub fn shed(&self) -> usize {
        self.shed.load(Ordering::Relaxed)
    }
}

/// The consumer half: TWO channels, because they hold work of different value.
/// The worker always takes an alarm before a caption.
pub struct Rx {
    alarms: Receiver<Job>,
    captions: Receiver<Job>,
}

/// The producer half. Each channel stays UNBOUNDED with an explicit depth counter
/// (the shape `notify::enqueue` uses) rather than a `sync_channel`: a bounded
/// `send` BLOCKS, and blocking here would stall the detection thread — the exact
/// failure the whole deferred-fire design exists to prevent.
///
/// Alarm fires and captions are separated so a slow model cannot make an ALERT
/// wait behind cosmetic work. Measured on this install with a stalled endpoint:
/// a fired VLM rule sat behind six queued captions, each burning a full 60 s
/// timeout — the alert would have been ~6 minutes late. One FIFO could not fix
/// that; two can.
#[derive(Clone)]
pub struct Queue {
    alarms: std::sync::mpsc::Sender<Job>,
    captions: std::sync::mpsc::Sender<Job>,
    stats: QueueStats,
}

impl Queue {
    pub fn new() -> (Queue, Rx, QueueStats) {
        let (atx, arx) = std::sync::mpsc::channel::<Job>();
        let (ctx, crx) = std::sync::mpsc::channel::<Job>();
        let stats = QueueStats::default();
        (
            Queue {
                alarms: atx,
                captions: ctx,
                stats: stats.clone(),
            },
            Rx {
                alarms: arx,
                captions: crx,
            },
            stats,
        )
    }

    /// Hand a job to the worker. `false` = shed (the caller should say so; for a
    /// `VlmGate` job that means an alert will never be delivered).
    pub fn send(&self, job: Job) -> bool {
        let is_alarm = job.is_alarm();
        let depth = self.stats.depth.load(Ordering::Relaxed);
        if !admits(is_alarm, depth) {
            self.stats.shed.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                kind = job.kind(),
                depth,
                "AI queue is saturated — dropping this job; the vision model is not keeping up"
            );
            return false;
        }
        self.stats.depth.fetch_add(1, Ordering::Relaxed);
        let tx = if is_alarm {
            &self.alarms
        } else {
            &self.captions
        };
        if tx.send(job).is_err() {
            self.stats.depth.fetch_sub(1, Ordering::Relaxed);
            return false; // worker gone
        }
        true
    }
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
        // Keep captions compact for the UI / push. Count CHARACTERS on both
        // sides: the guard used to test `len()` (BYTES) and then index by the
        // 280th CHAR, so any caption over 280 bytes but under 280 characters —
        // i.e. any non-ASCII reply, an accent or an emoji is enough — made
        // `nth(279)` return None and PANICKED the GenAI worker dead for the rest
        // of the process. Nothing restarts it, so captions and every deferred
        // VLM alarm fire would have stopped for good.
        if trimmed.chars().count() > 280 {
            format!("{}…", trimmed.chars().take(279).collect::<String>())
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
///
/// Also returns REACHABILITY, which the old `_ => None` threw away: "the model
/// answered something I can't parse" and "there is no model" both produced
/// `None`, so an owner whose endpoint was down could not be told — every
/// "AI-verified" rule just fired unverified, forever, in silence.
fn vlm_confirm(s: &crate::db::Settings, prompt: &str, image_b64: &str) -> (Option<bool>, Outcome) {
    let full = format!("{}\nAnswer with only one word: yes or no.", prompt.trim());
    let req = build_request(&s.genai_model, &full, image_b64);
    match call_vision(&s.genai_url, &s.genai_api_key, req) {
        Ok(Some(text)) => (interpret_yes_no(&text), Outcome::Reached),
        Ok(None) => (None, Outcome::Reached),
        Err(e) => (None, Outcome::Failed(e)),
    }
}

/// docs/10 P2.4 — the Alarms builder's "Test this question" (the last P1.10
/// carve-out): run a rule's yes/no question against one snapshot NOW,
/// synchronously, and hand back both the interpreted verdict and the model's
/// raw one-word reply so the user sees exactly what the gate will do.
pub fn vlm_ask(
    s: &crate::db::Settings,
    prompt: &str,
    image: &[u8],
) -> Result<(Option<bool>, String), String> {
    if !s.genai_enabled || s.genai_url.trim().is_empty() {
        return Err(
            "AI captions are off — enable them and set the AI server address in \
             Settings → AI event captions first"
                .to_string(),
        );
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(image);
    let full = format!("{}\nAnswer with only one word: yes or no.", prompt.trim());
    let req = build_request(&s.genai_model, &full, &b64);
    match call_vision(&s.genai_url, &s.genai_api_key, req)? {
        Some(text) => {
            let verdict = interpret_yes_no(&text);
            Ok((verdict, text))
        }
        None => Ok((None, String::new())),
    }
}

/// docs/10 P2.4 — probe an AI endpoint for its installed models so the
/// "vision model" field can become a picker with a real connected state.
/// Tries the Ollama shape first (`GET {origin}/api/tags` → `models[].name`),
/// then the OpenAI-compatible shape (`GET {origin}/v1/models` → `data[].id`).
/// Only the ORIGIN of the pasted URL is used, so it works whether the user
/// pasted `…/api/generate`, `…/v1`, or a bare host:port.
pub fn probe_models(url: &str, api_key: &str) -> Result<(String, Vec<String>), String> {
    let url = url.trim();
    let origin = {
        let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
            ("https", r)
        } else if let Some(r) = url.strip_prefix("http://") {
            ("http", r)
        } else {
            return Err("the address must start with http:// or https://".to_string());
        };
        let authority = rest.split('/').next().unwrap_or(rest);
        if authority.is_empty() || authority.chars().any(char::is_control) {
            return Err("that address has no host".to_string());
        }
        format!("{scheme}://{authority}")
    };
    let get = |path: &str| -> Result<serde_json::Value, String> {
        let mut call = ureq::get(&format!("{origin}{path}")).timeout(Duration::from_secs(6));
        if !api_key.trim().is_empty() {
            call = call.set("Authorization", &format!("Bearer {}", api_key.trim()));
        }
        match call.call() {
            Ok(resp) => resp
                .into_json::<serde_json::Value>()
                .map_err(|e| format!("{path}: response not JSON: {e}")),
            Err(ureq::Error::Status(code, _)) => Err(format!("{path}: HTTP {code}")),
            Err(e) => Err(format!("{e}")),
        }
    };
    // Ollama native.
    let ollama_err = match get("/api/tags") {
        Ok(body) => {
            if let Some(models) = body.get("models").and_then(|m| m.as_array()) {
                let names: Vec<String> = models
                    .iter()
                    .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                    .map(|s| s.to_string())
                    .collect();
                return Ok(("ollama".to_string(), names));
            }
            "unexpected /api/tags shape".to_string()
        }
        Err(e) => e,
    };
    // OpenAI-compatible (LM Studio, llama.cpp server, shims).
    match get("/v1/models") {
        Ok(body) => {
            if let Some(data) = body.get("data").and_then(|m| m.as_array()) {
                let names: Vec<String> = data
                    .iter()
                    .filter_map(|m| m.get("id").and_then(|n| n.as_str()))
                    .map(|s| s.to_string())
                    .collect();
                return Ok(("openai".to_string(), names));
            }
            Err(format!(
                "{origin} answered, but not like Ollama or an OpenAI-compatible server"
            ))
        }
        Err(openai_err) => Err(format!(
            "could not reach an AI server at {origin} ({ollama_err}; {openai_err})"
        )),
    }
}

/// VLM-verify a matched alarm and fire it if confirmed. Runs in the worker (off
/// the detection thread). **Fails OPEN**: fires unless the model gives a clear
/// "no", so a missing/unreachable model or an ambiguous reply never silently
/// suppresses a real alert.
///
/// Returns this job's endpoint reachability so the caller can raise the same
/// edge-triggered "unavailable/recovered" notification the caption path raises.
/// Failing open is right; failing open in SILENCE is not — it leaves the owner
/// believing an AI check happened on every alert when none did.
fn vlm_gate(
    db: &Db,
    j: &VlmGateJob,
    mqtt_tx: &std::sync::mpsc::Sender<crate::mqtt::EventMsg>,
) -> Outcome {
    let s = db.settings();
    let genai_on = s.genai_enabled && !s.genai_url.trim().is_empty();
    let mut outcome = Outcome::Skipped;
    let verdict = if genai_on {
        match (
            std::fs::read(&j.snapshot_path),
            j.rule.vlm_prompt.as_deref(),
        ) {
            (Ok(bytes), Some(prompt)) if !prompt.trim().is_empty() => {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                let (v, o) = vlm_confirm(&s, prompt, &b64);
                outcome = o;
                v
            }
            // No snapshot / no prompt → can't verify → fail open.
            _ => None,
        }
    } else {
        None // captioner/model disabled → can't verify → fail open
    };
    // The rule ASKED for verification and the endpoint could not answer. Say so
    // on the alert itself; anything else is a false claim of a check.
    let unverified = matches!(outcome, Outcome::Failed(_));
    if verdict == Some(false) {
        tracing::info!(rule = %j.rule.name, event = j.event_id, "vlm gate: suppressed (model said no)");
        return outcome;
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
            return outcome;
        }
    }
    // Describe-in-notification: reuse the caption the Caption job may have
    // already written, else generate one now (fail open — a model error just
    // fires a normal caption-less alert). Saved onto the event either way so
    // the UI shows what the push said.
    let mut caption: Option<String> = None;
    if j.rule.describe && genai_on {
        caption = match db.event_caption(j.event_id) {
            Ok(Some(c)) => Some(c),
            _ => match std::fs::read(&j.snapshot_path) {
                Ok(bytes) => {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    let req = build_request(&s.genai_model, &prompt_for(&j.label, &j.camera), &b64);
                    match call_vision(&s.genai_url, &s.genai_api_key, req) {
                        Ok(text) => {
                            // A `describe`-only rule makes no other call, so this
                            // is where its reachability comes from — without it a
                            // describe-only user got no signal either.
                            if matches!(outcome, Outcome::Skipped) {
                                outcome = Outcome::Reached;
                            }
                            if let Some(c) = text {
                                let _ = db.set_event_caption(j.event_id, &c);
                                Some(c)
                            } else {
                                None
                            }
                        }
                        Err(e) => {
                            outcome = Outcome::Failed(e);
                            None
                        }
                    }
                }
                Err(_) => None,
            },
        };
    }
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
        described = caption.is_some(), unverified, "deferred alarm: firing"
    );
    crate::notify::fire_unverified(&j.rule, &ev, mqtt_tx, j.suppressed, db, unverified);
    outcome
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

/// Decide the "AI queue backed up" notification (if any) for the current depth,
/// given whether we've already said so. Hysteresis on purpose: a queue that
/// hovers at the trigger must not toggle the bell. Pure → unit-tested.
fn backlog_transition(depth: usize, notified: bool) -> Option<bool> {
    // At CAPTION_CAP we are already shedding captions — that is the moment the
    // backlog stops being invisible slowness and starts costing work.
    if !notified && depth >= CAPTION_CAP {
        Some(true)
    } else if notified && depth <= CAPTION_CAP / 4 {
        Some(false)
    } else {
        None
    }
}

/// How long shutdown will keep firing QUEUED ALARMS before giving up. Captions
/// are abandoned immediately — they are cosmetic and each can cost 60 s.
const SHUTDOWN_DRAIN: Duration = Duration::from_secs(5);

pub fn run(
    db: Db,
    rx: Rx,
    stats: QueueStats,
    mqtt_tx: std::sync::mpsc::Sender<crate::mqtt::EventMsg>,
    shutdown: Arc<AtomicBool>,
) {
    // Edge-triggered failure surface: notify once when the endpoint goes
    // unreachable, once when it recovers.
    let mut err_notified = false;
    let mut backlog_notified = false;
    while !shutdown.load(Ordering::Relaxed) {
        // Report a backlog before taking the next job, so a queue that is
        // filling gets said out loud rather than only inferred from late alerts.
        if let Some(state) = backlog_transition(stats.depth(), backlog_notified) {
            let now = chrono::Utc::now().timestamp();
            let (title, body) = if state {
                (
                    "AI queue backed up",
                    format!(
                        "{} AI jobs are waiting and {} have been dropped. The vision model is \
                         answering slower than events arrive, so captions are being skipped and \
                         AI-verified alerts are late. Check the AI server in Settings.",
                        stats.depth(),
                        stats.shed()
                    ),
                )
            } else {
                (
                    "AI queue caught up",
                    "The vision model is keeping up again.".to_string(),
                )
            };
            let _ = db.add_notification(now, "genai_backlog", title, Some(&body), None);
            if state {
                tracing::warn!(depth = stats.depth(), shed = stats.shed(), "{title}");
            }
            backlog_notified = state;
        }
        // ALARMS FIRST, always. A caption is a sentence of description; an
        // alarm fire is the alert itself, and nothing retries it.
        let job = match rx.alarms.try_recv() {
            Ok(j) => Some(j),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
            // No alarm waiting: take a caption, blocking briefly so shutdown is
            // still noticed while both queues are idle.
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                match rx.captions.recv_timeout(Duration::from_millis(500)) {
                    Ok(j) => Some(j),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        };
        let Some(job) = job else { continue };
        stats.depth.fetch_sub(1, Ordering::Relaxed);
        match job {
            Job::Caption(job) => {
                let outcome = caption_one(&db, &job);
                report_reachability(&db, &outcome, &mut err_notified);
            }
            Job::VlmGate(j) => {
                // Same edge-triggered surface as the caption path: a
                // vlm_prompt-only owner used to get NO signal that their
                // model was down while every rule fired unverified.
                let outcome = vlm_gate(&db, &j, &mqtt_tx);
                report_reachability(&db, &outcome, &mut err_notified);
            }
        }
    }
    // Shutdown: what is still queued includes ALARM FIRES that nothing will ever
    // retry (the rule's cooldown was stamped at dispatch). Spend a bounded moment
    // delivering those; count and SAY what could not be delivered — the loop used
    // to just exit and drop the whole backlog in silence.
    let deadline = std::time::Instant::now() + SHUTDOWN_DRAIN;
    let (mut fired, mut lost_alarms, mut lost_captions) = (0usize, 0usize, 0usize);
    while let Ok(job) = rx.alarms.try_recv() {
        stats.depth.fetch_sub(1, Ordering::Relaxed);
        match job {
            Job::VlmGate(j) if std::time::Instant::now() < deadline => {
                let _ = vlm_gate(&db, &j, &mqtt_tx);
                fired += 1;
            }
            _ => lost_alarms += 1,
        }
    }
    while let Ok(_job) = rx.captions.try_recv() {
        stats.depth.fetch_sub(1, Ordering::Relaxed);
        lost_captions += 1;
    }
    if lost_alarms > 0 {
        tracing::warn!(
            lost_alarms,
            lost_captions,
            fired,
            "genai worker stopped with alarm verifications still queued — those alerts were NOT delivered"
        );
        let now = chrono::Utc::now().timestamp();
        let _ = db.add_notification(
            now,
            "genai_backlog",
            "Some alerts were not delivered",
            Some(&format!(
                "{lost_alarms} AI-verified alert(s) were still waiting on the vision model when \
                 Cammy shut down and could not be sent. The events themselves were recorded."
            )),
            None,
        );
    } else if fired > 0 || lost_captions > 0 {
        tracing::info!(fired, lost_captions, "genai worker drained on shutdown");
    }
}

/// Edge-triggered endpoint-reachability notification, shared by the caption path
/// and the VLM gate (an owner who uses only `vlm_prompt` rules used to get no
/// signal at all that their model was down — every "AI-verified" rule fired
/// unverified).
fn report_reachability(db: &Db, outcome: &Outcome, notified: &mut bool) {
    if let Some((new_state, title, body)) = err_transition(outcome, *notified) {
        let now = chrono::Utc::now().timestamp();
        let _ = db.add_notification(now, "genai_error", title, Some(&body), None);
        *notified = new_state;
        if new_state {
            tracing::warn!("genai endpoint unreachable: {title}");
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

    /// A multi-byte caption used to PANIC the worker dead: the length guard
    /// counted bytes and the slice counted characters, so anything over 280
    /// bytes but under 280 chars hit `nth(279).unwrap()` on a `None`. One
    /// accented word from the model would have ended captions AND every
    /// deferred VLM alarm fire for the life of the process.
    #[test]
    fn a_multibyte_caption_does_not_kill_the_worker() {
        // 200 chars, 400 bytes: over the old byte guard, under the char index.
        let two_hundred_accents = "é".repeat(200);
        let out = parse_response(&serde_json::json!({ "response": two_hundred_accents }))
            .expect("a caption");
        assert_eq!(out.chars().count(), 200, "short enough to keep whole");
        assert!(!out.ends_with('…'));
        // 400 chars → truncated on a character boundary, not a byte one.
        let long = "é".repeat(400);
        let out = parse_response(&serde_json::json!({ "response": long })).expect("a caption");
        assert_eq!(out.chars().count(), 280, "279 chars + the ellipsis");
        assert!(out.ends_with('…'));
        // Emoji (4-byte) too — the same trap with a bigger multiplier.
        let emoji = "🐈".repeat(100);
        let out = parse_response(&serde_json::json!({ "response": emoji })).expect("a caption");
        assert_eq!(out.chars().count(), 100);
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
    fn captions_are_shed_long_before_alarm_fires_are() {
        // A caption is a sentence; a VlmGate job is an alert nothing will retry.
        // Under the caption flood the alarm path must still have room.
        assert!(admits(false, CAPTION_CAP - 1));
        assert!(!admits(false, CAPTION_CAP));
        assert!(admits(true, CAPTION_CAP)); // …the alarm still gets in
        assert!(admits(true, ALARM_CAP - 1));
        assert!(!admits(true, ALARM_CAP));
        // The whole point of the split: an alarm arriving at a queue full of
        // captions is admitted, not dropped. Guarded at compile time so nobody
        // "tidies" the two caps into equality.
        const _: () = assert!(CAPTION_CAP < ALARM_CAP);
    }

    #[test]
    fn queue_sheds_captions_and_counts_them() {
        let (q, rx, stats) = Queue::new();
        let caption = || {
            Job::Caption(CaptionJob {
                event_id: 1,
                snapshot_path: PathBuf::from("x.jpg"),
                label: "person".into(),
                camera: "cam".into(),
            })
        };
        for _ in 0..CAPTION_CAP {
            assert!(q.send(caption()));
        }
        assert_eq!(stats.depth(), CAPTION_CAP);
        // Captions now shed…
        assert!(!q.send(caption()));
        assert_eq!(stats.shed(), 1);
        // …while the alarm path is unaffected.
        // Minimal rule: every other AlarmRule field carries `#[serde(default)]`.
        let rule: crate::db::AlarmRule = serde_json::from_value(serde_json::json!({
            "name": "r", "camera_id": null, "label": null,
            "face_like": null, "plate_like": null,
        }))
        .expect("minimal rule");
        assert!(q.send(Job::VlmGate(Box::new(VlmGateJob {
            rule,
            event_id: 7,
            camera: "cam".into(),
            camera_id: 1,
            label: "person".into(),
            score: 0.9,
            ts: 0,
            snapshot_url: String::new(),
            snapshot_path: PathBuf::from("x.jpg"),
            face: None,
            plate: None,
            severity: 2,
            suppressed: 0,
        }))));
        assert_eq!(stats.depth(), CAPTION_CAP + 1);
        assert_eq!(stats.shed(), 1);
        // …and it is FIRST in line, not behind the caption flood. Measured on
        // the live install: an alarm queued behind six stalled captions would
        // have been ~6 minutes late, which the single FIFO could not fix.
        assert!(
            matches!(rx.alarms.try_recv(), Ok(Job::VlmGate(_))),
            "the alarm must be immediately available while captions are backed up"
        );
        assert!(
            rx.alarms.try_recv().is_err(),
            "…and only the alarm is on that channel"
        );
        assert!(matches!(rx.captions.try_recv(), Ok(Job::Caption(_))));
    }

    #[test]
    fn backlog_notification_is_edge_triggered_with_hysteresis() {
        // Quiet below the trigger.
        assert_eq!(backlog_transition(0, false), None);
        assert_eq!(backlog_transition(CAPTION_CAP - 1, false), None);
        // Fires once on entry…
        assert_eq!(backlog_transition(CAPTION_CAP, false), Some(true));
        // …and not again while latched, even as it grows.
        assert_eq!(backlog_transition(CAPTION_CAP, true), None);
        assert_eq!(backlog_transition(ALARM_CAP, true), None);
        // Hysteresis: draining just under the trigger does NOT declare recovery,
        // so a queue hovering at the line can't toggle the bell.
        assert_eq!(backlog_transition(CAPTION_CAP - 1, true), None);
        assert_eq!(backlog_transition(CAPTION_CAP / 4, true), Some(false));
        assert_eq!(backlog_transition(0, true), Some(false));
        // Recovered state stays quiet.
        assert_eq!(backlog_transition(0, false), None);
    }

    /// The VLM gate used to collapse "the model gave an unparseable answer" and
    /// "there is no model" into the same `None`, so an unreachable endpoint could
    /// not be reported and every AI-verified rule fired unverified in silence.
    /// A closed local port is refused instantly, so this stays fast.
    #[test]
    fn vlm_confirm_reports_an_unreachable_endpoint() {
        let s = crate::db::Settings {
            genai_enabled: true,
            genai_url: "http://127.0.0.1:9/api/generate".into(),
            genai_model: "llava".into(),
            ..Default::default()
        };
        let (verdict, outcome) = vlm_confirm(&s, "Is a person there?", "QUJD");
        // Fails OPEN — the gate must not suppress on an outage…
        assert_eq!(verdict, None);
        // …but the outage is now REPORTABLE rather than indistinguishable from
        // an ambiguous reply, which is what feeds err_transition.
        assert!(
            matches!(outcome, Outcome::Failed(_)),
            "an unreachable endpoint must be Failed, not Reached"
        );
        // And that is exactly what stamps the alert as unverified.
        assert!(matches!(outcome, Outcome::Failed(_)));
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
