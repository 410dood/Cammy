# docs/08 — 2026-07 Competitor Parity Study & Unified Roadmap

*Surveyed 2026-07-02 via a 4-agent research pass over the user-supplied 30-product
field: a UniFi Protect deep-dive (the stated "balance" benchmark), enterprise VMS
(Nx Witness, Eagle Eye, Qognify/HxGN, Axis Camera Station Pro, Hanwha WAVE + a
refresh on Milestone/Genetec/Avigilon/Verkada/Bosch), prosumer (Synology
Surveillance Station deep, ZoneMinder 1.38, Shinobi, Bluecherry, Agent DVR, the
Home Assistant ecosystem + Blue Iris 6 / Frigate 0.17 / Scrypted refreshes), and
consumer (Lorex, Amcrest, Blink, Tapo/Aireal — never surveyed — + what's new in
Ring/Nest-Gemini/Arlo 6/Eufy S4/Wyze/Reolink-ReoNeura since docs/06). Every gap
was checked against the shipped feature set (#1–#70 commercial, R01–R24
residential, docs/06 backlog) so this lists only genuine gaps. Supersedes and
absorbs the docs/06 ranked list into one unified roadmap.*

---

## Executive summary

**Cammy already exceeds every product in this field on raw capability breadth**
— no competitor at any tier combines our analytics (tracker, tripwires,
occupancy, speed, heatmaps, Re-ID, gait), AI stack (CLIP NL search, YAMNet,
whisper, local VLM captions), residential safety suite, and WAN security
(2FA/RBAC/SSO/audit) in one free local package. The gaps are almost never
*capability*; they are **curation, notification quality, camera-ecosystem
leverage, and packaging**:

1. **UniFi wins on curation, not capability.** Its "fantastic balance" comes
   from investing in *output* UX — a curated "what matters" feed (Spotlights),
   detection clustering into sessions with motion-path thumbnails, one unified
   search surface, one Alarm Manager — while gating features by hardware tier
   instead of settings sprawl. Our parity work should copy the curation, not add
   knobs.
2. **The 2026 notification bar moved**: severity-tiered filtering (Wyze NBD 1–5,
   Reolink L1–L4), burst consolidation (Blink Single Event Alert, Tapo, UniFi
   sessions), and the AI description *in the push* (Blink/Wyze/Ring/Nest/Arlo —
   all subscription-gated). We compute every input (anomaly_score, eventGroups,
   captions) but wire none of it into the push path.
3. **Camera-side AI ingestion is the field's most-repeated prosumer feature**
   (Blue Iris 6 ONVIF triggers + event inspector, ZoneMinder 1.38 event
   listener, Agent DVR, Synology, Axis ACS's whole thesis, Hanwha WAVE). 2026
   cameras ship NPUs; consuming their ONVIF event streams cuts our GPU cost to
   ~0 per camera and is our biggest architectural gap.
4. **Two vendors now sell our exact thesis as appliances** — Eufy S4 Max
   ("world's first NVR with local AI agent") and Reolink's subscription-free AI
   Box (ReoNeura: NL search, Smart Summary, L1–L4 tiers, prompt-based alerts).
   Differentiation must come from openness (any camera brand) + analytics depth
   + the software-only cross-platform story.
5. **Blue Iris 6 shipped built-in DirectML YOLOv8** (128 cams, custom ONNX
   models, NVENC) — direct validation of our architecture, and it erodes our
   "BI needs CodeProject.AI" talking point. The single-detection-thread ceiling
   (docs/06 rec #12) matters more now.
6. **Packaging remains the biggest adoption blocker**: no Docker/compose path
   and no zero-port-forward remote access story, while Agent DVR/Scrypted/
   Synology monetize exactly that friction.

## What "UniFi balance" means for us (design guardrails)

- **Curation-first review**: invest in what the user *sees on open* (ranked
  feed, sessions, paths), not in more analytics toggles.
- **One Alarm Manager**: every new trigger/action lands in the existing alarm
  rule engine — never a per-feature notification setting.
- **Capability gating over configuration**: features appear when their
  prerequisite exists (model downloaded, camera capability detected), mirroring
  UniFi's hardware-tier gating — this is also docs/06 rec #8.
- **Opinionated defaults, few knobs**: prefer a good default + severity slider
  over exposing thresholds.
- **Deliberate omissions are a feature**: see the anti-feature list — the
  leanness IS the product.

---

## Unified ranked roadmap

Value is for the home/prosumer self-hoster. "docs/06 #N" marks items absorbed
from the previous backlog. Phases are effort-honest groupings, each phase
roughly in priority order.

### Phase 1 — quick wins (S effort, mostly reuse; the notification-quality + curation sprint)

| # | Item | Inspired by | Why / reuse |
|---|------|-------------|-------------|
| P1.1 | **Severity-tiered notifications**: map every event to a visible 1–4 severity (label class weights + anomaly_score + "new object" from stationary-suppression); per-user "notify ≥ N" gate; critical classes (glass/smoke/fall/duress) auto-max | Wyze NBD 1–5, Reolink L1–L4 | anomaly_score, ntfy/WebPush priority, cooldown all shipped — a mapping + one setting + an Events badge |
| P1.2 | **Burst-consolidated alerts**: one push for N similar events in a window ("3× person at Driveway in 10 min") | Blink Single Event Alert, Tapo, UniFi sessions | move the `eventGroups.ts` (camera,label,window) key server-side in front of the push dispatcher |
| P1.3 | **Caption-in-push** (docs/06 #4): the #38 GenAI description in the notification text, re-fired/enriched async | Wyze/Blink/Ring/Nest/Arlo descriptive alerts | captioner + notify fan-out + `{{transcript}}` precedent |
| P1.4 | **Photo-upload search** (docs/06 #1): POST an image → CLIP-rank the crop corpus | UniFi Protect 7.0 "Find Anything" | ~80% shipped: crop embeddings + `/similar` ranker; add multipart + vision-embed session |
| P1.5 | **Detection sessions + motion-path thumbnails**: cluster related detections into one card; draw the track polyline on the snapshot/preview | UniFi 7.0, Frigate 0.17 path overlay (docs/06 path item) | groupEvents + tracker `Track.history` + the motion-highlight snapshot burner; needs the `path_json` persist docs/06 scoped |
| P1.6 | **Activity-sorted live grid**: cameras currently showing people/vehicles float to the top of Live/Wall | Eagle Eye Smart Layouts | StatusBoard already knows per-camera live detections; sort the grid |
| P1.7 | **Soft triggers**: user-defined buttons on a camera tile ("Delivery arrived") that fire the alarm engine + create a bookmarked event | Nx Witness, WAVE | the gesture-event POST path is the template |
| P1.8 | **Absence/inactivity detection**: alert when no person/pet seen in a zone by/for N hours — aging-in-place & pet fit | Verkada inactivity, Family hub mandate | inverted loiter over tracker+zones+scheduler+alarm engine |
| P1.9 | **Alarm rule Test button + last-fired/24h hit stats** | UniFi Alarm Manager 6.0 | events count query + a test-dispatch endpoint |
| P1.10 | **Notification-snapshot privacy masking** (privacy correctness): burn privacy masks into snapshots leaving the box (push/webhook/email) — today the blur is a live CSS overlay only | Synology 9.2 | mask polygons + `save_snapshot` compositing; adjacent to `no_clip` |
| P1.11 | **External trigger API**: `POST /api/trigger/{camera}` + MQTT-subscribe trigger source — doorbells, alarm-panel PIRs, HA sensors synthesize events through the normal snapshot/alarm path | Blue Iris DIO, Synology action rules, UniFi Alarm Hub | gesture endpoint as template; MQTT client exists (outbound-only today) |
| P1.12 | **Deployment + remote-access docs** (docs/06 #13 + new): Dockerfile/compose + systemd + DEPLOYMENT.md **plus** a first-class Tailscale/cloudflared zero-port-forward guide + a Settings "remote access" checker | the whole field; Agent DVR/Scrypted monetize this friction | docs + config only; never build our own relay |
| P1.13 | **Digest-with-clips push**: the daily digest delivered as a push with the day's key clips linked | Nest "Home Brief", Tapo Aireal | digest worker + anomaly ranking + WebPush clip links — join them |
| P1.14 | **Footage-access auditing**: log clip views/downloads/exports/deletions per user in the existing audit_log | UniFi 6.0 expanded audit | add events on recording/clip endpoints |
| P1.15 | **Event tagging**: arbitrary multi-tags on events + tag filter | ZoneMinder 1.38 | bookmarks/notes columns + Events filter UI are the template |
| P1.16 | **Showreel/cycling + calendar picker** (polish): auto-rotating liveview tours (kiosk); month calendar showing recording/event days | Nx/WAVE | Liveviews+Wall+WakeLock; segment index by day |

### Phase 2 — medium efforts (the forensic + ecosystem sprint)

| # | Item | Inspired by | Why / reuse |
|---|------|-------------|-------------|
| P2.1 | **ONVIF camera-side analytics ingestion** — subscribe to camera IVS/smart events (pull-point) as a first-class event source, AND-able with server-side YOLO via cross-modal confirm; ship a Blue-Iris-style live ONVIF event inspector for debugging | Blue Iris 6, ZoneMinder 1.38, Agent DVR, Synology, Axis ACS, WAVE | the field's most-repeated prosumer gap; ONVIF stack + alarm engine + confirm_ok all reusable; ~0 GPU cost per NPU camera |
| P2.2 | **Prompt-based standing NL alert rules**: a persistent rule from typed text ("someone climbing the fence") — CLIP text-embed once, cosine vs per-crop embeddings at detection time, as a new AlarmRule condition | Reolink ReoNeura, Arlo Custom Detection | the most stealable genuinely-new consumer idea; distinct from ask-your-cameras (rule creation, not Q&A) |
| P2.3 | **Retroactive region motion search**: draw a region on recorded timeline → all archived motion in it | Nx Witness's most-loved feature, WAVE, Axis | persist the already-computed 64×64 motion masks per segment to SQLite; ZoneEditor + timeline for UI |
| P2.4 | **Thumbnail scrub search**: a day as a recursive thumbnail grid — eyeball-search in seconds | Nx, WAVE | ffmpeg keyframe extraction over segments |
| P2.5 | **CLIP attribute facets** (docs/06 #5): vehicle color/type, clothing color as filter chips + `attr_like` alarm condition | UniFi G6, Eagle Eye, Bosch, Sighthound | zero-shot prompt bank over stored crops; keep make/model OUT (needs a new model) |
| P2.6 | **Spotlights-style curated Home feed**: rank recent events (anomaly × identity × severity), learn from show-more/less feedback | UniFi 6.0 Spotlights | anomaly_score + faces/LPR + P1.1 severity; pairs with docs/06 #3 feedback loop |
| P2.7 | **Shareable expiring clip links**: tokenized single-resource URL with expiry ("send the porch clip to police") | Nx 6.1, every consumer app | API-token infra: scope=one-clip + expiry; needs user's reachable origin (P1.12) |
| P2.8 | **VLM alert-verification gate + per-camera feedback learning** (docs/06 #2+#3): local-LLM yes/no AND-gate; thumbs-down suppresses CLIP-similar future alerts | Agent DVR, Bosch IVA Pro Context | fail-OPEN, off the detection thread; crop embeddings + confirm_ok pattern |
| P2.9 | **Deterrence actions**: alarm action = play WAV / trigger camera siren/white-light (ONVIF) at the camera, arm-mode-gated; escalation ladder later | Ring Active Warnings, Lorex Smart Deterrence, Verkada (shipped 2/2026), docs/06 talk-down | two-way-audio backchannel + ONVIF outputs; build defensive — backchannel sink never live-validated |
| P2.10 | **Presence/geofence arm**: `POST /api/arm` for HA/Tasker/Shortcuts (S); multi-phone first-in/last-out mode logic | Synology Home Mode, UniFi location notifications | arm-mode KV is authoritative; avoid flaky PWA geolocation |
| P2.11 | **Per-user notification matrix**: push/email per user × rule/camera | UniFi Alarm Manager | RBAC users + WebPush + rules; a delivery-preference table |
| P2.12 | **Smart event-aware time-lapse**: condense a day into ~60s, near-real-time around events | Synology Smart Time Lapse | ffmpeg select/setpts over segments; digest worker schedules |
| P2.13 | **Evidence-grade export**: drawtext watermark (who/when) + SHA-256/Ed25519 signed manifest + `zoomy verify`; skip at-rest encryption (OS's job) | Synology Evidence Integrity Authenticator | clip-export path + in-tree hex/sha util |
| P2.14 | **Selective offsite + capacity-adaptive retention**: back up only event/bookmark segments (choose hi/lo stream); retention policy that degrades quality before deleting detection footage | Nx metadata-driven backup, UniFi 7.0 smart storage | offsite S3 worker + segment/event join + re-encode aging |
| P2.15 | **Analytics trends dashboard**: counts/occupancy/audio-event charts over days/weeks with thresholds | Axis Data Insights, Reolink Smart Summary | pure frontend over `/api/analytics/*` + digest data |
| P2.16 | **Object lifecycle detail view**: per-track narrative (entered zone → stationary → crossed line), click-to-seek | Frigate 0.17 Detail history | tracker+zones+tripwires+timeline exist; read-side aggregation (extends P1.5's path_json) |
| P2.17 | **Model-presence status + guided download; GenAI failure surface** (docs/06 #7/#8/#11 — the silent-failure cluster) | internal; UniFi-style capability gating | the honest prerequisite for every AI feature above |
| P2.18 | **Camera hotspots**: clickable in-video links to adjacent cameras | Nx Gen 6 | floor-plan pins + LiveVideo overlay |

### Phase 3 — large / strategic

| # | Item | Inspired by | Notes |
|---|------|-------------|-------|
| P3.1 | **Journey fusion capstone** (docs/06 #9, unified): auto-link cross-camera sightings → one updating notification + click-a-person → their appearances everywhere + FloorPlan path + optional stitched "Moments" export clip | Apple iOS 27 Home, UniFi click-to-journey, Eufy cross-camera handoff, Blink Moments, Verkada unified timeline | Re-ID + tracker + CrossTimeline + ffmpeg concat; honest-scope as appearance-similarity |
| P3.2 | **Ask-your-cameras** (docs/06 #6): bounded read-only tool-loop over events/search/counts | Nest Gemini Ask Home, Tapo Aireal, Reolink, Milestone | needs a tool-calling local model; off by default; untrusted tool output |
| P3.3 | **First-class Home Assistant integration** (docs/06): SSE event feed + inbound MQTT commands + HACS component | HA ecosystem (LLM Vision pattern) | P1.11 delivers the inbound-command half |
| P3.4 | **HomeKit (HAP) bridge** (docs/06): go2rtc does AV; event→characteristic glue is the new work | Scrypted's flagship | batch 1 live-view-only |
| P3.5 | **OpenVINO EP + trainable classifiers** (docs/06 #10 + XL item): named-accelerator dropdown; state-classifier vertical first (garage open/closed) — ship a **CLIP zero-shot zone-state v0** with no training at all | Frigate 0.17 (incl. Intel/Apple NPU detectors), Blue Iris 6 custom models | CLIP session exists for the v0 |
| P3.6 | **Detector worker pool** (docs/06 #12): overlap fetch/decode/motion/enrichment; multi-accelerator | Blue Iris 6's 128-cam headline | more urgent now BI6 bundles DirectML YOLO |
| P3.7 | **Adaptive-bitrate remote playback + dual-stream recording**: record sub-stream alongside main; scrub lo-res, play hi-res; quality selector | Scrypted (flagship), Nx/WAVE | sub-stream role exists; second ffmpeg copy per camera |
| P3.8 | **Detection-triggered recording mode**: per-camera "record only around detections" with pre/post-roll as a first-class alternative to continuous+retention | Axis ACS | recorder gating; event-only retention is the adjacent shipped primitive |
| P3.9 | **Pull-based two-box archive** (Archive Vault pattern): a second Cammy pulls selected cameras/events from the primary | Synology Archive Vault | the useful 90% of "multi-server" — build this and stop |
| P3.10 | **Virtual camera / offline footage import**: import phone/dashcam video into the searchable archive | Nx Witness | remux + run the existing pipeline over it |

### Deferred / watch

- **Matter 1.5 camera device** — rs-matter still immature (docs/06 verdict stands).
- **Edge recording gap-fill** (ONVIF Profile G SD-card re-fetch) — per-vendor XL; document as known limitation.
- **iOS critical alerts (DND bypass)** — needs a native app entitlement; interim: ntfy max-priority on Android + documented escalation bridges. Fold into any future mobile-app decision.
- **Consumer-cloud offsite (Drive/Dropbox)** — document rclone-as-S3-gateway instead of bespoke OAuth per provider.
- **Face/image enhancement (super-res)** — Tier 1 classic-CV enhance (docs/06) is enough; model-based upscaling later.

## Anti-features — deliberately NOT building (the UniFi lesson: leanness is a feature)

- **Any subscription/per-camera licensing**, camera-count paywalls, paywalled AI (Blink charges $6.99/mo for person detection — our marketing copy should say every AI feature is free).
- **Own NAT-relay infrastructure** (forces a subscription eventually — iSpy's whole business model); document Tailscale/cloudflared instead.
- **Enterprise ops**: federation/multi-site orgs, PSIM/case management (Qognify's entire value prop), access control/door controllers/SIP speakers/body-worn, video-wall matrix + joystick decks, LDAP/AD sync (forward-auth SSO suffices), compliance certs, license failover.
- **Community surveillance** (Ring Search Party drew congressional fire), cloud face rec defaults, engagement-bait/teaser notifications, upsell chrome in the event feed.
- **Weapons/PPE detection** (liability, near-zero home value; PPE only ever via user-trained classifiers).
- **Cloud LLMs in the default alert path** (opt-in provider at most; default stays Ollama-local).
- **Recording-at-rest encryption** (Synology needs it for NAS multi-tenancy; on a dedicated box it's the OS's job) and per-frame JPEG debris (packet-copy purity is a performance moat).
- **Feature fragmentation** (Lorex Connect vs Classic): one UI for everything, forever.
- **Battery-style degradation patterns** (auto energy-saving that silently disables detection): continuous recording is the differentiator.
- **Proprietary hardware ambitions** (sensors/doorbells/chimes/ViewPort): integrate via ONVIF/MQTT instead.

## Threats & positioning notes

- **Eufy S4 Max + Reolink AI Box** commoditize "local AI, no subscription" as appliances → our wedge narrows to: any camera brand, cross-platform software-only, analytics depth, open API/HA, and transparency.
- **Blue Iris 6** (built-in DirectML YOLO, 128 cams, custom ONNX) closes its AI-setup gap → our wedge vs BI is macOS/Linux, modern UI, the AI-experience layer (search/captions/Re-ID), and free.
- **Frigate 0.17** (Intel/Apple NPU detectors, state classifiers, lifecycle view) keeps pace on detection plumbing → our wedge is native Windows/macOS, the integrated suite (faces/LPR/audio/transcripts/analytics without add-ons), and UX.
- **ZoneMinder 1.39** will grow first-party object detection — the "legacy = no AI" framing expires this year.

## Sources

Per-agent source lists retained in the four research reports (2026-07-02 session);
headline primary sources: blog.ui.com/help.ui.com (Protect 6.0–7.1), Nx Witness
Gen 6/6.1 release notes, Axis Camera Station Pro feature guide, Hanwha WAVE
feature/licensing pages, Synology Surveillance Station 9.2/DSM 7.4 KB,
ZoneMinder 1.38 release notes, Blue Iris 6 changelog, Frigate 0.17 release
notes, Scrypted NVR docs, ispyconnect.com (Agent DVR), blinkforhome.com plans,
tp-link.com Aireal, blog.ring.com CES 2026, Google Gemini for Home help, Arlo
Secure 6, eufy.com S4 Max, Wyze NBD blog, reolink.com ReoNeura/CES 2026,
Eagle Eye Smart Video Search/Automations, Milestone XProtect 2026 R1, Verkada
AI-Powered Deterrence (Feb 2026), Bosch BVMS 12.3.
