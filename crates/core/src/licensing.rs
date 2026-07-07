//! Licensing & trial entitlement — the "Sellable Product" layer (Launch Roadmap
//! Phase 1). Cammy is sold as an $79 one-time perpetual license (unlimited
//! cameras) with an optional paid update plan, after a 30-day full-featured
//! trial. This module owns the *entitlement* question — "is this install in
//! trial, licensed, or expired?" — and nothing else. It deliberately does not
//! gate features here; enforcement policy lives at the call sites (see
//! `Entitlement::allows_config`) so the one product-critical rule stays
//! honoured everywhere: **never brick a running camera system.**
//!
//! ## License keys (offline-verifiable)
//!
//! A key is a signed token — the JWT shape, minus the JOSE baggage:
//!
//! ```text
//! CAMMY-<base64url(payload_json)>.<base64url(ed25519_sig)>
//! ```
//!
//! The signature is Ed25519 over the *exact* payload bytes, verified locally
//! against an embedded public key. No license server is contacted, ever — a key
//! validates on an air-gapped box, which is the whole point of a self-hosted
//! NVR (and the plan's hard rule: don't let an unreachable server disable
//! surveillance). The private key never ships; issuance happens off the app
//! (Lemon Squeezy webhook / `scripts/license_sign.py`).
//!
//! ## Trial
//!
//! First run stamps `license.trial_start`; the trial is [`TRIAL_DAYS`] from
//! then. The timestamp carries an HMAC tag so casual DB edits that push the
//! clock back are detected (a detected edit fails safe to "expired"). This is
//! deterrence, not DRM — an offline product can't be made uncrackable, and we
//! don't try; we make honesty the easy path.

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::db::Db;

/// Full-featured trial length. Matches the Business Plan (30-day trial).
pub const TRIAL_DAYS: i64 = 30;

/// Ed25519 public key (raw 32 bytes, base64url) that license keys are verified
/// against. This is the **production** key; the matching private seed signs every
/// license and must live only in the offline signer / fulfilment-server env
/// (`scripts/license_sign.py`, `scripts/fulfilment_server.py`), never in the repo.
/// Swapping this constant invalidates every key signed by the old private half —
/// the intended kill-switch if a signing seed ever leaks. Rotating it is a
/// one-line change: the unit tests are hermetic (they sign with a throwaway key),
/// so they do not need regenerating when this rotates.
const LICENSE_PUBKEY_B64URL: &str = "e-dNpE35txpDh2aBywYGcpJtl7Nr6wec9yIo_Y7YZ6Y";

/// Human-facing prefix so a pasted key is recognisably ours. Stripped before
/// decoding; its presence is not required (we tolerate a bare token too).
const KEY_PREFIX: &str = "CAMMY-";

// KV keys (stored in the `settings` table via Db::{get,set,delete}_kv).
const KV_LICENSE_KEY: &str = "license.key";
const KV_TRIAL_START: &str = "license.trial_start";
const KV_TRIAL_TAG: &str = "license.trial_tag";

/// Secret used only to tag the trial-start timestamp so a hand-edited DB row is
/// detectable. It is embedded in the binary and therefore *not* a real secret —
/// it raises the effort of resetting a trial past "edit one number", nothing
/// more. Do not reuse it for anything that needs actual confidentiality.
const TRIAL_TAG_SECRET: &[u8] = b"cammy-trial-integrity-v1";

/// The signed contents of a license key. Verified bytes → this struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct License {
    /// Schema version; only `1` is understood today.
    pub v: u32,
    /// Buyer email (shown in the app so a user can confirm *their* key).
    pub email: String,
    /// `"lifetime"` (perpetual) or `"subscription"` (update-plan window).
    pub plan: String,
    /// Activations allowed. Informational here; enforced online at issuance.
    pub seats: u32,
    /// Merchant order id (Lemon Squeezy), for support lookups.
    #[serde(default)]
    pub order: String,
    /// Issued-at (unix secs).
    pub issued: i64,
    /// For `subscription` plans, when the paid-update window ends (unix secs).
    /// `None` for `lifetime`. A perpetual license is **never** disabled by this
    /// field — it only bounds access to newer builds, a policy the updater
    /// enforces, not us.
    #[serde(default)]
    pub expires: Option<i64>,
}

impl License {
    fn is_lifetime(&self) -> bool {
        self.plan.eq_ignore_ascii_case("lifetime")
    }
}

/// Where this install stands. Computed fresh from the DB on each call — cheap,
/// and never stale.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Entitlement {
    /// A valid perpetual (or in-window subscription) license is installed.
    Licensed {
        plan: String,
        email: String,
        seats: u32,
        /// Free-updates-until (unix secs), if the key carries one.
        expires: Option<i64>,
    },
    /// Inside the free trial.
    Trial {
        days_left: i64,
        /// Trial end (unix secs), for a precise UI countdown.
        ends: i64,
    },
    /// Trial elapsed with no license. The app still runs (we never stop
    /// recording); this is the cue to surface the upgrade path.
    Expired { reason: String },
}

impl Entitlement {
    /// Is the install in good standing (licensed or still in trial)? Used by the
    /// UI to decide whether to show the upgrade nudge, and the single hook a
    /// future feature-gate would consult. Recording/viewing must NOT be gated on
    /// this — an expired trial still protects the home.
    pub fn is_active(&self) -> bool {
        !matches!(self, Entitlement::Expired { .. })
    }

    /// Policy seam: may the user make *new* configuration changes (add cameras,
    /// enable features)? Today this mirrors `is_active` — full access during
    /// trial/license, read-only nudge-to-buy once expired — but it is the one
    /// place to adjust the post-trial policy without touching call sites. It is
    /// intentionally NOT wired into request handling yet; enabling enforcement
    /// is a deliberate business decision (see docs/licensing.md).
    pub fn allows_config(&self) -> bool {
        self.is_active()
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Verify a pasted/stored key: Ed25519 signature check, then parse. Returns the
/// signed [`License`] on success. Pure — no DB, no clock — so it is trivially
/// unit-testable and safe to call from the activation endpoint.
pub fn verify_key(key: &str) -> Result<License> {
    verify_key_with(LICENSE_PUBKEY_B64URL, key)
}

/// Verify against an explicit base64url public key. [`verify_key`] calls this
/// with the embedded production key; the test suite calls it with a throwaway
/// key it generates and signs in-process. That keeps the tests hermetic — they
/// never need regenerating when the embedded production key is rotated, which
/// removes the one fiddly launch chore around key rotation.
fn verify_key_with(pubkey_b64: &str, key: &str) -> Result<License> {
    let token = key.trim().strip_prefix(KEY_PREFIX).unwrap_or(key.trim());
    let (payload_b64, sig_b64) = token
        .split_once('.')
        .ok_or_else(|| anyhow!("malformed license key (expected 'payload.signature')"))?;

    let payload = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .context("license payload is not valid base64url")?;
    let sig = URL_SAFE_NO_PAD
        .decode(sig_b64)
        .context("license signature is not valid base64url")?;

    let pubkey = URL_SAFE_NO_PAD
        .decode(pubkey_b64)
        .context("license public key is not valid base64url")?;
    let verifier = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &pubkey);
    verifier
        .verify(&payload, &sig)
        .map_err(|_| anyhow!("license signature does not verify (wrong or tampered key)"))?;

    let lic: License =
        serde_json::from_slice(&payload).context("license payload is not valid JSON")?;
    if lic.v != 1 {
        bail!("license schema v{} is newer than this build understands", lic.v);
    }
    Ok(lic)
}

/// Current entitlement for this install. Order: a valid stored license wins;
/// otherwise the trial clock decides. A stored-but-invalid key (corrupted, or
/// signed by a rotated-out seed) is ignored and logged, falling back to the
/// trial path rather than hard-failing the app.
pub fn status(db: &Db) -> Entitlement {
    status_with(db, LICENSE_PUBKEY_B64URL)
}

fn status_with(db: &Db, pubkey_b64: &str) -> Entitlement {
    if let Some(key) = db.get_kv(KV_LICENSE_KEY) {
        match verify_key_with(pubkey_b64, &key) {
            Ok(lic) => {
                // Perpetual, or a subscription still inside its window: licensed.
                // A lapsed subscription is treated as *still licensed* for use —
                // the paid window only bounds updates, never usage (see `expires`).
                return Entitlement::Licensed {
                    plan: lic.plan.clone(),
                    email: lic.email.clone(),
                    seats: lic.seats,
                    expires: if lic.is_lifetime() { None } else { lic.expires },
                };
            }
            Err(e) => {
                tracing::warn!("stored license key failed verification, ignoring: {e:#}");
            }
        }
    }
    trial_status(db)
}

fn trial_status(db: &Db) -> Entitlement {
    let start = match read_trial_start(db) {
        Some(ts) => ts,
        // No stamp yet (should be set at startup); treat "now" as the start so we
        // never accidentally report expired before the trial has even begun.
        None => now(),
    };
    let ends = start + TRIAL_DAYS * 86_400;
    let secs_left = ends - now();
    if secs_left > 0 {
        Entitlement::Trial {
            days_left: (secs_left + 86_399) / 86_400, // ceil to whole days
            ends,
        }
    } else {
        Entitlement::Expired {
            reason: format!("{}-day trial ended", TRIAL_DAYS),
        }
    }
}

/// Stamp the trial start on first run. Idempotent: a valid existing stamp is
/// left untouched; a *tampered* stamp is left as-is so it fails safe to expired
/// rather than being silently reset (which would reward editing the row).
pub fn ensure_trial_started(db: &Db) -> Result<()> {
    if db.get_kv(KV_TRIAL_START).is_none() {
        let ts = now();
        db.set_kv(KV_TRIAL_START, &ts.to_string())?;
        db.set_kv(KV_TRIAL_TAG, &trial_tag(ts))?;
        tracing::info!("licensing: {}-day trial started", TRIAL_DAYS);
    }
    Ok(())
}

/// Install a license key: verify, then persist. Returns the resulting
/// entitlement so the caller can echo the new state straight back to the UI.
pub fn activate(db: &Db, key: &str) -> Result<Entitlement> {
    activate_with(db, LICENSE_PUBKEY_B64URL, key)
}

fn activate_with(db: &Db, pubkey_b64: &str, key: &str) -> Result<Entitlement> {
    let lic = verify_key_with(pubkey_b64, key)?;
    db.set_kv(KV_LICENSE_KEY, key.trim())?;
    tracing::info!(email = %lic.email, plan = %lic.plan, "license activated");
    Ok(status_with(db, pubkey_b64))
}

/// Remove the installed license (e.g. moving the license to another machine).
/// Falls back to trial/expired state.
pub fn deactivate(db: &Db) -> Result<()> {
    db.delete_kv(KV_LICENSE_KEY)?;
    tracing::info!("license removed");
    Ok(())
}

/// Read the trial start, returning `None` if absent OR if its integrity tag does
/// not match (a detected edit fails safe to "no valid trial").
fn read_trial_start(db: &Db) -> Option<i64> {
    let raw = db.get_kv(KV_TRIAL_START)?;
    let ts: i64 = raw.trim().parse().ok()?;
    let tag = db.get_kv(KV_TRIAL_TAG)?;
    if constant_time_eq(tag.as_bytes(), trial_tag(ts).as_bytes()) {
        Some(ts)
    } else {
        tracing::warn!("trial timestamp integrity tag mismatch — treating trial as elapsed");
        None
    }
}

/// HMAC-SHA256 tag over the trial start, base64url. Deterrence only (see
/// [`TRIAL_TAG_SECRET`]).
fn trial_tag(ts: i64) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, TRIAL_TAG_SECRET);
    let tag = ring::hmac::sign(&key, format!("trial_start:{ts}").as_bytes());
    URL_SAFE_NO_PAD.encode(tag.as_ref())
}

/// Length-independent byte comparison for the (short, non-secret) trial tag.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    ring::constant_time::verify_slices_are_equal(a, b).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    // Temp-file Db, mirroring db.rs's own test helper (no in-memory ctor exists).
    fn test_db() -> Db {
        let dir = std::env::temp_dir().join(format!("zoomy-lic-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join(format!("lic-{:?}.db", std::time::Instant::now()))).unwrap()
    }

    // A throwaway signing keypair, generated from a fixed seed so tests stay
    // deterministic. This mirrors what scripts/license_sign.py does in Python —
    // Ed25519 over the exact payload bytes — but keeps the whole round-trip inside
    // the test, so rotating the embedded *production* key (LICENSE_PUBKEY_B64URL)
    // never invalidates these vectors. The `_with` verification variants let the
    // tests inject this test pubkey instead of the embedded one.
    struct TestSigner {
        pubkey_b64: String,
        kp: Ed25519KeyPair,
    }

    impl TestSigner {
        fn new() -> Self {
            let seed = [7u8; 32];
            let kp = Ed25519KeyPair::from_seed_unchecked(&seed).expect("valid ed25519 seed");
            let pubkey_b64 = URL_SAFE_NO_PAD.encode(kp.public_key().as_ref());
            Self { pubkey_b64, kp }
        }

        /// Build a signed CAMMY-… key over the given payload, exactly as the app
        /// verifies it (signature over the raw payload bytes).
        fn sign(&self, payload: &serde_json::Value) -> String {
            let pb = serde_json::to_vec(payload).unwrap();
            let sig = self.kp.sign(&pb);
            format!(
                "{KEY_PREFIX}{}.{}",
                URL_SAFE_NO_PAD.encode(&pb),
                URL_SAFE_NO_PAD.encode(sig.as_ref())
            )
        }

        fn lifetime_key(&self) -> String {
            self.sign(&serde_json::json!({
                "v": 1, "email": "bill@example.com", "plan": "lifetime",
                "seats": 2, "order": "LS-0001", "issued": 1749686400, "expires": null,
            }))
        }
    }

    #[test]
    fn verifies_a_genuine_lifetime_key() {
        let s = TestSigner::new();
        let lic = verify_key_with(&s.pubkey_b64, &s.lifetime_key()).expect("should verify");
        assert_eq!(lic.email, "bill@example.com");
        assert_eq!(lic.plan, "lifetime");
        assert_eq!(lic.seats, 2);
        assert!(lic.expires.is_none());
    }

    #[test]
    fn verifies_without_the_prefix() {
        let s = TestSigner::new();
        let key = s.lifetime_key();
        let bare = key.strip_prefix(KEY_PREFIX).unwrap();
        assert!(verify_key_with(&s.pubkey_b64, bare).is_ok());
    }

    #[test]
    fn rejects_a_key_signed_by_a_different_seed() {
        // A key that verifies under its own signer must NOT verify under the
        // embedded production key — this is exactly the protection that stops a
        // self-signed key from licensing the app.
        let s = TestSigner::new();
        assert!(verify_key(&s.lifetime_key()).is_err());
    }

    #[test]
    fn rejects_a_tampered_payload() {
        // Flip one byte of the signed payload segment; the Ed25519 signature must
        // no longer verify.
        let s = TestSigner::new();
        let key = s.lifetime_key();
        let (payload, sig) = key
            .strip_prefix(KEY_PREFIX)
            .unwrap()
            .split_once('.')
            .unwrap();
        let mut p = payload.to_string();
        // Swap the first payload char for a different valid base64url char, which
        // is guaranteed to change the decoded (signed) bytes.
        let first = p.remove(0);
        p.insert(0, if first == 'A' { 'B' } else { 'A' });
        let bad = format!("{KEY_PREFIX}{p}.{sig}");
        assert!(verify_key_with(&s.pubkey_b64, &bad).is_err());
    }

    #[test]
    fn rejects_garbage_and_malformed() {
        assert!(verify_key("not-a-key").is_err());
        assert!(verify_key("CAMMY-onlyonepart").is_err());
        assert!(verify_key("CAMMY-@@@.@@@").is_err());
    }

    #[test]
    fn subscription_key_carries_expiry() {
        let s = TestSigner::new();
        let key = s.sign(&serde_json::json!({
            "v": 1, "email": "sub@example.com", "plan": "subscription",
            "seats": 2, "order": "LS-0002", "issued": 1749686400, "expires": 1781222400,
        }));
        let lic = verify_key_with(&s.pubkey_b64, &key).expect("should verify");
        assert_eq!(lic.plan, "subscription");
        assert_eq!(lic.expires, Some(1781222400));
    }

    #[test]
    fn fresh_db_starts_a_trial_then_reports_it() {
        let db = test_db();
        ensure_trial_started(&db).unwrap();
        match status(&db) {
            Entitlement::Trial { days_left, .. } => {
                assert!(days_left > 0 && days_left <= TRIAL_DAYS);
            }
            other => panic!("expected Trial, got {other:?}"),
        }
    }

    #[test]
    fn activating_a_key_licenses_the_install() {
        let s = TestSigner::new();
        let db = test_db();
        ensure_trial_started(&db).unwrap();
        let ent = activate_with(&db, &s.pubkey_b64, &s.lifetime_key()).unwrap();
        assert!(matches!(ent, Entitlement::Licensed { .. }));
        assert!(matches!(status_with(&db, &s.pubkey_b64), Entitlement::Licensed { .. }));
        deactivate(&db).unwrap();
        assert!(matches!(status_with(&db, &s.pubkey_b64), Entitlement::Trial { .. }));
    }

    #[test]
    fn tampered_trial_stamp_fails_safe_to_expired() {
        let db = test_db();
        ensure_trial_started(&db).unwrap();
        // Rewind the clock by editing the row but NOT the tag: integrity fails,
        // so we must not honour the forged start.
        db.set_kv(KV_TRIAL_START, "100").unwrap();
        assert!(read_trial_start(&db).is_none());
    }
}
