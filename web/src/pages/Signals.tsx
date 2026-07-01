import { useEffect, useRef, useState } from "react";
import { api, Camera, Settings } from "../api";
import { loadPlayer } from "../LiveVideo";
import { useToast } from "../ui";
import { IconPlay, IconStop, IconSiren } from "../icons";

// MediaPipe Tasks Vision is loaded at runtime from a CDN (configurable), so the
// 21-point hand-landmark model runs GPU-accelerated in the browser on any OS —
// the same portable-AI thesis as the server's ONNX path, but for the live view.
// Pin a version; the WASM fileset and ESM bundle must match.
const MP_VERSION = "0.10.18";
const MP_MODULE = `https://cdn.jsdelivr.net/npm/@mediapipe/tasks-vision@${MP_VERSION}/vision_bundle.mjs`;
const MP_WASM = `https://cdn.jsdelivr.net/npm/@mediapipe/tasks-vision@${MP_VERSION}/wasm`;

// Canonical MediaPipe hand skeleton (21 landmarks).
const HAND_CONNECTIONS: [number, number][] = [
  [0, 1], [1, 2], [2, 3], [3, 4],
  [0, 5], [5, 6], [6, 7], [7, 8],
  [5, 9], [9, 10], [10, 11], [11, 12],
  [9, 13], [13, 14], [14, 15], [15, 16],
  [13, 17], [17, 18], [18, 19], [19, 20],
  [0, 17],
];

// Mirror the backend's gesture taxonomy so the UI can decide what's "armed"
// before sending. The server re-normalizes whatever name it receives.
const CANON: Record<string, string> = {
  Open_Palm: "open_palm",
  Closed_Fist: "fist",
  Victory: "victory",
  Pointing_Up: "point",
  Thumb_Up: "thumb_up",
  Thumb_Down: "thumb_down",
  ILoveYou: "love",
  None: "hand",
};
const canon = (name: string) => CANON[name] ?? name.toLowerCase();

const PRETTY: Record<string, string> = {
  open_palm: "Open palm",
  fist: "Fist",
  victory: "Victory",
  point: "Pointing",
  thumb_up: "Thumb up",
  thumb_down: "Thumb down",
  love: "I-love-you",
  call_me: "Call me",
  ok: "OK",
  hand: "Hand",
};
const pretty = (g: string) => PRETTY[g] ?? g;

export default function Signals({ cameras }: { cameras: Camera[] }) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const rafRef = useRef<number>(0);
  const recognizerRef = useRef<any>(null);
  const streamRef = useRef<MediaStream | null>(null);
  // For an IP-camera source, MediaPipe runs on the go2rtc `<video-stream>`'s own
  // <video> (the same player the Live grid uses) rather than the device webcam.
  const streamHostRef = useRef<HTMLDivElement>(null);
  const streamElRef = useRef<any>(null);
  // Mirror of the selected source for the rAF loop, which captures at start.
  const sourceRef = useRef<string>("webcam");
  // Hold-to-fire state, kept in a ref so the rAF loop reads fresh values.
  const holdRef = useRef<{ gesture: string; since: number; fired: boolean }>({
    gesture: "",
    since: 0,
    fired: false,
  });

  const toast = useToast();
  const [settings, setSettings] = useState<Settings | null>(null);
  const [camera, setCamera] = useState<string>("");
  const [running, setRunning] = useState(false);
  const [status, setStatus] = useState("Idle — start the camera to read hand signals.");
  const [current, setCurrent] = useState<{ gesture: string; score: number } | null>(null);
  const [touchless, setTouchless] = useState(false);
  const [ptzOk, setPtzOk] = useState<boolean | null>(null);
  const [duressFlash, setDuressFlash] = useState(false);
  const lastPtz = useRef(0);
  // Set true once the source produces decodable frames; drives the "no video"
  // hint so an offline IP camera doesn't spin on "Reading hand signals…".
  const framesSeenRef = useRef(false);
  // The rAF loop captures state at start; mirror live controls into refs.
  const touchlessRef = useRef(false);
  const ptzOkRef = useRef(false);
  const camIdRef = useRef<number | undefined>(undefined);

  useEffect(() => {
    api.settings().then(setSettings).catch(() => {});
  }, []);
  useEffect(() => {
    if (!camera) setCamera("webcam");
  }, [camera]);

  const armed = settings?.gesture_labels ?? [];
  const holdSecs = settings?.gesture_hold_secs ?? 1.5;
  const duress = settings?.gesture_duress ?? "";
  // The duress signal always fires, even when not in the armed list.
  const isArmed = (g: string) => g === duress || armed.length === 0 || armed.includes(g);
  const isWebcam = camera === "webcam" || camera === "";
  const camId = isWebcam ? undefined : cameras.find((c) => c.name === camera)?.id;

  useEffect(() => {
    touchlessRef.current = touchless;
  }, [touchless]);
  useEffect(() => {
    ptzOkRef.current = !!ptzOk;
  }, [ptzOk]);
  useEffect(() => {
    camIdRef.current = camId;
  }, [camId]);
  useEffect(() => {
    sourceRef.current = isWebcam ? "webcam" : camera;
  }, [camera, isWebcam]);

  // Does the attributed camera answer PTZ? (gates touchless steering)
  useEffect(() => {
    setPtzOk(null);
    if (camId == null) return;
    api
      .ptzCaps(camId)
      .then((r) => setPtzOk(r.supported))
      .catch(() => setPtzOk(false));
  }, [camId]);

  const stop = () => {
    cancelAnimationFrame(rafRef.current);
    streamRef.current?.getTracks().forEach((t) => t.stop());
    streamRef.current = null;
    if (videoRef.current) videoRef.current.srcObject = null;
    // Tear down the go2rtc stream element, if one was attached.
    if (streamElRef.current) {
      try {
        streamElRef.current.pc?.getSenders?.().forEach((s: any) => s.track && s.track.stop());
      } catch {
        /* best-effort */
      }
      streamElRef.current.parentNode?.removeChild(streamElRef.current);
      streamElRef.current = null;
    }
    setRunning(false);
    setCurrent(null);
    setStatus("Stopped.");
  };

  const start = async () => {
    if (settings && !settings.gesture_recognition) {
      setStatus("Hand-signal recognition is disabled in Settings.");
      return;
    }
    setStatus("Loading hand-landmark model…");
    try {
      // @vite-ignore — the module URL is dynamic (CDN / self-hosted).
      const vision: any = await import(/* @vite-ignore */ MP_MODULE);
      const fileset = await vision.FilesetResolver.forVisionTasks(MP_WASM);
      const modelUrl =
        settings?.gesture_model_url?.trim() ||
        "https://storage.googleapis.com/mediapipe-models/gesture_recognizer/gesture_recognizer/float16/1/gesture_recognizer.task";
      recognizerRef.current = await vision.GestureRecognizer.createFromOptions(fileset, {
        baseOptions: { modelAssetPath: modelUrl, delegate: "GPU" },
        runningMode: "VIDEO",
        numHands: 2,
      });

      if (isWebcam) {
        const stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: "user" } });
        streamRef.current = stream;
        const video = videoRef.current!;
        video.srcObject = stream;
        await video.play();
      } else {
        // Read from the selected IP camera's go2rtc stream (the same player the
        // Live grid uses). MediaPipe runs on its <video> once frames arrive.
        await loadPlayer();
        const el: any = document.createElement("video-stream");
        el.mode = "webrtc,mse,mjpeg";
        el.media = "video";
        el.background = false;
        el.src = `/api/ws?src=${encodeURIComponent(camera)}`;
        el.style.width = "100%";
        el.style.height = "100%";
        streamHostRef.current?.replaceChildren(el);
        streamElRef.current = el;
      }
      setRunning(true);
      setStatus("Reading hand signals…");
      framesSeenRef.current = false;
      // If an IP-camera source never delivers a frame, surface it instead of
      // spinning indefinitely on a black box.
      if (!isWebcam) {
        const src = camera;
        window.setTimeout(() => {
          if (!framesSeenRef.current && streamElRef.current) {
            setStatus(`No video from ${src} — check it's online and reachable.`);
          }
        }, 6000);
      }
      loop();
    } catch (e) {
      setStatus(
        `Could not start: ${e}. The model loads from a CDN — check your connection, or set a self-hosted model URL in Settings.`
      );
      stop();
    }
  };

  const fire = async (g: string) => {
    try {
      const r = await api.recordGesture({ gesture: g, camera: isWebcam ? undefined : camera });
      if (r.duress) {
        setDuressFlash(true);
        toast.error(`DURESS — ${pretty(g)} — high-priority alert sent`);
        setTimeout(() => setDuressFlash(false), 6000);
      } else if (r.recorded) {
        toast.success(`${pretty(g)} → signal sent`);
      }
    } catch (e) {
      toast.error(`Couldn't send signal: ${e}`);
    }
  };

  const loop = () => {
    // Resolve the active source <video> each frame: the device webcam, or the
    // go2rtc stream element's own <video> (which appears once it connects).
    const video =
      sourceRef.current === "webcam"
        ? videoRef.current
        : ((streamElRef.current?.video as HTMLVideoElement) ||
            (streamElRef.current?.querySelector?.("video") as HTMLVideoElement) ||
            null);
    const canvas = canvasRef.current;
    const recognizer = recognizerRef.current;
    if (!video || !canvas || !recognizer || video.readyState < 2) {
      rafRef.current = requestAnimationFrame(loop);
      return;
    }
    if (!framesSeenRef.current) {
      framesSeenRef.current = true;
      setStatus("Reading hand signals…");
    }
    canvas.width = video.videoWidth;
    canvas.height = video.videoHeight;
    const ctx = canvas.getContext("2d")!;
    ctx.clearRect(0, 0, canvas.width, canvas.height);

    let result: any;
    try {
      result = recognizer.recognizeForVideo(video, performance.now());
    } catch {
      rafRef.current = requestAnimationFrame(loop);
      return;
    }

    const hands: any[] = result?.landmarks ?? [];
    for (const lm of hands) drawHand(ctx, lm, canvas.width, canvas.height);

    // Top gesture across detected hands.
    const cats: any[] = (result?.gestures ?? []).map((g: any[]) => g[0]).filter(Boolean);
    const best = cats.sort((a, b) => b.score - a.score)[0];
    const now = performance.now();

    // Touchless PTZ: steer the camera toward an OPEN PALM (the hand's position
    // in frame), and STOP on a fist. Throttled, and only on PTZ cameras.
    const tcam = camIdRef.current;
    if (touchlessRef.current && ptzOkRef.current && tcam != null && hands[0] && now - lastPtz.current > 350) {
      lastPtz.current = now;
      const g = best && best.categoryName !== "None" ? canon(best.categoryName) : "";
      const palm = hands[0][9] ?? hands[0][0]; // middle-finger MCP ≈ palm center
      // Display is mirrored, so invert pan for intuitive control. Tilt up = -dy.
      const dx = -(palm.x - 0.5);
      const dy = palm.y - 0.5;
      if (g === "open_palm" && (Math.abs(dx) > 0.12 || Math.abs(dy) > 0.12)) {
        const pan = Math.max(-0.5, Math.min(0.5, dx * 1.2));
        const tilt = Math.max(-0.5, Math.min(0.5, -dy * 1.2));
        api.ptz(tcam, { action: "move", pan, tilt, zoom: 0 }).catch(() => {});
      } else {
        api.ptz(tcam, { action: "stop" }).catch(() => {});
      }
    }
    if (best && best.categoryName !== "None") {
      const g = canon(best.categoryName);
      setCurrent({ gesture: g, score: best.score });
      const h = holdRef.current;
      if (h.gesture !== g) {
        holdRef.current = { gesture: g, since: now, fired: false };
      } else if (!h.fired && isArmed(g) && now - h.since >= holdSecs * 1000) {
        holdRef.current.fired = true;
        fire(g);
      }
    } else {
      setCurrent(null);
      holdRef.current = { gesture: "", since: now, fired: false };
    }
    rafRef.current = requestAnimationFrame(loop);
  };

  // Tear down on unmount.
  useEffect(() => () => stop(), []);

  const held = current && holdRef.current.gesture === current.gesture;
  const progress =
    held && !holdRef.current.fired
      ? Math.min(1, (performance.now() - holdRef.current.since) / (holdSecs * 1000))
      : holdRef.current.fired
        ? 1
        : 0;

  return (
    <>
      <h1>Hand signals</h1>
      <p className="muted" style={{ marginTop: -8 }}>
        Real-time hand-landmark tracking in your browser — from this device's webcam{" "}
        <b>or any camera's live stream</b>. Hold an armed signal for {holdSecs.toFixed(1)}s to log
        an event and trigger any matching alarm — a silent hand-signal "panic button" for your NVR.
      </p>

      <div className="card">
        <div className="row" style={{ marginBottom: 12 }}>
          {!running ? (
            <button className="btn btn-primary" onClick={start}>
              <IconPlay size={14} /> Start camera
            </button>
          ) : (
            <button className="btn btn-danger-solid" onClick={stop}>
              <IconStop size={14} /> Stop
            </button>
          )}
          <label className="field" title="Run hand-signal detection on your device's webcam, or on one of your cameras' live streams.">
            read hand signals from
            <select value={camera} onChange={(e) => setCamera(e.target.value)} disabled={running}>
              <option value="webcam">This device's webcam</option>
              {cameras.map((c) => (
                <option key={c.id} value={c.name}>
                  {c.name}
                </option>
              ))}
            </select>
          </label>
          {ptzOk && (
            <label className="toggle field" title="Steer this PTZ camera with an open palm; make a fist to stop.">
              touchless PTZ
              <input type="checkbox" checked={touchless} onChange={() => setTouchless((t) => !t)} />
            </label>
          )}
          <span className="muted" role="status" aria-live="polite">{status}</span>
        </div>

        {duressFlash && (
          <div className="callout callout-danger" role="alert" style={{ fontWeight: 600 }}>
            <span className="callout-ico"><IconSiren size={16} /></span>
            <div>DURESS signal sent — a high-priority alert went out.</div>
          </div>
        )}

        <div
          style={{
            position: "relative",
            width: "100%",
            maxWidth: 720,
            aspectRatio: "4 / 3",
            background: "#000",
            borderRadius: 12,
            overflow: "hidden",
            // Selfie-mirror the device webcam only; IP camera streams aren't mirrored.
            transform: isWebcam ? "scaleX(-1)" : "none",
          }}
        >
          <video
            ref={videoRef}
            playsInline
            muted
            style={{
              position: "absolute",
              inset: 0,
              width: "100%",
              height: "100%",
              objectFit: "cover",
              display: isWebcam ? "block" : "none",
            }}
          />
          <div
            ref={streamHostRef}
            style={{
              position: "absolute",
              inset: 0,
              width: "100%",
              height: "100%",
              display: isWebcam ? "none" : "block",
            }}
          />
          <canvas
            ref={canvasRef}
            role="img"
            aria-label="Live hand-landmark overlay for gesture detection"
            style={{ position: "absolute", inset: 0, width: "100%", height: "100%" }}
          />
          {!running && (
            <div
              style={{
                position: "absolute",
                inset: 0,
                display: "flex",
                flexDirection: "column",
                alignItems: "center",
                justifyContent: "center",
                gap: 8,
                textAlign: "center",
                padding: 16,
                color: "var(--text-muted)",
                pointerEvents: "none",
                // Un-mirror the placeholder inside the selfie-mirrored webcam box.
                transform: isWebcam ? "scaleX(-1)" : "none",
              }}
            >
              <IconSiren size={30} />
              <div style={{ fontWeight: 600, color: "var(--text)" }}>Ready to read hand signals</div>
              <div style={{ fontSize: "var(--text-sm)" }}>{status}</div>
            </div>
          )}
          {current && (
            <div
              style={{
                position: "absolute",
                top: 12,
                left: 12,
                transform: isWebcam ? "scaleX(-1)" : "none", // un-mirror the label (webcam only)
                background: "rgba(0,0,0,0.6)",
                color: "#fff",
                padding: "6px 12px",
                borderRadius: 8,
                fontSize: "1.1rem",
              }}
            >
              {pretty(current.gesture)} {(current.score * 100).toFixed(0)}%
              {!isArmed(current.gesture) && <span style={{ opacity: 0.6 }}> · not armed</span>}
            </div>
          )}
        </div>

        {running && (
          <div style={{ maxWidth: 720, marginTop: 8 }}>
            <div style={{ height: 6, background: "var(--border)", borderRadius: 3, overflow: "hidden" }}>
              <div
                style={{
                  height: "100%",
                  width: `${progress * 100}%`,
                  background: progress >= 1 ? "var(--success)" : "var(--accent)",
                  transition: "width 80ms linear",
                }}
              />
            </div>
          </div>
        )}
      </div>

      <div className="card">
        <h2>Armed signals</h2>
        <p className="muted" style={{ marginTop: 0 }}>
          These hand signals create an event when held. Edit the list (and the hold time) in
          Settings → Hand signals. Create an Alarm with a matching <b>gesture</b> condition to get a
          push notification.
        </p>
        {armed.length ? (
          <div className="row" style={{ flexWrap: "wrap" }}>
            {armed.map((g) => (
              <span key={g} className="badge ok">
                {pretty(g)}
              </span>
            ))}
          </div>
        ) : (
          <p className="muted" style={{ marginBottom: 0 }}>
            <b>Any</b> recognized signal is currently armed. To arm only specific ones, set the list
            under Settings → Hand signals.
          </p>
        )}
        <p className="muted" style={{ marginBottom: 0 }}>
          Recognizes: open palm, fist, victory, pointing, thumb up/down, and I-love-you. Runs fully
          on this device — nothing leaves the browser except the recognized signal name.
        </p>
      </div>
    </>
  );
}

function drawHand(
  ctx: CanvasRenderingContext2D,
  lm: { x: number; y: number }[],
  w: number,
  h: number
) {
  ctx.lineWidth = 3;
  ctx.strokeStyle = "rgba(80,200,255,0.9)";
  for (const [a, b] of HAND_CONNECTIONS) {
    ctx.beginPath();
    ctx.moveTo(lm[a].x * w, lm[a].y * h);
    ctx.lineTo(lm[b].x * w, lm[b].y * h);
    ctx.stroke();
  }
  ctx.fillStyle = "#ff4070";
  for (const p of lm) {
    ctx.beginPath();
    ctx.arc(p.x * w, p.y * h, 4, 0, Math.PI * 2);
    ctx.fill();
  }
}
