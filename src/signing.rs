//! Optional **Ed25519 signatures** for `.unishare` tickets.
//!
//! A ticket is metadata that travels over untrusted channels (chats, QR codes
//! on a screen, e-mail). Anyone can forge one pointing at a malicious link.
//! Signing lets the recipient check that the ticket really comes from the
//! device it claims to — the sender's public key acts as a stable identity
//! ("signer fingerprint") that can be compared out-of-band once and then
//! recognised forever, exactly like an SSH host key.
//!
//! * Keys are generated on first use and stored as PKCS#8 in
//!   `<data_dir>/signing.key` (mode 0600 on Unix).
//! * The signed message is the ticket's canonical JSON **without** the
//!   `signature` field (see [`crate::ticket::Ticket::signing_bytes`]).
//! * Public keys and signatures are base64url (no padding) so they stay
//!   QR/URI friendly: +43 bytes for the key, +86 for the signature.
//!
//! Only `ring` is used (already a dependency through rustls/rcgen), so no new
//! crypto crate is pulled in.

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use std::path::{Path, PathBuf};

pub const KEY_FILE: &str = "signing.key";
pub const PUBLIC_KEY_LEN: usize = 32;
pub const SIGNATURE_LEN: usize = 64;

/// Persistent Ed25519 identity used to sign tickets.
pub struct SigningKey {
    pair: Ed25519KeyPair,
    pkcs8: Vec<u8>,
}

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningKey").field("public", &self.public_b64()).finish()
    }
}

impl SigningKey {
    /// Generate a fresh random key (not persisted).
    pub fn generate() -> Result<Self> {
        let rng = ring::rand::SystemRandom::new();
        let doc = Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| anyhow::anyhow!("ed25519 keygen failed"))?;
        Self::from_pkcs8(doc.as_ref())
    }

    pub fn from_pkcs8(bytes: &[u8]) -> Result<Self> {
        let pair = Ed25519KeyPair::from_pkcs8_maybe_unchecked(bytes).map_err(|_| anyhow::anyhow!("invalid ed25519 PKCS#8 key"))?;
        Ok(Self { pair, pkcs8: bytes.to_vec() })
    }

    pub fn key_path(dir: &Path) -> PathBuf {
        dir.join(KEY_FILE)
    }

    /// Load `<dir>/signing.key`, generating (and saving) it on first use.
    pub fn load_or_generate(dir: &Path) -> Result<Self> {
        let path = Self::key_path(dir);
        if path.exists() {
            let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            return Self::from_pkcs8(&bytes).with_context(|| format!("parsing {}", path.display()));
        }
        let k = Self::generate()?;
        std::fs::create_dir_all(dir)?;
        std::fs::write(&path, &k.pkcs8).with_context(|| format!("writing {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(k)
    }

    /// Load only if the key already exists (never creates one).
    pub fn load(dir: &Path) -> Result<Option<Self>> {
        let path = Self::key_path(dir);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(Self::from_pkcs8(&std::fs::read(&path)?)?))
    }

    pub fn public_key(&self) -> &[u8] {
        self.pair.public_key().as_ref()
    }

    /// base64url public key — this is what goes into `Ticket.signer`.
    pub fn public_b64(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.public_key())
    }

    /// Short human fingerprint of the public key (`ab12:cd34:…`, 8 groups).
    pub fn fingerprint(&self) -> String {
        fingerprint_of(self.public_key())
    }

    pub fn sign(&self, msg: &[u8]) -> Vec<u8> {
        self.pair.sign(msg).as_ref().to_vec()
    }

    pub fn sign_b64(&self, msg: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(self.sign(msg))
    }
}

/// Human fingerprint of a raw Ed25519 public key: first 16 bytes of its
/// BLAKE3 hash as `xxxx:xxxx:…` (short enough to read aloud, 128-bit).
pub fn fingerprint_of(public_key: &[u8]) -> String {
    let h = blake3::hash(public_key);
    let hex = hex::encode(&h.as_bytes()[..16]);
    hex.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap_or("")).collect::<Vec<_>>().join(":")
}

/// Fingerprint from a base64url public key (as stored in a ticket).
pub fn fingerprint_of_b64(public_b64: &str) -> Result<String> {
    Ok(fingerprint_of(&decode_public(public_b64)?))
}

pub fn decode_public(public_b64: &str) -> Result<Vec<u8>> {
    let pk = URL_SAFE_NO_PAD.decode(public_b64.trim().trim_end_matches('=')).context("signer is not valid base64url")?;
    ensure!(pk.len() == PUBLIC_KEY_LEN, "signer public key must be {PUBLIC_KEY_LEN} bytes (got {})", pk.len());
    Ok(pk)
}

/// Verify `signature_b64` over `msg` with the base64url public key.
pub fn verify(public_b64: &str, msg: &[u8], signature_b64: &str) -> Result<()> {
    let pk = decode_public(public_b64)?;
    let sig = URL_SAFE_NO_PAD.decode(signature_b64.trim().trim_end_matches('=')).context("signature is not valid base64url")?;
    if sig.len() != SIGNATURE_LEN {
        bail!("signature must be {SIGNATURE_LEN} bytes (got {})", sig.len());
    }
    UnparsedPublicKey::new(&ED25519, pk).verify(msg, &sig).map_err(|_| anyhow::anyhow!("Ed25519 signature does not match"))
}

/// Outcome of checking a ticket's signature, for CLI/GUI display.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SignatureStatus {
    /// Ticket carries no signature.
    Unsigned,
    /// Signature verified; `fingerprint` identifies the signer's key.
    Valid { fingerprint: String, signer: String },
    /// Ticket claims a signature that does not verify (tampered or forged).
    Invalid { reason: String },
}

impl SignatureStatus {
    pub fn is_invalid(&self) -> bool {
        matches!(self, SignatureStatus::Invalid { .. })
    }
    /// Short Spanish label for UIs.
    pub fn label(&self) -> String {
        match self {
            SignatureStatus::Unsigned => "sin firma".into(),
            SignatureStatus::Valid { fingerprint, .. } => format!("firma válida · {fingerprint}"),
            SignatureStatus::Invalid { reason } => format!("FIRMA INVÁLIDA ({reason})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_roundtrip_and_tamper() {
        let k = SigningKey::generate().unwrap();
        let msg = b"hello ticket";
        let sig = k.sign_b64(msg);
        verify(&k.public_b64(), msg, &sig).unwrap();
        assert!(verify(&k.public_b64(), b"hello tickeT", &sig).is_err());
        let other = SigningKey::generate().unwrap();
        assert!(verify(&other.public_b64(), msg, &sig).is_err());
        assert!(verify("not-base64!!", msg, &sig).is_err());
        assert!(verify(&k.public_b64(), msg, "AAAA").is_err());
        assert_eq!(k.public_b64().len(), 43);
        assert_eq!(sig.len(), 86);
    }

    #[test]
    fn key_persists_across_loads() {
        let dir = tempfile::tempdir().unwrap();
        assert!(SigningKey::load(dir.path()).unwrap().is_none());
        let a = SigningKey::load_or_generate(dir.path()).unwrap();
        let b = SigningKey::load_or_generate(dir.path()).unwrap();
        assert_eq!(a.public_b64(), b.public_b64());
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert!(SigningKey::load(dir.path()).unwrap().is_some());
        assert!(dir.path().join(KEY_FILE).exists());
        let fp = a.fingerprint();
        assert_eq!(fp.len(), 32 + 7, "{fp}");
        assert_eq!(fingerprint_of_b64(&a.public_b64()).unwrap(), fp);
    }
}
