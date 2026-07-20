//! Camera health watcher: the "did it even record?" guardian. Pushes a phone
//! notification (ntfy) + in-app notification when an enabled camera stops
//! delivering frames, when it's up but silently STOPPED RECORDING (the
//! silent-killer case a stream-only "offline" check misses — Frigate #11021 /
//! #18621), or when it recovers. Brief blips are de-bounced so a WiFi hiccup or
//! the recorder's self-healing ffmpeg reconcile doesn't spam you. A weekly
//! reassurance heartbeat ("all cameras healthy, N recording") turns
//! self-hosting's biggest anxiety — nobody's watching the watcher — into a trust
//! signal. Online logic mirrors /api/status so push and UI dot always agree.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::db::Db;
use crate::status::StatusBoard;

const CHECK_EVERY: Duration = Duration::from_secs(15);
/// Consecutive bad observations before we alert. De-bounces brief WiFi blips and
/// the recorder's self-healing ffmpeg reconciles (which recover within a cycle) —
/// so a momentary drop doesn't fire a false "offline" / "recording stopped".
const DEBOUNCE_CHECKS: u32 = 2; // ~30s at CHECK_EVERY
/// How often the "everything's healthy" reassurance heartbeat is sent.
const HEARTBEAT_SECS: i64 = 7 * 24 * 3600;
const HEARTBEAT_KEY: &str = "health_heartbeat_ts";

pub fn run(db: Db, status: StatusBoard, shutdown: Arc<AtomicBool>) {
    // None = no verdict yet (startup warmup / camera just added): never notify on
    // the first observation, only on a real transition.
    let mut online_state: HashMap<i64, bool> = HashMap::new();
    let mut offline_streak: HashMap<i64, u32> = HashMap::new();
    // Recording liveness, only tracked while the camera is online AND expected to
    // record 24/7 (continuous, no schedule) — so a scheduled pause is never a
    // false alarm. Dropped whenever those preconditions fail, re-seeding on return.
    let mut rec_state: HashMap<i64, bool> = HashMap::new();
    let mut rec_off_streak: HashMap<i64, u32> = HashMap::new();

    while !shutdown.load(Ordering::Relaxed) {
        let settings = db.settings();
        let url = settings.health_ntfy_url.trim().to_string();
        let cameras = db.list_cameras().unwrap_or_default();
        let board = status.snapshot();
        let now = chrono::Local::now().timestamp();
        let window = crate::status::freshness_window(settings.poll_ms);

        maybe_heartbeat(&db, &settings, &cameras, &board, now, window, &url);

        for cam in &cameras {
            if !cam.enabled {
                // Intentionally paused — not an outage. Forget its state so
                // re-enabling starts fresh instead of firing "back online".
                online_state.remove(&cam.id);
                offline_streak.remove(&cam.id);
                rec_state.remove(&cam.id);
                rec_off_streak.remove(&cam.id);
                continue;
            }
            let h = board.get(&cam.id).cloned().unwrap_or_default();
            let raw_online = h.is_online(cam.detect, now, window);

            // De-bounce: a camera counts as offline for alerting only after
            // DEBOUNCE_CHECKS consecutive misses; recovery is immediate.
            let streak = if raw_online {
                0
            } else {
                offline_streak.get(&cam.id).copied().unwrap_or(0) + 1
            };
            offline_streak.insert(cam.id, streak);
            let online = raw_online || streak < DEBOUNCE_CHECKS;

            if let Some(prev) = online_state.insert(cam.id, online) {
                if prev != online {
                    let (kind, title, msg, tags) = if online {
                        (
                            "camera_online",
                            "Camera back online",
                            format!("{} is delivering frames again", cam.name),
                            "white_check_mark",
                        )
                    } else {
                        (
                            "camera_offline",
                            "Camera offline",
                            format!(
                                "{} stopped responding{}",
                                cam.name,
                                h.last_error
                                    .as_deref()
                                    .map(|e| format!(" — {e}"))
                                    .unwrap_or_default()
                            ),
                            "warning",
                        )
                    };
                    tracing::info!(camera = %cam.name, online, "camera health changed");
                    let _ = db.add_camera_notification(now, kind, title, Some(&msg), None, cam.id);
                    if !url.is_empty() {
                        crate::notify::ntfy_text(&url, title, &msg, tags);
                    }
                }
            }

            // Silent recording failure: the stream is up but the recorder's ffmpeg
            // died (or never started) — only meaningful for cameras set to record
            // continuously with no schedule gating them off right now.
            let expect_record = cam.record && cam.detect_config.record_schedule.is_none();
            if online && expect_record {
                let rec_streak = if h.recording {
                    0
                } else {
                    rec_off_streak.get(&cam.id).copied().unwrap_or(0) + 1
                };
                rec_off_streak.insert(cam.id, rec_streak);
                let recording = h.recording || rec_streak < DEBOUNCE_CHECKS;
                if let Some(prev) = rec_state.insert(cam.id, recording) {
                    if prev != recording {
                        let (kind, title, msg, tags) = if recording {
                            (
                                "recording_resumed",
                                "Recording resumed",
                                format!("{} is recording again", cam.name),
                                "white_check_mark",
                            )
                        } else {
                            (
                                "recording_stopped",
                                "Recording stopped",
                                format!(
                                    "{} is online but has stopped recording — footage is \
                                     not being saved. The recorder will keep retrying.",
                                    cam.name
                                ),
                                "warning",
                            )
                        };
                        tracing::warn!(camera = %cam.name, recording, "recording liveness changed");
                        let _ =
                            db.add_camera_notification(now, kind, title, Some(&msg), None, cam.id);
                        if !url.is_empty() {
                            crate::notify::ntfy_text(&url, title, &msg, tags);
                        }
                    }
                }
            } else {
                // Not applicable (offline, or not a 24/7 recorder) — reset so
                // re-entry seeds fresh without a spurious "recording resumed".
                rec_state.remove(&cam.id);
                rec_off_streak.remove(&cam.id);
            }
        }
        // Drop state for cameras that no longer exist.
        online_state.retain(|id, _| cameras.iter().any(|c| c.id == *id));
        offline_streak.retain(|id, _| cameras.iter().any(|c| c.id == *id));

        let waited = std::time::Instant::now();
        while waited.elapsed() < CHECK_EVERY && !shutdown.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// Weekly "everything's healthy" reassurance. KV-persisted so a restart doesn't
/// re-send, and seeded (not sent) on first ever run so a fresh install doesn't
/// immediately buzz. Opt-out via `Settings.health_heartbeat`.
fn maybe_heartbeat(
    db: &Db,
    settings: &crate::db::Settings,
    cameras: &[crate::db::Camera],
    board: &HashMap<i64, crate::status::CamHealth>,
    now: i64,
    window: i64,
    url: &str,
) {
    if !settings.health_heartbeat {
        return;
    }
    let last = db
        .get_kv(HEARTBEAT_KEY)
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    if last == 0 {
        let _ = db.set_kv(HEARTBEAT_KEY, &now.to_string());
        return;
    }
    if now - last < HEARTBEAT_SECS {
        return;
    }
    let enabled: Vec<&crate::db::Camera> = cameras.iter().filter(|c| c.enabled).collect();
    let total = enabled.len();
    if total == 0 {
        let _ = db.set_kv(HEARTBEAT_KEY, &now.to_string());
        return;
    }
    let online = enabled
        .iter()
        .filter(|c| {
            board
                .get(&c.id)
                .map(|h| h.is_online(c.detect, now, window))
                .unwrap_or(false)
        })
        .count();
    let recording = enabled
        .iter()
        .filter(|c| c.record && board.get(&c.id).map(|h| h.recording).unwrap_or(false))
        .count();
    let retain = if settings.retention_days > 0 {
        format!(
            " Keeping about {} days of footage.",
            settings.retention_days
        )
    } else {
        String::new()
    };
    let (title, msg) = if online == total {
        (
            "Weekly check: all cameras healthy",
            format!(
                "All {total} cameras are online and {recording} are recording.{retain} \
                 Nothing needs your attention."
            ),
        )
    } else {
        (
            "Weekly check: attention needed",
            format!(
                "{online} of {total} cameras online, {recording} recording.{retain} \
                 Some cameras need a look."
            ),
        )
    };
    let _ = db.add_notification(now, "health_heartbeat", title, Some(&msg), None);
    if !url.is_empty() {
        let tag = if online == total {
            "white_check_mark"
        } else {
            "warning"
        };
        crate::notify::ntfy_text(url, title, &msg, tag);
    }
    let _ = db.set_kv(HEARTBEAT_KEY, &now.to_string());
    tracing::info!(online, total, recording, "weekly health heartbeat sent");
}
