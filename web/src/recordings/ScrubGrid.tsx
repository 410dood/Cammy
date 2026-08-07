import { useMemo, useState } from "react";
import { Segment } from "../api";

// P2.4 thumbnail scrub: the selected camera's window as a grid of segment
// keyframes — eyeball a whole day in seconds instead of scrubbing a timeline.
// One tile per 15-minute bucket; a multi-segment bucket expands in place to
// its per-minute tiles, and clicking any expanded tile plays that segment.
export default function ScrubGrid({
  segments,
  onPlay,
}: {
  segments: Segment[];
  onPlay: (s: Segment) => void;
}) {
  const [openBuckets, setOpenBuckets] = useState<Set<number>>(new Set());
  const buckets = useMemo(() => {
    const by = new Map<number, Segment[]>();
    for (const s of segments) {
      const b = Math.floor(s.start_ts / 900) * 900;
      const arr = by.get(b) ?? [];
      arr.push(s);
      by.set(b, arr);
    }
    return [...by.entries()]
      .map(([ts, segs]) => ({ ts, segs: segs.sort((a, b) => a.start_ts - b.start_ts) }))
      .sort((a, b) => a.ts - b.ts);
  }, [segments]);

  if (buckets.length === 0) return null;
  const tile = (s: Segment, caption: string, count?: number, onClick?: () => void) => (
    <button
      key={`${s.id}-${caption}`}
      type="button"
      className="scrub-tile"
      onClick={onClick ?? (() => onPlay(s))}
      title={count && count > 1 ? `${count} clips, click to expand` : `Play ${caption}`}
    >
      <img src={`/api/recordings/${s.id}/thumb.jpg`} loading="lazy" alt="" />
      <span className="scrub-cap">
        {caption}
        {count && count > 1 ? <span className="scrub-count">×{count}</span> : null}
      </span>
    </button>
  );
  return (
    <div className="card">
      <div className="scrub-grid">
        {buckets.map((b) => {
          const cap = new Date(b.ts * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
          if (!openBuckets.has(b.ts) && b.segs.length > 1) {
            return tile(b.segs[0], cap, b.segs.length, () =>
              setOpenBuckets((prev) => new Set(prev).add(b.ts))
            );
          }
          return b.segs.map((s) =>
            tile(s, new Date(s.start_ts * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" }))
          );
        })}
      </div>
      <p className="muted" style={{ marginBottom: 0 }}>
        Each thumbnail is one clip. Click a stacked (×N) tile to see the 15 minutes inside, or click any frame to play.
      </p>
    </div>
  );
}
