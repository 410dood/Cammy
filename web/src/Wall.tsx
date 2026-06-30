// C4 — Kiosk / Wall mode: a chromeless, always-on grid of live cameras with a
// clock, for a dedicated monitor. Keeps the screen awake (Wake Lock), Esc exits.

import { useEffect, useState } from "react";
import { Camera, StatusMap, StreamMode } from "./api";
import LiveVideo from "./LiveVideo";
import PrivacyOverlay from "./PrivacyOverlay";
import { IconX, IconAlert } from "./icons";

function gridCols(n: number): number {
  if (n <= 1) return 1;
  if (n <= 4) return 2;
  if (n <= 9) return 3;
  return 4;
}

const TAMPER_LABEL: Record<string, string> = {
  blackout: "Blacked out",
  defocus: "Defocused",
  scene_change: "View moved",
};

export default function Wall({
  cameras,
  mode,
  status,
  onClose,
}: {
  cameras: Camera[];
  mode: StreamMode;
  status?: StatusMap;
  onClose: () => void;
}) {
  const [clock, setClock] = useState("");

  useEffect(() => {
    const tick = () =>
      setClock(
        new Date().toLocaleTimeString([], {
          hour: "2-digit",
          minute: "2-digit",
          second: "2-digit",
        }),
      );
    tick();
    const t = setInterval(tick, 1000);
    const esc = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", esc);

    // Wake Lock keeps an always-on wall display from sleeping; re-acquire when
    // the tab becomes visible again (the lock is dropped on hide).
    let sentinel: { release?: () => Promise<void> } | null = null;
    const wl = (navigator as unknown as {
      wakeLock?: { request: (t: string) => Promise<{ release?: () => Promise<void> }> };
    }).wakeLock;
    const acquire = () => {
      if (wl?.request && document.visibilityState === "visible" && !sentinel) {
        wl.request("screen")
          .then((s) => {
            sentinel = s;
          })
          .catch(() => {});
      }
    };
    acquire();
    const onVis = () => {
      if (document.visibilityState === "visible") acquire();
      else sentinel = null;
    };
    document.addEventListener("visibilitychange", onVis);

    return () => {
      clearInterval(t);
      window.removeEventListener("keydown", esc);
      document.removeEventListener("visibilitychange", onVis);
      sentinel?.release?.().catch(() => {});
    };
  }, [onClose]);

  return (
    <div className="wall">
      {cameras.length === 0 ? (
        <div className="wall-empty">No cameras in this view.</div>
      ) : (
        <div
          className="wall-grid"
          style={{ gridTemplateColumns: `repeat(${gridCols(cameras.length)}, 1fr)` }}
        >
          {(() => {
            const st = status ?? {};
            const serverNow = Math.max(0, ...Object.values(st).map((x) => x.last_frame_ts || 0));
            return cameras.map((c) => {
              const s = st[String(c.id)];
              const tamper = s?.tamper || null;
              const stale =
                !!s && s.online && !tamper && !!s.last_frame_ts && serverNow - s.last_frame_ts > 30;
              const alert = tamper ? TAMPER_LABEL[tamper] ?? "Tampered" : stale ? "No signal" : null;
              return (
                <div className="wall-tile" key={c.id}>
                  <LiveVideo name={c.name} mode={mode} />
                  <PrivacyOverlay masks={c.detect_config.privacy_masks} />
                  <span className="wall-name">{c.name}</span>
                  {alert && (
                    <span className="wall-alert">
                      <IconAlert size={13} /> {alert}
                    </span>
                  )}
                </div>
              );
            });
          })()}
        </div>
      )}
      <div className="wall-clock">{clock}</div>
      <div className="wall-hint">Press Esc to exit</div>
      <button className="wall-exit" aria-label="Exit wall mode" onClick={onClose}>
        <IconX size={20} />
      </button>
    </div>
  );
}
