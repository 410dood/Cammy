import { useEffect, useState } from "react";
import { api, AppConfig, Camera } from "./api";
import Live from "./pages/Live";
import Cameras from "./pages/Cameras";
import Alarms from "./pages/Alarms";
import Events from "./pages/Events";
import Faces from "./pages/Faces";
import Recordings from "./pages/Recordings";
import Settings from "./pages/Settings";

const PAGES = ["Live", "Events", "Recordings", "Faces", "Alarms", "Cameras", "Settings"] as const;
type Page = (typeof PAGES)[number];

const ICONS: Record<Page, string> = {
  Live: "📺",
  Events: "🔔",
  Recordings: "🎞️",
  Faces: "👤",
  Alarms: "🚨",
  Cameras: "🎥",
  Settings: "⚙️",
};

function LoginOverlay() {
  const [pw, setPw] = useState("");
  const [err, setErr] = useState("");
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    try {
      await api.login(pw);
      window.location.reload();
    } catch {
      setErr("wrong password");
    }
  };
  return (
    <div className="modal-bg">
      <form className="card" style={{ minWidth: 320 }} onSubmit={submit}>
        <h2>🔒 ZoomyZoomyCamCam</h2>
        <p className="muted">This NVR is password-protected for remote access.</p>
        <div className="row">
          <input
            type="password"
            placeholder="password"
            value={pw}
            autoFocus
            onChange={(e) => setPw(e.target.value)}
            style={{ flex: 1 }}
          />
          <button className="primary">Unlock</button>
        </div>
        {err && <p style={{ color: "var(--danger)" }}>{err}</p>}
      </form>
    </div>
  );
}

export default function App() {
  const [page, setPage] = useState<Page>("Live");
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [cameras, setCameras] = useState<Camera[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [locked, setLocked] = useState(false);

  const refresh = () => {
    api.cameras().then(setCameras).catch((e) => setError(String(e)));
  };

  useEffect(() => {
    const onLocked = () => setLocked(true);
    window.addEventListener("zoomy-401", onLocked);
    api.config().then(setConfig).catch((e) => setError(String(e)));
    refresh();
    return () => window.removeEventListener("zoomy-401", onLocked);
  }, []);

  if (locked) return <LoginOverlay />;

  return (
    <>
      <nav className="sidebar">
        <div className="brand">
          Zoomy<span>Zoomy</span>CamCam
        </div>
        {PAGES.map((p) => (
          <button
            key={p}
            className={`nav-btn ${page === p ? "active" : ""}`}
            onClick={() => {
              setPage(p);
              refresh();
            }}
          >
            <span className="nav-ico">{ICONS[p]}</span>
            <span className="nav-label">{p}</span>
          </button>
        ))}
        <div className="foot">
          {cameras.length} camera{cameras.length === 1 ? "" : "s"} · self-hosted NVR
        </div>
      </nav>
      <main className="main">
        {error && (
          <div className="error-banner" onClick={() => setError(null)}>
            {error} (click to dismiss)
          </div>
        )}
        {page === "Live" && <Live cameras={cameras} config={config} />}
        {page === "Events" && <Events cameras={cameras} />}
        {page === "Recordings" && <Recordings cameras={cameras} />}
        {page === "Faces" && <Faces onError={setError} />}
        {page === "Alarms" && <Alarms cameras={cameras} onError={setError} />}
        {page === "Cameras" && (
          <Cameras cameras={cameras} onChange={refresh} onError={setError} />
        )}
        {page === "Settings" && <Settings onError={setError} />}
      </main>
    </>
  );
}
