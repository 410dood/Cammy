// C6 — Floor-plan / map: upload a plan of your property and drop camera markers
// on it. In view mode a marker glows by online status and opens the camera live;
// in edit mode you place/remove markers. Persisted client-resized in Settings.

import { ChangeEvent, MouseEvent, useEffect, useRef, useState } from "react";
import { api, Camera, FloorPlan, Settings, StatusMap } from "../api";
import { useToast, TogglePill, ErrorState, EmptyState, usePolling } from "../ui";
import { IconUpload, IconVideo, IconCheck, IconMap } from "../icons";

async function resizeToDataUrl(file: File, maxDim: number): Promise<string> {
  const bitmap = await createImageBitmap(file);
  const scale = Math.min(1, maxDim / Math.max(bitmap.width, bitmap.height));
  const w = Math.round(bitmap.width * scale);
  const h = Math.round(bitmap.height * scale);
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  canvas.getContext("2d")!.drawImage(bitmap, 0, 0, w, h);
  return canvas.toDataURL("image/jpeg", 0.85);
}

export default function FloorPlanPage({
  cameras,
  onOpenCamera,
}: {
  cameras: Camera[];
  onOpenCamera: (c: Camera) => void;
}) {
  const toast = useToast();
  const [plan, setPlan] = useState<FloorPlan>({ image: "", pins: [] });
  const [editing, setEditing] = useState(false);
  const [placing, setPlacing] = useState<number | "">("");
  // Suppresses the click-to-remove that follows a drag's pointerup.
  const draggedRef = useRef(false);
  const [status, setStatus] = useState<StatusMap>({});
  const [loaded, setLoaded] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const settingsRef = useRef<Settings | null>(null);
  const wrapRef = useRef<HTMLDivElement>(null);

  const loadPlan = () => {
    api
      .settings()
      .then((s) => {
        settingsRef.current = s;
        setLoadError(null);
        if (s.floorplan) {
          try {
            const p = JSON.parse(s.floorplan) as FloorPlan;
            // P3 heal: pins used to key by camera NAME, so a rename orphaned
            // the pin (and the hotspots derived from it). Stamp the stable id
            // from the name, and refresh stale names from the id, persisting
            // the repair so every name-based consumer sees current names.
            let changed = false;
            const pins = p.pins.map((pin) => {
              const cam =
                (pin.camera_id != null && cameras.find((c) => c.id === pin.camera_id)) ||
                cameras.find((c) => c.name === pin.camera);
              if (!cam || (pin.camera_id === cam.id && pin.camera === cam.name)) return pin;
              changed = true;
              return { ...pin, camera_id: cam.id, camera: cam.name };
            });
            const healed = { ...p, pins };
            setPlan(healed);
            if (changed) {
              const updated = { ...s, floorplan: JSON.stringify(healed) };
              settingsRef.current = updated;
              api.saveSettings(updated).catch(() => {});
            }
          } catch {
            /* ignore malformed */
          }
        }
      })
      .catch((e) => setLoadError(String(e)))
      .finally(() => setLoaded(true));
  };

  useEffect(() => {
    loadPlan();
    // Re-run when the camera list arrives/changes so the name↔id heal can see
    // real cameras (the first render's list is often still empty).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cameras.length]);
  // The Map's whole value is live online/offline dots — poll status (paused when
  // the tab is hidden) so a camera dropping while the Map is open doesn't show a
  // stale green dot indefinitely.
  usePolling(() => {
    api.status().then(setStatus).catch(() => {});
  }, 10000);

  const save = async (next: FloorPlan) => {
    setPlan(next);
    const s = settingsRef.current;
    if (!s) return;
    const updated = { ...s, floorplan: JSON.stringify(next) };
    settingsRef.current = updated;
    try {
      await api.saveSettings(updated);
    } catch (e) {
      toast.error(`Couldn't save floor plan: ${e}`);
    }
  };

  const onFile = async (e: ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    try {
      const url = await resizeToDataUrl(file, 1600);
      await save({ ...plan, image: url });
      setEditing(true);
      toast.success("Floor plan uploaded — place your cameras");
    } catch {
      toast.error("Couldn't read that image");
    }
  };

  const onMapClick = (e: MouseEvent<HTMLDivElement>) => {
    if (!editing || placing === "" || !wrapRef.current) return;
    const cam = cameras.find((c) => c.id === placing);
    if (!cam) return;
    const rect = wrapRef.current.getBoundingClientRect();
    const x = Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
    const y = Math.min(1, Math.max(0, (e.clientY - rect.top) / rect.height));
    save({
      ...plan,
      pins: [
        ...plan.pins.filter((p) => !pinIsFor(p, cam)),
        { camera: cam.name, camera_id: cam.id, x, y },
      ],
    });
    setPlacing("");
  };

  const pinIsFor = (p: FloorPlan["pins"][number], c: Camera) =>
    p.camera_id != null ? p.camera_id === c.id : p.camera === c.name;

  /// P3 drag-to-move: press a pin in edit mode and drag it; a press that never
  /// really moves stays a click (= remove, the existing gesture).
  const startDrag = (pinKey: string) => (e: React.PointerEvent) => {
    if (!editing || !wrapRef.current) return;
    e.preventDefault();
    e.stopPropagation();
    const rect = wrapRef.current.getBoundingClientRect();
    const startX = e.clientX;
    const startY = e.clientY;
    let moved = false;
    let last: { x: number; y: number } | null = null;
    const move = (ev: PointerEvent) => {
      if (!moved && Math.hypot(ev.clientX - startX, ev.clientY - startY) < 5) return;
      moved = true;
      const x = Math.min(1, Math.max(0, (ev.clientX - rect.left) / rect.width));
      const y = Math.min(1, Math.max(0, (ev.clientY - rect.top) / rect.height));
      last = { x, y };
      // Live visual feedback without a save per mousemove.
      setPlan((cur) => ({
        ...cur,
        pins: cur.pins.map((p) => (pinDomKey(p) === pinKey ? { ...p, x, y } : p)),
      }));
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
      if (moved && last) {
        draggedRef.current = true; // swallow the click that follows this pointerup
        setPlan((cur) => {
          const next = {
            ...cur,
            pins: cur.pins.map((p) => (pinDomKey(p) === pinKey ? { ...p, ...last! } : p)),
          };
          void save(next);
          return next;
        });
      }
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
  };

  const pinDomKey = (p: FloorPlan["pins"][number]) =>
    p.camera_id != null ? `id:${p.camera_id}` : `name:${p.camera}`;

  // docs/11 P3 — FOV cones. The cone is presentational (which way the camera
  // points, not a calibrated view field); the bearing comes from dragging the
  // small handle that orbits each pin in edit mode. Cone geometry is computed
  // in PIXELS (via a measured wrap size), because percentage coordinates
  // distort angles on a non-square plan image.
  const [wrapSize, setWrapSize] = useState<{ w: number; h: number } | null>(null);
  useEffect(() => {
    const el = wrapRef.current;
    if (!el || !plan.image) return;
    const measure = () => {
      const r = el.getBoundingClientRect();
      if (r.width > 0 && r.height > 0) setWrapSize({ w: r.width, h: r.height });
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [plan.image]);

  /// Screen-space unit vector for a bearing (0° = up, clockwise).
  const dirVec = (deg: number) => {
    const rad = (deg * Math.PI) / 180;
    return { dx: Math.sin(rad), dy: -Math.cos(rad) };
  };

  /// Drag the direction handle: the bearing tracks the pointer's angle from
  /// the pin; a press that never moves clears the bearing (removes the cone).
  const startDirDrag = (pinKey: string, pin: { x: number; y: number }) => (e: React.PointerEvent) => {
    if (!editing || !wrapRef.current) return;
    e.preventDefault();
    e.stopPropagation();
    const rect = wrapRef.current.getBoundingClientRect();
    const cx = rect.left + pin.x * rect.width;
    const cy = rect.top + pin.y * rect.height;
    const startX = e.clientX;
    const startY = e.clientY;
    let moved = false;
    let lastDir: number | null = null;
    const move = (ev: PointerEvent) => {
      if (!moved && Math.hypot(ev.clientX - startX, ev.clientY - startY) < 5) return;
      moved = true;
      const deg = (Math.atan2(ev.clientX - cx, cy - ev.clientY) * 180) / Math.PI;
      lastDir = Math.round((deg + 360) % 360);
      setPlan((cur) => ({
        ...cur,
        pins: cur.pins.map((p) => (pinDomKey(p) === pinKey ? { ...p, dir: lastDir! } : p)),
      }));
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
      setPlan((cur) => {
        const next = {
          ...cur,
          pins: cur.pins.map((p) =>
            pinDomKey(p) === pinKey
              ? moved && lastDir != null
                ? { ...p, dir: lastDir }
                : { ...p, dir: undefined }
              : p,
          ),
        };
        void save(next);
        return next;
      });
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
  };

  return (
    <>
      <div className="row" style={{ alignItems: "center" }}>
        <h1 style={{ marginRight: "auto" }}>Map</h1>
        {plan.image && (
          <>
            <button className={`btn ${editing ? "btn-primary" : "btn-ghost"}`} onClick={() => setEditing((v) => !v)}>
              {editing ? "Done" : "Edit pins"}
            </button>
            <label className="btn btn-ghost file-btn">
              <IconUpload size={15} /> Replace
              <input type="file" accept="image/*" onChange={onFile} />
            </label>
          </>
        )}
      </div>

      {!loaded ? (
        <span className="skeleton" style={{ height: 280, borderRadius: "var(--radius)" }} aria-busy="true" />
      ) : loadError && !plan.image ? (
        <ErrorState what="your floor plan" message={loadError} onRetry={loadPlan} />
      ) : !plan.image ? (
        <EmptyState
          icon={<IconMap />}
          title="Upload a floor plan"
          hint="A PNG or JPG of your home or property. Then drop camera markers onto it and click a marker to jump to that camera live."
          action={
            <label className="btn btn-primary file-btn">
              <IconUpload size={15} /> Choose image
              <input type="file" accept="image/*" onChange={onFile} />
            </label>
          }
        />
      ) : (
        <>
          {editing && (
            <div className="row" style={{ marginBottom: 10, flexWrap: "wrap" }}>
              <span className="muted">Place a camera:</span>
              {cameras.map((c) => (
                <TogglePill
                  key={c.id}
                  on={placing === c.id}
                  ariaLabel={`Place ${c.name} on the floor plan`}
                  onClick={() => setPlacing(placing === c.id ? "" : c.id)}
                >
                  {c.name}
                  {plan.pins.some((p) => pinIsFor(p, c)) && <IconCheck size={12} />}
                </TogglePill>
              ))}
              {placing !== "" && (
                <span className="muted">
                  click the map to place “{cameras.find((c) => c.id === placing)?.name}”
                </span>
              )}
              {placing === "" && (
                <span className="muted">drag a marker to move it · click a marker to remove it</span>
              )}
            </div>
          )}
          <div
            className="fp-wrap"
            ref={wrapRef}
            onClick={onMapClick}
            style={{ cursor: editing && placing ? "crosshair" : "default" }}
          >
            <img src={plan.image} alt="floor plan" className="fp-img" />
            {wrapSize && plan.pins.some((p) => p.dir != null) && (
              <svg
                className="fp-cones"
                width={wrapSize.w}
                height={wrapSize.h}
                viewBox={`0 0 ${wrapSize.w} ${wrapSize.h}`}
                aria-hidden="true"
              >
                {plan.pins
                  .filter((p) => p.dir != null)
                  .map((pin) => {
                    const px = pin.x * wrapSize.w;
                    const py = pin.y * wrapSize.h;
                    const r = Math.max(60, Math.min(wrapSize.w, wrapSize.h) * 0.16);
                    const half = 30; // degrees each side — presentational
                    const a = dirVec(pin.dir! - half);
                    const b = dirVec(pin.dir! + half);
                    return (
                      <path
                        key={pinDomKey(pin)}
                        className="fp-cone"
                        d={`M ${px} ${py} L ${px + a.dx * r} ${py + a.dy * r} A ${r} ${r} 0 0 1 ${px + b.dx * r} ${py + b.dy * r} Z`}
                      />
                    );
                  })}
              </svg>
            )}
            {editing &&
              wrapSize &&
              plan.pins.map((pin) => {
                const key = pinDomKey(pin);
                const v = dirVec(pin.dir ?? 0);
                const hx = pin.x * wrapSize.w + v.dx * 46;
                const hy = pin.y * wrapSize.h + v.dy * 46;
                return (
                  <button
                    key={`dir-${key}`}
                    type="button"
                    className={`fp-dir-handle ${pin.dir != null ? "set" : ""}`}
                    style={{ left: hx, top: hy, touchAction: "none" }}
                    title={
                      pin.dir != null
                        ? `Drag to aim ${pin.camera}'s view cone · click to remove the cone`
                        : `Drag to show which way ${pin.camera} points`
                    }
                    aria-label={`Set ${pin.camera}'s facing direction`}
                    onPointerDown={startDirDrag(key, pin)}
                    onClick={(e) => e.stopPropagation()}
                  />
                );
              })}
            {plan.pins.map((pin) => {
              const cam =
                (pin.camera_id != null && cameras.find((c) => c.id === pin.camera_id)) ||
                cameras.find((c) => c.name === pin.camera) ||
                null;
              const name = cam?.name ?? pin.camera;
              const online = cam && status[String(cam.id)]?.online;
              const key = pinDomKey(pin);
              return (
                <button
                  key={key}
                  className="fp-pin"
                  style={{
                    left: `${pin.x * 100}%`,
                    top: `${pin.y * 100}%`,
                    touchAction: editing ? "none" : undefined,
                    cursor: editing ? "grab" : undefined,
                  }}
                  title={editing ? `Drag to move · click to remove ${name}` : `Open ${name}`}
                  onPointerDown={editing ? startDrag(key) : undefined}
                  onClick={(e) => {
                    e.stopPropagation();
                    if (draggedRef.current) {
                      draggedRef.current = false; // that was a drag, not a click
                      return;
                    }
                    if (editing) save({ ...plan, pins: plan.pins.filter((p) => pinDomKey(p) !== key) });
                    else if (cam) onOpenCamera(cam);
                  }}
                >
                  <span className={`fp-dot ${online ? "on" : "off"}`} />
                  <IconVideo size={13} />
                  <span className="fp-label">{name}</span>
                </button>
              );
            })}
          </div>
        </>
      )}
    </>
  );
}
