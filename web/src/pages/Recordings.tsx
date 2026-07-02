import { useEffect, useMemo, useState } from "react";
import { api, CamEvent, Camera, capacityTone, fmtBytes, fmtDaysLeft, fmtTime, Segment, Stats } from "../api";
import Timeline from "../Timeline";
import CrossTimeline from "../CrossTimeline";
import { IconPlay, IconFilm, IconAlert, IconChevronDown, IconChevronRight } from "../icons";
import { Callout, EmptyState, ErrorState, Modal } from "../ui";

const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e));

type HourGroup = { key: string; camera: string; hourTs: number; segs: Segment[]; bytes: number };

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

  // The raw segment list is minute-granularity — hundreds of near-identical
  // rows. Fold it into one row per camera-hour, expandable to the segments.
  const [openHours, setOpenHours] = useState<Set<string>>(new Set());

  // Day picker: "" = live (anchored at now); a date scrubs that day's history.
  const [day, setDay] = useState("");
  const dayAnchor = () => {
    const nowSecs = Math.floor(Date.now() / 1000);
    if (!day) return nowSecs;
    const end = Math.floor(new Date(`${day}T23:59:59`).getTime() / 1000);
    return Number.isFinite(end) ? Math.min(end, nowSecs) : nowSecs;
  };
  const anchor = dayAnchor();

  const load = () => {
    api
      .recordings({
        camera_id: cameraId === "" ? undefined : cameraId,
        before: day ? anchor + 1 : undefined,
        limit: 1000,
      })
      .then((s) => {
        setSegments(s);
        setLoadError(null);
      })
      .catch((e) => setLoadError(errMsg(e)));
    api.stats().then(setStats).catch(() => {});
    // Fetch events for the timeline: all cameras (cross-camera lanes) or just one.
    api
      .events({
        camera_id: cameraId === "" ? undefined : cameraId,
        before: day ? anchor + 1 : undefined,
        limit: 1500,
      })
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
  }, [cameraId, day]);

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

  const hourGroups = useMemo<HourGroup[]>(() => {
    const map = new Map<string, HourGroup>();
    for (const s of segments) {
      const hourTs = Math.floor(s.start_ts / 3600) * 3600;
      const key = `${s.camera}|${hourTs}`;
      let g = map.get(key);
      if (!g) {
        g = { key, camera: s.camera, hourTs, segs: [], bytes: 0 };
        map.set(key, g);
      }
      g.segs.push(s);
      g.bytes += s.bytes;
    }
    // Newest hour first; within an hour, clips run oldest-first (scrub order).
    const groups = [...map.values()].sort(
      (a, b) => b.hourTs - a.hourTs || a.camera.localeCompare(b.camera)
    );
    for (const g of groups) g.segs.sort((a, b) => a.start_ts - b.start_ts);
    return groups;
  }, [segments]);

  return (
    <>
      <h1>Recordings</h1>
      <p className="muted" style={{ marginTop: -8 }}>
        Continuous footage and storage. For AI detections (person, vehicle, and more), see Events.
      </p>

      {stats && (() => {
        // Severity keys ONLY on actual disk headroom (days_until_full): <7 days
        // gets a warn callout and <2 a danger one instead of muted text. The
        // retention horizon is routine pruning, not data loss — it stays
        // neutral informational copy below, never a warning.
        const capTone = capacityTone(stats.days_until_full);
        const rh = stats.retention_horizon_days;
        const capDetail = (
          <>
            writing ~{fmtBytes(stats.write_bytes_per_day)}/day
            {stats.days_until_full != null && (
              <>
                {" "}
                · {fmtDaysLeft(stats.days_until_full)} until full
                {stats.est_full_ts != null && (
                  <> ({new Date(stats.est_full_ts * 1000).toLocaleDateString()})</>
                )}
              </>
            )}
            {rh != null && <> · retention caps history at {fmtDaysLeft(rh)}</>}
            <span style={{ opacity: 0.7 }}> · estimated</span>
          </>
        );
        // Badge and callout are gated together (the write-rate estimate must
        // exist) so a bare unexplained warning badge can never appear.
        const showCap = capTone != null && stats.write_bytes_per_day > 0;
        return (
        <div className="card">
          <div className="card-head">
            <h2 style={{ margin: 0 }}>Storage</h2>
            {showCap && (
              <span className={`badge ${capTone}`} style={{ marginLeft: 8 }}>
                <IconAlert size={11} /> {capTone === "danger" ? "Nearly full" : "Filling up"}
              </span>
            )}
          </div>
          <div className="row" style={{ marginBottom: 10 }}>
            <span className="muted">
              {fmtBytes(stats.total_bytes)} of recordings · {fmtBytes(stats.snapshots_bytes)} of
              snapshots · {stats.events_total} events all-time
              {stats.disk_free_bytes != null && <> · {fmtBytes(stats.disk_free_bytes)} free on disk</>}
            </span>
          </div>
          {stats.write_bytes_per_day > 0 &&
            (showCap ? (
              <Callout tone={capTone!} style={{ marginBottom: 12 }}>
                <b>Disk is filling up</b> — add disk or shorten retention to keep more history.
                <div className="muted" style={{ marginTop: 2 }}>
                  {capDetail}
                </div>
              </Callout>
            ) : (
              <div className="row" style={{ marginBottom: 12 }}>
                <span className="muted">
                  <b>Capacity</b> — {capDetail}
                </span>
              </div>
            ))}
          {stats.cameras.map((c) => (
            <div className="row" key={c.camera_id} style={{ marginBottom: 6 }}>
              <span style={{ width: 120 }}>
                <b>{c.camera}</b>
                {cameras.find((cc) => cc.id === c.camera_id)?.enabled === false && (
                  <span
                    className="badge"
                    style={{ marginLeft: 6 }}
                    title="This camera is disabled (Cameras page) — old footage is kept until retention prunes it"
                  >
                    disabled
                  </span>
                )}
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
        );
      })()}

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
        <label className="field" title="Scrub a past day's recordings; clear to return to live">
          day
          <input
            type="date"
            aria-label="Jump to a day"
            value={day}
            max={(() => {
              // Local date, not UTC — toISOString() flips days near midnight.
              const d = new Date();
              return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
            })()}
            onChange={(e) => {
              setDay(e.target.value);
              if (e.target.value) setWindowSecs(24 * 3600);
            }}
          />
        </label>
        {day && (
          <button className="btn btn-primary" onClick={() => setDay("")} title="Back to the live, auto-refreshing view">
            Live
          </button>
        )}
        <span className="muted">
          {segments.length} clips · {fmtBytes(segments.reduce((a, s) => a + s.bytes, 0))} total
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
            nowTs={anchor}
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
          nowTs={anchor}
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
                <th>When</th>
                <th>Size</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {hourGroups.map((g) => {
                const open = openHours.has(g.key);
                const hourLabel = `${new Date(g.hourTs * 1000).toLocaleString([], {
                  month: "numeric", day: "numeric", hour: "numeric",
                })} – ${new Date((g.hourTs + 3600) * 1000).toLocaleTimeString([], { hour: "numeric" })}`;
                if (g.segs.length === 1) {
                  const s = g.segs[0];
                  return (
                    <tr key={g.key}>
                      <td><b>{s.camera}</b></td>
                      <td>{fmtTime(s.start_ts)}</td>
                      <td className="muted">{fmtBytes(s.bytes)}</td>
                      <td>
                        <button className="btn btn-ghost ev-act" onClick={() => setPlaying({ segment: s, offset: 0 })}>
                          <IconPlay size={13} /> Play
                        </button>
                      </td>
                    </tr>
                  );
                }
                return (
                  <HourRows
                    key={g.key}
                    group={g}
                    open={open}
                    hourLabel={hourLabel}
                    onToggle={() =>
                      setOpenHours((prev) => {
                        const next = new Set(prev);
                        if (next.has(g.key)) next.delete(g.key);
                        else next.add(g.key);
                        return next;
                      })
                    }
                    onPlay={(s) => setPlaying({ segment: s, offset: 0 })}
                  />
                );
              })}
            </tbody>
          </table>
          </div>
        </div>
      )}

      {playing && (
        <Modal bare onClose={() => setPlaying(null)}>
          <video
            src={`/api/recordings/${playing.segment.id}/video`}
            controls
            autoPlay
            onLoadedMetadata={(e) => {
              const v = e.currentTarget;
              // Clamp: clicking near "now" can resolve into the last closed
              // segment with an offset past its end.
              if (playing.offset > 0)
                v.currentTime = Math.min(playing.offset, Math.max(0, v.duration - 2));
            }}
          />
        </Modal>
      )}
    </>
  );
}

/// One camera-hour of footage: a summary row that expands into its clips.
function HourRows({
  group,
  open,
  hourLabel,
  onToggle,
  onPlay,
}: {
  group: HourGroup;
  open: boolean;
  hourLabel: string;
  onToggle: () => void;
  onPlay: (s: Segment) => void;
}) {
  return (
    <>
      <tr>
        <td><b>{group.camera}</b></td>
        <td>
          <button
            type="button"
            className="btn btn-ghost ev-act"
            style={{ marginLeft: -8 }}
            aria-expanded={open}
            onClick={onToggle}
          >
            {open ? <IconChevronDown size={13} /> : <IconChevronRight size={13} />} {hourLabel}
            <span className="muted"> · {group.segs.length} clips</span>
          </button>
        </td>
        <td className="muted">{fmtBytes(group.bytes)}</td>
        <td></td>
      </tr>
      {open &&
        group.segs.map((s) => (
          <tr key={s.id}>
            <td></td>
            <td style={{ paddingLeft: 26 }}>{fmtTime(s.start_ts)}</td>
            <td className="muted">{fmtBytes(s.bytes)}</td>
            <td>
              <button className="btn btn-ghost ev-act" onClick={() => onPlay(s)}>
                <IconPlay size={13} /> Play
              </button>
            </td>
          </tr>
        ))}
    </>
  );
}
