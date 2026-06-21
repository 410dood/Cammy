import { useState } from "react";
import { Camera, CrossDir, GroundCalib, PolyZone, Tripwire, ZoneKind } from "./api";
import { IconRefresh } from "./icons";

type Mask = [number, number][];
type Draw =
  | { kind: "zone"; zoneKind: ZoneKind; points: Mask }
  | { kind: "mask"; points: Mask }
  | { kind: "tripwire"; points: Mask }
  | { kind: "calib"; points: Mask }
  | null;

const COLORS: Record<string, string> = {
  required: "#36d399",
  ignore: "#f87272",
  mask: "#a3a3a3",
  tripwire: "#38bdf8",
  calib: "#fbbf24",
};

/**
 * Draw polygon detection zones (required / ignore) and privacy masks directly
 * on a still frame from the camera. All coordinates are stored as 0..1 frame
 * fractions so they survive resolution and sub-stream changes.
 */
export default function ZoneEditor({
  camera,
  zones,
  masks,
  tripwires,
  calib,
  onChange,
  onTripwires,
  onCalib,
}: {
  camera: Camera;
  zones: PolyZone[];
  masks: Mask[];
  tripwires: Tripwire[];
  calib: GroundCalib | null;
  onChange: (zones: PolyZone[], masks: Mask[]) => void;
  onTripwires: (t: Tripwire[]) => void;
  onCalib: (c: GroundCalib | null) => void;
}) {
  const [draw, setDraw] = useState<Draw>(null);
  // Frame loading is resilient: go2rtc may be mid-restart or waiting for a
  // keyframe when the modal opens, so a single failed load shouldn't strand the
  // editor on "No live frame" forever. Retry a few times (cache-busting each
  // time), show the message only after that, and offer a manual retry.
  const [bust, setBust] = useState(0);
  const [autoTries, setAutoTries] = useState(0);
  const [failed, setFailed] = useState(false);
  const MAX_AUTO = 8;
  const onErr = () => {
    if (autoTries < MAX_AUTO) {
      setAutoTries((a) => a + 1);
      setTimeout(() => setBust((b) => b + 1), 900);
    } else {
      setFailed(true);
    }
  };
  const onOk = () => {
    setFailed(false);
    setAutoTries(0);
  };
  const retry = () => {
    setFailed(false);
    setAutoTries(0);
    setBust((b) => b + 1);
  };

  const addPoint = (e: React.MouseEvent) => {
    if (!draw) return;
    // A tripwire is exactly a 2-point segment; a calibration quad is 4 points.
    if (draw.kind === "tripwire" && draw.points.length >= 2) return;
    if (draw.kind === "calib" && draw.points.length >= 4) return;
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const x = Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
    const y = Math.min(1, Math.max(0, (e.clientY - rect.top) / rect.height));
    setDraw({ ...draw, points: [...draw.points, [Number(x.toFixed(4)), Number(y.toFixed(4))]] });
  };

  const minPts = (d: NonNullable<Draw>) =>
    d.kind === "tripwire" ? 2 : d.kind === "calib" ? 4 : 3;

  const finish = () => {
    if (!draw || draw.points.length < minPts(draw)) {
      setDraw(null);
      return;
    }
    if (draw.kind === "zone") {
      onChange(
        [
          ...zones,
          { name: `zone ${zones.length + 1}`, points: draw.points, kind: draw.zoneKind, labels: [] },
        ],
        masks
      );
    } else if (draw.kind === "tripwire") {
      onTripwires([
        ...tripwires,
        {
          name: `line ${tripwires.length + 1}`,
          a: draw.points[0],
          b: draw.points[1],
          direction: "both",
          labels: [],
          alert_wrong_way: false,
        },
      ]);
    } else if (draw.kind === "calib") {
      onCalib({
        points: [draw.points[0], draw.points[1], draw.points[2], draw.points[3]],
        width_m: 5,
        height_m: 5,
      });
    } else {
      onChange(zones, [...masks, draw.points]);
    }
    setDraw(null);
  };

  const polyStr = (pts: Mask) => pts.map((p) => `${p[0]},${p[1]}`).join(" ");

  return (
    <div>
      <div
        onClick={addPoint}
        style={{
          position: "relative",
          width: "100%",
          maxWidth: 640,
          aspectRatio: "16 / 9",
          background: "#000",
          borderRadius: 8,
          overflow: "hidden",
          cursor: draw ? "crosshair" : "default",
        }}
      >
        <img
          src={`/api/cameras/${camera.id}/frame.jpg?t=${bust}`}
          alt={camera.name}
          onError={onErr}
          onLoad={onOk}
          style={{ position: "absolute", inset: 0, width: "100%", height: "100%", objectFit: "contain" }}
        />
        {failed && (
          <div
            style={{
              position: "absolute",
              inset: 0,
              display: "grid",
              placeItems: "center",
              color: "#888",
              fontSize: "0.85rem",
              padding: 12,
              textAlign: "center",
            }}
          >
            <div>
              No live frame — the camera must be enabled and streaming to draw on it. You can
              still edit zones numerically after saving.
              <div style={{ marginTop: 8 }}>
                <button
                  type="button"
                  className="btn btn-ghost ev-act"
                  onClick={(e) => {
                    e.stopPropagation();
                    retry();
                  }}
                >
                  <IconRefresh size={14} /> retry
                </button>
              </div>
            </div>
          </div>
        )}
        <svg
          viewBox="0 0 1 1"
          preserveAspectRatio="none"
          role="img"
          aria-label="Detection zones and privacy masks drawn over the camera frame"
          style={{ position: "absolute", inset: 0, width: "100%", height: "100%", pointerEvents: "none" }}
        >
          {zones.map((z, i) => (
            <polygon
              key={`z${i}`}
              points={polyStr(z.points)}
              fill={COLORS[z.kind]}
              fillOpacity={0.2}
              stroke={COLORS[z.kind]}
              strokeWidth={2}
              vectorEffect="non-scaling-stroke"
            />
          ))}
          {masks.map((m, i) => (
            <polygon
              key={`m${i}`}
              points={polyStr(m)}
              fill="#000"
              fillOpacity={0.75}
              stroke={COLORS.mask}
              strokeWidth={2}
              vectorEffect="non-scaling-stroke"
            />
          ))}
          {calib && calib.points.length === 4 && (
            <polygon
              points={polyStr(calib.points)}
              fill={COLORS.calib}
              fillOpacity={0.12}
              stroke={COLORS.calib}
              strokeWidth={2}
              strokeDasharray="3 2"
              vectorEffect="non-scaling-stroke"
            />
          )}
          {tripwires.map((tw, i) => (
            <g key={`tw${i}`}>
              {/* Fractional stroke (viewBox units), NOT non-scaling-stroke: a
                  degenerate-bbox vertical/horizontal line + preserveAspectRatio
                  "none" makes Chrome blow a non-scaling stroke up to user units
                  (filling the frame), so size it in 0..1 space instead. */}
              <line
                x1={tw.a[0]}
                y1={tw.a[1]}
                x2={tw.b[0]}
                y2={tw.b[1]}
                stroke={COLORS.tripwire}
                strokeWidth={0.006}
              />
              <circle cx={tw.a[0]} cy={tw.a[1]} r={0.012} fill={COLORS.tripwire} />
              <circle cx={tw.b[0]} cy={tw.b[1]} r={0.012} fill={COLORS.tripwire} />
            </g>
          ))}
          {draw && draw.points.length > 0 && (
            <>
              <polyline
                points={polyStr(draw.points)}
                fill="none"
                stroke={
                  draw.kind === "mask"
                    ? COLORS.mask
                    : draw.kind === "tripwire"
                      ? COLORS.tripwire
                      : draw.kind === "calib"
                        ? COLORS.calib
                        : COLORS[draw.zoneKind]
                }
                strokeWidth={2}
                strokeDasharray="4 3"
                vectorEffect="non-scaling-stroke"
              />
              {draw.points.map((p, i) => (
                <circle key={i} cx={p[0]} cy={p[1]} r={4} fill="#fff" vectorEffect="non-scaling-stroke" />
              ))}
            </>
          )}
        </svg>
      </div>

      <div className="row" style={{ marginTop: 10, flexWrap: "wrap" }}>
        {!draw ? (
          <>
            <button
              type="button"
              className="ghost"
              onClick={() => setDraw({ kind: "zone", zoneKind: "required", points: [] })}
            >
              + required zone
            </button>
            <button
              type="button"
              className="ghost"
              onClick={() => setDraw({ kind: "zone", zoneKind: "ignore", points: [] })}
            >
              + ignore zone
            </button>
            <button type="button" className="ghost" onClick={() => setDraw({ kind: "mask", points: [] })}>
              + privacy mask
            </button>
            <button
              type="button"
              className="ghost"
              onClick={() => setDraw({ kind: "tripwire", points: [] })}
            >
              + tripwire
            </button>
            {!calib && (
              <button
                type="button"
                className="ghost"
                title="Calibrate the ground plane for speed estimation: click the 4 corners of a known rectangle on the ground, then enter its real size."
                onClick={() => setDraw({ kind: "calib", points: [] })}
              >
                + ground calibration
              </button>
            )}
            <span className="muted">
              polygon (zones/masks), 2 points for a tripwire, or 4 ground corners for speed — then Finish
            </span>
          </>
        ) : (
          <>
            <span className="pill on">
              drawing{" "}
              {draw.kind === "mask"
                ? "privacy mask"
                : draw.kind === "tripwire"
                  ? "tripwire"
                  : draw.kind === "calib"
                    ? "ground calibration"
                    : `${draw.zoneKind} zone`}{" "}
              · {draw.points.length} pts
            </span>
            <button
              type="button"
              className="ghost"
              disabled={draw.points.length === 0}
              onClick={() => setDraw({ ...draw, points: draw.points.slice(0, -1) })}
            >
              undo point
            </button>
            <button
              type="button"
              className="primary"
              disabled={draw.points.length < minPts(draw)}
              onClick={finish}
            >
              Finish
            </button>
            <button type="button" className="ghost" onClick={() => setDraw(null)}>
              cancel
            </button>
          </>
        )}
      </div>

      {(zones.length > 0 || masks.length > 0) && (
        <div style={{ marginTop: 10 }}>
          {zones.map((z, i) => (
            <div className="row" key={`zr${i}`} style={{ marginBottom: 6, alignItems: "center" }}>
              <span className="dot" style={{ background: COLORS[z.kind] }} />
              <input
                type="text"
                style={{ width: 130 }}
                value={z.name}
                onChange={(e) =>
                  onChange(zones.map((x, j) => (j === i ? { ...x, name: e.target.value } : x)), masks)
                }
              />
              <select
                value={z.kind}
                onChange={(e) =>
                  onChange(
                    zones.map((x, j) => (j === i ? { ...x, kind: e.target.value as ZoneKind } : x)),
                    masks
                  )
                }
              >
                <option value="required">required</option>
                <option value="ignore">ignore</option>
              </select>
              <input
                type="text"
                placeholder="objects (all)"
                style={{ width: 130 }}
                value={z.labels.join(", ")}
                onChange={(e) =>
                  onChange(
                    zones.map((x, j) =>
                      j === i
                        ? { ...x, labels: e.target.value.split(",").map((s) => s.trim()).filter(Boolean) }
                        : x
                    ),
                    masks
                  )
                }
              />
              <input
                type="number"
                min="0"
                step="5"
                placeholder="dwell s"
                title="Loiter alert: seconds an object must dwell inside this zone to fire a loiter event (blank/0 = off, needs tracking)"
                style={{ width: 76 }}
                value={z.dwell_secs ?? ""}
                onChange={(e) =>
                  onChange(
                    zones.map((x, j) =>
                      j === i
                        ? { ...x, dwell_secs: e.target.value === "" ? null : Number(e.target.value) }
                        : x
                    ),
                    masks
                  )
                }
              />
              <input
                type="number"
                min="0"
                step="1"
                placeholder="max #"
                title="Occupancy limit: an occupancy event fires when the live count of objects inside this zone first exceeds this number (blank/0 = off, needs tracking)"
                style={{ width: 76 }}
                value={z.occupancy_max ?? ""}
                onChange={(e) =>
                  onChange(
                    zones.map((x, j) =>
                      j === i
                        ? { ...x, occupancy_max: e.target.value === "" ? null : Number(e.target.value) }
                        : x
                    ),
                    masks
                  )
                }
              />
              <button
                type="button"
                className="danger"
                onClick={() => onChange(zones.filter((_, j) => j !== i), masks)}
              >
                remove
              </button>
            </div>
          ))}
          {masks.map((_, i) => (
            <div className="row" key={`mr${i}`} style={{ marginBottom: 6, alignItems: "center" }}>
              <span className="dot" style={{ background: COLORS.mask }} />
              <span className="muted" style={{ width: 130 }}>
                privacy mask {i + 1}
              </span>
              <button
                type="button"
                className="danger"
                onClick={() => onChange(zones, masks.filter((_, j) => j !== i))}
              >
                remove
              </button>
            </div>
          ))}
        </div>
      )}

      {tripwires.length > 0 && (
        <div style={{ marginTop: 10 }}>
          <div className="muted" style={{ fontSize: "0.8rem", marginBottom: 4 }}>
            Tripwires — an object crossing the line fires a <code>crossing</code> event (in/out counting,
            perimeter, one-way enforcement).
          </div>
          {tripwires.map((tw, i) => (
            <div className="row" key={`tw${i}`} style={{ marginBottom: 6, alignItems: "center" }}>
              <span className="dot" style={{ background: COLORS.tripwire }} />
              <input
                type="text"
                style={{ width: 110 }}
                value={tw.name}
                onChange={(e) =>
                  onTripwires(tripwires.map((x, j) => (j === i ? { ...x, name: e.target.value } : x)))
                }
              />
              <select
                value={tw.direction}
                title="Which crossing direction fires"
                onChange={(e) =>
                  onTripwires(
                    tripwires.map((x, j) =>
                      j === i ? { ...x, direction: e.target.value as CrossDir } : x
                    )
                  )
                }
              >
                <option value="both">both ways</option>
                <option value="a_to_b">A → B only</option>
                <option value="b_to_a">B → A only</option>
              </select>
              <input
                type="text"
                placeholder="objects (all)"
                style={{ width: 130 }}
                value={tw.labels.join(", ")}
                onChange={(e) =>
                  onTripwires(
                    tripwires.map((x, j) =>
                      j === i
                        ? { ...x, labels: e.target.value.split(",").map((s) => s.trim()).filter(Boolean) }
                        : x
                    )
                  )
                }
              />
              <label
                className="toggle field"
                title="One-way enforcement: a crossing against the chosen direction fires a wrong_way alert (only with a one-way direction)."
              >
                wrong-way
                <input
                  type="checkbox"
                  checked={!!tw.alert_wrong_way}
                  disabled={tw.direction === "both"}
                  onChange={(e) =>
                    onTripwires(
                      tripwires.map((x, j) => (j === i ? { ...x, alert_wrong_way: e.target.checked } : x))
                    )
                  }
                />
              </label>
              <button
                type="button"
                className="danger"
                onClick={() => onTripwires(tripwires.filter((_, j) => j !== i))}
              >
                remove
              </button>
            </div>
          ))}
        </div>
      )}

      {calib && (
        <div style={{ marginTop: 10 }}>
          <div className="muted" style={{ fontSize: "0.8rem", marginBottom: 4 }}>
            Ground calibration — speed estimation. The 4 marked corners are a real ground
            rectangle; enter its size so pixel motion becomes km/h on crossing events.
          </div>
          <div className="row" style={{ alignItems: "center" }}>
            <span className="dot" style={{ background: COLORS.calib }} />
            <label className="field">
              width (m)
              <input
                type="number"
                min="0.1"
                step="0.5"
                style={{ width: 90 }}
                value={calib.width_m}
                onChange={(e) => onCalib({ ...calib, width_m: Number(e.target.value) })}
              />
            </label>
            <label className="field">
              length (m)
              <input
                type="number"
                min="0.1"
                step="0.5"
                style={{ width: 90 }}
                value={calib.height_m}
                onChange={(e) => onCalib({ ...calib, height_m: Number(e.target.value) })}
              />
            </label>
            <button type="button" className="danger" onClick={() => onCalib(null)}>
              remove
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
