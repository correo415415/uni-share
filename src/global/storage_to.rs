//! storage.to REST client (https://storage.to/docs/api).
//!
//! Verified live against the API on 2026-09-07:
//! - `POST /upload/init`   → `{type:"single", upload_url, r2_key, headers}` (<50 MB)
//!                         or `{type:"multipart", upload_id, r2_key, part_size, total_parts, initial_urls, owner_token}`
//! - `PUT <presigned R2 url>` (parts return `ETag`)
//! - `POST /upload/complete-multipart {upload_id, parts:[{partNumber, etag}]}`
//! - `POST /upload/confirm` → `{file:{id,url,filename,size,expires_at}, owner_token}`
//! - `POST /collection {expected_file_count}` → `{collection:{id,url,expires_at}, owner_token}`
//! - `POST /file/{id}/password`, `/expiry`, `/max-downloads` (owner token)

use anyhow::{Context, Result, anyhow, bail};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

pub const DEFAULT_API: &str = "https://storage.to/api";
pub const MAX_FILE_SIZE: u64 = 25 * 1024 * 1024 * 1024; // 25 GB
pub const MAX_COLLECTION_SIZE: u64 = 25 * 1024 * 1024 * 1024;

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    /// Separate client without the JSON/visitor headers for R2 PUTs.
    r2: reqwest::Client,
    base: String,
    visitor_token: String,
    bearer: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InitRequest<'a> {
    pub filename: &'a str,
    pub content_type: &'a str,
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InitResponse {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub upload_url: Option<String>,
    #[serde(default)]
    pub upload_id: Option<String>,
    #[serde(default)]
    pub r2_key: String,
    #[serde(default)]
    pub part_size: Option<u64>,
    #[serde(default)]
    pub total_parts: Option<u32>,
    #[serde(default)]
    pub initial_urls: HashMap<String, String>,
    #[serde(default)]
    pub headers: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub owner_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Part {
    #[serde(rename = "partNumber")]
    pub part_number: u32,
    pub etag: String,
}

#[derive(Debug, Serialize)]
pub struct ConfirmRequest<'a> {
    pub filename: &'a str,
    pub size: u64,
    pub content_type: &'a str,
    pub r2_key: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiry_days: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FileInfo {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub human_size: String,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfirmResponse {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
    pub file: Option<FileInfo>,
    #[serde(default)]
    pub owner_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CollectionInfo {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CollectionResponse {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
    pub collection: Option<CollectionInfo>,
    #[serde(default)]
    pub owner_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CollectionStatus {
    #[serde(default)]
    pub files: Vec<CollectionFile>,
    #[serde(default)]
    pub is_uploading: bool,
    #[serde(default)]
    pub file_count: u32,
    #[serde(default)]
    pub total_size: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CollectionFile {
    pub id: String,
    pub filename: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Envelope {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    error: Option<serde_json::Value>,
    #[serde(default)]
    message: Option<String>,
}

impl Client {
    pub fn new(base: &str, visitor_token: &str, bearer: Option<&str>) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(reqwest::header::ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert("X-Visitor-Token", HeaderValue::from_str(visitor_token).context("visitor token header")?);
        if let Some(b) = bearer {
            headers.insert(reqwest::header::AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {b}"))?);
        }
        let http = reqwest::Client::builder()
            .user_agent(crate::user_agent())
            .default_headers(headers)
            .timeout(Duration::from_secs(60))
            .use_preconfigured_tls(platform_tls()?)
            .build()?;
        let r2 = reqwest::Client::builder()
            .user_agent(crate::user_agent())
            .use_preconfigured_tls(platform_tls()?)
            .tcp_nodelay(true)
            .build()?;
        Ok(Self {
            http,
            r2,
            base: base.trim_end_matches('/').to_string(),
            visitor_token: visitor_token.to_string(),
            bearer: bearer.map(str::to_string),
        })
    }

    pub fn visitor_token(&self) -> &str {
        &self.visitor_token
    }

    pub fn r2_client(&self) -> &reqwest::Client {
        &self.r2
    }

    async fn post_json<T: serde::de::DeserializeOwned>(&self, path: &str, body: &impl Serialize, owner: Option<&str>) -> Result<T> {
        let mut rb = self.http.post(format!("{}{}", self.base, path)).json(body);
        if let Some(o) = owner {
            rb = if self.bearer.is_some() { rb.header("X-Owner-Token", o) } else { rb.header(reqwest::header::AUTHORIZATION, format!("Owner {o}")) };
        }
        let resp = rb.send().await.with_context(|| format!("POST {path}"))?;
        parse(resp, path).await
    }

    async fn delete(&self, path: &str, owner: &str) -> Result<()> {
        let mut rb = self.http.delete(format!("{}{}", self.base, path));
        rb = if self.bearer.is_some() { rb.header("X-Owner-Token", owner) } else { rb.header(reqwest::header::AUTHORIZATION, format!("Owner {owner}")) };
        let resp = rb.send().await.with_context(|| format!("DELETE {path}"))?;
        let _: Envelope = parse(resp, path).await?;
        Ok(())
    }

    pub async fn init_upload(&self, filename: &str, content_type: &str, size: u64) -> Result<InitResponse> {
        anyhow::ensure!(size >= 1, "empty files cannot be uploaded to storage.to");
        anyhow::ensure!(size <= MAX_FILE_SIZE, "file exceeds the 25 GB storage.to limit");
        let r: InitResponse = self
            .post_json("/upload/init", &InitRequest { filename, content_type, size }, None)
            .await?;
        if !r.success {
            bail!("init failed: {}", r.error.unwrap_or_default());
        }
        Ok(r)
    }

    pub async fn part_urls(&self, upload_id: &str, owner: Option<&str>, parts: &[u32]) -> Result<HashMap<String, String>> {
        #[derive(Deserialize)]
        struct R {
            #[serde(default)]
            urls: HashMap<String, String>,
        }
        let body = serde_json::json!({ "upload_id": upload_id, "part_numbers": parts });
        let r: R = self.post_json("/upload/parts", &body, owner).await?;
        Ok(r.urls)
    }

    pub async fn complete_multipart(&self, upload_id: &str, owner: Option<&str>, parts: Vec<Part>) -> Result<()> {
        let body = serde_json::json!({ "upload_id": upload_id, "parts": parts });
        let _: Envelope = self.post_json("/upload/complete-multipart", &body, owner).await?;
        Ok(())
    }

    pub async fn abort(&self, upload_id: &str, owner: Option<&str>) -> Result<()> {
        let body = serde_json::json!({ "upload_id": upload_id });
        let _: Envelope = self.post_json("/upload/abort", &body, owner).await?;
        Ok(())
    }

    pub async fn confirm(&self, req: &ConfirmRequest<'_>) -> Result<ConfirmResponse> {
        let r: ConfirmResponse = self.post_json("/upload/confirm", req, None).await?;
        if !r.success || r.file.is_none() {
            bail!("confirm failed: {}", r.error.unwrap_or_else(|| "no file returned".into()));
        }
        Ok(r)
    }

    pub async fn create_collection(&self, expected_files: u32) -> Result<CollectionResponse> {
        let body = serde_json::json!({ "expected_file_count": expected_files });
        let r: CollectionResponse = self.post_json("/collection", &body, None).await?;
        if !r.success || r.collection.is_none() {
            bail!("collection create failed: {}", r.error.unwrap_or_default());
        }
        Ok(r)
    }

    pub async fn collection_status(&self, id: &str) -> Result<CollectionStatus> {
        let resp = self.http.get(format!("{}/collection/{id}/status", self.base)).send().await?;
        parse(resp, "collection status").await
    }

    pub async fn collection_ready(&self, id: &str, owner: &str) -> Result<()> {
        let _: Envelope = self.post_json(&format!("/collection/{id}/ready"), &serde_json::json!({}), Some(owner)).await?;
        Ok(())
    }

    pub async fn set_password(&self, kind: ResourceKind, id: &str, owner: &str, password: &str) -> Result<()> {
        anyhow::ensure!((4..=100).contains(&password.chars().count()), "password must be 4-100 characters");
        let body = serde_json::json!({ "password": password });
        let _: Envelope = self.post_json(&format!("/{}/{id}/password", kind.path()), &body, Some(owner)).await?;
        Ok(())
    }

    pub async fn set_expiry(&self, kind: ResourceKind, id: &str, owner: &str, days: u32) -> Result<()> {
        let body = serde_json::json!({ "days": days });
        let _: Envelope = self.post_json(&format!("/{}/{id}/expiry", kind.path()), &body, Some(owner)).await?;
        Ok(())
    }

    pub async fn set_max_downloads(&self, kind: ResourceKind, id: &str, owner: &str, max: u32) -> Result<()> {
        let body = serde_json::json!({ "max_downloads": max });
        let _: Envelope = self.post_json(&format!("/{}/{id}/max-downloads", kind.path()), &body, Some(owner)).await?;
        Ok(())
    }

    pub async fn delete_resource(&self, kind: ResourceKind, id: &str, owner: &str) -> Result<()> {
        self.delete(&format!("/{}/{id}", kind.path()), owner).await
    }

    /// PUT bytes to a presigned R2 URL; returns the ETag (parts) if any.
    pub async fn put_presigned(
        &self,
        url: &str,
        body: reqwest::Body,
        len: u64,
        content_type: Option<&str>,
    ) -> Result<Option<String>> {
        let mut rb = self
            .r2
            .put(url)
            .header(reqwest::header::CONTENT_LENGTH, len)
            .timeout(Duration::from_secs(60 * 30))
            .body(body);
        if let Some(ct) = content_type {
            rb = rb.header(reqwest::header::CONTENT_TYPE, ct);
        }
        let resp = rb.send().await.context("PUT to R2")?;
        if !resp.status().is_success() {
            let st = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("R2 upload failed ({st}): {}", text.chars().take(300).collect::<String>());
        }
        Ok(resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_matches('"').to_string()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    File,
    Collection,
}

impl ResourceKind {
    fn path(self) -> &'static str {
        match self {
            ResourceKind::File => "file",
            ResourceKind::Collection => "collection",
        }
    }
}

async fn parse<T: serde::de::DeserializeOwned>(resp: reqwest::Response, what: &str) -> Result<T> {
    let status = resp.status();
    let retry_after = resp.headers().get("Retry-After").and_then(|v| v.to_str().ok()).map(str::to_string);
    let text = resp.text().await.unwrap_or_default();
    if status.as_u16() == 429 {
        let msg = serde_json::from_str::<Envelope>(&text).ok().and_then(|e| e.error).map(|e| e.to_string()).unwrap_or_default();
        bail!(
            "storage.to rate limit / quota hit ({what}){}{}",
            retry_after.map(|r| format!(", retry after {r}s")).unwrap_or_default(),
            if msg.is_empty() { String::new() } else { format!(": {msg}") }
        );
    }
    if !status.is_success() {
        let env = serde_json::from_str::<Envelope>(&text).ok();
        let msg = env
            .as_ref()
            .and_then(|e| e.error.clone().map(|v| match v {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            }))
            .or_else(|| env.and_then(|e| e.message))
            .unwrap_or_else(|| text.chars().take(200).collect());
        bail!("storage.to {what} failed (HTTP {status}): {msg}");
    }
    serde_json::from_str::<T>(&text).map_err(|e| anyhow!("parsing {what} response: {e} — body: {}", text.chars().take(300).collect::<String>()))
}

/// rustls config using the OS trust store (rustls-platform-verifier).
pub fn platform_tls() -> Result<rustls::ClientConfig> {
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("tls versions")?
        .with_platform_verifier()
        .context("platform verifier")?
        .with_no_client_auth();
    Ok(cfg)
}

/// Extract a storage.to file or collection id from a URL or bare id.
pub fn parse_share_url(s: &str) -> Option<(ResourceKind, String)> {
    let s = s.trim();
    if let Ok(u) = url::Url::parse(s) {
        let host = u.host_str()?;
        if !host.ends_with("storage.to") {
            return None;
        }
        let segs: Vec<&str> = u.path_segments()?.filter(|p| !p.is_empty()).collect();
        return match segs.as_slice() {
            ["c", id] => Some((ResourceKind::Collection, id.to_string())),
            [id] if is_id(id) => Some((ResourceKind::File, id.to_string())),
            _ => None,
        };
    }
    if is_id(s) {
        return Some((ResourceKind::File, s.to_string()));
    }
    None
}

fn is_id(s: &str) -> bool {
    (6..=16).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_urls() {
        assert_eq!(parse_share_url("https://storage.to/XJRJIcIeY"), Some((ResourceKind::File, "XJRJIcIeY".into())));
        assert_eq!(parse_share_url("https://storage.to/c/087Gbyt8p"), Some((ResourceKind::Collection, "087Gbyt8p".into())));
        assert_eq!(parse_share_url("XJRJIcIeY"), Some((ResourceKind::File, "XJRJIcIeY".into())));
        assert_eq!(parse_share_url("https://example.com/abc"), None);
        assert_eq!(parse_share_url("https://storage.to/docs/api"), None);
    }

    #[test]
    fn init_response_parses_both_shapes() {
        let single = r#"{"success":true,"type":"single","upload_url":"https://r2/x","headers":{"Content-Type":["text/plain"]},"r2_key":"k"}"#;
        let r: InitResponse = serde_json::from_str(single).unwrap();
        assert_eq!(r.kind, "single");
        assert!(r.upload_url.is_some());
        let multi = r#"{"success":true,"type":"multipart","upload_id":"u","r2_key":"k","part_size":33554432,"total_parts":2,"owner_token":"o","initial_urls":{"1":"a","2":"b"}}"#;
        let r: InitResponse = serde_json::from_str(multi).unwrap();
        assert_eq!(r.total_parts, Some(2));
        assert_eq!(r.initial_urls.len(), 2);
    }

    #[test]
    fn confirm_parses() {
        let j = r#"{"success":true,"file":{"id":"XJRJIcIeY","url":"https://storage.to/XJRJIcIeY","filename":"t.txt","size":50,"human_size":"50 B","expires_at":"2026-09-10T20:16:16+00:00"},"owner_token":"owner_v1_x"}"#;
        let r: ConfirmResponse = serde_json::from_str(j).unwrap();
        assert_eq!(r.file.unwrap().id, "XJRJIcIeY");
        assert_eq!(r.owner_token.as_deref(), Some("owner_v1_x"));
    }
}
