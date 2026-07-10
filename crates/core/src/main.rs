//! zoomy CLI — thin wrapper over the `zoomy` library (see lib.rs). The Tauri
//! desktop app embeds the same library; this binary is the headless/server way
//! to run the platform. On Windows it can also install itself as a service
//! (`--install-service`) so recording continues at the lock screen / logged out.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use zoomy::ServerConfig;

#[cfg(windows)]
mod winsvc;

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

    /// Verify an exported evidence bundle (.zip) and exit, instead of starting
    /// the server. Re-checks the Ed25519 signature over its manifest and re-hashes
    /// the enclosed clip; exits non-zero on any mismatch. Fully offline.
    #[arg(long, value_name = "BUNDLE.zip")]
    verify: Option<PathBuf>,

    /// Install Cammy as a Windows service (auto-start at boot, restart on
    /// crash, records with nobody signed in). Run from an elevated prompt.
    /// Captures --data-dir/--ui-dir/--port as the service's configuration.
    #[cfg(windows)]
    #[arg(long)]
    install_service: bool,

    /// Stop and remove the Cammy Windows service. Run from an elevated prompt.
    #[cfg(windows)]
    #[arg(long)]
    uninstall_service: bool,

    /// (internal) Service entry point — only valid when launched by the
    /// Windows service control manager.
    #[cfg(windows)]
    #[arg(long, hide = true)]
    run_service: bool,

    /// (internal) Working directory recorded at service-install time.
    #[cfg(windows)]
    #[arg(long, hide = true)]
    service_workdir: Option<PathBuf>,
}

/// Parse this process's argv. Also used by the service entry path, where the
/// SCM passes the install-time launch arguments through the normal argv.
fn cli_args() -> Args {
    Args::parse()
}

/// Resolve CLI args into the library's `ServerConfig` (shared by the terminal
/// run and the Windows service body). TLS: explicit cert+key win; otherwise
/// --tls-self-signed mints a reusable self-signed pair under <data_dir>/tls.
fn server_config(args: &Args) -> Result<ServerConfig> {
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
    Ok(ServerConfig {
        port: args.port,
        data_dir: args.data_dir.clone(),
        ui_dir: args.ui_dir.clone(),
        go2rtc_bin: args.go2rtc_bin.clone(),
        ffmpeg_bin: args.ffmpeg_bin.clone(),
        tls_cert,
        tls_key,
        behind_proxy: args.trusted_proxy,
    })
}

fn main() -> Result<()> {
    let args = cli_args();

    // Service paths first: --run-service hands the process to the SCM
    // dispatcher (no console; it sets up its own file logging), the
    // install/uninstall paths are plain console commands.
    #[cfg(windows)]
    {
        if args.run_service {
            return winsvc::run();
        }
        if args.install_service && args.uninstall_service {
            anyhow::bail!("pick one of --install-service / --uninstall-service");
        }
        if args.install_service {
            return winsvc::install(&args.data_dir, &args.ui_dir, args.port);
        }
        if args.uninstall_service {
            return winsvc::uninstall();
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,zoomy=info".into()),
        )
        .init();

    // `--verify <bundle.zip>`: a standalone, offline check — no server, no data
    // dir needed. Print a human report; a failed check returns Err (non-zero exit).
    if let Some(bundle) = args.verify.as_ref() {
        return zoomy::evidence::verify_bundle_cli(bundle);
    }

    let cfg = server_config(&args)?;
    let scheme = if cfg.tls_cert.is_some() { "https" } else { "http" };

    println!();
    println!("  Cammy is starting");
    println!("      Web UI:   {scheme}://localhost:{}/", cfg.port);
    println!("      API:      {scheme}://localhost:{}/api/health", cfg.port);
    println!("  Press Ctrl+C to stop.");
    println!();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = shutdown_tx.send(true);
        });
        zoomy::run(cfg, shutdown_rx).await
    })
}
