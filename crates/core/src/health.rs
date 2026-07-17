//! Camera health watcher: pushes a phone notification (ntfy) when an enabled
//! camera stops delivering frames or comes back — the "camera disconnected"
//! alert every commercial NVR ships. Online logic mirrors the /api/status
//! endpoint so the push and the UI dot always agree.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::db::Db;
use crate::status::StatusBoard;

const CHECK_EVERY: Duration = Duration::from_secs(15);

pub fn run(db: Db, status: StatusBoard, shutdown: Arc<AtomicBool>) {
    // None = no verdict yet (startup warmup / camera just added): never
    // notify on the first observation, only on a real transition.
    let mut last_state: HashMap<i64, bool> = HashMap::new();

    while !shutdown.load(Ordering::Relaxed) {
        let settings = db.settings();
        let url = settings.health_ntfy_url.trim().to_string();
        let cameras = db.list_cameras().unwrap_or_default();
        let board = status.snapshot();
        let now = chrono::Local::now().timestamp();
        let window = crate::status::freshness_window(settings.poll_ms);

        for cam in &cameras {
            if !cam.enabled {
                // Intentionally paused — not an outage. Forget its state so
                // re-enabling starts fresh instead of firing "back online".
                last_state.remove(&cam.id);
                continue;
            }
            let h = board.get(&cam.id).cloned().unwrap_or_default();
            let online = h.is_online(cam.detect, now, window);

            match last_state.insert(cam.id, online) {
                Some(prev) if prev != online => {
                    let (title, msg, tags) = if online {
                        (
                            "Camera back online",
                            format!("{} is delivering frames again", cam.name),
                            "white_check_mark",
                        )
                    } else {
                        (
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
                    // A4: in-app notification on every transition; the ntfy phone
                    // push fires only when a topic URL is configured.
                    let _ = db.add_camera_notification(
                        now,
                        if online {
                            "camera_online"
                        } else {
                            "camera_offline"
                        },
                        title,
                        Some(&msg),
                        None,
                        cam.id,
                    );
                    if !url.is_empty() {
                        crate::notify::ntfy_text(&url, title, &msg, tags);
                    }
                }
                _ => {}
            }
        }
        last_state.retain(|id, _| cameras.iter().any(|c| c.id == *id));

        let waited = std::time::Instant::now();
        while waited.elapsed() < CHECK_EVERY && !shutdown.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}
