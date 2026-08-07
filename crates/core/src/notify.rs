//! Alarm action dispatch, shared by the video pipeline and the audio worker.
//! Actions: webhook (JSON POST), mqtt (custom topic), ntfy (phone push with
//! the snapshot attached — the self-hoster standard; works with ntfy.sh or a
//! private ntfy server, no account required).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::db::{Action, AlarmRule, Db};
use crate::mqtt::EventMsg;

/// Shared per-rule throttle state (rule id → (last-fired unix seconds, events
/// suppressed since then)). Lives in memory and is consulted by every dispatch
/// site (video pipeline, audio worker, the gesture endpoint) so a rule's
/// cooldown holds across cameras, detections and ticks without a DB round-trip
/// per event. The suppressed counter feeds the burst-consolidated push ("+N
/// more during cooldown") so throttled activity is summarized, not silently
/// dropped — see [`take_suppressed`].
pub type AlarmThrottle = Arc<Mutex<HashMap<i64, (i64, u32)>>>;

/// SMTP config for the "email" alarm action, borrowed from Settings at the
/// dispatch site. `to` is the default recipient(s) (comma-separated); an action
/// can override it with its own `target`.
pub struct SmtpConfig<'a> {
    pub url: &'a str,
    pub user: &'a str,
    pub pass: &'a str,
    pub from: &'a str,
    pub to: &'a str,
}

/// Queued deliveries allowed before new ones are dropped. Generous: a healthy
/// target drains this instantly, so reaching the cap means the target is down,
/// and those alerts are already worthless. Dropping them is strictly better than
/// the alternative it replaces — stalling detection on every camera.
const DISPATCH_CAP: usize = 512;

struct Dispatch {
    /// (rule name, event id, job) — the ids ride along so a failure logged
    /// from the worker carries the same context the inline path did.
    tx: std::sync::mpsc::Sender<(String, i64, Outbound)>,
    depth: Arc<std::sync::atomic::AtomicUsize>,
    /// Alerts thrown away because the queue was saturated. A `warn!` was the
    /// only trace of these, which means the one person who needs to know an
    /// alert never arrived — the owner, who is not reading the log — never did.
    dropped: Arc<std::sync::atomic::AtomicUsize>,
}

/// KV key holding the lifetime dropped-alert count, so the number survives a
/// restart instead of resetting to zero and looking healthy.
const KV_ALARM_DROPS: &str = "alarm_drops_total";

/// Live alarm-delivery queue counters `(depth, dropped)`, or `None` when no
/// dispatcher is running (unit tests, `--verify`). Exported at `/api/metrics`.
pub fn dispatch_stats() -> Option<(usize, usize)> {
    use std::sync::atomic::Ordering;
    let d = DISPATCH.get()?;
    Some((
        d.depth.load(Ordering::Relaxed),
        d.dropped.load(Ordering::Relaxed),
    ))
}

/// Decide the in-app notification (if any) for the drop counter, given how many
/// drops have already been reported and whether we are currently latched.
/// Edge-triggered like the offsite/health latches: one notification when alerts
/// start being dropped, one when the queue has drained. Pure → unit-tested.
fn drop_transition(
    dropped: usize,
    acknowledged: usize,
    depth: usize,
    notified: bool,
) -> Option<bool> {
    if !notified {
        (dropped > acknowledged).then_some(true)
    } else {
        // Recovery is "the backlog is gone", not "no drop in the last tick" —
        // a target that is still saturating the queue has not recovered.
        (depth == 0).then_some(false)
    }
}

/// Process-global and set-once, so `fire` needs no extra parameter at any of
/// its eleven call sites. NOTE for tests: once any test starts a dispatcher,
/// every later `fire` in that binary queues instead of delivering inline.
/// Only one test currently dispatches; a second one asserting on delivery
/// outcomes would need to account for this.
static DISPATCH: std::sync::OnceLock<Dispatch> = std::sync::OnceLock::new();

/// Start the alarm-delivery worker. Call once at startup; `lib.rs` joins the
/// returned handle at shutdown so queued alerts drain instead of vanishing.
///
/// Until this is called (unit tests, the `--verify` CLI) every delivery happens
/// inline exactly as before, so nothing silently no-ops in a context that has no
/// worker.
pub fn start_dispatch(
    db: crate::db::Db,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<()> {
    use std::sync::atomic::Ordering;
    let (tx, rx) = std::sync::mpsc::channel::<(String, i64, Outbound)>();
    let depth = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (worker_depth, worker_dropped) = (depth.clone(), dropped.clone());
    if DISPATCH.set(Dispatch { tx, depth, dropped }).is_err() {
        tracing::warn!("alarm dispatch already started");
    }
    std::thread::Builder::new()
        .name("alarm-dispatch".into())
        .spawn(move || {
            // On shutdown, finish the backlog — but only for this long. Each
            // delivery can burn its full 10 s timeout, so a queue pointed at a
            // dead target would otherwise hold the whole process open for many
            // minutes, right when the user is waiting for it to close.
            const SHUTDOWN_DRAIN: Duration = Duration::from_secs(5);
            let mut deadline: Option<std::time::Instant> = None;
            // Edge-triggered drop reporting. `acknowledged` is the drop total the
            // owner has already been told about, so a continuing outage doesn't
            // re-ring the bell and a NEW outage after recovery does.
            let mut acknowledged: usize = 0;
            let mut drop_notified = false;
            loop {
                if deadline.is_none() && shutdown.load(Ordering::Relaxed) {
                    deadline = Some(std::time::Instant::now() + SHUTDOWN_DRAIN);
                }
                if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                    break;
                }
                let dropped_now = worker_dropped.load(Ordering::Relaxed);
                if let Some(state) = drop_transition(
                    dropped_now,
                    acknowledged,
                    worker_depth.load(Ordering::Relaxed),
                    drop_notified,
                ) {
                    let now = chrono::Utc::now().timestamp();
                    if state {
                        let n = dropped_now - acknowledged;
                        // Lifetime total, persisted so a restart can't make a
                        // history of dropped alerts look like a clean slate.
                        let lifetime = db
                            .get_kv(KV_ALARM_DROPS)
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(0)
                            + n;
                        let _ = db.set_kv(KV_ALARM_DROPS, &lifetime.to_string());
                        tracing::warn!(dropped = n, lifetime, "alerts dropped: delivery queue full");
                        let _ = db.add_notification(
                            now,
                            "alarm_undelivered",
                            &format!(
                                "{n} alert{} could not be delivered",
                                if n == 1 { "" } else { "s" }
                            ),
                            Some(&format!(
                                "The alert delivery queue filled up, so {n} alert{} {} thrown away \
                                 without being sent. This means a webhook, push or email target is \
                                 not responding — check the targets on your alarm rules. \
                                 ({lifetime} dropped in total since this install began.)",
                                if n == 1 { "" } else { "s" },
                                if n == 1 { "was" } else { "were" }
                            )),
                            None,
                        );
                    } else {
                        tracing::info!("alert delivery queue drained");
                        let _ = db.add_notification(
                            now,
                            "alarm_undelivered",
                            "Alert delivery caught up",
                            Some("The alert delivery queue has drained; alerts are being sent again."),
                            None,
                        );
                    }
                    acknowledged = dropped_now;
                    drop_notified = state;
                }
                // Wake periodically rather than blocking forever, so shutdown is
                // noticed even while the queue is idle.
                match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok((rule, event, job)) => {
                        worker_depth.fetch_sub(1, Ordering::Relaxed);
                        let kind = job.kind();
                        if let Err(e) = job.deliver() {
                            // The owner needs to know an alert never arrived.
                            tracing::warn!(
                                rule = %rule, event, kind,
                                "alarm action FAILED to deliver: {e}"
                            );
                        }
                    }
                    // Idle: done if we are shutting down, since the queue is empty.
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if deadline.is_some() {
                            break;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            let abandoned = worker_depth.load(Ordering::Relaxed);
            if abandoned > 0 {
                tracing::warn!(
                    abandoned,
                    "alarm dispatch stopped with deliveries still queued (target too slow)"
                );
            } else {
                tracing::info!("alarm dispatch stopped");
            }
        })
        .expect("spawning the alarm dispatch thread")
}

/// Hand a delivery to the worker.
///
/// `None` = taken care of (queued, or deliberately dropped because the queue is
/// saturated). `Some(job)` hands the job back because there is no worker at all
/// — unit tests and the `--verify` CLI never start one — and the caller should
/// perform it inline, exactly as before this queue existed.
fn enqueue(rule_name: &str, event_id: i64, job: Outbound) -> Option<Outbound> {
    use std::sync::atomic::Ordering;
    let d = DISPATCH.get()?;
    if d.depth.load(Ordering::Relaxed) >= DISPATCH_CAP {
        // Counted, not just logged: the worker turns this into ONE in-app
        // notification per outage so the owner learns an alert never arrived.
        d.dropped.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            rule = %rule_name, kind = job.kind(),
            "alarm delivery queue is full ({DISPATCH_CAP}) — dropping this one; \
             the target is not keeping up"
        );
        // Dropped on purpose: falling back to inline here would re-introduce
        // exactly the detection-thread stall this queue exists to prevent.
        return None;
    }
    d.depth.fetch_add(1, Ordering::Relaxed);
    match d.tx.send((rule_name.to_string(), event_id, job)) {
        Ok(()) => None,
        Err(e) => {
            d.depth.fetch_sub(1, Ordering::Relaxed);
            Some(e.0 .2) // worker gone — give the job back
        }
    }
}

/// Edge-triggered health for the GLOBAL webhook (`Settings.webhook_url`) — the
/// every-event feed most installs wire into Home Assistant. Shared by all three
/// senders (detections, analytics/residential events, hand signals) so one
/// unreachable endpoint produces ONE notification, not one per source.
static GLOBAL_WEBHOOK: crate::degraded::Latch = crate::degraded::Latch::new();

/// POST one event to the global webhook and surface a delivery failure.
///
/// Every caller used to throw the error away at `debug!` (or with `let _ =`).
/// The per-RULE webhook path was made loud in `b6b42ee`; this one — the main
/// integration feed, which fails for exactly the same reasons — was missed, so
/// an owner whose Home Assistant automation had silently stopped receiving
/// anything had nothing anywhere to tell them.
///
/// Runs inline on the caller's thread (unchanged): this is a 3 s POST, not the
/// 10 s snapshot PUT that made rule actions worth queueing.
pub fn post_global_webhook(db: &Db, url: &str, body: &str) {
    let outcome = ureq::post(url)
        .timeout(Duration::from_secs(3))
        .set("Content-Type", "application/json")
        .send_string(body);
    let err = outcome.err().map(|e| e.to_string());
    GLOBAL_WEBHOOK.report(
        db,
        err.as_deref(),
        &crate::degraded::Messages {
            kind: "webhook_error",
            down_title: "Webhook is not receiving events",
            down_body: "Cammy could not reach the webhook address in Settings, so anything \
                        listening on it — a Home Assistant automation, a script — has stopped \
                        being told about events. Alerts set up as alarm rules are unaffected.",
            up_title: "Webhook is receiving events again",
            up_body: "The webhook address in Settings is reachable again.",
        },
    );
}

/// An owned copy of [`SmtpConfig`], so a built message can cross a thread
/// boundary into the dispatch worker.
#[derive(Clone)]
pub struct OwnedSmtp {
    url: String,
    user: String,
    pass: String,
    from: String,
    to: String,
}

impl OwnedSmtp {
    fn of(c: &SmtpConfig) -> Self {
        Self {
            url: c.url.to_string(),
            user: c.user.to_string(),
            pass: c.pass.to_string(),
            from: c.from.to_string(),
            to: c.to.to_string(),
        }
    }
    fn borrow(&self) -> SmtpConfig<'_> {
        SmtpConfig {
            url: &self.url,
            user: &self.user,
            pass: &self.pass,
            from: &self.from,
            to: &self.to,
        }
    }
}

/// One fully-prepared outbound delivery. Everything is owned, so it can be
/// handed to the dispatch worker; performing it is the ONLY part that touches
/// the network.
///
/// The split exists because `fire` runs inline on the detection thread. An ntfy
/// push is a `PUT` with the snapshot attached on a 10-second timeout, so a
/// single unreachable push server stalled detection for every camera (the
/// default `detect_workers` is 1) for ten seconds per firing rule. Building the
/// request is pure string work; only `deliver` blocks.
enum Outbound {
    Webhook {
        url: String,
        body: String,
        content_type: &'static str,
    },
    Ntfy {
        url: String,
        title: String,
        tags: &'static str,
        priority: u8,
        actions: Option<String>,
        message: String,
        snapshot: Option<Vec<u8>>,
    },
    Email {
        cfg: OwnedSmtp,
        msg: Box<lettre::Message>,
    },
}

impl Outbound {
    /// A short label for logs — the action kind this delivery belongs to.
    fn kind(&self) -> &'static str {
        match self {
            Outbound::Webhook { .. } => "webhook",
            Outbound::Ntfy { .. } => "ntfy",
            Outbound::Email { .. } => "email",
        }
    }

    /// Perform the delivery. Blocking; runs on the dispatch worker (or inline
    /// for a clicked Test, which wants the answer).
    fn deliver(self) -> Result<(), String> {
        match self {
            Outbound::Webhook {
                url,
                body,
                content_type,
            } => ureq::post(&url)
                .timeout(Duration::from_secs(3))
                .set("Content-Type", content_type)
                .send_string(&body)
                .map(|_| ())
                .map_err(|e| e.to_string()),
            Outbound::Ntfy {
                url,
                title,
                tags,
                priority,
                actions,
                message,
                snapshot,
            } => {
                let apply = |req: ureq::Request| {
                    let mut req = req.set("X-Title", &title).set("X-Tags", tags);
                    if (1..=5).contains(&priority) {
                        req = req.set("X-Priority", &priority.to_string());
                    }
                    if let Some(a) = &actions {
                        req = req.set("X-Actions", a);
                    }
                    req
                };
                let result = match snapshot {
                    Some(bytes) => apply(ureq::put(&url).timeout(Duration::from_secs(10)))
                        .set("X-Message", &message)
                        .set("Filename", "snapshot.jpg")
                        .send_bytes(&bytes),
                    None => apply(ureq::post(&url).timeout(Duration::from_secs(10)))
                        .send_string(&message),
                };
                result.map(|_| ()).map_err(|e| e.to_string())
            }
            Outbound::Email { cfg, msg } => send_built(&cfg.borrow(), *msg),
        }
    }
}

/// Synchronous test delivery for the Settings "Send a test" buttons (docs/10
/// P2.3): a wrong URL should fail loudly at configuration time, not at the
/// first real alarm. Returns the transport error verbatim. Blocking — call
/// from a handler via `spawn_blocking`.
pub fn test_target(kind: &str, target: &str, settings: &crate::db::Settings) -> Result<(), String> {
    match kind {
        "email" => {
            let cfg = smtp_cfg(settings).ok_or_else(|| {
                "SMTP isn't configured — set the server URL under Settings → Email first"
                    .to_string()
            })?;
            let to = if target.trim().is_empty() { cfg.to } else { target };
            if to.trim().is_empty() {
                return Err("no recipient — enter an address, or set a default recipient".into());
            }
            let from = cfg
                .from
                .trim()
                .parse()
                .map_err(|e| format!("bad from address {:?}: {e}", cfg.from))?;
            let msg = lettre::Message::builder()
                .from(from)
                .to(to.trim().parse().map_err(|e| format!("bad recipient: {e}"))?)
                .subject("Cammy test email")
                .body("Test email from Cammy — alarm emails will arrive like this.".to_string())
                .map_err(|e| format!("building the message: {e}"))?;
            send_built(&cfg, msg)
        }
        "webhook" => Outbound::Webhook {
            url: target.to_string(),
            body: r#"{"test":true,"source":"cammy","message":"Test delivery — your alarm webhooks will POST here."}"#.to_string(),
            content_type: "application/json",
        }
        .deliver(),
        "ntfy" => Outbound::Ntfy {
            url: target.to_string(),
            title: "Cammy test".to_string(),
            tags: "white_check_mark",
            priority: 0,
            actions: None,
            message: "Test push from Cammy — alerts to this topic will arrive like this."
                .to_string(),
            snapshot: None,
        }
        .deliver(),
        _ => Err(format!("unknown test kind '{kind}'")),
    }
}

/// Borrow an SmtpConfig from Settings when SMTP is configured (URL set), for the
/// `smtp` field of an AlarmEvent at each dispatch site. `None` = email off.
pub fn smtp_cfg(s: &crate::db::Settings) -> Option<SmtpConfig<'_>> {
    (!s.smtp_url.trim().is_empty()).then(|| SmtpConfig {
        url: &s.smtp_url,
        user: &s.smtp_user,
        pass: &s.smtp_pass,
        from: &s.smtp_from,
        to: &s.smtp_to,
    })
}

pub struct AlarmEvent<'a> {
    pub event_id: i64,
    pub camera: &'a str,
    pub label: &'a str,
    pub score: f32,
    pub ts: i64,
    /// Web path, e.g. "/api/snapshots/x.jpg" (for payload consumers).
    pub snapshot_url: &'a str,
    /// Local file, for attaching the image to push notifications.
    pub snapshot_path: Option<&'a Path>,
    pub face: Option<&'a str>,
    pub plate: Option<&'a str>,
    pub gesture: Option<&'a str>,
    /// Speech-to-text transcript (for spoken-keyword alarms) — carried in the
    /// webhook payload and shown in the push so the receiver sees what was said.
    pub transcript: Option<&'a str>,
    /// Estimated ground speed (km/h) for a calibrated traffic-analytics event;
    /// `None` for events without a ground calibration. Exposed as `{{speed}}`.
    pub speed: Option<f32>,
    /// Public base URL (e.g. "https://nvr.example.com"); when set, pushes carry
    /// tap-through "View clip" / "Snapshot" action links. Empty = no links.
    pub base_url: &'a str,
    /// Optional webhook body template ({{placeholder}} form). Empty = default
    /// detection JSON.
    pub webhook_template: &'a str,
    /// SMTP config for an "email" action; `None` = email not configured.
    pub smtp: Option<SmtpConfig<'a>>,
    /// Duress/panic event: force max push urgency and a distinct alarm tag.
    pub duress: bool,
    /// Severity tier 1..4 (see `crate::severity`); drives the default ntfy
    /// priority and the `notify_min_severity` gate. Dispatch sites overlay
    /// duress as 4.
    pub severity: u8,
    /// `Settings.notify_min_severity` at dispatch time: ntfy/email actions are
    /// skipped when `severity` is below it (duress excepted). 0/1 = no gate.
    pub min_push_severity: u8,
    /// GenAI description of the snapshot, when a describe-in-notification rule
    /// fired through the GenAI worker — leads the push text and fills
    /// `{{caption}}`. `None` on the normal inline path.
    pub caption: Option<&'a str>,
}

/// JSON-escape a value so substituting it into a JSON template stays valid.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Render a webhook body template, substituting `{{key}}` placeholders with the
/// event's fields (JSON-escaped). Unknown placeholders are left untouched.
pub fn render_template(tpl: &str, ev: &AlarmEvent) -> String {
    let fields: [(&str, String); 13] = [
        ("event_id", ev.event_id.to_string()),
        ("camera", json_escape(ev.camera)),
        ("label", json_escape(ev.label)),
        ("score", format!("{:.3}", ev.score)),
        ("ts", ev.ts.to_string()),
        ("snapshot", json_escape(ev.snapshot_url)),
        ("face", json_escape(ev.face.unwrap_or(""))),
        ("plate", json_escape(ev.plate.unwrap_or(""))),
        ("gesture", json_escape(ev.gesture.unwrap_or(""))),
        ("transcript", json_escape(ev.transcript.unwrap_or(""))),
        (
            "speed",
            ev.speed.map(|s| format!("{s:.0}")).unwrap_or_default(),
        ),
        ("caption", json_escape(ev.caption.unwrap_or(""))),
        ("severity", ev.severity.to_string()),
    ];
    let mut out = tpl.to_string();
    for (k, v) in &fields {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

/// Is the rule clear to fire right now? False when snoozed or still inside its
/// per-rule cooldown. On a `true` result the rule is stamped as fired `now`, so
/// callers should fire exactly when this returns true (no double-firing). A
/// suppressed match increments the rule's burst counter, which the next real
/// fire drains via [`take_suppressed`] into a "+N more" summary.
pub fn ready(rule: &AlarmRule, throttle: &AlarmThrottle, now: i64) -> bool {
    let mut map = throttle.lock().expect("alarm throttle poisoned");
    let suppressed = |map: &mut HashMap<i64, (i64, u32)>| {
        map.entry(rule.id).or_insert((0, 0)).1 += 1;
    };
    if rule.snooze_until > now {
        suppressed(&mut map);
        return false;
    }
    if rule.cooldown_secs > 0 {
        if let Some(&(last, _)) = map.get(&rule.id) {
            if now - last < rule.cooldown_secs {
                suppressed(&mut map);
                return false;
            }
        }
    }
    // Fire: stamp the clock, keep the accumulated burst count for take_suppressed.
    let count = map.get(&rule.id).map(|&(_, n)| n).unwrap_or(0);
    map.insert(rule.id, (now, count));
    true
}

/// Drain the rule's burst counter — how many matches its cooldown/snooze
/// swallowed since it last fired. Call exactly once per real fire (right after
/// `ready` returned true) and pass the count into [`fire`] so the push reads
/// "person on Driveway (+3 more during cooldown)" instead of losing them.
pub fn take_suppressed(throttle: &AlarmThrottle, rule_id: i64) -> u32 {
    let mut map = throttle.lock().expect("alarm throttle poisoned");
    match map.get_mut(&rule_id) {
        Some(entry) => std::mem::take(&mut entry.1),
        None => 0,
    }
}

/// Whether a rule is armed in the current system security mode (UniFi-style
/// Home/Away/Disarmed). An empty `modes` list means "armed in every *armed*
/// mode" (home + away) but suppressed while the system is "disarmed". A rule
/// that explicitly lists "disarmed" still fires while disarmed — a panic rule.
/// Callers OR this with the per-event `duress` flag so a panic always fires.
pub fn armed_in_mode(modes: &[String], arm_mode: &str) -> bool {
    if arm_mode == "disarmed" {
        modes.iter().any(|m| m == "disarmed")
    } else {
        modes.is_empty() || modes.iter().any(|m| m == arm_mode)
    }
}

/// Should a HUMAN-facing channel (ntfy/email) deliver this event, given the
/// global `notify_min_severity` gate? Automations (webhook/MQTT) are never
/// gated, and duress always delivers. Pure → unit-tested.
fn push_allowed(severity: u8, min_push_severity: u8, duress: bool) -> bool {
    duress || severity >= min_push_severity
}

/// Fire a matched rule's actions — a "scene" can be several at once (push AND
/// webhook AND …). Failures are logged and swallowed; notification problems
/// must never stall detection. `effective_actions` falls back to the legacy
/// single action for pre-scenes rules. `suppressed` is the rule's drained burst
/// counter ([`take_suppressed`]) — matches its cooldown swallowed since the
/// last fire, summarized into the push text.
///
/// P2.11: besides the rule author's own actions (webhook/mqtt/ntfy/email to
/// their explicit targets — dispatched EXACTLY as before), each fire also records
/// ONE `alarm` notification row tagged with the rule and (for a real event) the
/// camera. The async push worker consumes that row to deliver per-user PUSH +
/// EMAIL — so NO network I/O (SMTP included) happens here, keeping this off the
/// hot detection thread.
pub fn fire(
    rule: &AlarmRule,
    ev: &AlarmEvent,
    mqtt_tx: &std::sync::mpsc::Sender<EventMsg>,
    suppressed: u32,
    db: &Db,
) -> Vec<ActionOutcome> {
    fire_unverified(rule, ev, mqtt_tx, suppressed, db, false)
}

/// [`fire`], plus the fact that this alert was supposed to be AI-verified and
/// could not be (the vision endpoint was unreachable).
///
/// The VLM gate FAILS OPEN by design — a model that cannot be reached must never
/// swallow a real alert. But firing anyway while saying nothing lets the owner
/// believe a check happened that did not, which is the more expensive kind of
/// wrong. Only the VLM gate passes `true`; every other dispatch site goes
/// through [`fire`].
pub fn fire_unverified(
    rule: &AlarmRule,
    ev: &AlarmEvent,
    mqtt_tx: &std::sync::mpsc::Sender<EventMsg>,
    suppressed: u32,
    db: &Db,
    unverified: bool,
) -> Vec<ActionOutcome> {
    tracing::info!(rule = %rule.name, event = ev.event_id, suppressed, "alarm triggered");
    let outcomes: Vec<ActionOutcome> = rule
        .effective_actions()
        .iter()
        .map(|action| fire_action(action, &rule.name, ev, mqtt_tx, suppressed, db))
        .collect();
    // A synthetic/test fire (event_id 0) exercises ONLY the rule's own configured
    // actions above — it must not create a per-user notification (no bell entry /
    // push / email to everyone), so bail out here.
    if ev.event_id == 0 {
        return outcomes;
    }
    // Resolve the camera id from the camera NAME first. A NULL camera_id makes the
    // push worker skip the per-user camera-visibility gate (fail-OPEN), so we must
    // NOT depend on get_event, whose error would silently yield None. camera_by_name
    // is the reliable path; get_event is only a fallback, and if BOTH miss for a
    // real event we log so the NULL is explained, never silent.
    let camera_id = match db.camera_by_name(ev.camera) {
        Ok(Some(cid)) => Some(cid),
        _ => {
            let via_event = db
                .get_event(ev.event_id)
                .ok()
                .flatten()
                .map(|e| e.camera_id);
            if via_event.is_none() {
                tracing::warn!(
                    event = ev.event_id, camera = %ev.camera,
                    "alarm notification: unresolved camera id — per-user camera scoping cannot apply"
                );
            }
            via_event
        }
    };
    let title = if ev.duress {
        format!("DURESS — {}", rule.name)
    } else {
        rule.name.clone()
    };
    let mut body = format!("{} ({:.0}%) on {}", ev.label, ev.score * 100.0, ev.camera);
    if let Some(c) = ev.caption {
        body = format!("{c} — {body}");
    }
    if let Some(f) = ev.face {
        body.push_str(&format!(" — {f}"));
    }
    if let Some(p) = ev.plate {
        body.push_str(&format!(" — plate {p}"));
    }
    if suppressed > 0 {
        body.push_str(&format!(" (+{suppressed} more while muted by cooldown)"));
    }
    if unverified {
        // Sent BECAUSE the check failed, not after it passed. Without this the
        // alert is indistinguishable from a verified one.
        body.push_str(" — sent WITHOUT the AI check (the vision model could not be reached)");
    }
    if let Err(e) = db.add_alarm_notification(
        ev.ts,
        "alarm",
        &title,
        Some(&body),
        Some(ev.event_id),
        Some(rule.id),
        camera_id,
        Some(ev.severity as i64),
    ) {
        // The bell row failing means the in-app notification (and the per-user
        // push/email the worker derives from it) never happened — a delivery
        // failure like any other, so it warns rather than hiding at debug.
        tracing::warn!("alarm notification insert failed: {e:#}");
    }
    outcomes
}

/// What one action dispatch actually did — so a clicked "Test" can report the
/// truth instead of an unconditional success.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ActionOutcome {
    pub kind: String,
    pub ok: bool,
    /// Why it failed, or why it was deliberately skipped. `None` on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ActionOutcome {
    fn ok(kind: &str) -> Self {
        Self {
            kind: kind.to_string(),
            ok: true,
            detail: None,
        }
    }
    fn failed(kind: &str, detail: impl Into<String>) -> Self {
        Self {
            kind: kind.to_string(),
            ok: false,
            detail: Some(detail.into()),
        }
    }
    /// Not attempted, and correctly so (e.g. gated below `notify_min_severity`).
    /// Reported as ok — nothing is broken — but with the reason attached.
    fn skipped(kind: &str, why: impl Into<String>) -> Self {
        Self {
            kind: kind.to_string(),
            ok: true,
            detail: Some(why.into()),
        }
    }
}

fn fire_action(
    action: &Action,
    rule_name: &str,
    ev: &AlarmEvent,
    mqtt_tx: &std::sync::mpsc::Sender<EventMsg>,
    suppressed: u32,
    db: &Db,
) -> ActionOutcome {
    // One-knob fatigue gate: quiet the human channels below the configured
    // severity; automations still see everything. (Deterrence is a physical
    // automation, never a human-facing push, so it is intentionally NOT gated
    // here — its own master switch governs it below.)
    if matches!(action.kind.as_str(), "ntfy" | "email")
        && !push_allowed(ev.severity, ev.min_push_severity, ev.duress)
    {
        tracing::info!(
            rule = rule_name, event = ev.event_id, severity = ev.severity,
            min = ev.min_push_severity, kind = %action.kind,
            "push skipped: below notify_min_severity"
        );
        return ActionOutcome::skipped(
            &action.kind,
            "skipped: this alert is below your minimum notification severity",
        );
    }
    // Channels that touch the network are BUILT here (pure string work) and
    // performed on the dispatch worker; MQTT is already a channel send and
    // deterrence already offloads its SOAP call, so both stay inline.
    let job: Result<Option<Outbound>, String> = match action.kind.as_str() {
        "webhook" => Ok(Some(build_webhook(&action.target, ev))),
        "mqtt" => mqtt_tx
            .send(EventMsg {
                event_id: ev.event_id,
                camera: ev.camera.to_string(),
                label: ev.label.to_string(),
                score: ev.score,
                ts: ev.ts,
                snapshot: ev.snapshot_url.to_string(),
                topic: Some(format!("alarms/{}", action.target)),
            })
            .map(|()| None)
            .map_err(|e| format!("the MQTT worker is not accepting events: {e}")),
        "ntfy" => Ok(Some(build_ntfy(
            &action.target,
            rule_name,
            action.priority,
            ev,
            suppressed,
        ))),
        "email" => build_email(&action.target, rule_name, ev, suppressed).map(Some),
        "deterrence" => {
            deterrence(&action.target, rule_name, ev, db);
            Ok(None)
        }
        other => Err(format!("unknown alarm action {other:?}")),
    };

    let result: Result<(), String> = match job {
        Err(e) => Err(e),
        Ok(None) => Ok(()), // nothing to send (mqtt/deterrence), or a build-time skip
        Ok(Some(job)) => {
            // `event_id == 0` is the synthetic fire behind the alarm Test button
            // — the ONLY place in the tree that builds one. A clicked test must
            // wait and report the real answer, so it delivers inline. Everything
            // else is a live detection running on the detection thread and must
            // not block on a network round trip.
            if ev.event_id == 0 {
                job.deliver()
            } else {
                match enqueue(rule_name, ev.event_id, job) {
                    None => return ActionOutcome::skipped(&action.kind, "queued for delivery"),
                    // No worker running (tests / CLI): behave exactly as before.
                    Some(job) => job.deliver(),
                }
            }
        }
    };
    // An alert that did NOT reach anyone is the single most important thing this
    // module can report, and every channel used to swallow it at debug level —
    // invisible in normal operation. It is a warning now, and the outcome also
    // travels back to the caller so the "Test" button can stop claiming success
    // for a webhook that never connected.
    match result {
        Ok(()) => ActionOutcome::ok(&action.kind),
        Err(detail) => {
            tracing::warn!(
                rule = rule_name, event = ev.event_id, kind = %action.kind,
                "alarm action FAILED to deliver: {detail}"
            );
            ActionOutcome::failed(&action.kind, detail)
        }
    }
}

/// P2.9 deterrence action: pulse a camera's ONVIF relay output (siren / strobe /
/// light). Master-switch gated and fail-soft — arm-mode gating already happened
/// upstream (every dispatch site ANDs `armed_in_mode`), so there is no new
/// gating here beyond the honesty kill-switch.
fn deterrence(target_token: &str, rule_name: &str, ev: &AlarmEvent, db: &Db) {
    // Honesty kill-switch: with the master toggle off a "deterrence" action does
    // NOTHING physical — no SOAP is sent, no siren fires.
    if !db.settings().deterrence_enabled {
        tracing::info!(
            rule = rule_name,
            event = ev.event_id,
            "deterrence action skipped: master switch off"
        );
        return;
    }
    let token = target_token.trim();
    if token.is_empty() {
        tracing::warn!(
            rule = rule_name,
            "deterrence action has no relay token — skipping"
        );
        return;
    }
    // Resolve the firing camera → its ONVIF host+creds. Fail-soft at each hop.
    let cam = match db.camera_by_name(ev.camera) {
        Ok(Some(cid)) => db.get_camera(cid).ok().flatten(),
        _ => None,
    };
    let Some(cam) = cam else {
        tracing::warn!(
            rule = rule_name, camera = %ev.camera,
            "deterrence action: camera not found — skipping"
        );
        return;
    };
    let Some(camtarget) = crate::ptz::parse_source(&cam.source) else {
        tracing::warn!(
            rule = rule_name, camera = %ev.camera,
            "deterrence action: camera source has no ONVIF credentials — skipping"
        );
        return;
    };
    // Fixed, capped hold for v0 — no escalation ladder.
    crate::deterrence::pulse_relay_async(camtarget, token.to_string(), Duration::from_secs(5));
}

/// Email (SMTP) action: send the alarm detail with the snapshot attached.
/// Best-effort and log-and-swallow like every other channel. The recipient is
/// the action's `target` if set, else the configured default `smtp.to`.
fn build_email(
    target: &str,
    rule_name: &str,
    ev: &AlarmEvent,
    suppressed: u32,
) -> Result<Outbound, String> {
    use lettre::message::{header::ContentType, Attachment, MultiPart, SinglePart};
    use lettre::Message;

    let Some(cfg) = &ev.smtp else {
        return Err("SMTP is not configured in Settings".into());
    };
    let to_raw = if target.trim().is_empty() {
        cfg.to
    } else {
        target
    };
    if cfg.from.trim().is_empty() || to_raw.trim().is_empty() {
        return Err("no from/to address configured".into());
    }
    let from = cfg
        .from
        .trim()
        .parse()
        .map_err(|e| format!("bad from address {:?}: {e}", cfg.from))?;
    let subject = if ev.duress {
        format!("🚨 DURESS — {rule_name}")
    } else {
        format!("Alarm: {rule_name}")
    };
    let mut body = format!("{} ({:.0}%) on {}", ev.label, ev.score * 100.0, ev.camera);
    if let Some(c) = ev.caption {
        body = format!("{c}\n\n{body}");
    }
    if suppressed > 0 {
        body.push_str(&format!("\n(+{suppressed} more while muted by cooldown)"));
    }
    if let Some(f) = ev.face {
        body.push_str(&format!("\nFace: {f}"));
    }
    if let Some(p) = ev.plate {
        body.push_str(&format!("\nPlate: {p}"));
    }
    if let Some(t) = ev.transcript {
        body.push_str(&format!("\nHeard: \"{t}\""));
    }
    if !ev.base_url.is_empty() {
        let base = ev.base_url.trim_end_matches('/');
        body.push_str(&format!("\n\nClip: {base}/api/events/{}/clip", ev.event_id));
    }

    let mut builder = Message::builder().from(from).subject(subject);
    let mut any_to = false;
    for addr in to_raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match addr.parse() {
            Ok(a) => {
                builder = builder.to(a);
                any_to = true;
            }
            Err(e) => tracing::warn!("email: bad recipient {addr:?}: {e}"),
        }
    }
    if !any_to {
        return Err("no valid recipient address".into());
    }

    let text = SinglePart::plain(body);
    let msg = match ev.snapshot_path.and_then(|p| std::fs::read(p).ok()) {
        Some(bytes) => {
            let att = Attachment::new("snapshot.jpg".to_string())
                .body(bytes, ContentType::parse("image/jpeg").unwrap());
            builder.multipart(MultiPart::mixed().singlepart(text).singlepart(att))
        }
        None => builder.singlepart(text),
    };
    let msg = msg.map_err(|e| format!("building the message: {e}"))?;
    // Configuration errors (no SMTP, bad address) surfaced above, synchronously,
    // so a clicked Test still reports them immediately rather than queueing a
    // job that is already doomed.
    Ok(Outbound::Email {
        cfg: OwnedSmtp::of(cfg),
        msg: Box::new(msg),
    })
}

/// Build the SMTP transport and send a fully-built message. Shared by the
/// per-rule `email` action and the per-user [`email_simple`] path (P2.11) so both
/// go through the same bounded, log-and-swallow transport.
fn send_built(cfg: &SmtpConfig, msg: lettre::Message) -> Result<(), String> {
    use lettre::Transport;
    let mailer = build_smtp(cfg).map_err(|e| format!("transport: {e}"))?;
    mailer.send(&msg).map(|_| ()).map_err(|e| e.to_string())
}

/// P2.11: send a plain-text email (no attachment) with an explicit subject/body
/// to one recipient — the per-user notification email delivered by the push
/// worker. Factored to share [`send_built`] with the per-rule `email` action.
/// Best-effort / log-and-swallow, like every other channel.
pub(crate) fn email_simple(cfg: &SmtpConfig, to: &str, subject: &str, body: &str) {
    use lettre::message::SinglePart;
    use lettre::Message;

    if cfg.from.trim().is_empty() || to.trim().is_empty() {
        return;
    }
    let from = match cfg.from.trim().parse() {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("email skipped: bad from address {:?}: {e}", cfg.from);
            return;
        }
    };
    let mut builder = Message::builder().from(from).subject(subject.to_string());
    let mut any_to = false;
    for addr in to.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match addr.parse() {
            Ok(a) => {
                builder = builder.to(a);
                any_to = true;
            }
            Err(e) => tracing::warn!("email: bad recipient {addr:?}: {e}"),
        }
    }
    if !any_to {
        return;
    }
    let sent = match builder.singlepart(SinglePart::plain(body.to_string())) {
        Ok(msg) => send_built(cfg, msg),
        Err(e) => Err(format!("building the message: {e}")),
    };
    if let Err(e) = sent {
        tracing::warn!(to = %to, "notification email FAILED to send: {e}");
    }
}

/// Build a blocking SMTP transport from the config. URL forms: "smtps://host:465"
/// (implicit TLS), "smtp://host:587" (STARTTLS), or bare "host[:port]" (implicit
/// TLS). Any user:pass@ in the URL is ignored in favor of the explicit creds.
fn build_smtp(cfg: &SmtpConfig) -> Result<lettre::SmtpTransport, lettre::transport::smtp::Error> {
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::SmtpTransport;

    let raw = cfg.url.trim();
    let (starttls, rest) = if let Some(r) = raw.strip_prefix("smtps://") {
        (false, r)
    } else if let Some(r) = raw.strip_prefix("smtp://") {
        (true, r)
    } else {
        (false, raw)
    };
    let hostport = rest.rsplit('@').next().unwrap_or(rest);
    let (host, port) = match hostport.split_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()),
        None => (hostport, None),
    };
    let mut builder = if starttls {
        SmtpTransport::starttls_relay(host)?
    } else {
        SmtpTransport::relay(host)?
    };
    // Bound the send: this runs inline on the detection/audio worker threads, so
    // a hung SMTP server must not stall detection (lettre defaults to 60s).
    builder = builder.timeout(Some(Duration::from_secs(10)));
    if let Some(p) = port {
        builder = builder.port(p);
    }
    if !cfg.user.is_empty() {
        builder = builder.credentials(Credentials::new(cfg.user.to_string(), cfg.pass.to_string()));
    }
    Ok(builder.build())
}

/// Plain-text ntfy push (no attachment) — used for camera health alerts.
pub fn ntfy_text(url: &str, title: &str, message: &str, tags: &str) {
    if let Err(e) = ureq::post(url)
        .timeout(Duration::from_secs(10))
        .set("X-Title", title)
        .set("X-Tags", tags)
        .send_string(message)
    {
        // An undelivered alert is precisely what the owner needs to hear about,
        // so this is warn-level: debug is off in normal operation, which made
        // every failed health/alarm push invisible.
        tracing::warn!("ntfy push failed: {e}");
    }
}

fn build_webhook(url: &str, ev: &AlarmEvent) -> Outbound {
    let (body, content_type) = if ev.webhook_template.is_empty() {
        let payload = serde_json::json!({
            "type": "alarm",
            "event_id": ev.event_id,
            "camera": ev.camera,
            "label": ev.label,
            "score": ev.score,
            "ts": ev.ts,
            "snapshot": ev.snapshot_url,
            "face": ev.face,
            "plate": ev.plate,
            "gesture": ev.gesture,
            "transcript": ev.transcript,
            "caption": ev.caption,
            "severity": ev.severity,
        });
        (payload.to_string(), "application/json")
    } else {
        (render_template(ev.webhook_template, ev), "application/json")
    };
    Outbound::Webhook {
        url: url.to_string(),
        body,
        content_type,
    }
}

/// ntfy push: PUT with the snapshot attached when available, plain POST
/// otherwise. Title/extras travel as headers per the ntfy protocol. When a
/// public base URL is known the push carries tap-through "View clip" /
/// "Snapshot" actions, and `priority` (1..5) maps to ntfy's X-Priority.
fn build_ntfy(
    url: &str,
    rule_name: &str,
    priority: u8,
    ev: &AlarmEvent,
    suppressed: u32,
) -> Outbound {
    let mut detail = format!("{} ({:.0}%) on {}", ev.label, ev.score * 100.0, ev.camera);
    // A GenAI description leads the push (Wyze/Nest "descriptive alert" style);
    // the structured detail follows so nothing is lost if the caption is vague.
    if let Some(c) = ev.caption {
        detail = format!("{c} — {detail}");
    }
    if let Some(f) = ev.face {
        detail.push_str(&format!(" — {f}"));
    }
    if let Some(p) = ev.plate {
        detail.push_str(&format!(" — plate {p}"));
    }
    if let Some(g) = ev.gesture {
        detail.push_str(&format!(" — ✋ {g}"));
    }
    if let Some(t) = ev.transcript {
        detail.push_str(&format!(" — 🎙️ \"{t}\""));
    }
    if suppressed > 0 {
        detail.push_str(&format!(" (+{suppressed} more while muted by cooldown)"));
    }

    // Tap-through actions when we can build absolute links.
    let actions = if ev.base_url.is_empty() {
        None
    } else {
        let base = ev.base_url.trim_end_matches('/');
        Some(format!(
            "view, View clip, {base}/api/events/{}/clip; view, Snapshot, {base}{}",
            ev.event_id, ev.snapshot_url
        ))
    };

    // Duress overrides: max urgency, a distinct siren tag, and a flagged title.
    let title = if ev.duress {
        format!("🚨 DURESS — {rule_name}")
    } else {
        rule_name.to_string()
    };
    let (tags, eff_priority) = if ev.duress {
        ("warning,rotating_light,sos", 5)
    } else {
        // An explicit per-action priority wins; otherwise the event's severity
        // picks a sensible default (critical rings, low stays quiet, normal
        // leaves ntfy's default 3 by sending no header).
        let p = if (1..=5).contains(&priority) {
            priority
        } else {
            match ev.severity {
                4 => 5,
                3 => 4,
                1 => 2,
                _ => 0,
            }
        };
        ("rotating_light", p)
    };

    Outbound::Ntfy {
        url: url.to_string(),
        title,
        tags,
        priority: eff_priority,
        actions,
        message: detail,
        // Read here rather than in the worker: it is a local file of a couple
        // hundred KB, and reading it now means retention cannot delete it out
        // from under a queued push.
        snapshot: ev.snapshot_path.and_then(|p| std::fs::read(p).ok()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of the dispatch queue: a firing rule must not make the caller
    /// wait on a network round trip. `fire` runs inline on the detection thread,
    /// and an ntfy push is a PUT on a 10 s timeout — so before this, one
    /// unreachable push server stalled detection for EVERY camera.
    ///
    /// Uses a black-hole address so the delivery would certainly block if it
    /// were attempted inline, then asserts the call returned promptly and the
    /// action was reported as queued rather than delivered.
    #[test]
    fn a_firing_rule_does_not_block_the_caller_on_the_network() {
        let dir = std::env::temp_dir().join(format!("cammy-dispatch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir.join("t.db")).expect("test db");
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker = start_dispatch(db.clone(), stop.clone());

        let mut r = rule(1, 0, 0);
        r.action = "ntfy".into();
        // A closed local port: refused instantly, so the worker's attempt does
        // not add ten seconds of timeout to the suite. The load-bearing
        // assertion below is the ROUTING one ("queued for delivery"), which
        // proves the delivery left this thread; that is deterministic, whereas a
        // timing assertion against a black-hole address would be both slow and
        // flaky on a loaded machine.
        r.target = "http://127.0.0.1:9/closed".into();
        let (tx, _rx) = std::sync::mpsc::channel::<EventMsg>();
        let ev = AlarmEvent {
            event_id: 42, // non-zero: a real detection, so it must be queued
            camera: "cam",
            label: "person",
            score: 0.9,
            ts: 1,
            snapshot_url: "/s.jpg",
            snapshot_path: None,
            face: None,
            plate: None,
            gesture: None,
            transcript: None,
            speed: None,
            base_url: "",
            webhook_template: "",
            smtp: None,
            duress: false,
            severity: 2,
            min_push_severity: 0,
            caption: None,
        };

        let t = std::time::Instant::now();
        let outcomes = fire(&r, &ev, &tx, 0, &db);
        let elapsed = t.elapsed();

        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].kind, "ntfy");
        // THE assertion: the delivery was handed to the worker, not performed
        // here. Anything else means alarm I/O is back on the detection thread.
        assert_eq!(
            outcomes[0].detail.as_deref(),
            Some("queued for delivery"),
            "a live detection's alarm must be queued, never delivered inline"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "fire() took {elapsed:?} — it should only be building a request"
        );

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = worker.join();
    }

    /// A dropped alert used to leave a `warn!` and nothing else — invisible to
    /// the only person who needs it. The notification must fire ONCE per outage
    /// (not per dropped alert, which would be a second flood), and again for a
    /// NEW outage after the queue has recovered.
    #[test]
    fn dropped_alerts_notify_once_per_outage() {
        // Nothing dropped → silence.
        assert_eq!(drop_transition(0, 0, 0, false), None);
        assert_eq!(drop_transition(0, 0, 300, false), None);
        // First drop → notify + latch.
        assert_eq!(drop_transition(1, 0, DISPATCH_CAP, false), Some(true));
        // More drops while latched → no spam (caller has set acknowledged = 1).
        assert_eq!(drop_transition(50, 1, DISPATCH_CAP, true), None);
        // A still-backlogged queue has NOT recovered, even if drops paused.
        assert_eq!(drop_transition(50, 50, 17, true), None);
        // Drained → recovery notice, latch off.
        assert_eq!(drop_transition(50, 50, 0, true), Some(false));
        // A LATER outage speaks again rather than staying quiet forever.
        assert_eq!(drop_transition(51, 50, DISPATCH_CAP, false), Some(true));
        // …but the same total does not.
        assert_eq!(drop_transition(50, 50, DISPATCH_CAP, false), None);
    }

    fn rule(id: i64, cooldown: i64, snooze: i64) -> AlarmRule {
        AlarmRule {
            id,
            name: "r".into(),
            enabled: true,
            camera_id: None,
            label: None,
            face_like: None,
            plate_like: None,
            gesture_like: None,
            transcript_like: None,
            face_unknown: false,
            zone_like: None,
            confirm_label: None,
            confirm_within_secs: None,
            vlm_prompt: None,
            describe: false,
            prompt_like: None,
            attr_like: None,
            min_score: 0.0,
            action: "ntfy".into(),
            target: "t".into(),
            days: vec![],
            start_hhmm: None,
            end_hhmm: None,
            cooldown_secs: cooldown,
            priority: 0,
            snooze_until: snooze,
            created_ts: 0,
            modes: vec![],
            actions: vec![],
        }
    }

    #[test]
    fn prompt_rules_fire_only_via_the_prompt_path() {
        let mut r = rule(1, 0, 0);
        r.prompt_like = Some("a red pickup truck".into());
        // Plain `matches` (every normal dispatch site) must reject a prompt
        // rule outright — it was never compared against the prompt.
        assert!(!r.matches(1, "car", 0.9, None, None, None, None));
        // The embedding pass, which verified the similarity itself, matches on
        // the remaining conditions.
        assert!(r.matches_prompt(1, "car", 0.9, None, None));
        // …and those other conditions still gate: wrong camera scope → no fire.
        r.camera_id = Some(2);
        assert!(!r.matches_prompt(1, "car", 0.9, None, None));
        // A whitespace-only prompt is no condition at all (normal rule).
        let mut w = rule(2, 0, 0);
        w.prompt_like = Some("   ".into());
        assert!(w.matches(1, "car", 0.9, None, None, None, None));
        assert!(!w.matches_prompt(1, "car", 0.9, None, None));
    }

    #[test]
    fn arm_modes_gate_dispatch() {
        // Back-compat guard: a legacy empty-modes rule MUST still fire in the
        // default arm mode after an upgrade — i.e. the default is an *armed*
        // mode. If someone changes the default to "disarmed", this fails loudly
        // instead of silently muting every existing rule.
        assert!(armed_in_mode(&[], &crate::db::Settings::default().arm_mode));
        // Empty modes: armed in home + away, suppressed when disarmed.
        assert!(armed_in_mode(&[], "home"));
        assert!(armed_in_mode(&[], "away"));
        assert!(!armed_in_mode(&[], "disarmed"));
        // Opted into "away" only.
        let away = vec!["away".to_string()];
        assert!(armed_in_mode(&away, "away"));
        assert!(!armed_in_mode(&away, "home"));
        assert!(!armed_in_mode(&away, "disarmed"));
        // A panic rule opts into "disarmed": fires even while disarmed.
        let panic = vec![
            "disarmed".to_string(),
            "home".to_string(),
            "away".to_string(),
        ];
        assert!(armed_in_mode(&panic, "disarmed"));
        assert!(armed_in_mode(&panic, "home"));
    }

    #[test]
    fn cooldown_suppresses_within_window() {
        let throttle: AlarmThrottle = Default::default();
        let r = rule(1, 60, 0);
        assert!(ready(&r, &throttle, 1000)); // first fire
        assert!(!ready(&r, &throttle, 1030)); // 30s < 60s cooldown
        assert!(ready(&r, &throttle, 1061)); // 61s ≥ cooldown
    }

    #[test]
    fn no_cooldown_always_ready() {
        let throttle: AlarmThrottle = Default::default();
        let r = rule(2, 0, 0);
        assert!(ready(&r, &throttle, 100));
        assert!(ready(&r, &throttle, 100));
    }

    #[test]
    fn snooze_blocks_until_expiry() {
        let throttle: AlarmThrottle = Default::default();
        let r = rule(3, 0, 5000);
        assert!(!ready(&r, &throttle, 4999)); // still snoozed
        assert!(ready(&r, &throttle, 5001)); // snooze elapsed
    }

    #[test]
    fn template_renders_and_escapes() {
        let ev = AlarmEvent {
            event_id: 7,
            camera: "front-door",
            label: "person",
            score: 0.9123,
            ts: 1000,
            snapshot_url: "/api/snapshots/x.jpg",
            snapshot_path: None,
            face: Some("Bob \"the\" Builder"),
            plate: None,
            gesture: None,
            // Embeds a control char (vertical tab) to prove it's \u-escaped.
            transcript: Some("help\u{000b}me"),
            speed: None,
            base_url: "",
            webhook_template: "",
            smtp: None,
            duress: false,
            severity: 2,
            min_push_severity: 1,
            caption: Some(r#"A man in a "red" hat"#),
        };
        let out = render_template(
            r#"{"cam":"{{camera}}","obj":"{{label}}","who":"{{face}}","p":{{score}},"said":"{{transcript}}","desc":"{{caption}}","sev":{{severity}},"miss":"{{nope}}"}"#,
            &ev,
        );
        // Valid JSON after substitution (quotes + control chars are escaped).
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["cam"], "front-door");
        assert_eq!(v["obj"], "person");
        assert_eq!(v["who"], "Bob \"the\" Builder");
        assert_eq!(v["p"], 0.912);
        assert_eq!(v["said"], "help\u{000b}me");
        assert_eq!(v["desc"], "A man in a \"red\" hat");
        assert_eq!(v["sev"], 2);
        // Unknown placeholder is left as-is.
        assert_eq!(v["miss"], "{{nope}}");
    }

    #[test]
    fn cooldown_counts_suppressed_matches_for_the_burst_summary() {
        let throttle: AlarmThrottle = Default::default();
        let r = rule(9, 60, 0);
        assert!(ready(&r, &throttle, 1000)); // fires
        assert_eq!(take_suppressed(&throttle, 9), 0); // nothing swallowed yet
        assert!(!ready(&r, &throttle, 1010)); // swallowed x3
        assert!(!ready(&r, &throttle, 1020));
        assert!(!ready(&r, &throttle, 1030));
        assert!(ready(&r, &throttle, 1061)); // fires again
        assert_eq!(take_suppressed(&throttle, 9), 3); // burst reported once…
        assert_eq!(take_suppressed(&throttle, 9), 0); // …then drained
                                                      // Snoozed matches count toward the summary too.
        let s = rule(10, 0, 2000);
        assert!(!ready(&s, &throttle, 1500));
        assert!(ready(&s, &throttle, 2001));
        assert_eq!(take_suppressed(&throttle, 10), 1);
    }

    #[test]
    fn severity_gate_quiets_human_channels_only() {
        // Gate off (min 1) → everything delivers.
        assert!(push_allowed(1, 1, false));
        // Below the bar → quiet; at/above → delivers.
        assert!(!push_allowed(2, 3, false));
        assert!(push_allowed(3, 3, false));
        assert!(push_allowed(4, 3, false));
        // Duress always delivers, whatever the knob says.
        assert!(push_allowed(1, 4, true));
    }
}
