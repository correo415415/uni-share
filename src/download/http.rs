//! Generic resumable HTTP downloader (`.part` file + `Range`), shared by the
//! storage.to and SwissTransfer downloaders.

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

pub type ProgressFn = Arc<dyn Fn(u64) + Send + Sync>;

pub const BROWSER_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";

pub fn browser_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(BROWSER_UA)
        .cookie_store(true)
        .use_preconfigured_tls(crate::global::storage_to::platform_tls()?)
        .redirect(reqwest::redirect::Policy::limited(10))
        .connect_timeout(Duration::from_secs(20))
        .build()
        .context("building http client")
}

fn part_of(p: &Path) -> PathBuf {
    let mut os = p.as_os_str().to_owned();
    os.push(".part");
    PathBuf::from(os)
}

/// Download `url` to `dest` (final path). Resumes from `dest.part` when the
/// server honours `Range`. `expected_size` (if known) is verified at the end.
/// Retries transient failures up to `retries` times.
pub async fn download_resumable(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_size: Option<u64>,
    progress: ProgressFn,
    retries: usize,
) -> Result<u64> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let part = part_of(dest);
    let mut attempt = 0;
    loop {
        attempt += 1;
        match download_once(client, url, &part, expected_size, &progress).await {
            Ok(size) => {
                if let Some(exp) = expected_size {
                    if size != exp {
                        let _ = tokio::fs::remove_file(&part).await;
                        bail!("size mismatch for {}: expected {exp}, got {size}", dest.display());
                    }
                }
                tokio::fs::rename(&part, dest).await.with_context(|| format!("renaming to {}", dest.display()))?;
                return Ok(size);
            }
            Err(e) if attempt <= retries => {
                tracing::warn!(attempt, "download error, retrying: {e:#}");
                tokio::time::sleep(Duration::from_millis(800 * attempt as u64)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

async fn download_once(client: &reqwest::Client, url: &str, part: &Path, expected: Option<u64>, progress: &ProgressFn) -> Result<u64> {
    let mut existing = tokio::fs::metadata(part).await.map(|m| m.len()).unwrap_or(0);
    if let Some(exp) = expected {
        if existing > exp {
            let _ = tokio::fs::remove_file(part).await;
            existing = 0;
        }
        if existing == exp && exp > 0 {
            progress(existing);
            return Ok(existing);
        }
    }
    let mut req = client.get(url).timeout(Duration::from_secs(3600));
    if existing > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={existing}-"));
    }
    let resp = req.send().await.context("GET")?;
    let status = resp.status();
    let (mut file, mut written) = if existing > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT {
        progress(existing);
        (tokio::fs::OpenOptions::new().append(true).open(part).await?, existing)
    } else if status.is_success() {
        // Server ignored the range (or fresh start) → start over.
        (tokio::fs::File::create(part).await?, 0)
    } else {
        bail!("HTTP {status} downloading {url}");
    };
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("stream")?;
        file.write_all(&chunk).await?;
        written += chunk.len() as u64;
        progress(chunk.len() as u64);
    }
    file.flush().await?;
    Ok(written)
}

/// Extract filename from Content-Disposition header value, if any.
pub fn filename_from_disposition(v: &str) -> Option<String> {
    // filename*=UTF-8''name ; filename="name"
    if let Some(i) = v.find("filename*=") {
        let rest = &v[i + 10..];
        let rest = rest.split(';').next().unwrap_or(rest).trim();
        let enc = rest.splitn(3, '\'').nth(2).unwrap_or(rest);
        let dec = percent_encoding::percent_decode_str(enc).decode_utf8_lossy().to_string();
        if !dec.is_empty() {
            return Some(dec);
        }
    }
    if let Some(i) = v.find("filename=") {
        let rest = &v[i + 9..];
        let rest = rest.split(';').next().unwrap_or(rest).trim().trim_matches('"');
        if !rest.is_empty() {
            return Some(rest.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_parsing() {
        assert_eq!(filename_from_disposition(r#"attachment; filename="t.txt"; filename*=UTF-8''t%20x.txt"#), Some("t x.txt".into()));
        assert_eq!(filename_from_disposition(r#"attachment; filename="a.bin""#), Some("a.bin".into()));
        assert_eq!(filename_from_disposition("inline"), None);
    }
}
