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
