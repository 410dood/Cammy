//! TOTP two-factor authentication (RFC 6238) — a second login factor on top of
//! the password, for WAN exposure. Pure-Rust and dependency-light: HMAC-SHA1 is
//! built from the `sha1` crate we already pull, and base32 (RFC 4648) is
//! hand-rolled, so 2FA adds no new dependency.
//!
//! The secret is a 160-bit random value stored base32-encoded (the form every
//! authenticator app — Google Authenticator, Aegis, 1Password, … — accepts). A
//! 6-digit code rolls every 30 s; verification accepts a ±1 step window to
//! tolerate clock skew between the server and the phone. Single-use **recovery
//! codes** (stored only as SHA-256 hashes) are the escape hatch when the
//! authenticator is lost.
//!
//! Online brute force of the 6-digit code is blunted by the same per-IP login
//! throttle that guards the password (a wrong code is a login failure), so the
//! ~3-in-10^6 per-guess odds never get enough attempts to matter.

use sha1::{Digest, Sha1};

/// Code length. 6 digits is the universal authenticator-app default.
pub const DIGITS: u32 = 6;
/// Time step in seconds (RFC 6238 default).
pub const STEP: u64 = 30;
/// Accepted clock-skew window, in steps, on either side of the current step.
pub const SKEW: u64 = 1;
/// How many one-time recovery codes to mint at enrollment.
pub const RECOVERY_COUNT: usize = 10;

const B32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

// --- base32 (RFC 4648, no padding) ------------------------------------------

/// Encode bytes to upper-case base32 without padding (authenticator apps don't
/// require the `=` padding).
pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let mut buf: u64 = 0;
    let mut bits: u32 = 0;
    for &b in data {
        buf = (buf << 8) | b as u64;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(B32_ALPHABET[((buf >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(B32_ALPHABET[((buf << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

/// Decode base32, tolerating lower case, spaces, dashes, and `=` padding (so a
/// user can paste a grouped secret). Returns `None` on any non-alphabet char.
pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut buf: u64 = 0;
    let mut bits: u32 = 0;
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    for c in s.chars() {
        if matches!(c, '=' | ' ' | '-' | '\t' | '\n' | '\r') {
            continue;
        }
        let cu = c.to_ascii_uppercase();
        let v: u64 = match cu {
            'A'..='Z' => (cu as u8 - b'A') as u64,
            '2'..='7' => (cu as u8 - b'2' + 26) as u64,
            _ => return None,
        };
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

// --- HMAC-SHA1 / HOTP / TOTP ------------------------------------------------

const SHA1_BLOCK: usize = 64;

fn hmac_sha1(key: &[u8], msg: &[u8]) -> [u8; 20] {
    let mut k = [0u8; SHA1_BLOCK];
    if key.len() > SHA1_BLOCK {
        k[..20].copy_from_slice(&Sha1::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; SHA1_BLOCK];
    let mut opad = [0x5cu8; SHA1_BLOCK];
    for i in 0..SHA1_BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha1::new();
    inner.update(ipad);
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha1::new();
    outer.update(opad);
    outer.update(inner);
    let mut res = [0u8; 20];
    res.copy_from_slice(&outer.finalize());
    res
}

/// RFC 4226 HOTP: an `digits`-wide code for `secret` at `counter`.
fn hotp(secret: &[u8], counter: u64, digits: u32) -> u32 {
    let hs = hmac_sha1(secret, &counter.to_be_bytes());
    let offset = (hs[19] & 0x0f) as usize;
    let bin = ((hs[offset] as u32 & 0x7f) << 24)
        | ((hs[offset + 1] as u32) << 16)
        | ((hs[offset + 2] as u32) << 8)
        | (hs[offset + 3] as u32);
    bin % 10u32.pow(digits)
}

/// The zero-padded TOTP code for `secret` (raw bytes) at unix time `unix`.
/// Used by tests and as the reference for [`matched_step`]'s candidate codes.
#[cfg(test)]
fn totp_code_at(secret: &[u8], unix: u64) -> String {
    let code = hotp(secret, unix / STEP, DIGITS);
    format!("{code:0width$}", width = DIGITS as usize)
}

/// Verify a user-supplied `code` against the base32 `secret` at unix time
/// `unix`, returning the time-step that matched (within a ±[`SKEW`] window), or
/// `None`. The matched step lets the caller block intra-window **replay** by
/// refusing a step at or below the last one it accepted. Constant-time on the
/// digit comparison so a near-miss can't be timed.
pub fn matched_step(secret_b32: &str, code: &str, unix: u64) -> Option<u64> {
    let code = code.trim();
    if code.len() != DIGITS as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let secret = base32_decode(secret_b32)?;
    if secret.is_empty() {
        return None;
    }
    let counter = unix / STEP;
    let lo = counter.saturating_sub(SKEW);
    let mut found = None;
    // Walk the whole window (no early return) so timing doesn't reveal which
    // step matched. Only one step's code can equal a given input in practice.
    for c in lo..=counter.saturating_add(SKEW) {
        let cand = format!(
            "{:0width$}",
            hotp(&secret, c, DIGITS),
            width = DIGITS as usize
        );
        if ct_eq(cand.as_bytes(), code.as_bytes()) {
            found = Some(c);
        }
    }
    found
}

/// Whether `code` verifies against `secret` at `unix` (ignoring replay; use
/// [`matched_step`] when you need the step for replay protection).
pub fn verify(secret_b32: &str, code: &str, unix: u64) -> bool {
    matched_step(secret_b32, code, unix).is_some()
}

// --- secrets, provisioning, recovery codes ----------------------------------

/// Mint a fresh 160-bit base32 secret.
pub fn generate_secret() -> String {
    let bytes: [u8; 20] = rand::random();
    base32_encode(&bytes)
}

/// Build the `otpauth://` provisioning URI an authenticator app scans/imports.
pub fn provisioning_uri(secret_b32: &str, issuer: &str, account: &str) -> String {
    // Label is `Issuer:account`; both also appear as the `issuer=` param.
    format!(
        "otpauth://totp/{issuer_e}:{account_e}?secret={secret_b32}&issuer={issuer_e}&algorithm=SHA1&digits={DIGITS}&period={STEP}",
        issuer_e = pct(issuer),
        account_e = pct(account),
    )
}

/// Mint [`RECOVERY_COUNT`] human-friendly one-time codes ("xxxx-xxxx", base32).
/// Returns the plaintext (shown to the user once); store only [`hash_recovery`].
pub fn generate_recovery_codes() -> Vec<String> {
    (0..RECOVERY_COUNT)
        .map(|_| {
            let b: [u8; 5] = rand::random(); // 40 bits -> exactly 8 base32 chars
            let s = base32_encode(&b).to_lowercase();
            format!("{}-{}", &s[..4], &s[4..])
        })
        .collect()
}

/// Canonical form of a recovery code for hashing/compare: alphanumerics only,
/// lower-cased (so dashes/spaces/case in user input don't matter).
pub fn normalize_recovery(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// SHA-256 hex of a normalized recovery code — what we persist (never the code).
pub fn hash_recovery(code: &str) -> String {
    use sha2::Sha256;
    let d = Sha256::digest(normalize_recovery(code).as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Percent-encode for the otpauth URI label/issuer (RFC 3986 unreserved kept).
fn pct(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                o.push(b as char)
            }
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

/// Constant-time equality for equal-length byte slices.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_rfc4648_vectors() {
        // RFC 4648 §10 test vectors, in our no-padding form.
        let cases = [
            (&b""[..], ""),
            (b"f", "MY"),
            (b"fo", "MZXQ"),
            (b"foo", "MZXW6"),
            (b"foob", "MZXW6YQ"),
            (b"fooba", "MZXW6YTB"),
            (b"foobar", "MZXW6YTBOI"),
        ];
        for (raw, enc) in cases {
            assert_eq!(base32_encode(raw), enc, "encode {raw:?}");
            assert_eq!(base32_decode(enc).as_deref(), Some(raw), "decode {enc}");
        }
        // Tolerate lower case, padding, spaces, and grouping dashes.
        assert_eq!(
            base32_decode("mz xw-6yt=boi").as_deref(),
            Some(&b"foobar"[..])
        );
        // Reject a non-alphabet char (0, 1, 8, 9 aren't in base32).
        assert!(base32_decode("MZXW0").is_none());
    }

    #[test]
    fn hotp_rfc4226_vectors() {
        // RFC 4226 Appendix D: secret "12345678901234567890", counters 0..9.
        let secret = b"12345678901234567890";
        let expected = [
            755224, 287082, 359152, 969429, 338314, 254676, 287922, 162583, 399871, 520489,
        ];
        for (c, want) in expected.iter().enumerate() {
            assert_eq!(hotp(secret, c as u64, 6), *want, "counter {c}");
        }
    }

    #[test]
    fn totp_rfc6238_vectors_sha1() {
        // RFC 6238 Appendix B (SHA-1 row), reduced to our 6-digit truncation.
        let secret = b"12345678901234567890";
        let cases = [
            (59u64, "287082"),
            (1111111109, "081804"),
            (1111111111, "050471"),
            (1234567890, "005924"),
            (2000000000, "279037"),
        ];
        for (t, want) in cases {
            assert_eq!(totp_code_at(secret, t), want, "T={t}");
        }
    }

    #[test]
    fn verify_accepts_skew_window_and_rejects_outside() {
        let secret = generate_secret();
        let raw = base32_decode(&secret).unwrap();
        let now = 1_700_000_000u64;
        // Current, one step back, one step forward all verify.
        for dt in [0i64, -(STEP as i64), STEP as i64] {
            let t = (now as i64 + dt) as u64;
            let code = format!(
                "{:0width$}",
                hotp(&raw, t / STEP, DIGITS),
                width = DIGITS as usize
            );
            assert!(verify(&secret, &code, now), "dt={dt}");
        }
        // Two steps away is outside the window.
        let far = format!(
            "{:0width$}",
            hotp(&raw, (now + 2 * STEP) / STEP, DIGITS),
            width = DIGITS as usize
        );
        assert!(!verify(&secret, &far, now));
        // Wrong-length / non-digit / non-base32-secret all fail.
        assert!(!verify(&secret, "12345", now));
        assert!(!verify(&secret, "abcdef", now));
        assert!(!verify(&secret, "1234567", now));
        assert!(!verify("not base32 !!", "123456", now));
    }

    #[test]
    fn recovery_codes_are_distinct_hash_stable_and_normalize() {
        let codes = generate_recovery_codes();
        assert_eq!(codes.len(), RECOVERY_COUNT);
        // Format "xxxx-xxxx", lower-case base32.
        for c in &codes {
            assert_eq!(c.len(), 9, "code {c}");
            assert_eq!(&c[4..5], "-");
        }
        let set: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(set.len(), codes.len(), "codes must be unique");
        // Hash ignores case/dashes/spaces; same code -> same hash.
        let c = &codes[0];
        assert_eq!(hash_recovery(c), hash_recovery(&c.to_uppercase()));
        assert_eq!(hash_recovery(c), hash_recovery(&c.replace('-', " ")));
        assert_ne!(hash_recovery(&codes[0]), hash_recovery(&codes[1]));
        assert_eq!(hash_recovery(c).len(), 64); // sha256 hex
        assert_eq!(normalize_recovery("AB cd-Ef"), "abcdef");
    }

    #[test]
    fn provisioning_uri_is_well_formed() {
        let uri = provisioning_uri("JBSWY3DPEHPK3PXP", "Cammy", "alice");
        assert!(uri.starts_with("otpauth://totp/Cammy:alice?"));
        assert!(uri.contains("secret=JBSWY3DPEHPK3PXP"));
        assert!(uri.contains("issuer=Cammy"));
        assert!(uri.contains("digits=6"));
        assert!(uri.contains("period=30"));
        assert!(uri.contains("algorithm=SHA1"));
        // Spaces in the account are percent-encoded, not literal.
        let u2 = provisioning_uri("AAAA", "Cammy", "front door");
        assert!(u2.contains("Cammy:front%20door"));
        assert!(!u2.contains("front door"));
    }

    #[test]
    fn generated_secret_roundtrips_and_codes_verify() {
        let s = generate_secret();
        assert_eq!(base32_encode(&base32_decode(&s).unwrap()), s);
        // A freshly computed code for "now" verifies.
        let raw = base32_decode(&s).unwrap();
        let now = 1_650_000_000u64;
        let code = totp_code_at(&raw, now);
        assert!(verify(&s, &code, now));
    }

    #[test]
    fn matched_step_returns_the_step_for_replay_tracking() {
        let s = generate_secret();
        let raw = base32_decode(&s).unwrap();
        let now = 1_650_000_000u64;
        let step = now / STEP;
        let code = totp_code_at(&raw, now);
        // The current code matches at exactly this step.
        assert_eq!(matched_step(&s, &code, now), Some(step));
        // A code from the previous step matches at step-1 (skew window).
        let prev = totp_code_at(&raw, now - STEP);
        assert_eq!(matched_step(&s, &prev, now), Some(step - 1));
        // A wrong code matches nothing.
        assert_eq!(matched_step(&s, "000000", now + 9 * STEP), None);
    }
}
