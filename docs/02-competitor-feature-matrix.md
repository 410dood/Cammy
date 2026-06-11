# Competitor feature matrix & adoption backlog

*Surveyed June 2026. Complements `01-research-and-architecture.md` (field survey);
this doc drives the post-v0.1 feature backlog: what the incumbents do best and
which of those features we adopt, in what order.*

## The field

| Product | Platform | Strengths to steal | Weaknesses we exploit |
|---|---|---|---|
| **Frigate 0.16** | Linux/Docker | Two-stage motion→AI (our model already), zone/mask editor, review items split into alerts vs detections, event clips, sub-stream "detect role", MQTT events, face/LPR (now free) | No native Windows/macOS; needs Docker + accelerator setup |
| **Blue Iris 5** | Windows only | 64-camera scale, alarm-server webhooks on events, ONVIF events/PTZ, audio recording, profiles/schedules, &Deadband-style alert tuning | Windows-only, dated UI, per-seat license |
| **UniFi Protect (G6 era)** | UniFi hardware only | Timeline scrubbing UX, smart detect (person/vehicle/pet), camera health/status surface, storage dashboard | Closed ecosystem — only UniFi cameras |
| **Scrypted** | Node, cross-platform | Plugin architecture, HomeKit/Alexa/Google bridging, fast WebRTC | AI depends on plugins/cloud; core is a bridge more than an NVR |
| **ZoneMinder / Shinobi / MotionEye** | Linux | Long-tail camera support, mature retention models | Aged stacks, weak/no native AI, heavy setup |

## Feature adoption backlog (ranked)

| # | Feature | Inspired by | Status |
|---|---|---|---|
| 1 | Close-to-tray desktop NVR semantics | Blue Iris service mode | ✅ shipped |
| 2 | Bundled ffmpeg/go2rtc/model in installer | Blue Iris all-in-one install | ✅ shipped |
| 3 | Event → recording playback at timestamp | UniFi timeline, Frigate review | ✅ shipped |
| 4 | Camera health (online/offline, last frame, fps) | UniFi status surface, BI watchdog | ✅ shipped |
| 5 | Per-camera detect config: labels, thresholds, ignore zones | Frigate zones/masks, BI per-camera profiles | ✅ shipped |
| 6 | Webhook on event (alarm-server style) | Blue Iris alarm server, Frigate MQTT | ✅ shipped |
| 7 | Audio in recordings (AAC) | Blue Iris | ✅ shipped |
| 8 | Storage dashboard (per-camera usage, est. days left) | UniFi | ✅ shipped |
| 9 | Sub-stream detect role (decode low-res, record 4K) | Frigate detect role, BI dual-streaming | ✅ shipped (validated on real Dahua 4K) |
| 10 | Timeline scrubber UI across segments + event markers | UniFi Protect | ✅ shipped |
| 11 | Review split: alerts (person/car in zone) vs detections | Frigate 0.14 | ✅ shipped |
| 12 | Event clips (pre/post-roll MP4 export) | Frigate clips, BI export | ✅ shipped |
| 13 | ONVIF discovery + PTZ | Blue Iris, Frigate 0.16 | ✅ shipped (validated on Amcrest IP2M-866EW) |
| 14 | Face recognition / LPR | Frigate 0.16 (free since 0.16) | ✅ both shipped (faces validated live; LPR char-accurate on stills, validated on live stream) |
| 15 | MQTT broker integration (Home Assistant) | Frigate | ✅ shipped (verified against local broker) |
| 16 | Auth + HTTPS for off-LAN exposure | all commercial | ✅ shipped (LAN password; argon2+HTTPS still needed for WAN) |
| 17 | Natural-language smart search (CLIP) | UniFi AI Key | ✅ shipped (validated ranking on live events) |
| 18 | PTZ autotracking | Frigate 0.13 | ✅ shipped (closed loop validated in daylight on the Amcrest: car detected below center → tilt-down burst → frame re-centered, 38% pixel shift; velocity floor 0.4 + offset-scaled burst added because the motor ignores small velocities over short bursts) |
| 19 | Audio event classification (YAMNet) | Frigate, UniFi AI | ✅ shipped (validated live: tone -> alarm event @0.92 with snapshot) |
| 20 | Enhanced Retention (re-encode aging footage) | UniFi Protect | ✅ shipped (validated: 16.4MB segment -> 2.5MB, playable) |
| 21 | One-click network camera scan (WS-Discovery) | Blue Iris "Find", Synology camera search | ✅ shipped (validated: found 7 LAN cameras incl. both real ones; probes every local interface to survive WSL/Hyper-V multi-homing) |
| 22 | Phone push notifications via ntfy (snapshot attached) | UniFi/Reolink push, Frigate notify add-ons | ✅ shipped (validated live: rule fired, ntfy.sh delivered title + message + snapshot.jpg) |
| 23 | Configurable storage location + free-space display | Blue Iris multi-drive clips, Synology volumes | ✅ shipped (validated: segments wrote + indexed to alt dir, recorders auto-restart on dir change, bad path rejected with 400, free space shown) |
| 24 | Alarm rule schedules (days + time window, overnight) | Blue Iris profiles/schedules | ✅ shipped (validated live: in-window rule fired 5x while night-window and wrong-day rules on the same events stayed silent; unit tests cover overnight + day filters) |
| 25 | Event-only recording retention (per camera) | Frigate retain modes | ✅ shipped (validated live: swept 235 eventless porch segments after 15-min grace, kept the segment with a nearby event) |
| 26 | PWA install (manifest + icons, add-to-home-screen) | UniFi/Reolink mobile apps | ✅ shipped (manifest served as application/manifest+json with 192/512 + maskable icons, apple-touch-icon, theme color) |
| 27 | Camera disconnect push alerts (offline / back online) | UniFi Protect, Reolink, Blue Iris watchdog | ✅ shipped (validated live: killed a camera's source → "Camera offline" ntfy push with the error, restored it → "back online" push; intentionally disabled cameras don't alert) |
| 38 | Opt-in GenAI event captions (local Ollama or cloud) for review + search | Frigate+LLM community add-ons, UniFi AI descriptions | ✅ shipped (a captioner worker thread (off the detection path) sends an event snapshot to an Ollama-compatible vision model and writes a one-line description onto the event; displayed on cards and stored for search. **Explicit opt-in, OFF by default** — default endpoint is localhost Ollama so nothing leaves the machine unless the user points it at a cloud URL (bearer-key supported). Unit-tested request builder + Ollama/OpenAI response parsing; live captioning needs a running Ollama to validate) |
| 37 | Face recognition as per-camera opt-in + labeled-faces management | Frigate/UniFi face rec (opt-in, gated behind person detection) | ✅ shipped (per-camera `face_recognize` override — enable matching only where wanted (e.g. front door), inheriting the global switch otherwise; still gated behind person detection. Enrolled-identity rename (PATCH /api/faces/:id → relabels all that person's embeddings) added to the existing Faces management UI alongside enroll/forget. Builds on shipped face rec #14) |
| 36 | LPR vehicles-of-interest: plate allow/deny lists | Frigate LPR + UniFi/Reolink plate alerts | ✅ shipped (case-insensitive substring allow/deny lists tolerant of partial OCR; a deny-list "vehicle of interest" read fires a guaranteed high-priority push independent of alarm rules; Events badges plates ⚠ of interest / ✓ known. Unit-tested `plate_status` (deny > allow). Builds on the shipped LPR #14) |
| 35 | Duress/help hand signal (silent panic) + touchless PTZ control | Differentiator — personal-safety angle no NVR ships | ✅ shipped (a configurable `gesture_duress` signal always fires even when not armed; the gesture event is flagged duress → ntfy pushes go out at max urgency with a siren tag + "🚨 DURESS" title, plus a guaranteed direct push to the health ntfy topic so it alerts with zero alarm-rule setup. Touchless PTZ: on a PTZ camera, an open palm steers toward the hand's position and a fist stops — hands-free control from the Signals page. Backend build/clippy/test green; live overlay/PTZ need a webcam+camera to validate) |
| 34 | Per-camera detector/accelerator assignment + FPS governance + HW-accel encode + live metrics | Frigate detector groups, BI per-camera limits, UniFi perf surface | ✅ shipped (pipeline keeps one ONNX session per (model, accelerator, conf, iou); per-camera `model` override, `force_cpu` GPU/CPU assignment, and `poll_ms` FPS cap (resource governance); inference latency + accelerator + model recorded per camera and surfaced in a Perf column. Enhanced-retention re-encode can use NVENC/QuickSync/VideoToolbox with automatic CPU fallback (`hwaccel` setting). Build/clippy/test green; HW paths need GPU hardware to validate) |
| 33 | Restreaming fan-out: per-viewer WebRTC / MSE / MJPEG transport selection | go2rtc restreaming (the engine we already supervise) | ✅ shipped (the single-upstream→many-clients fan-out is inherent to the supervised go2rtc — this exposes a per-viewer transport picker: WebRTC for lowest latency with automatic MSE fallback, MSE for WebRTC-blocked/TCP-only networks, MJPEG for maximum compatibility. Preference persists in localStorage and applies to the Live grid + camera detail. No extra camera load) |
| 32 | Event review: zone tag on events + zone/time filters + Explore counts + thumbnails | Frigate Explore/review, UniFi timeline filters | ✅ shipped (events are tagged with the required-zone they occurred in; Events page filters by object/zone/camera/face/gesture/plate and a from–to time window (server-side `after`/`before`); a Frigate-style "Explore" header shows per-object counts as quick-filter chips; grid images load via on-the-fly cached `?w=` thumbnails. Existing Timeline scrubber retained. Unit-tested zone+time queries) |
| 31 | Home Assistant MQTT auto-discovery + detection-state entities + webhook templating | Frigate HA integration, BI alarm-server templating | ✅ shipped (publishes retained HA discovery configs → a `binary_sensor` per (camera, object) with auto-ON/OFF + a per-camera last-detection `sensor`, grouped under one HA device; re-published when the camera/label set changes. `{prefix}/{camera}/{slug}/state` ON/OFF topics + per-camera event JSON. Webhook body templating with `{{placeholder}}` substitution (JSON-escaped) across per-event + alarm + gesture webhooks; documented topic/payload/template schema in docs/03. Unit-tested template render; needs a live broker re-validation) |
| 30 | Anti-fatigue notifications: per-rule cooldown + snooze, clip-link & priority push | Frigate notify cooldowns, UniFi snooze, Reolink priority | ✅ shipped (per-rule `cooldown_secs` enforced via a shared in-memory throttle across pipeline/audio/gesture dispatch; manual `snooze`/`wake`; ntfy pushes carry `X-Priority` and, when a public base URL is set, tap-through "View clip"/"Snapshot" action links. Unit-tested. **Native WebPush deferred** — needs a VAPID/web-push crypto+HTTP dep stack (isahc/openssl) and browser+push-service validation that can't be exercised in this environment; ntfy already delivers thumbnail+clip push) |
| 29 | Polygon detection zones (required/ignore) + privacy masks + object-size filters | Frigate zones/masks, BI object-size, UniFi privacy zones | ✅ shipped (per-camera polygon zones with even-odd point-in-polygon test, required vs ignore + per-zone label scoping; privacy-mask polygons blacked out of the frame before motion/detect/snapshot; min/max object-area gate; visual editor draws on a same-origin `/api/cameras/:id/frame.jpg` still; legacy rectangle ignore_zones still honored. Unit-tested; needs live in-browser editor validation) |
| 28 | Hand-signal recognition (live 21-landmark mesh + held-gesture triggers) | Differentiator — no incumbent ships this; MediaPipe Hands | ✅ shipped (Signals page: GPU hand-landmark tracking in-browser via MediaPipe Tasks Vision, draws the skeletal mesh, classifies open-palm/fist/victory/point/thumb-up/down/I-love-you; a held armed signal POSTs `/api/gesture` → a `gesture` event with a context snapshot that fires matching alarm rules — a silent hand-signal "panic button". `gesture` crate has a pure, unit-tested geometric classifier + name taxonomy; per-camera toggle, Settings knobs, Alarm `gesture` condition, Events chip/filter. Needs in-browser live validation against a webcam) |

Sources: [Frigate docs](https://docs.frigate.video/), [Frigate releases](https://github.com/blakeblackshear/frigate/releases),
[Frigate review system](https://docs.frigate.video/configuration/review/),
[Blue Iris](https://blueirissoftware.com/), [BI alarm-server webhooks](https://wiki.instar.com/en/Frequently_Asked_Question/BlueIris_v5_http_alarmserver/),
[UniFi Protect G6 coverage](https://www.thesmarthomehookup.com/unifi-protect-got-amazing/),
[BI vs UniFi comparisons](https://ipcamtalk.com/threads/blue-iris-vs-unifi-protect.67055/).
