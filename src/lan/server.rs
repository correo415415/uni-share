//! LAN receiver: axum HTTPS server implementing the transfer protocol.
//!
//! The server is UI-agnostic: incoming offers are pushed to an
//! `mpsc` channel as [`IncomingOffer`]; whoever owns the receiver (CLI prompt,
//! daemon auto-accept, web GUI) answers through the embedded oneshot. Progress
//! is exposed through a watch channel so UIs can render it.

use super::protocol::*;
use super::tls::Identity;
use crate::fsutil::{destination_path, human_bytes};
use crate::hash::{Hasher, digests_equal};
use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde::Deserialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, RwLock, mpsc, oneshot, watch};

/// Offer waiting for a human/automatic decision.
pub struct IncomingOffer {
    pub transfer_id: String,
    pub manifest: Manifest,
    pub peer: SocketAddr,
    pub decision: oneshot::Sender<Decision>,
}

#[derive(Debug, Clone)]
pub enum Decision {
    Accept { dest_dir: PathBuf },
    Reject { reason: String },
}

#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub transfer_id: String,
    pub name: String,
    pub sender: String,
    pub total: u64,
    pub received: u64,
    pub files_done: usize,
    pub files_total: usize,
    pub current_file: String,
    pub finished: bool,
    pub error: Option<String>,
    pub dest_dir: Option<PathBuf>,
}

pub struct Transfer {
    manifest: Manifest,
    dest_dir: PathBuf,
    /// Final destination per file index (chosen once to keep dedup stable).
    targets: Vec<PathBuf>,
    complete: Vec<bool>,
    verified_hashes: Vec<String>,
    received_total: AtomicU64,
    started: Instant,
    peer: SocketAddr,
}

enum OfferState {
    Pending,
    Rejected(String),
    Accepted,
}

pub struct ServerState {
    pub identity: Identity,
    pub device_name: String,
    pub pin: Option<String>,
    pub force_overwrite: bool,
    pub default_dest: PathBuf,
    pub rate_limit_bps: u64,
    offers: RwLock<HashMap<String, OfferState>>,
    transfers: RwLock<HashMap<String, Arc<Mutex<Transfer>>>>,
    offer_tx: mpsc::Sender<IncomingOffer>,
    pub progress_tx: watch::Sender<Progress>,
    /// Fired when a transfer completes (for `--once` / notifications).
    pub completed_tx: mpsc::UnboundedSender<Progress>,
}

pub struct ServerHandle {
    pub addr: SocketAddr,
    pub state: Arc<ServerState>,
    pub offers: mpsc::Receiver<IncomingOffer>,
    pub progress: watch::Receiver<Progress>,
    pub completed: mpsc::UnboundedReceiver<Progress>,
    shutdown: Option<axum_server::Handle<SocketAddr>>,
    task: tokio::task::JoinHandle<()>,
}

impl ServerHandle {
    pub async fn shutdown(mut self) {
        if let Some(h) = self.shutdown.take() {
            h.graceful_shutdown(Some(std::time::Duration::from_secs(2)));
        }
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), &mut self.task).await;
    }
}

#[derive(Clone)]
pub struct ServerOptions {
    pub device_name: String,
    pub port: u16,
    pub pin: Option<String>,
    pub force_overwrite: bool,
    pub dest_dir: PathBuf,
    pub rate_limit_mbps: u32,
}

/// Start the HTTPS server on `0.0.0.0:port` (port 0 = ephemeral).
pub async fn start(identity: Identity, opts: ServerOptions) -> Result<ServerHandle> {
    let (offer_tx, offers) = mpsc::channel(16);
    let (progress_tx, progress) = watch::channel(Progress::default());
    let (completed_tx, completed) = mpsc::unbounded_channel();
    let state = Arc::new(ServerState {
        identity: identity.clone(),
        device_name: opts.device_name,
        pin: opts.pin.filter(|p| !p.is_empty()),
        force_overwrite: opts.force_overwrite,
        default_dest: opts.dest_dir,
        rate_limit_bps: (opts.rate_limit_mbps as u64) * 1_000_000 / 8,
        offers: RwLock::new(HashMap::new()),
        transfers: RwLock::new(HashMap::new()),
        offer_tx,
        progress_tx,
        completed_tx,
    });

    let app = Router::new()
        .route(&format!("{API_PREFIX}/info"), get(info))
        .route(&format!("{API_PREFIX}/offer"), post(offer))
        .route(&format!("{API_PREFIX}/offer/{{id}}"), get(offer_status))
        .route(&format!("{API_PREFIX}/transfer/{{id}}/file/{{idx}}"), get(file_state).put(upload_file))
        .route(&format!("{API_PREFIX}/transfer/{{id}}/complete"), post(complete))
        .with_state(state.clone());

    let tls = axum_server::tls_rustls::RustlsConfig::from_config(identity.server_config()?);
    let bind: SocketAddr = format!("0.0.0.0:{}", opts.port).parse()?;
    let listener = std::net::TcpListener::bind(bind).with_context(|| format!("binding {bind}"))?;
    let addr = listener.local_addr()?;
    let handle = axum_server::Handle::new();
    let server = axum_server::from_tcp_rustls(listener, tls).context("tcp listener")?.handle(handle.clone());
    let task = tokio::spawn(async move {
        if let Err(e) = server.serve(app.into_make_service_with_connect_info::<SocketAddr>()).await {
            tracing::error!("lan server error: {e}");
        }
    });
    tracing::info!(%addr, "LAN receiver listening (TLS 1.3)");
    Ok(ServerHandle { addr, state, offers, progress, completed, shutdown: Some(handle), task })
}

// ---------------------------------------------------------------- handlers

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(ErrorBody { error: msg.into() })).into_response()
}

fn check_pin(state: &ServerState, headers: &HeaderMap) -> Result<(), Response> {
    if let Some(pin) = &state.pin {
        let given = headers.get(HEADER_PIN).and_then(|v| v.to_str().ok()).unwrap_or("");
        if given != pin {
            return Err(err(StatusCode::UNAUTHORIZED, "invalid or missing PIN"));
        }
    }
    Ok(())
}

async fn info(State(s): State<Arc<ServerState>>) -> Json<DeviceInfo> {
    Json(DeviceInfo {
        name: s.device_name.clone(),
        version: crate::APP_VERSION.into(),
        fingerprint: s.identity.fingerprint.clone(),
        protocol: PROTOCOL_VERSION,
        requires_pin: s.pin.is_some(),
    })
}

async fn offer(
    State(s): State<Arc<ServerState>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(manifest): Json<Manifest>,
) -> Response {
    if let Err(r) = check_pin(&s, &headers) {
        return r;
    }
    if let Err(e) = manifest.validate() {
        return err(StatusCode::BAD_REQUEST, e);
    }
    let transfer_id = new_id();
    s.offers
        .write()
        .await
        .insert(transfer_id.clone(), OfferState::Pending);

    let (tx, rx) = oneshot::channel();
    let incoming = IncomingOffer { transfer_id: transfer_id.clone(), manifest: manifest.clone(), peer, decision: tx };
    if s.offer_tx.send(incoming).await.is_err() {
        return err(StatusCode::SERVICE_UNAVAILABLE, "receiver not accepting offers");
    }
    // Resolve the decision in the background so the sender can poll.
    let st = s.clone();
    let tid = transfer_id.clone();
    tokio::spawn(async move {
        let decision = match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
            Ok(Ok(d)) => d,
            _ => Decision::Reject { reason: "timed out waiting for receiver".into() },
        };
        match decision {
            Decision::Accept { dest_dir } => {
                let n = manifest.files.len();
                let targets = manifest
                    .files
                    .iter()
                    .map(|f| destination_path(&dest_dir, &f.path, st.force_overwrite))
                    .collect();
                let t = Transfer {
                    manifest: manifest.clone(),
                    dest_dir: dest_dir.clone(),
                    targets,
                    complete: vec![false; n],
                    verified_hashes: vec![String::new(); n],
                    received_total: AtomicU64::new(0),
                    started: Instant::now(),
                    peer,
                };
                st.transfers.write().await.insert(tid.clone(), Arc::new(Mutex::new(t)));
                st.offers.write().await.insert(tid.clone(), OfferState::Accepted);
                let _ = st.progress_tx.send(Progress {
                    transfer_id: tid.clone(),
                    name: manifest.name.clone(),
                    sender: manifest.sender.clone(),
                    total: manifest.total_size,
                    files_total: n,
                    dest_dir: Some(dest_dir),
                    ..Default::default()
                });
            }
            Decision::Reject { reason } => {
                st.offers.write().await.insert(tid, OfferState::Rejected(reason));
            }
        }
    });

    (StatusCode::ACCEPTED, Json(OfferResponse { transfer_id, status: OfferStatus::Pending })).into_response()
}

async fn offer_status(State(s): State<Arc<ServerState>>, AxPath(id): AxPath<String>) -> Response {
    let offers = s.offers.read().await;
    match offers.get(&id) {
        None => err(StatusCode::NOT_FOUND, "unknown offer"),
        Some(OfferState::Pending) => Json(OfferResponse { transfer_id: id, status: OfferStatus::Pending }).into_response(),
        Some(OfferState::Rejected(reason)) => {
            Json(OfferResponse { transfer_id: id, status: OfferStatus::Rejected { reason: reason.clone() } }).into_response()
        }
        Some(OfferState::Accepted) => Json(OfferResponse { transfer_id: id, status: OfferStatus::Accepted }).into_response(),
    }
}

async fn get_transfer(s: &ServerState, id: &str) -> Result<Arc<Mutex<Transfer>>, Response> {
    s.transfers
        .read()
        .await
        .get(id)
        .cloned()
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "unknown or not accepted transfer"))
}

fn part_path(target: &std::path::Path) -> PathBuf {
    let mut os = target.as_os_str().to_owned();
    os.push(".part");
    PathBuf::from(os)
}

async fn file_state(State(s): State<Arc<ServerState>>, AxPath((id, idx)): AxPath<(String, usize)>) -> Response {
    let t = match get_transfer(&s, &id).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let t = t.lock().await;
    if idx >= t.manifest.files.len() {
        return err(StatusCode::NOT_FOUND, "file index out of range");
    }
    if t.complete[idx] {
        return Json(FileState { received: t.manifest.files[idx].size, complete: true }).into_response();
    }
    let received = tokio::fs::metadata(part_path(&t.targets[idx])).await.map(|m| m.len()).unwrap_or(0);
    Json(FileState { received, complete: false }).into_response()
}

#[derive(Deserialize)]
struct UploadQuery {
    #[serde(default)]
    offset: u64,
}

async fn upload_file(
    State(s): State<Arc<ServerState>>,
    AxPath((id, idx)): AxPath<(String, usize)>,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let t_arc = match get_transfer(&s, &id).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    // Snapshot what we need, then release the lock while streaming.
    let (target, expected_size, expected_hash, rel, name, sender, total, files_total) = {
        let t = t_arc.lock().await;
        if idx >= t.manifest.files.len() {
            return err(StatusCode::NOT_FOUND, "file index out of range");
        }
        if t.complete[idx] {
            return Json(UploadResult::Ok { blake3: t.verified_hashes[idx].clone() }).into_response();
        }
        let f = &t.manifest.files[idx];
        let hdr_hash = headers.get(HEADER_HASH).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let expected = if f.blake3.is_empty() { hdr_hash } else { f.blake3.clone() };
        (
            t.targets[idx].clone(),
            f.size,
            expected,
            f.path.clone(),
            t.manifest.name.clone(),
            t.manifest.sender.clone(),
            t.manifest.total_size,
            t.manifest.files.len(),
        )
    };

    if let Some(parent) = target.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return err(StatusCode::INTERNAL_SERVER_ERROR, format!("mkdir: {e}"));
        }
    }
    let part = part_path(&target);
    let existing = tokio::fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0);
    if q.offset != existing {
        return err(
            StatusCode::CONFLICT,
            format!("offset mismatch: receiver has {existing} bytes, sender offered {}", q.offset),
        );
    }

    // Hash the existing prefix (resume) then continue with the stream.
    let mut hasher = Hasher::new();
    let mut file = match tokio::fs::OpenOptions::new().create(true).append(true).read(true).open(&part).await {
        Ok(f) => f,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("open: {e}")),
    };
    if existing > 0 {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        if file.seek(std::io::SeekFrom::Start(0)).await.is_err() {
            return err(StatusCode::INTERNAL_SERVER_ERROR, "seek failed");
        }
        let mut buf = vec![0u8; 1 << 20];
        let mut left = existing;
        while left > 0 {
            let n = match file.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("read: {e}")),
            };
            hasher.update(&buf[..n]);
            left = left.saturating_sub(n as u64);
        }
        let _ = file.seek(std::io::SeekFrom::End(0)).await;
    }

    let mut written = existing;
    let mut stream = body.into_data_stream();
    let mut limiter = RateLimiter::new(s.rate_limit_bps);
    let mut last_progress = Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                let _ = file.flush().await;
                return err(StatusCode::BAD_REQUEST, format!("stream error: {e}"));
            }
        };
        if let Err(e) = file.write_all(&chunk).await {
            return err(StatusCode::INSUFFICIENT_STORAGE, format!("write: {e}"));
        }
        hasher.update(&chunk);
        written += chunk.len() as u64;
        {
            let t = t_arc.lock().await;
            t.received_total.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            if last_progress.elapsed().as_millis() >= 100 {
                last_progress = Instant::now();
                let _ = s.progress_tx.send(Progress {
                    transfer_id: id.clone(),
                    name: name.clone(),
                    sender: sender.clone(),
                    total,
                    received: t.received_total.load(Ordering::Relaxed),
                    files_done: t.complete.iter().filter(|c| **c).count(),
                    files_total,
                    current_file: rel.clone(),
                    dest_dir: Some(t.dest_dir.clone()),
                    ..Default::default()
                });
            }
        }
        limiter.throttle(chunk.len()).await;
        if written > expected_size {
            break;
        }
    }
    if let Err(e) = file.flush().await {
        return err(StatusCode::INTERNAL_SERVER_ERROR, format!("flush: {e}"));
    }
    drop(file);

    if written != expected_size {
        // Incomplete (client disconnected) — keep .part for resume.
        if written < expected_size {
            return err(StatusCode::BAD_REQUEST, format!("incomplete: got {written} of {expected_size}"));
        }
        let _ = tokio::fs::remove_file(&part).await;
        return (StatusCode::CONFLICT, Json(UploadResult::SizeMismatch { expected: expected_size, actual: written }))
            .into_response();
    }
    let actual = hasher.finalize_hex();
    if !expected_hash.is_empty() && !digests_equal(&expected_hash, &actual) {
        tracing::warn!(file = %rel, "BLAKE3 mismatch, discarding partial file");
        let _ = tokio::fs::remove_file(&part).await;
        return (StatusCode::CONFLICT, Json(UploadResult::HashMismatch { expected: expected_hash, actual })).into_response();
    }
    if s.force_overwrite {
        let _ = tokio::fs::remove_file(&target).await;
    }
    if let Err(e) = tokio::fs::rename(&part, &target).await {
        return err(StatusCode::INTERNAL_SERVER_ERROR, format!("rename: {e}"));
    }
    {
        let mut t = t_arc.lock().await;
        t.complete[idx] = true;
        t.verified_hashes[idx] = actual.clone();
        tracing::info!(file = %rel, size = written, "file verified (BLAKE3)");
    }
    Json(UploadResult::Ok { blake3: actual }).into_response()
}

async fn complete(State(s): State<Arc<ServerState>>, AxPath(id): AxPath<String>) -> Response {
    let t_arc = match get_transfer(&s, &id).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let progress = {
        let t = t_arc.lock().await;
        let missing: Vec<&str> = t
            .manifest
            .files
            .iter()
            .zip(&t.complete)
            .filter(|(_, c)| !**c)
            .map(|(f, _)| f.path.as_str())
            .collect();
        if !missing.is_empty() {
            return err(StatusCode::CONFLICT, format!("{} file(s) not yet received", missing.len()));
        }
        let mut dest_dir = t.dest_dir.clone();
        // Optional archive unpacking.
        if t.manifest.compressed_archive && t.targets.len() == 1 {
            let archive = t.targets[0].clone();
            let out = t.dest_dir.clone();
            let res = tokio::task::spawn_blocking(move || crate::fsutil::unpack_tar_zst(&archive, &out)).await;
            match res {
                Ok(Ok(())) => {
                    let _ = tokio::fs::remove_file(&t.targets[0]).await;
                }
                Ok(Err(e)) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("unpack: {e}")),
                Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("unpack task: {e}")),
            }
        } else if t.targets.len() == 1 {
            dest_dir = t.targets[0].clone();
        } else if let Some(first) = t.targets.first() {
            // Folder transfer: report the top-level folder actually created.
            let depth = t.manifest.files[0].path.split('/').count();
            let mut p = first.clone();
            for _ in 1..depth {
                if let Some(parent) = p.parent() {
                    p = parent.to_path_buf();
                }
            }
            dest_dir = p;
        }
        let elapsed = t.started.elapsed().as_secs_f64().max(0.001);
        tracing::info!(
            peer = %t.peer,
            files = t.manifest.files.len(),
            size = %human_bytes(t.manifest.total_size),
            rate = %crate::fsutil::human_rate(t.manifest.total_size as f64 / elapsed),
            "transfer complete"
        );
        Progress {
            transfer_id: id.clone(),
            name: t.manifest.name.clone(),
            sender: t.manifest.sender.clone(),
            total: t.manifest.total_size,
            received: t.manifest.total_size,
            files_done: t.manifest.files.len(),
            files_total: t.manifest.files.len(),
            finished: true,
            dest_dir: Some(dest_dir),
            ..Default::default()
        }
    };
    s.transfers.write().await.remove(&id);
    let _ = s.progress_tx.send(progress.clone());
    let _ = s.completed_tx.send(progress);
    StatusCode::OK.into_response()
}

fn new_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 12];
    rand::rng().fill_bytes(&mut b);
    hex::encode(b)
}

/// Simple token-bucket-ish limiter (bytes per second, 0 = unlimited).
pub struct RateLimiter {
    bps: u64,
    window_start: Instant,
    window_bytes: u64,
}

impl RateLimiter {
    pub fn new(bps: u64) -> Self {
        Self { bps, window_start: Instant::now(), window_bytes: 0 }
    }
    pub async fn throttle(&mut self, n: usize) {
        if self.bps == 0 {
            return;
        }
        self.window_bytes += n as u64;
        let expected = std::time::Duration::from_secs_f64(self.window_bytes as f64 / self.bps as f64);
        let elapsed = self.window_start.elapsed();
        if expected > elapsed {
            tokio::time::sleep(expected - elapsed).await;
        }
        if elapsed.as_secs() >= 2 {
            self.window_start = Instant::now();
            self.window_bytes = 0;
        }
    }
}
