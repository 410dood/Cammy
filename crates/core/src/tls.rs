//! Native HTTPS support for off-LAN exposure.
//!
//! The server serves plain HTTP by default (the right choice on a trusted LAN
//! and behind a reverse proxy). When the operator points it at a cert+key it
//! serves HTTPS directly via rustls (pure Rust — same TLS backend reqwest
//! already links, so no OpenSSL and CI stays green on all three OSes).
//!
//! [`ensure_self_signed`] makes getting TLS a one-flag affair: it mints a
//! self-signed cert on first use and reuses it across runs, so a self-hoster
//! can flip on HTTPS without touching a CA. Browsers will warn on the
//! untrusted cert (expected for self-signed); for a clean padlock, front the
//! NVR with a real certificate or a reverse proxy.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Ensure a self-signed cert + key exist under `<dir>/tls`, generating them on
/// first use. Returns `(cert_path, key_path)`. The pair is reused across runs
/// so the certificate (and any browser exception granted to it) stays stable.
pub fn ensure_self_signed(dir: &Path) -> Result<(PathBuf, PathBuf)> {
    let tls_dir = dir.join("tls");
    std::fs::create_dir_all(&tls_dir).context("creating tls directory")?;
    // Lock the directory down *before* writing the key, so even the brief
    // moment the key file exists at default perms it isn't world-readable.
    restrict_dir(&tls_dir);
    let cert_path = tls_dir.join("cert.pem");
    let key_path = tls_dir.join("key.pem");
    if cert_path.exists() && key_path.exists() {
        return Ok((cert_path, key_path));
    }

    // SANs cover the common self-host access paths; access by raw LAN IP still
    // works (with a name-mismatch warning) since this is a self-signed cert.
    let sans = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    let certified =
        rcgen::generate_simple_self_signed(sans).context("generating self-signed certificate")?;
    std::fs::write(&cert_path, certified.cert.pem()).context("writing cert.pem")?;
    std::fs::write(&key_path, certified.key_pair.serialize_pem()).context("writing key.pem")?;
    restrict_key(&key_path);
    tracing::info!(cert = %cert_path.display(), "generated self-signed TLS certificate");
    Ok((cert_path, key_path))
}

/// Best-effort `0700` on the TLS directory (Unix). No-op elsewhere; on Windows
/// the per-user app-data / workspace dir is already user-scoped.
fn restrict_dir(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Best-effort `0600` on the private key (Unix). No-op elsewhere.
fn restrict_key(key: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(key, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = key;
}
