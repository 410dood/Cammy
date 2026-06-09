import { useEffect, useState } from "react";
import { api, CamEvent, Camera, fmtTime, Segment } from "../api";

export default function Events({ cameras }: { cameras: Camera[] }) {
  const [events, setEvents] = useState<CamEvent[]>([]);
  const [cameraId, setCameraId] = useState<number | "">("");
  const [label, setLabel] = useState("");
  const [open, setOpen] = useState<CamEvent | null>(null);
  const [playing, setPlaying] = useState<{ segment: Segment; offset: number } | null>(null);
  const [noClip, setNoClip] = useState<number | null>(null);

  const jumpToRecording = async (ev: CamEvent) => {
    try {
      const r = await api.recordingAt(ev.camera_id, ev.ts);
      // Land a few seconds before the event so you see it happen.
      setPlaying({ segment: r.segment, offset: Math.max(0, r.offset_secs - 3) });
    } catch {
      setNoClip(ev.id);
      setTimeout(() => setNoClip(null), 2500);
    }
  };

  const load = () => {
    api
      .events({
        camera_id: cameraId === "" ? undefined : cameraId,
        label: label || undefined,
        limit: 200,
      })
      .then(setEvents)
      .catch(() => {});
  };

  useEffect(() => {
    load();
    const t = setInterval(load, 5000); // events appear as they happen
    return () => clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cameraId, label]);

  const labels = [...new Set(events.map((e) => e.label))];

  return (
    <>
      <h1>Events</h1>
      <div className="row" style={{ marginBottom: 16 }}>
        <select value={cameraId} onChange={(e) => setCameraId(e.target.value === "" ? "" : Number(e.target.value))}>
          <option value="">all cameras</option>
          {cameras.map((c) => (
            <option key={c.id} value={c.id}>
              {c.name}
            </option>
          ))}
        </select>
        <select value={label} onChange={(e) => setLabel(e.target.value)}>
          <option value="">all objects</option>
          {labels.map((l) => (
            <option key={l} value={l}>
              {l}
            </option>
          ))}
        </select>
        <span className="muted">{events.length} events · auto-refreshing</span>
      </div>

      {events.length === 0 ? (
        <div className="empty">
          No events yet. They appear when a detect-enabled camera sees motion and the AI
          recognizes an object.
        </div>
      ) : (
        <div className="event-grid">
          {events.map((ev) => (
            <div className="event-card" key={ev.id} onClick={() => setOpen(ev)}>
              {ev.snapshot ? (
                <img src={`/api/snapshots/${ev.snapshot}`} alt={ev.label} loading="lazy" />
              ) : (
                <div style={{ aspectRatio: "4 / 3", background: "#000" }} />
              )}
              <div className="meta">
                <b>{ev.label}</b> {(ev.score * 100).toFixed(0)}% · {ev.camera}
                <div className="muted">{fmtTime(ev.ts)}</div>
                <button
                  className="ghost"
                  style={{ marginTop: 8 }}
                  onClick={(e) => {
                    e.stopPropagation();
                    jumpToRecording(ev);
                  }}
                >
                  {noClip === ev.id ? "no recording for this moment" : "▶ view recording"}
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      {open && (
        <div className="modal-bg" onClick={() => setOpen(null)}>
          {open.snapshot && <img src={`/api/snapshots/${open.snapshot}`} alt={open.label} />}
        </div>
      )}

      {playing && (
        <div className="modal-bg" onClick={() => setPlaying(null)}>
          <video
            src={`/api/recordings/${playing.segment.id}/video`}
            controls
            autoPlay
            onClick={(e) => e.stopPropagation()}
            onLoadedMetadata={(e) => {
              e.currentTarget.currentTime = playing.offset;
            }}
          />
        </div>
      )}
    </>
  );
}
