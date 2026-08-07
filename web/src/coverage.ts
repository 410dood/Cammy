// Recording coverage as time maths, with no React attached.
//
// `coalesce` used to live inside `CrossTimeline.tsx`. It moved here so the film
// strip can share it: the strip and the timeline lanes must agree about where
// footage runs, or the same recording would be described two different ways on
// one screen. Keeping it in a component file also made it untestable outside a
// browser, which is the whole reason `find/strip.ts` needs it here.

import type { Segment } from "./api";

export interface Block {
  start: number;
  end: number;
}

export function coalesce(segs: Segment[], segmentSecs: number): Block[] {
  const sorted = [...segs].sort((a, b) => a.start_ts - b.start_ts);
  const blocks: Block[] = [];
  for (const s of sorted) {
    const end = s.start_ts + segmentSecs;
    const last = blocks[blocks.length - 1];
    if (last && s.start_ts - last.end <= segmentSecs * 1.5) {
      last.end = Math.max(last.end, end);
    } else {
      blocks.push({ start: s.start_ts, end });
    }
  }
  return blocks;
}

/** Merge overlapping blocks from several cameras into one "something was
 *  recording here" set. Used to answer whether a quiet stretch was covered at
 *  all — a question no single camera's lane can answer on its own. */
export function unionBlocks(blocks: Block[]): Block[] {
  const sorted = [...blocks].sort((a, b) => a.start - b.start);
  const out: Block[] = [];
  for (const b of sorted) {
    const last = out[out.length - 1];
    if (last && b.start <= last.end) last.end = Math.max(last.end, b.end);
    else out.push({ start: b.start, end: b.end });
  }
  return out;
}

/** The parts of `[from, to)` NOT covered by `blocks`, in ascending order. */
export function complement(blocks: Block[], from: number, to: number): Block[] {
  const out: Block[] = [];
  let cursor = from;
  for (const b of unionBlocks(blocks)) {
    if (b.end <= from || b.start >= to) continue;
    if (b.start > cursor) out.push({ start: cursor, end: Math.min(b.start, to) });
    cursor = Math.max(cursor, b.end);
    if (cursor >= to) break;
  }
  if (cursor < to) out.push({ start: cursor, end: to });
  return out;
}
