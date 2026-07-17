# Agent task — HomeKit v1 (motion sensors + doorbell + pairing management)

**Status: v1 COMPLETE (2026-07-17).** All three slices shipped + pushed on `main`,
release-rebuilt and live-validated on :8080 (5/5 cams online+recording after):

- **v1a motion sensors** (`1f7e4fb`): `crates/core/src/homekit.rs` worker runs the
  in-process hap-rs "Cammy Sensors" bridge (hap `=0.1.0-pre.15`, TCP 32180,
  FileStorage under `data/homekit-bridge/`, KV `homekit.bridge_pin`), one
  MotionSensor per exposed camera off the events broadcast tap (motion-ish labels,
  45s auto-clear). NOTE: the published hap crate graph cannot resolve (two crates
  `links = "ifaddrs"`) — fixed with a zero-dep `crates/vendor/get_if_addrs` shim
  via `[patch.crates-io]`. 3-lens adversarial review confirmed + fixed: throwaway
  runtime per generation (stale controller sessions), catch_unwind (hap mDNS
  `expect`), aid-cache prune + c# bump on un-expose, 0700 secret dir. One finding
  refuted (bridge PIN Viewer-readable — get_homekit has an in-handler Admin gate).
- **v1b doorbell** (`2036b19`): `DetectConfig.homekit_doorbell` →
  StatelessProgrammableSwitch (aid = id+1+2^20), single press on YAMNet
  "Doorbell" or a soft trigger labeled `doorbell`. Deliberately NOT a HomeKit
  Doorbell service (Home rejects doorbells without a camera stream service).
- **v1c pairing management** (`d592b3a`): per-camera pairing counts + bridge count
  in GET /api/homekit; POST /api/homekit/unpair {camera}; POST /api/homekit/reset
  {target: cameras|sensors} (Admin, audited). Sensor reset = KV marker consumed by
  the WORKER between generations (race-free); rides config_sig for prompt teardown.
- **`a116e16` CRITICAL follow-up fix (live-repro'd):** every bridge teardown
  ABORTED the NVR — libmdns 0.6.3's Responder/Service `Drop` impls `.expect()` on
  a channel whose FSM task (inside hap's run_handle future, which borrows the
  server) always drops first ⇒ double panic in one unwind ⇒ process abort.
  Vendored `crates/vendor/libmdns` via `[patch.crates-io]` with a non-panicking
  send. Re-validated: reset rotates PIN with the server healthy; un-expose
  releases 32180; go2rtc.yaml stays homekit-free when nothing is exposed.

**LIVE-VALIDATED (server side):** bridge binds LAN IP:32180 only when
homekit_enabled + ≥1 exposed camera; teardown/rebuild/reset/unpair all clean;
honest 404 on unknown camera; audit rows written. **NEEDS OWNER (Apple side):**
pair "Cammy Sensors" in the Home app (second code on the Settings card), verify a
motion event flips the sensor + 45s clear, doorbell press on a chime, and Windows
Firewall UDP 5353 + TCP 32180 rules (DEPLOYMENT.md §5c). Note: at validation time
NO camera had homekit_expose on (owner's current state, restored byte-identical).

---

Original recon below (kept for reference).

## Recon findings (grounded, cited)

1. **go2rtc v1.9.14 CANNOT do sensors.** Its HAP accessory is exactly
   [AccessoryInformation, CameraRTPStreamManagement, Microphone] (confirmed via
   binary strings + upstream `pkg/hap/camera/accessory.go`). No MotionSensor/
   Doorbell/ProgrammableSwitchEvent services, no API/webhook to push an event to a
   paired controller. `category_id: doorbell` only changes the icon. Upstream
   feature requests open, unlanded: go2rtc issues #812, #842, #669. **A separate
   Cammy-owned HAP bridge is the only path.** Do NOT attempt linked-motion-inside-
   the-camera-accessory (would mean replacing go2rtc's camera HAP server; hap-rs
   has no camera-stream support — a v2-scale rewrite).

2. **Use the `hap` crate (hap-rs, ewilken/hap-rs) in-process.** Latest
   `0.1.0-pre.15` (pre-release; repo last push 2024-08; HAP protocol is frozen so
   bit-rot risk low — PIN the exact version, consider vendoring). Wants tokio 1.8+
   — our workspace has tokio 1 (lock 1.52.3, root Cargo.toml:33). Has
   `accessory/generated/motion_sensor.rs` (+ example), `occupancy_sensor`,
   `stateless_programmable_switch`, `service/generated/doorbell.rs`; a bridge +
   `server.add_accessory()` supports N MotionSensor accessories. mDNS via bundled
   pure-Rust `libmdns 0.6` responder (Windows-OK; SO_REUSEADDR port-share).
   **Risks:** Windows Firewall must allow UDP 5353 in (DEPLOYMENT.md firewall
   one-liner doesn't cover it); libmdns can be flaky on multi-homed machines (the
   test LAN IS multi-homed); pre-release crate quirks. No other maintained Rust
   HAP server exists; homebridge/HAP-NodeJS sidecar = rejected (Node in installer).

3. **Event source: the existing broadcast tap.** `mqtt.rs:136-162,284` taps EVERY
   EventMsg into `broadcast_tx` (even with MQTT off); channel created in
   `lib.rs:186-192` (`events_bcast_tx`, capacity 256), already consumed by the SSE
   feed. A new HomeKit worker spawned in lib.rs (like audio/posture) holds its own
   `.subscribe()`, filters by camera + motion-ish labels (person/car/pet/etc.),
   sets MotionDetected=true, auto-clears on a per-camera timer (~45s since last
   event). No new plumbing.

4. **Pairing management primitives:** go2rtc camera pairings live in the generated
   go2rtc.yaml (`pairings:` per stream; Cammy already re-parses them —
   `parse_homekit_pairings`, go2rtc.rs:430). "List" = parse counts; "Unpair a
   camera" = regenerate with `pairings: []` + go2rtc restart; "Reset HomeKit" =
   rotate KV `homekit.pin` + `homekit.device_id.*`/`homekit.device_private.*`
   (identity rotation is what actually invalidates pairings) + empty pairings.
   The hap-rs bridge persists its own identity via `FileStorage` — point at
   `<data_dir>/homekit-bridge/`; exposes pairing state programmatically.

5. **Two-pairing UX is unavoidable and honest:** the user pairs each go2rtc camera
   accessory (v0, already the case) + ONE "Cammy Sensors" bridge that brings all
   motion sensors at once. Say so in UI copy; the Home app can associate a
   sensor's automations with a camera tile manually.

## v1 slices (recommended order)
- **v1a — motion sensors** (~2-4 days, the automation unlock): new
  `crates/core/src/homekit.rs` worker, hap-rs bridge, one MotionSensor per
  `homekit_expose` camera, gated on `Settings.homekit_enabled`; label→motion map +
  45s auto-clear; Settings card shows the BRIDGE's own PIN (separate from the
  camera PIN — label them clearly). Windows Firewall UDP 5353 note in DEPLOYMENT.md.
- **v1b — doorbell** (~1 day after v1a): a per-camera "doorbell" flag driving
  `stateless_programmable_switch` (single press) off the YAMNet "Doorbell" audio
  label (already in the default set) or a soft trigger. TEST-FIRST: the Home app
  is picky about Doorbell accessories lacking a camera service — fall back to a
  plain programmable switch if needed.
- **v1c — pairing management UI** (~1 day): Settings card with per-camera go2rtc
  pairing counts + per-stream Unpair + "Reset HomeKit" (rotates pin+identities,
  clearly warns it unpairs everything) + the sensor bridge's own reset.

## Key files
`crates/core/src/go2rtc.rs` (v0 homekit config gen ~258-460, parse_homekit_pairings
~430, KV pin/identity ~373), `crates/core/src/mqtt.rs` (EventMsg + broadcast tap),
`crates/core/src/lib.rs` (worker spawn/join pattern + events_bcast_tx ~186-192),
`crates/core/src/db.rs` (Settings.homekit_enabled, DetectConfig.homekit_expose),
`web/src/pages/Settings.tsx` (the v0 HomeKit card), DEPLOYMENT.md §5c.

## Owner-verification notes from v0 (still true)
Pairing exercised and verified by the owner 2026-07-16/17. The NVR is the release
exe on :8080; release rebuild requires stop → `cargo build --release -p zoomy` →
detached `Start-Process` restart (pre-warm `--release --lib` first while it runs).
`LIBCLANG_PATH=%APPDATA%\Python\Python311\site-packages\clang\native` for cargo.
User directive (2026-07-16): keep new unit tests MINIMAL until the project is
finished — validate via clippy + build + tsc/vite; 1-2 tiny pure-logic tests max
where a bug would be silent.
