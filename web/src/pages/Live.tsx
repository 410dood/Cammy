import { useEffect, useRef, useState } from "react";
import { api, AppConfig, Camera, StatusMap } from "../api";

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

  const live = cameras.filter((c) => c.enabled);
  if (!config) return <p className="muted">Connecting…</p>;
  if (live.length === 0)
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
      <h1>Live</h1>
      <div className="live-grid">
        {live.map((cam) => {
          const s = status[String(cam.id)];
          return (
            <div className="tile" key={cam.id}>
              <div className="label">
                <span className={`dot ${s ? (s.online ? "on" : "off") : ""}`} /> {cam.name}
                {s?.recording && <span className="rec">● REC</span>}
              </div>
              <iframe
                title={cam.name}
                src={`${config.go2rtc_base}/stream.html?src=${encodeURIComponent(cam.name)}&mode=webrtc`}
                allow="autoplay"
              />
              {ptz[cam.id] && <PtzPad cameraId={cam.id} />}
            </div>
          );
        })}
      </div>
    </>
  );
}
