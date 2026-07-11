import { useEffect, useMemo, useState } from "react";
import { api, Camera, CamEvent, fmtTime } from "../api";
import { IconInfo, IconAlert } from "../icons";
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
      "On the Alarms page add rules: “Standing (standing)” in zone “Crib”, “Covered face (covered_face)” in zone “Crib”, and a “Baby cry” sound alarm. Pick how you want to be notified.",
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
      "On the Alarms page add rules: “Child alone (child_alone)” in your pool zone (and optionally “Motionless in water (still_water)”).",
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
}: {
  mode: Mode;
  cameras: Camera[];
  events: CamEvent[];
  loaded: boolean;
  loadError: string | null;
  poseAvailable: boolean;
  onGo?: (p: GoPage) => void;
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
        <div className="muted" style={{ fontSize: "var(--text-xs)", marginBottom: 4 }}>Set it up</div>
        <ol style={{ margin: 0, paddingLeft: 18, fontSize: "var(--text-sm)", lineHeight: 1.5 }}>
          {mode.setup.map((s, i) => (
            <li key={i}>{s}</li>
          ))}
        </ol>
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
  useEffect(() => {
    api
      .events({ limit: 300 })
      .then((d) => { setEvents(d); setLoadError(null); })
      .catch((e) => setLoadError(String(e)))
      .finally(() => setLoaded(true));
    api
      .capabilities()
      .then((r) => setPoseAvailable(r.features.find((f) => f.key === "pose")?.present ?? true))
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
      {cameras.length === 0 && (
        <p className="muted">Add a camera first (Cameras page), then come back to set up a mode.</p>
      )}
      <div style={{ display: "grid", gap: 12, gridTemplateColumns: "repeat(auto-fit, minmax(320px, 1fr))" }}>
        {MODES.map((m) => (
          <ModeCard key={m.key} mode={m} cameras={cameras} events={events} loaded={loaded} loadError={loadError} poseAvailable={poseAvailable} onGo={onGo} />
        ))}
      </div>
    </div>
  );
}
