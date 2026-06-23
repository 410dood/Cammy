// A2 — unified 24/7 cross-camera timeline. One lane per camera over a shared
// time axis: coalesced recording-coverage blocks plus class-colored event ticks.
// Clicking a lane seeks that camera's recording at that moment. Coverage is
// coalesced (not one div per 60s segment) so a full day stays light.

import { useState } from "react";
import { CamEvent, Camera, Segment } from "./api";

const HOUR = 3600;
const VEHICLES = ["car", "truck", "bus", "motorcycle", "bicycle"];

interface Block {
  start: number;
  end: number;
}

function coalesce(segs: Segment[], segmentSecs: number): Block[] {
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

function eventClass(label: string): string {
  if (label === "person") return "person";
  if (VEHICLES.includes(label)) return "vehicle";
  if (["knock", "speech", "glass", "alarm", "bark"].some((k) => label.toLowerCase().includes(k))) return "audio";
  return "";
}

export default function CrossTimeline({
  cameras,
  segments,
  events,
  windowSecs,
  segmentSecs,
  nowTs,
  onSeek,
}: {
  cameras: Camera[];
  segments: Segment[];
  events: CamEvent[];
  windowSecs: number;
  segmentSecs: number;
  nowTs: number;
  onSeek: (cameraId: number, ts: number) => void;
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

  return (
    <div className="xtl card">
      <div className="xtl-grid">
        {lines.map((t) => (
          <span key={t} className="xtl-line" style={{ left: `${pct(t)}%` }} />
        ))}
        {cursor != null && <span className="xtl-cursor" style={{ left: `${cursor * 100}%` }} />}
      </div>
      {cameras.map((cam) => {
        const blocks = coalesce(segments.filter((s) => s.camera_id === cam.id), segmentSecs);
        const evs = events.filter((e) => e.camera_id === cam.id && e.ts >= start);
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
                const frac = (e.clientX - rect.left) / rect.width;
                setCursor(clamp01(frac));
                onSeek(cam.id, Math.round(start + frac * windowSecs));
              }}
            >
              {blocks.map((b, i) => (
                <div
                  key={i}
                  className="xtl-cov"
                  style={{ left: `${pct(b.start)}%`, width: `${Math.max(0.3, ((b.end - b.start) / windowSecs) * 100)}%` }}
                />
              ))}
              {evs.map((ev) => (
                <div
                  key={ev.id}
                  className={`xtl-evt ${eventClass(ev.label)}`}
                  style={{ left: `${pct(ev.ts)}%` }}
                  title={`${ev.label} · ${new Date(ev.ts * 1000).toLocaleTimeString()}`}
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
    </div>
  );
}
