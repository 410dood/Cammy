# Agent task — Phase 2 remainder + Phase 3 (roadmap finish)

**Status: RECON DONE, BUILD NOT STARTED.** Session ended at the session limit right
after recon. Nothing in scope has been implemented or committed yet. The working tree
is clean (only `.claude/` + two installer preview PNGs untracked, neither ours). Start
the next session by reading this file, then `docs/08-competitor-parity-2026-07.md` and
`CLAUDE.md`.

Full per-feature recon (**all 18/18 features**, structured JSON — files to edit with
line refs, new files, API, db, web, conflicts, effort, descope, RBAC, validation plan)
is in **`docs/roadmap-recon-2026-07-16.json`** (337 KB; load & parse it before designing
any feature). The workflow completed clean (18/18 agents, 0 errors). Only **P2.18**
returned a garbage "test" summary and must be re-run first; P3.5/P3.9/P3.10 all returned
complete valid maps (an earlier in-session note wrongly listed them as failed — a journal
dedup bug, since corrected).

## Scope (18 features — exactly this, nothing else)

Phase 2 remainder: **P2.5** CLIP attribute facets · **P2.8b** per-camera feedback
learning · **P2.9** deterrence actions · **P2.10** presence/geofence arm · **P2.11**
per-user notification matrix · **P2.14** selective offsite + adaptive retention ·
**P2.16** object lifecycle view · **P2.18** camera hotspots.
Phase 3: **P3.1** journey fusion · **P3.2** ask-your-cameras · **P3.3** HA integration ·
**P3.4** HomeKit v0 · **P3.5** OpenVINO EP + CLIP zone-state v0 · **P3.6** detector
worker pool · **P3.7** dual-stream recording · **P3.8** detection-triggered recording ·
**P3.9** two-box archive · **P3.10** offline footage import.
Out of scope: anti-feature list, pet Re-ID, zoomy→cammy rename, owner-secret work.

## KEY RECON FINDINGS — several items are much smaller than the roadmap implies

- **P2.14 (XL→M/L):** the "degrade quality before deleting" half already SHIPS as
  age-based enhanced retention — `Settings.enhanced_retention_days` +
  `enhanced_retention_encoder`, `segments.reduced` column, `db.segments_to_reduce`
  (db.rs:3426), `db.mark_segment_reduced` (db.rs:3443). Genuinely-new work = (a)
  *selective* offsite (back up only event/bookmarked segments — filter
  `offsite.rs pending_offsite()` / `db.pending_offsite` db.rs:3579), (b) a
  *capacity-adaptive* trigger (degrade under space pressure, not just age).
- **P2.5 (M):** near-verbatim generalization of the already-shipped P2.2 prompt-rule
  mechanism (`AlarmRule.prompt_like` / `is_prompt_rule` / `matches_prompt` /
  `fire_prompt_alarms` in pipeline.rs) — reuse the CLIP-text-vs-crop-embedding cosine path.
- **P2.16 (M):** almost pure plumbing — `tracker::Track` already carries stable id +
  bounded `history: VecDeque<(ts,x,y)>` trajectory; analytics events already exist. Wire
  path_json persist + a read-side detail view.
- **P2.9 (L, DEFENSIVE):** NO ONVIF relay/siren/white-light output code exists. Reuse the
  ptz.rs ONVIF SOAP client (`CamTarget`/`parse_source`/`soap_call`/`extract_between`) to
  add SetRelayOutput/white-light SOAP; add action kind `"deterrence"` to
  `notify::fire_action` (notify.rs:211, `Action{kind,target,priority}`). Backchannel/siren
  sink has NEVER been live-validated → fail soft, probe + surface capability honestly,
  arm-mode-gate, never auto-escalate.
- **P2.10 (S):** `/api/arm` (get/set arm_mode KV, api.rs:4730-4792) + `POST /api/arm`
  already routed; layer multi-phone first-in/last-out presence table + endpoint. Almost
  entirely additive.
- **P2.11 (L):** two disconnected notify paths — `notify::fire()/fire_action()` (ntfy/
  email/webhook/mqtt, called from ~10 sites) AND the separate `push.rs` WebPush worker.
  Per-user matrix must route BOTH through a delivery-preference table.
- **P3.3 (XL):** the Rust half is M — `mqtt::EventMsg` is a single system-wide event
  choke point (mpsc::Sender cloned into every producer); add SSE feed + inbound MQTT
  command subscribe there. The HACS python component is the XL part.
- **P3.4 (L):** the vendored `bin/go2rtc.exe` (v1.9.14) ships real HAP support — v0 =
  configure go2rtc as a HAP accessory bridge, live-view only. Pairing needs real
  HomeKit hardware → ship config+docs, record what a human must verify.
- **P3.6 (L):** `pipeline::run` (pipeline.rs ~117-1331) is ONE thread serially iterating
  cameras with a BLOCKING `ureq::get` frame fetch. Pool = parallelize per-camera.
- **P3.7 (L):** go2rtc already publishes `{name}_sub` low-res when `detect_source` set;
  record it with a second reused `recorder::Recording::start`. Needs a `stream` column on
  segments (migration) + query-param plumbing.
- **P3.8 (M):** no packet ring buffer exists → real pre-roll impossible as literal
  start/stop; implement as retention/keep-window gating around events instead (honest v0).

## Grounding facts (verified against current code this session)

- **Worker spawn pattern** (lib.rs:159-310): each worker = named `std::thread` closure
  taking `db.clone()` + `workers_stop.clone()` (+ optional `go2rtc`/`status`/`mqtt_tx`
  clone/`alarm_throttle`), joined in the teardown block (lib.rs:406-423). New backend
  workers (P3.3 SSE, P3.5 zone-state, P3.6 pool, P3.9 archive, P3.10 import) follow this.
- **Migration idiom** (db.rs:1380+): `let _ = conn.execute("ALTER TABLE x ADD COLUMN y ...", []);`
  (idempotent, swallows dup-column err). Tables via `CREATE TABLE IF NOT EXISTS`
  (db.rs:1346+). Many features are JSON-blob fields on Settings/DetectConfig/AlarmRule =
  NO migration.
- **Router insert point:** api.rs:184, just before `.with_state(state)`. Helpers:
  `ApiError`, `bad_request`, `not_found`, `forbidden` (api.rs:189-211).
- **RBAC scoping:** every new endpoint MUST scope per-camera like `list_events`/
  `get_event_api` via `allowed_cameras`/`require_camera`; `user_cameras` table exists
  (db.rs:1597). Auth in auth.rs (`Role` Viewer<Operator<Admin, `middleware` at
  auth.rs:474, `min_role_for` method+path gating).
- **Alarm/notify model:** `Action{kind:String,target:String,priority:u8}` (db.rs:519);
  `AlarmRule` (db.rs:531) already has label/face_like/plate_like/gesture_like/
  transcript_like/face_unknown/zone_like/confirm_label/vlm_prompt/describe/prompt_like/
  modes/actions. Dispatch = `notify::fire_action` match on `action.kind` (notify.rs:230).
- **Live NVR baseline (:8080):** 5 enabled cams online+recording on DirectML — id 3
  front-door, 4 ptz-cam, 5 side, 6 pool2, 8 pool3. Cams 1 (driveway) & 2 (porch)
  disabled. Confirm 5/5 online+recording after every release restart.
- **Build gotchas (from CLAUDE.md/memory, verify):** `LIBCLANG_PATH=%APPDATA%\Python\Python311\site-packages\clang\native`
  for whisper-rs bindgen; debug builds run WITHOUT stopping the server; a release rebuild
  needs zoomy+go2rtc STOPPED first (exe file-locked), restart via detached PowerShell
  `Start-Process` (never a timed background shell); PWA service worker serves a stale
  shell for ONE load after a dist rebuild — validate on the SECOND reload.
- Remote: `github.com/410dood/Cammy`, main in sync with origin (0/0). Commit per validated
  feature, `Co-Authored-By` the model; push each wave when green.

## Proposed wave plan (group by SECONDARY conflict surface + dependency)

Almost every feature touches api.rs + db.rs + api.ts, so the orchestrator serializes
those small shared-file edits; parallelism comes from design/new-module-authoring/review
agents. Waves group by the *contended* secondary file (pipeline.rs, notify.rs, mqtt.rs,
record.rs, lib.rs) so no two concurrent impls fight over it.

- **Wave 1 — notify/arm spine** (shares notify.rs): P2.11 (do first — foundational
  routing) → P2.10 → P2.9. 
- **Wave 2 — CLIP/embedding + pipeline** (shares pipeline.rs): P2.5 → P2.8b → P2.16 →
  P3.5(classifier half).
- **Wave 3 — forensic/read-side + web** (additive api.rs + web, more parallelizable):
  P3.1, P2.18, P3.8.
- **Wave 4 — recording/perf core** (shares record.rs/pipeline.rs/lib.rs, serialize):
  P3.7 → P3.6 → P2.14.
- **Wave 5 — ecosystem**: P3.3, P3.2, P3.4, P3.9, P3.10, P3.5(OpenVINO EP half).

Per feature: design agent → impl (orchestrator integrates shared files; subagents author
self-contained new modules/components) → 3-lens adversarial review (correctness/security/
UX-honesty, hand-verify each finding) → fix → live-validate in Chrome on :8080 → 1 commit.
Between waves: self-review workflow over the accumulated diff. Update CLAUDE.md status +
this file's status after each wave.

## Quality bar per commit (non-negotiable)
`cargo clippy --all-targets -- -D warnings` clean · `cargo test` green (168+ core) ·
web `tsc --noEmit` + `vite build` green · live-validated in Chrome vs the real NVR.
Descope honestly (v0 documented in commit + CLAUDE.md + honest UI); never fake capability,
skip silently, or disable a test.
