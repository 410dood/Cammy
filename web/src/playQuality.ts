/** Deferred P3.7 half — a shared, persisted playback-quality preference for
 *  dual-stream cameras: "hd" plays the full-res main recording, "sd" the
 *  opt-in low-res sub copy (lighter to stream/scrub, e.g. over a phone link).
 *  Surfaces only offer the choice when the camera actually records a sub
 *  stream; a stored "sd" on a non-dual camera simply resolves to main. */

const KEY = "cammy-play-quality";

export type PlayQuality = "hd" | "sd";

export const getPlayQuality = (): PlayQuality => {
  try {
    return localStorage.getItem(KEY) === "sd" ? "sd" : "hd";
  } catch {
    return "hd";
  }
};

export const setPlayQuality = (q: PlayQuality) => {
  try {
    localStorage.setItem(KEY, q);
  } catch {
    /* private mode — the in-session state still applies */
  }
};
