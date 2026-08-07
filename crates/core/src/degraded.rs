//! Edge-triggered "this subsystem is degraded" reporting.
//!
//! Every silent-failure fix in this codebase has the same shape: something that
//! can fail over and over, where the owner must be told ONCE when it breaks and
//! ONCE when it recovers — never once per attempt (that is just a different way
//! to be ignored). `genai::err_transition` and the offsite/health latches each
//! re-derived it; this is that logic in one place so the next one doesn't
//! re-derive it slightly wrong.
//!
//! Deliberately NOT a general logger: the point is the in-app notification the
//! owner actually sees, which is why `report` takes a [`Db`].

use std::sync::atomic::{AtomicBool, Ordering};

use crate::db::Db;

/// Whether an outcome should produce a notification, given the current latch
/// state. `Some(true)` = it just broke, `Some(false)` = it just recovered,
/// `None` = say nothing. Pure → unit-tested.
pub fn transition(ok: bool, notified: bool) -> Option<bool> {
    match (ok, notified) {
        (false, false) => Some(true),
        (true, true) => Some(false),
        _ => None,
    }
}

/// What to say when a subsystem breaks and when it comes back. Written for the
/// homeowner: name the CONSEQUENCE, not the component.
pub struct Messages<'a> {
    /// Notification `kind` (also the de-dupe handle in the bell).
    pub kind: &'a str,
    pub down_title: &'a str,
    /// What stops working. The underlying error is appended in parentheses.
    pub down_body: &'a str,
    pub up_title: &'a str,
    pub up_body: &'a str,
}

/// A latch for one subsystem. Usually a `static`, so every call site that can
/// report the same failure shares one notification instead of each keeping its
/// own bool and speaking over the others.
pub struct Latch {
    down: AtomicBool,
}

impl Latch {
    pub const fn new() -> Self {
        Self {
            down: AtomicBool::new(false),
        }
    }

    /// Report one attempt's outcome. Writes at most one notification, on a
    /// transition. Returns true when it wrote one.
    pub fn report(&self, db: &Db, err: Option<&str>, m: &Messages<'_>) -> bool {
        let Some(broke) = transition(err.is_none(), self.down.load(Ordering::Relaxed)) else {
            return false;
        };
        // Claim the transition before writing, so two threads reporting the same
        // failure in the same instant produce one notification, not two.
        if self
            .down
            .compare_exchange(!broke, broke, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        let now = chrono::Utc::now().timestamp();
        if broke {
            let detail = err
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect::<String>();
            tracing::warn!(kind = m.kind, "{}: {detail}", m.down_title);
            let _ = db.add_notification(
                now,
                m.kind,
                m.down_title,
                Some(&format!("{} ({detail})", m.down_body)),
                None,
            );
        } else {
            tracing::info!(kind = m.kind, "{}", m.up_title);
            let _ = db.add_notification(now, m.kind, m.up_title, Some(m.up_body), None);
        }
        true
    }
}

impl Default for Latch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_is_edge_triggered_both_ways() {
        // Healthy and quiet → nothing to say.
        assert_eq!(transition(true, false), None);
        // First failure → speak, latch on.
        assert_eq!(transition(false, false), Some(true));
        // Still failing → silence (this is the anti-spam property; a webhook
        // failing on every event would otherwise write a notification per event).
        assert_eq!(transition(false, true), None);
        // Recovered → speak once, latch off.
        assert_eq!(transition(true, true), Some(false));
    }

    #[test]
    fn latch_reports_once_per_outage() {
        let dir = std::env::temp_dir().join(format!("cammy-latch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("latch.db")).expect("test db");
        let m = Messages {
            kind: "test_degraded",
            down_title: "Thing broke",
            down_body: "The thing stopped working.",
            up_title: "Thing fixed",
            up_body: "The thing works again.",
        };
        let l = Latch::new();
        assert!(l.report(&db, Some("boom"), &m), "first failure speaks");
        assert!(!l.report(&db, Some("boom"), &m), "repeat failure is silent");
        assert!(!l.report(&db, Some("other"), &m), "so is a different error");
        assert!(l.report(&db, None, &m), "recovery speaks");
        assert!(!l.report(&db, None, &m), "staying healthy is silent");
        assert!(
            l.report(&db, Some("again"), &m),
            "a NEW outage speaks again"
        );
        // Exactly three notifications: broke, recovered, broke.
        let rows = db.list_notifications(false, 50).expect("notifications");
        let ours: Vec<_> = rows.iter().filter(|n| n.kind == "test_degraded").collect();
        assert_eq!(ours.len(), 3, "one notification per transition, no more");
        // Newest first: the error detail rides in the body so the owner can act.
        assert!(ours[0]
            .body
            .as_deref()
            .unwrap_or_default()
            .contains("again"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
