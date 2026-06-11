import { useEffect, useState } from "react";
import { api, CamEvent, Camera, fmtTime, Segment } from "../api";

export default function Events({ cameras }: { cameras: Camera[] }) {
  const [events, setEvents] = useState<CamEvent[]>([]);
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

  const runSearch = async () => {
    const q = query.trim();
    if (!q) {
      setSearchResults(null);
      return;
    }
    setSearching(true);
    try {
      const r = await api.search(q, 24);
      setSearchResults(r.results.map((x) => x.event));
    } catch {
      setSearchResults([]);
    } finally {
      setSearching(false);
    }
  };
  const [open, setOpen] = useState<CamEvent | null>(null);
  const [playing, setPlaying] = useState<{ segment: Segment; offset: number } | null>(null);
  const [noClip, setNoClip] = useState<number | null>(null);

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

  const load = () => {
    const after = fromTime ? Math.floor(new Date(fromTime).getTime() / 1000) : undefined;
    const before = toTime ? Math.floor(new Date(toTime).getTime() / 1000) : undefined;
    api
      .events({
        camera_id: cameraId === "" ? undefined : cameraId,
        label: label || undefined,
        after,
        before,
        limit: 200,
      })
      .then(setEvents)
      .catch(() => {});
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
  }, [cameraId, label, fromTime, toTime]);

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
        <span>✨</span>
        <input
          type="text"
          placeholder='Smart search — describe what you are looking for ("person in a dark coat", "blue car")'
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            if (e.target.value.trim() === "") setSearchResults(null);
          }}
          onKeyDown={(e) => e.key === "Enter" && runSearch()}
        />
        {searchResults && (
          <button
            className="ghost"
            onClick={() => {
              setQuery("");
              setSearchResults(null);
            }}
          >
            clear
          </button>
        )}
        <button className="primary" onClick={runSearch} disabled={searching || !query.trim()}>
          {searching ? "searching…" : "Search"}
        </button>
      </div>
      <div className="row" style={{ marginBottom: 16 }}>
        <button className={review === "all" ? "primary" : "ghost"} onClick={() => setReview("all")}>
          All
        </button>
        <button
          className={review === "alerts" ? "primary" : "ghost"}
          onClick={() => setReview("alerts")}
          title={`alert labels: ${alertLabels.join(", ")}`}
        >
          🔔 Alerts
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
        <select value={faceFilter} onChange={(e) => setFaceFilter(e.target.value)}>
          <option value="">anyone</option>
          {faces.map((f) => (
            <option key={f} value={f}>
              👤 {f}
            </option>
          ))}
        </select>
        {gestures.length > 0 && (
          <select value={gestureFilter} onChange={(e) => setGestureFilter(e.target.value)}>
            <option value="">any signal</option>
            {gestures.map((g) => (
              <option key={g} value={g}>
                ✋ {g}
              </option>
            ))}
          </select>
        )}
        {zones.length > 0 && (
          <select value={zoneFilter} onChange={(e) => setZoneFilter(e.target.value)}>
            <option value="">any zone</option>
            {zones.map((z) => (
              <option key={z} value={z}>
                ▱ {z}
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
            clear time
          </button>
        )}
        <input
          type="text"
          placeholder="plate…"
          style={{ width: 110 }}
          value={plateFilter}
          onChange={(e) => setPlateFilter(e.target.value)}
        />
        <span className="muted">{shown.length} events · auto-refreshing</span>
      </div>

      {topLabels.length > 0 && !searchResults && (
        <div className="row" style={{ marginBottom: 12, flexWrap: "wrap" }}>
          <span className="muted">Explore:</span>
          <span
            className={`pill toggle ${label === "" ? "on" : ""}`}
            onClick={() => setLabel("")}
          >
            all ({exploreBase.length})
          </span>
          {topLabels.map(([l, n]) => (
            <span
              key={l}
              className={`pill toggle ${label === l ? "on" : ""}`}
              onClick={() => setLabel(label === l ? "" : l)}
            >
              {l} ({n})
            </span>
          ))}
        </div>
      )}

      {shown.length === 0 ? (
        <div className="empty">
          No events yet. They appear when a detect-enabled camera sees motion and the AI
          recognizes an object.
        </div>
      ) : (
        <div className="event-grid">
          {shown.map((ev) => (
            <div className="event-card" key={ev.id} onClick={() => setOpen(ev)}>
              {ev.snapshot ? (
                <img src={`/api/snapshots/${ev.snapshot}?w=400`} alt={ev.label} loading="lazy" />
              ) : (
                <div style={{ aspectRatio: "4 / 3", background: "#000" }} />
              )}
              <div className="meta">
                <b>{ev.label}</b> {(ev.score * 100).toFixed(0)}% · {ev.camera}
                {ev.face && <span style={{ color: "var(--ok)" }}> · 👤 {ev.face}</span>}
                {ev.plate && (
                  <span style={{ color: "var(--warn)" }}>
                    {" "}· 🚗 {ev.plate}
                    {plateClass(ev.plate) === "deny" && (
                      <span style={{ color: "var(--danger, #e5484d)", fontWeight: 700 }}> ⚠ of interest</span>
                    )}
                    {plateClass(ev.plate) === "allow" && <span style={{ color: "var(--ok)" }}> ✓ known</span>}
                  </span>
                )}
                {ev.gesture && <span style={{ color: "var(--accent, #4f8cff)" }}> · ✋ {ev.gesture}</span>}
                {ev.zone && <span className="muted"> · ▱ {ev.zone}</span>}
                {ev.caption && (
                  <div style={{ marginTop: 4, fontStyle: "italic", fontSize: "0.85rem" }}>
                    “{ev.caption}”
                  </div>
                )}
                <div className="muted">{fmtTime(ev.ts)}</div>
                <div className="row" style={{ marginTop: 8 }}>
                  <button
                    className="ghost"
                    onClick={(e) => {
                      e.stopPropagation();
                      jumpToRecording(ev);
                    }}
                  >
                    {noClip === ev.id ? "no recording" : "▶ view recording"}
                  </button>
                  <a
                    className="ghost"
                    style={{ padding: "8px 12px", borderRadius: 8, border: "1px solid var(--border)", textDecoration: "none", color: "var(--text)", fontSize: "0.9rem" }}
                    href={`/api/events/${ev.id}/clip`}
                    onClick={(e) => e.stopPropagation()}
                  >
                    ⬇ clip
                  </a>
                </div>
              </div>
            </div>
          ))}
        </div>
      )}

      {open && (
        <div className="modal-bg" onClick={() => setOpen(null)}>
          {open.snapshot && <img src={`/api/snapshots/${open.snapshot}`} alt={open.label} />}
        </div>
      )}

      {playing && (
        <div className="modal-bg" onClick={() => setPlaying(null)}>
          <video
            src={`/api/recordings/${playing.segment.id}/video`}
            controls
            autoPlay
            onClick={(e) => e.stopPropagation()}
            onLoadedMetadata={(e) => {
              e.currentTarget.currentTime = playing.offset;
            }}
          />
        </div>
      )}
    </>
  );
}
