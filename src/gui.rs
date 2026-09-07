//! Local web GUI (`uni-share gui`): a small axum app on 127.0.0.1 serving an
//! embedded single-page UI. It embeds the LAN receiver so incoming offers can
//! be previewed and accepted/rejected from the browser, and exposes actions
//! for LAN/global sending, downloads, device discovery and history.

use crate::config::Config;
use crate::fsutil::{collect_files, total_size};
use crate::history::{History, Kind, Status};
use crate::lan::client::{Sender, build_manifest, hash_all};
use crate::lan::discovery::{Announcer, discover, parse_target};
use crate::lan::server::{Decision, IncomingOffer, Progress, ServerHandle, ServerOptions};
use crate::lan::tls::Identity;
use anyhow::{Context, Result};
use axum::extract::{Path as AxPath, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};

pub const INDEX_HTML: &str = include_str!("gui/index.html");

#[derive(Clone, Serialize)]
pub struct PendingOffer {
    pub transfer_id: String,
    pub sender: String,
    pub peer: String,
    pub name: String,
    pub total_size: u64,
    pub files: Vec<(String, u64)>,
}

#[derive(Clone, Serialize, Default)]
pub struct Job {
    pub id: u64,
    pub kind: String,
    pub name: String,
    pub total: u64,
    pub done: u64,
    pub state: String, // running | completed | failed
    pub message: String,
    pub link: Option<String>,
}

pub struct GuiState {
    pub cfg: Config,
    pub cfg_path: PathBuf,
    pub history: History,
    pub identity: Identity,
    pub lan_addr: SocketAddr,
    pending: RwLock<HashMap<String, (PendingOffer, tokio::sync::oneshot::Sender<Decision>)>>,
    lan_progress: tokio::sync::watch::Receiver<Progress>,
    jobs: RwLock<Vec<Job>>,
    next_job: AtomicU64,
    visitor_token: Mutex<Option<String>>,
}

impl GuiState {
    async fn new_job(&self, kind: &str, name: &str, total: u64) -> (u64, Arc<AtomicU64>) {
        let id = self.next_job.fetch_add(1, Ordering::Relaxed) + 1;
        let mut jobs = self.jobs.write().await;
        jobs.insert(0, Job { id, kind: kind.into(), name: name.into(), total, state: "running".into(), ..Default::default() });
        jobs.truncate(50);
        (id, Arc::new(AtomicU64::new(0)))
    }
    async fn update_job(&self, id: u64, f: impl FnOnce(&mut Job)) {
        if let Some(j) = self.jobs.write().await.iter_mut().find(|j| j.id == id) {
            f(j);
        }
    }
}

pub struct GuiOptions {
    pub port: u16,
    pub open_browser: bool,
}

/// Run the GUI until Ctrl-C.
pub async fn run(cfg: Config, cfg_path: PathBuf, history: History, opts: GuiOptions) -> Result<()> {
    let identity = Identity::load_or_generate(&crate::config::data_dir(), &cfg.device_name)?;
    tokio::fs::create_dir_all(&cfg.download_dir).await.ok();
    let mut lan: ServerHandle = crate::lan::server::start(
        identity.clone(),
        ServerOptions {
            device_name: cfg.device_name.clone(),
            port: cfg.lan_port,
            pin: cfg.pin.clone(),
            force_overwrite: false,
            dest_dir: cfg.download_dir.clone(),
            rate_limit_mbps: cfg.rate_limit_mbps,
        },
    )
    .await?;
    let _announcer = Announcer::start(&cfg.device_name, lan.addr.port(), &identity.fingerprint, cfg.pin.is_some())?;

    let state = Arc::new(GuiState {
        cfg: cfg.clone(),
        cfg_path,
        history: history.clone(),
        identity,
        lan_addr: lan.addr,
        pending: RwLock::new(HashMap::new()),
        lan_progress: lan.progress.clone(),
        jobs: RwLock::new(Vec::new()),
        next_job: AtomicU64::new(0),
        visitor_token: Mutex::new(cfg.global.storage_to_visitor_token.clone()),
    });

    // Forward incoming LAN offers into the pending map (+ auto-accept if configured).
    let st = state.clone();
    let auto = cfg.auto_accept;
    let dest = cfg.download_dir.clone();
    tokio::spawn(async move {
        while let Some(IncomingOffer { transfer_id, manifest, peer, decision }) = lan.offers.recv().await {
            if auto {
                let _ = decision.send(Decision::Accept { dest_dir: dest.clone() });
                let _ = st.history.start(Kind::LanReceive, &manifest.name, manifest.total_size, &manifest.sender, manifest.files.len() as u32);
                continue;
            }
            let po = PendingOffer {
                transfer_id: transfer_id.clone(),
                sender: manifest.sender.clone(),
                peer: peer.ip().to_string(),
                name: manifest.name.clone(),
                total_size: manifest.total_size,
                files: manifest.files.iter().map(|f| (f.path.clone(), f.size)).collect(),
            };
            if st.cfg.notifications {
                crate::ui::notify("uni-share: solicitud entrante", &format!("{} quiere enviarte {}", manifest.sender, manifest.name));
            }
            st.pending.write().await.insert(transfer_id, (po, decision));
        }
    });
    // Record completed LAN receptions in history.
    let st = state.clone();
    tokio::spawn(async move {
        while let Some(done) = lan.completed.recv().await {
            if let Ok(id) = st.history.start(Kind::LanReceive, &done.name, done.total, &done.sender, done.files_total as u32) {
                let _ = st.history.finish(id, Status::Completed, None);
            }
            if st.cfg.notifications {
                crate::ui::notify("uni-share: transferencia recibida", &done.name);
            }
        }
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/api/state", get(api_state))
        .route("/api/devices", get(api_devices))
        .route("/api/history", get(api_history))
        .route("/api/offers/{id}/accept", post(api_accept))
        .route("/api/offers/{id}/reject", post(api_reject))
        .route("/api/send-lan", post(api_send_lan))
        .route("/api/send-global", post(api_send_global))
        .route("/api/download", post(api_download))
        .with_state(state.clone());

    let addr: SocketAddr = format!("127.0.0.1:{}", opts.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))?;
    let url = format!("http://{}", listener.local_addr()?);
    crate::ui::info("GUI", format!("interfaz disponible en {url}  (LAN receiver en {})", state.lan_addr));
    if opts.open_browser {
        let u = url.clone();
        std::thread::spawn(move || {
            let _ = std::process::Command::new(if cfg!(target_os = "macos") { "open" } else if cfg!(windows) { "cmd" } else { "xdg-open" })
                .args(if cfg!(windows) { vec!["/C", "start", "", u.as_str()] } else { vec![u.as_str()] })
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        });
    }
    axum::serve(listener, app).with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; }).await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

fn json_err(status: StatusCode, msg: impl ToString) -> Response {
    (status, Json(serde_json::json!({ "error": msg.to_string() }))).into_response()
}

async fn api_state(State(s): State<Arc<GuiState>>) -> Json<serde_json::Value> {
    let pending: Vec<PendingOffer> = s.pending.read().await.values().map(|(p, _)| p.clone()).collect();
    let p = s.lan_progress.borrow().clone();
    let jobs = s.jobs.read().await.clone();
    Json(serde_json::json!({
        "device_name": s.cfg.device_name,
        "fingerprint": crate::lan::tls::short_fingerprint(&s.identity.fingerprint),
        "lan_addr": s.lan_addr.to_string(),
        "download_dir": s.cfg.download_dir,
        "backend": s.cfg.global.backend,
        "smash_configured": s.cfg.global.smash_api_key.as_deref().map(|k| !k.is_empty()).unwrap_or(false),
        "pending": pending,
        "receiving": { "active": p.total > 0 && !p.finished, "name": p.name, "sender": p.sender, "total": p.total, "received": p.received, "current_file": p.current_file },
        "jobs": jobs,
        "version": crate::APP_VERSION,
    }))
}

async fn api_devices(State(s): State<Arc<GuiState>>) -> Response {
    match discover(Duration::from_secs(2), Some(&s.identity.fingerprint)).await {
        Ok(d) => Json(d).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
}

async fn api_history(State(s): State<Arc<GuiState>>) -> Response {
    match s.history.list(50) {
        Ok(l) => Json(l).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
}

async fn api_accept(State(s): State<Arc<GuiState>>, AxPath(id): AxPath<String>) -> Response {
    match s.pending.write().await.remove(&id) {
        Some((_, tx)) => {
            let _ = tx.send(Decision::Accept { dest_dir: s.cfg.download_dir.clone() });
            StatusCode::OK.into_response()
        }
        None => json_err(StatusCode::NOT_FOUND, "offer not found"),
    }
}

async fn api_reject(State(s): State<Arc<GuiState>>, AxPath(id): AxPath<String>) -> Response {
    match s.pending.write().await.remove(&id) {
        Some((p, tx)) => {
            let _ = tx.send(Decision::Reject { reason: "rechazada desde la GUI".into() });
            if let Ok(rid) = s.history.start(Kind::LanReceive, &p.name, p.total_size, &p.sender, p.files.len() as u32) {
                let _ = s.history.finish(rid, Status::Rejected, None);
            }
            StatusCode::OK.into_response()
        }
        None => json_err(StatusCode::NOT_FOUND, "offer not found"),
    }
}

#[derive(Deserialize)]
struct SendLanReq {
    path: String,
    target: String,
    #[serde(default)]
    fingerprint: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    pin: Option<String>,
}

async fn api_send_lan(State(s): State<Arc<GuiState>>, Json(req): Json<SendLanReq>) -> Response {
    let path = PathBuf::from(&req.path);
    if !path.exists() {
        return json_err(StatusCode::BAD_REQUEST, "path not found");
    }
    let (ip, port) = match parse_target(&req.target, req.port.unwrap_or(s.cfg.lan_port)) {
        Some(t) => t,
        None => return json_err(StatusCode::BAD_REQUEST, "target must be ip or ip:port"),
    };
    let files = match collect_files(&path) {
        Ok(f) => f,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    };
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let total = total_size(&files);
    let (job, counter) = s.new_job("lan_send", &name, total).await;
    let st = s.clone();
    tokio::spawn(async move {
        let res: Result<()> = async {
            let hashes = hash_all(&files, |_| {}).await?;
            let manifest = build_manifest(&st.cfg.device_name, &st.identity.fingerprint, &name, &files, &hashes);
            let sender = Sender::new(ip, port, req.fingerprint.as_deref().filter(|f| !f.is_empty()), req.pin.clone())?;
            let info = sender.info().await?;
            let rid = st.history.start(Kind::LanSend, &name, total, &info.name, files.len() as u32)?;
            let c2 = counter.clone();
            let st2 = st.clone();
            let progress: crate::lan::client::ProgressFn = Arc::new(move |n, f| {
                let done = c2.fetch_add(n, Ordering::Relaxed) + n;
                let st3 = st2.clone();
                let f = f.to_string();
                tokio::spawn(async move { st3.update_job(job, |j| { j.done = done; j.message = f; }).await });
            });
            let r = sender.send(&manifest, &files, &hashes, progress, Duration::from_secs(300), 0).await;
            st.history.finish(rid, if r.is_ok() { Status::Completed } else { Status::Failed }, r.as_ref().err().map(|e| format!("{e:#}")).as_deref())?;
            r.map(|_| ())
        }
        .await;
        st.update_job(job, |j| match &res {
            Ok(()) => { j.state = "completed".into(); j.done = j.total; j.message = "Verificación BLAKE3 correcta".into(); }
            Err(e) => { j.state = "failed".into(); j.message = format!("{e:#}"); }
        })
        .await;
    });
    Json(serde_json::json!({ "job": job })).into_response()
}

#[derive(Deserialize)]
struct SendGlobalReq {
    path: String,
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

async fn api_send_global(State(s): State<Arc<GuiState>>, Json(req): Json<SendGlobalReq>) -> Response {
    let path = PathBuf::from(&req.path);
    if !path.exists() {
        return json_err(StatusCode::BAD_REQUEST, "path not found");
    }
    let files: Vec<_> = match collect_files(&path) {
        Ok(f) => f.into_iter().filter(|f| f.size > 0).collect(),
        Err(e) => return json_err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    };
    if files.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "nothing to upload");
    }
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let total = total_size(&files);
    let backend = req.backend.clone().unwrap_or_else(|| s.cfg.global.backend.clone());
    let (job, counter) = s.new_job("global_upload", &name, total).await;
    let st = s.clone();
    tokio::spawn(async move {
        let res: Result<crate::global::upload::UploadOutcome> = async {
            let rid = st.history.start(Kind::GlobalUpload, &name, total, &backend, files.len() as u32)?;
            let c2 = counter.clone();
            let st2 = st.clone();
            let progress: crate::global::upload::ProgressFn = Arc::new(move |n| {
                let done = c2.fetch_add(n, Ordering::Relaxed) + n;
                let st3 = st2.clone();
                tokio::spawn(async move { st3.update_job(job, |j| j.done = done).await });
            });
            let opts = crate::global::upload::UploadOptions {
                expiry_days: Some(st.cfg.global.expiry_days),
                parallel_parts: st.cfg.global.parallel_parts,
                password: req.password.clone().filter(|p| !p.is_empty()),
                max_downloads: None,
            };
            let r = if backend == "smash" {
                let key = st.cfg.global.smash_api_key.clone().filter(|k| !k.is_empty()).context("Smash API key not configured")?;
                let client = crate::global::smash::SmashClient::new(&key, &st.cfg.global.smash_region)?;
                crate::global::upload::upload_entries_smash(&client, &files, &name, &opts, progress, |_| {}).await
            } else {
                let token = {
                    let mut vt = st.visitor_token.lock().await;
                    match vt.clone() {
                        Some(t) if !t.is_empty() => t,
                        _ => {
                            let mut cfg = st.cfg.clone();
                            let t = cfg.ensure_visitor_token(&st.cfg_path)?;
                            *vt = Some(t.clone());
                            t
                        }
                    }
                };
                let client = crate::global::storage_to::Client::new(&st.cfg.global.storage_to_api, &token, st.cfg.global.storage_to_token.as_deref())?;
                crate::global::upload::upload_entries(&client, &files, &opts, progress, |_| {}).await
            };
            match &r {
                Ok(o) => {
                    st.history.set_link(rid, &o.url, Some(&serde_json::json!({"owner_token": o.owner_token, "id": o.id, "kind": o.kind}).to_string()))?;
                    st.history.finish(rid, Status::Completed, None)?;
                }
                Err(e) => st.history.finish(rid, Status::Failed, Some(&format!("{e:#}")))?,
            }
            r
        }
        .await;
        st.update_job(job, |j| match &res {
            Ok(o) => { j.state = "completed".into(); j.done = j.total; j.link = Some(o.url.clone()); j.message = "Subida completada".into(); }
            Err(e) => { j.state = "failed".into(); j.message = format!("{e:#}"); }
        })
        .await;
    });
    Json(serde_json::json!({ "job": job })).into_response()
}

#[derive(Deserialize)]
struct DownloadReq {
    url: String,
    #[serde(default)]
    password: Option<String>,
}

async fn api_download(State(s): State<Arc<GuiState>>, Json(req): Json<DownloadReq>) -> Response {
    let (job, counter) = s.new_job("download", &req.url, 0).await;
    let st = s.clone();
    tokio::spawn(async move {
        let dest = st.cfg.download_dir.clone();
        let res: Result<usize> = async {
            let c2 = counter.clone();
            let st2 = st.clone();
            let progress: crate::download::http::ProgressFn = Arc::new(move |n| {
                let done = c2.fetch_add(n, Ordering::Relaxed) + n;
                let st3 = st2.clone();
                tokio::spawn(async move { st3.update_job(job, |j| j.done = done).await });
            });
            let pw = req.password.clone().filter(|p| !p.is_empty());
            if crate::download::swisstransfer::is_swisstransfer_url(&req.url) {
                let mut c = crate::download::swisstransfer::SwissTransferClient::new()?;
                let t = c.get_transfer(&req.url, pw.as_deref()).await?;
                st.update_job(job, |j| { j.total = t.total_size; j.name = t.title.clone().unwrap_or_else(|| t.link_id.clone()); }).await;
                let rid = st.history.start(Kind::Download, t.title.as_deref().unwrap_or(&t.link_id), t.total_size, &req.url, t.files.len() as u32)?;
                let r = c.download_all(&t, &dest, false, progress, |_| {}).await;
                st.history.finish(rid, if r.is_ok() { Status::Completed } else { Status::Failed }, r.as_ref().err().map(|e| format!("{e:#}")).as_deref())?;
                Ok(r?.len())
            } else {
                let d = crate::download::storage_to::StorageDownloader::new()?;
                let info = d.info(&req.url).await?;
                if info.password_protected {
                    d.verify_password(&info, pw.as_deref().context("password required")?).await?;
                }
                st.update_job(job, |j| { j.total = info.total_size; j.name = info.title.clone().unwrap_or_else(|| info.files[0].name.clone()); }).await;
                let rid = st.history.start(Kind::Download, info.title.as_deref().unwrap_or(&info.files[0].name), info.total_size, &req.url, info.files.len() as u32)?;
                let r = d.download_all(&info, &dest, false, progress, |_| {}).await;
                st.history.finish(rid, if r.is_ok() { Status::Completed } else { Status::Failed }, r.as_ref().err().map(|e| format!("{e:#}")).as_deref())?;
                Ok(r?.len())
            }
        }
        .await;
        st.update_job(job, |j| match &res {
            Ok(n) => { j.state = "completed".into(); j.done = j.total; j.message = format!("{n} archivo(s) en {}", dest.display()); }
            Err(e) => { j.state = "failed".into(); j.message = format!("{e:#}"); }
        })
        .await;
    });
    Json(serde_json::json!({ "job": job })).into_response()
}

#[allow(dead_code)]
fn _content_type() -> (header::HeaderName, &'static str) {
    (header::CONTENT_TYPE, "text/html; charset=utf-8")
}
