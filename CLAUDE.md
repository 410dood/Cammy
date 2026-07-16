# CLAUDE.md — Cammy

Guidance for Claude Code (and any AI agent) working in this repository. Read this
first. For the full background, read `docs/01-research-and-architecture.md`.

## What we are building

A **self-hosted, cross-platform home surveillance / NVR platform** — Blue Iris-class
features, but not locked to Windows, with Frigate-class AI object detection that runs
natively on **Windows and macOS** (and Linux). Target users self-host on a home
machine or NAS.

The differentiator: Blue Iris is Windows-only; Frigate needs Linux/Docker plus
Coral/Nvidia. We combine **Moonfire-class efficient recording** with **portable
GPU-accelerated AI** so the same model runs on Apple Silicon and any DirectX 12 GPU.

## Current status: v0.4 — two-round autonomous improvement sweep (audit → ship → verify), 2026-07-09

### Latest: roadmap-finish Wave 3 — forensic/web (P2.18 + P3.1 + P3.8), 2026-07-16

Third wave — hotspots, journey fusion, detection-triggered recording. Three
validated commits (`4fbc3f3`, `4351a2f`, `3af989f`+grace fix), the two web-only
ones live in Chrome, P3.8 release-rebuilt. clippy clean, **201 core tests**, web
green.

- **P2.18 camera hotspots** (`4fbc3f3`, web-only): clickable adjacent-camera chips
  on the expanded live view, adjacency DERIVED from the FloorPlan pins already in
  Settings.floorplan (nearest 3, Euclidean in the 0..1 plane), reusing .fp-* CSS +
  the #/live/<id> route. Neighbors filtered to the RBAC-scoped camera list (no dead
  links); renders null with no pins/plan. `.hotspot-layer` pointer-events:none at
  z-2 (below PTZ/playback, above privacy) so it never steals frame clicks.
- **P3.1 journey fusion capstone v0** (`4351a2f`, web-only): the Events "Similar"
  modal gains a Grid|Journey toggle — Journey re-sorts the SAME already-fetched
  cross-camera appearance matches chronologically into a numbered narrative + a new
  JourneyMap.tsx (SVG path over the floorplan pins) + the unmodified CrossTimeline
  scoped to the involved cameras, with a persistent "appearance-similarity, not
  identity" disclaimer. LIVE-validated in Chrome (event 8455 → 25 steps, scoped
  timeline, click-to-open). RBAC inherited from event_similar. DEFERRED v1: the
  optional stitched multi-camera "Moments" export (new ffmpeg filter_complex).
- **P3.8 detection-triggered recording** (`3af989f`): a per-camera "record only
  around detections" mode. No packet ring buffer exists, so recording stays
  CONTINUOUS (the pre-roll footage is real) and the mode is a tighter/asymmetric
  retention layer beside event-only — a segment survives only if it overlaps some
  event's [ts-pre, ts+post] window. db.eventless_segments widened to margin_before/
  after (event-only passes (span,span) → byte-for-byte unchanged); settle grace =
  max(pre,post)+30s so a still-writing / within-post-roll / reachable-by-a-later-
  detection's-pre-roll segment is never eligible; flagged/bookmarked footage always
  kept (tested); fail-SAFE (DB error → keep footage). Cameras 3-way arm-bar mode
  selector. No new endpoint (rides PATCH /api/cameras).

**⚠ LIVE-SYSTEM NOTE (owner action):** during the P3.8 release restart, **camera 6
(pool2, 192.168.1.139) stopped recording and would not re-establish** — its go2rtc
PRODUCER stays healthy and the detection pipeline gets fresh frames, but the
recorder's RTSP CONSUMER on the pool2 restream keeps flapping (attaches, drops
within seconds). NOT caused by this wave: the P3.8 record.rs diff is purely
additive (a retention-DELETE loop gated on `trigger_recording`, which NO camera
uses) and never touches recording-start/reconcile — verified by diff; and the
sibling pool3 records fine. Multiple full restarts + a go2rtc-only restart + 90s
waits did NOT restore it, so it's an environmental pool2 camera/restream issue
(likely an RTSP concurrent-session / codec-timestamp quirk on that specific cam).
The recorder reconcile loop keeps retrying, so it may recover once the camera/
network stabilizes; otherwise **power-cycle the pool2 camera / check its RTSP
feed**. Other 4 enabled cams (front-door, ptz-cam, side, pool3) record normally.

### Latest: roadmap-finish Wave 2 — CLIP/pipeline (P2.5 + P2.8b + P2.16 + P3.5·2), 2026-07-16

Second wave of the roadmap-finish run — the CLIP/embedding + pipeline cluster,
serialized on `main` (all four hook the crop-embedding pass). Five validated
commits (`49b1039`, `72f23c1`, `972351b`, `34d5cc2`, `314e3aa`), then batched:
release rebuilt + NVR restarted (5/5 cams back), cross-feature self-review
(no HIGH/MED integration bugs — verified the shared CLIP embedder isn't
double-init'd, the P2.8b suppression keys off the crop label not the rule label,
`row_to_event` columns align, zone_open events get no spurious track_id), and
API+Chrome live-validated on :8080. clippy -D warnings clean, **199 core tests**,
web green.

- **P2.5 CLIP attribute facets** (`49b1039`): a curated 26-facet catalog
  (attributes.rs: vehicle colour/type, person/clothing colour; make/model out) —
  a near-verbatim generalization of the shipped P2.2 prompt path. New
  `AlarmRule.attr_like` (catalog KEY, rides schedule_json, no migration) firing
  through the same is_prompt_rule/effective_prompt/crop-cosine machinery; `GET
  /api/attributes` + `GET /api/search/by-attr` (RBAC like event_similar). Events
  "Attributes" filter chips + an Alarms attr_like select. LIVE: catalog (26
  facets, CLIP available), by-attr `veh_color_red` returned 5 ranked results;
  chips render (Red/White/…/Sedan/SUV/Pickup).
- **P2.8b per-camera feedback learning** (`72f23c1`, honest v0): "Not this" stores
  an alert's crop embedding (new `alert_feedback` table, self-trimmed 200/(cam,
  label)); future CLIP-similar alerts on that camera+label are quieted via
  `smart::any_similar` (0.90 cosine, FAIL-OPEN everywhere). v0 gates ONLY the two
  paths where the crop embedding already exists at fire time — fire_prompt_alarms
  (prompt/attr) + genai::vlm_gate — NOT plain label-match rules (documented in UI
  + code; the hoist is deferred v1). `POST /api/events/{id}/feedback` (RBAC like
  bookmark_event; honest `{ok:false,reason:"no_crop"}`). LIVE: honest no_crop.
- **P2.16 object lifecycle detail view** (`972351b`, + docs/08 P1.5): persist
  `events.track_id`/`path_json` on tracker-driven narrative events; `GET
  /api/events/{id}/lifecycle` aggregates the same-track story, GAP-BOUNDED (≤600s
  clusters) so a per-camera track id that reset to 1 after a restart can't merge
  two objects (pure `cluster_bounds`, unit-tested). Events "Track" step-list modal
  (click-to-seek, reuses the covering-recording resolution). LIVE: honest
  `{available:false}` for non-track events.
- **P3.5 Part 2 — CLIP zone-state classifier v0** (`34d5cc2`): watch a named zone,
  classify open/closed from two CLIP text prompts (garage/gate/pool cover),
  reusing the SHARED CLIP session (never a 2nd), 15s cadence + 2-reading debounce,
  emitting `zone_open`/`zone_closed` (→ alarms via `zone_like`). New pure
  zonestate.rs (bbox + margin classify, 6 tests); fail-open + zero cost when
  unused. ZoneEditor experimental toggle + prompt inputs. `314e3aa` fix (self-
  review LOW): prune zone-state on classify-off/rename so re-enabling doesn't fire
  a stale transition. NEEDS OWNER LIVE CHECK: CLIP models present here, but needs
  a real garage/gate zone + prompt tuning (STATE_MARGIN=0.01 is a v0 guess).

**Deferred to later waves:** P3.5 Part 1 (OpenVINO EP → Wave 5). Next: W3 forensic/
web (P3.1 journey fusion, P2.18 hotspots, P3.8 detection-triggered recording).

### Latest: roadmap-finish Wave 1 — notify/arm spine (P2.10 + P2.11 + P2.9), 2026-07-16

First wave of the autonomous "finish the docs/08 roadmap" run (orchestrator +
per-feature design→impl→3-lens adversarial review→fix→live-validate→commit;
recon maps for all 18 remaining items in `docs/roadmap-recon-2026-07-16.json` +
plan in `docs/agent-task-phase2-3-remainder.md`). Three notify/arm-adjacent
features shipped one validated commit each on `main` (`9008d55`, `f905e63`,
`3f76c04`, `bbc3046`), then batched: release rebuilt + NVR restarted (~30s
downtime, 5/5 cams back online+recording) and **every feature live-validated in
Chrome + curl on :8080**. clippy -D warnings clean, **188 core tests**, web
tsc+build green.

- **P2.10 presence/geofence arm** (`9008d55`): new `occupants` table +
  `POST /api/arm {occupant,home}` first-in/last-out (any occupant home ⇒ "home",
  last leaves ⇒ "away"; presence never disarms) via a shared `apply_arm_mode`
  helper refactored out of the manual PUT; `GET/DELETE /api/presence`; Settings →
  Modes card with the webhook curl example. LIVE: occupant home→"home",
  last-out→"away", empty occupant→400, restored + cleaned up.
- **P2.11 per-user notification matrix** (`f905e63`): `notify_prefs(user_id,
  rule_id[0=default],channel,enabled)` opt-out model + `users.email` +
  `push_subscriptions.user_id` + `notifications.rule_id/camera_id/severity`;
  `fire()` writes one alarm-notification row (tagged) — the async `push.rs`
  worker is the single per-user PUSH+EMAIL delivery point (off the hot thread),
  gated by severity(notify_min_severity) + pref + `user_can_see_camera` (#66
  RBAC). Endpoints GET/PUT `/api/users/{id}/notify-prefs`, `POST /api/me/email`.
  **A 3-lens review found + fixed real leaks**: email restricted to alarm-tagged
  rows (no system-event storm); a one-time-guarded migration wipes stale
  unowned push subs + the web re-stamps them (closed a camera-scope PII leak to
  legacy anonymous subs); the severity gate now actually quiets per-user
  push/email; the alarm Test button no longer bell/push/emails everyone;
  camera_id resolved from the camera name (no get_event fail-open); email
  validation rejects comma/whitespace (SMTP-relay-abuse). Follow-up fix
  `bbc3046`: system pushes aren't muted by the per-user Default toggle. LIVE:
  notify-prefs round-trip, email set/appears, comma→400, matrix UI (grid +
  Default row + SMTP caveat). KNOWN v0 LIMIT: system-notification *push* stays
  camera-global (pre-existing all→all; source camera_id tagging is a follow-up).
- **P2.9 deterrence actions** (`3f76c04`, relay-only v0, DEFENSIVE): new
  `deterrence.rs` — ONVIF DeviceIO relay probe/`SetRelayOutputState`/retrying
  release (reuses the ptz.rs SOAP client, XML-escaped, panic-free, 9 unit
  tests), `Action.kind="deterrence"` fired from `fire_action` (db threaded, NO
  AlarmEvent change) gated by `Settings.deterrence_enabled` (default OFF);
  `GET/POST /api/cameras/{id}/deter` probe + manual Test, RBAC like PTZ; Alarms
  builder offers REAL probed relay tokens (or honest "no relay/no creds"
  callout, never a blind box). A 2-lens security review fixed SOAP-token
  injection, `validate_alarm_rule` rejecting the kind (+ restore re-validation),
  the ON call blocking the detection thread, and a stuck-siren OFF-retry gap.
  **LIVE: the DeviceIO probe found a REAL relay on the front-door cam
  (`token 00000, Bistable`) and honestly reported "no relay outputs" on the
  other 4** — the Alarms builder surfaced `00000 (Bistable)` + a Test button +
  the master-off warning. Deliberately did NOT fire the physical relay (live
  front-door cam, unknown wiring — owner's to verify).
  **DEFERRED (honest cut):** WAV-over-two-way-audio-backchannel (no server-side
  audio-push path exists). **NEEDS OWNER HW CHECK:** actually pulsing a wired
  siren/strobe on cam 3's relay `00000`.

**Next waves (docs/agent-task-phase2-3-remainder.md):** W2 CLIP/pipeline (P2.5
attr facets, P2.8b feedback learning, P2.16 lifecycle, P3.5 zone-state); W3
forensic/web (P3.1 journey fusion, P2.18 hotspots, P3.8 detection-triggered
rec); W4 recording/perf core (P3.7 dual-stream, P3.6 worker pool, P2.14
selective offsite); W5 ecosystem (P3.3 HA, P3.2 ask-your-cameras, P3.4 HomeKit
v0, P3.9 archive, P3.10 import). GOTCHA confirmed: release rebuild needs the NVR
stopped (exe file-locked) — pre-warm `cargo build --release --lib` while it runs,
then stop→link (~9s)→`Start-Process` detached from repo root cuts downtime to ~30s.

### Latest: UniFi-benchmark round 2 — the four structural patterns, 2026-07-15

Same session, second commit: the research's remaining "top transferable
patterns" shipped web-only (plus one tiny cross-page stash), tsc+vite green,
each LIVE-validated in Chrome on :8080:

- **Unified live↔playback player** (CameraDetail): clicking the camera's
  timeline now swaps the covering recording IN PLACE of the live stream — no
  modal. A "Recorded · h:mm:ss" pill ticks with playback (onTimeUpdate →
  whole-second state), a red `.tl-mark` playhead tracks it on the timeline
  (new Timeline `markTs` prop), segments **auto-advance** on end and return
  to live once caught up; "Back to live" button; Esc steps back one level
  (playback → live → close); a LIVE pill + PTZ/talk render only when live.
- **Activity count chart** (`ActivityStrip` in CrossTimeline.tsx): per-interval
  detection counts as slim accent bars over the same time axis — embedded as
  an "Activity" lane atop the cross-camera timeline, standalone above the
  single-camera Recordings timeline and the camera-detail timeline. Aim
  before scrubbing (Protect 6.0's object-count charts).
- **Frame-seeded search** ("Find in frame" in the camera-detail header):
  drag a box on the current frame → canvas-crop at native res (same-origin,
  untainted) → `POST /api/search/by-image` (existing CLIP endpoint) → ranked
  matches grid → click deep-links `#/events/<id>`. LIVE: boxed the blue Tesla
  → 91/89/88% matches of the same car across history. Because a match can be
  older than Events' loaded 200, the full event rides a
  `sessionStorage("cammy-focus-event")` stash that Events' focus effect
  falls back to (validated on a 7/4 event: viewer opened, honest
  "Snapshot only" state since retention had pruned that footage).
- **TuneModal → 3 task-scoped tabs** (Detection / Zones & privacy / Stream &
  recording) using the sanctioned `.arm-bar` segmented control; the five
  `details.adv` sections became plain `<section class=tune-sec>` headings
  under their tabs. Panels unmount on switch — safe because every field's
  value lives in lifted `dc`/`subSource` state (validated: min-score edit
  survives a tab round-trip; ZoneEditor mounts fresh per visit).
- Polish: event-viewer `← →` kbd hint (hidden on touch); ZoneEditor hides its
  frame `<img>` when failed (the plain-language fallback already explains).

Round 5 (same day) — **event deep links now work from cold loads**: new
`GET /api/events/{id}` (RBAC per-camera scoped like the list; 404 clean;
clippy + 168 tests green, release rebuilt + NVR restarted via detached
Start-Process, ~90s downtime) + `api.event(id)`; the Events focus effect
falls back list → sessionStorage stash → fetch → honest toast. TWO real bugs
found while validating: (1) pre-existing — `focusEvent` seeded `null` instead
of `parseHash().eventId`, so `#/events/<id>` NEVER opened on a fresh page
load (only via in-app hashchange; App.tsx one-liner); (2) self-inflicted —
consuming `focusEventId` up front re-runs the effect, so cleanup-based fetch
cancellation discarded the very response being awaited (now a ref token, no
cleanup cancel). LIVE: brand-new tab at `#/events/6079` (7/3, pruned
footage) → viewer opens with "Snapshot only". GOTCHA: the PWA service worker
serves a stale app shell for one load after a dist rebuild — validate on the
SECOND reload.

Round 4 (same day) — **adversarial self-review of the session diff** (3
parallel lens agents: React correctness / UX edge cases / perf+CSS; every
finding hand-verified). 2 HIGH-MED + several LOW confirmed, all fixed +
live-validated:

- **Stacked-modal Escape (HIGH)**: every `Modal` listened for Esc on `window`
  (stopPropagation can't silence sibling listeners there), so Esc with the
  clip player over the event viewer closed BOTH, and Esc in Find-in-frame
  tore down the whole camera view. Fix: a module-level modal stack in ui.tsx
  (push on mount; only the topmost closes on Esc) + a `findOpenRef` guard in
  CameraDetail's own Esc handler. LIVE: Esc peels 2→1→0.
- **Stacked-player shortcuts (MED)**: the playback-shortcut effect grabbed
  the FIRST `.modal-bg video` (the hidden inline one) — now targets the last
  (topmost); the inline viewer clip also pauses when the full player opens.
- **Find-in-frame offline dead-end (MED)**: frame.jpg failure now shows "No
  live picture — the camera must be online…" and disables the search (the
  same onError treatment Heatmap/ZoneEditor got; this modal was missed).
- **Honesty**: AI-gated rules' table figure is now "N candidates for the AI
  check" (matchPreview counts pre-CLIP candidates, not fires); counts fetch
  1000 events and the label switches to "since h:mm" if capped; a pasted
  deep link to an out-of-window event toasts instead of silently no-opping.
- **Perf**: hover-thumb requests debounced 150ms per segment (uncached
  keyframes cost an ffmpeg run behind the shared 3-permit semaphore the
  Scrub grid also uses); Timeline's track nodes memoized so pointer-move
  re-renders skip reconciling ~400 elements; Alarms counts/preview memoized
  off unrelated keystrokes; zero-width guards on timeline math.

Round 3 (same day, third commit) — the last three research findings:

- **Timeline hover-scrub previews**: pointer over any `Timeline` (camera
  detail, Recordings single-camera, event-viewer mini-timeline) floats a
  `.tl-bubble` above the position with the covering segment's keyframe
  (`/api/recordings/{id}/thumb.jpg`, P2.4 cache — cheap) + clock; clock-only
  over gaps. Component root became `.tl-wrap` (bubble escapes the old
  overflow:hidden).
- **Alarms 24h match counts**: the rules table's Last-fired cell adds "N
  matching events · 24h" per rule, computed client-side from one 24h events
  fetch (limit 500) through `matchPreview` — a faithful re-implementation of
  `AlarmRule::matches`'s event-shaped conditions (exact label, substring
  face/plate/zone/transcript, "?" stranger sentinel, exact gesture);
  schedules/min-score/cooldowns/AI gates deliberately excluded and the
  tooltip says so. LIVE: any-object AI-watch rules honestly show all 89
  candidates.
- **"Would have matched" builder preview** (Protect 6's historical trigger
  previews): as rule conditions change, a `.rule-preview` strip shows the
  live count + up to 6 snapshot thumbnails of last-24h events the rule would
  have fired on, "(before the AI check runs)" when vlm/prompt set — catches
  over-broad or dead rules before saving. LIVE: object=person → 10 events
  w/ thumbnails.

### Earlier: UniFi-benchmark de-clunk pass (grid→viewer inversion), 2026-07-15

A research-driven UX pass against UniFi Protect 5/6 (web-research agent report:
IA, timeline mechanics, event review, user complaints → 12 transferable
patterns) plus a live Chrome audit of every surface. **Web-only, 7 files, no
API/backend change**, tsc+vite green, every surface live-validated in Chrome on
:8080 (desktop + 390px mobile):

- **Events — the action model was inverted and is now Protect-shaped.** Cards
  dropped their 9 always-visible buttons (~1,800 buttons on a 200-event page)
  for a clean thumbnail + meta + hover/saved quick-save star; the detail
  lightbox became a real **event viewer**: probes `recordingAt` and plays the
  covering clip inline (snapshot fallback with an honest "no recording covers
  this moment" line + disabled recording actions), prev/next chevrons and
  ←/→ keys walk the filtered list ("N of M"), and an embedded **mini-timeline**
  (1h window around the event, coverage + class-colored ticks) scrubs to any
  retained moment via `recordingAt` — deliberately unbounded, vs Protect's
  ±5-min cap (their #1 player complaint). Keyboard guard: a focused video/
  timeline keeps its own arrows (`defaultPrevented` + tag check). LIVE: clip
  autoplay, arrow nav (1→2 of 200), timeline-click segment swap all verified.
- **Recordings — playback-first IA.** The big storage card moved off the top
  into a bottom `details.adv` disclosure ("Storage · 21 GB · 673 GB free · ~30
  days until full"); a warn/danger capacity callout still renders loud at the
  top (severity never hides). Timeline legibility: `.xtl-cov` opacity .34→.62,
  `.tl-block` .5→.72, row-hover highlights the lane + name.
- **Live — one toolbar, whole-tile click.** Header = h1 + Activity-first pill +
  playback select + Wall; groups and saved views merged into ONE chip row
  (`.chip-sep` divider, "Views none saved" label deleted). The whole tile now
  opens the camera (Protect's click-to-expand); the corner expand button stays
  as the keyboard/SR path; PTZ pad stops propagation (verified: pad click
  doesn't navigate).
- **Polish**: Heatmap hides its backdrop `frame.jpg` on error (no more broken-
  image glyph; retries per camera/range); Timeline's right edge label only says
  "now" when it IS now (day-scrub/event windows show the real clock); Home's
  digest text card moved below the Spotlights/last-seen columns (recap, not
  headline).

`f3ff9d3` on `main`: a three-agent copy audit (monitor / config / people
surfaces) found ~80 places the UI spoke developer at homeowners — raw model
filenames and `zoomy --verify` CLI commands in toasts, meaning hidden in
hover-only `title=` tooltips, bare jargon labels (cooldown, min confidence,
force CPU, enroll, armed, transport), dead-end errors. All rewritten in place,
**display strings only, zero behavior change** (option `value=`s, handlers,
state untouched). Terminology now consistent: "clips" not "segments", "saved"
not "bookmarked/enrolled", "recording history" not "retention" (technical term
kept as a parenthetical for the tinkerer persona). Every Settings detection
knob gained a visible one-line helper with a good-start value. tsc+vite green;
live-validated in Chrome on :8080 (Live playback dropdown, Settings helpers,
Events "Important only" filter).

### Earlier: overnight backlog sweep — P2.4 thumbnail scrub + P2.3 region motion search + self-review hardening, 2026-07-10

Autonomous overnight session (user asleep), commits `160a0a2`..`fa2d117` on `main`:

- **P2.4 thumbnail scrub**: `GET /api/recordings/{id}/thumb.jpg` (ffmpeg
  keyframe, cached under `data/thumbs`, RBAC'd, immutable headers, bounded
  cache) + a Recordings "Scrub" toggle rendering the window as a quarter-hour
  keyframe grid with ×N expanders. LIVE-validated (real IR keyframe, 25-tile
  grid in Chrome).
- **P2.3 retroactive region motion search**: `motion::packed_mask` (64×64
  changed-cell bitset, unit-tested) OR'd per camera-minute by the pipeline into
  a new `motion_grid` table (512 B/minute, WITHOUT ROWID, 45-day prune);
  `GET /api/motion/search` rasterizes a 0..1 rect, bit-tests stored minutes,
  folds consecutive-minute ranges, resolves each to its covering segment with
  a recording_at-style duration guard; Recordings "Motion search" modal
  (drag-a-box over the live frame → hit tiles → click to play). Read path
  LIVE-validated via a synthetic quadrant row (hit + miss + correct
  segment/offset); write path confirmed by a Monitor watch on the live DB.
- **Self-review hardening (`fa2d117`)**: an 8-angle adversarial review of the
  diff found 6 CONFIRMED issues, all fixed: trailing-minute flush gap (lone
  night motion unsearchable until the next motion), recording-gap
  misresolution (wrong footage playable), a prune that effectively never
  fired, `db.get_segment` full-table scan (now WHERE id=?), uncapped
  concurrent ffmpeg thumb extraction (now semaphore 3), sync scan in the
  async handler + 300-query N+1 (now spawn_blocking + one fetch + binary
  search), thumb cache key id-reuse hazard (now id+start_ts), per-miss
  directory sweeps (now ~1/32). Keep running the self-review pattern — it
  catches real bugs every time.
- Polish: prettyGesture in the Settings duress dropdown (deferred item cleared).

**Backlog next (docs/08)**: P2.5 CLIP attribute facets, P2.16 object lifecycle
view, P2.9 deterrence actions, P2.14 selective offsite. GOTCHA: to restart the
NVR from a session, use a detached process (PowerShell `Start-Process`), NOT a
timed background shell; the data-dir lock will correctly refuse a double-start.

### Earlier: Windows seamless-experience sweep (docs/agent-task-windows-seamless.md), 2026-07-09

All six deliverables of the seamless spec shipped on `main`, one commit each:

- **Data-dir exclusivity lock**: `zoomy::run` takes an advisory OS lock on
  `<data_dir>/.cammy.lock` (fs2, in-tree) — two engines on one data dir can no
  longer double-run go2rtc/recorder and corrupt recordings. Fails fast with a
  user-facing message; the desktop app surfaces engine-startup errors in its
  window via a self-contained `data:` URL error page (early-exit from the
  health wait). LIVE: second instance on the same dir exits 1 with the message.
- **Windows service** (`crates/core/src/winsvc.rs`, windows-service 0.8):
  `zoomy --install-service/--uninstall-service` + hidden `--run-service` SCM
  entry; LocalSystem, auto-start, restart-on-crash 3×/day, absolute paths +
  install-time workdir captured (services start in System32), logs to
  `<data_dir>/service.log`. Coexistence = mutually exclusive per data dir (the
  lock). main.rs is now sync; `server_config()` shared. NOT yet exercised: a
  real elevated install/boot (needs UAC).
- **Single-instance + autostart** (desktop): tauri-plugin-single-instance
  (second launch focuses the window), tauri-plugin-autostart with a tray
  check-item "Start Cammy when I sign in", enabled BY DEFAULT on first packaged
  run (marker `.autostart-default-applied`), plus a Settings → License-tab
  "Desktop app" card that toggles it over remote-origin Tauri IPC
  (`withGlobalTauri` + `capabilities/default.json` remote urls; hidden in plain
  browsers via `window.__TAURI__` detection).
- **Auto-update**: tauri-plugin-updater (rustls); endpoint = GitHub Releases
  `latest.json`, pubkey pinned in tauri.conf.json (private key at
  `~/.tauri/cammy_updater.key`, NEVER committed). Launch check + tray "Check
  for updates" → explicit "Install update vX" click (never interrupts recording
  unasked) → clean restart. `.github/workflows/release.yml` (tag `v*`):
  fetches go2rtc/ffmpeg/all models, tauri-action builds NSIS + updater
  artifacts + latest.json into a draft release; missing signing secrets degrade
  gracefully (ephemeral updater key + unsigned Authenticode).
- **Code signing**: `bundle.windows.signCommand` → `crates/desktop/sign.ps1`,
  a no-op without `CAMMY_SIGN_THUMBPRINT`/`CAMMY_SIGN_COMMAND` (owner supplies
  the cert; documented in DEPLOYMENT.md §6).
- **Seamless touches**: tray tooltip live status ("N cameras online · M
  recording" off `/api/status` each minute); DEPLOYMENT.md §2b Windows-service
  install steps + LAN firewall one-liner.

**Owner inputs still needed**: repo secrets `TAURI_SIGNING_PRIVATE_KEY(_PASSWORD)`
(from `~/.tauri/cammy_updater.key`) and an Authenticode cert
(`CAMMY_SIGN_THUMBPRINT` or `CAMMY_SIGN_COMMAND`); one elevated
`--install-service` run to confirm boot-survival; first `v*` tag to exercise
release.yml end-to-end.

### Earlier: deferred-feature follow-ups — event-aware time-lapse + signed evidence bundle, 2026-07-09

After the backlog below was exhausted, two deferred items shipped one at a time,
each live-validated on the running release NVR (:8080) and pushed to `main`:

- **Event-aware variable-speed time-lapse** (`b0c7b80`, upgrades P2.12): the day
  time-lapse now slows near events and zips through quiet stretches. Each event is
  mapped to its position in the back-to-back concat stream (`segment_index*60 +
  offset`), given a ±3s window (merged, cap 80), and ffmpeg is driven with a
  `select=…gte(t-prev_selected_t, if(inside-window, dense, sparse))` filter re-timed
  to a constant 24fps; `fast_stride` is solved so the clip lands near the requested
  length while reserving ≥20% of the frame budget for quiet parts. Falls back to
  uniform `setpts` on an event-free day. LIVE: 0-event → uniform (side, 2.35 MB); an
  event inside a retained segment → event-aware ("1 event windows", 15.7 MB, valid).

- **Signed, self-verifying evidence bundle + `zoomy --verify` CLI** (`cd0321f`,
  completes the P2.13 deferral): new `GET /api/events/{id}/evidence.zip` packages the
  watermarked clip with a `manifest.json` (SHA-256 pin + provenance), `manifest.sig`
  (Ed25519 over the exact manifest bytes), `PUBLIC_KEY.txt`, and `VERIFY.txt`.
  Per-install ed25519 seed under `data/keys` (0600). **No new deps** — hand-rolled
  uncompressed (STORED) zip + in-tree `ring`. `zoomy --verify <bundle.zip>` re-checks
  the signature + re-hashes the clip fully offline. New `crates/core/src/evidence.rs`
  (unit-tested: round-trip, CRC-32 vector, tamper). LIVE: Python's `zipfile` opens the
  bundle *and* our reader opens Python's repacked zip (bidirectional compat); good →
  VERIFIED, tampered clip → hash mismatch, tampered manifest → signature fails.
  **Bonus latent-bug fix:** `extract_event_clip`'s temp path ended in `.partial-N`, so
  ffmpeg couldn't infer the muxer and every clip extraction 500'd once a covering
  segment existed (unnoticed because happy-path events usually 404) — now the temp name
  keeps `.mp4` last, restoring `/clip`, share-serve, and both evidence exports. 167
  core tests (4 new).

### Earlier: two-round improvement sweep on main — 23 commits, 2026-07-09

A long autonomous session driven by two adversarially-verified audit workflows
(each fanning out read-only lens auditors → per-finding verify → ranked backlog),
then shipping the backlog one validated commit at a time. **23 commits on `main`**,
every one `cargo clippy -D warnings` clean + **163 core tests** + web `tsc`/`vite`
green, and **live-validated in headless Chrome / curl against the running release
NVR** (:8080, 7 real cameras) — Rust changes clippy+tested to `target/debug` first
(no server stop), then a release rebuild + restart to go live. A self-review
workflow over the session diff caught + fixed one real toast-timer bug.

**Round 1 (docs-audit backlog + verification):**
- Commercialization (`site/`, README): honest trial-first buy flow (the loud "Buy
  $79" CTA silently fell back to #download when `CAMMY_CHECKOUT_URL` is empty — now
  trial is the primary CTA, Buy routes to pricing until checkout is configured),
  a fears-answering FAQ + source-available trust band, an honest **competitor
  comparison table**, `$29/yr`-clarity, README v0.4.
- Correctness (`api.rs`/`db.rs`/`pipeline.rs`): **clip-cache poisoning** (ffmpeg
  wrote the final path with `-y`, so a failed run served a corrupt file forever →
  extract-to-temp + atomic rename), CSV export honoring its `tag` filter,
  self-password-change session invalidation, notifications hard-cap, pipeline
  settings reuse, `events(camera_id,label,ts)` index.
- UX: empty-state honesty (Recordings false "no recordings" flash, Events first-run
  self-check), **toast a11y** (assertive errors + pause-on-hover), title/nav
  consistency, copy-token buttons, a11y+touch sweep, mobile save-bar.
- Perf: visibility-aware polling, cameras-fetch-once.
- Features: **Spotlights** (Home feed ranked by importance×recency), **Insights**
  analytics dashboard over a NEW efficient `/api/analytics/timeseries` (SQL GROUP
  BY, no raw-event transfer).

**Round 2 (deeper audit — under-covered surfaces + security):**
- **Security / RBAC least-privilege (live-validated via a role-scoped token over
  the LAN IP, since loopback=admin):** camera RTSP/ONVIF credentials were readable
  by any Viewer via `GET /api/cameras` → `redact_url_creds` strips `user:pass@` for
  sub-Admins (+ a write-back guard so a re-submitted masked source can't wipe
  creds); Operators could raise `auth_proxy_default_role` to admin or repoint the
  offsite-backup destination → `put_settings` now 403s a sub-Admin delta to any
  `auth_proxy_*`/`offsite_*` field; login username-enumeration timing side channel
  closed with a dummy argon2 verify.
- Perf/correctness: bundle **code-split** (React.lazy + vendor chunk: 430 KB
  monolith → 122 KB entry + cached 142 KB react), faces loaded once/tick (not
  once/frame), bounded + `spawn_blocking`-offloaded smart-search, ONVIF worker
  hardening (lock-poison tolerance, map pruning, per-camera subscribe backoff),
  visibility-paused polls, Map live-status polling, Family mode honesty (gate "On"
  on pose-model presence; broaden activity-feed labels), Signals double-start
  guard, alarm no-condition warning, tuning-modal discard guard + busy state,
  ZoneEditor hides residential toggles on `ignore` zones.
- **Marquee features (each with adversarial or manual security review):**
  - **P2.7 shareable expiring clip links** — `clip_shares` table, `POST
    /api/events/{id}/share` mints a 256-bit `zoomy_share_<hex>` (hash-only stored,
    returned once), PUBLIC auth-exempt `GET /share/{token}` validates hash+expiry+
    !revoked then serves the clip via the shared `extract_event_clip` helper,
    rate-limited + audited, Events "Share" + Settings revoke UI. Manually
    security-reviewed (no bypass/traversal/IDOR; expiry+revoke enforced).
  - **Alarm rule Edit** — `db.update_alarm` + `PUT /api/alarms/{id}` +
    pre-fill-the-builder UI (was delete-and-recreate only).
  - **P2.12 day time-lapse** — background-job `POST …/timelapse?date=` (concat +
    setpts), cached under `clips_dir`, `GET …/timelapse.mp4`, Recordings button +
    poll (built a 315-segment day in ~7s → 12.9 MB mp4).
  - **P2.13 evidence export** — `GET /api/events/{id}/evidence.mp4` re-encodes with
    a drawtext provenance watermark (Cammy · camera · local time · event id;
    cross-platform fontfile with filtergraph path-escaping) + SHA-256 anchored in
    the append-only audit log + `X-Cammy-SHA256`. Watermark render confirmed on an
    extracted frame.

**Documented follow-ups (deliberately deferred):** event-aware variable-speed
time-lapse (v1 is uniform); a full evidence zip+manifest + Ed25519 signature +
`zoomy verify` CLI (v1 anchors the hash in the audit log); the ZoneEditor's full
visual card-restructure (only the semantic ignore-zone fix shipped); Home
`/api/overview` rewiring (superseded — Spotlights needs the events list);
prettyGesture in config dropdowns. **GOTCHA:** the test env's events and retained
segments are temporally misaligned (retention keeps ~3 days; events go back
further), so clip/share/evidence happy-paths often 404 on old events — this is
correct behavior, and the extract path is code-identical to the shipped `/clip`.

### Earlier: commercial launch readiness sweep on main, 2026-07-08

A "tighten up everything for a paid launch" pass driven by three parallel
audit agents (licensing security/correctness, web-UX first-impression, docs/
onboarding). Version bumped **0.3.2 → 0.4.0** (core + desktop + tauri). All
`cargo clippy -D warnings` + **206 tests** + web `tsc`/`vite` green; every fix
**live-E2E'd in Chrome against the rebuilt release NVR** (:8080, 7 real cameras).
Commits `b77633f` (hardening), `abf54f0` (README), `40685c7` (nits+shots) on
`main`.

- **Licensing HIGH — trial-tamper fail-OPEN fixed** (`licensing.rs`): a
  hand-edited trial stamp made `read_trial_start` return `None`, which
  `trial_status` mapped to `now()` → a *perpetual* 30-days-left trial (the exact
  opposite of the documented fail-safe, and cheaper than the acknowledged
  delete-the-DB reset). Now `TrialStamp::{Valid,Absent,Tampered}`; only `Absent`
  starts a fresh clock, `Tampered` → `Expired`. Test asserts `Expired` through
  `status()` (rewound-ts **and** trashed-tag). Also dropped the deprecated
  `ring::constant_time::verify_slices_are_equal` for `ring::hmac::verify`.
- **Fulfilment server hardening** (`scripts/fulfilment_server.py`): lock the
  ledger read-modify-write (ThreadingHTTPServer races could clobber/double-issue
  a paid order), fail loud on a corrupt existing ledger instead of overwriting
  history, anchor state paths to the script dir (not cwd), cap the pre-auth
  webhook body (1 MB), and fulfil **`order_updated`** so delayed-payment orders
  (created `pending` → later `paid`) aren't dropped. `ls_setup.py` subscribes
  both events. utf-8 writes + ASCII logs (ran clean on a cp1252 console;
  `--selftest` green).
- **Web UX**: Events **Clip** button now fetches the blob and toasts "No
  recording covers this event" on a 404 instead of navigating the tab to raw
  JSON (confirmed live: the 404 is `application/json`); **License pane** renders
  a skeleton/`ErrorState` (never blank — it's the tab you go to to *pay*);
  trial-dismiss persists per-day; **People** page warns when the face model is
  absent; friendly "Can't reach the Cammy server" banner copy; new **About &
  help** card (version from `/api/config` + site/docs/support links); CSV export
  honors the tag filter; **Map** page title matches the nav; assistive-asterisk
  footnote on the Alarms object dropdown. Killed user-visible `zoomy` remnants
  (backup filename, webhook/recordings placeholders, MQTT hint) and internal
  refs (`#63`, `docs/03`, "Faces page"→"People page").
- **Marketing site** (`site/`): **$79** one-time + **30-day free trial** +
  **Lemon Squeezy** checkout + the **never-brick promise**; refreshed 9-tile
  feature grid; recaptured hero/events screenshots from the Cammy-branded build
  (the old ones showed the pre-rebrand "Zoomy" title bar); buy button falls back
  to `#download` until `CAMMY_CHECKOUT_URL` is set.
- **Docs/infra**: `buy_url()` default → live `https://410dood.github.io/Cammy/`
  (`cammy.app` is unregistered — was a dead "give us money" button); added
  **LICENSE-MIT + LICENSE-APACHE** (README/Cargo claimed a dual license with no
  files); `docker-compose.yml` publishes **loopback-only** (was a spoofable-XFF
  hole with the `--trusted-proxy` default); **README rewritten customer-first**
  (download-installer lead, real ~15-surface feature set, `#optional-ai-models`
  table linked from the in-app Models card, `:18080` desktop note). `gitignore
  /bin/`. Rebuilt **`target/release/zoomy.exe`** at 0.4.0; desktop NSIS installer
  rebuilt.
- **Deliberately NOT changed**: the internal `zoomy_`/`mqtt_prefix`/Prometheus
  identifiers (HA discovery unique-ids are hardcoded `zoomy_` independent of the
  prefix; renaming only the default adds inconsistency and would orphan an
  existing HA setup — a coordinated rename is a separate, later effort). The
  post-trial `allows_config()` enforcement seam stays unwired by design (docs/09
  "never brick a camera system").

### Earlier: v0.3 — full competitor suite (#1–#70) integrated on main + cross-feature simplify, 2026-06-22

### Latest: docs/08 Phase 1 COMPLETE — merged to main + live-tested, 2026-07-02

The whole Phase 1 roadmap shipped across three slices on `feat/parity-phase1`
(merged `6894fda`, 25 files / +1454): the batch-1 items below **plus** P1.8
absence/inactivity watch (`absence.rs` worker + `DetectConfig.absence_hours`,
edge-latched, assistive-framed), P1.9 alarm **Test button** (`POST
/api/alarms/{id}/test`, no event/cooldown stamp, gate-bypassing) + "Last
fired" column (`GET /api/alarms/stats` off the in-memory throttle), P1.13
digest **key moments** w/ clip links (pure `key_moments`, severity≥3 or
anomaly≥0.6), P1.14 **footage-access audit** rows on clip/CSV access, P1.15
**event tags** (`events.tags` JSON, sanitized ≤8×24, case-insensitive `tag=`
filter, #chips + edit UI), P1.16 **Wall tour** (4-cam pages, 10/30/60s) +
**Recordings day picker** (`/api/recordings?before=` + `nowTs` anchor on both
timelines), and the P1.10 residual: gesture/audio/soft-trigger snapshots now
**burn privacy masks** (shared `apply_privacy_masks`, fail-closed) — detection
snapshots were already masked (#29). Also fixed: soft-trigger route used axum
0.7 `:id` (startup panic on 0.8) → `{id}`. 150 core tests, clippy/tsc/vite
green. **Live-E2E'd on the running NVR (rebuilt release, :8080, 7 cameras):**
severity on events (legacy re-derived), tags round-trip + filter, soft trigger
→ flagged event w/ snapshot, alarm Test → real ntfy push, clip-access audit
row, absence config round-trip, recordings `before`, health/UI 200. Not yet
observed live (need real traffic): burst "+N" text, caption-in-push
(needs Ollama), `last_detection_ts` (stamps on next detection).

### Earlier: parity Phase 1 batch 1 (branch `feat/parity-phase1`), 2026-07-02

First implementation slice of the docs/08 roadmap, stacked on main after the
docs/08 study (below). Shipped, each `clippy -D warnings` + tests + web
`tsc`/`vite` green (148 core tests, 11 new): **P1.1 severity tiers** (pure
`severity.rs` 1–4 mapping; `events.severity` column, legacy rows re-derived on
read; Events high/critical badges; severity→ntfy-priority default; one-knob
`Settings.notify_min_severity` gate on ntfy/email only — webhooks/MQTT/duress
never gated); **P1.2 burst consolidation** (AlarmThrottle counts cooldown-
swallowed matches → "(+N more while muted by cooldown)" in the next push);
**P1.3 caption-in-push** (per-rule `describe` flag rides schedule_json → fire
deferred through the GenAI worker which captions first; `{{caption}}`/
`{{severity}}` template vars; fails open); **P1.6 activity-first Live sort**
(StatusBoard `last_detection_ts` + opt-in persisted toggle); **P1.7 soft
triggers** (`POST /api/cameras/{id}/trigger` → bookmarked labeled event w/
live snapshot + alarm dispatch; "Log event" button on camera detail); **P1.12**
Tailscale/cloudflared zero-port-forward section in DEPLOYMENT.md.
**Discovered already shipped by a prior session** (docs/08 marks them): photo-
upload search (`/api/search/by-image` + Events UI), the VLM alert gate
(`vlm_prompt`), GenAI failure notifications, DEPLOYMENT.md/Dockerfile/compose,
model-presence Settings card. **Next up (docs/08 P1):** P1.8 absence detection,
P1.9 alarm-rule Test button + last-fired/24h stats, P1.10 privacy masks burned
into outbound notification snapshots, P1.13 digest-with-clips push, P1.14
footage-access audit, P1.15 event tags, P1.16 showreel/calendar. NOT yet
live-E2E'd against the running NVR — build/unit validated only.

### Earlier: 2026-07 competitor parity study → docs/08, 2026-07-02

A 4-agent research pass over a 30-product field (UniFi Protect deep-dive as the
"balance" benchmark; enterprise Nx Witness/Eagle Eye/Qognify/Axis ACS/Hanwha
WAVE; prosumer Synology/ZoneMinder 1.38/Shinobi/Bluecherry/Agent DVR/HA + Blue
Iris 6/Frigate 0.17/Scrypted refresh; consumer Lorex/Amcrest/Blink/Tapo-Aireal +
Ring/Nest-Gemini/Arlo 6/Eufy S4/Wyze/Reolink-ReoNeura), every candidate checked
against shipped #1–70/R01–24. Output: **`docs/08-competitor-parity-2026-07.md`**
— a unified ranked roadmap (absorbs the docs/06 backlog) in 3 phases: P1
notification-quality + curation quick wins (severity-tiered push, burst
consolidation, caption-in-push, photo-upload search, detection sessions +
motion-path thumbnails, soft triggers, absence detection, deployment/remote
docs), P2 forensic + ecosystem (ONVIF camera-side analytics ingestion — the
field's most-repeated gap, prompt-based NL alert rules, retro region motion
search, thumbnail scrub, attribute facets, Spotlights-style feed, deterrence
actions), P3 strategic (journey fusion, ask-your-cameras, HA/HomeKit, worker
pool). Key theses: Cammy exceeds the field on capability, loses on curation;
Blue Iris 6 shipped built-in DirectML YOLO; Eufy S4 Max + Reolink AI Box now
sell "local AI, no subscription" as appliances. Includes an explicit
anti-feature list (the UniFi leanness lesson).

### Earlier: cross-page UX pass, 2026-07-01

A live E2E tour of every page (Chrome vs the running :8080 backend, 7 cameras)
plus a **13-target grounded multi-agent UX audit** (adversarially verified → ranked)
drove a **web-only** improvement pass across 12 files, all reusing existing
design-system primitives (`callout`/`EmptyState`/`badge`/`details.adv`/`TogglePill`)
— no backend/API change. `tsc`+`vite` green, **each surface live-validated in
Chrome**. Committed on branch `ux/detection-tuning-modal-redesign` (`7e5ccbf`,
stacked on the tuning-modal redesign `f82ef6a`). The audit found three systemic
gaps; the fixes:

- **Severity is now encoded** (action-required status escalates out of muted
  grey): Recordings' near-full-disk/retention-pruning capacity line → a
  `callout-warn`/`-danger` + "Filling up"/"Nearly full" Storage badge naming the
  limiter + a fix action ("~0 days" copy → "under a day"); Home's Free-space stat
  card takes a warn/danger tone (+ "~N days until full") from `days_until_full`;
  Settings' passwordless remote access shows a `callout-warn`.
- **Empty/idle states unified**: Map's broken bespoke drop-box → centered
  `EmptyState`; Signals' black video void → in-box idle placeholder (+ armed tags
  → `badge ok`); People's unknown-face wall capped at 12 + "Show all N" + real
  vehicle-crop thumbnails + `EmptyState` for the empty People list.
- **Config pages lead with the list, creation forms collapse**: Cameras' "Add a
  camera" and Alarms' "New rule" builder both fold behind `details.adv`/a toggle
  (auto-open on first run); the Registered/Rules list leads once populated, with a
  count + "New rule" button and a footer-row Create.
- **Settings ~20-card scroll wall → 4 tabbed groups** (Detection & AI / Modes &
  alerts / Access & security / Recording & backup) — one `<form>` preserved
  (sticky save bar + dirty guard intact), cards **hidden not unmounted** (imperative
  `SettingsTabs` keyed off each card's `<h2>`), so the 9 stateful cards keep
  in-flight edits across tabs.
- **Polish**: Events' 14-control filter strip → primary row + "More filters"
  `details.adv` (force-open when a hidden filter is active); Home digest → bullet
  list + height-capped recent-activity feed + `.home-cols`/`.live-grid`
  auto-fill→auto-fit (kills lone-tile voids); Family per-mode "On/Partly set
  up/Not set up" badge from `detect_config` + bottom-aligned disclaimers; camera
  detail rail reuses Events' `groupEvents` (×N badge) + a Download-clip button.

### Earlier: Detection-tuning modal UX redesign, 2026-07-01

The per-camera **"Detection tuning"** modal (`web/src/pages/Cameras.tsx`
`TuneModal`) was a single flat `flex-wrap` `.row` cramming ~20 heterogeneous
controls (thresholds, ~10 feature checkboxes, stream/perf knobs, recording,
retention, schedule) with meaning hidden in `title=` tooltips, five different
words for "inherit/default", and Save buried past the ZoneEditor canvas. Driven
by an 8-lens multi-agent UX audit (adversarially verified → ranked plan), it was
**rebuilt web-only** — no backend/API change, `null`-on-clear inherit semantics
preserved verbatim — and **live-validated in Chrome against the running backend
(:8080, 7 real cameras)**:

- **Sectioned** into collapsible `details.adv` groups (reusing the existing
  recipe): *Detection sensitivity* (open by default) · *Detection features
  (N on)* · *Stream & performance* · *Recording & retention* · *Residential
  safety*, then the Zones/ZoneEditor. Each summary shows a live "(N on)" count.
- **Wider card** (`Modal className="modal-wide"` → `max-width: min(820px,…)`) with
  a **sticky header + sticky footer** (`.tune-foot`) so Save is always reachable;
  fields laid on an aligned **CSS grid** (`.tune-grid` / `.feat-grid`) that
  collapses to 1–2 columns on mobile.
- **Boolean capabilities → `TogglePill`** (accessible `<button aria-pressed>`)
  in a switch bank, each with a **visible one-line helper** promoted out of the
  `title=` tooltip (the real a11y fix); day-toggles became `TogglePill`s too.
- **Unified empty-state copy**: "Inherit global" (real fallback) vs "Off (no
  limit)" (disable fields), plus live **"using global: X"** hints (fetched
  `api.settings()`) on min-score / motion / interval / retention.
- **Liability caveats surfaced**: the residential disclaimer + the "pose model
  not downloaded" note are now always-visible `.callout callout-warn`/`-info`
  blocks (role="status"), not hover-only. Recording schedule shows a **live
  plain-language summary** ("Records Mon, Fri, 08:00–18:00 (overnight)…").
- **ZoneEditor touch fix** (`onPointerDown` + `touchAction:'none'` while drawing)
  so zone/mask drawing works on tablets/phones.

`tsc` + `vite build` green. Files: `web/src/pages/Cameras.tsx`,
`web/src/ZoneEditor.tsx`, `web/src/styles.css`. Committed as `f82ef6a` on
`ux/detection-tuning-modal-redesign`.

### Earlier: stationary-object suppression + motion highlight, 2026-06-30

Fix for "8 near-identical events of a parked car in 10 min, no actual motion":
ambient motion (wind/shadows/auto-exposure) keeps tripping the gate, YOLO re-sees
the still car, and the 10s per-(camera,label) cooldown lets each re-detection
through. **Two features, backend + web, all `cargo clippy -D warnings` +
`cargo test` (core 140 / motion 8 / tracker 14) + web `tsc`/`vite` green:**

1. **Per-camera `DetectConfig.suppress_stationary`** ("suppress stationary
   repeats", off by default). Drives the existing `crates/tracker` for the camera
   and, in `pipeline.rs`, filters `wanted` so a detection only fires when it's a
   **new** object (no confirmed track yet → fail-open, keeps first-arrival
   latency) or its matched confirmed track **moved** ≥ `STATIONARY_MOVE_FRAC`
   (0.05 frame-fraction, ground anchor) since it last alerted. A parked car =
   one stable track that stops moving → suppressed; a real arrival / departure
   still fires, rate-limited by `event_cooldown_secs`. Per-track last-alert
   anchors live in `alerted_tracks` (pruned to live track ids + on camera-delete +
   on disable). **Re-acquisition guard** (adversarial-review catch): the tracker
   keeps a vacated track alive for `max_age`, so a *different* object occupying the
   same spot could inherit the old id + its stale anchor and be wrongly suppressed
   — so a track that missed ≥ `STATIONARY_REACQUIRE_GAP` (2) frames before
   re-matching is treated as new (its pre-update miss count is threaded into the
   confirmed-track snapshot). `moved_enough` + `stationary_keep` are pure +
   unit-tested (incl. the re-acquisition regression); first-settle may emit ≤2
   events before quiescing. `suppress_stationary ⟹ tracker_on`, so a
   `suppress_stationary` camera runs YOLO ~1fps continuously (same cost model as
   any analytics camera) to keep the still object's track alive between gate trips.
2. **Global `Settings.highlight_motion`** (on by default). The motion gate
   (`crates/motion`) now keeps the changed-cell mask and exposes
   `motion_regions()` (4-connectivity connected-components over the 64×64 diff →
   ≤8 largest blob boxes in 0..1 fractions, single-cell noise dropped). The
   pipeline captures those right after `gate.update` and `save_snapshot` burns
   them onto the event JPEG in **amber**, under the **red** detection boxes — so a
   viewer can see *what actually triggered* an event (trees vs. the object). Both
   new tests in the motion crate; toggle in Settings → Detection.

Both are JSON-blob / `detect_json` fields — **no DB migration**. Build-validated
+ unit-tested; not yet live-E2E'd against cameras.

### Earlier: web UX/UI review pass (branch `ux-review-improvements`), 2026-06-29

A multi-agent UX audit (9 lenses → adversarially-verified → ranked plan) drove a
**web-only** improvement pass across 21 files in `web/src` (no backend changes).
Highlights, all `tsc`+`vite` green and **E2E-validated in Chrome against the live
backend** (4 real cameras): a shared accessible **`TogglePill`** primitive
(`ui.tsx`) replacing the keyboard/SR-broken `<span className="pill toggle">`
pattern that armed alarms / toggled cameras / picked safety sounds across 6 files;
**hash-based URL routing** (`#/page`, `#/events/<id>` deep links — refresh keeps
your place, Back/Forward + bookmarks work, single hashchange update path, no
setPage↔hash loop); **3-section nav rail** (Monitor/Detections/Configure; the
Monitor group == the mobile-primary set so the bottom tab bar is unchanged);
progressive-disclosure **`<details className="adv">`** on the dense Alarms
new-rule form (Advanced conditions) and Cameras tuning modal (Residential safety);
**clickable timeline ticks** (snap-to-event in Timeline + CrossTimeline);
**Settings dirty-state** ("Unsaved changes" + `beforeunload` guard); real
**error/loading states** on pages that swallowed fetch failures (Home/Faces/
Family/FloorPlan/Alarms); security caveats promoted to `.callout`s; themed delete
dialogs + confirm-before-delete (Cameras/Alarms); masked camera password;
colorblind-safe timeline/heatmap legends; plus polish (notifications a11y,
command-palette footer, Family page cross-links, privacy tag, Strangers drill-in,
ZoneEditor aria-labels). New shared CSS recipes: `button.pill` reset, `.nav-group`,
`details.adv`, `.evt-legend`, `.cmdk-foot`, `.privacy-tag`.

### Earlier: full-suite integration to main + cross-feature simplify

All outstanding feature branches were merged into `main` (fast-forwarded to the
`integration/merge-all` result, now `0d5b09d` + the simplify commit): **#62 TOTP
2FA** (PR #19), **#63 tamper + #64 gait** (PR #20), **#65 reverse-proxy SSO /
forward auth** (PR #21), **#66 per-camera RBAC scoping** (PR #22), **#67
per-camera recording schedules** (PR #23), **#68 native Web Push** (PR #24),
**#69 package / porch-piracy detection** (PR #25), and **#70 offsite S3 backup**
(PR #26, incl. its review-tightening pass). The 8 branches were integrated in
dependency order on an isolated git worktree (two-factor first, since tamper/gait
+ SSO stacked on it); every text/semantic conflict was hand-resolved — the
pipeline analytics/tracker gate (residential + gait + parcel all keep the tracker
running on motionless frames), the db schema batch + `Settings` struct/`Default`,
the `lib.rs` worker spawns + joins, the `api.rs` metrics call (RBAC-scoped event
count **and** backup gauges), and the Cameras/Settings UI. Validated:
`cargo clippy -D warnings` clean, **135 core tests pass**, web `tsc`+`vite` clean.
(The `zoomy-desktop` Tauri bundle needs the gitignored `clip_text.onnx`; that's an
environment resource, not a code issue.)

Then an **entire-codebase simplify pass** (commit `2c25228`): the merge surfaced
cross-feature duplication, consolidated into a new `crates/core/src/util.rs` —
one efficient `hex` (a single pre-allocated `String`, no per-byte alloc) replacing
**4** copies across auth / sigv4 / ptz / totp, and one `sleep_interruptible`
replacing **5** byte-identical copies across anomaly / digest / offsite / push /
schedule. ~60 fewer lines; behavior unchanged (SigV4 AWS, TOTP RFC, auth vectors
all still pass).

**GOTCHA:** this integration ran in an isolated worktree (`/e/dev/_cammy_integration`,
branch `integration/merge-all`) while a concurrent session held the main checkout
on `residential-analytics` — the worktree kept the two from colliding. Build needs
`LIBCLANG_PATH` set (whisper-rs bindgen) — see the memory note.

### Earlier: residential / consumer-camera analytics suite — batch 1 (PR #27, branch `residential-analytics` off main)

The consumer-camera parallel to the commercial suite — baby / pet / pool / kid /
aging-in-place — researched + ranked in `docs/05-residential-analytics-suite.md`
(12-agent competitor study of Cubo Ai / Nanit / Furbo / Petcube / Nest / Ring /
Nobi / AltumView + adversarial critic). **Thesis: ~70–80% of the field needs no
new ML model** — re-scope the tracker, zones, face rec, CLIP, and the YAMNet
521-class audio engine (which already classifies + fires on baby-cry / bark /
smoke-alarm / glass / scream via `settings.audio_labels` — the default set
already includes them). **Batch 1 shipped** (commit `a38800d`): new
`crates/core/src/residential.rs` `ResidentialState::tick` (driven beside
`AnalyticsState`) emits **zone_enter** (edge-triggered "person in the Pool",
"pet on the Couch" via the `alert_enter` zone flag), **child / child_alone** (a
bbox-height child/adult heuristic gated on per-camera `DetectConfig.child_height_frac`
→ child-in-restricted-zone `child_watch` + unattended-no-adult `supervise`),
**fall** (assistive motionless-in-lower-band, `fall_detect`, dwell-based not
aspect-flip), and **still_water** (EXPERIMENTAL motionless-in-water, zone `water`
flag). New **`AlarmRule.zone_like`** + `zone_ok()` AND-ed at every alarm site
(detection/analytics/audio/gesture/transcript) scopes a rule to a named zone
("person in the Pool zone") — rides `schedule_json`, **no migration**. Residential
events flow through `emit_analytics_event`, so they get snapshot + webhook + ntfy
+ MQTT + Alarm Manager for free. Frontend: `zone_like` field + residential event
labels (Alarms), enter/child*/alone*/water* per-zone toggles (ZoneEditor),
fall-detect + child-calibration (Cameras) — all with **liability tooltips +
asterisks**. **SAFETY framing (in code + UI):** every output is assistive,
best-effort, disclaimed — never "drowning detection", never a medical/SIDS
device; child split is fragile + calibration-gated. 8 unit tests; `cargo test`
85 pass, `clippy -D warnings` clean, web `tsc`+`vite` clean.

**Also shipped this session — batch 2** (`9f85bcf`): auto-arm/disarm **scheduler**
(`crates/core/src/schedule.rs`, `Settings.arm_schedule`, flips the authoritative
`arm_mode` KV on a day+time schedule + notifies; idempotent + once-per-minute
guarded; Settings "Modes schedule" card) · audio **"Family & Safety Sounds"**
(added Cat meow + Child crying to the existing YAMNet chip set — baby-cry/bark/
smoke/glass/doorbell already shipped) · **wildlife** animal COCO labels
(bird/bear/horse/sheep/cow) in the alarm dropdown. Doorbell/visitor + known-vehicle
need no new code (YAMNet Doorbell/Knock + person + `zone_like`; LPR `plate_like`).
**Batch 3** (`47047f0`): **cross-modal confirmation** — `AlarmRule.confirm_label`
+ `confirm_within_secs` (a rule fires only if a companion event of that label hit
the same camera within the window — glass-vs-dishes, fall+thud); `confirm_ok` +
`db.has_recent_event`, AND-ed at every alarm site, **fails open** so it never
suppresses a real alert; Alarms "confirmed by" UI; unit-tested.

**Batch 4 — server-side pose tier SHIPPED** (`d2fbd0c` + `17675f6`; the user chose
the headless server runtime over the browser-only path). New pure **`crates/pose`**
turns 17-keypoint COCO output into a posture (standing/sitting/lying/unknown +
face-visible + confidence; unit-tested). New **`detector::PoseEstimator`** runs a
YOLOv8-pose ONNX model on the same per-OS EP and decodes `[1,56,8400]`
(decode + NMS unit-tested). New **`crates/core/src/posture.rs`** worker (spawned
beside audio, opt-in per camera via `DetectConfig.pose_detect`, gated on
`Settings.pose_model` existing) runs 24/7 headless and emits **fall** (lying low,
held), **standing** (in a zone — crib climb-out) and **covered_face** (body present
+ no face in a zone — rollover/blanket) through the normal Alarm path (zone_ok +
confirm_ok + cooldown). Per-camera "body pose monitoring (assistive*)" toggle +
Settings pose-model path + alarm labels; all disclaimed assistive/not-medical.
**Live E2E needs the (uncommitted) `yolov8n-pose.onnx` model** — build-validated +
decode unit-tested only.

**Batch 6 — Family Safety hub SHIPPED** (`011107b`): a new **"Family"** nav page
(user-friendly capstone) with four plain-language guided **modes** — Baby & nursery,
Pets, Pool & water safety, Aging in place — each a recipe that ties together the
camera toggles / zones / sounds / alarm rules from batches 1-4, with step-by-step
setup, recent matching events, and per-mode safety disclaimers + a top "these are
assistive aids, not safety devices" banner. Pure frontend over existing APIs.

**Batch 7 — no-clip privacy SHIPPED** (`4c069a9`): `DetectConfig.no_clip` — on a
sensitive camera (nursery/bedroom/bathroom) the residential + pose safety events
still fire (alert + label + zone + time) but **write NO snapshot** to disk / MQTT /
webhook / email (both `emit_analytics_event` and the pose worker honor it).
Per-camera toggle. Pairs with privacy masks.

**The one substantial item left: the pet Re-ID vertical** (multi-pet *identity*
enrollment via CLIP Re-ID #61 + per-pet diary) — deferred as its own focused effort
(pet *detection*, off-limits zones, bark, escape and a diary-via-digest already work
with shipped primitives + the Family hub describes them). **Deferred sub-items**
(docs/05): offsite-backup #70 exclusion for sensitive zones (needs the offsite
branch — currently stashed), a full skeleton-only pose render, the audio ring-buffer
for sub-second transients, and the burst/rate aggregator (cooldown-entangled).

**Net this session: the residential suite shipped as 8 commits on `residential-analytics`
(PR #27), all `cargo test` + `clippy -D warnings` + web `tsc`/`vite` green.** New
crates `pose`; new core modules `residential.rs`, `schedule.rs`, `posture.rs`; new
`detector::PoseEstimator`. Live E2E for the pose tier needs `yolov8n-pose.onnx`.

**GOTCHA (this session): an uncommitted offsite #70 WIP was in the working tree at
start** (not ours); preserved as `git stash@{0}` "offsite #70 WIP (pre-existing…)"
so residential could branch cleanly off `main`. **Recover with**
`git checkout offsite-backup && git stash pop`.

### Earlier this session: commercial video-analytics suite (matrix #53–#61) on the object tracker

Capped by **#61 cross-camera appearance search / Re-ID** (PR #18) — "find this
person/vehicle everywhere": each object detection's CROP is CLIP-embedded at
detection time (reusing the existing CLIP session, **no new model**; capped 6
crops/frame so a crowd can't stall the shared detection thread) and stored in
`event_embeddings.crop_embedding`; `GET /api/events/{id}/similar` cosine-ranks
crops across all cameras+time (in `spawn_blocking`); an Events "Similar" button
opens a ranked modal. Validated cross-camera on two cams (0.91–0.97 matches vs
0.11–0.22 distractors). The remaining frontier is the **enterprise-governance
track**: OIDC/SAML SSO+MFA, **per-camera/group RBAC scoping** (today RBAC gates
method+path GLOBALLY — every authenticated user sees every camera), multi-site
federation.

Researched the commercial NVR/VMS field and built the **multi-object tracker**
(`crates/tracker`, SORT-lite: velocity-predicted IoU association, ByteTrack
two-pass, hit/miss hysteresis, persistent IDs + trajectories) — the foundational
gap that unlocks every flagship analytic — then shipped, on top of it, the whole
analytics family, each **live-validated + adversarially reviewed + CI-green +
merged** as its own PR:

- **#53 object tracker** + **#54 line-crossing tripwires** (directed, ByteTrack)
  + **#55 loitering / dwell** (PR #14): `crates/core/src/analytics.rs`
  `AnalyticsState::tick` is the pure per-frame engine over confirmed tracks; the
  pipeline drives it and emits `crossing`/`loiter` events through the existing
  snapshot+webhook+MQTT+alarm path (alarm rules match by `label`).
- **#56 speed estimation** + **#57 wrong-way** (PR #15): `crates/tracker/src/homography.rs`
  (hand-rolled 8×8 Gaussian DLT, `Homography::from_quad` from 4 ground-rectangle
  corners + real W×H; convex-quad + behind-horizon guards). `track_speed_kmh`
  warps the trajectory to the ground plane (millisecond timestamps, displacement-
  based, capped). Per-camera `DetectConfig.ground_calib`; per-tripwire
  `alert_wrong_way`. New `events.speed` column.
- **#58 occupancy + capacity alarm** + **#59 people-counting** (PR #16): `tick`
  also returns per-zone live counts (published to the `StatusBoard`,
  `GET /api/analytics/occupancy`); per-zone `PolyZone.occupancy_max` arms an
  **edge-triggered** `occupancy` event (latch keyed by config-shape fingerprint so
  a zone reorder can't suppress a breach). `db::analytics_counts`
  (`GET /api/analytics/counts`) rolls crossings into in/out/net throughput.
- **#60 activity heatmap** (PR #17): `db::heatmap` accumulates each detection's
  ground-anchor into a grid×grid density map (`GET /api/analytics/heatmap`); a
  `Heatmap.tsx` canvas overlays it on the camera detail frame. **Review caught a
  HIGH bug**: detection boxes were persisted in raw PIXELS while everything else
  used 0..1 fractions — fixed by normalising detection boxes at storage
  (`pipeline.rs` `add_event` now stores `[d.x1/fw, …]`), restoring the documented
  invariant; `db::heatmap` skips out-of-[0,1] legacy rows.

GOTCHAs this session: detection events historically stored **pixel** bboxes while
zones/masks/analytics store **0..1 fractions** — now unified to fractions (legacy
rows are pixel-scale; the heatmap filters them out). The synthetic `sample.mp4`
exec source serves a **frozen frame** to go2rtc (motion gate never trips, so
motion-gated detection *events* don't fire; `analytics_on` cameras still detect on
the still frame, which is why occupancy/tracks validated but raw events didn't) —
seed the events table directly to exercise read-side analytics. Adversarial
review repeatedly caught real bugs unit tests + happy-path live checks missed
(fail-open auth earlier; the pixel-vs-fraction heatmap bug) — keep running it.

### Earlier this session: roadmap feature batch — ALL 16 net-new features from docs/04

Built and validated **all 16 net-new features** proposed in
`docs/04-ux-ui-redesign-and-roadmap.md` (A1-A6, B1-B4, C1-C6). **All Rust is
`cargo check` + `clippy -D warnings` + `cargo test` green (50 tests)**; the web
builds clean; the new backend was run headless and the endpoints + UI were
**live-validated in Chrome** against real data. Shipped on branch
**`ux-redesign-and-roadmap-features`** → **PR #2** (two commits: `985d6ae`
redesign+14 features, `f2b72a2` C5 roles + B4 redaction).

**C5 multi-user roles** (backward compatible: legacy single-password + loopback
stay full-admin, so you can't lock yourself out locally; auth activates when a
password OR any user exists). Roles Viewer<Operator<Admin gate `/api/*` by
method+path. Was **hardened by a 3-lens adversarial security-review workflow that
found 2 real HIGHs** (fail-OPEN auth gate on a DB-count error; self-demotion +
last-admin TOCTOU → remote zero-admin lockout) plus a Viewer-reads-camera-creds
MED (`GET /api/backup`); all fixed (fail-closed, atomic guarded last-admin checks,
self-demote block, backup/restore→Admin, per-user session invalidation, audited
user changes). Live-tested with `--trusted-proxy`+XFF: viewer 200 read / 403
mutate / 403 users / 403 backup; no-auth 401; loopback 200; last-admin + self-
demote 400; Bearer token works but blocked from user/password mgmt. **B4 privacy
redaction**: per-camera privacy masks now render as a BLURRED overlay on live
tiles (Live/CameraDetail/Wall) — frontend-only.

Backend (new, in `crates/core`): a `notifications` table + `digests` table +
`events.anomaly_score` column (idempotent migrations), with bounded self-trim
helpers; `Settings` gained `anomaly_detection`, `digest_enabled`, `liveviews`
(`Vec<Liveview>`), `floorplan` (no migration — JSON blob). Two new opt-in workers
spawned/joined in `lib.rs`: **`digest.rs`** (B1, daily plain-language recap →
digest row + notification) and **`anomaly.rs`** (B3, scores events by how unusual
the camera/label/hour is vs 30-day history, writes `anomaly_score`, notifies on
high score). `health.rs` now also writes an in-app notification on camera
offline/online. New endpoints: `GET /api/overview` (A1 dashboard aggregator),
`GET /api/notifications` + `POST /api/notifications/{id}/read` + `.../read-all`
(A4), `GET /api/digests` + `POST /api/digests/run` (B1).

Frontend (new files under `web/src`): `pages/Home.tsx` (A1 Overview, now the
default page), `Notifications.tsx` (A4 bell + slide-in panel), `CommandPalette.tsx`
(C1, ⌘/Ctrl-K), `Onboarding.tsx` (C3 first-run wizard), `Wall.tsx` (C4 kiosk/wall
+ Wake Lock), `CrossTimeline.tsx` (A2 synchronized multi-camera timeline on
Recordings), `pages/FloorPlan.tsx` (C6 "Map" page, client-resized image + camera
pins via `Settings.floorplan`), `theme.ts` (C2 light theme). Events gained A3
detection grouping (the **Group** toggle) and B2 natural-language search (parses
time/camera/object/identity out of the query); People page (was Faces) gained A5
identity rollups (sightings/last-seen/cameras) + a vehicles section; Live gained A6
**Liveviews** (saved camera layouts) + the **Wall** button. PWA: `public/sw.js`
offline app-shell (bypasses `/api`), registered in `main.tsx`.

**Not done (the 2 invasive ones), deliberately scoped out to protect the no-bug
bar on security-critical code:** C5 multi-user roles (needs auth surgery —
`Sessions: HashSet<String>` → identity map, middleware role gating, login flow)
and B4 redaction (role-gated, depends on C5). The exact path is in
`docs/04` + the backend integration map; do these as a focused, separately-tested
pass. **GOTCHA: building `crates/core` on Windows needs `libclang`** (whisper-rs
bindgen) — none was installed; `pip install --user libclang` puts a usable DLL at
`%APPDATA%\Python\Python311\site-packages\clang\native`, set `LIBCLANG_PATH` to it.

### This session: high-end UX/UI redesign pass (UniFi-Protect-grade)

A design-system overhaul of the web UI, driven by an industry/competitor study and a
six-surface audit captured in **`docs/04-ux-ui-redesign-and-roadmap.md`** (the single
source of truth for the visual direction + a sequenced net-new feature backlog).
Shipped in one pass, no new runtime deps:
- **Design tokens** (`web/src/styles.css` `:root`): a 12-step OKLCH cool-tinted dark
  ramp (hex fallback behind `@supports`), three-tier elevation, semantic
  surface/text/accent/status tokens, a 4px space scale, tightened radii, motion
  tokens. All legacy var names (`--bg --panel --accent` …) kept as aliases so nothing
  broke. Optional `[data-theme="light"]` block. Global recipes: tabular-nums on data,
  one `:focus-visible` ring (fixes a WCAG strip), **`color-scheme: dark` + themed
  native date/time/select** (kills the light-OS-widget tell), `prefers-reduced-motion`.
- **Iconography** (`web/src/icons.tsx`, NEW): ~60 hand-rolled inline-SVG stroke icons
  (Lucide-style, `currentColor`, no dep). **Every emoji-as-icon removed** across all
  pages (nav, Events, Live/PTZ/REC, Settings audit log, Alarms, Cameras, Faces,
  Signals, Recordings, ZoneEditor).
- **Primitives** (`web/src/ui.tsx`, NEW): accessible Toast + promise-based
  Confirm/Prompt Dialog + Modal lightbox, wired via providers in `main.tsx`. Replaced
  all `window.alert/confirm/prompt` and inline "Saved ✓" spans.
- **Typography**: self-hosted **Inter Variable** (`web/public/fonts/`, latin subset,
  `font-display: swap`, system fallback) — local-first, no runtime network.
- Shell polish: darker rail (correct UniFi depth), accent active state via inset
  box-shadow marker (dropped the banned `border-left` side-stripe), sentence-case page
  titles (fixed the brand/`<h1>` collision), sticky Settings save bar, accent (not
  green) filter chips, blinking REC pip, themed event-card chips/actions.

**Live-validated in Chrome via the Vite dev server** (real cameras/events): Live,
Events (incl. the new note Dialog), Settings (incl. the Save toast). **GOTCHA: the
running desktop app serves a frozen `web/dist`** — `npm run build` on disk does NOT
reach it; preview web changes with `npm run dev` (temporarily point the `/api` proxy
at the desktop's :18080, revert to :8080 after). Lenses consulted: ui-ux-pro-max,
impeccable, frontend-design, gpt-taste.

Latest: **security audit log** (matrix #52). A bounded `audit_log` table records
security events — `login_success`/`login_failed` (with client IP via
`client_ip`, correct behind a trusted proxy), `password_set`/`password_cleared`,
`token_created`/`token_revoked` — surfaced newest-first at `GET /api/audit` and
in a Settings "Recent security activity" card. **No secrets logged** (action +
token name only). The table self-trims to the most recent 2000 rows
(`db::add_audit`, best-effort so it never blocks the audited action). The audit
log is **session-only** — `token_forbidden` blocks a Bearer token from reading
it so a leaked token can't recon login IPs / other tokens. Live-validated
(set-password, wrong+correct login, token create → correct action/IP/detail).

### Earlier this session: stranger / unfamiliar-face detection (matrix #51) — the marquee
smart-NVR feature (UniFi "unfamiliar face"). A person whose face is detected but
matches **no enrolled identity** is tagged with the reserved `db::UNKNOWN_FACE`
("?") sentinel on the event, and a new `AlarmRule.face_unknown` condition fires
a webhook/ntfy/MQTT action on it. `pipeline::run_faces` marks an unmatched
confident face on its person box only if not already recognized (a real name
wins) **and only when ≥1 identity is enrolled** (with none, everyone is
"unknown" = noise — a review-driven guard against a first-run flood; crops are
still saved for enrollment). Enroll/rename reject the reserved name. Alarms page
"unknown face (stranger)" condition (exclusive with face-name match, enroll-first
hint); Events shows "🚶 stranger". **Live-validated E2E on a USB webcam: un-enrolled
face → person event face="?" → face_unknown webhook fired**; also validated on
real IP cameras (Dahua + Amcrest over ONVIF→RTSP, DirectML). Review drove the
zero-enrolled guard + reserved-name rejection + face_like exclusivity.

### Earlier this session: event CSV export (matrix #50) `GET /api/events/export.csv` downloads
matching events as RFC 4180 CSV (same filters as the events list, up to a
generous cap, with a `Content-Disposition` attachment). Columns: id, local time,
camera, label, score, face, plate, gesture, zone, flagged, note, caption,
transcript. The renderer guards against spreadsheet **formula injection** (a
field starting with `= + - @` is prefixed with `'`) since transcripts/captions/
notes are partly attacker-influenced; pure `events_to_csv`/`csv_field` are
unit-tested. Events page gains a "⬇ Export CSV" link carrying the active filters.
Live-validated (headers + rows + the flagged filter narrowing the export).

### Earlier this session: Prometheus metrics (matrix #49)

`GET /api/metrics` returns
Prometheus 0.0.4 text exposition — `zoomy_build_info`, `zoomy_cameras`/`_online`,
`zoomy_events`, `zoomy_disk_free_bytes`, plus per-camera gauges
(`zoomy_camera_online`/`_recording`/`_storage_bytes`/`_segments`/`_inference_ms`/
`_last_frame_age_seconds`, labelled by camera). Hand-rendered from the segment
index + status board + event count (no new dep); the pure `render_metrics` is
unit-tested incl. label escaping. Gated by the same `/api` auth, so a scraper
uses an API token (#48) via `Authorization: Bearer` or runs on loopback.
Live-validated (valid exposition, `text/plain; version=0.0.4`, per-camera series,
401 when scraped remotely without auth). Review: 0 defects above a pre-existing
low (`poll_ms*3` overflow, hardened with `saturating_mul` in the new code).

### Earlier this session: API access tokens (matrix #48)

Bearer tokens let scripts /
integrations (Home Assistant, MQTT automations) call the JSON API from another
host without the session cookie. `POST /api/tokens` mints a `zoomy_<64-hex>`
(256-bit) token, returns it **once**, stores only its SHA-256 hash; `GET
/api/tokens` lists metadata; `DELETE /api/tokens/{id}` revokes. `auth::middleware`
accepts `Authorization: Bearer …` (scheme case-insensitive) after the existing
session/loopback checks, stamping last-used ≤once/min. **A Bearer token is denied
token-management + password-change** (`token_forbidden` → 403 on `/api/tokens`
POST·DELETE and `/api/auth/password`; only an interactive session/loopback can),
so a leaked token can't mint siblings or lock the owner out. New
`crates/core/src/db.rs` `api_tokens` table; Settings "API tokens" card.
Live-validated via `--trusted-proxy`+XFF (remote 200 w/ token, 401 without/bad/
revoked, 403 on escalation paths, loopback unaffected). Security review drove the
escalation gate + case-insensitive scheme.

### Earlier this session: event bookmarks (matrix #47)

A per-event `flagged` + free-text
`note` (new columns), `POST /api/events/{id}/bookmark`, and a server-side
`flagged` list filter. A bookmarked event is **exempt from the event-retention
prune** — `db::prune_events_before` keeps flagged rows and their snapshots (the
snapshot-delete query skips files still referenced by any flagged event), so a
saved clip survives past retention. Events page gains a ★/☆ save toggle + 📝
note per card and a "⭐ Saved" filter; un-saving a noted event confirms then
drops the note (no orphaned notes). The endpoint distinguishes absent (preserve)
/ null·"" (clear) / string (set, ≤500) via a custom serde deserializer
(`de_some`) — plain `Option<Option<String>>` can't tell absent from null.
Live-validated via the API; retention-protection + the serde semantics are
unit-tested. Review caught the note-orphan-on-unflag bug + absent-note-wipe.

### Earlier this session: spoken-keyword alarm (matrix #46)

A new Alarm Manager condition
`transcript_like` (case-insensitive substring on an event's speech-to-text
transcript) fires a webhook/ntfy/MQTT action when a phrase is *said* near a
camera — a spoken "safe word" (e.g. "help"/"fire"), the audio sibling of the #35
duress gesture. `AlarmRule::matches` gained a 7th `transcript` arg; the condition
is evaluated **only** in the transcribe worker after whisper writes the transcript
(`transcribe::fire_transcript_alarms`), guarded by a non-empty `transcript_like`
so it never double-fires against the audio-event dispatch (which passes
`transcript: None`). The matched transcript now rides into the webhook JSON /
ntfy push / template `{{transcript}}` placeholder. Alarms page gains a "spoken
phrase" field. Live-validated end-to-end (spoken "americans" → rule fired once →
webhook carried the transcript). Review fixed a redundant clippy attribute,
empty-phrase handling, and `\u`-escaping of control chars in webhook templates.

### Earlier this session: transcript-aware smart search (matrix #45)

The ✨ Events search is now hybrid — CLIP visual similarity **plus** a whole-word
text match on each event's transcript + caption (`smart::text_match_score`), so
you can search what was *said*, and a speech/caption hit outranks a pure-visual
one. It also works **without** the CLIP models now (text-only mode instead of
erroring). `db::search_corpus(with_embeddings)` joins event text + optional
embedding in one query over the full (retention-bounded) history (no recall cap;
embedding column skipped in text-only mode). Live-validated (search "americans" →
the jfk transcript event on top, `match=speech`). Review drove uncapping recall,
the text-only embedding skip, signal-only filtering, and whole-word matching.

### Earlier this session: bundled audio transcription (matrix #44)

Opt-in, off by default,
fully local: **whisper.cpp is compiled into the binary** (whisper-rs) — no
separate server. A YAMNet audio event triggers `crates/core/src/transcribe.rs`
(its own worker; model loaded once), which captures a short clip from the
camera's restream and writes a speech-to-text **transcript** onto the event
(🎙️ on Events cards, searchable). Per-camera via the existing `audio_detect`;
Settings card + model path (`ggml-tiny.en.bin`, downloaded not committed).
**Live-validated end-to-end with the bundled model:** a jfk.wav "intercom"
camera → "Speech" event → transcript "And so my fellow Americans ask not what
you are" on the card. **BUILD GOTCHA: whisper-rs compiles for every build** and
needs **cmake + libclang** — Linux can skip libclang via the crate's shipped
bindings (`WHISPER_DONT_GENERATE_BINDINGS=1`, glibc-x86_64 only), Windows needs
LLVM (`LIBCLANG_PATH`), macOS uses Xcode's; CI sets these per-OS (Windows has a
`choco install llvm` fallback for the June-2026 image swap). Capture is
watchdog-killed so a stalled stream can't hang shutdown; transcript text logs at
debug (no PII at info). Review (build-CI/correctness/privacy): 0 high findings.

### Earlier this session: two-way audio / push-to-talk (matrix #43)

A per-camera opt-in
`two_way_audio` (DetectConfig flag + tuning toggle) adds a **hold-to-talk**
button to the camera detail view; holding it streams the browser mic to the
camera over WebRTC (the go2rtc player adds a send-only audio track via
`getUserMedia` → `addTransceiver(sendonly)`, riding the #42 `/api/ws` proxy).
Forces WebRTC while talking. **Live-validated in Chrome with a real webcam mic**
(sendonly transceiver negotiated, red "Talking…" over live video). **Mic-privacy
GOTCHA the review caught:** go2rtc's `<video-rtc>` defers its own teardown — and
the sender `track.stop()` — behind a 5 s `DISCONNECT_TIMEOUT`, so just removing
the element leaves the mic hot ~5 s after release; `LiveVideo` cleanup now calls
`sender.track.stop()` immediately (verified: mic ends ~100 ms after release) and
CameraDetail adds a window pointer-up/blur release safety net. Speaker playout
needs a camera with a backchannel (the webcam has none → unvalidated end).

### Earlier this session: remote live-view via an authenticated WebSocket reverse-proxy (matrix #42)

The live player's WebSocket now connects to zoomy's **own origin**
`/api/ws?src=NAME` (`stream_ws`/`proxy_ws` in `api.rs`), which proxies to the
loopback-only go2rtc — browser ⇄ zoomy ⇄ go2rtc. This makes **remote/LAN
live-view work** (MSE/MJPEG media rides the proxied socket, so any viewer gets
video — not just one on the server box), lets us **drop `origin: "*"`** (go2rtc
keeps default same-origin protection — closes the localhost-CSRF the #41 review
flagged), and routes live streams through zoomy's **auth middleware** (a password
now gates live view too; loopback exempt). The proxy builds the upstream URL from
the fixed loopback base + only the urlencoded `src` (no SSRF), pumps both ways
with `tokio::select!`, times out the upstream connect (8s), rejects empty `src`,
and sends a clean Close on failure. Deps: tokio-tungstenite 0.29 (deduped to
axum's) + futures-util, axum `ws` feature — all pure-Rust, no TLS/C.
**Live-validated in Chrome with a real USB webcam: WebRTC + MSE both play 640×480
over `ws://localhost:8080/api/ws` (same origin), go2rtc.yaml has no `origin`.**
Adversarial review: SSRF closed, no leak, 0 real defects above nit. `LiveVideo`
now uses a relative `/api/ws` src; dead `base`/`config` props removed.

### Earlier this session: native live-view player (matrix #41)

The Live grid + camera detail
embed go2rtc's `<video-stream>` web component (a real `<video>` with WebRTC +
MSE/MJPEG fallback) instead of an `<iframe>` onto go2rtc's stream.html — the
long-standing CLAUDE next-step #2. A thin same-origin `GET /api/player/{file}`
proxy (allowlisted) serves go2rtc's player JS (it has no CORS). go2rtc's API is
bound to **127.0.0.1** (was `0.0.0.0` — off the LAN). The player module caches
only on success so a go2rtc restart can't permanently black out tiles
(adversarial review caught that). New `web/src/LiveVideo.tsx`.

### Earlier this session: config backup & restore (matrix #40)

`GET /api/backup` downloads a
JSON snapshot of the configuration (cameras + settings + alarm rules; not
recordings/events/faces) with a `Content-Disposition` header; `POST /api/restore`
imports it. Restore is additive (settings replaced; a camera/alarm whose name
already exists is kept) and **re-points per-camera alarm scopes by camera name**
so they hit the right camera on the new box. Settings page gains a Backup &
restore card. The adversarial review caught a **real high-severity bug**: camera
`source`/`detect_source` flow verbatim into go2rtc's generated YAML, so a newline
in a source (e.g. from a malicious imported backup) could inject an `exec:` stream
→ RCE on go2rtc restart. Fixed by rejecting control characters in source/sub-stream
at **every** entry point (`add_camera`/`patch_camera`/`restore`) — `exec:`/`ffmpeg:`
schemes still allowed — plus a defensive control-char strip in the go2rtc config
writer. Unit-tested + live-validated end-to-end across two instances (round-trip,
idempotent re-restore, alarm remap-by-name with shifted ids, injection rejected).

### Earlier this session: camera groups + Wall view (matrix #39)

Latest before backup/restore: **camera groups + Wall view** (matrix #39). An optional per-camera
`group` tag (nullable `group_name` column) lets the Live grid filter into group
tabs (All / each group / Ungrouped, persisted in localStorage); the Cameras page
gains an inline group editor + `<datalist>` autocomplete + an add-form field.
Bonus correctness win: `patch_camera` now restarts go2rtc **only** when a
stream-relevant field changed (name/source/detect_source/enabled) — metadata-only
edits (group, detect, record, zones) no longer needlessly restart go2rtc and blip
live streams (a step toward the "don't restart go2rtc on CRUD" goal). Server CRUD
live-validated incl. the restart-gating; build/clippy/test/web-build green;
adversarial 2-lens review clean (4 nits, 0 real defects). The group label is
capped at 64 chars.

### Earlier this session: WAN-ready security (matrix #16 closed)

Password storage moved from
salted SHA-256 to **argon2id** (legacy hashes still verify and are transparently
re-hashed on the next successful login). Added a **per-IP login brute-force
throttle** (8 wrong tries in 5 min → HTTP 429 + Retry-After lockout; loopback
exempt so the local box can never lock itself out; the map is swept + capped at
4096 IPs so address rotation can't grow it unbounded). Added **native HTTPS** via
rustls/axum-server: `--tls-self-signed` mints a reusable self-signed cert under
`<data_dir>/tls` (key `0600`/dir `0700` on Unix) for one-flag TLS, or pass
`--tls-cert`/`--tls-key` (and matching `ZOOMY_TLS_*` env vars) for a real
certificate; session cookies gain `Secure` when serving over TLS. Opt-in
`--trusted-proxy` makes auth + throttle key off the right-most `X-Forwarded-For`
hop so a same-host reverse proxy's loopback connection can't inherit the
local-access exemption (and a spoofed `XFF: 127.0.0.1` can't either — a proxied
request is never treated as loopback). **TLS is pinned to the `ring` crypto
provider** (`rustls`/`axum-server` with `default-features=false`), which is
already in-tree via ureq/rumqttc/rcgen — so HTTPS adds **no new C/assembly dep**
(notably no `aws-lc-sys`, which would pull CMake + NASM on Windows CI); confirmed
`aws-lc-sys` is absent from the binary's `cargo tree`. Build/clippy/test green and
**live-validated over HTTPS** (ring handshake, argon2 login, Secure cookie, 401 on
wrong password, plain HTTP to the TLS port refused; with `--trusted-proxy`:
proxied client forced to auth, spoofed-loopback rejected, XFF-keyed lockout fires
on the 9th attempt). New `crates/core/src/tls.rs` holds the self-signed cert
helper; `crates/core/src/auth.rs` gained argon2 + `LoginThrottle` + the
`client_ip` proxy resolver (all unit-tested). A 3-lens adversarial review
workflow (security/correctness/portability) drove the ring switch, key-perms,
throttle-bounding, and trusted-proxy hardening. The matrix rows #29–#38 (zones, anti-fatigue push,
HA discovery, event review, restream fan-out, per-camera detectors, gestures,
LPR/face/GenAI) all shipped earlier on this branch.

### Earlier: hand-signal recognition (#28), 2026-06-10

Latest before the roadmap batch: **hand-signal recognition** (#28) — a `Signals`
page tracks the 21-point
hand-landmark mesh live in the browser (MediaPipe Tasks Vision, GPU, loaded from
a configurable CDN so it stays portable/offline-capable), classifies hand signals
(open-palm/fist/victory/point/thumb-up·down/I-love-you), and on a *held* armed
signal POSTs `/api/gesture` → a first-class `gesture` event (with a context
snapshot) that fires the existing alarm/webhook/ntfy/MQTT machinery — a silent
hand-signal "panic button". New `crates/gesture` holds a pure, unit-tested
geometric classifier + the canonical gesture taxonomy the API normalizes against.
Per-camera toggle, Settings knobs (enable / hold-time / armed list / model URL),
an Alarm `gesture` condition, and Events chip+filter round it out. Server side is
build/clippy/test-green; the live browser overlay needs webcam validation.

### Earlier: competitor matrix shipped, 2026-06-09

On top of the v0.1 slice below: Tauri desktop app (close-to-tray keeps recording,
NSIS installer bundling go2rtc/ffmpeg/model/UI), validated against **real hardware**
(Dahua 4K fixed cam + Amcrest IP2M-866EW pan/tilt). Shipped from the docs/02
matrix: event→recording jump, camera health, per-camera detect tuning + ignore
zones, webhooks, AAC audio, storage stats, sub-stream detect role, timeline
scrubber, event clip export, review split (alerts/detections), ONVIF resolve
(IP+creds → stream URLs) and **ONVIF PTZ** (hold-to-move pad, physically validated),
remote-access auth (loopback exempt), and MQTT (events + availability, verified
against a local broker). CI workflow covers fmt/clippy/test on the three OSes +
web build. Every docs/02 matrix item through #27 is shipped — including face
recognition + LPR (#14), CLIP smart search (#17), PTZ autotrack (#18), YAMNet
audio events (#19), enhanced retention (#20), and camera health pushes (#27).

## v0.1 baseline: Phases 1-4 working on Windows, 2026-06-09

The platform runs end-to-end behind one binary (`cargo run -p zoomy`) + web UI:

- **Phase 0 (spikes):** validated 2026-06-09 — DirectML EP active, 8.7 ms GPU vs
  39.2 ms CPU on bus.jpg; WebRTC playback verified in Chrome. Spike crates are kept
  as standalone validation tools.
- **Phase 1 (core):** `crates/core` — a library (`zoomy::run(ServerConfig,
  shutdown_rx)`) plus a thin CLI bin. Axum API + SQLite (cameras, events, segments,
  settings JSON blob), go2rtc supervised as a child with config generated from the
  registry + watchdog, React/TS web UI in `web/` (live grid via go2rtc stream.html
  iframes, events, recordings, cameras, settings).
- **Desktop app:** `crates/desktop` — Tauri 2 shell embedding the zoomy library
  in-process on port 18080; native window onto the same UI; NSIS installer via
  `npx @tauri-apps/cli build` (bundles web/dist, go2rtc.exe, yolov8n.onnx as
  resources; data goes to the per-user app-data dir). Debug builds deliberately
  use the workspace checkout (shared `data/`) — see comment in `resolve_config`.
- **Phase 2 (recorder):** `crates/recorder` — ffmpeg `-c copy -f segment` per camera
  off go2rtc's RTSP restream, strftime-named 60 s MP4 segments (faststart), SQLite
  index, retention by age + total bytes. Reconciliation loop self-heals dead ffmpeg.
- **Phase 3 (motion gate):** `crates/motion` — 64×64 grayscale diff, noise floor 25,
  changed-pixel fraction vs threshold.
- **Phase 4 (detector):** `crates/detector` (lib form of spike-detect) — one shared
  ONNX session; pipeline polls go2rtc `/api/frame.jpeg` ~1 fps per camera, motion
  gate → YOLO → label/conf filter → per-(camera,label) cooldown → event + annotated
  snapshot.

Verified E2E with synthetic cameras (panning bus video over `exec:ffmpeg` loop):
live WebRTC grid, person/bus events with red-box snapshots, segment recording +
browser playback. A static camera correctly produces zero events (gate works).

Not yet validated: real RTSP camera hardware, macOS (CoreML), Linux (CUDA).
Known soft spots: go2rtc restart on camera CRUD briefly drops live streams; frame
sampling needs camera keyframe interval ≲ a few seconds (real cameras: fine; demo
videos need `-g`), recordings have no audio yet (`-an`).

```
cameras ──RTSP──▶ go2rtc (ingest + WebRTC) ──▶ recorder (packets→disk)   [Phase 2]
                          │                  └─▶ motion gate              [Phase 3]
                          │                       └─▶ AI detector (ONNX)  [Phase 4]
                          └──WebRTC──▶ web UI                             [Phase 1]
                                       core API + SQLite (config/events)  [Phase 1+]
```

## Architecture decisions (don't relitigate without reason)

- **Language:** Rust for the core/services; TypeScript/React for the web UI (future).
- **Reuse, don't rebuild, two binaries:** `go2rtc` handles all camera protocols +
  WebRTC; `FFmpeg` handles codec edge cases. We supervise them as child processes.
  Do NOT write our own RTSP/WebRTC stack. For in-process RTSP later, use the
  **Retina** crate (what Moonfire uses).
- **Recording model:** copy packets to disk WITHOUT decoding (Moonfire's approach) —
  cheap and lossless. Video segments on disk, metadata/index in SQLite.
- **Two-stage detection:** a cheap motion/pixel-diff pass on the low-res sub-stream
  gates expensive AI, which runs YOLO only on cropped motion regions. Never run the
  model on every frame of every camera.
- **AI portability via ONNX Runtime (`ort` crate):** one exported `.onnx`, with a
  per-OS execution provider chosen at runtime — DirectML (Windows), CoreML (macOS),
  CUDA (Linux), CPU fallback. This is the whole cross-platform AI thesis.

## Repository layout

```
Cammy/
├── Cargo.toml                 # workspace (resolver 2); shared dep versions
├── rust-toolchain.toml        # pinned stable + clippy/rustfmt
├── CLAUDE.md                  # this file
├── README.md
├── docs/01-research-and-architecture.md   # field survey, architecture, roadmap
├── config/go2rtc.example.yaml             # reference multi-camera config
├── web/                       # React + TypeScript UI (Vite); build -> web/dist
└── crates/
    ├── core/          # zoomy lib (+ CLI bin): Axum API + SQLite + supervisors + pipeline
    ├── desktop/       # Tauri 2 desktop app embedding the zoomy lib (port 18080)
    ├── detector/      # lib: YOLOv8 via ONNX Runtime, per-OS GPU EP
    ├── motion/        # lib: pixel-diff motion gate
    ├── recorder/      # lib: ffmpeg packet-copy segments + retention
    ├── spike-live/    # Phase 0 spike 1 (kept as standalone validation)
    └── spike-detect/  # Phase 0 spike 2 (kept as standalone validation)
```

Runtime state lives in `data/` (gitignored): `zoomy.db`, `go2rtc.yaml` (generated),
`recordings/{camera}/`, `snapshots/`.

## Build / run / test

```bash
# Build everything
cargo build

# Tests (db, motion gate, NMS/decode, segment scan/retention)
cargo test

# Lint + format (CI should enforce these)
cargo clippy --all-targets -- -D warnings
cargo fmt --all

# Web UI (one-time, or after changing web/)
cd web && npm install && npm run build

# Run the platform headless: http://localhost:8080 (needs bin/go2rtc.exe,
# ffmpeg on PATH, yolov8n.onnx in repo root — see README prerequisites)
cargo run -p zoomy

# Run the desktop app (same engine, native window, port 18080)
cargo run -p zoomy-desktop

# Build the Windows installer (target/release/bundle/nsis/*.exe)
cd crates/desktop && npx @tauri-apps/cli build

# Spikes still run standalone (validation tools)
cargo run -p spike-live -- --rtsp "rtsp://user:pass@192.168.1.50:554/stream1"
cargo run -p spike-detect -- --model yolov8n.onnx --image sample.jpg
```

## Known gotchas

- **`ort` is pinned to `=2.0.0-rc.10`.** Its execution-provider API has churned
  across pre-1.0 releases; if you bump the version, re-check `build_session` in
  `crates/spike-detect/src/main.rs` against the new API and keep the per-OS
  feature flags in `crates/spike-detect/Cargo.toml` in sync. With
  `default-features = false`, the **`std` feature must be re-enabled explicitly**
  (it gates `commit_from_file` and the `std::error::Error` impl on `ort::Error`),
  and **`copy-dylibs`** is needed on Windows so `onnxruntime.dll` lands next to
  the exe. `ort::inputs![...]` returns a value, not a `Result`.
- **External binaries are not vendored.** `go2rtc` and model weights are downloaded
  by the user, not committed (see `.gitignore`). Don't commit binaries or `*.onnx`.
- **YOLOv8 output layout** is assumed to be `[1, 84, 8400]` (4 box + 80 COCO
  classes). YOLOv5/older exports differ and would need decode changes.

## Conventions

- Keep `cargo clippy` clean (`-D warnings`).
- Shared dependencies go in the workspace `[workspace.dependencies]`, referenced with
  `dep.workspace = true` — don't pin versions per-crate except the per-OS `ort`
  feature flags.
- Prefer `anyhow::Result` + `.context(...)` for application errors; reserve custom
  error types for library crates if/when we add them.
- New first-party services become their own crate under `crates/`.

## What to work on next (suggested order)

1. **Real-camera + cross-OS validation:** point the platform at real RTSP/ONVIF
   hardware; build and validate on macOS (CoreML) and Linux (CUDA).
2. **Live-view polish:** replace per-camera stream.html iframes with go2rtc's
   video-stream.js (or MSE) embedded directly; add streams via go2rtc's REST API
   instead of restarting the child on camera CRUD.
3. **Event/recording linkage:** click an event → jump to the recording at that
   timestamp; event-bracketed clip export.
4. **Detection quality:** run YOLO on motion ROIs (crops) instead of full frames;
   sub-stream support (detect on low-res, record high-res); audio in recordings.
5. **Ops:** auth for non-LAN exposure, packaging (installer/service), CI running
   fmt/clippy/test on the three OSes.

When you ship a meaningful chunk, update this file's status section.
