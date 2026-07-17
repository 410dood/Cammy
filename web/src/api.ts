// Typed client for the zoomy core API.

export type ZoneKind = "ignore" | "required";

export interface PolyZone {
  name: string;
  points: [number, number][];
  kind: ZoneKind;
  labels: string[];
  /** Loitering threshold in seconds (requires tracking); null/0 = not a dwell zone. */
  dwell_secs?: number | null;
  /** Live-occupancy limit: an `occupancy` event fires when the count inside first
   *  exceeds this (edge-triggered). null/0 = no limit. Requires tracking. */
  occupancy_max?: number | null;
  /** Residential: fire a `zone_enter` event (labelled with the object's class)
   *  when a tracked object enters — "person in the pool", "pet on the couch". */
  alert_enter?: boolean;
  /** Residential: fire a `child` event when a child-classified person enters
   *  (stairs/kitchen/driveway). Requires child calibration. ASSISTIVE. */
  child_watch?: boolean;
  /** Residential: fire `child_alone` when a child is here with no adult present
   *  (unattended-near-pool). Requires child calibration. ASSISTIVE — not a
   *  substitute for supervision/fencing. */
  supervise?: boolean;
  /** Residential: this zone is water (a pool); a motionless person fires an
   *  EXPERIMENTAL `still_water` hint. NOT drowning detection. */
  water?: boolean;
  /** P3.5 zero-shot zone-state classifier (EXPERIMENTAL): classify a binary
   *  open/closed state from the two CLIP prompts below and emit a `zone_open` /
   *  `zone_closed` event scoped by the zone name. Needs the smart-search (CLIP)
   *  models; silently no-ops without them. Best-effort, not a security sensor. */
  state_classify?: boolean;
  /** CLIP prompt for the OPEN state, e.g. "an open garage door". */
  open_prompt?: string | null;
  /** CLIP prompt for the CLOSED state, e.g. "a closed garage door". */
  closed_prompt?: string | null;
}

export type CrossDir = "both" | "a_to_b" | "b_to_a";

/** A directed virtual line for line-crossing analytics (frame-fraction coords). */
export interface Tripwire {
  name: string;
  a: [number, number];
  b: [number, number];
  direction: CrossDir;
  labels: string[];
  /** One-way enforcement: a crossing in the forbidden direction fires wrong_way. */
  alert_wrong_way?: boolean;
}

/** Ground-plane calibration for speed estimation: 4 image corners (fractions)
 *  of a real ground rectangle, top-left/top-right/bottom-right/bottom-left. */
export interface GroundCalib {
  points: [[number, number], [number, number], [number, number], [number, number]];
  width_m: number;
  height_m: number;
}

/** A day-of-week + time-of-day window (recording schedule). days: 0=Sun..6=Sat,
 *  empty = every day; HH:MM strings; start > end = overnight window. */
export interface Schedule {
  days: number[];
  start_hhmm: string | null;
  end_hhmm: string | null;
}

/** Day-of-week display names, index-aligned with Schedule.days (0=Sun..6=Sat). */
export const DAY_NAMES = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

export interface DetectConfig {
  labels: string[] | null;
  min_score: number | null;
  motion_threshold: number | null;
  zones: PolyZone[];
  tripwires: Tripwire[];
  ground_calib?: GroundCalib | null;
  privacy_masks: [number, number][][];
  min_area: number | null;
  max_area: number | null;
  autotrack: boolean;
  audio_detect: boolean;
  event_only_recording: boolean;
  gesture_detect: boolean;
  model: string | null;
  force_cpu: boolean | null;
  /** Per-camera named execution provider. null/"" inherits the global
   *  Settings.accelerator; "auto" = best per-OS EP; "cpu"; "openvino" (Intel,
   *  only when the build/runtime supports it). Wins over force_cpu when set. */
  accelerator?: string | null;
  poll_ms: number | null;
  face_recognize: boolean | null;
  two_way_audio: boolean;
  /** Camera tamper/defocus/scene-change detection (#63). */
  tamper_detect: boolean;
  /** Gait analysis & identification on person tracks (#64). */
  gait_identify: boolean;
  /** Per-camera retention override in days; null inherits the global setting. */
  retention_days: number | null;
  /** Residential ASSISTIVE fall hint: a person who goes motionless low in frame
   *  fires a `fall` event. Best-effort at ~1 fps; NOT a medical-alert device. */
  fall_detect?: boolean;
  /** Residential child/adult calibration: a person whose normalized bbox height
   *  is ≤ this fraction is treated as a "child". null disables child features
   *  (the default). FRAGILE without per-camera setup. */
  child_height_frac?: number | null;
  /** Inactivity watch: alert when no person/pet seen for this many hours
   *  (edge-triggered; assistive only). null = off. */
  absence_hours?: number | null;
  /** Ingest the camera's own analytics (ONVIF events: motion/IVS/person/vehicle)
   *  as camera_* events — zero server GPU cost. Needs ONVIF credentials in the
   *  source URL. */
  onvif_events?: boolean;
  /** Server-side 24/7 body-pose monitoring (fall posture, crib standing/rollover,
   *  covered-face). Runs a YOLOv8-pose model. Opt-in + ASSISTIVE only. */
  pose_detect?: boolean;
  /** Privacy: residential + pose safety events on this camera fire WITHOUT saving
   *  a snapshot image (nursery/bedroom/bathroom dignity). Off by default. */
  no_clip?: boolean;
  /** Per-camera recording schedule; null = always record (#67). */
  record_schedule?: Schedule | null;
  /** Package/parcel monitoring (#69): emit package / package_removed events. */
  package_detect?: boolean;
  /** Optional polygon (0..1) the parcel must sit in; null = whole frame. */
  package_zone?: [number, number][] | null;
  /** Labels counting as a parcel; empty = suitcase/backpack/handbag. */
  package_labels?: string[];
  /** Stationary-object suppression: only alert on new or moving objects, not on
   *  every motion-gate trip while a parked car / idle object sits in view. */
  suppress_stationary?: boolean;
  /** Detection-triggered recording (P3.8): a tighter, asymmetric retention mode.
   *  Recording stays continuous; only pruning is harder — keep pre/post-roll
   *  around each detection and delete the rest within ~a minute. Mutually
   *  exclusive with event_only_recording (UI-enforced). Bookmarked events are
   *  always kept. */
  trigger_recording?: boolean;
  /** Seconds of footage kept BEFORE each detection (pre-roll); null = 10s. */
  trigger_pre_roll_secs?: number | null;
  /** Seconds of footage kept AFTER each detection (post-roll); null = 30s. */
  trigger_post_roll_secs?: number | null;
  /** Dual-stream recording (P3.7): also record the low-res detect sub-stream to
   *  disk alongside the main stream, so the UI can scrub SD and play HD. Opt-in,
   *  off by default; requires a detect sub-stream (detect_source) to exist. */
  record_substream?: boolean;
}

export interface Camera {
  id: number;
  name: string;
  source: string;
  detect_source: string | null;
  enabled: boolean;
  detect: boolean;
  record: boolean;
  created_ts: number;
  detect_config: DetectConfig;
  group: string | null;
}

/** P3.10 result of importing an offline video file as a virtual camera. */
export interface ImportSummary {
  camera_id: number;
  camera: string;
  frames_scanned: number;
  events_created: number;
}

export interface CamEvent {
  id: number;
  camera_id: number;
  camera: string;
  ts: number;
  label: string;
  score: number;
  box: [number, number, number, number];
  snapshot: string | null;
  face: string | null;
  plate: string | null;
  gesture: string | null;
  zone: string | null;
  caption: string | null;
  transcript: string | null;
  flagged: boolean;
  note: string | null;
  /** Attributed gait identity (#64): an enrolled name or "?" (unknown walker). */
  gait?: string | null;
  /** Severity tier 1 (low) .. 4 (critical) — drives push gating + badges. */
  severity?: number;
  /** Anomaly score 0..1 from the opt-in anomaly worker (null = unscored);
   *  a ranking signal for the Home Spotlights feed. */
  anomaly_score?: number | null;
  /** User-applied tags (multi-tag taxonomy beyond flag+note). */
  tags?: string[];
  /** Line-crossing direction ("a_to_b"/"b_to_a") on a tracker crossing event. */
  direction?: string | null;
  /** Estimated ground speed (km/h) on a calibrated crossing event. */
  speed?: number | null;
  /** Tracker track id on a tracker-driven narrative event (line-crossing,
   *  loiter, zone-enter, child, fall, still-water); null otherwise. Presence
   *  gates the object-lifecycle ("Track") view. */
  track_id?: number | null;
}

export interface GaitProfile {
  id: number;
  name: string;
  samples: number;
  created_ts: number;
  updated_ts: number;
}

export interface Segment {
  id: number;
  camera_id: number;
  camera: string;
  start_ts: number;
  bytes: number;
  path: string;
  /** P3.7: 'main' (full-res) or 'sub' (opt-in low-res scrub copy). */
  stream?: string;
}

/** One region-motion hit range (P2.3): consecutive minutes with motion in the region. */
export interface MotionHit {
  ts: number;
  end_ts: number;
  segment_id: number | null;
  offset_secs: number | null;
  segment_start_ts: number | null;
}

export interface Settings {
  detect_labels: string[];
  confidence: number;
  nms_iou: number;
  motion_threshold: number;
  poll_ms: number;
  event_cooldown_secs: number;
  segment_seconds: number;
  retention_days: number;
  retention_gb: number;
  event_retention_days: number;
  enhanced_retention_days: number;
  hwaccel: string;
  recordings_dir: string;
  model_path: string;
  force_cpu: boolean;
  /** Global named execution provider: "" = best per-OS EP (default), "cpu",
   *  "openvino" (Intel, only when the build/runtime supports it). Wins over
   *  force_cpu when set. */
  accelerator: string;
  go2rtc_api_port: number;
  webhook_url: string;
  record_audio: boolean;
  alert_labels: string[];
  mqtt_url: string;
  mqtt_prefix: string;
  mqtt_ha_discovery: boolean;
  mqtt_ha_prefix: string;
  mqtt_state_timeout_secs: number;
  mqtt_commands_enabled: boolean;
  webhook_template: string;
  face_recognition: boolean;
  face_match_threshold: number;
  face_det_model: string;
  face_rec_model: string;
  plate_denylist: string[];
  plate_allowlist: string[];
  health_ntfy_url: string;
  public_base_url: string;
  /** Minimum severity (1..4) for push/email alarm actions; 1 = everything.
   *  Webhook/MQTT automations and duress are never gated. */
  notify_min_severity: number;
  gesture_recognition: boolean;
  gesture_hold_secs: number;
  gesture_labels: string[];
  gesture_duress: string;
  gesture_model_url: string;
  genai_enabled: boolean;
  genai_url: string;
  genai_model: string;
  genai_api_key: string;
  transcription_enabled: boolean;
  transcription_model: string;
  /** P2.9 master kill-switch for deterrence (ONVIF relay siren/light) actions. */
  deterrence_enabled: boolean;
  anomaly_detection: boolean;
  digest_enabled: boolean;
  liveviews: Liveview[];
  floorplan: string;
  /** AudioSet display names (yamnet_class_map.csv) that fire audio events. */
  audio_labels: string[];
  /** Mean YAMNet score (0..1) needed to fire an audio event. */
  audio_threshold: number;
  /** SMTP for the "email" alarm action. smtp_pass is write-only (blank on read;
   *  blank on save keeps the stored one). */
  smtp_url: string;
  smtp_user: string;
  smtp_pass: string;
  smtp_from: string;
  smtp_to: string;
  /** Auto-arm/disarm schedule (residential "modes" automation). Empty = off. */
  arm_schedule: ArmScheduleEntry[];
  /** Path to the YOLOv8-pose ONNX model for the server-side pose worker
   *  (downloaded, not committed). The worker idles until it exists. */
  pose_model: string;
  /** Reverse-proxy SSO (forward auth): header carrying the authenticated user
   *  (e.g. "Remote-User"). Empty = off. Only honored with --trusted-proxy. */
  auth_proxy_header: string;
  /** Optional header carrying the user's role/group (admin/operator/viewer). */
  auth_proxy_role_header: string;
  /** Role for an SSO user with no role header + no matching account. */
  auth_proxy_default_role: string;
  /** Offsite backup of recordings to S3-compatible storage (#70). Global
   *  settings (mirrors the Rust `Settings` struct, not per-schedule-row).
   *  offsite_secret_key is write-only (blank on read; blank on save keeps it). */
  offsite_backup_enabled: boolean;
  offsite_endpoint: string;
  offsite_region: string;
  offsite_bucket: string;
  offsite_prefix: string;
  offsite_access_key: string;
  offsite_secret_key: string;
  /** P2.14 — back up only segments around events (far less upload/remote
   *  storage than a full continuous mirror). Bookmarked footage is still
   *  covered. Off by default = mirror every sealed segment. */
  offsite_events_only: boolean;
  /** Burn an amber outline of the motion region(s) that tripped the gate onto
   *  detection snapshots, so you can see what actually triggered an event. */
  highlight_motion: boolean;
  /** Number of parallel detection worker threads (1..8). Cameras are sharded
   *  across the workers so one slow camera can't stall the others. Takes effect
   *  after a restart; each worker uses its own detector session. */
  detect_workers: number;
}

/** One auto-arm/disarm schedule row: at `hhmm` on `days` (0=Sun; empty=every
 *  day), set the system to `mode`. Driven by the schedule worker. */
export interface ArmScheduleEntry {
  days: number[];
  hhmm: string;
  mode: ArmMode;
}

export interface OffsiteStatus {
  enabled: boolean;
  configured: boolean;
  last_success_ts: number | null;
  backlog: number;
  bytes_total: number;
  done: number;
  skipped: number;
  gaveup: number;
  last_error: string | null;
  per_camera: { camera: string; bytes: number }[];
}

export interface CamStorage {
  camera_id: number;
  camera: string;
  segments: number;
  bytes: number;
  oldest_ts: number | null;
  newest_ts: number | null;
}

export interface Stats {
  cameras: CamStorage[];
  total_bytes: number;
  snapshots_bytes: number;
  events_total: number;
  disk_free_bytes: number;
  recordings_root: string;
  /** Estimated write rate + when the disk fills / retention caps history. */
  write_bytes_per_day: number;
  days_until_full: number | null;
  est_full_ts: number | null;
  retention_horizon_days: number | null;
}

export interface DiscoveredCam {
  host: string;
  name: string | null;
}

export interface AppConfig {
  go2rtc_base: string;
  /** Server build version (Cargo package version), e.g. "0.4.0". */
  version?: string;
}

export type ArmMode = "home" | "away" | "disarmed";
export type ActionKind = "webhook" | "mqtt" | "ntfy" | "email" | "deterrence";

/** One ONVIF relay output a camera advertises (P2.9 deterrence). */
export interface RelayOutput {
  token: string;
  mode: string | null;
}

/** Result of probing a camera's relay-output (siren/light) capability. */
export interface DeterCaps {
  relays: RelayOutput[];
  error: string | null;
}

/** One tracked occupant for presence/geofence arming (P2.10). */
export interface Occupant {
  id: number;
  name: string;
  home: boolean;
  updated_ts: number;
  created_ts: number;
}

/// One action a rule fires. A rule can fire several at once (a "scene").
export interface Action {
  kind: ActionKind;
  target: string;
  priority: number;
}

export interface AlarmRule {
  id: number;
  name: string;
  enabled: boolean;
  camera_id: number | null;
  label: string | null;
  face_like: string | null;
  plate_like: string | null;
  gesture_like: string | null;
  transcript_like: string | null;
  face_unknown: boolean;
  /** Residential: scope the rule to a named detection zone (substring,
   *  case-insensitive) — "person in the Pool zone". null = any zone. */
  zone_like: string | null;
  /** Cross-modal confirmation: only fire when an event of this label also
   *  occurred on the same camera within confirm_within_secs (glass-vs-dishes).
   *  null = no confirmation. Fails open; don't gate life-safety rules on it. */
  confirm_label: string | null;
  confirm_within_secs: number | null;
  /** VLM alert-verification gate: a yes/no question asked of the GenAI vision
   *  model about the snapshot; the rule fires only when the model confirms
   *  ("Is a real person at the door?"). Runs off-thread; fails OPEN. Needs GenAI
   *  captions enabled. null/empty = no gate. Detection-event rules only. */
  vlm_prompt: string | null;
  /** Describe-in-notification: caption the snapshot (GenAI) and put the
   *  description IN the push/email. Fails open to a normal caption-less alert.
   *  Needs GenAI captions enabled. Detection-event rules only. */
  describe?: boolean;
  /** Prompt-based standing rule: free text ("someone climbing the fence")
   *  CLIP-matched against each detection's crop. Needs the CLIP models;
   *  best-effort semantic matching — scope with label/zone for precision. */
  prompt_like?: string | null;
  /** P2.5 attribute facet: a curated catalog KEY (e.g. "veh_color_red"), not
   *  free text — resolved server-side to a CLIP prompt and matched exactly like
   *  prompt_like (an "AI watch"-style best-effort gate). null = none. */
  attr_like?: string | null;
  min_score: number;
  /** Legacy single action; kept in sync with actions[0]. Prefer `actions`. */
  action: string;
  target: string;
  days: number[];
  start_hhmm: string | null;
  end_hhmm: string | null;
  cooldown_secs: number;
  priority: number;
  snooze_until: number;
  created_ts: number;
  /** Arm modes this rule fires in; empty = home+away (suppressed when disarmed). */
  modes: ArmMode[];
  /** Actions fired (a "scene"). Empty falls back to the legacy action/target. */
  actions: Action[];
}

export interface ApiToken {
  id: number;
  name: string;
  role: Role;
  created_ts: number;
  last_used_ts: number | null;
}

/** A shareable, expiring clip link (metadata only — never the token). */
export interface ClipShare {
  id: number;
  event_id: number;
  label: string | null;
  camera: string | null;
  expires_ts: number;
  revoked: boolean;
  created_ts: number;
}

export interface AuditEntry {
  id: number;
  ts: number;
  ip: string | null;
  action: string;
  detail: string | null;
}

export interface CamStatus {
  online: boolean;
  recording: boolean;
  last_frame_ts: number | null;
  last_error: string | null;
  inference_ms: number | null;
  accelerator: string | null;
  model: string | null;
  /** Active tamper kind (blackout/defocus/scene_change) if compromised (#63). */
  tamper?: string | null;
  /** Last detection event on this camera (unix secs) — drives the Live activity sort. */
  last_detection_ts?: number | null;
}

export type StatusMap = Record<string, CamStatus>;

export interface Notification {
  id: number;
  ts: number;
  kind: string; // "stranger" | "camera_offline" | "digest" | "anomaly" | ...
  title: string;
  body: string | null;
  event_id: number | null;
  read: boolean;
}

export interface Digest {
  id: number;
  ts: number;
  text: string;
}

export type PlateCategory = "known" | "watch";

/** A named entry in the license-plate library (the vehicle analog of an
 *  enrolled face). `plate` is the normalized key (uppercase, alphanumerics). */
export interface PlateEntry {
  id: number;
  plate: string;
  name: string;
  category: PlateCategory;
  note: string | null;
  created_ts: number;
}

export interface Overview {
  cameras_total: number;
  cameras_online: number;
  recording: number;
  events_total: number;
  events_today: number;
  disk_free_bytes: number;
  total_bytes: number;
  today_by_label: [string, number][];
  unread_notifications: number;
  arm_mode: ArmMode;
}

/** Historical throughput roll-up from the crossing/wrong_way + loiter events. */
export interface AnalyticsCounts {
  crossings: { tripwire: string | null; direction: string | null; count: number }[];
  loiters: { zone: string | null; count: number }[];
}

/** Live per-camera, per-zone occupancy (current # of tracks inside each zone). */
export interface OccupancyReport {
  cameras: { camera_id: number; camera: string; zones: Record<string, number> }[];
}

/** Activity heatmap: a `grid`×`grid` row-major density map + peak cell value. */
export interface Heatmap {
  grid: number;
  cells: number[];
  max: number;
}

/** Event trends for the Insights dashboard — all aggregated server-side. */
export interface Timeseries {
  days: { day: string; ts: number; count: number }[];
  by_label: [string, number][];
  by_hour: number[];
  total: number;
  from: number;
  range_days: number;
}

/** One cross-camera appearance match (CLIP crop cosine similarity ∈ [0,1]). */
export interface SimilarMatch {
  similarity: number;
  event: CamEvent;
}

/** Result of an appearance ("find this person/vehicle") search for an event. */
export interface SimilarResult {
  results: SimilarMatch[];
  /** False when the event has no crop embedding (not an object detection, or
   *  smart-search models aren't installed). */
  available: boolean;
}

/** One step in a tracked object's life-story — a tracker-driven narrative event
 *  (same shape as any event). */
export type LifecycleStep = CamEvent;

/** The object-lifecycle ("Track") view (P2.16): the ordered story of the physical
 *  object behind a tracker-driven event, plus its recorded trajectory. */
export interface Lifecycle {
  /** False when the event has no track id (ordinary detection / occupancy /
   *  package / camera_* event) — there's no object story to tell. */
  available: boolean;
  track_id?: number;
  /** The object's narrative events, oldest-first, bounded to the contiguous run
   *  around the seed event. */
  steps?: LifecycleStep[];
  /** The object's trajectory as `[ts_ms, x, y]` frame-fraction points (the
   *  richest path recorded across the steps); empty if none was captured. */
  path?: [number, number, number][];
}

/** One CLIP attribute facet (P2.5): a stable catalog key + display label. */
export interface AttrFacet {
  key: string;
  label: string;
  /** The underlying CLIP text prompt (shown as a tooltip — honest framing). */
  prompt: string;
}

/** A group of related attribute facets (e.g. "Vehicle colour"). */
export interface AttrGroup {
  group: string;
  label: string;
  attrs: AttrFacet[];
}

/** The attribute-facet catalog + whether the CLIP models back it. */
export interface AttributesCatalog {
  groups: AttrGroup[];
  /** False when the smart-search (CLIP) models aren't installed — the facets
   *  can't actually match anything, so the UI should say so. */
  available: boolean;
}

/** Optional-model presence for one AI feature (so the UI can flag features whose
 *  backing model isn't downloaded instead of letting them silently no-op). */
export interface Capability {
  key: string;
  label: string;
  /** Expected model filename(s) — never an absolute path. */
  model: string;
  present: boolean;
  /** True for the mandatory object detector (others are optional add-ons). */
  required: boolean;
}

export interface Liveview {
  name: string;
  cameras: string[];
}

export interface FloorPlan {
  image: string; // data URL or /api path
  pins: { camera: string; x: number; y: number }[];
}

export type Role = "viewer" | "operator" | "admin";

export interface User {
  id: number;
  username: string;
  role: Role;
  created_ts: number;
  /** P2.11 notification email (Admin-editable). null = unset. */
  email: string | null;
}

export interface Me {
  authenticated: boolean;
  /** true for a real user account; false for the legacy/loopback/token admin */
  named: boolean;
  username: string | null;
  role: Role;
  /** P2.11 the caller's own notification email (null for the legacy/loopback admin). */
  email?: string | null;
}

/** P2.11 one per-user notification preference: does `channel` deliver alerts from
 *  `rule_id` (0 = the user's default for every rule) to this user? Opt-out model —
 *  no row means enabled. */
export interface NotifyPref {
  user_id: number;
  rule_id: number;
  channel: "push" | "email";
  enabled: boolean;
}

/** Entitlement state mirrored from crates/core/src/licensing.rs (serde tag = "state"). */
export type Entitlement =
  | { state: "licensed"; plan: string; email: string; seats: number; expires: number | null }
  | { state: "trial"; days_left: number; ends: number }
  | { state: "expired"; reason: string };

export interface LicenseInfo {
  entitlement: Entitlement;
  /** Trial length in days (for copy like "30-day trial"). */
  trial_days: number;
  /** Storefront URL the Buy/Upgrade buttons open. */
  buy_url: string;
}

async function req<T>(url: string, init?: RequestInit): Promise<T> {
  const r = await fetch(url, {
    headers: { "Content-Type": "application/json" },
    ...init,
  });
  if (r.status === 401) {
    window.dispatchEvent(new Event("zoomy-401"));
  }
  if (!r.ok) {
    let msg = `${r.status} ${r.statusText}`;
    try {
      const body = await r.json();
      if (body.error) msg = body.error;
    } catch {
      /* keep status text */
    }
    throw new Error(msg);
  }
  if (r.status === 204) return undefined as T;
  return r.json();
}

export const api = {
  config: () => req<AppConfig>("/api/config"),
  capabilities: () => req<{ features: Capability[]; openvino?: boolean }>("/api/capabilities"),
  /** Current entitlement (trial countdown / licensed / expired). */
  license: () => req<LicenseInfo>("/api/license"),
  /** Install a license key (Admin). Returns the resulting entitlement. */
  activateLicense: (key: string) =>
    req<{ entitlement: Entitlement }>("/api/license", {
      method: "POST",
      body: JSON.stringify({ key }),
    }),
  /** Remove the installed license (Admin), reverting to trial/expired. */
  removeLicense: () => req<{ entitlement: Entitlement }>("/api/license", { method: "DELETE" }),
  status: () => req<StatusMap>("/api/status"),
  cameras: () => req<Camera[]>("/api/cameras"),
  addCamera: (c: {
    name: string;
    source: string;
    group?: string;
    detect_source?: string;
    detect: boolean;
    record: boolean;
  }) => req<Camera>("/api/cameras", { method: "POST", body: JSON.stringify(c) }),
  patchCamera: (id: number, patch: Partial<Camera>) =>
    req<Camera>(`/api/cameras/${id}`, { method: "PATCH", body: JSON.stringify(patch) }),
  deleteCamera: (id: number) => req<void>(`/api/cameras/${id}`, { method: "DELETE" }),
  /** P3.10 import offline footage (Admin): run detection over a SERVER-LOCAL
   *  video file, creating events on a disabled "virtual" camera. Not a browser
   *  upload — `path` is a file path on the machine running Cammy. */
  importFootage: (path: string, camera_name: string, base_ts?: number) =>
    req<ImportSummary>("/api/import", {
      method: "POST",
      body: JSON.stringify({ path, camera_name, base_ts: base_ts ?? null }),
    }),
  /** Replace an event's user tags (≤8, sanitized server-side). */
  setEventTags: (id: number, tags: string[]) =>
    req<{ tags: string[] }>(`/api/events/${id}/tags`, {
      method: "POST",
      body: JSON.stringify({ tags }),
    }),
  /** Fire a rule's actions once with a synthetic TEST event (no event created,
   *  cooldown untouched) — verifies the webhook/ntfy/email wiring. */
  testAlarm: (id: number) =>
    req<{ fired: boolean }>(`/api/alarms/${id}/test`, { method: "POST" }),
  /** Per-rule throttle stats (this run): last-fired ts + cooldown-suppressed count. */
  alarmStats: () =>
    req<Record<string, { last_fired_ts: number; suppressed_since: number }>>("/api/alarms/stats"),
  /** Soft trigger: create a bookmarked event ("Delivery arrived") with a live
   *  snapshot on a camera; alarm rules matching the label fire. */
  softTrigger: (id: number, label?: string) =>
    req<{ recorded: boolean; event_id: number; label: string }>(
      `/api/cameras/${id}/trigger`,
      { method: "POST", body: JSON.stringify({ label: label ?? null }) },
    ),
  restore: (backup: unknown) =>
    req<{
      settings_applied: boolean;
      cameras_added: number;
      cameras_skipped: number;
      alarms_added: number;
    }>("/api/restore", { method: "POST", body: JSON.stringify(backup) }),
  events: (
    q: {
      camera_id?: number;
      label?: string;
      gesture?: string;
      zone?: string;
      after?: number;
      before?: number;
      flagged?: boolean;
      tag?: string;
      limit?: number;
    } = {}
  ) => {
    const p = new URLSearchParams();
    if (q.camera_id != null) p.set("camera_id", String(q.camera_id));
    if (q.label) p.set("label", q.label);
    if (q.gesture) p.set("gesture", q.gesture);
    if (q.zone) p.set("zone", q.zone);
    if (q.after != null) p.set("after", String(q.after));
    if (q.before != null) p.set("before", String(q.before));
    if (q.flagged) p.set("flagged", "true");
    if (q.tag) p.set("tag", q.tag);
    if (q.limit) p.set("limit", String(q.limit));
    return req<CamEvent[]>(`/api/events?${p}`);
  },
  /** One event by id — resolves deep links to events older than a loaded list. */
  event: (id: number) => req<CamEvent>(`/api/events/${id}`),
  bookmarkEvent: (id: number, flagged: boolean, note?: string | null) =>
    req<{ id: number; flagged: boolean }>(`/api/events/${id}/bookmark`, {
      method: "POST",
      body: JSON.stringify({ flagged, note: note ?? null }),
    }),
  // P2.8b feedback learning: thumbs-down an alert. On success the server stores
  // the event's object-crop embedding so CLIP-similar FUTURE alerts on the same
  // camera are quieted (AI-watch / AI-verified rules only in v0). An event with
  // no object crop returns {ok:false, reason:"no_crop"}.
  eventFeedback: (id: number) =>
    req<{ ok: boolean; suppressed?: boolean; reason?: string }>(
      `/api/events/${id}/feedback`,
      { method: "POST" }
    ),
  recordGesture: (body: { camera?: string; gesture: string; score?: number }) =>
    req<{ recorded: boolean; event_id?: number; gesture?: string; reason?: string; duress?: boolean }>(
      "/api/gesture",
      { method: "POST", body: JSON.stringify(body) }
    ),
  recordings: (q: { camera_id?: number; before?: number; limit?: number } = {}) => {
    const p = new URLSearchParams();
    if (q.camera_id != null) p.set("camera_id", String(q.camera_id));
    if (q.before != null) p.set("before", String(q.before));
    if (q.limit) p.set("limit", String(q.limit));
    return req<Segment[]>(`/api/recordings?${p}`);
  },
  /** Resolve the segment covering `ts`. `stream` picks the recording stream
   *  (P3.7): omit or "main" for full-res HD, "sub" for the low-res scrub copy. */
  recordingAt: (camera_id: number, ts: number, stream?: "main" | "sub") =>
    req<{ segment: Segment; offset_secs: number }>(
      `/api/recordings/at?camera_id=${camera_id}&ts=${ts}${stream ? `&stream=${stream}` : ""}`
    ),
  /** P2.3: minutes with motion inside a 0..1 frame rectangle, as playable ranges. */
  motionSearch: (q: { camera_id: number; x1: number; y1: number; x2: number; y2: number; from: number; to: number }) =>
    req<{ hits: MotionHit[]; truncated: boolean }>(
      `/api/motion/search?camera_id=${q.camera_id}&x1=${q.x1}&y1=${q.y1}&x2=${q.x2}&y2=${q.y2}&from=${q.from}&to=${q.to}`
    ),
  // Kick off (or poll) a day time-lapse; returns {status: ready|building, url}.
  timelapse: (camera_id: number, date: string) =>
    req<{ status: "ready" | "building"; url: string }>(
      `/api/cameras/${camera_id}/timelapse?date=${encodeURIComponent(date)}`,
      { method: "POST" }
    ),
  armMode: () => req<{ arm_mode: ArmMode }>("/api/arm"),
  arm: (mode: ArmMode) =>
    req<{ arm_mode: ArmMode }>("/api/arm", { method: "PUT", body: JSON.stringify({ mode }) }),
  presence: () => req<Occupant[]>("/api/presence"),
  armPresence: (occupant: string, home: boolean) =>
    req<{ arm_mode: ArmMode; occupants_home: number }>("/api/arm", {
      method: "POST",
      body: JSON.stringify({ occupant, home }),
    }),
  deletePresence: (id: number) =>
    req<{ deleted: boolean }>(`/api/presence/${id}`, { method: "DELETE" }),
  alarms: () => req<AlarmRule[]>("/api/alarms"),
  addAlarm: (r: Omit<AlarmRule, "id" | "created_ts">) =>
    req<{ id: number }>("/api/alarms", { method: "POST", body: JSON.stringify(r) }),
  updateAlarm: (id: number, r: Omit<AlarmRule, "id" | "created_ts">) =>
    req<void>(`/api/alarms/${id}`, { method: "PUT", body: JSON.stringify(r) }),
  patchAlarm: (id: number, patch: { enabled?: boolean; snooze_secs?: number }) =>
    req<void>(`/api/alarms/${id}`, { method: "PATCH", body: JSON.stringify(patch) }),
  deleteAlarm: (id: number) => req<void>(`/api/alarms/${id}`, { method: "DELETE" }),
  search: (q: string, limit = 24) =>
    req<{ results: { similarity: number; event: CamEvent }[] }>(
      `/api/search?q=${encodeURIComponent(q)}&limit=${limit}`
    ),
  // Upload-a-reference-photo appearance search (UniFi "Find Anything"): POST the
  // raw image bytes; the server CLIP-embeds it and ranks the crop corpus.
  searchByImage: (file: File | Blob, limit = 24) =>
    req<SimilarResult>(`/api/search/by-image?limit=${limit}`, {
      method: "POST",
      body: file,
      // Clear the default JSON content-type so the browser tags the binary body;
      // the server sniffs the image format regardless.
      headers: {},
    }),
  // P2.5 CLIP attribute facets: the curated catalog (Events filter chips +
  // the attr_like alarm dropdown) and a facet search that ranks the crop corpus
  // against the facet's prompt (zero new inference — reuses Re-ID embeddings).
  attributes: () => req<AttributesCatalog>("/api/attributes"),
  searchByAttr: (key: string, limit = 48) =>
    req<SimilarResult>(`/api/search/by-attr?key=${encodeURIComponent(key)}&limit=${limit}`),
  faces: () =>
    req<{ enrolled: { id: number; name: string; created_ts: number }[]; unknown: string[] }>(
      "/api/faces"
    ),
  enrollFace: (name: string, unknown_file: string) =>
    req<{ id: number }>("/api/faces", {
      method: "POST",
      body: JSON.stringify({ name, unknown_file }),
    }),
  deleteFace: (id: number) => req<void>(`/api/faces/${id}`, { method: "DELETE" }),
  // Gait identification (#64): enrolled profiles + unknown-walker candidates.
  gait: () => req<{ profiles: GaitProfile[]; candidates: CamEvent[] }>("/api/gait"),
  enrollGait: (event_id: number, name: string) =>
    req<{ id: number; name: string }>("/api/gait", {
      method: "POST",
      body: JSON.stringify({ event_id, name }),
    }),
  renameGait: (id: number, name: string) =>
    req<void>(`/api/gait/${id}`, { method: "PATCH", body: JSON.stringify({ name }) }),
  deleteGait: (id: number) => req<void>(`/api/gait/${id}`, { method: "DELETE" }),
  renameFace: (id: number, name: string) =>
    req<void>(`/api/faces/${id}`, { method: "PATCH", body: JSON.stringify({ name }) }),
  plates: () => req<PlateEntry[]>("/api/plates"),
  addPlate: (body: { plate: string; name: string; category: PlateCategory; note?: string }) =>
    req<{ id: number; plate: string }>("/api/plates", { method: "POST", body: JSON.stringify(body) }),
  updatePlate: (id: number, body: { name: string; category: PlateCategory; note?: string }) =>
    req<void>(`/api/plates/${id}`, { method: "PATCH", body: JSON.stringify(body) }),
  deletePlate: (id: number) => req<void>(`/api/plates/${id}`, { method: "DELETE" }),
  ptzCaps: (id: number) => req<{ supported: boolean }>(`/api/cameras/${id}/ptz`),
  ptz: (id: number, cmd: { action: "move" | "stop"; pan?: number; tilt?: number; zoom?: number }) =>
    req<{ ok: boolean }>(`/api/cameras/${id}/ptz`, { method: "POST", body: JSON.stringify(cmd) }),
  deterProbe: (id: number) => req<DeterCaps>(`/api/cameras/${id}/deter`),
  deterTest: (id: number, token: string, activeSecs?: number) =>
    req<{ ok: boolean }>(`/api/cameras/${id}/deter`, {
      method: "POST",
      body: JSON.stringify({ token, active_secs: activeSecs }),
    }),
  tokens: () => req<ApiToken[]>("/api/tokens"),
  createToken: (name: string, role: Role = "operator") =>
    req<{ id: number; name: string; role: Role; token: string }>("/api/tokens", {
      method: "POST",
      body: JSON.stringify({ name, role }),
    }),
  deleteToken: (id: number) => req<void>(`/api/tokens/${id}`, { method: "DELETE" }),
  shareEvent: (id: number, ttlHours: number) =>
    req<{ id: number; token: string; path: string; expires_ts: number }>(
      `/api/events/${id}/share`,
      { method: "POST", body: JSON.stringify({ ttl_hours: ttlHours }) },
    ),
  shares: () => req<ClipShare[]>("/api/shares"),
  revokeShare: (id: number) => req<void>(`/api/shares/${id}`, { method: "DELETE" }),
  audit: (limit = 100) => req<AuditEntry[]>(`/api/audit?limit=${limit}`),
  authStatus: () => req<{ enabled: boolean; users: number }>("/api/auth"),
  login: (password: string, username?: string, otp?: string) =>
    req<{ ok: boolean; mfa_required?: boolean }>("/api/login", {
      method: "POST",
      body: JSON.stringify({ username: username || undefined, password, otp: otp || undefined }),
    }),
  me: () => req<Me>("/api/me"),
  changeMyPassword: (old_password: string, new_password: string) =>
    req<{ ok: boolean }>("/api/me/password", {
      method: "POST",
      body: JSON.stringify({ old_password, new_password }),
    }),
  // Two-factor authentication (TOTP) for the caller's own credential.
  twofaStatus: () =>
    req<{ enabled: boolean; pending: boolean; scope: "user" | "shared"; account: string }>(
      "/api/2fa"
    ),
  twofaSetup: () =>
    req<{ secret: string; otpauth_uri: string; account: string }>("/api/2fa/setup", {
      method: "POST",
    }),
  twofaEnable: (code: string) =>
    req<{ ok: boolean; recovery_codes: string[] }>("/api/2fa/enable", {
      method: "POST",
      body: JSON.stringify({ code }),
    }),
  twofaDisable: (code?: string) =>
    req<{ ok: boolean }>("/api/2fa/disable", {
      method: "POST",
      body: JSON.stringify({ code: code || "" }),
    }),
  users: () => req<User[]>("/api/users"),
  createUser: (body: { username: string; password: string; role: Role }) =>
    req<{ id: number }>("/api/users", { method: "POST", body: JSON.stringify(body) }),
  patchUser: (
    id: number,
    patch: { role?: Role; password?: string; disable_2fa?: boolean; email?: string | null }
  ) => req<{ id: number }>(`/api/users/${id}`, { method: "PATCH", body: JSON.stringify(patch) }),
  deleteUser: (id: number) => req<void>(`/api/users/${id}`, { method: "DELETE" }),
  // Per-camera RBAC scope: the camera ids a user may see (empty = all).
  userCameras: (id: number) => req<number[]>(`/api/users/${id}/cameras`),
  setUserCameras: (id: number, camera_ids: number[]) =>
    req<{ id: number; cameras: number }>(`/api/users/${id}/cameras`, {
      method: "PUT",
      body: JSON.stringify({ camera_ids }),
    }),
  // P2.11 per-user notification matrix (which rules reach the user over which
  // channel). Empty list = all defaults on.
  userNotifyPrefs: (id: number) => req<NotifyPref[]>(`/api/users/${id}/notify-prefs`),
  setUserNotifyPrefs: (id: number, prefs: NotifyPref[]) =>
    req<{ id: number; prefs: number }>(`/api/users/${id}/notify-prefs`, {
      method: "PUT",
      body: JSON.stringify(prefs),
    }),
  // Self-service: the logged-in named user sets their own notification email.
  setMyEmail: (email: string) =>
    req<{ email: string | null }>("/api/me/email", {
      method: "POST",
      body: JSON.stringify({ email }),
    }),
  setPassword: (password: string) =>
    req<{ enabled: boolean }>("/api/auth/password", {
      method: "POST",
      body: JSON.stringify({ password }),
    }),
  discover: (host: string, username: string, password: string) =>
    req<{ sources: { name: string; url: string }[] }>("/api/discover", {
      method: "POST",
      body: JSON.stringify({ host, username, password }),
    }),
  scanNetwork: () => req<{ cameras: DiscoveredCam[] }>("/api/discover/scan"),
  stats: () => req<Stats>("/api/stats"),
  settings: () => req<Settings>("/api/settings"),
  saveSettings: (s: Settings) =>
    req<Settings>("/api/settings", { method: "PUT", body: JSON.stringify(s) }),
  offsiteStatus: () => req<OffsiteStatus>("/api/offsite/status"),
  overview: () => req<Overview>("/api/overview"),
  analyticsCounts: (from?: number, to?: number) => {
    const p = new URLSearchParams();
    if (from != null) p.set("from", String(from));
    if (to != null) p.set("to", String(to));
    const qs = p.toString();
    return req<AnalyticsCounts>(`/api/analytics/counts${qs ? `?${qs}` : ""}`);
  },
  analyticsOccupancy: () => req<OccupancyReport>("/api/analytics/occupancy"),
  analyticsTimeseries: (days: number) => req<Timeseries>(`/api/analytics/timeseries?days=${days}`),
  eventSimilar: (id: number, limit?: number) => {
    const p = new URLSearchParams();
    if (limit != null) p.set("limit", String(limit));
    const qs = p.toString();
    return req<SimilarResult>(`/api/events/${id}/similar${qs ? `?${qs}` : ""}`);
  },
  eventLifecycle: (id: number) => req<Lifecycle>(`/api/events/${id}/lifecycle`),
  analyticsHeatmap: (camera: number, from?: number, to?: number, grid?: number) => {
    const p = new URLSearchParams({ camera: String(camera) });
    if (from != null) p.set("from", String(from));
    if (to != null) p.set("to", String(to));
    if (grid != null) p.set("grid", String(grid));
    return req<Heatmap>(`/api/analytics/heatmap?${p}`);
  },
  notifications: (q: { unread?: boolean; limit?: number } = {}) => {
    const p = new URLSearchParams();
    if (q.unread) p.set("unread", "true");
    if (q.limit) p.set("limit", String(q.limit));
    return req<Notification[]>(`/api/notifications?${p}`);
  },
  markNotificationRead: (id: number) =>
    req<{ id: number; read: boolean }>(`/api/notifications/${id}/read`, { method: "POST" }),
  markAllNotificationsRead: () =>
    req<{ updated: number }>("/api/notifications/read-all", { method: "POST" }),
  digests: (limit = 14) => req<Digest[]>(`/api/digests?limit=${limit}`),
  runDigest: () => req<Digest>("/api/digests/run", { method: "POST" }),
  // Native WebPush (#68): no third-party push service.
  pushVapid: () => req<{ public_key: string }>("/api/push/vapid"),
  pushSubscribe: (sub: PushSubscriptionJSON) =>
    req<{ ok: boolean }>("/api/push/subscribe", { method: "POST", body: JSON.stringify(sub) }),
  pushUnsubscribe: (endpoint: string) =>
    req<{ removed: boolean }>("/api/push/unsubscribe", {
      method: "POST",
      body: JSON.stringify({ endpoint }),
    }),
  pushTest: () => req<{ sent: number; failed: number }>("/api/push/test", { method: "POST" }),
};

// Live-view transport. go2rtc restreams a single upstream camera connection to
// any number of clients over WebRTC / MSE / MJPEG, so this is purely a
// per-viewer preference (no extra load on the camera).
export type StreamMode = "webrtc" | "mse" | "mjpeg";

export const getStreamMode = (): StreamMode =>
  (localStorage.getItem("zoomy-stream-mode") as StreamMode) || "webrtc";
export const setStreamMode = (m: StreamMode) => localStorage.setItem("zoomy-stream-mode", m);

/// Build a go2rtc player URL. A comma list is a fallback priority order, so
/// "webrtc" still degrades to MSE when UDP/WebRTC is blocked.
export const streamUrl = (base: string, name: string, mode: StreamMode) => {
  const order = mode === "webrtc" ? "webrtc,mse" : mode === "mse" ? "mse,webrtc" : "mjpeg";
  return `${base}/stream.html?src=${encodeURIComponent(name)}&mode=${order}`;
};

export const fmtTime = (ts: number) => new Date(ts * 1000).toLocaleString();

/** Compact relative time ("just now", "4m", "3h", "yesterday", "Apr 6") for
 *  live feeds. Pairs with a full `fmtTime` tooltip via the <RelTime> component. */
export function relTime(ts: number, nowMs: number = Date.now()): string {
  const s = Math.floor(nowMs / 1000 - ts);
  if (s < 45) return "just now";
  if (s < 90) return "1m ago";
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.floor(s / 86400);
  if (d === 1) return "yesterday";
  if (d < 7) return `${d}d ago`;
  const date = new Date(ts * 1000);
  const sameYear = date.getFullYear() === new Date(nowMs).getFullYear();
  return date.toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    ...(sameYear ? {} : { year: "numeric" }),
  });
}
/** Severity for a projected days-until-disk-full. Key this on actual disk
 *  headroom only — a deliberately short retention horizon is routine pruning,
 *  not an emergency. Shared by Home and Recordings so they can't drift. */
export const capacityTone = (days: number | null | undefined): "danger" | "warn" | null =>
  days == null ? null : days < 2 ? "danger" : days < 7 ? "warn" : null;

/** Human copy for a days-until-full estimate ("under a day", "~3 days"). */
export const fmtDaysLeft = (days: number) => (days < 1 ? "under a day" : `~${Math.round(days)} days`);

export const fmtBytes = (b: number) =>
  b > 1e12
    ? `${(b / 1e12).toFixed(2)} TB`
    : b > 1e9
      ? `${(b / 1e9).toFixed(2)} GB`
      : b > 1e6
        ? `${(b / 1e6).toFixed(1)} MB`
        : `${Math.round(b / 1e3)} KB`;
