// Runs on plain `node --test` — Node strips the types, so there is no test
// framework and no new dependency. `npm test` from `web/`.
//
// buildStrip is the only piece of the Phase 3 plan that is testable in
// isolation, so it is worth testing properly: every case below was checked to
// FAIL against a deliberately broken buildStrip before being kept.

import { test } from "node:test";
import assert from "node:assert/strict";
import { buildStrip, fmtSpanSecs, tsOf } from "./strip.ts";
import type { StripItem } from "./strip";
import type { CamEvent, Segment } from "../api";

const DAY0 = 1783656000; // 2026-07-10 00:00 local, a real day on the test NVR
const ev = (id: number, ts: number, label = "person", cameraId = 3): CamEvent =>
  ({
    id,
    camera_id: cameraId,
    camera: `cam${cameraId}`,
    ts,
    label,
    score: 0.9,
    box: [0, 0, 1, 1],
    snapshot: `s${id}.jpg`,
    face: null,
    plate: null,
    gesture: null,
    zone: null,
    caption: null,
    transcript: null,
    flagged: false,
    note: null,
  }) as CamEvent;

const seg = (id: number, startTs: number, cameraId = 3): Segment => ({
  id,
  camera_id: cameraId,
  camera: `cam${cameraId}`,
  start_ts: startTs,
  bytes: 1000,
  path: `/${id}.mp4`,
  stream: "main",
});

/** A run of back-to-back 60s segments starting at `from`. */
const segRun = (idBase: number, from: number, count: number, cameraId = 3): Segment[] =>
  Array.from({ length: count }, (_, i) => seg(idBase + i, from + i * 60, cameraId));

const base = { from: DAY0, to: DAY0 + 86400, segmentSecs: 60 };
const kinds = (items: StripItem[]) => items.filter((i) => i.kind !== "marker").map((i) => i.kind);

test("orders newest first, like every other list in the app", () => {
  const items = buildStrip([ev(1, DAY0 + 100), ev(2, DAY0 + 5000)], [], base);
  const evs = items.filter((i) => i.kind === "event");
  assert.equal(evs.length, 2);
  assert.ok(tsOf(evs[0]) > tsOf(evs[1]), "first event must be the newer one");
});

test("collapses a run of near-identical detections into one tile with a count", () => {
  // Five 'person' hits 30s apart on one camera: one activity, not five tiles.
  const evs = [0, 30, 60, 90, 120].map((d, i) => ev(i + 1, DAY0 + 3600 + d));
  const items = buildStrip(evs, [], base);
  const tiles = items.filter((i) => i.kind === "event");
  assert.equal(tiles.length, 1, "a burst is one tile");
  assert.equal(tiles[0].kind === "event" && tiles[0].count, 5);
});

test("a different label on the same camera is its own tile", () => {
  const items = buildStrip([ev(1, DAY0 + 3600, "person"), ev(2, DAY0 + 3630, "car")], [], base);
  assert.equal(items.filter((i) => i.kind === "event").length, 2);
});

// --- the honesty cases -----------------------------------------------------
// These are the reason this function exists as its own testable unit.

test("time we DIDN'T ASK ABOUT is 'unknown', never 'gap'", () => {
  // One hour of footage at the end of the day, and we only fetched back to
  // then. The other 23 hours are unasked-about. Calling them a gap would tell
  // someone a fully-recorded day held no video -- the exact lie that
  // /api/recordings' 1000-row cap sets up, since 1000 rows is ~2 hours of
  // five-camera footage.
  const covFrom = DAY0 + 82800;
  const items = buildStrip([], segRun(1, covFrom, 60), {
    ...base,
    coverageKnownFrom: covFrom,
  });
  const k = kinds(items);
  assert.ok(k.includes("unknown"), "must report the unfetched stretch as unknown");
  assert.ok(!k.includes("gap"), `must NOT claim a gap it never looked for: ${k.join(",")}`);
  const unk = items.find((i) => i.kind === "unknown");
  assert.ok(unk);
  assert.equal(unk.kind === "unknown" && unk.from, DAY0);
  assert.equal(unk.kind === "unknown" && unk.to, covFrom);
});

test("time we DID ask about and found empty is a real 'gap'", () => {
  // Same shape, but the fetch reached the start of the window: now the silence
  // is knowledge, and saying nothing recorded is true.
  const covFrom = DAY0 + 82800;
  const items = buildStrip([], segRun(1, covFrom, 60), {
    ...base,
    coverageKnownFrom: DAY0,
  });
  const k = kinds(items);
  assert.ok(k.includes("gap"), "an asked-about empty stretch is a gap");
  assert.ok(!k.includes("unknown"));
});

test("a quiet tile's frame comes from INSIDE the stretch it stands for", () => {
  // segment_seconds is a setting, and coalesce merges across gaps of up to
  // 1.5x it. At 600s that means a single coverage block can contain a real
  // 800s hole. A tile whose stretch starts in that hole has no segment nearby
  // INSIDE it, so picking merely the closest segment reaches back to the one
  // before the stretch — and captions a picture of 10 minutes you are not
  // looking at as if it were the stretch you are.
  const T = DAY0 + 3600;
  const SEG = 600;
  const segs = [seg(1, T - SEG), seg(100, T + 800)];
  const items = buildStrip([], segs, {
    from: T,
    to: T + 1400,
    segmentSecs: SEG,
    coverageKnownFrom: T,
    quietChunkSecs: 3600,
  });
  const foot = items.filter((i) => i.kind === "footage");
  assert.ok(foot.length > 0, "expected at least one quiet tile");
  const byId = new Map(segs.map((s) => [s.id, s]));
  for (const f of foot) {
    if (f.kind !== "footage") continue;
    const s = byId.get(f.segId);
    assert.ok(s, `segId ${f.segId} must be one we supplied`);
    assert.ok(
      s!.start_ts + SEG > f.from && s!.start_ts < f.to,
      `frame from the segment at ${s!.start_ts} lies outside the span ${f.from}..${f.to}`
    );
  }
});

test("counts how many cameras were recording through a quiet stretch", () => {
  const segs = [...segRun(1, DAY0 + 3600, 60, 3), ...segRun(1000, DAY0 + 3600, 60, 4)];
  const items = buildStrip([], segs, { ...base, coverageKnownFrom: DAY0 });
  const foot = items.find((i) => i.kind === "footage");
  assert.ok(foot);
  assert.equal(foot.kind === "footage" && foot.cameras, 2);
});

test("splits a long quiet stretch so there is more than one frame to scan", () => {
  // Four hours of continuous footage, nothing detected. One tile for four hours
  // would be a worse Recordings row, not a better one.
  const items = buildStrip([], segRun(1, DAY0 + 3600, 240), {
    ...base,
    coverageKnownFrom: DAY0,
    quietChunkSecs: 1800,
  });
  const foot = items.filter((i) => i.kind === "footage");
  assert.ok(foot.length >= 7, `4h at 30min chunks should be ~8 tiles, got ${foot.length}`);
  const ids = new Set(foot.map((f) => (f.kind === "footage" ? f.segId : 0)));
  assert.equal(ids.size, foot.length, "each chunk must show a DIFFERENT frame");
});

test("quiet stretches sit between the events that bound them", () => {
  const evs = [ev(1, DAY0 + 3600), ev(2, DAY0 + 20000, "car")];
  const items = buildStrip(evs, segRun(1, DAY0, 400), { ...base, coverageKnownFrom: DAY0 });
  const seq = items.filter((i) => i.kind !== "marker");
  // Descending by time, throughout — an out-of-order tile would send you to the
  // wrong part of the day.
  for (let i = 1; i < seq.length; i++) {
    assert.ok(tsOf(seq[i - 1]) >= tsOf(seq[i]), `item ${i} out of order`);
  }
  const firstEvent = seq.findIndex((i) => i.kind === "event");
  const lastEvent = seq.map((i) => i.kind).lastIndexOf("event");
  assert.ok(seq.slice(firstEvent, lastEvent).some((i) => i.kind === "footage"),
    "the stretch between two detections must be described");
});

test("a quiet tile never shows the low-res sub-stream copy", () => {
  // P3.7 dual recording writes a 'sub' segment beside every 'main' one at the
  // same instant. Both would satisfy "closest to the chunk start", so without
  // the filter the tile can end up showing the low-res copy of footage the
  // player will then open in HD.
  const main = segRun(1, DAY0 + 3600, 60, 3);
  const sub = segRun(9000, DAY0 + 3600, 60, 3).map((s) => ({ ...s, stream: "sub" }));
  const items = buildStrip([], [...sub, ...main], { ...base, coverageKnownFrom: DAY0 });
  const foot = items.filter((i) => i.kind === "footage");
  assert.ok(foot.length > 0);
  for (const f of foot) {
    assert.ok(f.kind === "footage" && f.segId < 9000, `segId ${f.kind === "footage" && f.segId} is a sub-stream row`);
  }
  // ...and the duplicate rows must not change the shape of the window either.
  assert.deepEqual(kinds(items), kinds(buildStrip([], main, { ...base, coverageKnownFrom: DAY0 })));
});

test("events outside the window are not in the strip", () => {
  const items = buildStrip([ev(1, DAY0 - 10), ev(2, DAY0 + 86400 + 10), ev(3, DAY0 + 5)], [], base);
  const evs = items.filter((i) => i.kind === "event");
  assert.equal(evs.length, 1);
  assert.equal(evs[0].kind === "event" && evs[0].ev.id, 3);
});

test("an inverted or empty window yields nothing rather than nonsense", () => {
  assert.deepEqual(buildStrip([ev(1, DAY0)], [], { ...base, to: DAY0 }), []);
  assert.deepEqual(buildStrip([ev(1, DAY0)], [], { ...base, to: DAY0 - 100 }), []);
});

test("hour headings appear once per hour, in order, never back-to-back", () => {
  const evs = [ev(1, DAY0 + 3600), ev(2, DAY0 + 7300, "car"), ev(3, DAY0 + 11000, "truck")];
  const items = buildStrip(evs, [], base);
  const marks = items.filter((i) => i.kind === "marker");
  assert.ok(marks.length >= 3);
  // ONE heading per hour. Asserting only "no two headings in a row" is not
  // enough -- a heading before every single item satisfies that and is exactly
  // the wrong behaviour.
  const ts = marks.map(tsOf);
  assert.equal(new Set(ts).size, ts.length, "the same hour must not head the list twice");
  assert.ok(marks.length < items.length / 2, "a heading before every item is not a heading");
  for (let i = 0; i < items.length - 1; i++) {
    assert.ok(!(items[i].kind === "marker" && items[i + 1].kind === "marker"), "empty heading");
  }
  assert.deepEqual([...ts].sort((a, b) => b - a), ts, "headings must descend with the list");
});

test("fmtSpanSecs reads like a person wrote it", () => {
  assert.equal(fmtSpanSecs(45), "45s");
  assert.equal(fmtSpanSecs(600), "10 min");
  assert.equal(fmtSpanSecs(3600), "1h");
  assert.equal(fmtSpanSecs(9000), "2h 30m");
});
