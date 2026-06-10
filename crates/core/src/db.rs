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
    /// Optional low-res second stream (e.g. a Dahua subtype=1 URL). When set,
    /// the detection pipeline samples frames from it instead of the main
    /// stream — decoding 640x480 instead of 4K (Frigate's "detect role").
    #[serde(default)]
    pub detect_source: Option<String>,
    pub enabled: bool,
    /// Run the motion gate + AI detector on this camera.
    pub detect: bool,
    /// Record this camera continuously to disk.
    pub record: bool,
    pub created_ts: i64,
    /// Per-camera detection tuning; unset fields inherit global settings.
    #[serde(default)]
    pub detect_config: DetectConfig,
}

/// A rectangle in frame-fraction coordinates (0..1), so it survives resolution
/// changes and sub-stream switches.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Zone {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Zone {
    pub fn contains(&self, fx: f32, fy: f32) -> bool {
        fx >= self.x && fx <= self.x + self.w && fy >= self.y && fy <= self.y + self.h
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DetectConfig {
    /// Override of the global label filter; `None` inherits.
    pub labels: Option<Vec<String>>,
    /// Per-camera minimum score; effective only above the global confidence
    /// (the model is run with the global threshold).
    pub min_score: Option<f32>,
    /// Override of the global motion threshold; `None` inherits.
    pub motion_threshold: Option<f32>,
    /// Detections whose box center falls in any of these are dropped —
    /// e.g. a busy street at the edge of a driveway camera.
    pub ignore_zones: Vec<Zone>,
    /// PTZ autotracking (Frigate-style): steer the camera to keep tracked
    /// objects centered. Only effective on ONVIF PTZ-capable cameras.
    pub autotrack: bool,
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
    /// Recognized identity (face recognition), when the detection is a person
    /// whose face matched an enrolled embedding.
    pub face: Option<String>,
    /// License plate text (LPR), when the detection is a vehicle with a
    /// readable plate.
    pub plate: Option<String>,
}

/// Alarm Manager rule (UniFi style if-this-then-that): all set conditions
/// must match an event; `None` conditions match anything.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AlarmRule {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub camera_id: Option<i64>,
    pub label: Option<String>,
    /// Substring match on the recognized face name.
    pub face_like: Option<String>,
    /// Substring match on the OCRed plate.
    pub plate_like: Option<String>,
    #[serde(default)]
    pub min_score: f32,
    /// "webhook" (POST event JSON to target URL) or "mqtt" (publish to
    /// {prefix}/{target}).
    pub action: String,
    pub target: String,
    #[serde(default)]
    pub created_ts: i64,
}

fn default_true() -> bool {
    true
}

impl AlarmRule {
    pub fn matches(
        &self,
        camera_id: i64,
        label: &str,
        score: f32,
        face: Option<&str>,
        plate: Option<&str>,
    ) -> bool {
        if !self.enabled || score < self.min_score {
            return false;
        }
        if self.camera_id.map(|c| c != camera_id).unwrap_or(false) {
            return false;
        }
        if self.label.as_deref().map(|l| l != label).unwrap_or(false) {
            return false;
        }
        if let Some(f) = self.face_like.as_deref() {
            let hit = face
                .map(|v| v.to_lowercase().contains(&f.to_lowercase()))
                .unwrap_or(false);
            if !hit {
                return false;
            }
        }
        if let Some(p) = self.plate_like.as_deref() {
            let hit = plate
                .map(|v| v.to_uppercase().contains(&p.to_uppercase()))
                .unwrap_or(false);
            if !hit {
                return false;
            }
        }
        true
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct FaceRow {
    pub id: i64,
    pub name: String,
    #[serde(skip)]
    pub embedding: Vec<f32>,
    pub created_ts: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct CamStorage {
    pub camera_id: i64,
    pub camera: String,
    pub segments: i64,
    pub bytes: u64,
    pub oldest_ts: Option<i64>,
    pub newest_ts: Option<i64>,
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
    /// Events (and their snapshots) older than this are deleted.
    pub event_retention_days: u32,
    pub model_path: String,
    pub force_cpu: bool,
    pub go2rtc_api_port: u16,
    /// POSTed a JSON payload for every event when non-empty (Blue Iris
    /// "alarm server" style).
    pub webhook_url: String,
    /// Transcode camera audio into recordings as AAC.
    pub record_audio: bool,
    /// Labels that count as "alerts" in the review UI (Frigate-style split);
    /// everything else files under plain "detections".
    pub alert_labels: Vec<String>,
    /// MQTT broker ("mqtt://user:pass@host:1883", "host:1883" or "host");
    /// empty = MQTT off.
    pub mqtt_url: String,
    /// Topic prefix for MQTT publishes.
    pub mqtt_prefix: String,
    /// Run face recognition on person detections (needs the two face models
    /// on disk; silently inactive when they are missing).
    pub face_recognition: bool,
    /// Cosine similarity needed to call a face a known person (ArcFace
    /// same-person scores typically land 0.4-0.7).
    pub face_match_threshold: f32,
    pub face_det_model: String,
    pub face_rec_model: String,
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
            event_retention_days: 30,
            model_path: "yolov8n.onnx".into(),
            force_cpu: false,
            go2rtc_api_port: 1984,
            webhook_url: String::new(),
            record_audio: false,
            alert_labels: ["person"].map(String::from).to_vec(),
            mqtt_url: String::new(),
            mqtt_prefix: "zoomy".into(),
            face_recognition: true,
            face_match_threshold: 0.4,
            face_det_model: "det_10g.onnx".into(),
            face_rec_model: "w600k_r50.onnx".into(),
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
        // Additive migrations; "duplicate column" on rerun is expected.
        let _ = conn.execute("ALTER TABLE cameras ADD COLUMN detect_json TEXT", []);
        let _ = conn.execute("ALTER TABLE cameras ADD COLUMN detect_source TEXT", []);
        let _ = conn.execute("ALTER TABLE events ADD COLUMN face TEXT", []);
        let _ = conn.execute("ALTER TABLE events ADD COLUMN plate TEXT", []);
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS faces (
                 id         INTEGER PRIMARY KEY,
                 name       TEXT NOT NULL,
                 embedding  BLOB NOT NULL,
                 created_ts INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS event_embeddings (
                 event_id  INTEGER PRIMARY KEY REFERENCES events(id) ON DELETE CASCADE,
                 embedding BLOB NOT NULL
             );
             CREATE TABLE IF NOT EXISTS alarms (
                 id         INTEGER PRIMARY KEY,
                 name       TEXT NOT NULL,
                 enabled    INTEGER NOT NULL DEFAULT 1,
                 camera_id  INTEGER,
                 label      TEXT,
                 face_like  TEXT,
                 plate_like TEXT,
                 min_score  REAL NOT NULL DEFAULT 0,
                 action     TEXT NOT NULL,
                 target     TEXT NOT NULL,
                 created_ts INTEGER NOT NULL
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
            "SELECT id, name, source, enabled, detect, record, created_ts, detect_json, detect_source
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
                "SELECT id, name, source, enabled, detect, record, created_ts, detect_json, detect_source
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
        detect_source: Option<&str>,
        detect: bool,
        record: bool,
    ) -> Result<Camera> {
        let now = chrono::Local::now().timestamp();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO cameras (name, source, detect_source, enabled, detect, record, created_ts)
             VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6)",
            params![name, source, detect_source, detect, record, now],
        )?;
        let id = conn.last_insert_rowid();
        Ok(Camera {
            id,
            name: name.into(),
            source: source.into(),
            detect_source: detect_source.map(String::from),
            enabled: true,
            detect,
            record,
            created_ts: now,
            detect_config: DetectConfig::default(),
        })
    }

    pub fn update_camera(&self, cam: &Camera) -> Result<()> {
        let detect_json = serde_json::to_string(&cam.detect_config)?;
        self.conn().execute(
            "UPDATE cameras SET name=?1, source=?2, enabled=?3, detect=?4, record=?5,
             detect_json=?6, detect_source=?7 WHERE id=?8",
            params![
                cam.name,
                cam.source,
                cam.enabled,
                cam.detect,
                cam.record,
                detect_json,
                cam.detect_source,
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
        face: Option<&str>,
        plate: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO events (camera_id, ts, label, score, x1, y1, x2, y2, snapshot, face, plate)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                camera_id, ts, label, score, bbox[0], bbox[1], bbox[2], bbox[3], snapshot, face,
                plate
            ],
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
                    e.x1, e.y1, e.x2, e.y2, e.snapshot, e.face, e.plate
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
                    face: r.get(11)?,
                    plate: r.get(12)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_event(&self, id: i64) -> Result<Option<Event>> {
        let conn = self.conn();
        let ev = conn
            .query_row(
                "SELECT e.id, e.camera_id, c.name, e.ts, e.label, e.score,
                        e.x1, e.y1, e.x2, e.y2, e.snapshot, e.face, e.plate
                 FROM events e JOIN cameras c ON c.id = e.camera_id WHERE e.id = ?1",
                [id],
                |r| {
                    Ok(Event {
                        id: r.get(0)?,
                        camera_id: r.get(1)?,
                        camera: r.get(2)?,
                        ts: r.get(3)?,
                        label: r.get(4)?,
                        score: r.get(5)?,
                        bbox: [r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?],
                        snapshot: r.get(10)?,
                        face: r.get(11)?,
                        plate: r.get(12)?,
                    })
                },
            )
            .optional()?;
        Ok(ev)
    }

    // --- alarms --------------------------------------------------------------

    pub fn add_alarm(&self, r: &AlarmRule) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO alarms (name, enabled, camera_id, label, face_like, plate_like,
             min_score, action, target, created_ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                r.name,
                r.enabled,
                r.camera_id,
                r.label,
                r.face_like,
                r.plate_like,
                r.min_score,
                r.action,
                r.target,
                chrono::Local::now().timestamp()
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_alarms(&self) -> Result<Vec<AlarmRule>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, enabled, camera_id, label, face_like, plate_like,
                    min_score, action, target, created_ts
             FROM alarms ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(AlarmRule {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    enabled: r.get::<_, i64>(2)? != 0,
                    camera_id: r.get(3)?,
                    label: r.get(4)?,
                    face_like: r.get(5)?,
                    plate_like: r.get(6)?,
                    min_score: r.get(7)?,
                    action: r.get(8)?,
                    target: r.get(9)?,
                    created_ts: r.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn set_alarm_enabled(&self, id: i64, enabled: bool) -> Result<()> {
        self.conn().execute(
            "UPDATE alarms SET enabled=?1 WHERE id=?2",
            params![enabled, id],
        )?;
        Ok(())
    }

    pub fn delete_alarm(&self, id: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM alarms WHERE id=?1", [id])?;
        Ok(())
    }

    // --- smart-search embeddings -------------------------------------------

    pub fn set_event_embedding(&self, event_id: i64, embedding: &[f32]) -> Result<()> {
        let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        self.conn().execute(
            "INSERT OR REPLACE INTO event_embeddings (event_id, embedding) VALUES (?1, ?2)",
            params![event_id, bytes],
        )?;
        Ok(())
    }

    pub fn all_event_embeddings(&self) -> Result<Vec<(i64, Vec<f32>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT event_id, embedding FROM event_embeddings")?;
        let rows = stmt
            .query_map([], |r| {
                let bytes: Vec<u8> = r.get(1)?;
                Ok((
                    r.get::<_, i64>(0)?,
                    bytes
                        .chunks_exact(4)
                        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                        .collect(),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // --- faces -------------------------------------------------------------

    pub fn add_face(&self, name: &str, embedding: &[f32]) -> Result<i64> {
        let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO faces (name, embedding, created_ts) VALUES (?1, ?2, ?3)",
            params![name, bytes, chrono::Local::now().timestamp()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_faces(&self) -> Result<Vec<FaceRow>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT id, name, embedding, created_ts FROM faces ORDER BY name")?;
        let rows = stmt
            .query_map([], |r| {
                let bytes: Vec<u8> = r.get(2)?;
                Ok(FaceRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    embedding: bytes
                        .chunks_exact(4)
                        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                        .collect(),
                    created_ts: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn delete_face(&self, id: i64) -> Result<()> {
        self.conn().execute("DELETE FROM faces WHERE id=?1", [id])?;
        Ok(())
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

    // --- stats -----------------------------------------------------------

    pub fn storage_stats(&self) -> Result<Vec<CamStorage>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.name, COUNT(s.id), COALESCE(SUM(s.bytes), 0),
                    MIN(s.start_ts), MAX(s.start_ts)
             FROM cameras c LEFT JOIN segments s ON s.camera_id = c.id
             GROUP BY c.id ORDER BY c.id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(CamStorage {
                    camera_id: r.get(0)?,
                    camera: r.get(1)?,
                    segments: r.get(2)?,
                    bytes: r.get::<_, i64>(3)? as u64,
                    oldest_ts: r.get(4)?,
                    newest_ts: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn count_events(&self) -> Result<i64> {
        Ok(self
            .conn()
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?)
    }

    /// Delete events older than `cutoff_ts`, returning their snapshot names
    /// so the caller can remove the files. Embeddings cascade.
    pub fn prune_events_before(&self, cutoff_ts: i64) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT snapshot FROM events WHERE ts < ?1 AND snapshot IS NOT NULL",
        )?;
        let snapshots = stmt
            .query_map([cutoff_ts], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        conn.execute("DELETE FROM events WHERE ts < ?1", [cutoff_ts])?;
        Ok(snapshots)
    }

    // --- generic KV (password hash etc.) ----------------------------------

    pub fn get_kv(&self, key: &str) -> Option<String> {
        self.conn()
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_kv(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_kv(&self, key: &str) -> Result<()> {
        self.conn()
            .execute("DELETE FROM settings WHERE key = ?1", [key])?;
        Ok(())
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
    let detect_json: Option<String> = r.get(7)?;
    Ok(Camera {
        id: r.get(0)?,
        name: r.get(1)?,
        source: r.get(2)?,
        enabled: r.get::<_, i64>(3)? != 0,
        detect: r.get::<_, i64>(4)? != 0,
        record: r.get::<_, i64>(5)? != 0,
        created_ts: r.get(6)?,
        detect_config: detect_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        detect_source: r.get(8)?,
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
        let cam = db
            .add_camera("porch", "rtsp://x", None, true, true)
            .unwrap();
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
        let cam = db
            .add_camera("porch", "rtsp://x", None, true, true)
            .unwrap();
        db.add_event(
            cam.id,
            100,
            "person",
            0.9,
            [1.0, 2.0, 3.0, 4.0],
            None,
            None,
            None,
        )
        .unwrap();
        db.add_event(cam.id, 200, "car", 0.8, [0.0; 4], None, None, None)
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
    fn detect_config_roundtrip_and_zone_math() {
        let db = mem_db();
        let mut cam = db
            .add_camera("porch", "rtsp://x", None, true, true)
            .unwrap();
        assert_eq!(cam.detect_config, DetectConfig::default());

        cam.detect_config = DetectConfig {
            labels: Some(vec!["person".into()]),
            min_score: Some(0.6),
            motion_threshold: Some(0.05),
            ignore_zones: vec![Zone {
                x: 0.0,
                y: 0.0,
                w: 0.5,
                h: 0.5,
            }],
            autotrack: true,
        };
        db.update_camera(&cam).unwrap();
        let back = db.get_camera(cam.id).unwrap().unwrap();
        assert_eq!(back.detect_config, cam.detect_config);

        let z = back.detect_config.ignore_zones[0];
        assert!(z.contains(0.25, 0.25));
        assert!(!z.contains(0.75, 0.25));
    }

    #[test]
    fn alarm_rules_match_conditions() {
        let rule = AlarmRule {
            id: 1,
            name: "person at door".into(),
            enabled: true,
            camera_id: Some(3),
            label: Some("person".into()),
            face_like: None,
            plate_like: None,
            min_score: 0.5,
            action: "webhook".into(),
            target: "http://x".into(),
            created_ts: 0,
        };
        assert!(rule.matches(3, "person", 0.8, None, None));
        assert!(!rule.matches(2, "person", 0.8, None, None)); // wrong camera
        assert!(!rule.matches(3, "car", 0.8, None, None)); // wrong label
        assert!(!rule.matches(3, "person", 0.3, None, None)); // below score

        let face_rule = AlarmRule {
            camera_id: None,
            label: None,
            face_like: Some("coat".into()),
            min_score: 0.0,
            ..rule.clone()
        };
        assert!(face_rule.matches(1, "person", 0.9, Some("dark-COAT-guy"), None));
        assert!(!face_rule.matches(1, "person", 0.9, None, None));

        let plate_rule = AlarmRule {
            face_like: None,
            plate_like: Some("au77".into()),
            ..face_rule
        };
        assert!(plate_rule.matches(1, "car", 0.9, None, Some("B8AU77")));
        assert!(!plate_rule.matches(1, "car", 0.9, None, Some("XYZ123")));

        let mut disabled = plate_rule.clone();
        disabled.enabled = false;
        assert!(!disabled.matches(1, "car", 0.9, None, Some("B8AU77")));
    }

    #[test]
    fn alarm_crud_roundtrip() {
        let db = mem_db();
        let id = db
            .add_alarm(&AlarmRule {
                id: 0,
                name: "r1".into(),
                enabled: true,
                camera_id: None,
                label: Some("person".into()),
                face_like: None,
                plate_like: None,
                min_score: 0.0,
                action: "webhook".into(),
                target: "http://t".into(),
                created_ts: 0,
            })
            .unwrap();
        assert_eq!(db.list_alarms().unwrap().len(), 1);
        db.set_alarm_enabled(id, false).unwrap();
        assert!(!db.list_alarms().unwrap()[0].enabled);
        db.delete_alarm(id).unwrap();
        assert!(db.list_alarms().unwrap().is_empty());
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
