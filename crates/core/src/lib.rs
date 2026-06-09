//! zoomy as a library — everything the `zoomy` CLI binary does, callable from
//! other shells (the Tauri desktop app embeds this and runs the whole platform
//! in-process).
//!
//! The platform:
//!   - serves the web UI and JSON API (Axum)
//!   - owns the camera registry / events / recordings index (SQLite)
//!   - supervises go2rtc (ingest + WebRTC) as a child process
//!   - runs continuous packet-copy recording with retention (ffmpeg)
//!   - runs the motion-gated AI detection pipeline (ONNX Runtime)

mod api;
mod auth;
mod db;
mod go2rtc;
mod pipeline;
mod record;
mod status;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Port for the web UI / API.
    pub port: u16,
    /// Where the database, recordings and snapshots live.
    pub data_dir: PathBuf,
    /// Built web UI to serve (Vite build output).
    pub ui_dir: PathBuf,
    /// Explicit go2rtc binary; `None` = ./bin, then PATH.
    pub go2rtc_bin: Option<PathBuf>,
    /// Explicit ffmpeg binary; `None` = ./bin, then PATH.
    pub ffmpeg_bin: Option<PathBuf>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 8080,
            data_dir: "data".into(),
            ui_dir: "web/dist".into(),
            go2rtc_bin: None,
            ffmpeg_bin: None,
        }
    }
}

/// Run the whole platform until `shutdown_rx` fires (any change), then tear
/// down in order: HTTP server -> workers (ffmpeg finalizes segments) -> go2rtc.
pub async fn run(
    cfg: ServerConfig,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let db = db::Db::open(&cfg.data_dir.join("zoomy.db")).context("opening database")?;
    let settings = db.settings();
    db.save_settings(&settings)?; // persist defaults on first run

    let go2rtc = Arc::new(go2rtc::Go2Rtc::new(
        cfg.go2rtc_bin.as_deref(),
        cfg.data_dir.join("go2rtc.yaml"),
        settings.go2rtc_api_port,
    )?);
    go2rtc.restart_with(&db).context("starting go2rtc")?;

    let workers_stop = Arc::new(AtomicBool::new(false));
    let snapshots_dir = cfg.data_dir.join("snapshots");
    let recordings_dir = cfg.data_dir.join("recordings");
    let status_board = status::StatusBoard::default();

    // Recording manager + detection pipeline run on their own threads (both
    // drive blocking child processes / inference).
    let rec_thread = std::thread::Builder::new().name("recorder".into()).spawn({
        let (db, go2rtc, dir, stop) = (
            db.clone(),
            go2rtc.clone(),
            recordings_dir.clone(),
            workers_stop.clone(),
        );
        let ffmpeg_bin = cfg.ffmpeg_bin.clone();
        let status = status_board.clone();
        move || record::run(db, go2rtc, dir, ffmpeg_bin, status, stop)
    })?;
    let det_thread = std::thread::Builder::new().name("detector".into()).spawn({
        let (db, go2rtc, dir, stop) = (
            db.clone(),
            go2rtc.clone(),
            snapshots_dir.clone(),
            workers_stop.clone(),
        );
        let status = status_board.clone();
        move || pipeline::run(db, go2rtc, dir, status, stop)
    })?;

    // go2rtc watchdog.
    tokio::spawn({
        let (db, go2rtc, stop) = (db.clone(), go2rtc.clone(), workers_stop.clone());
        async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tick.tick().await;
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                if let Err(e) = go2rtc.ensure_alive(&db) {
                    tracing::warn!("go2rtc watchdog: {e:#}");
                }
            }
        }
    });

    // API + static web UI (SPA fallback to index.html).
    let state = api::AppState {
        db: db.clone(),
        go2rtc: go2rtc.clone(),
        snapshots_dir,
        clips_dir: cfg.data_dir.join("clips"),
        ffmpeg_bin: cfg.ffmpeg_bin.clone(),
        status: status_board,
        sessions: auth::Sessions::default(),
    };
    let ui =
        ServeDir::new(&cfg.ui_dir).not_found_service(ServeFile::new(cfg.ui_dir.join("index.html")));
    let app = api::router(state.clone()).fallback_service(ui).layer(
        axum::middleware::from_fn_with_state(state, auth::middleware),
    );

    let addr = SocketAddr::from(([0, 0, 0, 0], cfg.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;

    tracing::info!(
        ui = format!("http://localhost:{}/", cfg.port),
        go2rtc = format!("{}/", go2rtc.api_base()),
        "ZoomyZoomyCamCam is running"
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = shutdown_rx.changed().await;
        tracing::info!("shutting down");
    })
    .await?;

    // Orderly teardown: stop workers (they finalize ffmpeg segments), then go2rtc.
    workers_stop.store(true, Ordering::Relaxed);
    let _ = tokio::task::spawn_blocking(move || {
        let _ = rec_thread.join();
        let _ = det_thread.join();
    })
    .await;
    go2rtc.stop();
    Ok(())
}
