//! `.unishare` **ticket** — a self-contained, shareable description of a
//! download.
//!
//! A ticket is a small file (or a `unishare:` URI, e.g. inside a QR code) that
//! bundles everything a recipient needs to fetch and *verify* a share:
//!
//! * one or more **sources** (storage.to link, SwissTransfer link, direct HTTP
//!   URL…) tried in order — so a share survives one backend going away;
//! * the share **password** (optional) — the link stays protected for anyone
//!   who only sees the URL, while the ticket holder downloads transparently;
//! * the **file list** with sizes and BLAKE3 digests — the downloader verifies
//!   integrity end-to-end, which a bare link can never offer;
//! * sender name, title, free-text message, creation/expiry timestamps.
//!
//! ### Container format (version 1)
//!
//! ```text
//! offset  size  field
//! 0       8     magic  "UNISHARE"
//! 8       1     format version (1)
//! 9       1     compression (0 = none, 1 = zstd)
//! 10      4     payload length, little-endian u32
//! 14      n     payload (JSON, possibly zstd-compressed)
//! 14+n    32    BLAKE3 of the *decompressed* JSON
//! ```
//!
//! **Why a custom binary container and not plain JSON?** A fixed magic makes
//! files self-identifying (`file`-style sniffing, drag & drop into the GUI),
//! zstd keeps QR codes small (JSON with hashes compresses ~2-3x), and the
//! trailing BLAKE3 detects truncation/corruption before we act on the data.
//!
//! ### URI form
//!
//! `unishare:<base64url(container)>` — safe in chats, e-mails and QR codes.
//! When a ticket is too big for a comfortable QR, [`Ticket::to_uri_compact`]
//! drops per-file details (the downloader then simply skips verification).

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const MAGIC: &[u8; 8] = b"UNISHARE";
pub const FORMAT_VERSION: u8 = 1;
pub const EXTENSION: &str = "unishare";
pub const URI_SCHEME: &str = "unishare:";
/// Practical upper bound for a scannable QR (byte mode, version ≤ 30 ≈ 1.3 KB).
pub const QR_SOFT_LIMIT: usize = 1200;

const COMPRESSION_NONE: u8 = 0;
const COMPRESSION_ZSTD: u8 = 1;
const HEADER_LEN: usize = 8 + 1 + 1 + 4;
const DIGEST_LEN: usize = 32;
/// Refuse absurd payloads (a ticket is metadata, not data).
const MAX_PAYLOAD: usize = 8 * 1024 * 1024;

/// Where the bytes can be fetched from. Tried in order by the downloader.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// storage.to file (`/{id}`) or collection (`/c/{id}`).
    StorageTo {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
    },
    /// SwissTransfer download link.
    SwissTransfer {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
    },
    /// Any direct HTTP(S) URL (single file, `Range` resume when supported).
    Http {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
    },
}

impl Source {
    pub fn url(&self) -> &str {
        match self {
            Source::StorageTo { url, .. } | Source::SwissTransfer { url, .. } | Source::Http { url, .. } => url,
        }
    }
    pub fn password(&self) -> Option<&str> {
        match self {
            Source::StorageTo { password, .. } | Source::SwissTransfer { password, .. } => password.as_deref(),
            Source::Http { .. } => None,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Source::StorageTo { .. } => "storage.to",
            Source::SwissTransfer { .. } => "SwissTransfer",
            Source::Http { .. } => "HTTP",
        }
    }
    /// Build the right source variant for a share URL.
    pub fn from_url(url: &str, password: Option<String>) -> Source {
        if crate::download::swisstransfer::is_swisstransfer_url(url) {
            Source::SwissTransfer { url: url.into(), password }
        } else if crate::global::storage_to::parse_share_url(url).is_some() {
            Source::StorageTo { url: url.into(), password }
        } else {
            Source::Http { url: url.into(), filename: None }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TicketFile {
    /// Relative path (forward slashes) as it should land in the destination.
    pub path: String,
    pub size: u64,
    /// BLAKE3 hex digest, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blake3: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ticket {
    /// Schema version of the JSON payload (independent from the container).
    pub v: u8,
    /// Random 64-bit id (hex) — lets the GUI/history dedupe tickets.
    pub id: String,
    pub created: DateTime<Utc>,
    /// Human title (usually the file/folder name).
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<DateTime<Utc>>,
    pub total_size: u64,
    pub sources: Vec<Source>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<TicketFile>,
}

impl Ticket {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            v: 1,
            id: random_id(),
            created: Utc::now(),
            name: name.into(),
            message: None,
            sender: None,
            expires: None,
            total_size: 0,
            sources: Vec::new(),
            files: Vec::new(),
        }
    }

    /// Ticket for an upload we just made (`send-global --ticket`).
    pub fn from_upload(
        name: &str,
        url: &str,
        password: Option<&str>,
        sender: &str,
        files: &[crate::fsutil::FileEntry],
        hashes: &[String],
        expires: Option<DateTime<Utc>>,
    ) -> Self {
        let mut t = Ticket::new(name);
        t.sender = Some(sender.to_string());
        t.expires = expires;
        t.sources.push(Source::from_url(url, password.map(str::to_string)));
        t.files = files
            .iter()
            .enumerate()
            .map(|(i, f)| TicketFile { path: f.rel_path.clone(), size: f.size, blake3: hashes.get(i).cloned() })
            .collect();
        t.total_size = files.iter().map(|f| f.size).sum();
        t
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(self.v == 1, "unsupported ticket schema v{}", self.v);
        ensure!(!self.sources.is_empty(), "ticket has no sources");
        ensure!(!self.name.trim().is_empty(), "ticket has no name");
        for s in &self.sources {
            ensure!(s.url().starts_with("http://") || s.url().starts_with("https://"), "invalid source url {}", s.url());
        }
        for f in &self.files {
            ensure!(!f.path.is_empty() && !f.path.contains(".."), "invalid file path {:?}", f.path);
            if let Some(h) = &f.blake3 {
                ensure!(h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()), "invalid BLAKE3 digest for {}", f.path);
            }
        }
        Ok(())
    }

    pub fn is_expired(&self) -> bool {
        self.expires.map(|e| e < Utc::now()).unwrap_or(false)
    }

    /// Serialise into the binary container (always zstd-compressed).
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let json = serde_json::to_vec(self).context("serialising ticket")?;
        encode_container(&json, true)
    }

    /// Parse a binary container (also accepts the URI form and raw JSON for
    /// convenience, e.g. when a user pastes a ticket into the GUI).
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let trimmed = trim_ascii(bytes);
        if trimmed.starts_with(MAGIC) {
            let json = decode_container(trimmed)?;
            let t: Ticket = serde_json::from_slice(&json).context("ticket JSON")?;
            t.validate()?;
            return Ok(t);
        }
        if let Ok(s) = std::str::from_utf8(trimmed) {
            if is_uri(s) {
                return Self::from_uri(s);
            }
            if s.starts_with('{') {
                let t: Ticket = serde_json::from_str(s).context("ticket JSON")?;
                t.validate()?;
                return Ok(t);
            }
        }
        bail!("not a uni-share ticket (missing UNISHARE magic)")
    }

    pub fn to_uri(&self) -> Result<String> {
        Ok(format!("{URI_SCHEME}{}", URL_SAFE_NO_PAD.encode(self.encode()?)))
    }

    /// URI that fits in a QR code: drops per-file details if needed.
    pub fn to_uri_compact(&self) -> Result<String> {
        let full = self.to_uri()?;
        if full.len() <= QR_SOFT_LIMIT {
            return Ok(full);
        }
        let mut slim = self.clone();
        // First drop hashes, then the whole list.
        for f in &mut slim.files {
            f.blake3 = None;
        }
        let u = slim.to_uri()?;
        if u.len() <= QR_SOFT_LIMIT {
            return Ok(u);
        }
        slim.files.clear();
        slim.to_uri()
    }

    pub fn from_uri(s: &str) -> Result<Self> {
        let s = s.trim();
        let b64 = s
            .strip_prefix(URI_SCHEME)
            .or_else(|| s.strip_prefix("unishare://"))
            .context("not a unishare: URI")?;
        let b64 = b64.trim_start_matches('/');
        let bytes = URL_SAFE_NO_PAD.decode(b64.trim_end_matches('=')).context("invalid base64 in unishare URI")?;
        Self::decode(&bytes)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.encode()?).with_context(|| format!("writing {}", path.display()))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::decode(&bytes).with_context(|| format!("parsing {}", path.display()))
    }

    /// Suggested filename: `<name>.unishare`.
    pub fn default_filename(&self) -> String {
        let base: String = self
            .name
            .chars()
            .map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ' ') { c } else { '_' })
            .collect();
        let base = base.trim().trim_matches('.');
        format!("{}.{EXTENSION}", if base.is_empty() { "share" } else { base })
    }

    /// One-line human summary.
    pub fn summary(&self) -> String {
        let files = if self.files.is_empty() { String::new() } else { format!(", {} archivo(s)", self.files.len()) };
        let srcs: Vec<&str> = self.sources.iter().map(|s| s.label()).collect();
        format!("«{}» — {}{} · fuentes: {}", self.name, crate::fsutil::human_bytes(self.total_size), files, srcs.join(", "))
    }
}

/// `true` when the argument looks like a ticket (file path with the right
/// extension, or a `unishare:` URI).
pub fn looks_like_ticket(arg: &str) -> bool {
    is_uri(arg) || Path::new(arg).extension().map(|e| e.eq_ignore_ascii_case(EXTENSION)).unwrap_or(false)
}

pub fn is_uri(s: &str) -> bool {
    let s = s.trim();
    s.starts_with(URI_SCHEME) || s.starts_with("unishare://")
}

/// Resolve a CLI argument into a ticket: URI string or path to a `.unishare` file.
pub fn resolve(arg: &str) -> Result<Ticket> {
    if is_uri(arg) {
        return Ticket::from_uri(arg);
    }
    Ticket::load(Path::new(arg))
}

fn random_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 8];
    rand::rng().fill_bytes(&mut b);
    hex::encode(b)
}

fn trim_ascii(b: &[u8]) -> &[u8] {
    let start = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !c.is_ascii_whitespace()).map(|i| i + 1).unwrap_or(start);
    &b[start..end]
}

fn encode_container(json: &[u8], compress: bool) -> Result<Vec<u8>> {
    let (payload, comp) = if compress {
        (zstd::bulk::compress(json, 19).context("zstd")?, COMPRESSION_ZSTD)
    } else {
        (json.to_vec(), COMPRESSION_NONE)
    };
    ensure!(payload.len() <= MAX_PAYLOAD, "ticket too large");
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len() + DIGEST_LEN);
    out.extend_from_slice(MAGIC);
    out.push(FORMAT_VERSION);
    out.push(comp);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    out.extend_from_slice(blake3::hash(json).as_bytes());
    Ok(out)
}

fn decode_container(bytes: &[u8]) -> Result<Vec<u8>> {
    ensure!(bytes.len() >= HEADER_LEN + DIGEST_LEN, "ticket truncated");
    ensure!(&bytes[..8] == MAGIC, "bad magic");
    let version = bytes[8];
    ensure!(version == FORMAT_VERSION, "unsupported ticket container version {version}");
    let comp = bytes[9];
    let len = u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]) as usize;
    ensure!(len <= MAX_PAYLOAD, "ticket payload too large");
    ensure!(bytes.len() >= HEADER_LEN + len + DIGEST_LEN, "ticket truncated (payload)");
    let payload = &bytes[HEADER_LEN..HEADER_LEN + len];
    let digest = &bytes[HEADER_LEN + len..HEADER_LEN + len + DIGEST_LEN];
    let json = match comp {
        COMPRESSION_NONE => payload.to_vec(),
        COMPRESSION_ZSTD => zstd::bulk::decompress(payload, MAX_PAYLOAD).context("zstd payload")?,
        other => bail!("unknown compression {other}"),
    };
    ensure!(blake3::hash(&json).as_bytes() == digest, "ticket checksum mismatch (corrupted file)");
    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Ticket {
        let mut t = Ticket::new("Fotos vacaciones");
        t.sender = Some("laptop".into());
        t.message = Some("Hola!".into());
        t.total_size = 3;
        t.sources.push(Source::StorageTo { url: "https://storage.to/c/abc123".into(), password: Some("s3cret".into()) });
        t.sources.push(Source::Http { url: "https://example.com/x.zip".into(), filename: None });
        t.files.push(TicketFile { path: "a/b.txt".into(), size: 3, blake3: Some(crate::hash::hash_bytes(b"abc")) });
        t
    }

    #[test]
    fn roundtrip_binary() {
        let t = sample();
        let bytes = t.encode().unwrap();
        assert!(bytes.starts_with(MAGIC));
        assert_eq!(bytes[8], FORMAT_VERSION);
        let back = Ticket::decode(&bytes).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn roundtrip_uri() {
        let t = sample();
        let uri = t.to_uri().unwrap();
        assert!(uri.starts_with("unishare:"));
        assert!(!uri.contains('=') && !uri.contains('+') && !uri.contains('/'), "must be URL-safe");
        assert_eq!(Ticket::from_uri(&uri).unwrap(), t);
        assert!(looks_like_ticket(&uri));
        assert!(looks_like_ticket("foo.unishare"));
        assert!(!looks_like_ticket("https://storage.to/abc"));
    }

    #[test]
    fn corruption_detected() {
        let mut bytes = sample().encode().unwrap();
        let i = bytes.len() - 40; // inside payload
        bytes[i] ^= 0xff;
        let e = Ticket::decode(&bytes).unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("checksum") || msg.contains("zstd"), "{msg}");
        assert!(Ticket::decode(&bytes[..20]).is_err());
        assert!(Ticket::decode(b"hello").is_err());
    }

    #[test]
    fn json_and_validation() {
        let t = sample();
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"kind\":\"storage_to\""));
        assert_eq!(Ticket::decode(json.as_bytes()).unwrap(), t);
        let mut bad = t.clone();
        bad.sources.clear();
        assert!(bad.encode().is_err());
        let mut bad = t.clone();
        bad.files[0].path = "../etc/passwd".into();
        assert!(bad.encode().is_err());
    }

    #[test]
    fn compact_uri_fits_qr() {
        let mut t = sample();
        for i in 0..400 {
            t.files.push(TicketFile { path: format!("dir/sub/file_{i:04}.bin"), size: i, blake3: Some(crate::hash::hash_bytes(&[i as u8])) });
        }
        assert!(t.to_uri().unwrap().len() > QR_SOFT_LIMIT);
        let c = t.to_uri_compact().unwrap();
        assert!(c.len() <= QR_SOFT_LIMIT, "{}", c.len());
        let back = Ticket::from_uri(&c).unwrap();
        assert_eq!(back.name, t.name);
        assert_eq!(back.sources, t.sources);
    }

    #[test]
    fn filename_sanitised() {
        let mut t = sample();
        t.name = "inv/alid:name?".into();
        assert_eq!(t.default_filename(), "inv_alid_name_.unishare");
        assert!(Source::from_url("https://www.swisstransfer.com/d/1", None).label() == "SwissTransfer");
        assert!(Source::from_url("https://storage.to/abc123", None).label() == "storage.to");
    }
}
