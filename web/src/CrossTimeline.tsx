// A2 — unified 24/7 cross-camera timeline. One lane per camera over a shared
// time axis: coalesced recording-coverage blocks plus class-colored event ticks.
// Clicking a lane seeks that camera's recording at that moment. Coverage is
// coalesced (not one div per 60s segment) so a full day stays light.

import { useMemo, useState } from "react";
import { CamEvent, Camera, Segment } from "./api";
import { Block, coalesce } from "./coverage";
import { prettyLabel } from "./labels";

const HOUR = 3600;
const VEHICLES = ["car", "truck", "bus", "motorcycle", "bicycle"];

/** Most positions a lane will ever draw. A lane is ~600 px wide here, so past
 *  a few hundred nodes the extras are painting on a pixel that is already
 *  occupied — measured on this NVR's 07/10: the pool3 lane put 821 ticks into
 *  112 distinct pixel columns, 38 of them on one single column. Those 38 were
 *  not merely wasted nodes; they made the lane LIE, because the tooltip you got
 *  when you hovered that column belonged to whichever tick happened to be last
 *  in DOM order and gave no hint the other 37 existed. */
const MAX_TICKS_PER_LANE = 400;

/** One drawn position on a lane: either a single event (identical to what this
 *  component always drew) or a bin standing in for several. */
export interface Tick {
  key: string;
  ts: number;
  cls: string;
  title: string;
  count: number;
}

/** Collapse a lane's events into at most `MAX_TICKS_PER_LANE` drawn positions.
 *
 *  A bin holding exactly one event keeps that event's exact timestamp, class
 *  and tooltip, so a quiet lane renders pixel-for-pixel as it did before. A bin
 *  holding several sits at its members' mean time, takes the class of whichever
 *  class is commonest in it, and — the point — says how many it stands for. */
export function binTicks(evs: CamEvent[], start: number, windowSecs: number): Tick[] {
  if (windowSecs <= 0) return [];
  const size = windowSecs / MAX_TICKS_PER_LANE;
  const bins = new Map<number, CamEvent[]>();
  for (const ev of evs) {
    const b = Math.min(MAX_TICKS_PER_LANE - 1, Math.max(0, Math.floor((ev.ts - start) / size)));
    const arr = bins.get(b);
    if (arr) arr.push(ev);
    else bins.set(b, [ev]);
  }
  const fmt = (ts: number) => new Date(ts * 1000).toLocaleTimeString();
  const out: Tick[] = [];
  for (const [b, members] of bins) {
    if (members.length === 1) {
      const ev = members[0];
      out.push({
        key: `e${ev.id}`,
        ts: ev.ts,
        cls: eventClass(ev.label),
        title: `${prettyLabel(ev.label)} · ${fmt(ev.ts)}`,
        count: 1,
      });
      continue;
    }
    const tally = new Map<string, number>();
    const clsTally = new Map<string, number>();
    let sum = 0;
    let lo = Infinity;
    let hi = -Infinity;
    for (const ev of members) {
      sum += ev.ts;
      if (ev.ts < lo) lo = ev.ts;
      if (ev.ts > hi) hi = ev.ts;
      tally.set(ev.label, (tally.get(ev.label) ?? 0) + 1);
      const c = eventClass(ev.label);
      clsTally.set(c, (clsTally.get(c) ?? 0) + 1);
    }
    const top = [...tally.entries()].sort((a, b2) => b2[1] - a[1]).slice(0, 3);
    const cls = [...clsTally.entries()].sort((a, b2) => b2[1] - a[1])[0][0];
    out.push({
      key: `b${b}`,
      ts: Math.round(sum / members.length),
      cls,
      title:
        `${members.length} detections · ${fmt(lo)}${hi !== lo ? `–${fmt(hi)}` : ""}` +
        ` · ${top.map(([l, n]) => `${n} ${prettyLabel(l)}`).join(", ")}`,
      count: members.length,
    });
  }
  return out;
}

export function eventClass(label: string): string {
  if (label === "person") return "person";
  if (VEHICLES.includes(label)) return "vehicle";
  if (["knock", "speech", "glass", "alarm", "bark"].some((k) => label.toLowerCase().includes(k))) return "audio";
  return "";
}

/** Swatch + text legend for the color-coded event ticks — the text is the
 *  color-blind / low-vision fallback for the color encoding. Shared with the
 *  single-camera Timeline's tick palette. */
export function EventLegend() {
  const items: [string, string][] = [
    ["person", "Person"],
    ["vehicle", "Vehicle"],
    ["audio", "Audio"],
    ["", "Other"],
  ];
  return (
    <div className="evt-legend" aria-hidden="true">
      {items.map(([cls, label]) => (
        <span className="evt-legend-item" key={label}>
          <span className={`evt-swatch ${cls}`} />
          {label}
        </span>
      ))}
    </div>
  );
}

/** Protect-style activity overview: per-interval detection counts as a slim
 *  bar chart over the same time axis as a timeline, so you can see WHERE the
 *  busy periods are before you scrub.
 *
 *  With no `onPick` the bars are inert `<span>`s inside a `role="img"`, exactly
 *  as they have always been — Recordings and the camera detail view pass
 *  nothing and are untouched. Give it an `onPick` and each bar becomes a real
 *  `<button>` that narrows the window to its interval, and the wrapper stops
 *  claiming to be a single image and becomes a labelled group. Find needs that:
 *  seeing where the busy hour is and then being unable to click it is the
 *  point of the strip left undone. */
export function ActivityStrip({
  events,
  windowSecs,
  nowTs,
  embedded = false,
  onPick,
}: {
  events: CamEvent[];
  windowSecs: number;
  nowTs: number;
  embedded?: boolean;
  onPick?: (from: number, to: number) => void;
}) {
  const start = nowTs - windowSecs;
  // The inert strip is a picture, so 48 slivers over a day read fine. The
  // pickable one is a row of TARGETS, and 48 of them across a tablet lane came
  // out 12px wide — measured on an 820px touch viewport, where this app serves
  // the desktop layout and no width-keyed rule reaches it. Halving the buckets
  // doubles the target and lands on exactly the granularity this control is
  // for: on a day window, one bar is one hour.
  const maxBuckets = onPick ? 24 : 48;
  const buckets = windowSecs <= 3600 ? 12 : windowSecs <= 6 * HOUR ? 24 : maxBuckets;
  const size = windowSecs / buckets;
  const counts = new Array<number>(buckets).fill(0);
  for (const e of events) {
    if (e.ts < start || e.ts > nowTs) continue;
    counts[Math.min(buckets - 1, Math.floor((e.ts - start) / size))]++;
  }
  const max = Math.max(...counts);
  if (max === 0) return null;
  const fmt = (t: number) =>
    new Date(t * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  const label = (n: number, i: number) =>
    `${n} event${n === 1 ? "" : "s"} · ${fmt(start + i * size)}–${fmt(start + (i + 1) * size)}`;
  return (
    <div
      className={`act-strip ${embedded ? "" : "standalone"} ${onPick ? "pickable" : ""}`}
      role={onPick ? "group" : "img"}
      aria-label={
        onPick
          ? "Detections per interval — choose one to narrow the window to it"
          : "Detections per interval across this window"
      }
    >
      {counts.map((n, i) =>
        onPick ? (
          <button
            key={i}
            type="button"
            className="act-col"
            title={label(n, i)}
            aria-label={`Narrow to ${label(n, i)}`}
            // An empty interval is still a real answer ("nothing here"), but
            // narrowing to it would strand you on a blank window.
            disabled={n === 0}
            onClick={() => onPick(Math.round(start + i * size), Math.round(start + (i + 1) * size))}
          >
            <span className="act-bar" style={{ height: `${n === 0 ? 0 : Math.max(14, (n / max) * 100)}%` }} />
          </button>
        ) : (
          <span key={i} className="act-col" title={label(n, i)}>
            <span className="act-bar" style={{ height: `${n === 0 ? 0 : Math.max(14, (n / max) * 100)}%` }} />
          </span>
        )
      )}
    </div>
  );
}

export default function CrossTimeline({
  cameras,
  segments,
  events,
  windowSecs,
  segmentSecs,
  nowTs,
  onSeek,
  onPickWindow,
}: {
  cameras: Camera[];
  segments: Segment[];
  events: CamEvent[];
  windowSecs: number;
  segmentSecs: number;
  nowTs: number;
  onSeek: (cameraId: number, ts: number) => void;
  /** Makes the Activity lane's bars clickable, narrowing to that interval.
   *  Omitted by Recordings, so its lane stays exactly as inert as it was. */
  onPickWindow?: (from: number, to: number) => void;
}) {
  const start = nowTs - windowSecs;
  const pct = (ts: number) => ((ts - start) / windowSecs) * 100;

  // Keyboard scrubbing: one shared playhead (0..1) the arrows move; Enter plays
  // the focused lane's camera at that moment.
  const [cursor, setCursor] = useState<number | null>(null);
  const clamp01 = (n: number) => Math.min(1, Math.max(0, n));
  const tsAt = (f: number) => Math.round(start + f * windowSecs);
  const fmtClock = (ts: number) =>
    new Date(ts * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const laneKey = (camId: number) => (e: React.KeyboardEvent<HTMLDivElement>) => {
    const step = e.shiftKey ? 0.1 : 0.02;
    if (e.key === "ArrowRight" || e.key === "ArrowUp") {
      e.preventDefault();
      setCursor((c) => clamp01((c ?? 1) + step));
    } else if (e.key === "ArrowLeft" || e.key === "ArrowDown") {
      e.preventDefault();
      setCursor((c) => clamp01((c ?? 1) - step));
    } else if (e.key === "Home") {
      e.preventDefault();
      setCursor(0);
    } else if (e.key === "End") {
      e.preventDefault();
      setCursor(1);
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      if (cursor != null) onSeek(camId, tsAt(cursor));
    }
  };

  const lines: number[] = [];
  const step = windowSecs <= 2 * HOUR ? HOUR / 4 : windowSecs <= 12 * HOUR ? HOUR : 3 * HOUR;
  const first = Math.ceil(start / step) * step;
  for (let t = first; t <= nowTs; t += step) lines.push(t);

  const fmtAxis = (t: number) => {
    const d = new Date(t * 1000);
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  };

  const hasActivity = events.some((e) => e.ts >= start && e.ts <= nowTs);

  // Per-camera coverage blocks + event ticks, precomputed once per data change
  // instead of re-coalescing/filtering inside the render map on every cursor
  // move or parent re-render.
  const laneData = useMemo(() => {
    const m = new Map<number, { blocks: Block[]; evs: CamEvent[]; ticks: Tick[] }>();
    for (const cam of cameras) {
      // Bound the top as well as the bottom: with a window that ends in the
      // past (Find hands this a chosen day), a later event would otherwise be
      // drawn past the right edge of an axis that does not cover it.
      const evs = events.filter((e) => e.camera_id === cam.id && e.ts >= start && e.ts <= nowTs);
      m.set(cam.id, {
        blocks: coalesce(segments.filter((s) => s.camera_id === cam.id), segmentSecs),
        evs,
        ticks: binTicks(evs, start, windowSecs),
      });
    }
    return m;
  }, [cameras, segments, events, segmentSecs, start, nowTs, windowSecs]);

  return (
    <div className="xtl card">
      <div className="xtl-grid">
        {lines.map((t) => (
          <span key={t} className="xtl-line" style={{ left: `${pct(t)}%` }} />
        ))}
        {cursor != null && <span className="xtl-cursor" style={{ left: `${cursor * 100}%` }} />}
      </div>
      {hasActivity && (
        <div className="xtl-row">
          <div
            className="xtl-name"
            title="Detections per interval, all cameras — spot the busy periods before scrubbing"
          >
            Activity
          </div>
          <div className="xtl-lane act-lane">
            <ActivityStrip
              events={events}
              windowSecs={windowSecs}
              nowTs={nowTs}
              embedded
              onPick={onPickWindow}
            />
          </div>
        </div>
      )}
      {cameras.map((cam) => {
        const { blocks, evs, ticks } = laneData.get(cam.id) ?? { blocks: [], evs: [], ticks: [] };
        return (
          <div className="xtl-row" key={cam.id}>
            <div className="xtl-name" title={cam.name}>{cam.name}</div>
            <div
              className="xtl-lane"
              role="slider"
              tabIndex={0}
              aria-label={`${cam.name} recording scrubber — arrow keys move the playhead, Enter plays`}
              aria-valuemin={0}
              aria-valuemax={windowSecs}
              aria-valuenow={cursor != null ? Math.round(cursor * windowSecs) : windowSecs}
              aria-valuetext={cursor != null ? fmtClock(tsAt(cursor)) : "now"}
              onKeyDown={laneKey(cam.id)}
              onClick={(e) => {
                const rect = e.currentTarget.getBoundingClientRect();
                const frac = clamp01((e.clientX - rect.left) / rect.width);
                // Snap to a nearby event tick (within ~6px) and seek a few seconds
                // before it; clicks elsewhere keep free-seeking.
                const thresh = 6 / rect.width;
                let best: CamEvent | null = null;
                let bestD = thresh;
                for (const ev of evs) {
                  const d = Math.abs((ev.ts - start) / windowSecs - frac);
                  if (d <= bestD) {
                    bestD = d;
                    best = ev;
                  }
                }
                if (best) {
                  setCursor(clamp01((best.ts - start) / windowSecs));
                  onSeek(cam.id, Math.max(start, best.ts - 3));
                } else {
                  setCursor(frac);
                  onSeek(cam.id, Math.round(start + frac * windowSecs));
                }
              }}
            >
              {blocks.map((b, i) => (
                <div
                  key={i}
                  className="xtl-cov"
                  style={{ left: `${pct(b.start)}%`, width: `${Math.max(0.3, ((b.end - b.start) / windowSecs) * 100)}%` }}
                />
              ))}
              {/* Click-to-snap still searches the RAW events above, so binning
                  changes only what is drawn, never where a click lands. */}
              {ticks.map((t) => (
                <div
                  key={t.key}
                  className={`xtl-evt ${t.cls} ${t.count > 1 ? "stack" : ""}`}
                  style={{ left: `${pct(t.ts)}%` }}
                  title={t.title}
                />
              ))}
            </div>
          </div>
        );
      })}
      <div className="xtl-axis">
        {lines.map((t) => (
          <span key={t} className="xtl-axis-label" style={{ left: `${pct(t)}%` }}>
            {fmtAxis(t)}
          </span>
        ))}
      </div>
      <EventLegend />
    </div>
  );
}
