# docs/11 — Priorities, second sweep (2026-08-07)

Produced after the docs/10 control-UX backlog shipped in full (P1+P2+P3,
commits `8b1631b`…`4f5688c`). Method: three parallel audits — (1) a fresh
control-UX pass over `web/src` with the docs/10 lenses, (2) a
capability-vs-surface + silent-failure sweep of `crates/core`, (3) a full
inventory of every known deferred/open item across CLAUDE.md, docs/ and code.
Findings verified against file:line; items that already shipped were dropped
(see "Verified closed" at the bottom).

**The headline shift: the biggest remaining risks are TRUST failures, not
control polish.** The docs/10 work made configuration honest at config time;
this sweep found the places the system can degrade silently at *run* time.
Those outrank everything below them — this codebase's history (the health
guardian, "Test always claimed success", "recording meant an ffmpeg process
exists", the 6-hour-retention surprise) is precisely a list of what silent
degradation costs.

---

## P0 — silent failures that lose alerts or footage (fix first)

> **STATUS 2026-08-07: ALL of P0 is SHIPPED** — P0.1–P0.6 (`77dc3c6`,
> `7c04282`, `8a37755`, `50d6d1b`, `a435fa1`) and the P0.7–P0.9 watchdog cluster
> (`4d6e06b`, `3310177`, `24478f8`), each live-validated on :8081 with owner
> state snapshotted and restored (diff NONE every time).
>
> Three things the audit did not name, found while verifying:
> - A lost `VlmGate` job is **unrecoverable**, not merely late: `notify::ready`
>   stamps the cooldown and `take_suppressed` drains the burst counter at the
>   dispatch site, *before* the hand-off. Nothing retries it.
> - The GenAI worker discarded its whole backlog on shutdown, silently.
> - `parse_response` guarded on BYTES and indexed by CHARS, so any caption over
>   280 bytes but under 280 chars (one accent or emoji) **panicked the GenAI
>   worker dead for the life of the process** — captions *and* every deferred
>   VLM alarm fire, gone until restart. Fixed with the queue work.
>
> Also measured live and fixed beyond the letter of P0.2: with a stalled model,
> a fired alarm sat behind six queued captions (~6 min late). Split caps only
> help at saturation, so alarm fires now have their own channel and are always
> taken first — verified delivering an alert with 12 captions queued ahead of it.

1. ~~**Detector load failure is invisible AND mis-diagnosed as "offline"**~~
   **DONE `77dc3c6`** — `CamHealth.detector_error` is a separate axis from
   `last_error`; the loop no longer `continue`s past the frame fetch, so
   reachability/tamper/motion keep working and only detection is skipped;
   "Model failed to load" badge on Cameras + an amber Live chip. Live: the
   camera read `online: true` with the model error named, and cleared on repair.
   (`pipeline.rs:487`). A bad/missing model or EP init failure `debug!`s and
   `continue`s *before* the frame fetch, so `last_frame_ts` stays None and the
   camera reads **offline** — the owner chases a network fault while the model
   is broken. Fix: `CamHealth.detector_error`, a distinct "Model failed to
   load" badge on Cameras, `warn!`.
2. ~~**The GenAI queue is unbounded and carries ALARM FIRES**~~ **DONE
   `7c04282`+`a435fa1`** — explicit depth counter (not `sync_channel`, whose
   send blocks the detection thread); captions shed at 64, alarm fires at 512;
   alarm fires on their own channel, taken first; shutdown drains alarms and
   SAYS what it could not deliver; `zoomy_genai_queue_depth` +
   `zoomy_genai_jobs_shed_total`; edge-triggered "AI queue backed up" with
   hysteresis. Was: (`lib.rs:208`,
   drained one multi-second vision call at a time in `genai.rs`). A busy
   camera + slow model = alerts minutes-to-hours late, RAM growth, zero
   signal. Fix: bounded channel + depth gauge in `/api/metrics` + "AI queue
   backed up" notification. (Long-known open item — now measured as P0.)
3. ~~**Alarm-dispatch drops are log-only**~~ **DONE `7c04282`** — counted,
   persisted as a lifetime total in KV, and one in-app notification per outage
   ("N alerts could not be delivered") plus a recovery notice;
   `zoomy_alarm_queue_depth` + `zoomy_alarm_drops_total`. Was:
   (`notify.rs:129-137`): queue full at
   512 → the alert is discarded at `warn!`. Fix: persisted drop counter + one
   in-app notification ("N alerts could not be delivered").
4. ~~**WebPush kills itself permanently on a VAPID error**~~ **DONE
   `8a37755`** — retried with bounded backoff; per-user EMAIL (delivered from
   the same worker, which the audit did not note) keeps flowing while push is
   down; edge-triggered `push_unavailable` + recovery. Was: (`push.rs:38-43`):
   the worker returns; subscribe/test UI keeps "working"; no push ever
   arrives. Fix: retry loop + a `push_unavailable` notification.
5. ~~**The GLOBAL webhook drops failures at `debug!`**~~ **DONE `50d6d1b`** —
   all THREE senders (detections, analytics/residential, hand signals) now share
   `notify::post_global_webhook` behind one `degraded::Latch`, so one dead
   endpoint = one notification. Both edges seen live. Was: (`pipeline.rs:2234`) —
   the per-rule path was fixed (`b6b42ee`) but the every-event
   `Settings.webhook_url` (the main HA integration) was missed. Fix:
   edge-triggered unreachable/recovered notification like `genai::err_transition`.
6. ~~**VLM-gated rules fail open silently**~~ **DONE `50d6d1b`** —
   `vlm_confirm` returns reachability (it used to collapse "unparseable answer"
   and "no model" into the same `None`, so the signal did not exist); the worker
   runs the same `err_transition` as captions; `notify::fire_unverified` appends
   "sent WITHOUT the AI check" to the alert. Live: an alert arrived stamped. Was:
   (`genai.rs:443`): the
   endpoint-unreachable notification only covers Caption jobs, so an owner
   using only `vlm_prompt` rules never learns their Ollama is down — every
   "AI-verified" rule fires unverified. Fix: run `err_transition` on the
   VlmGate path; stamp fired notifications "verification unavailable".
7. ~~**19 worker threads, zero liveness monitoring**~~ **DONE `4d6e06b`** —
   `health::WorkerBoard` shared between the health worker (which supervises it,
   inheriting its warmup AND its `while !shutdown` loop head, so a clean teardown
   can't emit 18 notifications) and the teardown join, which no longer discards
   panic payloads. `zoomy_worker_alive{worker=…}` is rendered by the axum
   runtime, so it stays observable when the dead worker is `health` itself.
   Forced two other fixes: the desktop app's `wait_for_health` treated a 503 as
   "not up" (a ~20 s blank launch on every degraded start), and `transcribe` /
   `audio` `return`ed when ffmpeg was missing — which would have read as a
   crashed worker on an ordinary no-ffmpeg install. Both now park and retry.
   Plus 20 `.lock().expect("… poisoned")` sites hardened to
   `unwrap_or_else(PoisonError::into_inner)` (db.rs's WRITER deliberately left
   alone). Was: (`lib.rs:168-360`; joins
   discard panic payloads at `lib.rs:463-478`; real panic sites exist —
   `status.rs:79`, `go2rtc.rs:230`). One poisoned lock silently ends
   detection/recording for the process lifetime. Fix: supervisor tick over
   `handle.is_finished()` → `worker_died` notification; catch_unwind+restart
   where cheap.
8. ~~**`/api/capabilities` proves file presence, not loadability**~~ **DONE
   `24478f8`** — new `models.rs` really opens each file (ONNX session build /
   JSON parse / ggml magic / readable text), cached on path+size+mtime+
   accelerator, in `spawn_blocking`. `{present, loadable, error, probed_ms}` per
   feature; the Models card gained a red "file is damaged" state and every web
   gate moved from `.present` to `capabilityUsable()`. Also fixed: `audio` and
   `lpr` checked for a sidecar they never NAMED. Live: a truncated yolov8n
   reported `loadable:false` with the protobuf error. Was:
   (`api.rs:343`): a truncated download reads `present:true` (and
   `models_dl.rs` job state is process-memory only, so a crash mid-download +
   B-item 1 = a green tick over a dead model). Fix: cached one-shot session
   build per model → `{present, loadable, error}`; persist download jobs +
   verify size before "installed".
9. ~~**MQTT state is asserted, never observed, never surfaced**~~ **DONE
   `3310177`** — `Packet::ConnAck` drives `connected` (distinguishing a refusal
   from an unreachable host) and `Packet::PubAck` stamps `last_publish_ts`; both
   were being discarded by the event loop's `Ok(_)` arm. `degraded::Latch` makes
   an outage one notification, not one per 2 s reconnect. Plus `POST
   /api/mqtt/test` (P1.4) which waits for the broker to ACK. Was: (`mqtt.rs:245`
   logs "connected" before any CONNACK; the `alive` flag is private). Fix:
   `MqttState {connected, last_error, last_publish_ts}` in AppState + a live
   badge beside the MQTT settings + a "Send a test" probe (see P1.4).

## P1 — control-UX gaps the docs/10 sweep missed (high)

> **STATUS 2026-08-07: ALL TEN SHIPPED** (`e15e6cd`, `fed2a39`, `dc98671`,
> `fcb2daa`, `33992e1`, + P1.4 with P0.9 in `3310177`), each live-validated.
> Three things found while doing them, worth carrying:
> - `/api/models/installed` never excluded the POSE model, so the new global
>   picker would have offered a 56-channel model as the object detector. Fixed
>   by excluding the CONFIGURED pose/face paths, not filenames.
> - `matchPreview` deliberately ignored min-score; once P1.3 added the slider a
>   preview that ignored it would have contradicted the control beside it.
> - P1.9 is NOT a migration. The settings lists match SUBSTRINGS, which the
>   library cannot express, so moving partials into it would silently stop them
>   matching. The library is now primary; the lists are relabelled for what only
>   they can do.

1. ~~**`alert_labels` is still raw comma-text**~~ **DONE `e15e6cd`** (`Settings.tsx:3095`) — the
   founding docs/10 sin, on the card where `ObjectPicker` already lives. A
   typo empties the Alerts review tab forever. → `LabelChips`.
2. ~~**Alarm actions have no per-action Test**~~ **DONE `e15e6cd`** (`Alarms.tsx:1507`) — the
   rule-level Test only exists after save; `TestSendButton` + `notifyTest`
   already exist. → mount per action row.
3. ~~**`min_score` is hard-coded to 0 in the builder**~~ **DONE `e15e6cd`** (+ the preview now applies it) (`Alarms.tsx:583`) — a
   first-class AlarmRule field with no control anywhere. → `InheritSlider`
   ("how confident before this rule fires").
4. ~~**MQTT broker URL has no Test**~~ **DONE `3310177`** (with P0.9) — a Test
   button on the System health pane, publishing a retained message and waiting
   for the broker's acknowledgement. Was: (`Settings.tsx:4177`) while every sibling
   channel has one. → `POST /api/mqtt/test` (publish a retained test message)
   + button; pairs with P0.9's state badge.
5. ~~**Detection sub-stream is a raw RTSP box in the TuneModal**~~ **DONE
   `fcb2daa`** — brand buttons (host/credentials lifted from the main source) +
   a real `POST /api/stream_probe` that registers a throwaway go2rtc stream,
   pulls one frame and removes it, reporting the SIZE so a 4K "sub"-stream is
   caught too. Note the audit's "the add-wizard has both" was half true: the
   wizard's only probe is an ONVIF profile enumeration that never fetches a
   frame. Was:
   (`Cameras.tsx:613`) — no brand template, no probe, though the add-wizard
   has both. A wrong URL silently kills detection. → reuse templates + "Test
   this stream".
6. ~~**Zone-state CLIP prompts have no Test**~~ **DONE `dc98671`** — scores both
   prompts against the CURRENT frame and shows both cosines, the margin needed,
   the crop size and the crop itself. **This produced the first real data for the
   STATE_MARGIN deferral: on this install the two prompts separate by
   0.001-0.011 against cosines of ~0.21-0.27, i.e. the 0.01 margin sits at the
   noise floor** — the prompts must be far more different from each other than
   "open gate"/"closed gate". Tuning stays the owner's call; the instrument
   exists. Was: (`ZoneEditor.tsx:817`) — the other
   two AI free-text prompts both got one. → "Test on the current frame"
   showing both prompts' scores (also unblocks the long-open prompt-tuning
   deferral).
7. ~~**Global detector model is a spell-the-path box**~~ **DONE `fed2a39`** (`Settings.tsx:4368`)
   while the per-camera one became `ModelOverrideField`. → same select.
8. ~~**`pose_model` is a free-text path outside ModelsCard**~~ **DONE `fed2a39`**
   — the field now carries its own live capability badge, which is the part that
   was actually missing (P0.8 already made the card honest). Was:
   (`Settings.tsx:4383`) — the nursery/elder-safety feature silently no-ops
   on a typo. → make pose a ModelsCard row (path field to Advanced; still no
   auto-download — it needs a local export).
9. ~~**Two competing plate stores**~~ **DONE `33992e1`** (see the note above —
   deliberately not a migration). Was: (`Settings.tsx:3217/3228` comma-text
   deny/allowlists vs the Plates library on People). → unify through the
   library.
10. ~~**`go2rtc_api_port` has no UI**~~ **DONE `fed2a39`** (`db.rs:1285`) — if 1984 is taken
    (Frigate co-install), every camera is dead with no knob. → Advanced field
    + surface bind failure via P0.9-style state.

## P2 — medium (trust surfacing + mechanical UX swaps)

> **STATUS 2026-08-13: P2 is COMPLETE.** The 2026-08-07 sweep did the trust
> half + dead ends; the 2026-08-13 sweep (`0b18f0f`, `bd31db8`, `65768fd`,
> `3d62501`) finished the mechanical half: per-camera retention → chips priced
> in THAT camera's measured write rate; absence hours → span chips;
> detect_workers anchored to the install's camera count; gesture hold / segment
> length / aging → chips with the real consequence stated (the aging estimate
> comes from the actual ffmpeg filter, scale='min(1280,iw)':-2); keep-events →
> the readout that defuses the events-outlive-footage confusion; the speech
> model → a tier select pointing at the in-app downloader, gated on LOADABLE,
> with a `degraded::Latch` on the whisper load failure; ObjectPicker/LabelChips
> now mark chips the system can never produce (⚠ + why) and canonicalize typed
> names toward the spelling the detector actually emits (caught a real
> foot-gun: "traffic light" was being stored as traffic_light, which never
> matches); tripwire endpoints are labelled A/B on the canvas; residential
> flags read as sentences; and BOTH wizards offer a no-new-apps channel via a
> new first-class "push" alarm-action kind (the notification row IS the
> delivery; a clicked Test pushes for real and reports the count — live:
> "delivered to 1 device"), with the add wizard auto-probing a templated URL
> before the camera is committed. Also done: the Family zone-name presets
> datalist and the GroupCell "No group" placeholder from P3.
>
> Original 2026-08-07 status: Shipped: go2rtc/HomeKit state + a real `/api/health` (`3310177`,
> `4d6e06b`); retention-encoder failures, face-stage failures, archive stall,
> offsite give-ups, and degraded prompt/VLM rules (`8ee17eb`); the push-test
> success lie (`8ee17eb`); the dead-end fixes on Home / Recordings / Alarms /
> offsite / archive (`27b5bf2`); loiter → DurationPicker + LIVE occupancy count,
> per-camera detection interval → InheritSlider, trigger pre/post-roll →
> DurationPicker (`e85b850`).
>
> **Still open in this section** (all small, all mechanical): absence hours,
> per-camera retention days → pills + span estimate, `detect_workers` context,
> quality-aging toggle + savings estimate, keep-events readout, segment length
> presets, gesture hold-time chips, the speech-model field duplicating the
> ModelsCard download + `TranscriptionReadiness` linking to GitHub instead of
> the in-app downloader, transcription gating on capabilities, ObjectPicker
> validating against the model's class list, the Onboarding/Family ntfy
> hard-coding, probe-before-enable on the brand-template Add (the endpoint now
> EXISTS — `POST /api/stream_probe` — so this is a UI wiring job), and the
> tripwire/residential-flag labelling polish.
>
> New reusable pieces for whoever picks these up: `degraded::Latch`,
> `/api/system` + `web/src/SystemHealth.tsx`, `POST /api/stream_probe`,
> `models::probe`, and the `no_user_facing_string_carries_collapsed_indentation`
> lint (long prose must use `concat!`).

- ~~**go2rtc death has no global signal**~~ **DONE `3310177`** — `Go2RtcState
  {running, restarts, last_error}` (keeping the exit status `try_wait` was
  discarding), in `/api/status`'s sibling `/api/system` and on the System health
  pane as one row. Was: (`go2rtc.rs:228`): N identical tile
  errors for one process fault. → `go2rtc: {running, restarts, last_error}`
  in `/api/status` + one banner.
- ~~**HomeKit reports the setting, not the bridge**~~ **DONE `3310177`** —
  `HomekitStatus {serving, last_error}` published around each generation. Was:
  (`api.rs:7181`): a
  crash-looping bridge still shows a pairing PIN. → worker publishes
  `{serving, last_error}`.
- **Enhanced retention marks failures done** (`record.rs:379`): broken
  encoder = disk keeps filling, DB says reduced. → consecutive-failure
  notification + metric.
- **Face stage errors at `debug!`** (`pipeline.rs:1322`, default-on feature):
  everyone is "unknown" forever with no reason shown. → edge-triggered
  notification + capabilities flag.
- **`prompt_like`/CLIP rules go dead silently** (`pipeline.rs:2488`): rule
  shows enabled while its model is unavailable. → `degraded: true` on
  `GET /api/alarms` + warning chip.
- **Archive-pull has no stalled notification** (`archive_pull.rs:114`) though
  offsite does — the disaster-recovery box silently not mirroring is its
  exact failure mode. → mirror `offsite.rs`'s stale check.
- **Offsite `gaveup` increments never notify** (`offsite.rs:203`): partial
  policy failures never trip the stall check. → notify on gaveup increase.
- **Push test toasts success on total failure** (`Settings.tsx:891`):
  "sent to 0 devices (2 failed)" renders green. → error toast when sent==0.
- ~~**`/api/health` cannot fail**~~ **DONE `4d6e06b`+`3310177`** — 503 when a
  worker has stopped, the DB is unreadable, go2rtc is down, or a CONFIGURED
  broker is disconnected. Body is names+booleans only (the endpoint is
  unauthenticated). DEPLOYMENT.md documents the new semantics. Was:
  (`api.rs:276`): green while everything is
  dead. → aggregate go2rtc + workers + DB; 503 when degraded.
- **Transcription silently produces nothing** (`transcribe.rs:193/213`) when
  the model is missing. → gate the toggle on capabilities / notify.
- DurationPicker/preset swaps: loiter secs + occupancy "off" placeholders
  (`ZoneEditor.tsx:727/740` — occupancy should also show the LIVE count via
  `analyticsOccupancy`), trigger pre/post-roll (`Cameras.tsx:755`), absence
  hours (`Cameras.tsx:1000`), per-camera retention days → pills + span
  estimate (`Cameras.tsx:845`), per-camera poll interval → `InheritSlider`
  (`Cameras.tsx:664`), `detect_workers` → show cores/cameras context
  (`Settings.tsx:3180`), quality-aging → toggle + savings estimate
  (`Settings.tsx:4344`), keep-events → outcome readout (`Settings.tsx:4356`),
  segment length → "clip file length" presets (`Settings.tsx:4335`), gesture
  hold time → chips (`Settings.tsx:3258`).
- **Speech model field duplicates the ModelsCard download** and
  `TranscriptionReadiness` links to GitHub instead of the in-app downloader
  (`Settings.tsx:3451/2721`). → tier select + in-app link.
- Dead-end fixes: offsite/archive error rows get inline Test/Retry
  (`Settings.tsx:1729`), "no zone named X" badge opens that camera's zone
  editor (`Alarms.tsx:812`), disk-filling callout links retention settings
  (`Recordings.tsx:255`), Home tiles link offline-cameras / storage
  (`Home.tsx:415/446`), spoken-phrase rules get "Test this phrase" over
  recent transcripts (`Alarms.tsx:1122`).
- **ObjectPicker/LabelChips accept undetectable objects** (`tuning.tsx:96`):
  "raccon" becomes a chip that can never match. → validate against the
  model's class list, mark unknowns.
- **Onboarding + Family wizard hard-code ntfy** (`Onboarding.tsx:144`,
  `Family.tsx:424`): a no-new-apps homeowner gets no alerts. → channel picker
  (push / email) reusing the provider pattern.
- **Brand-template Add commits an unverified stream** (`Cameras.tsx:1611`).
  → probe before enable, or preview thumbnail.
- Tripwire polish: name presets datalist + A/B endpoints actually labeled on
  the canvas (`ZoneEditor.tsx:885/904`). Residential zone flags get full
  labels, not tooltip-only tokens (`ZoneEditor.tsx:761`).

## P3 — surfacing existing capability + small stuff

> **ALL DONE as of 2026-08-13** (`de0a65c`..`d966a25`): ONVIF inspector UI
> (TuneModal "See what this camera is saying"), SSE consumption in Events
> (poll kept as fallback; Live deliberately untouched — its poll is
> /api/status, which the feed doesn't carry), face/nms_iou surfaced behind
> "Expert model settings (advanced)" (decided: surface, not drop — all three
> are live-wired), `zoomy_dropped_events_total{consumer="sse"|"homekit"}`,
> FOV cones via a per-pin bearing handle, and Insights "too many?"
> corrective-action expanders. **docs/11 is COMPLETE.**

- ~~**A "System health" pane**~~ **DONE `3310177`** — a sixth Settings tab over
  a new `GET /api/system`, leading with one verdict line and ending with the
  per-camera inference-ms / frame-age / accelerator table. Was: over
  `GET /api/metrics` (the richest data in the
  product — per-camera inference ms, frame age, offsite backlog — currently
  curl-only). Natural home for P0.7's worker liveness + P0.9 MQTT + go2rtc
  state.
- **ONVIF inspector UI** (`GET /api/onvif/inspect` is never called): the only
  debug surface for `onvif_events` is invisible.
- **Consume `GET /api/events/stream`** (SSE) in Events/Live instead of
  polling.
- `face_det_model`/`face_rec_model` + `nms_iou` fields: Advanced disclosure
  or drop from the public Settings type.
- SSE/HomeKit broadcast-lag drops → a `dropped_events` gauge.
- Family wizard zone-name presets datalist (`Family.tsx:394`); GroupCell
  "—" placeholder → "No group" (`Cameras.tsx:1146`).
- **FOV cones on Map pins** (docs/10 carry-over): needs per-pin direction —
  smallest viable: drag a second handle per pin for bearing, cone is
  presentational.
- Insights corrective actions (docs/10 half-done): "stop alerting on street
  cars" → link into the zone/label change, not just into Find.

## Known backlog (carried, classified — from the full inventory)

**Owner-action-required** (can't be done from here): HomeKit Apple-side
pairing + firewall; relay pulse on front-door `00000`; Ask endpoint
(tool-calling LLM); two-box archive peer; HACS on real HA; OpenVINO build on
Intel; Authenticode cert + Tauri updater secrets; first `v*` release tag;
`yolov8n-pose.onnx` export; **the retention_gb decision** (largely defused by
the honest retention control, but the choice itself is still the owner's);
elevated service install / boot-survival run; real signed update E2E.

**Needs real data / tuning**: `FEEDBACK_SUPPRESS_COSINE` (same-camera
calibration data), lens-suppressor thresholds (real bug footage), zone-state
`STATE_MARGIN` + prompts (P1.6's test button is the enabler), pet Re-ID
(multi-pet footage; CLIP same-camera floor ~0.90 makes it unreliable anyway),
child-height heuristic fragility.

**Deferred features** (bigger, deliberate): ~~Frigate-style unread review
inbox~~ **DONE `0f5b788`** (2026-08-13 — schema v3 `events.reviewed`,
history backfilled reviewed, To-review mode + auto-mark-on-open + scoped
bounded mark-all + "You're caught up");
suppressed-events bin; cross-camera one-clip-one-notification collapse;
actionable push buttons + animated thumbnails; package-still-present state
machine + audio backchannel; NL-to-rule generation; stitched multi-camera
Moments export; ~~browser upload for import~~ **DONE `a8a4f14`**; ~~dual-stream
toggles on Recordings/Events~~ **DONE `d828c57`**; archive events/snapshots
mirroring; ~~global timed snooze~~ **DONE `f655a2a`** (2026-08-13);
evidence bundle self-carried trust root; residential sub-items (sensitive-zone
offsite exclusion, skeleton pose render, audio ring buffer, burst aggregator);
docs/08 watch bucket (Matter camera, ONVIF Profile G gap-fill, iOS critical
alerts, consumer-cloud offsite, super-resolution).

**Design decisions pending**: Phase 4/5 nav collapse — still gated on Find
actually becoming how footage is reached (nav sits at 13 entries);
mobile-primary Recordings→Alarms swap; label-casing house style.

## Verified closed (do not re-open)

Arbitrary-range export, Events paging, VLM test button, camera-tagged system
notifications, poll unify, digest raw labels, event-aware time-lapse, signed
evidence bundle + `--verify`, FloorPlan id-keyed draggable pins, hwaccel
probe, webhook preview, SSO presets + header echo, one-knob retention +
folder probe, model downloads, ground-calib units, zone-kind relabel, Events
zone-filter promotion, audio slider labels, MQTT-prefix Advanced fold,
empty-day day-carry, package drop-spot drawing, import Browse, per-camera
model picker. Also: pool2 needs **no** power-cycle (schedule, not fault —
2026-08-05 correction).

## Sequencing recommendation

1. ~~**P0.1–P0.6**~~ **DONE 2026-08-07** — the alert-losing cluster. New shared
   infrastructure to reuse: **`crates/core/src/degraded.rs`** — a `Latch`
   (`report(&db, err, &Messages{kind, down_title, down_body, up_title, up_body})`)
   plus a pure `transition(ok, notified)`. Every further "surface a silent
   failure" row should go through it rather than re-deriving the latch.
2. **P0.7–P0.9 + the P2 trust-surfacing rows** as one "watchdog" feature:
   worker liveness + go2rtc + MQTT + HomeKit state feeding a System health
   pane (P3.1) and a real `/api/health`.
3. ~~**P1.1–P1.5**~~ / ~~**P1.6–P1.10**~~ **ALL DONE 2026-08-07.**
4. **P2 UX swaps + dead-end fixes** — the remaining work. New reusable pieces to
   build them on: `degraded::Latch` for every "surface a silent failure" row,
   `/api/system` + `web/src/SystemHealth.tsx` for anything the owner should see
   about the app itself, `POST /api/stream_probe` for URL checks, and
   `models::probe` for "is this file real".
5. P3 + deferred features as appetite allows.
