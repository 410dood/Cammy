# Cammy — Research & Architecture

*Self-hosted home surveillance platform (BlueIris / iSpy class). Prepared June 9, 2026.*

Goal: cross-platform (Windows + Mac, ideally Linux too) NVR with live viewing, continuous recording, motion detection, and AI object detection. This document surveys the existing field, then recommends an architecture — including an honest assessment of whether Rust is the right call.

---

## 1. The existing field

The smart move is not to build everything from scratch. Almost every serious NVR is really a thin app wrapping three hard, solved problems: pulling RTSP off flaky cameras, moving/recording video without re-encoding, and running an object-detection model. Knowing what the incumbents did tells us what to reuse.

| Platform | Lang / Runtime | Cross-platform | AI detection | License | Notes |
|---|---|---|---|---|---|
| **Frigate** | Python + Go (go2rtc) | Linux / Docker (not native Win/Mac) | Yes — best-in-class (Coral, OpenVINO, Nvidia, Apple not supported) | Open source (Frigate+ paid models) | The community reference. Motion → AI two-stage pipeline. |
| **Blue Iris** | C# / .NET | **Windows only** | Add-on (CodeProject.AI / DeepStack) | Paid, ~$70 | Powerful, mature, but single-OS — the gap you're targeting. |
| **Shinobi** | Node.js | Linux/Win/Mac | Plugin-based, weaker | Open core | One-maintainer, formal releases stalled. |
| **ZoneMinder** | PHP/Perl/C++ | Linux | No person detection natively | GPL | Stable, dated UI, now has stream passthrough. |
| **iSpy / AgentDVR** | C# / .NET | Win/Linux/Mac | Yes (paid tiers) | Freemium | Closest existing "runs everywhere" option. |
| **Moonfire NVR** | **Rust** + TS | Linux (builds elsewhere) | **No** (record-only by design) | Open source | The reference Rust NVR. Tiny CPU footprint, no analysis. |

### Key lessons to steal

The single most important architectural idea, proven by Frigate, is the **two-stage detection pipeline**: a cheap motion/pixel-difference pass on a low-resolution substream gates an expensive AI model that only runs on cropped regions where something moved. This is what makes real-time AI affordable on home hardware — you are not running YOLO on every frame of every camera.

The second lesson, from both Frigate and Moonfire, is **separation of record and analyze paths**. Cameras expose a high-res "main" stream and a low-res "sub" stream. You record the main stream by copying packets straight to disk with no decoding (cheap, lossless), and you decode only the sub stream for motion + AI. Moonfire records six 1080p/30 streams on a Raspberry Pi 2 at under 10% CPU precisely because it never decodes.

The third lesson is **don't reinvent the streaming gateway**. `go2rtc` already solves the ugliest part of this domain — talking to every camera protocol (RTSP, ONVIF, RTMP, HomeKit) and re-publishing as WebRTC/HLS/MJPEG with sub-second latency — and it runs natively on Windows, macOS, Linux, and ARM as a single zero-dependency binary. Frigate itself delegates all stream wrangling to it.

---

## 2. Cross-platform architecture — and the Rust question

You asked specifically: what's the best cross-platform architecture, can it run on Windows and Mac, and is Rust the answer?

### Short answer

Rust is a strong, defensible core choice — but the winning architecture is **polyglot**, not pure Rust. Use Rust for the recorder/coordinator where its safety and low footprint shine, lean on existing native binaries (`go2rtc`, FFmpeg) for the parts that are already solved and miserable to rewrite, and use ONNX Runtime for AI so the same model runs on every OS with GPU acceleration.

### Why Rust fits the core

The Rust ecosystem now has the exact pieces an NVR needs. **Retina** is a production-quality pure-Rust RTSP client (it's what Moonfire uses) built specifically to cope with broken cheap-camera firmware. **Moonfire** itself proves a Rust NVR can record many streams at trivial CPU cost. Rust gives you a single static binary per platform, no garbage-collector pauses while muxing video, and memory safety in code that runs 24/7 parsing untrusted network data from cameras — exactly where C/C++ NVRs have historically had CVEs.

### Why not *pure* Rust

Video codec and container handling is a deep, thankless pit. FFmpeg has 20 years of edge-case handling you will not reproduce. And camera-protocol breadth (ONVIF discovery, two-way audio, WebRTC) is already nailed by `go2rtc`. Rewriting these in Rust is months of work for no user-visible benefit. Treat them as dependencies, not things to build.

### AI: the cross-platform crux

This is where "runs on Windows and Mac" is won or lost. **Frigate can't run natively on either** largely because its acceleration assumes Linux + Coral/OpenVINO/Nvidia. The portable answer is **ONNX Runtime** via the Rust `ort` crate, which selects a hardware backend per OS from the *same* model file:

- **Windows** → DirectML execution provider (any DirectX 12 GPU — Nvidia, AMD, Intel).
- **macOS** → CoreML execution provider (Apple Silicon GPU/Neural Engine).
- **Linux / fallback** → CUDA, TensorRT, OpenVINO, or CPU.

So you train/export a YOLO model once, ship one `.onnx`, and get GPU acceleration on both target OSes with no per-platform code. The `usls` and Ultralytics-Rust libraries already wrap exactly this.

### Recommended topology

```
                 ┌─────────────────────────────────────────────┐
   IP cameras    │              Cammy                │
  (RTSP/ONVIF)   │                                             │
       │         │   ┌──────────┐   main stream (copy)         │
       ├────────────▶│  go2rtc  │────────────────┐             │
       │         │   │ gateway  │  sub stream     │            │
       │         │   └────┬─────┘  (low-res)      ▼            │
       │         │        │              ┌──────────────────┐  │
       │         │        ▼              │  Recorder (Rust) │  │
       │         │  ┌────────────┐       │  packets → disk  │  │
       │         │  │ Motion gate│       │  segment + index │  │
       │         │  │ (Rust/CV)  │       └──────────────────┘  │
       │         │  └─────┬──────┘                │            │
       │         │        │ motion ROI            │            │
       │         │        ▼                       │            │
       │         │  ┌────────────┐                │            │
       │         │  │ AI detector│  events        │            │
       │         │  │ ONNX/ort   │───────┐        │            │
       │         │  │ (YOLO)     │       │        │            │
       │         │  └────────────┘       ▼        ▼            │
       │         │                  ┌─────────────────────┐    │
       │         │                  │  Core API + SQLite  │    │
       │         │                  │  (events, config,   │    │
       │         │                  │   retention)        │    │
       │         │                  └──────────┬──────────┘    │
       └─ WebRTC ◀──────── go2rtc ◀────────────┘               │
                 │                  Web UI (browser)           │
                 └─────────────────────────────────────────────┘
```

**Components:**

1. **go2rtc** (bundled binary) — universal camera ingest + WebRTC/HLS restream for the live UI. One connection per camera; everything else pulls from its restream.
2. **Recorder** (Rust) — subscribes to each camera's main stream via go2rtc's RTSP restream, writes packets to disk without decoding (Moonfire's model: video to files, metadata to SQLite), handles segmenting and retention.
3. **Motion gate** (Rust) — decodes only the low-res sub stream, cheap pixel-difference, emits regions-of-interest. Gates the AI stage.
4. **AI detector** (Rust + `ort`/ONNX Runtime) — runs YOLO only on motion ROIs, per-OS GPU backend (DirectML/CoreML/CUDA). Emits typed events (person, vehicle, animal…).
5. **Core API + store** (Rust, e.g. Axum + SQLite) — config, camera registry, event log, retention policy, auth. Single source of truth.
6. **Web UI** (browser, TypeScript/React) — live grid via WebRTC, timeline scrubbing, event review. Pure web means zero per-OS UI work.

Packaging: one Rust binary that supervises/embeds the go2rtc and FFmpeg child processes, plus a static web bundle. Ship as a native installer per OS (and optionally Docker for the Linux/NAS crowd).

---

## 3. Recommended approach: fork-or-build decision

You have three honest options:

**A. Extend Frigate.** Best AI today, but Linux/Docker-bound and Python — fighting it onto native Windows/Mac with Apple-Silicon acceleration is swimming upstream. Good to run as a reference, poor to build on for your cross-platform goal.

**B. Build on Moonfire's foundation.** It's already a Rust NVR that records efficiently and cross-compiles. It deliberately has *no* motion or AI — which is exactly the layer you'd add. This is the highest-leverage starting point: you inherit the hard recording/storage core and bolt on the motion-gate + ONNX detector that nobody in the Rust ecosystem has packaged yet. That combination — Moonfire-class recording + Frigate-class detection, natively on Win/Mac — is a genuine gap in the market.

**C. Greenfield Rust.** Maximum control, maximum time. Only worth it if Moonfire's storage model doesn't fit.

**Recommendation: option B-flavored.** Architect as in §2, study Moonfire's recorder/storage design closely (reuse Retina for RTSP), bundle go2rtc for ingest/WebRTC, and make the AI detector (ONNX/`ort`) your differentiating first-party component.

---

## 4. Phased roadmap

**Phase 0 — Spike (prove the risky parts).** Stand up go2rtc against one real camera; view it via WebRTC in a browser. Separately, get `ort` running a YOLO model on a single image with the DirectML backend on Windows and CoreML on Mac. These two spikes de-risk 80% of the project.

**Phase 1 — Live viewing.** Camera registry + config store; go2rtc-driven multi-camera live grid in the web UI. No recording yet.

**Phase 2 — Continuous recording.** Rust recorder copying main-stream packets to disk (Moonfire model), SQLite index, retention/rotation, timeline playback in UI.

**Phase 3 — Motion detection.** Sub-stream decode + pixel-difference gate; event-based recording and zones/masks; basic alerts.

**Phase 4 — AI object detection.** ONNX/YOLO on motion ROIs, per-OS GPU backend, object classes and confidence thresholds to kill false alerts; notifications.

**Phase 5 — Polish.** Native installers (Win/Mac), auth/TLS, mobile-friendly UI, optional Docker image.

---

## 5. Core technology shortlist

- **Language:** Rust (core), TypeScript/React (web UI).
- **Camera ingest / WebRTC:** go2rtc (bundled binary).
- **RTSP client (in-process):** Retina crate.
- **Recording/codec edge cases:** FFmpeg (child process) where needed.
- **AI inference:** ONNX Runtime via `ort` crate; YOLO model; DirectML (Win) / CoreML (Mac) / CUDA (Linux).
- **API + storage:** Axum (or Actix) + SQLite, video segments on disk.
- **Reference to study:** Moonfire NVR (recording/storage), Frigate (detection pipeline design).

---

## Sources

- [Frigate vs Blue Iris — WunderTech](https://www.wundertech.net/frigate-vs-blue-iris/)
- [Who Makes the Best Home NVR Platform — Felenasoft](https://felenasoft.com/xeoma/en/articles/who-makes-the-best-home-nvr-platform/)
- [Best ZoneMinder Alternatives — SimpleHomelab](https://www.simplehomelab.com/best-zoneminder-alternatives-2023/)
- [Frigate vs Shinobi — selfhosting.sh](https://selfhosting.sh/compare/frigate-vs-shinobi/)
- [Frigate docs — Object Detectors](https://docs.frigate.video/configuration/object_detectors/)
- [Frigate docs — Introduction](https://docs.frigate.video/)
- [Configuring go2rtc — Frigate docs](https://docs.frigate.video/guides/configuring_go2rtc/)
- [go2rtc — GitHub (AlexxIT)](https://github.com/AlexxIT/go2rtc)
- [go2rtc.org](https://go2rtc.org/)
- [Retina RTSP library — GitHub (scottlamb)](https://github.com/scottlamb/retina)
- [Moonfire NVR — GitHub (scottlamb)](https://github.com/scottlamb/moonfire-nvr)
- [ort — ONNX Runtime Rust bindings (pykeio)](https://github.com/pykeio/ort)
- [usls — Rust ONNX vision library (jamjamjon)](https://github.com/jamjamjon/usls)
- [ONNX Runtime — microsoft/onnxruntime](https://github.com/microsoft/onnxruntime)
