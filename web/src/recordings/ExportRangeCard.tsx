import { useEffect, useRef, useState } from "react";
import { api, ExportPreview, fmtBytes } from "../api";
import { Callout, useToast } from "../ui";
import { errMsg } from "./buckets";

const EXPORT_DURATIONS = [
  { label: "1 min", secs: 60 },
  { label: "5 min", secs: 300 },
  { label: "15 min", secs: 900 },
  { label: "30 min", secs: 1800 },
  { label: "1 hour", secs: 3600 },
];

const mmss = (secs: number) => {
  const m = Math.floor(secs / 60);
  const s = Math.round(secs % 60);
  return m > 0 ? `${m} min${s ? ` ${s}s` : ""}` : `${s}s`;
};

/** `<input type="datetime-local">` speaks local wall-clock without a zone, so
 *  convert through the local offset rather than Date.toISOString() (which is
 *  UTC and would silently shift the export by the timezone offset). */
export const tsToLocalInput = (ts: number) => {
  const d = new Date(ts * 1000);
  d.setMinutes(d.getMinutes() - d.getTimezoneOffset());
  return d.toISOString().slice(0, 16);
};
export const localInputToTs = (v: string) => Math.floor(new Date(v).getTime() / 1000);

/** Export any span of recorded footage as one file.
 *
 *  The per-event clip is deliberately capped to the single segment containing
 *  the event, so a six-minute incident used to be six unlabelled downloads to
 *  stitch by hand. This spans segment boundaries, and previews coverage first
 *  because recording gaps (a restart, a schedule, retention) are normal and a
 *  download that quietly returns less than you asked for is worse than one that
 *  warns you. */
export default function ExportRangeCard({
  cameraId,
  defaultFrom,
}: {
  cameraId: number;
  defaultFrom: number;
}) {
  const toast = useToast();
  const [start, setStart] = useState(() => tsToLocalInput(defaultFrom));
  const [secs, setSecs] = useState(300);
  const [preview, setPreview] = useState<ExportPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const from = localInputToTs(start);
  const valid = Number.isFinite(from);
  const to = from + secs;

  // Preview follows the inputs; a stale token drops out-of-order responses.
  const token = useRef(0);
  useEffect(() => {
    if (!valid) {
      setPreview(null);
      setError("Pick a start time");
      return;
    }
    const mine = ++token.current;
    setError(null);
    api
      .exportPreview({ camera_id: cameraId, from, to })
      .then((p) => {
        if (token.current === mine) setPreview(p);
      })
      .catch((e) => {
        if (token.current === mine) {
          setPreview(null);
          setError(errMsg(e));
        }
      });
  }, [cameraId, from, to, valid]);

  const download = () => {
    if (!valid || busy) return;
    setBusy(true);
    // The server may take a few seconds to concat + trim; the browser shows its
    // own download progress, so just hand off and re-enable.
    window.location.href = api.exportUrl({ camera_id: cameraId, from, to });
    toast.success("Building the export — your download will start shortly");
    window.setTimeout(() => setBusy(false), 4000);
  };

  const missing = preview ? preview.requested_secs - preview.covered_secs : 0;
  return (
    <div className="card" style={{ marginTop: 10 }}>
      <h2 style={{ marginTop: 0 }}>Export a range</h2>
      <p className="muted" style={{ marginTop: 0 }}>
        Save any stretch of this camera&apos;s footage as a single video file — it can span as
        many clips as it needs to.
      </p>
      <div className="row" style={{ alignItems: "flex-end", gap: 10, flexWrap: "wrap" }}>
        <label className="field">
          starts at
          <input
            type="datetime-local"
            value={start}
            onChange={(e) => setStart(e.target.value)}
            aria-label="Export start time"
          />
        </label>
        <label className="field">
          length
          <select
            value={secs}
            onChange={(e) => setSecs(Number(e.target.value))}
            aria-label="Export length"
          >
            {EXPORT_DURATIONS.map((d) => (
              <option key={d.secs} value={d.secs}>
                {d.label}
              </option>
            ))}
          </select>
        </label>
        <button type="button" onClick={download} disabled={!valid || busy || !preview?.segments}>
          {busy ? "Preparing…" : "Download"}
        </button>
      </div>
      {error ? (
        <Callout tone="warn" role="status">{error}</Callout>
      ) : preview && preview.segments === 0 ? (
        <Callout tone="warn" role="status">
          No footage saved for that time. It may have passed your recording history limit, or the
          camera wasn&apos;t recording then.
        </Callout>
      ) : preview ? (
        <p className="muted" style={{ marginBottom: 0 }}>
          {preview.segments} clip{preview.segments === 1 ? "" : "s"} · about{" "}
          {fmtBytes(preview.approx_bytes)}
          {missing > 0 && (
            <>
              {" · "}
              <b>
                {preview.gaps.length} gap{preview.gaps.length === 1 ? "" : "s"} ({mmss(missing)}{" "}
                missing)
              </b>{" "}
              — the file will be shorter than {mmss(preview.requested_secs)}
            </>
          )}
        </p>
      ) : null}
    </div>
  );
}
