// Typed client for the zoomy core API.

export interface Zone {
  x: number;
  y: number;
  w: number;
  h: number;
}

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

export interface DetectConfig {
  labels: string[] | null;
  min_score: number | null;
  motion_threshold: number | null;
  ignore_zones: Zone[];
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
  poll_ms: number | null;
  face_recognize: boolean | null;
  two_way_audio: boolean;
  /** Per-camera retention override in days; null inherits the global setting. */
  retention_days: number | null;
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
}

export interface Segment {
  id: number;
  camera_id: number;
  camera: string;
  start_ts: number;
  bytes: number;
  path: string;
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
  go2rtc_api_port: number;
  webhook_url: string;
  record_audio: boolean;
  alert_labels: string[];
  mqtt_url: string;
  mqtt_prefix: string;
  mqtt_ha_discovery: boolean;
  mqtt_ha_prefix: string;
  mqtt_state_timeout_secs: number;
  webhook_template: string;
  face_recognition: boolean;
  face_match_threshold: number;
  face_det_model: string;
  face_rec_model: string;
  plate_denylist: string[];
  plate_allowlist: string[];
  health_ntfy_url: string;
  public_base_url: string;
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
}

export type ArmMode = "home" | "away" | "disarmed";
export type ActionKind = "webhook" | "mqtt" | "ntfy" | "email";

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
}

export interface Me {
  authenticated: boolean;
  /** true for a real user account; false for the legacy/loopback/token admin */
  named: boolean;
  username: string | null;
  role: Role;
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
    if (q.limit) p.set("limit", String(q.limit));
    return req<CamEvent[]>(`/api/events?${p}`);
  },
  bookmarkEvent: (id: number, flagged: boolean, note?: string | null) =>
    req<{ id: number; flagged: boolean }>(`/api/events/${id}/bookmark`, {
      method: "POST",
      body: JSON.stringify({ flagged, note: note ?? null }),
    }),
  recordGesture: (body: { camera?: string; gesture: string; score?: number }) =>
    req<{ recorded: boolean; event_id?: number; gesture?: string; reason?: string; duress?: boolean }>(
      "/api/gesture",
      { method: "POST", body: JSON.stringify(body) }
    ),
  recordings: (q: { camera_id?: number; limit?: number } = {}) => {
    const p = new URLSearchParams();
    if (q.camera_id != null) p.set("camera_id", String(q.camera_id));
    if (q.limit) p.set("limit", String(q.limit));
    return req<Segment[]>(`/api/recordings?${p}`);
  },
  recordingAt: (camera_id: number, ts: number) =>
    req<{ segment: Segment; offset_secs: number }>(
      `/api/recordings/at?camera_id=${camera_id}&ts=${ts}`
    ),
  armMode: () => req<{ arm_mode: ArmMode }>("/api/arm"),
  arm: (mode: ArmMode) =>
    req<{ arm_mode: ArmMode }>("/api/arm", { method: "PUT", body: JSON.stringify({ mode }) }),
  alarms: () => req<AlarmRule[]>("/api/alarms"),
  addAlarm: (r: Omit<AlarmRule, "id" | "created_ts">) =>
    req<{ id: number }>("/api/alarms", { method: "POST", body: JSON.stringify(r) }),
  patchAlarm: (id: number, patch: { enabled?: boolean; snooze_secs?: number }) =>
    req<void>(`/api/alarms/${id}`, { method: "PATCH", body: JSON.stringify(patch) }),
  deleteAlarm: (id: number) => req<void>(`/api/alarms/${id}`, { method: "DELETE" }),
  search: (q: string, limit = 24) =>
    req<{ results: { similarity: number; event: CamEvent }[] }>(
      `/api/search?q=${encodeURIComponent(q)}&limit=${limit}`
    ),
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
  tokens: () => req<ApiToken[]>("/api/tokens"),
  createToken: (name: string, role: Role = "operator") =>
    req<{ id: number; name: string; role: Role; token: string }>("/api/tokens", {
      method: "POST",
      body: JSON.stringify({ name, role }),
    }),
  deleteToken: (id: number) => req<void>(`/api/tokens/${id}`, { method: "DELETE" }),
  audit: (limit = 100) => req<AuditEntry[]>(`/api/audit?limit=${limit}`),
  authStatus: () => req<{ enabled: boolean; users: number }>("/api/auth"),
  login: (password: string, username?: string) =>
    req<{ ok: boolean }>("/api/login", {
      method: "POST",
      body: JSON.stringify({ username: username || undefined, password }),
    }),
  me: () => req<Me>("/api/me"),
  changeMyPassword: (old_password: string, new_password: string) =>
    req<{ ok: boolean }>("/api/me/password", {
      method: "POST",
      body: JSON.stringify({ old_password, new_password }),
    }),
  users: () => req<User[]>("/api/users"),
  createUser: (body: { username: string; password: string; role: Role }) =>
    req<{ id: number }>("/api/users", { method: "POST", body: JSON.stringify(body) }),
  patchUser: (id: number, patch: { role?: Role; password?: string }) =>
    req<{ id: number }>(`/api/users/${id}`, { method: "PATCH", body: JSON.stringify(patch) }),
  deleteUser: (id: number) => req<void>(`/api/users/${id}`, { method: "DELETE" }),
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
  overview: () => req<Overview>("/api/overview"),
  analyticsCounts: (from?: number, to?: number) => {
    const p = new URLSearchParams();
    if (from != null) p.set("from", String(from));
    if (to != null) p.set("to", String(to));
    const qs = p.toString();
    return req<AnalyticsCounts>(`/api/analytics/counts${qs ? `?${qs}` : ""}`);
  },
  analyticsOccupancy: () => req<OccupancyReport>("/api/analytics/occupancy"),
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
export const fmtBytes = (b: number) =>
  b > 1e9 ? `${(b / 1e9).toFixed(2)} GB` : b > 1e6 ? `${(b / 1e6).toFixed(1)} MB` : `${Math.round(b / 1e3)} KB`;
