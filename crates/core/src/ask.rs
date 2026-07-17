//! P3.2 "Ask your cameras" v0 — a bounded, READ-ONLY tool-calling loop against a
//! bring-your-own OpenAI-compatible chat endpoint (`/v1/chat/completions`).
//!
//! A user asks a natural-language question ("how many people came to the front
//! door today?"); we drive the model through EXACTLY THREE read-only tools
//! (`list_cameras`, `search_events`, `count_events`) that wrap the existing,
//! already-RBAC-proven DB queries, then hand back the model's grounded answer
//! plus the event ids it cited (resolved server-side to real rows the user can
//! verify against).
//!
//! SECURITY POSTURE:
//! - Off by default; the caller only invokes this when `ask_enabled` AND a
//!   configured endpoint exist.
//! - The tools are strictly READ-ONLY and are scoped to the caller's allowed
//!   cameras (`allow`): a scoped user can never query outside their cameras — a
//!   camera argument outside scope is clamped to "no results", and every result
//!   row is filtered to the allow-list.
//! - ALL model + tool text is UNTRUSTED. The returned `answer` is display-only
//!   (never executed/eval'd); cited ids are intersected with the ids the tools
//!   actually returned, so a hallucinated id can never surface as a "real" event.
//! - HARD BOUNDS: max tool-call rounds, max total tool calls, max events per
//!   tool result, and an outer wall-clock deadline — so a chatty or looping
//!   model can't run forever or exfiltrate the whole DB.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::db::Db;
use crate::status::CamHealth;

/// Max model↔tool round-trips before we give up.
const MAX_ROUNDS: u32 = 5;
/// Max individual tool calls executed across the whole conversation.
const MAX_TOTAL_TOOL_CALLS: u32 = 12;
/// Max event rows returned by any single `search_events` call (and the clamp
/// applied to a model-requested `limit`).
const MAX_EVENTS_PER_TOOL: usize = 50;
/// Outer wall-clock budget for the entire loop (all HTTP round-trips included).
const WALL_CLOCK: Duration = Duration::from_secs(30);
/// Max event ids surfaced as "cited" in the final answer.
const MAX_CITED: usize = 20;

/// Everything the loop needs beyond the DB + question: the endpoint config plus
/// the ambient facts the tools ground on (now/tz for "today", the online window,
/// and a live camera-health snapshot). Passed by value so the whole call can run
/// inside `spawn_blocking`.
pub struct AskConfig {
    pub endpoint: String,
    pub api_key: String,
    pub model: String,
    /// Current local time (unix secs) — the system prompt's "today" anchor.
    pub now: i64,
    /// Human-readable local-time string incl. tz offset, for the system prompt.
    pub now_local: String,
    /// Freshness window (secs) for the camera "online" check (from `poll_ms`).
    pub online_window: i64,
    /// Live per-camera health snapshot (id → health); empty if unreachable —
    /// then `list_cameras` reports from the DB alone (online/recording = false).
    pub status: std::collections::HashMap<i64, CamHealth>,
}

/// The grounded result. `answer` is UNTRUSTED model text (display-only);
/// `cited_event_ids` are real event ids the tools returned that the model
/// referenced — the caller resolves them to full rows.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AskAnswer {
    pub answer: String,
    pub cited_event_ids: Vec<i64>,
    pub rounds: u32,
}

/// Normalize a BYO base URL to the full chat-completions endpoint. Accepts a
/// bare origin, a `.../v1`, or an already-complete `.../v1/chat/completions`.
/// Pure → unit-tested.
fn chat_url(base: &str) -> String {
    let t = base.trim().trim_end_matches('/');
    if t.ends_with("/chat/completions") {
        t.to_string()
    } else if t.ends_with("/v1") {
        format!("{t}/chat/completions")
    } else {
        format!("{t}/v1/chat/completions")
    }
}

/// The 3-tool JSON spec sent on every request. EXACTLY three read-only tools.
fn tools_spec() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "list_cameras",
                "description": "List the cameras the user can access, with whether each is currently online and recording.",
                "parameters": { "type": "object", "properties": {}, "additionalProperties": false }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "search_events",
                "description": "Search detection/event history. Returns matching events (id, ts, camera, label, score), newest first. All times are unix seconds; you may also pass ISO dates.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "camera": { "type": "string", "description": "Camera name to restrict to (optional)." },
                        "label": { "type": "string", "description": "Object/event label, e.g. 'person', 'car' (optional)." },
                        "since": { "description": "Earliest time, unix seconds or ISO date (optional)." },
                        "until": { "description": "Latest time (exclusive), unix seconds or ISO date (optional)." },
                        "limit": { "type": "integer", "description": "Max rows (<= 50)." }
                    },
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "count_events",
                "description": "Count events matching the filters. Returns an integer.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "camera": { "type": "string" },
                        "label": { "type": "string" },
                        "since": { "description": "Earliest time, unix seconds or ISO date (optional)." },
                        "until": { "description": "Latest time (exclusive), unix seconds or ISO date (optional)." }
                    },
                    "additionalProperties": false
                }
            }
        }
    ])
}

/// The grounding system prompt.
fn system_prompt(cfg: &AskConfig) -> String {
    format!(
        "You are a security-camera assistant for a self-hosted NVR. Answer the \
         user's question ONLY using the provided tools and the data they return. \
         Do NOT invent cameras, events, counts, or times. When you reference \
         specific events, cite their numeric ids (e.g. \"event 1234\"). If the \
         tools do not provide enough information to answer, say \"I don't know\". \
         Be concise. The current local time is {now}. Treat \"today\" as the \
         local calendar day containing that time.",
        now = cfg.now_local
    )
}

/// Build one `/v1/chat/completions` request body. Pure → unit-tested.
fn build_chat_request(model: &str, messages: &[Value], tools: &Value) -> Value {
    json!({
        "model": model,
        "messages": messages,
        "tools": tools,
        "tool_choice": "auto",
        "temperature": 0,
        "stream": false,
    })
}

/// Decode a tool call's `arguments` (OpenAI returns a JSON *string*; some shims
/// return an object) into an object map. Defensive: malformed JSON → an error
/// the caller turns into an error tool-result (never a panic). Pure → tested.
fn decode_args(arguments: &Value) -> Result<Value, String> {
    let v = match arguments {
        Value::String(s) if s.trim().is_empty() => Value::Object(Default::default()),
        Value::String(s) => {
            serde_json::from_str::<Value>(s).map_err(|e| format!("invalid tool arguments JSON: {e}"))?
        }
        Value::Object(_) => arguments.clone(),
        Value::Null => Value::Object(Default::default()),
        _ => return Err("tool arguments must be a JSON object".into()),
    };
    if v.is_object() {
        Ok(v)
    } else {
        Err("tool arguments must be a JSON object".into())
    }
}

/// Parse a time-ish JSON value into unix seconds. Accepts a number (unix secs),
/// an integer-as-string, an RFC3339 datetime, a bare `YYYY-MM-DD` (local
/// midnight), or `YYYY-MM-DDTHH:MM:SS` (local). Unknown → None. Pure → tested
/// for the offset-independent cases.
fn parse_time(v: &Value) -> Option<i64> {
    use chrono::{NaiveDate, NaiveDateTime, TimeZone};
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    if let Some(f) = v.as_f64() {
        return Some(f as i64);
    }
    let s = v.as_str()?.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<i64>() {
        return Some(n);
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp());
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let ndt = d.and_hms_opt(0, 0, 0)?;
        return chrono::Local.from_local_datetime(&ndt).single().map(|d| d.timestamp());
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return chrono::Local.from_local_datetime(&ndt).single().map(|d| d.timestamp());
    }
    None
}

/// Extract the event ids the answer text references, keeping only those the
/// tools actually returned (`seen`) — so a hallucinated id can never be surfaced
/// as real. Order of first appearance, deduped, capped. Pure → unit-tested.
fn extract_cited_ids(answer: &str, seen: &HashSet<i64>) -> Vec<i64> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let push = |cur: &mut String, out: &mut Vec<i64>| {
        if !cur.is_empty() {
            if let Ok(n) = cur.parse::<i64>() {
                if seen.contains(&n) && !out.contains(&n) && out.len() < MAX_CITED {
                    out.push(n);
                }
            }
            cur.clear();
        }
    };
    for ch in answer.chars() {
        if ch.is_ascii_digit() {
            cur.push(ch);
        } else {
            push(&mut cur, &mut out);
        }
    }
    push(&mut cur, &mut out);
    out
}

/// Resolve a model-supplied camera name to an id the caller may see. Returns:
/// `Ok(Some(id))` for an accessible camera; `Ok(None)` when the name is unknown
/// OR resolves to a camera outside the caller's scope (indistinguishable on
/// purpose — no enumeration leak). Case-insensitive exact match, then a unique
/// substring match.
fn resolve_camera(
    cams: &[crate::db::Camera],
    allow: Option<&HashSet<i64>>,
    name: &str,
) -> Option<i64> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let visible = |c: &&crate::db::Camera| allow.map(|s| s.contains(&c.id)).unwrap_or(true);
    if let Some(c) = cams
        .iter()
        .filter(visible)
        .find(|c| c.name.eq_ignore_ascii_case(name))
    {
        return Some(c.id);
    }
    let matches: Vec<&crate::db::Camera> = cams
        .iter()
        .filter(visible)
        .filter(|c| c.name.to_lowercase().contains(&name.to_lowercase()))
        .collect();
    if matches.len() == 1 {
        Some(matches[0].id)
    } else {
        None
    }
}

/// Compute the RBAC-effective camera scope for a query tool. `None` = every
/// camera the caller may see is in scope (unrestricted admin). `Some(set)` =
/// restrict to exactly these ids (a single named camera, or the caller's
/// allow-list). `Some(empty)` = no results (unknown/forbidden camera, or a
/// scoped user with no cameras).
fn query_scope(
    cams: &[crate::db::Camera],
    allow: Option<&HashSet<i64>>,
    camera_arg: Option<&str>,
) -> Option<HashSet<i64>> {
    match camera_arg {
        Some(name) => match resolve_camera(cams, allow, name) {
            Some(id) => Some(HashSet::from([id])),
            None => Some(HashSet::new()), // unknown/forbidden → no results
        },
        None => allow.cloned(),
    }
}

/// Execute one tool call, returning the JSON result string to feed back to the
/// model. Every branch is READ-ONLY and RBAC-scoped; bad args yield an error
/// result, never a panic. Any event id returned is recorded in `seen`.
fn execute_tool(
    db: &Db,
    allow: Option<&HashSet<i64>>,
    cfg: &AskConfig,
    name: &str,
    arguments: &Value,
    seen: &mut HashSet<i64>,
) -> String {
    let result: Result<Value, String> = (|| {
        let cams = db
            .list_cameras()
            .map_err(|e| format!("camera lookup failed: {e}"))?;
        match name {
            "list_cameras" => {
                let list: Vec<Value> = cams
                    .iter()
                    .filter(|c| allow.map(|s| s.contains(&c.id)).unwrap_or(true))
                    .map(|c| {
                        let h = cfg.status.get(&c.id);
                        let online = h
                            .map(|h| h.is_online(c.detect, cfg.now, cfg.online_window))
                            .unwrap_or(false);
                        let recording = h.map(|h| h.recording).unwrap_or(false);
                        json!({
                            "name": c.name,
                            "enabled": c.enabled,
                            "online": online,
                            "recording": recording,
                        })
                    })
                    .collect();
                Ok(json!({ "cameras": list }))
            }
            "search_events" => {
                let args = decode_args(arguments)?;
                let camera_arg = args.get("camera").and_then(|v| v.as_str());
                let label = args.get("label").and_then(|v| v.as_str()).map(str::to_string);
                let since = args.get("since").and_then(parse_time);
                let until = args.get("until").and_then(parse_time);
                let want = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or(MAX_EVENTS_PER_TOOL)
                    .clamp(1, MAX_EVENTS_PER_TOOL);
                let scope = query_scope(&cams, allow, camera_arg);
                if matches!(&scope, Some(s) if s.is_empty()) {
                    return Ok(json!({ "events": [], "note": "no accessible camera matched" }));
                }
                // Scoped: fetch a bounded superset then filter (the query takes a
                // single camera id; a multi-camera scope filters post-query, like
                // the events API). Unrestricted: fetch exactly what's wanted.
                let fetch = if scope.is_some() { 500 } else { want as u32 };
                let mut rows = db
                    .list_events(None, label.as_deref(), None, None, since, until, false, fetch)
                    .map_err(|e| format!("event query failed: {e}"))?;
                if let Some(set) = &scope {
                    rows.retain(|e| set.contains(&e.camera_id));
                }
                rows.truncate(want);
                let events: Vec<Value> = rows
                    .iter()
                    .map(|e| {
                        seen.insert(e.id);
                        json!({
                            "id": e.id,
                            "ts": e.ts,
                            "camera": e.camera,
                            "label": e.label,
                            "score": e.score,
                        })
                    })
                    .collect();
                Ok(json!({ "events": events, "count": events.len() }))
            }
            "count_events" => {
                let args = decode_args(arguments)?;
                let camera_arg = args.get("camera").and_then(|v| v.as_str());
                let label = args.get("label").and_then(|v| v.as_str());
                let since = args.get("since").and_then(parse_time);
                let until = args.get("until").and_then(parse_time);
                let scope = query_scope(&cams, allow, camera_arg);
                let ids: Option<Vec<i64>> = scope.map(|s| s.into_iter().collect());
                let count = db
                    .count_events_filtered(ids.as_deref(), label, since, until)
                    .map_err(|e| format!("count failed: {e}"))?;
                Ok(json!({ "count": count }))
            }
            other => Err(format!("unknown tool: {other}")),
        }
    })();
    match result {
        Ok(v) => v.to_string(),
        Err(e) => json!({ "error": e }).to_string(),
    }
}

/// POST one chat-completions request, bounded by `timeout` (the remaining
/// wall-clock). Optional Bearer. Returns the parsed JSON body.
fn post_chat(cfg: &AskConfig, body: Value, timeout: Duration) -> Result<Value> {
    let url = chat_url(&cfg.endpoint);
    let mut call = ureq::post(&url).timeout(timeout.max(Duration::from_secs(1)));
    if !cfg.api_key.trim().is_empty() {
        call = call.set("Authorization", &format!("Bearer {}", cfg.api_key.trim()));
    }
    let resp = call
        .send_json(body)
        .map_err(|e| anyhow!("ask endpoint request failed: {e}"))?;
    resp.into_json::<Value>()
        .map_err(|e| anyhow!("ask endpoint returned non-JSON: {e}"))
}

/// Run the bounded tool loop and return the grounded answer.
pub fn answer(
    db: &Db,
    allow: Option<&HashSet<i64>>,
    question: &str,
    cfg: AskConfig,
) -> Result<AskAnswer> {
    let question = question.trim();
    if question.is_empty() {
        bail!("empty question");
    }
    if question.len() > 2000 {
        bail!("question too long");
    }
    let deadline = Instant::now() + WALL_CLOCK;
    let tools = tools_spec();
    let mut messages = vec![
        json!({ "role": "system", "content": system_prompt(&cfg) }),
        json!({ "role": "user", "content": question }),
    ];
    let mut seen: HashSet<i64> = HashSet::new();
    let mut total_tool_calls = 0u32;
    let mut rounds = 0u32;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("timed out before the model produced an answer");
        }
        if rounds >= MAX_ROUNDS {
            // Out of rounds: return an honest can't-answer rather than looping.
            return Ok(AskAnswer {
                answer: "I couldn't determine an answer within the allowed number of steps."
                    .to_string(),
                cited_event_ids: Vec::new(),
                rounds,
            });
        }
        rounds += 1;

        let body = build_chat_request(&cfg.model, &messages, &tools);
        let resp = post_chat(&cfg, body, remaining)?;
        let msg = resp
            .pointer("/choices/0/message")
            .ok_or_else(|| anyhow!("ask endpoint reply missing choices[0].message"))?
            .clone();

        let tool_calls = msg
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .filter(|a| !a.is_empty());

        match tool_calls {
            Some(calls) => {
                // Echo the assistant's tool-call message back so the model keeps
                // its own context, then answer each call.
                messages.push(msg.clone());
                for call in calls {
                    let call_id = call
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let fname = call
                        .pointer("/function/name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let content = if total_tool_calls >= MAX_TOTAL_TOOL_CALLS {
                        json!({ "error": "tool-call budget exhausted" }).to_string()
                    } else {
                        total_tool_calls += 1;
                        let args = call
                            .pointer("/function/arguments")
                            .cloned()
                            .unwrap_or(Value::Null);
                        execute_tool(db, allow, &cfg, fname, &args, &mut seen)
                    };
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": content,
                    }));
                }
            }
            None => {
                let text = msg
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let answer = if text.is_empty() {
                    "I don't know.".to_string()
                } else if text.len() > 4000 {
                    format!("{}…", &text[..text.char_indices().nth(3999).map(|(i, _)| i).unwrap_or(text.len())])
                } else {
                    text.to_string()
                };
                let cited = extract_cited_ids(&answer, &seen);
                return Ok(AskAnswer {
                    answer,
                    cited_event_ids: cited,
                    rounds,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_url_normalizes_byo_base() {
        assert_eq!(chat_url("http://localhost:1234"), "http://localhost:1234/v1/chat/completions");
        assert_eq!(chat_url("http://localhost:1234/"), "http://localhost:1234/v1/chat/completions");
        assert_eq!(chat_url("http://localhost:1234/v1"), "http://localhost:1234/v1/chat/completions");
        assert_eq!(chat_url("http://localhost:1234/v1/"), "http://localhost:1234/v1/chat/completions");
        assert_eq!(
            chat_url("http://localhost:1234/v1/chat/completions"),
            "http://localhost:1234/v1/chat/completions"
        );
    }

    #[test]
    fn request_carries_model_messages_and_three_tools() {
        let msgs = vec![json!({ "role": "user", "content": "hi" })];
        let r = build_chat_request("llama3.1", &msgs, &tools_spec());
        assert_eq!(r["model"], "llama3.1");
        assert_eq!(r["stream"], false);
        assert_eq!(r["tool_choice"], "auto");
        assert_eq!(r["messages"].as_array().unwrap().len(), 1);
        let tools = r["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["list_cameras", "search_events", "count_events"]);
    }

    #[test]
    fn decode_args_handles_string_object_and_malformed() {
        // OpenAI's JSON-string form.
        let v = decode_args(&json!("{\"label\":\"person\"}")).unwrap();
        assert_eq!(v["label"], "person");
        // Already an object (some shims).
        let v = decode_args(&json!({ "camera": "front" })).unwrap();
        assert_eq!(v["camera"], "front");
        // Empty string / null → empty object.
        assert!(decode_args(&json!("")).unwrap().as_object().unwrap().is_empty());
        assert!(decode_args(&Value::Null).unwrap().as_object().unwrap().is_empty());
        // Malformed JSON → Err, not a panic.
        assert!(decode_args(&json!("{not json")).is_err());
        // A non-object JSON (array) → Err.
        assert!(decode_args(&json!("[1,2,3]")).is_err());
    }

    #[test]
    fn parse_time_accepts_unix_string_and_rfc3339() {
        assert_eq!(parse_time(&json!(1_700_000_000_i64)), Some(1_700_000_000));
        assert_eq!(parse_time(&json!("1700000000")), Some(1_700_000_000));
        // RFC3339 with an explicit offset is machine-tz independent.
        assert_eq!(
            parse_time(&json!("2026-07-16T00:00:00+00:00")),
            Some(1_784_160_000)
        );
        // Unparseable → None (defensive).
        assert_eq!(parse_time(&json!("whenever")), None);
        assert_eq!(parse_time(&json!(true)), None);
    }

    #[test]
    fn cited_ids_are_grounded_deduped_and_ordered() {
        let seen: HashSet<i64> = [6079, 42, 7].into_iter().collect();
        // "2 total" must NOT be cited (2 isn't a real id); 6079 & 42 are, in order.
        let ids = extract_cited_ids("I found events 6079 and 42 (2 total).", &seen);
        assert_eq!(ids, vec![6079, 42]);
        // Dedup: a repeated id appears once.
        assert_eq!(extract_cited_ids("event 7, also 7 again", &seen), vec![7]);
        // A hallucinated id (not in seen) is dropped.
        assert_eq!(extract_cited_ids("see event 9999", &seen), Vec::<i64>::new());
    }

    #[test]
    fn resolve_camera_is_scoped_and_leak_safe() {
        let cams = vec![
            crate::db::Camera {
                id: 1,
                name: "Front Door".into(),
                source: String::new(),
                detect_source: None,
                enabled: true,
                detect: true,
                record: true,
                created_ts: 0,
                detect_config: Default::default(),
                group: None,
            },
            crate::db::Camera {
                id: 2,
                name: "Back Yard".into(),
                source: String::new(),
                detect_source: None,
                enabled: true,
                detect: true,
                record: true,
                created_ts: 0,
                detect_config: Default::default(),
                group: None,
            },
        ];
        // Case-insensitive exact + unique-substring, unrestricted.
        assert_eq!(resolve_camera(&cams, None, "front door"), Some(1));
        assert_eq!(resolve_camera(&cams, None, "back"), Some(2));
        // A scoped caller (only cam 2) can't resolve cam 1 → None (leak-safe).
        let allow: HashSet<i64> = [2].into_iter().collect();
        assert_eq!(resolve_camera(&cams, Some(&allow), "front door"), None);
        assert_eq!(resolve_camera(&cams, Some(&allow), "back yard"), Some(2));
        // Unknown → None.
        assert_eq!(resolve_camera(&cams, None, "garage"), None);

        // query_scope: forbidden/unknown camera → Some(empty) = no results.
        assert_eq!(
            query_scope(&cams, Some(&allow), Some("front door")),
            Some(HashSet::new())
        );
        // No camera arg, scoped → the allow-list.
        assert_eq!(query_scope(&cams, Some(&allow), None), Some(allow.clone()));
        // No camera arg, unrestricted → None (all cameras).
        assert_eq!(query_scope(&cams, None, None), None);
    }
}
