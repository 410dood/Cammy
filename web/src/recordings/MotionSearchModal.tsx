import { useState } from "react";
import { api, MotionHit } from "../api";
import { Modal } from "../ui";
import { errMsg } from "./buckets";

// P2.3 retroactive region motion search: draw a rectangle on the camera's
// frame, get every archived minute with motion inside it (from the persisted
// 64x64 motion-mask index — no video decode), click a hit to play it.
export default function MotionSearchModal({
  cameraId,
  from,
  to,
  onPlay,
  onClose,
}: {
  cameraId: number;
  from: number;
  to: number;
  onPlay: (segId: number, segStartTs: number, offset: number) => void;
  onClose: () => void;
}) {
  const [rect, setRect] = useState<{ x1: number; y1: number; x2: number; y2: number } | null>(null);
  const [drag, setDrag] = useState<{ x: number; y: number } | null>(null);
  const [hits, setHits] = useState<MotionHit[] | null>(null);
  const [truncated, setTruncated] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const frac = (e: React.PointerEvent<HTMLDivElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    return {
      x: Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)),
      y: Math.min(1, Math.max(0, (e.clientY - r.top) / r.height)),
    };
  };

  const search = async () => {
    if (!rect) return;
    setBusy(true);
    setError(null);
    try {
      const r = await api.motionSearch({ camera_id: cameraId, ...rect, from, to });
      setHits(r.hits);
      setTruncated(r.truncated);
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal onClose={onClose} className="modal-wide">
      <h2 style={{ marginTop: 0 }}>Motion search</h2>
      <p className="muted" style={{ marginTop: -6 }}>
        Drag a box over the area you care about (a gate, a driveway, a doorway), then search for
        recorded motion there in this time window.
      </p>
      <div
        className="motion-frame"
        style={{ touchAction: "none" }}
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
        <img src={`/api/cameras/${cameraId}/frame.jpg`} alt="Current camera frame" draggable={false} />
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
          disabled={!rect || rect.x2 - rect.x1 < 0.01 || busy}
          onClick={search}
        >
          {busy ? "Searching…" : "Search this window"}
        </button>
        {rect && (
          <button className="btn btn-ghost" onClick={() => { setRect(null); setHits(null); }}>
            Clear box
          </button>
        )}
        <span className="muted">
          {new Date(from * 1000).toLocaleString()} → {new Date(to * 1000).toLocaleString()}
        </span>
      </div>
      {error && <p className="muted" role="alert" style={{ color: "var(--danger, #e66)" }}>{error}</p>}
      {hits && (
        <div style={{ marginTop: 12 }}>
          {hits.length === 0 ? (
            <p className="muted">No motion recorded in that area during this window.</p>
          ) : (
            <>
              <p className="muted">
                {hits.length} moment{hits.length === 1 ? "" : "s"}
                {truncated ? " (showing the most recent 300)" : ""}. Click to play.
              </p>
              <div className="scrub-grid">
                {hits.map((h) => (
                  <button
                    key={h.ts}
                    type="button"
                    className="scrub-tile"
                    disabled={h.segment_id == null}
                    title={h.segment_id == null ? "Recording no longer retained" : "Play"}
                    onClick={() =>
                      h.segment_id != null && onPlay(h.segment_id, h.segment_start_ts ?? h.ts, h.offset_secs ?? 0)
                    }
                  >
                    {h.segment_id != null ? (
                      <img src={`/api/recordings/${h.segment_id}/thumb.jpg`} loading="lazy" alt="" />
                    ) : (
                      <div className="scrub-missing">expired</div>
                    )}
                    <span className="scrub-cap">
                      {new Date(h.ts * 1000).toLocaleString([], {
                        month: "numeric", day: "numeric", hour: "numeric", minute: "2-digit",
                      })}
                      {h.end_ts - h.ts > 60 && (
                        <span className="scrub-count">{Math.round((h.end_ts - h.ts) / 60)}m</span>
                      )}
                    </span>
                  </button>
                ))}
              </div>
            </>
          )}
        </div>
      )}
    </Modal>
  );
}
