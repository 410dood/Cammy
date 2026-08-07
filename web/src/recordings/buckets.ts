// How the clip table folds, and the time maths behind it. Lifted verbatim out
// of `pages/Recordings.tsx` so Find can fold footage the same way Recordings
// does — two surfaces disagreeing about where an hour starts would be a quiet
// way to make the same footage look like two different things.

import { Segment } from "../api";

export const GROUPINGS = [
  { label: "15 min", secs: 900 },
  { label: "hour", secs: 3600 },
  { label: "3 hours", secs: 3 * 3600 },
  { label: "day", secs: 86400 },
  { label: "no grouping", secs: 0 },
];
export const GROUP_KEY = "cammy-rec-group";

export type HourGroup = {
  key: string;
  camera: string;
  cameraId: number;
  hourTs: number;
  segs: Segment[];
  bytes: number;
  /// Detections inside this bucket, most common first — what turns a row of
  /// near-identical "47 clips" into something you can triage. `null` means the
  /// bucket falls outside the event window we actually fetched, so we say
  /// NOTHING rather than render a "0" that would claim the hour was quiet.
  counts: [string, number][] | null;
};

// Buckets anchor at local midnight, so "3 hours" and "day" line up with the
// user's clock instead of UTC epoch boundaries.
export function bucketOf(ts: number, secs: number): number {
  const dayStart = new Date(ts * 1000).setHours(0, 0, 0, 0) / 1000;
  return dayStart + Math.floor((ts - dayStart) / secs) * secs;
}

export function groupLabel(ts: number, secs: number): string {
  if (secs >= 86400)
    return new Date(ts * 1000).toLocaleDateString([], { weekday: "short", month: "numeric", day: "numeric" });
  const minute = secs < 3600 ? ("2-digit" as const) : undefined;
  const from = new Date(ts * 1000).toLocaleString([], { month: "numeric", day: "numeric", hour: "numeric", minute });
  const to = new Date((ts + secs) * 1000).toLocaleTimeString([], { hour: "numeric", minute });
  return `${from} – ${to}`;
}

export const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e));
