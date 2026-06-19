//! Live camera health, shared between the workers that observe it (pipeline
//! frame fetches, recording liveness) and the API that reports it. In-memory
//! only — health is ephemeral by nature and rebuilding it after restart takes
//! one poll cycle.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::Serialize;

#[derive(Clone, Debug, Default, Serialize)]
pub struct CamHealth {
    /// Last time a decoded frame was successfully pulled (unix secs).
    pub last_frame_ts: Option<i64>,
    /// Most recent frame-fetch failure, cleared on success.
    pub last_error: Option<String>,
    /// ffmpeg recorder process currently alive.
    pub recording: bool,
    /// Last YOLO inference latency for this camera (milliseconds).
    pub inference_ms: Option<f32>,
    /// Execution provider the camera's detector is using (DirectML/CoreML/CUDA/CPU).
    pub accelerator: Option<String>,
    /// Model file the camera's detector loaded.
    pub model: Option<String>,
}

/// Seconds within which a camera must have delivered a frame to count as
/// "online": three poll intervals plus slack, with a 20 s floor so a
/// sub-second `poll_ms` can't make healthy cameras flap, and saturating so an
/// absurd operator-set `poll_ms` can't overflow `i64` in debug builds.
///
/// Shared by `/api/status`, `/api/stats`, `/api/metrics`, and the health
/// notification worker so they never disagree about whether a camera is up.
pub fn freshness_window(poll_ms: u64) -> i64 {
    ((poll_ms as i64).saturating_mul(3) / 1000 + 5).max(20)
}

impl CamHealth {
    /// Whether the camera counts as online at `now` (unix secs): a detecting
    /// camera needs a frame within `window` seconds (see [`freshness_window`]);
    /// a detect-off camera just needs its recorder alive. Callers still gate on
    /// `camera.enabled` separately (a paused camera isn't an outage).
    pub fn is_online(&self, detect: bool, now: i64, window: i64) -> bool {
        if detect {
            self.last_frame_ts.map(|t| now - t <= window).unwrap_or(false)
        } else {
            self.recording
        }
    }
}

#[derive(Clone, Default)]
pub struct StatusBoard(Arc<RwLock<HashMap<i64, CamHealth>>>);

impl StatusBoard {
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<i64, CamHealth>> {
        self.0.write().expect("status board poisoned")
    }

    pub fn frame_ok(&self, camera_id: i64, ts: i64) {
        let mut m = self.write();
        let e = m.entry(camera_id).or_default();
        e.last_frame_ts = Some(ts);
        e.last_error = None;
    }

    pub fn frame_err(&self, camera_id: i64, err: String) {
        self.write().entry(camera_id).or_default().last_error = Some(err);
    }

    pub fn set_recording(&self, camera_id: i64, recording: bool) {
        self.write().entry(camera_id).or_default().recording = recording;
    }

    /// Record a detector run's latency + which accelerator/model served it.
    pub fn infer(&self, camera_id: i64, ms: f32, accelerator: &str, model: &str) {
        let mut m = self.write();
        let e = m.entry(camera_id).or_default();
        e.inference_ms = Some(ms);
        e.accelerator = Some(accelerator.to_string());
        e.model = Some(model.to_string());
    }

    /// Drop state for cameras that no longer exist.
    pub fn retain(&self, keep: &[i64]) {
        self.write().retain(|id, _| keep.contains(id));
    }

    pub fn snapshot(&self) -> HashMap<i64, CamHealth> {
        self.0.read().expect("status board poisoned").clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_ok_clears_error() {
        let b = StatusBoard::default();
        b.frame_err(1, "boom".into());
        assert_eq!(b.snapshot()[&1].last_error.as_deref(), Some("boom"));
        b.frame_ok(1, 123);
        let s = b.snapshot();
        assert_eq!(s[&1].last_frame_ts, Some(123));
        assert!(s[&1].last_error.is_none());
    }

    #[test]
    fn retain_drops_deleted_cameras() {
        let b = StatusBoard::default();
        b.set_recording(1, true);
        b.set_recording(2, true);
        b.retain(&[2]);
        let s = b.snapshot();
        assert!(!s.contains_key(&1));
        assert!(s.contains_key(&2));
    }

    #[test]
    fn freshness_window_floor_and_overflow() {
        // Typical 1 s poll: 3*1 + 5 = 8, raised to the 20 s floor.
        assert_eq!(freshness_window(1000), 20);
        // Sub-second poll must not drop below the floor (anti-flap).
        assert_eq!(freshness_window(100), 20);
        // A large but sane poll scales past the floor.
        assert_eq!(freshness_window(20_000), 65);
        // A huge-but-representable poll saturates instead of overflowing.
        assert_eq!(freshness_window(i64::MAX as u64), i64::MAX / 1000 + 5);
        // An absurd poll whose i64 cast wraps negative is still safe: the 20 s
        // floor catches it rather than returning a nonsensical tiny window.
        assert_eq!(freshness_window(u64::MAX), 20);
    }

    #[test]
    fn is_online_detect_vs_recording() {
        let now = 1_000;
        let window = 20;
        // Detecting camera: online iff a frame arrived within the window.
        let mut h = CamHealth {
            last_frame_ts: Some(now - 5),
            ..Default::default()
        };
        assert!(h.is_online(true, now, window));
        // Exact boundary: the window is inclusive (`<=`), so a frame exactly
        // `window` seconds old still counts as online.
        h.last_frame_ts = Some(now - window);
        assert!(h.is_online(true, now, window));
        h.last_frame_ts = Some(now - 30);
        assert!(!h.is_online(true, now, window));
        h.last_frame_ts = None;
        assert!(!h.is_online(true, now, window));
        // Detect-off camera ignores frames; it's online iff recording.
        h.recording = true;
        assert!(h.is_online(false, now, window));
        h.recording = false;
        assert!(!h.is_online(false, now, window));
    }
}
