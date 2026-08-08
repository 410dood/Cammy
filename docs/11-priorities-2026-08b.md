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

> **STATUS 2026-08-07: P0.1–P0.6 are SHIPPED** (`77dc3c6`, `7c04282`, `8a37755`,
> `50d6d1b`, `a435fa1`), each live-validated on :8081 with owner state
> snapshotted and restored. P0.7–P0.9 are the Phase 2 watchdog cluster.
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
7. **19 worker threads, zero liveness monitoring** (`lib.rs:168-360`; joins
   discard panic payloads at `lib.rs:463-478`; real panic sites exist —
   `status.rs:79`, `go2rtc.rs:230`). One poisoned lock silently ends
   detection/recording for the process lifetime. Fix: supervisor tick over
   `handle.is_finished()` → `worker_died` notification; catch_unwind+restart
   where cheap.
8. **`/api/capabilities` proves file presence, not loadability**
   (`api.rs:343`): a truncated download reads `present:true` (and
   `models_dl.rs` job state is process-memory only, so a crash mid-download +
   B-item 1 = a green tick over a dead model). Fix: cached one-shot session
   build per model → `{present, loadable, error}`; persist download jobs +
   verify size before "installed".
9. **MQTT state is asserted, never observed, never surfaced** (`mqtt.rs:245`
   logs "connected" before any CONNACK; the `alive` flag is private). Fix:
   `MqttState {connected, last_error, last_publish_ts}` in AppState + a live
   badge beside the MQTT settings + a "Send a test" probe (see P1.4).

## P1 — control-UX gaps the docs/10 sweep missed (high)

1. **`alert_labels` is still raw comma-text** (`Settings.tsx:3095`) — the
   founding docs/10 sin, on the card where `ObjectPicker` already lives. A
   typo empties the Alerts review tab forever. → `LabelChips`.
2. **Alarm actions have no per-action Test** (`Alarms.tsx:1507`) — the
   rule-level Test only exists after save; `TestSendButton` + `notifyTest`
   already exist. → mount per action row.
3. **`min_score` is hard-coded to 0 in the builder** (`Alarms.tsx:583`) — a
   first-class AlarmRule field with no control anywhere. → `InheritSlider`
   ("how confident before this rule fires").
4. **MQTT broker URL has no Test** (`Settings.tsx:4177`) while every sibling
   channel has one. → `POST /api/mqtt/test` (publish a retained test message)
   + button; pairs with P0.9's state badge.
5. **Detection sub-stream is a raw RTSP box in the TuneModal**
   (`Cameras.tsx:613`) — no brand template, no probe, though the add-wizard
   has both. A wrong URL silently kills detection. → reuse templates + "Test
   this stream".
6. **Zone-state CLIP prompts have no Test** (`ZoneEditor.tsx:817`) — the other
   two AI free-text prompts both got one. → "Test on the current frame"
   showing both prompts' scores (also unblocks the long-open prompt-tuning
   deferral).
7. **Global detector model is a spell-the-path box** (`Settings.tsx:4368`)
   while the per-camera one became `ModelOverrideField`. → same select.
8. **`pose_model` is a free-text path outside ModelsCard**
   (`Settings.tsx:4383`) — the nursery/elder-safety feature silently no-ops
   on a typo. → make pose a ModelsCard row (path field to Advanced; still no
   auto-download — it needs a local export).
9. **Two competing plate stores** (`Settings.tsx:3217/3228` comma-text
   deny/allowlists vs the Plates library on People). → unify through the
   library.
10. **`go2rtc_api_port` has no UI** (`db.rs:1285`) — if 1984 is taken
    (Frigate co-install), every camera is dead with no knob. → Advanced field
    + surface bind failure via P0.9-style state.

## P2 — medium (trust surfacing + mechanical UX swaps)

- **go2rtc death has no global signal** (`go2rtc.rs:228`): N identical tile
  errors for one process fault. → `go2rtc: {running, restarts, last_error}`
  in `/api/status` + one banner.
- **HomeKit reports the setting, not the bridge** (`api.rs:7181`): a
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
- **`/api/health` cannot fail** (`api.rs:276`): green while everything is
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

- **A "System health" pane** over `GET /api/metrics` (the richest data in the
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

**Deferred features** (bigger, deliberate): Frigate-style unread review inbox;
suppressed-events bin; cross-camera one-clip-one-notification collapse;
actionable push buttons + animated thumbnails; package-still-present state
machine + audio backchannel; NL-to-rule generation; stitched multi-camera
Moments export; browser upload for import; dual-stream toggles on
Recordings/Events; archive events/snapshots mirroring; global timed snooze;
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
3. **P1.1–P1.5** — the mechanical control-UX fixes (chips, Tests, min_score).
4. **P1.6–P1.10 + P2 UX swaps** in sweeps, live-validated as before.
5. P3 + deferred features as appetite allows.
