// Find — one time-first surface where "what happened" and "what was recorded"
// stop being a routing decision.
//
// Events knows WHAT with no time axis; Recordings knows WHEN with content-blind
// rows. So every "find that clip" used to begin with a question the app should
// never ask: WHICH PAGE? Find answers both from one window, and hands every
// result to the same destination — `#/live/<cam>/<ts>`, the camera's own
// timeline, already scrubbed. That is what lets this ship without extracting
// the Events viewer, which was the riskiest act in the whole plan.
//
// Find is deliberately a NAVIGATOR, not a player and not a search engine. The
// controls are things you point at (a day, a camera, an hour, a kind of thing),
// because the owner described his own job in pointing terms. Appearance search
// is here as a refinement of the chosen window, clearly labelled as a ranker,
// never as the front door: a fuzzy miss presented as "the answer" reads as
// "Cammy didn't record it", which in a security system is a trust failure.

import { useEffect, useMemo, useRef, useState } from "react";
import { api, CamEvent, Camera, Me, Segment } from "../api";
import CrossTimeline, { ActivityStrip } from "../CrossTimeline";
import Timeline from "../Timeline";
import DayStrip, { dayStart, DayWindow, fromDateInput, toDateInput, windowForDay } from "../DayStrip";
import { goToMoment } from "../moment";
import { prettyLabel } from "../labels";
import { Callout, EmptyState, ErrorState, TogglePill, useToast } from "../ui";
import { IconFilm, IconRadar, IconSearch, IconX } from "../icons";
import { buildStrip } from "../find/strip";
import FilmStrip from "../find/FilmStrip";

const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e));
const DAY = 86400;
/** Server caps both list endpoints here. Asking for more silently gets 1000. */
const LIMIT = 1000;
/** Auto-continue paging stops here. Never hammer the DB to fill a screen. */
const MAX_PAGES = 3;
/** Detection tiles in the DOM at once. Nothing here virtualises, so reveal in
 *  pages rather than pretending to. */
const GRID_PAGE = 150;

const clock = (ts: number) =>
  new Date(ts * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });

/** Find's whole state lives in the URL: `#/find?day=…&cam=…&label=…&view=…`.
 *
 *  Without this, following a result into a moment and pressing Back dropped you
 *  on a fresh Find — measured: "Yesterday + front-door" came back as "Today +
 *  All cameras". That breaks the actual browse loop (check one, go back, check
 *  the next), which would make Find WORSE than the pages it means to replace.
 *  The plan called this non-negotiable for Events; Events never got it either.
 *
 *  Self-contained, like Settings' `#/settings/<group>`: `parseHash` already
 *  strips the query string before routing, so none of this reaches the router. */
function readHash(): {
  day: DayWindow;
  cameraId: number | null;
  label: string | null;
  view: "grid" | "list";
  zoom: { from: number; to: number } | null;
} {
  const q = new URLSearchParams(window.location.hash.split("?")[1] ?? "");
  const rawDay = q.get("day");
  const parsed = rawDay ? fromDateInput(rawDay) : null;
  const day: DayWindow =
    rawDay === "all" ? null
    : parsed != null ? windowForDay(parsed)
    : windowForDay(dayStart(Math.floor(Date.now() / 1000)));
  const cam = Number(q.get("cam"));
  const z = (q.get("z") ?? "").split("-").map(Number);
  return {
    day,
    cameraId: Number.isFinite(cam) && cam > 0 ? cam : null,
    label: q.get("label") || null,
    view: q.get("view") === "list" ? "list" : "grid",
    zoom: z.length === 2 && z.every((n) => Number.isFinite(n) && n > 0) ? { from: z[0], to: z[1] } : null,
  };
}

export default function Find({ cameras }: { cameras: Camera[] }) {
  const toast = useToast();
  const live = useMemo(() => cameras.filter((c) => c.enabled), [cameras]);

  // ── the window ────────────────────────────────────────────────────────────
  // A day, optionally narrowed to one interval by clicking the activity strip.
  const seed = useRef(readHash()).current;
  const [day, setDay] = useState<DayWindow>(seed.day);
  const [zoom, setZoom] = useState<{ from: number; to: number } | null>(seed.zoom);
  const [cameraId, setCameraId] = useState<number | null>(seed.cameraId);
  const [label, setLabel] = useState<string | null>(seed.label);
  // The URL wins when it says something; otherwise fall back to the last view
  // this browser chose, so a bare `#/find` still opens the way you left it.
  const [view, setView] = useState<"grid" | "list">(() =>
    window.location.hash.includes("view=")
      ? seed.view
      : localStorage.getItem("cammy-find-view") === "list"
        ? "list"
        : "grid"
  );
  const pickView = (v: "grid" | "list") => {
    setView(v);
    localStorage.setItem("cammy-find-view", v);
  };

  // A window must never run past now. Today's day window ends at midnight, and
  // without this the strip reported the REST OF TODAY as "No footage · 14h 50m
  // — nothing was recording", which is a claim about time that has not happened
  // yet. Ticks once a minute so today stays current.
  const [nowTick, setNowTick] = useState(() => Math.floor(Date.now() / 1000));
  useEffect(() => {
    const t = setInterval(() => setNowTick(Math.floor(Date.now() / 1000)), 60_000);
    return () => clearInterval(t);
  }, []);

  // The whole day when nothing is zoomed; "all time" falls back to the last 24h
  // so the timeline always has a real axis to draw.
  //
  // Memoised on the NUMBERS, not on `nowTick`. Keying the memo on the tick
  // handed out a fresh object every minute even when the two values were
  // identical, and everything downstream keys off this object: measured on a
  // PAST day, whose window cannot change, one tick fired two event refetches
  // and collapsed the film strip from 361 revealed items back to 121 — yanking
  // you to the top of the list every 60 seconds.
  const rawFrom = zoom ? zoom.from : day ? day.from : nowTick - DAY;
  const rawTo = Math.min(zoom ? zoom.to : day ? day.to : nowTick, nowTick);
  const win = useMemo(() => ({ from: rawFrom, to: rawTo }), [rawFrom, rawTo]);

  // What the USER chose, as a stable key. Today's window legitimately advances
  // with the clock, so `win` alone cannot distinguish "you picked a new window"
  // from "a minute passed" — and resetting your scroll position or throwing
  // away your search results because a minute passed is not something the clock
  // gets to do.
  const scopeKey = `${day?.from ?? "all"}|${zoom?.from ?? ""}-${zoom?.to ?? ""}|${cameraId ?? "all"}`;

  // Mirror the scope back into the URL. replaceState, not push: a filter change
  // is not a place you navigated TO, and pushing one entry per chip click would
  // make Back mean "undo my last click" instead of "leave this page".
  useEffect(() => {
    if (!window.location.hash.startsWith("#/find")) return; // navigated away mid-render
    const q = new URLSearchParams();
    q.set("day", day ? toDateInput(day.from) : "all");
    if (cameraId != null) q.set("cam", String(cameraId));
    if (label) q.set("label", label);
    if (view === "list") q.set("view", "list");
    if (zoom) q.set("z", `${zoom.from}-${zoom.to}`);
    const next = `#/find?${q}`;
    if (window.location.hash !== next) window.history.replaceState(null, "", next);
  }, [day, cameraId, label, view, zoom]);

  // A hash change while Find is already mounted has to re-seed the state, or
  // the URL and the view disagree. Measured: the palette's "Find — front-door,
  // today" set the URL correctly and the page went on showing Jul 10, because
  // the seed is read once at mount and nothing remounts when you are already
  // here. `replaceState` never fires hashchange, so our own writes cannot loop.
  useEffect(() => {
    const onHash = () => {
      if (!window.location.hash.startsWith("#/find")) return;
      const h = readHash();
      setDay(h.day);
      setZoom(h.zoom);
      setCameraId(h.cameraId);
      setLabel(h.label);
      // Same rule as the seed: the URL only overrides the remembered view when
      // it actually names one.
      if (window.location.hash.includes("view=")) setView(h.view);
    };
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  // Keyboard. Find is meant to become how footage gets reached, and reaching it
  // was mouse-only. Every handler bails when you are typing, and when something
  // nearer the event already claimed the key — the cross-timeline lanes are
  // role="slider" and take the arrows for scrubbing, and they preventDefault,
  // so `defaultPrevented` is what keeps the two from fighting.
  const searchRef = useRef<HTMLInputElement | null>(null);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target as HTMLElement | null;
      const tag = t?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || t?.isContentEditable) return;
      const stepDay = (days: number) => {
        const today = dayStart(Math.floor(Date.now() / 1000));
        const base = day ? dayStart(day.from) : today;
        const next = Math.min(dayStart(base + days * DAY), today);
        setDay(windowForDay(next));
        setZoom(null);
      };
      if (e.key === "ArrowLeft") { e.preventDefault(); stepDay(-1); }
      else if (e.key === "ArrowRight") { e.preventDefault(); stepDay(1); }
      else if (e.key === "/") { e.preventDefault(); searchRef.current?.focus(); }
      else if (e.key === "g") { e.preventDefault(); pickView("grid"); }
      else if (e.key === "l") { e.preventDefault(); pickView("list"); }
      else if (e.key === "Escape" && zoom) { e.preventDefault(); setZoom(null); }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [day, zoom]);

  // ── data ──────────────────────────────────────────────────────────────────
  const [events, setEvents] = useState<CamEvent[]>([]);
  const [segments, setSegments] = useState<Segment[]>([]);
  const [segmentSecs, setSegmentSecs] = useState(60);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  /** Did the event fetch stop because it ran out of pages rather than events? */
  const [eventsCapped, setEventsCapped] = useState(false);
  /** Oldest instant the SEGMENT fetch actually reached — everything older is
   *  unknown, not empty. See buildStrip's `coverageKnownFrom`. */
  const [coverageKnownFrom, setCoverageKnownFrom] = useState<number | null>(null);
  const [me, setMe] = useState<Me | null>(null);

  useEffect(() => {
    api.settings().then((s) => setSegmentSecs(s.segment_seconds)).catch(() => {});
    api.me().then(setMe).catch(() => {});
  }, []);

  const token = useRef(0);
  // Which scope the on-screen data belongs to. Today's window advances with the
  // clock, so this effect re-runs once a minute on the SAME scope — and showing
  // the loading skeleton for that unmounted the film strip, taking its reveal
  // state with it (measured: 146 items revealed collapsed to 121 mid-read).
  // A background refresh swaps the data underneath; only a scope you actually
  // chose is allowed to blank the page.
  const loadedScope = useRef<string | null>(null);
  useEffect(() => {
    const mine = ++token.current;
    const freshScope = loadedScope.current !== scopeKey;
    loadedScope.current = scopeKey;
    if (freshScope) setLoading(true);
    setError(null);
    const scope = cameraId == null ? {} : { camera_id: cameraId };

    // Events: auto-continue up to MAX_PAGES, cursor `oldest + 1` with id
    // de-dupe — the filter is a strict `<` on whole seconds, so several events
    // can share the boundary second and a naive cursor would skip them.
    const loadEvents = async () => {
      const seen = new Set<number>();
      const all: CamEvent[] = [];
      let before = win.to;
      let capped = false;
      for (let page = 0; page < MAX_PAGES; page++) {
        const batch = await api.events({ ...scope, after: win.from, before, limit: LIMIT });
        for (const e of batch) {
          if (!seen.has(e.id)) {
            seen.add(e.id);
            all.push(e);
          }
        }
        if (batch.length < LIMIT) break;
        const oldest = Math.min(...batch.map((e) => e.ts));
        if (oldest <= win.from) break;
        before = oldest + 1;
        if (page === MAX_PAGES - 1) capped = true;
      }
      return { all, capped };
    };

    Promise.all([
      loadEvents(),
      // Recordings bounds only the top, so clamp to the window ourselves — the
      // same bug Phase 1 hit on this endpoint (a pruned day showed a DIFFERENT
      // day's footage under the right header).
      api.recordings({ ...scope, before: win.to, limit: LIMIT }),
    ])
      .then(([ev, segsRaw]) => {
        if (token.current !== mine) return;
        const segs = segsRaw.filter((s) => s.start_ts >= win.from && s.start_ts < win.to);
        setEvents(ev.all);
        setEventsCapped(ev.capped);
        setSegments(segs);
        // If the raw page came back full we were truncated by the cap, and the
        // oldest row we got is as far back as we actually looked.
        setCoverageKnownFrom(
          segsRaw.length >= LIMIT ? Math.min(...segsRaw.map((s) => s.start_ts)) : win.from
        );
        setLoading(false);
      })
      .catch((e) => {
        if (token.current !== mine) return;
        setError(errMsg(e));
        setLoading(false);
      });
    // scopeKey is derived from these same inputs; it is read through a ref so
    // it cannot itself trigger a fetch.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [win, cameraId]);

  // ── appearance search (a refinement, never the entry) ─────────────────────
  const [q, setQ] = useState("");
  const [ranked, setRanked] = useState<{ q: string; events: CamEvent[]; scanned: number } | null>(null);
  const [searching, setSearching] = useState(false);
  const runSearch = async () => {
    const term = q.trim();
    if (!term) {
      setRanked(null);
      return;
    }
    setSearching(true);
    try {
      const r = await api.search(term, 48, {
        camera_id: cameraId ?? undefined,
        label: label ?? undefined,
        after: win.from,
        before: win.to,
      });
      setRanked({ q: term, events: r.results.map((x) => x.event), scanned: events.length });
    } catch (e) {
      toast.error(`Search failed: ${errMsg(e)}`);
    } finally {
      setSearching(false);
    }
  };
  // A ranking of a window you have LEFT is a ranking of nothing — but a
  // ranking of the window you are still looking at survives the clock.
  useEffect(() => setRanked(null), [scopeKey]);

  // ── derived ───────────────────────────────────────────────────────────────
  const labelCounts = useMemo(() => {
    const t = new Map<string, number>();
    for (const e of events) t.set(e.label, (t.get(e.label) ?? 0) + 1);
    return [...t.entries()].sort((a, b) => b[1] - a[1]);
  }, [events]);

  const shownEvents = useMemo(() => {
    const base = ranked ? ranked.events : events;
    return label ? base.filter((e) => e.label === label) : base;
  }, [events, ranked, label]);

  // Reveal the grid in pages. A busy day here is 1026 detections, and putting
  // all of them in the DOM made Find heavier than the pages it is meant to
  // replace — the exact way this plan said the new surface could lose. Events
  // holds 200 cards; this holds fewer until you ask for more.
  const [gridShown, setGridShown] = useState(GRID_PAGE);
  useEffect(() => setGridShown(GRID_PAGE), [scopeKey, label, ranked]);
  const gridEvents = shownEvents.slice(0, gridShown);

  const strip = useMemo(
    () =>
      buildStrip(shownEvents, segments, {
        from: win.from,
        to: win.to,
        segmentSecs,
        coverageKnownFrom: coverageKnownFrom ?? win.from,
      }),
    [shownEvents, segments, win, segmentSecs, coverageKnownFrom]
  );

  // ── honesty ───────────────────────────────────────────────────────────────
  // list_events applies LIMIT *before* the RBAC retain and the tag filter
  // (api.rs, self-documented), so a short list is not proof of a complete one.
  // A named non-admin is the only principal that can be camera-scoped; it is a
  // superset (they may have no scoping at all), and over-warning is the right
  // side to be wrong on for a claim about whether you have seen everything.
  const maybeScoped = !!me?.named && me.role !== "admin";
  const underFill = eventsCapped
    ? `Showing the newest ${events.length.toLocaleString()} detections in this window — there are more. Narrow the time range to see them.`
    : maybeScoped
      ? `Showing ${events.length.toLocaleString()} detections. The server applies its ${LIMIT.toLocaleString()}-row limit before filtering to the cameras you can see, so this may not be all of them — narrow the time range to be sure.`
      : null;

  const windowLabel = zoom
    ? `${clock(zoom.from)}–${clock(zoom.to)}`
    : day
      ? new Date(day.from * 1000).toLocaleDateString(undefined, { weekday: "long", month: "long", day: "numeric" })
      : "the last 24 hours";

  const lanes = cameraId == null ? live : live.filter((c) => c.id === cameraId);

  return (
    <>
      <h1>Find</h1>
      <p className="muted" style={{ marginTop: -8 }}>
        Pick when and where, then scan what happened and what was recorded together. Anything you
        click opens that camera&apos;s timeline at that moment.{" "}
        <span className="muted kbd-hint">
          Arrow keys step days, <kbd>/</kbd> searches, <kbd>G</kbd>/<kbd>L</kbd> switch view.
        </span>
      </p>

      {/* ── band 1: when and where ── */}
      <div className="row find-controls">
        <DayStrip value={day} onChange={(w) => { setDay(w); setZoom(null); }} />
        <span className="chip-sep" aria-hidden="true" />
        <TogglePill on={cameraId == null} onClick={() => setCameraId(null)}>
          All cameras
        </TogglePill>
        {live.map((c) => (
          <TogglePill key={c.id} on={cameraId === c.id} onClick={() => setCameraId(cameraId === c.id ? null : c.id)}>
            {c.name}
          </TogglePill>
        ))}
      </div>

      {zoom && (
        <div className="row" style={{ marginTop: -4, marginBottom: 8 }}>
          <span className="badge">
            {clock(zoom.from)}–{clock(zoom.to)}
          </span>
          <button type="button" className="btn btn-ghost ev-act" onClick={() => setZoom(null)}>
            <IconX size={13} /> Back to the whole day
          </button>
        </div>
      )}

      {error ? (
        <ErrorState what="this window" message={error} onRetry={() => setDay((d) => (d ? { ...d } : d))} />
      ) : (
        <>
          {/* Exactly ONE activity row. CrossTimeline already draws an Activity
              lane aligned with the camera lanes, so rendering a standalone
              strip beside it put the same histogram on screen twice, at two
              different scales, with only the misaligned copy clickable. When a
              single camera is selected there is no CrossTimeline, so the
              standalone strip is the only one and carries the interaction. */}
          {cameraId != null && (
            <ActivityStrip
              events={events}
              windowSecs={win.to - win.from}
              nowTs={win.to}
              onPick={(from, to) => setZoom({ from, to })}
            />
          )}

          {/* ── band 2: the index ── */}
          {lanes.length > 0 &&
            (cameraId == null ? (
              <CrossTimeline
                cameras={lanes}
                segments={segments}
                events={events}
                windowSecs={win.to - win.from}
                segmentSecs={segmentSecs}
                nowTs={win.to}
                onSeek={goToMoment}
                onPickWindow={(from, to) => setZoom({ from, to })}
              />
            ) : (
              <Timeline
                windowSecs={win.to - win.from}
                segmentSecs={segmentSecs}
                segments={segments}
                events={events}
                nowTs={win.to}
                onSeek={(ts) => goToMoment(cameraId, ts)}
              />
            ))}

          {/* ── band 3: the content ── */}
          <div className="row find-controls" style={{ marginTop: 12 }}>
            <TogglePill on={!label} onClick={() => setLabel(null)}>
              Everything
            </TogglePill>
            {labelCounts.slice(0, 10).map(([l, n]) => (
              <TogglePill key={l} on={label === l} onClick={() => setLabel(label === l ? null : l)}>
                {prettyLabel(l)} <span className="muted">{n}</span>
              </TogglePill>
            ))}
          </div>

          <div className="row find-controls">
            <div className="arm-bar" role="group" aria-label="How to show results">
              <button
                type="button"
                className={view === "grid" ? "on" : ""}
                aria-pressed={view === "grid"}
                onClick={() => pickView("grid")}
              >
                <IconRadar size={13} /> Detections
              </button>
              <button
                type="button"
                className={view === "list" ? "on" : ""}
                aria-pressed={view === "list"}
                onClick={() => pickView("list")}
              >
                <IconFilm size={13} /> Everything, in order
              </button>
            </div>
            <label className="field" style={{ flex: "1 1 240px" }}>
              <span className="sr-only">Appearance search within this window</span>
              <input
                ref={searchRef}
                type="search"
                value={q}
                placeholder="Find by appearance — “red car”, “hi-vis jacket”"
                onChange={(e) => setQ(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && runSearch()}
                aria-label="Appearance search within this window"
              />
            </label>
            <button type="button" className="btn btn-ghost" onClick={runSearch} disabled={searching || !q.trim()}>
              <IconSearch size={14} /> {searching ? "Ranking…" : "Rank by appearance"}
            </button>
            {ranked && (
              <button type="button" className="btn btn-ghost" onClick={() => { setRanked(null); setQ(""); }}>
                <IconX size={13} /> Browse this window instead
              </button>
            )}
          </div>

          {/* Say which engine answered. A ranker's miss and an empty window are
              completely different facts and must never look the same. */}
          {ranked ? (
            <Callout tone="info" role="status">
              Ranked by appearance for <b>“{ranked.q}”</b> — top {ranked.events.length} of{" "}
              {ranked.scanned.toLocaleString()} detections in {windowLabel}. This is a visual
              similarity guess, not a filter: something can be here and not be ranked.
            </Callout>
          ) : (
            underFill && (
              <Callout tone="warn" role="status">
                {underFill}
              </Callout>
            )
          )}

          {loading ? (
            <div className="card" aria-busy="true" aria-label="Loading this window">
              {[0, 1, 2, 3, 4].map((i) => (
                <div key={i} className="skeleton" style={{ height: 44, margin: "10px 0" }} />
              ))}
            </div>
          ) : shownEvents.length === 0 && segments.length === 0 ? (
            <EmptyState
              icon={<IconFilm />}
              title={`Nothing recorded or detected in ${windowLabel}`}
              hint="Detections are kept far longer than video, so an older day can still have detections here after its footage has gone. Try a day the calendar shades darker."
              action={
                <button className="btn btn-ghost" onClick={() => { setDay(null); setZoom(null); }}>
                  Widen to the last 24 hours
                </button>
              }
            />
          ) : view === "list" ? (
            <FilmStrip items={strip} resetKey={`${scopeKey}|${label ?? ""}`} />
          ) : shownEvents.length === 0 ? (
            <EmptyState
              icon={<IconRadar />}
              title={`No detections in ${windowLabel}`}
              hint="There is footage from this window — switch to “Everything, in order” to scan it."
              action={
                <button className="btn btn-ghost" onClick={() => pickView("list")}>
                  Scan the footage
                </button>
              }
            />
          ) : (
            <>
            <div className="find-grid">
              {gridEvents.map((e) => (
                <a
                  key={e.id}
                  className="scrub-tile"
                  href={`#/live/${e.camera_id}/${e.ts}`}
                  title={`${prettyLabel(e.label)} on ${e.camera} — open the timeline here`}
                >
                  {e.snapshot ? (
                    <img
                      src={`/api/snapshots/${e.snapshot}?w=400`}
                      loading="lazy"
                      decoding="async"
                      alt={`${prettyLabel(e.label)} on ${e.camera}`}
                    />
                  ) : (
                    <div className="scrub-missing">no image</div>
                  )}
                  <span className="scrub-cap">
                    {prettyLabel(e.label)} · {clock(e.ts)}
                    <span className="scrub-count">{e.camera}</span>
                  </span>
                </a>
              ))}
            </div>
            {gridShown < shownEvents.length && (
              <button
                type="button"
                className="btn btn-ghost film-more"
                onClick={() => setGridShown((n) => n + GRID_PAGE)}
              >
                Show more — {(shownEvents.length - gridShown).toLocaleString()} more in{" "}
                {windowLabel}
              </button>
            )}
            </>
          )}
        </>
      )}
    </>
  );
}
