//! Remote-access auth: a single password gates non-loopback API access.
//! Loopback (the desktop app, local dev) is always exempt, so enabling a
//! password can never lock you out of the machine the NVR runs on.
//!
//! Storage: argon2id PHC strings (`$argon2id$v=19$...`) in the settings KV
//! table — memory-hard hashing fit for WAN exposure. Legacy salted-SHA-256
//! hashes (`v1$salt$hash`) written by older builds still verify, and are
//! transparently upgraded to argon2id on the next successful login.
//! Sessions are random in-memory tokens (cleared on restart and on password
//! change), delivered as an HttpOnly cookie (`Secure` too, when serving HTTPS).
//!
//! Off-LAN brute-force is blunted by a per-IP login throttle ([`LoginThrottle`]):
//! after [`MAX_FAILURES`] wrong passwords inside [`FAILURE_WINDOW`], that peer is
//! locked out for [`LOCKOUT`]. Loopback is never throttled.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use sha2::{Digest, Sha256};

use crate::api::AppState;

pub const KV_PASSWORD: &str = "password_hash";
const COOKIE_NAME: &str = "zoomy_session";

/// Wrong passwords from one IP inside [`FAILURE_WINDOW`] before lockout.
pub const MAX_FAILURES: u32 = 8;
/// Window over which failures accumulate toward [`MAX_FAILURES`].
pub const FAILURE_WINDOW: Duration = Duration::from_secs(300);
/// How long a peer stays locked out once it trips the limit.
pub const LOCKOUT: Duration = Duration::from_secs(300);
/// Hard cap on tracked source IPs, so a botnet rotating addresses (trivial over
/// an IPv6 /64) can't grow the throttle map without bound. Stale entries are
/// swept first; once genuinely full of *active* offenders, new IPs simply go
/// untracked (existing lockouts still hold) rather than exhausting memory.
const MAX_TRACKED_IPS: usize = 4096;

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

/// Per-IP failed-login accounting for brute-force lockout.
#[derive(Default)]
struct Attempt {
    count: u32,
    window_start: Option<Instant>,
    locked_until: Option<Instant>,
}

/// Per-peer login brute-force throttle. Cheap in-memory map; cleared on restart.
#[derive(Clone, Default)]
pub struct LoginThrottle(Arc<Mutex<HashMap<IpAddr, Attempt>>>);

impl LoginThrottle {
    /// If this peer is currently locked out, the remaining lockout duration.
    /// Loopback is never locked (the local box must always be able to log in).
    pub fn locked_for(&self, ip: IpAddr) -> Option<Duration> {
        if ip.is_loopback() {
            return None;
        }
        let mut map = self.0.lock().expect("throttle poisoned");
        let a = map.get_mut(&ip)?;
        let until = a.locked_until?;
        let now = Instant::now();
        if until > now {
            Some(until - now)
        } else {
            // Lockout served — reset so the peer gets a clean slate.
            map.remove(&ip);
            None
        }
    }

    /// Record a wrong-password attempt, tripping a lockout at the limit.
    pub fn record_failure(&self, ip: IpAddr) {
        if ip.is_loopback() {
            return;
        }
        let now = Instant::now();
        let mut map = self.0.lock().expect("throttle poisoned");
        // Sweep entries that are neither currently locked nor in a fresh
        // failure window — this keeps the map bounded under IP rotation.
        map.retain(|_, a| {
            let locked = matches!(a.locked_until, Some(u) if u > now);
            let fresh =
                matches!(a.window_start, Some(s) if now.duration_since(s) <= FAILURE_WINDOW);
            locked || fresh
        });
        // Hard cap: don't grow for a brand-new IP once full of live offenders.
        if !map.contains_key(&ip) && map.len() >= MAX_TRACKED_IPS {
            return;
        }
        let a = map.entry(ip).or_default();
        // Already locked? Leave the existing deadline alone (don't let attempts
        // arriving during a lockout perpetually extend it).
        if matches!(a.locked_until, Some(u) if u > now) {
            return;
        }
        let fresh_window =
            matches!(a.window_start, Some(s) if now.duration_since(s) <= FAILURE_WINDOW);
        if !fresh_window {
            a.window_start = Some(now);
            a.count = 0;
        }
        a.count += 1;
        if a.count >= MAX_FAILURES {
            a.locked_until = Some(now + LOCKOUT);
        }
    }

    /// A successful login clears the peer's failure history.
    pub fn record_success(&self, ip: IpAddr) {
        self.0.lock().expect("throttle poisoned").remove(&ip);
    }
}

/// Hash a password with argon2id (memory-hard), producing a self-describing
/// PHC string that carries its own salt and parameters.
pub fn hash_password(password: &str) -> String {
    // 16 random salt bytes from the same RNG the rest of the crate uses, so we
    // don't need argon2's optional getrandom-backed OsRng feature.
    let salt_bytes: [u8; 16] = rand::random();
    let salt = SaltString::encode_b64(&salt_bytes).expect("encoding salt");
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hashing")
        .to_string()
}

/// Verify against either an argon2id PHC string or a legacy `v1$salt$hash`
/// SHA-256 record (so passwords set by older builds keep working).
pub fn verify_password(stored: &str, password: &str) -> bool {
    if stored.starts_with("$argon2") {
        match PasswordHash::new(stored) {
            Ok(parsed) => Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok(),
            Err(_) => false,
        }
    } else {
        verify_legacy_sha256(stored, password)
    }
}

/// True if `stored` is an old-format hash that should be re-hashed with
/// argon2id after a successful login (upgrade-on-login).
pub fn needs_rehash(stored: &str) -> bool {
    !stored.starts_with("$argon2")
}

fn verify_legacy_sha256(stored: &str, password: &str) -> bool {
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
    hex(&digest) == hash_hex
}

pub fn new_token() -> String {
    hex(&rand::random::<[u8; 32]>())
}

/// Session cookie; `secure` adds the `Secure` attribute (set it when serving
/// over HTTPS so the token is never sent in clear).
pub fn session_cookie(token: &str, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{COOKIE_NAME}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age=2592000{secure}")
}

/// Effective client identity for auth + throttle decisions, and whether the
/// request reached us *through* the trusted proxy.
///
/// Default (no proxy): the TCP peer address. Behind a configured trusted proxy
/// we believe the right-most `X-Forwarded-For` hop — the address the proxy
/// itself saw — and flag the request as `via_proxy`. A request that arrives
/// without that header in proxy mode is a *direct* hit (something bypassing the
/// proxy) and is judged on its transport peer.
///
/// Callers must NOT grant the loopback exemption to a `via_proxy` request: that
/// is what stops a same-host proxy (connecting over 127.0.0.1) from bypassing
/// the password, and stops a forged `X-Forwarded-For: 127.0.0.1` on a
/// proxy-bypassing connection from doing the same.
pub fn client_ip(headers: &header::HeaderMap, peer: IpAddr, behind_proxy: bool) -> (IpAddr, bool) {
    if behind_proxy {
        if let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|xff| xff.rsplit(',').next())
            .and_then(|last| last.trim().parse::<IpAddr>().ok())
        {
            return (ip, true);
        }
    }
    (peer, false)
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
    // A proxied request is never granted the loopback exemption (the proxy's
    // own 127.0.0.1 connection, or a spoofed XFF, must not bypass the password).
    let (cip, via_proxy) = client_ip(req.headers(), addr.ip(), st.behind_proxy);
    let loopback_exempt = !via_proxy && cip.is_loopback();
    if exempt || loopback_exempt || st.db.get_kv(KV_PASSWORD).is_none() {
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
    use std::net::Ipv4Addr;

    #[test]
    fn argon2_hash_and_verify_roundtrip() {
        let stored = hash_password("hunter2");
        assert!(stored.starts_with("$argon2id$"), "got {stored}");
        assert!(verify_password(&stored, "hunter2"));
        assert!(!verify_password(&stored, "hunter3"));
        assert!(!verify_password("garbage", "hunter2"));
        // Distinct salts per call.
        assert_ne!(stored, hash_password("hunter2"));
        // A fresh argon2 hash never needs upgrading.
        assert!(!needs_rehash(&stored));
    }

    #[test]
    fn legacy_sha256_still_verifies_and_wants_upgrade() {
        // A hash in the old salted-SHA-256 format (as older builds stored).
        let salt: [u8; 16] = [7; 16];
        let salt_hex = hex(&salt);
        let digest = Sha256::new()
            .chain_update(salt)
            .chain_update(b"hunter2")
            .finalize();
        let legacy = format!("v1${salt_hex}${}", hex(&digest));
        assert!(verify_password(&legacy, "hunter2"));
        assert!(!verify_password(&legacy, "wrong"));
        // Legacy hashes are flagged for upgrade-on-login.
        assert!(needs_rehash(&legacy));
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

    #[test]
    fn cookie_secure_flag() {
        assert!(!session_cookie("abc", false).contains("Secure"));
        assert!(session_cookie("abc", true).contains("; Secure"));
        assert!(session_cookie("abc", true).contains("HttpOnly"));
    }

    #[test]
    fn throttle_locks_out_after_repeated_failures() {
        let t = LoginThrottle::default();
        let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));
        assert!(t.locked_for(ip).is_none());
        for _ in 0..MAX_FAILURES {
            assert!(t.locked_for(ip).is_none(), "locked too early");
            t.record_failure(ip);
        }
        assert!(t.locked_for(ip).is_some(), "should be locked after limit");
        // A success clears the slate.
        t.record_success(ip);
        assert!(t.locked_for(ip).is_none());
    }

    #[test]
    fn client_ip_proxy_resolution() {
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST); // proxy connects over loopback
        let real = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9));

        // No proxy mode: always the transport peer, never via_proxy.
        let mut h = header::HeaderMap::new();
        h.insert("x-forwarded-for", "198.51.100.9".parse().unwrap());
        assert_eq!(client_ip(&h, peer, false), (peer, false));

        // Proxy mode with XFF: trust the right-most hop, flagged via_proxy.
        assert_eq!(client_ip(&h, peer, true), (real, true));

        // Multi-hop XFF: right-most (closest trusted proxy) wins.
        let mut multi = header::HeaderMap::new();
        multi.insert("x-forwarded-for", "10.0.0.1, 198.51.100.9".parse().unwrap());
        assert_eq!(client_ip(&multi, peer, true), (real, true));

        // Proxy mode, no XFF (direct hit bypassing the proxy): transport peer,
        // NOT via_proxy — so a genuine local request still earns the exemption.
        let empty = header::HeaderMap::new();
        assert_eq!(client_ip(&empty, peer, true), (peer, false));

        // Forged loopback XFF is reported with via_proxy=true, so the caller's
        // `!via_proxy && is_loopback` exemption check correctly refuses it.
        let mut spoof = header::HeaderMap::new();
        spoof.insert("x-forwarded-for", "127.0.0.1".parse().unwrap());
        let (ip, via) = client_ip(&spoof, real, true);
        assert!(ip.is_loopback() && via, "spoof must be flagged via_proxy");
    }

    #[test]
    fn throttle_never_locks_loopback() {
        let t = LoginThrottle::default();
        let lo = IpAddr::V4(Ipv4Addr::LOCALHOST);
        for _ in 0..(MAX_FAILURES * 3) {
            t.record_failure(lo);
        }
        assert!(t.locked_for(lo).is_none(), "loopback must never lock out");
    }
}
