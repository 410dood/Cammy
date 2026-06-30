// A4 — Notifications center: a right-slide panel listing recent activity
// (strangers, camera offline/online, anomalies, daily digests), with mark-read.

import { useEffect, useRef } from "react";
import { Notification } from "./api";
import { RelTime, EmptyState, useFocusTrap } from "./ui";
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
}: {
  notes: Notification[];
  onClose: () => void;
  onMarkRead: (id: number) => void;
  onMarkAll: () => void;
  onOpenEvent: (eventId: number) => void;
}) {
  const panelRef = useRef<HTMLDivElement>(null);
  useFocusTrap(panelRef);
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
        <div className="notif-list">
          {notes.length === 0 ? (
            <EmptyState
              icon={<IconCheck />}
              title="You're all caught up"
              hint="Strangers, camera offline/online alerts, anomalies, and daily digests show up here."
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
              return (
                <button
                  key={n.id}
                  className={`notif-item ${n.read ? "" : "unread"}`}
                  aria-label={`${n.read ? "" : "Unread — "}${n.title}${n.body ? `. ${n.body}` : ""}`}
                  onClick={() => {
                    if (!n.read) onMarkRead(n.id);
                    if (n.event_id) onOpenEvent(n.event_id);
                  }}
                >
                  <span className={`notif-ico ${tone}`}><Icon size={16} /></span>
                  <div className="notif-body">
                    <b>{n.title}</b>
                    {n.body && <div className="muted notif-text">{n.body}</div>}
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
