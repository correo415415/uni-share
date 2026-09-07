//! Download everything described by a [`Ticket`]: try each source in order,
//! then verify BLAKE3 digests when the ticket carries them.

use super::http::{ProgressFn, browser_client, download_resumable, filename_from_disposition};
use crate::fsutil::destination_path;
use crate::hash;
use crate::ticket::{Source, Ticket};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
pub struct TicketReport {
    pub source: String,
    pub saved: Vec<PathBuf>,
    /// Files whose BLAKE3 was checked and matched.
    pub verified: usize,
    /// Files for which the ticket had no digest.
    pub unverified: usize,
}

/// Progress for the *whole* ticket (bytes) plus file names.
pub async fn download_ticket(
    t: &Ticket,
    dest_dir: &Path,
    force: bool,
    progress: ProgressFn,
    on_file: impl Fn(&str) + Clone,
    on_source: impl Fn(&Source, Option<&anyhow::Error>),
) -> Result<TicketReport> {
    t.validate()?;
    if t.is_expired() {
        tracing::warn!("ticket expired at {:?}; trying anyway", t.expires);
    }
    tokio::fs::create_dir_all(dest_dir).await?;
    let mut last_err: Option<anyhow::Error> = None;
    for src in &t.sources {
        on_source(src, last_err.as_ref());
        match fetch_from(src, t, dest_dir, force, progress.clone(), on_file.clone()).await {
            Ok(saved) => {
                let (verified, unverified) = verify(t, &saved).await?;
                return Ok(TicketReport { source: src.label().into(), saved, verified, unverified });
            }
            Err(e) => {
                tracing::warn!(source = src.url(), "source failed: {e:#}");
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("ticket has no usable source"))).context("all ticket sources failed")
}

async fn fetch_from(
    src: &Source,
    t: &Ticket,
    dest_dir: &Path,
    force: bool,
    progress: ProgressFn,
    on_file: impl Fn(&str),
) -> Result<Vec<PathBuf>> {
    match src {
        Source::StorageTo { url, password } => {
            let d = super::storage_to::StorageDownloader::new()?;
            let info = d.info(url).await?;
            if info.password_protected {
                let pw = password.as_deref().context("share is password-protected but the ticket has no password")?;
                d.verify_password(&info, pw).await?;
            }
            d.download_all(&info, dest_dir, force, progress, on_file).await
        }
        Source::SwissTransfer { url, password } => {
            let mut c = super::swisstransfer::SwissTransferClient::new()?;
            let tr = c.get_transfer(url, password.as_deref()).await?;
            c.download_all(&tr, dest_dir, force, progress, on_file).await
        }
        Source::Http { url, filename } => {
            let client = browser_client()?;
            // Name: explicit > single ticket file > Content-Disposition > URL tail.
            let name = match filename.clone().or_else(|| (t.files.len() == 1).then(|| t.files[0].path.clone())) {
                Some(n) => n,
                None => {
                    let head = client.head(url).send().await.ok();
                    head.as_ref()
                        .and_then(|r| r.headers().get(reqwest::header::CONTENT_DISPOSITION))
                        .and_then(|v| v.to_str().ok())
                        .and_then(filename_from_disposition)
                        .or_else(|| url.rsplit('/').next().filter(|s| !s.is_empty()).map(|s| s.split('?').next().unwrap_or(s).to_string()))
                        .unwrap_or_else(|| t.name.clone())
                }
            };
            on_file(&name);
            let dest = destination_path(dest_dir, &name, force);
            let expected = (t.files.len() == 1).then(|| t.files[0].size).filter(|s| *s > 0);
            download_resumable(&client, url, &dest, expected, progress, 3).await?;
            Ok(vec![dest])
        }
    }
}

/// Check saved files against the ticket digests (matched by file name /
/// relative path suffix). Returns (verified, unverified).
async fn verify(t: &Ticket, saved: &[PathBuf]) -> Result<(usize, usize)> {
    let mut verified = 0;
    let mut unverified = 0;
    for p in saved {
        let name = p.to_string_lossy().replace('\\', "/");
        // Duplicate renaming (`x (1).ext`) must not break matching: strip it.
        let canon = strip_dup_suffix(&name);
        let entry = t.files.iter().find(|f| canon.ends_with(&f.path) || name.ends_with(&f.path));
        match entry.and_then(|f| f.blake3.as_deref()) {
            Some(expected) => {
                let got = hash::hash_file(p).await?;
                if !hash::digests_equal(&got, expected) {
                    bail!("BLAKE3 mismatch for {}: expected {expected}, got {got}", p.display());
                }
                verified += 1;
            }
            None => unverified += 1,
        }
    }
    Ok((verified, unverified))
}

fn strip_dup_suffix(name: &str) -> String {
    // "dir/file (2).ext" -> "dir/file.ext"
    let re = regex::Regex::new(r" \(\d+\)(\.[^./]+)?$").expect("static regex");
    re.replace(name, "$1").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dup_suffix() {
        assert_eq!(strip_dup_suffix("a/b (1).txt"), "a/b.txt");
        assert_eq!(strip_dup_suffix("a/b (12)"), "a/b");
        assert_eq!(strip_dup_suffix("a/b.txt"), "a/b.txt");
    }
}
