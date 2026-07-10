//! Cammy desktop app.
//!
//! A thin Tauri shell around the `zoomy` library: it starts the whole platform
//! (API, go2rtc, recorder, AI pipeline) in-process on a background thread, waits
//! for the HTTP server to come up, then opens a native window onto the web UI.
//!
//! NVR semantics: closing the window hides to the system tray and KEEPS
//! recording — an NVR that stops when you close a window is a paperweight.
//! Quit (tray menu) shuts everything down in order, so ffmpeg finalizes its
//! open recording segments and go2rtc dies with the app.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use tauri::menu::{CheckMenuItem, Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Manager;
use tauri_plugin_autostart::ManagerExt as _;

/// Off the common 8080 so the desktop app coexists with ad-hoc dev servers.
const PORT: u16 = 18080;

struct ServerHandle {
    shutdown: tokio::sync::watch::Sender<bool>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,zoomy=info".into()),
        )
        .init();

    tauri::Builder::default()
        // Second launch (double-clicked icon while already running) focuses the
        // existing window instead of starting a second engine on :18080.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let cfg = resolve_config(app.handle()).context("resolving paths")?;
            apply_autostart_default(app.handle(), &cfg.data_dir);
            tracing::info!(?cfg, "starting embedded zoomy server");

            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let startup_err: std::sync::Arc<Mutex<Option<String>>> =
                std::sync::Arc::new(Mutex::new(None));
            let thread = std::thread::Builder::new()
                .name("zoomy-server".into())
                .spawn({
                    let startup_err = startup_err.clone();
                    move || {
                        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
                        if let Err(e) = rt.block_on(zoomy::run(cfg, shutdown_rx)) {
                            tracing::error!("zoomy server exited with error: {e:#}");
                            *startup_err.lock().expect("startup err") = Some(format!("{e:#}"));
                        }
                    }
                })?;
            app.manage(ServerHandle {
                shutdown: shutdown_tx,
                thread: Mutex::new(Some(thread)),
            });

            build_tray(app.handle())?;

            let base = format!("http://127.0.0.1:{PORT}");
            wait_for_health(&base, &startup_err);

            // If the engine died during startup (e.g. the data-dir lock is held
            // by another Cammy — service or CLI), show the reason instead of a
            // dead webview pointed at a server that isn't there.
            let url: tauri::Url = match startup_err.lock().expect("startup err").take() {
                Some(err) => error_page_url(&err),
                None => base.parse().expect("valid localhost url"),
            };
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::External(url))
                .title("Cammy")
                .inner_size(1440.0, 920.0)
                .min_inner_size(900.0, 600.0)
                .build()?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Close-to-tray: the NVR keeps recording in the background.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to build tauri app")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                let handle = app.state::<ServerHandle>();
                let _ = handle.shutdown.send(true);
                // Join so ffmpeg finalizes segments and go2rtc is killed before
                // the process disappears.
                let thread = handle.thread.lock().expect("server handle").take();
                if let Some(t) = thread {
                    let _ = t.join();
                }
            }
        });
}

/// First packaged run: register launch-at-login by default (an NVR that stays
/// off after a reboot is the worst failure mode). Applied exactly once — a
/// marker file makes the user's later choice (tray/Settings toggle) stick.
fn apply_autostart_default(app: &tauri::AppHandle, data_dir: &std::path::Path) {
    if cfg!(debug_assertions) {
        return; // dev builds: never touch the login items
    }
    let marker = data_dir.join(".autostart-default-applied");
    if marker.exists() {
        return;
    }
    if let Err(e) = app.autolaunch().enable() {
        tracing::warn!("could not enable launch-at-login: {e}");
    }
    let _ = std::fs::write(&marker, b"1");
}

fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Cammy", true, None::<&str>)?;
    let autostart_on = app.autolaunch().is_enabled().unwrap_or(false);
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        "Start Cammy when I sign in",
        true,
        autostart_on,
        None::<&str>,
    )?;
    let update = MenuItem::with_id(app, "update", "Check for updates", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit (stops recording)", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &autostart, &update, &quit])?;

    // Quiet launch check: if an update is waiting, the menu item announces it.
    // Installing is ALWAYS an explicit click — never interrupt recording unasked.
    app.manage(PendingUpdate(Mutex::new(None)));
    spawn_update_check(app.clone(), update.clone(), false);

    let mut tray = TrayIconBuilder::with_id("zoomy-tray")
        .tooltip("Cammy — recording")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "autostart" => {
                // The check item toggles itself on click; mirror its new state
                // into the OS launch-at-login registration.
                let want = autostart.is_checked().unwrap_or(false);
                let al = app.autolaunch();
                let res = if want { al.enable() } else { al.disable() };
                if let Err(e) = res {
                    tracing::warn!("launch-at-login toggle failed: {e}");
                    let _ = autostart.set_checked(al.is_enabled().unwrap_or(false));
                }
            }
            "update" => {
                // First click (or launch check) found one -> this click installs.
                let pending = app
                    .state::<PendingUpdate>()
                    .0
                    .lock()
                    .expect("pending update")
                    .take();
                match pending {
                    Some(u) => install_update(app.clone(), u, update.clone()),
                    None => spawn_update_check(app.clone(), update.clone(), true),
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    let tray_icon = tray.build(app)?;

    // Live tooltip: hovering the tray answers "is it actually recording?"
    // without opening the window. Loopback status poll, ~once a minute.
    std::thread::Builder::new()
        .name("tray-status".into())
        .spawn(move || {
            let url = format!("http://127.0.0.1:{PORT}/api/status");
            loop {
                std::thread::sleep(Duration::from_secs(60));
                let tip = status_tooltip(&url)
                    .unwrap_or_else(|| "Cammy — engine not responding".into());
                let _ = tray_icon.set_tooltip(Some(&tip));
            }
        })?;
    Ok(())
}

/// "Cammy — 7 cameras online · 7 recording", from GET /api/status.
fn status_tooltip(url: &str) -> Option<String> {
    let resp: serde_json::Value = ureq::get(url)
        .timeout(Duration::from_secs(3))
        .call()
        .ok()?
        .into_json()
        .ok()?;
    let map = resp.as_object()?;
    let online = map
        .values()
        .filter(|v| v["online"].as_bool().unwrap_or(false))
        .count();
    let recording = map
        .values()
        .filter(|v| v["recording"].as_bool().unwrap_or(false))
        .count();
    Some(format!(
        "Cammy — {online} camera{} online · {recording} recording",
        if online == 1 { "" } else { "s" }
    ))
}

/// An update found by the last check, parked until the user clicks Install.
struct PendingUpdate(Mutex<Option<tauri_plugin_updater::Update>>);

/// Query the release feed off-thread; on success the tray item flips to an
/// explicit "Install update vX" action.
fn spawn_update_check(
    app: tauri::AppHandle,
    item: MenuItem<tauri::Wry>,
    interactive: bool,
) {
    use tauri_plugin_updater::UpdaterExt as _;
    if interactive {
        let _ = item.set_text("Checking for updates…");
        let _ = item.set_enabled(false);
    }
    tauri::async_runtime::spawn(async move {
        let res = match app.updater() {
            Ok(u) => u.check().await,
            Err(e) => Err(e),
        };
        let _ = item.set_enabled(true);
        match res {
            Ok(Some(update)) => {
                let _ = item.set_text(format!(
                    "Install update v{} (restarts Cammy)",
                    update.version
                ));
                *app.state::<PendingUpdate>().0.lock().expect("pending update") = Some(update);
            }
            Ok(None) => {
                let _ = item.set_text(if interactive {
                    "Up to date — check again"
                } else {
                    "Check for updates"
                });
            }
            Err(e) => {
                tracing::warn!("update check failed: {e}");
                if interactive {
                    let _ = item.set_text("Check for updates (last check failed)");
                }
            }
        }
    });
}

/// Download + apply, then restart into the new version. Only ever reached from
/// an explicit user click on the tray item.
fn install_update(
    app: tauri::AppHandle,
    update: tauri_plugin_updater::Update,
    item: MenuItem<tauri::Wry>,
) {
    let _ = item.set_text("Downloading update…");
    let _ = item.set_enabled(false);
    tauri::async_runtime::spawn(async move {
        match update.download_and_install(|_, _| {}, || {}).await {
            Ok(()) => {
                // Clean restart: RunEvent::Exit joins the server thread, so
                // ffmpeg finalizes segments before the new version launches.
                app.restart();
            }
            Err(e) => {
                tracing::error!("update install failed: {e}");
                let _ = item.set_enabled(true);
                let _ = item.set_text("Update failed — check again");
            }
        }
    });
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Block until the embedded server answers (or ~20s pass; the window will show
/// the error state in that case rather than hanging forever). Bails out early
/// if the server thread has already reported a startup error.
fn wait_for_health(base: &str, startup_err: &Mutex<Option<String>>) {
    let url = format!("{base}/api/health");
    for _ in 0..100 {
        if startup_err.lock().expect("startup err").is_some() {
            return;
        }
        if ureq::get(&url)
            .timeout(Duration::from_millis(500))
            .call()
            .is_ok()
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    tracing::warn!("server did not report healthy in time; opening window anyway");
}

/// A self-contained `data:` URL error page — no server needed to render it.
fn error_page_url(err: &str) -> tauri::Url {
    let esc = err
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>Cammy could not start</title>\
         <body style=\"background:#0e1116;color:#dbe2ea;font:15px/1.6 system-ui;\
         display:grid;place-items:center;height:100vh;margin:0\">\
         <div style=\"max-width:560px;padding:32px\">\
         <h1 style=\"font-size:20px;margin:0 0 12px\">Cammy could not start</h1>\
         <p style=\"color:#9aa7b4;white-space:pre-wrap\">{esc}</p>\
         <p style=\"color:#9aa7b4\">Fix the issue above, then quit (tray icon \
         &rarr; Quit) and start Cammy again.</p></div>"
    );
    let data = format!(
        "data:text/html;base64,{}",
        base64_encode(html.as_bytes())
    );
    data.parse().expect("valid data url")
}

/// Tiny local base64 (standard alphabet, padded) — not worth a dependency.
fn base64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Figure out where everything lives.
///
/// Packaged install (release): resources (web UI, go2rtc, ffmpeg, model) are
/// bundled next to the exe and mutable state goes to the per-user app-data dir.
/// Dev (`cargo run -p zoomy-desktop`, debug): run against the workspace
/// checkout, same paths and data/ dir the `zoomy` CLI uses — cameras and
/// events carry over. The branch is decided by build profile, NOT by probing
/// the resource dir: tauri-build copies resources into target/debug too, and
/// running go2rtc from there write-locks files the next build must overwrite.
fn resolve_config(app: &tauri::AppHandle) -> Result<zoomy::ServerConfig> {
    if !cfg!(debug_assertions) {
        let res = app.path().resource_dir().context("resource dir")?;
        if res.join("web/dist/index.html").exists() {
            let data_dir = app.path().app_data_dir().context("app data dir")?;
            std::fs::create_dir_all(&data_dir).ok();
            // Relative paths in settings (e.g. model_path "yolov8n.onnx")
            // resolve against the resource dir.
            std::env::set_current_dir(&res).ok();
            return Ok(zoomy::ServerConfig {
                port: PORT,
                data_dir,
                ui_dir: res.join("web/dist"),
                go2rtc_bin: Some(res.join("bin").join(exe_name("go2rtc"))),
                ffmpeg_bin: existing(res.join("bin").join(exe_name("ffmpeg"))),
                // The desktop app talks to itself over loopback — no TLS needed.
                ..Default::default()
            });
        }
    }

    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workspace = workspace
        .canonicalize()
        .context("locating workspace root")?;
    std::env::set_current_dir(&workspace).context("entering workspace root")?;
    Ok(zoomy::ServerConfig {
        port: PORT,
        data_dir: workspace.join("data"),
        ui_dir: workspace.join("web/dist"),
        go2rtc_bin: Some(workspace.join("bin").join(exe_name("go2rtc"))),
        ffmpeg_bin: existing(workspace.join("bin").join(exe_name("ffmpeg"))),
        ..Default::default()
    })
}

/// ffmpeg is optional at the explicit path (recording falls back to PATH).
fn existing(p: PathBuf) -> Option<PathBuf> {
    p.exists().then_some(p)
}

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}
