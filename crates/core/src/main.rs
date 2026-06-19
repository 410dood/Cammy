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

    /// Serve HTTPS using this PEM certificate (requires --tls-key).
    #[arg(long, env = "ZOOMY_TLS_CERT")]
    tls_cert: Option<PathBuf>,

    /// PEM private key paired with --tls-cert.
    #[arg(long, env = "ZOOMY_TLS_KEY")]
    tls_key: Option<PathBuf>,

    /// Serve HTTPS with an auto-generated self-signed certificate stored under
    /// <data_dir>/tls (reused across runs). Ignored if both --tls-cert and
    /// --tls-key are given.
    #[arg(long)]
    tls_self_signed: bool,

    /// Trust X-Forwarded-For from a same-host reverse proxy (nginx/Caddy/etc.)
    /// for client-IP identification. Enable ONLY when the NVR is reachable
    /// solely through your proxy — otherwise a forged header could spoof the
    /// client address. Without this, a same-host proxy's loopback connection
    /// would inherit the local-access exemption and bypass the password.
    #[arg(long)]
    trusted_proxy: bool,
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

    // Resolve TLS: explicit cert+key win; otherwise --tls-self-signed mints a
    // reusable self-signed pair under <data_dir>/tls.
    let (tls_cert, tls_key) = match (&args.tls_cert, &args.tls_key) {
        (Some(c), Some(k)) => (Some(c.clone()), Some(k.clone())),
        (Some(_), None) | (None, Some(_)) => {
            anyhow::bail!("--tls-cert and --tls-key must be given together");
        }
        (None, None) if args.tls_self_signed => {
            let (c, k) = zoomy::tls::ensure_self_signed(&args.data_dir)?;
            (Some(c), Some(k))
        }
        (None, None) => (None, None),
    };
    let scheme = if tls_cert.is_some() { "https" } else { "http" };

    println!();
    println!("  Cammy is starting");
    println!("      Web UI:   {scheme}://localhost:{}/", args.port);
    println!(
        "      API:      {scheme}://localhost:{}/api/health",
        args.port
    );
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
            tls_cert,
            tls_key,
            behind_proxy: args.trusted_proxy,
        },
        shutdown_rx,
    )
    .await
}
