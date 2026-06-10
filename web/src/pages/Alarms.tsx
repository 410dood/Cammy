import { FormEvent, useEffect, useState } from "react";
import { api, AlarmRule, Camera } from "../api";

const LABELS = ["person", "car", "truck", "bus", "bicycle", "motorcycle", "dog", "cat"];

export default function Alarms({
  cameras,
  onError,
}: {
  cameras: Camera[];
  onError: (e: string) => void;
}) {
  const [rules, setRules] = useState<AlarmRule[]>([]);
  const [name, setName] = useState("");
  const [cameraId, setCameraId] = useState<number | "">("");
  const [label, setLabel] = useState("");
  const [faceLike, setFaceLike] = useState("");
  const [plateLike, setPlateLike] = useState("");
  const [action, setAction] = useState<"webhook" | "mqtt">("webhook");
  const [target, setTarget] = useState("");

  const load = () => {
    api.alarms().then(setRules).catch(() => {});
  };
  useEffect(load, []);

  const add = async (e: FormEvent) => {
    e.preventDefault();
    try {
      await api.addAlarm({
        name: name.trim(),
        enabled: true,
        camera_id: cameraId === "" ? null : cameraId,
        label: label || null,
        face_like: faceLike.trim() || null,
        plate_like: plateLike.trim() || null,
        min_score: 0,
        action,
        target: target.trim(),
      });
      setName("");
      setTarget("");
      setFaceLike("");
      setPlateLike("");
      load();
    } catch (err) {
      onError(String(err));
    }
  };

  const describe = (r: AlarmRule) => {
    const conds = [
      r.camera_id != null
        ? `camera ${cameras.find((c) => c.id === r.camera_id)?.name ?? r.camera_id}`
        : "any camera",
      r.label ?? "any object",
      r.face_like ? `face ~ "${r.face_like}"` : null,
      r.plate_like ? `plate ~ "${r.plate_like}"` : null,
    ].filter(Boolean);
    return conds.join(" · ");
  };

  return (
    <>
      <h1>Alarm Manager</h1>

      <div className="card">
        <h2>New rule — when this happens…</h2>
        <form onSubmit={add}>
          <div className="row" style={{ marginBottom: 12 }}>
            <label className="field">
              rule name
              <input type="text" value={name} onChange={(e) => setName(e.target.value)} required placeholder="person at the front door" />
            </label>
            <label className="field">
              camera
              <select value={cameraId} onChange={(e) => setCameraId(e.target.value === "" ? "" : Number(e.target.value))}>
                <option value="">any</option>
                {cameras.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </select>
            </label>
            <label className="field">
              object
              <select value={label} onChange={(e) => setLabel(e.target.value)}>
                <option value="">any</option>
                {LABELS.map((l) => (
                  <option key={l} value={l}>
                    {l}
                  </option>
                ))}
              </select>
            </label>
            <label className="field">
              face contains (optional)
              <input type="text" value={faceLike} onChange={(e) => setFaceLike(e.target.value)} placeholder="any face name" />
            </label>
            <label className="field">
              plate contains (optional)
              <input type="text" value={plateLike} onChange={(e) => setPlateLike(e.target.value)} placeholder="any plate" />
            </label>
          </div>
          <div className="row">
            <span className="muted">…do this:</span>
            <select value={action} onChange={(e) => setAction(e.target.value as "webhook" | "mqtt")}>
              <option value="webhook">POST webhook</option>
              <option value="mqtt">publish MQTT</option>
            </select>
            <input
              type="text"
              style={{ flex: 1, minWidth: 280 }}
              value={target}
              onChange={(e) => setTarget(e.target.value)}
              required
              placeholder={action === "webhook" ? "https://… (receives the event JSON)" : "topic suffix → zoomy/alarms/<suffix>"}
            />
            <button className="primary">Create rule</button>
          </div>
        </form>
      </div>

      <div className="card">
        <h2>Rules</h2>
        {rules.length === 0 ? (
          <p className="muted">No rules yet. Rules fire actions the moment a matching event is detected.</p>
        ) : (
          <div className="table-scroll">
          <table>
            <thead>
              <tr>
                <th>Rule</th>
                <th>When</th>
                <th>Then</th>
                <th>Active</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {rules.map((r) => (
                <tr key={r.id}>
                  <td>
                    <b>{r.name}</b>
                  </td>
                  <td className="muted">{describe(r)}</td>
                  <td className="muted">
                    {r.action === "webhook" ? `POST ${r.target}` : `MQTT zoomy/alarms/${r.target}`}
                  </td>
                  <td>
                    <span
                      className={`pill toggle ${r.enabled ? "on" : ""}`}
                      onClick={async () => {
                        await api.patchAlarm(r.id, !r.enabled).catch((e) => onError(String(e)));
                        load();
                      }}
                    >
                      {r.enabled ? "on" : "off"}
                    </span>
                  </td>
                  <td>
                    <button
                      className="danger"
                      onClick={async () => {
                        await api.deleteAlarm(r.id).catch((e) => onError(String(e)));
                        load();
                      }}
                    >
                      Delete
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          </div>
        )}
      </div>
    </>
  );
}
