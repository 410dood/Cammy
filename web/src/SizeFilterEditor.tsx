import { useEffect, useRef, useState } from "react";
import { api } from "./api";
import { TogglePill } from "./ui";
import { IconRefresh } from "./icons";

/// Live motion test (docs/10 P2.2, the Frigate Motion Tuner lesson): watch
/// what the motion gate ACTUALLY measures, live, while moving the threshold
/// slider — no more guess-save-wait. Each probe diffs two fresh frames ~0.7 s
/// apart server-side; nothing here touches the running pipeline.
export function MotionTuner({
  cameraId,
  cameraName,
  threshold,
}: {
  cameraId: number;
  cameraName: string;
  /** The effective threshold (camera override or global) to compare against. */
  threshold: number;
}) {
  const [on, setOn] = useState(false);
  const [probe, setProbe] = useState<null | { changed: number; regions: [number, number, number, number][] }>(null);
  const [err, setErr] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (!on) {
      setProbe(null);
      setErr(null);
      return;
    }
    let live = true;
    let timer: ReturnType<typeof setTimeout>;
    const run = async () => {
      try {
        const r = await api.motionProbe(cameraId);
        if (!live) return;
        setProbe(r);
        setErr(null);
        setTick((t) => t + 1);
      } catch (e) {
        if (live) setErr(e instanceof Error ? e.message : String(e));
      }
      if (live) timer = setTimeout(run, 1200);
    };
    run();
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [on, cameraId]);
  const wouldTrip = probe != null && probe.changed >= threshold;
  return (
    <div className="mtuner">
      <div className="feat">
        <TogglePill on={on} ariaLabel="Live motion test" onClick={() => setOn(!on)}>
          Live motion test
        </TogglePill>
        <span className="feat-help">
          Walk the scene (or watch the trees) and see what would wake detection at the
          current threshold — tune it against reality instead of guessing.
        </span>
      </div>
      {on && (
        <>
          <div className="sizef-surface" style={{ marginTop: 8 }}>
            <img
              src={`/api/cameras/${cameraId}/frame.jpg?t=${tick}`}
              alt={`Current view from ${cameraName}`}
              draggable={false}
            />
            {probe?.regions.map(([x1, y1, x2, y2], i) => (
              <div
                key={i}
                className="mtuner-region"
                style={{
                  left: `${x1 * 100}%`,
                  top: `${y1 * 100}%`,
                  width: `${(x2 - x1) * 100}%`,
                  height: `${(y2 - y1) * 100}%`,
                }}
              />
            ))}
          </div>
          <p className="feat-help" style={{ margin: "6px 0 0" }} role="status">
            {err
              ? `No measurement — ${err}`
              : probe == null
                ? "Measuring…"
                : `${(probe.changed * 100).toFixed(1)}% of the frame just changed — ` +
                  (wouldTrip
                    ? `WOULD wake detection (threshold ${(threshold * 100).toFixed(1)}%).`
                    : `stays asleep (threshold ${(threshold * 100).toFixed(1)}%).`)}
          </p>
        </>
      )}
    </div>
  );
}

/// Graphical object-size filter (Blue Iris / UniFi style): two resizable boxes
/// drawn over the camera's own live frame. Anything smaller than the inner box
/// or larger than the outer box is ignored. The backend stores only an AREA
/// FRACTION (w×h of the frame), so the box aspect is presentational — we keep
/// the aspect the user dragged for the session and re-open with a person-ish
/// default (taller than wide) when only the stored area is known.
const BOX_R = 1.5; // default h/w when reconstructing a box from a bare area

const boxFromArea = (area: number) => {
  const h = Math.min(1, Math.sqrt(area * BOX_R));
  const w = Math.min(1, area / h);
  return { hw: w / 2, hh: h / 2 };
};

type Box = { hw: number; hh: number };

export type Rect = [number, number, number, number]; // x1,y1,x2,y2 frame fractions

/// Drag one rectangle over the camera's live frame — the shared building block
/// for wizard zones (P2.5) and the package zone (P3). Not the full ZoneEditor:
/// one box, no vertices, presets over precision.
export function RectZoneDraw({
  cameraId,
  cameraName,
  label,
  rect,
  onRect,
}: {
  cameraId: number;
  cameraName: string;
  label: string;
  rect: Rect | null;
  onRect: (r: Rect) => void;
}) {
  const surface = useRef<HTMLDivElement>(null);
  const [failed, setFailed] = useState(false);
  const [bust, setBust] = useState(0);
  const start = (e: React.PointerEvent) => {
    const el = surface.current;
    if (!el) return;
    e.preventDefault();
    const r0 = el.getBoundingClientRect();
    const clamp = (v: number) => Math.min(1, Math.max(0, v));
    const sx = clamp((e.clientX - r0.left) / r0.width);
    const sy = clamp((e.clientY - r0.top) / r0.height);
    const move = (ev: PointerEvent) => {
      const x = clamp((ev.clientX - r0.left) / r0.width);
      const y = clamp((ev.clientY - r0.top) / r0.height);
      onRect([Math.min(sx, x), Math.min(sy, y), Math.max(sx, x), Math.max(sy, y)]);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
  };
  return (
    <div className="sizef">
      <div
        ref={surface}
        className="sizef-surface"
        style={{ touchAction: "none", cursor: "crosshair" }}
        onPointerDown={start}
      >
        <img
          src={`/api/cameras/${cameraId}/frame.jpg?t=${bust}`}
          alt={`Current view from ${cameraName}`}
          draggable={false}
          onError={() => setFailed(true)}
          onLoad={() => setFailed(false)}
          style={{ visibility: failed ? "hidden" : undefined }}
        />
        {failed && (
          <div className="sizef-fallback">
            <div>
              No live picture right now — you can still drag a box on the blank frame.
              <div style={{ marginTop: 8 }}>
                <button type="button" className="btn btn-ghost ev-act" onClick={() => setBust(Date.now())}>
                  <IconRefresh size={14} /> retry
                </button>
              </div>
            </div>
          </div>
        )}
        {rect && (
          <div
            style={{
              position: "absolute",
              left: `${rect[0] * 100}%`,
              top: `${rect[1] * 100}%`,
              width: `${(rect[2] - rect[0]) * 100}%`,
              height: `${(rect[3] - rect[1]) * 100}%`,
              border: "2px solid var(--accent)",
              background: "color-mix(in oklab, var(--accent) 18%, transparent)",
              borderRadius: 4,
              pointerEvents: "none",
            }}
          >
            <span className="sizef-tag" style={{ position: "absolute", top: 2, left: 4 }}>{label}</span>
          </div>
        )}
      </div>
      <p className="feat-help sizef-hint">
        Drag a box over the {label.toLowerCase()} on the picture — you can redraw it until it
        looks right.
      </p>
    </div>
  );
}

/// Child-height calibration, graphically (docs/10 P1.9): instead of typing a
/// "fraction of frame height", drag a marker on the camera's own frame to
/// about how tall your child appears where they'd stand. Only the HEIGHT
/// fraction is stored; the horizontal position is a visual aid.
export function ChildHeightEditor({
  cameraId,
  cameraName,
  frac,
  onChange,
}: {
  cameraId: number;
  cameraName: string;
  frac: number;
  onChange: (frac: number) => void;
}) {
  const [x, setX] = useState(0.5);
  const [failed, setFailed] = useState(false);
  const [bust, setBust] = useState(0);
  const surface = useRef<HTMLDivElement>(null);

  const drag = (mode: "height" | "move") => (e: React.PointerEvent) => {
    e.preventDefault();
    const el = surface.current;
    if (!el) return;
    const t = e.currentTarget as HTMLElement;
    try {
      t.setPointerCapture(e.pointerId);
    } catch {
      /* synthetic/released pointers can't be captured */
    }
    const move = (ev: PointerEvent) => {
      const r = el.getBoundingClientRect();
      if (mode === "height") {
        const h = 1 - (ev.clientY - r.top) / r.height;
        onChange(Math.min(0.95, Math.max(0.05, Number(h.toFixed(3)))));
      } else {
        setX(Math.min(0.95, Math.max(0.05, (ev.clientX - r.left) / r.width)));
      }
    };
    const up = () => {
      t.removeEventListener("pointermove", move);
      t.removeEventListener("pointerup", up);
      t.removeEventListener("pointercancel", up);
    };
    t.addEventListener("pointermove", move);
    t.addEventListener("pointerup", up);
    t.addEventListener("pointercancel", up);
  };

  return (
    <div className="sizef">
      <div ref={surface} className="sizef-surface">
        <img
          src={`/api/cameras/${cameraId}/frame.jpg?t=${bust}`}
          alt={`Current view from ${cameraName}`}
          draggable={false}
          onError={() => setFailed(true)}
          onLoad={() => setFailed(false)}
          style={{ visibility: failed ? "hidden" : undefined }}
        />
        {failed && (
          <div className="sizef-fallback">
            <div>
              No live picture right now — the marker still works on a blank frame, and the
              percent field below is exact.
              <div style={{ marginTop: 8 }}>
                <button type="button" className="btn btn-ghost ev-act" onClick={() => setBust(Date.now())}>
                  <IconRefresh size={14} /> retry
                </button>
              </div>
            </div>
          </div>
        )}
        <div
          className="childh-bar"
          style={{ left: `${x * 100}%`, height: `${frac * 100}%` }}
          onPointerDown={drag("move")}
          title="Drag sideways to where your child would stand (position is just a visual aid)"
        >
          <span className="sizef-tag childh-tag">Shorter than this: child</span>
          <div className="childh-top" onPointerDown={drag("height")} role="presentation" />
        </div>
      </div>
      <p className="feat-help sizef-hint">
        Drag the marker's top edge to about how tall your child appears on this camera.
        Anyone shorter counts as a child for the zone rules; it's a rough visual aid, not an
        exact measurement — check it against real events.
      </p>
      <div className="sizef-nums">
        <label className="sizef-num">
          Child height
          <span>
            <input
              type="number"
              min={5}
              max={95}
              step={1}
              value={Math.round(frac * 100)}
              onChange={(e) => {
                const v = Math.min(95, Math.max(5, Number(e.target.value) || 5));
                onChange(v / 100);
              }}
            />
            % of frame height
          </span>
        </label>
      </div>
    </div>
  );
}

export default function SizeFilterEditor({
  cameraId,
  cameraName,
  minArea,
  maxArea,
  onChange,
}: {
  cameraId: number;
  cameraName: string;
  minArea: number | null;
  maxArea: number | null;
  /// null = that filter is off.
  onChange: (minArea: number | null, maxArea: number | null) => void;
}) {
  // Box shapes live locally so a drag keeps its aspect; the parent only ever
  // sees the resulting area fraction.
  const [minBox, setMinBox] = useState<Box>(() => boxFromArea(minArea ?? 0.005));
  const [maxBox, setMaxBox] = useState<Box>(() => boxFromArea(maxArea ?? 0.5));
  const [failed, setFailed] = useState(false);
  const [bust, setBust] = useState(0);
  const surface = useRef<HTMLDivElement>(null);
  const area = (b: Box) => 2 * b.hw * (2 * b.hh);

  const startDrag = (which: "min" | "max") => (e: React.PointerEvent) => {
    e.preventDefault();
    const el = surface.current;
    if (!el) return;
    const target = e.currentTarget as HTMLElement;
    try {
      target.setPointerCapture(e.pointerId);
    } catch {
      /* a released/synthetic pointer can't be captured — dragging still works */
    }
    const move = (ev: PointerEvent) => {
      const r = el.getBoundingClientRect();
      // Boxes are centered; the handle drags the bottom-right corner, so the
      // half-extents are just the pointer's distance from center.
      const hw = Math.min(0.5, Math.max(0.015, (ev.clientX - r.left) / r.width - 0.5));
      const hh = Math.min(0.5, Math.max(0.015, (ev.clientY - r.top) / r.height - 0.5));
      const b = { hw, hh };
      if (which === "min") {
        // Keep the ordering honest: the "ignore smaller" box can't outgrow the
        // "ignore larger" box (that would filter everything).
        const cap = maxArea != null ? area(maxBox) : 1;
        if (area(b) > cap) return;
        setMinBox(b);
        onChange(area(b), maxArea);
      } else {
        const floor = minArea != null ? area(minBox) : 0;
        if (area(b) < floor) return;
        setMaxBox(b);
        onChange(minArea, area(b));
      }
    };
    const up = () => {
      target.removeEventListener("pointermove", move);
      target.removeEventListener("pointerup", up);
      target.removeEventListener("pointercancel", up);
    };
    target.addEventListener("pointermove", move);
    target.addEventListener("pointerup", up);
    target.addEventListener("pointercancel", up);
  };

  const boxStyle = (b: Box): React.CSSProperties => ({
    left: `${(0.5 - b.hw) * 100}%`,
    top: `${(0.5 - b.hh) * 100}%`,
    width: `${b.hw * 200}%`,
    height: `${b.hh * 200}%`,
  });

  // Accessible / precise path beside the drag surface: a percent number input
  // per filter. Percent of frame AREA, matching what the backend stores.
  const numField = (label: string, value: number, set: (a: number) => void) => (
    <label className="sizef-num">
      {label}
      <span>
        <input
          type="number"
          min={0.01}
          max={100}
          step={0.1}
          value={Number((value * 100).toFixed(2))}
          onChange={(e) => {
            const v = Math.min(100, Math.max(0.01, Number(e.target.value) || 0.01));
            set(v / 100);
          }}
        />
        % of frame
      </span>
    </label>
  );

  return (
    <div className="sizef">
      <div className="sizef-toggles">
        <div className="feat">
          <TogglePill
            on={minArea != null}
            ariaLabel="Ignore tiny objects"
            onClick={() => {
              if (minArea != null) onChange(null, maxArea);
              else {
                const a = area(minBox);
                onChange(maxArea != null ? Math.min(a, area(maxBox)) : a, maxArea);
              }
            }}
          >
            Ignore tiny objects
          </TogglePill>
          <span className="feat-help">Drops far-away specks, bugs, and headlight blips.</span>
        </div>
        <div className="feat">
          <TogglePill
            on={maxArea != null}
            ariaLabel="Ignore huge changes"
            onClick={() => {
              if (maxArea != null) onChange(minArea, null);
              else {
                const a = area(maxBox);
                onChange(minArea, minArea != null ? Math.max(a, area(minBox)) : a);
              }
            }}
          >
            Ignore huge changes
          </TogglePill>
          <span className="feat-help">Drops whole-frame flips from lighting or IR switching.</span>
        </div>
      </div>

      {(minArea != null || maxArea != null) && (
        <>
          <div ref={surface} className="sizef-surface">
            <img
              src={`/api/cameras/${cameraId}/frame.jpg?t=${bust}`}
              alt={`Current view from ${cameraName}`}
              draggable={false}
              onError={() => setFailed(true)}
              onLoad={() => setFailed(false)}
              style={{ visibility: failed ? "hidden" : undefined }}
            />
            {failed && (
              <div className="sizef-fallback">
                <div>
                  No live picture from this camera right now — the boxes below are drawn on a
                  blank frame, and the size fields beside them still work.
                  <div style={{ marginTop: 8 }}>
                    <button
                      type="button"
                      className="btn btn-ghost ev-act"
                      onClick={() => setBust(Date.now())}
                    >
                      <IconRefresh size={14} /> retry
                    </button>
                  </div>
                </div>
              </div>
            )}
            {maxArea != null && (
              <div className="sizef-box sizef-max" style={boxStyle(maxBox)}>
                <span className="sizef-tag">Bigger than this: ignored</span>
                <div
                  className="sizef-handle"
                  role="presentation"
                  onPointerDown={startDrag("max")}
                />
              </div>
            )}
            {minArea != null && (
              <div className="sizef-box sizef-min" style={boxStyle(minBox)}>
                <span className="sizef-tag">Smaller than this: ignored</span>
                <div
                  className="sizef-handle"
                  role="presentation"
                  onPointerDown={startDrag("min")}
                />
              </div>
            )}
          </div>
          <p className="feat-help sizef-hint">
            Drag a box corner until it just fits the smallest (or largest) thing you care
            about in this camera's view.
          </p>
          <div className="sizef-nums">
            {minArea != null &&
              numField("Smallest object", minArea, (a) => {
                const capped = maxArea != null ? Math.min(a, maxArea) : a;
                setMinBox(boxFromArea(capped));
                onChange(capped, maxArea);
              })}
            {maxArea != null &&
              numField("Largest object", maxArea, (a) => {
                const floored = minArea != null ? Math.max(a, minArea) : a;
                setMaxBox(boxFromArea(floored));
                onChange(minArea, floored);
              })}
          </div>
        </>
      )}
    </div>
  );
}
