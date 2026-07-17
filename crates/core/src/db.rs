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
/// 4-point special case.
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
    /// P3.5 zero-shot zone-state classifier (EXPERIMENTAL, best-effort): watch
    /// this zone and classify a binary open/closed state from the two CLIP text
    /// prompts below, emitting a `zone_open` / `zone_closed` state-change event
    /// scoped by the zone name (match it on an alarm rule's `zone_like`). Only
    /// active when this is on, BOTH prompts are non-empty, AND the CLIP
    /// smart-search models are present — otherwise it silently no-ops (never a
    /// fake event). Reuses the shared CLIP session; a state-classify zone needs
    /// its camera's detection on. See `zonestate.rs`. Off by default.
    #[serde(default)]
    pub state_classify: bool,
    /// CLIP prompt describing the zone's OPEN state, e.g. "an open garage door".
    #[serde(default)]
    pub open_prompt: Option<String>,
    /// CLIP prompt describing the zone's CLOSED state, e.g. "a closed garage door".
    #[serde(default)]
    pub closed_prompt: Option<String>,
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
    /// Polygon zones (required / ignore). A `Required` zone makes detections
    /// valid only when their anchor lands inside one; `Ignore` zones drop
    /// detections whose anchor falls inside (e.g. a busy street at the edge of
    /// a driveway camera).
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
    /// setting. Useful to keep a low-priority camera off a busy GPU. Kept for
    /// backward compatibility — the named `accelerator` field below supersedes
    /// it; `force_cpu` still applies when `accelerator` is unset.
    pub force_cpu: Option<bool>,
    /// Per-camera named execution provider: `None`/`""` inherits the global
    /// `Settings.accelerator`; `"auto"` = the best per-OS EP; `"cpu"`; or
    /// `"openvino"` (Intel iGPU/NPU, only when the build/runtime supports it).
    /// An explicit accelerator wins over `force_cpu`; `force_cpu` applies only
    /// when this is unset. See `detector::effective_accelerator`.
    #[serde(default)]
    pub accelerator: Option<String>,
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
    /// Camera tamper detection (blackout / defocus / scene-change): watch the
    /// optical integrity of this camera's feed and fire a `tamper` event +
    /// notification when the lens is covered, defocused, or the camera is moved.
    #[serde(default)]
    pub tamper_detect: bool,
    /// Gait analysis & identification: build a per-person walking signature from
    /// the object tracker and attribute person events to an enrolled gait
    /// identity (works at distance / when the face isn't visible). Opt-in.
    #[serde(default)]
    pub gait_identify: bool,
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
    /// Server-side body-pose monitoring (24/7, headless) for the residential
    /// safety tier: fall posture, crib standing/rollover, covered-face. Runs a
    /// YOLOv8-pose model (Settings.pose_model) on this camera. Opt-in + ASSISTIVE
    /// only — see `posture.rs` + `docs/05`. Off by default.
    #[serde(default)]
    pub pose_detect: bool,
    /// Privacy / dignity for sensitive cameras (nursery, bedroom, bathroom): when
    /// on, residential + pose safety events on this camera fire WITHOUT saving a
    /// snapshot image — you still get the alert (label + zone + time), but no
    /// picture is written to disk (or sent to webhook/MQTT with an image). Pairs
    /// with privacy masks for live view. Off by default.
    #[serde(default)]
    pub no_clip: bool,
    /// Absence / inactivity watch (Verkada-style, aging-in-place & pets): alert
    /// when this camera has seen NO person/pet event for this many hours. Edge-
    /// triggered (one notification per quiet spell, cleared by the next
    /// sighting). ASSISTIVE only — absence of detections is not proof of
    /// absence of activity (camera angle, lighting, model misses). `None` = off.
    #[serde(default)]
    pub absence_hours: Option<f32>,
    /// Ingest the camera's OWN analytics (ONVIF PullPoint events: motion, IVS
    /// tripwire/intrusion, person/vehicle classifications) as first-class
    /// `camera_*` events — zero server GPU cost, uses the camera's chip. The
    /// camera source must carry ONVIF credentials (onvif:// or rtsp:// with
    /// user:pass@host). Blue Iris "ONVIF triggers" / Axis-ACS-style. Off by
    /// default.
    #[serde(default)]
    pub onvif_events: bool,
    /// Per-camera recording schedule (#67, Blue Iris "profiles/schedules"): when
    /// set, continuous recording runs ONLY during the window (day-of-week +
    /// time-of-day, overnight-aware). `None` = always record (the default).
    /// Event/clip capture and detection are unaffected — this gates the
    /// continuous packet-copy recorder only.
    #[serde(default)]
    pub record_schedule: Option<Schedule>,
    /// Package / parcel monitoring (#69, "porch piracy"): emit a `package` event
    /// when a parcel-like object persists in the zone, and `package_removed` when
    /// it's taken. Off by default (opt-in).
    #[serde(default)]
    pub package_detect: bool,
    /// Polygon (0..1 fractions) the parcel must sit inside; `None` = whole frame.
    #[serde(default)]
    pub package_zone: Option<Vec<[f32; 2]>>,
    /// Labels that count as a parcel; empty = the default COCO carry-item set
    /// (`suitcase`/`backpack`/`handbag`). Add a real `package` class here if your
    /// model has one.
    #[serde(default)]
    pub package_labels: Vec<String>,
    /// Stationary-object suppression: only emit a detection event when the object
    /// is **new or has moved**, not on every motion-gate trip while a parked car /
    /// idle object sits in view. Runs the object tracker for this camera and
    /// drops a detection whose matching confirmed track was already alerted and
    /// hasn't moved past a small threshold. New arrivals and objects that move
    /// still fire (rate-limited by `event_cooldown_secs`). Off by default — a
    /// people-counter or doorway that wants every detection leaves it off. See
    /// `pipeline.rs` (`moved_enough`).
    #[serde(default)]
    pub suppress_stationary: bool,
    /// Detection-triggered recording (P3.8): a TIGHTER, asymmetric variant of
    /// event-only retention. Continuous packet-copy segmenting is UNCHANGED —
    /// real pre-roll footage exists only because the segmenter never stops — so
    /// this never starts/stops ffmpeg; it only prunes segments HARDER. A segment
    /// is deleted unless a detection lands inside its
    /// `[event.ts - pre_roll, event.ts + post_roll]` window, after a short
    /// settle grace (`post_roll + 30s` past the segment's END) instead of the
    /// flat 15-minute event-only grace, so a quiet camera sheds disk fast.
    /// Flagged/bookmarked events are ordinary event rows (never pruned from
    /// `events`), so their footage is always kept. Off by default; mutually
    /// exclusive with `event_only_recording` in the UI. See `record.rs`.
    #[serde(default)]
    pub trigger_recording: bool,
    /// Seconds of footage to keep BEFORE each detection (pre-roll). `None` = 10s.
    #[serde(default)]
    pub trigger_pre_roll_secs: Option<u32>,
    /// Seconds of footage to keep AFTER each detection (post-roll). `None` = 30s.
    #[serde(default)]
    pub trigger_post_roll_secs: Option<u32>,
    /// Dual-stream recording (P3.7): ALSO record go2rtc's low-res detect
    /// sub-stream (`{name}_sub`) to disk alongside the full-res main stream, so
    /// the UI can scrub the lightweight SD copy and play the HD one. OPT-IN and
    /// off by default — a camera that leaves this false records EXACTLY as before
    /// (main stream only). Requires a `detect_source` (there's no sub restream to
    /// record without one); it's silently a no-op otherwise. Sub segments are a
    /// local scrub aid — pruned by the same retention as main, but never shipped
    /// offsite. See `record.rs`.
    #[serde(default)]
    pub record_substream: bool,
    /// P3.4 HomeKit exposure: when the global `Settings.homekit_enabled` bridge is
    /// on, expose THIS camera as a HomeKit camera accessory (live view) through
    /// go2rtc's HAP server. Off by default and INDEPENDENT of `no_clip`/privacy —
    /// a sensitive camera stays off HomeKit unless explicitly exposed here (a
    /// deliberate privacy default). v0 is live-view only; pairing must be done on
    /// a real Apple Home device. See `go2rtc.rs`.
    #[serde(default)]
    pub homekit_expose: bool,
    /// P3.4 v1b: also expose a HomeKit doorbell BUTTON for this camera through
    /// the "Cammy Sensors" bridge. Rings (single press) on a YAMNet "Doorbell"
    /// audio event or a soft trigger labeled "doorbell". Ships as a stateless
    /// programmable switch, not a HomeKit Doorbell service — the Home app
    /// rejects doorbell accessories that lack a camera stream service, which
    /// only go2rtc's (sensor-less) HAP accessory has. Needs `homekit_expose`.
    #[serde(default)]
    pub homekit_doorbell: bool,
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
    /// Attributed gait identity (#64): an enrolled name, or `?` for a confident
    /// unknown walker, when gait identification ran on a person event.
    #[serde(default)]
    pub gait: Option<String>,
    /// Severity tier 1 (low) .. 4 (critical) — see [`crate::severity`]. Stored
    /// at emit time; rows from before the column existed are re-derived on read.
    #[serde(default)]
    pub severity: u8,
    /// User-applied tags ("insurance", "wildlife", …) — free multi-tag taxonomy
    /// beyond flag+note. Stored as a JSON array.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Tracker track id for a tracker-driven *narrative* event (line-crossing /
    /// wrong-way / loiter / zone-enter / child / fall / still-water); `None` for
    /// ordinary detections, occupancy, package and camera_* events. Powers the
    /// object-lifecycle view (P2.16). NB: per-camera track ids reset on every
    /// service restart, so it's only meaningful within a contiguous run — see
    /// [`Db::track_lifecycle`].
    #[serde(default)]
    pub track_id: Option<i64>,
    /// The object's serialized trajectory (`[[ts_ms, x, y], …]`, the track's
    /// bounded history) captured at emit time (docs/08 P1.5). Server-internal —
    /// never sent to clients (it's aggregated by the lifecycle endpoint instead
    /// of shipped on every event row).
    #[serde(skip)]
    pub path_json: Option<String>,
}

/// One enrolled gait identity (its averaged signature stays server-side).
#[derive(Clone, Debug, Serialize)]
pub struct GaitProfileRow {
    pub id: i64,
    pub name: String,
    pub samples: i64,
    pub created_ts: i64,
    pub updated_ts: i64,
}

/// One tracked occupant for presence/geofence arming (P2.10). A phone/automation
/// reports arrival/departure by flipping `home`; the arm mode is then derived
/// first-in/last-out from the count of occupants home.
#[derive(Clone, Debug, Serialize)]
pub struct Occupant {
    pub id: i64,
    pub name: String,
    pub home: bool,
    pub updated_ts: i64,
    pub created_ts: i64,
}

/// First-in/last-out arm mode from the count of occupants home: any occupant
/// home ⇒ "home", nobody home ⇒ "away". Presence never sets "disarmed" (that's
/// an explicit manual/scheduled choice).
pub fn derive_arm_mode(home_count: i64) -> &'static str {
    if home_count > 0 {
        "home"
    } else {
        "away"
    }
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

/// A shareable clip link's metadata for the manage UI (never the token/hash).
#[derive(Clone, Debug, Serialize)]
pub struct ClipShare {
    pub id: i64,
    pub event_id: i64,
    pub label: Option<String>,
    pub camera: Option<String>,
    pub expires_ts: i64,
    pub revoked: bool,
    pub created_ts: i64,
}

/// The clip an active share token resolves to (public /share route).
pub struct ShareTarget {
    pub event_id: i64,
    pub pre: i64,
    pub post: i64,
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
    /// P2.11: the alarm rule this notification came from (NULL for system
    /// notifications — stranger/offline/anomaly/digest). Used by the push worker
    /// to route per user × rule.
    pub rule_id: Option<i64>,
    /// P2.11: the camera the alarm fired on (NULL for system notifications).
    /// Used by the push worker to apply per-user camera visibility.
    pub camera_id: Option<i64>,
    /// P2.11: severity tier 1..4 of the alarm fire (NULL for system
    /// notifications). The push worker gates the human push/email channels on
    /// `notify_min_severity` using this; NULL always delivers.
    pub severity: Option<i64>,
}

/// P2.11 one per-user notification preference: does `channel` ('push' | 'email')
/// deliver alerts from `rule_id` (0 = the user's default for every rule) to this
/// user? An absent row means enabled (opt-out model). `enabled` is the stored
/// value; resolution (exact rule → default → true) lives in [`Db::pref_enabled`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NotifyPref {
    pub user_id: i64,
    pub rule_id: i64,
    pub channel: String,
    pub enabled: bool,
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
    /// P2.11 optional notification email — Admin surfaces/edits it in the
    /// Users card; the push worker delivers per-user email to it. `None` = unset.
    pub email: Option<String>,
}

/// A credential's TOTP 2FA config: `(secret_base32, enabled, recovery_json)`.
/// An inner `secret` of `None` means 2FA was never set up; `enabled` is false
/// while a freshly minted secret awaits confirmation during enrollment.
pub type TotpConfig = (Option<String>, bool, Option<String>);

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

/// One auto-arm/disarm schedule entry (residential "modes" automation): at
/// `hhmm` local time on the given `days` (0 = Sunday; empty = every day), set the
/// system to `mode` ("home" | "away" | "disarmed"). Driven by `schedule.rs`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ArmScheduleEntry {
    pub days: Vec<u8>,
    pub hhmm: String,
    pub mode: String,
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

/// Sentinel stored in an event's `gait` when a person was tracked walking but
/// matched no enrolled gait profile — an "unknown walker" (#64). Distinguishes
/// that from "gait not computed" (`None`); the raw signature is kept in
/// `gait_sig` so the walker can be enrolled from the event.
pub const UNKNOWN_GAIT: &str = "?";

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
    /// Cross-modal confirmation: only fire when an event of THIS label also
    /// occurred on the same camera within `confirm_within_secs` — e.g. a "Glass"
    /// audio event confirmed by a "person" within 10 s (glass-vs-dishes), or a
    /// "fall" confirmed by a "Screaming". `None` = no confirmation required.
    /// Opt-in and precision-oriented; **fails open** (fires) on any lookup error
    /// so an infra glitch never silently suppresses a real alert. Do NOT gate a
    /// life-safety rule on it — see docs/05 (confirmation should escalate, not gate).
    #[serde(default)]
    pub confirm_label: Option<String>,
    /// Window (seconds) for `confirm_label`. `None`/0 disables confirmation.
    #[serde(default)]
    pub confirm_within_secs: Option<i64>,
    /// VLM alert-verification gate (Agent DVR "Ask AI" / Bosch IVA Pro Context):
    /// a yes/no question asked of the GenAI vision model about the event snapshot;
    /// the rule only fires when the model answers "yes" (phrase the prompt as the
    /// condition to CONFIRM, e.g. "Is a real person actually at the front door?").
    /// Runs OFF the detection thread in the GenAI worker so the multi-second call
    /// never stalls detection, and **fails OPEN** — fires on any model error/timeout
    /// so a flaky endpoint never silently suppresses a real alert. Needs GenAI
    /// captions enabled (a vision model). `None`/empty = no gate. Detection-event
    /// rules only in v1. Rides `schedule_json` (no migration).
    #[serde(default)]
    pub vlm_prompt: Option<String>,
    /// Describe-in-notification (Wyze/Ring/Nest "descriptive alerts"): route the
    /// fire through the GenAI worker, caption the snapshot first, and put the
    /// description IN the push/email/webhook (`{{caption}}`). Fails open — any
    /// model error/timeout fires a normal (caption-less) alert. Needs GenAI
    /// captions enabled. Detection-event rules only in v1; rides `schedule_json`.
    #[serde(default)]
    pub describe: bool,
    /// Prompt-based standing rule (Reolink "Prompt-Based Alerts"): a free-text
    /// description ("someone climbing the fence", "a red pickup truck") that is
    /// CLIP-text-embedded once and cosine-compared against each detection's
    /// crop embedding at detection time — the rule fires when an object *looks
    /// like* the prompt. Needs the CLIP models installed; evaluated ONLY in the
    /// pipeline's embedding pass (`prompt_ok` in [`Self::matches`]), so other
    /// dispatch sites can never fire it. Best-effort semantic matching — pair
    /// with a label/zone scope for precision. Rides `schedule_json`.
    #[serde(default)]
    pub prompt_like: Option<String>,
    /// P2.5 — CLIP attribute facet (a curated generalisation of `prompt_like`):
    /// stores a catalog KEY (e.g. `veh_color_red`, see [`crate::attributes`]),
    /// NOT free text. Resolved to its CLIP text prompt by [`Self::effective_prompt`]
    /// and fired by the SAME embedding pass as `prompt_like` — so it's an
    /// "AI watch"-style best-effort gate too. Rides `schedule_json` (no migration).
    #[serde(default)]
    pub attr_like: Option<String>,
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

/// Whether a day+time window is active at `weekday` (0 = Sunday) / `minute` of
/// day. Empty `days` = every day; absent start/end = open-ended; start > end is
/// an overnight window (e.g. 22:00–06:00). Shared by alarm-rule schedules and
/// per-camera recording schedules.
pub fn window_active(
    days: &[u8],
    start_hhmm: Option<&str>,
    end_hhmm: Option<&str>,
    weekday: u8,
    minute: u16,
) -> bool {
    if !days.is_empty() && !days.contains(&weekday) {
        return false;
    }
    match (
        start_hhmm.and_then(parse_hhmm),
        end_hhmm.and_then(parse_hhmm),
    ) {
        (None, None) => true,
        (Some(s), None) => minute >= s,
        (None, Some(e)) => minute <= e,
        (Some(s), Some(e)) if s <= e => minute >= s && minute <= e,
        (Some(s), Some(e)) => minute >= s || minute <= e, // overnight
    }
}

/// Current local `(weekday 0=Sun, minute-of-day)` — the clock for schedules.
fn now_weekday_minute() -> (u8, u16) {
    use chrono::{Datelike as _, Timelike as _};
    let now = chrono::Local::now();
    (
        now.weekday().num_days_from_sunday() as u8,
        (now.hour() * 60 + now.minute()) as u16,
    )
}

/// A reusable day+time-of-day schedule (the shape alarm rules already use),
/// applied to per-camera **recording** windows (#67): record only when active.
/// Empty `days` + no start/end = always active (the default, so it's opt-in).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    /// Weekdays it applies on (0 = Sunday); empty = every day.
    #[serde(default)]
    pub days: Vec<u8>,
    #[serde(default)]
    pub start_hhmm: Option<String>,
    #[serde(default)]
    pub end_hhmm: Option<String>,
}

impl Schedule {
    pub fn active_at(&self, weekday: u8, minute: u16) -> bool {
        window_active(
            &self.days,
            self.start_hhmm.as_deref(),
            self.end_hhmm.as_deref(),
            weekday,
            minute,
        )
    }
    pub fn active_now(&self) -> bool {
        let (wd, min) = now_weekday_minute();
        self.active_at(wd, min)
    }
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
        window_active(
            &self.days,
            self.start_hhmm.as_deref(),
            self.end_hhmm.as_deref(),
            weekday,
            minute,
        )
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

    /// Cross-modal confirmation gate. Returns true (allow the rule to fire) unless
    /// both `confirm_label` and a positive `confirm_within_secs` are set AND no
    /// event of that label exists on this camera within the window. **Fails OPEN**
    /// (returns true) on any DB error so an infrastructure glitch can never
    /// silently suppress an alert — confirmation is for precision, not gating
    /// life-safety. AND-ed with [`AlarmRule::matches`] at every alarm call site.
    pub fn confirm_ok(&self, db: &Db, camera_id: i64, now: i64) -> bool {
        match (
            self.confirm_label
                .as_deref()
                .map(str::trim)
                .filter(|l| !l.is_empty()),
            self.confirm_within_secs,
        ) {
            (Some(lbl), Some(w)) if w > 0 => {
                db.has_recent_event(camera_id, lbl, now - w).unwrap_or(true)
            }
            _ => true,
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
        // A prompt rule (CLIP crop similarity) can only be satisfied by the
        // pipeline's embedding pass, which verifies the similarity itself and
        // then calls [`Self::matches_prompt`] — plain `matches` (every other
        // dispatch site) rejects prompt rules outright so they can never fire
        // on events that were never compared against the prompt.
        if self.is_prompt_rule() {
            return false;
        }
        true
    }

    /// Whether this rule carries a non-empty CLIP prompt condition — a free-text
    /// `prompt_like` (P2.2) OR an `attr_like` catalog key (P2.5). Both fire ONLY
    /// via the pipeline's crop-embedding pass, so every plain-`matches` dispatch
    /// site rejects them (see the tail of [`Self::matches`]).
    pub fn is_prompt_rule(&self) -> bool {
        let set = |o: &Option<String>| o.as_deref().map(str::trim).is_some_and(|p| !p.is_empty());
        set(&self.prompt_like) || set(&self.attr_like)
    }

    /// The CLIP text prompt this rule matches crops against: the explicit
    /// `prompt_like` text if set, else the `attr_like` catalog key resolved to
    /// its prompt (`None` if the key is empty or no longer in the catalog).
    pub fn effective_prompt(&self) -> Option<String> {
        if let Some(p) = self
            .prompt_like
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            return Some(p.to_string());
        }
        self.attr_like
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .and_then(|k| crate::attributes::prompt_for(k))
            .map(str::to_string)
    }

    /// The embedding pass's variant of [`Self::matches`]: the caller has
    /// already verified the CLIP prompt similarity, so only the OTHER
    /// conditions are checked. `false` for non-prompt rules (they fire on the
    /// normal path).
    #[allow(clippy::too_many_arguments)]
    pub fn matches_prompt(
        &self,
        camera_id: i64,
        label: &str,
        score: f32,
        face: Option<&str>,
        plate: Option<&str>,
    ) -> bool {
        if !self.is_prompt_rule() {
            return false;
        }
        // Reuse `matches` by masking the prompt conditions: a cloned rule
        // without the CLIP prompt/attr gate evaluates every other condition
        // identically (so `is_prompt_rule()` is false on the clone).
        let mut other = self.clone();
        other.prompt_like = None;
        other.attr_like = None;
        other.matches(camera_id, label, score, face, plate, None, None)
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
    /// P3.7: 'main' (full-res archive) or 'sub' (opt-in low-res scrub aid).
    pub stream: String,
}

/// After this many failed offsite-upload attempts a segment is given up on
/// (terminal `gaveup`) so a permanently-failing object stops being retried
/// forever. With the worker's capped backoff this is well over a day of
/// transient outage first (matrix #70).
pub const OFFSITE_MAX_ATTEMPTS: i64 = 20;

/// A recording segment still awaiting offsite backup (matrix #70). `attempts`
/// and `last_ts` drive the worker's exponential backoff for previously-failed
/// uploads (0 = never attempted).
#[derive(Clone, Debug)]
pub struct PendingUpload {
    pub path: String,
    pub camera: String,
    pub start_ts: i64,
    pub bytes: u64,
    pub attempts: i64,
    pub last_ts: i64,
}

/// Aggregate offsite-backup health for `GET /api/offsite/status` + metrics.
#[derive(Clone, Debug, Default, Serialize)]
pub struct OffsiteStats {
    /// Newest successful-upload timestamp, if any.
    pub last_success_ts: Option<i64>,
    /// Sealed segments not yet uploaded (pending or failed).
    pub backlog: i64,
    /// Total bytes successfully uploaded.
    pub bytes_total: i64,
    /// Successfully-uploaded segment count.
    pub done: i64,
    /// Segments whose local file was pruned before backup (terminal loss).
    pub skipped: i64,
    /// Segments that exhausted retries / were oversize (terminal loss).
    pub gaveup: i64,
    /// Most recent upload error, if any (no secrets — S3 error text only).
    pub last_error: Option<String>,
    /// Bytes uploaded per camera (camera, bytes).
    pub per_camera: Vec<(String, i64)>,
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
    /// Global named execution provider for detector/face/pose sessions: `""` =
    /// the best per-OS EP (today's default), `"cpu"`, or `"openvino"` (Intel
    /// iGPU/NPU, only when the build/runtime supports it — gated in the UI via
    /// `/api/capabilities`). An explicit accelerator wins over `force_cpu`.
    #[serde(default)]
    pub accelerator: String,
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
    /// INBOUND MQTT control (P3.3): when on, the NVR subscribes to
    /// `<mqtt_prefix>/cmd/#` and accepts arm/disarm + camera-trigger commands
    /// published to the broker. OFF by default — this is a control surface:
    /// ANYONE who can publish to your broker can arm/disarm and trigger cameras,
    /// so only enable it on a broker you trust. Every accepted command is audited.
    #[serde(default)]
    pub mqtt_commands_enabled: bool,
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
    /// Minimum event severity (1..4, see `crate::severity`) for the HUMAN-facing
    /// alarm channels (ntfy push + email). 1 = notify on everything (default);
    /// 3 = only high/critical. Webhook/MQTT automations are never gated, and a
    /// duress event always pushes. The one-knob Wyze-NBD-style fatigue filter.
    #[serde(default = "default_notify_min_severity")]
    pub notify_min_severity: u8,
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
    /// P2.9: global master kill-switch for deterrence actions (ONVIF relay
    /// siren/strobe/light). OFF by default — an alarm's "deterrence" action does
    /// NOTHING physical until this is on (see `notify::fire_action`), so a rule
    /// can't trip a real-world siren without an explicit, deliberate opt-in.
    #[serde(default)]
    pub deterrence_enabled: bool,
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
    /// Auto-arm/disarm schedule (residential "modes" automation): each entry
    /// flips `arm_mode` at a day+time. Empty = no automation. See `schedule.rs`.
    #[serde(default)]
    pub arm_schedule: Vec<ArmScheduleEntry>,
    /// Path to the YOLOv8-pose ONNX model used by the server-side pose worker
    /// (downloaded, not committed — like the YOLO/YAMNet models). The pose worker
    /// idles until this file exists AND a camera has `pose_detect` on.
    #[serde(default = "default_pose_model")]
    pub pose_model: String,
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
    /// Reverse-proxy SSO (forward auth). When non-empty AND the server runs with
    /// `--trusted-proxy`, a *proxied* request carrying this header is trusted as
    /// that authenticated user (the proxy — Authelia / oauth2-proxy / Cloudflare
    /// Access / Tailscale — already authenticated them). Empty = off. Only honored
    /// on requests that arrived through the proxy (have `X-Forwarded-For`), so a
    /// direct connection can never spoof it.
    #[serde(default)]
    pub auth_proxy_header: String,
    /// Optional header carrying the user's role/group; its value is parsed
    /// leniently (admin/operator/viewer, unknown → viewer). Empty = use the
    /// matched Cammy user's role, else `auth_proxy_default_role`.
    #[serde(default)]
    pub auth_proxy_role_header: String,
    /// Role granted to a forward-auth user with no role header and no matching
    /// Cammy account. Defaults to the least-privileged `viewer`.
    #[serde(default = "default_proxy_role")]
    pub auth_proxy_default_role: String,
    /// Matrix #70 — offsite backup of recordings to S3-compatible object
    /// storage. Off by default; a background worker mirrors sealed segments to
    /// the configured bucket. `offsite_secret_key` is write-only (blanked in
    /// GET /api/settings, preserved on a blank save) like `smtp_pass`.
    #[serde(default)]
    pub offsite_backup_enabled: bool,
    /// S3 endpoint origin, e.g. "https://s3.us-east-1.amazonaws.com" or a
    /// bring-your-own "http://192.168.1.10:9000" (MinIO/NAS — private endpoints
    /// are intentionally allowed). Path-style addressing.
    #[serde(default)]
    pub offsite_endpoint: String,
    #[serde(default = "default_offsite_region")]
    pub offsite_region: String,
    #[serde(default)]
    pub offsite_bucket: String,
    /// Optional key prefix (folder) inside the bucket. Empty = bucket root.
    #[serde(default)]
    pub offsite_prefix: String,
    #[serde(default)]
    pub offsite_access_key: String,
    #[serde(default)]
    pub offsite_secret_key: String,
    /// P2.14 — selective offsite. When set, the backup worker only mirrors
    /// segments that OVERLAP an event window (far less upload/remote storage
    /// than a full continuous mirror). Bookmarked footage is still covered: a
    /// flagged event is an ordinary event row, so its segment always overlaps
    /// an event and is included. Off by default = today's behavior (mirror
    /// every sealed segment). JSON-blob field — no migration.
    #[serde(default)]
    pub offsite_events_only: bool,
    /// Burn an amber outline of the motion region(s) that tripped the gate onto
    /// each detection snapshot (alongside the red object boxes), so a viewer can
    /// see *what actually triggered* an event — wind in the trees vs. the object.
    /// On by default; purely cosmetic on the saved JPEG.
    #[serde(default = "default_true")]
    pub highlight_motion: bool,
    /// P3.6 — number of parallel detection worker threads. Cameras are sharded
    /// across the workers by list position so one camera's slow/blocking frame
    /// fetch can't stall the others. Read ONCE at pipeline startup and fixed for
    /// the process lifetime (a change takes effect after a restart). Clamped to
    /// 1..=8; a 0/absent value resolves to 1 (the default = today's single
    /// detection thread, one shared ONNX session). Each extra worker uses its own
    /// detector session (more RAM/VRAM).
    #[serde(default = "default_detect_workers")]
    pub detect_workers: u32,
    /// P3.2 "Ask your cameras" — explicit opt-in for the natural-language
    /// question/answer tool loop. OFF by default: nothing is ever sent to an LLM
    /// until this is on AND `ask_endpoint` is set (see `capabilities()`).
    #[serde(default)]
    pub ask_enabled: bool,
    /// BYO OpenAI-compatible chat endpoint (a `/v1/chat/completions` base URL —
    /// a local llama.cpp / Ollama OpenAI shim / LM Studio). Empty = none. Use a
    /// LOCAL endpoint to keep questions + event metadata on-prem.
    #[serde(default)]
    pub ask_endpoint: String,
    /// Optional Bearer token for the ask endpoint. TREATED AS A SECRET: write-only
    /// (blanked in GET /api/settings, preserved on a blank save) like `smtp_pass`.
    #[serde(default)]
    pub ask_api_key: String,
    /// Model name for the ask endpoint (e.g. "llama3.1").
    #[serde(default = "default_ask_model")]
    pub ask_model: String,
    /// P3.9 — pull-based two-box archive (disaster recovery). When enabled, a
    /// background worker on THIS (secondary) box PULLS selected cameras'
    /// recording segments FROM another Cammy (the primary) over its HTTP API,
    /// authenticated with an api_tokens Bearer token created on the primary. Off
    /// by default; idle unless enabled AND a primary URL + token are set.
    #[serde(default)]
    pub archive_pull_enabled: bool,
    /// Origin of the primary Cammy to pull from, e.g. "https://nvr.example:8080".
    /// Admin-configured (trusted); validated to be an http(s):// URL.
    #[serde(default)]
    pub archive_primary_url: String,
    /// api_tokens Bearer token minted on the primary (`zoomy_<hex>`). TREATED AS
    /// A SECRET: write-only (blanked in GET /api/settings, preserved on a blank
    /// save) like `offsite_secret_key`.
    #[serde(default)]
    pub archive_token: String,
    /// Comma-separated remote camera names to mirror; empty = every camera the
    /// token is allowed to see on the primary.
    #[serde(default)]
    pub archive_cameras: String,
    /// P3.4 HomeKit (HAP) bridge master switch. When true, the supervised go2rtc
    /// child ALSO runs a local HomeKit accessory server exposing each camera whose
    /// `DetectConfig.homekit_expose` is set as a HomeKit camera (live view).
    /// Admin-gated. Default OFF — and when off (or with no camera exposed) the
    /// generated go2rtc.yaml carries NO `homekit:` section, so it is byte-for-byte
    /// identical to today. v0 = live-view only; pairing requires a real Apple Home
    /// device. See `go2rtc.rs`.
    #[serde(default)]
    pub homekit_enabled: bool,
}

fn default_detect_workers() -> u32 {
    1
}

fn default_ask_model() -> String {
    "llama3.1".into()
}

fn default_arm_mode() -> String {
    "away".into()
}

fn default_notify_min_severity() -> u8 {
    1
}

fn default_pose_model() -> String {
    "yolov8n-pose.onnx".into()
}

fn default_proxy_role() -> String {
    "viewer".into()
}

fn default_offsite_region() -> String {
    "us-east-1".into()
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
            accelerator: String::new(),
            go2rtc_api_port: 1984,
            webhook_url: String::new(),
            record_audio: false,
            alert_labels: ["person"].map(String::from).to_vec(),
            mqtt_url: String::new(),
            mqtt_prefix: "zoomy".into(),
            mqtt_ha_discovery: true,
            mqtt_ha_prefix: "homeassistant".into(),
            mqtt_state_timeout_secs: 30,
            mqtt_commands_enabled: false,
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
            notify_min_severity: 1,
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
            deterrence_enabled: false,
            anomaly_detection: false,
            digest_enabled: false,
            liveviews: Vec::new(),
            floorplan: String::new(),
            arm_mode: default_arm_mode(),
            arm_schedule: Vec::new(),
            pose_model: default_pose_model(),
            smtp_url: String::new(),
            smtp_user: String::new(),
            smtp_pass: String::new(),
            smtp_from: String::new(),
            smtp_to: String::new(),
            auth_proxy_header: String::new(),
            auth_proxy_role_header: String::new(),
            auth_proxy_default_role: default_proxy_role(),
            offsite_backup_enabled: false,
            offsite_endpoint: String::new(),
            offsite_region: default_offsite_region(),
            offsite_bucket: String::new(),
            offsite_prefix: String::new(),
            offsite_access_key: String::new(),
            offsite_secret_key: String::new(),
            offsite_events_only: false,
            highlight_motion: true,
            detect_workers: 1,
            ask_enabled: false,
            ask_endpoint: String::new(),
            ask_api_key: String::new(),
            ask_model: default_ask_model(),
            archive_pull_enabled: false,
            archive_primary_url: String::new(),
            archive_token: String::new(),
            archive_cameras: String::new(),
            homekit_enabled: false,
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
             CREATE INDEX IF NOT EXISTS events_cam_label_ts ON events(camera_id, label, ts DESC);
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
        // Severity tier 1..4 (see crate::severity); NULL rows are re-derived on read.
        let _ = conn.execute("ALTER TABLE events ADD COLUMN severity INTEGER", []);
        // User-applied tags (ZoneMinder 1.38-style): a JSON array of strings.
        let _ = conn.execute("ALTER TABLE events ADD COLUMN tags TEXT", []);
        // P2.16 object lifecycle + docs/08 P1.5: tracker track id and the
        // object's serialized trajectory on tracker-driven narrative events
        // (NULL for ordinary detections / occupancy / package / camera_* events).
        let _ = conn.execute("ALTER TABLE events ADD COLUMN track_id INTEGER", []);
        let _ = conn.execute("ALTER TABLE events ADD COLUMN path_json TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE segments ADD COLUMN reduced INTEGER NOT NULL DEFAULT 0",
            [],
        );
        // P3.7 dual-stream recording: which restream a segment came from —
        // 'main' (full-res, the archive + everything's default) or 'sub' (the
        // opt-in low-res scrub aid). Legacy rows and every existing caller are
        // 'main', so main-stream behavior is byte-for-byte unchanged.
        let _ = conn.execute(
            "ALTER TABLE segments ADD COLUMN stream TEXT NOT NULL DEFAULT 'main'",
            [],
        );
        conn.execute_batch(
            // P2.3 retroactive region motion search: per-camera, per-minute OR of
            // the 64x64 changed-cell motion mask (512-byte bitset). Written by the
            // detection pipeline, read by /api/motion/search. ~0.7 MB/day/camera
            // worst case; pruned alongside recordings retention.
            "CREATE TABLE IF NOT EXISTS motion_grid (
                 camera_id INTEGER NOT NULL,
                 minute_ts INTEGER NOT NULL,
                 cells     BLOB NOT NULL,
                 PRIMARY KEY (camera_id, minute_ts)
             ) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS faces (
                 id         INTEGER PRIMARY KEY,
                 name       TEXT NOT NULL,
                 embedding  BLOB NOT NULL,
                 created_ts INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS event_embeddings (
                 event_id  INTEGER PRIMARY KEY REFERENCES events(id) ON DELETE CASCADE,
                 embedding BLOB NOT NULL
             );
             -- P2.8b per-camera feedback learning: a thumbs-down on an alert
             -- stores that object crop's CLIP embedding so future CLIP-similar
             -- alerts on the SAME camera + SAME label can be quieted. `label` is
             -- stored so a person false-positive never gates a car rule. Self-
             -- trimmed per (camera_id,label); `event_id` is only provenance (the
             -- source event may be pruned, so nullable, no FK cascade).
             CREATE TABLE IF NOT EXISTS alert_feedback (
                 id         INTEGER PRIMARY KEY,
                 camera_id  INTEGER NOT NULL,
                 event_id   INTEGER,
                 label      TEXT NOT NULL,
                 embedding  BLOB NOT NULL,
                 created_ts INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_alert_feedback_cam_label
                 ON alert_feedback(camera_id, label);
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
             CREATE TABLE IF NOT EXISTS clip_shares (
                 id         INTEGER PRIMARY KEY,
                 token_hash TEXT NOT NULL UNIQUE,
                 event_id   INTEGER NOT NULL,
                 pre        INTEGER NOT NULL,
                 post       INTEGER NOT NULL,
                 expires_ts INTEGER NOT NULL,
                 revoked    INTEGER NOT NULL DEFAULT 0,
                 created_ts INTEGER NOT NULL,
                 label      TEXT,
                 camera     TEXT
             );
             CREATE INDEX IF NOT EXISTS clip_shares_hash ON clip_shares(token_hash);
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
             );
             CREATE TABLE IF NOT EXISTS push_subscriptions (
                 id         INTEGER PRIMARY KEY,
                 endpoint   TEXT NOT NULL UNIQUE,
                 p256dh     TEXT NOT NULL,
                 auth       TEXT NOT NULL,
                 created_ts INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS offsite_uploads (
                 path        TEXT PRIMARY KEY,
                 camera      TEXT NOT NULL,
                 key         TEXT NOT NULL,
                 bytes       INTEGER NOT NULL DEFAULT 0,
                 status      TEXT NOT NULL,
                 attempts    INTEGER NOT NULL DEFAULT 0,
                 last_error  TEXT,
                 updated_ts  INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_offsite_status ON offsite_uploads(status);
             -- Keep offsite_uploads strictly bounded by the live segment set: any
             -- segment delete drops its upload row. This is the single source of
             -- truth for cleanup — it fires on a direct DELETE (retention) AND on
             -- the cameras->segments ON DELETE CASCADE (camera removal), which a
             -- bare DELETE FROM cameras would otherwise leave orphaned, silently
             -- inflating the status/Prometheus counts forever (#70).
             CREATE TRIGGER IF NOT EXISTS trg_offsite_seg_del
                 AFTER DELETE ON segments
                 BEGIN DELETE FROM offsite_uploads WHERE path = OLD.path; END;",
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
        // Per-user TOTP 2FA: base32 secret, an enabled flag (a secret can be
        // set-but-not-yet-confirmed during enrollment), and a JSON array of
        // SHA-256-hashed one-time recovery codes. The shared single-password
        // admin keeps its equivalent under the settings KV (see auth::KV_TOTP_*).
        let _ = conn.execute("ALTER TABLE users ADD COLUMN totp_secret TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE users ADD COLUMN totp_enabled INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute("ALTER TABLE users ADD COLUMN totp_recovery TEXT", []);
        // Last TOTP time-step accepted for this user, to refuse intra-window
        // replay of a code (a code is valid for ~90 s across the skew window).
        let _ = conn.execute("ALTER TABLE users ADD COLUMN totp_last_step INTEGER", []);
        // P2.11 per-user notification matrix: an optional email address for
        // per-user email delivery; a nullable owner on each push subscription
        // (legacy rows stay NULL = anonymous/unrestricted, preserving today's
        // fan-out-to-everyone behaviour); and rule/camera tags on notifications
        // so the push worker can route per user × rule × channel. Every existing
        // add_notification caller leaves rule_id/camera_id NULL.
        let _ = conn.execute("ALTER TABLE users ADD COLUMN email TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE push_subscriptions ADD COLUMN user_id INTEGER",
            [],
        );
        let _ = conn.execute("ALTER TABLE notifications ADD COLUMN rule_id INTEGER", []);
        let _ = conn.execute("ALTER TABLE notifications ADD COLUMN camera_id INTEGER", []);
        // The severity tier (1..4) of an alarm fire, so the push worker can honour
        // `notify_min_severity` on the per-user push/email channels. NULL on
        // system notifications (offline/anomaly/digest) — those aren't gated.
        let _ = conn.execute("ALTER TABLE notifications ADD COLUMN severity INTEGER", []);
        // One-time: clear stale unowned push subscriptions from before `user_id`
        // existed. Left as-is they'd deliver as anonymous = unrestricted forever —
        // a durable camera-scope leak to whoever's browser was subscribed pre-
        // upgrade. Guarded by a KV marker so this wipes EXACTLY once (never on a
        // later startup, which would nuke freshly-owned subs). Browsers self-heal
        // by re-subscribing on next app load, stamping the current user_id.
        let reset_done = conn
            .query_row(
                "SELECT 1 FROM settings WHERE key = 'p211_pushsub_reset'",
                [],
                |_| Ok(()),
            )
            .is_ok();
        if !reset_done {
            let _ = conn.execute("DELETE FROM push_subscriptions", []);
            let _ = conn.execute(
                "INSERT INTO settings (key, value) VALUES ('p211_pushsub_reset', '1')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            );
        }
        // Gait identification (#64): the attributed walking identity on an event
        // (a name or the `?` unknown sentinel) plus its raw signature JSON (kept
        // so an unknown walker can be enrolled straight from the event).
        let _ = conn.execute("ALTER TABLE events ADD COLUMN gait TEXT", []);
        let _ = conn.execute("ALTER TABLE events ADD COLUMN gait_sig TEXT", []);
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS gait_profiles (
                 id         INTEGER PRIMARY KEY,
                 name       TEXT NOT NULL UNIQUE,
                 signature  TEXT NOT NULL,
                 samples    INTEGER NOT NULL DEFAULT 1,
                 created_ts INTEGER NOT NULL,
                 updated_ts INTEGER NOT NULL
             );",
        )?;
        // Per-camera RBAC scoping (#66): a non-admin user may be restricted to a
        // subset of cameras. No rows for a user = unrestricted (sees all), so
        // existing accounts are unaffected. Rows cascade-delete with the user or
        // the camera.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS user_cameras (
                 user_id   INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                 camera_id INTEGER NOT NULL REFERENCES cameras(id) ON DELETE CASCADE,
                 PRIMARY KEY (user_id, camera_id)
             );",
        )?;
        // Presence/geofence arming (P2.10): one row per tracked occupant. A phone
        // or automation flips `home` via the presence API; first-in/last-out then
        // derives the system arm mode (any occupant home ⇒ "home", nobody ⇒ "away").
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS occupants (
                 id         INTEGER PRIMARY KEY,
                 name       TEXT NOT NULL UNIQUE,
                 home       INTEGER NOT NULL DEFAULT 0,
                 updated_ts INTEGER NOT NULL,
                 created_ts INTEGER NOT NULL
             );",
        )?;
        // P2.11 per-user notification preferences: which alarm rules reach which
        // user over which channel. Opt-OUT model — no row for a (user, rule,
        // channel) triple means enabled (mirrors user_cameras' "no rows =
        // unrestricted"). rule_id 0 is the user's default applied to every rule
        // without a specific override. Rows cascade-delete with the user.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS notify_prefs (
                 user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                 rule_id INTEGER NOT NULL DEFAULT 0,
                 channel TEXT NOT NULL,
                 enabled INTEGER NOT NULL,
                 PRIMARY KEY (user_id, rule_id, channel)
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
            None, None,
        )
    }

    /// Like [`add_event`](Self::add_event) but also records a line-crossing
    /// `direction`, an estimated `speed` (km/h), and — for tracker-driven
    /// narrative events — the object's `track_id` and serialized trajectory
    /// (`path_json`), which power the object-lifecycle view (P2.16 / docs/08 P1.5).
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
        track_id: Option<i64>,
        path_json: Option<&str>,
    ) -> Result<i64> {
        let severity = crate::severity::severity_for(label, face, gesture) as i64;
        let conn = self.conn();
        conn.execute(
            "INSERT INTO events (camera_id, ts, label, score, x1, y1, x2, y2, snapshot, face, plate, gesture, zone, direction, speed, severity, track_id, path_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                camera_id, ts, label, score, bbox[0], bbox[1], bbox[2], bbox[3], snapshot, face,
                plate, gesture, zone, direction, speed, severity, track_id, path_json
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
                    e.flagged, e.note, e.anomaly_score, e.direction, e.speed, e.gait, e.severity, e.tags, e.track_id, e.path_json
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
                        e.flagged, e.note, e.anomaly_score, e.direction, e.speed, e.gait, e.severity, e.tags, e.track_id, e.path_json
                 FROM events e JOIN cameras c ON c.id = e.camera_id WHERE e.id = ?1",
                [id],
                row_to_event,
            )
            .optional()?;
        Ok(ev)
    }

    /// The ordered life-story (oldest→newest) of one physical object: every
    /// tracker-driven narrative event carrying `track_id` on this camera, bounded
    /// to the contiguous run around `around_ts`. Powers the object-lifecycle view.
    ///
    /// CORRECTNESS: per-camera track ids reset to 1 on every service restart (the
    /// in-memory [`tracker::Tracker`] map is rebuilt), so two entirely different
    /// physical objects can reuse the same id hours or days apart. A naive
    /// `WHERE camera_id=? AND track_id=?` would merge them into one bogus story.
    /// We therefore fetch all candidate rows for the id, then keep only the
    /// cluster whose consecutive gaps are `<= LIFECYCLE_GAP_SECS`, walking outward
    /// from the seed event nearest `around_ts` (see [`cluster_bounds`]).
    pub fn track_lifecycle(
        &self,
        camera_id: i64,
        track_id: i64,
        around_ts: i64,
    ) -> Result<Vec<Event>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT e.id, e.camera_id, c.name, e.ts, e.label, e.score,
                    e.x1, e.y1, e.x2, e.y2, e.snapshot, e.face, e.plate, e.gesture, e.zone, e.caption, e.transcript,
                    e.flagged, e.note, e.anomaly_score, e.direction, e.speed, e.gait, e.severity, e.tags, e.track_id, e.path_json
             FROM events e JOIN cameras c ON c.id = e.camera_id
             WHERE e.camera_id = ?1 AND e.track_id = ?2
             ORDER BY e.ts ASC, e.id ASC",
        )?;
        let rows: Vec<Event> = stmt
            .query_map(params![camera_id, track_id], row_to_event)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.is_empty() {
            return Ok(rows);
        }
        // Seed = the candidate nearest `around_ts` (the event the user opened).
        let ts: Vec<i64> = rows.iter().map(|e| e.ts).collect();
        let seed = ts
            .iter()
            .enumerate()
            .min_by_key(|(_, &t)| (t - around_ts).abs())
            .map(|(i, _)| i)
            .unwrap_or(0);
        let (lo, hi) = cluster_bounds(&ts, seed, LIFECYCLE_GAP_SECS);
        Ok(rows[lo..=hi].to_vec())
    }

    // --- gait identification (#64) --------------------------------------------

    /// Attach a gait identity (+ raw signature JSON) to an event. Returns whether
    /// the event existed.
    pub fn set_event_gait(
        &self,
        id: i64,
        gait: Option<&str>,
        sig_json: Option<&str>,
    ) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE events SET gait = ?1, gait_sig = ?2 WHERE id = ?3",
            params![gait, sig_json, id],
        )?;
        Ok(n > 0)
    }

    /// The stored gait signature JSON for an event (used to enroll a profile).
    pub fn event_gait_sig(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT gait_sig FROM events WHERE id = ?1", [id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten())
    }

    /// Recent unknown-walker events (gait = `?`, signature present) — the
    /// enrollment candidates for the gait UI.
    pub fn unknown_gait_events(&self, limit: u32) -> Result<Vec<Event>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT e.id, e.camera_id, c.name, e.ts, e.label, e.score,
                    e.x1, e.y1, e.x2, e.y2, e.snapshot, e.face, e.plate, e.gesture, e.zone, e.caption, e.transcript,
                    e.flagged, e.note, e.anomaly_score, e.direction, e.speed, e.gait, e.severity, e.tags, e.track_id, e.path_json
             FROM events e JOIN cameras c ON c.id = e.camera_id
             WHERE e.gait = ?1 AND e.gait_sig IS NOT NULL
             ORDER BY e.ts DESC, e.id DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![UNKNOWN_GAIT, limit], row_to_event)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// All enrolled gait profiles (metadata only — signatures stay server-side).
    pub fn list_gait_profiles(&self) -> Result<Vec<GaitProfileRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, samples, created_ts, updated_ts FROM gait_profiles ORDER BY name",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(GaitProfileRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    samples: r.get(2)?,
                    created_ts: r.get(3)?,
                    updated_ts: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// `(name, signature)` for every enrolled profile, for the gait matcher.
    pub fn gait_profile_sigs(&self) -> Result<Vec<(String, Vec<f32>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT name, signature FROM gait_profiles")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(n, s)| serde_json::from_str::<Vec<f32>>(&s).ok().map(|v| (n, v)))
            .collect())
    }

    /// Enroll / merge a gait signature under `name` (running average), returning
    /// the profile id. The read-modify-write runs under the single DB lock.
    pub fn enroll_gait(&self, name: &str, sig: &[f32], now: i64) -> Result<i64> {
        let conn = self.conn();
        let existing: Option<(i64, String, i64)> = conn
            .query_row(
                "SELECT id, signature, samples FROM gait_profiles WHERE name = ?1",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((id, old_json, samples)) = existing {
            let old: Vec<f32> = serde_json::from_str(&old_json).unwrap_or_default();
            let merged: Vec<f32> = if old.len() == sig.len() {
                let n = samples.max(0) as f32;
                old.iter()
                    .zip(sig)
                    .map(|(o, s)| (o * n + s) / (n + 1.0))
                    .collect()
            } else {
                sig.to_vec()
            };
            let mj = serde_json::to_string(&merged).unwrap_or_else(|_| "[]".into());
            conn.execute(
                "UPDATE gait_profiles SET signature = ?1, samples = samples + 1, updated_ts = ?2 WHERE id = ?3",
                params![mj, now, id],
            )?;
            Ok(id)
        } else {
            let sj = serde_json::to_string(sig).unwrap_or_else(|_| "[]".into());
            conn.execute(
                "INSERT INTO gait_profiles (name, signature, samples, created_ts, updated_ts) VALUES (?1, ?2, 1, ?3, ?3)",
                params![name, sj, now],
            )?;
            Ok(conn.last_insert_rowid())
        }
    }

    pub fn rename_gait(&self, id: i64, name: &str) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE gait_profiles SET name = ?1 WHERE id = ?2",
            params![name, id],
        )?;
        Ok(n > 0)
    }

    pub fn delete_gait(&self, id: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM gait_profiles WHERE id = ?1", [id])?;
        Ok(())
    }

    // --- presence / geofence arming (P2.10) --------------------------------

    /// Record an occupant's home/away state, creating the row on first sighting.
    pub fn upsert_occupant(&self, name: &str, home: bool, now: i64) -> Result<()> {
        self.conn().execute(
            "INSERT INTO occupants (name, home, updated_ts, created_ts) VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(name) DO UPDATE SET home = ?2, updated_ts = ?3",
            params![name, home as i64, now],
        )?;
        Ok(())
    }

    /// How many occupants are currently home (drives the derived arm mode).
    pub fn count_home_occupants(&self) -> Result<i64> {
        let n =
            self.conn()
                .query_row("SELECT COUNT(*) FROM occupants WHERE home = 1", [], |r| {
                    r.get(0)
                })?;
        Ok(n)
    }

    pub fn list_occupants(&self) -> Result<Vec<Occupant>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, home, updated_ts, created_ts FROM occupants ORDER BY name",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Occupant {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    home: r.get::<_, i64>(2)? != 0,
                    updated_ts: r.get(3)?,
                    created_ts: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Remove a tracked occupant. Returns whether a row was deleted.
    pub fn delete_occupant(&self, id: i64) -> Result<bool> {
        let n = self
            .conn()
            .execute("DELETE FROM occupants WHERE id = ?1", [id])?;
        Ok(n > 0)
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
        allowed: Option<&std::collections::HashSet<i64>>,
    ) -> Result<serde_json::Value> {
        let conn = self.conn();
        // Per-camera RBAC: restrict to the caller's allowed cameras (ids are our
        // own i64s -> inline IN-list is injection-safe). `None` = unrestricted;
        // `Some` is always non-empty (allowed_cameras maps empty -> None).
        let cam = match allowed {
            Some(ids) if !ids.is_empty() => format!(
                " AND camera_id IN ({})",
                ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
            ),
            _ => String::new(),
        };
        // Count both normal and wrong-way crossings: a wrong-way pass is still a
        // physical pass through the line, so it must count toward throughput
        // (it also carries a real `direction`). Excluding it under-reports.
        let mut cs = conn.prepare(&format!(
            "SELECT zone, direction, COUNT(*) FROM events
             WHERE label IN ('crossing', 'wrong_way') AND (?1 IS NULL OR ts >= ?1) AND (?2 IS NULL OR ts < ?2){cam}
             GROUP BY zone, direction ORDER BY zone, direction"
        ))?;
        let crossings: Vec<serde_json::Value> = cs
            .query_map(params![from, to], |r| {
                Ok(serde_json::json!({
                    "tripwire": r.get::<_, Option<String>>(0)?,
                    "direction": r.get::<_, Option<String>>(1)?,
                    "count": r.get::<_, i64>(2)?,
                }))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut ls = conn.prepare(&format!(
            "SELECT zone, COUNT(*) FROM events
             WHERE label = 'loiter' AND (?1 IS NULL OR ts >= ?1) AND (?2 IS NULL OR ts < ?2){cam}
             GROUP BY zone ORDER BY zone"
        ))?;
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

    /// Event trends over the last `days` local calendar days: per-day totals
    /// (zero-filled), top labels, and hour-of-day distribution — all computed in
    /// SQL (GROUP BY over the ts index) so the Insights page never pulls raw
    /// events to the browser. RBAC-scoped by the caller's allowed cameras.
    pub fn events_timeseries(
        &self,
        days: i64,
        allowed: Option<&std::collections::HashSet<i64>>,
    ) -> Result<serde_json::Value> {
        use chrono::{Duration as CDur, Local, TimeZone};
        let days = days.clamp(1, 90);
        let conn = self.conn();
        // Per-camera RBAC (ids are our own i64s -> inline IN-list is injection-safe).
        let cam = match allowed {
            Some(ids) if !ids.is_empty() => format!(
                " AND camera_id IN ({})",
                ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
            ),
            _ => String::new(),
        };
        // Local calendar boundaries so buckets line up with the user's days.
        let today = Local::now().date_naive();
        let start_date = today - CDur::days(days - 1);
        let local_midnight = |d: chrono::NaiveDate| -> i64 {
            d.and_hms_opt(0, 0, 0)
                .and_then(|ndt| Local.from_local_datetime(&ndt).single())
                .map(|dt| dt.timestamp())
                .unwrap_or(0)
        };
        let from_ts = local_midnight(start_date);

        // Per local day (only non-empty days come back; we zero-fill below).
        let mut per_day: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        {
            let mut stmt = conn.prepare(&format!(
                "SELECT date(ts,'unixepoch','localtime') d, COUNT(*) FROM events
                 WHERE ts >= ?1{cam} GROUP BY d"
            ))?;
            let rows = stmt.query_map([from_ts], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (d, c) = row?;
                per_day.insert(d, c);
            }
        }
        let mut day_series = Vec::with_capacity(days as usize);
        let mut total = 0i64;
        for i in 0..days {
            let d = start_date + CDur::days(i);
            let count = per_day
                .get(&d.format("%Y-%m-%d").to_string())
                .copied()
                .unwrap_or(0);
            total += count;
            day_series.push(serde_json::json!({
                "day": d.format("%m/%d").to_string(),
                "ts": local_midnight(d),
                "count": count,
            }));
        }

        // Top labels over the range.
        let mut by_label = Vec::new();
        {
            let mut stmt = conn.prepare(&format!(
                "SELECT label, COUNT(*) c FROM events WHERE ts >= ?1{cam}
                 GROUP BY label ORDER BY c DESC LIMIT 16"
            ))?;
            let rows = stmt.query_map([from_ts], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (l, c) = row?;
                by_label.push(serde_json::json!([l, c]));
            }
        }

        // Hour-of-day distribution (local), 0..23.
        let mut by_hour = vec![0i64; 24];
        {
            let mut stmt = conn.prepare(&format!(
                "SELECT CAST(strftime('%H', ts,'unixepoch','localtime') AS INTEGER) h, COUNT(*)
                 FROM events WHERE ts >= ?1{cam} GROUP BY h"
            ))?;
            let rows = stmt.query_map([from_ts], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (h, c) = row?;
                if (0..24).contains(&h) {
                    by_hour[h as usize] = c;
                }
            }
        }

        Ok(serde_json::json!({
            "days": day_series,
            "by_label": by_label,
            "by_hour": by_hour,
            "total": total,
            "from": from_ts,
            "range_days": days,
        }))
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
            // Residential zone scope + cross-modal confirmation ride the schedule
            // blob (no migration), like modes.
            "zone_like": r.zone_like,
            "confirm_label": r.confirm_label,
            "confirm_within": r.confirm_within_secs,
            "vlm_prompt": r.vlm_prompt,
            "describe": r.describe,
            "prompt_like": r.prompt_like,
            "attr_like": r.attr_like
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

    /// Replace an existing rule's definition in place (name/conditions/actions/
    /// schedule). Preserves the rule's id, created_ts and runtime snooze state —
    /// this edits the rule, not its live snooze. Returns false if no such id.
    pub fn update_alarm(&self, id: i64, r: &AlarmRule) -> Result<bool> {
        let schedule = serde_json::json!({
            "days": r.days, "start": r.start_hhmm, "end": r.end_hhmm, "modes": r.modes,
            "zone_like": r.zone_like,
            "confirm_label": r.confirm_label,
            "confirm_within": r.confirm_within_secs,
            "vlm_prompt": r.vlm_prompt,
            "describe": r.describe,
            "prompt_like": r.prompt_like,
            "attr_like": r.attr_like
        })
        .to_string();
        let actions = r.effective_actions();
        let actions_json = serde_json::to_string(&actions).unwrap_or_else(|_| "[]".into());
        let primary = &actions[0];
        let n = self.conn().execute(
            "UPDATE alarms SET name=?2, enabled=?3, camera_id=?4, label=?5, face_like=?6,
             plate_like=?7, gesture_like=?8, min_score=?9, action=?10, target=?11,
             schedule_json=?12, cooldown_secs=?13, priority=?14, transcript_like=?15,
             face_unknown=?16, actions_json=?17 WHERE id=?1",
            params![
                id,
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
                r.transcript_like,
                r.face_unknown as i64,
                actions_json
            ],
        )?;
        Ok(n > 0)
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
                    confirm_label: sched["confirm_label"].as_str().map(str::to_string),
                    confirm_within_secs: sched["confirm_within"].as_i64(),
                    vlm_prompt: sched["vlm_prompt"].as_str().map(str::to_string),
                    describe: sched["describe"].as_bool().unwrap_or(false),
                    prompt_like: sched["prompt_like"].as_str().map(str::to_string),
                    attr_like: sched["attr_like"].as_str().map(str::to_string),
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

    /// Segments of `camera_id` starting before `older_than` whose span
    /// `[start_ts, start_ts + span_secs]` overlaps NO event's keep window
    /// `[event.ts - margin_before, event.ts + margin_after]` — the deletion set
    /// for event-bracketed retention (event-only and detection-triggered modes).
    ///
    /// The margins are asymmetric so detection-triggered recording can keep a
    /// short pre-roll and a longer post-roll around each detection. A segment
    /// overlaps an event's window iff
    /// `event.ts BETWEEN start_ts - margin_after AND start_ts + span_secs + margin_before`,
    /// so `margin_before` (pre-roll) reaches back to segments that START before
    /// the event and `margin_after` (post-roll) forward to segments after it.
    /// Passing `(span, span)` reproduces the original symmetric event-only
    /// window byte-for-byte. Flagged/bookmarked events are ordinary event rows
    /// (never pruned from `events`), so the segment holding one is always kept.
    pub fn eventless_segments(
        &self,
        camera_id: i64,
        older_than: i64,
        span_secs: i64,
        margin_before: i64,
        margin_after: i64,
    ) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT s.path FROM segments s
             WHERE s.camera_id = ?1 AND s.start_ts < ?2
               AND NOT EXISTS (
                 SELECT 1 FROM events e
                 WHERE e.camera_id = s.camera_id
                   AND e.ts BETWEEN s.start_ts - ?5 AND s.start_ts + ?3 + ?4
               )",
        )?;
        let rows = stmt
            .query_map(
                params![
                    camera_id,
                    older_than,
                    span_secs,
                    margin_before,
                    margin_after
                ],
                |r| r.get(0),
            )?
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

    // --- shareable clip links (P2.7) ---------------------------------------

    /// Store a new clip share (only its token hash) and prune long-expired rows
    /// so the table stays bounded. Returns the new row id.
    #[allow(clippy::too_many_arguments)]
    pub fn add_clip_share(
        &self,
        token_hash: &str,
        event_id: i64,
        pre: i64,
        post: i64,
        expires_ts: i64,
        label: Option<&str>,
        camera: Option<&str>,
        now: i64,
    ) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO clip_shares (token_hash, event_id, pre, post, expires_ts, created_ts, label, camera)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![token_hash, event_id, pre, post, expires_ts, now, label, camera],
        )?;
        let id = conn.last_insert_rowid();
        // Bound the table: drop shares that expired over a day ago.
        let _ = conn.execute(
            "DELETE FROM clip_shares WHERE expires_ts < ?1",
            [now - 86400],
        );
        Ok(id)
    }

    /// Resolve an ACTIVE share (not revoked, not expired) from its token hash,
    /// for the public /share route. `None` = no such active share.
    pub fn get_active_clip_share(&self, token_hash: &str, now: i64) -> Result<Option<ShareTarget>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT event_id, pre, post FROM clip_shares
                 WHERE token_hash = ?1 AND revoked = 0 AND expires_ts > ?2",
                params![token_hash, now],
                |r| {
                    Ok(ShareTarget {
                        event_id: r.get(0)?,
                        pre: r.get(1)?,
                        post: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// List shares (metadata only — never the token or its hash) for the manage UI.
    pub fn list_clip_shares(&self) -> Result<Vec<ClipShare>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, event_id, label, camera, expires_ts, revoked, created_ts
             FROM clip_shares ORDER BY id DESC LIMIT 500",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ClipShare {
                    id: r.get(0)?,
                    event_id: r.get(1)?,
                    label: r.get(2)?,
                    camera: r.get(3)?,
                    expires_ts: r.get(4)?,
                    revoked: r.get::<_, i64>(5)? != 0,
                    created_ts: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Revoke a share (invalidates the public link immediately).
    pub fn revoke_clip_share(&self, id: i64) -> Result<bool> {
        let n = self
            .conn()
            .execute("UPDATE clip_shares SET revoked = 1 WHERE id = ?1", [id])?;
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
    /// Absolute row ceiling regardless of read state — a headless / API-token
    /// deployment never opens the Notifications panel, so nothing is ever marked
    /// read and the read-only trim never fires; well above NOTIF_KEEP so normal
    /// UI installs never hit it.
    const NOTIF_HARD_CAP: i64 = 20_000;

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
        // Hard ceiling so the table can't grow without bound when notifications
        // are never read (headless / token deployments) — matches add_audit.
        let _ = conn.execute(
            "DELETE FROM notifications WHERE id <= \
             (SELECT MAX(id) FROM notifications) - ?1",
            [Self::NOTIF_HARD_CAP],
        );
        Ok(id)
    }

    /// P2.11 alarm-tagged notification, written by `notify::fire` for every rule
    /// fire so the push worker can route it per user × rule × camera. Identical
    /// to [`add_notification`] plus the `rule_id`/`camera_id` tags — kept separate
    /// so every existing (system-notification) caller stays byte-for-byte the same.
    #[allow(clippy::too_many_arguments)]
    pub fn add_alarm_notification(
        &self,
        ts: i64,
        kind: &str,
        title: &str,
        body: Option<&str>,
        event_id: Option<i64>,
        rule_id: Option<i64>,
        camera_id: Option<i64>,
        severity: Option<i64>,
    ) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO notifications (ts, kind, title, body, event_id, rule_id, camera_id, severity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![ts, kind, title, body, event_id, rule_id, camera_id, severity],
        )?;
        let id = conn.last_insert_rowid();
        let _ = conn.execute(
            "DELETE FROM notifications WHERE read = 1 AND id <= \
             (SELECT MAX(id) FROM notifications) - ?1",
            [Self::NOTIF_KEEP],
        );
        let _ = conn.execute(
            "DELETE FROM notifications WHERE id <= \
             (SELECT MAX(id) FROM notifications) - ?1",
            [Self::NOTIF_HARD_CAP],
        );
        Ok(id)
    }

    /// Newest-first notifications; when `unread_only`, only rows with read = 0.
    pub fn list_notifications(&self, unread_only: bool, limit: u32) -> Result<Vec<Notification>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, ts, kind, title, body, event_id, read, rule_id, camera_id, severity FROM notifications
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
                    rule_id: r.get(7)?,
                    camera_id: r.get(8)?,
                    severity: r.get(9)?,
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

    /// The highest notification id (0 if none) — the WebPush worker starts from
    /// here so a fresh subscriber doesn't get the whole backlog pushed at once.
    pub fn max_notification_id(&self) -> i64 {
        self.conn()
            .query_row("SELECT COALESCE(MAX(id), 0) FROM notifications", [], |r| {
                r.get(0)
            })
            .unwrap_or(0)
    }

    /// Notifications created after `id`, oldest-first (for ordered fan-out).
    pub fn notifications_after(&self, id: i64, limit: u32) -> Result<Vec<Notification>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, ts, kind, title, body, event_id, read, rule_id, camera_id, severity FROM notifications
             WHERE id > ?1 ORDER BY id ASC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![id, limit], |r| {
                Ok(Notification {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    kind: r.get(2)?,
                    title: r.get(3)?,
                    body: r.get(4)?,
                    event_id: r.get(5)?,
                    read: r.get::<_, i64>(6)? != 0,
                    rule_id: r.get(7)?,
                    camera_id: r.get(8)?,
                    severity: r.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // --- WebPush subscriptions (#68) ----------------------------------------

    /// Store (or refresh) a browser push subscription, keyed by its endpoint.
    /// `user_id` is the subscribing account (P2.11) so the push worker can route
    /// per user; `None` for the loopback/legacy single-admin or open mode, which
    /// keeps the subscription anonymous (unrestricted — today's behaviour).
    pub fn add_push_subscription(
        &self,
        endpoint: &str,
        p256dh: &str,
        auth: &str,
        user_id: Option<i64>,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO push_subscriptions (endpoint, p256dh, auth, created_ts, user_id)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(endpoint) DO UPDATE SET p256dh = ?2, auth = ?3, user_id = ?5",
            params![
                endpoint,
                p256dh,
                auth,
                chrono::Local::now().timestamp(),
                user_id
            ],
        )?;
        Ok(())
    }

    pub fn list_push_subscriptions(&self) -> Result<Vec<crate::webpush::PushSub>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT endpoint, p256dh, auth, user_id FROM push_subscriptions ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(crate::webpush::PushSub {
                    endpoint: r.get(0)?,
                    p256dh: r.get(1)?,
                    auth: r.get(2)?,
                    user_id: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Remove a subscription by endpoint; returns whether a row existed.
    pub fn delete_push_subscription(&self, endpoint: &str) -> Result<bool> {
        let n = self.conn().execute(
            "DELETE FROM push_subscriptions WHERE endpoint = ?1",
            [endpoint],
        )?;
        Ok(n > 0)
    }

    pub fn count_push_subscriptions(&self) -> i64 {
        self.conn()
            .query_row("SELECT COUNT(*) FROM push_subscriptions", [], |r| r.get(0))
            .unwrap_or(0)
    }

    pub fn push_subscription_exists(&self, endpoint: &str) -> bool {
        self.conn()
            .query_row(
                "SELECT 1 FROM push_subscriptions WHERE endpoint = ?1",
                [endpoint],
                |_| Ok(()),
            )
            .is_ok()
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
        let mut stmt = conn
            .prepare("SELECT id, username, role, created_ts, email FROM users ORDER BY username")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(UserRow {
                    id: r.get(0)?,
                    username: r.get(1)?,
                    role: r.get(2)?,
                    created_ts: r.get(3)?,
                    email: r.get(4)?,
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

    /// A user's 2FA config `(secret_base32, enabled, recovery_codes_json)`.
    /// `None` if the user doesn't exist; an inner `secret` of `None` means 2FA
    /// has never been set up for them.
    pub fn user_totp(&self, id: i64) -> Result<Option<TotpConfig>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT totp_secret, totp_enabled, totp_recovery FROM users WHERE id = ?1",
                [id],
                |r| {
                    let secret: Option<String> = r.get(0)?;
                    let enabled: i64 = r.get(1)?;
                    let recovery: Option<String> = r.get(2)?;
                    Ok((secret, enabled != 0, recovery))
                },
            )
            .optional()?)
    }

    /// Replace a user's 2FA config (secret/enabled/recovery in one write). Always
    /// resets the replay watermark (`totp_last_step`) — used on setup/enable
    /// (fresh secret) and disable/reset (cleared). Returns whether the user
    /// existed.
    pub fn set_user_totp(
        &self,
        id: i64,
        secret: Option<&str>,
        enabled: bool,
        recovery: Option<&str>,
    ) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE users SET totp_secret = ?1, totp_enabled = ?2, totp_recovery = ?3, totp_last_step = NULL WHERE id = ?4",
            params![secret, enabled as i64, recovery, id],
        )?;
        Ok(n > 0)
    }

    /// Atomic replay guard for a user: accept `step` only if it is newer than the
    /// stored watermark, advancing the watermark in the SAME statement (compare-
    /// and-set under one DB lock). Returns true if accepted. Doing the check and
    /// the advance as one statement is what stops two concurrent logins carrying
    /// the same still-valid code from both passing the guard (a read-then-write
    /// across two lock acquisitions could let both win).
    pub fn advance_user_totp_step(&self, id: i64, step: i64) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE users SET totp_last_step = ?1
             WHERE id = ?2 AND (totp_last_step IS NULL OR totp_last_step < ?1)",
            params![step, id],
        )?;
        Ok(n > 0)
    }

    /// Atomic replay guard for the shared single-password admin (KV-backed),
    /// mirroring [`advance_user_totp_step`]: the read, compare, and conditional
    /// write all run while holding the single DB lock.
    pub fn advance_kv_totp_step(&self, key: &str, step: i64) -> Result<bool> {
        let conn = self.conn();
        let current: Option<i64> = conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .and_then(|s| s.parse::<i64>().ok());
        if current.is_some_and(|c| c >= step) {
            return Ok(false);
        }
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, step.to_string()],
        )?;
        Ok(true)
    }

    /// Consume one recovery code for a user given its hash: rewrite the stored
    /// JSON array without that hash. Returns true if a code was removed. The
    /// read-modify-write runs under the single DB lock so two parallel uses of
    /// the same code can't both succeed.
    pub fn consume_user_recovery(&self, id: i64, code_hash: &str) -> Result<bool> {
        let conn = self.conn();
        let current: Option<String> = conn
            .query_row("SELECT totp_recovery FROM users WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .flatten();
        let mut hashes: Vec<String> = current
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let before = hashes.len();
        hashes.retain(|h| h != code_hash);
        if hashes.len() == before {
            return Ok(false);
        }
        let json = serde_json::to_string(&hashes).unwrap_or_else(|_| "[]".into());
        conn.execute(
            "UPDATE users SET totp_recovery = ?1 WHERE id = ?2",
            params![json, id],
        )?;
        Ok(true)
    }

    /// Consume one recovery code from a settings-KV JSON array (the shared
    /// single-password admin's recovery set). Atomic under the single DB lock.
    pub fn consume_kv_recovery(&self, key: &str, code_hash: &str) -> Result<bool> {
        let conn = self.conn();
        let current: Option<String> = conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        let mut hashes: Vec<String> = current
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let before = hashes.len();
        hashes.retain(|h| h != code_hash);
        if hashes.len() == before {
            return Ok(false);
        }
        let json = serde_json::to_string(&hashes).unwrap_or_else(|_| "[]".into());
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, json],
        )?;
        Ok(true)
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

    // --- per-camera RBAC scoping (#66) -----------------------------------

    /// A user's camera allow-list (camera ids). An **empty** list means the user
    /// is unrestricted (sees every camera) — so existing accounts are unaffected.
    pub fn list_user_cameras(&self, user_id: i64) -> Result<Vec<i64>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT camera_id FROM user_cameras WHERE user_id = ?1 ORDER BY camera_id")?;
        let rows = stmt
            .query_map([user_id], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Replace a user's camera allow-list (atomically). An empty slice clears it
    /// (back to unrestricted). Non-existent camera ids are silently skipped.
    /// Returns whether the user exists.
    pub fn set_user_cameras(&self, user_id: i64, camera_ids: &[i64]) -> Result<bool> {
        let mut conn = self.conn();
        let exists = conn
            .query_row("SELECT 1 FROM users WHERE id = ?1", [user_id], |_| Ok(()))
            .optional()?
            .is_some();
        if !exists {
            return Ok(false);
        }
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM user_cameras WHERE user_id = ?1", [user_id])?;
        {
            let mut ins = tx.prepare(
                "INSERT OR IGNORE INTO user_cameras (user_id, camera_id)
                 SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM cameras WHERE id = ?2)",
            )?;
            for &cid in camera_ids {
                ins.execute(params![user_id, cid])?;
            }
        }
        tx.commit()?;
        Ok(true)
    }

    // --- per-user notification matrix (P2.11) ----------------------------

    /// Whether `channel` ('push' | 'email') should deliver alerts from `rule_id`
    /// (0 = the user's default) to this user. Resolution: an exact
    /// (user, rule, channel) row wins; else the (user, 0, channel) default row;
    /// else `true` (opt-out model — no row means enabled). **Fail-open**: any DB
    /// glitch resolves to `true` so a transient error never silently mutes a user.
    pub fn pref_enabled(&self, user_id: i64, rule_id: i64, channel: &str) -> bool {
        let conn = self.conn();
        let lookup = |rid: i64| -> Option<bool> {
            conn.query_row(
                "SELECT enabled FROM notify_prefs WHERE user_id = ?1 AND rule_id = ?2 AND channel = ?3",
                params![user_id, rid, channel],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .ok()
            .flatten()
            .map(|v| v != 0)
        };
        if let Some(v) = lookup(rule_id) {
            return v;
        }
        if rule_id != 0 {
            if let Some(v) = lookup(0) {
                return v;
            }
        }
        true
    }

    /// A user's stored notification preferences (only the explicit rows — absent
    /// triples default to enabled).
    pub fn list_notify_prefs(&self, user_id: i64) -> Result<Vec<NotifyPref>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT user_id, rule_id, channel, enabled FROM notify_prefs
             WHERE user_id = ?1 ORDER BY rule_id, channel",
        )?;
        let rows = stmt
            .query_map([user_id], |r| {
                Ok(NotifyPref {
                    user_id: r.get(0)?,
                    rule_id: r.get(1)?,
                    channel: r.get(2)?,
                    enabled: r.get::<_, i64>(3)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Replace a user's notification preferences atomically (delete-all + reinsert
    /// in one transaction, mirroring [`set_user_cameras`]). Unknown channels are
    /// skipped; the row's `user_id` is forced to the path `user_id` so a body
    /// can't write another user's prefs. Returns whether the user exists.
    pub fn set_notify_prefs(&self, user_id: i64, prefs: &[NotifyPref]) -> Result<bool> {
        let mut conn = self.conn();
        let exists = conn
            .query_row("SELECT 1 FROM users WHERE id = ?1", [user_id], |_| Ok(()))
            .optional()?
            .is_some();
        if !exists {
            return Ok(false);
        }
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM notify_prefs WHERE user_id = ?1", [user_id])?;
        {
            let mut ins = tx.prepare(
                "INSERT OR REPLACE INTO notify_prefs (user_id, rule_id, channel, enabled)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for p in prefs {
                if p.channel != "push" && p.channel != "email" {
                    continue;
                }
                ins.execute(params![user_id, p.rule_id, p.channel, p.enabled as i64])?;
            }
        }
        tx.commit()?;
        Ok(true)
    }

    /// Set (or clear, with `None`) a user's notification email. Returns whether
    /// the user existed.
    pub fn set_user_email(&self, user_id: i64, email: Option<&str>) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE users SET email = ?1 WHERE id = ?2",
            params![email, user_id],
        )?;
        Ok(n > 0)
    }

    /// A user's notification email (`None` if unset or the user is gone).
    pub fn user_email(&self, user_id: i64) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT email FROM users WHERE id = ?1", [user_id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten())
    }

    /// `(id, email)` for every user with a non-empty email (push worker email
    /// fan-out recipients).
    pub fn users_with_email(&self) -> Result<Vec<(i64, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, email FROM users WHERE email IS NOT NULL AND TRIM(email) <> '' ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Does any user have a notification email set? Lets the push worker skip a
    /// tick entirely when there's no push sub AND no email recipient.
    pub fn any_user_email(&self) -> bool {
        self.conn()
            .query_row(
                "SELECT 1 FROM users WHERE email IS NOT NULL AND TRIM(email) <> '' LIMIT 1",
                [],
                |_| Ok(()),
            )
            .optional()
            .map(|o| o.is_some())
            .unwrap_or(false)
    }

    /// Whether `user_id` may see `camera_id`, mirroring the server's
    /// `allowed_cameras`/`camera_allowed` semantics for the push worker (which has
    /// no [`crate::auth::Principal`]): Admin ⇒ all; else no user_cameras rows ⇒
    /// unrestricted; else the camera must be in the allow-list. Unlike a
    /// pref/read gate this is a **visibility** gate, so it is **conservative on
    /// error** — an unknown user or any DB failure returns `false` (never leak a
    /// camera-scoped alert) rather than fail-open.
    pub fn user_can_see_camera(&self, user_id: i64, camera_id: i64) -> bool {
        let conn = self.conn();
        let role: Option<String> = match conn
            .query_row("SELECT role FROM users WHERE id = ?1", [user_id], |r| {
                r.get(0)
            })
            .optional()
        {
            Ok(r) => r,
            Err(_) => return false, // DB error: refuse (conservative)
        };
        let Some(role) = role else {
            return false; // unknown user: refuse
        };
        if crate::auth::Role::parse(&role) == crate::auth::Role::Admin {
            return true;
        }
        // No allow-list rows ⇒ unrestricted (matches user_cameras' empty = all).
        let count: i64 = match conn.query_row(
            "SELECT COUNT(*) FROM user_cameras WHERE user_id = ?1",
            [user_id],
            |r| r.get(0),
        ) {
            Ok(c) => c,
            Err(_) => return false,
        };
        if count == 0 {
            return true;
        }
        matches!(
            conn.query_row(
                "SELECT 1 FROM user_cameras WHERE user_id = ?1 AND camera_id = ?2",
                params![user_id, camera_id],
                |_| Ok(()),
            )
            .optional(),
            Ok(Some(()))
        )
    }

    /// Resolve a snapshot filename to the camera that produced it, via the
    /// authoritative events table — camera names allow `-`, so parsing the
    /// filename prefix is ambiguous. `None` if no event references it (then a
    /// scoped caller is denied, fail-closed).
    pub fn camera_for_snapshot(&self, file: &str) -> Result<Option<i64>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT camera_id FROM events WHERE snapshot = ?1 LIMIT 1",
                [file],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Camera id for an exact camera name (the `/api/ws?src=` selector).
    pub fn camera_by_name(&self, name: &str) -> Result<Option<i64>> {
        Ok(self
            .conn()
            .query_row("SELECT id FROM cameras WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Total events restricted to a set of camera ids (scoped stats/overview).
    /// Ids are our own i64s, so the IN-list is built inline (injection-safe).
    pub fn count_events_in(&self, camera_ids: &[i64]) -> Result<i64> {
        if camera_ids.is_empty() {
            return Ok(0);
        }
        let in_list = camera_ids
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        Ok(self.conn().query_row(
            &format!("SELECT COUNT(*) FROM events WHERE camera_id IN ({in_list})"),
            [],
            |r| r.get(0),
        )?)
    }

    /// Accurate COUNT(*) over events with the same filters the events list uses,
    /// optionally restricted to a set of camera ids (RBAC scope). `camera_ids`:
    /// `None` = all cameras; `Some(&[])` = no cameras (returns 0). Ids are our own
    /// i64s so the IN-list is inlined (injection-safe, like `count_events_in`);
    /// label/time bind as params. Powers the P3.2 ask `count_events` tool.
    pub fn count_events_filtered(
        &self,
        camera_ids: Option<&[i64]>,
        label: Option<&str>,
        after_ts: Option<i64>,
        before_ts: Option<i64>,
    ) -> Result<i64> {
        if matches!(camera_ids, Some(ids) if ids.is_empty()) {
            return Ok(0);
        }
        let mut sql = String::from("SELECT COUNT(*) FROM events WHERE 1=1");
        let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(l) = label {
            sql.push_str(" AND label = ?");
            binds.push(Box::new(l.to_string()));
        }
        if let Some(a) = after_ts {
            sql.push_str(" AND ts >= ?");
            binds.push(Box::new(a));
        }
        if let Some(b) = before_ts {
            sql.push_str(" AND ts < ?");
            binds.push(Box::new(b));
        }
        if let Some(ids) = camera_ids {
            let in_list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
            sql.push_str(&format!(" AND camera_id IN ({in_list})"));
        }
        let params = rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref()));
        Ok(self.conn().query_row(&sql, params, |r| r.get(0))?)
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

    /// Replace an event's user tags (already sanitized by the API layer).
    /// Returns whether the event existed. Empty = clear (NULL, not "[]").
    pub fn set_event_tags(&self, id: i64, tags: &[String]) -> Result<bool> {
        let json = if tags.is_empty() {
            None
        } else {
            Some(serde_json::to_string(tags)?)
        };
        let n = self.conn().execute(
            "UPDATE events SET tags = ?1 WHERE id = ?2",
            params![json, id],
        )?;
        Ok(n > 0)
    }

    /// Most recent person/pet event timestamp on a camera (the absence watch's
    /// presence signal).
    pub fn last_presence_ts(&self, camera_id: i64) -> Result<Option<i64>> {
        Ok(self.conn().query_row(
            "SELECT MAX(ts) FROM events WHERE camera_id = ?1 AND label IN ('person','cat','dog')",
            [camera_id],
            |r| r.get::<_, Option<i64>>(0),
        )?)
    }

    /// The stored caption for an event, if the captioner already wrote one —
    /// lets a describe-in-notification fire reuse it instead of a second call.
    pub fn event_caption(&self, event_id: i64) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT caption FROM events WHERE id = ?1",
                [event_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
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

    // --- alert feedback (P2.8b per-camera feedback learning) ---------------

    /// Kept crops per (camera, label) — a generous window of recent thumbs-downs
    /// so old, no-longer-relevant false-positives age out and the per-camera scan
    /// stays cheap.
    const FEEDBACK_KEEP_PER_CAM_LABEL: i64 = 200;

    /// Record a thumbs-down: store this object crop's CLIP embedding so future
    /// CLIP-similar alerts on the same camera + same label are quieted. Self-trims
    /// to the most recent [`FEEDBACK_KEEP_PER_CAM_LABEL`] rows for that
    /// (camera_id,label) so the table stays bounded (mirrors `add_notification`).
    /// `embedding` uses the same little-endian f32 BLOB convention as the CLIP
    /// event embeddings.
    pub fn add_alert_feedback(
        &self,
        camera_id: i64,
        event_id: Option<i64>,
        label: &str,
        embedding: &[f32],
        now: i64,
    ) -> Result<i64> {
        let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO alert_feedback (camera_id, event_id, label, embedding, created_ts)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![camera_id, event_id, label, bytes, now],
        )?;
        let id = conn.last_insert_rowid();
        // Trim older rows for this exact (camera_id,label) beyond the keep window.
        let _ = conn.execute(
            "DELETE FROM alert_feedback WHERE camera_id = ?1 AND label = ?2 AND id <= \
             (SELECT MAX(id) FROM alert_feedback WHERE camera_id = ?1 AND label = ?2) - ?3",
            params![camera_id, label, Self::FEEDBACK_KEEP_PER_CAM_LABEL],
        );
        Ok(id)
    }

    /// Suppression corpus for one camera + label (the genai worker's per-fire
    /// lookup). Empty vec = nothing learned → caller fails OPEN (fires normally).
    pub fn feedback_embeddings_for_camera(
        &self,
        camera_id: i64,
        label: &str,
    ) -> Result<Vec<Vec<f32>>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT embedding FROM alert_feedback WHERE camera_id = ?1 AND label = ?2")?;
        let rows = stmt
            .query_map(params![camera_id, label], |r| {
                let b: Vec<u8> = r.get(0)?;
                Ok(bytes_to_f32(b))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// All feedback crops as `(camera_id, label, embedding)` — the bulk load the
    /// detection pipeline caches once per tick into a per-(camera,label) map.
    pub fn feedback_embeddings(&self) -> Result<Vec<(i64, String, Vec<f32>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT camera_id, label, embedding FROM alert_feedback")?;
        let rows = stmt
            .query_map([], |r| {
                let b: Vec<u8> = r.get(2)?;
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, bytes_to_f32(b)))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The most-recent `limit` events with their searchable text + (when
    /// `with_embeddings`) their CLIP snapshot embedding, newest first, for hybrid
    /// smart search (visual similarity + speech/caption text). Bounded to `limit`
    /// so a busy long-retention deployment doesn't scan + BLOB-decode the entire
    /// events table (holding the DB mutex) on every query; ORDER BY ts DESC keeps
    /// recent recall. The embedding column (and JOIN) is skipped in text-only mode.
    pub fn search_corpus(&self, with_embeddings: bool, limit: usize) -> Result<Vec<SearchRow>> {
        let conn = self.conn();
        let sql = if with_embeddings {
            "SELECT e.id, e.transcript, e.caption, em.embedding
             FROM events e LEFT JOIN event_embeddings em ON em.event_id = e.id
             ORDER BY e.ts DESC, e.id DESC LIMIT ?1"
        } else {
            "SELECT e.id, e.transcript, e.caption, NULL
             FROM events e ORDER BY e.ts DESC, e.id DESC LIMIT ?1"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map([limit as i64], |r| {
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
        stream: &str,
    ) -> Result<()> {
        // `stream` is 'main' or 'sub' (P3.7). It's only ever set on INSERT — a
        // path uniquely identifies one file on one stream, so a conflicting
        // upsert (re-index of a growing segment) never needs to change it.
        self.conn().execute(
            "INSERT INTO segments (camera_id, start_ts, path, bytes, stream) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET bytes = excluded.bytes",
            params![camera_id, start_ts, path, bytes as i64, stream],
        )?;
        Ok(())
    }

    pub fn delete_segment_by_path(&self, path: &str) -> Result<()> {
        // The trg_offsite_seg_del trigger drops the matching offsite_uploads row
        // (so the table stays bounded by the live segment set, and a path reused
        // after a backward clock step isn't skipped by a stale 'done' row) — #70.
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

    /// Newest-first segments, optionally only those starting before `before`
    /// (exclusive) — lets the Recordings day picker page into history.
    ///
    /// `stream` filters by recording stream (P3.7): `None` defaults to `'main'`
    /// so every existing caller (Recordings list, day time-lapse, motion search)
    /// keeps seeing only full-res segments, unchanged. Pass `Some("sub")` to page
    /// the low-res scrub stream.
    pub fn list_segments(
        &self,
        camera_id: Option<i64>,
        before: Option<i64>,
        limit: u32,
        stream: Option<&str>,
    ) -> Result<Vec<SegmentRow>> {
        let stream = stream.unwrap_or("main");
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT s.id, s.camera_id, c.name, s.start_ts, s.bytes, s.path, s.stream
             FROM segments s JOIN cameras c ON c.id = s.camera_id
             WHERE (?1 IS NULL OR s.camera_id = ?1)
               AND (?2 IS NULL OR s.start_ts < ?2)
               AND s.stream = ?4
             ORDER BY s.start_ts DESC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![camera_id, before, limit, stream], |r| {
                Ok(SegmentRow {
                    id: r.get(0)?,
                    camera_id: r.get(1)?,
                    camera: r.get(2)?,
                    start_ts: r.get(3)?,
                    bytes: r.get::<_, i64>(4)? as u64,
                    path: r.get(5)?,
                    stream: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// P3.9 — segments whose start_ts >= `since`, ASCENDING, for the archive
    /// puller's forward cursor scan. Main-stream only (v0 mirrors the full-res
    /// archive). The ascending order + `>=` lets the secondary page a big
    /// backlog forward a bounded chunk per tick, advancing a cursor as it goes.
    pub fn list_segments_since(
        &self,
        camera_id: Option<i64>,
        since_ts: i64,
        limit: u32,
    ) -> Result<Vec<SegmentRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT s.id, s.camera_id, c.name, s.start_ts, s.bytes, s.path, s.stream
             FROM segments s JOIN cameras c ON c.id = s.camera_id
             WHERE (?1 IS NULL OR s.camera_id = ?1)
               AND s.start_ts >= ?2
               AND s.stream = 'main'
             ORDER BY s.start_ts ASC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![camera_id, since_ts, limit], |r| {
                Ok(SegmentRow {
                    id: r.get(0)?,
                    camera_id: r.get(1)?,
                    camera: r.get(2)?,
                    start_ts: r.get(3)?,
                    bytes: r.get::<_, i64>(4)? as u64,
                    path: r.get(5)?,
                    stream: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Count of recording segments for one camera (P3.9 archive status readout).
    pub fn count_segments(&self, camera_id: i64) -> Result<i64> {
        let n = self.conn().query_row(
            "SELECT COUNT(*) FROM segments WHERE camera_id = ?1",
            [camera_id],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// Flush one minute's OR'd motion mask (P2.3). REPLACE semantics: the
    /// pipeline writes each (camera, minute) at most once per run; a restart
    /// mid-minute just rewrites the row with what it saw since.
    pub fn add_motion_grid(&self, camera_id: i64, minute_ts: i64, cells: &[u8]) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO motion_grid (camera_id, minute_ts, cells) VALUES (?1, ?2, ?3)",
            params![camera_id, minute_ts, cells],
        )?;
        Ok(())
    }

    pub fn prune_motion_grid_before(&self, ts: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM motion_grid WHERE minute_ts < ?1", params![ts])?;
        Ok(())
    }

    /// Minute rows for a camera in [from, to], ascending. The region bit-test
    /// happens in the caller (SQLite can't test bits inside a BLOB).
    pub fn motion_grid_rows(
        &self,
        camera_id: i64,
        from: i64,
        to: i64,
    ) -> Result<Vec<(i64, Vec<u8>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT minute_ts, cells FROM motion_grid
             WHERE camera_id = ?1 AND minute_ts >= ?2 AND minute_ts <= ?3
             ORDER BY minute_ts ASC",
        )?;
        let rows = stmt
            .query_map(params![camera_id, from, to], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_segment(&self, id: i64) -> Result<Option<SegmentRow>> {
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT s.id, s.camera_id, c.name, s.start_ts, s.bytes, s.path, s.stream
                 FROM segments s JOIN cameras c ON c.id = s.camera_id
                 WHERE s.id = ?1",
                params![id],
                |r| {
                    Ok(SegmentRow {
                        id: r.get(0)?,
                        camera_id: r.get(1)?,
                        camera: r.get(2)?,
                        start_ts: r.get(3)?,
                        bytes: r.get::<_, i64>(4)? as u64,
                        path: r.get(5)?,
                        stream: r.get(6)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// The newest segment for a camera on `stream` that starts at or before `ts`
    /// — i.e. the recording most likely to contain that instant. The caller
    /// checks whether `ts` actually falls inside the segment's duration.
    ///
    /// The `stream` filter is REQUIRED (P3.7): a main and a sub segment share a
    /// near-identical `start_ts`, so without it the `ORDER BY start_ts DESC
    /// LIMIT 1` would pick between the two arbitrarily. Every existing caller
    /// passes `"main"`, so full-res resolution is unchanged.
    pub fn find_segment_at(
        &self,
        camera_id: i64,
        ts: i64,
        stream: &str,
    ) -> Result<Option<SegmentRow>> {
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT s.id, s.camera_id, c.name, s.start_ts, s.bytes, s.path, s.stream
                 FROM segments s JOIN cameras c ON c.id = s.camera_id
                 WHERE s.camera_id = ?1 AND s.start_ts <= ?2 AND s.stream = ?3
                 ORDER BY s.start_ts DESC LIMIT 1",
                params![camera_id, ts, stream],
                |r| {
                    Ok(SegmentRow {
                        id: r.get(0)?,
                        camera_id: r.get(1)?,
                        camera: r.get(2)?,
                        start_ts: r.get(3)?,
                        bytes: r.get::<_, i64>(4)? as u64,
                        path: r.get(5)?,
                        stream: r.get(6)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// The set of recording-segment file paths that COVER a flagged (bookmarked)
    /// event — i.e. a segment whose span `[start_ts, start_ts + span_secs]`
    /// contains the event's `ts`, mirroring [`find_segment_at`](Self::find_segment_at)/
    /// `recording_at`'s "newest segment starting at or before the instant, if the
    /// instant falls inside it" resolution. `span_secs` should be
    /// `segment_seconds + slack` (ffmpeg cuts on keyframes, so a real segment can
    /// run a GOP past its configured length).
    ///
    /// P2.14 footage-safety fix: a flagged event's row + snapshot already survive
    /// event retention ([`prune_events_before`](Self::prune_events_before)), but
    /// the underlying MP4 could still be deleted by the recorder's age / byte-cap
    /// sweep — silently losing the very footage the user asked to keep. The caller
    /// threads this protected set into [`recorder::prune`] so those paths are
    /// never deleted. An event on the slack boundary can match two adjacent
    /// segments and protect both — the safe (over-keep) direction for explicit
    /// user bookmarks.
    pub fn flagged_segment_paths(
        &self,
        span_secs: i64,
    ) -> Result<std::collections::HashSet<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT s.path FROM segments s
             JOIN events e ON e.camera_id = s.camera_id
             WHERE e.flagged = 1
               AND e.ts >= s.start_ts
               AND e.ts <= s.start_ts + ?1",
        )?;
        let paths = stmt
            .query_map(params![span_secs], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<std::collections::HashSet<String>>>()?;
        Ok(paths)
    }

    // --- offsite backup (matrix #70) -------------------------------------

    /// P2.14 selective offsite: when `offsite_events_only` is set, returns the
    /// symmetric overlap window (`segment_seconds + slack`) used to restrict the
    /// backup to event-adjacent segments; `None` = full mirror (every segment).
    /// Shared by [`pending_offsite`](Self::pending_offsite) (upload candidates)
    /// and [`offsite_stats`](Self::offsite_stats) (actionable backlog) so the two
    /// never disagree about which segments are in scope. MUST be called before
    /// acquiring a `conn()` guard — it reads `settings()` (which locks the same
    /// Mutex), so calling it under the guard would deadlock.
    fn offsite_event_span(&self) -> Option<i64> {
        let s = self.settings();
        s.offsite_events_only
            .then(|| i64::from(s.segment_seconds) + 15)
    }

    /// Sealed segments still awaiting offsite backup (no terminal `done`/
    /// `skipped`/`gaveup` row — only never-attempted or retryable `failed`).
    /// Returns more than the worker sends per tick so rows still inside their
    /// backoff window don't head-of-line-block fresh ones — and orders
    /// **never-attempted segments ahead of previously-failed ones** so a backlog
    /// of failing rows can't starve brand-new segments out of the window. Under
    /// `offsite_events_only` (P2.14) only event-adjacent segments are candidates
    /// (see [`offsite_event_span`](Self::offsite_event_span)).
    pub fn pending_offsite(&self, limit: u32) -> Result<Vec<PendingUpload>> {
        // Resolve the events-only filter BEFORE taking the conn guard (it reads
        // settings, which locks the same Mutex — reentrant lock would deadlock).
        let event_span = self.offsite_event_span();
        let conn = self.conn();
        // When events-only backup is on, restrict candidates to segments whose
        // span overlaps at least one event — the inverse of `eventless_segments`'
        // NOT-EXISTS/BETWEEN idiom. A symmetric one-span margin also pulls in the
        // segments immediately around each event (pre/post-roll context). Because
        // a flagged (bookmarked) event is an ordinary event row, a bookmarked
        // segment always satisfies this EXISTS, so bookmarked footage is never
        // dropped from the backup under events-only. `None` = mirror everything.
        let event_filter = if event_span.is_some() {
            " AND EXISTS (SELECT 1 FROM events e
                 WHERE e.camera_id = s.camera_id
                   AND e.ts BETWEEN s.start_ts - ?2 AND s.start_ts + ?2 + ?2)"
        } else {
            ""
        };
        // P3.7: sub-stream segments are a LOCAL scrub aid, never part of the
        // offsite archive — restrict candidates to 'main'. Mirrored in
        // `offsite_stats`' backlog so the two never disagree.
        let sql = format!(
            "SELECT s.path, c.name, s.start_ts, s.bytes,
                    COALESCE(o.attempts, 0), COALESCE(o.updated_ts, 0)
             FROM segments s JOIN cameras c ON c.id = s.camera_id
             LEFT JOIN offsite_uploads o ON o.path = s.path
             WHERE (o.status IS NULL OR o.status = 'failed')
               AND s.stream = 'main'{event_filter}
             ORDER BY (o.status IS NOT NULL) ASC, s.start_ts ASC LIMIT ?1"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mapper = |r: &rusqlite::Row| -> rusqlite::Result<PendingUpload> {
            Ok(PendingUpload {
                path: r.get(0)?,
                camera: r.get(1)?,
                start_ts: r.get(2)?,
                bytes: r.get::<_, i64>(3)? as u64,
                attempts: r.get(4)?,
                last_ts: r.get(5)?,
            })
        };
        let rows = match event_span {
            Some(span) => stmt
                .query_map(params![limit, span], mapper)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            None => stmt
                .query_map(params![limit], mapper)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        };
        Ok(rows)
    }

    /// Record a verified successful upload (idempotent on `path`).
    pub fn mark_offsite_done(
        &self,
        path: &str,
        camera: &str,
        key: &str,
        bytes: u64,
        now: i64,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO offsite_uploads (path, camera, key, bytes, status, attempts, last_error, updated_ts)
             VALUES (?1, ?2, ?3, ?4, 'done', 1, NULL, ?5)
             ON CONFLICT(path) DO UPDATE SET
                 camera = excluded.camera, key = excluded.key, bytes = excluded.bytes,
                 status = 'done', attempts = offsite_uploads.attempts + 1,
                 last_error = NULL, updated_ts = excluded.updated_ts",
            params![path, camera, key, bytes as i64, now],
        )?;
        Ok(())
    }

    /// Record a failed attempt (increments `attempts`; drives backoff). Once
    /// `attempts` reaches [`OFFSITE_MAX_ATTEMPTS`] the row flips to the terminal
    /// `gaveup` status so a permanently-failing ("poison") segment stops being
    /// retried and leaves the active candidate set / backlog count (#70).
    pub fn mark_offsite_failed(
        &self,
        path: &str,
        camera: &str,
        key: &str,
        bytes: u64,
        err: &str,
        now: i64,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO offsite_uploads (path, camera, key, bytes, status, attempts, last_error, updated_ts)
             VALUES (?1, ?2, ?3, ?4,
                     CASE WHEN 1 >= ?7 THEN 'gaveup' ELSE 'failed' END, 1, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET
                 camera = excluded.camera, key = excluded.key, bytes = excluded.bytes,
                 status = CASE WHEN offsite_uploads.attempts + 1 >= ?7 THEN 'gaveup' ELSE 'failed' END,
                 attempts = offsite_uploads.attempts + 1,
                 last_error = excluded.last_error, updated_ts = excluded.updated_ts",
            params![path, camera, key, bytes as i64, err, now, OFFSITE_MAX_ATTEMPTS],
        )?;
        Ok(())
    }

    /// Mark a segment as terminally given-up (won't be retried) with a reason —
    /// e.g. it's too large to back up. Leaves the active candidate set/backlog.
    pub fn mark_offsite_gaveup(
        &self,
        path: &str,
        camera: &str,
        reason: &str,
        now: i64,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO offsite_uploads (path, camera, key, bytes, status, attempts, last_error, updated_ts)
             VALUES (?1, ?2, '', 0, 'gaveup',
                     COALESCE((SELECT attempts FROM offsite_uploads WHERE path = ?1), 0), ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET
                 status = 'gaveup', last_error = excluded.last_error, updated_ts = excluded.updated_ts",
            params![path, camera, reason, now],
        )?;
        Ok(())
    }

    /// Mark a segment whose local file vanished (retention pruned it before
    /// backup) so it isn't retried forever or counted as actionable backlog.
    pub fn mark_offsite_skipped(&self, path: &str, camera: &str, now: i64) -> Result<()> {
        self.conn().execute(
            "INSERT INTO offsite_uploads (path, camera, key, bytes, status, attempts, last_error, updated_ts)
             VALUES (?1, ?2, '', 0, 'skipped', COALESCE((SELECT attempts FROM offsite_uploads WHERE path = ?1), 0), 'source file removed before backup', ?3)
             ON CONFLICT(path) DO UPDATE SET status = 'skipped', updated_ts = excluded.updated_ts",
            params![path, camera, now],
        )?;
        Ok(())
    }

    pub fn offsite_stats(&self) -> Result<OffsiteStats> {
        // Resolve the events-only filter before the conn guard (settings() locks).
        let event_span = self.offsite_event_span();
        let conn = self.conn();
        // Backlog = sealed segments with no terminal row yet (never-attempted or
        // retryable 'failed'). Needs the segments join, so it stays its own query;
        // note it deliberately does NOT subtract done/skipped/gaveup arithmetic.
        // Under events-only backup (P2.14) the same EXISTS filter as
        // `pending_offsite` applies, so the backlog counts only *actionable*
        // segments — non-event segments (never candidates) don't inflate it into
        // a permanent false "backup stalled" alert.
        // P3.7: exclude sub-stream segments (local-only scrub aid) from the
        // backlog, exactly as `pending_offsite` excludes them from candidates.
        let backlog: i64 = match event_span {
            None => conn.query_row(
                "SELECT COUNT(*) FROM segments s
                 LEFT JOIN offsite_uploads o ON o.path = s.path
                 WHERE (o.status IS NULL OR o.status = 'failed')
                   AND s.stream = 'main'",
                [],
                |r| r.get(0),
            )?,
            Some(span) => conn.query_row(
                "SELECT COUNT(*) FROM segments s
                 LEFT JOIN offsite_uploads o ON o.path = s.path
                 WHERE (o.status IS NULL OR o.status = 'failed')
                   AND s.stream = 'main'
                   AND EXISTS (SELECT 1 FROM events e
                       WHERE e.camera_id = s.camera_id
                         AND e.ts BETWEEN s.start_ts - ?1 AND s.start_ts + ?1 + ?1)",
                [span],
                |r| r.get(0),
            )?,
        };
        // One pass over offsite_uploads for every per-status aggregate. `done`
        // carries bytes_total + last_success_ts (its MAX(updated_ts)); the
        // terminal losses `skipped` (local file pruned before backup) and
        // `gaveup` (retries exhausted / oversize) are surfaced so they're never
        // silent. 'failed' rows fold into `backlog` and aren't counted here (#70).
        let (mut last_success_ts, mut bytes_total, mut done, mut skipped, mut gaveup) =
            (None, 0i64, 0i64, 0i64, 0i64);
        {
            let mut stmt = conn.prepare(
                "SELECT status, COUNT(*), COALESCE(SUM(bytes), 0), MAX(updated_ts)
                 FROM offsite_uploads GROUP BY status",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            })?;
            for row in rows {
                let (status, count, bytes, max_ts) = row?;
                match status.as_str() {
                    "done" => {
                        done = count;
                        bytes_total = bytes;
                        last_success_ts = max_ts;
                    }
                    "skipped" => skipped = count,
                    "gaveup" => gaveup = count,
                    _ => {}
                }
            }
        }
        let last_error: Option<String> = conn
            .query_row(
                "SELECT last_error FROM offsite_uploads
                 WHERE status = 'failed' AND last_error IS NOT NULL
                 ORDER BY updated_ts DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let mut stmt = conn.prepare(
            "SELECT camera, COALESCE(SUM(bytes), 0) FROM offsite_uploads
             WHERE status = 'done' GROUP BY camera ORDER BY camera",
        )?;
        let per_camera = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(OffsiteStats {
            last_success_ts,
            backlog,
            bytes_total,
            done,
            skipped,
            gaveup,
            last_error,
            per_camera,
        })
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

    /// Does an event of `label` exist on `camera_id` at or after `since` (unix
    /// secs)? The cross-modal confirmation lookup (`AlarmRule::confirm_ok`).
    pub fn has_recent_event(&self, camera_id: i64, label: &str, since: i64) -> Result<bool> {
        let n: i64 = self.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM events
             WHERE camera_id = ?1 AND label = ?2 AND ts >= ?3)",
            params![camera_id, label, since],
            |r| r.get(0),
        )?;
        Ok(n != 0)
    }

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
    let label: String = r.get(4)?;
    let face: Option<String> = r.get(11)?;
    let gesture: Option<String> = r.get(13)?;
    // Rows written before the severity column existed are re-derived on read,
    // so old events badge/filter consistently with new ones.
    let severity = match r.get::<_, Option<i64>>(23)? {
        Some(v) => v.clamp(1, 4) as u8,
        None => crate::severity::severity_for(&label, face.as_deref(), gesture.as_deref()),
    };
    Ok(Event {
        id: r.get(0)?,
        camera_id: r.get(1)?,
        camera: r.get(2)?,
        ts: r.get(3)?,
        label,
        score: r.get(5)?,
        bbox: [r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?],
        snapshot: r.get(10)?,
        face,
        plate: r.get(12)?,
        gesture,
        zone: r.get(14)?,
        caption: r.get(15)?,
        transcript: r.get(16)?,
        flagged: r.get::<_, i64>(17)? != 0,
        note: r.get(18)?,
        anomaly_score: r.get(19)?,
        direction: r.get(20)?,
        speed: r.get(21)?,
        gait: r.get(22)?,
        severity,
        tags: r
            .get::<_, Option<String>>(24)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        track_id: r.get(25)?,
        path_json: r.get(26)?,
    })
}

/// Seconds of allowed gap between consecutive events of the same physical object
/// before they're treated as two different objects (see [`Db::track_lifecycle`]).
const LIFECYCLE_GAP_SECS: i64 = 600;

/// Given ascending timestamps and the index of the seed event, return the
/// inclusive `[lo, hi]` bounds of the *contiguous* cluster around it — walking
/// outward while each consecutive gap is `<= gap`. Pure so it can be unit-tested
/// against the track-id-reuse-after-restart hazard. `ts` must be sorted ascending.
fn cluster_bounds(ts: &[i64], seed: usize, gap: i64) -> (usize, usize) {
    let mut lo = seed;
    while lo > 0 && ts[lo] - ts[lo - 1] <= gap {
        lo -= 1;
    }
    let mut hi = seed;
    while hi + 1 < ts.len() && ts[hi + 1] - ts[hi] <= gap {
        hi += 1;
    }
    (lo, hi)
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
    fn cluster_bounds_isolates_the_seed_cluster() {
        // Two runs of the SAME (reused) track id, separated by a big gap.
        let ts = [1000, 1030, 1060, 90_000, 90_030];
        // Seed inside the first run → only the first run.
        assert_eq!(cluster_bounds(&ts, 1, LIFECYCLE_GAP_SECS), (0, 2));
        // Seed inside the second run → only the second run.
        assert_eq!(cluster_bounds(&ts, 3, LIFECYCLE_GAP_SECS), (3, 4));
        // A single-element series is its own cluster.
        assert_eq!(cluster_bounds(&[42], 0, LIFECYCLE_GAP_SECS), (0, 0));
        // A gap exactly at the threshold is still contiguous (<=).
        let edge = [0, LIFECYCLE_GAP_SECS, LIFECYCLE_GAP_SECS * 2 + 1];
        assert_eq!(cluster_bounds(&edge, 0, LIFECYCLE_GAP_SECS), (0, 1));
    }

    #[test]
    fn track_lifecycle_gap_bounds_reused_track_ids() {
        let db = mem_db();
        let cam = db.add_camera("gate", "rtsp://x", None, true, true).unwrap();
        // Helper: a tracker-driven narrative event for a given track id.
        let add = |ts: i64, label: &str, track: i64, path: Option<&str>| {
            db.add_event_dir(
                cam.id,
                ts,
                label,
                1.0,
                [0.4, 0.5, 0.6, 0.7],
                None,
                None,
                None,
                None,
                Some("yard"),
                None,
                None,
                Some(track),
                path,
            )
            .unwrap()
        };
        // Cluster A: three steps of object #5 over a minute.
        add(1000, "crossing", 5, Some("[[1000,0.1,0.9]]"));
        add(1030, "loiter", 5, Some("[[1000,0.1,0.9],[1030,0.2,0.8]]"));
        add(1060, "zone_enter", 5, None);
        // Cluster B: a DIFFERENT physical object that reused id #5 a day later.
        add(90_000, "crossing", 5, Some("[[90000,0.5,0.5]]"));
        add(90_030, "loiter", 5, None);
        // A non-track event on the same camera must never leak in (track_id NULL).
        db.add_event(
            cam.id, 1045, "person", 0.9, [0.0; 4], None, None, None, None, None,
        )
        .unwrap();

        // Seed in cluster A → exactly A's three steps, oldest-first.
        let a = db.track_lifecycle(cam.id, 5, 1030).unwrap();
        assert_eq!(a.len(), 3, "only the seed's contiguous cluster");
        assert_eq!(
            a.iter().map(|e| e.ts).collect::<Vec<_>>(),
            vec![1000, 1030, 1060]
        );
        assert_eq!(a[0].label, "crossing");
        assert_eq!(a[0].track_id, Some(5));

        // Seed in cluster B → exactly B's two steps (the reused id did NOT merge).
        let b = db.track_lifecycle(cam.id, 5, 90_000).unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(
            b.iter().map(|e| e.ts).collect::<Vec<_>>(),
            vec![90_000, 90_030]
        );

        // An id nobody used → empty.
        assert!(db.track_lifecycle(cam.id, 999, 1000).unwrap().is_empty());
    }

    #[test]
    fn count_events_filtered_scopes_by_camera_label_and_time() {
        let db = mem_db();
        let a = db
            .add_camera("front", "rtsp://a", None, true, true)
            .unwrap();
        let b = db.add_camera("back", "rtsp://b", None, true, true).unwrap();
        let ev = |cam: i64, ts: i64, label: &str| {
            db.add_event(cam, ts, label, 0.9, [0.0; 4], None, None, None, None, None)
                .unwrap();
        };
        ev(a.id, 1000, "person");
        ev(a.id, 1100, "person");
        ev(a.id, 1200, "car");
        ev(b.id, 1050, "person");

        // No filters, all cameras.
        assert_eq!(db.count_events_filtered(None, None, None, None).unwrap(), 4);
        // Label filter across all cameras.
        assert_eq!(
            db.count_events_filtered(None, Some("person"), None, None)
                .unwrap(),
            3
        );
        // Scoped to camera A only (RBAC scope) + label.
        assert_eq!(
            db.count_events_filtered(Some(&[a.id]), Some("person"), None, None)
                .unwrap(),
            2
        );
        // Time window [1050, 1200): person on A@1100 + person on B@1050 = 2.
        assert_eq!(
            db.count_events_filtered(None, Some("person"), Some(1050), Some(1200))
                .unwrap(),
            2
        );
        // Empty scope = 0 (a scoped user with no cameras).
        assert_eq!(
            db.count_events_filtered(Some(&[]), None, None, None)
                .unwrap(),
            0
        );
    }

    /// P2.14 Part 1: `flagged_segment_paths` returns exactly the segment(s) that
    /// COVER a flagged (bookmarked) event, and the flag drives membership.
    #[test]
    fn flagged_segment_paths_resolves_covering_segment() {
        let db = mem_db();
        let cam = db.add_camera("gate", "rtsp://x", None, true, true).unwrap();
        // Two 60s segments back to back; span slack = 60 + 15.
        db.upsert_segment(cam.id, 1000, "/rec/gate/a.mp4", 100, "main")
            .unwrap();
        db.upsert_segment(cam.id, 1060, "/rec/gate/b.mp4", 100, "main")
            .unwrap();
        let span = 60 + 15;

        // An UNflagged event inside segment a; a FLAGGED event squarely inside b
        // (ts 1100 > 1075 so it can't also match a's boundary).
        db.add_event(
            cam.id, 1010, "person", 0.9, [0.0; 4], None, None, None, None, None,
        )
        .unwrap();
        let flagged = db
            .add_event(
                cam.id, 1100, "person", 0.9, [0.0; 4], None, None, None, None, None,
            )
            .unwrap();
        db.set_event_flag(flagged, true).unwrap();

        let prot = db.flagged_segment_paths(span).unwrap();
        assert!(
            prot.contains("/rec/gate/b.mp4"),
            "segment covering the flagged event is protected"
        );
        assert!(
            !prot.contains("/rec/gate/a.mp4"),
            "a segment with only an unflagged event is not protected"
        );

        // Un-flagging drops it: the flag, not the event's mere existence, drives it.
        db.set_event_flag(flagged, false).unwrap();
        assert!(db.flagged_segment_paths(span).unwrap().is_empty());
    }

    /// P3.7: a segment is stored + read back with its stream tag, and
    /// `list_segments` filters by stream (defaulting to 'main' so existing
    /// callers only ever see full-res segments).
    #[test]
    fn segment_stream_tag_round_trip_and_filter() {
        let db = mem_db();
        let cam = db.add_camera("gate", "rtsp://x", None, true, true).unwrap();
        db.upsert_segment(cam.id, 1000, "/rec/gate/a.mp4", 100, "main")
            .unwrap();
        db.upsert_segment(cam.id, 1001, "/rec/gate__sub/a.mp4", 20, "sub")
            .unwrap();

        // Default (None) → main only, unchanged behavior for every existing caller.
        let main = db.list_segments(Some(cam.id), None, 100, None).unwrap();
        assert_eq!(main.len(), 1);
        assert_eq!(main[0].path, "/rec/gate/a.mp4");
        assert_eq!(main[0].stream, "main");

        // Explicit 'sub' pages the low-res copy.
        let sub = db
            .list_segments(Some(cam.id), None, 100, Some("sub"))
            .unwrap();
        assert_eq!(sub.len(), 1);
        assert_eq!(sub[0].path, "/rec/gate__sub/a.mp4");
        assert_eq!(sub[0].stream, "sub");

        // get_segment carries the tag through too.
        assert_eq!(db.get_segment(sub[0].id).unwrap().unwrap().stream, "sub");
    }

    /// P3.7: `find_segment_at` disambiguates by stream. A main and a sub segment
    /// share a near-identical start_ts, so the stream filter is what keeps a
    /// stream-scoped lookup from arbitrarily picking the other stream's file.
    #[test]
    fn find_segment_at_disambiguates_by_stream() {
        let db = mem_db();
        let cam = db.add_camera("gate", "rtsp://x", None, true, true).unwrap();
        // Sub starts one second AFTER main, so a naive "newest start_ts <= ts"
        // (no stream filter) would prefer the sub file for a main lookup.
        db.upsert_segment(cam.id, 1000, "/rec/gate/a.mp4", 100, "main")
            .unwrap();
        db.upsert_segment(cam.id, 1001, "/rec/gate__sub/a.mp4", 20, "sub")
            .unwrap();

        let m = db.find_segment_at(cam.id, 1030, "main").unwrap().unwrap();
        assert_eq!(m.path, "/rec/gate/a.mp4");
        assert_eq!(m.stream, "main");

        let s = db.find_segment_at(cam.id, 1030, "sub").unwrap().unwrap();
        assert_eq!(s.path, "/rec/gate__sub/a.mp4");
        assert_eq!(s.stream, "sub");

        // A stream with no segment for that camera resolves to nothing, even
        // though the other stream has coverage.
        let cam2 = db.add_camera("yard", "rtsp://y", None, true, true).unwrap();
        db.upsert_segment(cam2.id, 2000, "/rec/yard/a.mp4", 100, "main")
            .unwrap();
        assert!(db.find_segment_at(cam2.id, 2030, "sub").unwrap().is_none());
    }

    /// P2.14 Part 2: with `offsite_events_only`, `pending_offsite` returns only
    /// segments overlapping an event — and a bookmarked segment is always among
    /// them, so saved footage is never dropped from the backup.
    #[test]
    fn pending_offsite_events_only_filter() {
        let db = mem_db();
        let cam = db.add_camera("gate", "rtsp://x", None, true, true).unwrap();
        // Three widely-spaced segments so event windows don't overlap neighbours.
        db.upsert_segment(cam.id, 1000, "/rec/gate/a.mp4", 100, "main")
            .unwrap();
        db.upsert_segment(cam.id, 5000, "/rec/gate/b.mp4", 100, "main")
            .unwrap();
        db.upsert_segment(cam.id, 9000, "/rec/gate/c.mp4", 100, "main")
            .unwrap();
        // An ordinary (unflagged) event squarely inside segment b.
        db.add_event(
            cam.id, 5030, "person", 0.9, [0.0; 4], None, None, None, None, None,
        )
        .unwrap();

        // Full-mirror default: every sealed segment is a candidate.
        assert_eq!(db.pending_offsite(100).unwrap().len(), 3);

        // Turn events-only on.
        let mut s = db.settings();
        s.offsite_events_only = true;
        s.segment_seconds = 60;
        db.save_settings(&s).unwrap();

        let only: std::collections::HashSet<String> = db
            .pending_offsite(100)
            .unwrap()
            .into_iter()
            .map(|p| p.path)
            .collect();
        assert_eq!(
            only.len(),
            1,
            "only the event-adjacent segment is a candidate"
        );
        assert!(only.contains("/rec/gate/b.mp4"));

        // Bookmark a moment inside segment a: events-only must now also back it up
        // (a flagged event is an ordinary event row -> its segment overlaps one).
        let flagged = db
            .add_event(
                cam.id, 1030, "person", 0.9, [0.0; 4], None, None, None, None, None,
            )
            .unwrap();
        db.set_event_flag(flagged, true).unwrap();
        let with_bookmark: std::collections::HashSet<String> = db
            .pending_offsite(100)
            .unwrap()
            .into_iter()
            .map(|p| p.path)
            .collect();
        assert!(
            with_bookmark.contains("/rec/gate/a.mp4"),
            "bookmarked footage is backed up under events-only"
        );
        assert!(with_bookmark.contains("/rec/gate/b.mp4"));
        assert!(
            !with_bookmark.contains("/rec/gate/c.mp4"),
            "an event-less segment stays excluded"
        );
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
    fn occupants_presence_and_derived_mode() {
        let db = mem_db();
        // Two occupants: Alice home, Bob away.
        db.upsert_occupant("Alice", true, 100).unwrap();
        db.upsert_occupant("Bob", false, 100).unwrap();
        assert_eq!(db.count_home_occupants().unwrap(), 1);
        assert_eq!(db.list_occupants().unwrap().len(), 2);

        // Bob arrives ⇒ both home (upsert on the existing name, no duplicate row).
        db.upsert_occupant("Bob", true, 200).unwrap();
        assert_eq!(db.list_occupants().unwrap().len(), 2, "upsert, not insert");
        assert_eq!(db.count_home_occupants().unwrap(), 2);

        // Delete Alice ⇒ one occupant left, still home.
        let alice = db
            .list_occupants()
            .unwrap()
            .into_iter()
            .find(|o| o.name == "Alice")
            .unwrap();
        assert!(db.delete_occupant(alice.id).unwrap());
        assert!(!db.delete_occupant(alice.id).unwrap(), "already gone");
        let rows = db.list_occupants().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Bob");
        assert_eq!(db.count_home_occupants().unwrap(), 1);

        // First-in/last-out derivation.
        assert_eq!(derive_arm_mode(0), "away");
        assert_eq!(derive_arm_mode(2), "home");
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
    fn alert_feedback_roundtrip_and_scoping() {
        let db = mem_db();
        let cam = db.add_camera("yard", "rtsp://x", None, true, true).unwrap();
        // Two thumbs-downs on person, one on car (different label scope).
        db.add_alert_feedback(cam.id, Some(1), "person", &[1.0, 0.0, 0.0], 100)
            .unwrap();
        db.add_alert_feedback(cam.id, Some(2), "person", &[0.0, 1.0, 0.0], 110)
            .unwrap();
        db.add_alert_feedback(cam.id, None, "car", &[0.0, 0.0, 1.0], 120)
            .unwrap();

        // Per-(camera,label) lookup returns only same-label crops, decoded intact.
        let person = db.feedback_embeddings_for_camera(cam.id, "person").unwrap();
        assert_eq!(person.len(), 2);
        assert!(person.contains(&vec![1.0, 0.0, 0.0]) && person.contains(&vec![0.0, 1.0, 0.0]));
        let car = db.feedback_embeddings_for_camera(cam.id, "car").unwrap();
        assert_eq!(car, vec![vec![0.0, 0.0, 1.0]]);
        // A camera/label with nothing learned → empty (caller fails open).
        assert!(db
            .feedback_embeddings_for_camera(cam.id, "dog")
            .unwrap()
            .is_empty());
        assert!(db
            .feedback_embeddings_for_camera(9999, "person")
            .unwrap()
            .is_empty());

        // Bulk load carries camera_id + label for the pipeline's per-tick cache.
        let all = db.feedback_embeddings().unwrap();
        assert_eq!(all.len(), 3);
        assert!(all
            .iter()
            .any(|(c, l, e)| *c == cam.id && l == "person" && *e == vec![1.0, 0.0, 0.0]));
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
                // P3.5 zone-state classifier rides detect_json (no migration) —
                // set here so the full round-trip below covers its persistence.
                state_classify: true,
                open_prompt: Some("an open garage door".into()),
                closed_prompt: Some("a closed garage door".into()),
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
            accelerator: Some("openvino".into()),
            poll_ms: Some(2000),
            face_recognize: Some(true),
            two_way_audio: true,
            tamper_detect: true,
            gait_identify: true,
            retention_days: Some(14),
            fall_detect: true,
            child_height_frac: Some(0.45),
            absence_hours: Some(12.0),
            onvif_events: true,
            pose_detect: true,
            no_clip: false,
            record_schedule: Some(Schedule {
                days: vec![1, 2, 3, 4, 5],
                start_hhmm: Some("08:00".into()),
                end_hhmm: Some("18:00".into()),
            }),
            package_detect: true,
            package_zone: Some(vec![[0.1, 0.1], [0.9, 0.1], [0.9, 0.9], [0.1, 0.9]]),
            package_labels: vec!["package".to_string()],
            suppress_stationary: true,
            trigger_recording: true,
            trigger_pre_roll_secs: Some(15),
            trigger_post_roll_secs: Some(45),
            record_substream: true,
            homekit_expose: true,
            homekit_doorbell: true,
        };
        db.update_camera(&cam).unwrap();
        let back = db.get_camera(cam.id).unwrap().unwrap();
        assert_eq!(back.detect_config, cam.detect_config);

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
            confirm_label: None,
            confirm_within_secs: None,
            vlm_prompt: None,
            describe: false,
            prompt_like: None,
            attr_like: None,
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
        assert!(
            !zoned.zone_ok(None),
            "a zone-scoped rule needs a zoned event"
        );
        assert!(rule.zone_ok(None), "an unscoped rule matches any/no zone");
        assert!(rule.zone_ok(Some("Pool")));

        // P2.5 attr_like: a catalog key makes the rule a prompt rule (fires only
        // via the embedding pass) and resolves to its CLIP prompt; prompt_like
        // wins when both are set; an unknown/empty key resolves to nothing.
        let attr_rule = AlarmRule {
            label: Some("car".into()),
            attr_like: Some("veh_color_red".into()),
            ..rule.clone()
        };
        assert!(attr_rule.is_prompt_rule());
        assert_eq!(attr_rule.effective_prompt().as_deref(), Some("a red car"));
        // A prompt rule never fires on the plain dispatch path.
        assert!(!attr_rule.matches(3, "car", 0.9, None, None, None, None));
        // matches_prompt (embedding pass) checks the OTHER conditions with the
        // attr gate masked: right label passes, wrong label fails.
        assert!(attr_rule.matches_prompt(3, "car", 0.9, None, None));
        assert!(!attr_rule.matches_prompt(3, "person", 0.9, None, None));

        // prompt_like takes precedence over attr_like.
        let both = AlarmRule {
            prompt_like: Some("a red pickup truck".into()),
            attr_like: Some("veh_color_red".into()),
            ..rule.clone()
        };
        assert_eq!(
            both.effective_prompt().as_deref(),
            Some("a red pickup truck")
        );

        // An unknown / empty key: still a prompt rule (so it can't fire on the
        // plain path), but effective_prompt is None so the pipeline skips it.
        let stale = AlarmRule {
            attr_like: Some("veh_color_chartreuse".into()),
            ..rule.clone()
        };
        assert!(stale.is_prompt_rule());
        assert_eq!(stale.effective_prompt(), None);
        let empty = AlarmRule {
            attr_like: Some("   ".into()),
            ..rule.clone()
        };
        assert!(!empty.is_prompt_rule());
        assert_eq!(empty.effective_prompt(), None);
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
                zone_like: Some("Door".into()),
                confirm_label: Some("person".into()),
                confirm_within_secs: Some(10),
                vlm_prompt: None,
                describe: false,
                prompt_like: None,
                attr_like: None,
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
        // zone_like + cross-modal confirmation ride the schedule blob and round-trip.
        assert_eq!(back.zone_like.as_deref(), Some("Door"));
        assert_eq!(back.confirm_label.as_deref(), Some("person"));
        assert_eq!(back.confirm_within_secs, Some(10));
        // confirm_ok: needs a co-occurring "person" event on the same camera in 10 s.
        // Fails open only on DB error; a clean "no such event" correctly suppresses.
        let rule = back.clone();
        let cam = db
            .add_camera("confirm-cam", "rtsp://x", None, true, true)
            .unwrap();
        assert!(
            !rule.confirm_ok(&db, cam.id, 1_000_000),
            "no companion event -> not confirmed"
        );
        db.add_event(
            cam.id,
            1_000_000 - 5,
            "person",
            0.9,
            [0.0; 4],
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(
            rule.confirm_ok(&db, cam.id, 1_000_000),
            "companion person within window"
        );
        assert!(
            !rule.confirm_ok(&db, cam.id, 1_000_000 + 100),
            "companion now outside the window"
        );
        assert!(
            !rule.confirm_ok(&db, cam.id + 1, 1_000_000),
            "companion is on a different camera"
        );
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
    fn schedule_window_active_cases() {
        // Empty schedule = always active (the opt-in default).
        let always = Schedule::default();
        assert!(always.active_at(0, 0));
        assert!(always.active_at(3, 1439));
        // Daytime window 08:00–18:00 on weekdays (Mon=1..Fri=5).
        let day = Schedule {
            days: vec![1, 2, 3, 4, 5],
            start_hhmm: Some("08:00".into()),
            end_hhmm: Some("18:00".into()),
        };
        assert!(day.active_at(3, 12 * 60)); // Wed noon
        assert!(!day.active_at(3, 7 * 60)); // Wed 07:00 (before)
        assert!(!day.active_at(3, 19 * 60)); // Wed 19:00 (after)
        assert!(!day.active_at(0, 12 * 60)); // Sunday excluded
                                             // Boundaries inclusive.
        assert!(day.active_at(1, 8 * 60));
        assert!(day.active_at(5, 18 * 60));
        // Overnight window 22:00–06:00 (any day).
        let night = Schedule {
            days: vec![],
            start_hhmm: Some("22:00".into()),
            end_hhmm: Some("06:00".into()),
        };
        assert!(night.active_at(2, 23 * 60));
        assert!(night.active_at(2, 5 * 60));
        assert!(!night.active_at(2, 12 * 60));
        // Open-ended (start only / end only) + bad hhmm ignored.
        assert!(window_active(&[], Some("09:00"), None, 0, 10 * 60));
        assert!(!window_active(&[], Some("09:00"), None, 0, 8 * 60));
        assert!(window_active(&[], Some("25:99"), Some("99:99"), 0, 12 * 60)); // unparsable -> open
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
            confirm_label: None,
            confirm_within_secs: None,
            vlm_prompt: None,
            describe: false,
            prompt_like: None,
            attr_like: None,
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
            db.upsert_segment(cam.id, ts, &format!("p{ts}.mp4"), 10, "main")
                .unwrap();
        }
        db.add_event(
            cam.id, 2030, "person", 0.9, [0.0; 4], None, None, None, None, None,
        )
        .unwrap();
        // Event-only mode passes (span, span) → the original symmetric window,
        // byte-for-byte unchanged from the pre-P3.8 single-margin signature.
        let mut doomed = db.eventless_segments(cam.id, 5000, 60, 15, 15).unwrap();
        doomed.sort();
        assert_eq!(
            doomed,
            vec!["p1000.mp4".to_string(), "p3000.mp4".to_string()]
        );
        // Grace period: nothing older than 1500 except segment 1.
        assert_eq!(
            db.eventless_segments(cam.id, 1500, 60, 15, 15).unwrap(),
            vec!["p1000.mp4".to_string()]
        );
    }

    #[test]
    fn eventless_segments_asymmetric_margins() {
        // P3.8 detection-triggered recording: pre-roll (margin_before) keeps the
        // segment BEFORE a detection, post-roll (margin_after) the one AFTER.
        let db = mem_db();
        let cam = db
            .add_camera("porch", "rtsp://x", None, true, true)
            .unwrap();
        // Three 60s segments at t=1000/2000/3000; one detection early in the
        // middle segment (t=2010).
        for ts in [1000, 2000, 3000] {
            db.upsert_segment(cam.id, ts, &format!("p{ts}.mp4"), 10, "main")
                .unwrap();
        }
        db.add_event(
            cam.id, 2010, "person", 0.9, [0.0; 4], None, None, None, None, None,
        )
        .unwrap();

        // Large pre-roll (1000s), tiny post-roll (5s): the pre-roll reaches back
        // into the PREVIOUS segment, so only the later segment is eventless.
        assert_eq!(
            db.eventless_segments(cam.id, 100_000, 60, 1000, 5).unwrap(),
            vec!["p3000.mp4".to_string()],
        );
        // Mirror: tiny pre-roll (5s), large post-roll (1000s): the post-roll
        // reaches forward into the NEXT segment, so only the earlier is eventless.
        assert_eq!(
            db.eventless_segments(cam.id, 100_000, 60, 5, 1000).unwrap(),
            vec!["p1000.mp4".to_string()],
        );
    }

    #[test]
    fn eventless_segments_keeps_flagged_event_footage() {
        // A detection-triggered prune must never delete a bookmarked clip: a
        // flagged event survives event-retention, so it keeps protecting its
        // segment even after the surrounding un-flagged events are gone.
        let db = mem_db();
        let cam = db
            .add_camera("porch", "rtsp://x", None, true, true)
            .unwrap();
        for ts in [1000, 2000, 3000] {
            db.upsert_segment(cam.id, ts, &format!("p{ts}.mp4"), 10, "main")
                .unwrap();
        }
        // An un-flagged detection in seg1; a BOOKMARKED detection in seg2.
        db.add_event(
            cam.id, 1010, "person", 0.9, [0.0; 4], None, None, None, None, None,
        )
        .unwrap();
        let saved = db
            .add_event(
                cam.id, 2010, "person", 0.9, [0.0; 4], None, None, None, None, None,
            )
            .unwrap();
        db.set_event_bookmark(saved, true, Some("keep this"))
            .unwrap();

        // Event retention removes every un-flagged event; the bookmark survives.
        db.prune_events_before(5000).unwrap();

        // A tight detection-triggered prune (10s pre / 5s post) now finds seg1
        // and seg3 eventless, but seg2 is still protected by the bookmark.
        let mut doomed = db.eventless_segments(cam.id, 100_000, 60, 10, 5).unwrap();
        doomed.sort();
        assert_eq!(
            doomed,
            vec!["p1000.mp4".to_string(), "p3000.mp4".to_string()]
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

    // --- P2.11 per-user notification matrix ---------------------------------

    fn pref(user_id: i64, rule_id: i64, channel: &str, enabled: bool) -> NotifyPref {
        NotifyPref {
            user_id,
            rule_id,
            channel: channel.to_string(),
            enabled,
        }
    }

    #[test]
    fn notify_pref_resolution_exact_then_default_then_on() {
        let db = mem_db();
        let uid = db.add_user("alice", "h", "viewer", 0).unwrap();
        // No rows at all → opt-out default is ON.
        assert!(db.pref_enabled(uid, 7, "push"));
        assert!(db.pref_enabled(uid, 7, "email"));
        // A user default (rule 0) OFF for push applies to any rule with no override.
        db.set_notify_prefs(uid, &[pref(uid, 0, "push", false)])
            .unwrap();
        assert!(
            !db.pref_enabled(uid, 7, "push"),
            "falls back to the default"
        );
        assert!(db.pref_enabled(uid, 7, "email"), "email untouched → on");
        // An exact rule row beats the default.
        db.set_notify_prefs(
            uid,
            &[pref(uid, 0, "push", false), pref(uid, 7, "push", true)],
        )
        .unwrap();
        assert!(
            db.pref_enabled(uid, 7, "push"),
            "exact override beats default"
        );
        assert!(
            !db.pref_enabled(uid, 9, "push"),
            "others still use default-off"
        );
        // Replace-with-empty clears everything back to on.
        db.set_notify_prefs(uid, &[]).unwrap();
        assert!(db.pref_enabled(uid, 7, "push"));
    }

    #[test]
    fn notify_prefs_round_trip_and_unknown_channel_skipped() {
        let db = mem_db();
        let uid = db.add_user("bob", "h", "operator", 0).unwrap();
        let prefs = vec![
            pref(uid, 0, "push", true),
            pref(uid, 3, "email", false),
            pref(uid, 3, "sms", true), // unknown channel → dropped
        ];
        assert!(db.set_notify_prefs(uid, &prefs).unwrap());
        let got = db.list_notify_prefs(uid).unwrap();
        assert_eq!(got.len(), 2, "unknown channel dropped");
        assert!(got.iter().all(|p| p.user_id == uid));
        // A nonexistent user → false (the API turns that into 404).
        assert!(!db.set_notify_prefs(999_999, &prefs).unwrap());
    }

    #[test]
    fn user_can_see_camera_matches_rbac_semantics() {
        let db = mem_db();
        let a = db.add_camera("a", "rtsp://a", None, true, true).unwrap();
        let b = db.add_camera("b", "rtsp://b", None, true, true).unwrap();
        let admin = db.add_user("admin", "h", "admin", 0).unwrap();
        let viewer = db.add_user("viewer", "h", "viewer", 0).unwrap();
        // Admin sees everything.
        assert!(db.user_can_see_camera(admin, a.id));
        assert!(db.user_can_see_camera(admin, b.id));
        // Unrestricted viewer (no user_cameras rows) sees everything.
        assert!(db.user_can_see_camera(viewer, a.id));
        // Restrict the viewer to camera a only.
        assert!(db.set_user_cameras(viewer, &[a.id]).unwrap());
        assert!(db.user_can_see_camera(viewer, a.id), "in the allow-list");
        assert!(
            !db.user_can_see_camera(viewer, b.id),
            "out of the allow-list"
        );
        // Unknown user → conservative refuse (never leak a camera-scoped alert).
        assert!(!db.user_can_see_camera(999_999, a.id));
    }

    #[test]
    fn user_email_set_list_clear() {
        let db = mem_db();
        let uid = db.add_user("carol", "h", "viewer", 0).unwrap();
        assert!(!db.any_user_email());
        assert!(db.set_user_email(uid, Some("carol@example.com")).unwrap());
        assert_eq!(
            db.user_email(uid).unwrap().as_deref(),
            Some("carol@example.com")
        );
        assert!(db.any_user_email());
        assert_eq!(
            db.users_with_email().unwrap(),
            vec![(uid, "carol@example.com".to_string())]
        );
        // Clearing removes it from every read path.
        assert!(db.set_user_email(uid, None).unwrap());
        assert!(db.user_email(uid).unwrap().is_none());
        assert!(!db.any_user_email());
    }

    #[test]
    fn alarm_notification_tags_round_trip() {
        let db = mem_db();
        // Alarm-tagged fire: rule_id/camera_id/severity all persist for the worker.
        // (event_id NULL here — the column has a FK to events; production passes a
        // real event id.)
        let id = db
            .add_alarm_notification(
                100,
                "alarm",
                "Front door",
                Some("person"),
                None,
                Some(42),
                Some(11),
                Some(3),
            )
            .unwrap();
        let got = db.notifications_after(id - 1, 10).unwrap();
        let n = got.iter().find(|n| n.id == id).unwrap();
        assert_eq!(n.rule_id, Some(42));
        assert_eq!(n.camera_id, Some(11));
        assert_eq!(n.severity, Some(3));
        // A plain system notification leaves all the P2.11 tags NULL.
        let sid = db
            .add_notification(200, "camera_offline", "Cam down", None, None)
            .unwrap();
        let sys = db.notifications_after(sid - 1, 10).unwrap();
        let s = sys.iter().find(|n| n.id == sid).unwrap();
        assert_eq!(s.rule_id, None);
        assert_eq!(s.camera_id, None);
        assert_eq!(s.severity, None);
    }
}
