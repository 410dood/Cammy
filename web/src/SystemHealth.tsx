import { ReactNode, useState } from "react";
import { api, Camera, StatusMap, SystemState } from "./api";
import { Callout, ErrorState, RelTime, usePolling, useToast } from "./ui";

const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** One subsystem row. `ok: null` = the feature is off, which is not a fault. */
function Row({
  label,
  ok,
  detail,
  extra,
}: {
  label: string;
  ok: boolean | null;
  detail: string;
  extra?: ReactNode;
}) {
  return (
    <tr>
      <td style={{ whiteSpace: "nowrap" }}>{label}</td>
      <td style={{ whiteSpace: "nowrap" }}>
        <span className={`badge ${ok === null ? "" : ok ? "ok" : "danger"}`}>
          {ok === null ? "off" : ok ? "working" : "problem"}
        </span>
      </td>
      <td className="muted" style={{ fontSize: "var(--text-sm)" }}>
        {detail} {extra}
      </td>
    </tr>
  );
}

/**
 * docs/11 P3.1 — the System health pane.
 *
 * Everything the app knows about ITSELF: the background workers, the streaming
 * process every camera goes through, the two work queues that carry alerts, the
 * MQTT link and the HomeKit bridge — plus the per-camera speed and freshness
 * numbers that until now were reachable only by curling `/api/metrics`.
 *
 * It leads with ONE verdict line on purpose. The failure this exists to catch is
 * a system that is quietly broken while every individual surface still looks
 * fine, so "everything is working" has to be a claim someone made, not an
 * absence of red.
 */
export default function SystemHealthCard() {
  const toast = useToast();
  const [sys, setSys] = useState<SystemState | null>(null);
  const [status, setStatus] = useState<StatusMap>({});
  const [cameras, setCameras] = useState<Camera[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);

  const load = async () => {
    try {
      const [a, b, c] = await Promise.all([api.system(), api.status(), api.cameras()]);
      setSys(a);
      setStatus(b);
      setCameras(c);
      setErr(null);
    } catch (e) {
      setErr(errMsg(e));
    }
  };
  usePolling(load, 10000);

  const dead = sys?.workers.filter((w) => !w.alive) ?? [];
  const mqttBroken = !!sys?.mqtt.enabled && !sys.mqtt.connected;
  const plural = (n: number) => (n === 1 ? "" : "s");
  const problems: string[] = [];
  if (sys && !sys.go2rtc.running) problems.push("the camera streamer is not running");
  if (dead.length) problems.push(`${dead.length} background task${plural(dead.length)} stopped`);
  if (mqttBroken) problems.push("the home-automation link is down");
  if (sys && sys.alarm_queue.dropped > 0)
    problems.push(`${sys.alarm_queue.dropped} alert${plural(sys.alarm_queue.dropped)} never sent`);
  if (sys && sys.genai_queue.shed > 0)
    problems.push(`${sys.genai_queue.shed} AI job${plural(sys.genai_queue.shed)} dropped`);

  return (
    <div className="card" data-settings-group="system">
      <h2>System health</h2>
      {err && <ErrorState what="system health" message={err} onRetry={load} />}
      {!sys && !err && <p className="muted">Checking…</p>}
      {sys && (
        <>
          <Callout tone={problems.length ? "danger" : "info"}>
            {problems.length
              ? `Needs a look: ${problems.join(", ")}.`
              : "Everything Cammy runs in the background is working."}
          </Callout>
          <div className="table-scroll" style={{ marginTop: 10 }}>
            <table>
              <tbody>
                <Row
                  label="Camera streamer"
                  ok={sys.go2rtc.running}
                  detail={
                    sys.go2rtc.running
                      ? sys.go2rtc.restarts <= 1
                        ? "Running, and has not needed restarting."
                        : `Running, but it has been restarted ${
                            sys.go2rtc.restarts - 1
                          } time${plural(sys.go2rtc.restarts - 1)} since Cammy started${
                            sys.go2rtc.restarts > 4
                              ? " — that is a lot; something keeps stopping it."
                              : "."
                          }`
                      : `Not running, so EVERY camera is affected — not just one. ${
                          sys.go2rtc.last_error ?? ""
                        }`
                  }
                />
                <Row
                  label="Background tasks"
                  ok={dead.length === 0}
                  detail={
                    dead.length === 0
                      ? `All ${sys.workers.length} running (recording, detection, alerts, backups…).`
                      : `Stopped: ${dead
                          .map((d) => d.name)
                          .join(", ")}. Whatever they handle has silently stopped; restart Cammy to bring them back.`
                  }
                />
                <Row
                  label="Alert delivery"
                  ok={sys.alarm_queue.dropped === 0}
                  detail={
                    sys.alarm_queue.dropped === 0
                      ? `${sys.alarm_queue.depth} waiting to send.`
                      : `${sys.alarm_queue.dropped} alert${plural(
                          sys.alarm_queue.dropped,
                        )} thrown away because a target stopped responding.`
                  }
                />
                <Row
                  label="AI queue"
                  ok={sys.genai_queue.shed === 0}
                  detail={
                    sys.genai_queue.shed === 0
                      ? `${sys.genai_queue.depth} job${plural(
                          sys.genai_queue.depth,
                        )} waiting for the vision model.`
                      : `${sys.genai_queue.depth} waiting, ${sys.genai_queue.shed} dropped — the vision model is answering slower than events arrive.`
                  }
                />
                <Row
                  label="Home automation (MQTT)"
                  ok={sys.mqtt.enabled ? sys.mqtt.connected : null}
                  detail={
                    !sys.mqtt.enabled
                      ? "No broker address is set, so this is off."
                      : sys.mqtt.connected
                        ? "Connected."
                        : `Not connected. ${sys.mqtt.last_error ?? ""}`
                  }
                  extra={
                    sys.mqtt.enabled ? (
                      <button
                        type="button"
                        className="btn btn-ghost ev-act"
                        disabled={testing}
                        onClick={async () => {
                          setTesting(true);
                          try {
                            const r = await api.mqttTest();
                            if (r.ok) toast.success(r.detail ?? "The broker answered.");
                            else toast.error(`Test failed: ${r.error}`);
                          } catch (e) {
                            toast.error(`Test failed: ${errMsg(e)}`);
                          } finally {
                            setTesting(false);
                            load();
                          }
                        }}
                      >
                        {testing ? "Testing…" : "Send a test"}
                      </button>
                    ) : null
                  }
                />
                {(sys.homekit.serving || sys.homekit.last_error) && (
                  <Row
                    label="Apple Home bridge"
                    ok={sys.homekit.serving}
                    detail={
                      sys.homekit.serving
                        ? "Serving — the sensors can be paired."
                        : `Not serving, so no pairing code will work. ${
                            sys.homekit.last_error ?? ""
                          }`
                    }
                  />
                )}
              </tbody>
            </table>
          </div>

          <h4 style={{ marginTop: 14, marginBottom: 2 }}>Per-camera speed</h4>
          <p className="muted" style={{ fontSize: "var(--text-sm)", marginTop: 0 }}>
            How long the AI takes on each picture, and how fresh the last one is.
          </p>
          <div className="table-scroll">
            <table>
              <thead>
                <tr>
                  <th>Camera</th>
                  <th>AI speed</th>
                  <th>Runs on</th>
                  <th>Last picture</th>
                </tr>
              </thead>
              <tbody>
                {cameras
                  .filter((c) => c.enabled)
                  .map((c) => {
                    const st = status[String(c.id)];
                    return (
                      <tr key={c.id}>
                        <td>{c.name}</td>
                        <td>{st?.inference_ms != null ? `${st.inference_ms.toFixed(0)} ms` : "—"}</td>
                        <td>{st?.accelerator ?? "—"}</td>
                        <td>
                          {st?.last_frame_ts ? <RelTime ts={st.last_frame_ts} /> : "—"}
                          {st?.detector_error && (
                            <span
                              className="badge danger"
                              style={{ marginLeft: 6, whiteSpace: "nowrap" }}
                              title={st.detector_error}
                            >
                              Model failed to load
                            </span>
                          )}
                        </td>
                      </tr>
                    );
                  })}
              </tbody>
            </table>
          </div>
        </>
      )}
    </div>
  );
}
