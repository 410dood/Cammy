import { useEffect, useState } from "react";
import { api, AppConfig, Camera, StatusMap } from "../api";

export default function Live({
  cameras,
  config,
}: {
  cameras: Camera[];
  config: AppConfig | null;
}) {
  const [status, setStatus] = useState<StatusMap>({});

  useEffect(() => {
    const load = () => api.status().then(setStatus).catch(() => {});
    load();
    const t = setInterval(load, 5000);
    return () => clearInterval(t);
  }, []);

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
            </div>
          );
        })}
      </div>
    </>
  );
}
