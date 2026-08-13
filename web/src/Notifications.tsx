// A4 — Notifications center: a right-slide panel listing recent activity
// (strangers, camera offline/online, anomalies, daily digests), with mark-read.

import { useEffect, useRef, useState } from "react";
import { api, Notification } from "./api";
import { RelTime, EmptyState, useFocusTrap, useToast } from "./ui";
import {
  IconBell, IconX, IconWifiOff, IconWifi, IconStranger, IconSparkles,
  IconAlert, IconCheck, IconProps,
} from "./icons";

const KIND_ICON: Record<string, (p: IconProps) => JSX.Element> = {
  camera_offline: IconWifiOff,
  camera_online: IconWifi,
  stranger: IconStranger,
  digest: IconSparkles,
  anomaly: IconAlert,
  backup: IconAlert,
  genai_error: IconAlert,
};

export default function NotificationsPanel({
  notes,
  onClose,
  onMarkRead,
  onMarkAll,
  onOpenEvent,
  onOpenCamera,
  visibleCameraIds,
}: {
  notes: Notification[];
  onClose: () => void;
  onMarkRead: (id: number) => void;
  onMarkAll: () => void;
  onOpenEvent: (eventId: number) => void;
  onOpenCamera: (cameraId: number) => void;
  /** Camera ids the current user can see — a notification only deep-links to a
   * camera the viewer actually has (a scoped user's list won't include it). */
  visibleCameraIds: Set<number>;
}) {
  const panelRef = useRef<HTMLDivElement>(null);
  useFocusTrap(panelRef);
  const toast = useToast();
  // Global timed snooze (deferred-inventory): unix ts while active, null when
  // not. Loaded on open; setting it round-trips through the server so the
  // shown deadline is the one that will actually be honored.
  const [snoozeUntil, setSnoozeUntil] = useState<number | null>(null);
  useEffect(() => {
    api.snooze().then((r) => setSnoozeUntil(r.until)).catch(() => {});
  }, []);
  const snooze = async (minutes: number) => {
    try {
      const r = await api.setSnooze(minutes);
      setSnoozeUntil(r.until);
      if (r.until == null) toast.success("Alerts back on");
      else
        toast.success(
          `Alerts snoozed until ${new Date(r.until * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}`,
        );
    } catch (e) {
      toast.error(`Couldn't change the snooze: ${e}`);
    }
  };
  // Move focus into the panel on open so the trap has somewhere to start and
  // keyboard/screen-reader users land inside it.
  useEffect(() => {
    panelRef.current?.focus();
  }, []);
  // Escape closes the panel (the slide-in overlay otherwise had no keyboard exit).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="notif-overlay" onClick={onClose}>
      <div
        ref={panelRef}
        tabIndex={-1}
        className="notif-panel"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label="Notifications"
      >
        <div className="notif-head">
          <h2><IconBell size={16} /> Notifications</h2>
          <div className="spacer" />
          {notes.some((n) => !n.read) && (
            <button className="btn btn-ghost ev-act" onClick={onMarkAll}>Mark all read</button>
          )}
          <button className="icon-btn" aria-label="Close" onClick={onClose}>
            <IconX size={18} />
          </button>
        </div>
        <div className="notif-snooze">
          {snoozeUntil != null ? (
            <>
              <span className="muted" style={{ fontSize: "var(--text-sm)" }}>
                Phone &amp; email alerts snoozed until{" "}
                <b>
                  {new Date(snoozeUntil * 1000).toLocaleTimeString([], {
                    hour: "numeric",
                    minute: "2-digit",
                  })}
                </b>{" "}
                — urgent (duress) alerts still come through, and everything still lands here.
              </span>
              <button type="button" className="btn btn-ghost ev-act" onClick={() => snooze(0)}>
                Resume alerts
              </button>
            </>
          ) : (
            <>
              <span className="muted" style={{ fontSize: "var(--text-sm)" }}>
                Expecting activity (mowing, a party)? Quiet your phone for a bit:
              </span>
              {[30, 60, 480].map((m) => (
                <button
                  key={m}
                  type="button"
                  className="btn btn-ghost ev-act"
                  onClick={() => snooze(m)}
                >
                  {m < 60 ? `${m} min` : `${m / 60} h`}
                </button>
              ))}
            </>
          )}
        </div>
        <div className="notif-list">
          {notes.length === 0 ? (
            <EmptyState
              icon={<IconCheck />}
              title="You're all caught up"
              hint="Strangers, cameras going offline or coming back, unusual activity, and daily summaries show up here."
            />
          ) : (
            notes.map((n) => {
              const Icon = KIND_ICON[n.kind] ?? IconBell;
              const tone =
                n.kind === "camera_offline" || n.kind === "anomaly"
                  ? "warn"
                  : n.kind === "stranger"
                  ? "warn"
                  : n.kind === "camera_online"
                  ? "ok"
                  : "";
              // Where does clicking this go? An event notification opens the
              // event; a camera notification with no event (offline/online,
              // tamper, absence) deep-links to that camera's live view — but
              // only if the viewer can actually see that camera.
              const camLink =
                !n.event_id && n.camera_id != null && visibleCameraIds.has(n.camera_id);
              return (
                <button
                  key={n.id}
                  className={`notif-item ${n.read ? "" : "unread"}`}
                  aria-label={`${n.read ? "" : "Unread. "}${n.title}${n.body ? `. ${n.body}` : ""}${
                    n.event_id ? ". Opens the event." : camLink ? ". Opens the camera." : ""
                  }`}
                  onClick={() => {
                    if (!n.read) onMarkRead(n.id);
                    if (n.event_id) onOpenEvent(n.event_id);
                    else if (camLink) onOpenCamera(n.camera_id!);
                  }}
                >
                  <span className={`notif-ico ${tone}`}><Icon size={16} /></span>
                  <div className="notif-body">
                    <b>{n.title}</b>
                    {n.body && (
                      // Health errors carry a raw URL/status tail after the em
                      // dash — show the plain clause, full text on hover.
                      <div className="muted notif-text" title={/https?:\/\//.test(n.body) ? n.body : undefined}>
                        {/https?:\/\//.test(n.body) ? n.body.split(" — ")[0] : n.body}
                      </div>
                    )}
                    <RelTime ts={n.ts} className="muted clock notif-time" />
                  </div>
                  {!n.read && <span className="notif-dot" aria-hidden="true" />}
                </button>
              );
            })
          )}
        </div>
      </div>
    </div>
  );
}
