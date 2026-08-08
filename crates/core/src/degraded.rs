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

    /// A house-style lint, and it exists because this bit three times in one
    /// day: a Rust `\`-continued string literal looks fine in the editor, and
    /// then `cargo fmt` collapses it onto ONE line and BAKES IN the indentation.
    /// "Home                     Assistant" and "AI captions are off
    /// <22 spaces> so it fires unchecked" both reached user-facing text that way.
    ///
    /// Long prose must use `concat!`. This scans the crate's own source for the
    /// residue, which is cheap and catches it wherever it appears — no reviewer
    /// has to notice a run of spaces inside a quoted string again.
    #[test]
    fn no_user_facing_string_carries_collapsed_indentation() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut bad: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("src dir").flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            // Stop at the test module: fixtures legitimately contain messy
            // whitespace (a transcript normalizer is fed "  Help   me" on
            // purpose), and nothing in there is user-facing.
            let text = match text.find("\n#[cfg(test)]") {
                Some(i) => &text[..i],
                None => &text[..],
            };
            for (n, line) in text.lines().enumerate() {
                // Only inside a quoted run on the line, and only for a gap that
                // follows a word character — indentation residue always does.
                // (Deliberately ignores `//` comment lines and ASCII-art blocks.)
                let code = line.trim_start();
                if code.starts_with("//") || code.starts_with("///") {
                    continue;
                }
                let Some(first) = line.find('"') else {
                    continue;
                };
                let Some(last) = line.rfind('"') else {
                    continue;
                };
                if last <= first {
                    continue;
                }
                let inner = &line[first + 1..last];
                let bytes: Vec<char> = inner.chars().collect();
                for i in 0..bytes.len().saturating_sub(3) {
                    // Skip a run that follows an ESCAPE — `\n    foo` is
                    // deliberate layout in a multi-line message (the evidence
                    // report, the go2rtc YAML template), not collapsed source
                    // indentation.
                    if i > 0 && bytes[i - 1] == '\\' {
                        continue;
                    }
                    if bytes[i].is_alphanumeric()
                        || bytes[i] == ','
                        || bytes[i] == '.'
                        || bytes[i] == '—'
                    {
                        let run = bytes[i + 1..].iter().take_while(|c| **c == ' ').count();
                        // 3+ is never intentional prose spacing; `\n    ` style
                        // ASCII indentation in a multi-line message uses \n.
                        if run >= 3 && bytes.get(i + 1 + run).is_some_and(|c| c.is_alphanumeric()) {
                            bad.push(format!(
                                "{}:{}: {}",
                                path.file_name().unwrap_or_default().to_string_lossy(),
                                n + 1,
                                inner
                                    .chars()
                                    .skip(i.saturating_sub(30))
                                    .take(90)
                                    .collect::<String>()
                            ));
                            break;
                        }
                    }
                }
            }
        }
        assert!(
            bad.is_empty(),
            "string literal(s) carry collapsed indentation — use concat! for long prose:\n{}",
            bad.join("\n")
        );
    }

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
