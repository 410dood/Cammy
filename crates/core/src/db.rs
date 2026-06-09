//! SQLite store: camera registry, detection events, recording segment index,
//! and a single JSON settings blob. Connection is wrapped in a Mutex — every
//! query here is sub-millisecond, so contention is a non-issue at home scale.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct Db(Arc<Mutex<Connection>>);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Camera {
    pub id: i64,
    pub name: String,
    /// go2rtc source string: an rtsp:// URL, or any other source go2rtc accepts
    /// (ffmpeg:, exec:, onvif:, ...).
    pub source: String,
    pub enabled: bool,
    /// Run the motion gate + AI detector on this camera.
    pub detect: bool,
    /// Record this camera continuously to disk.
    pub record: bool,
    pub created_ts: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub id: i64,
    pub camera_id: i64,
    pub camera: String,
    pub ts: i64,
    pub label: String,
    pub score: f32,
    #[serde(rename = "box")]
    pub bbox: [f32; 4],
    pub snapshot: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SegmentRow {
    pub id: i64,
    pub camera_id: i64,
    pub camera: String,
    pub start_ts: i64,
    pub bytes: u64,
    pub path: String,
}

/// All tunables, stored as one JSON blob so adding a knob is not a migration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// COCO labels that produce events; empty = all labels.
    pub detect_labels: Vec<String>,
    pub confidence: f32,
    pub nms_iou: f32,
    /// Fraction (0..1) of changed pixels that counts as motion.
    pub motion_threshold: f32,
    /// How often the detection pipeline samples each camera.
    pub poll_ms: u64,
    /// Seconds between events of the same label on the same camera.
    pub event_cooldown_secs: i64,
    pub segment_seconds: u32,
    pub retention_days: u32,
    pub retention_gb: u32,
    pub model_path: String,
    pub force_cpu: bool,
    pub go2rtc_api_port: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            detect_labels: [
                "person",
                "car",
                "truck",
                "bus",
                "bicycle",
                "motorcycle",
                "dog",
                "cat",
            ]
            .map(String::from)
            .to_vec(),
            confidence: 0.45,
            nms_iou: 0.45,
            motion_threshold: 0.02,
            poll_ms: 1000,
            event_cooldown_secs: 10,
            segment_seconds: 60,
            retention_days: 7,
            retention_gb: 50,
            model_path: "yolov8n.onnx".into(),
            force_cpu: false,
            go2rtc_api_port: 1984,
        }
    }
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS cameras (
                 id         INTEGER PRIMARY KEY,
                 name       TEXT NOT NULL UNIQUE,
                 source     TEXT NOT NULL,
                 enabled    INTEGER NOT NULL DEFAULT 1,
                 detect     INTEGER NOT NULL DEFAULT 1,
                 record     INTEGER NOT NULL DEFAULT 1,
                 created_ts INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS events (
                 id        INTEGER PRIMARY KEY,
                 camera_id INTEGER NOT NULL REFERENCES cameras(id) ON DELETE CASCADE,
                 ts        INTEGER NOT NULL,
                 label     TEXT NOT NULL,
                 score     REAL NOT NULL,
                 x1 REAL NOT NULL, y1 REAL NOT NULL, x2 REAL NOT NULL, y2 REAL NOT NULL,
                 snapshot  TEXT
             );
             CREATE INDEX IF NOT EXISTS events_ts ON events(ts DESC);
             CREATE TABLE IF NOT EXISTS segments (
                 id        INTEGER PRIMARY KEY,
                 camera_id INTEGER NOT NULL REFERENCES cameras(id) ON DELETE CASCADE,
                 start_ts  INTEGER NOT NULL,
                 path      TEXT NOT NULL UNIQUE,
                 bytes     INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS segments_cam_ts ON segments(camera_id, start_ts DESC);
             CREATE TABLE IF NOT EXISTS settings (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );",
        )?;
        Ok(Self(Arc::new(Mutex::new(conn))))
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0.lock().expect("db mutex poisoned")
    }

    // --- cameras ---------------------------------------------------------

    pub fn list_cameras(&self) -> Result<Vec<Camera>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, source, enabled, detect, record, created_ts
             FROM cameras ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], row_to_camera)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_camera(&self, id: i64) -> Result<Option<Camera>> {
        let conn = self.conn();
        let cam = conn
            .query_row(
                "SELECT id, name, source, enabled, detect, record, created_ts
                 FROM cameras WHERE id = ?1",
                [id],
                row_to_camera,
            )
            .optional()?;
        Ok(cam)
    }

    pub fn add_camera(
        &self,
        name: &str,
        source: &str,
        detect: bool,
        record: bool,
    ) -> Result<Camera> {
        let now = chrono::Local::now().timestamp();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO cameras (name, source, enabled, detect, record, created_ts)
             VALUES (?1, ?2, 1, ?3, ?4, ?5)",
            params![name, source, detect, record, now],
        )?;
        let id = conn.last_insert_rowid();
        Ok(Camera {
            id,
            name: name.into(),
            source: source.into(),
            enabled: true,
            detect,
            record,
            created_ts: now,
        })
    }

    pub fn update_camera(&self, cam: &Camera) -> Result<()> {
        self.conn().execute(
            "UPDATE cameras SET name=?1, source=?2, enabled=?3, detect=?4, record=?5 WHERE id=?6",
            params![
                cam.name,
                cam.source,
                cam.enabled,
                cam.detect,
                cam.record,
                cam.id
            ],
        )?;
        Ok(())
    }

    pub fn delete_camera(&self, id: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM cameras WHERE id=?1", [id])?;
        Ok(())
    }

    // --- events ----------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn add_event(
        &self,
        camera_id: i64,
        ts: i64,
        label: &str,
        score: f32,
        bbox: [f32; 4],
        snapshot: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO events (camera_id, ts, label, score, x1, y1, x2, y2, snapshot)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![camera_id, ts, label, score, bbox[0], bbox[1], bbox[2], bbox[3], snapshot],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_events(
        &self,
        camera_id: Option<i64>,
        label: Option<&str>,
        before_ts: Option<i64>,
        limit: u32,
    ) -> Result<Vec<Event>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT e.id, e.camera_id, c.name, e.ts, e.label, e.score,
                    e.x1, e.y1, e.x2, e.y2, e.snapshot
             FROM events e JOIN cameras c ON c.id = e.camera_id
             WHERE (?1 IS NULL OR e.camera_id = ?1)
               AND (?2 IS NULL OR e.label = ?2)
               AND (?3 IS NULL OR e.ts < ?3)
             ORDER BY e.ts DESC, e.id DESC LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![camera_id, label, before_ts, limit], |r| {
                Ok(Event {
                    id: r.get(0)?,
                    camera_id: r.get(1)?,
                    camera: r.get(2)?,
                    ts: r.get(3)?,
                    label: r.get(4)?,
                    score: r.get(5)?,
                    bbox: [r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?],
                    snapshot: r.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // --- segments --------------------------------------------------------

    pub fn upsert_segment(
        &self,
        camera_id: i64,
        start_ts: i64,
        path: &str,
        bytes: u64,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO segments (camera_id, start_ts, path, bytes) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET bytes = excluded.bytes",
            params![camera_id, start_ts, path, bytes as i64],
        )?;
        Ok(())
    }

    pub fn delete_segment_by_path(&self, path: &str) -> Result<()> {
        self.conn()
            .execute("DELETE FROM segments WHERE path = ?1", [path])?;
        Ok(())
    }

    pub fn list_segments(&self, camera_id: Option<i64>, limit: u32) -> Result<Vec<SegmentRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT s.id, s.camera_id, c.name, s.start_ts, s.bytes, s.path
             FROM segments s JOIN cameras c ON c.id = s.camera_id
             WHERE (?1 IS NULL OR s.camera_id = ?1)
             ORDER BY s.start_ts DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![camera_id, limit], |r| {
                Ok(SegmentRow {
                    id: r.get(0)?,
                    camera_id: r.get(1)?,
                    camera: r.get(2)?,
                    start_ts: r.get(3)?,
                    bytes: r.get::<_, i64>(4)? as u64,
                    path: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_segment(&self, id: i64) -> Result<Option<SegmentRow>> {
        Ok(self
            .list_segments(None, u32::MAX)?
            .into_iter()
            .find(|s| s.id == id))
    }

    /// The newest segment for a camera that starts at or before `ts` — i.e. the
    /// recording most likely to contain that instant. The caller checks whether
    /// `ts` actually falls inside the segment's duration.
    pub fn find_segment_at(&self, camera_id: i64, ts: i64) -> Result<Option<SegmentRow>> {
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT s.id, s.camera_id, c.name, s.start_ts, s.bytes, s.path
                 FROM segments s JOIN cameras c ON c.id = s.camera_id
                 WHERE s.camera_id = ?1 AND s.start_ts <= ?2
                 ORDER BY s.start_ts DESC LIMIT 1",
                params![camera_id, ts],
                |r| {
                    Ok(SegmentRow {
                        id: r.get(0)?,
                        camera_id: r.get(1)?,
                        camera: r.get(2)?,
                        start_ts: r.get(3)?,
                        bytes: r.get::<_, i64>(4)? as u64,
                        path: r.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    // --- settings --------------------------------------------------------

    pub fn settings(&self) -> Settings {
        let json: Option<String> = self
            .conn()
            .query_row(
                "SELECT value FROM settings WHERE key = 'settings'",
                [],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        json.and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        let json = serde_json::to_string(s)?;
        self.conn().execute(
            "INSERT INTO settings (key, value) VALUES ('settings', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [json],
        )?;
        Ok(())
    }
}

fn row_to_camera(r: &rusqlite::Row<'_>) -> rusqlite::Result<Camera> {
    Ok(Camera {
        id: r.get(0)?,
        name: r.get(1)?,
        source: r.get(2)?,
        enabled: r.get::<_, i64>(3)? != 0,
        detect: r.get::<_, i64>(4)? != 0,
        record: r.get::<_, i64>(5)? != 0,
        created_ts: r.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_db() -> Db {
        let dir = std::env::temp_dir().join(format!("zoomy-db-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join(format!("t-{:?}.db", std::time::Instant::now()))).unwrap()
    }

    #[test]
    fn camera_crud_roundtrip() {
        let db = mem_db();
        let cam = db.add_camera("porch", "rtsp://x", true, true).unwrap();
        assert_eq!(db.list_cameras().unwrap().len(), 1);

        let mut cam2 = cam.clone();
        cam2.enabled = false;
        db.update_camera(&cam2).unwrap();
        assert!(!db.get_camera(cam.id).unwrap().unwrap().enabled);

        db.delete_camera(cam.id).unwrap();
        assert!(db.list_cameras().unwrap().is_empty());
    }

    #[test]
    fn events_filter_and_cascade() {
        let db = mem_db();
        let cam = db.add_camera("porch", "rtsp://x", true, true).unwrap();
        db.add_event(cam.id, 100, "person", 0.9, [1.0, 2.0, 3.0, 4.0], None)
            .unwrap();
        db.add_event(cam.id, 200, "car", 0.8, [0.0; 4], None)
            .unwrap();

        assert_eq!(db.list_events(None, None, None, 10).unwrap().len(), 2);
        assert_eq!(
            db.list_events(None, Some("person"), None, 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(db.list_events(None, None, Some(150), 10).unwrap().len(), 1);

        // Deleting the camera cascades to its events.
        db.delete_camera(cam.id).unwrap();
        assert!(db.list_events(None, None, None, 10).unwrap().is_empty());
    }

    #[test]
    fn settings_default_and_persist() {
        let db = mem_db();
        let mut s = db.settings();
        assert_eq!(s.go2rtc_api_port, 1984);
        s.confidence = 0.7;
        db.save_settings(&s).unwrap();
        assert_eq!(db.settings().confidence, 0.7);
    }
}
