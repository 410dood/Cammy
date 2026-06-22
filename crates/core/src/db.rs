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
    /// Optional organizational group (e.g. "downstairs", "outdoor") used to
    /// filter the live grid into camera groups / video walls. Pure metadata —
    /// it does not affect go2rtc, recording, or detection.
    #[serde(default)]
    pub group: Option<String>,
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

/// What a polygon zone does to detections whose anchor point falls inside it.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ZoneKind {
    /// Drop matching detections inside the zone (e.g. a public sidewalk).
    #[default]
    Ignore,
    /// Only keep matching detections that fall inside *some* required zone
    /// (e.g. only alert on people actually on the driveway).
    Required,
}

/// An arbitrary polygon zone in frame-fraction coordinates (0..1), so it
/// survives resolution changes and sub-stream switches. Rectangles are just a
/// 4-point special case — this supersedes [`Zone`] for new cameras while old
/// rectangle `ignore_zones` keep working.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PolyZone {
    pub name: String,
    /// Polygon vertices as [x, y] fractions, in order. Needs ≥3 to have area.
    pub points: Vec<[f32; 2]>,
    pub kind: ZoneKind,
    /// Object labels this zone applies to; empty = every object.
    pub labels: Vec<String>,
    /// Loitering threshold: if set (>0), a tracked object whose ground-contact
    /// point dwells inside this zone for this many seconds fires a `loiter`
    /// event. `None`/0 = not a dwell zone. Requires object tracking.
    #[serde(default)]
    pub dwell_secs: Option<u32>,
    /// Live-occupancy limit: if set (>0), an `occupancy` event fires when the
    /// number of confirmed tracks currently inside this zone first exceeds it
    /// (edge-triggered — one event per rising crossing of the limit, re-armed
    /// when the count drops back to/below it). `None`/0 = no limit. Requires
    /// object tracking. The live count is also published to the status board.
    #[serde(default)]
    pub occupancy_max: Option<u32>,
    /// Residential: fire a `zone_enter` event (its label is the object's class)
    /// the first frame a label-scoped track enters this zone — edge-triggered,
    /// once per entry. Powers "person enters the pool", "pet on the couch".
    /// Off by default. Requires object tracking. See `residential.rs`.
    #[serde(default)]
    pub alert_enter: bool,
    /// Residential: if a *child*-classified person (see
    /// `DetectConfig.child_height_frac`) enters this zone, fire a `child` event —
    /// child-in-restricted-zone (stairs / kitchen / driveway). Requires per-camera
    /// child calibration. ASSISTIVE — a detection aid, not guaranteed coverage.
    #[serde(default)]
    pub child_watch: bool,
    /// Residential: fire a `child_alone` event when a child is in this zone with
    /// NO adult present (edge-triggered) — the unattended-child-near-pool framing.
    /// Requires child calibration. ASSISTIVE — never a substitute for supervision
    /// or a pool fence; can miss a child if the height heuristic misreads them.
    #[serde(default)]
    pub supervise: bool,
    /// Residential: this zone is water (a pool). A person who goes motionless in
    /// it fires an EXPERIMENTAL `still_water` hint. This is NOT drowning
    /// detection — an above-water camera cannot see a submerged body. Off by default.
    #[serde(default)]
    pub water: bool,
}

impl PolyZone {
    /// Even-odd ray-casting point-in-polygon test (point in frame fractions).
    pub fn contains(&self, fx: f32, fy: f32) -> bool {
        point_in_polygon(&self.points, fx, fy)
    }

    /// Does this zone govern detections of `label`? (Empty `labels` = all.)
    pub fn applies_to(&self, label: &str) -> bool {
        self.labels.is_empty() || self.labels.iter().any(|l| l == label)
    }
}

/// Decode a little-endian f32 BLOB (as stored for CLIP embeddings) back to a
/// vector. Trailing bytes that don't form a whole f32 are ignored.
fn bytes_to_f32(b: Vec<u8>) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Even-odd ray-casting point-in-polygon. Returns false for degenerate
/// polygons (< 3 vertices).
pub fn point_in_polygon(poly: &[[f32; 2]], x: f32, y: f32) -> bool {
    if poly.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (xi, yi) = (poly[i][0], poly[i][1]);
        let (xj, yj) = (poly[j][0], poly[j][1]);
        let intersects =
            ((yi > y) != (yj > y)) && (x < (xj - xi) * (y - yi) / (yj - yi + f32::EPSILON) + xi);
        if intersects {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Ground-plane calibration for speed estimation: four image points (frame
/// fractions) marking the corners of a real, flat ground rectangle of `width_m`
/// × `height_m`, in the order top-left, top-right, bottom-right, bottom-left.
/// The pipeline solves a homography from this to turn pixel motion into m/s.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GroundCalib {
    pub points: [[f32; 2]; 4],
    pub width_m: f32,
    pub height_m: f32,
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
    /// e.g. a busy street at the edge of a driveway camera. Legacy rectangles;
    /// new cameras use `zones` (polygons). Both are honored.
    pub ignore_zones: Vec<Zone>,
    /// Polygon zones (required / ignore), the richer successor to
    /// `ignore_zones`. A `Required` zone makes detections valid only when their
    /// anchor lands inside one; `Ignore` zones drop them.
    pub zones: Vec<PolyZone>,
    /// Directed virtual lines for line-crossing analytics (in/out counting,
    /// perimeter, one-way enforcement). Requires object tracking; a confirmed
    /// track whose ground-contact point crosses a line fires a `crossing` event.
    #[serde(default)]
    pub tripwires: Vec<crate::analytics::Tripwire>,
    /// Optional ground-plane calibration enabling speed estimation (km/h on
    /// crossing events). `None` = no speed.
    #[serde(default)]
    pub ground_calib: Option<GroundCalib>,
    /// Polygon privacy masks: these regions are blacked out of the frame before
    /// motion, detection and snapshots — nothing inside is analyzed or stored.
    /// (Continuous recordings are packet-copied and are not masked.)
    pub privacy_masks: Vec<Vec<[f32; 2]>>,
    /// Object-size gate as a fraction of frame area (0..1). Detections smaller
    /// than `min_area` or larger than `max_area` are dropped — kills tiny
    /// far-field blips and whole-frame lighting flips. `None` = no bound.
    pub min_area: Option<f32>,
    pub max_area: Option<f32>,
    /// PTZ autotracking (Frigate-style): steer the camera to keep tracked
    /// objects centered. Only effective on ONVIF PTZ-capable cameras.
    pub autotrack: bool,
    /// Classify this camera's audio (YAMNet) for security-relevant sounds.
    pub audio_detect: bool,
    /// Frigate-style retain mode: when true, retention deletes segments with
    /// no nearby event after a grace period — continuous footage becomes
    /// event-bracketed clips, saving most of the disk.
    pub event_only_recording: bool,
    /// Offer the live hand-signal overlay for this camera (the Signals page can
    /// attribute recognized gestures to it). Detection itself runs client-side.
    pub gesture_detect: bool,
    /// Per-camera model override (e.g. a specialized .onnx); `None` inherits the
    /// global model. Lets different cameras run different detectors.
    pub model: Option<String>,
    /// Per-camera accelerator assignment: force this camera's detector onto CPU
    /// (`Some(true)`) or the GPU (`Some(false)`); `None` inherits the global
    /// setting. Useful to keep a low-priority camera off a busy GPU.
    pub force_cpu: Option<bool>,
    /// Per-camera sample interval cap in ms (resource governance / FPS cap);
    /// `None` uses the global poll interval. Only ever slows a camera down.
    pub poll_ms: Option<u64>,
    /// Per-camera face-recognition opt-in: `Some(true/false)` overrides the
    /// global switch, `None` inherits it. Lets you enable face matching only on
    /// the cameras where it's wanted (e.g. the front door).
    pub face_recognize: Option<bool>,
    /// Two-way audio (push-to-talk): when true, the camera detail view offers a
    /// hold-to-talk button that streams the browser mic to the camera over
    /// WebRTC (go2rtc backchannel). Opt-in because it only works on cameras with
    /// a speaker / ONVIF backchannel — purely a UI gate; the audio path is the
    /// player's WebRTC mic track through the `/api/ws` proxy.
    pub two_way_audio: bool,
    /// Per-camera recording RETENTION override in days (UniFi-style: keep the
    /// doorbell 30d, a quiet side camera 3d). `None` inherits the global
    /// `Settings.retention_days`. The global byte cap still applies as the
    /// disk-bound safety net, so this only tightens/extends *age*, never the
    /// total-disk guarantee.
    #[serde(default)]
    pub retention_days: Option<u32>,
    /// Residential ASSISTIVE fall hint: when true, a tracked person who goes
    /// motionless low in the frame fires a `fall` event. Best-effort at ~1 fps —
    /// misses occluded / soft / slow falls. NOT a medical-alert device; pair with
    /// a pendant. See `residential.rs` + `docs/05`.
    #[serde(default)]
    pub fall_detect: bool,
    /// Residential child/adult calibration: a tracked person whose normalized
    /// bbox height is at/below this fraction is treated as a "child". `None`
    /// disables all child features on this camera (the default). FRAGILE without
    /// per-camera setup — bbox height depends on the camera angle/distance.
    #[serde(default)]
    pub child_height_frac: Option<f32>,
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
    /// Recognized hand signal (e.g. "open_palm", "victory"), when the event
    /// came from the hand-signal recognizer.
    pub gesture: Option<String>,
    /// Name of the detection zone the object was in, when it fell inside a
    /// named polygon zone (used for review filtering).
    pub zone: Option<String>,
    /// Natural-language description from the optional GenAI captioner.
    pub caption: Option<String>,
    /// Speech-to-text of the event's audio, from the optional (bundled, opt-in)
    /// transcriber — set for audio events when transcription finds speech.
    pub transcript: Option<String>,
    /// User bookmark: a flagged event is kept in the Events review filter and is
    /// exempt from the event-retention auto-prune (its clip is "protected").
    #[serde(default)]
    pub flagged: bool,
    /// Free-text note the user attached to the event.
    #[serde(default)]
    pub note: Option<String>,
    /// Anomaly score (0..1) from the anomaly-detection worker; None = unscored.
    #[serde(default)]
    pub anomaly_score: Option<f32>,
    /// Line-crossing direction for a `crossing` event: `"a_to_b"` / `"b_to_a"`.
    #[serde(default)]
    pub direction: Option<String>,
    /// Estimated ground speed (km/h) on a calibrated crossing event.
    #[serde(default)]
    pub speed: Option<f32>,
}

/// One row of the smart-search corpus: an event's id, its searchable text
/// (transcript + caption) and its optional CLIP snapshot embedding.
pub struct SearchRow {
    pub id: i64,
    pub transcript: Option<String>,
    pub caption: Option<String>,
    pub embedding: Option<Vec<f32>>,
}

/// A named API access token for headless/automation callers. The raw token is
/// only ever shown once at creation; only its hash is stored, so this struct
/// never carries the secret.
#[derive(Clone, Debug, Serialize)]
pub struct ApiToken {
    pub id: i64,
    pub name: String,
    pub role: String,
    pub created_ts: i64,
    pub last_used_ts: Option<i64>,
}

/// One security-audit entry: a notable security event (login, password change,
/// token create/revoke) with when, the client IP, and a short detail.
#[derive(Clone, Debug, Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub ts: i64,
    pub ip: Option<String>,
    pub action: String,
    pub detail: Option<String>,
}

/// One in-app notification (A4 notifications center): a security/activity event
/// surfaced in the rail bell + notifications panel. Self-trimmed so it stays
/// bounded. `event_id` deep-links to the originating event when there is one.
#[derive(Clone, Debug, Serialize)]
pub struct Notification {
    pub id: i64,
    pub ts: i64,
    /// "stranger" | "camera_offline" | "camera_online" | "digest" | "anomaly" | ...
    pub kind: String,
    pub title: String,
    pub body: Option<String>,
    pub event_id: Option<i64>,
    pub read: bool,
}

/// One AI daily digest (B1): a natural-language recap of a period's activity.
#[derive(Clone, Debug, Serialize)]
pub struct Digest {
    pub id: i64,
    pub ts: i64,
    pub text: String,
}

/// A named user account (C5 multi-user roles). The password hash is never
/// serialized out of the API.
#[derive(Clone, Debug, Serialize)]
pub struct UserRow {
    pub id: i64,
    pub username: String,
    pub role: String,
    pub created_ts: i64,
}

/// Outcome of a guarded role change (keeps the last-admin check + update atomic).
pub enum SetRole {
    Ok,
    NotFound,
    LastAdmin,
}

/// Outcome of a guarded user delete.
pub enum DeleteUser {
    Deleted,
    NotFound,
    LastAdmin,
}

/// A saved named camera layout (A6 Liveviews), persisted in `Settings`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Liveview {
    pub name: String,
    #[serde(default)]
    pub cameras: Vec<String>,
}

/// Sentinel stored in an event's `face` when a face was detected on a person
/// but matched no enrolled identity — a "stranger". Distinguishes that from
/// "no face detected" (`None`); kept short and reserved so it can't be confused
/// with a real enrolled name.
pub const UNKNOWN_FACE: &str = "?";

/// One action a rule fires. A rule can fire several at once (a "scene"): e.g.
/// push to your phone AND POST a webhook AND email a snapshot. `kind` is
/// "webhook" | "mqtt" | "ntfy" | "email"; `target` is the URL / MQTT topic
/// suffix / ntfy topic / email recipient (blank email target = the default
/// `smtp_to` from Settings).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Action {
    pub kind: String,
    #[serde(default)]
    pub target: String,
    /// ntfy/push priority 1..5; 0 = channel default. Ignored by other kinds.
    #[serde(default)]
    pub priority: u8,
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
    /// Match on the recognized hand signal (exact canonical name, e.g.
    /// "open_palm"). Lets a held gesture arm a webhook/ntfy/MQTT action —
    /// a silent "panic" hand signal at the door, for instance.
    #[serde(default)]
    pub gesture_like: Option<String>,
    /// Substring match (case-insensitive) on an event's speech-to-text
    /// transcript — fire when a phrase is *said* near the camera, e.g. a spoken
    /// safe word "help"/"fire". Evaluated only for transcribed audio events.
    #[serde(default)]
    pub transcript_like: Option<String>,
    /// Fire only when a person is seen whose face was detected but did NOT match
    /// any enrolled identity — a "stranger"/unfamiliar-face alert (the event's
    /// face is the `UNKNOWN_FACE` sentinel). Mutually exclusive with `face_like`.
    #[serde(default)]
    pub face_unknown: bool,
    /// Substring match (case-insensitive) on the event's detection ZONE name —
    /// fire only when the object is in a named zone, e.g. `zone_like = "Pool"`.
    /// Combined with a `label` this expresses the residential primitives
    /// "person in the Pool zone", "dog on the Couch zone". `None` = any zone.
    #[serde(default)]
    pub zone_like: Option<String>,
    #[serde(default)]
    pub min_score: f32,
    /// Legacy single action: "webhook" / "mqtt" / "ntfy". Superseded by
    /// `actions` (a rule can now fire several). Kept in sync with `actions[0]`
    /// for back-compat, so older builds still read a usable rule.
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub target: String,
    /// Arming schedule (Blue Iris-style): days of week the rule is armed,
    /// 0 = Sunday .. 6 = Saturday; empty = every day.
    #[serde(default)]
    pub days: Vec<u8>,
    /// Arming window start/end as "HH:MM" local time; both unset = all day.
    /// end < start spans midnight (e.g. 22:00–06:00).
    #[serde(default)]
    pub start_hhmm: Option<String>,
    #[serde(default)]
    pub end_hhmm: Option<String>,
    /// Minimum seconds between firings of this rule — the per-rule anti-fatigue
    /// throttle. 0 = no cooldown.
    #[serde(default)]
    pub cooldown_secs: i64,
    /// ntfy priority 1 (min) .. 5 (max); 0 = leave at the ntfy default (3).
    #[serde(default)]
    pub priority: u8,
    /// Suppress the rule until this unix timestamp (manual "snooze"). 0 = off.
    #[serde(default)]
    pub snooze_until: i64,
    #[serde(default)]
    pub created_ts: i64,
    /// Arm modes this rule is active in (e.g. ["away"]). Empty = active in every
    /// armed mode (home + away) but suppressed when the system is "disarmed". A
    /// rule that lists "disarmed" still fires while disarmed (a panic rule).
    /// See `notify::armed_in_mode`.
    #[serde(default)]
    pub modes: Vec<String>,
    /// Actions fired when the rule matches — a "scene" (e.g. push + webhook +
    /// email at once). Empty falls back to the legacy single `action`/`target`.
    #[serde(default)]
    pub actions: Vec<Action>,
}

fn default_true() -> bool {
    true
}

fn parse_hhmm(s: &str) -> Option<u16> {
    let (h, m) = s.split_once(':')?;
    let (h, m): (u16, u16) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

impl AlarmRule {
    /// The rule's action list, always non-empty: the explicit `actions` scene
    /// if set, otherwise a single Action synthesized from the legacy
    /// `action`/`target`/`priority` fields. Used for both persistence and firing.
    pub fn effective_actions(&self) -> Vec<Action> {
        if self.actions.is_empty() {
            vec![Action {
                kind: self.action.clone(),
                target: self.target.clone(),
                priority: self.priority,
            }]
        } else {
            self.actions.clone()
        }
    }

    /// Is the rule armed on this weekday (0 = Sunday) at this minute of day?
    pub fn armed_at(&self, weekday: u8, minute: u16) -> bool {
        if !self.days.is_empty() && !self.days.contains(&weekday) {
            return false;
        }
        let start = self.start_hhmm.as_deref().and_then(parse_hhmm);
        let end = self.end_hhmm.as_deref().and_then(parse_hhmm);
        match (start, end) {
            (None, None) => true,
            (Some(s), None) => minute >= s,
            (None, Some(e)) => minute <= e,
            (Some(s), Some(e)) if s <= e => minute >= s && minute <= e,
            // Overnight window, e.g. 22:00–06:00.
            (Some(s), Some(e)) => minute >= s || minute <= e,
        }
    }

    fn armed_now(&self) -> bool {
        use chrono::{Datelike as _, Timelike as _};
        let now = chrono::Local::now();
        self.armed_at(
            now.weekday().num_days_from_sunday() as u8,
            (now.hour() * 60 + now.minute()) as u16,
        )
    }

    /// Does the event's detection zone satisfy this rule's optional `zone_like`
    /// filter? `None`/blank matches anything; otherwise a case-insensitive
    /// substring match on the event's zone (an event with no zone never matches a
    /// zone-scoped rule). Checked alongside [`AlarmRule::matches`] at every call
    /// site so a zone-scoped residential rule (e.g. "person in Pool") fires only
    /// in its zone. Kept separate from `matches` to avoid churning its many
    /// call sites; AND the two together.
    pub fn zone_ok(&self, zone: Option<&str>) -> bool {
        match self
            .zone_like
            .as_deref()
            .map(str::trim)
            .filter(|z| !z.is_empty())
        {
            None => true,
            Some(want) => zone
                .map(|z| z.to_lowercase().contains(&want.to_lowercase()))
                .unwrap_or(false),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn matches(
        &self,
        camera_id: i64,
        label: &str,
        score: f32,
        face: Option<&str>,
        plate: Option<&str>,
        gesture: Option<&str>,
        transcript: Option<&str>,
    ) -> bool {
        if !self.enabled || score < self.min_score {
            return false;
        }
        if !self.armed_now() {
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
        // Stranger condition: only an unrecognized-face event (face sentinel).
        if self.face_unknown && face != Some(UNKNOWN_FACE) {
            return false;
        }
        if let Some(p) = self.plate_like.as_deref() {
            let hit = plate
                .map(|v| v.to_uppercase().contains(&p.to_uppercase()))
                .unwrap_or(false);
            if !hit {
                return false;
            }
        }
        if let Some(g) = self.gesture_like.as_deref() {
            let want = g.to_lowercase();
            let hit = gesture
                .map(|v| v.eq_ignore_ascii_case(&want))
                .unwrap_or(false);
            if !hit {
                return false;
            }
        }
        // An empty/whitespace phrase is treated as no condition (it would
        // otherwise substring-match every transcript).
        if let Some(phrase) = self
            .transcript_like
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            let hit = transcript
                .map(|v| v.to_lowercase().contains(&phrase.to_lowercase()))
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

/// A named entry in the license-plate library (the vehicle analog of an enrolled
/// face). `category` is "known" (an expected vehicle) or "watch" (a vehicle of
/// interest that fires an alert when seen).
#[derive(Clone, Debug, Serialize)]
pub struct PlateRow {
    pub id: i64,
    pub plate: String,
    pub name: String,
    pub category: String,
    pub note: Option<String>,
    pub created_ts: i64,
}

/// Canonical plate key: uppercase, alphanumerics only (drop spaces/dashes), so
/// "ab-12 34" and "AB1234" match the same library entry and OCR spacing noise
/// doesn't matter.
pub fn normalize_plate(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_uppercase())
        .collect()
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
    /// Enhanced retention (UniFi-style): segments older than this many days
    /// are re-encoded to space-saving quality. 0 = off.
    pub enhanced_retention_days: u32,
    /// Hardware video encoder for the enhanced-retention re-encode: "" / "cpu"
    /// (libx264), "nvenc" (NVIDIA), "qsv" (Intel QuickSync), or "videotoolbox"
    /// (Apple). Falls back to CPU automatically if the HW encoder fails.
    pub hwaccel: String,
    /// Where new recordings go (any drive or UNC share); empty = the default
    /// data/recordings. Existing segments keep playing from where they are.
    pub recordings_dir: String,
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
    /// Publish Home Assistant MQTT-discovery configs so HA auto-creates a
    /// binary_sensor per (camera, object) and a last-detection sensor per camera.
    pub mqtt_ha_discovery: bool,
    /// HA discovery topic prefix (HA's default is "homeassistant").
    pub mqtt_ha_prefix: String,
    /// Seconds a discovery binary_sensor stays "ON" after a detection before it
    /// is auto-cleared to "OFF".
    pub mqtt_state_timeout_secs: u64,
    /// Optional webhook body template. Empty = the default detection JSON.
    /// Placeholders: {{event_id}} {{camera}} {{label}} {{score}} {{ts}}
    /// {{snapshot}} {{face}} {{plate}} {{gesture}} (unknowns render empty).
    pub webhook_template: String,
    /// Run face recognition on person detections (needs the two face models
    /// on disk; silently inactive when they are missing).
    pub face_recognition: bool,
    /// Cosine similarity needed to call a face a known person (ArcFace
    /// same-person scores typically land 0.4-0.7).
    pub face_match_threshold: f32,
    pub face_det_model: String,
    pub face_rec_model: String,
    /// License plates of interest (substring match, case-insensitive). A read
    /// that matches fires a guaranteed high-priority "vehicle of interest" push.
    pub plate_denylist: Vec<String>,
    /// Known/expected plates (substring match) — surfaced as "known" in review.
    pub plate_allowlist: Vec<String>,
    /// AudioSet display names (yamnet_class_map.csv) that produce events.
    pub audio_labels: Vec<String>,
    /// Mean YAMNet score required to fire an audio event.
    pub audio_threshold: f32,
    /// ntfy topic URL for camera health pushes (offline / back online);
    /// empty = off.
    pub health_ntfy_url: String,
    /// Public base URL this NVR is reachable at (e.g. "https://nvr.example.com").
    /// When set, push notifications include tap-through links to the event clip
    /// and snapshot. Empty = no links (the LAN default).
    pub public_base_url: String,
    /// Master switch for the live hand-signal recognizer (the Signals page).
    pub gesture_recognition: bool,
    /// How long (seconds) a hand signal must be held before it fires an event —
    /// debounces accidental poses.
    pub gesture_hold_secs: f32,
    /// Canonical gesture names that produce events (see the `gesture` crate's
    /// taxonomy). Empty = every recognized signal.
    pub gesture_labels: Vec<String>,
    /// A "duress"/help hand signal. When this signal is recognized, the gesture
    /// event is flagged high-priority and pushes go out at max urgency with a
    /// distinct tag — a silent panic button. Empty = no duress signal.
    pub gesture_duress: String,
    /// MediaPipe gesture-recognizer task bundle the browser loads. Defaults to
    /// Google's CDN; point it at a self-hosted copy for fully offline use.
    pub gesture_model_url: String,
    /// Explicit opt-in for GenAI event captions. OFF by default — nothing is
    /// ever sent to an LLM until this is enabled. With a localhost Ollama URL it
    /// stays fully local; pointing it at a cloud endpoint sends snapshots there.
    pub genai_enabled: bool,
    /// Ollama-compatible generate endpoint (default local Ollama).
    pub genai_url: String,
    /// Vision model used for captioning (e.g. "llava", "llama3.2-vision").
    pub genai_model: String,
    /// Optional bearer token (for cloud/proxied endpoints). Empty for local Ollama.
    pub genai_api_key: String,
    /// Opt-in speech-to-text of audio events, using the bundled (compiled-in)
    /// whisper.cpp engine — nothing leaves the machine. Off by default.
    pub transcription_enabled: bool,
    /// Path to the whisper GGML model (downloaded, not committed), e.g.
    /// `ggml-tiny.en.bin` (~75 MB) or `ggml-base.en.bin`.
    pub transcription_model: String,
    /// B3: master switch for the anomaly-detection worker (opt-in).
    #[serde(default)]
    pub anomaly_detection: bool,
    /// B1: master switch for the daily AI digest worker (opt-in).
    #[serde(default)]
    pub digest_enabled: bool,
    /// A6: saved named camera layouts ("Liveviews") for the Live wall.
    #[serde(default)]
    pub liveviews: Vec<Liveview>,
    /// C6: floor-plan camera map as JSON ({ image, pins:[{camera,x,y}] }); empty = none.
    #[serde(default)]
    pub floorplan: String,
    /// System security mode (UniFi-style): "home" | "away" | "disarmed". Gates
    /// which alarm rules fire — see `notify::armed_in_mode`.
    #[serde(default = "default_arm_mode")]
    pub arm_mode: String,
    /// SMTP for the "email" alarm action. `smtp_url` is "smtps://host:465"
    /// (implicit TLS) / "smtp://host:587" (STARTTLS) / "host[:port]"; creds go in
    /// `smtp_user`/`smtp_pass`. `smtp_pass` is write-only — blanked in
    /// GET /api/settings (Viewer-reachable) and preserved on a blank save.
    #[serde(default)]
    pub smtp_url: String,
    #[serde(default)]
    pub smtp_user: String,
    #[serde(default)]
    pub smtp_pass: String,
    #[serde(default)]
    pub smtp_from: String,
    /// Default recipient(s) (comma-separated) for email actions whose target is blank.
    #[serde(default)]
    pub smtp_to: String,
}

fn default_arm_mode() -> String {
    "away".into()
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
            enhanced_retention_days: 0,
            hwaccel: String::new(),
            recordings_dir: String::new(),
            model_path: "yolov8n.onnx".into(),
            force_cpu: false,
            go2rtc_api_port: 1984,
            webhook_url: String::new(),
            record_audio: false,
            alert_labels: ["person"].map(String::from).to_vec(),
            mqtt_url: String::new(),
            mqtt_prefix: "zoomy".into(),
            mqtt_ha_discovery: true,
            mqtt_ha_prefix: "homeassistant".into(),
            mqtt_state_timeout_secs: 30,
            webhook_template: String::new(),
            face_recognition: true,
            face_match_threshold: 0.4,
            face_det_model: "det_10g.onnx".into(),
            face_rec_model: "w600k_r50.onnx".into(),
            audio_labels: [
                "Glass",
                "Shatter",
                "Gunshot, gunfire",
                "Screaming",
                "Smoke detector, smoke alarm",
                "Fire alarm",
                "Siren",
                "Car alarm",
                "Alarm",
                "Bark",
                "Doorbell",
                "Knock",
                "Baby cry, infant cry",
            ]
            .map(String::from)
            .to_vec(),
            audio_threshold: 0.4,
            plate_denylist: Vec::new(),
            plate_allowlist: Vec::new(),
            health_ntfy_url: String::new(),
            public_base_url: String::new(),
            gesture_recognition: true,
            gesture_hold_secs: 1.5,
            gesture_labels: ["open_palm", "victory", "thumb_up"]
                .map(String::from)
                .to_vec(),
            gesture_duress: String::new(),
            gesture_model_url: "https://storage.googleapis.com/mediapipe-models/\
                gesture_recognizer/gesture_recognizer/float16/1/gesture_recognizer.task"
                .into(),
            genai_enabled: false,
            genai_url: "http://localhost:11434/api/generate".into(),
            genai_model: "llava".into(),
            genai_api_key: String::new(),
            transcription_enabled: false,
            transcription_model: "ggml-tiny.en.bin".into(),
            anomaly_detection: false,
            digest_enabled: false,
            liveviews: Vec::new(),
            floorplan: String::new(),
            arm_mode: default_arm_mode(),
            smtp_url: String::new(),
            smtp_user: String::new(),
            smtp_pass: String::new(),
            smtp_from: String::new(),
            smtp_to: String::new(),
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
        // `group` is a SQL reserved word, so the column is `group_name`.
        let _ = conn.execute("ALTER TABLE cameras ADD COLUMN group_name TEXT", []);
        let _ = conn.execute("ALTER TABLE events ADD COLUMN face TEXT", []);
        let _ = conn.execute("ALTER TABLE events ADD COLUMN plate TEXT", []);
        let _ = conn.execute("ALTER TABLE events ADD COLUMN gesture TEXT", []);
        let _ = conn.execute("ALTER TABLE events ADD COLUMN zone TEXT", []);
        let _ = conn.execute("ALTER TABLE events ADD COLUMN caption TEXT", []);
        let _ = conn.execute("ALTER TABLE events ADD COLUMN transcript TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE events ADD COLUMN flagged INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute("ALTER TABLE events ADD COLUMN note TEXT", []);
        // B3: anomaly score (0..1) written by the anomaly-detection worker.
        let _ = conn.execute("ALTER TABLE events ADD COLUMN anomaly_score REAL", []);
        // Line-crossing direction ("a_to_b"/"b_to_a") on tracker `crossing` events.
        let _ = conn.execute("ALTER TABLE events ADD COLUMN direction TEXT", []);
        // Estimated ground speed (km/h) on a crossing, when the camera is calibrated.
        let _ = conn.execute("ALTER TABLE events ADD COLUMN speed REAL", []);
        let _ = conn.execute(
            "ALTER TABLE segments ADD COLUMN reduced INTEGER NOT NULL DEFAULT 0",
            [],
        );
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
             CREATE TABLE IF NOT EXISTS plates (
                 id         INTEGER PRIMARY KEY,
                 plate      TEXT NOT NULL UNIQUE,
                 name       TEXT NOT NULL,
                 category   TEXT NOT NULL DEFAULT 'known',
                 note       TEXT,
                 created_ts INTEGER NOT NULL
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
             );
             CREATE TABLE IF NOT EXISTS api_tokens (
                 id           INTEGER PRIMARY KEY,
                 name         TEXT NOT NULL,
                 token_hash   TEXT NOT NULL UNIQUE,
                 created_ts   INTEGER NOT NULL,
                 last_used_ts INTEGER
             );
             CREATE TABLE IF NOT EXISTS audit_log (
                 id     INTEGER PRIMARY KEY,
                 ts     INTEGER NOT NULL,
                 ip     TEXT,
                 action TEXT NOT NULL,
                 detail TEXT
             );
             CREATE TABLE IF NOT EXISTS notifications (
                 id       INTEGER PRIMARY KEY,
                 ts       INTEGER NOT NULL,
                 kind     TEXT NOT NULL,
                 title    TEXT NOT NULL,
                 body     TEXT,
                 event_id INTEGER REFERENCES events(id) ON DELETE SET NULL,
                 read     INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS notifications_id ON notifications(id DESC);
             CREATE TABLE IF NOT EXISTS digests (
                 id   INTEGER PRIMARY KEY,
                 ts   INTEGER NOT NULL,
                 text TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS users (
                 id            INTEGER PRIMARY KEY,
                 username      TEXT NOT NULL UNIQUE,
                 password_hash TEXT NOT NULL,
                 role          TEXT NOT NULL DEFAULT 'viewer',
                 created_ts    INTEGER NOT NULL
             );",
        )?;
        // CLIP embedding of the object CROP (not the full frame) for cross-camera
        // appearance search / Re-ID. Nullable; only set for object detections.
        // Must run AFTER the CREATE TABLE batch above (the table is created here).
        let _ = conn.execute(
            "ALTER TABLE event_embeddings ADD COLUMN crop_embedding BLOB",
            [],
        );
        // Additive migration for pre-schedule alarms tables.
        let _ = conn.execute("ALTER TABLE alarms ADD COLUMN schedule_json TEXT", []);
        let _ = conn.execute("ALTER TABLE alarms ADD COLUMN gesture_like TEXT", []);
        let _ = conn.execute("ALTER TABLE alarms ADD COLUMN transcript_like TEXT", []);
        // Multi-action "scenes": the full Vec<Action> as JSON. NULL on legacy
        // rows -> list_alarms synthesizes one Action from the legacy columns.
        let _ = conn.execute("ALTER TABLE alarms ADD COLUMN actions_json TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE alarms ADD COLUMN face_unknown INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE alarms ADD COLUMN cooldown_secs INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE alarms ADD COLUMN priority INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE alarms ADD COLUMN snooze_until INTEGER NOT NULL DEFAULT 0",
            [],
        );
        // Scoped API tokens (C5 follow-up): existing tokens default to 'admin'
        // so they keep their pre-roles behaviour; new tokens pick a role.
        let _ = conn.execute(
            "ALTER TABLE api_tokens ADD COLUMN role TEXT NOT NULL DEFAULT 'admin'",
            [],
        );
        Ok(Self(Arc::new(Mutex::new(conn))))
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0.lock().expect("db mutex poisoned")
    }

    // --- cameras ---------------------------------------------------------

    pub fn list_cameras(&self) -> Result<Vec<Camera>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, source, enabled, detect, record, created_ts, detect_json, detect_source, group_name
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
                "SELECT id, name, source, enabled, detect, record, created_ts, detect_json, detect_source, group_name
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
            group: None,
        })
    }

    pub fn update_camera(&self, cam: &Camera) -> Result<()> {
        let detect_json = serde_json::to_string(&cam.detect_config)?;
        self.conn().execute(
            "UPDATE cameras SET name=?1, source=?2, enabled=?3, detect=?4, record=?5,
             detect_json=?6, detect_source=?7, group_name=?8 WHERE id=?9",
            params![
                cam.name,
                cam.source,
                cam.enabled,
                cam.detect,
                cam.record,
                detect_json,
                cam.detect_source,
                cam.group,
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
        gesture: Option<&str>,
        zone: Option<&str>,
    ) -> Result<i64> {
        self.add_event_dir(
            camera_id, ts, label, score, bbox, snapshot, face, plate, gesture, zone, None, None,
        )
    }

    /// Like [`add_event`](Self::add_event) but also records a line-crossing
    /// `direction` and an estimated `speed` (km/h) — used by the tracker-driven
    /// analytics for `crossing` / `wrong_way` events.
    #[allow(clippy::too_many_arguments)]
    pub fn add_event_dir(
        &self,
        camera_id: i64,
        ts: i64,
        label: &str,
        score: f32,
        bbox: [f32; 4],
        snapshot: Option<&str>,
        face: Option<&str>,
        plate: Option<&str>,
        gesture: Option<&str>,
        zone: Option<&str>,
        direction: Option<&str>,
        speed: Option<f32>,
    ) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO events (camera_id, ts, label, score, x1, y1, x2, y2, snapshot, face, plate, gesture, zone, direction, speed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                camera_id, ts, label, score, bbox[0], bbox[1], bbox[2], bbox[3], snapshot, face,
                plate, gesture, zone, direction, speed
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn list_events(
        &self,
        camera_id: Option<i64>,
        label: Option<&str>,
        gesture: Option<&str>,
        zone: Option<&str>,
        after_ts: Option<i64>,
        before_ts: Option<i64>,
        flagged_only: bool,
        limit: u32,
    ) -> Result<Vec<Event>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT e.id, e.camera_id, c.name, e.ts, e.label, e.score,
                    e.x1, e.y1, e.x2, e.y2, e.snapshot, e.face, e.plate, e.gesture, e.zone, e.caption, e.transcript,
                    e.flagged, e.note, e.anomaly_score, e.direction, e.speed
             FROM events e JOIN cameras c ON c.id = e.camera_id
             WHERE (?1 IS NULL OR e.camera_id = ?1)
               AND (?2 IS NULL OR e.label = ?2)
               AND (?3 IS NULL OR e.gesture = ?3)
               AND (?4 IS NULL OR e.zone = ?4)
               AND (?5 IS NULL OR e.ts >= ?5)
               AND (?6 IS NULL OR e.ts < ?6)
               AND (?7 = 0 OR e.flagged = 1)
             ORDER BY e.ts DESC, e.id DESC LIMIT ?8",
        )?;
        let rows = stmt
            .query_map(
                params![
                    camera_id,
                    label,
                    gesture,
                    zone,
                    after_ts,
                    before_ts,
                    flagged_only as i64,
                    limit
                ],
                row_to_event,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_event(&self, id: i64) -> Result<Option<Event>> {
        let conn = self.conn();
        let ev = conn
            .query_row(
                "SELECT e.id, e.camera_id, c.name, e.ts, e.label, e.score,
                        e.x1, e.y1, e.x2, e.y2, e.snapshot, e.face, e.plate, e.gesture, e.zone, e.caption, e.transcript,
                        e.flagged, e.note, e.anomaly_score, e.direction, e.speed
                 FROM events e JOIN cameras c ON c.id = e.camera_id WHERE e.id = ?1",
                [id],
                row_to_event,
            )
            .optional()?;
        Ok(ev)
    }

    /// Tracker-analytics roll-up over `crossing`/`loiter` events in a time range
    /// (unix secs; `None` = unbounded). Crossings are de-duplicated by the
    /// tracker (one physical pass = one event), so these are *true* throughput
    /// counts, not the cooldown-inflated detection tallies. Grouped by tripwire
    /// + direction (crossings) and by zone (loiters).
    pub fn analytics_counts(
        &self,
        from: Option<i64>,
        to: Option<i64>,
    ) -> Result<serde_json::Value> {
        let conn = self.conn();
        // Count both normal and wrong-way crossings: a wrong-way pass is still a
        // physical pass through the line, so it must count toward throughput
        // (it also carries a real `direction`). Excluding it under-reports.
        let mut cs = conn.prepare(
            "SELECT zone, direction, COUNT(*) FROM events
             WHERE label IN ('crossing', 'wrong_way') AND (?1 IS NULL OR ts >= ?1) AND (?2 IS NULL OR ts < ?2)
             GROUP BY zone, direction ORDER BY zone, direction",
        )?;
        let crossings: Vec<serde_json::Value> = cs
            .query_map(params![from, to], |r| {
                Ok(serde_json::json!({
                    "tripwire": r.get::<_, Option<String>>(0)?,
                    "direction": r.get::<_, Option<String>>(1)?,
                    "count": r.get::<_, i64>(2)?,
                }))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut ls = conn.prepare(
            "SELECT zone, COUNT(*) FROM events
             WHERE label = 'loiter' AND (?1 IS NULL OR ts >= ?1) AND (?2 IS NULL OR ts < ?2)
             GROUP BY zone ORDER BY zone",
        )?;
        let loiters: Vec<serde_json::Value> = ls
            .query_map(params![from, to], |r| {
                Ok(serde_json::json!({
                    "zone": r.get::<_, Option<String>>(0)?,
                    "count": r.get::<_, i64>(1)?,
                }))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(serde_json::json!({ "crossings": crossings, "loiters": loiters }))
    }

    /// Accumulate object-detection footprints into a `grid`×`grid` activity
    /// density map for a camera over an optional time range (row-major, length
    /// `grid*grid`). Each qualifying event contributes its ground-anchor
    /// (bottom-centre of the box) to one cell — a "foot-traffic" heatmap of where
    /// objects actually stood. Synthetic analytics-marker events
    /// (crossing/wrong_way/loiter/occupancy/gesture) and degenerate (zero-area)
    /// boxes — e.g. audio events — are excluded so the map reflects real object
    /// presence. `grid` is clamped to a sane range.
    pub fn heatmap(
        &self,
        camera_id: i64,
        from: Option<i64>,
        to: Option<i64>,
        grid: usize,
    ) -> Result<Vec<u32>> {
        let grid = grid.clamp(8, 128);
        let mut cells = vec![0u32; grid * grid];
        let conn = self.conn();
        let mut q = conn.prepare(
            "SELECT x1, y1, x2, y2 FROM events
             WHERE camera_id = ?1
               AND (?2 IS NULL OR ts >= ?2) AND (?3 IS NULL OR ts < ?3)
               AND label NOT IN ('crossing', 'wrong_way', 'loiter', 'occupancy', 'gesture')",
        )?;
        let rows = q.query_map(params![camera_id, from, to], |r| {
            Ok((
                r.get::<_, f64>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
            ))
        })?;
        let g = grid as f64;
        for row in rows {
            let (x1, y1, x2, y2) = row?;
            // Skip degenerate boxes (audio / synthetic events with no real area)
            // and any box not in 0..1 frame fractions — i.e. legacy rows written
            // before detection boxes were normalised, which hold raw pixel coords
            // and would otherwise all collapse into the bottom-right cell.
            if x2 <= x1 || y2 <= y1 || x1 < 0.0 || y1 < 0.0 || x2 > 1.0 || y2 > 1.0 {
                continue;
            }
            // Ground-contact anchor: bottom-centre of the box.
            let ax = ((x1 + x2) / 2.0).clamp(0.0, 0.999_999);
            let ay = y2.clamp(0.0, 0.999_999);
            let cx = (ax * g) as usize;
            let cy = (ay * g) as usize;
            cells[cy * grid + cx] += 1;
        }
        Ok(cells)
    }

    // --- alarms --------------------------------------------------------------

    pub fn add_alarm(&self, r: &AlarmRule) -> Result<i64> {
        let schedule = serde_json::json!({
            "days": r.days, "start": r.start_hhmm, "end": r.end_hhmm, "modes": r.modes,
            // Residential zone scope rides the schedule blob (no migration), like modes.
            "zone_like": r.zone_like
        })
        .to_string();
        // Persist the full scene, and dual-write the legacy action/target/priority
        // columns from action[0] so an older build still reads (a degraded) rule.
        let actions = r.effective_actions();
        let actions_json = serde_json::to_string(&actions).unwrap_or_else(|_| "[]".into());
        let primary = &actions[0];
        let conn = self.conn();
        conn.execute(
            "INSERT INTO alarms (name, enabled, camera_id, label, face_like, plate_like,
             gesture_like, min_score, action, target, schedule_json, cooldown_secs, priority,
             snooze_until, created_ts, transcript_like, face_unknown, actions_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                r.name,
                r.enabled,
                r.camera_id,
                r.label,
                r.face_like,
                r.plate_like,
                r.gesture_like,
                r.min_score,
                primary.kind,
                primary.target,
                schedule,
                r.cooldown_secs,
                primary.priority,
                r.snooze_until,
                chrono::Local::now().timestamp(),
                r.transcript_like,
                r.face_unknown as i64,
                actions_json
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_alarms(&self) -> Result<Vec<AlarmRule>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, enabled, camera_id, label, face_like, plate_like,
                    min_score, action, target, created_ts, schedule_json, gesture_like,
                    cooldown_secs, priority, snooze_until, transcript_like, face_unknown,
                    actions_json
             FROM alarms ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                let schedule: Option<String> = r.get(11)?;
                let sched: serde_json::Value = schedule
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or(serde_json::Value::Null);
                let action: String = r.get(8)?;
                let target: String = r.get(9)?;
                let priority = r.get::<_, i64>(14)? as u8;
                // Use the persisted scene; fall back to a one-element scene
                // synthesized from the legacy columns for pre-scenes rows.
                let actions_json: Option<String> = r.get(18)?;
                let actions: Vec<Action> = actions_json
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Vec<Action>>(s).ok())
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| {
                        vec![Action {
                            kind: action.clone(),
                            target: target.clone(),
                            priority,
                        }]
                    });
                let modes: Vec<String> = sched["modes"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(AlarmRule {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    enabled: r.get::<_, i64>(2)? != 0,
                    camera_id: r.get(3)?,
                    label: r.get(4)?,
                    face_like: r.get(5)?,
                    plate_like: r.get(6)?,
                    gesture_like: r.get(12)?,
                    zone_like: sched["zone_like"].as_str().map(str::to_string),
                    min_score: r.get(7)?,
                    action,
                    target,
                    days: sched["days"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_u64())
                                .map(|v| v as u8)
                                .collect()
                        })
                        .unwrap_or_default(),
                    start_hhmm: sched["start"].as_str().map(str::to_string),
                    end_hhmm: sched["end"].as_str().map(str::to_string),
                    cooldown_secs: r.get(13)?,
                    priority,
                    snooze_until: r.get(15)?,
                    transcript_like: r.get(16)?,
                    face_unknown: r.get::<_, i64>(17)? != 0,
                    created_ts: r.get(10)?,
                    modes,
                    actions,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Segments of `camera_id` starting before `older_than` with no event
    /// within `margin` seconds of the segment's span — the deletion set for
    /// event-only recording retention.
    pub fn eventless_segments(
        &self,
        camera_id: i64,
        older_than: i64,
        span_secs: i64,
        margin: i64,
    ) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT s.path FROM segments s
             WHERE s.camera_id = ?1 AND s.start_ts < ?2
               AND NOT EXISTS (
                 SELECT 1 FROM events e
                 WHERE e.camera_id = s.camera_id
                   AND e.ts BETWEEN s.start_ts - ?4 AND s.start_ts + ?3 + ?4
               )",
        )?;
        let rows = stmt
            .query_map(params![camera_id, older_than, span_secs, margin], |r| {
                r.get(0)
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

    /// Suppress a rule until `until` (unix seconds); 0 clears the snooze.
    pub fn set_alarm_snooze(&self, id: i64, until: i64) -> Result<()> {
        self.conn().execute(
            "UPDATE alarms SET snooze_until=?1 WHERE id=?2",
            params![until, id],
        )?;
        Ok(())
    }

    // --- API tokens --------------------------------------------------------

    /// Store a new API token (only its hash) and return its row id.
    pub fn add_api_token(&self, name: &str, token_hash: &str, role: &str, now: i64) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO api_tokens (name, token_hash, role, created_ts) VALUES (?1, ?2, ?3, ?4)",
            params![name, token_hash, role, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// List tokens (metadata only — never the hash or the secret).
    pub fn list_api_tokens(&self) -> Result<Vec<ApiToken>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, role, created_ts, last_used_ts FROM api_tokens ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ApiToken {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    role: r.get(2)?,
                    created_ts: r.get(3)?,
                    last_used_ts: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Look up a token by its hash, returning `(id, last_used_ts, role)` if it
    /// exists. The middleware uses this to authenticate a Bearer token per request.
    pub fn api_token_by_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<(i64, Option<i64>, String)>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, last_used_ts, role FROM api_tokens WHERE token_hash = ?1",
                [token_hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?)
    }

    /// Stamp a token's last-used time (the caller throttles how often).
    pub fn touch_api_token(&self, id: i64, now: i64) -> Result<()> {
        self.conn().execute(
            "UPDATE api_tokens SET last_used_ts = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(())
    }

    pub fn delete_api_token(&self, id: i64) -> Result<bool> {
        let n = self
            .conn()
            .execute("DELETE FROM api_tokens WHERE id=?1", [id])?;
        Ok(n > 0)
    }

    // --- security audit log ------------------------------------------------

    /// Cap on retained audit rows — bounded so a flood of (throttled) failed
    /// logins can't grow the table without limit.
    const AUDIT_KEEP: i64 = 2000;

    /// Record a security event. Best-effort: callers ignore the result so a
    /// logging failure never blocks the action being audited.
    pub fn add_audit(&self, ts: i64, ip: Option<&str>, action: &str, detail: Option<&str>) {
        let conn = self.conn();
        if conn
            .execute(
                "INSERT INTO audit_log (ts, ip, action, detail) VALUES (?1, ?2, ?3, ?4)",
                params![ts, ip, action, detail],
            )
            .is_ok()
        {
            // Trim to the most recent AUDIT_KEEP rows.
            let _ = conn.execute(
                "DELETE FROM audit_log WHERE id <= (SELECT MAX(id) FROM audit_log) - ?1",
                [Self::AUDIT_KEEP],
            );
        }
    }

    pub fn list_audit(&self, limit: u32) -> Result<Vec<AuditEntry>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, ts, ip, action, detail FROM audit_log ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([limit], |r| {
                Ok(AuditEntry {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    ip: r.get(2)?,
                    action: r.get(3)?,
                    detail: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // --- notifications (A4) --------------------------------------------------

    const NOTIF_KEEP: i64 = 2000;

    /// Insert a notification; returns its row id. Self-trims read rows beyond
    /// NOTIF_KEEP so the table stays bounded.
    pub fn add_notification(
        &self,
        ts: i64,
        kind: &str,
        title: &str,
        body: Option<&str>,
        event_id: Option<i64>,
    ) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO notifications (ts, kind, title, body, event_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![ts, kind, title, body, event_id],
        )?;
        let id = conn.last_insert_rowid();
        let _ = conn.execute(
            "DELETE FROM notifications WHERE read = 1 AND id <= \
             (SELECT MAX(id) FROM notifications) - ?1",
            [Self::NOTIF_KEEP],
        );
        Ok(id)
    }

    /// Newest-first notifications; when `unread_only`, only rows with read = 0.
    pub fn list_notifications(&self, unread_only: bool, limit: u32) -> Result<Vec<Notification>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, ts, kind, title, body, event_id, read FROM notifications
             WHERE (?1 = 0 OR read = 0) ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![unread_only as i64, limit], |r| {
                Ok(Notification {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    kind: r.get(2)?,
                    title: r.get(3)?,
                    body: r.get(4)?,
                    event_id: r.get(5)?,
                    read: r.get::<_, i64>(6)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn count_unread_notifications(&self) -> Result<i64> {
        Ok(self.conn().query_row(
            "SELECT COUNT(*) FROM notifications WHERE read = 0",
            [],
            |r| r.get(0),
        )?)
    }

    /// Mark one notification read; returns whether it existed.
    pub fn mark_notification_read(&self, id: i64) -> Result<bool> {
        let n = self
            .conn()
            .execute("UPDATE notifications SET read = 1 WHERE id = ?1", [id])?;
        Ok(n > 0)
    }

    /// Mark all notifications read; returns how many rows changed.
    pub fn mark_all_notifications_read(&self) -> Result<usize> {
        Ok(self
            .conn()
            .execute("UPDATE notifications SET read = 1 WHERE read = 0", [])?)
    }

    // --- digests (B1) --------------------------------------------------------

    const DIGEST_KEEP: i64 = 365;

    /// Insert a digest; returns its row id. Self-trims to DIGEST_KEEP rows.
    pub fn add_digest(&self, ts: i64, text: &str) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO digests (ts, text) VALUES (?1, ?2)",
            params![ts, text],
        )?;
        let id = conn.last_insert_rowid();
        let _ = conn.execute(
            "DELETE FROM digests WHERE id <= (SELECT MAX(id) FROM digests) - ?1",
            [Self::DIGEST_KEEP],
        );
        Ok(id)
    }

    pub fn list_digests(&self, limit: u32) -> Result<Vec<Digest>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT id, ts, text FROM digests ORDER BY id DESC LIMIT ?1")?;
        let rows = stmt
            .query_map([limit], |r| {
                Ok(Digest {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    text: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Store an anomaly score for an event (best-effort enrichment).
    pub fn set_event_anomaly(&self, event_id: i64, score: f32) -> Result<()> {
        self.conn().execute(
            "UPDATE events SET anomaly_score = ?1 WHERE id = ?2",
            params![score, event_id],
        )?;
        Ok(())
    }

    // --- users / roles (C5) --------------------------------------------------

    pub fn count_users(&self) -> Result<i64> {
        Ok(self
            .conn()
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?)
    }

    pub fn add_user(
        &self,
        username: &str,
        password_hash: &str,
        role: &str,
        now: i64,
    ) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO users (username, password_hash, role, created_ts) VALUES (?1, ?2, ?3, ?4)",
            params![username, password_hash, role, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// `(id, password_hash, role)` for a username, if it exists.
    pub fn user_by_name(&self, username: &str) -> Result<Option<(i64, String, String)>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, password_hash, role FROM users WHERE username = ?1",
                [username],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?)
    }

    pub fn list_users(&self) -> Result<Vec<UserRow>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT id, username, role, created_ts FROM users ORDER BY username")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(UserRow {
                    id: r.get(0)?,
                    username: r.get(1)?,
                    role: r.get(2)?,
                    created_ts: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn set_user_password(&self, id: i64, password_hash: &str) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE users SET password_hash = ?1 WHERE id = ?2",
            params![password_hash, id],
        )?;
        Ok(n > 0)
    }

    /// Set a user's role, refusing to strip the last admin. The role read, the
    /// admin count, and the update all run while the single DB lock is held, so
    /// two concurrent demotions can never both win and leave zero admins.
    pub fn set_user_role_guarded(&self, id: i64, role: &str) -> Result<SetRole> {
        let conn = self.conn();
        let cur: Option<String> = conn
            .query_row("SELECT role FROM users WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        let Some(cur) = cur else {
            return Ok(SetRole::NotFound);
        };
        if cur == "admin" && role != "admin" {
            let admins: i64 =
                conn.query_row("SELECT COUNT(*) FROM users WHERE role = 'admin'", [], |r| {
                    r.get(0)
                })?;
            if admins <= 1 {
                return Ok(SetRole::LastAdmin);
            }
        }
        conn.execute(
            "UPDATE users SET role = ?1 WHERE id = ?2",
            params![role, id],
        )?;
        Ok(SetRole::Ok)
    }

    /// Delete a user, refusing to delete the last admin (atomic under one lock).
    pub fn delete_user_guarded(&self, id: i64) -> Result<DeleteUser> {
        let conn = self.conn();
        let cur: Option<String> = conn
            .query_row("SELECT role FROM users WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        let Some(cur) = cur else {
            return Ok(DeleteUser::NotFound);
        };
        if cur == "admin" {
            let admins: i64 =
                conn.query_row("SELECT COUNT(*) FROM users WHERE role = 'admin'", [], |r| {
                    r.get(0)
                })?;
            if admins <= 1 {
                return Ok(DeleteUser::LastAdmin);
            }
        }
        conn.execute("DELETE FROM users WHERE id = ?1", [id])?;
        Ok(DeleteUser::Deleted)
    }

    pub fn delete_alarm(&self, id: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM alarms WHERE id=?1", [id])?;
        Ok(())
    }

    /// Bookmark an event: set/clear the flag and replace its note. A flagged
    /// event survives the event-retention prune. Returns whether the event
    /// existed.
    pub fn set_event_bookmark(&self, id: i64, flagged: bool, note: Option<&str>) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE events SET flagged = ?1, note = ?2 WHERE id = ?3",
            params![flagged as i64, note, id],
        )?;
        Ok(n > 0)
    }

    /// Set/clear an event's bookmark flag, leaving any existing note untouched.
    /// Returns whether the event existed.
    pub fn set_event_flag(&self, id: i64, flagged: bool) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE events SET flagged = ?1 WHERE id = ?2",
            params![flagged as i64, id],
        )?;
        Ok(n > 0)
    }

    // --- smart-search embeddings -------------------------------------------

    /// Store a GenAI caption for an event (best-effort enrichment).
    pub fn set_event_caption(&self, event_id: i64, caption: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE events SET caption = ?1 WHERE id = ?2",
            params![caption, event_id],
        )?;
        Ok(())
    }

    /// Store a speech-to-text transcript for an event (best-effort enrichment).
    pub fn set_event_transcript(&self, event_id: i64, transcript: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE events SET transcript = ?1 WHERE id = ?2",
            params![transcript, event_id],
        )?;
        Ok(())
    }

    /// Store an event's full-frame `embedding` (text smart-search) together with
    /// the optional object-`crop` embedding (cross-camera appearance search). One
    /// upsert so the two never race on row creation.
    pub fn set_event_embeddings(
        &self,
        event_id: i64,
        embedding: &[f32],
        crop: Option<&[f32]>,
    ) -> Result<()> {
        let emb: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        let crop_bytes: Option<Vec<u8>> =
            crop.map(|c| c.iter().flat_map(|f| f.to_le_bytes()).collect());
        self.conn().execute(
            "INSERT OR REPLACE INTO event_embeddings (event_id, embedding, crop_embedding)
             VALUES (?1, ?2, ?3)",
            params![event_id, emb, crop_bytes],
        )?;
        Ok(())
    }

    /// An event's object-crop embedding, if one was stored (appearance search).
    pub fn crop_embedding_for(&self, event_id: i64) -> Result<Option<Vec<f32>>> {
        let blob: Option<Vec<u8>> = self
            .conn()
            .query_row(
                "SELECT crop_embedding FROM event_embeddings WHERE event_id = ?1",
                params![event_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        Ok(blob.map(bytes_to_f32))
    }

    /// All `(event_id, crop_embedding)` pairs for events that have one — the
    /// candidate corpus for cross-camera appearance search. Retention-bounded
    /// (deleted events cascade), no row cap so recall isn't truncated.
    pub fn crop_embeddings(&self) -> Result<Vec<(i64, Vec<f32>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT event_id, crop_embedding FROM event_embeddings
             WHERE crop_embedding IS NOT NULL",
        )?;
        let rows = stmt
            .query_map([], |r| {
                let b: Vec<u8> = r.get(1)?;
                Ok((r.get::<_, i64>(0)?, bytes_to_f32(b)))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every event with its searchable text + (when `with_embeddings`) its CLIP
    /// snapshot embedding, newest first, for hybrid smart search (visual
    /// similarity + speech/caption text). No row cap — the corpus is the full
    /// (retention-bounded) event history, so search recall isn't truncated.
    /// The embedding column (and JOIN) is skipped entirely in text-only mode.
    pub fn search_corpus(&self, with_embeddings: bool) -> Result<Vec<SearchRow>> {
        let conn = self.conn();
        let sql = if with_embeddings {
            "SELECT e.id, e.transcript, e.caption, em.embedding
             FROM events e LEFT JOIN event_embeddings em ON em.event_id = e.id
             ORDER BY e.ts DESC, e.id DESC"
        } else {
            "SELECT e.id, e.transcript, e.caption, NULL
             FROM events e ORDER BY e.ts DESC, e.id DESC"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map([], |r| {
                let emb: Option<Vec<u8>> = r.get(3)?;
                Ok(SearchRow {
                    id: r.get(0)?,
                    transcript: r.get(1)?,
                    caption: r.get(2)?,
                    embedding: emb.map(bytes_to_f32),
                })
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

    /// Rename an enrolled identity (relabel all its embeddings at once).
    pub fn rename_face(&self, id: i64, name: &str) -> Result<()> {
        self.conn()
            .execute("UPDATE faces SET name=?1 WHERE id=?2", params![name, id])?;
        Ok(())
    }

    // --- license-plate library -------------------------------------------

    /// Add a library entry (upsert by normalized plate). Returns the row id.
    pub fn add_plate(
        &self,
        plate: &str,
        name: &str,
        category: &str,
        note: Option<&str>,
    ) -> Result<i64> {
        let key = normalize_plate(plate);
        let conn = self.conn();
        conn.execute(
            "INSERT INTO plates (plate, name, category, note, created_ts)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(plate) DO UPDATE SET
                 name = excluded.name, category = excluded.category, note = excluded.note",
            params![key, name, category, note, chrono::Local::now().timestamp()],
        )?;
        Ok(
            conn.query_row("SELECT id FROM plates WHERE plate = ?1", [key], |r| {
                r.get(0)
            })?,
        )
    }

    pub fn list_plates(&self) -> Result<Vec<PlateRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, plate, name, category, note, created_ts FROM plates ORDER BY name",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(PlateRow {
                    id: r.get(0)?,
                    plate: r.get(1)?,
                    name: r.get(2)?,
                    category: r.get(3)?,
                    note: r.get(4)?,
                    created_ts: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Look up a library entry by an already-normalized plate key.
    pub fn plate_by_text(&self, normalized: &str) -> Result<Option<PlateRow>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, plate, name, category, note, created_ts FROM plates WHERE plate = ?1",
                [normalized],
                |r| {
                    Ok(PlateRow {
                        id: r.get(0)?,
                        plate: r.get(1)?,
                        name: r.get(2)?,
                        category: r.get(3)?,
                        note: r.get(4)?,
                        created_ts: r.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn update_plate(
        &self,
        id: i64,
        name: &str,
        category: &str,
        note: Option<&str>,
    ) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE plates SET name = ?1, category = ?2, note = ?3 WHERE id = ?4",
            params![name, category, note, id],
        )?;
        Ok(n > 0)
    }

    pub fn delete_plate(&self, id: i64) -> Result<bool> {
        let n = self
            .conn()
            .execute("DELETE FROM plates WHERE id = ?1", [id])?;
        Ok(n > 0)
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

    /// Oldest not-yet-reduced segments that started before `cutoff_ts`,
    /// for the enhanced-retention re-encoder. Bounded by `limit`.
    pub fn reduction_candidates(&self, cutoff_ts: i64, limit: u32) -> Result<Vec<(String, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT path, start_ts FROM segments
             WHERE reduced = 0 AND start_ts < ?1
             ORDER BY start_ts ASC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![cutoff_ts, limit], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn mark_segment_reduced(&self, path: &str, new_bytes: u64) -> Result<()> {
        self.conn().execute(
            "UPDATE segments SET reduced = 1, bytes = ?1 WHERE path = ?2",
            params![new_bytes as i64, path],
        )?;
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
    /// so the caller can remove the files. Embeddings cascade. Flagged
    /// (bookmarked) events are protected — neither they nor their snapshots are
    /// removed, even past retention.
    pub fn prune_events_before(&self, cutoff_ts: i64) -> Result<Vec<String>> {
        let conn = self.conn();
        // Don't delete a snapshot still referenced by a kept (flagged) event.
        let mut stmt = conn.prepare(
            "SELECT DISTINCT snapshot FROM events
             WHERE ts < ?1 AND flagged = 0 AND snapshot IS NOT NULL
               AND snapshot NOT IN (SELECT snapshot FROM events WHERE flagged = 1)",
        )?;
        let snapshots = stmt
            .query_map([cutoff_ts], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        conn.execute(
            "DELETE FROM events WHERE ts < ?1 AND flagged = 0",
            [cutoff_ts],
        )?;
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
        let mut s: Settings = json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        // arm_mode is stored in its own KV row (the authoritative source), not the
        // settings blob — so a Settings-page save can never clobber the live arm
        // state, and `set_arm_mode` is a single-key write with no read-modify-write
        // race. Overlay it here so every reader (the dispatch sites) sees it.
        if let Some(mode) = self.get_kv("arm_mode") {
            if matches!(mode.as_str(), "home" | "away" | "disarmed") {
                s.arm_mode = mode;
            }
        }
        s
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

fn row_to_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
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
        gesture: r.get(13)?,
        zone: r.get(14)?,
        caption: r.get(15)?,
        transcript: r.get(16)?,
        flagged: r.get::<_, i64>(17)? != 0,
        note: r.get(18)?,
        anomaly_score: r.get(19)?,
        direction: r.get(20)?,
        speed: r.get(21)?,
    })
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
        group: r.get(9)?,
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
    fn heatmap_accumulates_anchors_excluding_synthetic() {
        let db = mem_db();
        let cam = db.add_camera("yard", "rtsp://x", None, true, true).unwrap();
        // Two people standing at anchor (0.5, 0.8).
        for ts in [100, 110] {
            db.add_event(
                cam.id,
                ts,
                "person",
                0.9,
                [0.45, 0.6, 0.55, 0.8],
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        }
        // One car at anchor (0.1, 0.2).
        db.add_event(
            cam.id,
            120,
            "car",
            0.9,
            [0.05, 0.0, 0.15, 0.2],
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        // A synthetic analytics marker (excluded by label) and a degenerate
        // zero-area box (an audio event, excluded by area) must NOT accumulate.
        db.add_event(
            cam.id,
            130,
            "crossing",
            1.0,
            [0.4, 0.4, 0.6, 0.6],
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        db.add_event(
            cam.id, 140, "Speech", 0.9, [0.0; 4], None, None, None, None, None,
        )
        .unwrap();
        // A legacy row in raw PIXEL coordinates (pre-normalisation): any coord > 1
        // must be excluded, not collapsed into the corner cell.
        db.add_event(
            cam.id,
            150,
            "person",
            0.9,
            [230.0, 600.0, 290.0, 967.0],
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let grid = 10;
        let cells = db.heatmap(cam.id, None, None, grid).unwrap();
        assert_eq!(cells.len(), grid * grid);
        assert_eq!(cells[8 * grid + 5], 2, "two people at (0.5,0.8)");
        assert_eq!(cells[2 * grid + 1], 1, "one car at (0.1,0.2)");
        assert_eq!(
            cells[grid * grid - 1],
            0,
            "legacy pixel row not collapsed into corner"
        );
        assert_eq!(
            cells.iter().sum::<u32>(),
            3,
            "crossing + audio + pixel row excluded"
        );
    }

    #[test]
    fn crop_embeddings_roundtrip_and_corpus() {
        let db = mem_db();
        let cam = db.add_camera("yard", "rtsp://x", None, true, true).unwrap();
        let e1 = db
            .add_event(
                cam.id,
                100,
                "person",
                0.9,
                [0.1, 0.1, 0.2, 0.4],
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        let e2 = db
            .add_event(
                cam.id,
                110,
                "person",
                0.9,
                [0.5, 0.5, 0.6, 0.8],
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        let e3 = db
            .add_event(
                cam.id,
                120,
                "car",
                0.9,
                [0.0, 0.0, 0.3, 0.3],
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        // e1, e2 get a frame + crop embedding; e3 gets a frame embedding only.
        db.set_event_embeddings(e1, &[1.0, 0.0, 0.0], Some(&[0.0, 1.0, 0.0]))
            .unwrap();
        db.set_event_embeddings(e2, &[0.0, 1.0, 0.0], Some(&[0.0, 0.9, 0.1]))
            .unwrap();
        db.set_event_embeddings(e3, &[0.5, 0.5, 0.0], None).unwrap();

        assert_eq!(
            db.crop_embedding_for(e1).unwrap(),
            Some(vec![0.0, 1.0, 0.0])
        );
        assert_eq!(db.crop_embedding_for(e3).unwrap(), None);
        // The corpus holds only events that have a crop embedding (e1, e2).
        let corpus = db.crop_embeddings().unwrap();
        let ids: Vec<i64> = corpus.iter().map(|(id, _)| *id).collect();
        assert_eq!(corpus.len(), 2);
        assert!(ids.contains(&e1) && ids.contains(&e2) && !ids.contains(&e3));
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
            None,
            Some("driveway"),
        )
        .unwrap();
        db.add_event(
            cam.id, 200, "car", 0.8, [0.0; 4], None, None, None, None, None,
        )
        .unwrap();
        db.add_event(
            cam.id,
            300,
            "gesture",
            1.0,
            [0.0; 4],
            None,
            None,
            None,
            Some("open_palm"),
            None,
        )
        .unwrap();

        let all = |db: &Db| {
            db.list_events(None, None, None, None, None, None, false, 10)
                .unwrap()
        };
        assert_eq!(all(&db).len(), 3);
        assert_eq!(
            db.list_events(None, Some("person"), None, None, None, None, false, 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.list_events(None, None, Some("open_palm"), None, None, None, false, 10)
                .unwrap()
                .len(),
            1
        );
        // Zone filter.
        assert_eq!(
            db.list_events(None, None, None, Some("driveway"), None, None, false, 10)
                .unwrap()
                .len(),
            1
        );
        // before / after time bounds.
        assert_eq!(
            db.list_events(None, None, None, None, None, Some(150), false, 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.list_events(None, None, None, None, Some(250), None, false, 10)
                .unwrap()
                .len(),
            1
        );

        // Bookmark filter + retention protection: flag one event, prune
        // everything (cutoff in the far future), and the flagged event + its
        // snapshot survive while the rest are pruned.
        let flagged_id = all(&db)[0].id;
        assert!(db
            .set_event_bookmark(flagged_id, true, Some("check this"))
            .unwrap());
        assert!(!db.set_event_bookmark(999_999, true, None).unwrap()); // missing id
        let only_flagged = db
            .list_events(None, None, None, None, None, None, true, 10)
            .unwrap();
        assert_eq!(only_flagged.len(), 1);
        assert_eq!(only_flagged[0].id, flagged_id);
        assert!(only_flagged[0].flagged);
        assert_eq!(only_flagged[0].note.as_deref(), Some("check this"));
        let removed = db.prune_events_before(i64::MAX).unwrap();
        let kept = all(&db);
        assert_eq!(kept.len(), 1, "flagged event survives prune");
        assert_eq!(kept[0].id, flagged_id);
        // Snapshots of pruned events are returned for file cleanup, never the
        // flagged event's own snapshot.
        assert!(!removed
            .iter()
            .any(|s| Some(s.as_str()) == kept[0].snapshot.as_deref()));

        // Deleting the camera cascades to its events.
        db.delete_camera(cam.id).unwrap();
        assert!(all(&db).is_empty());
    }

    #[test]
    fn flagged_event_protects_shared_snapshot_from_prune() {
        let db = mem_db();
        let cam = db.add_camera("gate", "rtsp://x", None, true, true).unwrap();
        // Two events share one snapshot file; a third has a unique one.
        let shared = "gate-shared.jpg";
        db.add_event(
            cam.id,
            100,
            "person",
            0.9,
            [0.0; 4],
            Some(shared),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let keep = db
            .add_event(
                cam.id,
                200,
                "person",
                0.9,
                [0.0; 4],
                Some(shared),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        db.add_event(
            cam.id,
            150,
            "car",
            0.8,
            [0.0; 4],
            Some("gate-lone.jpg"),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        // Flag the newer shared-snapshot event, then prune everything older.
        assert!(db.set_event_flag(keep, true).unwrap());
        let removed = db.prune_events_before(1000).unwrap();
        // Only the flagged event survives (it was ts=200, well under the cutoff).
        assert_eq!(db.count_events().unwrap(), 1);
        assert!(db.get_event(keep).unwrap().is_some());
        // The unshared snapshot of a pruned event is returned for file cleanup…
        assert!(removed.iter().any(|s| s == "gate-lone.jpg"));
        // …but the snapshot shared with the kept (flagged) event is protected.
        assert!(
            !removed.iter().any(|s| s == shared),
            "shared snapshot must not be deleted out from under the kept event"
        );
    }

    #[test]
    fn api_token_crud_and_lookup() {
        let db = mem_db();
        let id = db
            .add_api_token("home-assistant", "hash_abc", "operator", 100)
            .unwrap();
        let list = db.list_api_tokens().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "home-assistant");
        assert_eq!(list[0].role, "operator");
        assert!(list[0].last_used_ts.is_none());
        // Lookup by hash returns (id, last_used, role); unknown hash → None.
        assert_eq!(
            db.api_token_by_hash("hash_abc").unwrap(),
            Some((id, None, "operator".to_string()))
        );
        assert_eq!(db.api_token_by_hash("nope").unwrap(), None);
        // Touch records last-used; relisting reflects it.
        db.touch_api_token(id, 200).unwrap();
        assert_eq!(
            db.api_token_by_hash("hash_abc").unwrap(),
            Some((id, Some(200), "operator".to_string()))
        );
        // Delete is idempotent on a hit and reports a miss.
        assert!(db.delete_api_token(id).unwrap());
        assert!(!db.delete_api_token(id).unwrap());
        assert!(db.list_api_tokens().unwrap().is_empty());
    }

    #[test]
    fn audit_log_records_and_caps() {
        let db = mem_db();
        db.add_audit(100, Some("203.0.113.7"), "login_failed", None);
        db.add_audit(200, None, "token_created", Some("home-assistant"));
        let rows = db.list_audit(10).unwrap();
        assert_eq!(rows.len(), 2);
        // Newest first.
        assert_eq!(rows[0].action, "token_created");
        assert_eq!(rows[0].detail.as_deref(), Some("home-assistant"));
        assert_eq!(rows[1].action, "login_failed");
        assert_eq!(rows[1].ip.as_deref(), Some("203.0.113.7"));
        // Retention cap: after many inserts, the table stays bounded to AUDIT_KEEP.
        for i in 0..(Db::AUDIT_KEEP + 50) {
            db.add_audit(1000 + i, None, "login_failed", None);
        }
        let total: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))
            .unwrap();
        assert!(
            total <= Db::AUDIT_KEEP,
            "audit table must stay bounded, got {total}"
        );
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
            zones: vec![PolyZone {
                name: "driveway".into(),
                points: vec![[0.1, 0.1], [0.9, 0.1], [0.9, 0.9], [0.1, 0.9]],
                kind: ZoneKind::Required,
                labels: vec!["person".into()],
                dwell_secs: Some(30),
                occupancy_max: Some(5),
                alert_enter: true,
                child_watch: true,
                supervise: true,
                water: false,
            }],
            tripwires: vec![crate::analytics::Tripwire {
                name: "gate".into(),
                a: [0.0, 0.5],
                b: [1.0, 0.5],
                direction: crate::analytics::CrossDir::AToB,
                labels: vec!["car".into()],
                alert_wrong_way: true,
            }],
            ground_calib: Some(GroundCalib {
                points: [[0.2, 0.4], [0.8, 0.4], [0.9, 0.9], [0.1, 0.9]],
                width_m: 6.0,
                height_m: 12.0,
            }),
            privacy_masks: vec![vec![[0.0, 0.0], [0.2, 0.0], [0.2, 0.2], [0.0, 0.2]]],
            min_area: Some(0.001),
            max_area: Some(0.8),
            autotrack: true,
            audio_detect: false,
            event_only_recording: false,
            gesture_detect: true,
            model: Some("yolov8s.onnx".into()),
            force_cpu: Some(true),
            poll_ms: Some(2000),
            face_recognize: Some(true),
            two_way_audio: true,
            retention_days: Some(14),
            fall_detect: true,
            child_height_frac: Some(0.45),
        };
        db.update_camera(&cam).unwrap();
        let back = db.get_camera(cam.id).unwrap().unwrap();
        assert_eq!(back.detect_config, cam.detect_config);

        let z = back.detect_config.ignore_zones[0];
        assert!(z.contains(0.25, 0.25));
        assert!(!z.contains(0.75, 0.25));

        let pz = &back.detect_config.zones[0];
        assert_eq!(pz.kind, ZoneKind::Required);
        assert!(pz.applies_to("person"));
        assert!(!pz.applies_to("car"));

        // Group is persisted, defaults to None, and can be set + cleared.
        assert_eq!(back.group, None);
        cam.group = Some("outdoor".into());
        db.update_camera(&cam).unwrap();
        assert_eq!(
            db.get_camera(cam.id).unwrap().unwrap().group.as_deref(),
            Some("outdoor")
        );
        cam.group = None;
        db.update_camera(&cam).unwrap();
        assert_eq!(db.get_camera(cam.id).unwrap().unwrap().group, None);
    }

    #[test]
    fn point_in_polygon_math() {
        // Unit square.
        let sq = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        assert!(point_in_polygon(&sq, 0.5, 0.5));
        assert!(!point_in_polygon(&sq, 1.5, 0.5));
        assert!(!point_in_polygon(&sq, -0.1, 0.5));

        // Concave arrow / chevron: a point in the notch must read as outside.
        let chevron = [[0.0, 0.0], [0.5, 0.4], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        assert!(point_in_polygon(&chevron, 0.5, 0.8)); // body
        assert!(!point_in_polygon(&chevron, 0.5, 0.1)); // inside the V notch

        // Degenerate polygons never contain anything.
        assert!(!point_in_polygon(&[[0.0, 0.0], [1.0, 1.0]], 0.5, 0.5));
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
            gesture_like: None,
            transcript_like: None,
            face_unknown: false,
            zone_like: None,
            min_score: 0.5,
            action: "webhook".into(),
            target: "http://x".into(),
            days: vec![],
            start_hhmm: None,
            end_hhmm: None,
            cooldown_secs: 0,
            priority: 0,
            snooze_until: 0,
            created_ts: 0,
            modes: vec![],
            actions: vec![],
        };
        assert!(rule.matches(3, "person", 0.8, None, None, None, None));
        assert!(!rule.matches(2, "person", 0.8, None, None, None, None)); // wrong camera
        assert!(!rule.matches(3, "car", 0.8, None, None, None, None)); // wrong label
        assert!(!rule.matches(3, "person", 0.3, None, None, None, None)); // below score

        let face_rule = AlarmRule {
            camera_id: None,
            label: None,
            face_like: Some("coat".into()),
            min_score: 0.0,
            ..rule.clone()
        };
        assert!(face_rule.matches(1, "person", 0.9, Some("dark-COAT-guy"), None, None, None));
        assert!(!face_rule.matches(1, "person", 0.9, None, None, None, None));

        let plate_rule = AlarmRule {
            face_like: None,
            plate_like: Some("au77".into()),
            ..face_rule
        };
        assert!(plate_rule.matches(1, "car", 0.9, None, Some("B8AU77"), None, None));
        assert!(!plate_rule.matches(1, "car", 0.9, None, Some("XYZ123"), None, None));

        let mut disabled = plate_rule.clone();
        disabled.enabled = false;
        assert!(!disabled.matches(1, "car", 0.9, None, Some("B8AU77"), None, None));

        // Gesture rule: a held hand signal arms the action.
        let gesture_rule = AlarmRule {
            label: Some("gesture".into()),
            plate_like: None,
            gesture_like: Some("open_palm".into()),
            ..rule.clone()
        };
        assert!(gesture_rule.matches(3, "gesture", 1.0, None, None, Some("open_palm"), None));
        assert!(!gesture_rule.matches(3, "gesture", 1.0, None, None, Some("victory"), None));
        assert!(!gesture_rule.matches(3, "gesture", 1.0, None, None, None, None));

        // Spoken-keyword rule: fires only when the transcript contains the phrase.
        let spoken_rule = AlarmRule {
            label: None,
            gesture_like: None,
            transcript_like: Some("help".into()),
            ..rule.clone()
        };
        assert!(spoken_rule.matches(3, "speech", 1.0, None, None, None, Some("please HELP me")));
        assert!(!spoken_rule.matches(3, "speech", 1.0, None, None, None, Some("good morning")));
        // No transcript present → a transcript rule can't match (e.g. on a
        // detection event), so it never double-fires on non-audio sources.
        assert!(!spoken_rule.matches(3, "speech", 1.0, None, None, None, None));

        // Stranger rule: fires only on a person with an unrecognized face
        // (the UNKNOWN_FACE sentinel), not on a recognized face or no face.
        let stranger_rule = AlarmRule {
            label: Some("person".into()),
            gesture_like: None,
            face_unknown: true,
            ..rule.clone()
        };
        assert!(stranger_rule.matches(3, "person", 1.0, Some(UNKNOWN_FACE), None, None, None));
        assert!(!stranger_rule.matches(3, "person", 1.0, Some("Alice"), None, None, None));
        assert!(!stranger_rule.matches(3, "person", 1.0, None, None, None, None));

        // zone_like scopes a residential rule to a named detection zone (e.g.
        // "person in the Pool zone") via a case-insensitive substring match.
        let zoned = AlarmRule {
            zone_like: Some("Pool".into()),
            ..rule.clone()
        };
        assert!(zoned.zone_ok(Some("Backyard Pool")));
        assert!(!zoned.zone_ok(Some("Driveway")));
        assert!(!zoned.zone_ok(None), "a zone-scoped rule needs a zoned event");
        assert!(rule.zone_ok(None), "an unscoped rule matches any/no zone");
        assert!(rule.zone_ok(Some("Pool")));
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
                gesture_like: None,
                transcript_like: Some("help".into()),
                face_unknown: true,
                zone_like: None,
                min_score: 0.0,
                action: "webhook".into(),
                target: "http://t".into(),
                days: vec![1, 2, 3],
                start_hhmm: Some("22:00".into()),
                end_hhmm: Some("06:00".into()),
                cooldown_secs: 30,
                priority: 4,
                snooze_until: 0,
                created_ts: 0,
                modes: vec![],
                actions: vec![],
            })
            .unwrap();
        let back = &db.list_alarms().unwrap()[0];
        assert_eq!(back.days, vec![1, 2, 3]);
        assert_eq!(back.start_hhmm.as_deref(), Some("22:00"));
        assert_eq!(back.end_hhmm.as_deref(), Some("06:00"));
        assert_eq!(back.cooldown_secs, 30);
        assert_eq!(back.priority, 4);
        assert_eq!(back.transcript_like.as_deref(), Some("help"));
        assert!(back.face_unknown);
        // A legacy rule (no explicit scene) reads back as a 1-action scene
        // synthesized from the legacy action/target/priority columns.
        assert_eq!(back.actions.len(), 1);
        assert_eq!(back.actions[0].kind, "webhook");
        assert_eq!(back.actions[0].target, "http://t");
        assert_eq!(back.actions[0].priority, 4);
        assert_eq!(db.list_alarms().unwrap().len(), 1);
        db.set_alarm_enabled(id, false).unwrap();
        assert!(!db.list_alarms().unwrap()[0].enabled);
        db.delete_alarm(id).unwrap();
        assert!(db.list_alarms().unwrap().is_empty());

        // Multi-action scene + arm modes round-trip; the legacy action/target
        // columns are dual-written from actions[0] so an older build still reads
        // a usable (degraded) rule.
        let scene = AlarmRule {
            actions: vec![
                Action {
                    kind: "ntfy".into(),
                    target: "https://ntfy.sh/x".into(),
                    priority: 5,
                },
                Action {
                    kind: "mqtt".into(),
                    target: "door".into(),
                    priority: 0,
                },
            ],
            modes: vec!["away".into()],
            action: String::new(),
            target: String::new(),
            ..back.clone()
        };
        db.add_alarm(&scene).unwrap();
        let got = db.list_alarms().unwrap().remove(0);
        assert_eq!(got.actions.len(), 2);
        assert_eq!(got.actions[0].kind, "ntfy");
        assert_eq!(got.actions[0].priority, 5);
        assert_eq!(got.actions[1].target, "door");
        assert_eq!(got.modes, vec!["away".to_string()]);
        assert_eq!(got.action, "ntfy"); // legacy column dual-written from actions[0]
        assert_eq!(got.target, "https://ntfy.sh/x");
    }

    #[test]
    fn plate_library_roundtrip() {
        let db = mem_db();
        assert_eq!(normalize_plate("ab-12 34"), "AB1234");
        assert_eq!(normalize_plate("  -- "), "");
        let id = db
            .add_plate("ab 12-34", "My car", "known", Some("daily driver"))
            .unwrap();
        // Upsert by normalized plate updates the same row, doesn't duplicate.
        let id2 = db.add_plate("AB1234", "My Car", "watch", None).unwrap();
        assert_eq!(id, id2);
        let list = db.list_plates().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].plate, "AB1234");
        assert_eq!(list[0].category, "watch");
        // Lookup is by the normalized key; misses return None.
        assert_eq!(db.plate_by_text("AB1234").unwrap().unwrap().name, "My Car");
        assert!(db.plate_by_text("ZZ9999").unwrap().is_none());
        assert!(db.update_plate(id, "Renamed", "known", None).unwrap());
        assert_eq!(db.plate_by_text("AB1234").unwrap().unwrap().name, "Renamed");
        assert!(db.delete_plate(id).unwrap());
        assert!(db.list_plates().unwrap().is_empty());
        assert!(!db.delete_plate(id).unwrap());
    }

    #[test]
    fn alarm_schedule_windows() {
        let base = AlarmRule {
            id: 1,
            name: "night".into(),
            enabled: true,
            camera_id: None,
            label: None,
            face_like: None,
            plate_like: None,
            gesture_like: None,
            transcript_like: None,
            face_unknown: false,
            zone_like: None,
            min_score: 0.0,
            action: "webhook".into(),
            target: "http://x".into(),
            days: vec![],
            start_hhmm: None,
            end_hhmm: None,
            cooldown_secs: 0,
            priority: 0,
            snooze_until: 0,
            created_ts: 0,
            modes: vec![],
            actions: vec![],
        };
        // No schedule = always armed.
        assert!(base.armed_at(0, 0));
        assert!(base.armed_at(6, 1439));

        // Day filter: weekdays only (Mon=1..Fri=5).
        let weekdays = AlarmRule {
            days: vec![1, 2, 3, 4, 5],
            ..base.clone()
        };
        assert!(weekdays.armed_at(3, 600));
        assert!(!weekdays.armed_at(0, 600)); // Sunday

        // Same-day window 09:00-17:00.
        let work = AlarmRule {
            start_hhmm: Some("09:00".into()),
            end_hhmm: Some("17:00".into()),
            ..base.clone()
        };
        assert!(work.armed_at(2, 9 * 60));
        assert!(work.armed_at(2, 17 * 60));
        assert!(!work.armed_at(2, 8 * 60 + 59));
        assert!(!work.armed_at(2, 20 * 60));

        // Overnight window 22:00-06:00 spans midnight.
        let night = AlarmRule {
            start_hhmm: Some("22:00".into()),
            end_hhmm: Some("06:00".into()),
            ..base.clone()
        };
        assert!(night.armed_at(2, 23 * 60));
        assert!(night.armed_at(2, 3 * 60));
        assert!(!night.armed_at(2, 12 * 60));

        // Garbage times are ignored (treated as unset bound).
        let bad = AlarmRule {
            start_hhmm: Some("25:99".into()),
            end_hhmm: None,
            ..base
        };
        assert!(bad.armed_at(2, 0));
    }

    #[test]
    fn eventless_segments_query() {
        let db = mem_db();
        let cam = db
            .add_camera("porch", "rtsp://x", None, true, true)
            .unwrap();
        // Three 60s segments: t=1000, 2000, 3000. One event at t=2030
        // (inside segment 2; within margin of nothing else at margin=15).
        for ts in [1000, 2000, 3000] {
            db.upsert_segment(cam.id, ts, &format!("p{ts}.mp4"), 10)
                .unwrap();
        }
        db.add_event(
            cam.id, 2030, "person", 0.9, [0.0; 4], None, None, None, None, None,
        )
        .unwrap();
        let mut doomed = db.eventless_segments(cam.id, 5000, 60, 15).unwrap();
        doomed.sort();
        assert_eq!(
            doomed,
            vec!["p1000.mp4".to_string(), "p3000.mp4".to_string()]
        );
        // Grace period: nothing older than 1500 except segment 1.
        assert_eq!(
            db.eventless_segments(cam.id, 1500, 60, 15).unwrap(),
            vec!["p1000.mp4".to_string()]
        );
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
