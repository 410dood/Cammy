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
}
