import { Camera, FloorPlan, StatusMap } from "./api";

/// P2.18 — Camera hotspots: clickable chips overlaid on the expanded live view
/// (CameraDetail) that jump to physically ADJACENT cameras, so a viewer
/// following someone hops camera-to-camera with one click.
///
/// Adjacency is derived (option b) from the FloorPlan pins already stored in
/// `Settings.floorplan` — NO new storage, NO backend. Presentational only.
/// Renders nothing (inert graceful degradation) when the current camera isn't
/// on the floor plan, or has no pinned, visible neighbors.
export default function CameraHotspots({
  camera,
  cameras,
  pins,
  status,
  max = 3,
}: {
  camera: Camera;
  cameras: Camera[];
  pins: FloorPlan["pins"];
  status: StatusMap;
  /** Nearest-K neighbors to surface as hotspots. */
  max?: number;
}) {
  // The current camera must itself be placed on the plan to have a position to
  // measure adjacency from. No pin → nothing to anchor against.
  const self = pins.find((p) => p.camera === camera.name);
  if (!self) return null;

  // Nearest pinned neighbors by Euclidean distance in the 0..1 plane. Each must
  // resolve to a camera present in the passed (RBAC-scoped) `cameras` list — a
  // camera the viewer can't see, or one deleted since the plan was drawn, must
  // NOT become a clickable dead link (RBAC + existence guard).
  const neighbors = pins
    .filter((p) => p.camera !== camera.name)
    .map((p) => ({
      cam: cameras.find((c) => c.name === p.camera),
      dist: Math.hypot(p.x - self.x, p.y - self.y),
    }))
    .filter((n): n is { cam: Camera; dist: number } => !!n.cam)
    .sort((a, b) => a.dist - b.dist)
    .slice(0, max);

  if (neighbors.length === 0) return null;

  return (
    <div className="hotspot-layer">
      <div className="hotspot-strip" role="group" aria-label="Nearby cameras">
        <span className="hotspot-title">Nearby</span>
        {neighbors.map(({ cam }) => {
          const online = status[String(cam.id)]?.online;
          return (
            <button
              key={cam.id}
              type="button"
              className="hotspot-chip"
              aria-label={`Jump to ${cam.name}${online ? "" : " (offline)"}`}
              onClick={() => {
                // Same navigation as Live.showCamera / App.openCamera — the hash
                // is the single source of truth for the open camera.
                window.location.hash = `#/live/${cam.id}`;
              }}
            >
              <span className={`fp-dot ${online ? "on" : "off"}`} aria-hidden="true" />
              <span className="fp-label">{cam.name}</span>
            </button>
          );
        })}
      </div>
    </div>
  );
}
