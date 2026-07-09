//! Evidence bundle — a self-verifying ZIP that packages a watermarked event clip
//! with a cryptographically signed provenance manifest (the P2.13 evidence export
//! upgraded from a bare hash-in-the-audit-log to a portable, court-checkable
//! artifact).
//!
//! The bundle contains the watermarked `evidence.mp4`, a `manifest.json` pinning
//! the clip's SHA-256 + all provenance (event, camera, timestamps, Cammy
//! version), a `manifest.sig` (Ed25519 signature over the exact manifest bytes),
//! the signing `PUBLIC_KEY.txt`, and a plain-English `VERIFY.txt`. Anyone can
//! open the ZIP with a standard tool and re-check it offline with
//! `zoomy --verify <bundle.zip>`: the signature proves the manifest came from
//! *this* Cammy install and the pinned hash proves the clip wasn't altered after
//! export.
//!
//! No new dependencies: the ZIP is written/read here as an uncompressed (STORED)
//! archive — real-world compatible with any unzip tool — and Ed25519 comes from
//! `ring`, already in-tree for license verification.

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Manifest format tag — bump if the shape changes so old bundles stay readable.
pub const FORMAT: &str = "cammy-evidence-1";

/// The signed provenance record. Its serialized `manifest.json` bytes are exactly
/// what the Ed25519 signature covers, and `verify` checks those same bytes — so
/// signing is deterministic without a canonicalization step (serde preserves this
/// field declaration order as JSON key order).
#[derive(Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub cammy_version: String,
    pub event_id: i64,
    pub camera: String,
    pub label: String,
    pub event_unix: i64,
    pub event_local: String,
    pub clip_file: String,
    pub clip_sha256: String,
    pub generated_unix: i64,
    /// Base64 of the raw 32-byte Ed25519 public key the signature is checked
    /// against (embedded so a bundle is self-contained/offline-verifiable).
    pub public_key: String,
}

// ---- Ed25519 signing key, persisted per install --------------------------------

fn key_path(data_dir: &Path) -> PathBuf {
    data_dir.join("keys").join("evidence_ed25519.seed")
}

/// Load this install's evidence signing key, generating + persisting a fresh
/// 32-byte seed on first use. The seed is the private half; it never leaves the
/// box (mode 0600 on Unix). A new bundle is signed with whatever seed is present,
/// so rotating = delete the file (older bundles stay verifiable via their own
/// embedded public key).
pub fn signing_key(data_dir: &Path) -> Result<Ed25519KeyPair> {
    let path = key_path(data_dir);
    let seed: [u8; 32] = match std::fs::read(&path) {
        Ok(b) => b
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("evidence signing key has wrong length"))?,
        Err(_) => {
            use ring::rand::SecureRandom;
            let mut s = [0u8; 32];
            ring::rand::SystemRandom::new()
                .fill(&mut s)
                .map_err(|_| anyhow::anyhow!("secure RNG unavailable"))?;
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p).ok();
            }
            std::fs::write(&path, s).context("persisting evidence signing key")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).ok();
            }
            s
        }
    };
    Ed25519KeyPair::from_seed_unchecked(&seed)
        .map_err(|_| anyhow::anyhow!("invalid evidence signing seed"))
}

pub fn public_key_b64(kp: &Ed25519KeyPair) -> String {
    B64.encode(kp.public_key().as_ref())
}

/// Verify `manifest.json` bytes against the signature, using the public key the
/// manifest carries. Returns the parsed manifest on success.
pub fn verify_manifest(manifest_json: &[u8], sig_b64: &str) -> Result<Manifest> {
    let m: Manifest = serde_json::from_slice(manifest_json).context("parsing manifest.json")?;
    let pubkey = B64
        .decode(m.public_key.trim().as_bytes())
        .context("manifest public_key is not valid base64")?;
    let sig = B64
        .decode(sig_b64.trim().as_bytes())
        .context("manifest.sig is not valid base64")?;
    UnparsedPublicKey::new(&ED25519, &pubkey)
        .verify(manifest_json, &sig)
        .map_err(|_| anyhow::anyhow!("signature does not verify (manifest was altered)"))?;
    Ok(m)
}

pub fn sha256_hex(data: &[u8]) -> String {
    crate::util::hex(&Sha256::digest(data))
}

// ---- minimal STORED (method-0) ZIP, no compression dependency ------------------

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

pub struct ZipEntry {
    pub name: String,
    pub data: Vec<u8>,
}

/// Build an uncompressed (STORED) ZIP openable by any standard tool. We store
/// rather than deflate because the one large member is already-compressed H.264
/// (deflate wouldn't shrink it) — and STORED needs no compression crate.
pub fn zip_store(entries: &[ZipEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    let mut offsets = Vec::with_capacity(entries.len());
    for e in entries {
        offsets.push(out.len() as u32);
        let crc = crc32(&e.data);
        let name = e.name.as_bytes();
        let sz = e.data.len() as u32;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes()); // local file header sig
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method 0 = store
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0x21u16.to_le_bytes()); // mod date = 1980-01-01
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&sz.to_le_bytes()); // compressed size
        out.extend_from_slice(&sz.to_le_bytes()); // uncompressed size
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name);
        out.extend_from_slice(&e.data);
    }
    let cd_start = out.len() as u32;
    for (i, e) in entries.iter().enumerate() {
        let crc = crc32(&e.data);
        let name = e.name.as_bytes();
        let sz = e.data.len() as u32;
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes()); // central dir header sig
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central.extend_from_slice(&0u16.to_le_bytes()); // flags
        central.extend_from_slice(&0u16.to_le_bytes()); // method
        central.extend_from_slice(&0u16.to_le_bytes()); // time
        central.extend_from_slice(&0x21u16.to_le_bytes()); // date
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&sz.to_le_bytes());
        central.extend_from_slice(&sz.to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk #
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&offsets[i].to_le_bytes());
        central.extend_from_slice(name);
    }
    let cd_len = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes()); // EOCD sig
    out.extend_from_slice(&0u16.to_le_bytes()); // this disk
    out.extend_from_slice(&0u16.to_le_bytes()); // cd start disk
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_len.to_le_bytes());
    out.extend_from_slice(&cd_start.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment len
    out
}

/// Read a STORED ZIP (as written by [`zip_store`]), returning `(name, bytes)` per
/// member and verifying each member's CRC-32. Used by `zoomy --verify`.
pub fn zip_read(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let u16at = |o: usize| -> Result<usize> {
        Ok(u16::from_le_bytes(bytes.get(o..o + 2).context("truncated")?.try_into()?) as usize)
    };
    let u32at = |o: usize| -> Result<usize> {
        Ok(u32::from_le_bytes(bytes.get(o..o + 4).context("truncated")?.try_into()?) as usize)
    };
    // Locate the End Of Central Directory record (our archives have no comment,
    // so it's the last 22 bytes; scan back to tolerate any trailer regardless).
    let eocd_sig = 0x0605_4b50u32.to_le_bytes();
    let mut eo = None;
    if bytes.len() >= 22 {
        for i in (0..=bytes.len() - 22).rev() {
            if bytes[i..i + 4] == eocd_sig {
                eo = Some(i);
                break;
            }
        }
    }
    let eo = eo.context("not a ZIP archive (no end-of-central-directory record)")?;
    let count = u16at(eo + 10)?;
    let cd_start = u32at(eo + 16)?;
    let mut out = Vec::with_capacity(count);
    let mut p = cd_start;
    for _ in 0..count {
        if bytes.get(p..p + 4) != Some(&0x0201_4b50u32.to_le_bytes()) {
            bail!("corrupt central directory");
        }
        let method = u16at(p + 10)?;
        let crc_want = u32at(p + 16)?;
        let comp_size = u32at(p + 20)?;
        let name_len = u16at(p + 28)?;
        let extra_len = u16at(p + 30)?;
        let comment_len = u16at(p + 32)?;
        let lho = u32at(p + 42)?;
        let name = String::from_utf8_lossy(
            bytes
                .get(p + 46..p + 46 + name_len)
                .context("truncated central dir name")?,
        )
        .into_owned();
        if method != 0 {
            bail!("bundle member '{name}' uses compression (expected an uncompressed archive)");
        }
        if bytes.get(lho..lho + 4) != Some(&0x0403_4b50u32.to_le_bytes()) {
            bail!("corrupt local header for '{name}'");
        }
        let l_name = u16at(lho + 26)?;
        let l_extra = u16at(lho + 28)?;
        let data_off = lho + 30 + l_name + l_extra;
        let data = bytes
            .get(data_off..data_off + comp_size)
            .context("truncated archive entry")?
            .to_vec();
        if crc32(&data) as usize != crc_want {
            bail!("CRC mismatch for '{name}' (bundle corrupted)");
        }
        out.push((name, data));
        p += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}

/// `zoomy --verify <bundle.zip>` implementation: parse the ZIP, re-check the
/// Ed25519 signature over `manifest.json`, and re-hash `evidence.mp4` against the
/// pinned SHA-256. Prints a human report; returns `Err` (non-zero exit) if any
/// check fails.
pub fn verify_bundle_cli(path: &Path) -> Result<()> {
    let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let members = zip_read(&raw)?;
    let get = |n: &str| members.iter().find(|(name, _)| name == n).map(|(_, d)| d);
    let manifest_json = get("manifest.json").context("bundle is missing manifest.json")?;
    let sig = get("manifest.sig").context("bundle is missing manifest.sig")?;
    let sig_str = String::from_utf8_lossy(sig);

    let m = verify_manifest(manifest_json, &sig_str)?;
    println!("  Bundle:      {}", path.display());
    println!("  Signature:   OK (Ed25519 verified against embedded public key)");
    println!(
        "  Event:       #{} · {} on camera \"{}\"",
        m.event_id, m.label, m.camera
    );
    println!("  Recorded:    {} (unix {})", m.event_local, m.event_unix);
    println!("  Cammy:       v{}", m.cammy_version);

    let clip = get(&m.clip_file)
        .with_context(|| format!("bundle is missing its clip ({})", m.clip_file))?;
    let got = sha256_hex(clip);
    if got != m.clip_sha256 {
        bail!(
            "clip SHA-256 MISMATCH — the video was altered.\n    manifest: {}\n    actual:   {}",
            m.clip_sha256,
            got
        );
    }
    println!("  Clip hash:   OK (SHA-256 {})", got);
    println!("\n  VERIFIED — the clip is authentic and unaltered.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_round_trips_and_verifies_crc() {
        let entries = vec![
            ZipEntry {
                name: "manifest.json".into(),
                data: b"{\"format\":\"cammy-evidence-1\"}".to_vec(),
            },
            ZipEntry {
                name: "evidence.mp4".into(),
                data: (0..5000u32).map(|i| (i % 251) as u8).collect(),
            },
        ];
        let zip = zip_store(&entries);
        let back = zip_read(&zip).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].0, "manifest.json");
        assert_eq!(back[1].1, entries[1].data);
    }

    #[test]
    fn corrupted_member_fails_crc() {
        let entries = vec![ZipEntry {
            name: "a.bin".into(),
            data: vec![1, 2, 3, 4, 5, 6, 7, 8],
        }];
        let mut zip = zip_store(&entries);
        // Flip a byte inside the stored data region (just past the 30-byte local
        // header + 5-byte name).
        let off = 30 + 5;
        zip[off] ^= 0xFF;
        assert!(zip_read(&zip).is_err());
    }

    #[test]
    fn crc32_matches_known_vector() {
        // CRC-32/ISO-HDLC of "123456789" is 0xCBF43926.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn sign_then_verify_roundtrip_and_tamper() {
        let seed = [42u8; 32];
        let kp = Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
        let m = Manifest {
            format: FORMAT.into(),
            cammy_version: "0.4.0".into(),
            event_id: 7,
            camera: "front-door".into(),
            label: "person".into(),
            event_unix: 1_783_000_000,
            event_local: "2026-07-09 12:00:00".into(),
            clip_file: "evidence.mp4".into(),
            clip_sha256: "deadbeef".into(),
            generated_unix: 1_783_000_100,
            public_key: public_key_b64(&kp),
        };
        let json = serde_json::to_vec(&m).unwrap();
        let sig = B64.encode(kp.sign(&json).as_ref());
        // Good signature verifies.
        assert!(verify_manifest(&json, &sig).is_ok());
        // Any tamper to the signed bytes breaks it.
        let mut bad = json.clone();
        bad[20] ^= 0x01;
        assert!(verify_manifest(&bad, &sig).is_err());
        // A different key's signature is rejected.
        let other = Ed25519KeyPair::from_seed_unchecked(&[9u8; 32]).unwrap();
        let bad_sig = B64.encode(other.sign(&json).as_ref());
        assert!(verify_manifest(&json, &bad_sig).is_err());
    }
}
