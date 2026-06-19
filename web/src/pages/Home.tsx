import { useEffect, useMemo, useState } from "react";
import {
  api,
  CamEvent,
  Camera,
  Stats,
  StatusMap,
  Digest,
  Notification,
  fmtBytes,
  fmtTime,
  ArmMode,
} from "../api";
import { RelTime, useToast, Modal } from "../ui";
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

const VEHICLES = ["car", "truck", "bus", "motorcycle", "bicycle"];

const ARM_MODES: { id: ArmMode; label: string; icon: JSX.Element; hint: string }[] = [
  { id: "home", label: "Home", icon: <IconHome size={15} />, hint: "Armed — you're home" },
  { id: "away", label: "Away", icon: <IconShield size={15} />, hint: "Armed — fully away" },
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

export default function Home({
  cameras,
  onOpenEvents,
  onOpenCamera,
}: {
  cameras: Camera[];
  onOpenEvents: () => void;
  onOpenCamera: (c: Camera) => void;
}) {
  const toast = useToast();
  const [stats, setStats] = useState<Stats | null>(null);
  const [status, setStatus] = useState<StatusMap>({});
  const [events, setEvents] = useState<CamEvent[]>([]);
  const [digest, setDigest] = useState<Digest | null>(null);
  const [notes, setNotes] = useState<Notification[]>([]);
  const [arm, setArm] = useState<ArmMode | null>(null);
  const [lightbox, setLightbox] = useState<CamEvent | null>(null);

  useEffect(() => {
    const load = () => {
      api.stats().then(setStats).catch(() => {});
      api.status().then(setStatus).catch(() => {});
      api.armMode().then((r) => setArm(r.arm_mode)).catch(() => {});
      api.events({ limit: 500 }).then(setEvents).catch(() => {});
      // These two are best-effort: the endpoints exist only once the backend
      // build ships the digest/notifications features.
      api.digests(1).then((d) => setDigest(d[0] ?? null)).catch(() => {});
      api.notifications({ limit: 6 }).then(setNotes).catch(() => {});
    };
    load();
    const t = setInterval(load, 15000);
    return () => clearInterval(t);
  }, []);

  const setMode = async (m: ArmMode) => {
    const prev = arm;
    setArm(m); // optimistic
    try {
      const r = await api.arm(m);
      setArm(r.arm_mode);
      toast.success(m === "disarmed" ? "System disarmed" : `Armed — ${m === "home" ? "Home" : "Away"}`);
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

  const lastPerson = events.find((e) => e.label === "person");
  const lastVehicle = events.find((e) => VEHICLES.includes(e.label));
  const lastStranger = events.find((e) => e.face === "?");

  const recent = events.slice(0, 10);

  return (
    <>
      <h1>Overview</h1>

      <div className="arm-bar" role="group" aria-label="Security mode">
        {ARM_MODES.map((m) => (
          <button
            key={m.id}
            type="button"
            className={`arm-opt arm-${m.id} ${arm === m.id ? "active" : ""}`}
            aria-pressed={arm === m.id}
            title={m.hint}
            disabled={arm === null}
            onClick={() => arm !== m.id && setMode(m.id)}
          >
            {m.icon}
            <span>{m.label}</span>
          </button>
        ))}
      </div>

      <div className="stat-grid">
        <StatCard
          icon={<IconVideo size={20} />}
          label="Cameras online"
          value={`${online}/${enabled.length}`}
          sub={offline.length ? `${offline.length} offline` : "all healthy"}
          tone={offline.length ? "warn" : "ok"}
        />
        <StatCard
          icon={<IconRecDot size={18} />}
          label="Recording"
          value={recording}
          sub={`of ${enabled.length} cameras`}
          tone={recording > 0 ? "danger" : undefined}
        />
        <StatCard
          icon={<IconSparkles size={20} />}
          label="Events today"
          value={today.length}
          sub={stats ? `${stats.events_total.toLocaleString()} all time` : ""}
        />
        <StatCard
          icon={<IconDatabase size={20} />}
          label="Free space"
          value={stats ? fmtBytes(stats.disk_free_bytes) : "…"}
          sub={stats ? `${fmtBytes(stats.total_bytes)} recorded` : ""}
        />
      </div>

      {digest && (
        <div className="card digest-card">
          <div className="card-head">
            <span className="eyebrow"><IconSparkles size={13} /> Daily digest</span>
            <span className="muted clock" style={{ marginLeft: "auto" }}>{fmtTime(digest.ts)}</span>
          </div>
          <p className="digest-text">{digest.text}</p>
        </div>
      )}

      <div className="home-cols">
        <div className="card">
          <h2>Today by type</h2>
          {counts.length === 0 ? (
            <p className="muted">No detections yet today.</p>
          ) : (
            <div className="row" style={{ flexWrap: "wrap" }}>
              {counts.map(([label, n]) => (
                <span key={label} className="badge accent" style={{ textTransform: "capitalize" }}>
                  {label} <b className="tnum">{n}</b>
                </span>
              ))}
            </div>
          )}

          <h2 style={{ marginTop: 18 }}>Last seen</h2>
          <div className="lastseen">
            <LastSeen icon={<IconUser size={15} />} label="Person" ev={lastPerson} />
            <LastSeen icon={<IconCar size={15} />} label="Vehicle" ev={lastVehicle} />
            {lastStranger && (
              <LastSeen icon={<IconStranger size={15} />} label="Stranger" ev={lastStranger} tone="warn" />
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
            <h2 style={{ margin: 0 }}>Recent activity</h2>
            <button className="btn btn-ghost ev-act" style={{ marginLeft: "auto" }} onClick={onOpenEvents}>
              View all
            </button>
          </div>
          {recent.length === 0 ? (
            <p className="muted">No events yet.</p>
          ) : (
            <div className="recent-feed">
              {recent.map((e) => (
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
                    <b style={{ textTransform: "capitalize" }}>{e.label}</b>{" "}
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
                    <RelTime ts={e.ts} className="muted clock" style={{ display: "block", fontSize: "0.75rem" }} />
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>

      {notes.length > 0 && (
        <div className="card">
          <h2><IconBell size={13} /> Latest notifications</h2>
          {notes.map((n) => (
            <div className="feed-item" key={n.id} style={{ cursor: "default" }}>
              <div>
                <b>{n.title}</b>
                {n.body && <span className="muted"> — {n.body}</span>}
                <RelTime ts={n.ts} className="muted clock" style={{ display: "block", fontSize: "0.75rem" }} />
              </div>
            </div>
          ))}
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
}: {
  icon: React.ReactNode;
  label: string;
  ev?: CamEvent;
  tone?: "warn";
}) {
  return (
    <div className="lastseen-item">
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
      {ev?.snapshot && <img className="lastseen-thumb" src={`/api/snapshots/${ev.snapshot}?w=120`} alt={label} loading="lazy" />}
    </div>
  );
}
