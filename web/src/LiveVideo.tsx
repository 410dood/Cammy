import { useEffect, useRef, useState } from "react";
import { StreamMode } from "./api";
import { IconWifiOff, IconAlert, IconRefresh } from "./icons";

/// Native live player: embeds go2rtc's `<video-stream>` web component (a real
/// `<video>` element with WebRTC + automatic MSE/MJPEG fallback) instead of an
/// iframe onto go2rtc's stream.html. Lower overhead, proper sizing, and no
/// nested-page chrome.
///
/// Everything is same-origin to zoomy: the player JS loads via the `/api/player`
/// proxy (go2rtc serves it without CORS), and the streaming WebSocket connects
/// to zoomy's `/api/ws`, which reverse-proxies to the loopback-only go2rtc. So
/// go2rtc never needs to accept a cross-origin browser connection, and live
/// streams ride zoomy's auth like every other API route.

// Imported once per page; the module calls customElements.define('video-stream').
let playerLoad: Promise<unknown> | null = null;
export function loadPlayer(): Promise<unknown> {
  if (!playerLoad) {
    // A variable specifier keeps both TS and Vite from trying to resolve this
    // runtime-only, server-proxied module at build time.
    const mod = "/api/player/video-stream.js";
    // Cache only on success. go2rtc is a supervised child that restarts on
    // camera CRUD; during that window the proxy 502s and this import rejects.
    // Clearing the cache on failure lets the next tile mount retry instead of
    // every future tile inheriting the poisoned promise until a page reload.
    playerLoad = import(/* @vite-ignore */ mod).catch((e) => {
      playerLoad = null;
      throw e;
    });
  }
  return playerLoad;
}

// Map the user's transport preference to go2rtc's comma-separated priority list,
// keeping sensible fallbacks so a blocked transport degrades instead of failing.
const MODE_FALLBACKS: Record<StreamMode, string> = {
  webrtc: "webrtc,mse,mjpeg",
  mse: "mse,mjpeg",
  mjpeg: "mjpeg",
};

type VideoStreamEl = HTMLElement & {
  mode: string;
  media: string;
  background: boolean;
  src: string;
  // go2rtc's VideoRTC exposes its RTCPeerConnection here.
  pc?: RTCPeerConnection;
};

export default function LiveVideo({
  name,
  mode,
  audio = false,
  mic = false,
  online,
}: {
  name: string;
  mode: StreamMode;
  audio?: boolean;
  /// Push-to-talk: stream the browser mic to the camera. Forces WebRTC (the
  /// only transport that carries an outbound track) and asks the player for a
  /// `microphone` media, which adds a send-only audio track to the connection.
  mic?: boolean;
  /// Camera reachability from the status board. `false` shows a branded
  /// "offline" state and skips mounting the player; `undefined` (status not
  /// loaded yet) is treated as "try to connect".
  online?: boolean;
}) {
  const host = useRef<HTMLDivElement>(null);
  // We own the status layer (go2rtc's raw "mse: stream not found" text is hidden
  // in CSS): "connecting" until the first frame plays, "live" once it does, or
  // "error" if no frame ever decodes (so we never expose the browser's native
  // "Unable to play media." chrome — that reads as broken).
  const [phase, setPhase] = useState<"connecting" | "live" | "error">("connecting");
  // Bumping this remounts the player — the Retry affordance on the error state.
  const [reload, setReload] = useState(0);

  // Collapse `online: boolean | undefined` to a stable boolean: `undefined`
  // (status not loaded yet) and `true` both mean "try to connect", so keying the
  // effect on `offline` instead of raw `online` stops the first status poll
  // (undefined → true) from needlessly tearing down and remounting the player.
  const offline = online === false;

  useEffect(() => {
    if (offline) return; // offline: don't even mount the player
    setPhase("connecting");
    let el: VideoStreamEl | null = null;
    let video: HTMLVideoElement | null = null;
    let cancelled = false;
    const markLive = () => setPhase("live");
    // Grace backstop: `playing`/`loadeddata` normally flip us to "live" within a
    // second or two. If neither fired, decide by whether a frame ACTUALLY
    // decoded — `readyState >= 2` (HAVE_CURRENT_DATA) — flipping to "live" if so
    // (covers a missed event on a working stream), otherwise surface a branded
    // "error" state instead of unveiling go2rtc's broken native player. NB:
    // `videoWidth > 0` is NOT sufficient — it's already true at HAVE_METADATA
    // (readyState 1), which a stuck stream reaches without ever decoding a
    // frame. go2rtc keeps retrying under the hood, so a later `playing` event
    // still recovers us.
    const grace = setTimeout(() => {
      if (cancelled) return;
      if (video && video.readyState >= 2) markLive();
      else setPhase("error");
    }, 10000);
    loadPlayer()
      .then(() => {
        if (cancelled || !host.current) return;
        el = document.createElement("video-stream") as VideoStreamEl;
        // Talking needs WebRTC; MSE/MJPEG can't carry the mic upstream.
        el.mode = mic ? "webrtc" : MODE_FALLBACKS[mode];
        el.media = mic ? "video,audio,microphone" : audio ? "video,audio" : "video";
        el.background = false; // stop streaming when the tab is hidden
        // Relative path → the player resolves it to ws://<this origin>/api/ws,
        // which zoomy reverse-proxies to the loopback-only go2rtc.
        el.src = `/api/ws?src=${encodeURIComponent(name)}`;
        el.className = "live-video";
        host.current.appendChild(el);
        // Flip to "live" on the first decoded frame so the overlay clears exactly
        // when video appears.
        video = el.querySelector("video");
        if (video) {
          video.addEventListener("playing", markLive);
          video.addEventListener("loadeddata", markLive);
        }
      })
      .catch(() => {
        /* go2rtc unreachable — the grace timer surfaces the error state */
      });
    return () => {
      cancelled = true;
      clearTimeout(grace);
      if (video) {
        video.removeEventListener("playing", markLive);
        video.removeEventListener("loadeddata", markLive);
      }
      if (el) {
        // Stop any local capture (the push-to-talk mic) IMMEDIATELY. go2rtc's
        // player defers its own teardown — and the sender track.stop() — behind
        // a 5s timer, which for push-to-talk would leave the mic hot (OS
        // indicator lit) for ~5s after release. Don't wait for it.
        el.pc?.getSenders?.().forEach((s) => s.track && s.track.stop());
        el.parentNode?.removeChild(el);
      }
    };
  }, [name, mode, audio, mic, offline, reload]);

  return (
    <div className="live-video-host" ref={host}>
      {offline ? (
        <div className="live-state offline">
          <IconWifiOff size={30} />
          <span>Camera offline</span>
        </div>
      ) : phase === "error" ? (
        <div className="live-state error">
          <IconAlert size={28} />
          <span>Live video unavailable</span>
          <button
            type="button"
            className="live-retry"
            onClick={() => setReload((r) => r + 1)}
          >
            <IconRefresh size={14} /> Retry
          </button>
        </div>
      ) : phase === "connecting" ? (
        <div className="live-state connecting">
          <span className="live-spinner" aria-hidden="true" />
          <span>Connecting…</span>
        </div>
      ) : null}
    </div>
  );
}
