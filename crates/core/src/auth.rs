//! Remote-access auth: a single password gates non-loopback API access.
//! Loopback (the desktop app, local dev) is always exempt, so enabling a
//! password can never lock you out of the machine the NVR runs on.
//!
//! Storage: salted SHA-256 ("v1$salt$hash") in the settings KV table — fine
//! for a LAN appliance password; swap for argon2 before any WAN exposure.
//! Sessions are random in-memory tokens (cleared on restart and on password
//! change), delivered as an HttpOnly cookie.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use sha2::{Digest, Sha256};

use crate::api::AppState;

pub const KV_PASSWORD: &str = "password_hash";
const COOKIE_NAME: &str = "zoomy_session";

#[derive(Clone, Default)]
pub struct Sessions(Arc<Mutex<HashSet<String>>>);

impl Sessions {
    pub fn insert(&self, token: String) {
        self.0.lock().expect("sessions poisoned").insert(token);
    }
    pub fn contains(&self, token: &str) -> bool {
        self.0.lock().expect("sessions poisoned").contains(token)
    }
    pub fn clear(&self) {
        self.0.lock().expect("sessions poisoned").clear();
    }
}

pub fn hash_password(password: &str) -> String {
    let salt: [u8; 16] = rand::random();
    let salt_hex = hex(&salt);
    let digest = Sha256::new()
        .chain_update(salt)
        .chain_update(password.as_bytes())
        .finalize();
    format!("v1${salt_hex}${}", hex(&digest))
}

pub fn verify_password(stored: &str, password: &str) -> bool {
    let mut parts = stored.split('$');
    let (Some("v1"), Some(salt_hex), Some(hash_hex)) = (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let Some(salt) = unhex(salt_hex) else {
        return false;
    };
    let digest = Sha256::new()
        .chain_update(&salt)
        .chain_update(password.as_bytes())
        .finalize();
    // Length is fixed; comparison time leaks nothing useful on a LAN box.
    hex(&digest) == hash_hex
}

pub fn new_token() -> String {
    hex(&rand::random::<[u8; 32]>())
}

pub fn session_cookie(token: &str) -> String {
    format!("{COOKIE_NAME}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age=2592000")
}

fn request_token(req: &Request) -> Option<String> {
    let cookies = req.headers().get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|c| {
        c.trim()
            .strip_prefix(&format!("{COOKIE_NAME}="))
            .map(str::to_string)
    })
}

/// Gate `/api/*` for non-loopback peers when a password is set. Static assets
/// stay open so the login screen can render; login/health/auth-status stay
/// open so logging in is possible.
pub async fn middleware(
    State(st): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path();
    let exempt =
        !path.starts_with("/api") || matches!(path, "/api/login" | "/api/health" | "/api/auth");
    if exempt || addr.ip().is_loopback() || st.db.get_kv(KV_PASSWORD).is_none() {
        return next.run(req).await;
    }
    if let Some(token) = request_token(&req) {
        if st.sessions.contains(&token) {
            return next.run(req).await;
        }
    }
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "error": "login required" })),
    )
        .into_response()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify_roundtrip() {
        let stored = hash_password("hunter2");
        assert!(verify_password(&stored, "hunter2"));
        assert!(!verify_password(&stored, "hunter3"));
        assert!(!verify_password("garbage", "hunter2"));
        // Distinct salts per call.
        assert_ne!(stored, hash_password("hunter2"));
    }

    #[test]
    fn sessions_lifecycle() {
        let s = Sessions::default();
        let t = new_token();
        assert!(!s.contains(&t));
        s.insert(t.clone());
        assert!(s.contains(&t));
        s.clear();
        assert!(!s.contains(&t));
    }
}
