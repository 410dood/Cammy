import { useEffect, useRef, useState } from "react";
import { api, Camera, Heatmap as HeatmapData } from "./api";

/** Time ranges for the heatmap query. `from` is computed at fetch time. */
const RANGES: { label: string; from: () => number | undefined }[] = [
  { label: "Today", from: startOfToday },
  { label: "24h", from: () => nowSec() - 86400 },
  { label: "7d", from: () => nowSec() - 7 * 86400 },
  { label: "30d", from: () => nowSec() - 30 * 86400 },
  { label: "All", from: () => undefined },
];

const nowSec = () => Math.floor(Date.now() / 1000);
function startOfToday(): number {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  return Math.floor(d.getTime() / 1000);
}

/** Blue → cyan → green → yellow → red ramp for a normalized t in [0,1]. */
function heatColor(t: number): [number, number, number] {
  const stops: [number, [number, number, number]][] = [
    [0.0, [30, 60, 200]],
    [0.25, [0, 200, 200]],
    [0.5, [0, 200, 60]],
    [0.75, [240, 220, 0]],
    [1.0, [230, 40, 30]],
  ];
  for (let i = 1; i < stops.length; i++) {
    if (t <= stops[i][0]) {
      const [t0, c0] = stops[i - 1];
      const [t1, c1] = stops[i];
      const f = (t - t0) / (t1 - t0);
      return [0, 1, 2].map((k) => Math.round(c0[k] + (c1[k] - c0[k]) * f)) as [number, number, number];
    }
  }
  return stops[stops.length - 1][1];
}

/** An activity heatmap overlaid on a dimmed live frame for one camera. */
export default function Heatmap({ camera }: { camera: Camera }) {
  const [range, setRange] = useState(0);
  const [data, setData] = useState<HeatmapData | null>(null);
  const [err, setErr] = useState(false);
  // The native frame aspect ratio, so the canvas overlay (which spans the whole
  // box in normalized 0..1 space) lines up with the contain-fitted frame even for
  // non-16:9 cameras. Set from the frame image's natural size on load.
  const [aspect, setAspect] = useState(16 / 9);
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    let alive = true;
    setErr(false);
    setData(null); // clear the previous camera/range's map while the next loads
    api
      .analyticsHeatmap(camera.id, RANGES[range].from(), undefined, 32)
      .then((d) => alive && setData(d))
      .catch(() => alive && setErr(true));
    return () => {
      alive = false;
    };
  }, [camera.id, range]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !data) return;
    const g = data.grid;
    canvas.width = g;
    canvas.height = g;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.clearRect(0, 0, g, g);
    if (data.max <= 0) return;
    const img = ctx.createImageData(g, g);
    for (let i = 0; i < data.cells.length; i++) {
      const v = data.cells[i];
      // sqrt spreads the low end so sparse activity is still visible.
      const t = Math.sqrt(v / data.max);
      const [r, gr, b] = heatColor(t);
      img.data[i * 4] = r;
      img.data[i * 4 + 1] = gr;
      img.data[i * 4 + 2] = b;
      img.data[i * 4 + 3] = v === 0 ? 0 : Math.round(50 + 190 * t);
    }
    ctx.putImageData(img, 0, 0);
  }, [data]);

  const loading = !err && data == null;
  const empty = !err && data != null && data.max === 0;

  return (
    <div>
      <div className="row" style={{ marginBottom: 8, alignItems: "center", flexWrap: "wrap" }}>
        {RANGES.map((r, i) => (
          <button
            key={r.label}
            type="button"
            className={`chip ${i === range ? "on" : ""}`}
            aria-pressed={i === range}
            onClick={() => setRange(i)}
          >
            {r.label}
          </button>
        ))}
        <span className="muted" style={{ marginLeft: "auto", fontSize: "var(--text-xs)" }}>
          where objects were detected
        </span>
      </div>
      <div
        style={{
          position: "relative",
          width: "100%",
          maxWidth: 720,
          // Match the frame's true aspect ratio so the overlay registers exactly.
          aspectRatio: String(aspect),
          background: "#000",
          borderRadius: 8,
          overflow: "hidden",
        }}
      >
        <img
          src={`/api/cameras/${camera.id}/frame.jpg`}
          alt=""
          onLoad={(e) => {
            const im = e.currentTarget;
            if (im.naturalWidth > 0 && im.naturalHeight > 0)
              setAspect(im.naturalWidth / im.naturalHeight);
          }}
          style={{ position: "absolute", inset: 0, width: "100%", height: "100%", objectFit: "fill", opacity: 0.5 }}
        />
        <canvas
          ref={canvasRef}
          style={{
            position: "absolute",
            inset: 0,
            width: "100%",
            height: "100%",
            mixBlendMode: "screen",
            pointerEvents: "none",
          }}
        />
        {(loading || empty || err) && (
          <div className="muted" style={{ position: "absolute", inset: 0, display: "grid", placeItems: "center" }}>
            {err ? "Heatmap unavailable." : loading ? "Loading…" : "No activity in this range yet."}
          </div>
        )}
      </div>
      {!loading && !err && (
        <div className="row" style={{ marginTop: 8, gap: 8, alignItems: "center", maxWidth: 720 }}>
          <span className="muted" style={{ fontSize: "var(--text-xs)" }}>Less</span>
          <span
            aria-hidden="true"
            style={{
              flex: 1,
              height: 8,
              borderRadius: 4,
              background:
                "linear-gradient(90deg, rgb(30,60,200), rgb(0,200,200), rgb(0,200,60), rgb(240,220,0), rgb(230,40,30))",
            }}
          />
          <span className="muted" style={{ fontSize: "var(--text-xs)" }}>More activity</span>
          <span className="muted" style={{ fontSize: "var(--text-xs)", marginLeft: 6 }}>
            · relative to this camera &amp; range
          </span>
        </div>
      )}
    </div>
  );
}
