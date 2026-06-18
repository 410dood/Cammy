import { useEffect, useRef, useState } from "react";
import { api, AppConfig, Camera, StatusMap, StreamMode, getStreamMode, setStreamMode } from "../api";
import CameraDetail from "../CameraDetail";
import LiveVideo from "../LiveVideo";

/// Hold-to-move PTZ pad, shown only on cameras that answer ONVIF PTZ.
function PtzPad({ cameraId }: { cameraId: number }) {
  const moving = useRef(false);

  const start = (pan: number, tilt: number, zoom: number) => {
    moving.current = true;
    api.ptz(cameraId, { action: "move", pan, tilt, zoom }).catch(() => {});
  };
  const stop = () => {
    if (!moving.current) return;
    moving.current = false;
    api.ptz(cameraId, { action: "stop" }).catch(() => {});
  };

  const btn = (label: string, pan: number, tilt: number, zoom: number) => (
    <button
      className="ptz-btn"
      onPointerDown={(e) => {
        e.preventDefault();
        start(pan, tilt, zoom);
      }}
      onPointerUp={stop}
      onPointerLeave={stop}
    >
      {label}
    </button>
  );

  return (
    <div className="ptz-pad">
      <span />
      {btn("▲", 0, 0.5, 0)}
      <span />
      {btn("◀", -0.5, 0, 0)}
      {btn("▼", 0, -0.5, 0)}
      {btn("▶", 0.5, 0, 0)}
      {btn("+", 0, 0, 0.5)}
      <span />
      {btn("−", 0, 0, -0.5)}
    </div>
  );
}

export default function Live({
  cameras,
  config,
}: {
  cameras: Camera[];
  config: AppConfig | null;
}) {
  const [status, setStatus] = useState<StatusMap>({});
  const [ptz, setPtz] = useState<Record<number, boolean>>({});
  const [detail, setDetail] = useState<Camera | null>(null);
  const [mode, setMode] = useState<StreamMode>(getStreamMode());
  const [group, setGroup] = useState<string>(() => localStorage.getItem("zoomy-live-group") || "All");

  useEffect(() => {
    const load = () => api.status().then(setStatus).catch(() => {});
    load();
    const t = setInterval(load, 5000);
    return () => clearInterval(t);
  }, []);

  useEffect(() => {
    cameras.forEach((cam) => {
      if (ptz[cam.id] === undefined) {
        api
          .ptzCaps(cam.id)
          .then((r) => setPtz((p) => ({ ...p, [cam.id]: r.supported })))
          .catch(() => setPtz((p) => ({ ...p, [cam.id]: false })));
      }
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cameras]);

  const enabled = cameras.filter((c) => c.enabled);
  const groups = Array.from(
    new Set(enabled.map((c) => c.group).filter((g): g is string => !!g)),
  ).sort();
  const hasUngrouped = enabled.some((c) => !c.group);

  // Snap a stale selection (its cameras were removed/regrouped) back to All.
  useEffect(() => {
    if (group !== "All" && group !== "Ungrouped" && !groups.includes(group)) setGroup("All");
    if (group === "Ungrouped" && !hasUngrouped) setGroup("All");
    // NUL separator can't appear in a typed group name, so the dep key is
    // unambiguous even for names containing punctuation.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [groups.join("\u0000"), hasUngrouped]);

  const pickGroup = (g: string) => {
    setGroup(g);
    localStorage.setItem("zoomy-live-group", g);
  };

  const live = enabled.filter((c) =>
    group === "All" ? true : group === "Ungrouped" ? !c.group : c.group === group,
  );

  if (!config) return <p className="muted">Connecting…</p>;
  if (enabled.length === 0)
    return (
      <>
        <h1>Live</h1>
        <div className="empty">
          No cameras yet — add one on the <b>Cameras</b> page.
        </div>
      </>
    );

  return (
    <>
      <div className="row" style={{ alignItems: "center" }}>
        <h1 style={{ marginRight: "auto" }}>Live</h1>
        <label className="field" title="go2rtc restreams one camera connection to all viewers; pick the transport that works best on your network.">
          transport
          <select
            value={mode}
            onChange={(e) => {
              const m = e.target.value as StreamMode;
              setMode(m);
              setStreamMode(m);
            }}
          >
            <option value="webrtc">WebRTC (lowest latency)</option>
            <option value="mse">MSE (compatible, over TCP)</option>
            <option value="mjpeg">MJPEG (most compatible)</option>
          </select>
        </label>
      </div>
      {groups.length > 0 && (
        <div className="row" style={{ gap: 6, flexWrap: "wrap", marginBottom: 12 }}>
          {["All", ...groups, ...(hasUngrouped ? ["Ungrouped"] : [])].map((g) => (
            <span
              key={g}
              className={`pill toggle ${group === g ? "on" : ""}`}
              onClick={() => pickGroup(g)}
            >
              {g}
            </span>
          ))}
        </div>
      )}
      {live.length === 0 ? (
        <div className="empty">No cameras in “{group}”.</div>
      ) : (
      <div className="live-grid">
        {live.map((cam) => {
          const s = status[String(cam.id)];
          return (
            <div className="tile" key={cam.id}>
              <div className="label">
                <span className={`dot ${s ? (s.online ? "on" : "off") : ""}`} /> {cam.name}
                {s?.recording && <span className="rec">● REC</span>}
              </div>
              <LiveVideo base={config.go2rtc_base} name={cam.name} mode={mode} />
              <button className="expand" title="Open camera view" onClick={() => setDetail(cam)}>
                ⤢
              </button>
              {ptz[cam.id] && <PtzPad cameraId={cam.id} />}
            </div>
          );
        })}
      </div>
      )}

      {detail && (
        <CameraDetail
          camera={detail}
          config={config}
          ptz={!!ptz[detail.id]}
          onClose={() => setDetail(null)}
        />
      )}
    </>
  );
}
