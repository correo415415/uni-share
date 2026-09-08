//! Smash (fromsmash.com) transfer API client — requires an API key (Bearer).
//!
//! Endpoints (verified live 2026-09-07, `?version=01-2024`, regional host
//! `https://transfer.<region>.fromsmash.co`):
//! - `POST /transfer {size, filesNumber, title?, availabilityDuration?, password?, language?}`
//!   → `{transfer:{id, transferUrl, region, uploadState:"Draft", ...}}`
//! - `POST /transfer/{tid}/file {name, size}` → `{file:{id, chunkSize, partsCount, parts:[{id,url,...}]}}`
//! - `PUT <part url>` (S3 presigned) → `ETag` header (**keep the quotes** when reporting)
//! - `POST /transfer/{tid}/file/{fid}/parts {parts:[{id}]}` → more presigned URLs
//! - `PUT /transfer/{tid}/file/{fid} {parts:[{id, etag:"\"…\"", crc32}]}`
//! - `PUT /transfer/{tid}/lock` → `{transfer:{status:"Uploaded", transferUrl, availabilityEndDate}}`

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const API_VERSION: &str = "01-2024";

#[derive(Clone)]
pub struct SmashClient {
    http: reqwest::Client,
    s3: reqwest::Client,
    base: String,
    key: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Transfer {
    pub id: String,
    #[serde(rename = "transferUrl", default)]
    pub transfer_url: String,
    #[serde(default)]
    pub region: String,
    #[serde(rename = "uploadState", default)]
    pub upload_state: String,
    #[serde(default)]
    pub status: String,
    #[serde(rename = "availabilityEndDate", default)]
    pub availability_end: Option<String>,
    #[serde(rename = "parallelConnections", default)]
    pub parallel_connections: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PartUrl {
    pub id: u32,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TransferFile {
    pub id: String,
    #[serde(rename = "chunkSize")]
    pub chunk_size: u64,
    #[serde(rename = "partsCount")]
    pub parts_count: u32,
    #[serde(default)]
    pub parts: Vec<PartUrl>,
}

#[derive(Debug, Serialize)]
pub struct DonePart {
    pub id: u32,
    /// Must include the surrounding double quotes exactly as S3 returned them.
    pub etag: String,
    pub crc32: u32,
}

#[derive(Debug, Deserialize)]
struct TransferEnvelope {
    transfer: Transfer,
}
#[derive(Debug, Deserialize)]
struct FileEnvelope {
    file: TransferFile,
}
#[derive(Debug, Deserialize)]
struct PartsEnvelope {
    parts: Vec<PartUrl>,
}
#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    details: Option<serde_json::Value>,
}

pub fn host_for_region(region: &str) -> String {
    format!("https://transfer.{region}.fromsmash.co")
}

/// Best-effort region extraction from the JWT payload (`"region":"eu-west-3"`).
pub fn region_from_key(key: &str) -> Option<String> {
    let payload = key.split('.').nth(1)?;
    let decoded = base64_url_decode(payload)?;
    let v: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    v.get("region")?.as_str().map(str::to_string)
}

fn base64_url_decode(s: &str) -> Option<Vec<u8>> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let v = T.iter().position(|&t| t == c || (t == b'-' && c == b'+') || (t == b'_' && c == b'/'))? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

impl SmashClient {
    pub fn new(api_key: &str, region: &str) -> Result<Self> {
        anyhow::ensure!(!api_key.trim().is_empty(), "Smash API key is empty (set global.smash_api_key in config.toml)");
        let region = region_from_key(api_key).unwrap_or_else(|| region.to_string());
        let tls = super::storage_to::platform_tls()?;
        let http = reqwest::Client::builder()
            .user_agent(crate::user_agent())
            .use_preconfigured_tls(tls.clone())
            .timeout(Duration::from_secs(60))
            .build()?;
        let s3 = reqwest::Client::builder().user_agent(crate::user_agent()).use_preconfigured_tls(tls).tcp_nodelay(true).build()?;
        Ok(Self { http, s3, base: host_for_region(&region), key: api_key.trim().to_string() })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}?version={}", self.base, path, API_VERSION)
    }

    async fn call<T: serde::de::DeserializeOwned>(&self, method: reqwest::Method, path: &str, body: Option<serde_json::Value>) -> Result<T> {
        let mut rb = self.http.request(method.clone(), self.url(path)).bearer_auth(&self.key).header(reqwest::header::ACCEPT, "application/json");
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let resp = rb.send().await.with_context(|| format!("{method} {path}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let e: Option<ApiError> = serde_json::from_str(&text).ok();
            let msg = e
                .map(|e| {
                    let mut m = e.name.unwrap_or_default();
                    if let Some(x) = e.message.or(e.error) {
                        m = format!("{m}: {x}");
                    }
                    if let Some(d) = e.details {
                        m = format!("{m} {d}");
                    }
                    m
                })
                .unwrap_or_else(|| text.chars().take(200).collect());
            if status.as_u16() == 401 || status.as_u16() == 403 {
                bail!("Smash rejected the API key ({status}): {msg}");
            }
            bail!("Smash {method} {path} failed ({status}): {msg}");
        }
        serde_json::from_str(&text).map_err(|e| anyhow!("parsing Smash response for {path}: {e} — {}", text.chars().take(300).collect::<String>()))
    }

    pub async fn create_transfer(
        &self,
        size: u64,
        files_number: u32,
        title: Option<&str>,
        availability_secs: Option<u64>,
        password: Option<&str>,
        language: &str,
    ) -> Result<Transfer> {
        let mut body = serde_json::json!({ "size": size, "filesNumber": files_number, "language": language });
        if let Some(t) = title {
            body["title"] = serde_json::Value::String(t.chars().take(100).collect());
        }
        if let Some(a) = availability_secs {
            body["availabilityDuration"] = serde_json::json!(a);
        }
        if let Some(p) = password {
            body["password"] = serde_json::Value::String(p.to_string());
        }
        let r: TransferEnvelope = self.call(reqwest::Method::POST, "/transfer", Some(body)).await?;
        Ok(r.transfer)
    }

    pub async fn create_file(&self, transfer_id: &str, name: &str, size: u64) -> Result<TransferFile> {
        let body = serde_json::json!({ "name": name, "size": size });
        let r: FileEnvelope = self.call(reqwest::Method::POST, &format!("/transfer/{transfer_id}/file"), Some(body)).await?;
        Ok(r.file)
    }

    pub async fn more_parts(&self, transfer_id: &str, file_id: &str, ids: &[u32]) -> Result<Vec<PartUrl>> {
        let body = serde_json::json!({ "parts": ids.iter().map(|i| serde_json::json!({"id": i})).collect::<Vec<_>>() });
        let r: PartsEnvelope = self.call(reqwest::Method::POST, &format!("/transfer/{transfer_id}/file/{file_id}/parts"), Some(body)).await?;
        Ok(r.parts)
    }

    pub async fn update_file(&self, transfer_id: &str, file_id: &str, parts: &[DonePart]) -> Result<()> {
        let body = serde_json::json!({ "parts": parts });
        let _: serde_json::Value = self.call(reqwest::Method::PUT, &format!("/transfer/{transfer_id}/file/{file_id}"), Some(body)).await?;
        Ok(())
    }

    pub async fn lock(&self, transfer_id: &str) -> Result<Transfer> {
        let r: TransferEnvelope = self.call(reqwest::Method::PUT, &format!("/transfer/{transfer_id}/lock"), Some(serde_json::json!({}))).await?;
        Ok(r.transfer)
    }

    pub async fn get_transfer(&self, transfer_id: &str) -> Result<Transfer> {
        let r: TransferEnvelope = self.call(reqwest::Method::GET, &format!("/transfer/{transfer_id}"), None).await?;
        Ok(r.transfer)
    }

    pub async fn delete_transfer(&self, transfer_id: &str) -> Result<()> {
        let _: serde_json::Value = self.call(reqwest::Method::DELETE, &format!("/transfer/{transfer_id}"), None).await?;
        Ok(())
    }

    /// PUT one part to S3; returns the quoted ETag as S3 sent it.
    pub async fn put_part(&self, url: &str, body: reqwest::Body, len: u64) -> Result<String> {
        let resp = self
            .s3
            .put(url)
            .header(reqwest::header::CONTENT_LENGTH, len)
            .timeout(Duration::from_secs(60 * 30))
            .body(body)
            .send()
            .await
            .context("PUT part to S3")?;
        if !resp.status().is_success() {
            let st = resp.status();
            let t = resp.text().await.unwrap_or_default();
            bail!("S3 part upload failed ({st}): {}", t.chars().take(300).collect::<String>());
        }
        let etag = resp.headers().get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()).ok_or_else(|| anyhow!("S3 returned no ETag"))?;
        Ok(if etag.starts_with('"') { etag.to_string() } else { format!("\"{etag}\"") })
    }
}

/// Parse a Smash share URL (`https://fromsmash.com/<id>`), returning the id.
pub fn parse_share_url(s: &str) -> Option<String> {
    let u = url::Url::parse(s.trim()).ok()?;
    let host = u.host_str()?;
    if !(host.ends_with("fromsmash.com") || host.ends_with("fromsmash.co")) {
        return None;
    }
    let seg: Vec<&str> = u.path_segments()?.filter(|p| !p.is_empty()).collect();
    match seg.as_slice() {
        [id] if id.len() >= 6 => Some(id.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_from_jwt() {
        // header.payload.sig with payload {"region":"eu-west-3"}
        let payload = "eyJyZWdpb24iOiJldS13ZXN0LTMifQ";
        assert_eq!(region_from_key(&format!("aaa.{payload}.bbb")), Some("eu-west-3".into()));
        assert_eq!(region_from_key("garbage"), None);
    }

    #[test]
    fn parse_smash_url() {
        assert_eq!(parse_share_url("https://fromsmash.com/rxqMzb1OCg-ct"), Some("rxqMzb1OCg-ct".into()));
        assert_eq!(parse_share_url("https://storage.to/abc"), None);
    }

    #[test]
    fn file_envelope_parses() {
        let j = r#"{"file":{"name":"s.txt","size":40,"id":"dbd4","transfer":"t","chunkSize":20971520,"partsCount":1,"parts":[{"url":"https://s3/x","id":1,"startIndex":0,"endIndex":39}]}}"#;
        let f: FileEnvelope = serde_json::from_str(j).unwrap();
        assert_eq!(f.file.parts_count, 1);
        assert_eq!(f.file.parts[0].id, 1);
    }
}
