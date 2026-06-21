import { FormEvent, useEffect, useState } from "react";
import { api, AlarmRule, Action, ActionKind, ArmMode, Camera } from "../api";
import { IconStranger, IconMoon, IconPlus, IconX } from "../icons";

const LABELS = [
  "person", "car", "truck", "bus", "bicycle", "motorcycle", "dog", "cat",
  // Tracker-driven analytics events fire alarms via the same label match.
  "crossing", "wrong_way", "loiter", "occupancy",
];
const GESTURES = ["open_palm", "fist", "victory", "point", "thumb_up", "thumb_down", "love", "ok", "call_me"];
const ARM_OPTS: { id: ArmMode; label: string }[] = [
  { id: "home", label: "Home" },
  { id: "away", label: "Away" },
  { id: "disarmed", label: "Disarmed" },
];
const ACTION_KINDS: { id: ActionKind; label: string }[] = [
  { id: "webhook", label: "POST webhook" },
  { id: "mqtt", label: "publish MQTT" },
  { id: "ntfy", label: "push via ntfy" },
  { id: "email", label: "send email" },
];
const targetHint = (kind: ActionKind) =>
  kind === "webhook"
    ? "https://… (receives the event JSON)"
    : kind === "mqtt"
      ? "topic suffix → zoomy/alarms/<suffix>"
      : kind === "email"
        ? "recipient@example.com (blank = default from Settings)"
        : "https://ntfy.sh/your-secret-topic (push, snapshot attached)";

export default function Alarms({
  cameras,
  onError,
}: {
  cameras: Camera[];
  onError: (e: string) => void;
}) {
  const [rules, setRules] = useState<AlarmRule[]>([]);
  const [name, setName] = useState("");
  const [cameraId, setCameraId] = useState<number | "">("");
  const [label, setLabel] = useState("");
  const [faceLike, setFaceLike] = useState("");
  const [plateLike, setPlateLike] = useState("");
  const [gestureLike, setGestureLike] = useState("");
  const [transcriptLike, setTranscriptLike] = useState("");
  const [faceUnknown, setFaceUnknown] = useState(false);
  const [actions, setActions] = useState<Action[]>([{ kind: "webhook", target: "", priority: 0 }]);
  const [modes, setModes] = useState<ArmMode[]>([]);
  const [cooldown, setCooldown] = useState(0);
  const [days, setDays] = useState<number[]>([]);
  const [startTime, setStartTime] = useState("");
  const [endTime, setEndTime] = useState("");

  const DAY_NAMES = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  const toggleDay = (d: number) =>
    setDays((prev) => (prev.includes(d) ? prev.filter((x) => x !== d) : [...prev, d].sort()));
  const toggleMode = (m: ArmMode) =>
    setModes((p) => (p.includes(m) ? p.filter((x) => x !== m) : [...p, m]));
  const updateAction = (i: number, patch: Partial<Action>) =>
    setActions((p) => p.map((a, j) => (j === i ? { ...a, ...patch } : a)));
  const addAction = () => setActions((p) => [...p, { kind: "ntfy", target: "", priority: 0 }]);
  const removeAction = (i: number) =>
    setActions((p) => (p.length > 1 ? p.filter((_, j) => j !== i) : p));

  const load = () => {
    api.alarms().then(setRules).catch(() => {});
  };
  useEffect(load, []);

  const add = async (e: FormEvent) => {
    e.preventDefault();
    const acts = actions.map((a) => ({ ...a, target: a.target.trim() }));
    if (acts.some((a) => a.kind !== "email" && !a.target)) {
      onError("every action needs a target (URL or MQTT topic)");
      return;
    }
    try {
      await api.addAlarm({
        name: name.trim(),
        enabled: true,
        camera_id: cameraId === "" ? null : cameraId,
        label: label || null,
        // face_unknown and face_like are mutually exclusive (both set = a rule
        // that can never fire), so don't submit a stale face_like alongside it.
        face_like: faceUnknown ? null : faceLike.trim() || null,
        plate_like: plateLike.trim() || null,
        gesture_like: gestureLike || null,
        transcript_like: transcriptLike.trim() || null,
        face_unknown: faceUnknown,
        min_score: 0,
        // Legacy single-action mirror (kept in sync with actions[0] server-side too).
        action: acts[0].kind,
        target: acts[0].target,
        days,
        start_hhmm: startTime || null,
        end_hhmm: endTime || null,
        cooldown_secs: cooldown,
        priority: acts[0].priority,
        snooze_until: 0,
        modes,
        actions: acts,
      });
      setName("");
      setActions([{ kind: "webhook", target: "", priority: 0 }]);
      setModes([]);
      setFaceLike("");
      setPlateLike("");
      setGestureLike("");
      setTranscriptLike("");
      setFaceUnknown(false);
      setCooldown(0);
      setDays([]);
      setStartTime("");
      setEndTime("");
      load();
    } catch (err) {
      onError(String(err));
    }
  };

  const describe = (r: AlarmRule) => {
    const sched =
      (r.days ?? []).length > 0 || r.start_hhmm || r.end_hhmm
        ? [
            (r.days ?? []).length > 0 ? (r.days ?? []).map((d) => DAY_NAMES[d]).join(",") : null,
            r.start_hhmm || r.end_hhmm
              ? `${r.start_hhmm ?? "00:00"}–${r.end_hhmm ?? "24:00"}`
              : null,
          ]
            .filter(Boolean)
            .join(" ")
        : null;
    const conds = [
      r.camera_id != null
        ? `camera ${cameras.find((c) => c.id === r.camera_id)?.name ?? r.camera_id}`
        : "any camera",
      r.label ?? "any object",
      r.face_like ? `face ~ "${r.face_like}"` : null,
      r.face_unknown ? `unknown face (stranger)` : null,
      r.plate_like ? `plate ~ "${r.plate_like}"` : null,
      r.gesture_like ? `signal ${r.gesture_like}` : null,
      r.transcript_like ? `said "${r.transcript_like}"` : null,
      sched ? `armed ${sched}` : null,
      (r.modes ?? []).length > 0 ? `modes ${(r.modes ?? []).join("/")}` : null,
      r.cooldown_secs > 0 ? `cooldown ${r.cooldown_secs}s` : null,
    ].filter(Boolean);
    return conds.join(" · ");
  };

  const actionText = (a: Action) =>
    a.kind === "webhook"
      ? `POST ${a.target}`
      : a.kind === "mqtt"
        ? `MQTT zoomy/alarms/${a.target}`
        : a.kind === "email"
          ? `email → ${a.target || "default recipient"}`
          : `ntfy → ${a.target}${a.priority ? ` (p${a.priority})` : ""}`;

  const ruleActions = (r: AlarmRule): Action[] =>
    r.actions && r.actions.length > 0
      ? r.actions
      : [{ kind: r.action as ActionKind, target: r.target, priority: r.priority }];

  const snoozeText = (r: AlarmRule) => {
    const left = r.snooze_until - Math.floor(Date.now() / 1000);
    if (left <= 0) return null;
    return left > 3600 ? `${Math.round(left / 3600)}h` : `${Math.round(left / 60)}m`;
  };

  return (
    <>
      <h1>Alarm Manager</h1>

      <div className="card">
        <h2>New rule — when this happens…</h2>
        <form onSubmit={add}>
          <div className="row" style={{ marginBottom: 12 }}>
            <label className="field">
              rule name
              <input type="text" value={name} onChange={(e) => setName(e.target.value)} required placeholder="person at the front door" />
            </label>
            <label className="field">
              camera
              <select value={cameraId} onChange={(e) => setCameraId(e.target.value === "" ? "" : Number(e.target.value))}>
                <option value="">any</option>
                {cameras.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </select>
            </label>
            <label className="field">
              object
              <select value={label} onChange={(e) => setLabel(e.target.value)}>
                <option value="">any</option>
                {LABELS.map((l) => (
                  <option key={l} value={l}>
                    {l}
                  </option>
                ))}
              </select>
            </label>
            <label className="field">
              face contains (optional)
              <input
                type="text"
                value={faceLike}
                onChange={(e) => setFaceLike(e.target.value)}
                placeholder="any face name"
                disabled={faceUnknown}
              />
            </label>
            <label
              className="field"
              title="Fire when a person's face is detected but matches nobody you've enrolled — a stranger / unfamiliar-face alert. Needs face recognition on the camera, and at least one enrolled identity (enroll known faces on the Faces page) so only true unknowns alert."
            >
              <span>
                <input
                  type="checkbox"
                  checked={faceUnknown}
                  onChange={(e) => setFaceUnknown(e.target.checked)}
                />{" "}
                <IconStranger size={14} /> unknown face (stranger)
              </span>
              {faceUnknown && (
                <small className="muted">
                  Enroll known faces (Faces page) first — strangers are detected only
                  relative to enrolled identities.
                </small>
              )}
            </label>
            <label className="field">
              plate contains (optional)
              <input type="text" value={plateLike} onChange={(e) => setPlateLike(e.target.value)} placeholder="any plate" />
            </label>
            <label className="field">
              hand signal (optional)
              <select value={gestureLike} onChange={(e) => setGestureLike(e.target.value)}>
                <option value="">any / none</option>
                {GESTURES.map((g) => (
                  <option key={g} value={g}>
                    {g}
                  </option>
                ))}
              </select>
            </label>
            <label className="field" title="Fire when this phrase is spoken near the camera (needs audio transcription enabled). A spoken safe word, e.g. 'help'.">
              spoken phrase (optional)
              <input
                type="text"
                value={transcriptLike}
                onChange={(e) => setTranscriptLike(e.target.value)}
                placeholder='e.g. "help"'
              />
            </label>
          </div>
          <div className="row" style={{ marginBottom: 12 }}>
            <span className="muted">…armed (optional):</span>
            {DAY_NAMES.map((d, i) => (
              <span
                key={d}
                className={`pill toggle ${days.includes(i) ? "on" : ""}`}
                onClick={() => toggleDay(i)}
              >
                {d}
              </span>
            ))}
            <label className="field">
              from
              <input type="time" value={startTime} onChange={(e) => setStartTime(e.target.value)} />
            </label>
            <label className="field">
              to
              <input type="time" value={endTime} onChange={(e) => setEndTime(e.target.value)} />
            </label>
            <span className="muted">no days/times = always armed; to &lt; from spans midnight</span>
          </div>
          <div className="row" style={{ marginBottom: 12 }}>
            <span className="muted">…in modes (optional):</span>
            {ARM_OPTS.map((m) => (
              <span
                key={m.id}
                className={`pill toggle ${modes.includes(m.id) ? "on" : ""}`}
                onClick={() => toggleMode(m.id)}
                title={
                  m.id === "disarmed"
                    ? "Include Disarmed to make this a panic rule that fires even when the system is disarmed"
                    : `Active in ${m.label} mode`
                }
              >
                {m.label}
              </span>
            ))}
            <span className="muted">none = active in Home + Away (paused when Disarmed)</span>
          </div>
          <div className="actions-edit">
            <span className="muted">…do this (one or more actions):</span>
            {actions.map((a, i) => (
              <div className="row action-row" key={i}>
                <select
                  value={a.kind}
                  onChange={(e) => updateAction(i, { kind: e.target.value as ActionKind })}
                >
                  {ACTION_KINDS.map((k) => (
                    <option key={k.id} value={k.id}>
                      {k.label}
                    </option>
                  ))}
                </select>
                <input
                  type="text"
                  style={{ flex: 1, minWidth: 240 }}
                  value={a.target}
                  onChange={(e) => updateAction(i, { target: e.target.value })}
                  placeholder={targetHint(a.kind)}
                />
                {a.kind === "ntfy" && (
                  <select
                    value={a.priority}
                    onChange={(e) => updateAction(i, { priority: Number(e.target.value) })}
                    title="ntfy push priority"
                  >
                    <option value={0}>default</option>
                    <option value={1}>1 · min</option>
                    <option value={2}>2 · low</option>
                    <option value={3}>3 · normal</option>
                    <option value={4}>4 · high</option>
                    <option value={5}>5 · urgent</option>
                  </select>
                )}
                <button
                  type="button"
                  className="ghost"
                  title="Remove this action"
                  disabled={actions.length === 1}
                  onClick={() => removeAction(i)}
                >
                  <IconX size={14} />
                </button>
              </div>
            ))}
            <button type="button" className="ghost" onClick={addAction} style={{ marginTop: 2 }}>
              <IconPlus size={14} /> Add action
            </button>
          </div>
          <div className="row" style={{ marginTop: 12 }}>
            <span className="muted">…anti-fatigue:</span>
            <label className="field">
              cooldown (s)
              <input
                type="number" min="0" style={{ width: 90 }}
                value={cooldown}
                onChange={(e) => setCooldown(Math.max(0, Number(e.target.value) || 0))}
                title="Minimum seconds between firings of this rule (debounces the whole scene)."
              />
            </label>
            <div className="spacer" />
            <button className="primary">Create rule</button>
          </div>
        </form>
      </div>

      <div className="card">
        <h2>Rules</h2>
        {rules.length === 0 ? (
          <p className="muted">No rules yet. Rules fire actions the moment a matching event is detected.</p>
        ) : (
          <div className="table-scroll">
          <table>
            <thead>
              <tr>
                <th>Rule</th>
                <th>When</th>
                <th>Then</th>
                <th>Active</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {rules.map((r) => (
                <tr key={r.id}>
                  <td>
                    <b>{r.name}</b>
                  </td>
                  <td className="muted">{describe(r)}</td>
                  <td className="muted">
                    {ruleActions(r).map((a, i) => (
                      <div key={i}>{actionText(a)}</div>
                    ))}
                  </td>
                  <td>
                    <span
                      className={`pill toggle ${r.enabled ? "on" : ""}`}
                      onClick={async () => {
                        await api
                          .patchAlarm(r.id, { enabled: !r.enabled })
                          .catch((e) => onError(String(e)));
                        load();
                      }}
                    >
                      {r.enabled ? "on" : "off"}
                    </span>
                    {snoozeText(r) && (
                      <span className="pill" style={{ marginLeft: 6 }} title="snoozed">
                        <IconMoon size={12} /> {snoozeText(r)}
                      </span>
                    )}
                  </td>
                  <td>
                    {snoozeText(r) ? (
                      <button
                        className="ghost"
                        onClick={async () => {
                          await api
                            .patchAlarm(r.id, { snooze_secs: 0 })
                            .catch((e) => onError(String(e)));
                          load();
                        }}
                      >
                        wake
                      </button>
                    ) : (
                      <button
                        className="ghost"
                        title="Suppress this rule for 1 hour"
                        onClick={async () => {
                          await api
                            .patchAlarm(r.id, { snooze_secs: 3600 })
                            .catch((e) => onError(String(e)));
                          load();
                        }}
                      >
                        snooze 1h
                      </button>
                    )}
                    <button
                      className="danger"
                      style={{ marginLeft: 8 }}
                      onClick={async () => {
                        await api.deleteAlarm(r.id).catch((e) => onError(String(e)));
                        load();
                      }}
                    >
                      Delete
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          </div>
        )}
      </div>
    </>
  );
}
