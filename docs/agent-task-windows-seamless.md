# Agent task: make the Cammy Windows desktop/appliance experience seamless

> A self-contained prompt to hand to a fresh agent. It covers autostart,
> single-instance, a data-dir lock, a Windows service, auto-update, code-signing,
> and smaller seamless touches — end to end. Copy the whole thing below the line.

---

# Task: make the Cammy Windows desktop/appliance experience seamless (autostart, single-instance, Windows service, auto-update, code-signing) — end to end

You are working in the **Cammy** repo at `e:\dev\ZoomyZoomyCamCam` (a self-hosted,
cross-platform NVR: Rust core in `crates/core`, a Tauri 2 desktop shell in
`crates/desktop`, a React/TS web UI in `web/`, marketing site in `site/`).
**Read `CLAUDE.md` first** (full project status + conventions) and skim the
auto-memory index at `C:\Users\wdood\.claude\projects\e--dev-ZoomyZoomyCamCam\memory\MEMORY.md`.

## Current state (verified — don't re-discover, but do confirm before changing)
- Desktop app: `crates/desktop/src/main.rs` — Tauri 2, runs the whole engine
  in-process via `zoomy::run()` on **port 18080**, close-to-tray KEEPS recording,
  tray has Open/Quit, clean ordered shutdown (ffmpeg finalizes segments). State →
  per-user app-data dir. `crates/desktop/tauri.conf.json`: productName "Cammy",
  identifier `com.cammy.desktop`, bundle targets `["nsis"]`.
- Headless CLI: `crates/core/src/main.rs`, `cargo run -p zoomy`, **port 8080**,
  clap `Args`. Already has a `--verify` flag (evidence bundles).
- `deploy/` has a Linux `zoomy.service` (systemd) + `Caddyfile.example`. There is
  **NO** Windows service, autostart, single-instance guard, updater, or code
  signing anywhere. `DEPLOYMENT.md` only *mentions* "a Windows service" with no
  artifact.
- Precedent for signing: `crates/core/src/evidence.rs` + `licensing.rs` already use
  `ring` Ed25519 — reuse those patterns/habits for the updater key if helpful.

## Deliverables (build ALL of these, one validated commit per item)

### 1. Single-instance + autostart (do this first — highest value/lowest risk)
- Add `tauri-plugin-single-instance`: a second launch focuses the existing window
  instead of starting a second engine.
- Add `tauri-plugin-autostart`: register the desktop app to launch at login.
- Add a **"Start Cammy when I sign in"** toggle in the web UI Settings (reuse the
  existing `TogglePill` primitive + settings plumbing; find how other Settings
  toggles round-trip). It should reflect and control the autostart state. If the
  toggle can only work in the desktop shell (Tauri IPC), gate/hide it gracefully in
  plain browser/server mode.

### 2. Data-dir exclusivity lock (REQUIRED — prevents corruption; do before the service)
- Two engine processes writing the same `data_dir` (e.g. the desktop app AND the
  service, or app + CLI) = duplicate go2rtc/recorder → **corrupted recordings**.
- In `crates/core` (in `zoomy::run` startup), acquire an **exclusive OS lock** on
  `<data_dir>/.cammy.lock` (advisory file lock; use `fs4`/`fs2` or a native lock —
  pick a pure-Rust crate, no new C deps). If held, fail fast with a clear,
  user-facing error ("Another Cammy instance is already using this data folder").
- The desktop app must surface that error in its window rather than silently
  opening a broken second engine.

### 3. Windows service for headless 24/7 recording
- The **headless `zoomy` CLI** (not the Tauri app) is the service body — it must
  record at the lock screen / when logged out.
- Add subcommands to `crates/core/src/main.rs` using the `windows-service` crate
  (Windows-only, `#[cfg(windows)]`): `--install-service`, `--uninstall-service`,
  and the hidden SCM dispatch entry point (`--run-service` or equivalent). Install
  should register auto-start-at-boot + OS restart-on-crash; run as a sensible
  account; pass through `--data-dir`/`--port`.
- **Decide and implement the app-vs-service coexistence model** and document it:
  recommended model = they are mutually exclusive on a given data dir (the lock in
  #2 enforces it). Bonus (optional): when the desktop app detects the service is
  already running (probe its port/health), open as a **UI client pointed at the
  service** instead of starting its own engine. If you don't do the bonus, at least
  make the failure clean and explained.
- Update `DEPLOYMENT.md` with the real Windows-service install steps, and drop any
  helper artifact under `deploy/` if useful.

### 4. Auto-update (Tauri updater)
- Wire `tauri-plugin-updater`: add `plugins.updater` config (pubkey + endpoints),
  `createUpdaterArtifacts: true`, and generate an updater signing keypair. The
  **private key must NOT be committed** — it's supplied via `TAURI_SIGNING_PRIVATE_KEY`
  env at build time; commit only the public key in config. Document key generation.
- Point the update endpoint at a GitHub Releases `latest.json` (repo is
  `410dood/Cammy`). Add/extend a CI workflow (`.github/workflows/`) that builds the
  NSIS bundle + updater artifacts, signs them, and attaches them + `latest.json` to
  the release. If secrets aren't configured, CI must **skip signing gracefully**,
  not fail.
- In-app UX: check on launch (+ a manual "Check for updates" in the tray or
  Settings), show "an update is available → install", and apply on next restart.
  Never interrupt recording without consent.

### 5. Code signing (wire it; cert is a business input)
- Add Windows signing config to `tauri.conf.json` driven by env (e.g.
  `certificateThumbprint`/`signCommand` or Azure Trusted Signing). If no cert/secret
  is present, the build must still succeed **unsigned** (no hard failure). Document
  what the owner must supply. Do not fabricate or hardcode any cert.

### 6. Smaller seamless touches (include if time allows; each its own small commit)
- First-run/Settings helper to add the Windows Firewall rule + show the LAN URL (and
  optionally a QR) so phones on the LAN can reach the UI. Adding a firewall rule
  needs elevation — do it safely (a documented one-liner or an elevated helper),
  never silently.
- Tray tooltip/menu showing live status ("N cameras recording · X GB free") off the
  existing `/api/health` / status board.

## Conventions & validation bar (non-negotiable — see CLAUDE.md)
- `cargo clippy --all-targets -- -D warnings` MUST stay clean; `cargo test -p zoomy`
  green; web `npx tsc --noEmit` + `npm run build` green.
- **Live-validate** each item, don't just build. For engine changes: build to
  `target/debug` first (no server stop), then for release/desktop changes rebuild
  release and actually run it. Note: a Windows-service and login-autostart genuinely
  require testing a reboot/logout or `sc start` — do the real check, and if a step
  truly can't be exercised in this environment, say so explicitly rather than
  claiming it works.
- Prefer **no new C/build-tool dependencies** (the project guards this hard — TLS is
  pinned to `ring`, evidence zip is hand-rolled to avoid deps). Pure-Rust crates are
  fine; call out anything that pulls C.
- Work **incrementally**: one feature → validate → commit → push to `main`. End Rust
  commits' messages with the Co-Authored-By line from CLAUDE.md. Update `CLAUDE.md`'s
  status section and `DEPLOYMENT.md`/`README.md` where relevant when you finish.

## Environment gotchas (will bite you if ignored)
- Building `crates/core` on Windows needs **`LIBCLANG_PATH`** (whisper-rs bindgen):
  `export LIBCLANG_PATH="$APPDATA/Python/Python311/site-packages/clang/native"`.
- A release NVR may be running on **:8080** (7 real cameras). To rebuild the release
  binary you must **stop it first** (`taskkill //IM zoomy.exe //F` and
  `taskkill //IM go2rtc.exe //F`), rebuild, then restart it. The desktop app uses
  :18080 and won't collide with the :8080 server, but they must not share a data dir
  (see the lock in #2).
- Building the **NSIS installer** needs the gitignored model files present in the
  repo root (`yolov8n.onnx`, `clip_text.onnx`, `det_10g.onnx`, etc. — see the
  `resources` map in `tauri.conf.json`). If any are missing the bundle step fails;
  report that rather than working around it by removing resources.
- Installer build: `cd crates/desktop && npx @tauri-apps/cli build`.
- **NEVER commit**: `*.onnx`/model weights, the go2rtc/ffmpeg binaries, real camera
  RTSP credentials (they appear in `data/` and logs — e.g. `rtsp://admin:...@192.168...`),
  or any signing private key.
- Shell is PowerShell primary + a Git-Bash tool. For multi-line git commit messages
  use `git commit -F <file>` (write the message to a temp file) — do NOT use
  PowerShell `@'...'@` here-strings in the Bash tool (they break).

## Definition of done
All six sections implemented and committed on `main`; clippy/tests/tsc/vite green;
each item live-exercised (or its untestable step explicitly flagged); `CLAUDE.md`,
`DEPLOYMENT.md`, and `README.md` updated to describe autostart, the Windows service,
and auto-update; no secrets or binaries committed. Report a concise summary of what
shipped, what was validated how, and any owner-supplied inputs still needed (code-
signing cert, CI secrets).
