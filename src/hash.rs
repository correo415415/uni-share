//! Integrity hashing (BLAKE3).
//!
//! BLAKE3 was chosen over SHA-256 because it is ~5-10x faster on modern CPUs
//! (SIMD + multi-threading via `rayon`), is cryptographically secure and has a
//! stable, small Rust implementation. Hashes are hex-encoded (64 chars).

use anyhow::{Context, Result};
use std::path::Path;
use tokio::io::AsyncReadExt;

pub const HASH_ALGO: &str = "blake3";
const CHUNK: usize = 1024 * 1024; // 1 MiB

/// Incremental hasher usable while streaming data to disk / network.
#[derive(Clone)]
pub struct Hasher(blake3::Hasher);

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher {
    pub fn new() -> Self {
        Self(blake3::Hasher::new())
    }
    pub fn update(&mut self, data: &[u8]) {
        // rayon-parallel update for big chunks, plain for small ones.
        if data.len() >= 128 * 1024 {
            self.0.update_rayon(data);
        } else {
            self.0.update(data);
        }
    }
    pub fn finalize_hex(&self) -> String {
        self.0.finalize().to_hex().to_string()
    }
}

/// Hash an in-memory buffer.
pub fn hash_bytes(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

/// Hash a file asynchronously (streaming, constant memory).
pub async fn hash_file(path: &Path) -> Result<String> {
    let mut f = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Hasher::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize_hex())
}

/// Blocking variant (used from spawn_blocking contexts / tests).
pub fn hash_file_blocking(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Hasher::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize_hex())
}

/// Constant-time-ish comparison of two hex digests (case-insensitive).
pub fn digests_equal(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim().to_ascii_lowercase(), b.trim().to_ascii_lowercase());
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn known_vector_empty() {
        // BLAKE3 of empty input.
        assert_eq!(
            hash_bytes(b""),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn known_vector_abc() {
        assert_eq!(
            hash_bytes(b"abc"),
            "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
        );
    }

    #[test]
    fn incremental_matches_oneshot() {
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        let mut h = Hasher::new();
        for c in data.chunks(7_777) {
            h.update(c);
        }
        assert_eq!(h.finalize_hex(), hash_bytes(&data));
    }

    #[tokio::test]
    async fn file_hash_async_and_blocking_agree() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.bin");
        let data: Vec<u8> = (0..(2 * CHUNK + 123)).map(|i| (i * 31 % 256) as u8).collect();
        std::fs::File::create(&p).unwrap().write_all(&data).unwrap();
        let a = hash_file(&p).await.unwrap();
        let b = hash_file_blocking(&p).unwrap();
        assert_eq!(a, b);
        assert_eq!(a, hash_bytes(&data));
    }

    #[test]
    fn digest_compare() {
        assert!(digests_equal("ABC", "abc"));
        assert!(!digests_equal("abc", "abd"));
        assert!(!digests_equal("abc", "abcd"));
    }
}
