// C6 — Floor-plan / map: upload a plan of your property and drop camera markers
// on it. In view mode a marker glows by online status and opens the camera live;
// in edit mode you place/remove markers. Persisted client-resized in Settings.

import { ChangeEvent, MouseEvent, useEffect, useRef, useState } from "react";
import { api, Camera, FloorPlan, Settings, StatusMap } from "../api";
import { useToast } from "../ui";
import { IconUpload, IconVideo } from "../icons";

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
  const [placing, setPlacing] = useState("");
  const [status, setStatus] = useState<StatusMap>({});
  const settingsRef = useRef<Settings | null>(null);
  const wrapRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    api
      .settings()
      .then((s) => {
        settingsRef.current = s;
        if (s.floorplan) {
          try {
            setPlan(JSON.parse(s.floorplan));
          } catch {
            /* ignore malformed */
          }
        }
      })
      .catch(() => {});
    api.status().then(setStatus).catch(() => {});
  }, []);

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
    if (!editing || !placing || !wrapRef.current) return;
    const rect = wrapRef.current.getBoundingClientRect();
    const x = Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
    const y = Math.min(1, Math.max(0, (e.clientY - rect.top) / rect.height));
    save({ ...plan, pins: [...plan.pins.filter((p) => p.camera !== placing), { camera: placing, x, y }] });
    setPlacing("");
  };

  return (
    <>
      <div className="row" style={{ alignItems: "center" }}>
        <h1 style={{ marginRight: "auto" }}>Floor plan</h1>
        {plan.image && (
          <>
            <button className={`btn ${editing ? "btn-primary" : "btn-ghost"}`} onClick={() => setEditing((v) => !v)}>
              {editing ? "Done" : "Edit pins"}
            </button>
            <label className="btn btn-ghost">
              <IconUpload size={15} /> Replace
              <input type="file" accept="image/*" style={{ display: "none" }} onChange={onFile} />
            </label>
          </>
        )}
      </div>

      {!plan.image ? (
        <label className="empty fp-drop">
          <IconUpload size={22} />
          <b>Upload a floor plan</b>
          <p className="muted" style={{ margin: 0 }}>
            A PNG or JPG of your home or property. Then drop camera markers onto it and click a
            marker to jump to that camera live.
          </p>
          <input type="file" accept="image/*" style={{ display: "none" }} onChange={onFile} />
        </label>
      ) : (
        <>
          {editing && (
            <div className="row" style={{ marginBottom: 10, flexWrap: "wrap" }}>
              <span className="muted">Place a camera:</span>
              {cameras.map((c) => (
                <span
                  key={c.id}
                  className={`pill toggle ${placing === c.name ? "on" : ""}`}
                  onClick={() => setPlacing(placing === c.name ? "" : c.name)}
                >
                  {c.name}
                  {plan.pins.some((p) => p.camera === c.name) ? " ✓" : ""}
                </span>
              ))}
              {placing && <span className="muted">click the map to place “{placing}” (or a marker to remove it)</span>}
            </div>
          )}
          <div
            className="fp-wrap"
            ref={wrapRef}
            onClick={onMapClick}
            style={{ cursor: editing && placing ? "crosshair" : "default" }}
          >
            <img src={plan.image} alt="floor plan" className="fp-img" />
            {plan.pins.map((pin) => {
              const cam = cameras.find((c) => c.name === pin.camera);
              const online = cam && status[String(cam.id)]?.online;
              return (
                <button
                  key={pin.camera}
                  className="fp-pin"
                  style={{ left: `${pin.x * 100}%`, top: `${pin.y * 100}%` }}
                  title={editing ? `Remove ${pin.camera}` : `Open ${pin.camera}`}
                  onClick={(e) => {
                    e.stopPropagation();
                    if (editing) save({ ...plan, pins: plan.pins.filter((p) => p.camera !== pin.camera) });
                    else if (cam) onOpenCamera(cam);
                  }}
                >
                  <span className={`fp-dot ${online ? "on" : "off"}`} />
                  <IconVideo size={13} />
                  <span className="fp-label">{pin.camera}</span>
                </button>
              );
            })}
          </div>
        </>
      )}
    </>
  );
}
