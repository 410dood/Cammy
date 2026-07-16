import { useEffect, useRef, useState, lazy, Suspense } from "react";
import { api, AppConfig, Camera, Notification } from "./api";
import { useFocusTrap, useToast } from "./ui";
import NotificationsPanel from "./Notifications";
import Onboarding, { shouldOnboard } from "./Onboarding";
import Home from "./pages/Home";
import Live from "./pages/Live";
import Events from "./pages/Events";
// Heavier / rarely-first pages are code-split so the entry bundle (Home/Live/
// Events) stays small — a remote or mobile visitor over a home-upload link isn't
// forced to download the ZoneEditor canvas, Settings' ~20 cards, or the Insights
// charts before the first paint.
const Cameras = lazy(() => import("./pages/Cameras"));
const Alarms = lazy(() => import("./pages/Alarms"));
const Signals = lazy(() => import("./pages/Signals"));
const Faces = lazy(() => import("./pages/Faces"));
const Recordings = lazy(() => import("./pages/Recordings"));
const FloorPlanPage = lazy(() => import("./pages/FloorPlan"));
const Family = lazy(() => import("./pages/Family"));
const Insights = lazy(() => import("./pages/Insights"));
const Settings = lazy(() => import("./pages/Settings"));
import { LicenseBanner } from "./License";
import {
  IconLive,
  IconBell,
  IconHand,
  IconFilm,
  IconUser,
  IconSiren,
  IconVideo,
  IconSettings,
  IconLock,
  IconProps,
  IconSun,
  IconMoon,
  IconCommand,
  IconSearch,
  IconHome,
  IconMap,
  IconShield,
  IconGrid,
  IconRadar,
  IconSparkles,
  IconChart,
} from "./icons";
import CommandPalette, { Command } from "./CommandPalette";
import { getTheme, toggleTheme, Theme } from "./theme";

const PAGES = ["Home", "Live", "Events", "Family", "Signals", "Recordings", "People", "Insights", "Alarms", "Cameras", "Map", "Settings"] as const;
type Page = (typeof PAGES)[number];

// Display labels: the route key stays terse (and drives the Page union), but the
// nav/More/palette can read more clearly — e.g. "Signals" is ambiguous for what
// is a silent hand-signal panic button.
const LABELS: Record<Page, string> = {
  Home: "Home",
  Live: "Live",
  Events: "Events",
  Family: "Family",
  Signals: "Hand signals",
  Recordings: "Recordings",
  People: "People",
  Insights: "Insights",
  Alarms: "Alarms",
  Cameras: "Cameras",
  Map: "Map",
  Settings: "Settings",
};

// On mobile the bottom tab bar can't hold 11 tabs, so only these four show as
// tabs; the rest live behind a "More" overflow sheet. (Desktop shows them all.)
const MOBILE_PRIMARY: readonly Page[] = ["Home", "Live", "Events", "Recordings"];

// Desktop rail grouping — three labeled sections with hairline dividers so the
// 11 tabs read as an organized hierarchy rather than a flat wall. The "Monitor"
// group is exactly MOBILE_PRIMARY (same order), so on mobile (where the labels +
// dividers are hidden) the bottom tab bar is unchanged.
const NAV_GROUPS: { label: string; pages: readonly Page[] }[] = [
  { label: "Monitor", pages: ["Home", "Live", "Events", "Recordings"] },
  { label: "Detections", pages: ["Family", "Signals", "People", "Insights"] },
  { label: "Configure", pages: ["Alarms", "Cameras", "Map", "Settings"] },
];

const ICONS: Record<Page, (p: IconProps) => JSX.Element> = {
  Home: IconHome,
  Live: IconLive,
  Events: IconRadar,
  Family: IconShield,
  Signals: IconHand,
  Recordings: IconFilm,
  People: IconUser,
  Insights: IconChart,
  Alarms: IconSiren,
  Cameras: IconVideo,
  Map: IconMap,
  Settings: IconSettings,
};

// ── Hash routing ───────────────────────────────────────────────────────────
// The URL hash mirrors the current page, so a refresh keeps your place, the
// browser Back/Forward buttons work, and pages are bookmarkable. `#/events/<id>`
// is a deep link that opens a specific event (e.g. from a notification).
function pageHash(p: Page): string {
  return `#/${p.toLowerCase()}`;
}
function parseHash(): { page: Page; eventId?: number; cameraId?: number } {
  const raw = window.location.hash.replace(/^#\/?/, "");
  const [seg, arg] = raw.split("/");
  const page = PAGES.find((p) => p.toLowerCase() === seg.toLowerCase()) ?? "Home";
  const eventId = page === "Events" && arg ? Number(arg) || undefined : undefined;
  // `#/live/<id>` deep-links a camera's detail view (refresh / Back / bookmark).
  const cameraId = page === "Live" && arg ? Number(arg) || undefined : undefined;
  return { page, eventId, cameraId };
}

function LoginOverlay() {
  const [user, setUser] = useState("");
  const [pw, setPw] = useState("");
  const [otp, setOtp] = useState("");
  const [needOtp, setNeedOtp] = useState(false);
  const [err, setErr] = useState("");
  const [hasUsers, setHasUsers] = useState(false);

  useEffect(() => {
    api.authStatus().then((a) => setHasUsers(a.users > 0)).catch(() => {});
  }, []);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setErr("");
    try {
      const res = await api.login(pw, user.trim() || undefined, needOtp ? otp.trim() : undefined);
      if (res.mfa_required) {
        // Password accepted; this credential needs a second factor.
        setNeedOtp(true);
        return;
      }
      window.location.reload();
    } catch {
      setErr(
        needOtp
          ? "wrong code — try a current authenticator code or a recovery code"
          : hasUsers
          ? "wrong username or password"
          : "wrong password"
      );
    }
  };
  return (
    <div className="modal-bg">
      <form
        className="card login-card"
        onSubmit={submit}
        role="dialog"
        aria-modal="true"
        aria-labelledby="login-title"
      >
        <h2 className="login-title" id="login-title">
          <IconLock size={18} /> Cammy
        </h2>
        {!needOtp ? (
          <>
            <p className="muted">Log in to access this NVR remotely.</p>
            {hasUsers && (
              <input
                type="text"
                placeholder="username"
                aria-label="Username"
                value={user}
                autoFocus
                autoComplete="username"
                onChange={(e) => setUser(e.target.value)}
                style={{ width: "100%", marginBottom: 8 }}
              />
            )}
            <div className="row">
              <input
                type="password"
                placeholder="password"
                aria-label="Password"
                aria-invalid={!!err || undefined}
                aria-describedby={err ? "login-err" : undefined}
                value={pw}
                autoFocus={!hasUsers}
                autoComplete="current-password"
                onChange={(e) => setPw(e.target.value)}
                style={{ flex: 1 }}
              />
              <button className="btn btn-primary">Unlock</button>
            </div>
          </>
        ) : (
          <>
            <p className="muted">
              Two-factor is on. Enter the 6-digit code from your authenticator app, or a recovery
              code.
            </p>
            <div className="row">
              <input
                type="text"
                inputMode="numeric"
                placeholder="123456"
                aria-label="Authentication code"
                aria-invalid={!!err || undefined}
                aria-describedby={err ? "login-err" : undefined}
                value={otp}
                autoFocus
                autoComplete="one-time-code"
                onChange={(e) => setOtp(e.target.value.replace(/\s/g, ""))}
                style={{ flex: 1 }}
              />
              <button className="btn btn-primary">Verify</button>
            </div>
            <button
              type="button"
              className="btn btn-ghost"
              style={{ marginTop: 8 }}
              onClick={() => {
                setNeedOtp(false);
                setOtp("");
                setErr("");
              }}
            >
              Back
            </button>
          </>
        )}
        {err && <p id="login-err" role="alert" style={{ color: "var(--danger)" }}>{err}</p>}
      </form>
    </div>
  );
}

// Mobile "More" overflow sheet, extracted so it can own its own focus trap,
// focus-on-open, focus-restore-on-close, and Escape handler (an inline element
// can't call hooks). role=dialog + aria-modal match the other overlays.
function MoreSheet({
  page,
  onClose,
  onGo,
}: {
  page: Page;
  onClose: () => void;
  onGo: (p: Page) => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useFocusTrap(ref);
  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null;
    ref.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      if (prev && prev.isConnected && document.activeElement !== prev) prev.focus?.();
    };
  }, [onClose]);
  return (
    <div className="more-overlay" onClick={onClose}>
      <div
        ref={ref}
        tabIndex={-1}
        className="more-sheet"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label="More pages"
      >
        <div className="more-grid">
          {PAGES.filter((p) => !MOBILE_PRIMARY.includes(p)).map((p) => {
            const Icon = ICONS[p];
            return (
              <button
                key={p}
                className={`more-item ${page === p ? "active" : ""}`}
                onClick={() => onGo(p)}
                aria-current={page === p ? "page" : undefined}
              >
                <Icon size={22} />
                <span>{LABELS[p]}</span>
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );
}

/** Turn a raw fetch/JS error into something a non-technical user can act on.
 *  A bare "TypeError: Failed to fetch" reads like a crash; it usually just means
 *  the server isn't reachable. */
function friendlyError(e: unknown): string {
  const raw = e instanceof Error ? e.message : String(e);
  if (/failed to fetch|networkerror|load failed/i.test(raw)) {
    return "Can't reach the Cammy server — check that it's running, then click to retry.";
  }
  return raw;
}

export default function App() {
  const [page, setPage] = useState<Page>(() => parseHash().page);
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [cameras, setCameras] = useState<Camera[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [locked, setLocked] = useState(false);
  const [palette, setPalette] = useState(false);
  const [focusCameraId, setFocusCameraId] = useState<number | null>(() => parseHash().cameraId ?? null);
  // Seed from the hash like focusCameraId, so a bookmarked / pasted
  // `#/events/<id>` link opens the event viewer on a fresh page load too —
  // not only after an in-app hashchange.
  const [focusEvent, setFocusEvent] = useState<number | null>(() => parseHash().eventId ?? null);
  const [theme, setThemeState] = useState<Theme>(getTheme());
  const [notifOpen, setNotifOpen] = useState(false);
  const [moreOpen, setMoreOpen] = useState(false);
  const [notifs, setNotifs] = useState<Notification[]>([]);
  const [camerasLoaded, setCamerasLoaded] = useState(false);
  const [dismissedOnboard, setDismissedOnboard] = useState(false);
  const toast = useToast();

  const loadNotifs = () => api.notifications({ limit: 50 }).then(setNotifs).catch(() => {});
  const unread = notifs.filter((n) => !n.read).length;
  const markRead = async (id: number) => {
    setNotifs((ns) => ns.map((n) => (n.id === id ? { ...n, read: true } : n)));
    await api.markNotificationRead(id).catch(() => {});
  };
  const markAllNotifs = async () => {
    setNotifs((ns) => ns.map((n) => ({ ...n, read: true })));
    await api.markAllNotificationsRead().catch(() => {});
  };

  const refresh = () => {
    api
      .cameras()
      .then((c) => {
        setCameras(c);
        setCamerasLoaded(true);
      })
      .catch((e) => setError(friendlyError(e)));
  };

  // Navigate by writing the hash; the single hashchange listener applies it to
  // state — so UI clicks, Back/Forward, and manual hash edits share one path and
  // can't drive a setPage↔hashchange loop.
  const navigate = (hash: string) => {
    // Same hash won't fire hashchange — nothing to re-apply. (Cameras no longer
    // refetch per-navigation; they load once on mount + on Cameras-page edits.)
    if (window.location.hash !== hash) window.location.hash = hash;
  };
  const go = (p: Page) => navigate(pageHash(p));
  const openCamera = (c: Camera) => navigate(`#/live/${c.id}`);
  const openEvent = (eventId: number) => navigate(`#/events/${eventId}`);
  const flipTheme = () => setThemeState(toggleTheme());

  useEffect(() => {
    const onLocked = () => setLocked(true);
    window.addEventListener("zoomy-401", onLocked);

    // Keep state in sync with the URL: applies the initial hash (incl. an event
    // deep-link) and reacts to Back/Forward + manual hash edits.
    const applyHash = () => {
      const { page: p, eventId, cameraId } = parseHash();
      setPage(p);
      if (eventId != null) setFocusEvent(eventId);
      // Camera detail is fully URL-driven: `#/live/<id>` opens it, `#/live` (or any
      // other page) closes it. Resolution against the camera list happens in Live.
      setFocusCameraId(cameraId ?? null);
    };
    applyHash();
    window.addEventListener("hashchange", applyHash);
    refresh(); // cameras load once on mount; Cameras-page edits call refresh() again

    api.config().then(setConfig).catch((e) => setError(friendlyError(e)));
    loadNotifs();
    // Pause the always-on notification poll while the tab is backgrounded.
    const notifTimer = setInterval(() => { if (!document.hidden) loadNotifs(); }, 20000);
    // Cmd/Ctrl-K toggles the command palette.
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && (e.key === "k" || e.key === "K")) {
        e.preventDefault();
        setPalette((v) => !v);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("zoomy-401", onLocked);
      window.removeEventListener("hashchange", applyHash);
      window.removeEventListener("keydown", onKey);
      clearInterval(notifTimer);
    };
  }, []);

  const commands: Command[] = [
    ...PAGES.map((p) => {
      const Icon = ICONS[p];
      return {
        id: `page-${p}`,
        label: LABELS[p],
        group: "Pages",
        keywords: `go open navigate ${p === "Signals" ? "hand gesture panic signals" : ""}`,
        icon: <Icon size={16} />,
        run: () => go(p),
      };
    }),
    ...cameras.map((c) => ({
      id: `cam-${c.id}`,
      label: c.name,
      hint: "Open camera",
      group: "Cameras",
      keywords: `camera live ${c.group ?? ""}`,
      icon: <IconVideo size={16} />,
      run: () => openCamera(c),
    })),
    {
      id: "action-theme",
      label: theme === "light" ? "Switch to dark theme" : "Switch to light theme",
      group: "Actions",
      keywords: "theme dark light appearance",
      icon: theme === "light" ? <IconMoon size={16} /> : <IconSun size={16} />,
      run: flipTheme,
    },
    {
      id: "action-search",
      label: "Search events",
      group: "Actions",
      keywords: "find smart search",
      icon: <IconSearch size={16} />,
      run: () => go("Events"),
    },
    {
      id: "action-arm-home",
      label: "Arm — Home",
      group: "Actions",
      keywords: "arm home mode security",
      icon: <IconHome size={16} />,
      run: () =>
        api.arm("home").then(() => toast.success("Armed — Home")).catch((e) => toast.error(String(e))),
    },
    {
      id: "action-arm-away",
      label: "Arm — Away",
      group: "Actions",
      keywords: "arm away mode security",
      icon: <IconShield size={16} />,
      run: () =>
        api.arm("away").then(() => toast.success("Armed — Away")).catch((e) => toast.error(String(e))),
    },
    {
      id: "action-disarm",
      label: "Disarm",
      group: "Actions",
      keywords: "disarm off mode security pause",
      icon: <IconLock size={16} />,
      run: () =>
        api.arm("disarmed").then(() => toast.success("System disarmed")).catch((e) => toast.error(String(e))),
    },
    {
      id: "action-add-camera",
      label: "Add a camera",
      group: "Actions",
      keywords: "add new camera setup onvif rtsp",
      icon: <IconVideo size={16} />,
      run: () => go("Cameras"),
    },
    {
      id: "action-run-digest",
      label: "Generate daily digest",
      group: "Actions",
      keywords: "digest summary recap report",
      icon: <IconSparkles size={16} />,
      run: () =>
        api.runDigest().then(() => toast.success("Digest generated — see Home")).catch((e) => toast.error(String(e))),
    },
  ];

  if (locked) return <LoginOverlay />;

  // Shared by the desktop rail (.rail-tools) and the mobile top bar (.topbar) —
  // on mobile the sidebar collapses to a bottom tab bar, so without this the
  // bell / command palette / theme toggle would be unreachable.
  const toolButtons = (
    <>
      <button
        className="icon-btn rail-bell"
        title="Notifications"
        aria-label={`Notifications${unread ? `, ${unread} unread` : ""}`}
        onClick={() => setNotifOpen(true)}
      >
        <IconBell size={17} />
        {unread > 0 && <span className="notif-badge">{unread > 9 ? "9+" : unread}</span>}
      </button>
      <button
        className="icon-btn"
        title="Command palette (Ctrl/⌘ K)"
        aria-label="Open command palette"
        onClick={() => setPalette(true)}
      >
        <IconCommand size={17} />
      </button>
      <button
        className="icon-btn"
        title={theme === "light" ? "Switch to dark theme" : "Switch to light theme"}
        aria-label="Toggle color theme"
        onClick={flipTheme}
      >
        {theme === "light" ? <IconMoon size={17} /> : <IconSun size={17} />}
      </button>
    </>
  );

  return (
    <>
      <nav className="sidebar">
        <div className="brand">
          Cam<span>my</span>
        </div>
        {NAV_GROUPS.map((grp) => (
          <div className="nav-group" key={grp.label} role="group" aria-label={grp.label}>
            <div className="nav-group-label">{grp.label}</div>
            {grp.pages.map((p) => {
              const Icon = ICONS[p];
              return (
                <button
                  key={p}
                  className={`nav-btn ${page === p ? "active" : ""} ${
                    MOBILE_PRIMARY.includes(p) ? "" : "nav-secondary"
                  }`}
                  onClick={() => go(p)}
                  aria-current={page === p ? "page" : undefined}
                >
                  <span className="nav-ico">
                    <Icon size={20} />
                  </span>
                  <span className="nav-label">{LABELS[p]}</span>
                </button>
              );
            })}
          </div>
        ))}
        {/* Mobile-only overflow tab; CSS hides it on desktop (where all tabs show). */}
        <button
          className={`nav-btn nav-more ${!MOBILE_PRIMARY.includes(page) ? "active" : ""}`}
          onClick={() => setMoreOpen(true)}
          aria-haspopup="dialog"
        >
          <span className="nav-ico">
            <IconGrid size={20} />
          </span>
          <span className="nav-label">More</span>
        </button>
        <div className="rail-tools">{toolButtons}</div>
        <div className="foot">
          {cameras.length} camera{cameras.length === 1 ? "" : "s"} · self-hosted NVR
        </div>
      </nav>
      <header className="topbar">
        <span className="topbar-brand">
          Cam<span>my</span>
        </span>
        <span className="spacer" />
        {toolButtons}
      </header>
      <main className="main">
        {error && (
          <div className="error-banner" role="alert" onClick={() => setError(null)} title="Click to dismiss">
            {error}
          </div>
        )}
        <LicenseBanner />
        <Suspense fallback={<div className="skeleton" style={{ height: 260, borderRadius: 12, marginTop: 8 }} aria-busy="true" aria-label="Loading page" />}>
          {page === "Home" && (
            <Home
              cameras={cameras}
              onOpenEvents={() => go("Events")}
              onOpenCamera={openCamera}
              onOpenEvent={openEvent}
            />
          )}
          {page === "Live" && (
            <Live cameras={cameras} config={config} focusCameraId={focusCameraId} />
          )}
          {page === "Events" && (
            <Events
              cameras={cameras}
              focusEventId={focusEvent}
              onFocusHandled={() => setFocusEvent(null)}
            />
          )}
          {page === "Family" && <Family cameras={cameras} onGo={go} />}
          {page === "Signals" && <Signals cameras={cameras} />}
          {page === "Recordings" && <Recordings cameras={cameras} />}
          {page === "People" && <Faces onError={setError} />}
          {page === "Insights" && <Insights onError={setError} />}
          {page === "Map" && <FloorPlanPage cameras={cameras} onOpenCamera={openCamera} />}
          {page === "Alarms" && <Alarms cameras={cameras} onError={setError} />}
          {page === "Cameras" && (
            <Cameras cameras={cameras} onChange={refresh} onError={setError} />
          )}
          {page === "Settings" && <Settings onError={setError} />}
        </Suspense>
      </main>
      {camerasLoaded && cameras.length === 0 && !dismissedOnboard && shouldOnboard() && (
        <Onboarding
          onAddCamera={() => {
            setDismissedOnboard(true);
            go("Cameras");
          }}
          onClose={() => setDismissedOnboard(true)}
        />
      )}
      {moreOpen && (
        <MoreSheet
          page={page}
          onClose={() => setMoreOpen(false)}
          onGo={(p) => {
            go(p);
            setMoreOpen(false);
          }}
        />
      )}
      {palette && <CommandPalette commands={commands} onClose={() => setPalette(false)} />}
      {notifOpen && (
        <NotificationsPanel
          notes={notifs}
          onClose={() => setNotifOpen(false)}
          onMarkRead={markRead}
          onMarkAll={markAllNotifs}
          onOpenEvent={(eventId) => {
            setNotifOpen(false);
            openEvent(eventId);
          }}
        />
      )}
    </>
  );
}
