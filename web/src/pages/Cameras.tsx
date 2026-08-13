import { ChangeEvent, FormEvent, useEffect, useRef, useState } from "react";
import { api, Camera, CamStorage, DetectConfig, DiscoveredCam, DAY_NAMES, fmtBytes, Settings, StatusMap, capabilityUsable } from "../api";
import ZoneEditor, { COLORS } from "../ZoneEditor";
import { prettyLabel, recordState, recordStateHint, scheduleWindow } from "../labels";
import SizeFilterEditor, { ChildHeightEditor, MotionTuner, RectZoneDraw } from "../SizeFilterEditor";
import { ObjectPicker, InheritSlider, LabelChips, DurationPicker } from "../tuning";
import { Modal, EmptyState, TogglePill, Callout, useToast, useDialog, usePolling } from "../ui";
import {
  IconRadar,
  IconSearch,
  IconCheck,
  IconVideo,
  IconAlert,
  IconSliders,
  IconLayers,
  IconCctv,
  IconFilm,
  IconShield,
  IconZone,
} from "../icons";

/// Mirror of the server's onvif_events::label_for — which raw camera topics
/// become Cammy events. Kept in sync so the inspector can say honestly whether
/// a notification is recorded or shown-only.
function onvifLabelFor(topic: string, objectClass: string | null): string | null {
  if (objectClass) {
    const c = objectClass.toLowerCase();
    if (c.includes("human") || c.includes("person") || c.includes("people")) return "camera_person";
    if (c.includes("vehicle") || c.includes("car") || c.includes("truck")) return "camera_vehicle";
  }
  const t = topic.toLowerCase();
  if (t.includes("crossline") || t.includes("tripwire") || t.includes("linedetector"))
    return "camera_tripwire";
  if (t.includes("intrusion") || t.includes("fielddetector") || t.includes("objectsinside"))
    return "camera_intrusion";
  if (t.includes("motion")) return "camera_motion";
  return null;
}

/// docs/11 P3 — the ONVIF event inspector (Blue Iris-style): a live view of the
/// raw notifications this camera's own chip is emitting, so the owner can see
/// exactly what it says BEFORE writing alarm rules against the camera_* labels
/// — and can tell "the camera never sends anything" apart from "it sends
/// events Cammy doesn't ingest".
function OnvifInspectorModal({ camera, onClose }: { camera: Camera; onClose: () => void }) {
  const [rows, setRows] = useState<import("../api").OnvifNotifyRow[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  usePolling(
    () => {
      api
        .onvifInspect(camera.id)
        .then((r) => {
          setRows(r[String(camera.id)] ?? []);
          setErr(null);
        })
        .catch((e) => setErr(String(e)));
    },
    3000,
    [camera.id],
  );
  return (
    <Modal onClose={onClose} title={`What ${camera.name} is saying`} className="modal-wide">
      <p className="hint" style={{ marginTop: 0 }}>
        The most recent notifications this camera's own detection sent over ONVIF (newest first,
        refreshing live). Rows marked <b>recorded</b> become events an alarm rule can match; the
        rest are topics Cammy doesn't turn into events — shown here so you can see everything the
        camera emits.
      </p>
      {err && <Callout tone="warn">Couldn't load the camera's notifications: {err}</Callout>}
      {rows !== null && rows.length === 0 && (
        <EmptyState
          title="Nothing received from this camera yet"
          hint="The camera only reports when its own detection triggers something — walk in front of it, or check that its built-in motion/IVS rules are enabled in the camera's own settings. The list starts empty after every Cammy restart, and needs ONVIF credentials (user:pass@host) in the camera source."
        />
      )}
      {rows !== null && rows.length > 0 && (
        <div className="table-scroll">
          <table>
            <thead>
              <tr>
                <th>When</th>
                <th>Camera topic</th>
                <th>State</th>
                <th>Object</th>
                <th>Becomes</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r, i) => {
                const label = onvifLabelFor(r.notify.topic, r.notify.object_class);
                return (
                  <tr key={`${r.ts}-${i}`}>
                    <td style={{ whiteSpace: "nowrap" }}>
                      {new Date(r.ts * 1000).toLocaleTimeString()}
                    </td>
                    <td style={{ wordBreak: "break-all" }}>{r.notify.topic}</td>
                    <td>
                      {r.notify.active === null ? "pulse" : r.notify.active ? "active" : "cleared"}
                    </td>
                    <td>{r.notify.object_class ?? "—"}</td>
                    <td>
                      {label ? (
                        <span className="badge ok" title={`Recorded as a "${prettyLabel(label)}" event when it turns active (alarm rules match the label "${label}").`}>
                          {prettyLabel(label)}
                        </span>
                      ) : (
                        <span className="hint" title="Cammy doesn't turn this topic into an event — it's shown here only.">
                          shown only
                        </span>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </Modal>
  );
}

/// docs/10 P3 — the per-camera model override was a spell-the-filename
/// textbox; now it's a select of the detector `.onnx` files actually present
/// in the app directory (a custom path stays possible via "Other…").
function ModelOverrideField({
  value,
  onChange,
}: {
  value: string | null;
  onChange: (m: string | null) => void;
}) {
  const [installed, setInstalled] = useState<string[] | null>(null);
  const [custom, setCustom] = useState(false);
  useEffect(() => {
    api.modelsInstalled().then((r) => setInstalled(r.models)).catch(() => setInstalled(null));
  }, []);
  const showSelect = installed !== null && !custom && (value === null || installed.includes(value));
  return (
    <label className="field" title="Per-camera detector model. Inherit uses the global model; the list is what's actually installed.">
      Model override
      {showSelect ? (
        <select
          value={value ?? ""}
          onChange={(e) => {
            if (e.target.value === "__other__") setCustom(true);
            else onChange(e.target.value || null);
          }}
        >
          <option value="">Inherit global</option>
          {installed.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
          <option value="__other__">Other (type a path)…</option>
        </select>
      ) : (
        <input
          type="text"
          placeholder="Inherit global"
          value={value ?? ""}
          onChange={(e) => onChange(e.target.value.trim() || null)}
        />
      )}
    </label>
  );
}

/// docs/10 P3 — a minimal server-side folder browser for "Import footage":
/// walk drives/folders and pick a video file instead of hand-typing an
/// absolute path. Lists only folders + video files (server-filtered).
function FileBrowser({ onPick, onClose }: { onPick: (path: string) => void; onClose: () => void }) {
  const [listing, setListing] = useState<Awaited<ReturnType<typeof api.fsList>> | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const open = (path: string) => {
    api
      .fsList(path)
      .then((r) => {
        if (r.ok) {
          setListing(r);
          setErr(null);
        } else setErr(r.error ?? "couldn't open that folder");
      })
      .catch((e) => setErr(String(e)));
  };
  useEffect(() => open(""), []);
  const join = (dir: string) => {
    if (!listing?.path) return dir; // a root like "E:\"
    const sep = listing.path.includes("\\") ? "\\" : "/";
    return listing.path.endsWith(sep) ? `${listing.path}${dir}` : `${listing.path}${sep}${dir}`;
  };
  return (
    <Modal title="Pick a video file on the Cammy server" onClose={onClose}>
      <div style={{ minWidth: "min(560px, 84vw)", maxHeight: "60vh", overflowY: "auto", padding: "2px 4px" }}>
        {err && <p style={{ color: "var(--danger)" }}>{err}</p>}
        {!listing ? (
          <span className="skeleton" style={{ height: 80, width: "100%" }} />
        ) : (
          <>
            <div className="muted" style={{ fontSize: "var(--text-sm)", marginBottom: 8 }}>
              {listing.path || "This computer"}
            </div>
            {listing.path && (
              <button type="button" className="btn btn-ghost ev-act" onClick={() => open(listing.parent ?? "")}>
                ← Up
              </button>
            )}
            <ul style={{ listStyle: "none", margin: "8px 0 0", padding: 0 }}>
              {listing.dirs.map((d) => (
                <li key={d}>
                  <button
                    type="button"
                    className="btn btn-ghost ev-act"
                    style={{ width: "100%", justifyContent: "flex-start" }}
                    onClick={() => open(join(d))}
                  >
                    📁 {d}
                  </button>
                </li>
              ))}
              {listing.files.map((f) => (
                <li key={f.name}>
                  <button
                    type="button"
                    className="btn btn-ghost ev-act"
                    style={{ width: "100%", justifyContent: "flex-start" }}
                    onClick={() => onPick(join(f.name))}
                  >
                    <IconFilm size={13} /> {f.name}{" "}
                    <span className="muted" style={{ marginLeft: "auto", fontSize: "var(--text-xs)" }}>
                      {(f.size / 1e6).toFixed(1)} MB
                    </span>
                  </button>
                </li>
              ))}
              {listing.dirs.length === 0 && listing.files.length === 0 && (
                <li className="muted" style={{ fontSize: "var(--text-sm)" }}>
                  No subfolders or video files here.
                </li>
              )}
            </ul>
          </>
        )}
      </div>
    </Modal>
  );
}

const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e));

/// The stream engine needs a slug id, but that's OUR constraint, not the
/// user's problem: they type "Front Door", we store front-door (previewed
/// live under the field, enforced at submit).
const slugifyName = (s: string) =>
  s
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 32);

/// RTSP path templates for the common consumer brands, so "add manually"
/// means picking a brand — not finding and pasting a URL with credentials in
/// it from the camera's own web admin.
const BRAND_TEMPLATES: { key: string; label: string; main: string; sub?: string }[] = [
  { key: "onvif", label: "Any ONVIF camera (auto-negotiates)", main: "onvif://{auth}{host}" },
  { key: "dahua", label: "Dahua / Amcrest / Lorex", main: "rtsp://{auth}{host}:554/cam/realmonitor?channel=1&subtype=0", sub: "rtsp://{auth}{host}:554/cam/realmonitor?channel=1&subtype=1" },
  { key: "hikvision", label: "Hikvision / Annke / Hilook", main: "rtsp://{auth}{host}:554/Streaming/Channels/101", sub: "rtsp://{auth}{host}:554/Streaming/Channels/102" },
  { key: "reolink", label: "Reolink", main: "rtsp://{auth}{host}:554/h264Preview_01_main", sub: "rtsp://{auth}{host}:554/h264Preview_01_sub" },
  { key: "generic", label: "Generic RTSP", main: "rtsp://{auth}{host}:554/" },
];
const fillTemplate = (tpl: string, host: string, user: string, pass: string) =>
  tpl
    .replace("{auth}", user ? `${encodeURIComponent(user)}:${encodeURIComponent(pass)}@` : "")
    .replace("{host}", host.trim());

/// docs/11 P2 — per-camera retention as chips, with an estimate of what the
/// chosen span COSTS on this camera specifically. The global retention control
/// earned an honest readout after the 6-hour-retention surprise; the per-camera
/// one was still a bare number box that said nothing about consequences, on the
/// setting where cameras differ most (a 4K doorbell writes an order of
/// magnitude more than a quiet side yard).
function CamRetentionField({
  cameraId,
  value,
  globalDays,
  onChange,
}: {
  cameraId: number;
  value: number | null;
  globalDays: number | null;
  onChange: (days: number | null) => void;
}) {
  const [cam, setCam] = useState<CamStorage | null>(null);
  useEffect(() => {
    api
      .stats()
      .then((st) => setCam(st.cameras.find((c) => c.camera_id === cameraId) ?? null))
      .catch(() => setCam(null));
  }, [cameraId]);
  // This camera's measured write rate, from what it has actually written. Needs
  // at least ~an hour of span to be worth stating.
  const rate = (() => {
    if (!cam || cam.oldest_ts == null || cam.newest_ts == null) return null;
    const days = (cam.newest_ts - cam.oldest_ts) / 86_400;
    if (days < 0.04 || cam.bytes <= 0) return null;
    return cam.bytes / days; // bytes per day
  })();
  const chosen = value ?? globalDays;
  return (
    <label
      className="field"
      title="Keep this camera's footage for its own number of days (e.g. a doorbell 30, a quiet side camera 3). Inherit uses the global retention. The global disk size cap still applies as the safety net."
      style={{ minWidth: 300 }}
    >
      Keep this camera&apos;s footage for
      <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
        <TogglePill
          on={value == null}
          ariaLabel="Inherit the global retention"
          onClick={() => onChange(null)}
        >
          {globalDays != null ? `Global (${globalDays} days)` : "Global"}
        </TogglePill>
        {[
          { d: 1, t: "1 day" },
          { d: 3, t: "3 days" },
          { d: 7, t: "1 week" },
          { d: 14, t: "2 weeks" },
          { d: 30, t: "30 days" },
        ].map((p) => (
          <TogglePill
            key={p.d}
            on={value === p.d}
            ariaLabel={`Keep this camera's footage for ${p.t}`}
            onClick={() => onChange(p.d)}
          >
            {p.t}
          </TogglePill>
        ))}
        {value != null && ![1, 3, 7, 14, 30].includes(value) && (
          <span className="badge accent">custom: {value} days</span>
        )}
      </div>
      {rate != null && chosen != null && chosen > 0 && (
        <span className="feat-help">
          At this camera&apos;s measured rate ({fmtBytes(rate)}/day), {chosen}{" "}
          {chosen === 1 ? "day" : "days"} is about {fmtBytes(rate * chosen)}.
        </span>
      )}
    </label>
  );
}

/// docs/11 P1.5 — "Test this stream": prove a camera URL produces a picture
/// BEFORE saving it.
///
/// A wrong detection sub-stream silently kills detection on that camera while
/// everything still looks configured. And finding out by saving is expensive:
/// editing `detect_source` forces a full go2rtc restart, i.e. a live-view blip
/// for every camera in the house — twice, if the first guess was wrong.
///
/// Reports the SIZE too. A sub-stream that works but is 4K is a different
/// mistake from one that doesn't work, and it's the mistake this field exists to
/// prevent.
function StreamProbe({
  src,
  auto = false,
  warnBig = true,
}: {
  src: string;
  auto?: boolean;
  /** Warn when the stream is full-size. Right for the DETECTION sub-stream
   *  (whose whole point is being small); wrong for a main address, where 4K is
   *  simply the camera working. */
  warnBig?: boolean;
}) {
  const [busy, setBusy] = useState(false);
  const [res, setRes] = useState<Awaited<ReturnType<typeof api.streamProbe>> | null>(null);
  useEffect(() => setRes(null), [src]);
  // docs/11 P2 — probe-before-enable: in the add wizard the URL came from a
  // brand template the user never typed, so verify it unprompted (debounced so
  // hand-editing the address doesn't fire a probe per keystroke).
  useEffect(() => {
    if (!auto || !src.trim()) return;
    let stale = false;
    const t = setTimeout(async () => {
      setBusy(true);
      try {
        const r = await api.streamProbe(src.trim());
        if (!stale) setRes(r);
      } catch (e) {
        if (!stale) setRes({ ok: false, error: e instanceof Error ? e.message : String(e) });
      } finally {
        if (!stale) setBusy(false);
      }
    }, 600);
    return () => {
      stale = true;
      clearTimeout(t);
    };
  }, [auto, src]);
  if (!src.trim()) return null;
  const big = warnBig && (res?.width ?? 0) > 1280;
  return (
    <div style={{ marginTop: 4 }}>
      <button
        type="button"
        className="btn btn-ghost ev-act"
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          try {
            setRes(await api.streamProbe(src.trim()));
          } catch (e) {
            setRes({ ok: false, error: e instanceof Error ? e.message : String(e) });
          } finally {
            setBusy(false);
          }
        }}
      >
        {busy ? "Connecting…" : "Test this stream"}
      </button>
      {res && (
        <div className="row" style={{ gap: 8, alignItems: "flex-start", marginTop: 6 }}>
          {res.frame && (
            <img
              src={res.frame}
              alt="Picture from the stream being tested"
              style={{ maxWidth: 160, borderRadius: 6, border: "1px solid var(--border)" }}
            />
          )}
          <span
            className="feat-help"
            style={{ color: res.ok ? (big ? "var(--warn)" : "var(--success)") : "var(--danger)" }}
          >
            {res.ok
              ? big
                ? `Works — but it's ${res.width}×${res.height}. That's a full-size stream; detection is meant to run on the small one, so this won't save any work.`
                : `Works — ${res.width}×${res.height}.`
              : `No picture: ${res.error}`}
          </span>
        </div>
      )}
    </div>
  );
}

/// docs/11 P1.5 — offer the brand's known low-res path instead of asking someone
/// to remember that Dahua's sub-stream is `subtype=1` and Hikvision's is `/102`.
///
/// The add wizard has had these templates since the discover-first rewrite; the
/// tuning modal, where a sub-stream is actually most often added, did not. The
/// host and credentials are lifted out of the camera's MAIN source, so there is
/// nothing to retype (and no second place for a password to be typed wrong).
function SubStreamTemplates({
  mainSource,
  onPick,
}: {
  mainSource: string;
  onPick: (url: string) => void;
}) {
  // `scheme://user:pass@host[:port]/…` → the pieces a template needs.
  const m = /^\w+:\/\/(?:([^/@\s:]+)(?::([^/@\s]*))?@)?([^/@\s:?]+)/.exec(mainSource.trim());
  if (!m) return null;
  const [, user = "", pass = "", host = ""] = m;
  const withSub = BRAND_TEMPLATES.filter((b) => b.sub);
  if (!host || withSub.length === 0) return null;
  return (
    <div className="row" style={{ gap: 6, flexWrap: "wrap", marginBottom: 4 }}>
      <span className="feat-help" style={{ alignSelf: "center" }}>
        Use my camera brand&apos;s usual one:
      </span>
      {withSub.map((b) => (
        <button
          key={b.key}
          type="button"
          className="btn btn-ghost ev-act"
          title={`Fill in ${b.label}'s standard low-res path for ${host}`}
          onClick={() => onPick(fillTemplate(b.sub as string, host, decodeURIComponent(user), decodeURIComponent(pass)))}
        >
          {b.label.split(" / ")[0]}
        </button>
      ))}
    </div>
  );
}

/// Hide the password (and blur the username) in a displayed camera URL so a
/// glance / screenshot / screen-share can't leak camera credentials. The full
/// URL stays available in the edit form, where showing it is deliberate.
function maskSource(src: string): string {
  return src.replace(/^(\w+:\/\/)([^/@\s]+)@/, (_, scheme, userinfo) => {
    const user = String(userinfo).split(":")[0];
    return `${scheme}${user}:•••@`;
  });
}

/// Plain-language recap of a recording schedule, shown live under the controls
/// so the user reads intent ("Records Mon–Fri, 22:00–06:00 (overnight)") rather
/// than decoding day chips + time pickers. Mirrors the server's window logic:
/// an absent start/end is open-ended (records from midnight / until midnight).
function scheduleSummary(s: NonNullable<DetectConfig["record_schedule"]>): string {
  const days =
    s.days.length === 0 || s.days.length === 7
      ? "every day"
      : s.days
          .slice()
          .sort((a, b) => a - b)
          .map((i) => DAY_NAMES[i])
          .join(", ");
  const start = s.start_hhmm || null;
  const end = s.end_hhmm || null;
  const when =
    start && end
      ? `${start}–${end}${end < start ? " (overnight)" : ""}`
      : start
        ? `from ${start}`
        : end
          ? `until ${end}`
          : "all day";
  return `Records ${days}, ${when}. Outside the window this camera stops recording; detection & event clips still run.`;
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
  settings,
  poseModelMissing,
  openvinoAvailable,
  clipAvailable,
  audioAvailable,
  onClose,
  onSaved,
  onError,
}: {
  camera: Camera;
  /** Global Settings (fetched once by the page), so a blank inherit-field can
   *  show the value it resolves to ("using global: 0.4") instead of leaving
   *  per-camera tuning to guesswork. Null while still loading. */
  settings: Settings | null;
  /** True when the pose model isn't downloaded, so an enabled feature doesn't
   *  silently no-op (the gitignored-pose-model case CLAUDE.md flags). */
  poseModelMissing: boolean;
  /** True when the OpenVINO EP actually runs in this build — the Accelerator
   *  dropdown enables its OpenVINO option only then (honest gating). */
  openvinoAvailable: boolean;
  /** CLIP models present — gates zone open/closed classification honestly. */
  clipAvailable: boolean;
  /** YAMNet present — the audio-detection toggle says so when it can't run. */
  audioAvailable: boolean;
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
    accelerator: camera.detect_config.accelerator ?? null,
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
    absence_hours: camera.detect_config.absence_hours ?? null,
    onvif_events: camera.detect_config.onvif_events ?? false,
    pose_detect: camera.detect_config.pose_detect ?? false,
    no_clip: camera.detect_config.no_clip ?? false,
    record_schedule: camera.detect_config.record_schedule ?? null,
    suppress_stationary: camera.detect_config.suppress_stationary ?? false,
    trigger_recording: camera.detect_config.trigger_recording ?? false,
    trigger_pre_roll_secs: camera.detect_config.trigger_pre_roll_secs ?? null,
    trigger_post_roll_secs: camera.detect_config.trigger_post_roll_secs ?? null,
    record_substream: camera.detect_config.record_substream ?? false,
    homekit_expose: camera.detect_config.homekit_expose ?? false,
    homekit_doorbell: camera.detect_config.homekit_doorbell ?? false,
  });
  const [subSource, setSubSource] = useState(camera.detect_source ?? "");
  const [saving, setSaving] = useState(false);
  // An accidental off/on of the child-height toggle shouldn't silently replace
  // a tuned value with the seed default — remember it for the session.
  const lastChildFrac = useRef(camera.detect_config.child_height_frac ?? 0.35);
  // Protect-style task-scoped tabs (Detection / Zones / Stream & recording)
  // instead of a scroll of disclosures. Every field's value lives in dc /
  // subSource (lifted state), so switching panels never loses an in-flight
  // edit even though inactive panels unmount.
  const [tab, setTab] = useState<"detect" | "zones" | "stream">("detect");
  // docs/11 P3 — the ONVIF inspector, reachable from the camera-side toggle.
  const [inspectOpen, setInspectOpen] = useState(false);

  const toast = useToast();
  const dialog = useDialog();
  // Guard the tall tuning form against a stray backdrop/Escape click discarding
  // every edit (thresholds, feature pills, zones) — snapshot the initial state,
  // and confirm on close when it's dirty. Save/onClose paths bypass the prompt.
  const initialSnapshot = useRef(JSON.stringify({ dc, subSource }));
  const confirming = useRef(false);
  const requestClose = async () => {
    const dirty = JSON.stringify({ dc, subSource }) !== initialSnapshot.current;
    if (!dirty || confirming.current) {
      if (!dirty) onClose();
      return;
    }
    confirming.current = true;
    const ok = await dialog.confirm({
      title: "Discard changes?",
      body: `You have unsaved detection-tuning changes for ${camera.name}.`,
      confirmLabel: "Discard",
      danger: true,
    });
    confirming.current = false;
    if (ok) onClose();
  };
  const save = async () => {
    if (saving) return; // patch restarts go2rtc on detect_source change — don't double-submit
    setSaving(true);
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
    } finally {
      setSaving(false);
    }
  };

  // One descriptor list drives both the "(N on)" summary count and the toggle
  // pills below, so a newly added feature flag can't drift out of the count.
  const features: { label: string; help: string; title?: string; on: boolean; toggle: () => void }[] = [
    {
      label: "PTZ autotrack",
      help: "Pan/tilt the camera to follow a tracked object.",
      on: dc.autotrack,
      toggle: () => setDc({ ...dc, autotrack: !dc.autotrack }),
    },
    {
      label: "Audio detection",
      help: audioAvailable
        ? "Listen for sounds (baby cry, bark, glass, smoke alarm…). Also required for speech-to-text — turn on transcription in Settings → Detection & AI to see what was said on event cards."
        : "⚠ Audio model (yamnet.onnx) not downloaded — this can't run until it's added (Settings → Models & capabilities).",
      on: dc.audio_detect,
      toggle: () => setDc({ ...dc, audio_detect: !dc.audio_detect }),
    },
    {
      label: "Two-way audio",
      help: "Adds a hold-to-talk button — only works on cameras that have a built-in speaker.",
      title:
        "Show a hold-to-talk button in this camera's detail view (streams your mic to the camera over WebRTC). Only works on cameras with a speaker / ONVIF backchannel.",
      on: dc.two_way_audio,
      toggle: () => setDc({ ...dc, two_way_audio: !dc.two_way_audio }),
    },
    {
      label: "Hand signals",
      help: "Offer the live hand-signal panic overlay (Signals page).",
      on: dc.gesture_detect,
      toggle: () => setDc({ ...dc, gesture_detect: !dc.gesture_detect }),
    },
    {
      label: "Camera-side detection",
      help: "Use the camera's own built-in person/vehicle/motion detection instead of the server's — it runs on the camera, so it adds no extra load here.",
      title:
        "Subscribe to this camera's ONVIF events and record what its chip detects as camera_motion / camera_tripwire / camera_intrusion / camera_person / camera_vehicle events (alarm rules match those labels). Needs ONVIF credentials (user:pass@host) in the camera source.",
      on: dc.onvif_events ?? false,
      toggle: () => setDc({ ...dc, onvif_events: !dc.onvif_events }),
    },
    {
      label: "Tamper detection",
      help: "Alert when the lens is covered, defocused, or the camera is moved.",
      title:
        "Watch this camera's optical integrity: alert when the lens is covered/blacked out, defocused, or the camera is moved/redirected.",
      on: dc.tamper_detect,
      toggle: () => setDc({ ...dc, tamper_detect: !dc.tamper_detect }),
    },
    {
      label: "Gait identification",
      help: "Attribute events by walking signature when the face isn't visible.",
      title:
        "Build a walking-signature for each person tracked here and attribute the event to an enrolled gait identity (works at distance / when the face isn't visible). Enroll on the People page.",
      on: dc.gait_identify,
      toggle: () => setDc({ ...dc, gait_identify: !dc.gait_identify }),
    },
    {
      label: "Package detection",
      help: "Alert when a parcel appears or is taken (porch piracy).",
      title:
        "Porch-piracy alerts: fire a 'package' event when a parcel-like object sits in view for a while, and 'package_removed' when it's taken. Draw the drop spot below, or watch the whole frame. Make alarm rules with label 'package' / 'package_removed'.",
      on: dc.package_detect ?? false,
      toggle: () => setDc({ ...dc, package_detect: !dc.package_detect }),
    },
  ];
  const featCount = features.filter((f) => f.on).length;

  return (
    <Modal onClose={requestClose} title={`Detection tuning — ${camera.name}`} className="modal-wide">
      <div className="tune-body">
        <Callout tone="info">
          Everything here starts on the <b>global Settings defaults</b> — customize a control to
          override it for this camera only, and reset it any time to fall back.
        </Callout>

        <div className="arm-bar tune-tabs" role="group" aria-label="Tuning sections">
          {(
            [
              ["detect", "Detection"],
              ["zones", "Zones & privacy"],
              ["stream", "Stream & recording"],
            ] as const
          ).map(([k, label]) => (
            <button
              key={k}
              type="button"
              aria-pressed={tab === k}
              className={`arm-opt ${tab === k ? "active" : ""}`}
              onClick={() => setTab(k)}
            >
              {label}
            </button>
          ))}
        </div>

        {tab === "detect" && (
          <>
        {/* 1. Detection sensitivity — the recurring false-positive tuning task. */}
        <section className="tune-sec">
          <h3 className="tune-h">
            <IconSliders size={15} /> Detection sensitivity
          </h3>
          <div className="tune-sub">
            <h4 className="tune-sub-h">Objects to detect</h4>
            <ObjectPicker
              value={dc.labels}
              globalLabels={settings?.detect_labels ?? []}
              onChange={(labels) => setDc({ ...dc, labels })}
            />
          </div>
          <div className="islider-row">
            <InheritSlider
              label="How confident before alerting"
              value={dc.min_score}
              globalValue={settings?.confidence ?? null}
              min={0.25}
              max={0.95}
              step={0.05}
              format={(v) => `${Math.round(v * 100)}% sure`}
              lowHint="More alerts"
              highHint="Fewer false alerts"
              onChange={(min_score) => setDc({ ...dc, min_score })}
            />
            <InheritSlider
              label="How much motion wakes detection"
              value={dc.motion_threshold}
              globalValue={settings?.motion_threshold ?? null}
              min={0.005}
              max={0.2}
              step={0.005}
              format={(v) => `${(v * 100).toFixed(1)}% of frame`}
              lowHint="Any flicker"
              highHint="Big movement only"
              onChange={(motion_threshold) => setDc({ ...dc, motion_threshold })}
            />
          </div>
          <MotionTuner
            cameraId={camera.id}
            cameraName={camera.name}
            threshold={dc.motion_threshold ?? settings?.motion_threshold ?? 0.02}
          />
          <div className="tune-sub">
            <h4 className="tune-sub-h">Object size filter</h4>
            <SizeFilterEditor
              cameraId={camera.id}
              cameraName={camera.name}
              minArea={dc.min_area ?? null}
              maxArea={dc.max_area ?? null}
              onChange={(min_area, max_area) => setDc({ ...dc, min_area, max_area })}
            />
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
        </section>

        {/* 2. Detection features — install-once capability toggles. */}
        <section className="tune-sec">
          <h3 className="tune-h">
            <IconLayers size={15} /> Detection features <span className="tune-count">({featCount} on)</span>
          </h3>
          <div className="feat-grid">
            {features.map((f) => (
              <Feature
                key={f.label}
                on={f.on}
                onToggle={f.toggle}
                label={f.label}
                help={f.help}
                title={f.title}
              />
            ))}
          </div>
          {dc.onvif_events && (
            <div style={{ marginTop: 8 }}>
              {/* docs/11 P3 — the only debug surface for camera-side detection
                  was a curl-only endpoint; without it "the camera never fires"
                  and "the camera fires topics we don't ingest" look identical. */}
              <button type="button" className="pill" onClick={() => setInspectOpen(true)}>
                <IconSearch size={14} /> See what this camera is saying
              </button>
            </div>
          )}
          {dc.package_detect && (
            <label className="field span-full" style={{ marginTop: 12 }}>
              Where parcels get left (optional)
              {/* docs/10 P3: the package zone was API-settable while the UI
                  claimed whole-frame only — now it's drawn like everything else. */}
              <RectZoneDraw
                cameraId={camera.id}
                cameraName={camera.name}
                label="Package drop spot"
                rect={
                  dc.package_zone && dc.package_zone.length >= 3
                    ? [
                        Math.min(...dc.package_zone.map((p) => p[0])),
                        Math.min(...dc.package_zone.map((p) => p[1])),
                        Math.max(...dc.package_zone.map((p) => p[0])),
                        Math.max(...dc.package_zone.map((p) => p[1])),
                      ]
                    : null
                }
                onRect={(r) =>
                  setDc({
                    ...dc,
                    package_zone: [
                      [r[0], r[1]],
                      [r[2], r[1]],
                      [r[2], r[3]],
                      [r[0], r[3]],
                    ],
                  })
                }
              />
              {dc.package_zone ? (
                <button
                  type="button"
                  className="btn btn-ghost ev-act"
                  style={{ alignSelf: "flex-start", marginTop: 4 }}
                  onClick={() => setDc({ ...dc, package_zone: null })}
                >
                  Watch the whole frame instead
                </button>
              ) : (
                <span className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 4 }}>
                  No spot drawn — the whole frame is watched for parcels.
                </span>
              )}
            </label>
          )}
          {dc.package_detect && (
            <label className="field span-full" style={{ marginTop: 12 }}>
              What counts as a parcel
              <LabelChips
                value={dc.package_labels ?? []}
                onChange={(package_labels) => setDc({ ...dc, package_labels })}
                catalog={["suitcase", "backpack", "handbag"]}
                emptyHint="Default: suitcase, backpack, handbag (the shapes the detector reads a parcel as)"
              />
            </label>
          )}
        </section>
          </>
        )}

        {tab === "stream" && (
          <>
        {/* 3. Stream & performance — install-once / expert knobs, off the everyday path. */}
        <section className="tune-sec">
          <h3 className="tune-h">
            <IconCctv size={15} /> Stream &amp; performance
          </h3>
          <div className="tune-grid">
            <label className="field span-full">
              Detection sub-stream
              <SubStreamTemplates mainSource={camera.source} onPick={setSubSource} />
              <input
                type="text"
                placeholder="rtsp://user:pass@cam/...subtype=1"
                value={subSource}
                onChange={(e) => setSubSource(e.target.value)}
              />
              <span className="feat-help">Low-res stream to run detection on; empty = detect on the main stream.</span>
              <StreamProbe src={subSource} />
            </label>
            <ModelOverrideField value={dc.model ?? null} onChange={(model) => setDc({ ...dc, model })} />
            <label className="field" title="Which processor runs this camera's detector. Inherit uses the global setting.">
              Accelerator
              <select
                value={
                  dc.accelerator === "openvino"
                    ? "openvino"
                    : dc.accelerator === "cpu"
                    ? "cpu"
                    : dc.accelerator === "auto"
                    ? "gpu"
                    : dc.force_cpu === null
                    ? ""
                    : dc.force_cpu
                    ? "cpu"
                    : "gpu"
                }
                onChange={(e) => {
                  const v = e.target.value;
                  // Keep the legacy force_cpu in lockstep for backward safety; an
                  // explicit accelerator (auto/cpu/openvino) is what the backend
                  // resolves on. "" inherits both from the global settings.
                  if (v === "") setDc({ ...dc, accelerator: null, force_cpu: null });
                  else if (v === "gpu") setDc({ ...dc, accelerator: "auto", force_cpu: false });
                  else if (v === "cpu") setDc({ ...dc, accelerator: "cpu", force_cpu: true });
                  else if (v === "openvino") setDc({ ...dc, accelerator: "openvino", force_cpu: false });
                }}
              >
                <option value="">Inherit global</option>
                <option value="gpu">GPU (best for this OS)</option>
                <option value="cpu">CPU</option>
                <option value="openvino" disabled={!openvinoAvailable}>
                  OpenVINO (Intel){openvinoAvailable ? "" : " — not available in this build"}
                </option>
              </select>
              {!openvinoAvailable && (
                <span className="feat-help">
                  OpenVINO (Intel iGPU/NPU) needs a special build compiled with Intel OpenVINO support; unavailable here.
                </span>
              )}
            </label>
            {/* docs/11 P2 — raw milliseconds, on the one control whose whole
                meaning is a trade-off. The global Settings version of this
                setting is already an InheritSlider with outcome-labelled ends;
                the per-camera one should not be a different kind of control. */}
            <div className="field">
              <InheritSlider
                label="How often this camera is checked"
                value={dc.poll_ms ?? null}
                globalValue={settings?.poll_ms ?? 1000}
                min={200}
                max={5000}
                step={100}
                format={(v) =>
                  v >= 1000 ? `every ${(v / 1000).toFixed(1).replace(/\.0$/, "")} s` : `every ${v} ms`
                }
                lowHint="Reacts fastest"
                highHint="Lightest on your machine"
                onChange={(v) => setDc({ ...dc, poll_ms: v })}
              />
            </div>
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
        </section>

        {/* 4. Recording & retention. */}
        <section className="tune-sec">
          <h3 className="tune-h">
            <IconFilm size={15} /> Recording &amp; retention
          </h3>
          {/* Recording mode: Continuous | Event-only | Detection-triggered.
              Mutually exclusive — selecting one clears the others' flags. The
              recorder never stops segmenting; these modes only prune harder. */}
          <div className="feat" style={{ marginBottom: 10 }}>
            <span style={{ fontSize: "var(--text-sm)", fontWeight: 600, color: "var(--text-muted)" }}>
              Recording mode
            </span>
            <div
              className="arm-bar"
              role="group"
              aria-label="Recording mode"
              style={{ margin: "4px 0 6px", flexWrap: "wrap" }}
            >
              {(
                [
                  { id: "continuous", label: "Continuous" },
                  { id: "event", label: "Event-only" },
                  { id: "trigger", label: "Detection-triggered" },
                ] as const
              ).map((m) => {
                const cur = dc.trigger_recording
                  ? "trigger"
                  : dc.event_only_recording
                    ? "event"
                    : "continuous";
                return (
                  <button
                    key={m.id}
                    type="button"
                    className={`arm-opt ${cur === m.id ? "active" : ""}`}
                    aria-pressed={cur === m.id}
                    onClick={() =>
                      setDc({
                        ...dc,
                        event_only_recording: m.id === "event",
                        trigger_recording: m.id === "trigger",
                      })
                    }
                  >
                    {m.label}
                  </button>
                );
              })}
            </div>
            <span className="feat-help">
              {dc.trigger_recording
                ? `Keeps ${dc.trigger_pre_roll_secs ?? 10}s before and ${
                    dc.trigger_post_roll_secs ?? 30
                  }s after each detection; everything else is deleted within ~a minute. Recording itself stays continuous, so the "before" footage is real. Trades storage for less history — bookmarked events are always kept.`
                : dc.event_only_recording
                  ? "Keeps footage near any detection and deletes quiet segments after a ~15-minute grace. Trades storage for less history — bookmarked events are always kept."
                  : "Keeps all footage until the age / disk retention below prunes it."}
            </span>
          </div>
          {dc.trigger_recording && (
            <div className="tune-grid" style={{ marginBottom: 10 }}>
              <label className="field">
                Keep this much BEFORE each detection
                <DurationPicker
                  value={dc.trigger_pre_roll_secs ?? 10}
                  onChange={(secs) => setDc({ ...dc, trigger_pre_roll_secs: secs })}
                  zeroLabel="Nothing before"
                  ariaLabel="Pre-roll kept before each detection"
                />
              </label>
              <label className="field">
                Keep this much AFTER each detection
                <DurationPicker
                  value={dc.trigger_post_roll_secs ?? 30}
                  onChange={(secs) => setDc({ ...dc, trigger_post_roll_secs: secs })}
                  zeroLabel="Nothing after"
                  ariaLabel="Post-roll kept after each detection"
                />
              </label>
            </div>
          )}
          <div className="feat-grid">
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
            {/* Dual-stream (P3.7): only meaningful when a detect sub-stream
                exists to record — hidden otherwise. */}
            {subSource.trim() !== "" && (
              <Feature
                on={dc.record_substream ?? false}
                onToggle={() => setDc({ ...dc, record_substream: !dc.record_substream })}
                label="Also record the low-res sub-stream"
                help="Lets you scrub fast in SD and play back in HD. Uses more disk."
                title="Record the detection sub-stream to disk alongside the main stream, so the camera view can scrub the lightweight SD copy and play the full-res HD one. Sub footage is kept by the same retention as the main stream, but never uploaded offsite."
              />
            )}
            {/* P3.4 HomeKit: only offered when the global bridge is on
                (Settings → Apple HomeKit). Off by default — a sensitive camera
                stays off HomeKit unless explicitly exposed here. */}
            {settings?.homekit_enabled && (
              <Feature
                on={dc.homekit_expose ?? false}
                onToggle={() => setDc({ ...dc, homekit_expose: !dc.homekit_expose })}
                label="Expose to HomeKit"
                help="Show this camera in the Apple Home app: live view, plus a motion sensor (via the separate “Cammy Sensors” pairing) for Home automations."
                title="Expose this camera as a HomeKit camera (live view) via the HomeKit bridge, and as a motion sensor through the separate Cammy Sensors bridge. Requires the bridge to be enabled in Settings; pair on a real Apple device."
              />
            )}
            {settings?.homekit_enabled && dc.homekit_expose && (
              <Feature
                on={dc.homekit_doorbell ?? false}
                onToggle={() => setDc({ ...dc, homekit_doorbell: !dc.homekit_doorbell })}
                label="HomeKit doorbell button"
                help="Adds a doorbell button in Home (via Cammy Sensors) that “presses” when this camera hears a doorbell chime or gets a soft trigger labeled “doorbell”."
                title="Appears in the Home app as a programmable switch (single press), not a full HomeKit doorbell — Home only accepts doorbell accessories that carry their own camera stream. Use it to trigger Home automations/notifications on a ring."
              />
            )}
          </div>
          <div className="tune-grid" style={{ marginTop: 12 }}>
            {/* docs/11 P2 — a raw number box where the global control is chips,
                and no idea what the number COSTS. The estimate below uses this
                camera's own measured write rate, because a 4K doorbell and a
                quiet side camera differ by an order of magnitude. */}
            <CamRetentionField
              cameraId={camera.id}
              value={dc.retention_days ?? null}
              globalDays={settings?.retention_days ?? null}
              onChange={(d) => setDc({ ...dc, retention_days: d })}
            />
          </div>
          {dc.record_schedule && (
            <div className="sched" style={{ marginTop: 12 }}>
              <div className="row" style={{ gap: 6, flexWrap: "wrap", marginBottom: 8 }}>
                {DAY_NAMES.map((d, i) => {
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
        </section>
          </>
        )}

        {tab === "detect" && (
          <>
        {/* 5. Residential safety & privacy — assistive, liability-sensitive. */}
        <section className="tune-sec">
          <h3 className="tune-h">
            <IconShield size={15} /> Residential safety &amp; privacy (assistive*)
          </h3>
          <Callout tone="warn" style={{ marginTop: 8 }}>
            Fall detection and child classification are <b>assistive, best-effort</b> safety aids —
            not medical devices and not guaranteed. They can miss events and must never replace
            supervision or a personal alarm.
          </Callout>
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
            <Callout tone="warn" style={{ marginTop: 10, marginBottom: 0 }}>
              Pose model not downloaded — body pose monitoring won't run until
              <code> yolov8n-pose.onnx</code> is added (see Settings → Models &amp; capabilities).
            </Callout>
          )}
          <div className="tune-grid" style={{ marginTop: 12 }}>
            <div className="field span-full">
              <div className="feat">
                <TogglePill
                  on={dc.child_height_frac != null}
                  ariaLabel="Tell children apart from adults on this camera"
                  title="Residential child calibration: a tracked person shorter than the marker counts as a 'child', enabling the child / child-alone zone rules. Assistive only — apparent height depends on camera angle and distance."
                  onClick={() => {
                    if (dc.child_height_frac != null) {
                      lastChildFrac.current = dc.child_height_frac;
                      setDc({ ...dc, child_height_frac: null });
                    } else {
                      setDc({ ...dc, child_height_frac: lastChildFrac.current });
                    }
                  }}
                >
                  Tell children apart from adults (assistive*)
                </TogglePill>
                <span className="feat-help">
                  Needed by the child / child-alone zone rules (pool &amp; crib safety).
                </span>
              </div>
              {dc.child_height_frac != null && (
                <div style={{ marginTop: 8 }}>
                  <ChildHeightEditor
                    cameraId={camera.id}
                    cameraName={camera.name}
                    frac={dc.child_height_frac}
                    onChange={(child_height_frac) => setDc({ ...dc, child_height_frac })}
                  />
                </div>
              )}
            </div>
            <label
              className="field"
              title="Inactivity watch (aging-in-place & pets): notify when this camera has seen NO person or pet for this many hours. One alert per quiet spell, cleared by the next sighting. Assistive only — absence of detections is not proof of absence of activity."
            >
              Alert if no one has been seen for…
              {/* docs/11 P2 — decimal hours with a blank-means-off placeholder.
                  The real choices are a handful of spans; state them, and make
                  "off" a choice rather than an absence. */}
              <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
                {[
                  { h: null, t: "Off" },
                  { h: 4, t: "4 hours" },
                  { h: 8, t: "8 hours" },
                  { h: 12, t: "12 hours" },
                  { h: 24, t: "a day" },
                  { h: 48, t: "2 days" },
                ].map((p) => (
                  <TogglePill
                    key={String(p.h)}
                    on={(dc.absence_hours ?? null) === p.h}
                    ariaLabel={p.h == null ? "Inactivity watch off" : `Alert after ${p.t} of no one seen`}
                    onClick={() => setDc({ ...dc, absence_hours: p.h })}
                  >
                    {p.t}
                  </TogglePill>
                ))}
                {dc.absence_hours != null && ![4, 8, 12, 24, 48].includes(dc.absence_hours) && (
                  <span className="badge accent">custom: {dc.absence_hours} h</span>
                )}
              </div>
              <span className="feat-help">
                No person or pet detected for that long → one notification per quiet spell,
                cleared by the next sighting (assistive*).
              </span>
            </label>
          </div>
        </section>
          </>
        )}

        {tab === "zones" && (
          <>
        {/* 6. Zones & privacy masks. */}
        <div className="card-head" style={{ marginTop: 8, marginBottom: 8 }}>
          <IconZone size={18} />
          <div>
            <p className="eyebrow">Detection areas</p>
            <h2 style={{ margin: 0 }}>Zones &amp; privacy masks</h2>
          </div>
        </div>
        <p className="muted" style={{ marginTop: 0 }}>
          Draw polygons on the live frame. <b style={{ color: COLORS.required }}>Watch-inside</b> zones
          only count objects inside them; <b style={{ color: COLORS.ignore }}>never-alert</b> zones drop
          objects inside; <b style={{ color: COLORS.mask }}>privacy masks</b> are blacked out before any
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
          clipAvailable={clipAvailable}
          childCalibrated={dc.child_height_frac != null}
          onNeedChildCalib={() => setTab("detect")}
        />
          </>
        )}
      </div>

      <div className="dialog-actions tune-foot">
        <button className="btn btn-ghost" onClick={requestClose} disabled={saving}>
          Cancel
        </button>
        <button className="btn btn-primary" onClick={save} disabled={saving}>
          {saving ? "Saving…" : "Save"}
        </button>
      </div>
      {inspectOpen && <OnvifInspectorModal camera={camera} onClose={() => setInspectOpen(false)} />}
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
      placeholder="No group"
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
  // Fetched once for the page and passed into TuneModal, which is remounted per
  // open — this keeps the "using global: X" hints and the pose-model-missing
  // callout without a refetch on every modal open.
  const [settings, setSettings] = useState<Settings | null>(null);
  const [poseModelMissing, setPoseModelMissing] = useState(false);
  // Whether OpenVINO (Intel iGPU/NPU) is genuinely runnable in this build, so the
  // per-camera Accelerator dropdown offers it only when it works (never a silent
  // no-op) — false out-of-the-box.
  const [openvinoAvailable, setOpenvinoAvailable] = useState(false);
  // Honest gating (docs/10 P2.1): features whose backing model is absent say
  // so at the toggle, not in a silent no-op. Default true so a failed
  // capabilities fetch never false-alarms.
  const [clipAvailable, setClipAvailable] = useState(true);
  const [audioAvailable, setAudioAvailable] = useState(true);

  usePolling(() => api.status().then(setStatus).catch(() => {}), 5000);
  useEffect(() => {
    api.settings().then(setSettings).catch(() => {});
    api.me().then((m) => setIsAdmin(m.role === "admin")).catch(() => {});
    api
      .capabilities()
      .then((r) => {
        setPoseModelMissing(!capabilityUsable(r.features.find((f) => f.key === "pose")));
        setOpenvinoAvailable(!!r.openvino);
        setClipAvailable(capabilityUsable(r.features.find((f) => f.key === "smart_search")));
        setAudioAvailable(capabilityUsable(r.features.find((f) => f.key === "audio")));
      })
      .catch(() => {});
  }, []);

  // Auto-open the "Add a camera" form once, on first seeing zero cameras — an
  // uncontrolled <details>, so the user's own toggling always wins afterwards
  // (a React-controlled `open` would force-collapse the form the instant the
  // first camera registered, mid-flow).
  const addFormRef = useRef<HTMLDetailsElement>(null);
  const autoOpened = useRef(false);
  useEffect(() => {
    if (autoOpened.current) return;
    autoOpened.current = true;
    if (cameras.length === 0 && addFormRef.current) addFormRef.current.open = true;
  }, [cameras.length]);
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
  // P3.10 offline footage import (Admin-only surface; server enforces the gate).
  const [isAdmin, setIsAdmin] = useState(false);
  const [importPath, setImportPath] = useState("");
  const [browsing, setBrowsing] = useState(false);
  const [importName, setImportName] = useState("");
  const [importBusy, setImportBusy] = useState(false);
  const [importResult, setImportResult] = useState<string | null>(null);
  const [uploading, setUploading] = useState(false);

  // Upload a clip from THIS device to the server's imports folder, then fill
  // the path field so the normal Import step runs on it. Also seeds the name
  // field from the filename when it's still empty.
  const onUploadFile = async (e: ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    setUploading(true);
    try {
      const r = await api.importUpload(file);
      setImportPath(r.path);
      if (!importName.trim()) {
        setImportName(
          file.name
            .replace(/\.[^.]+$/, "")
            .toLowerCase()
            .replace(/[^a-z0-9_-]+/g, "-")
            .replace(/^-+|-+$/g, "")
            .slice(0, 32) || "imported",
        );
      }
      toast.success(`Uploaded ${file.name} (${fmtBytes(r.bytes)}) — now click Import`);
    } catch (err) {
      onError(`Couldn't upload that file: ${errMsg(err)}`);
    } finally {
      setUploading(false);
    }
  };

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
      const deviceName = streams[0].name.replace(/ stream\d+$/, "");
      setFound(`${deviceName} — ${streams.length} stream${streams.length === 1 ? "" : "s"}`);
      // Suggest a name from what the camera calls itself; the user can edit.
      if (!name.trim() && deviceName) setName(deviceName);
    } catch (e) {
      onError(
        `Couldn't get streams from that camera automatically — check the IP, username and password, or pick its brand under "Enter the address manually". (${errMsg(e)})`,
      );
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

  const runImport = async (e: FormEvent) => {
    e.preventDefault();
    setImportBusy(true);
    setImportResult(null);
    try {
      const r = await api.importFootage(importPath.trim(), importName.trim());
      setImportResult(
        `Scanned ${r.frames_scanned} frame${r.frames_scanned === 1 ? "" : "s"}, created ${r.events_created} event${r.events_created === 1 ? "" : "s"} on “${r.camera}”.`
      );
      toast.success(`Imported ${r.events_created} events from footage`);
      setImportPath("");
      setImportName("");
      onChange();
    } catch (err) {
      onError(`Couldn't import that footage — check the file path is correct on the Cammy server and is a video file. (${errMsg(err)})`);
    } finally {
      setImportBusy(false);
    }
  };

  const add = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      await api.addCamera({
        name: slugifyName(name),
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
      <div className="card" style={{ margin: 0 }}>
        <h2>Registered</h2>
        {cameras.length === 0 ? (
          <EmptyState
            icon={<IconVideo />}
            title="No cameras yet"
            hint="Add your first camera using the form below to start recording and detection."
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
                <th title="How long the AI takes per frame and whether it uses the GPU or CPU.">Speed</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {cameras.map((cam) => {
                const s = status[String(cam.id)];
                return (
                <tr key={cam.id}>
                  <td title={cam.enabled ? (s?.last_error ?? "") : "Turned off on purpose — not a fault"}>
                    {/* A deliberately disabled camera is not a fault — show it
                        neutral, not as a red "offline". */}
                    {!cam.enabled ? (
                      <>
                        <span className="dot" aria-hidden="true" />{" "}
                        <span className="muted">disabled</span>
                      </>
                    ) : (
                      <>
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
                        {/* A broken model is not an offline camera. Say which one
                            it is, so nobody goes hunting for a network fault. */}
                        {s?.detector_error && (
                          <span
                            className="badge danger"
                            style={{ marginLeft: 6, whiteSpace: "nowrap" }}
                            title={`The AI model could not be loaded, so nothing is being detected on this camera. The camera itself is fine. Details: ${s.detector_error}`}
                          >
                            <IconAlert size={11} /> Model failed to load
                          </span>
                        )}
                        {/* Online but not writing footage. "Paused" is the
                            camera's own schedule doing its job; "not recording"
                            means footage is being lost right now. */}
                        {(() => {
                          const rec = recordState(cam, s);
                          if (rec !== "paused" && rec !== "fault") return null;
                          const hint = recordStateHint(
                            rec,
                            scheduleWindow(cam.detect_config.record_schedule),
                          );
                          return (
                            <span
                              className={`badge ${rec === "fault" ? "danger" : ""}`}
                              style={{ marginLeft: 6 }}
                              title={hint}
                            >
                              {rec === "fault" ? <IconAlert size={11} /> : null}{" "}
                              {rec === "fault" ? "not recording" : "paused"}
                            </span>
                          );
                        })()}
                      </>
                    )}
                  </td>
                  <td>
                    <NameCell cam={cam} onChange={onChange} onError={onError} />
                  </td>
                  <td
                    className="muted"
                    style={{ maxWidth: 360, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
                    title="Credentials are hidden here — edit the camera to see the full URL"
                  >
                    {maskSource(cam.source)}
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

      <div className="card" style={{ margin: 0 }}>
        <details
          ref={addFormRef}
          className="adv tune-sec"
          onToggle={(e) => {
            // Discovery-first: opening the panel scans the LAN right away, so
            // the first thing the user sees is their cameras — not a URL form.
            if ((e.currentTarget as HTMLDetailsElement).open && scanned === null && !scanning) scan();
          }}
        >
        <summary><IconVideo size={15} /> Add a camera</summary>
        <div className="row" style={{ marginBottom: 10, marginTop: 8, alignItems: "center" }}>
          {scanning ? (
            <span className="muted"><IconRadar size={15} /> Looking for cameras on your network…</span>
          ) : (
            <button type="button" className="btn btn-ghost" onClick={scan}>
              <IconRadar size={15} /> {scanned === null ? "Scan network for cameras" : "Scan again"}
            </button>
          )}
          {scanned !== null && scanned.length === 0 && !scanning && (
            <span className="muted">No cameras answered the scan. You can still add one below.</span>
          )}
          {scanned && scanned.length > 0 && !scanning && (
            <span className="muted">
              Found {scanned.length} — pick one, enter its login, and Connect:
            </span>
          )}
          {scanned?.map((c) => (
            <TogglePill
              key={c.host}
              on={ip === c.host}
              ariaLabel={`Use ${c.host}${c.name ? ` (${c.name})` : ""}`}
              onClick={() => {
                setIp(c.host);
                if (c.name && !name.trim()) setName(c.name);
              }}
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
            <IconSearch size={15} /> {busy ? "Connecting…" : "Connect"}
          </button>
          {found && (
            <span className="save-ok"><IconCheck size={14} /> {found} — ready to add below</span>
          )}
        </div>
        <form onSubmit={add} className="row">
          <label className="field">
            name
            <input
              type="text"
              placeholder="Front door"
              value={name}
              onChange={(e) => setName(e.target.value)}
              required
            />
            {name.trim() !== "" && slugifyName(name) !== name.trim() && (
              <span className="feat-help">will be saved as “{slugifyName(name) || "…"}”</span>
            )}
          </label>
          {!source && (
            <label className="field" style={{ minWidth: 260 }}>
              or enter the address manually — camera brand
              <select
                aria-label="Camera brand (fills the address pattern)"
                defaultValue=""
                onChange={(e) => {
                  const t = BRAND_TEMPLATES.find((b) => b.key === e.target.value);
                  if (!t || !ip.trim()) return;
                  setSource(fillTemplate(t.main, ip, user, pass));
                  setDetectSource(t.sub ? fillTemplate(t.sub, ip, user, pass) : "");
                }}
              >
                <option value="" disabled>
                  {ip.trim() ? "pick the brand…" : "enter the IP above first"}
                </option>
                {BRAND_TEMPLATES.map((b) => (
                  <option key={b.key} value={b.key} disabled={!ip.trim()}>
                    {b.label}
                  </option>
                ))}
              </select>
              <span className="feat-help">Builds the video address from the IP + login above.</span>
            </label>
          )}
          {source && (
            <div className="field" style={{ alignSelf: "center", minWidth: 260 }}>
              <span className="save-ok">
                <IconCheck size={14} /> video address set
                {detectSource ? " · AI will watch the camera's smaller stream (saves CPU)" : ""}
              </span>
              {/* docs/11 P2 — the address above may have come from a brand
                  template the user never typed. Prove it produces a picture
                  BEFORE the camera is added, not after it sits dark on Live. */}
              <StreamProbe src={source} auto warnBig={false} />
            </div>
          )}
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
          <button className="btn btn-primary" disabled={busy || !source.trim()} title={source.trim() ? undefined : "Connect to a camera (or pick a brand) first"}>
            Add
          </button>
          <details className="adv" style={{ flexBasis: "100%" }}>
            <summary>Advanced — edit the stream addresses</summary>
            <div className="row" style={{ marginTop: 8 }}>
              <label className="field" style={{ flex: 1, minWidth: 280 }}>
                camera address (RTSP link or other source)
                <input
                  type="text"
                  placeholder="rtsp://user:pass@192.168.1.50:554/stream1"
                  value={source}
                  onChange={(e) => setSource(e.target.value)}
                  style={{ width: "100%" }}
                />
              </label>
              <label
                className="field"
                style={{ flex: 1, minWidth: 220 }}
                title="A smaller copy of the video the AI analyzes to save CPU. Usually filled in automatically."
              >
                low-res stream for detection (optional)
                <input
                  type="text"
                  placeholder="usually filled in automatically"
                  value={detectSource}
                  onChange={(e) => setDetectSource(e.target.value)}
                  style={{ width: "100%" }}
                />
              </label>
            </div>
            <p className="muted" style={{ marginBottom: 0 }}>
              Advanced sources (<code>ffmpeg:</code>, <code>exec:</code>…) are passed to the stream
              engine verbatim.
            </p>
          </details>
        </form>
        </details>
      </div>

      {isAdmin && (
      <div className="card" style={{ margin: 0 }}>
        <details className="adv tune-sec">
        <summary><IconFilm size={15} /> Import footage</summary>
        <p className="muted" style={{ marginTop: 8 }}>
          Run detection over an existing video file (a phone clip, a dashcam recording, exported
          footage) to add its events to the archive. <b>Upload…</b> sends a file from this device
          to the Cammy server; or, for a file already on that machine, give its full path.
        </p>
        <Callout tone="info">
          The footage is imported as a <strong>virtual camera</strong>: it appears in the list but
          stays paused (it never goes live or records). Its events show up on the Events page like
          any other, searchable and filterable.
        </Callout>
        <form onSubmit={runImport} className="row" style={{ marginTop: 10 }}>
          <label className="field" style={{ flex: 1, minWidth: 320 }}>
            video file path (on the Cammy server)
            <span className="row" style={{ gap: 6 }}>
              <input
                type="text"
                placeholder="C:\\Users\\me\\Videos\\dashcam.mp4"
                value={importPath}
                onChange={(e) => setImportPath(e.target.value)}
                required
                style={{ flex: 1 }}
              />
              <button type="button" className="btn btn-ghost" onClick={() => setBrowsing(true)}>
                Browse…
              </button>
              {/* Deferred P3.10 half: upload from THIS device instead of
                  spelling a server path. Streams to <data>/imports/ and fills
                  the path field, so the Import step below stays the same. */}
              <label className={`btn btn-ghost file-btn ${uploading ? "disabled" : ""}`}>
                {uploading ? "Uploading…" : "Upload…"}
                <input
                  type="file"
                  accept="video/*,.mkv,.ts,.flv"
                  disabled={uploading}
                  onChange={onUploadFile}
                />
              </label>
            </span>
            <span className="feat-help">Sampled at about one frame per second, so a long clip takes a while.</span>
          </label>
          <label className="field" style={{ minWidth: 180 }}>
            name for this footage
            <input
              type="text"
              placeholder="dashcam-jul-16"
              value={importName}
              onChange={(e) => setImportName(e.target.value)}
              required
            />
          </label>
          <button className="btn btn-primary" disabled={importBusy || !importPath.trim() || !importName.trim()}>
            {importBusy ? "Importing…" : "Import"}
          </button>
        </form>
        {importResult && (
          <span className="save-ok" style={{ marginTop: 8 }}><IconCheck size={14} /> {importResult}</span>
        )}
        {browsing && (
          <FileBrowser
            onClose={() => setBrowsing(false)}
            onPick={(p) => {
              setImportPath(p);
              setBrowsing(false);
            }}
          />
        )}
        </details>
      </div>
      )}
      </div>

      {tuning && (
        <TuneModal
          camera={tuning}
          settings={settings}
          poseModelMissing={poseModelMissing}
          openvinoAvailable={openvinoAvailable}
          clipAvailable={clipAvailable}
          audioAvailable={audioAvailable}
          onClose={() => setTuning(null)}
          onSaved={onChange}
          onError={onError}
        />
      )}
    </>
  );
}
