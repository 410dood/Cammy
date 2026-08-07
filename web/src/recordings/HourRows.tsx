import { fmtBytes, fmtTime, Segment } from "../api";
import { IconChevronDown, IconChevronRight, IconFilm, IconPlay } from "../icons";
import { prettyLabel } from "../labels";
import { HourGroup } from "./buckets";

/// One camera-bucket of footage: a summary row that expands into its clips.
export default function HourRows({
  group,
  open,
  hourLabel,
  onToggle,
  onPlay,
  onPlayAll,
}: {
  group: HourGroup;
  open: boolean;
  hourLabel: string;
  onToggle: () => void;
  onPlay: (s: Segment) => void;
  onPlayAll: () => void;
}) {
  return (
    <>
      <tr>
        <td><b>{group.camera}</b></td>
        <td>
          <button
            type="button"
            className="btn btn-ghost ev-act"
            style={{ marginLeft: -8 }}
            aria-expanded={open}
            onClick={onToggle}
          >
            {open ? <IconChevronDown size={13} /> : <IconChevronRight size={13} />} {hourLabel}
            <span className="muted"> · {group.segs.length} clips</span>
            {/* What is IN the footage. Without this every row read the same and
                the only way to triage an hour was to watch it. `null` counts
                mean "outside the events we hold" — render nothing rather than
                imply the hour was quiet. */}
            {group.counts && group.counts.length > 0 && (
              <span className="muted">
                {" · "}
                {group.counts.map(([l, n]) => `${n} ${prettyLabel(l)}`).join(", ")}
              </span>
            )}
          </button>
        </td>
        <td className="muted">{fmtBytes(group.bytes)}</td>
        <td>
          <a
            className="btn btn-ghost ev-act"
            href={`#/live/${group.cameraId}/${group.hourTs}`}
            title="Open this camera's timeline here — scrub freely either side"
          >
            <IconFilm size={13} /> Timeline
          </a>
          <button
            className="btn btn-ghost ev-act"
            title="Play these clips back-to-back, like one recording"
            onClick={onPlayAll}
          >
            <IconPlay size={13} /> Play all
          </button>
        </td>
      </tr>
      {open &&
        group.segs.map((s) => (
          <tr key={s.id}>
            <td></td>
            <td style={{ paddingLeft: 26 }}>{fmtTime(s.start_ts)}</td>
            <td className="muted">{fmtBytes(s.bytes)}</td>
            <td>
              <button className="btn btn-ghost ev-act" onClick={() => onPlay(s)}>
                <IconPlay size={13} /> Play
              </button>
            </td>
          </tr>
        ))}
    </>
  );
}
