import { FormEvent, useEffect, useState } from "react";
import { api, Camera, DetectConfig, StatusMap, Zone } from "../api";

function TuneModal({
  camera,
  onClose,
  onSaved,
  onError,
}: {
  camera: Camera;
  onClose: () => void;
  onSaved: () => void;
  onError: (e: string) => void;
}) {
  const [dc, setDc] = useState<DetectConfig>({
    labels: camera.detect_config.labels,
    min_score: camera.detect_config.min_score,
    motion_threshold: camera.detect_config.motion_threshold,
    ignore_zones: [...camera.detect_config.ignore_zones],
    autotrack: camera.detect_config.autotrack ?? false,
  });
  const [subSource, setSubSource] = useState(camera.detect_source ?? "");

  const setZone = (i: number, field: keyof Zone, v: number) => {
    const zones = dc.ignore_zones.map((z, j) => (j === i ? { ...z, [field]: v } : z));
    setDc({ ...dc, ignore_zones: zones });
  };

  const save = async () => {
    try {
      await api.patchCamera(camera.id, {
        detect_config: dc,
        detect_source: subSource.trim(),
      } as Partial<Camera>);
      onSaved();
      onClose();
    } catch (e) {
      onError(String(e));
    }
  };

  return (
    <div className="modal-bg" onClick={onClose}>
      <div className="card" style={{ minWidth: 540 }} onClick={(e) => e.stopPropagation()}>
        <h2>Detection tuning — {camera.name}</h2>
        <p className="muted" style={{ marginTop: 0 }}>
          Empty fields inherit the global Settings values.
        </p>
        <div className="row" style={{ marginBottom: 10 }}>
          <label className="field" style={{ flex: 1, minWidth: 380 }}>
            low-res sub-stream for detection (empty = detect on main stream)
            <input
              type="text"
              placeholder="rtsp://user:pass@cam/...subtype=1"
              value={subSource}
              onChange={(e) => setSubSource(e.target.value)}
            />
          </label>
        </div>
        <div className="row">
          <label className="field" style={{ flex: 1 }}>
            objects (comma-separated override)
            <input
              type="text"
              value={dc.labels ? dc.labels.join(", ") : ""}
              placeholder="inherit global"
              onChange={(e) => {
                const v = e.target.value.trim();
                setDc({
                  ...dc,
                  labels: v === "" ? null : v.split(",").map((x) => x.trim()).filter(Boolean),
                });
              }}
            />
          </label>
          <label className="field">
            min score (0-1)
            <input
              type="number" step="0.05" min="0" max="1"
              value={dc.min_score ?? ""}
              placeholder="inherit"
              onChange={(e) =>
                setDc({ ...dc, min_score: e.target.value === "" ? null : Number(e.target.value) })
              }
            />
          </label>
          <label className="field">
            motion threshold (0-1)
            <input
              type="number" step="0.005" min="0" max="1"
              value={dc.motion_threshold ?? ""}
              placeholder="inherit"
              onChange={(e) =>
                setDc({
                  ...dc,
                  motion_threshold: e.target.value === "" ? null : Number(e.target.value),
                })
              }
            />
          </label>
          <label className="toggle field">
            PTZ autotrack
            <input
              type="checkbox"
              checked={dc.autotrack}
              onChange={() => setDc({ ...dc, autotrack: !dc.autotrack })}
            />
          </label>
        </div>

        <h2 style={{ marginTop: 18 }}>Ignore zones</h2>
        <p className="muted" style={{ marginTop: 0 }}>
          Detections whose center falls inside a zone are dropped. Coordinates are fractions of
          the frame (0–1) from the top-left.
        </p>
        {dc.ignore_zones.map((z, i) => (
          <div className="row" key={i} style={{ marginBottom: 8 }}>
            {(["x", "y", "w", "h"] as const).map((f) => (
              <label className="field" key={f}>
                {f}
                <input
                  type="number" step="0.05" min="0" max="1" style={{ width: 80 }}
                  value={z[f]}
                  onChange={(e) => setZone(i, f, Number(e.target.value))}
                />
              </label>
            ))}
            <button
              className="danger"
              onClick={() => setDc({ ...dc, ignore_zones: dc.ignore_zones.filter((_, j) => j !== i) })}
            >
              remove
            </button>
          </div>
        ))}
        <div className="row" style={{ marginTop: 12 }}>
          <button
            className="ghost"
            onClick={() =>
              setDc({ ...dc, ignore_zones: [...dc.ignore_zones, { x: 0, y: 0, w: 0.25, h: 0.25 }] })
            }
          >
            + add zone
          </button>
          <div className="spacer" />
          <button className="ghost" onClick={onClose}>
            Cancel
          </button>
          <button className="primary" onClick={save}>
            Save
          </button>
        </div>
      </div>
    </div>
  );
}

export default function Cameras({
  cameras,
  onChange,
  onError,
}: {
  cameras: Camera[];
  onChange: () => void;
  onError: (e: string) => void;
}) {
  const [status, setStatus] = useState<StatusMap>({});
  const [tuning, setTuning] = useState<Camera | null>(null);

  useEffect(() => {
    const load = () => api.status().then(setStatus).catch(() => {});
    load();
    const t = setInterval(load, 5000);
    return () => clearInterval(t);
  }, []);
  const [name, setName] = useState("");
  const [source, setSource] = useState("");
  const [detectSource, setDetectSource] = useState("");
  const [detect, setDetect] = useState(true);
  const [record, setRecord] = useState(true);
  const [busy, setBusy] = useState(false);
  const [ip, setIp] = useState("");
  const [user, setUser] = useState("admin");
  const [pass, setPass] = useState("");
  const [found, setFound] = useState<string | null>(null);

  const resolve = async () => {
    setBusy(true);
    setFound(null);
    try {
      const r = await api.discover(ip.trim(), user, pass);
      const streams = r.sources.filter((s) => !s.url.includes("snapshot"));
      if (streams.length === 0) throw new Error("no streams found");
      setSource(streams[0].url);
      if (streams.length > 1) setDetectSource(streams[1].url);
      setFound(`${streams[0].name.replace(/ stream\d+$/, "")} — ${streams.length} streams`);
    } catch (e) {
      onError(`ONVIF resolve failed: ${e}`);
    } finally {
      setBusy(false);
    }
  };

  const add = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      await api.addCamera({
        name: name.trim(),
        source: source.trim(),
        detect_source: detectSource.trim() || undefined,
        detect,
        record,
      });
      setName("");
      setSource("");
      setDetectSource("");
      setFound(null);
      onChange();
    } catch (err) {
      onError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const toggle = async (cam: Camera, field: "enabled" | "detect" | "record") => {
    try {
      await api.patchCamera(cam.id, { [field]: !cam[field] });
      onChange();
    } catch (err) {
      onError(String(err));
    }
  };

  const remove = async (cam: Camera) => {
    if (!window.confirm(`Delete camera "${cam.name}"? Its events are removed too.`)) return;
    try {
      await api.deleteCamera(cam.id);
      onChange();
    } catch (err) {
      onError(String(err));
    }
  };

  return (
    <>
      <h1>Cameras</h1>

      <div className="card">
        <h2>Add camera</h2>
        <div className="row" style={{ marginBottom: 14 }}>
          <label className="field">
            camera IP / host
            <input type="text" placeholder="192.168.1.50" value={ip} onChange={(e) => setIp(e.target.value)} />
          </label>
          <label className="field">
            username
            <input type="text" value={user} onChange={(e) => setUser(e.target.value)} />
          </label>
          <label className="field">
            password
            <input type="text" value={pass} onChange={(e) => setPass(e.target.value)} />
          </label>
          <button type="button" className="ghost" disabled={busy || !ip.trim()} onClick={resolve}>
            🔍 Resolve via ONVIF
          </button>
          {found && <span style={{ color: "var(--ok)" }}>✓ {found} (form filled below)</span>}
        </div>
        <form onSubmit={add} className="row">
          <label className="field">
            name
            <input
              type="text"
              placeholder="front-door"
              value={name}
              onChange={(e) => setName(e.target.value)}
              required
            />
          </label>
          <label className="field" style={{ flex: 1, minWidth: 280 }}>
            source (RTSP URL or any go2rtc source)
            <input
              type="text"
              placeholder="rtsp://user:pass@192.168.1.50:554/stream1"
              value={source}
              onChange={(e) => setSource(e.target.value)}
              required
              style={{ width: "100%" }}
            />
          </label>
          <label className="field" style={{ flex: 1, minWidth: 220 }}>
            sub-stream for detection (optional)
            <input
              type="text"
              placeholder="auto-filled by ONVIF resolve"
              value={detectSource}
              onChange={(e) => setDetectSource(e.target.value)}
              style={{ width: "100%" }}
            />
          </label>
          <label className="toggle">
            <input type="checkbox" checked={detect} onChange={() => setDetect(!detect)} /> detect
          </label>
          <label className="toggle">
            <input type="checkbox" checked={record} onChange={() => setRecord(!record)} /> record
          </label>
          <button className="primary" disabled={busy}>
            Add
          </button>
        </form>
        <p className="muted" style={{ marginBottom: 0 }}>
          Names: lowercase letters, digits, "-", "_". The source is passed to go2rtc verbatim, so{" "}
          <code>rtsp://</code>, <code>ffmpeg:</code>, <code>exec:</code>… all work.
        </p>
      </div>

      <div className="card">
        <h2>Registered</h2>
        {cameras.length === 0 ? (
          <p className="muted">No cameras registered.</p>
        ) : (
          <div className="table-scroll">
          <table>
            <thead>
              <tr>
                <th>Status</th>
                <th>Name</th>
                <th>Source</th>
                <th>Enabled</th>
                <th>Detect</th>
                <th>Record</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {cameras.map((cam) => (
                <tr key={cam.id}>
                  <td title={status[String(cam.id)]?.last_error ?? ""}>
                    <span
                      className={`dot ${
                        status[String(cam.id)] ? (status[String(cam.id)].online ? "on" : "off") : ""
                      }`}
                    />{" "}
                    <span className="muted">
                      {status[String(cam.id)]?.online
                        ? "online"
                        : status[String(cam.id)]
                          ? "offline"
                          : "…"}
                    </span>
                  </td>
                  <td>
                    <b>{cam.name}</b>
                  </td>
                  <td className="muted" style={{ maxWidth: 360, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                    {cam.source}
                  </td>
                  {(["enabled", "detect", "record"] as const).map((f) => (
                    <td key={f}>
                      <span className={`pill toggle ${cam[f] ? "on" : ""}`} onClick={() => toggle(cam, f)}>
                        {cam[f] ? "on" : "off"}
                      </span>
                    </td>
                  ))}
                  <td>
                    <button className="ghost" onClick={() => setTuning(cam)} style={{ marginRight: 8 }}>
                      Tune
                    </button>
                    <button className="danger" onClick={() => remove(cam)}>
                      Delete
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          </div>
        )}
      </div>

      {tuning && (
        <TuneModal
          camera={tuning}
          onClose={() => setTuning(null)}
          onSaved={onChange}
          onError={onError}
        />
      )}
    </>
  );
}
