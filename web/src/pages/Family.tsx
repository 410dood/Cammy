import { useEffect, useMemo, useState } from "react";
import { api, AlarmRule, Camera, CamEvent, DetectConfig, fmtTime, PolyZone, capabilityUsable } from "../api";
import { enableWebPush, webPushSupported } from "../webpush";
import { IconInfo, IconAlert, IconCheck } from "../icons";
import { Modal, useToast } from "../ui";
import { ChildHeightEditor, Rect, RectZoneDraw } from "../SizeFilterEditor";
import { prettyLabel } from "../labels";

/// A residential "mode": a plain-language recipe that ties together the camera
/// toggles, zones, sounds and alarm rules already shipped — so a non-expert can
/// set up "baby monitoring" without hunting across four settings pages.
type Mode = {
  key: string;
  title: string;
  blurb: string;
  /// Friendly names of what it watches for (shown as chips).
  watches: string[];
  /// Event labels that count as "recent activity" for this mode.
  labels: string[];
  /// Step-by-step setup, in plain language.
  setup: string[];
  /// Safety/limitation note (shown for the safety-critical modes).
  safety?: string;
};

const MODES: Mode[] = [
  {
    key: "baby",
    title: "Baby & nursery",
    blurb: "Watch the crib for your baby standing up, their face becoming covered, or crying.",
    watches: ["standing in crib", "covered face", "baby crying", "fall"],
    labels: ["standing", "covered_face", "fall"],
    setup: [
      "On the nursery camera (Cameras page) turn on “body pose monitoring” and “audio detection”.",
      "In that camera’s zone editor, draw a zone over the crib and name it (e.g. “Crib”).",
      "Posture and fall alerts need a one-time extra download (the pose model). Get it from the models list in the README, then point Settings, Models & capabilities at the file.",
      "On the Alarms page add rules: “Standing — crib climb-out” in zone “Crib”, “Covered face” in zone “Crib”, and a “Baby cry” sound alarm. Pick how you want to be notified.",
    ],
    safety:
      "Assistive only. This is NOT a breathing, oxygen, or SIDS monitor and cannot guarantee detection. Always follow safe-sleep practices and check on your baby in person.",
  },
  {
    key: "pet",
    title: "Pets",
    blurb: "Know when a pet is somewhere off-limits, barking, or has slipped out of the yard.",
    watches: ["dog / cat detected", "on the couch / counter", "barking", "left the yard"],
    labels: ["dog", "cat"],
    setup: [
      "On the indoor/yard camera turn on object detection (dog & cat are detected by default).",
      "Draw a zone over an off-limits spot (couch, counter) and tick its “enter” flag for a “pet on the couch” alert; for the yard, draw a perimeter tripwire for an “escaped” alert.",
      "Turn on “audio detection” and enable the “Dog bark” / “Cat meow” sounds in Settings.",
      "On the Alarms page add rules scoped by object (dog/cat) and zone.",
    ],
    safety:
      "Assistive only — best-effort detection that can miss a pet (small breeds, odd angles, poor light) and isn’t a substitute for secure fencing, gates or supervision.",
  },
  {
    key: "pool",
    title: "Pool & water safety",
    blurb: "Get alerted when someone enters the pool area — especially a child with no adult nearby.",
    watches: ["person enters pool", "child alone near pool", "no movement in water"],
    // "person" catches the headline zone-enter event (a zone-enter fires with the
    // object's own label, e.g. "person"), not just the child-alone / still-water hints.
    labels: ["child_alone", "still_water", "person"],
    setup: [
      "On the pool camera, draw a zone over the pool/deck. Tick “enter” for a presence alert, “alone” for the child-with-no-adult alert, and “water” for the motionless-in-water hint.",
      "For the child alerts, set “child height ≤” on that camera (Cameras page) so it can tell children from adults — tune it once for your view.",
      "On the Alarms page add rules: “Child alone” in your pool zone (and optionally “Motionless in water”).",
    ],
    safety:
      "This is a supplement, NOT a replacement for a pool fence and active supervision. It is NOT drowning detection — an above-water camera cannot see a submerged child, and the child/adult guess can be wrong. Never rely on it alone.",
  },
  {
    key: "aging",
    title: "Aging in place",
    blurb: "A gentle watch for a fall, a bathroom overstay, or nighttime wandering for a loved one living alone.",
    watches: ["fall", "left a zone at night", "stayed in a zone too long"],
    // Include the overstay/wandering events (loiter dwell, tripwire crossing), not
    // just fall — the setup steps point users at exactly those.
    labels: ["fall", "loiter", "crossing"],
    setup: [
      "On the room camera turn on “body pose monitoring” and “fall detection”.",
      "For overstay/wandering, draw a zone (e.g. a bed) and set a dwell time, or use a doorway tripwire; add a night time-window to the alarm rule.",
      "On the Alarms page add a “fall” rule (any zone). Consider requiring confirmation by a “Screaming” sound to cut false alarms — but never let it suppress a real alert.",
    ],
    safety:
      "Assistive only — it can miss falls (behind furniture, soft/slow falls) and is NOT a substitute for a medical-alert pendant. Don’t auto-dial emergency services from a single visual trigger.",
  },
];

type GoPage = "Cameras" | "Alarms" | "Settings" | "Live";

// ---------------------------------------------------------------------------
// docs/10 P2.5 — the guided setup wizard. Each mode used to be a 3–4 step prose
// manual spanning four pages; this walks the same recipe in-card: pick the
// camera → draw the zone on its live frame → the app flips the toggles and
// creates the alarm rules itself → send a test push. The prose stays available
// as "manual steps" for people who want to wire it themselves.
// ---------------------------------------------------------------------------


/// Everything the wizard creates for one mode. Rules are built at apply time so
/// they carry the chosen camera / zone / push topic.
type WizardPlan = {
  /// Preset zone name (the zone step is skipped when absent — e.g. aging).
  zoneName?: string;
  zoneLabels?: string[];
  zoneFlags?: Partial<PolyZone>;
  /// What to say the box means on the draw step ("Crib", "Pool area", …).
  zoneThing?: string;
  /// Plain-language consequence of the (required-kind) zone, stated up front.
  zoneNote?: string;
  camPatch: Partial<DetectConfig>;
  needsPose?: boolean;
  childHeight?: boolean;
  /// AudioSet display names the mode listens for — added to Settings if missing.
  ensureAudioLabels?: string[];
  rules: (cameraId: number, zone: string | null, ntfy: string) => NewRule[];
};

type NewRule = Omit<AlarmRule, "id" | "created_ts">;

/// One alarm rule in the exact shape the builder submits (the Onboarding
/// starter-rule shape), with only the interesting fields varying.
function mkRule(
  name: string,
  ntfy: string,
  over: Partial<NewRule> & { pushPriority?: number },
): NewRule {
  const { pushPriority, ...rest } = over;
  return {
    name,
    enabled: true,
    camera_id: null,
    label: null,
    face_like: null,
    plate_like: null,
    gesture_like: null,
    transcript_like: null,
    face_unknown: false,
    zone_like: null,
    confirm_label: null,
    confirm_within_secs: null,
    vlm_prompt: null,
    min_score: 0,
    // docs/11 P2 — an empty topic means the built-in push channel (subscribed
    // browsers/phones), so a no-new-apps homeowner still gets these alerts.
    action: ntfy ? "ntfy" : "push",
    target: ntfy,
    days: [],
    start_hhmm: null,
    end_hhmm: null,
    cooldown_secs: 120,
    priority: 0,
    snooze_until: 0,
    modes: [],
    actions: [{ kind: ntfy ? "ntfy" : "push", target: ntfy, priority: pushPriority ?? 0 }],
    ...rest,
  } as NewRule;
}

const WIZARDS: Record<string, WizardPlan> = {
  baby: {
    zoneName: "Crib",
    zoneThing: "crib",
    zoneLabels: ["person"],
    zoneNote:
      "The crib becomes this camera's watched area: person alerts on this camera will focus on it.",
    camPatch: { pose_detect: true, audio_detect: true },
    needsPose: true,
    ensureAudioLabels: ["Baby cry, infant cry"],
    rules: (cam, zone, ntfy) => [
      mkRule("Baby standing in the crib", ntfy, {
        camera_id: cam, label: "standing", zone_like: zone, cooldown_secs: 300,
      }),
      mkRule("Face covered in the crib", ntfy, {
        camera_id: cam, label: "covered_face", zone_like: zone, cooldown_secs: 120, pushPriority: 5,
      }),
      mkRule("Baby crying heard", ntfy, {
        camera_id: cam, label: "baby cry, infant cry", cooldown_secs: 300,
      }),
    ],
  },
  pet: {
    zoneName: "Couch",
    zoneThing: "off-limits spot (couch, counter…)",
    zoneLabels: ["dog", "cat"],
    zoneFlags: { alert_enter: true },
    zoneNote:
      "Dog/cat alerts on this camera will fire only from this spot — that's the point of an off-limits zone.",
    camPatch: { audio_detect: true },
    ensureAudioLabels: ["Bark"],
    rules: (cam, zone, ntfy) => [
      mkRule("Pet somewhere off-limits", ntfy, {
        camera_id: cam, zone_like: zone, cooldown_secs: 300,
      }),
      mkRule("Dog barking heard", ntfy, {
        camera_id: cam, label: "bark", cooldown_secs: 600,
      }),
    ],
  },
  pool: {
    zoneName: "Pool",
    zoneThing: "pool and its deck",
    zoneLabels: [],
    zoneFlags: { alert_enter: true, supervise: true, water: true },
    zoneNote:
      "This camera's alerts will focus on the pool area you draw (detections outside it are ignored on this camera).",
    camPatch: {},
    childHeight: true,
    rules: (cam, zone, ntfy) => [
      mkRule("Someone entered the pool area", ntfy, {
        camera_id: cam, label: "person", zone_like: zone, cooldown_secs: 120,
      }),
      mkRule("Child alone near the pool", ntfy, {
        camera_id: cam, label: "child_alone", zone_like: zone, cooldown_secs: 60, pushPriority: 5,
      }),
      mkRule("Motionless in the water (experimental)", ntfy, {
        camera_id: cam, label: "still_water", zone_like: zone, cooldown_secs: 300, pushPriority: 5,
      }),
    ],
  },
  aging: {
    camPatch: { pose_detect: true, fall_detect: true },
    needsPose: true,
    rules: (cam, _zone, ntfy) => [
      mkRule("Fall detected", ntfy, {
        camera_id: cam, label: "fall", cooldown_secs: 60, pushPriority: 5,
      }),
    ],
  },
};

function ModeWizard({
  mode,
  cameras,
  poseAvailable,
  onClose,
  onDone,
}: {
  mode: Mode;
  cameras: Camera[];
  poseAvailable: boolean;
  onClose: () => void;
  onDone: () => void;
}) {
  const toast = useToast();
  const plan = WIZARDS[mode.key];
  const usable = cameras.filter((c) => c.enabled);
  const [camId, setCamId] = useState<number | "">(usable.length === 1 ? usable[0].id : "");
  const [zoneName, setZoneName] = useState(plan.zoneName ?? "");
  const [rect, setRect] = useState<Rect | null>(null);
  const [childFrac, setChildFrac] = useState(0.45);
  // docs/11 P2 — "device" = built-in Web Push, no new apps.
  const [channel, setChannel] = useState<"device" | "ntfy">(webPushSupported() ? "device" : "ntfy");
  const [pushReady, setPushReady] = useState(false);
  const [ntfy, setNtfy] = useState("");
  const [busy, setBusy] = useState(false);
  const [testBusy, setTestBusy] = useState<number | null>(null);
  const [created, setCreated] = useState<{ id: number; name: string }[] | null>(null);
  const cam = usable.find((c) => c.id === camId) ?? null;
  // Offer the push topic the owner already uses — one home, one topic.
  useEffect(() => {
    api
      .alarms()
      .then((rs) => {
        const t = rs.flatMap((r) => r.actions ?? []).find((a) => a.kind === "ntfy")?.target;
        if (t) setNtfy((cur) => cur || t);
      })
      .catch(() => {});
  }, []);

  // Ordered steps for this mode (zone / child-height are per-plan).
  const steps = [
    "camera",
    ...(plan.zoneName ? ["zone"] : []),
    ...(plan.childHeight ? ["child"] : []),
    "notify",
  ];
  const [stepIx, setStepIx] = useState(0);
  const step = created ? "done" : steps[stepIx];
  const canNext =
    step === "camera" ? camId !== "" :
    step === "zone" ? rect !== null && zoneName.trim() !== "" :
    true;

  const apply = async () => {
    if (!cam) return;
    setBusy(true);
    try {
      // 1. Camera toggles + the zone (+ child height), in one PATCH. An existing
      //    zone with the same name is reused, not duplicated.
      const dc: DetectConfig = { ...cam.detect_config, ...plan.camPatch };
      let zone: string | null = null;
      if (plan.zoneName && rect) {
        zone = zoneName.trim();
        const exists = (dc.zones ?? []).some((z) => z.name === zone);
        if (!exists) {
          dc.zones = [
            ...(dc.zones ?? []),
            {
              name: zone,
              points: [
                [rect[0], rect[1]],
                [rect[2], rect[1]],
                [rect[2], rect[3]],
                [rect[0], rect[3]],
              ],
              kind: "required",
              labels: [...(plan.zoneLabels ?? [])],
              ...plan.zoneFlags,
            } as PolyZone,
          ];
        }
      }
      if (plan.childHeight) dc.child_height_frac = childFrac;
      await api.patchCamera(cam.id, { detect_config: dc, detect: true });
      // 2. Make sure the sounds the mode listens for are actually monitored.
      if (plan.ensureAudioLabels?.length) {
        try {
          const s = await api.settings();
          const missing = plan.ensureAudioLabels.filter((l) => !s.audio_labels.includes(l));
          if (missing.length) {
            await api.saveSettings({ ...s, audio_labels: [...s.audio_labels, ...missing] });
          }
        } catch {
          // Non-admin viewers can't write settings; the default set already
          // includes these labels, so this is best-effort.
        }
      }
      // 3. The alarm rules, wired to the push topic.
      const madeRules: { id: number; name: string }[] = [];
      for (const r of plan.rules(cam.id, zone, ntfy.trim())) {
        const { id } = await api.addAlarm(r);
        madeRules.push({ id, name: r.name });
      }
      setCreated(madeRules);
      onDone();
    } catch (e) {
      toast.error(`Setup failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title={`Set up ${mode.title}`} onClose={onClose} className="modal-wide">
      <div style={{ display: "flex", flexDirection: "column", gap: 14, padding: "4px 6px 8px" }}>
        {!created && (
          <div className="muted" style={{ fontSize: "var(--text-sm)" }}>
            Step {stepIx + 1} of {steps.length}
          </div>
        )}

        {step === "camera" && (
          <>
            <p style={{ margin: 0 }}>Which camera should watch?</p>
            <select
              value={camId}
              onChange={(e) => setCamId(e.target.value === "" ? "" : Number(e.target.value))}
              aria-label="Camera to set up"
            >
              <option value="">— pick a camera —</option>
              {usable.map((c) => (
                <option key={c.id} value={c.id}>{c.name}</option>
              ))}
            </select>
            {cam && !cam.detect && (
              <p className="muted" style={{ margin: 0, fontSize: "var(--text-sm)" }}>
                Detection is off on {cam.name} — the wizard will turn it on.
              </p>
            )}
            {plan.needsPose && !poseAvailable && (
              <div className="callout callout-warn" role="status">
                <span className="callout-ico"><IconAlert size={16} /></span>
                <div>
                  The body-pose model isn't installed, so posture alerts (standing / covered face /
                  fall) stay silent until it's added under Settings → Models &amp; capabilities. You
                  can finish this setup now — it starts working the moment the model is there.
                </div>
              </div>
            )}
          </>
        )}

        {step === "zone" && cam && (
          <>
            <p style={{ margin: 0 }}>
              Draw a box over the <b>{plan.zoneThing}</b> on {cam.name}'s picture.
            </p>
            <RectZoneDraw
              cameraId={cam.id}
              cameraName={cam.name}
              label={zoneName || plan.zoneName || "Zone"}
              rect={rect}
              onRect={setRect}
            />
            <label className="field" style={{ maxWidth: 240 }}>
              zone name
              <input type="text" value={zoneName} onChange={(e) => setZoneName(e.target.value)} />
            </label>
            {plan.zoneNote && (
              <p className="muted" style={{ margin: 0, fontSize: "var(--text-sm)" }}>{plan.zoneNote}</p>
            )}
          </>
        )}

        {step === "child" && cam && (
          <>
            <p style={{ margin: 0 }}>
              So the pool alerts can tell a child from an adult, drag the marker to about how tall
              a child looks on this camera.
            </p>
            <ChildHeightEditor
              cameraId={cam.id}
              cameraName={cam.name}
              frac={childFrac}
              onChange={setChildFrac}
            />
          </>
        )}

        {step === "notify" && (
          <>
            <p style={{ margin: 0 }}>Where should these alerts go?</p>
            <div className="row" style={{ gap: 8 }}>
              <button
                type="button"
                className={`btn ${channel === "device" ? "btn-primary" : "btn-ghost"}`}
                disabled={!webPushSupported()}
                title={
                  webPushSupported()
                    ? "Built-in notifications to this browser or phone — nothing to install"
                    : "This browser doesn't support push notifications — use the ntfy app instead"
                }
                onClick={() => setChannel("device")}
              >
                This device (no new apps)
              </button>
              <button
                type="button"
                className={`btn ${channel === "ntfy" ? "btn-primary" : "btn-ghost"}`}
                onClick={() => setChannel("ntfy")}
              >
                The ntfy app
              </button>
            </div>
            {channel === "device" && (
              <div>
                {pushReady ? (
                  <span className="save-ok">
                    <IconCheck size={14} /> This device will get these alerts.
                  </span>
                ) : (
                  <button
                    type="button"
                    className="btn btn-secondary"
                    onClick={async () => {
                      try {
                        await enableWebPush();
                        setPushReady(true);
                        toast.success("Notifications enabled on this device");
                      } catch (e) {
                        toast.error(e instanceof Error ? e.message : String(e));
                      }
                    }}
                  >
                    Turn on notifications here
                  </button>
                )}
              </div>
            )}
            {channel === "ntfy" && (
              <p className="muted" style={{ margin: 0, fontSize: "var(--text-sm)" }}>
                Install the free <b>ntfy</b> app on your phone, then subscribe to this topic in
                it.
              </p>
            )}
            <div className="row" style={{ display: channel === "ntfy" ? undefined : "none" }}>
              <input
                type="text"
                placeholder="https://ntfy.sh/your-private-topic"
                value={ntfy}
                onChange={(e) => setNtfy(e.target.value)}
                style={{ flex: 1, minWidth: 260 }}
                aria-label="ntfy topic URL"
              />
              <button
                type="button"
                className="btn btn-ghost"
                onClick={() => {
                  const b = new Uint8Array(8);
                  crypto.getRandomValues(b);
                  setNtfy(`https://ntfy.sh/cammy-${[...b].map((x) => x.toString(16).padStart(2, "0")).join("")}`);
                }}
              >
                Generate a private topic
              </button>
            </div>
            {channel === "ntfy" && ntfy.trim() !== "" && (
              <button
                type="button"
                className="btn btn-secondary"
                style={{ alignSelf: "flex-start" }}
                disabled={testBusy === -1}
                onClick={async () => {
                  setTestBusy(-1);
                  try {
                    const r = await api.notifyTest("ntfy", ntfy.trim());
                    if (r.ok) toast.success("Test push sent — check the ntfy app on your phone");
                    else toast.error(`Test failed: ${r.error}`);
                  } catch (e) {
                    toast.error(String(e));
                  } finally {
                    setTestBusy(null);
                  }
                }}
              >
                {testBusy === -1 ? "Sending…" : "Send a test push"}
              </button>
            )}
            <div className="muted" style={{ fontSize: "var(--text-sm)" }}>
              Finishing will set up <b>{cam?.name}</b> and create{" "}
              {plan.rules(0, plan.zoneName ?? null, "x").length} alarm rule
              {plan.rules(0, plan.zoneName ?? null, "x").length === 1 ? "" : "s"}:{" "}
              {plan.rules(0, plan.zoneName ?? null, "x").map((r) => r.name).join(" · ")}
            </div>
          </>
        )}

        {created && (
          <>
            <p style={{ margin: 0 }}>
              <b>Done.</b> {cam?.name} is set up and these alerts are live — send a test to see
              exactly what will arrive on your phone:
            </p>
            <ul style={{ margin: 0, paddingLeft: 18 }}>
              {created.map((r) => (
                <li key={r.id} style={{ marginBottom: 6 }}>
                  {r.name}{" "}
                  <button
                    type="button"
                    className="btn btn-ghost ev-act"
                    disabled={testBusy === r.id}
                    onClick={async () => {
                      setTestBusy(r.id);
                      try {
                        const res = await api.testAlarm(r.id);
                        if (res.fired) toast.success("Test alert delivered — check your phone");
                        else toast.error("Test failed — check the rule's target on the Alarms page");
                      } catch (e) {
                        toast.error(String(e));
                      } finally {
                        setTestBusy(null);
                      }
                    }}
                  >
                    {testBusy === r.id ? "Sending…" : "Send a test alert"}
                  </button>
                </li>
              ))}
            </ul>
            <p className="muted" style={{ margin: 0, fontSize: "var(--text-sm)" }}>
              Adjust anything later on the Alarms page; the zone lives in {cam?.name}'s zone
              editor (Cameras page).
            </p>
            <div className="row" style={{ justifyContent: "flex-end" }}>
              <button type="button" className="btn btn-primary" onClick={onClose}>Close</button>
            </div>
          </>
        )}

        {!created && (
          <div className="row" style={{ justifyContent: "space-between", marginTop: 4 }}>
            <button
              type="button"
              className="btn btn-ghost"
              onClick={() => (stepIx === 0 ? onClose() : setStepIx(stepIx - 1))}
            >
              {stepIx === 0 ? "Cancel" : "← Back"}
            </button>
            {stepIx < steps.length - 1 ? (
              <button
                type="button"
                className="btn btn-primary"
                disabled={!canNext}
                onClick={() => setStepIx(stepIx + 1)}
              >
                Next →
              </button>
            ) : (
              <button
                type="button"
                className="btn btn-primary"
                disabled={busy || !canNext || (channel === "ntfy" ? ntfy.trim() === "" : !pushReady)}
                onClick={apply}
              >
                {busy ? "Setting up…" : "Finish setup"}
              </button>
            )}
          </div>
        )}
      </div>
    </Modal>
  );
}

/// Best-effort "is this mode wired up anywhere?" from the per-camera detect
/// config the page already has. Deliberately coarse — a check means "the enabling
/// toggle is on somewhere", NOT "fully configured for this room" (zones/alarms
/// can't be inferred cheaply). Drives the status badge so a set-up mode looks
/// different from an untouched one.
/** True when a pose-dependent mode has the pose toggle on but the model is absent. */
function poseGap(mode: Mode, cams: Camera[], poseAvailable: boolean): boolean {
  return (
    (mode.key === "baby" || mode.key === "aging") &&
    !poseAvailable &&
    cams.some((c) => !!c.detect_config.pose_detect)
  );
}

function modeStatus(mode: Mode, cams: Camera[], poseAvailable: boolean): "active" | "partial" | "off" {
  const any = (pick: (c: Camera) => boolean) => cams.some(pick);
  const pose = any((c) => !!c.detect_config.pose_detect);
  const audio = any((c) => !!c.detect_config.audio_detect);
  const detect = any((c) => c.enabled && c.detect);
  const fall = any((c) => !!c.detect_config.fall_detect);
  const child = any((c) => c.detect_config.child_height_frac != null);
  const tri = (n: number, total: number) => (n >= total ? "active" : n > 0 ? "partial" : "off");
  let s: "active" | "partial" | "off";
  switch (mode.key) {
    case "baby":
      s = tri([pose, audio].filter(Boolean).length, 2);
      break;
    case "pet":
      // Pet OBJECT detection (dog/cat events) works out of the box on any
      // detecting camera — that alone is "partly set up"; the bark/meow audio
      // toggle is the remaining gap, and turning it on completes the mode.
      s = audio ? "active" : detect ? "partial" : "off";
      break;
    case "pool":
      s = child ? "active" : "off";
      break;
    case "aging":
      s = tri([pose, fall].filter(Boolean).length, 2);
      break;
    default:
      s = "off";
  }
  // Don't claim a pose-dependent mode is fully "On" when the pose MODEL isn't
  // installed — the toggle is on but the worker silently no-ops until the file exists.
  if (s === "active" && poseGap(mode, cams, poseAvailable)) s = "partial";
  return s;
}

const STATUS_BADGE: Record<"active" | "partial" | "off", { cls: string; text: string }> = {
  active: { cls: "badge ok", text: "On" },
  partial: { cls: "badge warn", text: "Partly set up" },
  off: { cls: "badge", text: "Not set up" },
};

// Modes where jumping straight to the live view is useful (watch the crib, pool
// deck, or a room). Pets is about zones/audio, so it doesn't get a Live shortcut.
const LIVE_MODES = ["baby", "pool", "aging"];

function ModeCard({
  mode,
  cameras,
  events,
  loaded,
  loadError,
  poseAvailable,
  onGo,
  onSetup,
}: {
  mode: Mode;
  cameras: Camera[];
  events: CamEvent[];
  loaded: boolean;
  loadError: string | null;
  poseAvailable: boolean;
  onGo?: (p: GoPage) => void;
  onSetup?: () => void;
}) {
  const recent = useMemo(
    () => events.filter((e) => mode.labels.includes(e.label)).slice(0, 4),
    [events, mode.labels]
  );
  const badge = STATUS_BADGE[modeStatus(mode, cameras, poseAvailable)];
  const showPoseGap = poseGap(mode, cameras, poseAvailable);
  return (
    <div className="card" style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <h2 style={{ margin: 0 }}>{mode.title}</h2>
        <span className={badge.cls} style={{ marginLeft: "auto" }}>{badge.text}</span>
      </div>
      <p className="muted" style={{ margin: 0 }}>{mode.blurb}</p>
      {showPoseGap && (
        <div className="callout callout-warn" role="status">
          <span className="callout-ico"><IconAlert size={16} /></span>
          <div>
            Body pose monitoring is on, but the pose model isn’t installed. Posture and fall alerts
            stay off until the pose model is added (Settings, Models &amp; capabilities).
          </div>
        </div>
      )}

      <div>
        <div className="muted" style={{ fontSize: "var(--text-xs)", marginBottom: 4 }}>Watches for</div>
        <div className="row" style={{ flexWrap: "wrap", gap: 6 }}>
          {mode.watches.map((w) => (
            <span key={w} className="pill">{w}</span>
          ))}
        </div>
      </div>

      <div>
        {onSetup && (
          <button className="btn btn-primary" style={{ marginBottom: 8 }} onClick={onSetup}>
            Set up now
          </button>
        )}
        <details className="adv">
          <summary>Manual steps (do it yourself)</summary>
          <ol style={{ margin: "8px 0 0", paddingLeft: 18, fontSize: "var(--text-sm)", lineHeight: 1.5 }}>
            {mode.setup.map((s, i) => (
              <li key={i}>{s}</li>
            ))}
          </ol>
        </details>
        {onGo && (
          <div className="row" style={{ gap: 6, flexWrap: "wrap", marginTop: 8 }}>
            {LIVE_MODES.includes(mode.key) && (
              <button className="btn btn-ghost ev-act" onClick={() => onGo("Live")}>Open Live →</button>
            )}
            <button className="btn btn-ghost ev-act" onClick={() => onGo("Cameras")}>Open Cameras →</button>
            <button className="btn btn-ghost ev-act" onClick={() => onGo("Alarms")}>Open Alarms →</button>
            <button className="btn btn-ghost ev-act" onClick={() => onGo("Settings")}>Open Settings →</button>
          </div>
        )}
      </div>

      <div>
        <div className="muted" style={{ fontSize: "var(--text-xs)", marginBottom: 4 }}>Recent activity</div>
        {loadError ? (
          <span className="muted" style={{ fontSize: "var(--text-sm)" }}>Couldn’t load recent activity.</span>
        ) : !loaded ? (
          <span className="skeleton" style={{ height: 18, width: "70%" }} />
        ) : recent.length === 0 ? (
          <span className="muted" style={{ fontSize: "var(--text-sm)" }}>Nothing yet.</span>
        ) : (
          <ul style={{ margin: 0, paddingLeft: 18, fontSize: "var(--text-sm)" }}>
            {recent.map((e) => (
              <li key={e.id}>
                <b>{prettyLabel(e.label)}</b> on {e.camera}
                {e.zone ? ` · ${e.zone}` : ""} · {fmtTime(e.ts)}
              </li>
            ))}
          </ul>
        )}
      </div>

      {mode.safety && (
        <p
          className="muted"
          style={{ fontSize: "var(--text-xs)", margin: 0, marginTop: "auto", borderTop: "1px solid var(--border)", paddingTop: 8 }}
        >
          <IconInfo size={12} /> {mode.safety}
        </p>
      )}
    </div>
  );
}

export default function Family({ cameras, onGo }: { cameras: Camera[]; onGo?: (p: GoPage) => void }) {
  const [events, setEvents] = useState<CamEvent[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [poseAvailable, setPoseAvailable] = useState(true); // assume present until told otherwise
  // P2.5 wizard: which mode is being set up, and a fresh camera list after a
  // wizard applied changes (the prop is the parent's snapshot).
  const [wizMode, setWizMode] = useState<Mode | null>(null);
  const [freshCams, setFreshCams] = useState<Camera[] | null>(null);
  const cams = freshCams ?? cameras;
  useEffect(() => {
    api
      .events({ limit: 300 })
      .then((d) => { setEvents(d); setLoadError(null); })
      .catch((e) => setLoadError(String(e)))
      .finally(() => setLoaded(true));
    api
      .capabilities()
      .then((r) => setPoseAvailable(capabilityUsable(r.features.find((f) => f.key === "pose"))))
      .catch(() => {});
  }, []);

  return (
    <div>
      <h1>Family</h1>
      <p className="muted" style={{ marginTop: 0 }}>
        Guided “modes” for the home — baby, pets, pool and aging-in-place. Each one is a recipe over
        the camera, zone, sound and alarm settings you already have; follow the steps to set it up.
      </p>
      <div className="callout callout-warn" role="note">
        <span className="callout-ico"><IconAlert size={16} /></span>
        <div>
          <b>Please read:</b>{" "}
          These are <b>assistive aids, not safety devices</b>. They are best-effort, can miss events,
          and are not medical, breathing/SIDS, or drowning detection. Never rely on them in place of
          supervision, a fence, safe-sleep practices, or a medical-alert pendant.
        </div>
      </div>
      {cams.length === 0 && (
        <p className="muted">Add a camera first (Cameras page), then come back to set up a mode.</p>
      )}
      <div style={{ display: "grid", gap: 12, gridTemplateColumns: "repeat(auto-fit, minmax(320px, 1fr))" }}>
        {MODES.map((m) => (
          <ModeCard
            key={m.key}
            mode={m}
            cameras={cams}
            events={events}
            loaded={loaded}
            loadError={loadError}
            poseAvailable={poseAvailable}
            onGo={onGo}
            onSetup={cams.length > 0 ? () => setWizMode(m) : undefined}
          />
        ))}
      </div>
      {wizMode && (
        <ModeWizard
          mode={wizMode}
          cameras={cams}
          poseAvailable={poseAvailable}
          onClose={() => setWizMode(null)}
          onDone={() => {
            api.cameras().then(setFreshCams).catch(() => {});
          }}
        />
      )}
    </div>
  );
}
