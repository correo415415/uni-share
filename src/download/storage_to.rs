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
    /// Cookie jar file for the curl fallback (password cookies `pwok_*`).
    cookie_jar: std::path::PathBuf,
    /// Set once reqwest was challenged by Cloudflare; then always use curl.
    use_curl: std::sync::atomic::AtomicBool,
}

impl StorageDownloader {
    pub fn new() -> Result<Self> {
        let jar = std::env::temp_dir().join(format!("uni-share-cookies-{}.txt", std::process::id()));
        Ok(Self { client: browser_client()?, cookie_jar: jar, use_curl: std::sync::atomic::AtomicBool::new(false) })
    }

    /// storage.to sits behind Cloudflare, whose bot heuristics challenge
    /// rustls-based clients (TLS fingerprint) on the *site* routes, while the
    /// public `/api` is fine. `curl` (bundled with Linux, macOS and Windows
    /// 10+) is not challenged, so we try reqwest first and transparently fall
    /// back to curl (with a cookie jar) for the whole session afterwards.
    async fn site_request(&self, method: &str, url: &str, headers: &[(&str, String)], body: Option<String>) -> Result<(u16, String)> {
        use std::sync::atomic::Ordering;
        if !self.use_curl.load(Ordering::Relaxed) {
            let m = reqwest::Method::from_bytes(method.as_bytes()).context("method")?;
            let mut rb = self.client.request(m, url);
            for (k, v) in headers {
                rb = rb.header(*k, v.as_str());
            }
            if let Some(b) = &body {
                rb = rb.header(reqwest::header::CONTENT_TYPE, "application/json").body(b.clone());
            }
            match rb.send().await {
                Ok(r) => {
                    let cf = r.headers().get("cf-mitigated").is_some()
                        || (r.status().as_u16() == 403 && r.headers().get("server").and_then(|v| v.to_str().ok()) == Some("cloudflare"));
                    if !cf {
                        let st = r.status().as_u16();
                        return Ok((st, r.text().await?));
                    }
                    tracing::debug!(url, "Cloudflare challenge from reqwest, switching to curl");
                }
                Err(e) => tracing::debug!(url, "reqwest error ({e}), switching to curl"),
            }
            self.use_curl.store(true, Ordering::Relaxed);
        }
        curl_request(method, url, headers, body.as_deref(), &self.cookie_jar).await
    }

    async fn fetch_page_html(&self, page_url: &str) -> Result<String> {
        let (status, html) = self
            .site_request("GET", page_url, &[("Accept", "text/html,application/xhtml+xml,*/*;q=0.8".into()), ("Accept-Language", "en-US,en;q=0.9".into())], None)
            .await?;
        if status == 404 || html.contains("<title>Page not found") {
            bail!("link not found or expired: {page_url}");
        }
        if !(200..300).contains(&status) || html.contains("Just a moment...") {
            bail!("storage.to blocked the request (HTTP {status}); open {page_url} in a browser instead");
        }
        Ok(html)
    }

    pub async fn info(&self, url_or_id: &str) -> Result<ShareInfo> {
        let (kind, id) = parse_share_url(url_or_id).ok_or_else(|| anyhow!("not a storage.to link: {url_or_id}"))?;
        let page_url = match kind {
            ResourceKind::File => format!("{SITE}/{id}"),
            ResourceKind::Collection => format!("{SITE}/c/{id}"),
        };
        let html = self.fetch_page_html(&page_url).await?;
        let data = extract_loader_data(&html).ok_or_else(|| anyhow!("could not parse storage.to page (site changed?)"))?;
        parse_share(kind, &id, &data)
    }

    /// Verify the password; the server answers with a `pwok_*` cookie that
    /// authorises the subsequent URL requests (kept in the client/curl jar).
    pub async fn verify_password(&self, info: &ShareInfo, password: &str) -> Result<()> {
        let path = if info.kind == "collection" { "collection" } else { "file" };
        let url = format!("{SITE}/api/{path}/{}/verify-password", info.id);
        let (status, text) = self
            .site_request("POST", &url, &[("Accept", "application/json".into())], Some(serde_json::json!({ "password": password }).to_string()))
            .await?;
        match status {
            200 => Ok(()),
            401 => bail!("incorrect password"),
            s => bail!("password verification failed (HTTP {s}): {}", text.chars().take(200).collect::<String>()),
        }
    }

    /// Resolve CDN download URLs for every file in the share.
    pub async fn resolve_urls(&self, info: &ShareInfo) -> Result<Vec<(RemoteFile, String)>> {
        let mut out = Vec::new();
        if info.kind == "collection" {
            let ids: Vec<&str> = info.files.iter().map(|f| f.id.as_str()).collect();
            for chunk in ids.chunks(50) {
                let (status, text) = self
                    .site_request(
                        "POST",
                        &format!("{SITE}/c/{}/urls", info.id),
                        &[("Accept", "application/json".into()), ("x-mint-proof", info.mint_proof.clone()), ("Referer", format!("{SITE}/c/{}", info.id))],
                        Some(serde_json::json!({ "file_ids": chunk }).to_string()),
                    )
                    .await?;
                let v = check_json(status, &text)?;
                let urls = v.pointer("/data/urls").or_else(|| v.get("urls")).cloned().unwrap_or_default();
                for f in info.files.iter().filter(|f| chunk.contains(&f.id.as_str())) {
                    let u = urls.get(&f.id).and_then(|x| x.as_str()).ok_or_else(|| anyhow!("no download url for {} (wrong password?)", f.name))?;
                    out.push((f.clone(), u.to_string()));
                }
            }
        } else {
            let (status, text) = self
                .site_request(
                    "GET",
                    &format!("{SITE}/{}/download", info.id),
                    &[("Accept", "application/json".into()), ("x-mint-proof", info.mint_proof.clone()), ("Referer", format!("{SITE}/{}", info.id))],
                    None,
                )
                .await?;
            let v = check_json(status, &text)?;
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
            // CDN (cdn.storagetobox.com) is not challenged → plain reqwest with Range resume.
            download_resumable(&self.client, &url, &dest, Some(f.size).filter(|s| *s > 0), progress.clone(), 3).await?;
            saved.push(dest);
        }
        Ok(saved)
    }
}

impl Drop for StorageDownloader {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.cookie_jar);
    }
}

async fn curl_request(method: &str, url: &str, headers: &[(&str, String)], body: Option<&str>, jar: &Path) -> Result<(u16, String)> {
    let mut cmd = tokio::process::Command::new("curl");
    cmd.args(["-sS", "-L", "--max-time", "60", "-A", super::http::BROWSER_UA, "-X", method, "-w", "\n__STATUS__%{http_code}"]);
    cmd.arg("-c").arg(jar).arg("-b").arg(jar);
    for (k, v) in headers {
        cmd.arg("-H").arg(format!("{k}: {v}"));
    }
    if let Some(b) = body {
        cmd.arg("-H").arg("Content-Type: application/json").arg("--data-binary").arg(b);
    }
    cmd.arg(url);
    let out = cmd.output().await.context("running curl (needed to bypass the Cloudflare bot check; install curl)")?;
    if !out.status.success() {
        bail!("curl exited with {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim());
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (body, status) = match text.rsplit_once("\n__STATUS__") {
        Some((b, s)) => (b.to_string(), s.trim().parse::<u16>().unwrap_or(0)),
        None => (text, 0),
    };
    Ok((status, body))
}

fn check_json(status: u16, text: &str) -> Result<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
    if !(200..300).contains(&status) {
        let code = v.pointer("/error/code").and_then(|c| c.as_str()).unwrap_or("");
        let msg = v.pointer("/error/message").and_then(|c| c.as_str()).or_else(|| v.get("error").and_then(|c| c.as_str())).unwrap_or("");
        match code {
            "password_required" | "password" => bail!("this share is password protected (use --password)"),
            "challenge_required" | "turnstile_required" => bail!("storage.to requires a browser captcha for this download; open the link in a browser"),
            _ if text.contains("Just a moment...") => bail!("storage.to (Cloudflare) blocked the request; open the link in a browser"),
            _ => bail!(
                "storage.to download failed (HTTP {status}): {}",
                if msg.is_empty() { text.chars().take(200).collect::<String>() } else { msg.to_string() }
            ),
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

    #[test]
    fn parses_real_file_page() {
        let html = include_str!("../../tests/fixtures_file_page.html");
        let data = extract_loader_data(html).expect("loader data");
        let info = parse_share(ResourceKind::File, "XJRJIcIeY", &data).unwrap();
        assert_eq!(info.files[0].name, "t.txt");
        assert_eq!(info.files[0].size, 50);
        assert!(info.mint_proof.starts_with("17888"), "{}", info.mint_proof);
        assert_eq!(info.expires_at.as_deref(), Some("2026-09-10T20:16:16+00:00"));
        assert!(!info.password_protected);
    }

    #[test]
    fn parses_real_collection_page() {
        let html = include_str!("../../tests/fixtures_coll_page.html");
        let data = extract_loader_data(html).expect("loader data");
        let info = parse_share(ResourceKind::Collection, "087Gbyt8p", &data).unwrap();
        assert_eq!(info.files.len(), 2);
        let names: Vec<&str> = info.files.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"sub/dir/a.txt"));
        assert!(names.contains(&"b.txt"));
        assert_eq!(info.total_size, 30);
        assert!(!info.mint_proof.is_empty());
    }
}
