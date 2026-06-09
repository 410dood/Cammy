// Typed client for the zoomy core API.

export interface Zone {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface DetectConfig {
  labels: string[] | null;
  min_score: number | null;
  motion_threshold: number | null;
  ignore_zones: Zone[];
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
  model_path: string;
  force_cpu: boolean;
  go2rtc_api_port: number;
  webhook_url: string;
  record_audio: boolean;
  alert_labels: string[];
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
}

export interface AppConfig {
  go2rtc_base: string;
}

export interface CamStatus {
  online: boolean;
  recording: boolean;
  last_frame_ts: number | null;
  last_error: string | null;
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
    detect_source?: string;
    detect: boolean;
    record: boolean;
  }) => req<Camera>("/api/cameras", { method: "POST", body: JSON.stringify(c) }),
  patchCamera: (id: number, patch: Partial<Camera>) =>
    req<Camera>(`/api/cameras/${id}`, { method: "PATCH", body: JSON.stringify(patch) }),
  deleteCamera: (id: number) => req<void>(`/api/cameras/${id}`, { method: "DELETE" }),
  events: (q: { camera_id?: number; label?: string; limit?: number } = {}) => {
    const p = new URLSearchParams();
    if (q.camera_id != null) p.set("camera_id", String(q.camera_id));
    if (q.label) p.set("label", q.label);
    if (q.limit) p.set("limit", String(q.limit));
    return req<CamEvent[]>(`/api/events?${p}`);
  },
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
  stats: () => req<Stats>("/api/stats"),
  settings: () => req<Settings>("/api/settings"),
  saveSettings: (s: Settings) =>
    req<Settings>("/api/settings", { method: "PUT", body: JSON.stringify(s) }),
};

export const fmtTime = (ts: number) => new Date(ts * 1000).toLocaleString();
export const fmtBytes = (b: number) =>
  b > 1e9 ? `${(b / 1e9).toFixed(2)} GB` : b > 1e6 ? `${(b / 1e6).toFixed(1)} MB` : `${Math.round(b / 1e3)} KB`;
