import { useEffect, useRef, useState } from "react";
import { api } from "./api";
import { IconChevronLeft, IconChevronRight } from "./icons";

/// A day window in unix seconds, `[from, to)`. `null` = no time filter at all.
export type DayWindow = { from: number; to: number } | null;

/// Local midnight for the day containing `ts`. Everything here works in the
/// viewer's local time, because "Tuesday afternoon" is a local-clock memory —
/// a UTC day boundary would put late-evening footage on the wrong day.
export function dayStart(ts: number): number {
  const d = new Date(ts * 1000);
  d.setHours(0, 0, 0, 0);
  return Math.floor(d.getTime() / 1000);
}
const DAY = 86400;

/// `YYYY-MM-DD` in LOCAL time, for `<input type="date">` (which speaks local
/// wall-clock without a zone). `toISOString()` is UTC and would shift the day.
export function toDateInput(ts: number): string {
  const d = new Date(ts * 1000);
  d.setMinutes(d.getMinutes() - d.getTimezoneOffset());
  return d.toISOString().slice(0, 10);
}
export function fromDateInput(v: string): number | null {
  const [y, m, d] = v.split("-").map(Number);
  if (!y || !m || !d) return null;
  return Math.floor(new Date(y, m - 1, d, 0, 0, 0, 0).getTime() / 1000);
}

export const windowForDay = (start: number): DayWindow => ({ from: start, to: start + DAY });

/// Per-day event counts for the density calendar. Fetched once per session and
/// shared by every DayStrip — it lands on a hot path (Events and Recordings both
/// mount one) and the answer barely moves within a session.
let densityCache: Promise<Map<number, number>> | null = null;
function density(): Promise<Map<number, number>> {
  if (!densityCache) {
    densityCache = api
      .analyticsTimeseries(90)
      .then((t) => new Map((t.days ?? []).map((d) => [dayStart(d.ts), d.count])))
      // A failed density fetch must not break day navigation — the calendar
      // just renders without shading.
      .catch(() => new Map<number, number>());
  }
  return densityCache;
}

/// Pick a day to look at.
///
/// This is the control that was missing: Events had no day picker at all (its
/// only time inputs were two `datetime-local` fields hidden inside a collapsed
/// "More filters"), and Recordings had a bare date input with no sense of where
/// anything actually happened. Stepping days is the common case, so it is one
/// click; the calendar exists for "sometime last week" and shades days by how
/// much was detected, so a vague memory converges on a visible target instead of
/// being guessed at one day per attempt.
export default function DayStrip({
  value,
  onChange,
  className,
}: {
  value: DayWindow;
  onChange: (w: DayWindow) => void;
  className?: string;
}) {
  const today = dayStart(Math.floor(Date.now() / 1000));
  const selected = value ? dayStart(value.from) : null;
  const [calOpen, setCalOpen] = useState(false);
  const [counts, setCounts] = useState<Map<number, number>>(new Map());
  const popRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (calOpen) density().then(setCounts);
  }, [calOpen]);

  // Close the calendar on outside click / Escape, like every other popover here.
  useEffect(() => {
    if (!calOpen) return;
    const away = (e: MouseEvent) => {
      if (popRef.current && !popRef.current.contains(e.target as Node)) setCalOpen(false);
    };
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setCalOpen(false);
    document.addEventListener("mousedown", away);
    window.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", away);
      window.removeEventListener("keydown", esc);
    };
  }, [calOpen]);

  const pick = (start: number) => {
    onChange(windowForDay(start));
    setCalOpen(false);
  };
  const step = (days: number) => {
    const base = selected ?? today;
    const next = Math.min(dayStart(base + days * DAY), today); // never past today
    pick(next);
  };

  // 13 weeks ending this week, laid out as columns of 7 so density reads at a
  // glance. Weeks start Sunday to match the rest of the app's day handling.
  const weeks: number[][] = [];
  const lastSunday = today - new Date(today * 1000).getDay() * DAY;
  for (let w = 12; w >= 0; w--) {
    const col: number[] = [];
    for (let d = 0; d < 7; d++) col.push(lastSunday - w * 7 * DAY + d * DAY);
    weeks.push(col);
  }
  const max = Math.max(1, ...[...counts.values()]);
  const shade = (n: number) => (n <= 0 ? 0 : Math.min(4, Math.ceil((n / max) * 4)));

  const label = !selected
    ? "All time"
    : selected === today
      ? "Today"
      : selected === today - DAY
        ? "Yesterday"
        : new Date(selected * 1000).toLocaleDateString(undefined, {
            weekday: "short",
            month: "short",
            day: "numeric",
          });

  return (
    <div className={`daystrip ${className ?? ""}`} role="group" aria-label="Choose a day">
      <button
        type="button"
        className={`btn ${selected === today ? "btn-primary" : "btn-ghost"}`}
        aria-pressed={selected === today}
        onClick={() => pick(today)}
      >
        Today
      </button>
      <button
        type="button"
        className={`btn ${selected === today - DAY ? "btn-primary" : "btn-ghost"}`}
        aria-pressed={selected === today - DAY}
        onClick={() => pick(today - DAY)}
      >
        Yesterday
      </button>

      <div className="daystep">
        <button type="button" className="btn btn-ghost" aria-label="Previous day" onClick={() => step(-1)}>
          <IconChevronLeft size={15} />
        </button>
        <div className="daycal-wrap" ref={popRef}>
          <button
            type="button"
            className="btn btn-ghost daystep-label"
            aria-haspopup="dialog"
            aria-expanded={calOpen}
            onClick={() => setCalOpen((o) => !o)}
            title="Pick a day — shaded by how much was detected"
          >
            {label}
          </button>
          {calOpen && (
            <div className="daycal" role="dialog" aria-label="Pick a day">
              <div className="daycal-grid">
                {weeks.map((col, i) => (
                  <div className="daycal-col" key={i}>
                    {col.map((d) => {
                      const n = counts.get(d) ?? 0;
                      const future = d > today;
                      return (
                        <button
                          type="button"
                          key={d}
                          className={`daycal-day s${shade(n)} ${d === selected ? "sel" : ""}`}
                          disabled={future}
                          onClick={() => pick(d)}
                          title={`${new Date(d * 1000).toLocaleDateString()} — ${
                            future ? "in the future" : n === 0 ? "nothing detected" : `${n} detections`
                          }`}
                          aria-label={`${new Date(d * 1000).toLocaleDateString()}, ${
                            n === 0 ? "nothing detected" : `${n} detections`
                          }`}
                        />
                      );
                    })}
                  </div>
                ))}
              </div>
              <div className="daycal-foot">
                <span className="muted">less</span>
                <span className="daycal-day s0" aria-hidden="true" />
                <span className="daycal-day s1" aria-hidden="true" />
                <span className="daycal-day s2" aria-hidden="true" />
                <span className="daycal-day s3" aria-hidden="true" />
                <span className="daycal-day s4" aria-hidden="true" />
                <span className="muted">more</span>
                <input
                  type="date"
                  aria-label="Jump to an exact date"
                  max={toDateInput(today)}
                  value={selected ? toDateInput(selected) : ""}
                  onChange={(e) => {
                    const t = fromDateInput(e.target.value);
                    if (t != null) pick(t);
                  }}
                />
              </div>
            </div>
          )}
        </div>
        <button
          type="button"
          className="btn btn-ghost"
          aria-label="Next day"
          disabled={selected != null && selected >= today}
          onClick={() => step(1)}
        >
          <IconChevronRight size={15} />
        </button>
      </div>

      <button
        type="button"
        className={`btn ${!selected ? "btn-primary" : "btn-ghost"}`}
        aria-pressed={!selected}
        onClick={() => {
          onChange(null);
          setCalOpen(false);
        }}
        title="Every day — the newest events across all time"
      >
        All
      </button>
    </div>
  );
}
