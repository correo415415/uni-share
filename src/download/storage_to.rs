//! storage.to downloader: single files (`/{id}`) and collections (`/c/{id}`).
//!
//! Flow (reverse-engineered from the web client, verified live):
//! 1. `GET https://storage.to/{id}` with a browser UA → HTML containing the
//!    React-Router loader data with `mint_proof`, filename, size, files[]…
//! 2. File: `GET /{id}/download` with `Accept: application/json` and
//!    `x-mint-proof: <proof>` → `{url}` (CDN URL, supports `Range`).
//!    Collection: `POST /c/{id}/urls {file_ids:[…]}` → `{data:{urls:{id:url}}}`.
//! 3. Password-protected shares: `POST /api/file|collection/{id}/verify-password`.

use super::http::{ProgressFn, browser_client, download_resumable};
use crate::fsutil::destination_path;
use crate::global::storage_to::{ResourceKind, parse_share_url};
use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub const SITE: &str = "https://storage.to";

#[derive(Debug, Clone, Serialize)]
pub struct RemoteFile {
    pub id: String,
    pub name: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShareInfo {
    pub kind: String,
    pub id: String,
    pub title: Option<String>,
    pub files: Vec<RemoteFile>,
    pub total_size: u64,
    pub expires_at: Option<String>,
    pub password_protected: bool,
    #[serde(skip)]
    pub mint_proof: String,
}

pub struct StorageDownloader {
    client: reqwest::Client,
}

impl Default for StorageDownloader {
    fn default() -> Self {
        Self::new().expect("http client")
    }
}

impl StorageDownloader {
    pub fn new() -> Result<Self> {
        Ok(Self { client: browser_client()? })
    }

    pub async fn info(&self, url_or_id: &str) -> Result<ShareInfo> {
        let (kind, id) = parse_share_url(url_or_id).ok_or_else(|| anyhow!("not a storage.to link: {url_or_id}"))?;
        let page_url = match kind {
            ResourceKind::File => format!("{SITE}/{id}"),
            ResourceKind::Collection => format!("{SITE}/c/{id}"),
        };
        let resp = self.client.get(&page_url).header(reqwest::header::ACCEPT, "text/html").send().await.context("fetching share page")?;
        if resp.status().as_u16() == 404 {
            bail!("link not found or expired: {url_or_id}");
        }
        let html = resp.error_for_status()?.text().await?;
        let data = extract_loader_data(&html).ok_or_else(|| anyhow!("could not parse storage.to page (site changed?)"))?;
        parse_share(kind, &id, &data)
    }

    pub async fn verify_password(&self, info: &ShareInfo, password: &str) -> Result<()> {
        let path = if info.kind == "collection" { "collection" } else { "file" };
        let r = self
            .client
            .post(format!("{SITE}/api/{path}/{}/verify-password", info.id))
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&serde_json::json!({ "password": password }))
            .send()
            .await?;
        match r.status().as_u16() {
            200 => Ok(()),
            401 => bail!("incorrect password"),
            s => bail!("password verification failed (HTTP {s})"),
        }
    }

    /// Resolve CDN download URLs for every file in the share.
    pub async fn resolve_urls(&self, info: &ShareInfo) -> Result<Vec<(RemoteFile, String)>> {
        let mut out = Vec::new();
        if info.kind == "collection" {
            let ids: Vec<&str> = info.files.iter().map(|f| f.id.as_str()).collect();
            for chunk in ids.chunks(50) {
                let r = self
                    .client
                    .post(format!("{SITE}/c/{}/urls", info.id))
                    .header(reqwest::header::ACCEPT, "application/json")
                    .header("x-mint-proof", &info.mint_proof)
                    .header(reqwest::header::REFERER, format!("{SITE}/c/{}", info.id))
                    .json(&serde_json::json!({ "file_ids": chunk }))
                    .send()
                    .await?;
                let v: serde_json::Value = check_json(r).await?;
                let urls = v.pointer("/data/urls").or_else(|| v.get("urls")).cloned().unwrap_or_default();
                for f in info.files.iter().filter(|f| chunk.contains(&f.id.as_str())) {
                    let u = urls.get(&f.id).and_then(|x| x.as_str()).ok_or_else(|| anyhow!("no download url for {}", f.name))?;
                    out.push((f.clone(), u.to_string()));
                }
            }
        } else {
            let r = self
                .client
                .get(format!("{SITE}/{}/download", info.id))
                .header(reqwest::header::ACCEPT, "application/json")
                .header("x-mint-proof", &info.mint_proof)
                .header(reqwest::header::REFERER, format!("{SITE}/{}", info.id))
                .send()
                .await?;
            let v: serde_json::Value = check_json(r).await?;
            let u = v.pointer("/data/url").or_else(|| v.get("url")).and_then(|x| x.as_str()).ok_or_else(|| anyhow!("no download url returned"))?;
            let f = info.files.first().cloned().ok_or_else(|| anyhow!("no file in share"))?;
            out.push((f, u.to_string()));
        }
        Ok(out)
    }

    /// Download every file into `dest_dir` (collections keep relative paths).
    pub async fn download_all(
        &self,
        info: &ShareInfo,
        dest_dir: &Path,
        force: bool,
        progress: ProgressFn,
        on_file: impl Fn(&str),
    ) -> Result<Vec<PathBuf>> {
        let pairs = self.resolve_urls(info).await?;
        let mut saved = Vec::new();
        for (f, url) in pairs {
            on_file(&f.name);
            let dest = destination_path(dest_dir, &f.name, force);
            download_resumable(&self.client, &url, &dest, Some(f.size).filter(|s| *s > 0), progress.clone(), 3).await?;
            saved.push(dest);
        }
        Ok(saved)
    }
}

async fn check_json(r: reqwest::Response) -> Result<serde_json::Value> {
    let status = r.status();
    let text = r.text().await?;
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    if !status.is_success() {
        let code = v.pointer("/error/code").and_then(|c| c.as_str()).unwrap_or("");
        let msg = v.pointer("/error/message").and_then(|c| c.as_str()).or_else(|| v.get("error").and_then(|c| c.as_str())).unwrap_or(&text);
        match code {
            "password_required" | "password" => bail!("this share is password protected (use --password)"),
            "challenge_required" | "turnstile_required" => bail!("storage.to requires a browser captcha for this download; open the link in a browser"),
            _ => bail!("storage.to download failed (HTTP {status}): {msg}"),
        }
    }
    Ok(v)
}

/// The page embeds `window.__reactRouterContext.streamController.enqueue("<json>")`
/// — a "turbo-stream" flat array where objects reference indices. We decode
/// the JS string and walk the array to find our keys.
pub fn extract_loader_data(html: &str) -> Option<serde_json::Value> {
    let mut best: Option<serde_json::Value> = None;
    for (start, _) in html.match_indices("streamController.enqueue(\"") {
        let s = &html[start + "streamController.enqueue(\"".len()..];
        let end = find_js_string_end(s)?;
        let raw = &s[..end];
        let decoded: String = serde_json::from_str(&format!("\"{raw}\"")).ok()?;
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&decoded) {
            if v.is_array() {
                // prefer the chunk that has mint_proof
                if decoded.contains("mint_proof") || best.is_none() {
                    best = Some(v);
                }
            }
        }
    }
    best
}

fn find_js_string_end(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// Resolve turbo-stream references: an object `{"_k": v}` means key = arr[k], value = arr[v] (negative = special).
fn resolve(arr: &[serde_json::Value], v: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value::*;
    match v {
        Object(m) => {
            let mut out = serde_json::Map::new();
            for (k, val) in m {
                let key = k.strip_prefix('_').and_then(|n| n.parse::<usize>().ok()).and_then(|i| arr.get(i)).and_then(|x| x.as_str().map(str::to_string));
                let Some(key) = key else { continue };
                let value = match val {
                    Number(n) => match n.as_i64() {
                        Some(i) if i >= 0 => arr.get(i as usize).map(|x| resolve(arr, x)).unwrap_or(Null),
                        _ => Null, // -5 = undefined, -7 = null-ish
                    },
                    other => resolve(arr, other),
                };
                out.insert(key, value);
            }
            Object(out)
        }
        Array(items) => Array(
            items
                .iter()
                .map(|x| match x {
                    Number(n) => n.as_i64().filter(|i| *i >= 0).and_then(|i| arr.get(i as usize)).map(|y| resolve(arr, y)).unwrap_or(Null),
                    other => resolve(arr, other),
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn parse_share(kind: ResourceKind, id: &str, data: &serde_json::Value) -> Result<ShareInfo> {
    let arr = data.as_array().ok_or_else(|| anyhow!("unexpected page data"))?;
    let root = resolve(arr, &arr[0]);
    // root.loaderData["routes/file"|"routes/collection"].view
    let loader = root.get("loaderData").cloned().unwrap_or_default();
    let view = loader
        .as_object()
        .and_then(|m| m.values().find_map(|r| r.get("view").cloned()))
        .ok_or_else(|| anyhow!("no view data in page"))?;
    let g = |k: &str| view.get(k).cloned().unwrap_or(serde_json::Value::Null);
    let state = g("state").as_str().unwrap_or("").to_string();
    if state == "pending" {
        bail!("the upload for this link is still pending");
    }
    if state == "unavailable" || state == "expired" || state == "deleted" {
        bail!("this link is no longer available ({state})");
    }
    let mint_proof = g("mint_proof").as_str().unwrap_or("").to_string();
    let password_protected = g("is_password_protected").as_bool().unwrap_or(false);
    let expires_at = g("expires_at").as_str().map(str::to_string);
    let mut files = Vec::new();
    match kind {
        ResourceKind::File => {
            files.push(RemoteFile {
                id: id.to_string(),
                name: g("filename").as_str().unwrap_or("file").to_string(),
                size: g("size").as_u64().unwrap_or(0),
            });
        }
        ResourceKind::Collection => {
            for f in g("files").as_array().cloned().unwrap_or_default() {
                files.push(RemoteFile {
                    id: f.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    name: f.get("filename").and_then(|x| x.as_str()).unwrap_or("file").to_string(),
                    size: f.get("size").and_then(|x| x.as_u64()).unwrap_or(0),
                });
            }
        }
    }
    anyhow::ensure!(!files.is_empty(), "no files found in share");
    Ok(ShareInfo {
        kind: if kind == ResourceKind::Collection { "collection".into() } else { "file".into() },
        id: id.to_string(),
        title: g("title").as_str().map(str::to_string),
        total_size: g("total_size").as_u64().unwrap_or_else(|| files.iter().map(|f| f.size).sum()),
        files,
        expires_at,
        password_protected,
        mint_proof,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE_PAGE: &str = r##"<script>window.__reactRouterContext.streamController.enqueue("[{\"_1\":2,\"_3\":-5,\"_4\":-5},\"loaderData\",{\"_5\":6,\"_7\":8},\"actionData\",\"errors\",\"root\",{\"_51\":52},\"routes/file\",{\"_9\":10,\"_11\":12},\"kind\",\"file\",\"view\",{\"_13\":14,\"_15\":16,\"_17\":18,\"_19\":20,\"_21\":22,\"_23\":24,\"_25\":26,\"_27\":-5,\"_28\":29},\"turnstileSiteKey\",\"0x4\",\"id\",\"XJRJIcIeY\",\"state\",\"ready\",\"filename\",\"t.txt\",\"size\",50,\"human_size\",\"50 B\",\"downloads\",0,\"max_downloads\",\"expires_at\",\"2026-09-10T20:16:16+00:00\",\"is_password_protected\",false,\"mint_proof\",\"1788812498.aca2\",\"locale\",\"en\"]\n");</script>"##;

    #[test]
    fn parses_file_page() {
        let data = extract_loader_data(FILE_PAGE).unwrap();
        let info = parse_share(ResourceKind::File, "XJRJIcIeY", &data).unwrap();
        assert_eq!(info.files[0].name, "t.txt");
        assert_eq!(info.files[0].size, 50);
        assert_eq!(info.mint_proof, "1788812498.aca2");
        assert_eq!(info.expires_at.as_deref(), Some("2026-09-10T20:16:16+00:00"));
    }

    const COLL_PAGE: &str = r##"enqueue("[{\"_1\":2},\"loaderData\",{\"_7\":8},\"actionData\",\"errors\",\"root\",{},\"routes/collection\",{\"_9\":10},\"view\",{\"_13\":14,\"_15\":16,\"_17\":18,\"_36\":37},\"turnstileSiteKey\",\"0x\",\"id\",\"087Gbyt8p\",\"state\",\"ready\",\"mint_proof\",\"1788812870.8dd\",\"x\",\"y\",\"a\",\"b\",\"c\",\"d\",\"e\",\"f\",\"g\",\"h\",\"i\",\"j\",\"k\",\"l\",\"m\",\"n\",\"o\",\"files\",[38,39],{\"_13\":50,\"_41\":51,\"_43\":52},{\"_13\":40,\"_41\":42,\"_43\":44},\"ZzPb6JEqf\",\"filename\",\"b.txt\",\"size\",11,\"human_size\",\"11 B\",\"file_type\",\"file\",\"hgRjMXtwp\",\"sub/dir/a.txt\",19]\n");"##;

    #[test]
    fn parses_collection_page() {
        let data = extract_loader_data(COLL_PAGE).unwrap();
        let info = parse_share(ResourceKind::Collection, "087Gbyt8p", &data).unwrap();
        assert_eq!(info.files.len(), 2);
        assert_eq!(info.files[0].name, "sub/dir/a.txt");
        assert_eq!(info.files[0].size, 19);
        assert_eq!(info.files[1].name, "b.txt");
        assert_eq!(info.total_size, 30);
    }
}
