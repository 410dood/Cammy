//! ZoomyZoomyCamCam desktop app.
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
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Manager;

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
        .setup(|app| {
            let cfg = resolve_config(app.handle()).context("resolving paths")?;
            tracing::info!(?cfg, "starting embedded zoomy server");

            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let thread = std::thread::Builder::new()
                .name("zoomy-server".into())
                .spawn(move || {
                    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
                    if let Err(e) = rt.block_on(zoomy::run(cfg, shutdown_rx)) {
                        tracing::error!("zoomy server exited with error: {e:#}");
                    }
                })?;
            app.manage(ServerHandle {
                shutdown: shutdown_tx,
                thread: Mutex::new(Some(thread)),
            });

            build_tray(app.handle())?;

            let base = format!("http://127.0.0.1:{PORT}");
            wait_for_health(&base);

            let url: tauri::Url = base.parse().expect("valid localhost url");
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::External(url))
                .title("ZoomyZoomyCamCam")
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

fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open ZoomyZoomyCamCam", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit (stops recording)", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    let mut tray = TrayIconBuilder::with_id("zoomy-tray")
        .tooltip("ZoomyZoomyCamCam — recording")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
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
    tray.build(app)?;
    Ok(())
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Block until the embedded server answers (or ~20s pass; the window will show
/// the error state in that case rather than hanging forever).
fn wait_for_health(base: &str) {
    let url = format!("{base}/api/health");
    for _ in 0..100 {
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
