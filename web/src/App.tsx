import { useEffect, useState } from "react";
import { api, AppConfig, Camera, Notification } from "./api";
import NotificationsPanel from "./Notifications";
import Onboarding, { shouldOnboard } from "./Onboarding";
import Home from "./pages/Home";
import Live from "./pages/Live";
import Cameras from "./pages/Cameras";
import Alarms from "./pages/Alarms";
import Events from "./pages/Events";
import Signals from "./pages/Signals";
import Faces from "./pages/Faces";
import Recordings from "./pages/Recordings";
import FloorPlanPage from "./pages/FloorPlan";
import Family from "./pages/Family";
import Settings from "./pages/Settings";
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
} from "./icons";
import CommandPalette, { Command } from "./CommandPalette";
import { getTheme, toggleTheme, Theme } from "./theme";

const PAGES = ["Home", "Live", "Events", "Family", "Signals", "Recordings", "People", "Alarms", "Cameras", "Map", "Settings"] as const;
type Page = (typeof PAGES)[number];

const ICONS: Record<Page, (p: IconProps) => JSX.Element> = {
  Home: IconHome,
  Live: IconLive,
  Events: IconBell,
  Family: IconShield,
  Signals: IconHand,
  Recordings: IconFilm,
  People: IconUser,
  Alarms: IconSiren,
  Cameras: IconVideo,
  Map: IconMap,
  Settings: IconSettings,
};

function LoginOverlay() {
  const [user, setUser] = useState("");
  const [pw, setPw] = useState("");
  const [err, setErr] = useState("");
  const [hasUsers, setHasUsers] = useState(false);

  useEffect(() => {
    api.authStatus().then((a) => setHasUsers(a.users > 0)).catch(() => {});
  }, []);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    try {
      await api.login(pw, user.trim() || undefined);
      window.location.reload();
    } catch {
      setErr(hasUsers ? "wrong username or password" : "wrong password");
    }
  };
  return (
    <div className="modal-bg">
      <form className="card login-card" onSubmit={submit}>
        <h2 className="login-title">
          <IconLock size={18} /> Cammy
        </h2>
        <p className="muted">Log in to access this NVR remotely.</p>
        {hasUsers && (
          <input
            type="text"
            placeholder="username"
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
            value={pw}
            autoFocus={!hasUsers}
            autoComplete="current-password"
            onChange={(e) => setPw(e.target.value)}
            style={{ flex: 1 }}
          />
          <button className="btn btn-primary">Unlock</button>
        </div>
        {err && <p style={{ color: "var(--danger)" }}>{err}</p>}
      </form>
    </div>
  );
}

export default function App() {
  const [page, setPage] = useState<Page>("Home");
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [cameras, setCameras] = useState<Camera[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [locked, setLocked] = useState(false);
  const [palette, setPalette] = useState(false);
  const [focusCamera, setFocusCamera] = useState<Camera | null>(null);
  const [theme, setThemeState] = useState<Theme>(getTheme());
  const [notifOpen, setNotifOpen] = useState(false);
  const [notifs, setNotifs] = useState<Notification[]>([]);
  const [camerasLoaded, setCamerasLoaded] = useState(false);
  const [dismissedOnboard, setDismissedOnboard] = useState(false);

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
      .catch((e) => setError(String(e)));
  };

  const go = (p: Page) => {
    setPage(p);
    refresh();
  };
  const openCamera = (c: Camera) => {
    setFocusCamera(c);
    setPage("Live");
    refresh();
  };
  const flipTheme = () => setThemeState(toggleTheme());

  useEffect(() => {
    const onLocked = () => setLocked(true);
    window.addEventListener("zoomy-401", onLocked);
    api.config().then(setConfig).catch((e) => setError(String(e)));
    refresh();
    loadNotifs();
    const notifTimer = setInterval(loadNotifs, 20000);
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
      window.removeEventListener("keydown", onKey);
      clearInterval(notifTimer);
    };
  }, []);

  const commands: Command[] = [
    ...PAGES.map((p) => {
      const Icon = ICONS[p];
      return {
        id: `page-${p}`,
        label: p,
        group: "Pages",
        keywords: "go open navigate",
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
  ];

  if (locked) return <LoginOverlay />;

  return (
    <>
      <nav className="sidebar">
        <div className="brand">
          Cam<span>my</span>
        </div>
        {PAGES.map((p) => {
          const Icon = ICONS[p];
          return (
            <button
              key={p}
              className={`nav-btn ${page === p ? "active" : ""}`}
              onClick={() => {
                setPage(p);
                refresh();
              }}
              aria-current={page === p ? "page" : undefined}
            >
              <span className="nav-ico">
                <Icon size={20} />
              </span>
              <span className="nav-label">{p}</span>
            </button>
          );
        })}
        <div className="rail-tools">
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
        </div>
        <div className="foot">
          {cameras.length} camera{cameras.length === 1 ? "" : "s"} · self-hosted NVR
        </div>
      </nav>
      <main className="main">
        {error && (
          <div className="error-banner" role="alert" onClick={() => setError(null)}>
            {error} (click to dismiss)
          </div>
        )}
        {page === "Home" && (
          <Home
            cameras={cameras}
            onOpenEvents={() => go("Events")}
            onOpenCamera={openCamera}
          />
        )}
        {page === "Live" && (
          <Live
            cameras={cameras}
            config={config}
            focusCamera={focusCamera}
            onFocusHandled={() => setFocusCamera(null)}
          />
        )}
        {page === "Events" && <Events cameras={cameras} />}
        {page === "Family" && <Family cameras={cameras} />}
        {page === "Signals" && <Signals cameras={cameras} />}
        {page === "Recordings" && <Recordings cameras={cameras} />}
        {page === "People" && <Faces onError={setError} />}
        {page === "Map" && <FloorPlanPage cameras={cameras} onOpenCamera={openCamera} />}
        {page === "Alarms" && <Alarms cameras={cameras} onError={setError} />}
        {page === "Cameras" && (
          <Cameras cameras={cameras} onChange={refresh} onError={setError} />
        )}
        {page === "Settings" && <Settings onError={setError} />}
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
      {palette && <CommandPalette commands={commands} onClose={() => setPalette(false)} />}
      {notifOpen && (
        <NotificationsPanel
          notes={notifs}
          onClose={() => setNotifOpen(false)}
          onMarkRead={markRead}
          onMarkAll={markAllNotifs}
          onOpenEvent={() => {
            setNotifOpen(false);
            go("Events");
          }}
        />
      )}
    </>
  );
}
