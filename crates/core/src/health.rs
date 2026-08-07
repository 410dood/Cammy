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
/// the recorder's self-healing ffmpeg reconciles (which recover within a cycle,
/// `RECONCILE_EVERY` = 3 s) — so a momentary drop doesn't fire a false "offline"
/// / "recording stopped".
const DEBOUNCE_CHECKS: u32 = 2; // ~30s at CHECK_EVERY
/// Observations to watch a camera before its first verdict counts. go2rtc, the
/// recorder's ffmpeg and the detection pipeline each need time to produce a
/// first frame, so a perfectly healthy camera legitimately reads offline and
/// not-recording for the first ~30-60 s after the NVR starts (or after a camera
/// is added / re-enabled). Without this the 30 s de-bounce expired mid-warmup
/// and every ordinary restart emitted a burst of false "Camera offline" then
/// "Camera back online" pushes — observed live: 6 junk notifications within
/// 31 s of a clean start.
const WARMUP_CHECKS: u32 = 6; // ~90s at CHECK_EVERY
/// How often the "everything's healthy" reassurance heartbeat is sent.
const HEARTBEAT_SECS: i64 = 7 * 24 * 3600;
const HEARTBEAT_KEY: &str = "health_heartbeat_ts";

/// Whether a verdict should notify, given the previous verdict for that camera.
///
/// `prev == None` is the camera's FIRST verdict (worker start, camera added, or
/// camera re-enabled) and only happens once warmup has elapsed, so the reading
/// is trustworthy by then. Staying silent on a healthy first verdict keeps an
/// ordinary restart quiet; but a first verdict that is BAD must still speak —
/// otherwise restarting the NVR would launder an already-broken camera into
/// permanent silence, which is exactly the "did it even record?" failure this
/// watcher exists to catch. (Observed live: pool2 sat online-but-not-recording
/// across 30+ checks after a restart and never alerted, because the old code
/// required a transition it could no longer see.)
fn should_notify(prev: Option<bool>, healthy: bool) -> bool {
    match prev {
        Some(previous) => previous != healthy,
        None => !healthy,
    }
}

/// What the owner should be told after one observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Say {
    Nothing,
    Broken,
    Recovered,
}

/// Warmup-gated, de-bounced liveness verdict for ONE signal on ONE camera
/// (frames arriving, or the recorder being alive).
///
/// Pure and self-contained so the restart/blip behaviour is unit-testable
/// without a running NVR — the previous inline version spread `seen`/`streak`/
/// `state` across parallel `HashMap`s per signal and could only be validated by
/// restarting the server and watching the phone.
///
/// Dropping the `Watch` resets it, which is how "camera disabled", "camera went
/// offline" and "camera deleted" all re-arm: the replacement warms up again
/// rather than firing a spurious recovery.
#[derive(Debug, Default)]
struct Watch {
    /// Observations since this watch started (warmup counter).
    seen: u32,
    /// Consecutive bad readings (de-bounce counter).
    bad: u32,
    /// Last published verdict; `None` until warmup elapses.
    state: Option<bool>,
}

impl Watch {
    fn observe(&mut self, good: bool) -> Say {
        self.seen = self.seen.saturating_add(1);
        self.bad = if good { 0 } else { self.bad.saturating_add(1) };
        // A bad reading only counts once it has persisted; recovery is immediate.
        let healthy = good || self.bad < DEBOUNCE_CHECKS;
        if self.seen < WARMUP_CHECKS {
            return Say::Nothing; // still coming up — observe, don't judge
        }
        let prev = self.state.replace(healthy);
        match (should_notify(prev, healthy), healthy) {
            (false, _) => Say::Nothing,
            (true, true) => Say::Recovered,
            (true, false) => Say::Broken,
        }
    }
}

pub fn run(db: Db, status: StatusBoard, shutdown: Arc<AtomicBool>) {
    let mut online: HashMap<i64, Watch> = HashMap::new();
    // Recording liveness, only tracked while the camera is online AND expected to
    // record 24/7 (continuous, no schedule) — so a scheduled pause is never a
    // false alarm. Dropped whenever those preconditions fail, re-warming on return.
    let mut rec: HashMap<i64, Watch> = HashMap::new();
    let mut ticks: u32 = 0;

    while !shutdown.load(Ordering::Relaxed) {
        let settings = db.settings();
        let url = settings.health_ntfy_url.trim().to_string();
        let cameras = db.list_cameras().unwrap_or_default();
        let board = status.snapshot();
        let now = chrono::Local::now().timestamp();
        let window = crate::status::freshness_window(settings.poll_ms);

        // The heartbeat reads the whole board at once, so it waits for the same
        // warmup — otherwise a restart whose heartbeat happened to fall due sent
        // "0 of 5 cameras online … some cameras need a look" off a board no
        // camera had reported into yet. A false alarm from the feature whose
        // entire job is reassurance is worse than no heartbeat at all.
        if ticks >= WARMUP_CHECKS {
            maybe_heartbeat(&db, &settings, &cameras, &board, now, window, &url);
        }
        ticks = ticks.saturating_add(1);

        for cam in &cameras {
            if !cam.enabled {
                // Intentionally paused — not an outage. Forget its state so
                // re-enabling warms up fresh instead of firing "back online".
                online.remove(&cam.id);
                rec.remove(&cam.id);
                continue;
            }
            let h = board.get(&cam.id).cloned().unwrap_or_default();
            let is_online = h.is_online(cam.detect, now, window);
            let say = online.entry(cam.id).or_default().observe(is_online);

            if say != Say::Nothing {
                let (kind, title, msg, tags) = if say == Say::Recovered {
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
                tracing::info!(camera = %cam.name, ?say, "camera health changed");
                let _ = db.add_camera_notification(now, kind, title, Some(&msg), None, cam.id);
                if !url.is_empty() {
                    crate::notify::ntfy_text(&url, title, &msg, tags);
                }
            }
            // The de-bounced verdict, not the raw reading, gates the recording
            // watch below — a one-cycle frame gap must not tear down (and so
            // re-warm) recording liveness.
            let online_now = online.get(&cam.id).and_then(|w| w.state).unwrap_or(true);

            // Silent recording failure: the stream is up but the recorder's ffmpeg
            // died (or never started) — only meaningful for cameras set to record
            // continuously with no schedule gating them off right now.
            let expect_record = cam.record && cam.detect_config.record_schedule.is_none();
            if online_now && expect_record {
                let say = rec.entry(cam.id).or_default().observe(h.recording);
                if say != Say::Nothing {
                    let (kind, title, msg, tags) = if say == Say::Recovered {
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
                    tracing::warn!(camera = %cam.name, ?say, "recording liveness changed");
                    let _ = db.add_camera_notification(now, kind, title, Some(&msg), None, cam.id);
                    if !url.is_empty() {
                        crate::notify::ntfy_text(&url, title, &msg, tags);
                    }
                }
            } else {
                // Not applicable (offline, or not a 24/7 recorder) — reset so
                // re-entry warms up fresh without a spurious "recording resumed".
                rec.remove(&cam.id);
            }
        }
        // Drop state for cameras that no longer exist. A deleted camera never
        // reaches the disabled/offline reset branches above (it is simply gone
        // from `cameras`), so anything left keyed by its id would linger for the
        // life of the process.
        let live: std::collections::HashSet<i64> = cameras.iter().map(|c| c.id).collect();
        online.retain(|id, _| live.contains(id));
        rec.retain(|id, _| live.contains(id));

        let waited = std::time::Instant::now();
        while waited.elapsed() < CHECK_EVERY && !shutdown.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// Measured write rate -> how far back footage really reaches, and which cap is
/// binding. Same basis as `/api/stats`, so the phone push and the storage card
/// can never disagree. `None` when it isn't measurable yet (a fresh install).
fn retention_estimate(
    db: &Db,
    settings: &crate::db::Settings,
) -> Option<(f64, crate::util::RetentionLimit)> {
    let stats = db.storage_stats().ok()?;
    let mut per_day = 0.0f64;
    for c in &stats {
        if let (Some(o), Some(n)) = (c.oldest_ts, c.newest_ts) {
            let span_days = (n - o) as f64 / 86_400.0;
            // Need at least an hour of span before a rate means anything.
            if span_days >= 1.0 / 24.0 {
                per_day += c.bytes as f64 / span_days;
            }
        }
    }
    let (days, limit) = crate::util::retention_horizon(
        settings.retention_days,
        settings.retention_gb,
        per_day.round() as u64,
    );
    days.map(|d| (d, limit))
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
    // Report how far back footage ACTUALLY reaches, not the configured ceiling.
    // `retention_days` is only an upper bound; the byte cap normally binds first
    // and on a multi-camera 4K install it binds hard — measured here, a
    // configured "7 days" against a 20 GB cap and 78.8 GB/day is really about
    // six hours. Pushing "about 7 days" to the owner's phone told them they
    // could go back and find footage that had already been recycled, which is
    // the most damaging thing a reassurance message can get wrong.
    let retain = match retention_estimate(db, settings) {
        Some((days, limit)) => {
            let because = match limit {
                crate::util::RetentionLimit::Disk => " (limited by the storage cap)",
                _ => "",
            };
            format!(
                " Keeping {} of footage{because}.",
                crate::util::humanize_days(days)
            )
        }
        None => String::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a watch a sequence of raw readings, returning everything it said.
    fn run_watch(readings: &[bool]) -> Vec<Say> {
        let mut w = Watch::default();
        readings
            .iter()
            .map(|&good| w.observe(good))
            .filter(|s| *s != Say::Nothing)
            .collect()
    }

    #[test]
    fn should_notify_speaks_on_change_and_on_a_bad_first_verdict() {
        // Ordinary transitions.
        assert!(should_notify(Some(true), false));
        assert!(should_notify(Some(false), true));
        assert!(!should_notify(Some(true), true));
        assert!(!should_notify(Some(false), false));
        // First verdict: quiet when healthy (a restart must not buzz)...
        assert!(!should_notify(None, true));
        // ...but a camera already broken when we start MUST still be reported,
        // or a restart would launder it into permanent silence.
        assert!(should_notify(None, false));
    }

    #[test]
    fn healthy_camera_that_is_slow_to_warm_up_stays_silent() {
        // The live regression: go2rtc/ffmpeg/the pipeline need ~30-45 s, so a
        // perfectly healthy camera reads offline for the first few checks. The
        // old code seeded optimistically then flipped, emitting a false
        // "Camera offline" + "Camera back online" pair on every restart.
        let mut readings = vec![false; 3]; // ~45 s of warmup
        readings.extend(vec![true; 10]);
        assert_eq!(run_watch(&readings), Vec::<Say>::new());
    }

    #[test]
    fn camera_broken_before_startup_is_reported_once() {
        // pool2's live case: online but never recording since the NVR started.
        // There is no transition to observe, so the old code never spoke.
        assert_eq!(run_watch(&[false; 20]), vec![Say::Broken]);
    }

    #[test]
    fn blip_after_warmup_is_debounced_but_a_real_outage_is_not() {
        let warm = vec![true; WARMUP_CHECKS as usize];

        // One bad reading (a single missed poll / ffmpeg reconcile) says nothing.
        let mut blip = warm.clone();
        blip.extend([false, true, true]);
        assert_eq!(run_watch(&blip), Vec::<Say>::new());

        // Sustained badness crosses the de-bounce, then recovery is immediate.
        let mut outage = warm;
        outage.extend([false, false, false, true]);
        assert_eq!(run_watch(&outage), vec![Say::Broken, Say::Recovered]);
    }

    #[test]
    fn recovery_is_reported_only_once() {
        let mut readings = vec![true; WARMUP_CHECKS as usize];
        readings.extend(vec![false; 4]);
        readings.extend(vec![true; 4]);
        assert_eq!(run_watch(&readings), vec![Say::Broken, Say::Recovered]);
    }

    #[test]
    fn a_dropped_watch_re_warms_instead_of_claiming_recovery() {
        // Dropping is how "camera disabled" / "went offline" / "deleted" re-arm.
        // The replacement must observe afresh, not immediately announce.
        let mut w = Watch::default();
        for _ in 0..20 {
            w.observe(false);
        }
        assert_eq!(w.state, Some(false));
        let mut replacement = Watch::default();
        assert_eq!(replacement.observe(true), Say::Nothing);
    }
}
