# ZoomyZoomyCamCam

A self-hosted, **cross-platform** (Windows + macOS + Linux) home surveillance / NVR
platform — think Blue Iris, but not chained to Windows, with Frigate-class AI object
detection that runs natively on Apple Silicon and any DirectX 12 GPU.

> Status: **v0.1 — working vertical slice.** Live grid, continuous recording with
> retention, and motion-gated AI detection events all work end-to-end behind one
> binary + web UI. See [`docs/01-research-and-architecture.md`](docs/01-research-and-architecture.md)
> for the full survey, architecture, and roadmap.

## Why another NVR?

| Gap in the field | Our answer |
|---|---|
| Blue Iris is Windows-only | Rust core + web UI → runs everywhere |
| Frigate needs Linux/Docker + Coral/Nvidia | ONNX Runtime: DirectML on Windows, CoreML on Mac |
| Moonfire records but has no AI | We add the motion-gate + detector layer |

## Architecture at a glance

```
cameras ──RTSP──▶ go2rtc (ingest + WebRTC) ──▶ recorder (ffmpeg -c copy → mp4 segments)
                          │                  └─▶ motion gate ─▶ AI detector (ONNX/YOLO)
                          └──WebRTC──▶ web UI            └─▶ core API + SQLite (events/config)
```

The design deliberately reuses two battle-tested binaries — **go2rtc** (camera
protocols + WebRTC) and **FFmpeg** (codec edge cases, packet-copy segmenting) — and
writes first-party Rust for everything else. AI is portable because the same exported
YOLO `.onnx` runs through ONNX Runtime with a per-OS GPU backend (DirectML / CoreML /
CUDA, CPU fallback).

## Quick start

Prerequisites:

- **Rust** (stable) via [rustup](https://rustup.rs); on Windows also the MSVC Build
  Tools (VS installer → "Desktop development with C++" or just the
  `VC.Tools.x86.x64` + Windows SDK components).
- **CMake** and **LLVM/libclang** — the bundled speech-to-text engine
  (whisper.cpp, compiled in) builds with CMake and generates its FFI bindings
  with bindgen (libclang). Linux can skip libclang by building with
  `WHISPER_DONT_GENERATE_BINDINGS=1` (the crate ships Linux bindings); Windows
  needs LLVM (`winget install LLVM.LLVM`, then `LIBCLANG_PATH` → its `bin`),
  macOS gets libclang from Xcode. CMake: `winget install Kitware.CMake` /
  `brew install cmake` / `apt install cmake`.
- **Node.js** ≥ 20 (to build the web UI once).
- **go2rtc** from [releases](https://github.com/AlexxIT/go2rtc/releases) → drop it at
  `./bin/go2rtc(.exe)`, or on `PATH`, or set `GO2RTC_BIN`.
- **ffmpeg** on `PATH` (e.g. `winget install Gyan.FFmpeg`).
- A **YOLOv8 ONNX model** at `./yolov8n.onnx`:
  `pip install ultralytics && yolo export model=yolov8n.pt format=onnx imgsz=640 opset=12`
- *(Optional, for face recognition)* the InsightFace **buffalo_l** pair from
  [Hugging Face](https://huggingface.co/immich-app/buffalo_l): save
  `detection/model.onnx` as `./det_10g.onnx` and `recognition/model.onnx` as
  `./w600k_r50.onnx`. Face recognition silently stays off without them.
- *(Optional, for license plate recognition)* save
  [onnx-community/yolos-small-finetuned-license-plate-detection-ONNX](https://huggingface.co/onnx-community/yolos-small-finetuned-license-plate-detection-ONNX)
  `onnx/model_quantized.onnx` as `./plate_det.onnx`, and from
  [monkt/paddleocr-onnx](https://huggingface.co/monkt/paddleocr-onnx)
  `languages/english/rec.onnx` as `./plate_rec.onnx` plus
  `languages/english/dict.txt` as `./plate_dict.txt`.
- *(Optional, for audio event detection — glass break, sirens, barking…)* YAMNet
  from [jafet21/yamnetonnx](https://huggingface.co/jafet21/yamnetonnx): save
  `yamnet.onnx` and `yamnet_class_map.csv` in the repo root, then enable
  *audio detection* per camera in its Tune dialog.
- *(Optional, for natural-language smart search)* CLIP from
  [Xenova/clip-vit-base-patch32](https://huggingface.co/Xenova/clip-vit-base-patch32):
  save `onnx/vision_model_quantized.onnx` as `./clip_vision.onnx`,
  `onnx/text_model_quantized.onnx` as `./clip_text.onnx`, and `tokenizer.json`
  as `./clip_tokenizer.json`. Enables the ✨ search box on the Events page.
- *(Optional, for audio transcription / speech-to-text)* a whisper GGML model,
  e.g. `ggml-tiny.en.bin` (~75 MB) or `ggml-base.en.bin` from
  [ggerganov/whisper.cpp](https://huggingface.co/ggerganov/whisper.cpp), saved
  in the repo root. Then enable *Audio transcription* in Settings (off by
  default) and *audio detection* on the camera; speech in audio events is
  transcribed onto the event. Fully local — the whisper engine is compiled in.

Build and run:

```bash
# one-time: build the web UI
cd web && npm install && npm run build && cd ..

# Desktop app (native window; engine embedded, UI on :18080)
cargo run -p zoomy-desktop

# ...or headless server mode (API + UI on :8080) for a NAS / home server
cargo run -p zoomy

# Windows installer (NSIS): produces target/release/bundle/nsis/*-setup.exe
# with the web UI, go2rtc and the model bundled inside
cd crates/desktop && npx @tauri-apps/cli build
```

Both modes run the identical engine and share nothing but the codebase: the
desktop app keeps its data in the per-user app-data dir when installed, while
server mode uses `./data`. (In dev, `cargo run -p zoomy-desktop` deliberately
shares the workspace `./data` so your cameras carry over.)

Open **http://localhost:8080**, go to *Cameras*, and add your camera's RTSP URL
(any go2rtc source string works — `rtsp://`, `ffmpeg:`, `exec:`, ONVIF, …). You get:

- **Live** — WebRTC grid of all enabled cameras (sub-second latency)
- **Events** — motion-gated AI detections with annotated snapshots, filterable
- **Recordings** — continuous 60 s MP4 segments, browser playback, retention by
  age and total size
- **Settings** — object filter, confidence, motion threshold, retention, all live

No camera handy? Make a fake one (a panning video on loop) and add it with source
`exec:ffmpeg -re -stream_loop -1 -i driveway.mp4 -c copy -rtsp_transport tcp -f rtsp {output}`.

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

- **Behind a reverse proxy?** A same-host proxy reaches the NVR over `127.0.0.1`,
  which would otherwise inherit the local-access exemption and bypass the
  password. Pass **`--trusted-proxy`** so auth and the brute-force throttle key
  off the proxy's `X-Forwarded-For` header instead — and make sure the NVR is
  reachable *only* through the proxy (bind it to loopback), since that flag tells
  it to trust the header.

The TLS stack is pure-Rust (rustls + the `ring` provider already in-tree) — no
OpenSSL, no extra build tooling.

## Layout

```
ZoomyZoomyCamCam/
├── Cargo.toml                # workspace
├── docs/                     # research, architecture, roadmap
├── config/                   # example go2rtc config
├── web/                      # React + TypeScript UI (Vite)
└── crates/
    ├── core/                 # zoomy library + CLI: API + SQLite + supervisors
    ├── desktop/              # Tauri 2 desktop app (embeds the zoomy library)
    ├── detector/             # YOLOv8 via ONNX Runtime, per-OS GPU EP
    ├── motion/               # cheap pixel-diff motion gate
    ├── recorder/             # ffmpeg packet-copy segments + retention
    ├── spike-live/           # Phase 0 spike (kept as standalone validation)
    └── spike-detect/         # Phase 0 spike (kept as standalone validation)
```

## License

Dual-licensed under MIT or Apache-2.0.
