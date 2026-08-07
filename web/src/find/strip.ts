// The film strip: a window of time as one ordered, scannable list.
//
// This is the part of Find that Recordings and Events each only half have.
// Events knows WHAT happened and has no time axis. Recordings knows WHEN and
// its rows are content-blind — "11 AM-12 PM · 47 clips" reads the same whether
// that hour held a burglary or an empty driveway. A timeline lane fixes neither:
// ticks on a lane still make you hover to learn anything.
//
// So the strip interleaves detections with the QUIET STRETCHES BETWEEN THEM,
// and gives the quiet stretches real keyframes. Finding a clip is mostly
// scanning quiet time, and this is the only view here that lets you.
//
// The imports below carry explicit `.ts` extensions. That is deliberate: this
// module is pure, and the extensions are what let `node --test` load it with no
// bundler and no test framework, which is how strip.test.ts runs.
import type { CamEvent, Segment } from "../api";
import type { Block } from "../coverage";
import { coalesce, complement, unionBlocks } from "../coverage.ts";
import { groupEvents } from "../eventGroups.ts";

export type StripItem =
  /** A detection, or a run of near-identical ones collapsed into one tile. */
  | { kind: "event"; ts: number; ev: CamEvent; count: number; startTs: number; endTs: number }
  /** Recorded footage with nothing detected in it. `segId` is a real segment
   *  inside the span, so the tile can show what that stretch actually looked
   *  like rather than asserting it was uneventful. */
  | {
      kind: "footage";
      from: number;
      to: number;
      cameraId: number;
      camera: string;
      segId: number;
      cameras: number;
    }
  /** Time we KNOW held no footage: nothing was recording, or retention has
   *  since deleted it. */
  | { kind: "gap"; from: number; to: number }
  /** Time we did NOT ASK ABOUT. Distinct from `gap` on purpose — see below. */
  | { kind: "unknown"; from: number; to: number }
  /** A wall-clock heading between items. */
  | { kind: "marker"; ts: number; label: string };

export interface StripOptions {
  /** The window, `[from, to)`. */
  from: number;
  to: number;
  /** Segment length, for coalescing coverage. */
  segmentSecs: number;
  /** Split a long quiet stretch into chunks this size so there is more than one
   *  frame to scan. */
  quietChunkSecs?: number;
  /** Quiet stretches shorter than this aren't worth a tile of their own. */
  minQuietSecs?: number;
  /** Oldest instant the SEGMENT fetch actually reached.
   *
   *  This is the load-bearing one. `/api/recordings` bounds only the top and
   *  caps at 1000 rows, and 1000 rows is about two hours of five-camera
   *  footage (measured on this NVR: one full page spanned 06:39-08:39). So a
   *  day window cannot be covered by one request, and everything older than
   *  the oldest row we got back is UNKNOWN, not empty. Calling it a gap would
   *  tell someone that 22 hours of a fully-recorded day held no video. */
  coverageKnownFrom?: number;
}

const HOUR = 3600;

const hourLabel = (ts: number) =>
  new Date(ts * 1000).toLocaleTimeString([], { hour: "numeric" });

/** Local-hour bucket, so headings line up with the viewer's clock. */
function hourOf(ts: number): number {
  const d = new Date(ts * 1000);
  d.setMinutes(0, 0, 0);
  return Math.floor(d.getTime() / 1000);
}

/** Build the ordered strip for a window, newest first (the order every other
 *  list in this app uses).
 *
 *  Pure: same inputs, same output, no clock, no network. */
export function buildStrip(
  events: CamEvent[],
  segments: Segment[],
  opts: StripOptions
): StripItem[] {
  const { from, to, segmentSecs } = opts;
  const quietChunk = opts.quietChunkSecs ?? 1800;
  const minQuiet = opts.minQuietSecs ?? 90;
  if (!(to > from)) return [];

  // Coverage, per camera and merged. Per-camera is what gives a quiet tile a
  // real segment to show; merged is what answers "was ANYTHING recording".
  const byCamera = new Map<number, { camera: string; blocks: Block[]; segs: Segment[] }>();
  for (const s of segments) {
    if (s.stream && s.stream !== "main") continue; // the sub-stream is the same footage twice
    const e = byCamera.get(s.camera_id);
    if (e) e.segs.push(s);
    else byCamera.set(s.camera_id, { camera: s.camera, blocks: [], segs: [s] });
  }
  const allBlocks: Block[] = [];
  for (const e of byCamera.values()) {
    e.blocks = coalesce(e.segs, segmentSecs);
    allBlocks.push(...e.blocks);
  }
  const covered = unionBlocks(allBlocks);

  // Below this instant we simply did not ask, so we cannot claim anything.
  const knownFrom = Math.max(from, opts.coverageKnownFrom ?? from);

  const inWindow = events
    .filter((e) => e.ts >= from && e.ts < to)
    .sort((a, b) => b.ts - a.ts);
  const clusters = groupEvents(inWindow); // expects newest-first, returns newest-first

  /** How many cameras had footage overlapping `[a, b)`, and the best segment to
   *  show for it (the one that starts closest to `a`). */
  const quietDetail = (a: number, b: number) => {
    let cams = 0;
    let best: { seg: Segment; camera: string } | null = null;
    for (const e of byCamera.values()) {
      const overlaps = e.blocks.some((bl) => bl.start < b && bl.end > a);
      if (!overlaps) continue;
      cams++;
      for (const s of e.segs) {
        if (s.start_ts + segmentSecs <= a || s.start_ts >= b) continue;
        if (!best || Math.abs(s.start_ts - a) < Math.abs(best.seg.start_ts - a)) {
          best = { seg: s, camera: e.camera };
        }
      }
    }
    return { cams, best };
  };

  /** Describe an event-free stretch: what was recorded, what wasn't, and what
   *  we never looked at. Emitted oldest-first, then reversed by the caller. */
  const describeQuiet = (a: number, b: number): StripItem[] => {
    if (b - a < minQuiet) return [];
    const out: StripItem[] = [];
    // Anything older than the segment horizon is unasked-about, full stop.
    if (a < knownFrom) {
      const end = Math.min(b, knownFrom);
      out.push({ kind: "unknown", from: a, to: end });
      a = end;
      if (b - a < minQuiet) return out;
    }
    for (const hole of complement(covered, a, b)) {
      if (hole.end - hole.start >= minQuiet) out.push({ kind: "gap", from: hole.start, to: hole.end });
    }
    for (const block of covered) {
      const start = Math.max(block.start, a);
      const end = Math.min(block.end, b);
      if (end - start < minQuiet) continue;
      for (let c = start; c < end; c += quietChunk) {
        const cEnd = Math.min(c + quietChunk, end);
        if (cEnd - c < minQuiet && c !== start) break;
        const { cams, best } = quietDetail(c, cEnd);
        if (!best) continue; // union said covered but no segment survived the filter
        out.push({
          kind: "footage",
          from: c,
          to: cEnd,
          cameraId: best.seg.camera_id,
          camera: best.camera,
          segId: best.seg.id,
          cameras: cams,
        });
      }
    }
    return out.sort((x, y) => tsOf(x) - tsOf(y));
  };

  // Walk newest -> oldest, describing the quiet stretch above each cluster.
  const items: StripItem[] = [];
  let cursor = to;
  for (const c of clusters) {
    items.push(...describeQuiet(c.endTs, cursor).reverse());
    items.push({
      kind: "event",
      ts: c.rep.ts,
      ev: c.rep,
      count: c.count,
      startTs: c.startTs,
      endTs: c.endTs,
    });
    cursor = Math.min(cursor, c.startTs);
  }
  items.push(...describeQuiet(from, cursor).reverse());

  // Wall-clock headings, inserted where the hour actually turns over.
  const out: StripItem[] = [];
  let lastHour: number | null = null;
  for (const it of items) {
    const h = hourOf(tsOf(it));
    if (h !== lastHour) {
      out.push({ kind: "marker", ts: h, label: hourLabel(h) });
      lastHour = h;
    }
    out.push(it);
  }
  return out;
}

/** The instant an item is filed under (its newest edge, matching the sort). */
export function tsOf(it: StripItem): number {
  switch (it.kind) {
    case "event":
      return it.ts;
    case "marker":
      return it.ts;
    default:
      return it.to;
  }
}

/** Whole hours, for "quiet · 2:10–4:35" style captions. */
export function fmtSpanSecs(secs: number): string {
  if (secs < 90) return `${Math.round(secs)}s`;
  if (secs < HOUR) return `${Math.round(secs / 60)} min`;
  const h = Math.floor(secs / HOUR);
  const m = Math.round((secs % HOUR) / 60);
  return m ? `${h}h ${m}m` : `${h}h`;
}
