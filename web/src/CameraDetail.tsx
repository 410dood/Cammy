import { ReactNode, useEffect, useRef, useState } from "react";
import { api, CamEvent, Camera, Segment, SimilarResult, getStreamMode } from "./api";
import { RelTime, Modal, useToast, useDialog, useFocusTrap } from "./ui";
import Timeline from "./Timeline";
import { ActivityStrip } from "./CrossTimeline";
import LiveVideo from "./LiveVideo";
import PrivacyOverlay from "./PrivacyOverlay";
import Heatmap from "./Heatmap";
import {
  IconX, IconMic, IconUser, IconCar, IconDownload, IconVideo, IconSparkles, IconRecDot,
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
  // Unified live↔playback player (Protect's signature): seeking the timeline
  // swaps the live stream for the covering recording IN PLACE — no modal, no
  // page change — and "Back to live" (or Esc, or playing past the last
  // segment) returns to the stream.
  const [playback, setPlayback] = useState<{ segment: Segment; offset: number } | null>(null);
  const playbackRef = useRef<typeof playback>(null);
  playbackRef.current = playback;
  // Coarse playhead position (whole seconds) for the timeline marker.
  const [posTs, setPosTs] = useState<number | null>(null);
  const [findOpen, setFindOpen] = useState(false);
  const findOpenRef = useRef(false);
  findOpenRef.current = findOpen;
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
        label: "What happened? Cammy saves a snapshot, and any alert rules for this label will fire.",
        placeholder: "e.g. Delivery arrived",
        maxLength: 48,
      })
    )?.trim();
    if (!label) return;
    try {
      await api.softTrigger(camera.id, label);
      toast.success(`Logged “${label}”, saved with a snapshot`);
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
      toast.error("Microphone blocked. Allow mic access in your browser to talk.");
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
    // Esc steps back one level: an open modal handles its own Esc, then
    // playback → live, and only then close the whole view.
    const esc = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (findOpenRef.current) return; // the Find-in-frame modal closes itself
      if (playbackRef.current) setPlayback(null);
      else onClose();
    };
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
      setPlayback({ segment: r.segment, offset: r.offset_secs });
      setPosTs(Math.floor(r.segment.start_ts + r.offset_secs));
    } catch {
      // No segment covers this instant (retention-pruned or a recording gap) —
      // tell the user instead of a silent dead-click.
      toast.error("No recording covers this moment");
    }
  };

  // Played past the end of a segment: continue seamlessly into the next one,
  // or return to live once we've caught up with now.
  const advancePlayback = () => {
    const cur = playbackRef.current;
    if (!cur) return;
    const sorted = [...segments].sort((a, b) => a.start_ts - b.start_ts);
    const next = sorted.find((s) => s.start_ts > cur.segment.start_ts);
    if (next) {
      setPlayback({ segment: next, offset: 0 });
      setPosTs(next.start_ts);
    } else {
      setPlayback(null);
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
          className="btn btn-secondary"
          onClick={() => setFindOpen(true)}
          title="Draw a box around a person or vehicle in this camera's view, then find them across all cameras."
        >
          <IconSparkles size={14} /> Find in frame
        </button>
        <button
          className="btn btn-primary"
          onClick={logEvent}
          title="Save this moment as an event with a snapshot."
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
            {playback ? (
              <>
                <video
                  key={`${playback.segment.id}-${playback.offset}`}
                  className="detail-playback"
                  src={`/api/recordings/${playback.segment.id}/video`}
                  controls
                  autoPlay
                  playsInline
                  onLoadedMetadata={(e) => {
                    const v = e.currentTarget;
                    // Clamp: clicking near "now" can resolve into the last
                    // closed segment with an offset past its end.
                    if (playback.offset > 0)
                      v.currentTime = Math.min(playback.offset, Math.max(0, v.duration - 2));
                  }}
                  onTimeUpdate={(e) => {
                    const t = Math.floor(playback.segment.start_ts + e.currentTarget.currentTime);
                    setPosTs((prev) => (prev === t ? prev : t));
                  }}
                  onEnded={advancePlayback}
                />
                <div className="playback-bar">
                  <span className="playback-when">
                    Recorded ·{" "}
                    {new Date((posTs ?? playback.segment.start_ts) * 1000).toLocaleTimeString([], {
                      hour: "numeric",
                      minute: "2-digit",
                      second: "2-digit",
                    })}
                  </span>
                  <a
                    className="btn btn-ghost ev-act"
                    href={`/api/recordings/${playback.segment.id}/video`}
                    download
                  >
                    <IconDownload size={14} /> Download
                  </a>
                  <button className="btn btn-primary ev-act" onClick={() => setPlayback(null)}>
                    <IconRecDot size={10} /> Back to live
                  </button>
                </div>
              </>
            ) : (
              <>
                <LiveVideo name={camera.name} mode={getStreamMode()} audio mic={talking} online={online} />
                <PrivacyOverlay masks={camera.detect_config.privacy_masks} />
                <span className="live-pill" aria-hidden="true">
                  <IconRecDot size={9} /> LIVE
                </span>
                {ptz && <PtzInline cameraId={camera.id} />}
                {twoWay && (
                  <button
                    className={`talk-btn ${talking ? "on" : ""}`}
                    title="Hold to talk through this camera's speaker, if it has one."
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
              </>
            )}
          </div>
          <div>
            <ActivityStrip
              events={events}
              windowSecs={windowSecs}
              nowTs={Math.floor(Date.now() / 1000)}
            />
            <Timeline
              windowSecs={windowSecs}
              segmentSecs={segmentSecs}
              segments={segments}
              events={events}
              onSeek={seekTo}
              markTs={playback ? posTs : null}
            />
          </div>
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

      {findOpen && <FrameSearchModal cameraId={camera.id} onClose={() => setFindOpen(false)} />}
    </div>
  );
}

/// Frame-seeded appearance search (Protect 6's "pause a frame, select a
/// person, find every moment they appear"): drag a box on this camera's
/// current frame, the crop is CLIP-matched against every camera's history.
/// Clicking a match deep-links into that event's viewer.
function FrameSearchModal({ cameraId, onClose }: { cameraId: number; onClose: () => void }) {
  const [rect, setRect] = useState<{ x1: number; y1: number; x2: number; y2: number } | null>(null);
  const [drag, setDrag] = useState<{ x: number; y: number } | null>(null);
  const [res, setRes] = useState<SimilarResult | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // The search crops from the frame image — if it can't load (camera offline),
  // say so and keep the search disabled instead of a silent dead click.
  const [frameOk, setFrameOk] = useState(true);
  const imgRef = useRef<HTMLImageElement>(null);

  const frac = (e: React.PointerEvent<HTMLDivElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    return {
      x: Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)),
      y: Math.min(1, Math.max(0, (e.clientY - r.top) / r.height)),
    };
  };

  const search = async () => {
    const img = imgRef.current;
    if (!img || !rect || !img.naturalWidth) return;
    setBusy(true);
    setError(null);
    try {
      // Crop the selection out of the frame at native resolution (same-origin
      // image, so the canvas stays untainted) and search by that image.
      const w = img.naturalWidth;
      const h = img.naturalHeight;
      const sw = Math.max(1, Math.round((rect.x2 - rect.x1) * w));
      const sh = Math.max(1, Math.round((rect.y2 - rect.y1) * h));
      const cv = document.createElement("canvas");
      cv.width = sw;
      cv.height = sh;
      const ctx = cv.getContext("2d");
      if (!ctx) throw new Error("Couldn't crop the frame in this browser.");
      ctx.drawImage(img, rect.x1 * w, rect.y1 * h, sw, sh, 0, 0, sw, sh);
      const blob = await new Promise<Blob | null>((r) => cv.toBlob(r, "image/jpeg", 0.9));
      if (!blob) throw new Error("Couldn't crop the frame.");
      setRes(await api.searchByImage(blob, 24));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal onClose={onClose} className="modal-wide">
      <h2 style={{ marginTop: 0 }}>Find in frame</h2>
      <p className="muted" style={{ marginTop: -6 }}>
        Drag a box around a person or vehicle in the current view, then search everywhere your
        cameras have seen them.
      </p>
      <div
        className="motion-frame"
        style={{
          touchAction: "none",
          // A failed image has no intrinsic size — keep the box from collapsing
          // so the "camera offline" message has somewhere to live.
          ...(frameOk ? {} : { aspectRatio: "16 / 9", background: "var(--bg-sunken)" }),
        }}
        onPointerDown={(e) => {
          e.currentTarget.setPointerCapture(e.pointerId);
          const p = frac(e);
          setDrag(p);
          setRect({ x1: p.x, y1: p.y, x2: p.x, y2: p.y });
        }}
        onPointerMove={(e) => {
          if (!drag) return;
          const p = frac(e);
          setRect({
            x1: Math.min(drag.x, p.x),
            y1: Math.min(drag.y, p.y),
            x2: Math.max(drag.x, p.x),
            y2: Math.max(drag.y, p.y),
          });
        }}
        onPointerUp={() => setDrag(null)}
      >
        <img
          ref={imgRef}
          src={`/api/cameras/${cameraId}/frame.jpg`}
          alt="Current camera frame"
          draggable={false}
          onError={() => setFrameOk(false)}
          style={frameOk ? undefined : { visibility: "hidden" }}
        />
        {!frameOk && (
          <div
            className="muted"
            style={{
              position: "absolute",
              inset: 0,
              display: "grid",
              placeItems: "center",
              padding: 12,
              textAlign: "center",
            }}
          >
            No live picture — the camera must be online to search from its view.
          </div>
        )}
        {rect && (
          <div
            className="motion-rect"
            style={{
              left: `${rect.x1 * 100}%`,
              top: `${rect.y1 * 100}%`,
              width: `${(rect.x2 - rect.x1) * 100}%`,
              height: `${(rect.y2 - rect.y1) * 100}%`,
            }}
          />
        )}
      </div>
      <div className="row" style={{ marginTop: 10, alignItems: "center" }}>
        <button
          className="btn btn-primary"
          disabled={!frameOk || !rect || rect.x2 - rect.x1 < 0.01 || busy}
          onClick={search}
        >
          {busy ? "Searching…" : "Find across cameras"}
        </button>
        {rect && (
          <button
            className="btn btn-ghost"
            onClick={() => {
              setRect(null);
              setRes(null);
            }}
          >
            Clear box
          </button>
        )}
      </div>
      {error && (
        <p className="muted" role="alert" style={{ color: "var(--danger)" }}>{error}</p>
      )}
      {res &&
        (!res.available ? (
          <p className="muted" style={{ marginTop: 12 }}>
            This search needs the smart search models installed (Settings, Models &amp;
            capabilities). It matches people and vehicles your cameras have seen.
          </p>
        ) : res.results.length === 0 ? (
          <p className="muted" style={{ marginTop: 12 }}>
            No matches on any camera yet. Try a tighter box around just the person or vehicle.
          </p>
        ) : (
          <div style={{ marginTop: 12 }}>
            <p className="muted">Closest matches across your cameras. Click one to open it.</p>
            <div className="scrub-grid">
              {res.results.map((m) => (
                <button
                  key={m.event.id}
                  type="button"
                  className="scrub-tile"
                  title="Open this event"
                  onClick={() => {
                    // A match can be older than the Events page's loaded list —
                    // stash the full event so its viewer can open regardless.
                    try {
                      sessionStorage.setItem("cammy-focus-event", JSON.stringify(m.event));
                    } catch {
                      /* stash is best-effort; the deep link still works for recent events */
                    }
                    window.location.hash = `#/events/${m.event.id}`;
                  }}
                >
                  {m.event.snapshot ? (
                    <img src={`/api/snapshots/${m.event.snapshot}?w=300`} loading="lazy" alt="" />
                  ) : (
                    <div className="scrub-missing">no snapshot</div>
                  )}
                  <span className="scrub-cap">
                    {(m.similarity * 100).toFixed(0)}% · {m.event.camera}
                    <span className="scrub-count">
                      {new Date(m.event.ts * 1000).toLocaleString([], {
                        month: "numeric",
                        day: "numeric",
                        hour: "numeric",
                        minute: "2-digit",
                      })}
                    </span>
                  </span>
                </button>
              ))}
            </div>
          </div>
        ))}
    </Modal>
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
