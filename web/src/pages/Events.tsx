import { useEffect, useRef, useState } from "react";
import { api, AttributesCatalog, CamEvent, Camera, fmtTime, Segment, SimilarResult } from "../api";
import { useToast, useDialog, Modal, RelTime, EmptyState, ErrorState, TogglePill } from "../ui";
import Timeline from "../Timeline";

const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e));

/// Hide an AI caption that argues with the detection ("No cat detected, just a
/// swimming pool…") — the card contradicting itself erodes trust more than a
/// missing caption does. Cheap heuristic: the caption denies the event label.
function captionContradicts(ev: { label: string; caption: string | null }): boolean {
  const c = (ev.caption ?? "").toLowerCase();
  const l = ev.label.toLowerCase();
  return c.includes(`no ${l}`) || c.includes(`not a ${l}`);
}
import {
  IconSparkles, IconBell, IconStar, IconDownload, IconPlay, IconPencil, IconLink, IconShield,
  IconUser, IconStranger, IconCar, IconHand, IconZone, IconMic,
  IconAlert, IconCheck, IconLayers, IconUpload, IconTag, IconX, IconVideo,
  IconChevronLeft, IconChevronRight, IconThumbDown, IconRadar,
} from "../icons";
import LifecycleModal from "../LifecycleModal";
// A3 smart-detection grouping lives in a shared module (the camera detail rail
// uses it too) — see eventGroups.ts.
import { Cluster, groupEvents } from "../eventGroups";
import { isCameraSide, prettyLabel, prettyZone, prettyGesture } from "../labels";

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
  // P2.5 attribute facets: the catalog (fetched once) + the active facet chip.
  const [attrCatalog, setAttrCatalog] = useState<AttributesCatalog | null>(null);
  const [attrKey, setAttrKey] = useState<string | null>(null);
  const [faceFilter, setFaceFilter] = useState("");
  const [plateFilter, setPlateFilter] = useState("");
  const [gestureFilter, setGestureFilter] = useState("");
  const [zoneFilter, setZoneFilter] = useState("");
  const [fromTime, setFromTime] = useState("");
  const [toTime, setToTime] = useState("");
  const [flaggedOnly, setFlaggedOnly] = useState(false);
  const [tagFilter, setTagFilter] = useState<string | null>(null);
  const [highOnly, setHighOnly] = useState(false);
  const [grouped, setGrouped] = useState(false);
  // "More filters" disclosure is state-driven (not derived from the filters), so
  // clearing the last active filter can't collapse the panel around the control
  // being used (React would drop focus mid-typing). A user toggle always wins;
  // the effect below only ever OPENS it when a hidden filter becomes active.
  const [moreFilters, setMoreFilters] = useState(false);
  // Filters that live inside the disclosure (the time range counts as one).
  const hiddenFilters =
    [gestureFilter, zoneFilter, fromTime || toTime, plateFilter.trim()].filter(Boolean).length;
  const anyHiddenFilter = hiddenFilters > 0;
  useEffect(() => {
    if (anyHiddenFilter) setMoreFilters(true);
  }, [anyHiddenFilter]);
  const [interpreted, setInterpreted] = useState<string[]>([]);
  // Bulk triage: select multiple events, then bookmark/unbookmark them at once.
  const [selectMode, setSelectMode] = useState(false);
  const [selected, setSelected] = useState<Set<number>>(new Set());

  // Fetch the attribute-facet catalog once (static; drives the filter chips).
  useEffect(() => {
    let alive = true;
    api.attributes().then(
      (c) => alive && setAttrCatalog(c),
      () => {}, // non-fatal: the chip row just won't render
    );
    return () => {
      alive = false;
    };
  }, []);

  const runSearch = async () => {
    // A text search and a facet chip are mutually exclusive result sets.
    setAttrKey(null);
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
    setAttrKey(null);
  };

  // P2.5: rank the crop corpus against a facet's CLIP prompt (best-effort, like
  // the "AI watch" alarm gate). Toggling the active chip clears it. Mirrors
  // runSearch/runImageSearch: null (not []) on failure falls back to the list.
  const runAttrSearch = async (key: string) => {
    if (attrKey === key) {
      setAttrKey(null);
      setSearchResults(null);
      return;
    }
    if (attrCatalog && !attrCatalog.available) {
      toast.error("Attribute search needs the smart-search (CLIP) models installed.");
      return;
    }
    setAttrKey(key);
    setQuery("");
    setInterpreted([]);
    setSearching(true);
    try {
      const r = await api.searchByAttr(key, 48);
      if (!r.available) {
        toast.error("Attribute search needs the smart-search (CLIP) models installed.");
        setAttrKey(null);
        setSearchResults(null);
        return;
      }
      setSearchResults(r.results.map((x) => x.event));
    } catch (e) {
      toast.error(`Attribute search failed: ${errMsg(e)}`);
      setAttrKey(null);
      setSearchResults(null);
    } finally {
      setSearching(false);
    }
  };
  const [open, setOpen] = useState<CamEvent | null>(null);
  const [playing, setPlaying] = useState<{ segment: Segment; offset: number } | null>(null);
  const [similar, setSimilar] = useState<{ ev: CamEvent; res: SimilarResult | null } | null>(null);
  // Object-lifecycle ("Track story") view: the seed event whose track is being told.
  const [lifecycleFor, setLifecycleFor] = useState<CamEvent | null>(null);
  // Upload-a-photo appearance search: query is a local object-URL preview.
  const [imgSearch, setImgSearch] = useState<{ url: string; res: SimilarResult | null } | null>(null);
  const imgReq = useRef(0);
  const imgFileRef = useRef<HTMLInputElement>(null);

  // The detail viewer plays the covering recording inline when one exists
  // (Protect-style: open an event → watch it happen). "none" = probed and no
  // recording covers this moment → fall back to the snapshot, and recording-
  // dependent actions disable themselves instead of failing on click.
  const [openClip, setOpenClip] = useState<{ segId: number; offset: number } | "none" | null>(null);
  // Coverage for the viewer's mini-timeline: this camera's segments around the
  // event, so you can scrub for context without leaving the event (and without
  // Protect's ±5-minute cap — any retained moment is reachable).
  const [openSegs, setOpenSegs] = useState<Segment[]>([]);
  const [segmentSecs, setSegmentSecs] = useState(60);
  const clipReq = useRef(0);
  useEffect(() => {
    setOpenClip(null);
    setOpenSegs([]);
    if (!open) return;
    const token = ++clipReq.current;
    api.recordingAt(open.camera_id, open.ts).then(
      (r) => {
        if (token === clipReq.current)
          // Land a few seconds before the event so you see it happen.
          setOpenClip({ segId: r.segment.id, offset: Math.max(0, r.offset_secs - 3) });
      },
      () => {
        if (token === clipReq.current) setOpenClip("none");
      },
    );
    api
      .recordings({ camera_id: open.camera_id, before: open.ts + 1800, limit: 90 })
      .then((s) => {
        if (token === clipReq.current) setOpenSegs(s);
      })
      .catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open?.id]);

  // Scrub within the open event's viewer: resolve the covering segment at ts
  // and swap the inline player to it.
  const seekOpenClip = async (ts: number) => {
    if (!open) return;
    const token = ++clipReq.current;
    try {
      const r = await api.recordingAt(open.camera_id, ts);
      if (token === clipReq.current) setOpenClip({ segId: r.segment.id, offset: r.offset_secs });
    } catch {
      toast.error("No recording covers that moment.");
    }
  };

  // Deep-link from a notification: once events have loaded, open the matching
  // event's detail (best-effort — the event is usually recent and in the list),
  // then clear the request. onFocusHandled clears focusEventId synchronously, so
  // this fires once per click without a guard (and re-clicks work).
  const focusReq = useRef(0);
  useEffect(() => {
    if (!focusEventId || events.length === 0) return;
    const id = focusEventId;
    // Consume the request up front so a poll-driven re-render can't re-enter
    // while the fallback fetch below is in flight.
    onFocusHandled?.();
    let ev = events.find((e) => e.id === id);
    // Not in the loaded window (frame-seeded search can land on an old event) —
    // the navigating surface stashes the full event for exactly this case.
    if (!ev) {
      try {
        const raw = sessionStorage.getItem("cammy-focus-event");
        if (raw) {
          const stashed = JSON.parse(raw) as CamEvent;
          if (stashed.id === id) ev = stashed;
        }
      } catch {
        /* fall through to the fetch below */
      }
    }
    try {
      sessionStorage.removeItem("cammy-focus-event");
    } catch {
      /* ignore */
    }
    if (ev) {
      setOpen(ev);
      return;
    }
    // Fresh tab / pasted link to an old event: fetch it directly. Guarded by a
    // token, NOT effect cleanup — consuming the request above re-runs this
    // effect (the prop changes), and a cleanup-based cancel would discard the
    // very response we're waiting for.
    const token = ++focusReq.current;
    api.event(id).then(
      (fetched) => {
        if (token === focusReq.current) setOpen(fetched);
      },
      () => {
        if (token === focusReq.current)
          toast.info("Couldn't open that event — it may have been removed at retention.");
      },
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focusEventId, events, onFocusHandled]);

  // Protect-style playback shortcuts: space pause, arrows seek (shift =
  // frame-ish steps), f fullscreen, Esc close.
  useEffect(() => {
    if (!playing) return;
    const onKey = (e: KeyboardEvent) => {
      // The clip player can stack over the event viewer (which has its own
      // inline video) — control the TOPMOST modal's video, i.e. the last one
      // in DOM order, not the hidden one behind it.
      const vids = document.querySelectorAll<HTMLVideoElement>(".modal-bg video");
      const v = vids[vids.length - 1];
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

  // When the full-recording player stacks over the event viewer, silence the
  // viewer's inline clip behind it — two audio tracks at once is chaos.
  useEffect(() => {
    if (!playing) return;
    document.querySelector<HTMLVideoElement>(".lightbox-video")?.pause();
  }, [playing]);

  const jumpToRecording = async (ev: CamEvent) => {
    try {
      const r = await api.recordingAt(ev.camera_id, ev.ts);
      // Land a few seconds before the event so you see it happen.
      setPlaying({ segment: r.segment, offset: Math.max(0, r.offset_secs - 3) });
    } catch {
      toast.error("No recording covers this event.");
    }
  };

  // Download the event clip via fetch so a missing recording (normal for a
  // detect-only camera or pruned footage) shows a friendly message instead of
  // navigating the whole tab to a raw JSON error page.
  const downloadClip = async (ev: CamEvent) => {
    try {
      const r = await fetch(`/api/events/${ev.id}/clip`);
      if (!r.ok) throw new Error();
      const blob = await r.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `event-${ev.id}.mp4`;
      a.click();
      URL.revokeObjectURL(url);
    } catch {
      toast.error("No recording covers this event, so there's no clip to save.");
    }
  };

  // Export an evidence-grade copy: a watermarked clip whose SHA-256 is logged in
  // the audit trail, so the file can be proven unaltered later.
  const exportEvidence = async (ev: CamEvent) => {
    try {
      toast.info("Preparing your evidence copy…");
      const r = await fetch(`/api/events/${ev.id}/evidence.mp4`);
      if (!r.ok) throw new Error();
      const sha = r.headers.get("x-cammy-sha256") ?? "";
      const blob = await r.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `evidence-${ev.id}.mp4`;
      a.click();
      URL.revokeObjectURL(url);
      toast.success(
        sha
          ? "Evidence saved. A tamper-evident fingerprint was recorded so the clip can be verified later."
          : "Evidence saved",
      );
    } catch {
      toast.error("Couldn't create the evidence copy. No recording covers this event.");
    }
  };

  // Export a self-verifying evidence *bundle*: a ZIP with the watermarked clip, a
  // signed manifest pinning its SHA-256, and instructions to re-check it offline
  // (`zoomy --verify bundle.zip`). The recipient needs no login and no Cammy.
  const exportBundle = async (ev: CamEvent) => {
    try {
      toast.info("Preparing your evidence bundle…");
      const r = await fetch(`/api/events/${ev.id}/evidence.zip`);
      if (!r.ok) throw new Error();
      const sha = r.headers.get("x-cammy-sha256") ?? "";
      const blob = await r.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `evidence-${ev.id}.zip`;
      a.click();
      URL.revokeObjectURL(url);
      toast.success(
        sha
          ? "Evidence bundle saved. It includes instructions for proving the clip is genuine."
          : "Evidence bundle saved",
      );
    } catch {
      toast.error("Couldn't create the evidence bundle. No recording covers this event.");
    }
  };

  // Mint a no-login, auto-expiring link to this event's clip and copy it.
  const shareClip = async (ev: CamEvent) => {
    try {
      const r = await api.shareEvent(ev.id, 24);
      const url = `${location.origin}${r.path}`;
      const copied = await navigator.clipboard
        ?.writeText(url)
        .then(() => true)
        .catch(() => false);
      toast.success(
        copied
          ? "Share link copied. Anyone with it can view this clip for 24 hours."
          : `Share link (valid 24h): ${url}`,
      );
    } catch {
      toast.error("Couldn't create a share link.");
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
  // P2.8b feedback learning: thumbs-down an alert. The server stores this
  // object's crop embedding so CLIP-similar FUTURE alerts on the same camera are
  // quieted — honest v0: only AI-watch (prompt/attribute) and AI-verified (VLM)
  // rules are filtered, not plain object rules.
  const suppressAlert = async (ev: CamEvent) => {
    try {
      const r = await api.eventFeedback(ev.id);
      if (r.ok) {
        toast.success(
          `Similar AI-watch/AI-verified alerts on ${ev.camera} will be quieted`
        );
      } else {
        toast.error(
          "Can't learn from this one — it has no object crop to compare (needs the smart-search models)"
        );
      }
    } catch (e) {
      toast.error(`Couldn't send feedback: ${e}`);
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

  const editTags = async (ev: CamEvent) => {
    const raw = await dialog.prompt({
      title: (ev.tags ?? []).length ? "Edit tags" : "Add tags",
      label: "Tags for this event (comma-separated, up to 8)",
      defaultValue: (ev.tags ?? []).join(", "),
      placeholder: "e.g. insurance, wildlife",
      maxLength: 200,
    });
    if (raw === null) return; // cancelled
    const tags = raw.split(",").map((t) => t.trim()).filter(Boolean);
    try {
      const res = await api.setEventTags(ev.id, tags);
      setEvents((list) => list.map((e) => (e.id === ev.id ? { ...e, tags: res.tags } : e)));
      toast.success(res.tags.length ? "Tags saved" : "Tags cleared");
    } catch (e) {
      toast.error(`Couldn't save tags: ${e}`);
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
        tag: tagFilter || undefined,
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
    if (tagFilter) p.set("tag", tagFilter);
    return `/api/events/export.csv?${p}`;
  };

  useEffect(() => {
    api
      .settings()
      .then((s) => {
        setAlertLabels(s.alert_labels ?? ["person"]);
        setPlateDeny(s.plate_denylist ?? []);
        setPlateAllow(s.plate_allowlist ?? []);
        setSegmentSecs(s.segment_seconds);
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

  // Pause the 5s background poll while the tab is hidden or a modal / multi-select
  // is open, so we don't refetch ~200 rows behind the user's back or yank the grid
  // out from under an open dialog. (A filter change still forces an immediate
  // reload via this effect's deps.)
  const pollBusy = useRef(false);
  pollBusy.current = !!open || !!playing || !!similar || !!imgSearch || !!lifecycleFor || selectMode;
  useEffect(() => {
    load();
    const t = setInterval(() => {
      if (document.hidden || pollBusy.current) return;
      load();
    }, 5000); // events appear as they happen
    return () => clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cameraId, label, fromTime, toTime, flaggedOnly, tagFilter]);

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
  if (highOnly) shown = shown.filter((e) => (e.severity ?? 2) >= 3);
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

  // Detail-viewer navigation through the list as currently filtered/ordered
  // (newest first): ← steps to the newer event, → to the older one.
  const openIdx = open ? list.findIndex(({ ev }) => ev.id === open.id) : -1;
  const openPrev = openIdx > 0 ? list[openIdx - 1].ev : null;
  const openNext = openIdx >= 0 && openIdx < list.length - 1 ? list[openIdx + 1].ev : null;
  useEffect(() => {
    // Only while the detail viewer is the top surface — the clip-playback modal
    // keeps its own arrow-seek shortcuts, and search modals keep their focus.
    if (!open || playing || similar || imgSearch || lifecycleFor) return;
    const onKey = (e: KeyboardEvent) => {
      // Don't double-act: a focused <video> uses arrows to seek, and the
      // mini-timeline's own arrow scrubbing calls preventDefault().
      if (e.defaultPrevented) return;
      const t = e.target as HTMLElement | null;
      if (t && ["VIDEO", "INPUT", "TEXTAREA", "SELECT"].includes(t.tagName)) return;
      if (e.key === "ArrowLeft" && openPrev) setOpen(openPrev);
      else if (e.key === "ArrowRight" && openNext) setOpen(openNext);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, playing, similar, imgSearch, lifecycleFor, openPrev, openNext]);

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
          placeholder='Smart search: what you saw or heard ("person in a dark coat", "someone yelling help")'
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            if (e.target.value.trim() === "") {
              setSearchResults(null);
              setAttrKey(null);
            }
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
          title="Upload a photo to find that person or vehicle across your cameras."
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
        <button
          className={`btn ${review === "all" ? "btn-primary" : "btn-ghost"}`}
          onClick={() => setReview("all")}
          aria-pressed={review === "all"}
        >
          All
        </button>
        <button
          className={`btn ${review === "alerts" ? "btn-primary" : "btn-ghost"}`}
          onClick={() => setReview("alerts")}
          aria-pressed={review === "alerts"}
          title={`alert labels: ${alertLabels.join(", ")}`}
        >
          <IconBell size={15} /> Alerts
        </button>
        <button
          className={`btn ${flaggedOnly ? "btn-primary" : "btn-ghost"}`}
          onClick={() => setFlaggedOnly((v) => !v)}
          aria-pressed={flaggedOnly}
          title="Show only saved events. Saved events are never deleted automatically."
        >
          <IconStar size={15} filled={flaggedOnly} /> Saved
        </button>
        <button
          className={`btn ${highOnly ? "btn-primary" : "btn-ghost"}`}
          onClick={() => setHighOnly((v) => !v)}
          aria-pressed={highOnly}
          title="Show only events worth a look, like strangers, safety events, or someone lingering. Matches the notification filter in Settings."
        >
          <IconAlert size={15} /> Important only
        </button>
        {tagFilter && (
          <button
            className="btn btn-primary"
            onClick={() => setTagFilter(null)}
            title="Showing only this tag. Click to clear."
            aria-label={`Clear tag filter ${tagFilter}`}
          >
            <IconTag size={13} /> {tagFilter} <IconX size={12} />
          </button>
        )}
        <button
          className={`btn ${grouped ? "btn-primary" : "btn-ghost"}`}
          onClick={() => setGrouped((v) => !v)}
          aria-pressed={grouped}
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
        <select
          value={cameraId}
          onChange={(e) => setCameraId(e.target.value === "" ? "" : Number(e.target.value))}
          aria-label="Filter by camera"
        >
          <option value="">all cameras</option>
          {cameras.map((c) => (
            <option key={c.id} value={c.id}>
              {c.name}
            </option>
          ))}
        </select>
        <select value={label} onChange={(e) => setLabel(e.target.value)} aria-label="Filter by object">
          <option value="">all objects</option>
          {labels.map((l) => (
            <option key={l} value={l}>
              {prettyLabel(l)}
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
        <span className="muted count" style={{ marginLeft: "auto" }}>{shown.length} events · auto-refreshing</span>
        <a
          className="btn btn-ghost"
          href={exportUrl()}
          title="Download events as CSV (camera, object, time, saved and tag filters apply)"
        >
          <IconDownload size={15} /> Export CSV
        </a>
      </div>

      {/* Power-user filters tucked behind a disclosure so the everyday triage row
          stays scannable; opens itself when one of them becomes active, but a
          user toggle always wins (never force-closed — see moreFilters above). */}
      <details
        className="adv"
        open={moreFilters}
        onToggle={(e) => setMoreFilters(e.currentTarget.open)}
      >
        <summary>
          More filters: hand signal, zone, time range, plate
          {anyHiddenFilter && (
            <span className="badge accent" style={{ marginLeft: 8 }}>
              {hiddenFilters} active
            </span>
          )}
        </summary>
        <div className="row" style={{ marginTop: 8, marginBottom: 16 }}>
          {gestures.length > 0 && (
            <select value={gestureFilter} onChange={(e) => setGestureFilter(e.target.value)} aria-label="Filter by hand signal">
              <option value="">any signal</option>
              {gestures.map((g) => (
                <option key={g} value={g}>
                  {prettyGesture(g)}
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
          <label className="field">
            from
            <input type="datetime-local" value={fromTime} onChange={(e) => setFromTime(e.target.value)} />
          </label>
          <label className="field">
            to
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
              {prettyLabel(l)} ({n})
            </TogglePill>
          ))}
        </div>
      )}

      {/* P2.5 attribute facets: pick "red car" / "person in blue" to CLIP-rank
          the crop corpus. Best-effort semantic match (same framing as the AI
          watch alarm gate) — collapsed by default to keep the header calm. */}
      {attrCatalog && attrCatalog.groups.length > 0 && (
        <details className="adv" style={{ marginBottom: 12 }}>
          <summary>Attributes — find by appearance (red car, person in blue…)</summary>
          <div style={{ marginTop: 8 }}>
            {!attrCatalog.available && (
              <div className="muted" style={{ fontSize: "var(--text-sm)", marginBottom: 8 }}>
                Needs the smart-search (CLIP) models installed to match appearances.
              </div>
            )}
            {attrCatalog.groups.map((g) => (
              <div key={g.group} className="row" style={{ marginBottom: 6, flexWrap: "wrap" }}>
                <span className="muted" style={{ minWidth: 150 }}>{g.label}</span>
                {g.attrs.map((a) => (
                  <TogglePill
                    key={a.key}
                    on={attrKey === a.key}
                    ariaLabel={`Find ${a.label} (${a.prompt})`}
                    onClick={() => runAttrSearch(a.key)}
                  >
                    {a.label}
                  </TogglePill>
                ))}
              </div>
            ))}
          </div>
        </details>
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
            hint="Events appear here when a camera with detection turned on sees motion and the AI recognizes an object (a person, vehicle, package, and more). Detections only fire on movement, so try walking in front of a camera."
            action={
              <div style={{ display: "flex", flexDirection: "column", gap: 8, alignItems: "center" }}>
                <a className="btn btn-ghost" href="#/cameras">Is a camera online with “detect” on? → Cameras</a>
                <a className="btn btn-ghost" href="#/settings">Is the detector model installed? → Settings › Models</a>
              </div>
            }
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
              {!selectMode && (
                <button
                  className={`ev-save ${ev.flagged ? "on" : ""}`}
                  aria-pressed={ev.flagged}
                  aria-label={ev.flagged ? `Unsave this ${ev.label} event` : `Save this ${ev.label} event`}
                  title={
                    ev.flagged
                      ? "Saved. This event is never deleted automatically. Click to unsave."
                      : "Save this event so it's never deleted automatically"
                  }
                  onClick={(e) => {
                    e.stopPropagation();
                    toggleFlag(ev);
                  }}
                >
                  <IconStar size={15} filled={ev.flagged} />
                </button>
              )}
              {ev.snapshot ? (
                <img src={`/api/snapshots/${ev.snapshot}?w=400`} alt={`${ev.label} on ${ev.camera}`} loading="lazy" decoding="async" />
              ) : (
                <div
                  style={{
                    aspectRatio: "4 / 3", background: "var(--bg-sunken)",
                    display: "grid", placeItems: "center", color: "var(--text-muted)",
                    fontSize: "var(--text-sm)", gap: 6,
                  }}
                >
                  <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                    <IconVideo size={16} /> No snapshot for this event
                  </span>
                </div>
              )}
              <div className="meta">
                <div className="ev-head">
                  <b className="ev-label">{prettyLabel(ev.label)}</b>
                  {!isCameraSide(ev.label) && (
                    <span className="ev-score score">{(ev.score * 100).toFixed(0)}%</span>
                  )}
                  <span className="muted">{ev.camera}</span>
                  {(ev.severity ?? 2) >= 4 ? (
                    <span className="badge danger" title="Critical: a possible safety or security concern.">
                      <IconAlert size={12} /> critical
                    </span>
                  ) : (ev.severity ?? 2) === 3 ? (
                    <span className="badge warn" title="High: likely worth a look.">
                      <IconAlert size={12} /> high
                    </span>
                  ) : null}
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
                          <span className="badge danger"><IconAlert size={12} /> on watch list</span>
                        )}
                        {plateClass(ev.plate) === "allow" && (
                          <span className="badge ok"><IconCheck size={12} /> known</span>
                        )}
                      </>
                    )}
                    {ev.gesture && <span className="badge accent"><IconHand size={13} /> {prettyGesture(ev.gesture)}</span>}
                    {ev.zone && (
                      <span className="badge" title={ev.zone}>
                        <IconZone size={13} /> {prettyZone(ev.zone)}
                      </span>
                    )}
                    {ev.gait === "?" ? (
                      <span className="badge warn" title="Seen walking but not recognized">
                        <IconStranger size={13} /> unrecognized walk
                      </span>
                    ) : ev.gait ? (
                      <span className="badge ok" title="Identified by how they walk (gait)">
                        <IconUser size={13} /> {ev.gait} · gait
                      </span>
                    ) : null}
                  </div>
                )}
                {ev.caption && !captionContradicts(ev) && (
                  <div className="ev-caption">“{ev.caption}”</div>
                )}
                {ev.transcript && (
                  <div className="ev-line" title="What was heard (speech to text)">
                    <IconMic size={13} /> <span>“{ev.transcript}”</span>
                  </div>
                )}
                {ev.note && (
                  <div className="ev-line" title="Your note">
                    <IconPencil size={13} /> <span>{ev.note}</span>
                  </div>
                )}
                {(ev.tags ?? []).length > 0 && (
                  <div className="ev-chips">
                    {(ev.tags ?? []).map((t) => (
                      <button
                        key={t}
                        className="badge"
                        title={`Show only events tagged “${t}”`}
                        onClick={(e) => {
                          e.stopPropagation();
                          setTagFilter(t);
                        }}
                      >
                        <IconTag size={11} /> {t}
                      </button>
                    ))}
                  </div>
                )}
                <RelTime ts={ev.ts} className="muted ev-time" />
              </div>
            </div>
            );
          })}
        </div>
      )}

      {open && (() => {
        const noRec = openClip === "none";
        const recTitle = (base: string) =>
          noRec ? "This camera wasn't recording at that moment." : base;
        return (
        <Modal className="lightbox" title={`${prettyLabel(open.label)} · ${open.camera}`} onClose={() => setOpen(null)}>
          <div className="lightbox-media">
            {openClip && openClip !== "none" ? (
              <video
                key={`${open.id}-${openClip.segId}-${openClip.offset}`}
                className="lightbox-video"
                src={`/api/recordings/${openClip.segId}/video`}
                poster={open.snapshot ? `/api/snapshots/${open.snapshot}` : undefined}
                controls
                autoPlay
                playsInline
                onLoadedMetadata={(e) => {
                  const v = e.currentTarget;
                  // Clamp: an event near a segment's end can resolve past it.
                  if (openClip.offset > 0)
                    v.currentTime = Math.min(openClip.offset, Math.max(0, v.duration - 2));
                }}
              />
            ) : open.snapshot ? (
              <img
                className="lightbox-img"
                src={`/api/snapshots/${open.snapshot}`}
                alt={`${open.label} detected at ${open.camera}, ${fmtTime(open.ts)}`}
              />
            ) : (
              <div className="lightbox-img" style={{ display: "grid", placeItems: "center", minHeight: 200 }}>
                <span className="muted" style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                  <IconVideo size={16} /> No snapshot for this event
                </span>
              </div>
            )}
            {openPrev && (
              <button
                className="lightbox-nav prev"
                onClick={() => setOpen(openPrev)}
                aria-label="Newer event"
                title="Newer event (←)"
              >
                <IconChevronLeft size={20} />
              </button>
            )}
            {openNext && (
              <button
                className="lightbox-nav next"
                onClick={() => setOpen(openNext)}
                aria-label="Older event"
                title="Older event (→)"
              >
                <IconChevronRight size={20} />
              </button>
            )}
          </div>
          {openSegs.length > 0 && (
            <div className="lightbox-tl">
              <Timeline
                windowSecs={3600}
                segmentSecs={segmentSecs}
                segments={openSegs}
                events={events.filter((e) => e.camera_id === open.camera_id)}
                onSeek={seekOpenClip}
                nowTs={open.ts + 900}
              />
            </div>
          )}
          <div className="lightbox-meta">
            {!isCameraSide(open.label) && (
              <span className="badge accent score">{(open.score * 100).toFixed(0)}%</span>
            )}
            {open.face === "?" ? (
              <span className="badge warn"><IconStranger size={13} /> stranger</span>
            ) : open.face ? (
              <span className="badge ok"><IconUser size={13} /> {open.face}</span>
            ) : null}
            {open.plate && <span className="badge warn"><IconCar size={13} /> {open.plate}</span>}
            {open.gesture && <span className="badge accent"><IconHand size={13} /> {prettyGesture(open.gesture)}</span>}
            {open.zone && (
              <span className="badge" title={open.zone}><IconZone size={13} /> {prettyZone(open.zone)}</span>
            )}
            {noRec && (
              <span className="muted" style={{ fontSize: "var(--text-sm)" }}>
                Snapshot only — no recording covers this moment.
              </span>
            )}
            <span className="muted clock" style={{ marginLeft: "auto" }}>
              {openIdx >= 0 && <span className="tnum">{openIdx + 1} of {list.length} · </span>}
              {fmtTime(open.ts)}
            </span>
          </div>
          {open.transcript && <p className="ev-line" style={{ margin: "8px 16px 0" }}><IconMic size={14} /> <span>“{open.transcript}”</span></p>}
          {open.note && <p className="ev-line" style={{ margin: "8px 16px 0" }}><IconPencil size={14} /> <span>{open.note}</span></p>}
          <div className="lightbox-actions">
            <button
              className={`btn ev-act ${open.flagged ? "btn-primary" : "btn-ghost"}`}
              aria-pressed={open.flagged}
              title={
                open.flagged
                  ? "Saved. This event is never deleted automatically. Click to unsave."
                  : "Save this event so it's never deleted automatically"
              }
              onClick={() => toggleFlag(open)}
            >
              <IconStar size={14} filled={open.flagged} /> {open.flagged ? "Saved" : "Save"}
            </button>
            <button className="btn btn-ghost ev-act" onClick={() => editNote(open)}>
              <IconPencil size={14} /> {open.note ? "Edit note" : "Note"}
            </button>
            <button className="btn btn-ghost ev-act" title={(open.tags ?? []).length ? "Edit tags" : "Add tags"} onClick={() => editTags(open)}>
              <IconTag size={14} /> Tags
            </button>
            {open.snapshot && (
              <button
                className="btn btn-ghost ev-act"
                title="Find this person/vehicle on other cameras (appearance search)"
                onClick={() => findSimilar(open)}
              >
                <IconUser size={14} /> Similar
              </button>
            )}
            {open.track_id != null && (
              <button
                className="btn btn-ghost ev-act"
                title="Follow this object's path — entered a zone, loitered, crossed a line"
                onClick={() => setLifecycleFor(open)}
              >
                <IconRadar size={14} /> Track
              </button>
            )}
            <button
              className="btn btn-ghost ev-act"
              title="Not a real alert? Quiet future look-alikes on this camera. Note: this only quiets AI-watch and AI-verified rules — plain object rules aren't filtered yet."
              onClick={() => suppressAlert(open)}
            >
              <IconThumbDown size={14} /> Not this
            </button>
            <span className="lightbox-sep" aria-hidden="true" />
            <button
              className="btn btn-ghost ev-act"
              disabled={noRec}
              title={recTitle("Open the full recording around this moment")}
              onClick={() => jumpToRecording(open)}
            >
              <IconPlay size={13} /> Full recording
            </button>
            <button
              className="btn btn-ghost ev-act"
              disabled={noRec}
              title={recTitle("Download a short clip")}
              onClick={() => downloadClip(open)}
            >
              <IconDownload size={14} /> Clip
            </button>
            <button
              className="btn btn-ghost ev-act"
              disabled={noRec}
              title={recTitle("Copy a no-login link to this clip (expires in 24h)")}
              onClick={() => shareClip(open)}
            >
              <IconLink size={14} /> Share
            </button>
            <button
              className="btn btn-ghost ev-act"
              disabled={noRec}
              title={recTitle("Save a watermarked copy. A tamper-evident fingerprint is recorded so the clip can be verified later.")}
              onClick={() => exportEvidence(open)}
            >
              <IconShield size={14} /> Evidence
            </button>
            <button
              className="btn btn-ghost ev-act"
              disabled={noRec}
              title={recTitle("Save a tamper-evident copy (watermarked clip plus a signed manifest) that anyone can verify offline. Instructions are included in the file.")}
              onClick={() => exportBundle(open)}
            >
              <IconShield size={14} /> Bundle
            </button>
            {(openPrev || openNext) && (
              <span className="muted lightbox-hint" aria-hidden="true">
                <kbd>←</kbd> <kbd>→</kbd> move between events
              </span>
            )}
          </div>
        </Modal>
        );
      })()}

      {similar && (
        <Modal
          title={`Similar to this ${prettyLabel(similar.ev.label)} · ${similar.ev.camera}`}
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
                  This event can't be matched yet. Similar search needs the smart search models
                  installed (Settings, Models &amp; capabilities) and works on people and vehicles.
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

      {lifecycleFor && (
        <LifecycleModal
          seed={lifecycleFor}
          onClose={() => setLifecycleFor(null)}
          onOpenEvent={(ev) => {
            setLifecycleFor(null);
            setOpen(ev);
          }}
        />
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
                  Photo search needs the smart search models installed (Settings, Models &amp;
                  capabilities). It compares your photo against people and vehicles your cameras
                  have seen.
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
