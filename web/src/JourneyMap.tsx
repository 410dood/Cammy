// P3.1 — Journey fusion (v0): draw an object's cross-camera APPEARANCE path over
// the saved floor plan. Given the ordered journey steps (the seed event plus its
// appearance-similarity matches, sorted by time) and the same Viewer-readable
// `Settings.floorplan` = { image, pins:[{camera,x,y}] } the Map page uses, plot
// each involved camera's pin and thread a numbered polyline through them in
// chronological order. Reuses the `.fp-*` floor-plan CSS; graceful empty state
// when there's no plan or no pins for the involved cameras.
//
// FRAMING: this is appearance-similarity ("same-looking clothing / vehicle"), NOT
// confirmed identity — the parent view carries the disclaimer, and the waypoint
// tooltips say "% match", never "the same person".
//
// v0 SCOPE (deferred to v1): this is the visual path only. It does NOT stitch a
// combined "Moments" export clip spanning cameras — that needs new multi-camera
// ffmpeg filter_complex work and its own backend routes, out of scope for this
// pure-web pass.

import { useEffect, useState } from "react";
import { api, CamEvent, FloorPlan, fmtTime } from "./api";
import { EmptyState } from "./ui";
import { IconMap } from "./icons";

export interface JourneyStep {
  ev: CamEvent;
  /** Appearance-similarity 0..1, or null for the seed event you started from. */
  similarity: number | null;
}

type Pin = { camera: string; x: number; y: number };
type Waypoint = { n: number; ev: CamEvent; similarity: number | null; pin: Pin };

export default function JourneyMap({
  steps,
  onPick,
}: {
  steps: JourneyStep[];
  onPick: (ev: CamEvent) => void;
}) {
  const [plan, setPlan] = useState<FloorPlan | null>(null);
  const [loaded, setLoaded] = useState(false);

  // The floor plan lives in Settings (Viewer-readable) — lazily fetched, same
  // parse the Map page does. Failure is non-fatal (renders the empty state).
  useEffect(() => {
    let alive = true;
    api.settings().then(
      (s) => {
        if (!alive) return;
        if (s.floorplan) {
          try {
            setPlan(JSON.parse(s.floorplan) as FloorPlan);
          } catch {
            /* malformed — treat as no plan */
          }
        }
        setLoaded(true);
      },
      () => alive && setLoaded(true),
    );
    return () => {
      alive = false;
    };
  }, []);

  const pinFor = (ev: CamEvent): Pin | null =>
    plan?.pins.find((p) => p.camera === ev.camera) ?? null;

  // Waypoints = the steps whose camera has a pin, keeping chronological order.
  const waypoints: Waypoint[] = [];
  steps.forEach((s, i) => {
    const pin = pinFor(s.ev);
    if (pin) waypoints.push({ n: i + 1, ev: s.ev, similarity: s.similarity, pin });
  });

  // Involved cameras with no pin can't be plotted — named in the narrative only.
  const involvedNames = [...new Set(steps.map((s) => s.ev.camera))];
  const unpinned = involvedNames.filter((name) => !plan?.pins.some((p) => p.camera === name));

  if (!loaded) {
    return (
      <span
        className="skeleton"
        style={{ height: 220, borderRadius: "var(--r-md)", display: "block" }}
        aria-busy="true"
      />
    );
  }

  if (!plan?.image || waypoints.length === 0) {
    return (
      <EmptyState
        icon={<IconMap />}
        title="No map path yet"
        hint={
          !plan?.image
            ? "Add a floor plan and drop camera pins on the Map page to see this path drawn across your property."
            : "None of the cameras in this path have a pin yet. Add camera pins on the Map page to see the route."
        }
        action={
          <a className="btn btn-ghost" href="#/map">
            Add camera pins on the Map page
          </a>
        }
      />
    );
  }

  // One node per involved pinned camera; the polyline threads every waypoint in
  // chronological order, so a revisit shows as repeated numbers at the same pin.
  const byCamera = new Map<string, Waypoint[]>();
  for (const w of waypoints) {
    const list = byCamera.get(w.ev.camera) ?? [];
    list.push(w);
    byCamera.set(w.ev.camera, list);
  }
  const polyPoints = waypoints
    .map((w) => `${(w.pin.x * 100).toFixed(2)},${(w.pin.y * 100).toFixed(2)}`)
    .join(" ");

  return (
    <div className="journey-map">
      <div className="fp-wrap journey-fp">
        <img src={plan.image} alt="Floor plan with the appearance path" className="fp-img" />
        {waypoints.length > 1 && (
          <svg className="journey-svg" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
            <polyline className="journey-path" points={polyPoints} />
          </svg>
        )}
        {[...byCamera.entries()].map(([camera, ws]) => (
          <div
            key={camera}
            className="journey-node"
            style={{ left: `${ws[0].pin.x * 100}%`, top: `${ws[0].pin.y * 100}%` }}
          >
            <span className="journey-nums">
              {ws.map((w) => (
                <button
                  key={w.ev.id}
                  className="journey-wp"
                  title={`Step ${w.n} · ${camera} · ${
                    w.similarity == null
                      ? "the event you started from"
                      : `${(w.similarity * 100).toFixed(0)}% match`
                  } · ${fmtTime(w.ev.ts)}`}
                  onClick={() => onPick(w.ev)}
                >
                  {w.n}
                </button>
              ))}
            </span>
            <span className="journey-node-label">{camera}</span>
          </div>
        ))}
      </div>
      {unpinned.length > 0 && (
        <p className="muted journey-unpinned">
          Not on the map (no pin): {unpinned.join(", ")}. Add pins on the{" "}
          <a href="#/map">Map page</a> to include them in the path.
        </p>
      )}
    </div>
  );
}
