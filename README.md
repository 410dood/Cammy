# Cammy

**Self-hosted AI camera security that runs on your hardware — Windows, macOS and
Linux.** Blue Iris-class NVR features with Frigate-class AI object detection, but
not chained to any one OS and with no cloud and no monthly fees. Every frame is
processed locally on your machine.

> **Status: v0.4 — launched.** 24/7 recording, a live WebRTC grid, motion-gated
> AI detection, face recognition, license-plate reading, natural-language search,
> family-safety modes, and an Alarm Manager all work end to end. Sold as a
> **$79 one-time license** (unlimited cameras, 2 machines) after a **30-day
> full-featured free trial** — no card required, and it never stops recording
> when the trial ends.

## Install (Windows)

1. Download the latest **`Cammy_x64-setup.exe`** from
   [Releases](https://github.com/410dood/Cammy/releases/latest).
2. Run it. go2rtc, ffmpeg, and the core AI models are **bundled inside** — there
   is nothing else to install — and no admin rights needed (it installs
   per-user). You get Start Menu + desktop shortcuts, and uninstalling later
   **keeps your recordings and settings** unless you say otherwise.
3. Launch Cammy. A first-run wizard walks you through a password and your first
   camera. Your 30-day trial starts automatically; no signup.

That's it — AI detection works out of the box. Cammy **starts with Windows** by
default (toggle it in the tray menu or Settings → Desktop app), only ever runs
one copy (a second launch just focuses the window), and **updates itself** —
tray → "Check for updates" installs a new version with one click and resumes
recording. For headless 24/7 recording at the lock screen / logged out, install
the engine as a **Windows service**: see
[DEPLOYMENT.md §2b](DEPLOYMENT.md) (`zoomy --install-service`).

macOS and Linux run the same engine; build from source (below) until native
installers ship.

Stuck? A black camera tile, no events, or an ONVIF scan that finds nothing are
almost always a quick fix — see [`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md).

## What you get

- **Live** — a WebRTC grid of every camera at sub-second latency, camera groups,
  saved layouts, a kiosk Wall view, and two-way push-to-talk on supported cameras.
- **Events** — motion-gated AI detections with annotated snapshots, severity
  tiers, tags, bookmarks, natural-language + photo search, CSV export, shareable
  expiring clip links (send one to the police, no login), and a signed
  **evidence bundle** — a watermarked clip zipped with an Ed25519-signed manifest
  that anyone can re-check offline with `zoomy --verify` to prove it came from
  your box and wasn't altered.
- **Recordings** — continuous lossless recording with a scrubbable multi-camera
  timeline, event-to-recording jumps, clip export, one-tap day time-lapse, and
  retention by age or size.
- **People** — face recognition with enrollment from an unknown-faces gallery,
  stranger alerts, and a vehicle / license-plate library.
- **Family** — guided safety modes (baby & nursery, pets, pool & water, aging in
  place) built from zones, sounds, and pose/fall/absence watching. Assistive by
  design — never a medical device.
- **Alarms** — plain-English if-this-then-that rules (a known face, a plate, a
  spoken phrase, a zone crossing, a sound) that fire phone push, webhooks, MQTT,
  or email.
- **Analytics** — object tracking, line-crossing tripwires, loitering, people
  counting, occupancy limits, speed, heatmaps, cross-camera appearance search, and
  an Insights dashboard of detection trends over days and weeks.
- **Private & secure** — multi-user roles, two-factor auth, privacy masks, an
  audit log, API tokens, HTTPS, and config backup/restore. Zero cloud, zero
  telemetry.

Running Cammy as a headless server on a NAS or home server? See
[`DEPLOYMENT.md`](DEPLOYMENT.md) for systemd, Docker/Compose, reverse-proxy, and
remote-access (Tailscale / Cloudflare Tunnel) recipes.

## Why another NVR?

| Gap in the field | Cammy's answer |
|---|---|
| Blue Iris is Windows-only | Rust core + web UI → runs everywhere |
| Frigate needs Linux/Docker + Coral/Nvidia | ONNX Runtime: DirectML on Windows, CoreML on Mac, CUDA on Linux |
| Cloud NVRs charge monthly, per camera | $79 once, unlimited cameras, no subscription |
| Everything phones home | 100% local — video, models, and face data never leave your machine |

## Architecture at a glance

```
cameras ──RTSP──▶ go2rtc (ingest + WebRTC) ──▶ recorder (ffmpeg -c copy → mp4 segments)
                          │                  └─▶ motion gate ─▶ AI detector (ONNX/YOLO)
                          └──WebRTC──▶ web UI            └─▶ core API + SQLite (events/config)
```

Cammy reuses two battle-tested binaries — **go2rtc** (camera protocols + WebRTC)
and **FFmpeg** (codec edge cases, packet-copy segmenting) — and writes
first-party Rust for everything else. AI is portable because the same exported
YOLO `.onnx` runs through ONNX Runtime with a per-OS GPU backend (DirectML /
CoreML / CUDA, CPU fallback).

## Building from source (macOS / Linux / development)

Prerequisites:

- **Rust** (stable) via [rustup](https://rustup.rs); on Windows also the MSVC
  Build Tools (VS installer → "Desktop development with C++").
- **CMake** and **LLVM/libclang** — the bundled speech-to-text engine
  (whisper.cpp, compiled in) builds with CMake and generates its FFI bindings
  with bindgen (libclang). Linux can skip libclang with
  `WHISPER_DONT_GENERATE_BINDINGS=1` (the crate ships Linux bindings); Windows
  needs LLVM (`winget install LLVM.LLVM`, then point `LIBCLANG_PATH` at its
  `bin`); macOS gets libclang from Xcode. CMake: `winget install Kitware.CMake` /
  `brew install cmake` / `apt install cmake`.
- **Node.js** ≥ 20 (to build the web UI once).
- **go2rtc** from [releases](https://github.com/AlexxIT/go2rtc/releases) → drop it
  at `./bin/go2rtc(.exe)`, or on `PATH`, or set `GO2RTC_BIN`.
- **ffmpeg** on `PATH` (e.g. `winget install Gyan.FFmpeg`).
- A **YOLOv8 ONNX model** at `./yolov8n.onnx`:
  `pip install ultralytics && yolo export model=yolov8n.pt format=onnx imgsz=640 opset=12`

```bash
# one-time: build the web UI
cd web && npm install && npm run build && cd ..

# Desktop app (native window; engine embedded, UI at http://localhost:18080)
cargo run -p zoomy-desktop

# ...or headless server mode (API + UI at http://localhost:8080) for a NAS / home server
cargo run -p zoomy

# Windows installer (NSIS): produces target/release/bundle/nsis/*-setup.exe
# with the web UI, go2rtc and the models bundled inside
cd crates/desktop && npx @tauri-apps/cli build
```

The desktop app keeps its data in the per-user app-data dir when installed;
server mode uses `./data`. (In dev, `cargo run -p zoomy-desktop` deliberately
shares the workspace `./data` so your cameras carry over.)

In **server mode**, open **http://localhost:8080**; in the **desktop app** the
native window opens itself (on :18080). Go to *Cameras* and add your camera's RTSP
URL — any go2rtc source string works (`rtsp://`, ONVIF auto-discovery, `ffmpeg:`,
`exec:`, …).

No camera handy? Make a fake one (a panning video on loop) with source
`exec:ffmpeg -re -stream_loop -1 -i driveway.mp4 -c copy -rtsp_transport tcp -f rtsp {output}`.

## Optional AI models

The Windows installer bundles the core detector and CLIP/YAMNet models. When
building from source — or to enable a feature the installer doesn't ship — drop
these files in the working directory (repo root for source builds; the app
directory otherwise). Each feature stays silently off until its model is present;
**Settings → Detection & AI → Models & capabilities** shows exactly which are
found. Models are picked up within a minute of being added.

| Feature | Files | Source |
|---|---|---|
| **Face recognition** | `det_10g.onnx`, `w600k_r50.onnx` | [immich-app/buffalo_l](https://huggingface.co/immich-app/buffalo_l) — save `detection/model.onnx` and `recognition/model.onnx` |
| **License-plate reading** | `plate_det.onnx`, `plate_rec.onnx`, `plate_dict.txt` | [yolos plate detector](https://huggingface.co/onnx-community/yolos-small-finetuned-license-plate-detection-ONNX) (`onnx/model_quantized.onnx`) + [paddleocr-onnx](https://huggingface.co/monkt/paddleocr-onnx) (`languages/english/rec.onnx`, `dict.txt`) |
| **Smart / natural-language search** | `clip_vision.onnx`, `clip_text.onnx`, `clip_tokenizer.json` | [Xenova/clip-vit-base-patch32](https://huggingface.co/Xenova/clip-vit-base-patch32) — `onnx/vision_model_quantized.onnx`, `onnx/text_model_quantized.onnx`, `tokenizer.json` |
| **Audio events** (glass, sirens, barking, cry, smoke alarm) | `yamnet.onnx`, `yamnet_class_map.csv` | [jafet21/yamnetonnx](https://huggingface.co/jafet21/yamnetonnx) |
| **Audio transcription** (speech-to-text) | `ggml-tiny.en.bin` (~75 MB) | [ggerganov/whisper.cpp](https://huggingface.co/ggerganov/whisper.cpp) — enable *Audio transcription* in Settings |
| **Body-pose safety** (fall / crib climb-out) | `yolov8n-pose.onnx` | `yolo export model=yolov8n-pose.pt format=onnx imgsz=640 opset=12` — set its path in Settings → Recording & backup |

## Remote access & HTTPS

On a trusted LAN you can run plain HTTP. Before exposing the NVR off-LAN:

- **Set a password** in *Settings* — it gates all non-loopback API access (the
  local box stays exempt, so you can't lock yourself out). Stored as **argon2id**;
  repeated wrong logins from one IP get throttled (lockout after 8 tries in 5 min).
- **Serve HTTPS** so the session cookie and traffic aren't in the clear:

  ```bash
  # one-flag TLS with an auto-generated, reused self-signed cert (<data>/tls)
  cargo run -p zoomy -- --tls-self-signed --port 8443
  # …or bring your own certificate
  cargo run -p zoomy -- --tls-cert fullchain.pem --tls-key privkey.pem
  ```

  Self-signed certs trip a browser warning (expected); for a clean padlock use a
  real certificate or front the NVR with a reverse proxy (Caddy/nginx/Traefik).

- **Behind a reverse proxy?** Pass **`--trusted-proxy`** so auth and the
  brute-force throttle key off the proxy's `X-Forwarded-For` header, and bind the
  NVR to loopback so it's reachable *only* through the proxy. See
  [`DEPLOYMENT.md`](DEPLOYMENT.md) for full recipes.

The TLS stack is pure-Rust (rustls + the `ring` provider already in-tree) — no
OpenSSL, no extra build tooling.

## Layout

```
Cammy/
├── Cargo.toml                # workspace
├── docs/                     # research, architecture, roadmap, licensing
├── config/                   # example go2rtc config
├── web/                      # React + TypeScript UI (Vite)
└── crates/
    ├── core/                 # zoomy library + CLI: API + SQLite + supervisors
    ├── desktop/              # Tauri 2 desktop app (embeds the zoomy library)
    ├── detector/             # YOLOv8 via ONNX Runtime, per-OS GPU EP
    ├── motion/               # cheap pixel-diff motion gate
    ├── recorder/             # ffmpeg packet-copy segments + retention
    └── ...                   # tracker, pose, facerec, gesture, spikes
```

## License

The Cammy source is dual-licensed under [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option. The pre-built binaries are sold
under the $79 commercial license described in
[`docs/09-licensing-and-monetization.md`](docs/09-licensing-and-monetization.md);
buying one supports development and gets you the batteries-included installer.
