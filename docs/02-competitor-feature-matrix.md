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
| 14 | Face recognition / LPR | Frigate 0.16 (free since 0.16) | later — model sourcing + privacy defaults |
| 15 | MQTT broker integration (Home Assistant) | Frigate | later — webhook covers notify path first |
| 16 | Auth + HTTPS for off-LAN exposure | all commercial | ✅ shipped (LAN password; argon2+HTTPS still needed for WAN) |

Sources: [Frigate docs](https://docs.frigate.video/), [Frigate releases](https://github.com/blakeblackshear/frigate/releases),
[Frigate review system](https://docs.frigate.video/configuration/review/),
[Blue Iris](https://blueirissoftware.com/), [BI alarm-server webhooks](https://wiki.instar.com/en/Frequently_Asked_Question/BlueIris_v5_http_alarmserver/),
[UniFi Protect G6 coverage](https://www.thesmarthomehookup.com/unifi-protect-got-amazing/),
[BI vs UniFi comparisons](https://ipcamtalk.com/threads/blue-iris-vs-unifi-protect.67055/).
