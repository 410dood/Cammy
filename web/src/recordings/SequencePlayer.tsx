import { useEffect, useRef, useState } from "react";
import { Segment } from "../api";
import { IconChevronLeft, IconChevronRight, IconX } from "../icons";
import { Modal, useToast } from "../ui";

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
}: {
  queue: Segment[];
  index: number;
  offset: number;
  onClose: () => void;
}) {
  const [pos, setPos] = useState(index);
  const [rate, setRate] = useState(1);
  const [clock, setClock] = useState<number | null>(null);
  const [atEnd, setAtEnd] = useState(false);
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
              src={`/api/recordings/${s.id}/video`}
              controls={active}
              preload="auto"
              muted={!active}
              autoPlay={active}
              style={{ display: active ? "block" : "none" }}
              onLoadedMetadata={(e) => {
                // Clamp: clicking near "now" can resolve into the last closed
                // clip with an offset past its end.
                if (active && segIdx === index && offset > 0) {
                  const v = e.currentTarget;
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
