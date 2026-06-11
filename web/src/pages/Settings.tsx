import { FormEvent, useEffect, useState } from "react";
import { api, Settings as S } from "../api";

function RemoteAccessCard({ onError }: { onError: (e: string) => void }) {
  const [enabled, setEnabled] = useState(false);
  const [pw, setPw] = useState("");
  const [msg, setMsg] = useState("");

  useEffect(() => {
    api.authStatus().then((a) => setEnabled(a.enabled)).catch(() => {});
  }, []);

  const apply = async (password: string) => {
    try {
      const r = await api.setPassword(password);
      setEnabled(r.enabled);
      setPw("");
      setMsg(r.enabled ? "Password set — other devices must now log in." : "Password cleared.");
    } catch (e) {
      onError(String(e));
    }
  };

  return (
    <div className="card">
      <h2>Remote access</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        When a password is set, other devices on your network must log in. This computer
        (localhost / the desktop app) is always exempt.
      </p>
      <div className="row">
        <span className={`pill ${enabled ? "on" : ""}`}>{enabled ? "protected" : "open"}</span>
        <input
          type="password"
          placeholder="new password (min 6 chars)"
          value={pw}
          onChange={(e) => setPw(e.target.value)}
        />
        <button type="button" className="primary" disabled={pw.trim().length < 6} onClick={() => apply(pw)}>
          Set password
        </button>
        {enabled && (
          <button type="button" className="danger" onClick={() => apply("")}>
            Clear
          </button>
        )}
        {msg && <span style={{ color: "var(--ok)" }}>{msg}</span>}
      </div>
    </div>
  );
}

export default function Settings({ onError }: { onError: (e: string) => void }) {
  const [s, setS] = useState<S | null>(null);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    api.settings().then(setS).catch((e) => onError(String(e)));
  }, [onError]);

  if (!s) return <p className="muted">Loading…</p>;

  const set = (patch: Partial<S>) => {
    setS({ ...s, ...patch });
    setSaved(false);
  };

  const save = async (e: FormEvent) => {
    e.preventDefault();
    try {
      setS(await api.saveSettings(s));
      setSaved(true);
    } catch (err) {
      onError(String(err));
    }
  };

  const num = (v: string, fallback: number) => {
    const n = Number(v);
    return Number.isFinite(n) ? n : fallback;
  };

  return (
    <>
      <h1>Settings</h1>
      <form onSubmit={save}>
        <div className="card">
          <h2>Detection</h2>
          <div className="row">
            <label className="field">
              objects (comma-separated, empty = all)
              <input
                type="text"
                style={{ minWidth: 380 }}
                value={s.detect_labels.join(", ")}
                onChange={(e) =>
                  set({
                    detect_labels: e.target.value
                      .split(",")
                      .map((x) => x.trim())
                      .filter(Boolean),
                  })
                }
              />
            </label>
            <label className="field">
              alert objects (shown in the Alerts review tab)
              <input
                type="text"
                value={(s.alert_labels ?? []).join(", ")}
                onChange={(e) =>
                  set({
                    alert_labels: e.target.value
                      .split(",")
                      .map((x) => x.trim())
                      .filter(Boolean),
                  })
                }
              />
            </label>
            <label className="field">
              min confidence (0-1)
              <input
                type="number" step="0.05" min="0" max="1"
                value={s.confidence}
                onChange={(e) => set({ confidence: num(e.target.value, s.confidence) })}
              />
            </label>
            <label className="field">
              motion threshold (0-1)
              <input
                type="number" step="0.005" min="0" max="1"
                value={s.motion_threshold}
                onChange={(e) => set({ motion_threshold: num(e.target.value, s.motion_threshold) })}
              />
            </label>
            <label className="field">
              sample interval (ms)
              <input
                type="number" step="100" min="100"
                value={s.poll_ms}
                onChange={(e) => set({ poll_ms: num(e.target.value, s.poll_ms) })}
              />
            </label>
            <label className="field">
              event cooldown (s)
              <input
                type="number" min="0"
                value={s.event_cooldown_secs}
                onChange={(e) => set({ event_cooldown_secs: num(e.target.value, s.event_cooldown_secs) })}
              />
            </label>
            <label className="toggle field">
              force CPU
              <input type="checkbox" checked={s.force_cpu} onChange={() => set({ force_cpu: !s.force_cpu })} />
            </label>
            <label className="toggle field">
              face recognition
              <input
                type="checkbox"
                checked={s.face_recognition}
                onChange={() => set({ face_recognition: !s.face_recognition })}
              />
            </label>
            <label className="field">
              face match threshold (0-1)
              <input
                type="number" step="0.05" min="0" max="1"
                value={s.face_match_threshold}
                onChange={(e) => set({ face_match_threshold: num(e.target.value, s.face_match_threshold) })}
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 280 }} title="Plates (or partials) of interest — a match fires a guaranteed high-priority push.">
              plate deny-list (vehicles of interest, comma-separated)
              <input
                type="text"
                placeholder="B8AU77, STOLEN1"
                value={(s.plate_denylist ?? []).join(", ")}
                onChange={(e) =>
                  set({ plate_denylist: e.target.value.split(",").map((x) => x.trim()).filter(Boolean) })
                }
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 280 }} title="Known/expected plates — surfaced as 'known' in review.">
              plate allow-list (known vehicles)
              <input
                type="text"
                placeholder="MYCAR1, SPOUSE2"
                value={(s.plate_allowlist ?? []).join(", ")}
                onChange={(e) =>
                  set({ plate_allowlist: e.target.value.split(",").map((x) => x.trim()).filter(Boolean) })
                }
              />
            </label>
          </div>
        </div>

        <div className="card">
          <h2>Hand signals ✋</h2>
          <p className="muted" style={{ marginTop: 0 }}>
            The Signals page tracks hand landmarks live in the browser. A held, armed signal logs
            an event and fires any Alarm with a matching <b>gesture</b> condition.
          </p>
          <div className="row">
            <label className="toggle field">
              enable hand signals
              <input
                type="checkbox"
                checked={s.gesture_recognition}
                onChange={() => set({ gesture_recognition: !s.gesture_recognition })}
              />
            </label>
            <label className="field">
              hold time before firing (s)
              <input
                type="number" step="0.1" min="0"
                value={s.gesture_hold_secs}
                onChange={(e) => set({ gesture_hold_secs: num(e.target.value, s.gesture_hold_secs) })}
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 300 }}>
              armed signals (comma-separated, empty = any)
              <input
                type="text"
                placeholder="open_palm, victory, thumb_up"
                value={(s.gesture_labels ?? []).join(", ")}
                onChange={(e) =>
                  set({
                    gesture_labels: e.target.value
                      .split(",")
                      .map((x) => x.trim())
                      .filter(Boolean),
                  })
                }
              />
            </label>
            <label className="field" title="A silent panic signal: when recognized it always fires at max push urgency (and pushes to the health ntfy topic), even if not in the armed list.">
              duress / help signal
              <select value={s.gesture_duress ?? ""} onChange={(e) => set({ gesture_duress: e.target.value })}>
                <option value="">none</option>
                {["open_palm", "fist", "victory", "point", "thumb_up", "thumb_down", "love", "ok", "call_me"].map(
                  (g) => (
                    <option key={g} value={g}>
                      {g}
                    </option>
                  )
                )}
              </select>
            </label>
            <label className="field" style={{ flex: 1, minWidth: 320 }}>
              model URL (MediaPipe .task; default = Google CDN, override to self-host offline)
              <input
                type="text"
                value={s.gesture_model_url ?? ""}
                onChange={(e) => set({ gesture_model_url: e.target.value })}
              />
            </label>
          </div>
        </div>

        <div className="card">
          <h2>AI event captions (opt-in)</h2>
          <p className="muted" style={{ marginTop: 0 }}>
            Generate a short natural-language description of each event for review and search.
            <b> Off by default.</b> With the default localhost Ollama URL nothing leaves this
            machine; pointing it at a cloud endpoint sends snapshots there — that's a deliberate
            choice you make here.
          </p>
          <div className="row">
            <label className="toggle field">
              enable captions
              <input
                type="checkbox"
                checked={s.genai_enabled}
                onChange={() => set({ genai_enabled: !s.genai_enabled })}
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 320 }}>
              endpoint (Ollama-compatible /api/generate)
              <input
                type="text"
                placeholder="http://localhost:11434/api/generate"
                value={s.genai_url ?? ""}
                onChange={(e) => set({ genai_url: e.target.value })}
              />
            </label>
            <label className="field">
              vision model
              <input
                type="text"
                placeholder="llava"
                value={s.genai_model ?? ""}
                onChange={(e) => set({ genai_model: e.target.value })}
              />
            </label>
            <label className="field" style={{ minWidth: 220 }}>
              API key (cloud only; blank for local)
              <input
                type="password"
                value={s.genai_api_key ?? ""}
                onChange={(e) => set({ genai_api_key: e.target.value })}
              />
            </label>
          </div>
        </div>

        <RemoteAccessCard onError={onError} />

        <div className="card">
          <h2>Notifications</h2>
          <div className="row">
            <label className="field" style={{ flex: 1, minWidth: 320 }}>
              webhook URL (POST per event; empty = off)
              <input
                type="text"
                placeholder="http://homeassistant.local:8123/api/webhook/zoomy"
                value={s.webhook_url}
                onChange={(e) => set({ webhook_url: e.target.value })}
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 320 }}>
              camera health push — ntfy topic URL (offline/online alerts; empty = off)
              <input
                type="text"
                placeholder="https://ntfy.sh/your-secret-topic"
                value={s.health_ntfy_url ?? ""}
                onChange={(e) => set({ health_ntfy_url: e.target.value })}
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 320 }}>
              public base URL (adds tap-through clip/snapshot links to pushes)
              <input
                type="text"
                placeholder="https://nvr.example.com"
                value={s.public_base_url ?? ""}
                onChange={(e) => set({ public_base_url: e.target.value })}
              />
            </label>
            <label className="field" style={{ minWidth: 240 }}>
              MQTT broker (empty = off)
              <input
                type="text"
                placeholder="mqtt://homeassistant.local:1883"
                value={s.mqtt_url}
                onChange={(e) => set({ mqtt_url: e.target.value })}
              />
            </label>
            <label className="field">
              MQTT topic prefix
              <input
                type="text"
                value={s.mqtt_prefix}
                onChange={(e) => set({ mqtt_prefix: e.target.value })}
              />
            </label>
            <label className="toggle field" title="Publish MQTT-discovery configs so Home Assistant auto-creates a binary_sensor per (camera, object) and a last-detection sensor per camera.">
              Home Assistant discovery
              <input
                type="checkbox"
                checked={s.mqtt_ha_discovery}
                onChange={() => set({ mqtt_ha_discovery: !s.mqtt_ha_discovery })}
              />
            </label>
            <label className="field">
              HA discovery prefix
              <input
                type="text"
                value={s.mqtt_ha_prefix}
                onChange={(e) => set({ mqtt_ha_prefix: e.target.value })}
              />
            </label>
            <label className="field" title="Seconds a Home Assistant binary_sensor stays ON after a detection before auto-clearing.">
              sensor ON timeout (s)
              <input
                type="number" min="1"
                value={s.mqtt_state_timeout_secs}
                onChange={(e) => set({ mqtt_state_timeout_secs: num(e.target.value, s.mqtt_state_timeout_secs) })}
              />
            </label>
          </div>
          <div className="row" style={{ marginTop: 10 }}>
            <label className="field" style={{ flex: 1, minWidth: 420 }}>
              webhook body template (empty = default JSON; placeholders like{" "}
              <code>{"{{camera}}"}</code> <code>{"{{label}}"}</code> <code>{"{{score}}"}</code>{" "}
              <code>{"{{snapshot}}"}</code> — see docs/03)
              <textarea
                rows={2}
                placeholder='{"text":"{{label}} on {{camera}} ({{score}})"}'
                value={s.webhook_template ?? ""}
                onChange={(e) => set({ webhook_template: e.target.value })}
                style={{ width: "100%", fontFamily: "monospace" }}
              />
            </label>
          </div>
        </div>

        <div className="card">
          <h2>Recording &amp; retention</h2>
          <div className="row">
            <label className="field">
              segment length (s)
              <input
                type="number" min="10"
                value={s.segment_seconds}
                onChange={(e) => set({ segment_seconds: num(e.target.value, s.segment_seconds) })}
              />
            </label>
            <label className="field">
              keep at most (days)
              <input
                type="number" min="1"
                value={s.retention_days}
                onChange={(e) => set({ retention_days: num(e.target.value, s.retention_days) })}
              />
            </label>
            <label className="field">
              keep at most (GB)
              <input
                type="number" min="1"
                value={s.retention_gb}
                onChange={(e) => set({ retention_gb: num(e.target.value, s.retention_gb) })}
              />
            </label>
            <label className="field">
              reduce quality after (days, 0 = off)
              <input
                type="number" min="0"
                value={s.enhanced_retention_days}
                onChange={(e) =>
                  set({ enhanced_retention_days: num(e.target.value, s.enhanced_retention_days) })
                }
              />
            </label>
            <label className="field" title="Hardware video encoder for the enhanced-retention re-encode. Falls back to CPU automatically if unavailable.">
              re-encode with
              <select value={s.hwaccel ?? ""} onChange={(e) => set({ hwaccel: e.target.value })}>
                <option value="">CPU (libx264)</option>
                <option value="nvenc">NVIDIA NVENC</option>
                <option value="qsv">Intel QuickSync</option>
                <option value="videotoolbox">Apple VideoToolbox</option>
              </select>
            </label>
            <label className="field">
              keep events (days)
              <input
                type="number" min="1"
                value={s.event_retention_days}
                onChange={(e) =>
                  set({ event_retention_days: num(e.target.value, s.event_retention_days) })
                }
              />
            </label>
            <label className="field" style={{ minWidth: 300 }}>
              recordings folder (empty = data/recordings; another drive or NAS share works)
              <input
                type="text"
                placeholder="D:\zoomy-recordings or \\nas\cams"
                value={s.recordings_dir ?? ""}
                onChange={(e) => set({ recordings_dir: e.target.value })}
              />
            </label>
            <label className="field">
              model path
              <input
                type="text"
                value={s.model_path}
                onChange={(e) => set({ model_path: e.target.value })}
              />
            </label>
            <label className="toggle field">
              record audio (AAC)
              <input
                type="checkbox"
                checked={s.record_audio}
                onChange={() => set({ record_audio: !s.record_audio })}
              />
            </label>
          </div>
        </div>

        <div className="row">
          <button className="primary">Save</button>
          {saved && <span style={{ color: "var(--ok)" }}>Saved ✓</span>}
          <span className="muted">Changes apply within a few seconds — no restart needed.</span>
        </div>
      </form>
    </>
  );
}
