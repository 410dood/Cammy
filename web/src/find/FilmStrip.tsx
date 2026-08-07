import { useEffect, useState } from "react";
import { momentHref } from "../moment";
import { prettyLabel } from "../labels";
import { eventClass } from "../CrossTimeline";
import { fmtSpanSecs, StripItem, tsOf } from "./strip";

const clock = (ts: number) =>
  new Date(ts * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });

/** How many items to put in the DOM at once. Nothing in this app virtualises
 *  anything, so the strip reveals in pages rather than pretending to. */
const PAGE = 120;

/** Renders a built strip.
 *
 *  Every tile is a link to `#/live/<cam>/<ts>`, so the strip navigates and
 *  never plays — the camera's own timeline is the player. Gaps and unloaded
 *  stretches are rendered, not skipped: knowing there is nothing between two
 *  detections is most of the value of scanning a day.
 */
export default function FilmStrip({ items, resetKey }: { items: StripItem[]; resetKey?: string }) {
  const [shown, setShown] = useState(PAGE);
  // Start at the top again when the USER picks a new window — never merely
  // because `items` is a new array. buildStrip returns a fresh array on every
  // refetch, and today's window refetches on the clock, so keying this on
  // `items` collapsed the reveal under the reader once a minute.
  useEffect(() => setShown(PAGE), [resetKey]);
  const visible = items.slice(0, shown);

  return (
    <div className="film">
      {visible.map((it, i) => {
        switch (it.kind) {
          case "marker":
            return (
              <h3 className="film-hour" key={`m${it.ts}-${i}`}>
                {it.label}
              </h3>
            );

          case "event": {
            const ev = it.ev;
            return (
              <a
                className="film-row film-evt"
                key={`e${ev.id}`}
                href={momentHref(ev.camera_id, ev.ts)}
                title="Open this camera's timeline at this moment"
              >
                <span className="film-thumb">
                  {ev.snapshot ? (
                    <img
                      src={`/api/snapshots/${ev.snapshot}?w=200`}
                      loading="lazy"
                      decoding="async"
                      alt={`${prettyLabel(ev.label)} on ${ev.camera}`}
                    />
                  ) : (
                    <span className="film-noimg">no image</span>
                  )}
                  {it.count > 1 && <span className="film-count">×{it.count}</span>}
                </span>
                <span className="film-meta">
                  <b>
                    <span className={`evt-swatch ${eventClass(ev.label)}`} aria-hidden="true" />
                    {prettyLabel(ev.label)}
                  </b>
                  <span className="muted">
                    {ev.camera} · {clock(it.startTs)}
                    {it.count > 1 && it.endTs !== it.startTs ? `–${clock(it.endTs)}` : ""}
                    {it.count > 1 ? ` · ${it.count} detections` : ""}
                  </span>
                  {ev.caption && <span className="muted film-cap">{ev.caption}</span>}
                </span>
                <span className="film-side muted">details →</span>
              </a>
            );
          }

          case "footage":
            return (
              <a
                className="film-row film-quiet"
                key={`f${it.segId}-${it.from}`}
                href={momentHref(it.cameraId, it.from)}
                title={`Nothing was detected here — open ${it.camera} at ${clock(it.from)}`}
              >
                <span className="film-thumb">
                  <img
                    src={`/api/recordings/${it.segId}/thumb.jpg`}
                    loading="lazy"
                    decoding="async"
                    alt={`Recorded footage from ${it.camera} at ${clock(it.from)}`}
                  />
                </span>
                <span className="film-meta">
                  <b>Quiet · {fmtSpanSecs(it.to - it.from)}</b>
                  <span className="muted">
                    {clock(it.from)}–{clock(it.to)} · recorded, nothing detected ·{" "}
                    {it.cameras} camera{it.cameras === 1 ? "" : "s"}
                  </span>
                </span>
                <span className="film-side muted">watch →</span>
              </a>
            );

          case "gap":
            return (
              <div className="film-band film-gap" key={`g${it.from}`}>
                No footage · {fmtSpanSecs(it.to - it.from)} ({clock(it.from)}–{clock(it.to)})
                <span className="muted"> — nothing was recording, or it has passed the recording-history limit</span>
              </div>
            );

          case "unknown":
            // NOT a gap. /api/recordings bounds only the top and caps at 1000
            // rows, so a day cannot be fetched in one request. Drawing this as
            // "no footage" would claim a fully-recorded day was empty.
            return (
              <div className="film-band film-unknown" key={`u${it.from}`}>
                {clock(it.from)}–{clock(it.to)} not checked · {fmtSpanSecs(it.to - it.from)}
                <span className="muted">
                  {" "}
                  — this window holds more clips than one request returns, so whether anything was
                  recorded here is unknown, not empty. Pick a narrower time to find out.
                </span>
              </div>
            );
        }
      })}

      {shown < items.length && (
        <button type="button" className="btn btn-ghost film-more" onClick={() => setShown((n) => n + PAGE)}>
          Show more — {items.length - shown} left, back to {clock(tsOf(items[items.length - 1]))}
        </button>
      )}
    </div>
  );
}
