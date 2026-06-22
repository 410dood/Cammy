import { FormEvent, useEffect, useState } from "react";
import { api, Camera, DetectConfig, DiscoveredCam, StatusMap, Zone } from "../api";
import ZoneEditor from "../ZoneEditor";
import { Modal } from "../ui";
import { IconRadar, IconSearch, IconCheck } from "../icons";

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
    zones: camera.detect_config.zones ? [...camera.detect_config.zones] : [],
    tripwires: camera.detect_config.tripwires ? [...camera.detect_config.tripwires] : [],
    ground_calib: camera.detect_config.ground_calib ?? null,
    privacy_masks: camera.detect_config.privacy_masks ? [...camera.detect_config.privacy_masks] : [],
    min_area: camera.detect_config.min_area ?? null,
    max_area: camera.detect_config.max_area ?? null,
    autotrack: camera.detect_config.autotrack ?? false,
    audio_detect: camera.detect_config.audio_detect ?? false,
    event_only_recording: camera.detect_config.event_only_recording ?? false,
    gesture_detect: camera.detect_config.gesture_detect ?? false,
    model: camera.detect_config.model ?? null,
    force_cpu: camera.detect_config.force_cpu ?? null,
    poll_ms: camera.detect_config.poll_ms ?? null,
    face_recognize: camera.detect_config.face_recognize ?? null,
    two_way_audio: camera.detect_config.two_way_audio ?? false,
    retention_days: camera.detect_config.retention_days ?? null,
    fall_detect: camera.detect_config.fall_detect ?? false,
    child_height_frac: camera.detect_config.child_height_frac ?? null,
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
    <Modal onClose={onClose} title={`Detection tuning — ${camera.name}`}>
      <div style={{ padding: "16px 18px 18px" }}>
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
          <label className="field" title="Drop detections smaller than this fraction of the frame area (kills far-field blips).">
            min object size (0-1)
            <input
              type="number" step="0.005" min="0" max="1"
              value={dc.min_area ?? ""}
              placeholder="none"
              onChange={(e) =>
                setDc({ ...dc, min_area: e.target.value === "" ? null : Number(e.target.value) })
              }
            />
          </label>
          <label className="field" title="Drop detections larger than this fraction of the frame area (kills whole-frame lighting flips).">
            max object size (0-1)
            <input
              type="number" step="0.05" min="0" max="1"
              value={dc.max_area ?? ""}
              placeholder="none"
              onChange={(e) =>
                setDc({ ...dc, max_area: e.target.value === "" ? null : Number(e.target.value) })
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
          <label className="toggle field">
            audio detection
            <input
              type="checkbox"
              checked={dc.audio_detect}
              onChange={() => setDc({ ...dc, audio_detect: !dc.audio_detect })}
            />
          </label>
          <label
            className="toggle field"
            title="Show a hold-to-talk button in this camera's detail view (streams your mic to the camera over WebRTC). Only works on cameras with a speaker / ONVIF backchannel."
          >
            two-way audio (talk)
            <input
              type="checkbox"
              checked={dc.two_way_audio}
              onChange={() => setDc({ ...dc, two_way_audio: !dc.two_way_audio })}
            />
          </label>
          <label
            className="toggle field"
            title="Offer the live hand-signal overlay (Signals page) for this camera."
          >
            hand signals
            <input
              type="checkbox"
              checked={dc.gesture_detect}
              onChange={() => setDc({ ...dc, gesture_detect: !dc.gesture_detect })}
            />
          </label>
          <label className="field" title="Per-camera model override (e.g. a specialized .onnx). Empty inherits the global model.">
            model override
            <input
              type="text"
              placeholder="inherit global"
              value={dc.model ?? ""}
              onChange={(e) => setDc({ ...dc, model: e.target.value.trim() || null })}
            />
          </label>
          <label className="field" title="Accelerator assignment for this camera's detector.">
            accelerator
            <select
              value={dc.force_cpu === null ? "" : dc.force_cpu ? "cpu" : "gpu"}
              onChange={(e) =>
                setDc({ ...dc, force_cpu: e.target.value === "" ? null : e.target.value === "cpu" })
              }
            >
              <option value="">inherit</option>
              <option value="gpu">GPU</option>
              <option value="cpu">CPU</option>
            </select>
          </label>
          <label className="field" title="Per-camera sample-interval cap (resource governance). Only slows this camera down.">
            FPS cap — sample every (ms)
            <input
              type="number" step="100" min="0"
              placeholder="inherit"
              value={dc.poll_ms ?? ""}
              onChange={(e) => setDc({ ...dc, poll_ms: e.target.value === "" ? null : Number(e.target.value) })}
            />
          </label>
          <label className="field" title="Opt this camera into (or out of) face recognition. Inherit uses the global Settings switch.">
            face recognition
            <select
              value={dc.face_recognize === null ? "" : dc.face_recognize ? "on" : "off"}
              onChange={(e) =>
                setDc({ ...dc, face_recognize: e.target.value === "" ? null : e.target.value === "on" })
              }
            >
              <option value="">inherit</option>
              <option value="on">on</option>
              <option value="off">off</option>
            </select>
          </label>
          <label
            className="toggle field"
            title="Keep only footage near events: segments with no detection within a segment-length margin are deleted after a 15-minute grace period. Saves most of the disk on quiet cameras."
          >
            event-only recording
            <input
              type="checkbox"
              checked={dc.event_only_recording}
              onChange={() => setDc({ ...dc, event_only_recording: !dc.event_only_recording })}
            />
          </label>
          <label
            className="field"
            title="Keep this camera's footage for a custom number of days (e.g. a doorbell 30, a quiet side camera 3). Blank inherits the global retention. The global disk size cap still applies as the safety net."
          >
            retention (days, blank = global)
            <input
              type="number"
              min="0"
              style={{ width: 110 }}
              value={dc.retention_days ?? ""}
              placeholder="global"
              onChange={(e) =>
                setDc({
                  ...dc,
                  retention_days: e.target.value === "" ? null : Math.max(0, Number(e.target.value) || 0),
                })
              }
            />
          </label>
          <label
            className="toggle field"
            title="Residential ASSISTIVE fall hint: a tracked person who goes motionless low in the frame fires a 'fall' event. Best-effort at ~1 fps — it MISSES occluded, soft, or slow falls. NOT a medical-alert device; pair it with a pendant and never auto-dial emergency services off a single visual trigger."
          >
            fall detection (assistive*)
            <input
              type="checkbox"
              checked={dc.fall_detect}
              onChange={() => setDc({ ...dc, fall_detect: !dc.fall_detect })}
            />
          </label>
          <label
            className="field"
            title="Residential child calibration: a tracked person whose normalized bbox HEIGHT (0..1 of the frame) is at/below this fraction is treated as a 'child', enabling the child / child-alone zone rules. Blank disables child features. FRAGILE — bbox height depends on camera angle/distance; tune per camera and treat results as a detection aid only."
          >
            child height ≤ (frac, blank = off*)
            <input
              type="number"
              step="0.05"
              min="0"
              max="1"
              style={{ width: 110 }}
              placeholder="off"
              value={dc.child_height_frac ?? ""}
              onChange={(e) =>
                setDc({
                  ...dc,
                  child_height_frac:
                    e.target.value === "" ? null : Math.min(1, Math.max(0, Number(e.target.value) || 0)),
                })
              }
            />
          </label>
        </div>
        <p className="muted" style={{ fontSize: 12, marginTop: -4 }}>
          * Fall detection and child classification are <b>assistive, best-effort</b>{" "}
          safety aids — not medical devices and not guaranteed. They can miss events
          and must never replace supervision or a personal alarm. See the zone editor
          below to arm child / unattended / pool-water hints.
        </p>

        <h2 style={{ marginTop: 18 }}>Zones &amp; privacy masks</h2>
        <p className="muted" style={{ marginTop: 0 }}>
          Draw polygons on the live frame. <b style={{ color: "#36d399" }}>Required</b> zones keep
          only objects inside them; <b style={{ color: "#f87272" }}>ignore</b> zones drop objects
          inside; <b style={{ color: "#a3a3a3" }}>privacy masks</b> are blacked out before any
          analysis or snapshot (continuous recordings are not masked).
        </p>
        <ZoneEditor
          camera={camera}
          zones={dc.zones}
          masks={dc.privacy_masks}
          tripwires={dc.tripwires ?? []}
          calib={dc.ground_calib ?? null}
          onChange={(zones, masks) => setDc({ ...dc, zones, privacy_masks: masks })}
          onTripwires={(tripwires) => setDc({ ...dc, tripwires })}
          onCalib={(ground_calib) => setDc({ ...dc, ground_calib })}
        />

        <h2 style={{ marginTop: 18 }}>Ignore zones (legacy rectangles)</h2>
        <p className="muted" style={{ marginTop: 0 }}>
          Detections whose center falls inside a rectangle are dropped. Coordinates are fractions of
          the frame (0–1) from the top-left. Prefer the polygon zones above for new setups.
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
    </Modal>
  );
}

/// Inline camera-name editor: commits on blur/Enter. Renaming restarts go2rtc
/// (a brief live-stream blip) since the stream name changes. Names are
/// lowercase letters/digits/_/- (≤32); the server rejects others and we revert.
function NameCell({
  cam,
  onChange,
  onError,
}: {
  cam: Camera;
  onChange: () => void;
  onError: (e: string) => void;
}) {
  const [val, setVal] = useState(cam.name);
  useEffect(() => {
    setVal(cam.name);
  }, [cam.name]);
  const commit = async () => {
    const next = val.trim();
    if (next === cam.name) return;
    if (!next) {
      setVal(cam.name); // a name can't be empty
      return;
    }
    try {
      await api.patchCamera(cam.id, { name: next } as Partial<Camera>);
      onChange();
    } catch (e) {
      setVal(cam.name); // revert on rejection (e.g. invalid chars)
      onError(String(e));
    }
  };
  return (
    <input
      className="field"
      style={{ width: 130, fontWeight: 600 }}
      value={val}
      onChange={(e) => setVal(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
      }}
      title="Rename (lowercase/digits/_/-; restarts the stream briefly)"
    />
  );
}

/// Inline group editor: commits on blur/Enter; empty string clears the group.
/// Patching only `group` is metadata-only, so the server skips the go2rtc
/// restart and live streams keep playing.
function GroupCell({
  cam,
  onChange,
  onError,
}: {
  cam: Camera;
  onChange: () => void;
  onError: (e: string) => void;
}) {
  const [val, setVal] = useState(cam.group ?? "");
  useEffect(() => {
    setVal(cam.group ?? "");
  }, [cam.group]);
  const commit = async () => {
    const next = val.trim();
    if (next === (cam.group ?? "")) return;
    try {
      await api.patchCamera(cam.id, { group: next } as Partial<Camera>);
      onChange();
    } catch (e) {
      onError(String(e));
    }
  };
  return (
    <input
      className="field"
      list="cam-groups"
      placeholder="—"
      style={{ width: 110 }}
      value={val}
      onChange={(e) => setVal(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
      }}
    />
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
  const [group, setGroup] = useState("");
  const [detect, setDetect] = useState(true);
  const [record, setRecord] = useState(true);
  const [busy, setBusy] = useState(false);
  const [ip, setIp] = useState("");
  const [user, setUser] = useState("admin");
  const [pass, setPass] = useState("");
  const [found, setFound] = useState<string | null>(null);
  const [scanning, setScanning] = useState(false);
  const [scanned, setScanned] = useState<DiscoveredCam[] | null>(null);

  const scan = async () => {
    setScanning(true);
    try {
      const r = await api.scanNetwork();
      setScanned(r.cameras);
    } catch (e) {
      onError(`network scan failed: ${e}`);
    } finally {
      setScanning(false);
    }
  };

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
        group: group.trim() || undefined,
        detect,
        record,
      });
      setName("");
      setSource("");
      setDetectSource("");
      setGroup("");
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

  const groups = Array.from(
    new Set(cameras.map((c) => c.group).filter((g): g is string => !!g)),
  ).sort();

  return (
    <>
      <h1>Cameras</h1>
      <datalist id="cam-groups">
        {groups.map((g) => (
          <option key={g} value={g} />
        ))}
      </datalist>

      <div className="card">
        <h2>Add camera</h2>
        <div className="row" style={{ marginBottom: 10 }}>
          <button type="button" className="btn btn-ghost" disabled={scanning} onClick={scan}>
            {scanning ? "Scanning…" : (<><IconRadar size={15} /> Scan network for cameras</>)}
          </button>
          {scanned !== null && scanned.length === 0 && (
            <span className="muted">no ONVIF cameras responded</span>
          )}
          {scanned?.map((c) => (
            <span
              key={c.host}
              className={`pill toggle ${ip === c.host ? "on" : ""}`}
              title="click to fill the IP field"
              onClick={() => setIp(c.host)}
            >
              {c.host}
              {c.name ? ` — ${c.name}` : ""}
            </span>
          ))}
        </div>
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
          <button type="button" className="btn btn-ghost" disabled={busy || !ip.trim()} onClick={resolve}>
            <IconSearch size={15} /> Resolve via ONVIF
          </button>
          {found && (
            <span className="save-ok"><IconCheck size={14} /> {found} (form filled below)</span>
          )}
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
          <label className="field" style={{ minWidth: 130 }} title="Optional: group cameras for the Live view (e.g. 'outdoor', 'downstairs').">
            group (optional)
            <input
              type="text"
              list="cam-groups"
              placeholder="e.g. outdoor"
              value={group}
              onChange={(e) => setGroup(e.target.value)}
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
                <th>Group</th>
                <th>Perf</th>
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
                    <NameCell cam={cam} onChange={onChange} onError={onError} />
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
                    <GroupCell cam={cam} onChange={onChange} onError={onError} />
                  </td>
                  <td className="muted" style={{ whiteSpace: "nowrap" }}>
                    {(() => {
                      const s = status[String(cam.id)];
                      if (!s?.accelerator) return "—";
                      return `${s.inference_ms != null ? s.inference_ms.toFixed(1) + "ms · " : ""}${s.accelerator}`;
                    })()}
                  </td>
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
