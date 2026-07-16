import { useEffect, useMemo, useRef, useState } from "react";
import { api, CamEvent, Camera, capacityTone, fmtBytes, fmtDaysLeft, fmtTime, MotionHit, Segment, Stats } from "../api";
import Timeline from "../Timeline";
import CrossTimeline, { ActivityStrip } from "../CrossTimeline";
import { IconPlay, IconFilm, IconAlert, IconChevronDown, IconChevronRight, IconChevronLeft, IconX } from "../icons";
import { Callout, EmptyState, ErrorState, Modal, useToast } from "../ui";

const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e));

type HourGroup = { key: string; camera: string; hourTs: number; segs: Segment[]; bytes: number };

const WINDOWS = [
  { label: "1h", secs: 3600 },
  { label: "6h", secs: 6 * 3600 },
  { label: "24h", secs: 24 * 3600 },
];

// How the clip table folds: one row per camera × bucket. 0 = flat list.
const GROUPINGS = [
  { label: "15 min", secs: 900 },
  { label: "hour", secs: 3600 },
  { label: "3 hours", secs: 3 * 3600 },
  { label: "day", secs: 86400 },
  { label: "no grouping", secs: 0 },
];
const GROUP_KEY = "cammy-rec-group";

// Buckets anchor at local midnight, so "3 hours" and "day" line up with the
// user's clock instead of UTC epoch boundaries.
function bucketOf(ts: number, secs: number): number {
  const dayStart = new Date(ts * 1000).setHours(0, 0, 0, 0) / 1000;
  return dayStart + Math.floor((ts - dayStart) / secs) * secs;
}

function groupLabel(ts: number, secs: number): string {
  if (secs >= 86400)
    return new Date(ts * 1000).toLocaleDateString([], { weekday: "short", month: "numeric", day: "numeric" });
  const minute = secs < 3600 ? ("2-digit" as const) : undefined;
  const from = new Date(ts * 1000).toLocaleString([], { month: "numeric", day: "numeric", hour: "numeric", minute });
  const to = new Date((ts + secs) * 1000).toLocaleTimeString([], { hour: "numeric", minute });
  return `${from} – ${to}`;
}

// P2.3 retroactive region motion search: draw a rectangle on the camera's
// frame, get every archived minute with motion inside it (from the persisted
// 64x64 motion-mask index — no video decode), click a hit to play it.
function MotionSearchModal({
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

// P2.4 thumbnail scrub: the selected camera's window as a grid of segment
// keyframes — eyeball a whole day in seconds instead of scrubbing a timeline.
// One tile per 15-minute bucket; a multi-segment bucket expands in place to
// its per-minute tiles, and clicking any expanded tile plays that segment.
function ScrubGrid({ segments, onPlay }: { segments: Segment[]; onPlay: (s: Segment) => void }) {
  const [openBuckets, setOpenBuckets] = useState<Set<number>>(new Set());
  const buckets = useMemo(() => {
    const by = new Map<number, Segment[]>();
    for (const s of segments) {
      const b = Math.floor(s.start_ts / 900) * 900;
      const arr = by.get(b) ?? [];
      arr.push(s);
      by.set(b, arr);
    }
    return [...by.entries()]
      .map(([ts, segs]) => ({ ts, segs: segs.sort((a, b) => a.start_ts - b.start_ts) }))
      .sort((a, b) => a.ts - b.ts);
  }, [segments]);

  if (buckets.length === 0) return null;
  const tile = (s: Segment, caption: string, count?: number, onClick?: () => void) => (
    <button
      key={`${s.id}-${caption}`}
      type="button"
      className="scrub-tile"
      onClick={onClick ?? (() => onPlay(s))}
      title={count && count > 1 ? `${count} clips, click to expand` : `Play ${caption}`}
    >
      <img src={`/api/recordings/${s.id}/thumb.jpg`} loading="lazy" alt="" />
      <span className="scrub-cap">
        {caption}
        {count && count > 1 ? <span className="scrub-count">×{count}</span> : null}
      </span>
    </button>
  );
  return (
    <div className="card">
      <div className="scrub-grid">
        {buckets.map((b) => {
          const cap = new Date(b.ts * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
          if (!openBuckets.has(b.ts) && b.segs.length > 1) {
            return tile(b.segs[0], cap, b.segs.length, () =>
              setOpenBuckets((prev) => new Set(prev).add(b.ts))
            );
          }
          return b.segs.map((s) =>
            tile(s, new Date(s.start_ts * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" }))
          );
        })}
      </div>
      <p className="muted" style={{ marginBottom: 0 }}>
        Each thumbnail is one clip. Click a stacked (×N) tile to see the 15 minutes inside, or click any frame to play.
      </p>
    </div>
  );
}

export default function Recordings({ cameras }: { cameras: Camera[] }) {
  const [segments, setSegments] = useState<Segment[]>([]);
  const [events, setEvents] = useState<CamEvent[]>([]);
  const [cameraId, setCameraId] = useState<number | "">("");
  // Playback is a queue, not a single file: starting anywhere keeps playing
  // through the camera's following clips so a moment never cuts off at a
  // minute boundary.
  const [playing, setPlaying] = useState<{ queue: Segment[]; index: number; offset: number } | null>(null);
  const [stats, setStats] = useState<Stats | null>(null);
  const [windowSecs, setWindowSecs] = useState(6 * 3600);
  const [segmentSecs, setSegmentSecs] = useState(60);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  // The raw segment list is minute-granularity — hundreds of near-identical
  // rows. Fold it into one row per camera-bucket, expandable to the clips;
  // the bucket size is the user's choice (persisted).
  const [openHours, setOpenHours] = useState<Set<string>>(new Set());
  const [groupSecs, setGroupSecsRaw] = useState(() => {
    const v = Number(localStorage.getItem(GROUP_KEY) ?? 3600);
    return GROUPINGS.some((g) => g.secs === v) ? v : 3600;
  });
  const setGroupSecs = (v: number) => {
    setGroupSecsRaw(v);
    localStorage.setItem(GROUP_KEY, String(v));
  };

  // Day picker: "" = live (anchored at now); a date scrubs that day's history.
  const [day, setDay] = useState("");
  const [scrub, setScrub] = useState(false);
  const [motionOpen, setMotionOpen] = useState(false);
  const [tlBusy, setTlBusy] = useState(false);
  const toast = useToast();

  // Build (or fetch a cached) time-lapse of the selected camera's whole day. The
  // server builds it in the background, so poll until it's ready, then open it.
  const makeTimelapse = async () => {
    if (cameraId === "" || !day || tlBusy) return;
    setTlBusy(true);
    try {
      let r = await api.timelapse(cameraId, day);
      if (r.status === "building") {
        toast.info("Building the time-lapse. A full day can take a minute…");
        const started = Date.now();
        while (r.status === "building" && Date.now() - started < 5 * 60 * 1000) {
          await new Promise((res) => setTimeout(res, 4000));
          r = await api.timelapse(cameraId, day);
        }
      }
      if (r.status === "ready") {
        window.open(r.url, "_blank");
        toast.success("Time-lapse ready, opening it now");
      } else {
        toast.error("Time-lapse is taking longer than expected. Check back shortly.");
      }
    } catch (e) {
      toast.error(`Couldn't build the time-lapse: ${errMsg(e)}`);
    } finally {
      setTlBusy(false);
    }
  };
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
      .catch((e) => setLoadError(errMsg(e)))
      .finally(() => setLoaded(true));
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

  // Open the player on `seg` with the camera's whole loaded window queued
  // after it, so playback rolls clip-to-clip instead of stopping each minute.
  const openSeq = (seg: Segment, offset: number) => {
    const queue = segments
      .filter((x) => x.camera === seg.camera)
      .sort((a, b) => a.start_ts - b.start_ts);
    const index = queue.findIndex((x) => x.id === seg.id);
    if (index < 0) {
      // e.g. a motion-search hit older than the loaded window — play it alone.
      setPlaying({ queue: [seg], index: 0, offset });
      return;
    }
    setPlaying({ queue, index, offset });
  };

  const seekTo = async (ts: number) => {
    if (cameraId === "") return;
    seekCamera(cameraId, ts);
  };
  const seekCamera = async (camId: number, ts: number) => {
    try {
      const r = await api.recordingAt(camId, ts);
      openSeq(r.segment, r.offset_secs);
    } catch {
      /* clicked a gap — nothing recorded there */
    }
  };

  const hourGroups = useMemo<HourGroup[]>(() => {
    const map = new Map<string, HourGroup>();
    for (const s of segments) {
      const hourTs = groupSecs > 0 ? bucketOf(s.start_ts, groupSecs) : s.start_ts;
      const key = `${s.camera}|${hourTs}|${groupSecs > 0 ? "" : s.id}`;
      let g = map.get(key);
      if (!g) {
        g = { key, camera: s.camera, hourTs, segs: [], bytes: 0 };
        map.set(key, g);
      }
      g.segs.push(s);
      g.bytes += s.bytes;
    }
    // Newest bucket first; within a bucket, clips run oldest-first (playback order).
    const groups = [...map.values()].sort(
      (a, b) => b.hourTs - a.hourTs || a.camera.localeCompare(b.camera)
    );
    for (const g of groups) g.segs.sort((a, b) => a.start_ts - b.start_ts);
    return groups;
  }, [segments, groupSecs]);

  // Severity keys ONLY on actual disk headroom (days_until_full): <7 days
  // gets a warn callout and <2 a danger one instead of muted text. The
  // retention horizon is routine pruning, not data loss — it stays
  // neutral informational copy, never a warning. Badge and callout are gated
  // together (the write-rate estimate must exist) so a bare unexplained
  // warning badge can never appear.
  const capTone = stats ? capacityTone(stats.days_until_full) : null;
  const showCap = stats != null && capTone != null && stats.write_bytes_per_day > 0;
  const capDetail = stats && (
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
      {stats.retention_horizon_days != null && (
        <> · recordings older than {fmtDaysLeft(stats.retention_horizon_days)} are removed</>
      )}
      <span style={{ opacity: 0.7 }}> · estimated</span>
    </>
  );

  return (
    <>
      <h1>Recordings</h1>
      <p className="muted" style={{ marginTop: -8 }}>
        Continuous footage — pick a moment on the timeline to play it. For AI detections (person,
        vehicle, and more), see Events.
      </p>

      {/* An action-required disk warning stays loud and first; the routine
          storage breakdown lives in a disclosure at the bottom of the page so
          footage — what people come here for — leads. */}
      {stats && showCap && (
        <Callout tone={capTone!} style={{ marginBottom: 14 }}>
          <b>Disk is filling up</b>. Add more storage, or shorten recording history (retention) so
          it doesn't run out.
          <div className="muted" style={{ marginTop: 2 }}>{capDetail}</div>
        </Callout>
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
        {cameraId !== "" && (
          <button
            className={`btn ${scrub ? "btn-primary" : "btn-ghost"}`}
            onClick={() => setScrub((v) => !v)}
            title="Show this window as a grid of video thumbnails"
            aria-pressed={scrub}
          >
            Scrub
          </button>
        )}
        {cameraId !== "" && (
          <button
            className="btn btn-ghost"
            onClick={() => setMotionOpen(true)}
            title="Find all recorded motion inside an area you draw on the frame"
          >
            Motion search
          </button>
        )}
        {cameraId !== "" && day && (
          <button
            className="btn btn-ghost"
            disabled={tlBusy}
            onClick={makeTimelapse}
            title="Condense this camera's whole day into a short time-lapse video"
          >
            <IconFilm size={14} /> {tlBusy ? "Building…" : "Time-lapse"}
          </button>
        )}
        <label className="field" title="How the clip list below is folded">
          group by
          <select
            aria-label="Group clips by"
            value={groupSecs}
            onChange={(e) => setGroupSecs(Number(e.target.value))}
          >
            {GROUPINGS.map((g) => (
              <option key={g.secs} value={g.secs}>
                {g.label}
              </option>
            ))}
          </select>
        </label>
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
        <>
          <ActivityStrip events={events} windowSecs={windowSecs} nowTs={anchor} />
          <Timeline
            windowSecs={windowSecs}
            segmentSecs={segmentSecs}
            segments={segments}
            events={events}
            onSeek={seekTo}
            nowTs={anchor}
          />
        </>
      )}

      {scrub && cameraId !== "" && segments.length > 0 && (
        <ScrubGrid segments={segments} onPlay={(s) => openSeq(s, 0)} />
      )}

      {segments.length === 0 ? (
        !loaded ? (
          <div className="card" aria-busy="true" aria-label="Loading recordings">
            {[0, 1, 2, 3, 4].map((i) => (
              <div key={i} className="skeleton" style={{ height: 18, margin: "10px 0" }} />
            ))}
          </div>
        ) : loadError ? (
          <ErrorState what="recordings" message={loadError} onRetry={load} />
        ) : (
          <EmptyState
            icon={<IconFilm />}
            title="No recordings yet"
            hint="Recordings appear here about a minute after a camera with recording turned on connects. Check that recording is on for at least one camera."
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
                if (g.segs.length === 1) {
                  const s = g.segs[0];
                  return (
                    <tr key={g.key}>
                      <td><b>{s.camera}</b></td>
                      <td>{fmtTime(s.start_ts)}</td>
                      <td className="muted">{fmtBytes(s.bytes)}</td>
                      <td>
                        <button
                          className="btn btn-ghost ev-act"
                          title="Play, continuing into the clips that follow"
                          onClick={() => openSeq(s, 0)}
                        >
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
                    hourLabel={groupLabel(g.hourTs, groupSecs)}
                    onToggle={() =>
                      setOpenHours((prev) => {
                        const next = new Set(prev);
                        if (next.has(g.key)) next.delete(g.key);
                        else next.add(g.key);
                        return next;
                      })
                    }
                    onPlay={(s) => openSeq(s, 0)}
                    onPlayAll={() => openSeq(g.segs[0], 0)}
                  />
                );
              })}
            </tbody>
          </table>
          </div>
        </div>
      )}

      {stats && (
        <details className="adv" style={{ marginTop: 16 }}>
          <summary>
            Storage
            <span className="muted" style={{ marginLeft: 8 }}>
              {fmtBytes(stats.total_bytes)} recorded
              {stats.disk_free_bytes != null && <> · {fmtBytes(stats.disk_free_bytes)} free</>}
              {stats.write_bytes_per_day > 0 && stats.days_until_full != null && (
                <> · {fmtDaysLeft(stats.days_until_full)} until full</>
              )}
            </span>
            {showCap && (
              <span className={`badge ${capTone}`} style={{ marginLeft: 8 }}>
                <IconAlert size={11} /> {capTone === "danger" ? "Nearly full" : "Filling up"}
              </span>
            )}
          </summary>
          <div className="card" style={{ marginTop: 10 }}>
            <div className="row" style={{ marginBottom: 10 }}>
              <span className="muted">
                {fmtBytes(stats.total_bytes)} of recordings · {fmtBytes(stats.snapshots_bytes)} of
                snapshots · {stats.events_total} events all-time
                {stats.disk_free_bytes != null && <> · {fmtBytes(stats.disk_free_bytes)} free on disk</>}
              </span>
            </div>
            {stats.write_bytes_per_day > 0 && !showCap && (
              <div className="row" style={{ marginBottom: 12 }}>
                <span className="muted">
                  <b>Capacity</b>: {capDetail}
                </span>
              </div>
            )}
            {stats.cameras.map((c) => (
              <div className="row" key={c.camera_id} style={{ marginBottom: 6 }}>
                <span style={{ width: 120 }}>
                  <b>{c.camera}</b>
                  {cameras.find((cc) => cc.id === c.camera_id)?.enabled === false && (
                    <span
                      className="badge"
                      style={{ marginLeft: 6 }}
                      title="This camera is turned off (Cameras page). Its old footage is kept until recording history limits remove it."
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
                  {fmtBytes(c.bytes)} · {c.segments} clips
                  {c.oldest_ts ? ` · since ${new Date(c.oldest_ts * 1000).toLocaleDateString()}` : ""}
                </span>
              </div>
            ))}
          </div>
        </details>
      )}

      {motionOpen && cameraId !== "" && (
        <MotionSearchModal
          cameraId={cameraId}
          from={anchor - windowSecs}
          to={anchor}
          onClose={() => setMotionOpen(false)}
          onPlay={(segId, segStartTs, offset) => {
            const cam = cameras.find((c) => c.id === cameraId);
            const seg =
              segments.find((s) => s.id === segId) ??
              ({ id: segId, camera_id: cameraId, camera: cam?.name ?? "", start_ts: segStartTs, bytes: 0, path: "" } as Segment);
            openSeq(seg, offset);
          }}
        />
      )}

      {playing && (
        <SequencePlayer
          queue={playing.queue}
          index={playing.index}
          offset={playing.offset}
          onClose={() => setPlaying(null)}
        />
      )}
    </>
  );
}

// Playback speeds. 16× turns an hour of footage into a ~4-minute skim — a
// client-side time-lapse with no server render (the ffmpeg day time-lapse
// stays for shareable files).
const RATES = [1, 2, 4, 8, 16];

/// Plays a queue of clips as one continuous recording: the next clip preloads
/// in a hidden <video> while the current one plays, and swaps in on end, so
/// minute boundaries pass without a stall. Prev/next, a ticking wall clock,
/// and a speed control ride in a bar under the video.
function SequencePlayer({
  queue,
  index,
  offset,
  onClose,
}: {
  queue: Segment[];
  index: number;
  offset: number;
  onClose: () => void;
}) {
  const [pos, setPos] = useState(index);
  const [rate, setRate] = useState(1);
  const [clock, setClock] = useState<number | null>(null);
  const [atEnd, setAtEnd] = useState(false);
  // Two persistent <video> slots, addressed by position parity: one is
  // visible and playing, the other buffers the next clip. Advancing flips
  // which slot is live — the incoming clip is already loaded.
  const vids = useRef<(HTMLVideoElement | null)[]>([null, null]);
  const toast = useToast();

  const seg = queue[pos];

  const go = (next: number) => {
    if (next < 0 || next >= queue.length) {
      if (next >= queue.length) setAtEnd(true);
      return;
    }
    vids.current[pos % 2]?.pause(); // never two audio tracks at once
    setAtEnd(false);
    setClock(queue[next].start_ts);
    setPos(next);
  };

  // Promote the active slot whenever position or speed changes.
  useEffect(() => {
    const v = vids.current[pos % 2];
    if (!v) return;
    v.playbackRate = rate;
    v.play().catch(() => {
      if (v.error) {
        // Clip vanished (retention pruned it mid-session) — skip ahead.
        toast.info("That clip isn't available anymore — skipping ahead.");
        setPos((p) => (p + 1 < queue.length ? p + 1 : p));
      } else {
        // Autoplay policy refused audible playback — retry muted.
        v.muted = true;
        v.play().catch(() => {});
      }
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pos, rate]);

  return (
    <Modal bare onClose={onClose}>
      <div className="seq-player">
        {[0, 1].map((par) => {
          const segIdx = pos % 2 === par ? pos : pos + 1;
          const s = queue[segIdx];
          if (!s)
            return (
              <video key={par} ref={(el) => (vids.current[par] = el)} style={{ display: "none" }} />
            );
          const active = segIdx === pos;
          return (
            <video
              key={par}
              ref={(el) => (vids.current[par] = el)}
              src={`/api/recordings/${s.id}/video`}
              controls={active}
              preload="auto"
              muted={!active}
              autoPlay={active}
              style={{ display: active ? "block" : "none" }}
              onLoadedMetadata={(e) => {
                // Clamp: clicking near "now" can resolve into the last closed
                // clip with an offset past its end.
                if (active && segIdx === index && offset > 0) {
                  const v = e.currentTarget;
                  v.currentTime = Math.min(offset, Math.max(0, v.duration - 2));
                }
              }}
              onTimeUpdate={(e) => {
                if (active) setClock(Math.floor(s.start_ts + e.currentTarget.currentTime));
              }}
              onEnded={active ? () => go(pos + 1) : undefined}
              onError={
                active
                  ? () => {
                      toast.info("That clip isn't available — skipping ahead.");
                      go(pos + 1);
                    }
                  : undefined
              }
            />
          );
        })}
        <div className="seq-bar">
          <b>{seg.camera}</b>
          <span className="muted clock">
            {new Date((clock ?? seg.start_ts) * 1000).toLocaleTimeString()}
          </span>
          <span className="muted tnum">
            clip {pos + 1}/{queue.length}
          </span>
          {atEnd && <span className="muted">end of footage</span>}
          <span style={{ flex: 1 }} />
          <button
            type="button"
            className="btn btn-ghost"
            disabled={pos === 0}
            aria-label="Previous clip"
            onClick={() => go(pos - 1)}
          >
            <IconChevronLeft size={14} />
          </button>
          <button
            type="button"
            className="btn btn-ghost"
            disabled={pos + 1 >= queue.length}
            aria-label="Next clip"
            onClick={() => go(pos + 1)}
          >
            <IconChevronRight size={14} />
          </button>
          <label className="field" title="Playback speed — high speeds skim like a time-lapse">
            speed
            <select
              aria-label="Playback speed"
              value={rate}
              onChange={(e) => setRate(Number(e.target.value))}
            >
              {RATES.map((r) => (
                <option key={r} value={r}>
                  {r}×
                </option>
              ))}
            </select>
          </label>
          <button type="button" className="btn btn-ghost" aria-label="Close player" onClick={onClose}>
            <IconX size={14} />
          </button>
        </div>
      </div>
    </Modal>
  );
}

/// One camera-bucket of footage: a summary row that expands into its clips.
function HourRows({
  group,
  open,
  hourLabel,
  onToggle,
  onPlay,
  onPlayAll,
}: {
  group: HourGroup;
  open: boolean;
  hourLabel: string;
  onToggle: () => void;
  onPlay: (s: Segment) => void;
  onPlayAll: () => void;
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
        <td>
          <button
            className="btn btn-ghost ev-act"
            title="Play these clips back-to-back, like one recording"
            onClick={onPlayAll}
          >
            <IconPlay size={13} /> Play all
          </button>
        </td>
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
