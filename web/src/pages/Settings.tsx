import { ChangeEvent, FormEvent, useEffect, useState } from "react";
import { api, ApiToken, AuditEntry, fmtTime, Settings as S } from "../api";

const AUDIT_LABELS: Record<string, string> = {
  login_success: "✅ login",
  login_failed: "⛔ failed login",
  password_set: "🔑 password set",
  password_cleared: "🔓 password cleared",
  token_created: "🎫 token created",
  token_revoked: "🗑️ token revoked",
};

function AuditCard() {
  const [rows, setRows] = useState<AuditEntry[]>([]);
  useEffect(() => {
    api.audit(100).then(setRows).catch(() => {});
  }, []);
  if (rows.length === 0) return null;
  return (
    <div className="card">
      <h2>Recent security activity</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Logins, password changes, and API-token changes — most recent first. Useful for
        spotting unexpected access on a WAN-exposed server.
      </p>
      <table style={{ width: "100%", borderCollapse: "collapse" }}>
        <tbody>
          {rows.map((r) => (
            <tr key={r.id}>
              <td>{AUDIT_LABELS[r.action] ?? r.action}</td>
              <td className="muted">{fmtTime(r.ts)}</td>
              <td className="muted">{r.ip ?? ""}</td>
              <td className="muted">{r.detail ?? ""}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

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

function TokensCard({ onError }: { onError: (e: string) => void }) {
  const [tokens, setTokens] = useState<ApiToken[]>([]);
  const [name, setName] = useState("");
  const [fresh, setFresh] = useState<{ name: string; token: string } | null>(null);

  const load = () => {
    api.tokens().then(setTokens).catch(() => {});
  };
  useEffect(load, []);

  const create = async () => {
    const n = name.trim();
    if (!n) return;
    try {
      const r = await api.createToken(n);
      setFresh({ name: r.name, token: r.token });
      setName("");
      load();
    } catch (e) {
      onError(String(e));
    }
  };
  const remove = async (id: number) => {
    if (!window.confirm("Revoke this token? Anything using it will lose access.")) return;
    try {
      await api.deleteToken(id);
      load();
    } catch (e) {
      onError(String(e));
    }
  };

  return (
    <div className="card">
      <h2>API tokens</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Bearer tokens let scripts and integrations (Home Assistant, MQTT automations) call the
        API from another machine without logging in — send{" "}
        <code>Authorization: Bearer &lt;token&gt;</code>. A token can do almost anything the API
        can, so keep it secret and revoke any that leak. (Tokens cannot change the password or
        create/revoke other tokens — those need an interactive login here, so a leaked token
        can't lock you out.)
      </p>
      <div className="row">
        <input
          type="text"
          placeholder="token name (e.g. home-assistant)"
          value={name}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && create()}
        />
        <button type="button" className="primary" disabled={!name.trim()} onClick={create}>
          Create token
        </button>
      </div>
      {fresh && (
        <div
          className="row"
          style={{ marginTop: 8, flexDirection: "column", alignItems: "flex-start", gap: 4 }}
        >
          <span style={{ color: "var(--ok)" }}>
            New token “{fresh.name}” — copy it now, it won’t be shown again:
          </span>
          <code style={{ userSelect: "all", wordBreak: "break-all" }}>{fresh.token}</code>
        </div>
      )}
      {tokens.length > 0 && (
        <table style={{ marginTop: 12, width: "100%", borderCollapse: "collapse" }}>
          <tbody>
            {tokens.map((t) => (
              <tr key={t.id}>
                <td>
                  <b>{t.name}</b>
                </td>
                <td className="muted">created {fmtTime(t.created_ts)}</td>
                <td className="muted">
                  {t.last_used_ts ? `last used ${fmtTime(t.last_used_ts)}` : "never used"}
                </td>
                <td style={{ textAlign: "right" }}>
                  <button type="button" className="danger" onClick={() => remove(t.id)}>
                    Revoke
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

function BackupCard({ onError }: { onError: (e: string) => void }) {
  const [msg, setMsg] = useState("");
  const [busy, setBusy] = useState(false);

  const onFile = async (e: ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = ""; // let the user re-pick the same file later
    if (!file) return;
    if (
      !window.confirm(
        "Restore configuration from this file? Settings are replaced; cameras and alarms whose names already exist are kept as-is.",
      )
    )
      return;
    setBusy(true);
    setMsg("");
    try {
      const backup = JSON.parse(await file.text());
      const r = await api.restore(backup);
      setMsg(
        `Restored — ${r.cameras_added} camera(s) added, ${r.cameras_skipped} skipped, ${r.alarms_added} alarm(s) added, settings applied. Reload to see changes.`,
      );
    } catch (err) {
      onError(`restore failed: ${err}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="card">
      <h2>Backup &amp; restore</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Export your configuration — cameras, settings and alarm rules — to a JSON file to move to
        another machine. Recordings, events and enrolled faces are <b>not</b> included. The file can
        contain camera credentials, so keep it private. Restore is additive: settings are replaced,
        but a camera/alarm whose name already exists is left untouched.
      </p>
      <div className="row" style={{ alignItems: "center" }}>
        <a
          className="pill"
          href="/api/backup"
          download="zoomy-backup.json"
          style={{ textDecoration: "none" }}
        >
          ⬇ Download backup
        </a>
        <label className="pill" style={{ cursor: busy ? "wait" : "pointer" }}>
          ⬆ Restore from file…
          <input
            type="file"
            accept="application/json,.json"
            style={{ display: "none" }}
            disabled={busy}
            onChange={onFile}
          />
        </label>
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

        <div className="card">
          <h2>Audio transcription 🎙️</h2>
          <p className="muted" style={{ marginTop: 0 }}>
            Speech-to-text for audio events, using a <b>bundled, in-process</b> whisper.cpp engine —
            audio never leaves this machine and there's no separate software to run.{" "}
            <b>Off by default.</b> When on, this applies to <b>every camera with audio detection
            enabled</b>: a sound event captures a short clip and the transcript is written onto the
            event (shown on cards). Needs the whisper model file present.
          </p>
          <div className="row">
            <label className="toggle field">
              enable transcription
              <input
                type="checkbox"
                checked={s.transcription_enabled}
                onChange={() => set({ transcription_enabled: !s.transcription_enabled })}
              />
            </label>
            <label className="field" style={{ flex: 1, minWidth: 320 }} title="Path to a whisper GGML model (downloaded separately), e.g. ggml-tiny.en.bin or ggml-base.en.bin.">
              whisper model file
              <input
                type="text"
                placeholder="ggml-tiny.en.bin"
                value={s.transcription_model ?? ""}
                onChange={(e) => set({ transcription_model: e.target.value })}
              />
            </label>
          </div>
        </div>

        <RemoteAccessCard onError={onError} />

        <TokensCard onError={onError} />

        <AuditCard />

        <BackupCard onError={onError} />

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
