# Troubleshooting

Common problems, by symptom. Most issues are a camera that speaks a slightly
different dialect of RTSP/ONVIF, a model that isn't installed yet, or an
access/networking hurdle — all fixable without touching the code.

If none of this helps, open an issue with your OS, the Cammy version (Settings →
About), and the relevant log lines: <https://github.com/410dood/Cammy/issues>.

---

## A camera tile is black, spinning, or says "connecting…"

The live grid plays your camera through **go2rtc** (WebRTC, with an MSE/MJPEG
fallback). A black tile almost always means Cammy can't pull a stream from the
camera, not that the UI is broken.

1. **Is go2rtc running?** Cammy supervises it as a child process. On a
   from-source run it must be at `./bin/go2rtc(.exe)`, on your `PATH`, or pointed
   to by the `GO2RTC_BIN` env var. The Windows installer bundles it. If the whole
   grid is black, go2rtc probably isn't starting — check the Cammy log at startup.
2. **Does the stream URL actually work?** Copy the camera's RTSP URL into
   [VLC](https://www.videolan.org) (*Media → Open Network Stream*). If VLC can't
   play it either, the URL, credentials, or the camera itself is the problem —
   not Cammy.
3. **Right stream path?** Many cameras expose a high-res *main* stream and a
   low-res *sub* stream at different paths (e.g. `/cam/realmonitor?channel=1&subtype=0`
   vs `subtype=1`). Use the sub-stream for the detect role and the main stream for
   recording (Cameras → the camera → detect source).
4. **Codec.** Cammy records by copying packets (no re-encode), so the camera must
   output **H.264 or H.265**. MJPEG-only cameras play live but won't record; set
   the camera to H.264 in its own web UI.
5. **Try the other transport.** If WebRTC won't establish (some networks block
   UDP), the player falls back to MSE automatically — give it a few seconds. A
   persistent spinner usually means step 2 failed.
6. **Demo/looped video source?** A synthetic `exec:ffmpeg …` source needs a small
   keyframe interval — add `-g 30` (real IP cameras are fine as-is).

## No events / detections ever appear

Detection is deliberately **two-stage and motion-gated** — Cammy only runs the AI
model when the cheap motion detector sees the scene change. A perfectly still
scene legitimately produces **zero events forever**. Work down this list:

1. **Is the camera online and is "detect" enabled?** Cameras page → the camera
   must be connected and have detection turned on (it's off for cameras you only
   want to record).
2. **Is the detector model present?** Settings → *Detection & AI → Models &
   capabilities* shows exactly which models were found. The core object detector
   (`yolov8n.onnx`) ships with the installer; a from-source build needs it in the
   working directory. Each AI feature stays silently off until its model is present
   — see the [Optional AI models](https://github.com/410dood/Cammy#optional-ai-models)
   table.
3. **Did anything actually move?** Detections only fire on movement the model then
   recognizes as a person/vehicle/etc. **Walk in front of a camera** to test.
4. **Is motion sensitivity too strict?** Cameras → the camera → *Detection tuning*
   → motion threshold. Lower it (temporarily set it to `0`) to confirm the pipeline
   works, then tune up to cut noise.
5. **Recording works but no *events*?** That's normal and expected — continuous
   recording (Recordings) and AI events (Events) are separate. A camera can record
   24/7 with detection off.
6. **Frozen frame?** If the source serves a single still frame (some demo setups),
   the motion gate never trips. Real cameras with a live feed are fine.

## ONVIF discovery / "resolve" failed

The Add-camera and onboarding *Scan network* use ONVIF (WS-Discovery) to find
cameras and resolve their RTSP URLs.

1. **ONVIF enabled on the camera?** Many cameras ship with ONVIF **off** — enable
   it in the camera's own web UI (often under *Network → ONVIF* or *Integration*).
2. **Credentials.** ONVIF frequently uses a **separate account** from the camera's
   web login, sometimes with its own password. Create/confirm an ONVIF user on the
   camera.
3. **Same subnet.** WS-Discovery is multicast and doesn't cross subnets/VLANs — the
   Cammy host and the camera must be on the same L2 network, or you must add the
   camera by IP instead.
4. **Fall back to a direct URL.** You never *need* ONVIF — add the camera by typing
   its RTSP URL on the Cameras page (any go2rtc source string works: `rtsp://`,
   `ffmpeg:`, `exec:`).

## Can't reach the web UI from another device

1. **Loopback is exempt; everything else needs the password.** If you set a
   password (Settings → Remote access) other devices must log in; the Cammy host
   itself never has to. No password + LAN access = open on the LAN by design.
2. **Firewall.** Allow inbound TCP on the Cammy port (default **8080** headless,
   **18080** for the desktop app) on the host.
3. **Wrong address.** Use the host's LAN IP, not `localhost`, from another device.
4. **Exposing off-LAN?** Don't port-forward plain HTTP. Use HTTPS
   (`--tls-self-signed` or a real cert) and, behind a reverse proxy, `--trusted-proxy`.
   See [`DEPLOYMENT.md`](../DEPLOYMENT.md) for Tailscale / Cloudflare Tunnel recipes
   (zero port-forwarding).
5. **Locked out by the brute-force throttle?** Eight wrong logins from one IP in 5
   minutes → a temporary lockout (loopback is exempt, so the host can always get in).

## An AI feature (faces, plates, search, audio, pose) does nothing

Each optional feature is gated on its model file being present *and* the feature
being enabled:

1. Settings → *Detection & AI → Models & capabilities* — the card lists every
   optional model and whether it was found. Missing files are the #1 cause.
2. Download the files from the
   [Optional AI models](https://github.com/410dood/Cammy#optional-ai-models) table
   into the working directory (repo root for source builds; the app data dir
   otherwise). Models are picked up within a minute — no restart needed.
3. **Faces:** at least one identity must be enrolled before *stranger* alerts fire
   (with nobody enrolled, everyone is "unknown" = noise, so it's suppressed).
4. **Captions / VLM alert gate / ask:** these need a local Ollama endpoint
   configured; they fail *open* (never block a real alert) and log at debug.

## Recordings are missing, short, or filling the disk

1. **Recording enabled for that camera?** Cameras → detect/record toggles are
   independent. Recordings page shows per-camera segment counts.
2. **First segments take ~a minute** after a record-enabled camera connects.
3. **Disk filling up?** Recordings shows "~N days until full" and the limiter
   (age vs size retention). Set retention (Settings → Recording & backup) — Cammy
   prunes oldest-first, but bookmarked/flagged events are kept past retention.
4. **No audio in recordings?** Enable AAC audio per camera; the camera must send an
   audio track.

## Build-from-source fails

- **`libclang` / bindgen error** (the bundled whisper.cpp speech-to-text): install
  LLVM and point `LIBCLANG_PATH` at its `bin` (Windows: `winget install LLVM.LLVM`);
  macOS gets it from Xcode; Linux can skip it with `WHISPER_DONT_GENERATE_BINDINGS=1`.
- **`cmake` not found:** `winget install Kitware.CMake` / `brew install cmake` /
  `apt install cmake`.
- See the *Building from source* section of the [README](../README.md) for the full
  prerequisite list.
