import { useRef, useState } from "react";
import { TogglePill } from "./ui";
import { IconRefresh } from "./icons";

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
