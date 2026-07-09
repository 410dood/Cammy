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

mod absence;
mod analytics;
mod anomaly;
mod api;
mod audio;
mod auth;
mod db;
mod digest;
pub mod evidence;
mod gait;
mod genai;
mod go2rtc;
mod health;
mod licensing;
pub mod lpr;
mod mqtt;
mod notify;
mod offsite;
mod onvif_events;
mod parcel;
mod pipeline;
mod posture;
mod proc;
mod ptz;
mod push;
mod record;
mod residential;
mod schedule;
mod severity;
mod sigv4;
mod smart;
mod status;
mod tamper;
pub mod tls;
mod totp;
mod transcribe;
mod util;
mod webpush;

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
    /// PEM certificate to serve HTTPS with. With `tls_key`, the server speaks
    /// TLS instead of plain HTTP. `None` = HTTP (the LAN/reverse-proxy default).
    pub tls_cert: Option<PathBuf>,
    /// PEM private key paired with `tls_cert`.
    pub tls_key: Option<PathBuf>,
    /// Trust a same-host reverse proxy: derive the client IP from the
    /// `X-Forwarded-For` header for auth + throttle decisions, instead of the
    /// (loopback) transport peer. Only enable when the NVR is reachable *only*
    /// through your proxy — see `auth::client_ip`.
    pub behind_proxy: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 8080,
            data_dir: "data".into(),
            ui_dir: "web/dist".into(),
            go2rtc_bin: None,
            ffmpeg_bin: None,
            tls_cert: None,
            tls_key: None,
            behind_proxy: false,
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

    // Stamp the trial clock on first run (no-op once licensed or already begun).
    if let Err(e) = licensing::ensure_trial_started(&db) {
        tracing::warn!("licensing: could not start trial clock: {e:#}");
    }

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
        let (db, go2rtc, dir, snaps, stop) = (
            db.clone(),
            go2rtc.clone(),
            recordings_dir.clone(),
            snapshots_dir.clone(),
            workers_stop.clone(),
        );
        let ffmpeg_bin = cfg.ffmpeg_bin.clone();
        let status = status_board.clone();
        move || record::run(db, go2rtc, dir, snaps, ffmpeg_bin, status, stop)
    })?;
    let (mqtt_tx, mqtt_rx) = std::sync::mpsc::channel::<mqtt::EventMsg>();
    let mqtt_tx2 = mqtt_tx.clone();
    let mqtt_tx_tr = mqtt_tx.clone();
    let mqtt_tx_pose = mqtt_tx.clone();
    let mqtt_tx_onvif = mqtt_tx.clone();
    let mqtt_tx_api = mqtt_tx.clone();
    // The GenAI worker fires VLM-gated alarms after off-thread verification.
    let mqtt_tx_genai = mqtt_tx.clone();
    // Shared per-rule cooldown clock across pipeline / audio / API dispatch.
    let alarm_throttle: notify::AlarmThrottle = Arc::new(std::sync::Mutex::new(Default::default()));
    // GenAI worker channel (pipeline -> captioner + VLM alarm verification).
    let (genai_tx, genai_rx) = std::sync::mpsc::channel::<genai::Job>();
    let det_thread = std::thread::Builder::new().name("detector".into()).spawn({
        let (db, go2rtc, dir, stop) = (
            db.clone(),
            go2rtc.clone(),
            snapshots_dir.clone(),
            workers_stop.clone(),
        );
        let status = status_board.clone();
        let throttle = alarm_throttle.clone();
        move || pipeline::run(db, go2rtc, dir, status, mqtt_tx, throttle, genai_tx, stop)
    })?;
    let genai_thread = std::thread::Builder::new().name("genai".into()).spawn({
        let (db, stop) = (db.clone(), workers_stop.clone());
        move || genai::run(db, genai_rx, mqtt_tx_genai, stop)
    })?;
    // Speech-to-text worker (audio event -> capture -> bundled whisper.cpp).
    let (transcribe_tx, transcribe_rx) = std::sync::mpsc::channel::<transcribe::TranscribeJob>();
    let transcribe_thread = std::thread::Builder::new()
        .name("transcribe".into())
        .spawn({
            let (db, go2rtc, stop) = (db.clone(), go2rtc.clone(), workers_stop.clone());
            let ffmpeg_bin = cfg.ffmpeg_bin.clone();
            let snaps = snapshots_dir.clone();
            let throttle = alarm_throttle.clone();
            move || {
                transcribe::run(
                    db,
                    go2rtc,
                    ffmpeg_bin,
                    snaps,
                    mqtt_tx_tr,
                    throttle,
                    transcribe_rx,
                    stop,
                )
            }
        })?;
    let mqtt_thread = std::thread::Builder::new().name("mqtt".into()).spawn({
        let (db, stop) = (db.clone(), workers_stop.clone());
        move || mqtt::run(db, mqtt_rx, stop)
    })?;
    let health_thread = std::thread::Builder::new().name("health".into()).spawn({
        let (db, stop) = (db.clone(), workers_stop.clone());
        let status = status_board.clone();
        move || health::run(db, status, stop)
    })?;
    // B1: daily AI digest. B3: anomaly scoring. Both opt-in (gated on settings),
    // re-read live config each tick, and join cleanly at shutdown.
    let digest_thread = std::thread::Builder::new().name("digest".into()).spawn({
        let (db, stop) = (db.clone(), workers_stop.clone());
        move || digest::run(db, stop)
    })?;
    let anomaly_thread = std::thread::Builder::new().name("anomaly".into()).spawn({
        let (db, stop) = (db.clone(), workers_stop.clone());
        move || anomaly::run(db, stop)
    })?;
    // Absence/inactivity watch: idles unless a camera sets absence_hours.
    let absence_thread = std::thread::Builder::new().name("absence".into()).spawn({
        let (db, stop) = (db.clone(), workers_stop.clone());
        move || absence::run(db, stop)
    })?;
    // Auto-arm/disarm scheduler (residential modes automation); idles unless
    // Settings.arm_schedule has entries. Re-reads config each tick.
    let schedule_thread = std::thread::Builder::new().name("schedule".into()).spawn({
        let (db, stop) = (db.clone(), workers_stop.clone());
        move || schedule::run(db, stop)
    })?;
    // Generate/persist the VAPID keypair eagerly so the public key handed to
    // subscribing browsers is stable, then run the WebPush fan-out worker.
    if let Err(e) = webpush::vapid_keys(&db) {
        tracing::warn!("WebPush VAPID init failed: {e:#}");
    }
    let push_thread = std::thread::Builder::new().name("push".into()).spawn({
        let (db, stop) = (db.clone(), workers_stop.clone());
        move || push::run(db, stop)
    })?;
    // #70: offsite/cloud backup of recordings to S3-compatible storage. Opt-in
    // (gated on Settings.offsite_backup_enabled), re-reads live config each tick.
    let offsite_thread = std::thread::Builder::new().name("offsite".into()).spawn({
        let (db, stop) = (db.clone(), workers_stop.clone());
        move || offsite::run(db, stop)
    })?;
    let audio_thread = std::thread::Builder::new().name("audio".into()).spawn({
        let (db, go2rtc, dir, stop) = (
            db.clone(),
            go2rtc.clone(),
            snapshots_dir.clone(),
            workers_stop.clone(),
        );
        let (ffmpeg_bin, tx) = (cfg.ffmpeg_bin.clone(), mqtt_tx2);
        let throttle = alarm_throttle.clone();
        let transcribe_tx = transcribe_tx.clone();
        move || {
            audio::run(
                db,
                go2rtc,
                ffmpeg_bin,
                dir,
                tx,
                throttle,
                transcribe_tx,
                stop,
            )
        }
    })?;
    // P2.1 camera-side analytics ingestion (ONVIF PullPoint). Idles unless a
    // camera opts in via detect_config.onvif_events; re-reads config each tick.
    let onvif_inspector: onvif_events::InspectorBoard = Default::default();
    let onvif_thread = std::thread::Builder::new().name("onvif-events".into()).spawn({
        let (db, dir, stop) = (db.clone(), snapshots_dir.clone(), workers_stop.clone());
        let api_base = go2rtc.api_base();
        let inspector = onvif_inspector.clone();
        let (tx, throttle) = (mqtt_tx_onvif, alarm_throttle.clone());
        move || onvif_events::run(db, api_base, dir, inspector, throttle, tx, stop)
    })?;
    // Server-side body-pose worker (residential safety tier: fall / crib standing
    // / covered-face). Idles unless a camera has pose_detect on AND the pose model
    // exists; re-reads config each tick.
    let pose_thread = std::thread::Builder::new().name("pose".into()).spawn({
        let (db, go2rtc, dir, stop) = (
            db.clone(),
            go2rtc.clone(),
            snapshots_dir.clone(),
            workers_stop.clone(),
        );
        let (tx, throttle) = (mqtt_tx_pose, alarm_throttle.clone());
        move || posture::run(db, go2rtc, dir, tx, throttle, stop)
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

    let tls_enabled = cfg.tls_cert.is_some() && cfg.tls_key.is_some();

    // API + static web UI (SPA fallback to index.html).
    let state = api::AppState {
        db: db.clone(),
        go2rtc: go2rtc.clone(),
        snapshots_dir,
        data_dir: cfg.data_dir.clone(),
        clips_dir: cfg.data_dir.join("clips"),
        faces_dir: cfg.data_dir.join("faces"),
        recordings_dir_default: recordings_dir.clone(),
        ffmpeg_bin: cfg.ffmpeg_bin.clone(),
        status: status_board,
        sessions: auth::Sessions::default(),
        login_throttle: auth::LoginThrottle::default(),
        tls: tls_enabled,
        behind_proxy: cfg.behind_proxy,
        mqtt_tx: mqtt_tx_api,
        alarm_throttle,
        onvif_inspector,
    };
    let ui =
        ServeDir::new(&cfg.ui_dir).not_found_service(ServeFile::new(cfg.ui_dir.join("index.html")));
    let app = api::router(state.clone()).fallback_service(ui).layer(
        axum::middleware::from_fn_with_state(state, auth::middleware),
    );

    let addr = SocketAddr::from(([0, 0, 0, 0], cfg.port));
    let make = app.into_make_service_with_connect_info::<SocketAddr>();
    let scheme = if tls_enabled { "https" } else { "http" };
    tracing::info!(
        ui = format!("{scheme}://localhost:{}/", cfg.port),
        go2rtc = format!("{}/", go2rtc.api_base()),
        "Cammy is running"
    );

    if let (Some(cert), Some(key)) = (&cfg.tls_cert, &cfg.tls_key) {
        // Pin the rustls crypto provider so the config builder never panics on
        // an ambiguous default when multiple providers are linked in-tree.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let rustls_cfg = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
            .await
            .with_context(|| {
                format!(
                    "loading TLS cert {} / key {}",
                    cert.display(),
                    key.display()
                )
            })?;
        let handle = axum_server::Handle::new();
        tokio::spawn({
            let handle = handle.clone();
            async move {
                let _ = shutdown_rx.changed().await;
                tracing::info!("shutting down");
                handle.graceful_shutdown(Some(std::time::Duration::from_secs(5)));
            }
        });
        axum_server::bind_rustls(addr, rustls_cfg)
            .handle(handle)
            .serve(make)
            .await
            .with_context(|| format!("serving HTTPS on {addr}"))?;
    } else {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding {addr}"))?;
        axum::serve(listener, make)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.changed().await;
                tracing::info!("shutting down");
            })
            .await?;
    }

    // Orderly teardown: stop workers (they finalize ffmpeg segments), then go2rtc.
    workers_stop.store(true, Ordering::Relaxed);
    let _ = tokio::task::spawn_blocking(move || {
        let _ = rec_thread.join();
        let _ = det_thread.join();
        let _ = audio_thread.join();
        let _ = mqtt_thread.join();
        let _ = health_thread.join();
        let _ = genai_thread.join();
        let _ = transcribe_thread.join();
        let _ = digest_thread.join();
        let _ = anomaly_thread.join();
        let _ = absence_thread.join();
        let _ = onvif_thread.join();
        let _ = schedule_thread.join();
        let _ = pose_thread.join();
        let _ = push_thread.join();
        let _ = offsite_thread.join();
    })
    .await;
    go2rtc.stop();
    Ok(())
}
