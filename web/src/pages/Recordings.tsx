import { useEffect, useMemo, useRef, useState } from "react";
import { api, CamEvent, Camera, capacityTone, fmtBytes, fmtDaysLeft, fmtSpan, fmtTime, Segment, Stats } from "../api";
import Timeline from "../Timeline";
import CrossTimeline, { ActivityStrip } from "../CrossTimeline";
import { IconPlay, IconFilm, IconAlert } from "../icons";
import { Callout, EmptyState, ErrorState, useToast } from "../ui";
import DayStrip, { fromDateInput, toDateInput, windowForDay } from "../DayStrip";
// The page's parts live in `../recordings/` so Find can mount the same footage
// machinery without importing a 1200-line page. Moving them changed nothing
// about what they do.
import { bucketOf, errMsg, GROUP_KEY, GROUPINGS, groupLabel, HourGroup } from "../recordings/buckets";
import ExportRangeCard from "../recordings/ExportRangeCard";
import MotionSearchModal from "../recordings/MotionSearchModal";
import ScrubGrid from "../recordings/ScrubGrid";
import SequencePlayer from "../recordings/SequencePlayer";
import HourRows from "../recordings/HourRows";

const WINDOWS = [
  { label: "1h", secs: 3600 },
  { label: "6h", secs: 6 * 3600 },
  { label: "24h", secs: 24 * 3600 },
];

export default function Recordings({ cameras }: { cameras: Camera[] }) {
  const [segments, setSegments] = useState<Segment[]>([]);
  const [events, setEvents] = useState<CamEvent[]>([]);
  const [cameraId, setCameraId] = useState<number | "">("");
  // Playback is a queue, not a single file: starting anywhere keeps playing
  // through the camera's following clips so a moment never cuts off at a
  // minute boundary.
  const [playing, setPlaying] = useState<{ queue: Segment[]; index: number; offset: number } | null>(null);
  // Skip the 10s recordings/events refetch (1000+1500 rows) while the tab is
  // hidden or a clip is playing — both cases don't want the list churning.
  const playingRef = useRef(false);
  useEffect(() => {
    playingRef.current = playing !== null;
  }, [playing]);
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
      .then((all) => {
        // `/api/recordings` bounds only the top (`before`), so asking for a day
        // whose footage retention has already deleted returns the newest
        // SURVIVING segments from before it — silently showing you a different
        // day. Clamp to the requested day so an empty day reads as empty.
        const from = day ? fromDateInput(day) : null;
        setSegments(
          from == null ? all : all.filter((x) => x.start_ts >= from && x.start_ts < from + 86400),
        );
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
    const t = setInterval(() => {
      if (document.hidden || playingRef.current) return;
      load();
    }, 10000);
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
        g = { key, camera: s.camera, cameraId: s.camera_id, hourTs, segs: [], bytes: 0, counts: null };
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

    // Label each bucket with what was detected in it, derived from the events
    // this page ALREADY fetched for the timeline — no extra request. Buckets
    // older than the oldest event we hold get `null`, not an empty list: a bare
    // "0 detections" on an hour we simply didn't ask about would be a lie about
    // whether that footage is worth watching.
    const oldestEventTs = events.length ? Math.min(...events.map((e) => e.ts)) : Infinity;
    const span = groupSecs > 0 ? groupSecs : segmentSecs;
    for (const g of groups) {
      if (g.hourTs < oldestEventTs) continue; // outside what we know — stay silent
      const tally = new Map<string, number>();
      for (const e of events) {
        if (e.camera_id !== g.cameraId) continue;
        if (e.ts < g.hourTs || e.ts >= g.hourTs + span) continue;
        tally.set(e.label, (tally.get(e.label) ?? 0) + 1);
      }
      g.counts = [...tally.entries()].sort((a, b) => b[1] - a[1]).slice(0, 3);
    }
    return groups;
  }, [segments, groupSecs, events, segmentSecs]);

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
        <>
          {" · footage goes back "}
          {fmtSpan(stats.retention_horizon_days)}
          {stats.retention_limit === "disk" && " (limited by the storage cap, not the day setting)"}
        </>
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
        {/* The same day control Events uses, so moving between the two pages
            doesn't mean re-learning how to pick a day — and the density calendar
            shows where the activity actually is rather than making you guess one
            date at a time. `day` stays a YYYY-MM-DD string because the timelapse
            endpoint takes one; DayStrip is bridged to it. */}
        <DayStrip
          value={day ? windowForDay(fromDateInput(day) ?? 0) : null}
          onChange={(w) => {
            setDay(w ? toDateInput(w.from) : "");
            if (w) setWindowSecs(24 * 3600);
          }}
        />
        {/* Say how far back footage reaches, right where days are chosen. The
            number lived only inside the collapsed storage disclosure, so the
            day picker cheerfully offered weeks that hold no video at all. */}
        {stats?.retention_horizon_days != null && stats.retention_horizon_days < 2 && (
          <span
            className="muted"
            title={
              stats.retention_limit === "disk"
                ? "The storage cap fills before the day limit is reached, so it is what decides how far back you can look. Raise it in Settings › Recording, or record a lower-resolution sub-stream."
                : "Set by your recording-history limit in Settings › Recording."
            }
          >
            footage goes back {fmtSpan(stats.retention_horizon_days)}
            {stats.retention_limit === "disk" && " · storage cap"}
          </span>
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

      {cameraId !== "" && segments.length > 0 && (
        <ExportRangeCard
          cameraId={cameraId as number}
          // Default to the start of the newest clip — the most recent footage
          // is what someone reaching for "export" almost always wants.
          defaultFrom={Math.max(...segments.map((s) => s.start_ts))}
        />
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
          day ? (
            // Detections are kept far longer than video (event retention vs the
            // disk cap), so the density calendar happily offers a day whose
            // footage is long gone. Say that, and point at where the record of
            // that day DOES still live, instead of showing an empty table.
            <EmptyState
              icon={<IconFilm />}
              title={`No footage saved from ${new Date((fromDateInput(day) ?? 0) * 1000).toLocaleDateString(
                undefined,
                { weekday: "long", month: "long", day: "numeric" },
              )}`}
              hint="Video is deleted once it passes your recording-history limit or the disk cap, which is usually sooner than detections are. The detections from that day are likely still in Find."
              action={
                // Carry the day the user just picked (docs/10 P3) — Find's hash
                // schema restores it, so the link lands scoped, not on "today".
                <a className="btn btn-ghost" href={`#/find?day=${day}&view=list`}>
                  See that day&apos;s detections → Find
                </a>
              }
            />
          ) : (
            <EmptyState
              icon={<IconFilm />}
              title="No recordings yet"
              hint="Recordings appear here about a minute after a camera with recording turned on connects. Check that recording is on for at least one camera."
            />
          )
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
              /* docs/11 P2 — "Nearly full" with nowhere to click is a worry,
                 not a warning. The knob that fixes it is one page away. */
              <a
                className={`badge ${capTone}`}
                style={{ marginLeft: 8, textDecoration: "none" }}
                href="#/settings/recording"
                title="Change how long footage is kept, the disk budget, or where recordings are stored"
                onClick={(e) => e.stopPropagation()}
              >
                <IconAlert size={11} /> {capTone === "danger" ? "Nearly full" : "Filling up"} ·
                change retention
              </a>
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
          subAvailable={
            !!cameras.find((c) => c.id === playing.queue[playing.index]?.camera_id)?.detect_config
              .record_substream
          }
        />
      )}
    </>
  );
}
