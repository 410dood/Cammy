import { useState } from "react";
import { TogglePill, Callout } from "./ui";
import { prettyLabel } from "./labels";

/// Shared homeowner-speak tuning controls (docs/10): grouped object chips
/// instead of comma-separated text, and outcome-labeled sliders instead of raw
/// 0–1 decimals. Used by the per-camera tuning modal and the global Settings
/// detection card so the two can't drift.

/// Curated detector object catalog for the chip picker — the COCO classes a
/// homeowner actually filters on, grouped for scanning. Anything else (a custom
/// COCO label) still round-trips: unknown labels render as removable chips.
export const OBJECT_GROUPS: { name: string; labels: string[] }[] = [
  { name: "People", labels: ["person"] },
  { name: "Vehicles", labels: ["car", "truck", "bus", "motorcycle", "bicycle", "boat"] },
  { name: "Animals", labels: ["dog", "cat", "bird", "bear", "horse", "sheep", "cow"] },
];
const CATALOG = new Set(OBJECT_GROUPS.flatMap((g) => g.labels));

/// docs/11 P2 — every name the system can actually PRODUCE, so a typed chip
/// that can never match ("raccon", "racoon", "delivery guy") is marked instead
/// of silently accepted forever. Two sources:
/// - the detector's 80 COCO class names, exactly as `detector::coco_label`
///   spells them (some contain SPACES — "traffic light", not traffic_light);
/// - Cammy's own synthetic event labels (analytics / camera-side / residential
///   / pose / zone-state), which alert_labels and zone chips legitimately match.
/// Deliberately NOT a block: a custom-exported model could know other classes,
/// so unknowns stay usable — they just say what they are.
const COCO_80 = [
  "person", "bicycle", "car", "motorcycle", "airplane", "bus", "train", "truck",
  "boat", "traffic light", "fire hydrant", "stop sign", "parking meter", "bench",
  "bird", "cat", "dog", "horse", "sheep", "cow", "elephant", "bear", "zebra",
  "giraffe", "backpack", "umbrella", "handbag", "tie", "suitcase", "frisbee",
  "skis", "snowboard", "sports ball", "kite", "baseball bat", "baseball glove",
  "skateboard", "surfboard", "tennis racket", "bottle", "wine glass", "cup",
  "fork", "knife", "spoon", "bowl", "banana", "apple", "sandwich", "orange",
  "broccoli", "carrot", "hot dog", "pizza", "donut", "cake", "chair", "couch",
  "potted plant", "bed", "dining table", "toilet", "tv", "laptop", "mouse",
  "remote", "keyboard", "cell phone", "microwave", "oven", "toaster", "sink",
  "refrigerator", "book", "clock", "vase", "scissors", "teddy bear",
  "hair drier", "toothbrush",
];
const SYNTHETIC_LABELS = [
  "crossing", "wrong_way", "loiter", "occupancy", "zone_enter",
  "camera_person", "camera_vehicle", "camera_motion", "camera_tripwire", "camera_intrusion",
  "child", "child_alone", "fall", "still_water", "standing", "covered_face",
  "zone_open", "zone_closed", "package_delivered", "package_removed",
  "gesture", "tamper", "doorbell",
];
const PRODUCIBLE = new Set<string>([...COCO_80, ...SYNTHETIC_LABELS]);

/** Whether the system can ever emit this label. */
export const labelProducible = (l: string) => PRODUCIBLE.has(l);

/** Canonicalize a typed label toward a spelling that can actually match: the
 *  detector's COCO names contain spaces, Cammy's synthetic names underscores,
 *  and a homeowner should not need to know which is which. */
export function canonicalLabel(typed: string): string {
  const t = typed.trim().toLowerCase();
  if (!t) return "";
  const under = t.replace(/\s+/g, "_");
  const spaced = t.replace(/_+/g, " ");
  if (PRODUCIBLE.has(t)) return t;
  if (PRODUCIBLE.has(spaced)) return spaced;
  if (PRODUCIBLE.has(under)) return under;
  // Unknown either way: keep the snake_case house style for event labels.
  return under;
}

const UNKNOWN_TITLE =
  "The AI doesn't know this name, so it will never match anything — check the spelling. " +
  "(Only kept in case a custom model produces it.)";

/// Chip-based multi-select for "objects to detect".
/// variant "camera": null = inherit the global list (shown live); tapping any
/// chip forks a per-camera list seeded from the global one.
/// variant "global": value is the list itself; an empty list means every
/// supported object (the backend's `labels.is_empty() || match` semantics).
export function ObjectPicker({
  value,
  globalLabels,
  onChange,
  variant = "camera",
}: {
  value: string[] | null;
  globalLabels: string[];
  onChange: (labels: string[] | null) => void;
  variant?: "camera" | "global";
}) {
  const [other, setOther] = useState("");
  const effective = value ?? globalLabels;
  const toggle = (label: string) => {
    const next = effective.includes(label)
      ? effective.filter((l) => l !== label)
      : [...effective, label];
    onChange(next);
  };
  const extras = effective.filter((l) => !CATALOG.has(l));
  const addOther = () => {
    const l = canonicalLabel(other);
    if (l && !effective.includes(l)) onChange([...effective, l]);
    setOther("");
  };
  return (
    <div className="objpick">
      {OBJECT_GROUPS.map((g) => (
        <div key={g.name} className="objpick-group">
          <span className="objpick-name">{g.name}</span>
          <div className="objpick-chips">
            {g.labels.map((l) => (
              <TogglePill key={l} on={effective.includes(l)} ariaLabel={`Detect ${l}`} onClick={() => toggle(l)}>
                {prettyLabel(l)}
              </TogglePill>
            ))}
          </div>
        </div>
      ))}
      {extras.length > 0 && (
        <div className="objpick-group">
          <span className="objpick-name">Other</span>
          <div className="objpick-chips">
            {extras.map((l) => (
              <TogglePill
                key={l}
                on
                ariaLabel={labelProducible(l) ? `Stop detecting ${l}` : `${l} — the AI doesn't know this name`}
                title={labelProducible(l) ? undefined : UNKNOWN_TITLE}
                onClick={() => toggle(l)}
              >
                {prettyLabel(l)}
                {!labelProducible(l) && " ⚠"}
              </TogglePill>
            ))}
          </div>
        </div>
      )}
      <div className="objpick-foot">
        {variant === "camera" ? (
          value == null ? (
            <span className="feat-help">
              Using the global list from Settings — tap a chip to customize for this camera.
            </span>
          ) : (
            <>
              <span className="feat-help">Custom list for this camera.</span>
              <button type="button" className="btn-link" onClick={() => onChange(null)}>
                Reset to global list
              </button>
            </>
          )
        ) : (
          <span className="feat-help">
            The default for every camera — cameras can override it in their own tuning.
          </span>
        )}
        <span className="objpick-other">
          <input
            type="text"
            value={other}
            placeholder="Other object…"
            aria-label="Add another object type"
            onChange={(e) => setOther(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                addOther();
              }
            }}
          />
          <button type="button" className="btn btn-ghost ev-act" onClick={addOther} disabled={!other.trim()}>
            Add
          </button>
        </span>
      </div>
      {effective.length === 0 && (value != null || variant === "global") && (
        <Callout tone="info">
          Nothing selected — every supported object will be detected.
        </Callout>
      )}
    </div>
  );
}

const FLAT_CATALOG = OBJECT_GROUPS.flatMap((g) => g.labels);

/// Compact flat chip row for the smaller "which objects does this apply to"
/// slots (zone cards, tripwires, package objects) where the grouped picker is
/// too tall. Empty selection = applies to everything, and says so.
export function LabelChips({
  value,
  onChange,
  catalog = FLAT_CATALOG,
  emptyHint = "Any object",
}: {
  value: string[];
  onChange: (labels: string[]) => void;
  catalog?: string[];
  emptyHint?: string;
}) {
  const [other, setOther] = useState("");
  const toggle = (label: string) =>
    onChange(value.includes(label) ? value.filter((l) => l !== label) : [...value, label]);
  const extras = value.filter((l) => !catalog.includes(l));
  const addOther = () => {
    const l = canonicalLabel(other);
    if (l && !value.includes(l)) onChange([...value, l]);
    setOther("");
  };
  return (
    <div className="labelchips">
      <div className="objpick-chips">
        {catalog.map((l) => (
          <TogglePill key={l} on={value.includes(l)} ariaLabel={`Applies to ${l}`} onClick={() => toggle(l)}>
            {prettyLabel(l)}
          </TogglePill>
        ))}
        {extras.map((l) => (
          <TogglePill
            key={l}
            on
            ariaLabel={labelProducible(l) ? `Stop applying to ${l}` : `${l} — the AI doesn't know this name`}
            title={labelProducible(l) ? undefined : UNKNOWN_TITLE}
            onClick={() => toggle(l)}
          >
            {prettyLabel(l)}
            {!labelProducible(l) && " ⚠"}
          </TogglePill>
        ))}
        <input
          type="text"
          className="labelchips-other"
          value={other}
          placeholder="Other…"
          aria-label="Add another object type"
          onChange={(e) => setOther(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              addOther();
            }
          }}
          onBlur={addOther}
        />
      </div>
      {value.length === 0 && <span className="feat-help">{emptyHint}</span>}
    </div>
  );
}

const DUR_PRESETS = [0, 10, 30, 120, 600, 3600];
const fmtDur = (s: number, zeroLabel: string) =>
  s === 0
    ? zeroLabel
    : s < 60
      ? `${s} seconds`
      : s < 3600
        ? `${s / 60} minute${s === 60 ? "" : "s"}`
        : `${s / 3600} hour${s === 3600 ? "" : "s"}`;

/// Duration presets instead of a bare "(s)" number box. An off-preset stored
/// value (old installs, power users) surfaces as "Custom…" with the seconds
/// field shown.
export function DurationPicker({
  value,
  onChange,
  zeroLabel = "No limit",
  ariaLabel,
}: {
  value: number;
  onChange: (secs: number) => void;
  zeroLabel?: string;
  ariaLabel: string;
}) {
  const [customMode, setCustomMode] = useState(false);
  const custom = customMode || !DUR_PRESETS.includes(value);
  return (
    <span className="durpick">
      <select
        aria-label={ariaLabel}
        value={custom ? "__custom" : String(value)}
        onChange={(e) => {
          if (e.target.value === "__custom") {
            setCustomMode(true);
          } else {
            setCustomMode(false);
            onChange(Number(e.target.value));
          }
        }}
      >
        {DUR_PRESETS.map((s) => (
          <option key={s} value={String(s)}>
            {fmtDur(s, zeroLabel)}
          </option>
        ))}
        <option value="__custom">Custom…</option>
      </select>
      {custom && (
        <span className="durpick-custom">
          <input
            type="number"
            min={0}
            value={value}
            aria-label={`${ariaLabel} (seconds)`}
            onChange={(e) => onChange(Math.max(0, Number(e.target.value) || 0))}
          />
          seconds
        </span>
      )}
    </span>
  );
}

/// A slider that knows about inherit-vs-custom: while inheriting it shows the
/// live global value (disabled), and one tap forks a per-camera override. The
/// endpoints speak outcomes ("More alerts" / "Fewer false alerts"), not floats.
/// With resettable={false} it is a plain always-on slider (the global tier).
export function InheritSlider({
  label,
  value,
  globalValue,
  min,
  max,
  step,
  format,
  lowHint,
  highHint,
  onChange,
  resettable = true,
}: {
  label: string;
  value: number | null;
  globalValue: number | null;
  min: number;
  max: number;
  step: number;
  format: (v: number) => string;
  lowHint: string;
  highHint: string;
  onChange: (v: number | null) => void;
  resettable?: boolean;
}) {
  const shown = value ?? globalValue ?? (min + max) / 2;
  const inherited = resettable && value == null;
  return (
    <div className="islider">
      <div className="islider-head">
        <span className="islider-label">{label}</span>
        <span className="islider-val">
          {format(shown)}
          {inherited && <span className="islider-src"> · global</span>}
        </span>
      </div>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={Math.min(max, Math.max(min, shown))}
        disabled={inherited}
        aria-label={label}
        onChange={(e) => onChange(Number(e.target.value))}
      />
      <div className="islider-hints" aria-hidden="true">
        <span>{lowHint}</span>
        <span>{highHint}</span>
      </div>
      {resettable && (
        <div className="islider-foot">
          {inherited ? (
            <button type="button" className="btn-link" onClick={() => onChange(shown)}>
              Customize for this camera
            </button>
          ) : (
            <button type="button" className="btn-link" onClick={() => onChange(null)}>
              Reset to global{globalValue != null ? ` (${format(globalValue)})` : ""}
            </button>
          )}
        </div>
      )}
    </div>
  );
}
