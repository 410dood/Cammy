//! Native Web Push (RFC 8030 delivery + RFC 8292 VAPID + RFC 8291 `aes128gcm`
//! payload encryption), built entirely on `ring` (already in-tree for TLS) — so
//! Cammy can push to subscribed browsers with **no third-party push service**
//! and no new dependency.
//!
//! The whole crypto stack is here:
//! - VAPID server identity: a persistent P-256 keypair; each request carries an
//!   ES256 JWT signed with it (`Authorization: vapid t=<jwt>, k=<pubkey>`).
//! - Message encryption: per-message ephemeral P-256 ECDH against the
//!   subscription's `p256dh` key, mixed with its `auth` secret through
//!   HKDF-SHA256, then AES-128-GCM in the RFC 8188 `aes128gcm` framing.
//!
//! The post-ECDH pipeline (`derive_keys` + `build_body`) is a pure function
//! unit-tested **byte-for-byte against the RFC 8291 Appendix A vector**.

use crate::db::Db;
use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const KV_VAPID_PRIV: &str = "vapid_private_pkcs8"; // base64 (standard) PKCS#8
const KV_VAPID_PUB: &str = "vapid_public"; // base64url uncompressed point (65 B)
/// Default VAPID `sub` contact (a push service may reject a missing/!mailto sub).
const VAPID_SUB: &str = "mailto:admin@cammy.local";
const RECORD_SIZE: u32 = 4096;

fn b64url(b: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(b)
}
fn b64url_d(s: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .context("invalid base64url")
}

/// A browser push subscription (the `endpoint` + the keys it handed us).
#[derive(Clone, Debug)]
pub struct PushSub {
    pub endpoint: String,
    pub p256dh: String, // base64url uncompressed point
    pub auth: String,   // base64url 16-byte secret
    /// P2.11 owning user account (NULL for anonymous/legacy subs = unrestricted).
    pub user_id: Option<i64>,
}

/// The server's persistent VAPID identity.
#[derive(Clone)]
pub struct VapidKeys {
    pkcs8: Vec<u8>,  // PKCS#8 DER private key
    public: Vec<u8>, // uncompressed point (65 bytes)
}

impl VapidKeys {
    /// The applicationServerKey the browser subscribes with (base64url).
    pub fn public_b64url(&self) -> String {
        b64url(&self.public)
    }
}

/// Outcome that callers care about: a `Gone` subscription should be deleted.
#[derive(Debug)]
pub enum SendError {
    /// 404/410 — the subscription is expired/unsubscribed; drop it.
    Gone,
    /// Any other failure (network, 4xx/5xx, encryption).
    Other(String),
}

/// HKDF-SHA256 for outputs ≤ 32 bytes (one expand block — all WebPush uses are
/// 16/12/32). Implemented over `ring::hmac` to keep the derivation explicit.
fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    use ring::hmac;
    debug_assert!(len <= 32);
    let prk = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, salt), ikm);
    let mut msg = info.to_vec();
    msg.push(0x01);
    let t = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, prk.as_ref()), &msg);
    t.as_ref()[..len].to_vec()
}

/// RFC 8291 §3.4 + RFC 8188 §2.1: derive the content-encryption key (16 B) and
/// nonce (12 B) from the ECDH shared secret, the two public keys, the `auth`
/// secret, and the per-message salt. Pure — the ECDH itself happens upstream.
fn derive_keys(
    ecdh_secret: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    auth: &[u8],
    salt: &[u8],
) -> (Vec<u8>, Vec<u8>) {
    // IKM = HKDF(salt=auth, ikm=ecdh, info="WebPush: info"||0||ua||as, 32)
    let mut key_info = b"WebPush: info\x00".to_vec();
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(as_public);
    let ikm = hkdf_sha256(auth, ecdh_secret, &key_info, 32);
    // CEK / NONCE keyed by the random salt over that IKM.
    let cek = hkdf_sha256(salt, &ikm, b"Content-Encoding: aes128gcm\x00", 16);
    let nonce = hkdf_sha256(salt, &ikm, b"Content-Encoding: nonce\x00", 12);
    (cek, nonce)
}

/// RFC 8188 `aes128gcm` framing: a single encrypted record with the sender's
/// public key as the keyid. `salt`/`cek`/`nonce` are derived per message.
fn build_body(
    plaintext: &[u8],
    as_public: &[u8],
    salt: &[u8],
    cek: &[u8],
    nonce: &[u8],
) -> Result<Vec<u8>> {
    use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_128_GCM};
    // A single record advertises rs = RECORD_SIZE, which (RFC 8188) must cover the
    // content + 0x02 delimiter + 16-byte GCM tag. Guard so we never emit a record
    // larger than we advertise (a receiver would reject it).
    if plaintext.len() + 1 + 16 > RECORD_SIZE as usize {
        return Err(anyhow!("push payload too large for a single record"));
    }
    // Record content: plaintext || 0x02 (last-record delimiter), no zero padding.
    let mut content = plaintext.to_vec();
    content.push(0x02);
    let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, cek).map_err(|_| anyhow!("bad CEK"))?);
    let nonce_arr: [u8; 12] = nonce.try_into().map_err(|_| anyhow!("bad nonce len"))?;
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce_arr),
        Aad::empty(),
        &mut content,
    )
    .map_err(|_| anyhow!("AES-GCM seal failed"))?;

    // header: salt(16) | rs(4, big-endian) | idlen(1) | keyid(as_public) | ciphertext
    let mut body = Vec::with_capacity(16 + 4 + 1 + as_public.len() + content.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(as_public.len() as u8);
    body.extend_from_slice(as_public);
    body.extend_from_slice(&content);
    Ok(body)
}

/// Encrypt `plaintext` for a subscription's keys, generating a fresh ephemeral
/// keypair + salt (the production path). Returns the `aes128gcm` body.
fn encrypt(plaintext: &[u8], ua_public_b64: &str, auth_b64: &str) -> Result<Vec<u8>> {
    use ring::agreement::{agree_ephemeral, EphemeralPrivateKey, UnparsedPublicKey, ECDH_P256};
    use ring::rand::SecureRandom;

    let ua_public = b64url_d(ua_public_b64).context("p256dh")?;
    let auth = b64url_d(auth_b64).context("auth")?;
    if ua_public.len() != 65 || auth.len() != 16 {
        return Err(anyhow!("malformed subscription keys"));
    }
    let rng = SystemRandom::new();
    let eph =
        EphemeralPrivateKey::generate(&ECDH_P256, &rng).map_err(|_| anyhow!("ephemeral keygen"))?;
    let as_public = eph
        .compute_public_key()
        .map_err(|_| anyhow!("ephemeral pubkey"))?
        .as_ref()
        .to_vec();
    let mut salt = [0u8; 16];
    rng.fill(&mut salt).map_err(|_| anyhow!("salt rng"))?;

    let peer = UnparsedPublicKey::new(&ECDH_P256, &ua_public);
    let ecdh_secret = agree_ephemeral(eph, &peer, |s| s.to_vec())
        .map_err(|_| anyhow!("ECDH agreement failed"))?;

    let (cek, nonce) = derive_keys(&ecdh_secret, &ua_public, &as_public, &auth, &salt);
    build_body(plaintext, &as_public, &salt, &cek, &nonce)
}

/// The origin (`scheme://host[:port]`) of a push endpoint — the VAPID `aud`.
fn endpoint_origin(endpoint: &str) -> Result<String> {
    let scheme_end = endpoint.find("://").context("endpoint has no scheme")?;
    let rest = &endpoint[scheme_end + 3..];
    let authority = rest.split('/').next().unwrap_or(rest);
    if authority.is_empty() {
        return Err(anyhow!("endpoint has no host"));
    }
    Ok(format!("{}{}", &endpoint[..scheme_end + 3], authority))
}

/// Build the RFC 8292 `Authorization: vapid …` header value for an endpoint.
fn vapid_header(keys: &VapidKeys, endpoint: &str) -> Result<String> {
    let aud = endpoint_origin(endpoint)?;
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
        + 12 * 3600; // 12h (RFC 8292 caps at 24h)
    let header = b64url(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = b64url(format!(r#"{{"aud":"{aud}","exp":{exp},"sub":"{VAPID_SUB}"}}"#).as_bytes());
    let signing_input = format!("{header}.{claims}");

    let rng = SystemRandom::new();
    let kp = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &keys.pkcs8, &rng)
        .map_err(|_| anyhow!("load VAPID key"))?;
    let sig = kp
        .sign(&rng, signing_input.as_bytes())
        .map_err(|_| anyhow!("VAPID sign"))?;
    let jwt = format!("{signing_input}.{}", b64url(sig.as_ref()));
    Ok(format!("vapid t={jwt}, k={}", keys.public_b64url()))
}

/// Is this IP one we must never make a server-side request to (SSRF guard)?
fn is_private_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.octets()[0] == 0 // 0.0.0.0/8
        }
        IpAddr::V6(v6) => {
            // IPv4-mapped (::ffff:a.b.c.d) must be re-checked as its v4 address.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_ip(IpAddr::V4(v4));
            }
            let seg0 = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (seg0 & 0xfe00) == 0xfc00 // fc00::/7 unique-local
                || (seg0 & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

/// SSRF guard: the endpoint is attacker-influenced (any authenticated user can
/// subscribe), and the worker/test then makes a *server-side* request to it.
/// Require https and refuse hosts that point at the server's own network.
pub fn validate_endpoint(endpoint: &str) -> Result<()> {
    if endpoint.len() > 2048 {
        return Err(anyhow!("push endpoint too long"));
    }
    let rest = endpoint
        .strip_prefix("https://")
        .ok_or_else(|| anyhow!("push endpoint must be https"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    // Strip any userinfo + port, and IPv6 brackets, to isolate the host.
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(stripped) = host.strip_prefix('[') {
        stripped.split(']').next().unwrap_or(stripped) // [::1]:443 -> ::1
    } else {
        host.split(':').next().unwrap_or(host)
    };
    if host.is_empty() {
        return Err(anyhow!("push endpoint has no host"));
    }
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") || lower.ends_with(".local") {
        return Err(anyhow!("push endpoint host not allowed"));
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if is_private_ip(ip) {
            return Err(anyhow!("push endpoint points at a private address"));
        }
    }
    Ok(())
}

/// Validate a full subscription at registration time: a safe endpoint plus
/// well-formed keys (a 65-byte uncompressed point + 16-byte auth secret), so
/// malformed keys are rejected up front rather than failing forever in the worker.
pub fn validate_subscription(endpoint: &str, p256dh: &str, auth: &str) -> Result<()> {
    validate_endpoint(endpoint)?;
    let p = b64url_d(p256dh).context("p256dh")?;
    let a = b64url_d(auth).context("auth")?;
    if p.len() != 65 || p[0] != 0x04 {
        return Err(anyhow!("p256dh must be a 65-byte uncompressed point"));
    }
    if a.len() != 16 {
        return Err(anyhow!("auth must be a 16-byte secret"));
    }
    Ok(())
}

/// Send one encrypted push. Network/crypto errors are returned; a `Gone` result
/// signals the caller to delete the (expired) subscription. Re-validates the
/// endpoint (defense in depth) and never follows redirects (an SSRF bounce).
pub fn send(keys: &VapidKeys, sub: &PushSub, payload: &[u8], ttl: u32) -> Result<(), SendError> {
    validate_endpoint(&sub.endpoint).map_err(|e| SendError::Other(e.to_string()))?;
    let body =
        encrypt(payload, &sub.p256dh, &sub.auth).map_err(|e| SendError::Other(e.to_string()))?;
    let auth = vapid_header(keys, &sub.endpoint).map_err(|e| SendError::Other(e.to_string()))?;
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(8))
        .build();
    let resp = agent
        .post(&sub.endpoint)
        .set("Authorization", &auth)
        .set("Content-Encoding", "aes128gcm")
        .set("Content-Type", "application/octet-stream")
        .set("TTL", &ttl.to_string())
        .set("Urgency", "normal")
        .send_bytes(&body);
    match resp {
        Ok(r) if (200..300).contains(&r.status()) => Ok(()),
        // A 3xx (redirects disabled) or other status: not delivered, not Gone.
        Ok(r) => Err(SendError::Other(format!(
            "unexpected status {}",
            r.status()
        ))),
        Err(ureq::Error::Status(404 | 410, _)) => Err(SendError::Gone),
        Err(e) => Err(SendError::Other(e.to_string())),
    }
}

/// Load the persistent VAPID keypair, generating + storing it on first use.
/// Call once at startup so the public key handed to browsers is stable.
pub fn vapid_keys(db: &Db) -> Result<VapidKeys> {
    use base64::engine::general_purpose::STANDARD;
    if let (Some(priv_b64), Some(pub_b64)) = (db.get_kv(KV_VAPID_PRIV), db.get_kv(KV_VAPID_PUB)) {
        if let (Ok(pkcs8), Ok(public)) = (STANDARD.decode(&priv_b64), b64url_d(&pub_b64)) {
            return Ok(VapidKeys { pkcs8, public });
        }
    }
    // Generate a fresh P-256 keypair and persist it.
    let rng = SystemRandom::new();
    let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
        .map_err(|_| anyhow!("VAPID keygen"))?;
    let pkcs8 = doc.as_ref().to_vec();
    let kp = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &pkcs8, &rng)
        .map_err(|_| anyhow!("VAPID reload"))?;
    let public = kp.public_key().as_ref().to_vec();
    db.set_kv(KV_VAPID_PRIV, &STANDARD.encode(&pkcs8))?;
    db.set_kv(KV_VAPID_PUB, &b64url(&public))?;
    Ok(VapidKeys { pkcs8, public })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Vec<u8> {
        b64url_d(s).unwrap()
    }

    // RFC 8291 Appendix A — the canonical worked example. Validates the entire
    // post-ECDH pipeline (HKDF derivation + AES-128-GCM + RFC 8188 framing)
    // byte-for-byte. The ECDH itself is ring's trusted ECDH_P256.
    #[test]
    fn rfc8291_appendix_a_vector() {
        let ecdh = d("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs");
        let ua = d("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4");
        let as_ = d("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8");
        let auth = d("BTBZMqHH6r4Tts7J_aSIgg");
        let salt = d("DGv6ra1nlYgDCS1FRnbzlw");

        let (cek, nonce) = derive_keys(&ecdh, &ua, &as_, &auth, &salt);
        assert_eq!(cek, d("oIhVW04MRdy2XN9CiKLxTg"), "CEK mismatch");
        assert_eq!(nonce, d("4h_95klXJ5E_qnoN"), "NONCE mismatch");

        let body = build_body(
            b"When I grow up, I want to be a watermelon",
            &as_,
            &salt,
            &cek,
            &nonce,
        )
        .unwrap();
        let expected = d("DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN");
        assert_eq!(body, expected, "encrypted body mismatch");
    }

    #[test]
    fn vapid_jwt_is_well_formed_and_verifies() {
        use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_FIXED};
        // Generate a throwaway keypair (no DB needed).
        let rng = SystemRandom::new();
        let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let pkcs8 = doc.as_ref().to_vec();
        let kp = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &pkcs8, &rng).unwrap();
        let keys = VapidKeys {
            pkcs8,
            public: kp.public_key().as_ref().to_vec(),
        };
        let hdr = vapid_header(&keys, "https://fcm.googleapis.com/fcm/send/abc123").unwrap();
        // Shape: "vapid t=<jwt>, k=<pubkey>"
        let t = hdr.strip_prefix("vapid t=").unwrap();
        let (jwt, k) = t.split_once(", k=").unwrap();
        assert_eq!(b64url_d(k).unwrap(), keys.public);
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        // Claims carry the endpoint ORIGIN as aud (no path).
        let claims = String::from_utf8(b64url_d(parts[1]).unwrap()).unwrap();
        assert!(
            claims.contains(r#""aud":"https://fcm.googleapis.com""#),
            "{claims}"
        );
        // The signature verifies against the public key over header.claims.
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let sig = b64url_d(parts[2]).unwrap();
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, &keys.public)
            .verify(signing_input.as_bytes(), &sig)
            .expect("VAPID signature should verify");
    }

    #[test]
    fn encrypt_produces_parseable_framing() {
        // End-to-end production path (real ephemeral ECDH); we can't decrypt
        // without the UA private key, but the framing must be well-formed.
        let ua = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
        let auth = "BTBZMqHH6r4Tts7J_aSIgg";
        let body = encrypt(b"hello", ua, auth).unwrap();
        assert!(body.len() > 16 + 4 + 1 + 65 + 16);
        assert_eq!(body[20], 65, "idlen should be 65 (uncompressed point)");
        assert_eq!(&body[..16].len(), &16); // salt present
                                            // rs field = 4096.
        assert_eq!(&body[16..20], &4096u32.to_be_bytes());
    }

    #[test]
    fn endpoint_origin_strips_path() {
        assert_eq!(
            endpoint_origin("https://updates.push.services.mozilla.com/wpush/v2/abc").unwrap(),
            "https://updates.push.services.mozilla.com"
        );
        assert!(endpoint_origin("not-a-url").is_err());
    }

    #[test]
    fn validate_endpoint_blocks_ssrf() {
        // Real push services pass.
        assert!(validate_endpoint("https://fcm.googleapis.com/fcm/send/abc").is_ok());
        assert!(validate_endpoint("https://updates.push.services.mozilla.com/wpush/v2/x").is_ok());
        // http and non-https are refused.
        assert!(validate_endpoint("http://fcm.googleapis.com/x").is_err());
        assert!(validate_endpoint("ftp://x/y").is_err());
        // Private / loopback / link-local / metadata hosts are refused.
        assert!(validate_endpoint("https://127.0.0.1/x").is_err());
        assert!(validate_endpoint("https://localhost/x").is_err());
        assert!(validate_endpoint("https://192.168.1.10:443/x").is_err());
        assert!(validate_endpoint("https://10.0.0.5/x").is_err());
        assert!(validate_endpoint("https://169.254.169.254/latest/meta-data").is_err()); // cloud metadata
        assert!(validate_endpoint("https://[::1]/x").is_err());
        assert!(validate_endpoint("https://[::ffff:192.168.0.1]/x").is_err()); // v4-mapped
        assert!(validate_endpoint("https://router.local/x").is_err());
    }

    #[test]
    fn validate_subscription_checks_keys() {
        let ep = "https://fcm.googleapis.com/fcm/send/abc";
        let ua = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
        let auth = "BTBZMqHH6r4Tts7J_aSIgg";
        assert!(validate_subscription(ep, ua, auth).is_ok());
        assert!(validate_subscription(ep, "tooshort", auth).is_err()); // bad p256dh
        assert!(validate_subscription(ep, ua, "AAAA").is_err()); // bad auth len
        assert!(validate_subscription("https://127.0.0.1/x", ua, auth).is_err());
        // bad endpoint
    }
}
