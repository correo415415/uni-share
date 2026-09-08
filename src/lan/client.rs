//! LAN sender: connects to a receiver (pinned TLS), offers a manifest, waits
//! for acceptance, streams each file (resuming from the receiver's offset)
//! and retries individual files on hash mismatch.

use super::protocol::*;
use super::tls::pinned_reqwest_client;
use crate::fsutil::FileEntry;
use crate::hash;
use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use reqwest::StatusCode;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

pub const MAX_FILE_RETRIES: usize = 3;

/// Progress callback: (bytes_sent_delta, current_file_rel_path).
pub type ProgressFn = Arc<dyn Fn(u64, &str) + Send + Sync>;

pub struct Sender {
    client: reqwest::Client,
    base: String,
    pin: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SendReport {
    pub transfer_id: String,
    pub files: usize,
    pub bytes: u64,
    pub retries: usize,
}

impl Sender {
    pub fn new(ip: IpAddr, port: u16, fingerprint: Option<&str>, pin: Option<String>) -> Result<Self> {
        let client = pinned_reqwest_client(fingerprint)?;
        let host = match ip {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("[{v6}]"),
        };
        Ok(Self { client, base: format!("https://{host}:{port}{API_PREFIX}"), pin })
    }

    fn req(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.pin {
            Some(p) => rb.header(HEADER_PIN, p),
            None => rb,
        }
    }

    pub async fn info(&self) -> Result<DeviceInfo> {
        let r = self
            .client
            .get(format!("{}/info", self.base))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .context("connecting to receiver")?;
        Ok(r.error_for_status()?.json().await?)
    }

    /// Send the offer and poll until accepted / rejected.
    pub async fn offer(&self, manifest: &Manifest, wait: Duration) -> Result<String> {
        let r = self
            .req(self.client.post(format!("{}/offer", self.base)).json(manifest))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .context("sending offer")?;
        if r.status() == StatusCode::UNAUTHORIZED {
            bail!("receiver requires a valid PIN (use --pin)");
        }
        let r = check(r).await?;
        let resp: OfferResponse = r.json().await.context("offer response")?;
        let id = resp.transfer_id;
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let st = self
                .client
                .get(format!("{}/offer/{id}", self.base))
                .timeout(Duration::from_secs(10))
                .send()
                .await
                .context("polling offer")?;
            let st: OfferResponse = check(st).await?.json().await?;
            match st.status {
                OfferStatus::Accepted | OfferStatus::Active => return Ok(id),
                OfferStatus::Rejected { reason } => bail!("receiver rejected the transfer: {reason}"),
                OfferStatus::Pending => {
                    if tokio::time::Instant::now() > deadline {
                        bail!("receiver did not answer in time");
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }

    async fn file_state(&self, id: &str, idx: usize) -> Result<FileState> {
        let r = self.client.get(format!("{}/transfer/{id}/file/{idx}", self.base)).send().await?;
        Ok(check(r).await?.json().await?)
    }

    /// Upload one file, resuming from the receiver's offset. Returns bytes sent
    /// in this call (delta) on success.
    async fn upload_once(
        &self,
        id: &str,
        idx: usize,
        entry: &FileEntry,
        blake3_hex: &str,
        progress: &ProgressFn,
        rate_limit_bps: u64,
    ) -> Result<UploadResult> {
        let state = self.file_state(id, idx).await?;
        if state.complete {
            return Ok(UploadResult::Ok { blake3: blake3_hex.to_string() });
        }
        let offset = state.received.min(entry.size);
        let mut file = tokio::fs::File::open(&entry.abs_path)
            .await
            .with_context(|| format!("opening {}", entry.abs_path.display()))?;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        let remaining = entry.size - offset;
        let rel = entry.rel_path.clone();
        let progress = progress.clone();
        let sent = Arc::new(AtomicU64::new(0));
        let sent2 = sent.clone();

        let stream = async_stream_reader(file, remaining, rate_limit_bps, move |n| {
            sent2.fetch_add(n, Ordering::Relaxed);
            progress(n, &rel);
        });
        let body = reqwest::Body::wrap_stream(stream);
        let r = self
            .req(
                self.client
                    .put(format!("{}/transfer/{id}/file/{idx}?offset={offset}", self.base))
                    .header(HEADER_HASH, blake3_hex)
                    .header(reqwest::header::CONTENT_LENGTH, remaining)
                    .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                    .body(body),
            )
            .send()
            .await
            .with_context(|| format!("uploading {}", entry.rel_path))?;
        match r.status() {
            StatusCode::OK | StatusCode::CONFLICT => {
                let text = r.text().await?;
                serde_json::from_str::<UploadResult>(&text).map_err(|_| anyhow!("receiver error: {text}"))
            }
            s => {
                let text = r.text().await.unwrap_or_default();
                Err(anyhow!("upload failed ({s}): {text}"))
            }
        }
    }

    /// Upload a single file of an accepted transfer (resumes from the receiver's
    /// offset). Building block for custom flows and tests; `send` does the
    /// whole manifest with retries.
    pub async fn upload_file(
        &self,
        id: &str,
        idx: usize,
        entry: &FileEntry,
        blake3_hex: &str,
        progress: &ProgressFn,
        rate_limit_bps: u64,
    ) -> Result<UploadResult> {
        self.upload_once(id, idx, entry, blake3_hex, progress, rate_limit_bps).await
    }

    /// Full send: offer → upload all files (with per-file retry) → complete.
    pub async fn send(
        &self,
        manifest: &Manifest,
        files: &[FileEntry],
        hashes: &[String],
        progress: ProgressFn,
        accept_wait: Duration,
        rate_limit_bps: u64,
    ) -> Result<SendReport> {
        anyhow::ensure!(files.len() == manifest.files.len() && hashes.len() == files.len(), "manifest mismatch");
        let id = self.offer(manifest, accept_wait).await?;
        let mut retries = 0usize;
        for (idx, entry) in files.iter().enumerate() {
            let mut attempt = 0;
            loop {
                attempt += 1;
                match self.upload_once(&id, idx, entry, &hashes[idx], &progress, rate_limit_bps).await {
                    Ok(UploadResult::Ok { .. }) => break,
                    Ok(other) => {
                        retries += 1;
                        tracing::warn!(file = %entry.rel_path, ?other, attempt, "integrity failure, retrying file");
                        if attempt >= MAX_FILE_RETRIES {
                            bail!("{} failed integrity check {attempt} times: {other:?}", entry.rel_path);
                        }
                    }
                    Err(e) => {
                        retries += 1;
                        tracing::warn!(file = %entry.rel_path, attempt, "upload error: {e:#}");
                        if attempt >= MAX_FILE_RETRIES {
                            return Err(e.context(format!("giving up on {}", entry.rel_path)));
                        }
                        tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await;
                    }
                }
            }
        }
        let r = self
            .req(self.client.post(format!("{}/transfer/{id}/complete", self.base)))
            .send()
            .await
            .context("completing transfer")?;
        check(r).await?;
        Ok(SendReport { transfer_id: id, files: files.len(), bytes: manifest.total_size, retries })
    }
}

async fn check(r: reqwest::Response) -> Result<reqwest::Response> {
    if r.status().is_success() {
        return Ok(r);
    }
    let status = r.status();
    let text = r.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<ErrorBody>(&text).map(|e| e.error).unwrap_or(text);
    Err(anyhow!("receiver returned {status}: {msg}"))
}

/// Turn a file into a byte stream of `remaining` bytes with progress + optional
/// throttling.
fn async_stream_reader(
    file: tokio::fs::File,
    remaining: u64,
    rate_limit_bps: u64,
    on_chunk: impl Fn(u64) + Send + 'static,
) -> impl futures::Stream<Item = std::result::Result<Bytes, std::io::Error>> + Send + 'static {
    const CHUNK: usize = 1 << 20; // 1 MiB
    struct St<F> {
        file: tokio::fs::File,
        remaining: u64,
        limiter: super::server::RateLimiter,
        on_chunk: F,
    }
    let st = St { file, remaining, limiter: super::server::RateLimiter::new(rate_limit_bps), on_chunk };
    futures::stream::unfold(st, |mut st| async move {
        if st.remaining == 0 {
            return None;
        }
        let want = (st.remaining as usize).min(CHUNK);
        let mut buf = vec![0u8; want];
        let mut filled = 0;
        while filled < want {
            match st.file.read(&mut buf[filled..]).await {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) => return Some((Err(e), st)),
            }
        }
        if filled == 0 {
            return Some((
                Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "file shrank during transfer")),
                st,
            ));
        }
        buf.truncate(filled);
        st.remaining -= filled as u64;
        st.limiter.throttle(filled).await;
        (st.on_chunk)(filled as u64);
        Some((Ok(Bytes::from(buf)), st))
    })
}

/// Compute BLAKE3 for every file (used by the sender before offering).
pub async fn hash_all(files: &[FileEntry], on_file: impl Fn(&str)) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(files.len());
    for f in files {
        on_file(&f.rel_path);
        out.push(hash::hash_file(&f.abs_path).await?);
    }
    Ok(out)
}

/// Build a manifest from collected files + hashes.
pub fn build_manifest(sender: &str, fingerprint: &str, name: &str, files: &[FileEntry], hashes: &[String]) -> Manifest {
    let mf = files
        .iter()
        .zip(hashes)
        .map(|(f, h)| ManifestFile { path: f.rel_path.clone(), size: f.size, blake3: h.clone() })
        .collect();
    Manifest::new(sender, fingerprint, name, mf)
}

/// Helper for `--compress`: pack folder into temp .tar.zst and return a single entry.
pub async fn compress_to_temp(src: &Path) -> Result<(tempfile::TempDir, Vec<FileEntry>)> {
    let tmp = tempfile::tempdir()?;
    let name = src.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "folder".into());
    let dest = tmp.path().join(format!("{name}.tar.zst"));
    let (src2, dest2) = (src.to_path_buf(), dest.clone());
    let size = tokio::task::spawn_blocking(move || crate::fsutil::pack_tar_zst(&src2, &dest2, 3)).await??;
    Ok((tmp, vec![FileEntry { rel_path: format!("{name}.tar.zst"), size, abs_path: dest }]))
}
