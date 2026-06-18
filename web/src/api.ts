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
}

export interface DetectConfig {
  labels: string[] | null;
  min_score: number | null;
  motion_threshold: number | null;
  ignore_zones: Zone[];
  zones: PolyZone[];
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
}

export interface DiscoveredCam {
  host: string;
  name: string | null;
}

export interface AppConfig {
  go2rtc_base: string;
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
  min_score: number;
  action: string;
  target: string;
  days: number[];
  start_hhmm: string | null;
  end_hhmm: string | null;
  cooldown_secs: number;
  priority: number;
  snooze_until: number;
  created_ts: number;
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
    if (q.limit) p.set("limit", String(q.limit));
    return req<CamEvent[]>(`/api/events?${p}`);
  },
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
  ptzCaps: (id: number) => req<{ supported: boolean }>(`/api/cameras/${id}/ptz`),
  ptz: (id: number, cmd: { action: "move" | "stop"; pan?: number; tilt?: number; zoom?: number }) =>
    req<{ ok: boolean }>(`/api/cameras/${id}/ptz`, { method: "POST", body: JSON.stringify(cmd) }),
  authStatus: () => req<{ enabled: boolean }>("/api/auth"),
  login: (password: string) =>
    req<{ ok: boolean }>("/api/login", { method: "POST", body: JSON.stringify({ password }) }),
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
export const fmtBytes = (b: number) =>
  b > 1e9 ? `${(b / 1e9).toFixed(2)} GB` : b > 1e6 ? `${(b / 1e6).toFixed(1)} MB` : `${Math.round(b / 1e3)} KB`;
