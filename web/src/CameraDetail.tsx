import { ReactNode, useEffect, useRef, useState } from "react";
import { api, CamEvent, Camera, Segment, getStreamMode } from "./api";
import { RelTime, Modal, useToast, useDialog, useFocusTrap } from "./ui";
import Timeline from "./Timeline";
import LiveVideo from "./LiveVideo";
import PrivacyOverlay from "./PrivacyOverlay";
import Heatmap from "./Heatmap";
import {
  IconX, IconMic, IconUser, IconCar, IconDownload, IconVideo,
  IconArrowUp, IconArrowDown, IconArrowLeft, IconArrowRight, IconPlus, IconMinus,
} from "./icons";
import { groupEvents } from "./eventGroups";
import { isCameraSide, prettyLabel } from "./labels";

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
  const toast = useToast();
  const dialog = useDialog();

  // Soft trigger (Nx-style): press a button, get a bookmarked event with a
  // snapshot of what the camera sees right now — "Delivery arrived".
  const logEvent = async () => {
    const label = (
      await dialog.prompt({
        title: "Log an event",
        label: "What happened? Saved as a bookmarked event with a snapshot; alarm rules matching the label will fire.",
        placeholder: "e.g. Delivery arrived",
        maxLength: 48,
      })
    )?.trim();
    if (!label) return;
    try {
      await api.softTrigger(camera.id, label);
      toast.success(`Logged “${label}” — saved with a snapshot`);
    } catch (e) {
      toast.error(String(e));
    }
  };
  // Full-screen overlay acts as a modal dialog: trap Tab focus inside it and
  // move focus into it on open (Esc-to-close is wired in the effect below).
  const overlayRef = useRef<HTMLDivElement>(null);
  useFocusTrap(overlayRef);
  useEffect(() => {
    overlayRef.current?.focus();
  }, []);
  // Tracks whether the talk button is still held, so an async permission probe
  // that resolves AFTER release can't latch the "Talking…" state on.
  const holdingTalk = useRef(false);

  // Verify mic access before entering the "Talking…" state, so push-to-talk
  // doesn't show a success-looking UI while the browser has the mic blocked.
  const startTalk = async () => {
    holdingTalk.current = true;
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      // We only needed the permission probe; the player opens its own track.
      stream.getTracks().forEach((t) => t.stop());
      if (holdingTalk.current) setTalking(true);
    } catch {
      holdingTalk.current = false;
      toast.error("Microphone blocked — allow mic access in your browser to talk.");
    }
  };
  const stopTalk = () => {
    holdingTalk.current = false;
    setTalking(false);
  };

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
    <div
      className="detail-overlay"
      ref={overlayRef}
      role="dialog"
      aria-modal="true"
      aria-label={camera.name}
      tabIndex={-1}
    >
      <div className="detail-head">
        <h1 style={{ border: "none", margin: 0, padding: 0 }}>{camera.name}</h1>
        <div className="spacer" />
        <div className="row" role="group" aria-label="Timeline range" style={{ gap: 6 }}>
          {[
            { label: "1h", secs: 3600 },
            { label: "6h", secs: 6 * 3600 },
            { label: "24h", secs: 24 * 3600 },
          ].map((w) => (
            <button
              key={w.secs}
              className={`btn ${windowSecs === w.secs ? "btn-primary" : "btn-secondary"}`}
              aria-pressed={windowSecs === w.secs}
              onClick={() => setWindowSecs(w.secs)}
            >
              {w.label}
            </button>
          ))}
        </div>
        <button
          className="btn btn-primary"
          onClick={logEvent}
          title="Create a bookmarked event with a snapshot of this moment (soft trigger)"
        >
          <IconPlus size={14} /> Log event
        </button>
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
                title="Hold to talk through this camera's speaker (best-effort; needs a camera with a speaker)"
                aria-pressed={talking}
                onPointerDown={(e) => {
                  e.preventDefault();
                  startTalk();
                }}
                onPointerUp={stopTalk}
                onPointerLeave={stopTalk}
                onPointerCancel={stopTalk}
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
          <h2 className="eyebrow" style={{ margin: "4px 0 10px" }}>
            Recent detections
          </h2>
          {events.length === 0 && <p className="muted">No events for this camera yet.</p>}
          {groupEvents(events).slice(0, 20).map(({ rep: ev, count }) => (
            <button
              type="button"
              className="feed-item"
              key={ev.id}
              aria-label={`Jump to this ${prettyLabel(ev.label)} detection in the recording`}
              onClick={() => seekTo(ev.ts)}
            >
              {ev.snapshot ? (
                <img src={`/api/snapshots/${ev.snapshot}?w=160`} alt={prettyLabel(ev.label)} loading="lazy" decoding="async" />
              ) : (
                // Keep the thumbnail column so snapshot-less rows stay aligned.
                <span
                  aria-hidden="true"
                  style={{
                    width: 84, aspectRatio: "4 / 3", borderRadius: 6, flexShrink: 0,
                    background: "var(--bg-sunken)", display: "grid", placeItems: "center",
                    color: "var(--text-faint)",
                  }}
                >
                  <IconVideo size={16} />
                </span>
              )}
              <div>
                <b style={{ textTransform: "capitalize" }}>{prettyLabel(ev.label)}</b>{" "}
                {!isCameraSide(ev.label) && <span className="score">{(ev.score * 100).toFixed(0)}%</span>}
                {count > 1 && (
                  <span className="badge" style={{ marginLeft: 6 }}>×{count}</span>
                )}
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
                <RelTime ts={ev.ts} className="muted clock" style={{ display: "block", fontSize: "var(--text-xs)" }} />
              </div>
            </button>
          ))}
        </div>
      </div>

      <div className="card" style={{ marginTop: 14 }}>
        <h2 className="eyebrow" style={{ margin: "0 0 10px" }}>
          Activity heatmap
        </h2>
        <Heatmap camera={camera} />
      </div>

      {playing && (
        <Modal bare onClose={() => setPlaying(null)}>
          <div style={{ position: "relative", lineHeight: 0 }}>
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
            <a
              className="btn btn-ghost ev-act"
              href={`/api/recordings/${playing.segment.id}/video`}
              download
              style={{ position: "absolute", top: 8, right: 8 }}
            >
              <IconDownload size={14} /> Download
            </a>
          </div>
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
