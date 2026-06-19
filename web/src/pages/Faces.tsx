import { useEffect, useState } from "react";
import { api, CamEvent, fmtTime } from "../api";
import { useToast, useDialog, Modal } from "../ui";
import { IconUser, IconStranger, IconCar, IconAlert, IconCheck } from "../icons";

interface Enrolled {
  id: number;
  name: string;
  created_ts: number;
}

interface IdentityStat {
  name: string;
  enrolled?: Enrolled;
  count: number;
  last: CamEvent;
  cameras: Record<string, number>;
  sightings: CamEvent[];
}

interface PlateStat {
  plate: string;
  count: number;
  last: CamEvent;
  cameras: Record<string, number>;
  cls: "deny" | "allow" | "";
}

function topCamera(cameras: Record<string, number>): string {
  return Object.entries(cameras).sort((a, b) => b[1] - a[1])[0]?.[0] ?? "";
}

export default function Faces({ onError }: { onError: (e: string) => void }) {
  const toast = useToast();
  const dialog = useDialog();
  const [enrolled, setEnrolled] = useState<Enrolled[]>([]);
  const [unknown, setUnknown] = useState<string[]>([]);
  const [names, setNames] = useState<Record<string, string>>({});
  const [events, setEvents] = useState<CamEvent[]>([]);
  const [plateDeny, setPlateDeny] = useState<string[]>([]);
  const [plateAllow, setPlateAllow] = useState<string[]>([]);
  const [openId, setOpenId] = useState<IdentityStat | null>(null);

  const load = () => {
    api.faces().then((r) => {
      setEnrolled(r.enrolled);
      setUnknown(r.unknown);
    }).catch(() => {});
    api.events({ limit: 3000 }).then(setEvents).catch(() => {});
  };

  useEffect(() => {
    load();
    api.settings().then((s) => {
      setPlateDeny(s.plate_denylist ?? []);
      setPlateAllow(s.plate_allowlist ?? []);
    }).catch(() => {});
    const t = setInterval(load, 15000);
    return () => clearInterval(t);
  }, []);

  const enroll = async (file: string) => {
    const name = (names[file] || "").trim();
    if (!name) return;
    try {
      await api.enrollFace(name, file);
      setNames((n) => ({ ...n, [file]: "" }));
      toast.success(`Enrolled ${name}`);
      load();
    } catch (e) {
      onError(String(e));
    }
  };

  const rename = async (f: Enrolled) => {
    const next = await dialog.prompt({
      title: "Rename person",
      label: `New name for "${f.name}"`,
      defaultValue: f.name,
      maxLength: 64,
    });
    if (!next || !next.trim() || next.trim() === f.name) return;
    try {
      await api.renameFace(f.id, next.trim());
      toast.success("Renamed");
      load();
    } catch (e) {
      onError(String(e));
    }
  };

  const forget = async (f: Enrolled) => {
    const ok = await dialog.confirm({
      title: `Forget "${f.name}"?`,
      body: "Their past events keep the name, but new detections won't match them.",
      confirmLabel: "Forget",
      danger: true,
    });
    if (!ok) return;
    try {
      await api.deleteFace(f.id);
      toast.success(`Forgot ${f.name}`);
      load();
    } catch (e) {
      onError(String(e));
    }
  };

  // --- A5 aggregation: people + vehicles seen, from the event history ---------
  const idMap: Record<string, IdentityStat> = {};
  let strangerCount = 0;
  let lastStranger: CamEvent | undefined;
  const plateMap: Record<string, PlateStat> = {};

  const plateClass = (plate: string): "deny" | "allow" | "" => {
    const p = plate.toUpperCase();
    if (plateDeny.some((e) => e.trim() && p.includes(e.trim().toUpperCase()))) return "deny";
    if (plateAllow.some((e) => e.trim() && p.includes(e.trim().toUpperCase()))) return "allow";
    return "";
  };

  for (const e of events) {
    if (e.face === "?") {
      strangerCount++;
      if (!lastStranger) lastStranger = e;
    } else if (e.face) {
      const s =
        idMap[e.face] ??
        (idMap[e.face] = { name: e.face, count: 0, last: e, cameras: {}, sightings: [] });
      s.count++;
      s.cameras[e.camera] = (s.cameras[e.camera] ?? 0) + 1;
      if (s.sightings.length < 24) s.sightings.push(e);
    }
    if (e.plate) {
      const s =
        plateMap[e.plate] ??
        (plateMap[e.plate] = { plate: e.plate, count: 0, last: e, cameras: {}, cls: plateClass(e.plate) });
      s.count++;
      s.cameras[e.camera] = (s.cameras[e.camera] ?? 0) + 1;
    }
  }
  // Enrolled people with zero recent sightings still get a card.
  for (const f of enrolled) {
    if (idMap[f.name]) idMap[f.name].enrolled = f;
    else idMap[f.name] = { name: f.name, enrolled: f, count: 0, last: undefined as unknown as CamEvent, cameras: {}, sightings: [] };
  }
  const identities = Object.values(idMap).sort((a, b) => b.count - a.count);
  const plates = Object.values(plateMap).sort((a, b) => b.count - a.count);

  return (
    <>
      <h1>People &amp; Vehicles</h1>

      <div className="card">
        <h2>People</h2>
        {identities.length === 0 ? (
          <p className="muted">
            Nobody enrolled or seen yet. Name a face from the unknown gallery below — detections of
            that person will then carry their name.
          </p>
        ) : (
          <div className="identity-grid">
            {strangerCount > 0 && lastStranger && (
              <div className="identity-card" style={{ cursor: "default" }}>
                <span className="identity-thumb warn"><IconStranger size={22} /></span>
                <div className="identity-body">
                  <b>Strangers</b>
                  <div className="muted">
                    {strangerCount} sighting{strangerCount === 1 ? "" : "s"} · last{" "}
                    <span className="clock">{fmtTime(lastStranger.ts)}</span>
                  </div>
                </div>
              </div>
            )}
            {identities.map((s) => (
              <button key={s.name} className="identity-card" onClick={() => s.count > 0 && setOpenId(s)}>
                {s.last?.snapshot ? (
                  <img className="identity-thumb" src={`/api/snapshots/${s.last.snapshot}?w=120`} alt={s.name} loading="lazy" />
                ) : (
                  <span className="identity-thumb"><IconUser size={22} /></span>
                )}
                <div className="identity-body">
                  <div className="identity-head">
                    <b>{s.name}</b>
                    {!s.enrolled && <span className="badge" title="Seen in past events but no longer enrolled">past</span>}
                  </div>
                  <div className="muted">
                    {s.count === 0
                      ? "no recent sightings"
                      : `${s.count} sighting${s.count === 1 ? "" : "s"} · ${topCamera(s.cameras)}`}
                  </div>
                  {s.count > 0 && (
                    <div className="muted clock" style={{ fontSize: "0.72rem" }}>last {fmtTime(s.last.ts)}</div>
                  )}
                </div>
                {s.enrolled && (
                  <div className="identity-actions">
                    <button
                      className="btn btn-ghost ev-act"
                      onClick={(e) => { e.stopPropagation(); rename(s.enrolled!); }}
                    >
                      Rename
                    </button>
                    <button
                      className="btn btn-danger ev-act"
                      onClick={(e) => { e.stopPropagation(); forget(s.enrolled!); }}
                    >
                      Forget
                    </button>
                  </div>
                )}
              </button>
            ))}
          </div>
        )}
      </div>

      {plates.length > 0 && (
        <div className="card">
          <h2>Vehicles seen</h2>
          <div className="identity-grid">
            {plates.map((s) => (
              <div key={s.plate} className="identity-card" style={{ cursor: "default" }}>
                <span className={`identity-thumb ${s.cls === "deny" ? "danger" : s.cls === "allow" ? "ok" : ""}`}>
                  <IconCar size={20} />
                </span>
                <div className="identity-body">
                  <div className="identity-head">
                    <b style={{ letterSpacing: "0.04em" }}>{s.plate}</b>
                    {s.cls === "deny" && <span className="badge danger"><IconAlert size={11} /> of interest</span>}
                    {s.cls === "allow" && <span className="badge ok"><IconCheck size={11} /> known</span>}
                  </div>
                  <div className="muted">
                    {s.count} sighting{s.count === 1 ? "" : "s"} · {topCamera(s.cameras)}
                  </div>
                  <div className="muted clock" style={{ fontSize: "0.72rem" }}>last {fmtTime(s.last.ts)}</div>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      <div className="card">
        <h2>Unknown faces</h2>
        <p className="muted" style={{ marginTop: 0 }}>
          Confident face detections that didn't match anyone. Name one to enroll that person
          (a clear, frontal crop works best).
        </p>
        {unknown.length === 0 ? (
          <p className="muted">None waiting.</p>
        ) : (
          <div className="event-grid">
            {unknown.map((file) => (
              <div className="event-card" key={file} style={{ cursor: "default" }}>
                <img src={`/api/faces/unknown/${file}`} alt="unknown face" loading="lazy" />
                <div className="meta">
                  <div className="row">
                    <input
                      type="text"
                      placeholder="who is this?"
                      value={names[file] || ""}
                      onChange={(e) => setNames((n) => ({ ...n, [file]: e.target.value }))}
                      style={{ flex: 1 }}
                      onKeyDown={(e) => e.key === "Enter" && enroll(file)}
                    />
                    <button
                      className="btn btn-primary"
                      disabled={!(names[file] || "").trim()}
                      onClick={() => enroll(file)}
                    >
                      Enroll
                    </button>
                  </div>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>

      {openId && (
        <Modal title={`${openId.name} — recent sightings`} onClose={() => setOpenId(null)} className="lightbox">
          <div className="event-grid" style={{ padding: 16 }}>
            {openId.sightings.map((ev) => (
              <div className="event-card" key={ev.id} style={{ cursor: "default" }}>
                {ev.snapshot && <img src={`/api/snapshots/${ev.snapshot}?w=300`} alt={openId.name} loading="lazy" />}
                <div className="meta">
                  <span className="muted">{ev.camera}</span>
                  <div className="muted clock" style={{ fontSize: "0.75rem" }}>{fmtTime(ev.ts)}</div>
                </div>
              </div>
            ))}
          </div>
        </Modal>
      )}
    </>
  );
}
