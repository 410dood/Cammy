import { ReactNode, useEffect, useRef, useState } from "react";
import {
  api, AppConfig, Camera, Liveview, Settings, StatusMap, StreamMode,
  getStreamMode, setStreamMode,
} from "../api";
import CameraDetail from "../CameraDetail";
import LiveVideo from "../LiveVideo";
import Wall from "../Wall";
import PrivacyOverlay from "../PrivacyOverlay";
import { useToast, useDialog, EmptyState, TogglePill } from "../ui";
import {
  IconArrowUp, IconArrowDown, IconArrowLeft, IconArrowRight,
  IconPlus, IconMinus, IconExpand, IconRecDot, IconLayers, IconX, IconVideo, IconAlert,
} from "../icons";

// Humanized camera-tamper kinds (#63) for the live-tile warning chip.
const TAMPER_LABEL: Record<string, string> = {
  blackout: "Blacked out",
  defocus: "Defocused",
  scene_change: "View moved",
};
// A camera whose stream froze (online but no new frame) this many seconds behind
// the freshest camera is flagged "No signal". Using the freshest frame across all
// cameras as "now" makes this immune to client/server clock skew.
const STALE_SECS = 30;

/// Hold-to-move PTZ pad, shown only on cameras that answer ONVIF PTZ.
function PtzPad({ cameraId }: { cameraId: number }) {
  const moving = useRef(false);

  const start = (pan: number, tilt: number, zoom: number) => {
    moving.current = true;
    api.ptz(cameraId, { action: "move", pan, tilt, zoom }).catch(() => {});
  };
  const stop = () => {
    if (!moving.current) return;
    moving.current = false;
    api.ptz(cameraId, { action: "stop" }).catch(() => {});
  };

  const btn = (icon: ReactNode, label: string, pan: number, tilt: number, zoom: number) => (
    <button
      className="ptz-btn"
      aria-label={label}
      title={label}
      onPointerDown={(e) => {
        e.preventDefault();
        start(pan, tilt, zoom);
      }}
      onPointerUp={stop}
      onPointerLeave={stop}
    >
      {icon}
    </button>
  );

  return (
    <div className="ptz-pad">
      <span />
      {btn(<IconArrowUp size={17} />, "Tilt up", 0, 0.5, 0)}
      <span />
      {btn(<IconArrowLeft size={17} />, "Pan left", -0.5, 0, 0)}
      {btn(<IconArrowDown size={17} />, "Tilt down", 0, -0.5, 0)}
      {btn(<IconArrowRight size={17} />, "Pan right", 0.5, 0, 0)}
      {btn(<IconPlus size={17} />, "Zoom in", 0, 0, 0.5)}
      <span />
      {btn(<IconMinus size={17} />, "Zoom out", 0, 0, -0.5)}
    </div>
  );
}

export default function Live({
  cameras,
  config,
  focusCameraId,
}: {
  cameras: Camera[];
  config: AppConfig | null;
  /** Camera id from the `#/live/<id>` hash; opens that camera's detail view. */
  focusCameraId?: number | null;
}) {
  const [status, setStatus] = useState<StatusMap>({});
  const [ptz, setPtz] = useState<Record<number, boolean>>({});
  const [wall, setWall] = useState(false);

  // The detail view is derived from the URL hash (resolved against the loaded
  // camera list), so opening a camera, refreshing, Back/Forward and bookmarks
  // all stay in sync. Writing the hash is the single source of truth.
  const detail = focusCameraId != null ? cameras.find((c) => c.id === focusCameraId) ?? null : null;
  const showCamera = (cam: Camera) => {
    window.location.hash = `#/live/${cam.id}`;
  };
  const closeCamera = () => {
    window.location.hash = "#/live";
  };
  const [mode, setMode] = useState<StreamMode>(getStreamMode());
  const [group, setGroup] = useState<string>(() => localStorage.getItem("zoomy-live-group") || "All");
  // A6 Liveviews: saved named camera layouts, persisted in Settings.
  const toast = useToast();
  const dialog = useDialog();
  const [views, setViews] = useState<Liveview[]>([]);
  const [viewName, setViewName] = useState<string | null>(null);
  const settingsRef = useRef<Settings | null>(null);

  useEffect(() => {
    const load = () => api.status().then(setStatus).catch(() => {});
    load();
    const t = setInterval(load, 5000);
    api.settings().then((s) => {
      settingsRef.current = s;
      setViews(s.liveviews ?? []);
    }).catch(() => {});
    return () => clearInterval(t);
  }, []);

  const persistViews = async (next: Liveview[]) => {
    setViews(next);
    const s = settingsRef.current;
    if (!s) return;
    const updated = { ...s, liveviews: next };
    settingsRef.current = updated;
    try {
      await api.saveSettings(updated);
    } catch (e) {
      toast.error(`Couldn't save view: ${e}`);
    }
  };

  useEffect(() => {
    cameras.forEach((cam) => {
      if (ptz[cam.id] === undefined) {
        api
          .ptzCaps(cam.id)
          .then((r) => setPtz((p) => ({ ...p, [cam.id]: r.supported })))
          .catch(() => setPtz((p) => ({ ...p, [cam.id]: false })));
      }
    });
    // Re-probe only when the set of camera ids changes (not on every new array
    // reference from a parent re-render).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cameras.map((c) => c.id).join(",")]);

  const enabled = cameras.filter((c) => c.enabled);
  const groups = Array.from(
    new Set(enabled.map((c) => c.group).filter((g): g is string => !!g)),
  ).sort();
  const hasUngrouped = enabled.some((c) => !c.group);

  // Snap a stale selection (its cameras were removed/regrouped) back to All.
  useEffect(() => {
    if (group !== "All" && group !== "Ungrouped" && !groups.includes(group)) setGroup("All");
    if (group === "Ungrouped" && !hasUngrouped) setGroup("All");
    // NUL separator can't appear in a typed group name, so the dep key is
    // unambiguous even for names containing punctuation.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [groups.join("\u0000"), hasUngrouped]);

  const pickGroup = (g: string) => {
    setGroup(g);
    setViewName(null); // selecting a group exits any active saved view
    localStorage.setItem("zoomy-live-group", g);
  };

  const activeView = viewName ? views.find((v) => v.name === viewName) : null;
  const live = activeView
    ? enabled.filter((c) => activeView.cameras.includes(c.name))
    : enabled.filter((c) =>
        group === "All" ? true : group === "Ungrouped" ? !c.group : c.group === group,
      );

  const saveView = async () => {
    const name = (
      await dialog.prompt({
        title: "Save liveview",
        label: "Name this camera layout",
        placeholder: "e.g. Front of house",
        maxLength: 48,
      })
    )?.trim();
    if (!name) return;
    const next = [...views.filter((v) => v.name !== name), { name, cameras: live.map((c) => c.name) }];
    await persistViews(next);
    setViewName(name);
    toast.success(`Saved view “${name}”`);
  };
  const deleteView = async (name: string) => {
    if (!(await dialog.confirm({ title: `Delete view “${name}”?`, confirmLabel: "Delete", danger: true }))) return;
    await persistViews(views.filter((v) => v.name !== name));
    if (viewName === name) setViewName(null);
    toast.success("View deleted");
  };

  if (!config)
    return (
      <>
        <h1>Live</h1>
        <div className="live-grid" aria-busy="true">
          {Array.from({ length: 4 }).map((_, i) => (
            <div className="tile" key={i}>
              <span className="skeleton" style={{ position: "absolute", inset: 0, borderRadius: 0 }} />
            </div>
          ))}
        </div>
      </>
    );
  if (enabled.length === 0)
    return (
      <>
        <h1>Live</h1>
        <EmptyState
          icon={<IconVideo />}
          title="No cameras yet"
          hint={
            <>
              Add your first camera on the <b>Cameras</b> page — paste an RTSP URL or let ONVIF
              auto-discover it on your network.
            </>
          }
        />
      </>
    );

  return (
    <>
      <div className="row" style={{ alignItems: "center" }}>
        <h1 style={{ marginRight: "auto" }}>Live</h1>
        <label className="field" title="go2rtc restreams one camera connection to all viewers; pick the transport that works best on your network.">
          transport
          <select
            value={mode}
            onChange={(e) => {
              const m = e.target.value as StreamMode;
              setMode(m);
              setStreamMode(m);
            }}
          >
            <option value="webrtc">WebRTC (lowest latency)</option>
            <option value="mse">MSE (compatible, over TCP)</option>
            <option value="mjpeg">MJPEG (most compatible)</option>
          </select>
        </label>
        <button
          className="btn btn-secondary"
          style={{ alignSelf: "flex-end" }}
          title="Full-screen wall mode for a dedicated monitor"
          onClick={() => {
            setWall(true);
            document.documentElement.requestFullscreen?.().catch(() => {});
          }}
        >
          <IconExpand size={15} /> Wall
        </button>
      </div>
      <div className="row" style={{ gap: 6, flexWrap: "wrap", marginBottom: 12 }}>
        <span className="muted views-label">
          <IconLayers size={14} /> Views
        </span>
        {views.length === 0 && <span className="muted" style={{ fontSize: "var(--text-sm)" }}>none saved</span>}
        {views.map((v) =>
          viewName === v.name ? (
            // Active view: a sibling delete button beside the pill (not nested
            // inside it — an interactive element must not contain another).
            <span key={v.name} className="view-chip">
              <TogglePill on ariaLabel={`View ${v.name}`} onClick={() => setViewName(null)}>
                {v.name}
              </TogglePill>
              <button
                type="button"
                className="view-x"
                aria-label={`Delete view ${v.name}`}
                onClick={() => deleteView(v.name)}
              >
                <IconX size={11} />
              </button>
            </span>
          ) : (
            <TogglePill
              key={v.name}
              on={false}
              title="Show this saved view"
              ariaLabel={`View ${v.name}`}
              onClick={() => setViewName(v.name)}
            >
              {v.name}
            </TogglePill>
          ),
        )}
        <button
          className="btn btn-ghost ev-act"
          onClick={saveView}
          title="Save the cameras currently shown as a named view"
        >
          <IconPlus size={13} /> Save view
        </button>
      </div>
      {groups.length > 0 && (
        <div className="row" style={{ gap: 6, flexWrap: "wrap", marginBottom: 12 }}>
          {["All", ...groups, ...(hasUngrouped ? ["Ungrouped"] : [])].map((g) => (
            <TogglePill
              key={g}
              on={!viewName && group === g}
              ariaLabel={`Show ${g} cameras`}
              onClick={() => pickGroup(g)}
            >
              {g}
            </TogglePill>
          ))}
        </div>
      )}
      {live.length === 0 ? (
        <div className="empty">No cameras in “{group}”.</div>
      ) : (
      <div className="live-grid">
        {(() => {
          // "now" = freshest frame across all cameras (clock-skew-immune).
          const serverNow = Math.max(
            0,
            ...Object.values(status).map((st) => st.last_frame_ts || 0),
          );
          return live.map((cam) => {
          const s = status[String(cam.id)];
          const tamper = s?.tamper || null;
          const stale =
            !!s && s.online && !tamper && !!s.last_frame_ts && serverNow - s.last_frame_ts > STALE_SECS;
          const dotCls = !s ? "" : !s.online ? "off" : tamper || stale ? "warn" : "on";
          const alert = tamper ? TAMPER_LABEL[tamper] ?? "Tampered" : stale ? "No signal" : null;
          return (
            <div className="tile" key={cam.id}>
              <div className="label">
                <span className={`dot ${dotCls}`} /> {cam.name}
                {s?.recording && (
                  <span className="rec"><IconRecDot size={9} /> REC</span>
                )}
              </div>
              {alert && (
                <div className="tile-alert" title={tamper ? "Possible camera tampering (#63)" : "Stream frozen — camera online but no fresh frames"}>
                  <IconAlert size={13} /> {alert}
                </div>
              )}
              <LiveVideo name={cam.name} mode={mode} online={s ? s.online : undefined} />
              <PrivacyOverlay masks={cam.detect_config.privacy_masks} />
              <button
                className="expand"
                title="Open camera view"
                aria-label={`Open ${cam.name} camera view`}
                onClick={() => showCamera(cam)}
              >
                <IconExpand size={16} />
              </button>
              {ptz[cam.id] && <PtzPad cameraId={cam.id} />}
            </div>
          );
          });
        })()}
      </div>
      )}

      {detail && (
        <CameraDetail
          camera={detail}
          ptz={!!ptz[detail.id]}
          onClose={closeCamera}
        />
      )}

      {wall && (
        <Wall
          cameras={live}
          mode={mode}
          status={status}
          onClose={() => {
            setWall(false);
            if (document.fullscreenElement) document.exitFullscreen?.().catch(() => {});
          }}
        />
      )}
    </>
  );
}
