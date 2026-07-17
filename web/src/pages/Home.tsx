import { useEffect, useMemo, useState } from "react";
import {
  api,
  CamEvent,
  Camera,
  Stats,
  StatusMap,
  Digest,
  Notification,
  AnalyticsCounts,
  OccupancyReport,
  fmtBytes,
  fmtTime,
  ArmMode,
  capacityTone,
  fmtDaysLeft,
} from "../api";
import { RelTime, useToast, Modal, usePolling } from "../ui";
import {
  IconVideo,
  IconRecDot,
  IconDatabase,
  IconBell,
  IconUser,
  IconCar,
  IconStranger,
  IconHand,
  IconSparkles,
  IconWifiOff,
  IconHome,
  IconShield,
  IconLock,
} from "../icons";

import { prettyLabel } from "../labels";

const VEHICLES = ["car", "truck", "bus", "motorcycle", "bicycle"];

// Spotlights (docs/08 P2.6): rank recent events by "how much would you want to
// see this first" rather than pure recency, so a 2am stranger doesn't sink below
// ten routine daytime car detections. All signals already ride each event.
function importanceScore(e: CamEvent): number {
  const sev = e.severity ?? 0;
  const anom = e.anomaly_score ?? 0;
  let s = sev * 2; // severity tier 1..4 → 2..8
  if (e.face === "?") s += 6; // an unrecognized face (stranger) is the headline
  else if (e.face) s += 3; // a recognized person
  if (e.gesture) s += 5; // a hand-signal event (often a silent panic button)
  if (e.plate) s += 2;
  if (anom >= 0.6) s += anom * 5; // flagged unusual by the anomaly worker
  if (e.flagged) s += 4; // the user bookmarked it
  return s;
}

// A short "why surfaced" chip — only for signals NOT already shown as a badge
// (stranger / known-face / gesture render their own badges in the feed row).
function spotlightReason(e: CamEvent): string | null {
  const sev = e.severity ?? 0;
  const anom = e.anomaly_score ?? 0;
  if (sev >= 4) return "Critical";
  if (anom >= 0.6) return "Unusual";
  if (sev >= 3) return "High";
  if (e.plate && !e.face) return `Plate ${e.plate}`;
  if (e.flagged) return "Saved";
  return null;
}

const ARM_MODES: { id: ArmMode; label: string; icon: JSX.Element; hint: string }[] = [
  { id: "home", label: "Home", icon: <IconHome size={15} />, hint: "Armed while you're home" },
  { id: "away", label: "Away", icon: <IconShield size={15} />, hint: "Armed, fully away" },
  { id: "disarmed", label: "Disarmed", icon: <IconLock size={15} />, hint: "Alerts paused" },
];

/** Local start-of-today in unix seconds. */
function startOfToday(): number {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  return Math.floor(d.getTime() / 1000);
}

function StatCard({
  icon,
  label,
  value,
  sub,
  tone,
}: {
  icon: React.ReactNode;
  label: string;
  value: React.ReactNode;
  sub?: string;
  tone?: "ok" | "warn" | "danger";
}) {
  return (
    <div className="stat-card">
      <span className={`stat-ico ${tone ?? ""}`}>{icon}</span>
      <div className="stat-body">
        <div className="stat-value tnum">{value}</div>
        <div className="stat-label">{label}</div>
        {sub && <div className="stat-sub muted">{sub}</div>}
      </div>
    </div>
  );
}

/** A pulsing placeholder shown in a stat card's value slot before first load. */
function SkelValue() {
  return (
    <span
      className="skeleton"
      style={{ display: "inline-block", width: 56, height: 22, verticalAlign: "-4px" }}
    />
  );
}

/** P3.2 "Ask your cameras": a grounded natural-language question box. Hidden
 *  unless the feature is enabled AND a local AI endpoint is configured
 *  (capabilities.ask). The answer is UNTRUSTED model text (display only); the
 *  cited events are REAL rows the user can open to verify. */
function AskBox({ onOpenEvent }: { onOpenEvent?: (id: number) => void }) {
  const [available, setAvailable] = useState<boolean | null>(null);
  const [q, setQ] = useState("");
  const [busy, setBusy] = useState(false);
  const [answer, setAnswer] = useState<string | null>(null);
  const [cited, setCited] = useState<CamEvent[]>([]);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    api
      .capabilities()
      .then((c) => setAvailable(!!c.ask))
      .catch(() => setAvailable(false));
  }, []);

  if (!available) return null; // hidden + honest when unconfigured

  const ask = async () => {
    const question = q.trim();
    if (!question || busy) return;
    setBusy(true);
    setErr(null);
    setAnswer(null);
    setCited([]);
    try {
      const r = await api.ask(question);
      if (!r.available) {
        setErr(r.reason ?? "Ask is not available right now.");
      } else {
        setAnswer(r.answer ?? "");
        setCited(r.cited_events ?? []);
      }
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="card" style={{ marginBottom: 18 }}>
      <div className="card-head">
        <span className="eyebrow">
          <IconSparkles size={13} /> Ask your cameras
        </span>
      </div>
      <div className="row" style={{ gap: 8 }}>
        <input
          type="text"
          value={q}
          placeholder="e.g. how many people came to the front door today?"
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && ask()}
          style={{ flex: 1, minWidth: 240 }}
          aria-label="Ask a question about your cameras"
        />
        <button type="button" className="primary" onClick={ask} disabled={busy || !q.trim()}>
          {busy ? "Thinking…" : "Ask"}
        </button>
      </div>
      {err && (
        <div className="callout callout-warn" role="alert" style={{ marginTop: 10 }}>
          <span className="callout-ico"><IconWifiOff size={16} /></span>
          <div>{err}</div>
        </div>
      )}
      {answer !== null && (
        <div style={{ marginTop: 12 }}>
          {answer.trim() === "" && cited.length === 0 ? (
            <p className="muted" role="status" aria-live="polite" style={{ marginTop: 0 }}>
              No answer for that — try rephrasing your question.
            </p>
          ) : (
            <div role="status" aria-live="polite" aria-atomic="false">
              <p style={{ whiteSpace: "pre-wrap", marginTop: 0 }}>{answer}</p>
              <p className="muted" style={{ fontSize: "var(--text-xs)", marginTop: 4 }}>
                AI-generated from your event history — check it against the events below.
              </p>
            </div>
          )}
          {cited.length > 0 && (
            <div className="recent-feed" style={{ marginTop: 8 }}>
              {cited.map((e) => (
                <button
                  type="button"
                  className="feed-item"
                  key={e.id}
                  onClick={() => onOpenEvent?.(e.id)}
                  style={{ textAlign: "left", background: "none", border: "none", cursor: "pointer", padding: 0 }}
                  title="Open this event"
                >
                  {e.snapshot && (
                    <span className="feed-thumb">
                      <img src={`/api/snapshots/${e.snapshot}?w=160`} alt={e.label} loading="lazy" />
                    </span>
                  )}
                  <div>
                    <b style={{ textTransform: "capitalize" }}>{prettyLabel(e.label)}</b>{" "}
                    <span className="muted">· {e.camera}</span>
                    <span className="muted" style={{ marginLeft: 6, fontSize: "var(--text-xs)" }}>
                      #{e.id}
                    </span>
                    <RelTime
                      ts={e.ts}
                      className="muted clock"
                      style={{ display: "block", fontSize: "var(--text-xs)" }}
                    />
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export default function Home({
  cameras,
  onOpenEvents,
  onOpenCamera,
  onOpenEvent,
}: {
  cameras: Camera[];
  onOpenEvents: () => void;
  onOpenCamera: (c: Camera) => void;
  onOpenEvent?: (eventId: number) => void;
}) {
  const toast = useToast();
  const [stats, setStats] = useState<Stats | null>(null);
  const [status, setStatus] = useState<StatusMap>({});
  const [events, setEvents] = useState<CamEvent[]>([]);
  const [digest, setDigest] = useState<Digest | null>(null);
  const [notes, setNotes] = useState<Notification[]>([]);
  const [arm, setArm] = useState<ArmMode | null>(null);
  const [armErr, setArmErr] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [lightbox, setLightbox] = useState<CamEvent | null>(null);
  const [throughput, setThroughput] = useState<AnalyticsCounts | null>(null);
  const [occ, setOcc] = useState<OccupancyReport | null>(null);
  const [feedMode, setFeedMode] = useState<"spot" | "recent">(() =>
    localStorage.getItem("home_feed_mode") === "recent" ? "recent" : "spot",
  );
  useEffect(() => {
    localStorage.setItem("home_feed_mode", feedMode);
  }, [feedMode]);

  const load = () => {
    // Core data drives the at-a-glance cards; mark loaded only once these
    // settle so the health tiles don't flash a confident "all healthy" / 0
    // before the first response.
    const core = [
      api.stats().then(setStats),
      api.status().then(setStatus),
      api.armMode().then((r) => { setArm(r.arm_mode); setArmErr(false); }).catch((e) => { setArmErr(true); throw e; }),
      api.events({ limit: 500 }).then(setEvents),
    ].map((p) => p.catch(() => {}));
    Promise.allSettled(core).then(() => setLoaded(true));
    // These are best-effort: the endpoints exist only once the backend
    // build ships the digest/notifications/analytics features.
    api.digests(1).then((d) => setDigest(d[0] ?? null)).catch(() => {});
    api.notifications({ limit: 6 }).then(setNotes).catch(() => {});
    api.analyticsCounts(startOfToday()).then(setThroughput).catch(() => {});
    api.analyticsOccupancy().then(setOcc).catch(() => {});
  };
  // Poll every 15s, but pause while backgrounded + refresh on return.
  usePolling(load, 15000);

  const setMode = async (m: ArmMode) => {
    const prev = arm;
    setArm(m); // optimistic
    try {
      const r = await api.arm(m);
      setArm(r.arm_mode);
      toast.success(m === "disarmed" ? "System disarmed" : `Armed (${m === "home" ? "Home" : "Away"})`);
    } catch (e) {
      setArm(prev);
      toast.error(String(e));
    }
  };

  const enabled = cameras.filter((c) => c.enabled);
  const online = enabled.filter((c) => status[String(c.id)]?.online).length;
  const recording = Object.values(status).filter((s) => s.recording).length;
  const offline = enabled.filter((c) => status[String(c.id)] && !status[String(c.id)]?.online);

  const todayStart = startOfToday();
  const today = useMemo(() => events.filter((e) => e.ts >= todayStart), [events, todayStart]);
  const counts = useMemo(() => {
    const acc: Record<string, number> = {};
    for (const e of today) acc[e.label] = (acc[e.label] ?? 0) + 1;
    return Object.entries(acc).sort((a, b) => b[1] - a[1]);
  }, [today]);

  // Per-tripwire throughput today: a_to_b = "in", b_to_a = "out", net = in − out.
  const lines = useMemo(() => {
    const m = new Map<string, { in: number; out: number }>();
    for (const c of throughput?.crossings ?? []) {
      const name = c.tripwire ?? "(unnamed)";
      const e = m.get(name) ?? { in: 0, out: 0 };
      if (c.direction === "a_to_b") e.in += c.count;
      else if (c.direction === "b_to_a") e.out += c.count;
      m.set(name, e);
    }
    return [...m.entries()].map(([name, v]) => ({ name, ...v, net: v.in - v.out }));
  }, [throughput]);

  // Live occupancy rows (one per camera+zone), busiest first.
  const occRows = useMemo(() => {
    const rows: { camera: string; zone: string; count: number }[] = [];
    for (const c of occ?.cameras ?? [])
      for (const [zone, count] of Object.entries(c.zones)) rows.push({ camera: c.camera, zone, count });
    return rows.sort((a, b) => b.count - a.count);
  }, [occ]);

  const lastPerson = events.find((e) => e.label === "person");
  const lastVehicle = events.find((e) => VEHICLES.includes(e.label));
  const lastStranger = events.find((e) => e.face === "?");

  const recent = events.slice(0, 10);
  // Rank the most-recent ~120 events by importance × recency for the Spotlights
  // feed: a stranger/critical event floats up, but a fresh routine event still
  // beats a 3-day-old one — recency halves the weight roughly every 12h.
  const spotlights = useMemo(() => {
    const now = Date.now() / 1000;
    return events
      .slice(0, 120)
      .map((e) => {
        const ageHours = Math.max(0, (now - e.ts) / 3600);
        const recency = 1 / (1 + ageHours / 12);
        return { ev: e, score: importanceScore(e) * recency };
      })
      .sort((a, b) => b.score - a.score || b.ev.ts - a.ev.ts)
      .slice(0, 6)
      .map((x) => x.ev);
  }, [events]);
  const spotOn = feedMode === "spot" && spotlights.length > 0;
  const feed = spotOn ? spotlights : recent;

  // Escalate the disk tile when the drive is filling up (data-loss risk),
  // sharing the Recordings capacity thresholds. days_until_full is the only
  // clean signal (Stats carries no disk-total to derive a fraction).
  const daysUntilFull = stats?.days_until_full ?? null;
  const diskTone = capacityTone(daysUntilFull) ?? undefined;

  // The digest is a run-on paragraph; split into sentences so it reads as a
  // scannable list rather than a wall of prose. (No regex lookbehind here —
  // it's a parse-time SyntaxError on Safari <16.4 and esbuild doesn't
  // transpile regex, so a lookbehind would white-screen the whole app.)
  const digestSentences = (digest?.text ?? "")
    .split(/\.\s+/)
    .map((s, i, all) => (i < all.length - 1 ? `${s.trim()}.` : s.trim()))
    .filter(Boolean);

  return (
    <>
      <h1>Home</h1>

      <div className="arm-bar" role="group" aria-label="Security mode" aria-busy={!loaded}>
        {ARM_MODES.map((m) => (
          <button
            key={m.id}
            type="button"
            className={`arm-opt arm-${m.id} ${arm === m.id ? "active" : ""}`}
            aria-pressed={arm === m.id}
            title={arm === null ? "Security mode unavailable" : m.hint}
            disabled={arm === null}
            onClick={() => arm !== m.id && setMode(m.id)}
          >
            {m.icon}
            <span>{m.label}</span>
          </button>
        ))}
      </div>
      {loaded && arm === null && (
        <p className="muted" style={{ marginTop: -10, marginBottom: 18 }}>
          {armErr ? "Couldn't reach the security mode control. Retrying." : "Security mode unavailable."}
        </p>
      )}

      <div className="stat-grid" aria-busy={!loaded}>
        <StatCard
          icon={<IconVideo size={20} />}
          label="Cameras online"
          value={loaded ? `${online}/${enabled.length}` : <SkelValue />}
          sub={
            loaded
              ? [
                  offline.length ? `${offline.length} offline` : "all healthy",
                  cameras.length > enabled.length
                    ? `${cameras.length - enabled.length} disabled`
                    : null,
                ]
                  .filter(Boolean)
                  .join(" · ")
              : undefined
          }
          tone={loaded ? (offline.length ? "warn" : "ok") : undefined}
        />
        <StatCard
          icon={<IconRecDot size={18} />}
          label="Recording"
          value={loaded ? recording : <SkelValue />}
          sub={`of ${enabled.length} cameras`}
          tone={loaded && recording > 0 ? "danger" : undefined}
        />
        <StatCard
          icon={<IconSparkles size={20} />}
          label="Events today"
          value={loaded ? today.length : <SkelValue />}
          sub={stats ? `${stats.events_total.toLocaleString()} all time` : ""}
        />
        <StatCard
          icon={<IconDatabase size={20} />}
          label="Free space"
          value={stats ? fmtBytes(stats.disk_free_bytes) : <SkelValue />}
          sub={
            stats
              ? diskTone && daysUntilFull != null
                ? `${fmtDaysLeft(daysUntilFull)} until full`
                : `${fmtBytes(stats.total_bytes)} recorded`
              : ""
          }
          tone={diskTone}
        />
      </div>

      <AskBox onOpenEvent={onOpenEvent} />

      <div className="home-cols">
        <div className="card">
          <h2>Today by type</h2>
          {counts.length === 0 ? (
            <p className="muted">No detections yet today.</p>
          ) : (
            <div className="row" style={{ flexWrap: "wrap" }}>
              {counts.map(([label, n]) => (
                <span key={label} className="badge" style={{ textTransform: "capitalize" }}>
                  {prettyLabel(label)} <b className="tnum">{n}</b>
                </span>
              ))}
            </div>
          )}

          <h2 style={{ marginTop: 18 }}>Last seen</h2>
          <div className="lastseen">
            <LastSeen icon={<IconUser size={15} />} label="Person" ev={lastPerson} onOpen={onOpenEvent} />
            <LastSeen icon={<IconCar size={15} />} label="Vehicle" ev={lastVehicle} onOpen={onOpenEvent} />
            {lastStranger && (
              <LastSeen icon={<IconStranger size={15} />} label="Stranger" ev={lastStranger} tone="warn" onOpen={onOpenEvent} />
            )}
          </div>

          {offline.length > 0 && (
            <>
              <h2 style={{ marginTop: 18 }}>Needs attention</h2>
              {offline.map((c) => (
                <button key={c.id} className="attn-row" onClick={() => onOpenCamera(c)}>
                  <IconWifiOff size={15} />
                  <b>{c.name}</b>
                  <span className="muted">offline</span>
                </button>
              ))}
            </>
          )}
        </div>

        <div className="card">
          <div className="card-head">
            <h2 style={{ margin: 0 }}>{spotOn ? "Spotlights" : "Recent activity"}</h2>
            <div className="row" style={{ marginLeft: "auto", gap: 6, alignItems: "center" }}>
              <div className="row" role="group" aria-label="Feed ranking" style={{ gap: 4 }}>
                <button
                  type="button"
                  className={`btn ${feedMode === "spot" ? "btn-primary" : "btn-ghost"} ev-act`}
                  aria-pressed={feedMode === "spot"}
                  title="Rank by importance: strangers, critical and unusual events first"
                  onClick={() => setFeedMode("spot")}
                >
                  Spotlights
                </button>
                <button
                  type="button"
                  className={`btn ${feedMode === "recent" ? "btn-primary" : "btn-ghost"} ev-act`}
                  aria-pressed={feedMode === "recent"}
                  title="Most recent first"
                  onClick={() => setFeedMode("recent")}
                >
                  Recent
                </button>
              </div>
              <button className="btn btn-ghost ev-act" onClick={onOpenEvents}>
                View all
              </button>
            </div>
          </div>
          {feed.length === 0 ? (
            <p className="muted">No events yet.</p>
          ) : (
            <div className="recent-feed">
              {feed.map((e) => {
                const reason = spotOn ? spotlightReason(e) : null;
                return (
                <div className="feed-item" key={e.id}>
                  {e.snapshot && (
                    <button
                      type="button"
                      className="feed-thumb"
                      title="View snapshot"
                      aria-label={`View ${e.label} snapshot from ${e.camera}`}
                      onClick={() => setLightbox(e)}
                    >
                      <img src={`/api/snapshots/${e.snapshot}?w=160`} alt={e.label} loading="lazy" />
                    </button>
                  )}
                  <div>
                    <b style={{ textTransform: "capitalize" }}>{prettyLabel(e.label)}</b>{" "}
                    <span className="muted">· {e.camera}</span>
                    {e.face === "?" ? (
                      <span className="badge warn" style={{ marginLeft: 6 }}>
                        <IconStranger size={11} /> stranger
                      </span>
                    ) : e.face ? (
                      <span className="badge ok" style={{ marginLeft: 6 }}>
                        <IconUser size={11} /> {e.face}
                      </span>
                    ) : null}
                    {e.gesture && (
                      <span className="badge accent" style={{ marginLeft: 6 }}>
                        <IconHand size={11} /> {e.gesture}
                      </span>
                    )}
                    {reason && (
                      <span className="badge" style={{ marginLeft: 6 }} title="Why this is spotlighted">
                        {reason}
                      </span>
                    )}
                    <RelTime ts={e.ts} className="muted clock" style={{ display: "block", fontSize: "var(--text-xs)" }} />
                  </div>
                </div>
                );
              })}
            </div>
          )}
        </div>
      </div>

      {/* The digest is a recap, not the headline — live activity above reads
          first; yesterday's summary follows it. */}
      {digest && (
        <div className="card digest-card">
          <div className="card-head">
            <span className="eyebrow"><IconSparkles size={13} /> Daily digest</span>
            <span className="muted clock" style={{ marginLeft: "auto" }} title={fmtTime(digest.ts)}>
              {Date.now() / 1000 - digest.ts > 26 * 3600 && (
                <span className="badge warn" style={{ marginRight: 8 }} title="No new daily summary yet. You can adjust it under Settings, Detection & AI, daily digest.">
                  not updated
                </span>
              )}
              <RelTime ts={digest.ts} />
            </span>
          </div>
          {digestSentences.length > 1 ? (
            <ul className="digest-list">
              {digestSentences.map((s, i) => (
                <li key={i}>{s}</li>
              ))}
            </ul>
          ) : (
            <p className="digest-text">{digest.text}</p>
          )}
        </div>
      )}

      {(lines.length > 0 || occRows.length > 0) && (
        <div className="home-cols">
          {lines.length > 0 && (
            <div className="card">
              <h2>Comings and goings today</h2>
              <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                <div className="muted" style={{ display: "flex", fontSize: "var(--text-xs)" }}>
                  <span style={{ flex: 1 }}>Crossing</span>
                  <span style={{ width: 48, textAlign: "right" }}>In</span>
                  <span style={{ width: 48, textAlign: "right" }}>Out</span>
                  <span style={{ width: 56, textAlign: "right" }}>Difference</span>
                </div>
                {lines.map((l) => (
                  <div key={l.name} style={{ display: "flex", alignItems: "center" }}>
                    <span style={{ flex: 1 }}>{l.name}</span>
                    <span className="tnum" style={{ width: 48, textAlign: "right" }}>{l.in}</span>
                    <span className="tnum" style={{ width: 48, textAlign: "right" }}>{l.out}</span>
                    <b className="tnum" style={{ width: 56, textAlign: "right" }}>
                      {l.net >= 0 ? `+${l.net}` : l.net}
                    </b>
                  </div>
                ))}
              </div>
            </div>
          )}
          {occRows.length > 0 && (
            <div className="card">
              <h2>Live occupancy</h2>
              {occRows.some((r) => r.count > 0) ? (
                <div className="row" style={{ flexWrap: "wrap" }}>
                  {occRows
                    .filter((r) => r.count > 0)
                    .map((r) => (
                      <span key={`${r.camera}-${r.zone}`} className="badge">
                        {r.camera} · {r.zone} <b className="tnum">{r.count}</b>
                      </span>
                    ))}
                </div>
              ) : (
                <p className="muted" style={{ margin: 0 }}>
                  No one in any monitored zone right now
                  <span className="muted"> · watching {occRows.length} zone{occRows.length === 1 ? "" : "s"}</span>
                </p>
              )}
            </div>
          )}
        </div>
      )}

      {notes.length > 0 && (
        <div className="card">
          <h2><IconBell size={13} /> Latest notifications</h2>
          {notes.map((n) => {
            const clickable = n.event_id != null && !!onOpenEvent;
            // Health notifications append the raw fetch error (URL, status code)
            // after an em dash — keep the plain-language clause, tuck the
            // technical tail into a hover title.
            const tech = n.body != null && /https?:\/\//.test(n.body);
            const shownBody = tech ? n.body!.split(" — ")[0] : n.body;
            const body = (
              <div>
                <b>{n.title}</b>
                {shownBody && (
                  <span className="muted" title={tech ? (n.body ?? undefined) : undefined}>
                    {" "}— {shownBody}
                  </span>
                )}
                <RelTime ts={n.ts} className="muted clock" style={{ display: "block", fontSize: "var(--text-xs)" }} />
              </div>
            );
            return clickable ? (
              <button
                key={n.id}
                className="feed-item"
                style={{ width: "100%", textAlign: "left", background: "none", border: "none", color: "inherit", font: "inherit" }}
                onClick={() => onOpenEvent!(n.event_id!)}
                aria-label={`Open event: ${n.title}`}
              >
                {body}
              </button>
            ) : (
              <div className="feed-item" key={n.id} style={{ cursor: "default" }}>
                {body}
              </div>
            );
          })}
        </div>
      )}

      {lightbox && lightbox.snapshot && (
        <Modal
          className="lightbox"
          title={`${lightbox.label} · ${lightbox.camera}`}
          onClose={() => setLightbox(null)}
        >
          <img
            src={`/api/snapshots/${lightbox.snapshot}`}
            alt={`${lightbox.label} on ${lightbox.camera}`}
            style={{ display: "block", width: "100%" }}
          />
          <div className="lightbox-meta">
            <span className="muted">{lightbox.camera}</span>
            {lightbox.face && lightbox.face !== "?" && (
              <span className="badge ok">
                <IconUser size={11} /> {lightbox.face}
              </span>
            )}
            <RelTime ts={lightbox.ts} className="muted clock" style={{ marginLeft: "auto" }} />
          </div>
        </Modal>
      )}
    </>
  );
}

function LastSeen({
  icon,
  label,
  ev,
  tone,
  onOpen,
}: {
  icon: React.ReactNode;
  label: string;
  ev?: CamEvent;
  tone?: "warn";
  onOpen?: (eventId: number) => void;
}) {
  const body = (
    <>
      <span className={`lastseen-ico ${tone ?? ""}`}>{icon}</span>
      <div className="lastseen-body">
        <div className="lastseen-label">{label}</div>
        {ev ? (
          <div className="muted">
            {ev.camera} · <RelTime ts={ev.ts} className="clock" />
          </div>
        ) : (
          <div className="muted">not seen recently</div>
        )}
      </div>
      {ev?.snapshot && <img className="lastseen-thumb" src={`/api/snapshots/${ev.snapshot}?w=120`} alt={label} loading="lazy" decoding="async" />}
    </>
  );
  // Clickable (opens the underlying event) only when there's an event and a handler.
  if (ev && onOpen) {
    return (
      <button type="button" className="lastseen-item" aria-label={`Open last ${label.toLowerCase()} event`} onClick={() => onOpen(ev.id)}>
        {body}
      </button>
    );
  }
  return <div className="lastseen-item">{body}</div>;
}
