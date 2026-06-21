import { ReactNode, useEffect, useState } from "react";
import { api, CamEvent, Camera, Segment, getStreamMode } from "./api";
import { RelTime, Modal } from "./ui";
import Timeline from "./Timeline";
import LiveVideo from "./LiveVideo";
import PrivacyOverlay from "./PrivacyOverlay";
import Heatmap from "./Heatmap";
import {
  IconX, IconMic, IconUser, IconCar,
  IconArrowUp, IconArrowDown, IconArrowLeft, IconArrowRight, IconPlus, IconMinus,
} from "./icons";

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
  const [talking, setTalking] = useState(false);
  const [online, setOnline] = useState<boolean | undefined>(undefined);
  const twoWay = !!camera.detect_config.two_way_audio;

  // Safety net: guarantee push-to-talk releases (mic off) on ANY pointer-up or
  // window blur, even if the up/cancel misses the button (drag-off, alt-tab).
  useEffect(() => {
    if (!talking) return;
    const release = () => setTalking(false);
    window.addEventListener("pointerup", release);
    window.addEventListener("blur", release);
    return () => {
      window.removeEventListener("pointerup", release);
      window.removeEventListener("blur", release);
    };
  }, [talking]);

  useEffect(() => {
    api.settings().then((s) => setSegmentSecs(s.segment_seconds)).catch(() => {});
    const load = () => {
      api.recordings({ camera_id: camera.id, limit: 1000 }).then(setSegments).catch(() => {});
      api.events({ camera_id: camera.id, limit: 50 }).then(setEvents).catch(() => {});
      api.status().then((m) => setOnline(m[String(camera.id)]?.online)).catch(() => {});
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
        <button className="btn btn-ghost" onClick={onClose}>
          <IconX size={15} /> Close
        </button>
      </div>

      <div className="detail-body">
        <div className="detail-main">
          <div className="tile" style={{ aspectRatio: "16 / 9" }}>
            <LiveVideo name={camera.name} mode={getStreamMode()} audio mic={talking} online={online} />
            <PrivacyOverlay masks={camera.detect_config.privacy_masks} />
            {ptz && <PtzInline cameraId={camera.id} />}
            {twoWay && (
              <button
                className={`talk-btn ${talking ? "on" : ""}`}
                title="Hold to talk through this camera's speaker"
                aria-pressed={talking}
                onPointerDown={(e) => {
                  e.preventDefault();
                  setTalking(true);
                }}
                onPointerUp={() => setTalking(false)}
                onPointerLeave={() => setTalking(false)}
                onPointerCancel={() => setTalking(false)}
              >
                <IconMic size={15} /> {talking ? "Talking…" : "Hold to talk"}
              </button>
            )}
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
                <span className="score">{(ev.score * 100).toFixed(0)}%</span>
                {ev.face && (
                  <span className="badge ok" style={{ marginLeft: 6 }}>
                    <IconUser size={12} /> {ev.face}
                  </span>
                )}
                {ev.plate && (
                  <span className="badge warn" style={{ marginLeft: 6 }}>
                    <IconCar size={12} /> {ev.plate}
                  </span>
                )}
                <RelTime ts={ev.ts} className="muted clock" style={{ display: "block", fontSize: "0.75rem" }} />
              </div>
            </div>
          ))}
        </div>
      </div>

      <div className="card" style={{ marginTop: 14 }}>
        <h2 style={{ margin: "0 0 10px", fontSize: "0.78rem", textTransform: "uppercase", color: "var(--muted)" }}>
          Activity heatmap
        </h2>
        <Heatmap camera={camera} />
      </div>

      {playing && (
        <Modal bare onClose={() => setPlaying(null)}>
          <video
            src={`/api/recordings/${playing.segment.id}/video`}
            controls
            autoPlay
            onLoadedMetadata={(e) => {
              const v = e.currentTarget;
              if (playing.offset > 0)
                v.currentTime = Math.min(playing.offset, Math.max(0, v.duration - 2));
            }}
          />
        </Modal>
      )}
    </div>
  );
}

function PtzInline({ cameraId }: { cameraId: number }) {
  const move = (pan: number, tilt: number, zoom: number) =>
    api.ptz(cameraId, { action: "move", pan, tilt, zoom }).catch(() => {});
  const stop = () => api.ptz(cameraId, { action: "stop" }).catch(() => {});
  const btn = (icon: ReactNode, label: string, p: number, t: number, z: number) => (
    <button
      className="ptz-btn"
      aria-label={label}
      title={label}
      onPointerDown={(e) => {
        e.preventDefault();
        move(p, t, z);
      }}
      onPointerUp={stop}
      onPointerLeave={stop}
    >
      {icon}
    </button>
  );
  return (
    <div className="ptz-pad">
      <span />
      {btn(<IconArrowUp size={17} />, "Tilt up", 0, 0.5, 0)}
      <span />
      {btn(<IconArrowLeft size={17} />, "Pan left", -0.5, 0, 0)}
      {btn(<IconArrowDown size={17} />, "Tilt down", 0, -0.5, 0)}
      {btn(<IconArrowRight size={17} />, "Pan right", 0.5, 0, 0)}
      {btn(<IconPlus size={17} />, "Zoom in", 0, 0, 0.5)}
      <span />
      {btn(<IconMinus size={17} />, "Zoom out", 0, 0, -0.5)}
    </div>
  );
}
