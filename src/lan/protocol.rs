//! LAN transfer protocol — HTTP/2 over TLS 1.3, JSON control messages.
//!
//! ```text
//! sender                                   receiver
//!   |  POST /api/v1/offer  {Manifest}        |   -> 202 {OfferResponse::Pending}  (user asked)
//!   |  GET  /api/v1/offer/{id}  (poll)       |   -> {accepted|rejected|pending}
//!   |  GET  /api/v1/transfer/{id}/file/{i}   |   -> {received: N}   (resume offset)
//!   |  PUT  /api/v1/transfer/{id}/file/{i}?offset=N  <body bytes>  -> 200 {UploadResult}
//!   |      (receiver hashes while writing; 409 hash_mismatch -> sender retries this file)
//!   |  POST /api/v1/transfer/{id}/complete   |   -> 200
//! ```

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const API_PREFIX: &str = "/api/v1";
pub const HEADER_PIN: &str = "x-unishare-pin";
pub const HEADER_HASH: &str = "x-unishare-blake3";
pub const HEADER_DEVICE: &str = "x-unishare-device";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestFile {
    /// Relative path with `/` separators.
    pub path: String,
    pub size: u64,
    /// BLAKE3 hex, may be empty if the sender hashes lazily (then sent in header).
    #[serde(default)]
    pub blake3: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub version: u32,
    /// Sender device name.
    pub sender: String,
    /// Sender TLS fingerprint (informational).
    #[serde(default)]
    pub sender_fingerprint: String,
    /// Top-level name (file or folder).
    pub name: String,
    pub total_size: u64,
    pub files: Vec<ManifestFile>,
    /// True when the payload is a single .tar.zst that should be unpacked.
    #[serde(default)]
    pub compressed_archive: bool,
}

impl Manifest {
    pub fn new(sender: &str, sender_fingerprint: &str, name: &str, files: Vec<ManifestFile>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            sender: sender.to_string(),
            sender_fingerprint: sender_fingerprint.to_string(),
            name: name.to_string(),
            total_size: files.iter().map(|f| f.size).sum(),
            files,
            compressed_archive: false,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != PROTOCOL_VERSION {
            return Err(format!("unsupported protocol version {}", self.version));
        }
        if self.files.is_empty() {
            return Err("empty manifest".into());
        }
        if self.files.len() > 200_000 {
            return Err("too many files".into());
        }
        for f in &self.files {
            if f.path.is_empty() || f.path.len() > 4096 {
                return Err("invalid file path".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OfferStatus {
    Pending,
    Accepted,
    Rejected { reason: String },
    /// Transfer was accepted earlier and is receiving/finished.
    Active,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferResponse {
    pub transfer_id: String,
    #[serde(flatten)]
    pub status: OfferStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileState {
    /// Bytes already present on disk for this file (resume offset).
    pub received: u64,
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum UploadResult {
    Ok { blake3: String },
    HashMismatch { expected: String, actual: String },
    SizeMismatch { expected: u64, actual: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    pub version: String,
    pub fingerprint: String,
    pub protocol: u32,
    pub requires_pin: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrip_and_totals() {
        let m = Manifest::new(
            "Laptop",
            "abcd",
            "proj",
            vec![
                ManifestFile { path: "proj/a.txt".into(), size: 10, blake3: String::new() },
                ManifestFile { path: "proj/b/c.bin".into(), size: 32, blake3: "ff".into() },
            ],
        );
        assert_eq!(m.total_size, 42);
        assert!(m.validate().is_ok());
        let json = serde_json::to_string(&m).unwrap();
        let back: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn manifest_validation() {
        let mut m = Manifest::new("s", "", "n", vec![]);
        assert!(m.validate().is_err());
        m.files.push(ManifestFile { path: "".into(), size: 0, blake3: String::new() });
        assert!(m.validate().is_err());
        m.files[0].path = "x".into();
        assert!(m.validate().is_ok());
        m.version = 99;
        assert!(m.validate().is_err());
    }

    #[test]
    fn tagged_enums() {
        let r = OfferResponse { transfer_id: "t1".into(), status: OfferStatus::Rejected { reason: "no".into() } };
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["status"], "rejected");
        assert_eq!(j["reason"], "no");
        let u: UploadResult = serde_json::from_str(r#"{"result":"hash_mismatch","expected":"a","actual":"b"}"#).unwrap();
        assert_eq!(u, UploadResult::HashMismatch { expected: "a".into(), actual: "b".into() });
    }
}
