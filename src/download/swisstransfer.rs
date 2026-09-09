//! SwissTransfer downloader (100 % Rust; the original Python helper was retired).
//!
//! 1. `GET /dl/{uuid}` → Inertia page; `<script data-page="app" type="application/json">`
//!    (or `<div id="app" data-page="…">`) holds `{component, props:{transfer:{files[]}}}`.
//! 2. Password: `POST /dl/{uuid}` `{password}` with `X-Inertia`, `X-Inertia-Version`,
//!    `X-XSRF-TOKEN` (from cookie `SWISSTRANSFER-API-XSRF-TOKEN`). A correct password
//!    answers `200 application/json` with `component: "link/show"`; a wrong one answers
//!    `302 → GET /dl/{uuid}` whose page is `link/password` with
//!    `props.errors.password = "Contraseña incorrecta"`. The unlocked state lives in the
//!    `ST_SESSION` cookie, so the same client must be used for step 3.
//! 3. `GET /api/1/links/{uuid}/files/{fileId}` (Accept: application/json) →
//!    `{result:"success", data:{url}}` presigned S3 URL, valid 1 h; supports `Range`.
//!    Without the session cookie it answers `403 access_denied`.
//!
//! Verified against the live site on 2026-09-09 (password-protected link).

use super::http::{ProgressFn, browser_client, download_resumable};
use crate::fsutil::destination_path;
use anyhow::{Context, Result, anyhow, bail};
use regex::Regex;
use serde::Serialize;
use std::path::{Path, PathBuf};

pub const BASE_URL: &str = "https://www.swisstransfer.com";
const XSRF_COOKIE: &str = "SWISSTRANSFER-API-XSRF-TOKEN";

#[derive(Debug, Clone, Serialize)]
pub struct StFile {
    pub id: String,
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StTransfer {
    pub link_id: String,
    pub title: Option<String>,
    pub message: Option<String>,
    pub total_size: u64,
    pub expires_at: Option<i64>,
    pub files: Vec<StFile>,
}

/// The link is protected and no password was supplied.
#[derive(Debug, thiserror::Error)]
#[error("this transfer is password protected")]
pub struct PasswordRequired;

/// The supplied password was rejected by SwissTransfer.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct WrongPassword(pub String);

pub fn extract_link_id(s: &str) -> Option<String> {
    let re = Regex::new(r"(?i)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").ok()?;
    re.find(s).map(|m| m.as_str().to_lowercase())
}

pub fn is_swisstransfer_url(s: &str) -> bool {
    s.contains("swisstransfer.com") || (extract_link_id(s).is_some() && !s.contains("://"))
}

pub struct SwissTransferClient {
    client: reqwest::Client,
    inertia_version: Option<String>,
    last_xsrf: Option<String>,
}

impl SwissTransferClient {
    pub fn new() -> Result<Self> {
        Ok(Self { client: browser_client()?, inertia_version: None, last_xsrf: None })
    }

    fn parse_inertia_page(html: &str) -> Result<serde_json::Value> {
        let re1 = Regex::new(r#"(?s)<script[^>]+data-page="app"[^>]*type="application/json"[^>]*>(.*?)</script>"#)?;
        if let Some(c) = re1.captures(html) {
            return serde_json::from_str(&c[1]).context("invalid inertia JSON");
        }
        let re2 = Regex::new(r#"id="app"[^>]*data-page="([^"]+)""#)?;
        if let Some(c) = re2.captures(html) {
            let raw = html_unescape(&c[1]);
            return serde_json::from_str(&raw).context("invalid inertia JSON (legacy)");
        }
        bail!("could not read the SwissTransfer page (format changed?)")
    }

    async fn fetch_page(&mut self, link_id: &str) -> Result<serde_json::Value> {
        let r = self.client.get(format!("{BASE_URL}/dl/{link_id}")).header(reqwest::header::ACCEPT_LANGUAGE, "es-ES,es;q=0.9,en;q=0.8").send().await?;
        if r.status().as_u16() == 404 || r.url().path().contains("not_found") {
            bail!("link not found: it may have expired, been deleted or reached its download limit");
        }
        if r.status().as_u16() >= 400 {
            bail!("SwissTransfer responded HTTP {}", r.status());
        }
        // Capture the XSRF cookie value (needed for the password POST).
        for v in r.headers().get_all(reqwest::header::SET_COOKIE).iter() {
            if let Ok(c) = v.to_str() {
                if let Some(rest) = c.strip_prefix(&format!("{XSRF_COOKIE}=")).or_else(|| c.strip_prefix("XSRF-TOKEN=")) {
                    let raw = rest.split(';').next().unwrap_or("");
                    self.last_xsrf = Some(percent_encoding::percent_decode_str(raw).decode_utf8_lossy().to_string());
                }
            }
        }
        let html = r.text().await?;
        let page = Self::parse_inertia_page(&html)?;
        self.inertia_version = page.get("version").and_then(|v| v.as_str()).map(str::to_string);
        Ok(page)
    }

    fn xsrf_token(&self, _link_id: &str) -> Option<String> {
        self.last_xsrf.clone()
    }

    async fn submit_password(&mut self, link_id: &str, password: &str) -> Result<serde_json::Value> {
        let mut req = self
            .client
            .post(format!("{BASE_URL}/dl/{link_id}"))
            .header(reqwest::header::ACCEPT, "text/html, application/xhtml+xml")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("X-Inertia", "true")
            .header(reqwest::header::REFERER, format!("{BASE_URL}/dl/{link_id}"))
            .header(reqwest::header::ORIGIN, BASE_URL)
            .json(&serde_json::json!({ "password": password }));
        if let Some(v) = &self.inertia_version {
            req = req.header("X-Inertia-Version", v);
        }
        if let Some(x) = self.xsrf_token(link_id) {
            req = req.header("X-XSRF-TOKEN", x);
        }
        let mut r = req.send().await?;
        if r.status().as_u16() == 409 {
            self.inertia_version = None;
            let mut req2 = self
                .client
                .post(format!("{BASE_URL}/dl/{link_id}"))
                .header(reqwest::header::ACCEPT, "text/html, application/xhtml+xml")
                .header("X-Requested-With", "XMLHttpRequest")
                .header("X-Inertia", "true")
                .header(reqwest::header::REFERER, format!("{BASE_URL}/dl/{link_id}"))
                .header(reqwest::header::ORIGIN, BASE_URL)
                .json(&serde_json::json!({ "password": password }));
            if let Some(x) = self.xsrf_token(link_id) {
                req2 = req2.header("X-XSRF-TOKEN", x);
            }
            r = req2.send().await?;
        }
        match r.status().as_u16() {
            401 | 403 | 422 => return Err(WrongPassword("Contraseña incorrecta".into()).into()),
            s if s >= 400 => bail!("HTTP {s} submitting password"),
            _ => {}
        }
        let ct = r.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        if ct.contains("application/json") {
            Ok(r.json().await?)
        } else {
            Self::parse_inertia_page(&r.text().await?)
        }
    }

    pub async fn get_transfer(&mut self, link: &str, password: Option<&str>) -> Result<StTransfer> {
        let link_id = extract_link_id(link).ok_or_else(|| anyhow!("no SwissTransfer id found in: {link}"))?;
        let mut page = self.fetch_page(&link_id).await?;
        let mut component = page.get("component").and_then(|c| c.as_str()).unwrap_or("").to_string();
        if component.ends_with("password") {
            let pw = password.filter(|p| !p.is_empty()).ok_or(PasswordRequired)?;
            page = self.submit_password(&link_id, pw).await?;
            component = page.get("component").and_then(|c| c.as_str()).unwrap_or("").to_string();
            if component.ends_with("password") {
                let msg = page.pointer("/props/errors/password").and_then(|v| v.as_str()).unwrap_or("Contraseña incorrecta");
                return Err(WrongPassword(msg.to_string()).into());
            }
        }
        Self::transfer_from_page(link_id, &component, &page)
    }

    /// Build the transfer description from an unlocked Inertia page.
    fn transfer_from_page(link_id: String, component: &str, page: &serde_json::Value) -> Result<StTransfer> {
        if component.ends_with("not-found") {
            bail!("link not found or expired");
        }
        if component.contains("unavailable") || component.contains("expired") {
            bail!("the transfer is no longer available (expired or limit reached)");
        }
        if component.contains("infected") {
            bail!("SwissTransfer's antivirus flagged this transfer as infected");
        }
        if component.contains("antivirus") || component.contains("waiting") {
            bail!("the transfer is still pending antivirus scan; try later");
        }
        let transfer = page.pointer("/props/transfer").ok_or_else(|| anyhow!("unexpected SwissTransfer response (component '{component}')"))?;
        anyhow::ensure!(transfer.is_object(), "unexpected SwissTransfer response (component '{component}')");
        let files: Vec<StFile> = transfer
            .get("files")
            .and_then(|f| f.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|f| {
                        let id = f.get("id").and_then(|v| v.as_str())?.to_string();
                        let path = f
                            .get("path")
                            .and_then(|v| v.as_str())
                            .or_else(|| f.get("name").and_then(|v| v.as_str()))
                            .unwrap_or(&id)
                            .to_string();
                        let size = f.get("size").and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0);
                        Some(StFile { id, path, size })
                    })
                    .collect()
            })
            .unwrap_or_default();
        anyhow::ensure!(!files.is_empty(), "transfer has no files");
        let sum: u64 = files.iter().map(|f| f.size).sum();
        Ok(StTransfer {
            link_id,
            title: transfer.get("title").and_then(|v| v.as_str()).map(str::to_string),
            message: transfer.get("message").and_then(|v| v.as_str()).map(str::to_string),
            total_size: transfer.get("total_size").and_then(|v| v.as_u64()).unwrap_or(sum),
            expires_at: transfer.get("expires_at").and_then(|v| v.as_i64()),
            files,
        })
    }

    pub async fn download_url(&self, link_id: &str, file_id: &str) -> Result<String> {
        let r = self
            .client
            .get(format!("{BASE_URL}/api/1/links/{link_id}/files/{file_id}"))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::REFERER, format!("{BASE_URL}/dl/{link_id}"))
            .header("X-Requested-With", "XMLHttpRequest")
            .send()
            .await?;
        if r.status().as_u16() == 429 {
            bail!("too many requests (429); wait a moment");
        }
        if r.status().as_u16() >= 400 {
            bail!("could not get download URL (HTTP {})", r.status());
        }
        let v: serde_json::Value = r.json().await?;
        let url = match v.get("data") {
            Some(serde_json::Value::Object(m)) => m.get("url").and_then(|u| u.as_str()),
            Some(serde_json::Value::String(s)) => Some(s.as_str()),
            _ => None,
        };
        url.map(str::to_string).ok_or_else(|| anyhow!("SwissTransfer returned no valid download URL"))
    }

    pub async fn download_all(
        &self,
        t: &StTransfer,
        dest_dir: &Path,
        force: bool,
        progress: ProgressFn,
        on_file: impl Fn(&str),
    ) -> Result<Vec<PathBuf>> {
        let mut saved = Vec::new();
        for f in &t.files {
            on_file(&f.path);
            let dest = destination_path(dest_dir, &f.path, force);
            let url = self.download_url(&t.link_id, &f.id).await?;
            download_resumable(&self.client, &url, &dest, Some(f.size).filter(|s| *s > 0), progress.clone(), 3).await?;
            saved.push(dest);
        }
        Ok(saved)
    }
}

fn html_unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&#039;", "'").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_id_extraction() {
        assert_eq!(
            extract_link_id("https://www.swisstransfer.com/dl/01A061BE-81b3-739f-a19e-e8853a7638d6").as_deref(),
            Some("01a061be-81b3-739f-a19e-e8853a7638d6")
        );
        assert!(extract_link_id("https://storage.to/abc").is_none());
        assert!(is_swisstransfer_url("01a061be-81b3-739f-a19e-e8853a7638d6"));
    }

    #[test]
    fn inertia_parse_both_formats() {
        let modern = r#"<html><script data-page="app" type="application/json">{"component":"link/show","props":{"transfer":{"id":"t","files":[{"id":"f1","path":"a/b.txt","size":3}]}}}</script></html>"#;
        let p = SwissTransferClient::parse_inertia_page(modern).unwrap();
        assert_eq!(p["component"], "link/show");
        let legacy = r#"<div id="app" data-page="{&quot;component&quot;:&quot;link/password&quot;,&quot;props&quot;:{}}"></div>"#;
        let p = SwissTransferClient::parse_inertia_page(legacy).unwrap();
        assert_eq!(p["component"], "link/password");
    }

    /// Shape of the JSON answered by `POST /dl/{id}` with the right password
    /// (captured live on 2026-09-09; promotions trimmed).
    const UNLOCKED: &str = r#"{"component":"link/show","props":{"errors":{},"user":null,"lang":"es_ES",
      "link":{"id":"01a086f1-7dcf-71e9-b8b5-6c1dc79c8e6e"},
      "transfer":{"id":"01a086f1-7dcd-7356-bad2-f7b6467c2d44","title":null,"message":null,"total_size":5,
        "expires_at":1789059600,"files":[{"id":"01a086f1-7dd2-7336-92d9-f7ca05d69083","path":"test.txt","size":5,"mime_type":"text/plain"}],"type":"link"}},
      "url":"/dl/01a086f1-7dcf-71e9-b8b5-6c1dc79c8e6e","version":"5344225efc0e48d7f53f62005fd4733e"}"#;

    #[test]
    fn unlocked_page_to_transfer() {
        let page: serde_json::Value = serde_json::from_str(UNLOCKED).unwrap();
        let t = SwissTransferClient::transfer_from_page("01a086f1-7dcf-71e9-b8b5-6c1dc79c8e6e".into(), "link/show", &page).unwrap();
        assert_eq!(t.files.len(), 1);
        assert_eq!(t.files[0].path, "test.txt");
        assert_eq!(t.files[0].id, "01a086f1-7dd2-7336-92d9-f7ca05d69083");
        assert_eq!(t.total_size, 5);
        assert_eq!(t.expires_at, Some(1789059600));
        assert!(t.title.is_none());
    }

    #[test]
    fn password_component_states() {
        let page: serde_json::Value = serde_json::from_str(r#"{"component":"link/password","props":{"errors":{"password":"Contraseña incorrecta"}}}"#).unwrap();
        assert_eq!(page.pointer("/props/errors/password").and_then(|v| v.as_str()), Some("Contraseña incorrecta"));
        let page: serde_json::Value = serde_json::from_str(r#"{"component":"link/not-found","props":{}}"#).unwrap();
        let err = SwissTransferClient::transfer_from_page("x".into(), "link/not-found", &page).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    /// Live check against a real password-protected link. Opt-in: needs network and
    /// the link to still exist. `UNI_SHARE_ST_LINK` / `UNI_SHARE_ST_PASSWORD` override the
    /// defaults. Run with `cargo test --lib swisstransfer -- --ignored`.
    #[tokio::test]
    #[ignore = "network: live SwissTransfer link"]
    async fn live_password_protected_download() {
        let link = std::env::var("UNI_SHARE_ST_LINK").unwrap_or_else(|_| "https://www.swisstransfer.com/dl/01a086f1-7dcf-71e9-b8b5-6c1dc79c8e6e".into());
        let pw = std::env::var("UNI_SHARE_ST_PASSWORD").unwrap_or_else(|_| "test1234".into());
        let mut c = SwissTransferClient::new().unwrap();
        // Without password → typed error.
        match c.get_transfer(&link, None).await {
            Err(e) if e.downcast_ref::<PasswordRequired>().is_some() => {}
            Err(e) if e.to_string().contains("not found") || e.to_string().contains("no longer available") => {
                eprintln!("live link gone ({e}); skipping");
                return;
            }
            other => panic!("expected PasswordRequired, got {other:?}"),
        }
        // Wrong password → typed error.
        let mut c = SwissTransferClient::new().unwrap();
        let err = c.get_transfer(&link, Some("definitely-wrong")).await.unwrap_err();
        assert!(err.downcast_ref::<WrongPassword>().is_some(), "expected WrongPassword, got {err:#}");
        // Right password → transfer + presigned URL + bytes.
        let mut c = SwissTransferClient::new().unwrap();
        let t = c.get_transfer(&link, Some(&pw)).await.unwrap();
        assert!(!t.files.is_empty());
        let dir = tempfile::tempdir().unwrap();
        let saved = c.download_all(&t, dir.path(), true, std::sync::Arc::new(|_| {}), |_| {}).await.unwrap();
        assert_eq!(saved.len(), t.files.len());
        for (f, p) in t.files.iter().zip(&saved) {
            assert_eq!(std::fs::metadata(p).unwrap().len(), f.size, "size of {}", f.path);
        }
    }
}
