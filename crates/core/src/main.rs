//! zoomy CLI — thin wrapper over the `zoomy` library (see lib.rs). The Tauri
//! desktop app embeds the same library; this binary is the headless/server way
//! to run the platform.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use zoomy::ServerConfig;

#[derive(Parser, Debug)]
#[command(name = "zoomy", version, about)]
struct Args {
    /// Port for the web UI / API.
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// Where the database, recordings and snapshots live.
    #[arg(long, default_value = "data")]
    data_dir: PathBuf,

    /// Built web UI to serve (Vite build output).
    #[arg(long, default_value = "web/dist")]
    ui_dir: PathBuf,

    /// Path to the go2rtc binary. Falls back to ./bin, then PATH.
    #[arg(long, env = "GO2RTC_BIN")]
    go2rtc_bin: Option<PathBuf>,

    /// Path to the ffmpeg binary. Falls back to ./bin, then PATH.
    #[arg(long, env = "FFMPEG_BIN")]
    ffmpeg_bin: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,zoomy=info".into()),
        )
        .init();
    let args = Args::parse();

    println!();
    println!("  ZoomyZoomyCamCam is starting");
    println!("      Web UI:   http://localhost:{}/", args.port);
    println!("      API:      http://localhost:{}/api/health", args.port);
    println!("  Press Ctrl+C to stop.");
    println!();

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = shutdown_tx.send(true);
    });

    zoomy::run(
        ServerConfig {
            port: args.port,
            data_dir: args.data_dir,
            ui_dir: args.ui_dir,
            go2rtc_bin: args.go2rtc_bin,
            ffmpeg_bin: args.ffmpeg_bin,
        },
        shutdown_rx,
    )
    .await
}
