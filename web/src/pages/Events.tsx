import { useEffect, useRef, useState } from "react";
import { api, CamEvent, Camera, fmtTime, Segment, SimilarResult } from "../api";
import { useToast, useDialog, Modal, RelTime, EmptyState, ErrorState, TogglePill } from "../ui";

const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e));
import {
  IconSparkles, IconBell, IconStar, IconDownload, IconPlay, IconPencil,
  IconUser, IconStranger, IconCar, IconHand, IconZone, IconMic,
  IconAlert, IconCheck, IconLayers, IconUpload,
} from "../icons";

// --- A3: smart-detection grouping --------------------------------------------
// Collapse a run of same-camera, same-label detections that happen close in
// time into one "activity" card (best frame + count + duration), the way
// UniFi Protect groups motion into a single smart detection.
export interface Cluster {
  rep: CamEvent; // best (highest-score) frame in the run
  count: number;
  startTs: number; // oldest
  endTs: number; // newest
}
const GROUP_GAP = 120; // seconds between detections that still count as one activity

// Collapse a run of same-camera/same-label detections within GROUP_GAP into one
// representative cluster (highest-score frame + a count). Shared with the camera
// detail rail so a parked car doesn't flood it with identical thumbnails.
export function groupEvents(list: CamEvent[]): Cluster[] {
  const out: Cluster[] = [];
  for (const e of list) {
    // `list` is newest-first, so the cluster's first member is its newest (endTs).
    const last = out[out.length - 1];
    if (last && last.rep.camera_id === e.camera_id && last.rep.label === e.label && last.startTs - e.ts <= GROUP_GAP) {
      last.count++;
      last.startTs = e.ts;
      if (e.score > last.rep.score) last.rep = e;
    } else {
      out.push({ rep: e, count: 1, startTs: e.ts, endTs: e.ts });
    }
  }
  return out;
}

function durationLabel(secs: number): string {
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.round(secs / 60)}m`;
  return `${(secs / 3600).toFixed(1)}h`;
}

// --- B2: natural-language search parser --------------------------------------
// Pulls structured filters (time window, camera, object, identity) out of a
// plain-language query and leaves the rest as the visual/text search residual.
const LABEL_WORDS: Record<string, string> = {
  person: "person", people: "person", man: "person", woman: "person", someone: "person",
  car: "car", cars: "car", truck: "truck", trucks: "truck", bus: "bus",
  bike: "bicycle", bicycle: "bicycle", motorcycle: "motorcycle", motorbike: "motorcycle",
  dog: "dog", cat: "cat",
};

interface Parsed {
  residual: string;
  label?: string;
  cameraId?: number;
  face?: string;
  after?: number;
  before?: number;
  chips: string[];
}

function toLocalInput(ts: number): string {
  const d = new Date(ts * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}

function parseNL(raw: string, cameras: Camera[], faces: string[]): Parsed {
  let q = ` ${raw.toLowerCase()} `;
  const chips: string[] = [];
  const out: Parsed = { residual: raw, chips };
  const strip = (re: RegExp, chip?: string) => {
    if (re.test(q)) {
      q = q.replace(re, " ");
      if (chip) chips.push(chip);
      return true;
    }
    return false;
  };
  const startOfDay = (offsetDays = 0) => {
    const d = new Date();
    d.setHours(0, 0, 0, 0);
    return Math.floor(d.getTime() / 1000) + offsetDays * 86400;
  };

  // time windows (first match wins)
  let m: RegExpExecArray | null;
  if ((m = /\blast (\d+) (hour|hours|day|days|week|weeks)\b/.exec(q))) {
    const n = parseInt(m[1], 10);
    const unit = m[2].startsWith("hour") ? 3600 : m[2].startsWith("day") ? 86400 : 604800;
    out.after = Math.floor(Date.now() / 1000) - n * unit;
    strip(/\blast \d+ (hour|hours|day|days|week|weeks)\b/, `last ${n} ${m[2]}`);
  } else if (strip(/\b(today)\b/, "today")) {
    out.after = startOfDay(0);
  } else if (strip(/\b(yesterday)\b/, "yesterday")) {
    out.after = startOfDay(-1);
    out.before = startOfDay(0);
  } else if (strip(/\b(this week|past week|last week)\b/, "this week")) {
    out.after = Math.floor(Date.now() / 1000) - 7 * 86400;
  } else if (strip(/\b(last hour|past hour)\b/, "last hour")) {
    out.after = Math.floor(Date.now() / 1000) - 3600;
  } else if (strip(/\b(tonight|at night|after dark|overnight)\b/, "at night")) {
    out.after = startOfDay(0); // today; night nuance is left to visual search
  }

  // camera by name
  for (const c of cameras) {
    const re = new RegExp(`\\b${c.name.toLowerCase().replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`);
    if (strip(re, c.name)) {
      out.cameraId = c.id;
      break;
    }
  }
  // stranger
  if (strip(/\b(stranger|strangers|unfamiliar|unknown face|unknown person)\b/, "stranger")) {
    out.face = "?";
  } else {
    // enrolled face by name
    for (const f of faces) {
      if (f === "?") continue;
      const re = new RegExp(`\\b${f.toLowerCase().replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`);
      if (strip(re, f)) {
        out.face = f;
        break;
      }
    }
  }
  // object label
  for (const [word, label] of Object.entries(LABEL_WORDS)) {
    const re = new RegExp(`\\b${word}\\b`);
    if (re.test(q)) {
      out.label = label;
      q = q.replace(re, " ");
      if (!chips.includes(label)) chips.push(label);
      break;
    }
  }

  out.residual = q.replace(/\s+/g, " ").trim();
  return out;
}

export default function Events({
  cameras,
  focusEventId,
  onFocusHandled,
}: {
  cameras: Camera[];
  /** When set (e.g. from tapping a notification), open this event's detail. */
  focusEventId?: number | null;
  onFocusHandled?: () => void;
}) {
  const toast = useToast();
  const dialog = useDialog();
  const [events, setEvents] = useState<CamEvent[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [cameraId, setCameraId] = useState<number | "">("");
  const [label, setLabel] = useState("");
  const [review, setReview] = useState<"all" | "alerts">("all");
  const [alertLabels, setAlertLabels] = useState<string[]>(["person"]);
  const [plateDeny, setPlateDeny] = useState<string[]>([]);
  const [plateAllow, setPlateAllow] = useState<string[]>([]);
  const [query, setQuery] = useState("");
  const [searchResults, setSearchResults] = useState<CamEvent[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [faceFilter, setFaceFilter] = useState("");
  const [plateFilter, setPlateFilter] = useState("");
  const [gestureFilter, setGestureFilter] = useState("");
  const [zoneFilter, setZoneFilter] = useState("");
  const [fromTime, setFromTime] = useState("");
  const [toTime, setToTime] = useState("");
  const [flaggedOnly, setFlaggedOnly] = useState(false);
  const [grouped, setGrouped] = useState(false);
  const [interpreted, setInterpreted] = useState<string[]>([]);
  // Bulk triage: select multiple events, then bookmark/unbookmark them at once.
  const [selectMode, setSelectMode] = useState(false);
  const [selected, setSelected] = useState<Set<number>>(new Set());

  const runSearch = async () => {
    const raw = query.trim();
    if (!raw) {
      setSearchResults(null);
      setInterpreted([]);
      return;
    }
    // B2: pull structured filters (time / camera / object / identity) out of the
    // natural-language query; the leftover text is the visual + transcript search.
    const faceNames = [...new Set(events.map((e) => e.face).filter(Boolean))] as string[];
    const p = parseNL(raw, cameras, faceNames);
    if (p.label) setLabel(p.label);
    if (p.cameraId != null) setCameraId(p.cameraId);
    if (p.face) setFaceFilter(p.face);
    if (p.after != null) setFromTime(toLocalInput(p.after));
    if (p.before != null) setToTime(toLocalInput(p.before));
    setInterpreted(p.chips);

    if (!p.residual) {
      // Fully structured query — let the filters drive the server-side list.
      setSearchResults(null);
      setSearching(false);
      return;
    }
    setSearching(true);
    try {
      const r = await api.search(p.residual, 48);
      setSearchResults(r.results.map((x) => x.event));
    } catch (e) {
      // null (not []) falls back to the normal event list instead of showing a
      // false "no events match" — and the toast says why the search didn't run.
      toast.error(`Search failed: ${errMsg(e)}`);
      setSearchResults(null);
    } finally {
      setSearching(false);
    }
  };

  const clearSearch = () => {
    setQuery("");
    setSearchResults(null);
    setInterpreted([]);
  };
  const [open, setOpen] = useState<CamEvent | null>(null);
  const [playing, setPlaying] = useState<{ segment: Segment; offset: number } | null>(null);
  const [similar, setSimilar] = useState<{ ev: CamEvent; res: SimilarResult | null } | null>(null);
  // Upload-a-photo appearance search: query is a local object-URL preview.
  const [imgSearch, setImgSearch] = useState<{ url: string; res: SimilarResult | null } | null>(null);
  const imgReq = useRef(0);
  const imgFileRef = useRef<HTMLInputElement>(null);
  const [noClip, setNoClip] = useState<number | null>(null);

  // Deep-link from a notification: once events have loaded, open the matching
  // event's detail (best-effort — the event is usually recent and in the list),
  // then clear the request. onFocusHandled clears focusEventId synchronously, so
  // this fires once per click without a guard (and re-clicks work).
  useEffect(() => {
    if (!focusEventId || events.length === 0) return;
    const ev = events.find((e) => e.id === focusEventId);
    if (ev) setOpen(ev);
    onFocusHandled?.();
  }, [focusEventId, events, onFocusHandled]);

  // Protect-style playback shortcuts: space pause, arrows seek (shift =
  // frame-ish steps), f fullscreen, Esc close.
  useEffect(() => {
    if (!playing) return;
    const onKey = (e: KeyboardEvent) => {
      const v = document.querySelector<HTMLVideoElement>(".modal-bg video");
      if (!v) return;
      if (e.key === " ") {
        e.preventDefault();
        if (v.paused) v.play();
        else v.pause();
      } else if (e.key === "ArrowLeft") {
        v.currentTime = Math.max(0, v.currentTime - (e.shiftKey ? 1 / 15 : 5));
      } else if (e.key === "ArrowRight") {
        v.currentTime = Math.min(v.duration, v.currentTime + (e.shiftKey ? 1 / 15 : 5));
      } else if (e.key === "f") {
        v.requestFullscreen().catch(() => {});
      } else if (e.key === "Escape") {
        setPlaying(null);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [playing]);

  const jumpToRecording = async (ev: CamEvent) => {
    try {
      const r = await api.recordingAt(ev.camera_id, ev.ts);
      // Land a few seconds before the event so you see it happen.
      setPlaying({ segment: r.segment, offset: Math.max(0, r.offset_secs - 3) });
    } catch {
      setNoClip(ev.id);
      setTimeout(() => setNoClip(null), 2500);
    }
  };

  // Appearance search: find the same person/vehicle on other cameras/times.
  // A token guards against out-of-order responses (and a late response
  // re-opening a modal the user already closed — the close handler bumps it too).
  const similarReq = useRef(0);
  const findSimilar = async (ev: CamEvent) => {
    const token = ++similarReq.current;
    setSimilar({ ev, res: null }); // open the modal in a loading state
    try {
      const res = await api.eventSimilar(ev.id, 24);
      if (token === similarReq.current) setSimilar({ ev, res });
    } catch (e) {
      if (token === similarReq.current) {
        toast.error(String(e));
        setSimilar(null);
      }
    }
  };

  // Upload-a-reference-photo search: CLIP-rank the crop corpus against an image
  // the user picks (a suspect/vehicle never enrolled or even seen by our cameras).
  const runImageSearch = async (file: File) => {
    const token = ++imgReq.current;
    const url = URL.createObjectURL(file);
    setImgSearch((prev) => {
      if (prev) URL.revokeObjectURL(prev.url);
      return { url, res: null };
    });
    try {
      const res = await api.searchByImage(file, 24);
      if (token === imgReq.current) setImgSearch({ url, res });
      else URL.revokeObjectURL(url);
    } catch (e) {
      if (token === imgReq.current) {
        toast.error(errMsg(e));
        URL.revokeObjectURL(url);
        setImgSearch(null);
      }
    }
  };

  // Bookmarks: a flagged event is kept past retention; a note adds context.
  const applyBookmark = (id: number, flagged: boolean, note: string | null) => {
    const upd = (e: CamEvent) => (e.id === id ? { ...e, flagged, note } : e);
    setEvents((prev) => prev.map(upd));
    setSearchResults((prev) => (prev ? prev.map(upd) : prev));
    setOpen((prev) => (prev && prev.id === id ? { ...prev, flagged, note } : prev));
  };
  const toggleFlag = async (ev: CamEvent) => {
    // A note only lives on a saved event — un-saving discards it (so a note is
    // never left orphaned, hidden from the Saved filter and lost at retention).
    if (
      ev.flagged &&
      ev.note &&
      !(await dialog.confirm({
        title: "Remove bookmark?",
        body: "Its note will be deleted, and the event can be pruned at retention.",
        confirmLabel: "Remove",
        danger: true,
      }))
    ) {
      return;
    }
    const flagged = !ev.flagged;
    const note = flagged ? ev.note : null;
    try {
      await api.bookmarkEvent(ev.id, flagged, note);
      applyBookmark(ev.id, flagged, note);
      toast.success(flagged ? "Event saved" : "Bookmark removed");
    } catch (e) {
      toast.error(`Couldn't update bookmark: ${e}`);
    }
  };
  const toggleSelect = (id: number) =>
    setSelected((prev) => {
      const n = new Set(prev);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });
  const exitSelect = () => {
    setSelectMode(false);
    setSelected(new Set());
  };
  const bulkBookmark = async (flag: boolean) => {
    const ids = [...selected];
    if (ids.length === 0) return;
    if (
      !flag &&
      !(await dialog.confirm({
        title: `Remove bookmark from ${ids.length} event${ids.length === 1 ? "" : "s"}?`,
        body: "Any notes are deleted and the events can be pruned at retention.",
        confirmLabel: "Remove",
        danger: true,
      }))
    )
      return;
    // Preserve each event's existing note when saving (the endpoint always sends
    // a note, so pass the current one rather than clearing it).
    const byId = new Map<number, CamEvent>(
      [...events, ...(searchResults ?? [])].map((e) => [e.id, e]),
    );
    let ok = 0;
    for (const id of ids) {
      const note = flag ? byId.get(id)?.note ?? null : null;
      try {
        await api.bookmarkEvent(id, flag, note);
        applyBookmark(id, flag, note);
        ok++;
      } catch {
        /* keep going; report the count below */
      }
    }
    toast[ok === ids.length ? "success" : "info"](
      `${flag ? "Saved" : "Unsaved"} ${ok} event${ok === 1 ? "" : "s"}${
        ok < ids.length ? ` · ${ids.length - ok} failed` : ""
      }`,
    );
    setSelected(new Set());
  };

  const editNote = async (ev: CamEvent) => {
    const note = await dialog.prompt({
      title: ev.note ? "Edit note" : "Add note",
      label: "Note for this event",
      defaultValue: ev.note ?? "",
      placeholder: "e.g. delivery driver, ignore",
      multiline: true,
      maxLength: 500,
    });
    if (note === null) return; // cancelled
    const trimmed = note.trim();
    // Adding a note implies keeping the event (flag it); clearing leaves the flag.
    const flagged = trimmed ? true : ev.flagged;
    try {
      await api.bookmarkEvent(ev.id, flagged, trimmed || null);
      applyBookmark(ev.id, flagged, trimmed || null);
      toast.success(trimmed ? "Note saved" : "Note cleared");
    } catch (e) {
      toast.error(`Couldn't save note: ${e}`);
    }
  };

  const load = () => {
    const after = fromTime ? Math.floor(new Date(fromTime).getTime() / 1000) : undefined;
    const before = toTime ? Math.floor(new Date(toTime).getTime() / 1000) : undefined;
    api
      .events({
        camera_id: cameraId === "" ? undefined : cameraId,
        label: label || undefined,
        after,
        before,
        flagged: flaggedOnly || undefined,
        limit: 200,
      })
      .then((d) => {
        setEvents(d);
        setLoadError(null);
      })
      .catch((e) => setLoadError(errMsg(e)))
      .finally(() => setLoaded(true));
  };

  // Download the current filter set as a CSV (server streams it with the same
  // filters the list uses).
  const exportUrl = () => {
    const p = new URLSearchParams();
    if (cameraId !== "") p.set("camera_id", String(cameraId));
    if (label) p.set("label", label);
    const after = fromTime ? Math.floor(new Date(fromTime).getTime() / 1000) : undefined;
    const before = toTime ? Math.floor(new Date(toTime).getTime() / 1000) : undefined;
    if (after != null) p.set("after", String(after));
    if (before != null) p.set("before", String(before));
    if (flaggedOnly) p.set("flagged", "true");
    return `/api/events/export.csv?${p}`;
  };

  useEffect(() => {
    api
      .settings()
      .then((s) => {
        setAlertLabels(s.alert_labels ?? ["person"]);
        setPlateDeny(s.plate_denylist ?? []);
        setPlateAllow(s.plate_allowlist ?? []);
      })
      .catch(() => {});
  }, []);

  // Classify a read plate against the watch lists (deny wins).
  const plateClass = (plate: string | null): "deny" | "allow" | "" => {
    if (!plate) return "";
    const p = plate.toUpperCase();
    if (plateDeny.some((e) => e.trim() && p.includes(e.trim().toUpperCase()))) return "deny";
    if (plateAllow.some((e) => e.trim() && p.includes(e.trim().toUpperCase()))) return "allow";
    return "";
  };

  useEffect(() => {
    load();
    const t = setInterval(load, 5000); // events appear as they happen
    return () => clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cameraId, label, fromTime, toTime, flaggedOnly]);

  const labels = [...new Set(events.map((e) => e.label))];
  const faces = [...new Set(events.map((e) => e.face).filter(Boolean))] as string[];
  const gestures = [...new Set(events.map((e) => e.gesture).filter(Boolean))] as string[];
  const zones = [...new Set(events.map((e) => e.zone).filter(Boolean))] as string[];
  let shown =
    searchResults ??
    (review === "alerts" ? events.filter((e) => alertLabels.includes(e.label)) : events);
  if (faceFilter) shown = shown.filter((e) => e.face === faceFilter);
  if (gestureFilter) shown = shown.filter((e) => e.gesture === gestureFilter);
  if (zoneFilter) shown = shown.filter((e) => e.zone === zoneFilter);
  if (plateFilter.trim())
    shown = shown.filter((e) =>
      (e.plate ?? "").toUpperCase().includes(plateFilter.trim().toUpperCase())
    );
  // Apply the time window client-side too, so it also narrows smart-search
  // results (the server only time-filters the plain list, not the search).
  const afterTs = fromTime ? Math.floor(new Date(fromTime).getTime() / 1000) : undefined;
  const beforeTs = toTime ? Math.floor(new Date(toTime).getTime() / 1000) : undefined;
  if (afterTs != null) shown = shown.filter((e) => e.ts >= afterTs);
  if (beforeTs != null) shown = shown.filter((e) => e.ts < beforeTs);

  // A3: optionally collapse runs of detections into activity clusters.
  const list: { ev: CamEvent; cluster?: Cluster }[] = grouped
    ? groupEvents(shown).map((c) => ({ ev: c.rep, cluster: c }))
    : shown.map((ev) => ({ ev }));

  // Explore: object-type counts across the loaded window (pre object-filter).
  const exploreBase = searchResults ?? events;
  const counts = exploreBase.reduce<Record<string, number>>((acc, e) => {
    acc[e.label] = (acc[e.label] ?? 0) + 1;
    return acc;
  }, {});
  const topLabels = Object.entries(counts).sort((a, b) => b[1] - a[1]);

  return (
    <>
      <h1>Events</h1>

      <div className="smart-search">
        <span className="smart-ico"><IconSparkles size={18} /></span>
        <input
          type="text"
          placeholder='Smart search — what you saw or heard ("person in a dark coat", "someone yelling help")'
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            if (e.target.value.trim() === "") setSearchResults(null);
          }}
          onKeyDown={(e) => e.key === "Enter" && runSearch()}
        />
        {(searchResults || interpreted.length > 0) && (
          <button className="btn btn-ghost" onClick={clearSearch}>Clear</button>
        )}
        <input
          ref={imgFileRef}
          type="file"
          accept="image/*"
          style={{ display: "none" }}
          onChange={(e) => {
            const f = e.target.files?.[0];
            if (f) runImageSearch(f);
            e.target.value = ""; // allow re-picking the same file
          }}
        />
        <button
          className="btn btn-ghost"
          title="Search by a reference photo — find this person/vehicle across all cameras (CLIP appearance match)"
          onClick={() => imgFileRef.current?.click()}
        >
          <IconUpload size={16} /> Photo
        </button>
        <button className="btn btn-primary" onClick={runSearch} disabled={searching || !query.trim()}>
          {searching ? "searching…" : "Search"}
        </button>
      </div>
      {interpreted.length > 0 && (
        <div className="row" style={{ marginTop: -8, marginBottom: 14, gap: 6, flexWrap: "wrap" }}>
          <span className="muted" style={{ fontSize: "var(--text-sm)" }}>Interpreted as</span>
          {interpreted.map((c) => (
            <span key={c} className="badge accent">{c}</span>
          ))}
        </div>
      )}
      <div className="row" style={{ marginBottom: 16 }}>
        <button className={review === "all" ? "primary" : "ghost"} onClick={() => setReview("all")}>
          All
        </button>
        <button
          className={`btn ${review === "alerts" ? "btn-primary" : "btn-ghost"}`}
          onClick={() => setReview("alerts")}
          title={`alert labels: ${alertLabels.join(", ")}`}
        >
          <IconBell size={15} /> Alerts
        </button>
        <button
          className={`btn ${flaggedOnly ? "btn-primary" : "btn-ghost"}`}
          onClick={() => setFlaggedOnly((v) => !v)}
          title="Show only bookmarked events (kept past retention)"
        >
          <IconStar size={15} filled={flaggedOnly} /> Saved
        </button>
        <button
          className={`btn ${grouped ? "btn-primary" : "btn-ghost"}`}
          onClick={() => setGrouped((v) => !v)}
          title="Group a run of detections into one activity (best frame + count + duration)"
        >
          <IconLayers size={15} /> Group
        </button>
        <button
          className={`btn ${selectMode ? "btn-primary" : "btn-ghost"}`}
          onClick={() => (selectMode ? exitSelect() : setSelectMode(true))}
          title="Select multiple events to bookmark in bulk"
          aria-pressed={selectMode}
        >
          <IconCheck size={15} /> {selectMode ? "Done" : "Select"}
        </button>
        <select value={cameraId} onChange={(e) => setCameraId(e.target.value === "" ? "" : Number(e.target.value))}>
          <option value="">all cameras</option>
          {cameras.map((c) => (
            <option key={c.id} value={c.id}>
              {c.name}
            </option>
          ))}
        </select>
        <select value={label} onChange={(e) => setLabel(e.target.value)}>
          <option value="">all objects</option>
          {labels.map((l) => (
            <option key={l} value={l}>
              {l}
            </option>
          ))}
        </select>
        <select value={faceFilter} onChange={(e) => setFaceFilter(e.target.value)} aria-label="Filter by face">
          <option value="">anyone</option>
          {faces.map((f) => (
            <option key={f} value={f}>
              {f === "?" ? "stranger (unknown)" : f}
            </option>
          ))}
        </select>
        <span className="muted count">{shown.length} events · auto-refreshing</span>
        <a className="btn btn-ghost" href={exportUrl()} title="Download the current filter as a CSV">
          <IconDownload size={15} /> Export CSV
        </a>
      </div>

      {/* Power-user filters tucked behind a disclosure so the everyday triage row
          stays scannable; force-open whenever one of them is active. */}
      <details
        className="adv"
        open={!!(gestureFilter || zoneFilter || fromTime || toTime || plateFilter)}
      >
        <summary>More filters — hand signal, zone, time range, plate</summary>
        <div className="row" style={{ marginTop: 8, marginBottom: 16 }}>
          {gestures.length > 0 && (
            <select value={gestureFilter} onChange={(e) => setGestureFilter(e.target.value)} aria-label="Filter by hand signal">
              <option value="">any signal</option>
              {gestures.map((g) => (
                <option key={g} value={g}>
                  {g}
                </option>
              ))}
            </select>
          )}
          {zones.length > 0 && (
            <select value={zoneFilter} onChange={(e) => setZoneFilter(e.target.value)} aria-label="Filter by zone">
              <option value="">any zone</option>
              {zones.map((z) => (
                <option key={z} value={z}>
                  {z}
                </option>
              ))}
            </select>
          )}
          <label className="field" title="from">
            <input type="datetime-local" value={fromTime} onChange={(e) => setFromTime(e.target.value)} />
          </label>
          <label className="field" title="to">
            <input type="datetime-local" value={toTime} onChange={(e) => setToTime(e.target.value)} />
          </label>
          {(fromTime || toTime) && (
            <button
              className="ghost"
              onClick={() => {
                setFromTime("");
                setToTime("");
              }}
            >
              Clear time
            </button>
          )}
          <input
            type="text"
            placeholder="plate…"
            style={{ width: 110 }}
            value={plateFilter}
            onChange={(e) => setPlateFilter(e.target.value)}
          />
        </div>
      </details>

      {topLabels.length > 0 && !searchResults && (
        <div className="row" style={{ marginBottom: 12, flexWrap: "wrap" }}>
          <span className="muted">Explore:</span>
          <TogglePill on={label === ""} ariaLabel="Show all objects" onClick={() => setLabel("")}>
            all ({exploreBase.length})
          </TogglePill>
          {topLabels.map(([l, n]) => (
            <TogglePill
              key={l}
              on={label === l}
              ariaLabel={`Filter to ${l}`}
              onClick={() => setLabel(label === l ? "" : l)}
            >
              {l} ({n})
            </TogglePill>
          ))}
        </div>
      )}

      {selectMode && (
        <div className="select-bar">
          <span className="select-count">
            <b>{selected.size}</b> selected
          </span>
          <button
            className="btn btn-ghost ev-act"
            onClick={() => setSelected(new Set(list.map(({ ev }) => ev.id)))}
          >
            Select all ({list.length})
          </button>
          <button
            className="btn btn-ghost ev-act"
            onClick={() => setSelected(new Set())}
            disabled={selected.size === 0}
          >
            Clear
          </button>
          <span className="spacer" />
          <button
            className="btn btn-primary ev-act"
            onClick={() => bulkBookmark(true)}
            disabled={selected.size === 0}
          >
            <IconStar size={14} /> Save
          </button>
          <button
            className="btn btn-ghost ev-act"
            onClick={() => bulkBookmark(false)}
            disabled={selected.size === 0}
          >
            Unsave
          </button>
        </div>
      )}

      {list.length === 0 ? (
        !loaded && !searchResults && interpreted.length === 0 && !loadError ? (
          <div className="event-grid" aria-busy="true">
            {Array.from({ length: 8 }).map((_, i) => (
              <div className="event-card" key={i}>
                <span className="skeleton" style={{ display: "block", aspectRatio: "4 / 3" }} />
                <div className="meta">
                  <span className="skeleton" style={{ height: 14, width: "70%" }} />
                </div>
              </div>
            ))}
          </div>
        ) : loadError && !searchResults ? (
          <ErrorState what="events" message={loadError} onRetry={load} />
        ) : searchResults || interpreted.length > 0 ? (
          <EmptyState
            icon={<IconSparkles />}
            title={query ? `No events match “${query}”` : "No events match these filters"}
            hint="Try a broader search, a different camera, or clearing the active filters."
          />
        ) : (
          <EmptyState
            icon={<IconBell />}
            title="No events yet"
            hint="Events appear here when a detect-enabled camera sees motion and the AI recognizes an object — a person, vehicle, package, and more."
          />
        )
      ) : (
        <div className="event-grid">
          {list.map(({ ev, cluster }) => {
            const isSel = selected.has(ev.id);
            const activate = () => (selectMode ? toggleSelect(ev.id) : setOpen(ev));
            return (
            <div
              className={`event-card ${selectMode && isSel ? "selected" : ""}`}
              key={ev.id}
              role="button"
              tabIndex={0}
              aria-pressed={selectMode ? isSel : undefined}
              aria-label={
                selectMode
                  ? `${isSel ? "Deselect" : "Select"} ${ev.label} from ${ev.camera}`
                  : `Open ${ev.label} event from ${ev.camera}`
              }
              onClick={activate}
              onKeyDown={(e) => {
                // Enter/Space act on the card, but only when the card itself is
                // focused — not when a nested action button has focus.
                if ((e.key === "Enter" || e.key === " ") && e.target === e.currentTarget) {
                  e.preventDefault();
                  activate();
                }
              }}
            >
              {selectMode && (
                <span className={`event-check ${isSel ? "on" : ""}`} aria-hidden="true">
                  {isSel && <IconCheck size={14} />}
                </span>
              )}
              {ev.snapshot ? (
                <img src={`/api/snapshots/${ev.snapshot}?w=400`} alt={`${ev.label} on ${ev.camera}`} loading="lazy" decoding="async" />
              ) : (
                <div style={{ aspectRatio: "4 / 3", background: "var(--bg-sunken)" }} />
              )}
              <div className="meta">
                <div className="ev-head">
                  <b className="ev-label">{ev.label}</b>
                  <span className="ev-score score">{(ev.score * 100).toFixed(0)}%</span>
                  <span className="muted">{ev.camera}</span>
                  {cluster && cluster.count > 1 && (
                    <span className="badge" title={`${cluster.count} detections in this activity`}>
                      <IconLayers size={11} /> ×{cluster.count} · {durationLabel(cluster.endTs - cluster.startTs)}
                    </span>
                  )}
                </div>
                {(ev.face || ev.plate || ev.gesture || ev.zone || ev.gait) && (
                  <div className="ev-chips">
                    {ev.face === "?" ? (
                      <span className="badge warn" title="A face was seen but matched nobody enrolled">
                        <IconStranger size={13} /> stranger
                      </span>
                    ) : ev.face ? (
                      <span className="badge ok"><IconUser size={13} /> {ev.face}</span>
                    ) : null}
                    {ev.plate && (
                      <>
                        <span className={`badge ${plateClass(ev.plate) === "deny" ? "danger" : plateClass(ev.plate) === "allow" ? "ok" : "warn"}`}>
                          <IconCar size={13} /> {ev.plate}
                        </span>
                        {plateClass(ev.plate) === "deny" && (
                          <span className="badge danger"><IconAlert size={12} /> of interest</span>
                        )}
                        {plateClass(ev.plate) === "allow" && (
                          <span className="badge ok"><IconCheck size={12} /> known</span>
                        )}
                      </>
                    )}
                    {ev.gesture && <span className="badge accent"><IconHand size={13} /> {ev.gesture}</span>}
                    {ev.zone && <span className="badge"><IconZone size={13} /> {ev.zone}</span>}
                    {ev.gait === "?" ? (
                      <span className="badge warn" title="Tracked walking but matched no enrolled gait">
                        <IconStranger size={13} /> unknown walk
                      </span>
                    ) : ev.gait ? (
                      <span className="badge ok" title="Identified by gait (how they walk)">
                        <IconUser size={13} /> {ev.gait} · gait
                      </span>
                    ) : null}
                  </div>
                )}
                {ev.caption && <div className="ev-caption">“{ev.caption}”</div>}
                {ev.transcript && (
                  <div className="ev-line" title="Speech-to-text of the event audio">
                    <IconMic size={13} /> <span>“{ev.transcript}”</span>
                  </div>
                )}
                {ev.note && (
                  <div className="ev-line" title="Your note">
                    <IconPencil size={13} /> <span>{ev.note}</span>
                  </div>
                )}
                <RelTime ts={ev.ts} className="muted ev-time" />
                <div className="ev-actions">
                  <button
                    className={`btn ev-act ${ev.flagged ? "btn-primary" : "btn-ghost"}`}
                    aria-pressed={ev.flagged}
                    title={
                      ev.flagged
                        ? "Bookmarked — kept past retention. Click to remove."
                        : "Bookmark this event (keep it past retention)"
                    }
                    onClick={(e) => {
                      e.stopPropagation();
                      toggleFlag(ev);
                    }}
                  >
                    <IconStar size={14} filled={ev.flagged} /> {ev.flagged ? "Saved" : "Save"}
                  </button>
                  <button
                    className="btn btn-ghost ev-act"
                    aria-label={ev.note ? "Edit note" : "Add note"}
                    title={ev.note ? "Edit note" : "Add a note"}
                    onClick={(e) => {
                      e.stopPropagation();
                      editNote(ev);
                    }}
                  >
                    <IconPencil size={14} />
                  </button>
                  <button
                    className="btn btn-ghost ev-act"
                    onClick={(e) => {
                      e.stopPropagation();
                      jumpToRecording(ev);
                    }}
                  >
                    <IconPlay size={13} /> {noClip === ev.id ? "no clip" : "Recording"}
                  </button>
                  <a
                    className="btn btn-ghost ev-act"
                    href={`/api/events/${ev.id}/clip`}
                    onClick={(e) => e.stopPropagation()}
                    title="Download a short clip"
                  >
                    <IconDownload size={14} /> Clip
                  </a>
                  {ev.snapshot && (
                    <button
                      className="btn btn-ghost ev-act"
                      title="Find this person/vehicle on other cameras (appearance search)"
                      onClick={(e) => {
                        e.stopPropagation();
                        findSimilar(ev);
                      }}
                    >
                      <IconUser size={14} /> Similar
                    </button>
                  )}
                </div>
              </div>
            </div>
            );
          })}
        </div>
      )}

      {open && (
        <Modal className="lightbox" title={`${open.label} · ${open.camera}`} onClose={() => setOpen(null)}>
          {open.snapshot && (
            <img
              className="lightbox-img"
              src={`/api/snapshots/${open.snapshot}`}
              alt={`${open.label} detected at ${open.camera}, ${fmtTime(open.ts)}`}
            />
          )}
          <div className="lightbox-meta">
            <span className="badge accent score">{(open.score * 100).toFixed(0)}%</span>
            {open.face === "?" ? (
              <span className="badge warn"><IconStranger size={13} /> stranger</span>
            ) : open.face ? (
              <span className="badge ok"><IconUser size={13} /> {open.face}</span>
            ) : null}
            {open.plate && <span className="badge warn"><IconCar size={13} /> {open.plate}</span>}
            {open.gesture && <span className="badge accent"><IconHand size={13} /> {open.gesture}</span>}
            {open.zone && <span className="badge"><IconZone size={13} /> {open.zone}</span>}
            <span className="muted clock" style={{ marginLeft: "auto" }}>{fmtTime(open.ts)}</span>
          </div>
          {open.transcript && <p className="ev-line" style={{ margin: "10px 0 0" }}><IconMic size={14} /> <span>“{open.transcript}”</span></p>}
        </Modal>
      )}

      {similar && (
        <Modal
          title={`Similar to this ${similar.ev.label} · ${similar.ev.camera}`}
          onClose={() => {
            similarReq.current++; // ignore any in-flight response after close
            setSimilar(null);
          }}
        >
          <div className="row" style={{ alignItems: "flex-start", gap: 14, flexWrap: "wrap" }}>
            {similar.ev.snapshot && (
              <div style={{ flex: "0 0 180px" }}>
                <img
                  src={`/api/snapshots/${similar.ev.snapshot}?w=360`}
                  alt={`${similar.ev.label} on ${similar.ev.camera}`}
                  decoding="async"
                  style={{ width: "100%", borderRadius: 8, border: "2px solid var(--accent-border)" }}
                />
                <div className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 4 }}>
                  query · {fmtTime(similar.ev.ts)}
                </div>
              </div>
            )}
            <div style={{ flex: "1 1 320px", minWidth: "min(280px, 100%)" }}>
              {!similar.res ? (
                <p className="muted">Searching across cameras…</p>
              ) : !similar.res.available ? (
                <p className="muted">
                  No appearance fingerprint for this event — appearance search needs the smart-search
                  (CLIP) models installed and applies to object detections.
                </p>
              ) : similar.res.results.length === 0 ? (
                <p className="muted">No similar appearances found on any camera yet.</p>
              ) : (
                <div
                  style={{
                    display: "grid",
                    gridTemplateColumns: "repeat(auto-fill, minmax(140px, 1fr))",
                    gap: 10,
                  }}
                >
                  {similar.res.results.map((m) => (
                    <button
                      key={m.event.id}
                      className="event-card"
                      style={{
                        textAlign: "left",
                        cursor: "pointer",
                        // .event-card was authored for a <div>; reset UA button chrome.
                        appearance: "none",
                        font: "inherit",
                        color: "inherit",
                        padding: 0,
                        width: "100%",
                      }}
                      onClick={() => {
                        similarReq.current++;
                        setSimilar(null);
                        setOpen(m.event);
                      }}
                    >
                      {m.event.snapshot ? (
                        <img src={`/api/snapshots/${m.event.snapshot}?w=300`} alt={`${m.event.label} on ${m.event.camera}`} loading="lazy" decoding="async" />
                      ) : (
                        <div style={{ aspectRatio: "4 / 3", background: "var(--bg-sunken)" }} />
                      )}
                      <div className="meta">
                        <div className="ev-head">
                          <span className="badge accent score">{(m.similarity * 100).toFixed(0)}%</span>
                          <span className="muted">{m.event.camera}</span>
                        </div>
                        <RelTime ts={m.event.ts} className="muted ev-time" />
                      </div>
                    </button>
                  ))}
                </div>
              )}
            </div>
          </div>
        </Modal>
      )}

      {imgSearch && (
        <Modal
          title="Similar to your photo"
          onClose={() => {
            imgReq.current++; // ignore any in-flight response after close
            URL.revokeObjectURL(imgSearch.url);
            setImgSearch(null);
          }}
        >
          <div className="row" style={{ alignItems: "flex-start", gap: 14, flexWrap: "wrap" }}>
            <div style={{ flex: "0 0 180px" }}>
              <img
                src={imgSearch.url}
                alt="Your uploaded query photo"
                style={{ width: "100%", borderRadius: 8, border: "2px solid var(--accent-border)" }}
              />
              <div className="muted" style={{ fontSize: "0.78rem", marginTop: 4 }}>
                uploaded photo
              </div>
            </div>
            <div style={{ flex: "1 1 320px", minWidth: "min(280px, 100%)" }}>
              {!imgSearch.res ? (
                <p className="muted">Matching your photo across cameras…</p>
              ) : !imgSearch.res.available ? (
                <p className="muted">
                  Photo search needs the smart-search (CLIP) models installed; it ranks against
                  stored object-detection crops.
                </p>
              ) : imgSearch.res.results.length === 0 ? (
                <p className="muted">No similar appearances found on any camera yet.</p>
              ) : (
                <div
                  style={{
                    display: "grid",
                    gridTemplateColumns: "repeat(auto-fill, minmax(140px, 1fr))",
                    gap: 10,
                  }}
                >
                  {imgSearch.res.results.map((m) => (
                    <button
                      key={m.event.id}
                      className="event-card"
                      style={{
                        textAlign: "left",
                        cursor: "pointer",
                        appearance: "none",
                        font: "inherit",
                        color: "inherit",
                        padding: 0,
                        width: "100%",
                      }}
                      onClick={() => {
                        imgReq.current++;
                        URL.revokeObjectURL(imgSearch.url);
                        setImgSearch(null);
                        setOpen(m.event);
                      }}
                    >
                      {m.event.snapshot ? (
                        <img src={`/api/snapshots/${m.event.snapshot}?w=300`} alt={`${m.event.label} on ${m.event.camera}`} loading="lazy" decoding="async" />
                      ) : (
                        <div style={{ aspectRatio: "4 / 3", background: "var(--bg-sunken)" }} />
                      )}
                      <div className="meta">
                        <div className="ev-head">
                          <span className="badge accent score">{(m.similarity * 100).toFixed(0)}%</span>
                          <span className="muted">{m.event.camera}</span>
                        </div>
                        <RelTime ts={m.event.ts} className="muted ev-time" />
                      </div>
                    </button>
                  ))}
                </div>
              )}
            </div>
          </div>
        </Modal>
      )}

      {playing && (
        <Modal bare onClose={() => setPlaying(null)}>
          <video
            src={`/api/recordings/${playing.segment.id}/video`}
            controls
            autoPlay
            onLoadedMetadata={(e) => {
              e.currentTarget.currentTime = playing.offset;
            }}
          />
        </Modal>
      )}
    </>
  );
}
