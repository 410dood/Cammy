import { useEffect, useState } from "react";
import { api, CamEvent, Camera, fmtBytes, fmtTime, Segment, Stats } from "../api";
import Timeline from "../Timeline";
import CrossTimeline from "../CrossTimeline";
import { IconPlay, IconFilm } from "../icons";
import { EmptyState, ErrorState } from "../ui";

const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e));

const WINDOWS = [
  { label: "1h", secs: 3600 },
  { label: "6h", secs: 6 * 3600 },
  { label: "24h", secs: 24 * 3600 },
];

export default function Recordings({ cameras }: { cameras: Camera[] }) {
  const [segments, setSegments] = useState<Segment[]>([]);
  const [events, setEvents] = useState<CamEvent[]>([]);
  const [cameraId, setCameraId] = useState<number | "">("");
  const [playing, setPlaying] = useState<{ segment: Segment; offset: number } | null>(null);
  const [stats, setStats] = useState<Stats | null>(null);
  const [windowSecs, setWindowSecs] = useState(6 * 3600);
  const [segmentSecs, setSegmentSecs] = useState(60);
  const [loadError, setLoadError] = useState<string | null>(null);

  const load = () => {
    api
      .recordings({ camera_id: cameraId === "" ? undefined : cameraId, limit: 1000 })
      .then((s) => {
        setSegments(s);
        setLoadError(null);
      })
      .catch((e) => setLoadError(errMsg(e)));
    api.stats().then(setStats).catch(() => {});
    // Fetch events for the timeline: all cameras (cross-camera lanes) or just one.
    api
      .events({ camera_id: cameraId === "" ? undefined : cameraId, limit: 1500 })
      .then(setEvents)
      .catch(() => {});
  };

  useEffect(() => {
    api.settings().then((s) => setSegmentSecs(s.segment_seconds)).catch(() => {});
  }, []);

  useEffect(() => {
    load();
    const t = setInterval(load, 10000);
    return () => clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cameraId]);

  const seekTo = async (ts: number) => {
    if (cameraId === "") return;
    seekCamera(cameraId, ts);
  };
  const seekCamera = async (camId: number, ts: number) => {
    try {
      const r = await api.recordingAt(camId, ts);
      setPlaying({ segment: r.segment, offset: r.offset_secs });
    } catch {
      /* clicked a gap — nothing recorded there */
    }
  };

  return (
    <>
      <h1>Recordings</h1>

      {stats && (
        <div className="card">
          <h2>Storage</h2>
          <div className="row" style={{ marginBottom: 10 }}>
            <span className="muted">
              {fmtBytes(stats.total_bytes)} of recordings · {fmtBytes(stats.snapshots_bytes)} of
              snapshots · {stats.events_total} events all-time
              {stats.disk_free_bytes != null && <> · {fmtBytes(stats.disk_free_bytes)} free on disk</>}
            </span>
          </div>
          {stats.write_bytes_per_day > 0 && (
            <div className="row" style={{ marginBottom: 12 }}>
              <span className="muted">
                <b>Capacity</b> — writing ~{fmtBytes(stats.write_bytes_per_day)}/day
                {stats.days_until_full != null && (
                  <>
                    {" "}
                    · ~{Math.round(stats.days_until_full)} days until full
                    {stats.est_full_ts != null && (
                      <> ({new Date(stats.est_full_ts * 1000).toLocaleDateString()})</>
                    )}
                  </>
                )}
                {stats.retention_horizon_days != null && (
                  <> · retention caps history at ~{Math.round(stats.retention_horizon_days)} days</>
                )}
                <span style={{ opacity: 0.7 }}> · estimated</span>
              </span>
            </div>
          )}
          {stats.cameras.map((c) => (
            <div className="row" key={c.camera_id} style={{ marginBottom: 6 }}>
              <span style={{ width: 120 }}>
                <b>{c.camera}</b>
              </span>
              <div className="usage-bar">
                <div
                  className="usage-fill"
                  style={{
                    width: `${stats.total_bytes ? Math.max(2, (c.bytes / stats.total_bytes) * 100) : 0}%`,
                  }}
                />
              </div>
              <span className="muted" style={{ width: 220 }}>
                {fmtBytes(c.bytes)} · {c.segments} segments
                {c.oldest_ts ? ` · since ${new Date(c.oldest_ts * 1000).toLocaleDateString()}` : ""}
              </span>
            </div>
          ))}
        </div>
      )}

      <div className="row" style={{ marginBottom: 16 }}>
        <select value={cameraId} onChange={(e) => setCameraId(e.target.value === "" ? "" : Number(e.target.value))}>
          <option value="">all cameras</option>
          {cameras.map((c) => (
            <option key={c.id} value={c.id}>
              {c.name}
            </option>
          ))}
        </select>
        {WINDOWS.map((w) => (
          <button
            key={w.secs}
            className={`btn ${windowSecs === w.secs ? "btn-primary" : "btn-ghost"}`}
            onClick={() => setWindowSecs(w.secs)}
          >
            {w.label}
          </button>
        ))}
        <span className="muted">
          {segments.length} segments · {fmtBytes(segments.reduce((a, s) => a + s.bytes, 0))} total
        </span>
      </div>

      {cameraId === "" ? (
        cameras.length > 0 && (
          <CrossTimeline
            cameras={cameras.filter((c) => c.enabled)}
            segments={segments}
            events={events}
            windowSecs={windowSecs}
            segmentSecs={segmentSecs}
            nowTs={Math.floor(Date.now() / 1000)}
            onSeek={seekCamera}
          />
        )
      ) : (
        <Timeline
          windowSecs={windowSecs}
          segmentSecs={segmentSecs}
          segments={segments}
          events={events}
          onSeek={seekTo}
        />
      )}

      {segments.length === 0 ? (
        loadError ? (
          <ErrorState what="recordings" message={loadError} onRetry={load} />
        ) : (
          <EmptyState
            icon={<IconFilm />}
            title="No recordings yet"
            hint="Segments land here about a minute after a record-enabled camera connects. Check that recording is on for at least one camera."
          />
        )
      ) : (
        <div className="card">
          <div className="table-scroll">
          <table>
            <thead>
              <tr>
                <th>Camera</th>
                <th>Start</th>
                <th>Size</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {segments.slice(0, 200).map((s) => (
                <tr key={s.id}>
                  <td>
                    <b>{s.camera}</b>
                  </td>
                  <td>{fmtTime(s.start_ts)}</td>
                  <td className="muted">{fmtBytes(s.bytes)}</td>
                  <td>
                    <button className="btn btn-ghost ev-act" onClick={() => setPlaying({ segment: s, offset: 0 })}>
                      <IconPlay size={13} /> Play
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          </div>
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
              const v = e.currentTarget;
              // Clamp: clicking near "now" can resolve into the last closed
              // segment with an offset past its end.
              if (playing.offset > 0)
                v.currentTime = Math.min(playing.offset, Math.max(0, v.duration - 2));
            }}
          />
        </div>
      )}
    </>
  );
}
