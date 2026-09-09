//! Local web GUI (`uni-share gui`): thin axum layer over [`crate::engine::Engine`]
//! serving an embedded single-page app on 127.0.0.1.
//!
//! The web UI is the *portable* front-end (any machine with a browser, also
//! remotely over SSH port-forwarding); the native Slint window shares the
//! same engine, so both behave identically.

use crate::config::Config;
use crate::engine::{ConfigPatch, DownloadReq, Engine, SendGlobalReq, SendLanReq, Snapshot, TicketCreateReq};
use crate::history::History;
use crate::ticket::Ticket;
use anyhow::{Context, Result};
use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const INDEX_HTML: &str = include_str!("gui/index.html");
pub const APP_CSS: &str = include_str!("gui/app.css");
pub const APP_JS: &str = include_str!("gui/app.js");
/// Phone GUI (the Android shell loads `/m`): same engine and API, layout designed for thumbs.
pub const M_HTML: &str = include_str!("gui/mobile/index.html");
pub const M_CSS: &str = include_str!("gui/mobile/m.css");
pub const M_JS: &str = include_str!("gui/mobile/m.js");

type St = State<Arc<Engine>>;

pub struct GuiOptions {
    pub port: u16,
    pub open_browser: bool,
    /// Serve `Snapshot::demo()` instead of starting the engine (design reviews, screenshots; no network).
    pub demo: bool,
}

/// Run the web GUI until Ctrl-C.
pub async fn run(cfg: Config, cfg_path: PathBuf, history: History, opts: GuiOptions) -> Result<()> {
    let (app, lan) = if opts.demo {
        (demo_router(cfg, cfg_path), "demo, sin red".to_string())
    } else {
        let engine = Engine::start(cfg, cfg_path, history).await?;
        let lan = engine.lan_addr.to_string();
        (router(engine), lan)
    };
    let addr: SocketAddr = format!("127.0.0.1:{}", opts.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))?;
    let url = format!("http://{}", listener.local_addr()?);
    crate::ui::info("GUI", format!("interfaz disponible en {url}  (LAN receiver en {lan})"));
    if opts.open_browser {
        crate::engine::open_in_system(&url);
    }
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

pub fn router(engine: Arc<Engine>) -> Router {
    Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/app.css", get(|| async { ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], APP_CSS) }))
        .route("/app.js", get(|| async { ([(header::CONTENT_TYPE, "application/javascript; charset=utf-8")], APP_JS) }))
        .route("/m", get(|| async { Html(M_HTML) }))
        .route("/m/m.css", get(|| async { ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], M_CSS) }))
        .route("/m/m.js", get(|| async { ([(header::CONTENT_TYPE, "application/javascript; charset=utf-8")], M_JS) }))
        .route("/api/state", get(api_state))
        .route("/api/events", get(api_events))
        .route("/api/devices", get(api_devices))
        .route("/api/history", get(api_history).delete(api_history_clear))
        .route("/api/offers/{id}/accept", post(api_accept))
        .route("/api/offers/{id}/reject", post(api_reject))
        .route("/api/jobs/{id}/cancel", post(api_job_cancel))
        .route("/api/jobs/{id}/scan", post(api_job_scan))
        .route("/api/jobs/{id}/retry", post(api_job_retry))
        .route("/api/jobs/{id}", axum::routing::delete(api_job_remove))
        .route("/api/jobs/clear-finished", post(api_jobs_clear))
        .route("/api/send-lan", post(api_send_lan))
        .route("/api/send-global", post(api_send_global))
        .route("/api/download", post(api_download))
        .route("/api/fs", get(api_fs))
        .route("/api/preview", get(api_preview))
        .route("/api/open", post(api_open))
        .route("/api/qr", get(api_qr))
        .route("/api/config", get(api_config_get).put(api_config_put))
        .route("/api/notices/ack", post(api_notices_ack))
        .route("/api/scan", post(api_scan))
        .route("/api/ticket/parse", post(api_ticket_parse))
        .route("/api/ticket/create", post(api_ticket_create))
        .route("/api/ticket/file", get(api_ticket_file))
        .with_state(engine)
}

/// Static-state router for `gui --demo`: same pages and read endpoints as [`router`], fed by
/// `Snapshot::demo()` (running jobs tick their progress); mutations answer OK without doing anything.
pub fn demo_router(cfg: Config, cfg_path: PathBuf) -> Router {
    #[derive(Clone)]
    struct Demo {
        snap: Snapshot,
        cfg: Config,
        cfg_path: PathBuf,
        t0: std::time::Instant,
    }
    impl Demo {
        fn now(&self) -> Snapshot {
            let mut s = self.snap.clone();
            let ticks = self.t0.elapsed().as_millis() as u64 / 500;
            for j in s.jobs.iter_mut().filter(|j| j.state == crate::engine::JobState::Running) {
                j.done = (j.done + (j.speed / 2).saturating_mul(ticks)).min(j.total);
                j.eta = (j.speed > 0).then(|| (j.total - j.done) / j.speed);
            }
            s.uptime += self.t0.elapsed().as_secs();
            s
        }
    }
    // Coherent with the snapshot and free of the host's real name/paths (screenshots are published).
    let snap = Snapshot::demo();
    let mut cfg = cfg;
    cfg.device_name = snap.device_name.clone();
    cfg.download_dir = snap.download_dir.clone();
    cfg.pin = None;
    cfg.global.smash_api_key = None;
    cfg.global.storage_to_token = None;
    cfg.global.storage_to_visitor_token = None;
    let _ = cfg_path;
    let cfg_path = PathBuf::from("/home/user/.config/uni-share/config.toml");
    let d = Demo { snap, cfg, cfg_path, t0: std::time::Instant::now() };
    async fn ok() -> Response {
        Json(serde_json::json!({ "ok": true })).into_response()
    }
    async fn no_engine() -> Response {
        json_err(StatusCode::CONFLICT, "modo demo: sin motor")
    }
    async fn events(State(d): State<Demo>) -> Response {
        let stream = futures::stream::unfold(d, |d| async move {
            let json = serde_json::to_string(&d.now()).unwrap_or_else(|_| "{}".into());
            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
            Some((Ok::<_, std::io::Error>(format!("event: state\ndata: {json}\n\n")), d))
        });
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from_stream(stream))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    }
    Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/app.css", get(|| async { ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], APP_CSS) }))
        .route("/app.js", get(|| async { ([(header::CONTENT_TYPE, "application/javascript; charset=utf-8")], APP_JS) }))
        .route("/m", get(|| async { Html(M_HTML) }))
        .route("/m/m.css", get(|| async { ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], M_CSS) }))
        .route("/m/m.js", get(|| async { ([(header::CONTENT_TYPE, "application/javascript; charset=utf-8")], M_JS) }))
        .route("/api/state", get(|State(d): State<Demo>| async move { Json(d.now()).into_response() }))
        .route("/api/events", get(events))
        .route("/api/devices", get(|State(d): State<Demo>| async move { Json(d.snap.devices.clone()).into_response() }))
        .route("/api/history", get(|| async { Json(Vec::<crate::history::Record>::new()).into_response() }).delete(ok))
        .route(
            "/api/config",
            get(|State(d): State<Demo>| async move { Json(serde_json::json!({ "path": d.cfg_path, "config": d.cfg })).into_response() })
                .put(|State(d): State<Demo>| async move { Json(serde_json::json!({ "config": d.cfg })).into_response() }),
        )
        .route("/api/fs", get(api_fs))
        .route("/api/qr", get(api_qr))
        .route("/api/ticket/parse", post(api_ticket_parse))
        .route("/api/notices/ack", post(ok))
        .route("/api/jobs/clear-finished", post(ok))
        .route("/api/offers/{id}/accept", post(ok))
        .route("/api/offers/{id}/reject", post(ok))
        .route("/api/jobs/{id}/cancel", post(ok))
        .route("/api/jobs/{id}/retry", post(ok))
        .route("/api/jobs/{id}", axum::routing::delete(ok))
        .route("/api/send-lan", post(no_engine))
        .route("/api/send-global", post(no_engine))
        .route("/api/download", post(no_engine))
        .with_state(d)
}

fn json_err(status: StatusCode, msg: impl ToString) -> Response {
    (status, Json(serde_json::json!({ "error": msg.to_string() }))).into_response()
}
fn bad(e: impl std::fmt::Display) -> Response {
    json_err(StatusCode::BAD_REQUEST, format!("{e:#}"))
}
fn res_json<T: serde::Serialize>(r: Result<T>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => bad(e),
    }
}

async fn api_state(State(e): St) -> Response {
    Json(e.snapshot().await).into_response()
}
/// Server-Sent Events: a full snapshot (same JSON as `/api/state`) whenever the engine
/// state changes (bursts coalesced over 60 ms), plus a frame every 700 ms while transfers
/// are running (progress counters are lock-free and do not trigger change notifications).
/// When idle a frame is still sent every 15 s as a keep-alive.
async fn api_events(State(e): St) -> Response {
    let stream = async_stream(e);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn async_stream(e: Arc<Engine>) -> impl futures::Stream<Item = Result<String, std::io::Error>> {
    futures::stream::unfold((e, 0u32), |(e, n)| async move {
        if n > 0 {
            let snap = e.snapshot().await;
            let active = snap.jobs.iter().any(|j| j.is_active()) || !snap.pending.is_empty();
            // Wait for a change, but not longer than the refresh cadence.
            let max = if active { std::time::Duration::from_millis(700) } else { std::time::Duration::from_secs(15) };
            e.changed(max).await;
            tokio::time::sleep(std::time::Duration::from_millis(60)).await; // coalesce bursts
        }
        let snap = e.snapshot().await;
        let json = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into());
        Some((Ok(format!("event: state\ndata: {json}\n\n")), (e, n.wrapping_add(1))))
    })
}

async fn api_devices(State(e): St) -> Response {
    Json(e.refresh_devices().await).into_response()
}
async fn api_history(State(e): St) -> Response {
    res_json(e.history.list(200))
}
async fn api_history_clear(State(e): St) -> Response {
    res_json(e.history.clear().map(|n| serde_json::json!({ "deleted": n })))
}

#[derive(Deserialize, Default)]
struct AcceptReq {
    #[serde(default)]
    dest: Option<String>,
}
async fn api_accept(State(e): St, AxPath(id): AxPath<String>, body: Option<Json<AcceptReq>>) -> Response {
    let dest = body.and_then(|Json(b)| b.dest).filter(|d| !d.trim().is_empty()).map(PathBuf::from);
    res_json(e.accept_offer(&id, dest).await.map(|j| serde_json::json!({ "job": j })))
}
async fn api_reject(State(e): St, AxPath(id): AxPath<String>) -> Response {
    res_json(e.reject_offer(&id).await.map(|_| serde_json::json!({ "ok": true })))
}
async fn api_job_cancel(State(e): St, AxPath(id): AxPath<u64>) -> Response {
    if e.cancel_job(id).await { StatusCode::OK.into_response() } else { json_err(StatusCode::CONFLICT, "job is not running") }
}
async fn api_job_scan(State(e): St, AxPath(id): AxPath<u64>) -> Response {
    res_json(e.rescan_job(id).await.map(|r| {
        serde_json::json!({ "severity": r.severity(), "summary": r.summary(), "detail": r.detail(), "report": r })
    }))
}
async fn api_job_retry(State(e): St, AxPath(id): AxPath<u64>) -> Response {
    res_json(e.retry_job(id).await.map(|id| serde_json::json!({ "id": id })))
}
async fn api_job_remove(State(e): St, AxPath(id): AxPath<u64>) -> Response {
    if e.remove_job(id).await { StatusCode::OK.into_response() } else { json_err(StatusCode::CONFLICT, "job not found or still active") }
}
async fn api_jobs_clear(State(e): St) -> Response {
    Json(serde_json::json!({ "removed": e.clear_finished().await })).into_response()
}
async fn api_send_lan(State(e): St, Json(r): Json<SendLanReq>) -> Response {
    res_json(e.send_lan(r).await.map(|j| serde_json::json!({ "job": j })))
}
async fn api_send_global(State(e): St, Json(r): Json<SendGlobalReq>) -> Response {
    res_json(e.send_global(r).await.map(|j| serde_json::json!({ "job": j })))
}
async fn api_download(State(e): St, Json(r): Json<DownloadReq>) -> Response {
    res_json(e.download(r).await.map(|j| serde_json::json!({ "job": j })))
}

#[derive(Deserialize)]
struct FsQuery {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    dirs_only: bool,
}
async fn api_fs(Query(q): Query<FsQuery>) -> Response {
    res_json(crate::engine::list_dir(q.path.as_deref().map(Path::new), q.dirs_only, false))
}

#[derive(Deserialize)]
struct PathQuery {
    path: String,
}
async fn api_preview(Query(q): Query<PathQuery>) -> Response {
    let p = PathBuf::from(&q.path);
    if !p.exists() {
        return bad("path not found");
    }
    match tokio::task::spawn_blocking(move || crate::fsutil::collect_files(&p)).await {
        Ok(Ok(files)) => {
            let total = crate::fsutil::total_size(&files);
            let list: Vec<crate::engine::JobFile> =
                files.iter().take(2000).map(|f| crate::engine::JobFile { path: f.rel_path.clone(), size: f.size }).collect();
            Json(serde_json::json!({ "count": files.len(), "total": total, "files": list, "truncated": files.len() > 2000 })).into_response()
        }
        Ok(Err(e)) => bad(e),
        Err(e) => bad(e),
    }
}
async fn api_open(Json(r): Json<PathQuery>) -> Response {
    let p = PathBuf::from(&r.path);
    if !p.exists() {
        return bad("path not found");
    }
    let target = if p.is_dir() { p } else { p.parent().map(Path::to_path_buf).unwrap_or(p) };
    crate::engine::open_in_system(&target.display().to_string());
    StatusCode::OK.into_response()
}

#[derive(Deserialize)]
struct QrQuery {
    data: String,
}
async fn api_qr(Query(q): Query<QrQuery>) -> Response {
    match crate::ui::qr_svg(&q.data) {
        Some(svg) => ([(header::CONTENT_TYPE, "image/svg+xml"), (header::CACHE_CONTROL, "no-store")], svg).into_response(),
        None => bad("data too long for a QR code"),
    }
}

async fn api_config_get(State(e): St) -> Response {
    let mut cfg = e.config().await;
    cfg.global.smash_api_key = cfg.global.smash_api_key.map(|_| "••••••".into());
    cfg.global.storage_to_token = cfg.global.storage_to_token.map(|_| "••••••".into());
    Json(serde_json::json!({ "path": e.cfg_path, "config": cfg })).into_response()
}
async fn api_config_put(State(e): St, Json(p): Json<ConfigPatch>) -> Response {
    res_json(e.patch_config(p).await.map(|restart| serde_json::json!({ "ok": true, "restart_needed": restart })))
}
#[derive(Deserialize)]
struct AckReq {
    up_to: u64,
}
async fn api_notices_ack(State(e): St, Json(r): Json<AckReq>) -> Response {
    e.ack_notices(r.up_to).await;
    Json(serde_json::json!({ "ok": true })).into_response()
}
#[derive(Deserialize)]
struct ScanReq {
    paths: Vec<PathBuf>,
    #[serde(default)]
    quarantine: bool,
}
/// On-demand scan (report only unless `quarantine`), independent of the `[scan]` switch.
async fn api_scan(State(e): St, Json(r): Json<ScanReq>) -> Response {
    let mut cfg = e.config().await.scan;
    cfg.enabled = true;
    cfg.on_danger = if r.quarantine { crate::scan::DangerAction::Quarantine } else { crate::scan::DangerAction::Report };
    Json(crate::scan::scan_paths_async(r.paths, cfg).await).into_response()
}

#[derive(Deserialize)]
struct TicketParseReq {
    data: String,
}
async fn api_ticket_parse(Json(r): Json<TicketParseReq>) -> Response {
    res_json(Engine::parse_ticket(&r.data).map(|t| {
        let uri = t.to_uri_compact().unwrap_or_default();
        serde_json::json!({ "ticket": t, "uri": uri, "expired": t.is_expired(), "summary": t.summary(), "signature": t.verify_signature(), "lan": t.is_lan() })
    }))
}
async fn api_ticket_create(State(e): St, Json(r): Json<TicketCreateReq>) -> Response {
    res_json(e.create_ticket(r).await)
}

#[derive(Deserialize)]
struct TicketFileQuery {
    uri: String,
    #[serde(default)]
    name: Option<String>,
}
/// Serve a ticket as a `.unishare` attachment (browser "Save as").
async fn api_ticket_file(Query(q): Query<TicketFileQuery>) -> Response {
    let t = match Ticket::from_uri(&q.uri) {
        Ok(t) => t,
        Err(e) => return bad(e),
    };
    let bytes = match t.encode() {
        Ok(b) => b,
        Err(e) => return bad(e),
    };
    let fname = q.name.filter(|n| !n.is_empty()).unwrap_or_else(|| t.default_filename());
    (
        [
            (header::CONTENT_TYPE, "application/vnd.unishare".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{}\"", fname.replace('"', ""))),
        ],
        Body::from(bytes),
    )
        .into_response()
}
