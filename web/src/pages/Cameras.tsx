import { FormEvent, useEffect, useState } from "react";
import { api, Camera, DetectConfig, DiscoveredCam, Settings, StatusMap } from "../api";
import ZoneEditor, { COLORS } from "../ZoneEditor";
import { Modal, EmptyState, TogglePill, useToast, useDialog } from "../ui";
import {
  IconRadar,
  IconSearch,
  IconCheck,
  IconVideo,
  IconAlert,
  IconInfo,
  IconSliders,
  IconLayers,
  IconCctv,
  IconFilm,
  IconShield,
  IconZone,
} from "../icons";

const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e));

const DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Plain-language recap of a recording schedule, shown live under the controls
/// so the user reads intent ("Records Mon–Fri, 22:00–06:00 (overnight)") rather
/// than decoding day chips + time pickers. Mirrors the server's window logic.
function scheduleSummary(s: NonNullable<DetectConfig["record_schedule"]>): string {
  const days =
    s.days.length === 0 || s.days.length === 7
      ? "every day"
      : s.days
          .slice()
          .sort((a, b) => a - b)
          .map((i) => DAYS[i])
          .join(", ");
  const start = s.start_hhmm || "00:00";
  const end = s.end_hhmm || "00:00";
  const overnight = !!s.start_hhmm && !!s.end_hhmm && end < start;
  return `Records ${days}, ${start}–${end}${overnight ? " (overnight)" : ""}. Outside the window this camera stops recording; detection & event clips still run.`;
}

/// A pure on/off capability rendered as an accessible TogglePill (a real
/// <button aria-pressed>) with a visible one-line description. The meaning used
/// to live only in a `title=` tooltip — invisible on touch and to screen
/// readers — so the `help` line is the real accessibility fix, not the pill.
function Feature({
  on,
  onToggle,
  label,
  help,
  title,
}: {
  on: boolean;
  onToggle: () => void;
  label: string;
  help?: string;
  title?: string;
}) {
  return (
    <div className="feat">
      <TogglePill on={on} onClick={onToggle} ariaLabel={label} title={title}>
        {label}
      </TogglePill>
      {help && <span className="feat-help">{help}</span>}
    </div>
  );
}

function TuneModal({
  camera,
  onClose,
  onSaved,
  onError,
}: {
  camera: Camera;
  onClose: () => void;
  onSaved: () => void;
  onError: (e: string) => void;
}) {
  const [dc, setDc] = useState<DetectConfig>({
    labels: camera.detect_config.labels,
    min_score: camera.detect_config.min_score,
    motion_threshold: camera.detect_config.motion_threshold,
    zones: camera.detect_config.zones ? [...camera.detect_config.zones] : [],
    tripwires: camera.detect_config.tripwires ? [...camera.detect_config.tripwires] : [],
    ground_calib: camera.detect_config.ground_calib ?? null,
    privacy_masks: camera.detect_config.privacy_masks ? [...camera.detect_config.privacy_masks] : [],
    min_area: camera.detect_config.min_area ?? null,
    max_area: camera.detect_config.max_area ?? null,
    autotrack: camera.detect_config.autotrack ?? false,
    audio_detect: camera.detect_config.audio_detect ?? false,
    event_only_recording: camera.detect_config.event_only_recording ?? false,
    gesture_detect: camera.detect_config.gesture_detect ?? false,
    model: camera.detect_config.model ?? null,
    force_cpu: camera.detect_config.force_cpu ?? null,
    poll_ms: camera.detect_config.poll_ms ?? null,
    face_recognize: camera.detect_config.face_recognize ?? null,
    two_way_audio: camera.detect_config.two_way_audio ?? false,
    tamper_detect: camera.detect_config.tamper_detect ?? false,
    gait_identify: camera.detect_config.gait_identify ?? false,
    retention_days: camera.detect_config.retention_days ?? null,
    package_detect: camera.detect_config.package_detect ?? false,
    package_zone: camera.detect_config.package_zone ?? null,
    package_labels: camera.detect_config.package_labels ?? [],
    fall_detect: camera.detect_config.fall_detect ?? false,
    child_height_frac: camera.detect_config.child_height_frac ?? null,
    pose_detect: camera.detect_config.pose_detect ?? false,
    no_clip: camera.detect_config.no_clip ?? false,
    record_schedule: camera.detect_config.record_schedule ?? null,
    suppress_stationary: camera.detect_config.suppress_stationary ?? false,
  });
  const [subSource, setSubSource] = useState(camera.detect_source ?? "");
  // Flag toggles whose backing model isn't downloaded, so an enabled feature
  // doesn't silently no-op (the gitignored-pose-model case CLAUDE.md flags).
  const [poseModelMissing, setPoseModelMissing] = useState(false);
  // Global Settings, so a blank inherit-field can show the value it resolves to
  // ("using global: 0.4") instead of leaving per-camera tuning to guesswork.
  const [settings, setSettings] = useState<Settings | null>(null);
  useEffect(() => {
    api
      .capabilities()
      .then((r) => setPoseModelMissing(!(r.features.find((f) => f.key === "pose")?.present ?? true)))
      .catch(() => {});
    api.settings().then(setSettings).catch(() => {});
  }, []);

  const toast = useToast();
  const save = async () => {
    try {
      await api.patchCamera(camera.id, {
        detect_config: dc,
        detect_source: subSource.trim(),
      } as Partial<Camera>);
      toast.success(`Saved tuning for ${camera.name}`);
      onSaved();
      onClose();
    } catch (e) {
      onError(String(e));
    }
  };

  const featCount = [
    dc.autotrack,
    dc.audio_detect,
    dc.two_way_audio,
    dc.gesture_detect,
    dc.tamper_detect,
    dc.gait_identify,
    dc.package_detect,
  ].filter(Boolean).length;

  return (
    <Modal onClose={onClose} title={`Detection tuning — ${camera.name}`} className="modal-wide">
      <div className="tune-body">
        <div className="callout callout-info">
          <span className="callout-ico">
            <IconInfo size={16} />
          </span>
          <span>
            Empty fields <b>inherit the global Settings value</b> — clear a field to fall back to the
            default. Size filters and child height are simply <b>off</b> when left blank.
          </span>
        </div>

        {/* 1. Detection sensitivity — the recurring false-positive tuning task; open by default. */}
        <details className="adv tune-sec" open>
          <summary>
            <IconSliders size={15} /> Detection sensitivity
          </summary>
          <div className="tune-grid">
            <label className="field span-full">
              Objects to detect
              <input
                type="text"
                value={dc.labels ? dc.labels.join(", ") : ""}
                placeholder="Inherit global"
                onChange={(e) => {
                  const v = e.target.value.trim();
                  setDc({
                    ...dc,
                    labels: v === "" ? null : v.split(",").map((x) => x.trim()).filter(Boolean),
                  });
                }}
              />
              <span className="feat-help">Comma-separated; overrides the global object list.</span>
            </label>
            <label className="field">
              Minimum score
              <input
                type="number" step="0.05" min="0" max="1"
                value={dc.min_score ?? ""}
                placeholder="Inherit global"
                onChange={(e) =>
                  setDc({ ...dc, min_score: e.target.value === "" ? null : Math.min(1, Math.max(0, Number(e.target.value) || 0)) })
                }
              />
              {dc.min_score == null && settings && (
                <span className="feat-help">using global: {settings.confidence}</span>
              )}
            </label>
            <label className="field">
              Motion threshold
              <input
                type="number" step="0.005" min="0" max="1"
                value={dc.motion_threshold ?? ""}
                placeholder="Inherit global"
                onChange={(e) =>
                  setDc({
                    ...dc,
                    motion_threshold: e.target.value === "" ? null : Math.min(1, Math.max(0, Number(e.target.value) || 0)),
                  })
                }
              />
              {dc.motion_threshold == null && settings ? (
                <span className="feat-help">using global: {settings.motion_threshold}</span>
              ) : (
                <span className="feat-help">Fraction of frame that must change to run detection.</span>
              )}
            </label>
            <label className="field" title="Drop detections smaller than this fraction of the frame area (kills far-field blips).">
              Min object size
              <input
                type="number" step="0.005" min="0" max="1"
                value={dc.min_area ?? ""}
                placeholder="Off (no limit)"
                onChange={(e) =>
                  setDc({ ...dc, min_area: e.target.value === "" ? null : Math.min(1, Math.max(0, Number(e.target.value) || 0)) })
                }
              />
              <span className="feat-help">Fraction of frame area; drops far-field blips.</span>
            </label>
            <label className="field" title="Drop detections larger than this fraction of the frame area (kills whole-frame lighting flips).">
              Max object size
              <input
                type="number" step="0.05" min="0" max="1"
                value={dc.max_area ?? ""}
                placeholder="Off (no limit)"
                onChange={(e) =>
                  setDc({ ...dc, max_area: e.target.value === "" ? null : Math.min(1, Math.max(0, Number(e.target.value) || 0)) })
                }
              />
              <span className="feat-help">Fraction of frame area; drops whole-frame lighting flips.</span>
            </label>
          </div>
          <div className="feat-grid" style={{ marginTop: 12 }}>
            <Feature
              on={dc.suppress_stationary ?? false}
              onToggle={() => setDc({ ...dc, suppress_stationary: !dc.suppress_stationary })}
              label="Suppress stationary repeats"
              help="Only alert on new or moving objects — mutes a parked car re-tripping the gate."
              title="Only alert on new or moving objects. Suppresses repeat events for a parked car / idle object that keeps re-tripping the motion gate (wind, shadows, lighting). A new arrival or an object that moves still fires; the event cooldown still rate-limits moving objects. Leave off for a doorway counter that wants every detection."
            />
          </div>
        </details>

        {/* 2. Detection features — install-once capability toggles. */}
        <details className="adv tune-sec">
          <summary>
            <IconLayers size={15} /> Detection features <span className="tune-count">({featCount} on)</span>
          </summary>
          <div className="feat-grid">
            <Feature
              on={dc.autotrack}
              onToggle={() => setDc({ ...dc, autotrack: !dc.autotrack })}
              label="PTZ autotrack"
              help="Pan/tilt the camera to follow a tracked object."
            />
            <Feature
              on={dc.audio_detect}
              onToggle={() => setDc({ ...dc, audio_detect: !dc.audio_detect })}
              label="Audio detection"
              help="Classify sounds (baby cry, bark, glass, smoke alarm…)."
            />
            <Feature
              on={dc.two_way_audio}
              onToggle={() => setDc({ ...dc, two_way_audio: !dc.two_way_audio })}
              label="Two-way audio"
              help="Adds a hold-to-talk button (camera needs a speaker/backchannel)."
              title="Show a hold-to-talk button in this camera's detail view (streams your mic to the camera over WebRTC). Only works on cameras with a speaker / ONVIF backchannel."
            />
            <Feature
              on={dc.gesture_detect}
              onToggle={() => setDc({ ...dc, gesture_detect: !dc.gesture_detect })}
              label="Hand signals"
              help="Offer the live hand-signal panic overlay (Signals page)."
            />
            <Feature
              on={dc.tamper_detect}
              onToggle={() => setDc({ ...dc, tamper_detect: !dc.tamper_detect })}
              label="Tamper detection"
              help="Alert when the lens is covered, defocused, or the camera is moved."
              title="Watch this camera's optical integrity: alert when the lens is covered/blacked out, defocused, or the camera is moved/redirected."
            />
            <Feature
              on={dc.gait_identify}
              onToggle={() => setDc({ ...dc, gait_identify: !dc.gait_identify })}
              label="Gait identification"
              help="Attribute events by walking signature when the face isn't visible."
              title="Build a walking-signature for each person tracked here and attribute the event to an enrolled gait identity (works at distance / when the face isn't visible). Enroll on the People page."
            />
            <Feature
              on={dc.package_detect ?? false}
              onToggle={() => setDc({ ...dc, package_detect: !dc.package_detect })}
              label="Package detection"
              help="Alert when a parcel appears or is taken (porch piracy)."
              title="Porch-piracy alerts: fire a 'package' event when a parcel-like object sits in view for a while, and 'package_removed' when it's taken. Watches the whole frame (a package zone is API-settable). Make alarm rules with label 'package' / 'package_removed'."
            />
          </div>
          {dc.package_detect && (
            <label className="field span-full" style={{ marginTop: 12 }}>
              Package objects
              <input
                type="text"
                placeholder="suitcase, backpack, handbag"
                value={(dc.package_labels ?? []).join(", ")}
                onChange={(e) =>
                  setDc({
                    ...dc,
                    package_labels: e.target.value
                      .split(",")
                      .map((s) => s.trim())
                      .filter(Boolean),
                  })
                }
              />
              <span className="feat-help">
                Labels that count as a parcel. Blank uses the defaults: suitcase, backpack, handbag.
              </span>
            </label>
          )}
        </details>

        {/* 3. Stream & performance — install-once / expert knobs, off the everyday path. */}
        <details className="adv tune-sec">
          <summary>
            <IconCctv size={15} /> Stream &amp; performance
          </summary>
          <div className="tune-grid">
            <label className="field span-full">
              Detection sub-stream
              <input
                type="text"
                placeholder="rtsp://user:pass@cam/...subtype=1"
                value={subSource}
                onChange={(e) => setSubSource(e.target.value)}
              />
              <span className="feat-help">Low-res stream to run detection on; empty = detect on the main stream.</span>
            </label>
            <label className="field" title="Per-camera model override (e.g. a specialized .onnx). Empty inherits the global model.">
              Model override
              <input
                type="text"
                placeholder="Inherit global"
                value={dc.model ?? ""}
                onChange={(e) => setDc({ ...dc, model: e.target.value.trim() || null })}
              />
            </label>
            <label className="field" title="Accelerator assignment for this camera's detector.">
              Accelerator
              <select
                value={dc.force_cpu === null ? "" : dc.force_cpu ? "cpu" : "gpu"}
                onChange={(e) =>
                  setDc({ ...dc, force_cpu: e.target.value === "" ? null : e.target.value === "cpu" })
                }
              >
                <option value="">Inherit global</option>
                <option value="gpu">GPU</option>
                <option value="cpu">CPU</option>
              </select>
            </label>
            <label className="field" title="Per-camera sample-interval cap (resource governance). Only slows this camera down.">
              Detection interval (ms)
              <input
                type="number" step="100" min="0"
                placeholder="Inherit global"
                value={dc.poll_ms ?? ""}
                onChange={(e) => setDc({ ...dc, poll_ms: e.target.value === "" ? null : Number(e.target.value) })}
              />
              {dc.poll_ms == null && settings ? (
                <span className="feat-help">using global: {settings.poll_ms} ms</span>
              ) : (
                <span className="feat-help">ms between analyzed frames; higher = lighter load.</span>
              )}
            </label>
            <label className="field" title="Opt this camera into (or out of) face recognition. Inherit uses the global Settings switch.">
              Face recognition
              <select
                value={dc.face_recognize === null ? "" : dc.face_recognize ? "on" : "off"}
                onChange={(e) =>
                  setDc({ ...dc, face_recognize: e.target.value === "" ? null : e.target.value === "on" })
                }
              >
                <option value="">Inherit global</option>
                <option value="on">On</option>
                <option value="off">Off</option>
              </select>
            </label>
          </div>
        </details>

        {/* 4. Recording & retention. */}
        <details className="adv tune-sec">
          <summary>
            <IconFilm size={15} /> Recording &amp; retention
          </summary>
          <div className="feat-grid">
            <Feature
              on={dc.event_only_recording}
              onToggle={() => setDc({ ...dc, event_only_recording: !dc.event_only_recording })}
              label="Event-only recording"
              help="Keep only footage near events; delete quiet segments after a grace period."
              title="Keep only footage near events: segments with no detection within a segment-length margin are deleted after a 15-minute grace period. Saves most of the disk on quiet cameras."
            />
            <Feature
              on={dc.record_schedule != null}
              onToggle={() =>
                setDc({
                  ...dc,
                  record_schedule:
                    dc.record_schedule != null
                      ? null
                      : { days: [], start_hhmm: "08:00", end_hhmm: "18:00" },
                })
              }
              label="Recording schedule"
              help="Record continuously only on chosen days/times (off = always record)."
              title="Record continuously only during these days/times (Blue Iris-style schedule). Off = always record. Detection and event clips are unaffected."
            />
          </div>
          <div className="tune-grid" style={{ marginTop: 12 }}>
            <label
              className="field"
              title="Keep this camera's footage for a custom number of days (e.g. a doorbell 30, a quiet side camera 3). Blank inherits the global retention. The global disk size cap still applies as the safety net."
            >
              Retention (days)
              <input
                type="number"
                min="0"
                value={dc.retention_days ?? ""}
                placeholder="Inherit global"
                onChange={(e) =>
                  setDc({
                    ...dc,
                    retention_days: e.target.value === "" ? null : Math.max(0, Number(e.target.value) || 0),
                  })
                }
              />
              {dc.retention_days == null && settings && (
                <span className="feat-help">using global: {settings.retention_days} days</span>
              )}
            </label>
          </div>
          {dc.record_schedule && (
            <div className="sched" style={{ marginTop: 12 }}>
              <div className="row" style={{ gap: 6, flexWrap: "wrap", marginBottom: 8 }}>
                {DAYS.map((d, i) => {
                  const on = dc.record_schedule!.days.includes(i);
                  return (
                    <TogglePill
                      key={d}
                      on={on}
                      ariaLabel={`${d} ${on ? "on" : "off"}`}
                      onClick={() =>
                        setDc({
                          ...dc,
                          record_schedule: {
                            ...dc.record_schedule!,
                            days: on
                              ? dc.record_schedule!.days.filter((x) => x !== i)
                              : [...dc.record_schedule!.days, i].sort((a, b) => a - b),
                          },
                        })
                      }
                    >
                      {d}
                    </TogglePill>
                  );
                })}
              </div>
              <div className="row" style={{ gap: 8, alignItems: "center" }}>
                <span className="muted">from</span>
                <input
                  type="time"
                  value={dc.record_schedule.start_hhmm ?? ""}
                  onChange={(e) =>
                    setDc({
                      ...dc,
                      record_schedule: { ...dc.record_schedule!, start_hhmm: e.target.value || null },
                    })
                  }
                />
                <span className="muted">to</span>
                <input
                  type="time"
                  value={dc.record_schedule.end_hhmm ?? ""}
                  onChange={(e) =>
                    setDc({
                      ...dc,
                      record_schedule: { ...dc.record_schedule!, end_hhmm: e.target.value || null },
                    })
                  }
                />
              </div>
              <p className="feat-help" style={{ marginTop: 6 }}>
                {scheduleSummary(dc.record_schedule)}
              </p>
            </div>
          )}
        </details>

        {/* 5. Residential safety & privacy — assistive, liability-sensitive. */}
        <details className="adv tune-sec">
          <summary>
            <IconShield size={15} /> Residential safety &amp; privacy (assistive*)
          </summary>
          <div className="callout callout-warn" style={{ marginTop: 8 }}>
            <span className="callout-ico">
              <IconAlert size={16} />
            </span>
            <span>
              Fall detection and child classification are <b>assistive, best-effort</b> safety aids —
              not medical devices and not guaranteed. They can miss events and must never replace
              supervision or a personal alarm.
            </span>
          </div>
          <div className="feat-grid">
            <Feature
              on={dc.fall_detect ?? false}
              onToggle={() => setDc({ ...dc, fall_detect: !dc.fall_detect })}
              label="Fall detection (assistive*)"
              help="A person going motionless low in frame fires a 'fall' event. Not a medical device."
              title="Residential ASSISTIVE fall hint: a tracked person who goes motionless low in the frame fires a 'fall' event. Best-effort at ~1 fps — it MISSES occluded, soft, or slow falls. NOT a medical-alert device; pair it with a pendant and never auto-dial emergency services off a single visual trigger."
            />
            <Feature
              on={dc.pose_detect ?? false}
              onToggle={() => setDc({ ...dc, pose_detect: !dc.pose_detect })}
              label="Body pose monitoring (assistive*)"
              help="24/7 pose model: fall, crib climb-out, covered face. Draw a crib/bed zone."
              title="Server-side 24/7 body-pose monitoring for the nursery/elder camera: emits 'fall' (lying on the floor), 'standing' (a child standing up in a crib zone — climb-out) and 'covered_face' (body present but face not visible in a zone — rollover / blanket). Runs a YOLOv8-pose model on the server (download yolov8n-pose.onnx; set the path in Settings). ASSISTIVE only — not a medical/SIDS device, draw a crib/bed zone for standing + covered-face."
            />
            <Feature
              on={dc.no_clip ?? false}
              onToggle={() => setDc({ ...dc, no_clip: !dc.no_clip })}
              label="No snapshot on safety events"
              help="Safety events still fire, but no image is saved (nursery/bathroom dignity)."
              title="Privacy / dignity for a sensitive camera (nursery, bedroom, bathroom): residential + pose safety events still fire (you get the alert — label, zone, time), but NO snapshot image is saved to disk or sent to webhook/MQTT/email. Pair with a privacy mask for live view."
            />
          </div>
          {poseModelMissing && (
            <div
              className="callout callout-warn"
              role="status"
              style={{ marginTop: 10, marginBottom: 0 }}
            >
              <span className="callout-ico">
                <IconAlert size={16} />
              </span>
              <span>
                Pose model not downloaded — body pose monitoring won't run until
                <code> yolov8n-pose.onnx</code> is added (see Settings → Models &amp; capabilities).
              </span>
            </div>
          )}
          <div className="tune-grid" style={{ marginTop: 12 }}>
            <label
              className="field"
              title="Residential child calibration: a tracked person whose normalized bbox HEIGHT (0..1 of the frame) is at/below this fraction is treated as a 'child', enabling the child / child-alone zone rules. Blank disables child features. FRAGILE — bbox height depends on camera angle/distance; tune per camera and treat results as a detection aid only."
            >
              Child height ≤ (fraction)
              <input
                type="number"
                step="0.05"
                min="0"
                max="1"
                placeholder="Off"
                value={dc.child_height_frac ?? ""}
                onChange={(e) =>
                  setDc({
                    ...dc,
                    child_height_frac:
                      e.target.value === "" ? null : Math.min(1, Math.max(0, Number(e.target.value) || 0)),
                  })
                }
              />
              <span className="feat-help">
                Fraction of frame height at/below which a person counts as a child (fragile — tune per
                camera). Blank = off.
              </span>
            </label>
          </div>
        </details>

        {/* 6. Zones & privacy masks. */}
        <div className="card-head" style={{ marginTop: 16, marginBottom: 8 }}>
          <IconZone size={18} />
          <div>
            <p className="eyebrow">Detection areas</p>
            <h2 style={{ margin: 0 }}>Zones &amp; privacy masks</h2>
          </div>
        </div>
        <p className="muted" style={{ marginTop: 0 }}>
          Draw polygons on the live frame. <b style={{ color: COLORS.required }}>Required</b> zones keep
          only objects inside them; <b style={{ color: COLORS.ignore }}>ignore</b> zones drop objects
          inside; <b style={{ color: COLORS.mask }}>privacy masks</b> are blacked out before any
          analysis or snapshot (continuous recordings are not masked).
        </p>
        <ZoneEditor
          camera={camera}
          zones={dc.zones}
          masks={dc.privacy_masks}
          tripwires={dc.tripwires ?? []}
          calib={dc.ground_calib ?? null}
          onChange={(zones, masks) => setDc({ ...dc, zones, privacy_masks: masks })}
          onTripwires={(tripwires) => setDc({ ...dc, tripwires })}
          onCalib={(ground_calib) => setDc({ ...dc, ground_calib })}
        />
      </div>

      <div className="dialog-actions tune-foot">
        <button className="btn btn-ghost" onClick={onClose}>
          Cancel
        </button>
        <button className="btn btn-primary" onClick={save}>
          Save
        </button>
      </div>
    </Modal>
  );
}

/// Inline camera-name editor: commits on blur/Enter. Renaming restarts go2rtc
/// (a brief live-stream blip) since the stream name changes. Names are
/// lowercase letters/digits/_/- (≤32); the server rejects others and we revert.
function NameCell({
  cam,
  onChange,
  onError,
}: {
  cam: Camera;
  onChange: () => void;
  onError: (e: string) => void;
}) {
  const toast = useToast();
  const [val, setVal] = useState(cam.name);
  useEffect(() => {
    setVal(cam.name);
  }, [cam.name]);
  const commit = async () => {
    const next = val.trim();
    if (next === cam.name) return;
    if (!next) {
      setVal(cam.name); // a name can't be empty
      return;
    }
    try {
      await api.patchCamera(cam.id, { name: next } as Partial<Camera>);
      toast.success(`Renamed to ${next}`);
      onChange();
    } catch (e) {
      setVal(cam.name); // revert on rejection (e.g. invalid chars)
      onError(String(e));
    }
  };
  return (
    <input
      className="field"
      style={{ width: 130, fontWeight: 600 }}
      value={val}
      onChange={(e) => setVal(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
      }}
      title="Rename (lowercase/digits/_/-; restarts the stream briefly)"
    />
  );
}

/// Inline group editor: commits on blur/Enter; empty string clears the group.
/// Patching only `group` is metadata-only, so the server skips the go2rtc
/// restart and live streams keep playing.
function GroupCell({
  cam,
  onChange,
  onError,
}: {
  cam: Camera;
  onChange: () => void;
  onError: (e: string) => void;
}) {
  const toast = useToast();
  const [val, setVal] = useState(cam.group ?? "");
  useEffect(() => {
    setVal(cam.group ?? "");
  }, [cam.group]);
  const commit = async () => {
    const next = val.trim();
    if (next === (cam.group ?? "")) return;
    try {
      await api.patchCamera(cam.id, { group: next } as Partial<Camera>);
      toast.success(next ? `Moved to “${next}”` : "Removed from group");
      onChange();
    } catch (e) {
      onError(String(e));
    }
  };
  return (
    <input
      className="field"
      list="cam-groups"
      placeholder="—"
      style={{ width: 110 }}
      value={val}
      onChange={(e) => setVal(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
      }}
    />
  );
}

export default function Cameras({
  cameras,
  onChange,
  onError,
}: {
  cameras: Camera[];
  onChange: () => void;
  onError: (e: string) => void;
}) {
  const toast = useToast();
  const dialog = useDialog();
  const [status, setStatus] = useState<StatusMap>({});
  const [tuning, setTuning] = useState<Camera | null>(null);

  useEffect(() => {
    const load = () => api.status().then(setStatus).catch(() => {});
    load();
    const t = setInterval(load, 5000);
    return () => clearInterval(t);
  }, []);
  const [name, setName] = useState("");
  const [source, setSource] = useState("");
  const [detectSource, setDetectSource] = useState("");
  const [group, setGroup] = useState("");
  const [detect, setDetect] = useState(true);
  const [record, setRecord] = useState(true);
  const [busy, setBusy] = useState(false);
  const [ip, setIp] = useState("");
  const [user, setUser] = useState("admin");
  const [pass, setPass] = useState("");
  const [found, setFound] = useState<string | null>(null);
  const [scanning, setScanning] = useState(false);
  const [scanned, setScanned] = useState<DiscoveredCam[] | null>(null);

  const scan = async () => {
    setScanning(true);
    try {
      const r = await api.scanNetwork();
      setScanned(r.cameras);
    } catch (e) {
      onError(`Couldn't scan the network for cameras — check the server can reach your LAN. (${errMsg(e)})`);
    } finally {
      setScanning(false);
    }
  };

  const resolve = async () => {
    setBusy(true);
    setFound(null);
    try {
      const r = await api.discover(ip.trim(), user, pass);
      const streams = r.sources.filter((s) => !s.url.includes("snapshot"));
      if (streams.length === 0) throw new Error("no streams found");
      setSource(streams[0].url);
      if (streams.length > 1) setDetectSource(streams[1].url);
      setFound(`${streams[0].name.replace(/ stream\d+$/, "")} — ${streams.length} streams`);
    } catch (e) {
      onError(`Couldn't get streams from that camera over ONVIF — check the IP, username and password. (${errMsg(e)})`);
    } finally {
      setBusy(false);
    }
  };

  // Enter in any of the IP / username / password fields triggers Resolve (these
  // inputs aren't inside a <form>, so there's no implicit submit to rely on).
  const onResolveKey = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && ip.trim() && !busy) {
      e.preventDefault();
      resolve();
    }
  };

  const add = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      await api.addCamera({
        name: name.trim(),
        source: source.trim(),
        detect_source: detectSource.trim() || undefined,
        group: group.trim() || undefined,
        detect,
        record,
      });
      const added = name.trim();
      setName("");
      setSource("");
      setDetectSource("");
      setGroup("");
      setFound(null);
      toast.success(`Added ${added}`);
      onChange();
    } catch (err) {
      onError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const toggle = async (cam: Camera, field: "enabled" | "detect" | "record") => {
    try {
      await api.patchCamera(cam.id, { [field]: !cam[field] });
      toast.success(`${cam.name}: ${field} ${!cam[field] ? "on" : "off"}`);
      onChange();
    } catch (err) {
      onError(String(err));
    }
  };

  const remove = async (cam: Camera) => {
    const ok = await dialog.confirm({
      title: `Delete camera “${cam.name}”?`,
      body: "Its events are removed too. This can't be undone.",
      confirmLabel: "Delete",
      danger: true,
    });
    if (!ok) return;
    try {
      await api.deleteCamera(cam.id);
      toast.success(`Deleted ${cam.name}`);
      onChange();
    } catch (err) {
      onError(String(err));
    }
  };

  const groups = Array.from(
    new Set(cameras.map((c) => c.group).filter((g): g is string => !!g)),
  ).sort();

  return (
    <>
      <h1>Cameras</h1>
      <datalist id="cam-groups">
        {groups.map((g) => (
          <option key={g} value={g} />
        ))}
      </datalist>

      <div style={{ display: "flex", flexDirection: "column", gap: 14 }}>
      <div className="card" style={{ order: cameras.length > 0 ? 2 : 1, margin: 0 }}>
        <details className="adv tune-sec" open={cameras.length === 0}>
        <summary><IconVideo size={15} /> Add a camera</summary>
        <div className="row" style={{ marginBottom: 10, marginTop: 8 }}>
          <button type="button" className="btn btn-ghost" disabled={scanning} onClick={scan}>
            {scanning ? "Scanning…" : (<><IconRadar size={15} /> Scan network for cameras</>)}
          </button>
          {scanned !== null && scanned.length === 0 && (
            <span className="muted">no ONVIF cameras responded</span>
          )}
          {scanned?.map((c) => (
            <TogglePill
              key={c.host}
              on={ip === c.host}
              title="click to fill the IP field"
              ariaLabel={`Use ${c.host}${c.name ? ` (${c.name})` : ""}`}
              onClick={() => setIp(c.host)}
            >
              {c.host}
              {c.name ? ` — ${c.name}` : ""}
            </TogglePill>
          ))}
        </div>
        <div className="row" style={{ marginBottom: 14 }}>
          <label className="field">
            camera IP / host
            <input type="text" inputMode="url" placeholder="192.168.1.50" value={ip} onChange={(e) => setIp(e.target.value)} onKeyDown={onResolveKey} />
          </label>
          <label className="field">
            username
            <input type="text" autoComplete="off" value={user} onChange={(e) => setUser(e.target.value)} onKeyDown={onResolveKey} />
          </label>
          <label className="field">
            password
            <input type="password" autoComplete="off" value={pass} onChange={(e) => setPass(e.target.value)} onKeyDown={onResolveKey} />
          </label>
          <button type="button" className="btn btn-ghost" disabled={busy || !ip.trim()} onClick={resolve}>
            <IconSearch size={15} /> Resolve via ONVIF
          </button>
          {found && (
            <span className="save-ok"><IconCheck size={14} /> {found} (form filled below)</span>
          )}
        </div>
        <form onSubmit={add} className="row">
          <label className="field">
            name
            <input
              type="text"
              placeholder="front-door"
              value={name}
              onChange={(e) => setName(e.target.value)}
              required
            />
          </label>
          <label className="field" style={{ flex: 1, minWidth: 280 }}>
            source (RTSP URL or any go2rtc source)
            <input
              type="text"
              placeholder="rtsp://user:pass@192.168.1.50:554/stream1"
              value={source}
              onChange={(e) => setSource(e.target.value)}
              required
              style={{ width: "100%" }}
            />
          </label>
          <label className="field" style={{ flex: 1, minWidth: 220 }}>
            sub-stream for detection (optional)
            <input
              type="text"
              placeholder="auto-filled by ONVIF resolve"
              value={detectSource}
              onChange={(e) => setDetectSource(e.target.value)}
              style={{ width: "100%" }}
            />
          </label>
          <label className="field" style={{ minWidth: 130 }} title="Optional: group cameras for the Live view (e.g. 'outdoor', 'downstairs').">
            group (optional)
            <input
              type="text"
              list="cam-groups"
              placeholder="e.g. outdoor"
              value={group}
              onChange={(e) => setGroup(e.target.value)}
            />
          </label>
          <label className="toggle">
            <input type="checkbox" checked={detect} onChange={() => setDetect(!detect)} /> detect
          </label>
          <label className="toggle">
            <input type="checkbox" checked={record} onChange={() => setRecord(!record)} /> record
          </label>
          <button className="btn btn-primary" disabled={busy}>
            Add
          </button>
        </form>
        <p className="muted" style={{ marginBottom: 0 }}>
          Names: lowercase letters, digits, "-", "_". The source is passed to go2rtc verbatim, so{" "}
          <code>rtsp://</code>, <code>ffmpeg:</code>, <code>exec:</code>… all work.
        </p>
        </details>
      </div>

      <div className="card" style={{ order: cameras.length > 0 ? 1 : 2, margin: 0 }}>
        <h2>Registered</h2>
        {cameras.length === 0 ? (
          <EmptyState
            icon={<IconVideo />}
            title="No cameras yet"
            hint="Add your first camera using the form above to start recording and detection."
          />
        ) : (
          <div className="table-scroll">
          <table>
            <thead>
              <tr>
                <th>Status</th>
                <th>Name</th>
                <th>Source</th>
                <th>Enabled</th>
                <th>Detect</th>
                <th>Record</th>
                <th>Group</th>
                <th>Perf</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {cameras.map((cam) => {
                const s = status[String(cam.id)];
                return (
                <tr key={cam.id}>
                  <td title={s?.last_error ?? ""}>
                    <span
                      className={`dot ${s ? (s.online ? "on" : "off") : ""}`}
                      aria-hidden="true"
                    />{" "}
                    <span className="muted">
                      {s?.online ? "online" : s ? "offline" : "checking…"}
                    </span>
                    {s && !s.online && s.last_error && (
                      <span className="badge danger" style={{ marginLeft: 6 }} title={s.last_error}>
                        <IconAlert size={11} /> error
                      </span>
                    )}
                  </td>
                  <td>
                    <NameCell cam={cam} onChange={onChange} onError={onError} />
                  </td>
                  <td className="muted" style={{ maxWidth: 360, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                    {cam.source}
                  </td>
                  {(["enabled", "detect", "record"] as const).map((f) => (
                    <td key={f}>
                      <TogglePill
                        on={cam[f]}
                        ariaLabel={`${cam.name} ${f} ${cam[f] ? "on" : "off"}`}
                        onClick={() => toggle(cam, f)}
                      >
                        {cam[f] ? "on" : "off"}
                      </TogglePill>
                    </td>
                  ))}
                  <td>
                    <GroupCell cam={cam} onChange={onChange} onError={onError} />
                  </td>
                  <td className="muted" style={{ whiteSpace: "nowrap" }}>
                    {!s?.accelerator
                      ? "—"
                      : `${s.inference_ms != null ? s.inference_ms.toFixed(1) + "ms · " : ""}${s.accelerator}`}
                  </td>
                  <td>
                    <button className="btn btn-ghost ev-act" onClick={() => setTuning(cam)} style={{ marginRight: 8 }}>
                      Tune
                    </button>
                    <button className="btn btn-danger ev-act" onClick={() => remove(cam)}>
                      Delete
                    </button>
                  </td>
                </tr>
                );
              })}
            </tbody>
          </table>
          </div>
        )}
      </div>
      </div>

      {tuning && (
        <TuneModal
          camera={tuning}
          onClose={() => setTuning(null)}
          onSaved={onChange}
          onError={onError}
        />
      )}
    </>
  );
}
