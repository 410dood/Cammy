import { useMemo, useState } from "react";
import { CamEvent, Segment } from "./api";
import { eventClass } from "./CrossTimeline";
import { isCameraSide, prettyLabel } from "./labels";

/// UniFi-style scrubber: recorded coverage as blocks, events as ticks.
/// Click anywhere in a recorded span to start playback at that instant.
export default function Timeline({
  windowSecs,
  segmentSecs,
  segments,
  events,
  onSeek,
  nowTs,
  markTs,
}: {
  windowSecs: number;
  segmentSecs: number;
  segments: Segment[];
  events: CamEvent[];
  onSeek: (ts: number) => void;
  /** Right edge of the window (unix secs); defaults to now — a day picker
   *  passes end-of-day to scrub history. */
  nowTs?: number;
  /** Current playback position (unix secs) — rendered as a playhead so the
   *  unified live/playback player shows where in time you are. */
  markTs?: number | null;
}) {
  const now = nowTs ?? Math.floor(Date.now() / 1000);
  const start = now - windowSecs;

  const frac = (ts: number) => (ts - start) / windowSecs;

  // Keyboard scrubbing: a cursor (0..1) the arrow keys move; Enter plays from it.
  const [cursor, setCursor] = useState<number | null>(null);
  // Hover preview: the keyframe of the segment under the pointer plus the
  // clock, so scrubbing gives instant visual feedback before any click
  // (the thumbs endpoint caches per segment, so tracking is cheap).
  const [hover, setHover] = useState<{ frac: number; ts: number; segId: number | null } | null>(
    null,
  );
  const clamp01 = (n: number) => Math.min(1, Math.max(0, n));
  const tsAt = (f: number) => Math.round(start + f * windowSecs);
  const fmtClock = (ts: number) =>
    new Date(ts * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const onKey = (e: React.KeyboardEvent<HTMLDivElement>) => {
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
      if (cursor != null) onSeek(tsAt(cursor));
    }
  };

  const blocks = useMemo(
    () =>
      segments
        .filter((s) => s.start_ts + segmentSecs > start && s.start_ts < now)
        .map((s) => ({
          left: Math.max(0, frac(s.start_ts)),
          width: Math.min(1, frac(s.start_ts + segmentSecs)) - Math.max(0, frac(s.start_ts)),
        })),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [segments, windowSecs, segmentSecs]
  );

  const ticks = useMemo(
    () => events.filter((e) => e.ts >= start && e.ts <= now).map((e) => ({ left: frac(e.ts), e })),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [events, windowSecs]
  );

  // Hour gridlines (or 10-minute lines for the 1h window).
  const gridStep = windowSecs <= 3600 ? 600 : 3600;
  const gridLines: number[] = [];
  for (let t = Math.ceil(start / gridStep) * gridStep; t < now; t += gridStep) {
    gridLines.push(frac(t));
  }

  const click = (ev: React.MouseEvent<HTMLDivElement>) => {
    const rect = ev.currentTarget.getBoundingClientRect();
    const f = clamp01((ev.clientX - rect.left) / rect.width);
    // Snap to a nearby event tick (within ~6px) so the markers are actionable —
    // seek a few seconds before the event so you see it happen. Clicks elsewhere
    // keep free-seeking.
    const thresh = 6 / rect.width;
    let best: { left: number; e: CamEvent } | null = null;
    for (const t of ticks) {
      if (Math.abs(t.left - f) <= thresh && (!best || Math.abs(t.left - f) < Math.abs(best.left - f))) {
        best = t;
      }
    }
    if (best) {
      setCursor(best.left);
      onSeek(Math.max(start, best.e.ts - 3));
      return;
    }
    setCursor(f);
    onSeek(tsAt(f));
  };

  const onMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    const f = clamp01((e.clientX - rect.left) / rect.width);
    const ts = tsAt(f);
    const seg = segments.find((s) => ts >= s.start_ts && ts < s.start_ts + segmentSecs);
    setHover({ frac: f, ts, segId: seg?.id ?? null });
  };

  return (
    <div className="tl-wrap">
      {hover && (
        <div
          className="tl-bubble"
          style={{ left: `clamp(84px, ${hover.frac * 100}%, calc(100% - 84px))` }}
          aria-hidden="true"
        >
          {hover.segId != null && (
            <img src={`/api/recordings/${hover.segId}/thumb.jpg`} alt="" />
          )}
          <span className="clock">{fmtClock(hover.ts)}</span>
        </div>
      )}
    <div
      className="timeline"
      onClick={click}
      onKeyDown={onKey}
      onPointerMove={onMove}
      onPointerLeave={() => setHover(null)}
      role="slider"
      tabIndex={0}
      aria-label="Recording scrubber — arrow keys move the cursor, Enter plays from there"
      aria-valuemin={0}
      aria-valuemax={windowSecs}
      aria-valuenow={cursor != null ? Math.round(cursor * windowSecs) : windowSecs}
      aria-valuetext={cursor != null ? fmtClock(tsAt(cursor)) : "now"}
      title="Click, or focus and use ← → + Enter, to play from a moment"
    >
      {gridLines.map((g, i) => (
        <div className="tl-grid" key={i} style={{ left: `${g * 100}%` }} />
      ))}
      {blocks.map((b, i) => (
        <div
          className="tl-block"
          key={i}
          style={{ left: `${b.left * 100}%`, width: `${Math.max(0.15, b.width * 100)}%` }}
        />
      ))}
      {ticks.map(({ left, e }, i) => (
        <div
          className={`tl-tick ${eventClass(e.label)}`}
          key={i}
          style={{ left: `${left * 100}%` }}
          title={`${prettyLabel(e.label)}${isCameraSide(e.label) ? "" : ` ${(e.score * 100).toFixed(0)}%`} @ ${new Date(e.ts * 1000).toLocaleTimeString()}`}
        />
      ))}
      {cursor != null && (
        <div className="tl-cursor" style={{ left: `${cursor * 100}%` }} aria-hidden="true" />
      )}
      {markTs != null && markTs >= start && markTs <= now && (
        <div className="tl-mark" style={{ left: `${frac(markTs) * 100}%` }} aria-hidden="true" />
      )}
      <div className="tl-times">
        <span>{new Date(start * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</span>
        {/* Only claim "now" when the right edge really is now — a day scrub or
            an event viewer anchors the window in the past. */}
        <span>{Math.abs(Date.now() / 1000 - now) < 120 ? "now" : fmtClock(now)}</span>
      </div>
    </div>
    </div>
  );
}
