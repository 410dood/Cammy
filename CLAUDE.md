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

## Current status: v0.3 — competitor matrix 61/61, full commercial analytics suite, 2026-06-20

### This session: commercial video-analytics suite (matrix #53–#61) on the object tracker

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
