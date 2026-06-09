import { useEffect, useState } from "react";
import { api } from "../api";

interface Enrolled {
  id: number;
  name: string;
  created_ts: number;
}

export default function Faces({ onError }: { onError: (e: string) => void }) {
  const [enrolled, setEnrolled] = useState<Enrolled[]>([]);
  const [unknown, setUnknown] = useState<string[]>([]);
  const [names, setNames] = useState<Record<string, string>>({});

  const load = () => {
    api.faces().then((r) => {
      setEnrolled(r.enrolled);
      setUnknown(r.unknown);
    }).catch(() => {});
  };

  useEffect(() => {
    load();
    const t = setInterval(load, 10000);
    return () => clearInterval(t);
  }, []);

  const enroll = async (file: string) => {
    const name = (names[file] || "").trim();
    if (!name) return;
    try {
      await api.enrollFace(name, file);
      setNames((n) => ({ ...n, [file]: "" }));
      load();
    } catch (e) {
      onError(String(e));
    }
  };

  const forget = async (f: Enrolled) => {
    if (!window.confirm(`Forget "${f.name}"? Their events keep the name, new ones won't match.`))
      return;
    try {
      await api.deleteFace(f.id);
      load();
    } catch (e) {
      onError(String(e));
    }
  };

  return (
    <>
      <h1>Faces</h1>

      <div className="card">
        <h2>Known people</h2>
        {enrolled.length === 0 ? (
          <p className="muted">
            Nobody enrolled yet. Name a face from the unknown gallery below — detections of that
            person will then carry their name.
          </p>
        ) : (
          <div className="row">
            {enrolled.map((f) => (
              <span key={f.id} className="pill on" style={{ padding: "6px 14px", fontSize: "0.9rem" }}>
                👤 {f.name}{" "}
                <button
                  className="danger"
                  style={{ marginLeft: 8, padding: "2px 8px" }}
                  onClick={() => forget(f)}
                >
                  forget
                </button>
              </span>
            ))}
          </div>
        )}
      </div>

      <div className="card">
        <h2>Unknown faces</h2>
        <p className="muted" style={{ marginTop: 0 }}>
          Confident face detections that didn't match anyone. Name one to enroll that person
          (a clear, frontal crop works best).
        </p>
        {unknown.length === 0 ? (
          <p className="muted">None waiting.</p>
        ) : (
          <div className="event-grid">
            {unknown.map((file) => (
              <div className="event-card" key={file} style={{ cursor: "default" }}>
                <img src={`/api/faces/unknown/${file}`} alt="unknown face" loading="lazy" />
                <div className="meta">
                  <div className="row">
                    <input
                      type="text"
                      placeholder="who is this?"
                      value={names[file] || ""}
                      onChange={(e) => setNames((n) => ({ ...n, [file]: e.target.value }))}
                      style={{ flex: 1 }}
                    />
                    <button
                      className="primary"
                      disabled={!(names[file] || "").trim()}
                      onClick={() => enroll(file)}
                    >
                      Enroll
                    </button>
                  </div>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </>
  );
}
