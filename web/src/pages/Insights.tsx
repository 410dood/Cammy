import { useEffect, useState } from "react";
import { api, Timeseries } from "../api";
import { ErrorState } from "../ui";
import { prettyLabel } from "../labels";

const RANGES = [
  { label: "7 days", days: 7 },
  { label: "14 days", days: 14 },
  { label: "30 days", days: 30 },
];

/** A labeled hour of day, e.g. 0 -> "12a", 13 -> "1p". */
function hourLabel(h: number): string {
  const ampm = h < 12 ? "a" : "p";
  const h12 = h % 12 === 0 ? 12 : h % 12;
  return `${h12}${ampm}`;
}

function Stat({ label, value, sub }: { label: string; value: React.ReactNode; sub?: string }) {
  return (
    <div className="stat-card">
      <div className="stat-body">
        <div className="stat-value tnum">{value}</div>
        <div className="stat-label">{label}</div>
        {sub && <div className="stat-sub muted">{sub}</div>}
      </div>
    </div>
  );
}

export default function Insights({ onError }: { onError?: (e: string) => void }) {
  const [days, setDays] = useState(7);
  const [data, setData] = useState<Timeseries | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  // Tapped/focused bar readout — the chart's touch + keyboard + SR alternative
  // to the hover-only tooltip (which never appears on a phone).
  const [selDay, setSelDay] = useState<string | null>(null);
  const [selHour, setSelHour] = useState<string | null>(null);

  const load = () => {
    setLoaded(false);
    api
      .analyticsTimeseries(days)
      .then((d) => {
        setData(d);
        setErr(null);
      })
      .catch((e) => {
        setErr(String(e));
        onError?.(String(e));
      })
      .finally(() => setLoaded(true));
  };
  useEffect(() => {
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [days]);

  const maxDay = data ? Math.max(1, ...data.days.map((d) => d.count)) : 1;
  const maxHour = data ? Math.max(1, ...data.by_hour) : 1;
  const maxLabel = data && data.by_label.length ? data.by_label[0][1] : 1;
  const busiestDay = data ? data.days.reduce((a, b) => (b.count > a.count ? b : a), data.days[0]) : null;
  const busiestHour = data ? data.by_hour.indexOf(Math.max(...data.by_hour)) : 0;
  const avgPerDay = data ? Math.round(data.total / Math.max(1, data.range_days)) : 0;
  // Keep the day axis readable at 30 days: label roughly 8 ticks.
  const stride = data ? Math.max(1, Math.ceil(data.days.length / 8)) : 1;

  return (
    <>
      <h1>Insights</h1>
      <p className="muted" style={{ marginTop: 0 }}>
        Detection trends across your cameras over the last {days} days.
      </p>

      <div className="row" style={{ gap: 6, marginBottom: 18 }}>
        {RANGES.map((r) => (
          <button
            key={r.days}
            type="button"
            className={`btn ${days === r.days ? "btn-primary" : "btn-ghost"}`}
            aria-pressed={days === r.days}
            onClick={() => setDays(r.days)}
          >
            {r.label}
          </button>
        ))}
      </div>

      {err ? (
        <ErrorState what="insights" message={err} onRetry={load} />
      ) : !loaded && !data ? (
        <div className="card" aria-busy="true" aria-label="Loading insights">
          {[0, 1, 2].map((i) => (
            <div key={i} className="skeleton" style={{ height: 120, margin: "12px 0" }} />
          ))}
        </div>
      ) : data && data.total === 0 ? (
        <div className="card">
          <p className="muted" style={{ margin: 0 }}>
            No detections in the last {days} days. Trends appear here as your cameras record events.
          </p>
        </div>
      ) : data ? (
        <>
          <div className="stat-grid" style={{ marginBottom: 18 }}>
            <Stat label="Total detections" value={data.total.toLocaleString()} sub={`over ${data.range_days} days`} />
            <Stat label="Average / day" value={avgPerDay.toLocaleString()} />
            <Stat
              label="Busiest day"
              value={busiestDay ? busiestDay.day : "—"}
              sub={busiestDay ? `${busiestDay.count.toLocaleString()} detections` : undefined}
            />
            <Stat label="Peak hour" value={hourLabel(busiestHour)} sub={`${data.by_hour[busiestHour].toLocaleString()} detections`} />
          </div>

          <div className="card">
            <h2>Detections per day</h2>
            <div style={{ display: "flex", alignItems: "flex-end", gap: 3, height: 170, marginTop: 8 }}>
              {data.days.map((d) => {
                const readout = `${d.day}: ${d.count.toLocaleString()} detections`;
                return (
                  <button
                    key={d.ts}
                    type="button"
                    className="chart-bar"
                    title={readout}
                    aria-label={readout}
                    aria-pressed={selDay === readout}
                    onClick={() => setSelDay(readout)}
                    style={{ flex: 1, display: "flex", flexDirection: "column", justifyContent: "flex-end", height: "100%" }}
                  >
                    <div
                      style={{
                        height: `${(d.count / maxDay) * 100}%`,
                        minHeight: d.count > 0 ? 3 : 0,
                        background: "var(--accent)",
                        borderRadius: "3px 3px 0 0",
                      }}
                    />
                  </button>
                );
              })}
            </div>
            <div className="muted" style={{ fontSize: "var(--text-xs)", marginTop: 6, minHeight: "1.2em" }} aria-live="polite">
              {selDay ?? "Tap a bar for its count."}
            </div>
            <div style={{ display: "flex", gap: 3, marginTop: 6 }}>
              {data.days.map((d, i) => (
                <div
                  key={d.ts}
                  style={{ flex: 1, textAlign: "center", fontSize: "var(--text-xs)", color: "var(--text-subtle)" }}
                >
                  {i % stride === 0 ? d.day : ""}
                </div>
              ))}
            </div>
          </div>

          <div className="home-cols">
            <div className="card">
              <h2>Top objects</h2>
              {data.by_label.length === 0 ? (
                <p className="muted" style={{ margin: 0 }}>No objects yet.</p>
              ) : (
                data.by_label.map(([label, n]) => (
                  <div key={label} style={{ display: "flex", alignItems: "center", gap: 10, margin: "7px 0" }}>
                    <div style={{ width: 96, textAlign: "right", textTransform: "capitalize", fontSize: "var(--text-sm)" }}>
                      {prettyLabel(label)}
                    </div>
                    <div style={{ flex: 1, height: 18, background: "var(--surface-hover)", borderRadius: "var(--radius-xs)", overflow: "hidden" }}>
                      <div
                        title={`${n.toLocaleString()} detections`}
                        style={{ width: `${(n / maxLabel) * 100}%`, height: "100%", background: "var(--accent)", borderRadius: "var(--radius-xs)" }}
                      />
                    </div>
                    <div className="tnum muted" style={{ width: 60, textAlign: "right", fontSize: "var(--text-sm)" }}>
                      {n.toLocaleString()}
                    </div>
                  </div>
                ))
              )}
            </div>

            <div className="card">
              <h2>Activity by hour of day</h2>
              <div style={{ display: "flex", alignItems: "flex-end", gap: 2, height: 150, marginTop: 8 }}>
                {data.by_hour.map((c, h) => {
                  const readout = `${hourLabel(h)}: ${c.toLocaleString()} detections`;
                  return (
                    <button
                      key={h}
                      type="button"
                      className="chart-bar"
                      title={readout}
                      aria-label={readout}
                      aria-pressed={selHour === readout}
                      onClick={() => setSelHour(readout)}
                      style={{ flex: 1, display: "flex", flexDirection: "column", justifyContent: "flex-end", height: "100%" }}
                    >
                      <div
                        style={{
                          height: `${(c / maxHour) * 100}%`,
                          minHeight: c > 0 ? 2 : 0,
                          background: h === busiestHour ? "var(--accent)" : "var(--accent-muted, var(--accent))",
                          opacity: h === busiestHour ? 1 : 0.55,
                          borderRadius: "2px 2px 0 0",
                        }}
                      />
                    </button>
                  );
                })}
              </div>
              <div className="muted" style={{ fontSize: "var(--text-xs)", marginTop: 6, minHeight: "1.2em" }} aria-live="polite">
                {selHour ?? "Tap a bar for its count."}
              </div>
              <div style={{ display: "flex", gap: 2, marginTop: 6 }}>
                {data.by_hour.map((_, h) => (
                  <div
                    key={h}
                    style={{ flex: 1, textAlign: "center", fontSize: "var(--text-xs)", color: "var(--text-subtle)" }}
                  >
                    {h % 6 === 0 ? hourLabel(h) : ""}
                  </div>
                ))}
              </div>
            </div>
          </div>
        </>
      ) : null}
    </>
  );
}
