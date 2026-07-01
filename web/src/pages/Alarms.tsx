import { FormEvent, useEffect, useState } from "react";
import { api, AlarmRule, Action, ActionKind, ArmMode, Camera } from "../api";
import { IconStranger, IconMoon, IconPlus, IconX, IconSiren } from "../icons";
import { EmptyState, ErrorState, TogglePill, useDialog, useToast } from "../ui";

const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e));

const LABELS = [
  "person", "car", "truck", "bus", "bicycle", "motorcycle", "dog", "cat",
  // Wildlife / nuisance-animal (COCO classes the detector knows — add them to
  // detect labels in Settings to enable). Raccoon/deer aren't COCO; use smart search.
  "bird", "bear", "horse", "sheep", "cow",
  // Tracker-driven analytics events fire alarms via the same label match.
  "crossing", "wrong_way", "loiter", "occupancy",
  // Residential analytics events (see ZoneEditor + per-camera detect config).
  "child", "child_alone", "fall", "still_water",
  // Server-side pose events (enable "body pose monitoring" on the camera).
  "standing", "covered_face",
];
// Friendly names for the non-obvious analytics / residential event labels, shown
// in the dropdowns. The raw token is kept in parentheses (and as the option
// `value`) so it still matches what the Family page and docs refer to.
const LABEL_PRETTY: Record<string, string> = {
  crossing: "Line crossing",
  wrong_way: "Wrong way",
  loiter: "Loitering",
  occupancy: "Occupancy limit",
  child: "Child*",
  child_alone: "Child alone*",
  fall: "Fall*",
  still_water: "Motionless in water*",
  standing: "Standing — crib climb-out*",
  covered_face: "Covered face*",
};
const labelText = (l: string) => (LABEL_PRETTY[l] ? `${LABEL_PRETTY[l]} (${l})` : l);
const GESTURES = ["open_palm", "fist", "victory", "point", "thumb_up", "thumb_down", "love", "ok", "call_me"];
const ARM_OPTS: { id: ArmMode; label: string }[] = [
  { id: "home", label: "Home" },
  { id: "away", label: "Away" },
  { id: "disarmed", label: "Disarmed" },
];
const ACTION_KINDS: { id: ActionKind; label: string }[] = [
  { id: "webhook", label: "POST webhook" },
  { id: "mqtt", label: "publish MQTT" },
  { id: "ntfy", label: "push to phone (ntfy app)" },
  { id: "email", label: "send email" },
];
const targetHint = (kind: ActionKind) =>
  kind === "webhook"
    ? "https://… (receives the event JSON)"
    : kind === "mqtt"
      ? "topic suffix → zoomy/alarms/<suffix>"
      : kind === "email"
        ? "recipient@example.com (blank = default from Settings)"
        : "https://ntfy.sh/your-private-topic — free phone push, snapshot attached";

export default function Alarms({
  cameras,
  onError,
}: {
  cameras: Camera[];
  onError: (e: string) => void;
}) {
  const dialog = useDialog();
  const toast = useToast();
  const [rules, setRules] = useState<AlarmRule[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [creating, setCreating] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [name, setName] = useState("");
  const [cameraId, setCameraId] = useState<number | "">("");
  const [label, setLabel] = useState("");
  const [faceLike, setFaceLike] = useState("");
  const [plateLike, setPlateLike] = useState("");
  const [gestureLike, setGestureLike] = useState("");
  const [transcriptLike, setTranscriptLike] = useState("");
  const [zoneLike, setZoneLike] = useState("");
  const [confirmLabel, setConfirmLabel] = useState("");
  const [confirmWithin, setConfirmWithin] = useState(10);
  const [vlmPrompt, setVlmPrompt] = useState("");
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
    api
      .alarms()
      .then((r) => {
        setRules(r);
        setLoadError(null);
      })
      .catch((e) => setLoadError(errMsg(e)))
      .finally(() => setLoaded(true));
  };
  useEffect(load, []);
  // First-run: open the builder when there are no rules yet (gated on `loaded`
  // so it doesn't flash open before the list resolves). Once rules exist the
  // list leads and the builder stays collapsed behind the "New rule" button.
  useEffect(() => {
    if (loaded && rules.length === 0) setCreating(true);
  }, [loaded, rules.length]);

  const removeRule = async (r: AlarmRule) => {
    if (!(await dialog.confirm({ title: `Delete rule “${r.name}”?`, confirmLabel: "Delete", danger: true }))) return;
    try {
      await api.deleteAlarm(r.id);
      toast.success("Rule deleted");
      load();
    } catch (e) {
      onError(String(e));
    }
  };

  const add = async (e: FormEvent) => {
    e.preventDefault();
    const acts = actions.map((a) => ({ ...a, target: a.target.trim() }));
    if (acts.some((a) => a.kind !== "email" && !a.target)) {
      onError("every action needs a target (URL or MQTT topic)");
      return;
    }
    const badEmail = acts.find((a) => a.kind === "email" && a.target && !/.+@.+/.test(a.target));
    if (badEmail) {
      onError("the email action needs a valid recipient address (or leave it blank to use the Settings default)");
      return;
    }
    const badUrl = acts.find(
      (a) => (a.kind === "webhook" || a.kind === "ntfy") && a.target && !/^https?:\/\//i.test(a.target),
    );
    if (badUrl) {
      onError(`the ${badUrl.kind} action's target must start with http:// or https://`);
      return;
    }
    setBusy(true);
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
        zone_like: zoneLike.trim() || null,
        confirm_label: confirmLabel.trim() || null,
        confirm_within_secs: confirmLabel.trim() ? confirmWithin : null,
        vlm_prompt: vlmPrompt.trim() || null,
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
      toast.success(`Rule “${name.trim()}” created`);
      setCreating(false);
      setName("");
      setActions([{ kind: "webhook", target: "", priority: 0 }]);
      setModes([]);
      setFaceLike("");
      setPlateLike("");
      setGestureLike("");
      setTranscriptLike("");
      setZoneLike("");
      setConfirmLabel("");
      setConfirmWithin(10);
      setVlmPrompt("");
      setFaceUnknown(false);
      setCooldown(0);
      setDays([]);
      setStartTime("");
      setEndTime("");
      load();
    } catch (err) {
      onError(String(err));
    } finally {
      setBusy(false);
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
      r.zone_like ? `in zone ~ "${r.zone_like}"` : null,
      r.confirm_label ? `confirmed by ${r.confirm_label} ≤${r.confirm_within_secs ?? 0}s` : null,
      r.vlm_prompt ? `AI-verified: "${r.vlm_prompt}"` : null,
      sched ? `armed ${sched}` : null,
      (r.modes ?? []).length > 0 ? `modes ${(r.modes ?? []).join("/")}` : null,
      r.cooldown_secs > 0 ? `cooldown ${r.cooldown_secs}s` : null,
    ].filter(Boolean);
    return conds.join(" · ");
  };

  // Structured split of a rule's conditions into what FIRES it (trigger) vs the
  // qualifiers that SCOPE it (camera/zone/schedule/modes), so the "When" column
  // is scannable instead of one long dot-joined gray string.
  const describeParts = (r: AlarmRule): { trigger: string[]; scope: string[] } => {
    const sched =
      (r.days ?? []).length > 0 || r.start_hhmm || r.end_hhmm
        ? [
            (r.days ?? []).length > 0 ? (r.days ?? []).map((d) => DAY_NAMES[d]).join(",") : null,
            r.start_hhmm || r.end_hhmm ? `${r.start_hhmm ?? "00:00"}–${r.end_hhmm ?? "24:00"}` : null,
          ]
            .filter(Boolean)
            .join(" ")
        : null;
    const trigger = [
      r.label ?? null,
      r.face_like ? `face ~ "${r.face_like}"` : null,
      r.face_unknown ? "stranger" : null,
      r.plate_like ? `plate ~ "${r.plate_like}"` : null,
      r.gesture_like ? `signal ${r.gesture_like}` : null,
      r.transcript_like ? `said "${r.transcript_like}"` : null,
    ].filter(Boolean) as string[];
    if (trigger.length === 0) trigger.push("any object");
    const scope = [
      r.camera_id != null ? (cameras.find((c) => c.id === r.camera_id)?.name ?? `camera ${r.camera_id}`) : null,
      r.zone_like ? `zone ~ "${r.zone_like}"` : null,
      r.confirm_label ? `confirmed by ${r.confirm_label} ≤${r.confirm_within_secs ?? 0}s` : null,
      r.vlm_prompt ? `AI-verified: "${r.vlm_prompt}"` : null,
      sched ? `armed ${sched}` : null,
      (r.modes ?? []).length > 0 ? `modes ${(r.modes ?? []).join("/")}` : null,
      r.cooldown_secs > 0 ? `cooldown ${r.cooldown_secs}s` : null,
    ].filter(Boolean) as string[];
    return { trigger, scope };
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
      <h1>Alarm manager</h1>

      <div style={{ display: "flex", flexDirection: "column", gap: 14 }}>
      {creating && (
      <div className="card" style={{ order: 2, margin: 0 }}>
        <h2>New rule — when this happens…</h2>
        <form onSubmit={add}>
          <div className="row" style={{ marginBottom: 8 }}>
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
                    {labelText(l)}
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
            <label className="field">
              plate contains (optional)
              <input type="text" value={plateLike} onChange={(e) => setPlateLike(e.target.value)} placeholder="any plate" />
            </label>
          </div>
          <details className="adv">
            <summary>Advanced conditions — stranger, hand signal, spoken phrase, zone, confirmation</summary>
            <div className="row" style={{ marginTop: 8, marginBottom: 12 }}>
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
              <label className="field" title="Fire only when the object is inside a named detection zone (substring, case-insensitive) — e.g. a 'Pool' zone for 'person in the Pool'. Draw zones on the camera's detect config.">
                in zone (optional)
                <input
                  type="text"
                  value={zoneLike}
                  onChange={(e) => setZoneLike(e.target.value)}
                  placeholder='e.g. "Pool"'
                />
              </label>
              <label className="field" title="Cross-modal confirmation: only fire when an event of THIS label also happened on the same camera recently — e.g. a Glass sound confirmed by a 'person' (glass-vs-dishes). Fails open (fires) on any error, so don't use it to gate a life-safety rule.">
                confirmed by (optional)
                <select value={confirmLabel} onChange={(e) => setConfirmLabel(e.target.value)}>
                  <option value="">none</option>
                  {LABELS.map((l) => (
                    <option key={l} value={l}>
                      {labelText(l)}
                    </option>
                  ))}
                </select>
              </label>
              {confirmLabel && (
                <label className="field" title="Time window (seconds) the confirming event must fall within.">
                  within (s)
                  <input
                    type="number"
                    min="1"
                    step="1"
                    style={{ width: 80 }}
                    value={confirmWithin}
                    onChange={(e) => setConfirmWithin(Math.max(1, Number(e.target.value) || 1))}
                  />
                </label>
              )}
              <label
                className="field"
                style={{ flex: "1 1 100%" }}
                title="Before firing, a local vision model is asked this yes/no question about the snapshot, and the rule fires only if it answers yes — e.g. 'Is a real person at the door?' to filter out shadows, animals and headlights. Needs AI captions + a vision model (Settings); fails open (fires) if the model is unavailable. Detection events only."
              >
                AI verification — fire only if the vision model confirms (optional)
                <input
                  type="text"
                  value={vlmPrompt}
                  onChange={(e) => setVlmPrompt(e.target.value)}
                  placeholder='e.g. "Is a real person actually at the door?"'
                />
              </label>
            </div>
          </details>
          <div className="row" style={{ marginBottom: 12 }}>
            <span className="muted">…armed (optional):</span>
            {DAY_NAMES.map((d, i) => (
              <TogglePill key={d} on={days.includes(i)} onClick={() => toggleDay(i)} ariaLabel={`Armed on ${d}`}>
                {d}
              </TogglePill>
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
              <TogglePill
                key={m.id}
                on={modes.includes(m.id)}
                onClick={() => toggleMode(m.id)}
                ariaLabel={`Active in ${m.label} mode`}
                title={
                  m.id === "disarmed"
                    ? "Include Disarmed to make this a panic rule that fires even when the system is disarmed"
                    : `Active in ${m.label} mode`
                }
              >
                {m.label}
              </TogglePill>
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
                  inputMode={a.kind === "email" ? "email" : a.kind === "webhook" || a.kind === "ntfy" ? "url" : undefined}
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
          </div>
          <div
            className="row"
            style={{ marginTop: 14, paddingTop: 14, borderTop: "1px solid var(--border)", justifyContent: "flex-end" }}
          >
            <button className="btn btn-primary" disabled={busy || !name.trim()}>
              {busy ? "Creating…" : "Create rule"}
            </button>
          </div>
        </form>
      </div>
      )}

      <div className="card" style={{ order: 1, margin: 0 }}>
        <div className="card-head">
          <h2 style={{ margin: 0 }}>
            Rules{loaded && rules.length > 0 ? <span className="tune-count"> ({rules.length})</span> : null}
          </h2>
          <button
            type="button"
            className="btn btn-primary ev-act"
            style={{ marginLeft: "auto" }}
            onClick={() => setCreating((v) => !v)}
          >
            <IconPlus size={14} /> {creating ? "Close" : "New rule"}
          </button>
        </div>
        {!loaded ? (
          <div aria-busy="true">
            {Array.from({ length: 3 }).map((_, i) => (
              <span key={i} className="skeleton" style={{ height: 38, marginBottom: 8 }} />
            ))}
          </div>
        ) : rules.length === 0 ? (
          loadError ? (
            <ErrorState what="alarm rules" message={loadError} onRetry={load} />
          ) : (
            <EmptyState
              icon={<IconSiren />}
              title="No alarm rules yet"
              hint="Rules fire actions the moment a matching event is detected. Create one with the New rule button above."
            />
          )
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
              {rules.map((r) => {
                const parts = describeParts(r);
                return (
                <tr key={r.id}>
                  <td>
                    <b>{r.name}</b>
                  </td>
                  <td title={describe(r)}>
                    <div className="ev-chips" style={{ marginBottom: 4 }}>
                      {parts.trigger.map((t, i) => (
                        <span key={i} className="badge accent" style={{ textTransform: "capitalize" }}>{t}</span>
                      ))}
                    </div>
                    {parts.scope.length > 0 && (
                      <div className="ev-chips">
                        {parts.scope.map((s, i) => (
                          <span key={i} className="badge">{s}</span>
                        ))}
                      </div>
                    )}
                  </td>
                  <td className="muted">
                    {ruleActions(r).map((a, i) => (
                      <div key={i}>{actionText(a)}</div>
                    ))}
                  </td>
                  <td>
                    <TogglePill
                      on={r.enabled}
                      ariaLabel={`Rule ${r.name} ${r.enabled ? "enabled" : "disabled"}`}
                      onClick={async () => {
                        await api
                          .patchAlarm(r.id, { enabled: !r.enabled })
                          .catch((e) => onError(String(e)));
                        load();
                      }}
                    >
                      {r.enabled ? "on" : "off"}
                    </TogglePill>
                    {snoozeText(r) && (
                      <span className="pill" style={{ marginLeft: 6 }} title="snoozed">
                        <IconMoon size={12} /> {snoozeText(r)}
                      </span>
                    )}
                  </td>
                  <td>
                    {snoozeText(r) ? (
                      <button
                        className="btn btn-ghost ev-act"
                        onClick={async () => {
                          await api
                            .patchAlarm(r.id, { snooze_secs: 0 })
                            .catch((e) => onError(String(e)));
                          load();
                        }}
                      >
                        Wake
                      </button>
                    ) : (
                      <button
                        className="btn btn-ghost ev-act"
                        title="Suppress this rule for 1 hour"
                        onClick={async () => {
                          await api
                            .patchAlarm(r.id, { snooze_secs: 3600 })
                            .catch((e) => onError(String(e)));
                          load();
                        }}
                      >
                        Snooze 1h
                      </button>
                    )}
                    <button
                      className="btn btn-danger ev-act"
                      style={{ marginLeft: 8 }}
                      onClick={() => removeRule(r)}
                    >
                      Delete
                    </button>
                  </td>
                </tr>
                );
              })}
            </tbody>
          </table>
          </div>
        )}
      </div>
      </div>
    </>
  );
}
