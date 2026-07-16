import { useEffect, useRef, useState } from "react";
import { api, CamEvent, fmtTime, Lifecycle } from "./api";
import { Modal } from "./ui";
import { prettyLabel, prettyZone } from "./labels";
import { IconRadar, IconZone, IconArrowRight, IconArrowLeft } from "./icons";

/** Human-readable crossing direction ("a_to_b" → "A → B"). */
function dirLabel(d: string) {
  if (d === "a_to_b") return "A → B";
  if (d === "b_to_a") return "B → A";
  return d.replace(/_/g, " ");
}

/** A tiny normalized-space sketch of the object's trajectory (P2.16). Points are
 *  frame fractions (0..1); we draw them into a fixed box so you can see the path
 *  shape at a glance. Start = hollow dot, end = filled accent dot. */
function PathSketch({ path }: { path: [number, number, number][] }) {
  const W = 240;
  const H = 135; // 16:9-ish sketch box
  const pts = path.map(([, x, y]) => `${(x * W).toFixed(1)},${(y * H).toFixed(1)}`).join(" ");
  const [, sx, sy] = path[0];
  const [, ex, ey] = path[path.length - 1];
  return (
    <svg
      width={W}
      height={H}
      viewBox={`0 0 ${W} ${H}`}
      role="img"
      aria-label="Object path"
      style={{
        background: "var(--bg-sunken)",
        border: "1px solid var(--border)",
        borderRadius: "var(--radius-sm)",
        flex: "0 0 auto",
      }}
    >
      <polyline points={pts} fill="none" stroke="var(--accent)" strokeWidth="2" strokeLinejoin="round" strokeLinecap="round" opacity="0.9" />
      <circle cx={sx * W} cy={sy * H} r="4" fill="none" stroke="var(--text-muted)" strokeWidth="2" />
      <circle cx={ex * W} cy={ey * H} r="5" fill="var(--accent)" />
    </svg>
  );
}

/**
 * Object-lifecycle ("Track") view (P2.16): the ordered story of the physical
 * object behind a tracker-driven event — entered a zone, loitered, crossed a
 * line. Clicking a step seeks the event viewer to that moment (reusing the
 * parent's open-event / covering-recording resolution — no new video code here).
 *
 * `seed` is the event whose Track button was clicked; `onOpenEvent` swaps the
 * viewer to a chosen step; `onClose` dismisses the modal. Only tracker-driven
 * events have a story — for anything else the endpoint says so, honestly.
 */
export default function LifecycleModal({
  seed,
  onOpenEvent,
  onClose,
}: {
  seed: CamEvent;
  onOpenEvent: (ev: CamEvent) => void;
  onClose: () => void;
}) {
  const [data, setData] = useState<Lifecycle | null>(null);
  const [failed, setFailed] = useState(false);
  const reqRef = useRef(0);

  useEffect(() => {
    const token = ++reqRef.current;
    setData(null);
    setFailed(false);
    api.eventLifecycle(seed.id).then(
      (d) => {
        if (token === reqRef.current) setData(d);
      },
      () => {
        if (token === reqRef.current) setFailed(true);
      },
    );
    return () => {
      // Ignore any in-flight response once this modal instance is replaced/closed.
      reqRef.current++;
    };
  }, [seed.id]);

  const steps = data?.steps ?? [];
  const path = data?.path ?? [];

  return (
    <Modal title={`Track story · ${seed.camera}`} onClose={onClose}>
      <div style={{ minWidth: "min(420px, 100%)", maxWidth: 560 }}>
        {failed ? (
          <p className="muted">Couldn't load this object's track history. Please try again.</p>
        ) : !data ? (
          <p className="muted">Tracing this object's path…</p>
        ) : !data.available ? (
          <p className="muted">
            No track history for this event. Only tracker-driven events — line crossings,
            loitering, zone entries, and family-safety hints (child, fall, water) — carry a story
            of one object across the scene.
          </p>
        ) : (
          <>
            <div className="row" style={{ alignItems: "flex-start", gap: 14, flexWrap: "wrap", marginBottom: 10 }}>
              {path.length >= 2 && <PathSketch path={path} />}
              <p className="muted" style={{ flex: "1 1 200px", margin: 0, fontSize: "var(--text-sm)" }}>
                <IconRadar size={14} /> The same object, followed across {steps.length}{" "}
                {steps.length === 1 ? "moment" : "moments"}. Click a step to jump the viewer there.
              </p>
            </div>
            <ol className="lc-steps">
              {steps.map((s, i) => {
                const isSeed = s.id === seed.id;
                return (
                  <li key={s.id}>
                    <button
                      className={`lc-step${isSeed ? " lc-step-current" : ""}`}
                      onClick={() => onOpenEvent(s)}
                      aria-current={isSeed ? "true" : undefined}
                    >
                      <span className="lc-step-num" aria-hidden="true">{i + 1}</span>
                      <span className="lc-step-body">
                        <span className="lc-step-head">
                          <span className="lc-step-label">{prettyLabel(s.label)}</span>
                          {s.zone && (
                            <span className="badge accent" title={`Zone: ${s.zone}`}>
                              <IconZone size={12} /> {prettyZone(s.zone)}
                            </span>
                          )}
                          {s.direction && (
                            <span className="badge" title="Crossing direction">
                              {s.direction === "b_to_a" ? <IconArrowLeft size={12} /> : <IconArrowRight size={12} />}{" "}
                              {dirLabel(s.direction)}
                            </span>
                          )}
                          {s.speed != null && (
                            <span className="badge" title="Estimated ground speed">
                              {Math.round(s.speed)} km/h
                            </span>
                          )}
                          {isSeed && <span className="badge accent">this event</span>}
                        </span>
                        <span className="muted lc-step-time">{fmtTime(s.ts)}</span>
                      </span>
                    </button>
                  </li>
                );
              })}
            </ol>
          </>
        )}
      </div>
    </Modal>
  );
}
