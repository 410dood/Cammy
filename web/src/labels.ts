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
};

export const prettyLabel = (l: string) => PRETTY[l] ?? l.replace(/_/g, " ");

// Hand-signal tokens → readable names (Signals overlay, alarm builder, event
// chips/filters). The value stays the raw token; only rendering changes.
const GESTURE_PRETTY: Record<string, string> = {
  open_palm: "Open palm",
  fist: "Fist",
  victory: "Victory",
  point: "Pointing",
  thumb_up: "Thumb up",
  thumb_down: "Thumb down",
  love: "I-love-you",
  call_me: "Call me",
  ok: "OK",
  hand: "Hand",
};
export const prettyGesture = (g: string) => GESTURE_PRETTY[g] ?? g.replace(/_/g, " ");

// Camera-side (ONVIF-ingested) events carry a synthetic 1.0 confidence — a
// "100%" badge on every one of them is noise, so score displays skip them.
export const isCameraSide = (l: string) => l.startsWith("camera_");

// Camera-side events reuse the zone field for the ONVIF rule topic
// ("RuleEngine/CellMotionDetector/Motion"). Show the leaf; callers should put
// the full topic in a title attribute.
export const prettyZone = (z: string) =>
  z.includes("/") ? (z.split("/").filter(Boolean).pop() ?? z) : z;
