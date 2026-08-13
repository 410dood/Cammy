import { useEffect, useRef, useState } from "react";
import { api, Segment } from "../api";
import { IconChevronLeft, IconChevronRight, IconX } from "../icons";
import { Modal, useToast } from "../ui";
import { getPlayQuality, setPlayQuality, PlayQuality } from "../playQuality";

// Playback speeds. 16× turns an hour of footage into a ~4-minute skim — a
// client-side time-lapse with no server render (the ffmpeg day time-lapse
// stays for shareable files).
const RATES = [1, 2, 4, 8, 16];

/// Plays a queue of clips as one continuous recording: the next clip preloads
/// in a hidden <video> while the current one plays, and swaps in on end, so
/// minute boundaries pass without a stall. Prev/next, a ticking wall clock,
/// and a speed control ride in a bar under the video.
export default function SequencePlayer({
  queue,
  index,
  offset,
  onClose,
  subAvailable = false,
}: {
  queue: Segment[];
  index: number;
  offset: number;
  onClose: () => void;
  /** The playing camera records a low-res sub stream (P3.7) — offer HD/SD. */
  subAvailable?: boolean;
}) {
  const [pos, setPos] = useState(index);
  const [rate, setRate] = useState(1);
  const [clock, setClock] = useState<number | null>(null);
  const [atEnd, setAtEnd] = useState(false);
  // Deferred P3.7 half: SD plays the covering SUB segment of each queued main
  // clip. The queue itself stays main (it is the source of truth for what to
  // play); each main id resolves its sub sibling once, cached, with an honest
  // silent fallback to main when no sub covers that minute.
  const [quality, setQuality] = useState<PlayQuality>(() =>
    subAvailable ? getPlayQuality() : "hd",
  );
  const subIds = useRef(new Map<number, number | null>());
  const [, subResolved] = useState(0);
  const srcFor = (s: Segment): string => {
    if (quality !== "sd" || !subAvailable) return `/api/recordings/${s.id}/video`;
    const cached = subIds.current.get(s.id);
    if (cached != null) return `/api/recordings/${cached}/video`;
    if (cached === null) return `/api/recordings/${s.id}/video`; // no sub — main
    // Unknown yet: kick off the lookup, play main meanwhile, swap on arrival.
    subIds.current.set(s.id, null);
    api
      .recordingAt(s.camera_id, s.start_ts + 1, "sub")
      .then((r) => {
        subIds.current.set(s.id, r.segment.id);
        // The visible clip is about to swap src to the sub copy — resume it
        // where it was instead of restarting the minute.
        const visible = vids.current.find((v) => v && v.style.display !== "none");
        if (visible) resumeAt.current = visible.currentTime;
        subResolved((n) => n + 1);
      })
      .catch(() => {
        subIds.current.set(s.id, null); // stays main, no re-asking
      });
    return `/api/recordings/${s.id}/video`;
  };
  // Switching quality reloads the current clip — keep the in-clip position.
  const resumeAt = useRef<number | null>(null);
  const switchQuality = (q: PlayQuality) => {
    if (q === quality) return;
    resumeAt.current = vids.current[pos % 2]?.currentTime ?? null;
    setPlayQuality(q);
    setQuality(q);
  };
  // Two persistent <video> slots, addressed by position parity: one is
  // visible and playing, the other buffers the next clip. Advancing flips
  // which slot is live — the incoming clip is already loaded.
  const vids = useRef<(HTMLVideoElement | null)[]>([null, null]);
  const toast = useToast();

  const seg = queue[pos];

  const go = (next: number) => {
    if (next < 0 || next >= queue.length) {
      if (next >= queue.length) setAtEnd(true);
      return;
    }
    vids.current[pos % 2]?.pause(); // never two audio tracks at once
    setAtEnd(false);
    setClock(queue[next].start_ts);
    setPos(next);
  };

  // Promote the active slot whenever position or speed changes.
  useEffect(() => {
    const v = vids.current[pos % 2];
    if (!v) return;
    v.playbackRate = rate;
    v.play().catch(() => {
      if (v.error) {
        // Clip vanished (retention pruned it mid-session) — skip ahead.
        toast.info("That clip isn't available anymore — skipping ahead.");
        setPos((p) => (p + 1 < queue.length ? p + 1 : p));
      } else {
        // Autoplay policy refused audible playback — retry muted.
        v.muted = true;
        v.play().catch(() => {});
      }
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pos, rate]);

  return (
    <Modal bare onClose={onClose}>
      <div className="seq-player">
        {[0, 1].map((par) => {
          const segIdx = pos % 2 === par ? pos : pos + 1;
          const s = queue[segIdx];
          if (!s)
            return (
              <video key={par} ref={(el) => (vids.current[par] = el)} style={{ display: "none" }} />
            );
          const active = segIdx === pos;
          return (
            <video
              key={par}
              ref={(el) => (vids.current[par] = el)}
              src={srcFor(s)}
              controls={active}
              preload="auto"
              muted={!active}
              autoPlay={active}
              style={{ display: active ? "block" : "none" }}
              onLoadedMetadata={(e) => {
                const v = e.currentTarget;
                // A quality switch reloaded this clip — resume where it was.
                if (active && resumeAt.current != null) {
                  v.currentTime = Math.min(resumeAt.current, Math.max(0, v.duration - 0.5));
                  resumeAt.current = null;
                  return;
                }
                // Clamp: clicking near "now" can resolve into the last closed
                // clip with an offset past its end.
                if (active && segIdx === index && offset > 0) {
                  v.currentTime = Math.min(offset, Math.max(0, v.duration - 2));
                }
              }}
              onTimeUpdate={(e) => {
                if (active) setClock(Math.floor(s.start_ts + e.currentTarget.currentTime));
              }}
              onEnded={active ? () => go(pos + 1) : undefined}
              onError={
                active
                  ? () => {
                      toast.info("That clip isn't available — skipping ahead.");
                      go(pos + 1);
                    }
                  : undefined
              }
            />
          );
        })}
        <div className="seq-bar">
          <b>{seg.camera}</b>
          <span className="muted clock">
            {new Date((clock ?? seg.start_ts) * 1000).toLocaleTimeString()}
          </span>
          <span className="muted tnum">
            clip {pos + 1}/{queue.length}
          </span>
          {atEnd && <span className="muted">end of footage</span>}
          <span style={{ flex: 1 }} />
          <button
            type="button"
            className="btn btn-ghost"
            disabled={pos === 0}
            aria-label="Previous clip"
            onClick={() => go(pos - 1)}
          >
            <IconChevronLeft size={14} />
          </button>
          <button
            type="button"
            className="btn btn-ghost"
            disabled={pos + 1 >= queue.length}
            aria-label="Next clip"
            onClick={() => go(pos + 1)}
          >
            <IconChevronRight size={14} />
          </button>
          {subAvailable && (
            <span
              className="row"
              style={{ gap: 4 }}
              title="HD plays the full-resolution recording; SD the lighter low-res copy this camera also records (kinder to a slow connection)."
            >
              {(["hd", "sd"] as const).map((q) => (
                <button
                  key={q}
                  type="button"
                  className={`btn btn-ghost ${quality === q ? "active" : ""}`}
                  aria-pressed={quality === q}
                  onClick={() => switchQuality(q)}
                >
                  {q.toUpperCase()}
                </button>
              ))}
            </span>
          )}
          <label className="field" title="Playback speed — high speeds skim like a time-lapse">
            speed
            <select
              aria-label="Playback speed"
              value={rate}
              onChange={(e) => setRate(Number(e.target.value))}
            >
              {RATES.map((r) => (
                <option key={r} value={r}>
                  {r}×
                </option>
              ))}
            </select>
          </label>
          <button type="button" className="btn btn-ghost" aria-label="Close player" onClick={onClose}>
            <IconX size={14} />
          </button>
        </div>
      </div>
    </Modal>
  );
}
