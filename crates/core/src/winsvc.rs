//! Windows service plumbing for the headless NVR.
//!
//! The desktop app runs in a login session and stops at sign-out; a true
//! always-on appliance needs a service that records at the lock screen and
//! with nobody signed in. The **headless CLI** is the service body:
//!
//!   zoomy --install-service [--data-dir D] [--port P]   (elevated prompt)
//!   zoomy --uninstall-service                           (elevated prompt)
//!   zoomy --run-service ...                             (SCM only, hidden)
//!
//! Coexistence model: the service and the desktop app / CLI are **mutually
//! exclusive per data dir** — `zoomy::run`'s advisory lock on
//! `<data_dir>/.cammy.lock` enforces it, and each side fails fast with a
//! message naming the other. (The default desktop data dir differs from the
//! service's, so a stock install of both never collides.)
//!
//! This module is only compiled on Windows (`#[cfg(windows)]` in main.rs).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
    ServiceErrorControl, ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

/// SCM key (`net start cammy`); the display name is what Services.msc shows.
pub const SERVICE_NAME: &str = "cammy";
const DISPLAY_NAME: &str = "Cammy NVR";

/// Register the service: auto-start at boot, restart-on-crash, running the
/// current exe with `--run-service` + absolute paths captured NOW (a service
/// starts in System32, so nothing may stay relative).
pub fn install(data_dir: &Path, ui_dir: &Path, port: u16) -> Result<()> {
    let exe = std::env::current_exe().context("locating zoomy.exe")?;
    let cwd = std::env::current_dir().context("current dir")?;
    let abs = |p: &Path| {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        }
    };
    let (data_dir, ui_dir) = (abs(data_dir), abs(ui_dir));

    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .context("opening the service manager (run this from an elevated/Administrator prompt)")?;

    let launch_arguments = vec![
        OsString::from("--run-service"),
        OsString::from("--service-workdir"),
        cwd.clone().into_os_string(),
        OsString::from("--data-dir"),
        data_dir.clone().into_os_string(),
        OsString::from("--ui-dir"),
        ui_dir.into_os_string(),
        OsString::from("--port"),
        OsString::from(port.to_string()),
    ];
    let info = ServiceInfo {
        name: SERVICE_NAME.into(),
        display_name: DISPLAY_NAME.into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe,
        launch_arguments,
        dependencies: vec![],
        account_name: None, // LocalSystem: records with nobody signed in
        account_password: None,
    };
    let service = manager
        .create_service(
            &info,
            ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::QUERY_STATUS,
        )
        .context("creating the service (already installed? --uninstall-service first)")?;
    service.set_description(
        "Cammy self-hosted NVR: 24/7 camera recording, AI detection and the web UI. \
         Runs headless — records at the lock screen and with nobody signed in.",
    )?;
    // OS-level crash recovery: restart 5s after each of the first 3 failures
    // per day. (Clean stops don't count as failures.)
    service.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(86400)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![
            ServiceAction {
                action_type: ServiceActionType::Restart,
                delay: Duration::from_secs(5)
            };
            3
        ]),
    })?;
    service
        .start::<&std::ffi::OsStr>(&[])
        .context("starting the service")?;

    println!("Installed and started the '{DISPLAY_NAME}' service ({SERVICE_NAME}).");
    println!("  Data dir : {}", data_dir.display());
    println!("  Web UI   : http://localhost:{port}/");
    println!("  Logs     : {}", data_dir.join("service.log").display());
    println!("  Manage   : net stop {SERVICE_NAME} / net start {SERVICE_NAME}, or Services.msc");
    println!();
    println!("Note: the service and the Cammy desktop app must not share a data folder —");
    println!("whichever starts second will refuse to run (recording-corruption guard).");
    Ok(())
}

/// Stop (if running) and delete the service.
pub fn uninstall() -> Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("opening the service manager (run from an elevated prompt)")?;
    let service = manager
        .open_service(
            SERVICE_NAME,
            ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
        )
        .context("service not found (is it installed?)")?;
    if service.query_status()?.current_state != ServiceState::Stopped {
        let _ = service.stop();
        // Give the engine a moment to finalize open recording segments.
        for _ in 0..50 {
            if service.query_status()?.current_state == ServiceState::Stopped {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    service.delete().context("deleting the service")?;
    println!("Uninstalled the '{DISPLAY_NAME}' service.");
    Ok(())
}

define_windows_service!(ffi_service_main, service_main);

/// `--run-service`: hand this process to the SCM dispatcher. Blocks until the
/// service stops. Only the SCM may call this (a terminal launch errors fast).
pub fn run() -> Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).context(
        "connecting to the service control manager \
         (--run-service is only valid when launched BY the SCM; \
         use --install-service to set that up)",
    )?;
    Ok(())
}

fn service_main(_ffi_args: Vec<OsString>) {
    // The install-time launch arguments arrive as this process's argv; reparse
    // them with the normal CLI parser so the service honors the same flags.
    let args = crate::cli_args();
    if let Err(e) = service_body(args) {
        tracing::error!("service exited with error: {e:#}");
    }
}

fn service_body(args: crate::Args) -> Result<()> {
    // A service starts in System32 — enter the install-time workdir so ./bin
    // (go2rtc/ffmpeg) and relative model paths resolve exactly like the CLI.
    if let Some(dir) = args.service_workdir.as_ref() {
        std::env::set_current_dir(dir)
            .with_context(|| format!("entering workdir {}", dir.display()))?;
    }
    init_service_logging(&args.data_dir);

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let status_handle =
        service_control_handler::register(SERVICE_NAME, move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown | ServiceControl::Preshutdown => {
                let _ = shutdown_tx.send(true);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })
        .context("registering the service control handler")?;

    let set_state = |state: ServiceState, exit: ServiceExitCode| {
        let _ = status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: if state == ServiceState::Running {
                ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
            } else {
                ServiceControlAccept::empty()
            },
            exit_code: exit,
            checkpoint: 0,
            wait_hint: Duration::from_secs(30),
            process_id: None,
        });
    };
    set_state(ServiceState::Running, ServiceExitCode::Win32(0));

    let rt = tokio::runtime::Runtime::new().context("tokio runtime")?;
    let cfg = crate::server_config(&args)?;
    let result = rt.block_on(zoomy::run(cfg, shutdown_rx));

    let code = match &result {
        Ok(()) => ServiceExitCode::Win32(0),
        Err(_) => ServiceExitCode::ServiceSpecific(1),
    };
    set_state(ServiceState::Stopped, code);
    result
}

/// Services have no console — mirror tracing into `<data_dir>/service.log`
/// (append; simple single file, the engine's own logs are low-volume at info).
fn init_service_logging(data_dir: &Path) {
    let _ = std::fs::create_dir_all(data_dir);
    let path: PathBuf = data_dir.join("service.log");
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "info,zoomy=info".into()),
            )
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .try_init();
    }
}
