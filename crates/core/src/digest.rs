//! B1 — daily AI digest worker. Once per day (when enabled) it summarizes the
//! last 24 hours of events into a short, plain-language recap, stores it, and
//! drops a notification so the Home dashboard and notifications center can show
//! "what happened". The summary is deterministic and templated, so there is no
//! external LLM dependency (the GenAI captioner already enriches individual
//! events; this stitches the day together).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{Datelike, Local, TimeZone, Timelike};

use crate::db::{Db, Event};

const TICK: Duration = Duration::from_secs(900); // wake every 15 min
const DIGEST_HOUR: u32 = 7; // emit the morning recap at/after 07:00 local

pub fn run(db: Db, shutdown: Arc<AtomicBool>) {
    // Don't re-emit a digest for a day already covered (survives restarts by
    // reading the latest stored digest's local day).
    let mut last_day = last_digest_day(&db);
    while !shutdown.load(Ordering::Relaxed) {
        if db.settings().digest_enabled {
            let now = Local::now();
            let day_num = now.date_naive().num_days_from_ce() as i64;
            if last_day != Some(day_num) && now.hour() >= DIGEST_HOUR {
                let since = now.timestamp() - 86_400;
                let events = db
                    .list_events(None, None, None, None, Some(since), None, false, 20_000)
                    .unwrap_or_default();
                let text = summarize(&events);
                if db.add_digest(now.timestamp(), &text).is_ok() {
                    let _ = db.add_notification(
                        now.timestamp(),
                        "digest",
                        "Daily digest ready",
                        Some(&text),
                        None,
                    );
                    last_day = Some(day_num);
                    tracing::info!(events = events.len(), "daily digest written");
                }
            }
        }
        crate::util::sleep_interruptible(TICK, &shutdown);
    }
}

fn last_digest_day(db: &Db) -> Option<i64> {
    let ts = db.list_digests(1).ok()?.first()?.ts;
    let dt = Local.timestamp_opt(ts, 0).single()?;
    Some(dt.date_naive().num_days_from_ce() as i64)
}

/// Build a deterministic, plain-language recap of a window's events. Public so
/// the `POST /api/digests/run` endpoint can generate one on demand.
pub fn summarize(events: &[Event]) -> String {
    if events.is_empty() {
        return "Quiet day. No detections in the last 24 hours.".to_string();
    }
    let mut by_label: BTreeMap<&str, u32> = BTreeMap::new();
    let mut by_camera: BTreeMap<&str, u32> = BTreeMap::new();
    let mut known_people: BTreeSet<&str> = BTreeSet::new();
    let mut plates: BTreeSet<&str> = BTreeSet::new();
    let mut strangers = 0u32;
    let mut hours = [0u32; 24];

    for e in events {
        *by_label.entry(e.label.as_str()).or_default() += 1;
        *by_camera.entry(e.camera.as_str()).or_default() += 1;
        match e.face.as_deref() {
            Some("?") => strangers += 1,
            Some(name) => {
                known_people.insert(name);
            }
            None => {}
        }
        if let Some(p) = e.plate.as_deref() {
            plates.insert(p);
        }
        if let Some(dt) = Local.timestamp_opt(e.ts, 0).single() {
            hours[dt.hour() as usize] += 1;
        }
    }

    let mut parts: Vec<String> = vec![format!("{} detections in the last 24 hours.", events.len())];

    let mut labels: Vec<(&str, u32)> = by_label.into_iter().collect();
    labels.sort_by_key(|x| std::cmp::Reverse(x.1));
    let label_str = labels
        .iter()
        .take(4)
        .map(|(l, n)| format!("{n} {l}"))
        .collect::<Vec<_>>()
        .join(", ");
    if !label_str.is_empty() {
        parts.push(format!("Mostly {label_str}."));
    }
    if !known_people.is_empty() {
        parts.push(format!(
            "Recognized {}.",
            known_people.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    if strangers > 0 {
        parts.push(format!(
            "{strangers} stranger sighting{}.",
            if strangers == 1 { "" } else { "s" }
        ));
    }
    if !plates.is_empty() {
        parts.push(format!(
            "Plates seen: {}.",
            plates.into_iter().take(6).collect::<Vec<_>>().join(", ")
        ));
    }
    if let Some((cam, n)) = by_camera.iter().max_by_key(|(_, n)| **n) {
        parts.push(format!("Busiest camera: {cam} ({n})."));
    }
    if let Some((h, n)) = hours.iter().enumerate().max_by_key(|(_, n)| **n) {
        if *n > 0 {
            parts.push(format!("Peak activity around {h:02}:00."));
        }
    }
    parts.join(" ")
}
