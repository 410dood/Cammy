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
    const l = other.trim().toLowerCase().replace(/\s+/g, "_");
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
              <TogglePill key={l} on ariaLabel={`Stop detecting ${l}`} onClick={() => toggle(l)}>
                {prettyLabel(l)}
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
