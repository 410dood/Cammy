// A4 — Notifications center: a right-slide panel listing recent activity
// (strangers, camera offline/online, anomalies, daily digests), with mark-read.

import { Notification } from "./api";
import { RelTime } from "./ui";
import {
  IconBell, IconX, IconWifiOff, IconWifi, IconStranger, IconSparkles,
  IconAlert, IconProps,
} from "./icons";

const KIND_ICON: Record<string, (p: IconProps) => JSX.Element> = {
  camera_offline: IconWifiOff,
  camera_online: IconWifi,
  stranger: IconStranger,
  digest: IconSparkles,
  anomaly: IconAlert,
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
  onOpenEvent: () => void;
}) {
  return (
    <div className="notif-overlay" onClick={onClose}>
      <div
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
            <div className="empty" style={{ margin: 16 }}>You're all caught up.</div>
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
                  onClick={() => {
                    if (!n.read) onMarkRead(n.id);
                    if (n.event_id) onOpenEvent();
                  }}
                >
                  <span className={`notif-ico ${tone}`}><Icon size={16} /></span>
                  <div className="notif-body">
                    <b>{n.title}</b>
                    {n.body && <div className="muted notif-text">{n.body}</div>}
                    <RelTime ts={n.ts} className="muted clock notif-time" />
                  </div>
                  {!n.read && <span className="notif-dot" aria-label="unread" />}
                </button>
              );
            })
          )}
        </div>
      </div>
    </div>
  );
}
