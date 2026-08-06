// Display helpers for machine event labels. Event labels are stored as raw
// tokens ("camera_tripwire", "still_water") that alarm rules and the API match
// on verbatim — only the *rendering* is prettified here, never the value.

// Wording overrides where a plain underscore→space swap reads wrong. Display
// sites keep their own capitalization (most apply `text-transform: capitalize`).
const PRETTY: Record<string, string> = {
  crossing: "line crossing",
  loiter: "loitering",
  occupancy: "occupancy limit",
  still_water: "motionless in water",
  zone_open: "zone opened",
  zone_closed: "zone closed",
};

export const prettyLabel = (l: string) => PRETTY[l] ?? l.replace(/_/g, " ");

// Hide an AI caption that argues with the detection ("No cat detected, just a
// swimming pool…") — a card contradicting itself erodes trust more than a
// missing caption does. Cheap heuristic: the caption denies the event label.
export function captionContradicts(ev: { label: string; caption: string | null }): boolean {
  const c = (ev.caption ?? "").toLowerCase();
  const l = ev.label.toLowerCase();
  return c.includes(`no ${l}`) || c.includes(`not a ${l}`);
}

// Hand-signal tokens → readable names (Signals overlay, alarm builder, event
// chips/filters). The value stays the raw token; only rendering changes.
const GESTURE_PRETTY: Record<string, string> = {
  open_palm: "Open palm",
  fist: "Fist",
  victory: "Victory",
  point: "Pointing",
  thumb_up: "Thumb up",
  thumb_down: "Thumb down",
  love: "I-love-you sign",
  call_me: "Call me",
  ok: "OK",
  hand: "Hand",
};
export const prettyGesture = (g: string) => GESTURE_PRETTY[g] ?? g.replace(/_/g, " ");

// --- recording state -------------------------------------------------------

/** Why a camera is or isn't writing footage right now.
 *  - "rec"    — recording.
 *  - "paused" — deliberately parked by its recording schedule (#67).
 *  - "fault"  — it is online and set to record, but ISN'T. Footage is being lost.
 *  - null     — nothing meaningful to say (offline, or recording turned off).
 *
 *  Splitting "paused" from "fault" is the whole point: they used to render
 *  identically (no REC chip at all), so a healthy scheduled pause and a dead
 *  recorder were indistinguishable at a glance. */
export type RecordState = "rec" | "paused" | "fault" | null;

export function recordState(
  cam: { enabled: boolean; record: boolean },
  st: { online: boolean; recording: boolean; record_paused?: boolean } | undefined,
): RecordState {
  if (!st || !cam.enabled || !cam.record) return null;
  if (st.recording) return "rec";
  if (st.record_paused) return "paused";
  // Only claim a fault once the stream itself is up: an offline camera is
  // already reported as offline, and stacking "not recording" on top of that is
  // noise rather than news.
  return st.online ? "fault" : null;
}

/** Tooltip explaining a non-recording state in the owner's terms. */
export function recordStateHint(state: RecordState, scheduleWindow?: string | null): string {
  if (state === "paused")
    return scheduleWindow
      ? `Not recording right now — this camera's recording schedule (${scheduleWindow}) has it paused. Detection and event clips still run.`
      : "Not recording right now — this camera's recording schedule has it paused. Detection and event clips still run.";
  if (state === "fault")
    return "This camera is online and set to record, but no footage is being saved. The recorder keeps retrying; check Recordings and the camera's stream.";
  return "";
}

/** Compact "08:00–18:00" window from a recording schedule, for the hints above. */
export function scheduleWindow(
  s: { start_hhmm?: string | null; end_hhmm?: string | null } | null | undefined,
): string | null {
  if (!s) return null;
  const { start_hhmm: a, end_hhmm: b } = s;
  if (a && b) return `${a}–${b}`;
  if (a) return `from ${a}`;
  if (b) return `until ${b}`;
  return null;
}

// Camera-side (ONVIF-ingested) events carry a synthetic 1.0 confidence — a
// "100%" badge on every one of them is noise, so score displays skip them.
export const isCameraSide = (l: string) => l.startsWith("camera_");

// Camera-side events reuse the zone field for the ONVIF rule topic
// ("RuleEngine/CellMotionDetector/Motion"). Show the leaf; callers should put
// the full topic in a title attribute.
export const prettyZone = (z: string) =>
  z.includes("/") ? (z.split("/").filter(Boolean).pop() ?? z) : z;
