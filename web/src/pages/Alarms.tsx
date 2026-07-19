import { FormEvent, useEffect, useMemo, useRef, useState } from "react";
import { api, AlarmRule, Action, ActionKind, ArmMode, AttributesCatalog, CamEvent, Camera, DAY_NAMES, DeterCaps } from "../api";
import { IconStranger, IconMoon, IconPlus, IconX, IconSiren, IconPencil } from "../icons";
import { EmptyState, ErrorState, TogglePill, useDialog, useToast } from "../ui";
import { prettyGesture, prettyLabel } from "../labels";

const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e));

/// Client-side approximation of the server's AlarmRule::matches, used for the
/// historical "would have matched" preview and the rules-table 24h counts.
/// Same semantics for the event-shaped conditions (exact label, substring
/// face/plate/zone/transcript, exact-word gesture, "?" stranger sentinel);
/// deliberately NOT applied: schedules/arm modes, min-score, cooldowns,
/// cross-modal confirmation, and the AI gates (vlm_prompt / prompt_like /
/// attr_like — the CLIP appearance gates need the crop embedding, which lives
/// server-side) — the preview shows candidates before those server-side filters
/// run.
function matchPreview(
  cond: {
    camera_id: number | null;
    label: string | null;
    face_like: string | null;
    plate_like: string | null;
    gesture_like: string | null;
    transcript_like: string | null;
    zone_like: string | null;
    face_unknown: boolean;
  },
  ev: CamEvent,
): boolean {
  if (cond.camera_id != null && ev.camera_id !== cond.camera_id) return false;
  if (cond.label && ev.label !== cond.label) return false;
  if (cond.face_unknown && ev.face !== "?") return false;
  if (cond.face_like && !(ev.face ?? "").toLowerCase().includes(cond.face_like.toLowerCase()))
    return false;
  if (cond.plate_like && !(ev.plate ?? "").toUpperCase().includes(cond.plate_like.toUpperCase()))
    return false;
  if (cond.gesture_like && (ev.gesture ?? "").toLowerCase() !== cond.gesture_like.toLowerCase())
    return false;
  const phrase = cond.transcript_like?.trim();
  if (phrase && !(ev.transcript ?? "").toLowerCase().includes(phrase.toLowerCase())) return false;
  if (cond.zone_like && !(ev.zone ?? "").toLowerCase().includes(cond.zone_like.toLowerCase()))
    return false;
  return true;
}

const LABELS = [
  "person", "car", "truck", "bus", "bicycle", "motorcycle", "dog", "cat",
  // Wildlife / nuisance-animal (COCO classes the detector knows — add them to
  // detect labels in Settings to enable). Raccoon/deer aren't COCO; use smart search.
  "bird", "bear", "horse", "sheep", "cow",
  // Tracker-driven analytics events fire alarms via the same label match.
  "crossing", "wrong_way", "loiter", "occupancy",
  // Camera-side (ONVIF-ingested) events — the camera's own chip detected these.
  "camera_person", "camera_vehicle", "camera_motion", "camera_tripwire", "camera_intrusion",
  // Residential analytics events (see ZoneEditor + per-camera detect config).
  "child", "child_alone", "fall", "still_water",
  // Server-side pose events (enable "body pose monitoring" on the camera).
  "standing", "covered_face",
  // P3.5 zero-shot zone-state classifier (experimental; enable per-zone in the
  // ZoneEditor). Scope to a specific instance with "zone contains" (zone_like).
  "zone_open", "zone_closed",
];
// Friendly names for the non-obvious analytics / residential event labels, shown
// in the dropdowns. The raw token is kept in parentheses (and as the option
// `value`) so it still matches what the Family page and docs refer to.
const LABEL_PRETTY: Record<string, string> = {
  camera_person: "Camera: person",
  camera_vehicle: "Camera: vehicle",
  camera_motion: "Camera: motion",
  camera_tripwire: "Camera: tripwire",
  camera_intrusion: "Camera: intrusion",
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
  zone_open: "Zone became open*",
  zone_closed: "Zone became closed*",
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
  { id: "deterrence", label: "trigger siren/light (ONVIF relay)" },
];
const targetHint = (kind: ActionKind) =>
  kind === "webhook"
    ? "https://… (receives the event JSON)"
    : kind === "mqtt"
      ? "topic name, published under <your MQTT prefix>/alarms/<name>"
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
  const [editingId, setEditingId] = useState<number | null>(null);
  const [editEnabled, setEditEnabled] = useState(true); // preserve on-edit enabled state
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
  const [describeAlert, setDescribeAlert] = useState(false);
  const [promptLike, setPromptLike] = useState("");
  // P2.5 attribute facet: a curated catalog KEY (or "" for none).
  const [attrLike, setAttrLike] = useState("");
  const [attrCatalog, setAttrCatalog] = useState<AttributesCatalog | null>(null);
  const [faceUnknown, setFaceUnknown] = useState(false);
  const [actions, setActions] = useState<Action[]>([{ kind: "webhook", target: "", priority: 0 }]);
  const [modes, setModes] = useState<ArmMode[]>([]);
  const [cooldown, setCooldown] = useState(0);
  const [days, setDays] = useState<number[]>([]);
  const [startTime, setStartTime] = useState("");
  const [endTime, setEndTime] = useState("");

  const toggleDay = (d: number) =>
    setDays((prev) => (prev.includes(d) ? prev.filter((x) => x !== d) : [...prev, d].sort()));
  const toggleMode = (m: ArmMode) =>
    setModes((p) => (p.includes(m) ? p.filter((x) => x !== m) : [...p, m]));
  const updateAction = (i: number, patch: Partial<Action>) =>
    setActions((p) => p.map((a, j) => (j === i ? { ...a, ...patch } : a)));
  const addAction = () => setActions((p) => [...p, { kind: "ntfy", target: "", priority: 0 }]);
  const removeAction = (i: number) =>
    setActions((p) => (p.length > 1 ? p.filter((_, j) => j !== i) : p));

  // P2.9 deterrence: when a "trigger siren/light" action is present on a
  // camera-scoped rule, probe that camera's ONVIF relay outputs so we can offer
  // the REAL relay tokens (never a blind text box), or be honest about why there
  // are none.
  const hasDeter = actions.some((a) => a.kind === "deterrence");
  const [deterCaps, setDeterCaps] = useState<DeterCaps | null>(null);
  const [deterLoading, setDeterLoading] = useState(false);
  // A pulse fires a PHYSICAL relay (siren/light); one shared in-flight flag
  // disables every Test button so rapid clicks can't queue overlapping pulses.
  const [deterTesting, setDeterTesting] = useState(false);
  // Which rule's "Test" (fires the real webhook/push/email) is in flight.
  const [testingId, setTestingId] = useState<number | null>(null);
  useEffect(() => {
    if (!hasDeter || cameraId === "") {
      setDeterCaps(null);
      setDeterLoading(false);
      return;
    }
    const cid = cameraId as number;
    let cancelled = false;
    setDeterLoading(true);
    setDeterCaps(null);
    api
      .deterProbe(cid)
      .then((c) => {
        if (cancelled) return;
        setDeterCaps(c);
        // Drop a stale relay token this camera doesn't actually expose, so we
        // never save camera A's token against camera B.
        const valid = new Set(c.relays.map((r) => r.token));
        setActions((prev) =>
          prev.map((a) =>
            a.kind === "deterrence" && a.target && !valid.has(a.target) ? { ...a, target: "" } : a,
          ),
        );
      })
      .catch((e) => !cancelled && setDeterCaps({ relays: [], error: errMsg(e) }))
      .finally(() => !cancelled && setDeterLoading(false));
    return () => {
      cancelled = true;
    };
  }, [hasDeter, cameraId]);
  const testRelay = async (token: string) => {
    if (cameraId === "" || deterTesting) return;
    setDeterTesting(true);
    try {
      await api.deterTest(cameraId as number, token, 2);
      toast.success(
        "Relay command accepted by the camera — confirm a siren/light is actually wired to this relay",
      );
    } catch (e) {
      onError(errMsg(e));
    } finally {
      setDeterTesting(false);
    }
  };

  // First-run gate for auto-opening the builder: only the FIRST successful
  // load may open it (never a fetch error, never a later refetch — e.g. after
  // deleting the last rule the list should lead, not the builder).
  const firstLoad = useRef(true);
  const [stats, setStats] = useState<Record<string, { last_fired_ts: number; suppressed_since: number }>>({});
  // The last 24 hours of events, for the per-rule match counts and the
  // builder's "would have matched" preview (Protect-style trigger previews).
  const [recent, setRecent] = useState<CamEvent[]>([]);
  // Whether speech transcription is on globally — a spoken-phrase rule can
  // never fire without it, so the builder warns instead of arming a dead rule.
  const [transcriptionOn, setTranscriptionOn] = useState<boolean | null>(null);
  // Whether deterrence actions are enabled globally — a rule can carry a
  // siren/light action while the master switch is OFF, so the builder warns
  // that it won't actually fire until enabled in Settings.
  const [deterEnabled, setDeterEnabled] = useState<boolean | null>(null);
  const load = () => {
    api
      .settings()
      .then((s) => {
        setTranscriptionOn(s.transcription_enabled);
        setDeterEnabled(s.deterrence_enabled);
      })
      .catch(() => {});
    api
      .alarms()
      .then((r) => {
        setRules(r);
        setLoadError(null);
        if (firstLoad.current && r.length === 0) setCreating(true);
        firstLoad.current = false;
      })
      .catch((e) => setLoadError(errMsg(e)))
      .finally(() => setLoaded(true));
    api.alarmStats().then(setStats).catch(() => {});
    api
      .events({ after: Math.floor(Date.now() / 1000) - 86400, limit: 1000 })
      .then(setRecent)
      .catch(() => {});
  };
  // On a busy system the 24h window can exceed the server's 1000-row cap —
  // the previews then cover a shorter window and must say so, not read low.
  const recentCapped = recent.length >= 1000;
  const recentWindow = recentCapped
    ? `since ${new Date(recent[recent.length - 1].ts * 1000).toLocaleTimeString([], {
        hour: "numeric",
        minute: "2-digit",
      })}`
    : "24h";
  // Memoized so typing in unrelated builder fields (rule name, action targets)
  // doesn't re-run rules × recent matching on every keystroke.
  const ruleCounts = useMemo(
    () => new Map(rules.map((r) => [r.id, recent.filter((e) => matchPreview(r, e)).length])),
    [rules, recent],
  );
  // Builder preview inputs, memoized on the condition fields only.
  const previewCond = useMemo(
    () => ({
      camera_id: cameraId === "" ? null : cameraId,
      label: label || null,
      face_like: faceUnknown ? null : faceLike.trim() || null,
      plate_like: plateLike.trim() || null,
      gesture_like: gestureLike.trim() || null,
      transcript_like: transcriptLike.trim() || null,
      zone_like: zoneLike.trim() || null,
      face_unknown: faceUnknown,
    }),
    [cameraId, label, faceLike, plateLike, gestureLike, transcriptLike, zoneLike, faceUnknown],
  );
  const previewHits = useMemo(
    () => recent.filter((e) => matchPreview(previewCond, e)),
    [recent, previewCond],
  );
  useEffect(load, []);
  // The attribute-facet catalog (static) drives the attr_like dropdown.
  useEffect(() => {
    api.attributes().then(setAttrCatalog).catch(() => {});
  }, []);

  // "New rule" can sit below a long rules list — when the builder opens via
  // the button, bring it into view and focus its first input.
  const builderRef = useRef<HTMLDivElement | null>(null);
  const openBuilder = () => {
    setCreating(true);
    requestAnimationFrame(() => {
      builderRef.current?.scrollIntoView({ behavior: "smooth", block: "start" });
      builderRef.current?.querySelector("input")?.focus({ preventScroll: true });
    });
  };

  const removeRule = async (r: AlarmRule) => {
    if (
      !(await dialog.confirm({
        title: `Delete rule “${r.name}”?`,
        body: "This alert rule is removed permanently.",
        confirmLabel: "Delete",
        danger: true,
      }))
    )
      return;
    try {
      await api.deleteAlarm(r.id);
      toast.success("Rule deleted");
      load();
    } catch (e) {
      onError(String(e));
    }
  };

  const resetBuilder = () => {
    setEditingId(null);
    setEditEnabled(true);
    setName("");
    setCameraId("");
    setLabel("");
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
    setDescribeAlert(false);
    setPromptLike("");
    setAttrLike("");
    setFaceUnknown(false);
    setCooldown(0);
    setDays([]);
    setStartTime("");
    setEndTime("");
  };

  // Load an existing rule into the builder to EDIT it in place (rather than the
  // former delete-and-recreate-from-scratch), then open the builder.
  const editRule = (r: AlarmRule) => {
    setEditingId(r.id);
    setEditEnabled(r.enabled);
    setName(r.name);
    setCameraId(r.camera_id ?? "");
    setLabel(r.label ?? "");
    setFaceLike(r.face_like ?? "");
    setPlateLike(r.plate_like ?? "");
    setGestureLike(r.gesture_like ?? "");
    setTranscriptLike(r.transcript_like ?? "");
    setZoneLike(r.zone_like ?? "");
    setConfirmLabel(r.confirm_label ?? "");
    setConfirmWithin(r.confirm_within_secs ?? 10);
    setVlmPrompt(r.vlm_prompt ?? "");
    setDescribeAlert(!!r.describe);
    setPromptLike(r.prompt_like ?? "");
    setAttrLike(r.attr_like ?? "");
    setFaceUnknown(!!r.face_unknown);
    setCooldown(r.cooldown_secs ?? 0);
    setDays(r.days ?? []);
    setStartTime(r.start_hhmm ?? "");
    setEndTime(r.end_hhmm ?? "");
    setActions(
      r.actions && r.actions.length
        ? r.actions.map((a) => ({ ...a }))
        : [{ kind: "webhook", target: r.target ?? "", priority: r.priority ?? 0 }],
    );
    setModes(r.modes ?? []);
    setCreating(true);
  };

  const add = async (e: FormEvent) => {
    e.preventDefault();
    const acts = actions.map((a) => ({ ...a, target: a.target.trim() }));
    if (acts.some((a) => a.kind === "deterrence") && cameraId === "") {
      onError("a siren/light action needs the rule scoped to a specific camera (the “on camera” selector)");
      return;
    }
    if (acts.some((a) => a.kind === "deterrence" && !a.target)) {
      onError("pick a relay output for the siren/light action");
      return;
    }
    if (acts.some((a) => a.kind !== "email" && a.kind !== "deterrence" && !a.target)) {
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
      const body = {
        name: name.trim(),
        enabled: editingId != null ? editEnabled : true,
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
        describe: describeAlert,
        prompt_like: promptLike.trim() || null,
        attr_like: attrLike || null,
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
      };
      if (editingId != null) {
        await api.updateAlarm(editingId, body);
        toast.success(`Rule “${name.trim()}” updated`);
      } else {
        await api.addAlarm(body);
        toast.success(`Rule “${name.trim()}” created`);
      }
      setCreating(false);
      resetBuilder();
      load();
    } catch (err) {
      onError(String(err));
    } finally {
      setBusy(false);
    }
  };

  // Resolve an attr_like catalog key to its "Group · Label" for the summaries
  // (falls back to the raw key if the catalog isn't loaded / the key is stale).
  const attrLabel = (key: string): string => {
    for (const g of attrCatalog?.groups ?? []) {
      const a = g.attrs.find((x) => x.key === key);
      if (a) return `${g.label} · ${a.label}`;
    }
    return key;
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
      r.gesture_like ? `signal ${prettyGesture(r.gesture_like)}` : null,
      r.transcript_like ? `said "${r.transcript_like}"` : null,
      r.zone_like ? `in zone ~ "${r.zone_like}"` : null,
      r.confirm_label ? `confirmed by ${r.confirm_label} ≤${r.confirm_within_secs ?? 0}s` : null,
      r.prompt_like ? `AI watch: "${r.prompt_like}"` : null,
      r.attr_like ? `AI watch: ${attrLabel(r.attr_like)}` : null,
      r.vlm_prompt ? `AI-verified: "${r.vlm_prompt}"` : null,
      r.describe ? "AI-described push" : null,
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
      r.gesture_like ? `signal ${prettyGesture(r.gesture_like)}` : null,
      r.transcript_like ? `said "${r.transcript_like}"` : null,
    ].filter(Boolean) as string[];
    if (trigger.length === 0) trigger.push("any object");
    const scope = [
      r.camera_id != null ? (cameras.find((c) => c.id === r.camera_id)?.name ?? `camera ${r.camera_id}`) : null,
      r.zone_like ? `zone ~ "${r.zone_like}"` : null,
      r.confirm_label ? `confirmed by ${r.confirm_label} ≤${r.confirm_within_secs ?? 0}s` : null,
      r.prompt_like ? `AI watch: "${r.prompt_like}"` : null,
      r.attr_like ? `AI watch: ${attrLabel(r.attr_like)}` : null,
      r.vlm_prompt ? `AI-verified: "${r.vlm_prompt}"` : null,
      r.describe ? "AI-described push" : null,
      sched ? `armed ${sched}` : null,
      (r.modes ?? []).length > 0 ? `modes ${(r.modes ?? []).join("/")}` : null,
      r.cooldown_secs > 0 ? `cooldown ${r.cooldown_secs}s` : null,
    ].filter(Boolean) as string[];
    return { trigger, scope };
  };

  /// Humanized action line: kind + just the host/topic (the full URL stays in
  /// the tooltip — a raw ntfy secret-topic URL in a table cell is noise AND a
  /// shoulder-surf leak).
  const actionHost = (target: string) => {
    try {
      return new URL(target).host || target;
    } catch {
      return target;
    }
  };
  const actionText = (a: Action) =>
    a.kind === "webhook"
      ? `Webhook → ${actionHost(a.target)}`
      : a.kind === "mqtt"
        ? `MQTT → alarms/${a.target}`
        : a.kind === "email"
          ? `Email → ${a.target || "default recipient"}`
          : a.kind === "deterrence"
            ? `Siren/light → relay ${a.target}`
            : `Push (ntfy)${a.priority ? ` · priority ${a.priority}` : ""}`;

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
      <h1>Alarms</h1>

      <div style={{ display: "flex", flexDirection: "column", gap: 14 }}>
      <div className="card" style={{ margin: 0 }}>
        <div className="card-head">
          <h2 style={{ margin: 0 }}>
            Rules{loaded && rules.length > 0 ? <span className="tune-count"> ({rules.length})</span> : null}
          </h2>
          <button
            type="button"
            className="btn btn-primary ev-act"
            style={{ marginLeft: "auto" }}
            onClick={() => {
              // Closing (or opening a fresh New rule) always clears any in-progress
              // edit so the builder doesn't inherit the last-edited rule's state.
              if (creating) {
                setCreating(false);
                resetBuilder();
              } else {
                resetBuilder();
                openBuilder();
              }
            }}
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
                <th>Last fired</th>
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
                        <span key={i} className="badge accent">{t}</span>
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
                  <td className="muted" title="A live counter since the server last started — not stored history">
                    {stats[String(r.id)]?.last_fired_ts ? (
                      <>
                        {new Date(stats[String(r.id)].last_fired_ts * 1000).toLocaleString([], {
                          month: "short", day: "numeric", hour: "2-digit", minute: "2-digit",
                        })}
                        {stats[String(r.id)].suppressed_since > 0 && (
                          <div style={{ fontSize: "var(--text-sm)" }}>
                            +{stats[String(r.id)].suppressed_since} held back by cooldown
                          </div>
                        )}
                      </>
                    ) : (
                      <span style={{ fontSize: "var(--text-sm)" }}>not since startup</span>
                    )}
                    {recent.length > 0 && (() => {
                      const n = ruleCounts.get(r.id) ?? 0;
                      // An AI-gated rule (watch prompt / verification question)
                      // decides on far fewer than its raw candidates — label
                      // the number honestly instead of implying fired alerts.
                      const aiGated =
                        !!(r.prompt_like ?? "").trim() ||
                        !!(r.attr_like ?? "").trim() ||
                        !!(r.vlm_prompt ?? "").trim();
                      return (
                        <div
                          style={{ fontSize: "var(--text-sm)" }}
                          title="Events matching this rule's conditions — before schedules, cooldowns, and AI checks."
                        >
                          <span className="tnum">{n}</span>{" "}
                          {aiGated
                            ? `candidate${n === 1 ? "" : "s"} for the AI check`
                            : `matching event${n === 1 ? "" : "s"}`}{" "}
                          · {recentWindow}
                        </div>
                      );
                    })()}
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
                      className="btn btn-ghost ev-act"
                      style={{ marginLeft: 8 }}
                      disabled={testingId === r.id}
                      title="Fire this rule's actions once with a synthetic test event — verifies the webhook/push/email wiring without waiting for a detection"
                      onClick={async () => {
                        if (testingId != null) return;
                        setTestingId(r.id);
                        try {
                          await api.testAlarm(r.id);
                          toast.success(
                            `Test sent — check that the ${[...new Set(ruleActions(r).map((a) => a.kind))].join(" + ")} target received it`,
                          );
                        } catch (e) {
                          onError(String(e));
                        } finally {
                          setTestingId(null);
                        }
                      }}
                    >
                      {testingId === r.id ? "Sending…" : "Test"}
                    </button>
                    <button
                      className="btn btn-ghost ev-act"
                      title="Edit this rule"
                      onClick={() => editRule(r)}
                    >
                      <IconPencil size={13} /> Edit
                    </button>
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

      {creating && (
      <div ref={builderRef} className="card" style={{ margin: 0 }}>
        <h2>{editingId != null ? "Edit rule" : "New rule"} — when this happens…</h2>
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
              {label && LABEL_PRETTY[label]?.includes("*") && (
                <small className="muted">
                  * Assistive best-effort detection — not a safety or medical device. Confirm in
                  person.
                </small>
              )}
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
            <summary>Advanced conditions — stranger, hand signal, spoken phrase, zone, confirmation, AI watch &amp; verification</summary>
            <div className="row" style={{ marginTop: 8, marginBottom: 12 }}>
              <label
                className="field"
                title="Fire when a person's face is detected but matches nobody you've enrolled — a stranger / unfamiliar-face alert. Needs face recognition on the camera, and at least one enrolled identity (enroll known faces on the People page) so only true unknowns alert."
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
                    Enroll known faces (People page) first — strangers are detected only
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
                      {prettyGesture(g)}
                    </option>
                  ))}
                </select>
              </label>
              <label className="field">
                spoken phrase (optional)
                <input
                  type="text"
                  value={transcriptLike}
                  onChange={(e) => setTranscriptLike(e.target.value)}
                  placeholder='e.g. "help"'
                />
                <small className="muted">
                  Fires when this is heard near the camera — a spoken safe word.
                </small>
                {transcriptLike.trim() !== "" && transcriptionOn === false && (
                  <small style={{ color: "var(--warn)" }} role="status">
                    Speech transcription is off, so this rule can't fire — turn it on in
                    Settings → Detection &amp; AI.
                  </small>
                )}
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
                <small className="muted">
                  Fire only if this other event was also seen on the same camera within the time
                  window.
                </small>
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
                title="A standing description matched against every detection's image crop (CLIP similarity) — 'someone climbing the fence', 'a red pickup truck'. Fires when an object LOOKS like the prompt. Needs the smart-search (CLIP) models; best-effort semantic matching, so scope it with an object/camera/zone for precision."
              >
                AI watch — fire when an object looks like… (optional)
                <input
                  type="text"
                  value={promptLike}
                  onChange={(e) => setPromptLike(e.target.value)}
                  placeholder='e.g. "someone climbing the fence", "a red pickup truck"'
                />
                <small className="muted">
                  Adds matches: fires when a detected object visually resembles this description
                  (needs the smart-search models). Best-effort — pair with an object/camera/zone
                  scope for precision.
                </small>
              </label>
              <label
                className="field"
                style={{ flex: "1 1 100%" }}
                title="A curated appearance attribute (vehicle colour/type, clothing colour) matched against each detection's image crop via CLIP — the same 'AI watch' mechanism as the free-text field above, but picked from a list. Needs the smart-search (CLIP) models; best-effort semantic matching, so scope it with an object/camera/zone for precision."
              >
                AI watch — attribute (optional)
                <select value={attrLike} onChange={(e) => setAttrLike(e.target.value)}>
                  <option value="">none</option>
                  {(attrCatalog?.groups ?? []).map((g) => (
                    <optgroup key={g.group} label={g.label}>
                      {g.attrs.map((a) => (
                        <option key={a.key} value={a.key}>
                          {a.label}
                        </option>
                      ))}
                    </optgroup>
                  ))}
                </select>
                <small className="muted">
                  Adds matches: fires when a detected object looks like this attribute, e.g. a red
                  car or a person in blue (needs the smart-search models). Best-effort — scope it
                  with an object/camera/zone for precision.
                </small>
                {attrCatalog && !attrCatalog.available && (
                  <small style={{ color: "var(--warn)" }} role="status">
                    Smart-search (CLIP) models aren't installed, so this can't fire — see Settings.
                  </small>
                )}
              </label>
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
                <small className="muted">
                  Filters matches: a yes/no question the vision model answers about the snapshot
                  before this rule fires (needs AI captions in Settings). Fails open — a model
                  outage never silences the rule. Detection events only.
                </small>
              </label>
              <div
                className="row"
                style={{ flex: "1 1 100%", alignItems: "center", gap: 8 }}
                title="The vision model writes a one-line description of the snapshot and it leads the push/email text (like Wyze/Nest descriptive alerts) — 'A courier is leaving a box on the porch' instead of 'person (91%)'. Needs AI captions + a vision model (Settings); if the model is unavailable the alert still fires without a description. Detection events only."
              >
                <TogglePill on={describeAlert} onClick={() => setDescribeAlert(!describeAlert)} ariaLabel="Describe in notification">
                  AI description in the notification
                </TogglePill>
                <span className="muted" style={{ fontSize: "var(--text-sm)" }}>
                  The push leads with what the camera saw, in plain language (needs AI captions in
                  Settings; falls back to a normal alert if the model is unavailable).
                </span>
              </div>
            </div>
          </details>
          {/* Protect-style historical trigger preview: as conditions change,
              show what the rule WOULD have fired on in the last 24 hours —
              catching an over-broad or dead rule before it's saved. */}
          {(() => {
            const cond = previewCond;
            const anyCond =
              cond.camera_id != null ||
              !!cond.label ||
              !!cond.face_like ||
              !!cond.plate_like ||
              !!cond.gesture_like ||
              !!cond.transcript_like ||
              !!cond.zone_like ||
              cond.face_unknown;
            if (!anyCond) return null;
            const hits = previewHits;
            const aiGated = !!(vlmPrompt.trim() || promptLike.trim() || attrLike);
            return (
              <div className="rule-preview" role="status">
                <span className="muted">
                  Would have matched <b className="tnum">{hits.length}</b> event
                  {hits.length === 1 ? "" : "s"}{" "}
                  {recentCapped ? recentWindow : "in the last 24 hours"}
                  {aiGated ? " (before the AI check runs)" : ""}
                  {hits.length === 0 ? " — nothing recent fits these conditions." : "."}
                </span>
                {hits.length > 0 && (
                  <div className="rule-preview-strip">
                    {hits.slice(0, 6).map((e) =>
                      e.snapshot ? (
                        <img
                          key={e.id}
                          className="rule-preview-thumb"
                          src={`/api/snapshots/${e.snapshot}?w=160`}
                          alt={`${prettyLabel(e.label)} on ${e.camera}`}
                          loading="lazy"
                          title={`${prettyLabel(e.label)} · ${e.camera} · ${new Date(e.ts * 1000).toLocaleString()}`}
                        />
                      ) : null,
                    )}
                    {hits.length > 6 && <span className="muted tnum">+{hits.length - 6} more</span>}
                  </div>
                )}
              </div>
            );
          })()}
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
                  onChange={(e) => {
                    const kind = e.target.value as ActionKind;
                    // Switching to deterrence clears the old URL/topic target —
                    // its target is a relay token chosen from the probe below.
                    updateAction(i, kind === "deterrence" ? { kind, target: "", priority: 0 } : { kind });
                  }}
                >
                  {ACTION_KINDS.map((k) => (
                    <option key={k.id} value={k.id}>
                      {k.label}
                    </option>
                  ))}
                </select>
                {a.kind === "deterrence" ? (
                  <div style={{ flex: 1, minWidth: 240, display: "flex", flexDirection: "column", gap: 6 }}>
                    {cameraId === "" ? (
                      <div className="callout callout-info" role="status">
                        Pick a specific camera for this rule (the “on camera” selector above) so its relay
                        output can be triggered.
                      </div>
                    ) : deterLoading ? (
                      <span className="muted">Checking this camera’s relay outputs…</span>
                    ) : deterCaps && deterCaps.relays.length > 0 ? (
                      <div className="row" style={{ gap: 8 }}>
                        <select
                          value={a.target}
                          style={{ flex: 1, minWidth: 180 }}
                          onChange={(e) => updateAction(i, { target: e.target.value })}
                        >
                          <option value="">choose a relay output…</option>
                          {deterCaps.relays.map((r) => (
                            <option key={r.token} value={r.token}>
                              {r.token}
                              {r.mode ? ` (${r.mode})` : ""}
                            </option>
                          ))}
                        </select>
                        <button
                          type="button"
                          className="ghost"
                          disabled={!a.target || deterTesting}
                          title="Pulse this relay now to confirm the siren/light is wired (the camera must accept the command)"
                          onClick={() => testRelay(a.target)}
                        >
                          {deterTesting ? "Pulsing…" : "Test"}
                        </button>
                      </div>
                    ) : (
                      <div className="callout callout-warn" role="status">
                        {deterCaps?.error ??
                          "This camera reports no ONVIF relay output for a siren or light."}
                      </div>
                    )}
                  </div>
                ) : (
                  <input
                    type="text"
                    inputMode={a.kind === "email" ? "email" : a.kind === "webhook" || a.kind === "ntfy" ? "url" : undefined}
                    style={{ flex: 1, minWidth: 240 }}
                    value={a.target}
                    onChange={(e) => updateAction(i, { target: e.target.value })}
                    placeholder={targetHint(a.kind)}
                  />
                )}
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
            {hasDeter && deterEnabled === false && (
              <div className="callout callout-warn" role="status" style={{ marginTop: 8 }}>
                Deterrence actions are turned off globally — this won’t trigger anything until you
                enable them in Settings → Modes &amp; alerts.
              </div>
            )}
          </div>
          <div className="row" style={{ marginTop: 12 }}>
            <span className="muted">…anti-fatigue:</span>
            <label className="field">
              quiet period (s)
              <input
                type="number" min="0" style={{ width: 90 }}
                value={cooldown}
                onChange={(e) => setCooldown(Math.max(0, Number(e.target.value) || 0))}
                title="Minimum seconds between firings of this rule (debounces the whole scene)."
              />
              <small className="muted">
                Least time between alerts from this rule, so you aren't pinged repeatedly.
              </small>
            </label>
          </div>
          {!label &&
            cameraId === "" &&
            !zoneLike.trim() &&
            !faceLike.trim() &&
            !plateLike.trim() &&
            !gestureLike.trim() &&
            !transcriptLike.trim() &&
            !promptLike.trim() &&
            !attrLike &&
            !vlmPrompt.trim() &&
            !confirmLabel.trim() &&
            !faceUnknown && (
              <div className="callout callout-warn" role="status" style={{ marginTop: 14 }}>
                Heads up — this rule has no conditions, so it matches <b>every detection on every camera</b>. Add
                an object, camera, zone, face, or plate to scope it (or expect a lot of alerts).
              </div>
            )}
          <div
            className="row"
            style={{ marginTop: 14, paddingTop: 14, borderTop: "1px solid var(--border)", justifyContent: "flex-end" }}
          >
            <button className="btn btn-primary" disabled={busy || !name.trim()}>
              {busy
                ? editingId != null
                  ? "Saving…"
                  : "Creating…"
                : editingId != null
                  ? "Save changes"
                  : "Create rule"}
            </button>
          </div>
        </form>
      </div>
      )}
      </div>
    </>
  );
}
