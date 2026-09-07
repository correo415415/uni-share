//! High-level storage.to upload orchestration: single files, multipart
//! (parallel parts with retry + abort on failure) and folder collections.

use super::storage_to::{Client, ConfirmRequest, FileInfo, InitResponse, Part, ResourceKind};
use crate::fsutil::FileEntry;
use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use futures::StreamExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

pub const MAX_PART_RETRIES: usize = 4;
pub const SINGLE_RETRIES: usize = 3;

/// Progress callback receives byte deltas.
pub type ProgressFn = Arc<dyn Fn(u64) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct UploadOptions {
    pub expiry_days: Option<u32>,
    pub parallel_parts: usize,
    pub password: Option<String>,
    pub max_downloads: Option<u32>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UploadOutcome {
    pub url: String,
    pub id: String,
    pub kind: String,
    pub owner_token: Option<String>,
    pub size: u64,
    pub files: usize,
    pub expires_at: Option<String>,
    pub password_protected: bool,
}

pub fn content_type_for(path: &Path) -> String {
    mime_guess::from_path(path).first_or_octet_stream().essence_str().to_string()
}

/// Upload one file; `filename` may contain `/` (collection relative path).
pub async fn upload_file(
    client: &Client,
    entry: &FileEntry,
    filename: &str,
    collection_id: Option<&str>,
    opts: &UploadOptions,
    progress: ProgressFn,
) -> Result<(FileInfo, Option<String>)> {
    let ct = content_type_for(&entry.abs_path);
    let init = client.init_upload(filename, &ct, entry.size).await.context("upload init")?;
    match init.kind.as_str() {
        "single" => upload_single(client, entry, &init, &ct, progress).await?,
        "multipart" => upload_multipart(client, entry, &init, opts.parallel_parts, progress).await?,
        other => bail!("unknown upload type '{other}'"),
    }
    let confirm = client
        .confirm(&ConfirmRequest {
            filename,
            size: entry.size,
            content_type: &ct,
            r2_key: &init.r2_key,
            collection_id,
            expiry_days: opts.expiry_days,
        })
        .await
        .context("upload confirm")?;
    let file = confirm.file.ok_or_else(|| anyhow!("confirm returned no file"))?;
    Ok((file, confirm.owner_token))
}

async fn upload_single(client: &Client, entry: &FileEntry, init: &InitResponse, ct: &str, progress: ProgressFn) -> Result<()> {
    let url = init.upload_url.as_deref().ok_or_else(|| anyhow!("init returned no upload_url"))?;
    // R2 presigned URL signs Content-Type when the server tells us so.
    let ct_hdr = init.headers.get("Content-Type").and_then(|v| v.first()).map(String::as_str).unwrap_or(ct);
    let mut attempt = 0;
    loop {
        attempt += 1;
        let sent_before = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let sb = sent_before.clone();
        let p = progress.clone();
        let stream = file_stream(&entry.abs_path, 0, entry.size, move |n| {
            sb.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
            p(n);
        })
        .await?;
        let res = client.put_presigned(url, reqwest::Body::wrap_stream(stream), entry.size, Some(ct_hdr)).await;
        match res {
            Ok(_) => return Ok(()),
            Err(e) if attempt < SINGLE_RETRIES => {
                // roll back progress for a clean retry
                let done = sent_before.load(std::sync::atomic::Ordering::Relaxed);
                tracing::warn!(attempt, "single upload failed, retrying: {e:#}");
                rollback(&progress, done);
                tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

fn rollback(progress: &ProgressFn, _done: u64) {
    // Progress bars are monotonic in indicatif; we simply don't re-count.
    // Callers wanting exact accounting can wrap `progress`.
    let _ = progress;
}

async fn upload_multipart(
    client: &Client,
    entry: &FileEntry,
    init: &InitResponse,
    parallel: usize,
    progress: ProgressFn,
) -> Result<()> {
    let upload_id = init.upload_id.clone().ok_or_else(|| anyhow!("multipart init without upload_id"))?;
    let part_size = init.part_size.ok_or_else(|| anyhow!("multipart init without part_size"))?;
    let total_parts = init.total_parts.ok_or_else(|| anyhow!("multipart init without total_parts"))?;
    let owner = init.owner_token.clone();
    tracing::info!(total_parts, part_size, "multipart upload");

    let urls = Arc::new(tokio::sync::Mutex::new(init.initial_urls.clone()));
    let client = client.clone();
    let path = entry.abs_path.clone();
    let size = entry.size;

    let results: Vec<Result<Part>> = futures::stream::iter(1..=total_parts)
        .map(|n| {
            let client = client.clone();
            let urls = urls.clone();
            let path = path.clone();
            let progress = progress.clone();
            let upload_id = upload_id.clone();
            let owner = owner.clone();
            async move {
                let offset = (n as u64 - 1) * part_size;
                let len = if n == total_parts { size - offset } else { part_size };
                let mut attempt = 0;
                loop {
                    attempt += 1;
                    let url = {
                        let mut map = urls.lock().await;
                        match map.get(&n.to_string()) {
                            Some(u) => u.clone(),
                            None => {
                                let hi = (n + 49).min(total_parts);
                                let batch: Vec<u32> = (n..=hi).collect();
                                let more = client.part_urls(&upload_id, owner.as_deref(), &batch).await?;
                                map.extend(more);
                                map.get(&n.to_string()).cloned().ok_or_else(|| anyhow!("no URL for part {n}"))?
                            }
                        }
                    };
                    let counted = Arc::new(std::sync::atomic::AtomicU64::new(0));
                    let c2 = counted.clone();
                    let p2 = progress.clone();
                    let stream = file_stream(&path, offset, len, move |k| {
                        c2.fetch_add(k, std::sync::atomic::Ordering::Relaxed);
                        p2(k);
                    })
                    .await?;
                    match client.put_presigned(&url, reqwest::Body::wrap_stream(stream), len, None).await {
                        Ok(Some(etag)) => return Ok(Part { part_number: n, etag }),
                        Ok(None) => return Err(anyhow!("R2 returned no ETag for part {n}")),
                        Err(e) => {
                            if attempt >= MAX_PART_RETRIES {
                                return Err(e.context(format!("part {n} failed after {attempt} attempts")));
                            }
                            tracing::warn!(part = n, attempt, "part upload failed, retrying: {e:#}");
                            // URL may have expired → drop it so it gets refreshed.
                            if attempt >= 2 {
                                urls.lock().await.remove(&n.to_string());
                            }
                            tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
                        }
                    }
                }
            }
        })
        .buffer_unordered(parallel.max(1))
        .collect()
        .await;

    let mut parts = Vec::with_capacity(results.len());
    for r in results {
        match r {
            Ok(p) => parts.push(p),
            Err(e) => {
                let _ = client.abort(&upload_id, owner.as_deref()).await;
                return Err(e);
            }
        }
    }
    parts.sort_by_key(|p| p.part_number);
    if let Err(e) = client.complete_multipart(&upload_id, owner.as_deref(), parts).await {
        let _ = client.abort(&upload_id, owner.as_deref()).await;
        return Err(e.context("complete multipart"));
    }
    Ok(())
}

async fn file_stream(
    path: &Path,
    offset: u64,
    len: u64,
    on_chunk: impl Fn(u64) + Send + 'static,
) -> Result<impl futures::Stream<Item = std::io::Result<Bytes>> + Send + 'static> {
    let mut f = tokio::fs::File::open(path).await.with_context(|| format!("opening {}", path.display()))?;
    f.seek(std::io::SeekFrom::Start(offset)).await?;
    const CHUNK: usize = 1 << 20;
    Ok(futures::stream::unfold((f, len, on_chunk), |(mut f, mut left, cb)| async move {
        if left == 0 {
            return None;
        }
        let want = (left as usize).min(CHUNK);
        let mut buf = vec![0u8; want];
        let mut filled = 0;
        while filled < want {
            match f.read(&mut buf[filled..]).await {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) => return Some((Err(e), (f, left, cb))),
            }
        }
        if filled == 0 {
            return Some((Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "file shrank")), (f, 0, cb)));
        }
        buf.truncate(filled);
        left -= filled as u64;
        cb(filled as u64);
        Some((Ok(Bytes::from(buf)), (f, left, cb)))
    }))
}

/// Upload a set of files. One file → plain file link; several → collection
/// whose `filename`s carry the relative paths (folder hierarchy preserved).
pub async fn upload_entries(
    client: &Client,
    files: &[FileEntry],
    opts: &UploadOptions,
    progress: ProgressFn,
    on_file: impl Fn(&str),
) -> Result<UploadOutcome> {
    anyhow::ensure!(!files.is_empty(), "nothing to upload");
    let total: u64 = files.iter().map(|f| f.size).sum();
    anyhow::ensure!(total <= super::storage_to::MAX_COLLECTION_SIZE, "total size exceeds 25 GB");

    if files.len() == 1 {
        let f = &files[0];
        // For a single file the server only needs the base name.
        let name = f.rel_path.rsplit('/').next().unwrap_or(&f.rel_path);
        on_file(name);
        let (info, owner) = upload_file(client, f, name, None, opts, progress).await?;
        apply_settings(client, ResourceKind::File, &info.id, owner.as_deref(), opts).await?;
        return Ok(UploadOutcome {
            url: info.url.clone(),
            id: info.id.clone(),
            kind: "file".into(),
            owner_token: owner,
            size: f.size,
            files: 1,
            expires_at: info.expires_at.clone(),
            password_protected: opts.password.is_some(),
        });
    }

    let coll = client.create_collection(files.len() as u32).await?;
    let info = coll.collection.ok_or_else(|| anyhow!("no collection info"))?;
    let owner = coll.owner_token.clone();
    for f in files {
        on_file(&f.rel_path);
        upload_file(client, f, &f.rel_path, Some(&info.id), opts, progress.clone())
            .await
            .with_context(|| format!("uploading {}", f.rel_path))?;
    }
    // Ensure ready (auto when expected_file_count reached; explicit is harmless).
    if let Some(o) = &owner {
        if let Err(e) = client.collection_ready(&info.id, o).await {
            tracing::debug!("collection ready call: {e:#}");
        }
    }
    apply_settings(client, ResourceKind::Collection, &info.id, owner.as_deref(), opts).await?;
    Ok(UploadOutcome {
        url: info.url.clone(),
        id: info.id.clone(),
        kind: "collection".into(),
        owner_token: owner,
        size: total,
        files: files.len(),
        expires_at: info.expires_at.clone(),
        password_protected: opts.password.is_some(),
    })
}

async fn apply_settings(client: &Client, kind: ResourceKind, id: &str, owner: Option<&str>, opts: &UploadOptions) -> Result<()> {
    if opts.password.is_none() && opts.max_downloads.is_none() {
        return Ok(());
    }
    let owner = owner.ok_or_else(|| anyhow!("server did not return an owner token; cannot apply password/max-downloads"))?;
    if let Some(p) = &opts.password {
        client.set_password(kind, id, owner, p).await.context("setting password")?;
    }
    if let Some(m) = opts.max_downloads {
        client.set_max_downloads(kind, id, owner, m).await.context("setting max downloads")?;
    }
    Ok(())
}

// ------------------------------------------------------------------ Smash

/// Upload files as one Smash transfer (every file keeps its relative path as
/// name, so folders arrive with hierarchy). Requires an API key.
pub async fn upload_entries_smash(
    client: &super::smash::SmashClient,
    files: &[FileEntry],
    title: &str,
    opts: &UploadOptions,
    progress: ProgressFn,
    on_file: impl Fn(&str),
) -> Result<UploadOutcome> {
    use super::smash::DonePart;
    anyhow::ensure!(!files.is_empty(), "nothing to upload");
    let total: u64 = files.iter().map(|f| f.size).sum();
    let availability = opts.expiry_days.map(|d| d as u64 * 86_400);
    let transfer = client
        .create_transfer(total, files.len() as u32, Some(title), availability, opts.password.as_deref(), "es")
        .await
        .context("creating Smash transfer")?;
    let parallel = opts.parallel_parts.max(1).min(transfer.parallel_connections.unwrap_or(4) as usize);

    let result: Result<()> = async {
        for f in files {
            let name = if files.len() == 1 { f.rel_path.rsplit('/').next().unwrap_or(&f.rel_path) } else { f.rel_path.as_str() };
            on_file(name);
            let tf = client.create_file(&transfer.id, name, f.size.max(1)).await.with_context(|| format!("creating file {name}"))?;
            let chunk = tf.chunk_size.max(1);
            let mut urls: std::collections::HashMap<u32, String> = tf.parts.iter().map(|p| (p.id, p.url.clone())).collect();
            // Fetch any missing URLs up-front in batches of 50.
            let missing: Vec<u32> = (1..=tf.parts_count).filter(|i| !urls.contains_key(i)).collect();
            for batch in missing.chunks(50) {
                for p in client.more_parts(&transfer.id, &tf.id, batch).await? {
                    urls.insert(p.id, p.url);
                }
            }
            let path = f.abs_path.clone();
            let size = f.size;
            let results: Vec<Result<DonePart>> = futures::stream::iter(1..=tf.parts_count)
                .map(|n| {
                    let client = client.clone();
                    let url = urls.get(&n).cloned();
                    let path = path.clone();
                    let progress = progress.clone();
                    async move {
                        let url = url.ok_or_else(|| anyhow!("no URL for part {n}"))?;
                        let offset = (n as u64 - 1) * chunk;
                        let len = if n == tf.parts_count { size - offset } else { chunk };
                        // CRC32 of the part (Smash requires it).
                        let crc = crc32_of(&path, offset, len).await?;
                        let mut attempt = 0;
                        loop {
                            attempt += 1;
                            let p2 = progress.clone();
                            let stream = file_stream(&path, offset, len, move |k| p2(k)).await?;
                            match client.put_part(&url, reqwest::Body::wrap_stream(stream), len).await {
                                Ok(etag) => return Ok(DonePart { id: n, etag, crc32: crc }),
                                Err(e) if attempt < MAX_PART_RETRIES => {
                                    tracing::warn!(part = n, attempt, "Smash part failed, retrying: {e:#}");
                                    tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
                                }
                                Err(e) => return Err(e),
                            }
                        }
                    }
                })
                .buffer_unordered(parallel)
                .collect()
                .await;
            let mut parts = results.into_iter().collect::<Result<Vec<_>>>()?;
            parts.sort_by_key(|p| p.id);
            client.update_file(&transfer.id, &tf.id, &parts).await.with_context(|| format!("finalising {name}"))?;
        }
        Ok(())
    }
    .await;
    if let Err(e) = result {
        let _ = client.delete_transfer(&transfer.id).await;
        return Err(e);
    }
    let locked = client.lock(&transfer.id).await.context("locking Smash transfer")?;
    Ok(UploadOutcome {
        url: if locked.transfer_url.is_empty() { transfer.transfer_url.clone() } else { locked.transfer_url.clone() },
        id: locked.id.clone(),
        kind: "smash_transfer".into(),
        owner_token: None,
        size: total,
        files: files.len(),
        expires_at: locked.availability_end.clone(),
        password_protected: opts.password.is_some(),
    })
}

async fn crc32_of(path: &Path, offset: u64, len: u64) -> Result<u32> {
    let mut f = tokio::fs::File::open(path).await?;
    f.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut h = crc32fast::Hasher::new();
    let mut left = len;
    let mut buf = vec![0u8; 1 << 20];
    while left > 0 {
        let want = (left as usize).min(buf.len());
        let n = f.read(&mut buf[..want]).await?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        left -= n as u64;
    }
    Ok(h.finalize())
}
