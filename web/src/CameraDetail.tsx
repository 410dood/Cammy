import { useEffect, useState } from "react";
import { api, CamEvent, Camera, Segment, fmtTime, getStreamMode } from "./api";
import Timeline from "./Timeline";
import LiveVideo from "./LiveVideo";

/// UniFi Protect-style camera view: large live player with the camera's own
/// timeline underneath and its recent detections alongside. Esc closes.
export default function CameraDetail({
  camera,
  ptz,
  onClose,
}: {
  camera: Camera;
  ptz: boolean;
  onClose: () => void;
}) {
  const [segments, setSegments] = useState<Segment[]>([]);
  const [events, setEvents] = useState<CamEvent[]>([]);
  const [playing, setPlaying] = useState<{ segment: Segment; offset: number } | null>(null);
  const [windowSecs, setWindowSecs] = useState(6 * 3600);
  const [segmentSecs, setSegmentSecs] = useState(60);

  useEffect(() => {
    api.settings().then((s) => setSegmentSecs(s.segment_seconds)).catch(() => {});
    const load = () => {
      api.recordings({ camera_id: camera.id, limit: 1000 }).then(setSegments).catch(() => {});
      api.events({ camera_id: camera.id, limit: 50 }).then(setEvents).catch(() => {});
    };
    load();
    const t = setInterval(load, 10000);
    const esc = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", esc);
    return () => {
      clearInterval(t);
      window.removeEventListener("keydown", esc);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [camera.id]);

  const seekTo = async (ts: number) => {
    try {
      const r = await api.recordingAt(camera.id, ts);
      setPlaying({ segment: r.segment, offset: r.offset_secs });
    } catch {
      /* gap */
    }
  };

  return (
    <div className="detail-overlay">
      <div className="detail-head">
        <h1 style={{ border: "none", margin: 0, padding: 0 }}>{camera.name}</h1>
        <div className="spacer" />
        {[
          { label: "1h", secs: 3600 },
          { label: "6h", secs: 6 * 3600 },
          { label: "24h", secs: 24 * 3600 },
        ].map((w) => (
          <button
            key={w.secs}
            className={windowSecs === w.secs ? "primary" : "ghost"}
            onClick={() => setWindowSecs(w.secs)}
          >
            {w.label}
          </button>
        ))}
        <button className="ghost" onClick={onClose}>
          ✕ close
        </button>
      </div>

      <div className="detail-body">
        <div className="detail-main">
          <div className="tile" style={{ aspectRatio: "16 / 9" }}>
            <LiveVideo name={camera.name} mode={getStreamMode()} audio />
            {ptz && <PtzInline cameraId={camera.id} />}
          </div>
          <Timeline
            windowSecs={windowSecs}
            segmentSecs={segmentSecs}
            segments={segments}
            events={events}
            onSeek={seekTo}
          />
        </div>

        <div className="detail-side">
          <h2 style={{ margin: "4px 0 10px", fontSize: "0.78rem", textTransform: "uppercase", color: "var(--muted)" }}>
            Recent detections
          </h2>
          {events.length === 0 && <p className="muted">No events for this camera yet.</p>}
          {events.slice(0, 20).map((ev) => (
            <div className="feed-item" key={ev.id} onClick={() => seekTo(ev.ts)}>
              {ev.snapshot && <img src={`/api/snapshots/${ev.snapshot}`} alt={ev.label} loading="lazy" />}
              <div>
                <b style={{ textTransform: "capitalize" }}>{ev.label}</b>{" "}
                {(ev.score * 100).toFixed(0)}%
                {ev.face && <span style={{ color: "var(--ok)" }}> 👤 {ev.face}</span>}
                {ev.plate && <span style={{ color: "var(--warn)" }}> 🚗 {ev.plate}</span>}
                <div className="muted" style={{ fontSize: "0.75rem" }}>{fmtTime(ev.ts)}</div>
              </div>
            </div>
          ))}
        </div>
      </div>

      {playing && (
        <div className="modal-bg" onClick={() => setPlaying(null)}>
          <video
            src={`/api/recordings/${playing.segment.id}/video`}
            controls
            autoPlay
            onClick={(e) => e.stopPropagation()}
            onLoadedMetadata={(e) => {
              const v = e.currentTarget;
              if (playing.offset > 0)
                v.currentTime = Math.min(playing.offset, Math.max(0, v.duration - 2));
            }}
          />
        </div>
      )}
    </div>
  );
}

function PtzInline({ cameraId }: { cameraId: number }) {
  const move = (pan: number, tilt: number, zoom: number) =>
    api.ptz(cameraId, { action: "move", pan, tilt, zoom }).catch(() => {});
  const stop = () => api.ptz(cameraId, { action: "stop" }).catch(() => {});
  const btn = (label: string, p: number, t: number, z: number) => (
    <button
      className="ptz-btn"
      onPointerDown={(e) => {
        e.preventDefault();
        move(p, t, z);
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
