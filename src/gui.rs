//! Local web GUI (`uni-share gui`): thin axum layer over [`crate::engine::Engine`]
//! serving an embedded single-page app on 127.0.0.1.
//!
//! The web UI is the *portable* front-end (any machine with a browser, also
//! remotely over SSH port-forwarding); the native Slint window shares the
//! same engine, so both behave identically.

use crate::config::Config;
use crate::engine::{ConfigPatch, DownloadReq, Engine, SendGlobalReq, SendLanReq, TicketCreateReq};
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

type St = State<Arc<Engine>>;

pub struct GuiOptions {
    pub port: u16,
    pub open_browser: bool,
}

/// Run the web GUI until Ctrl-C.
pub async fn run(cfg: Config, cfg_path: PathBuf, history: History, opts: GuiOptions) -> Result<()> {
    let engine = Engine::start(cfg, cfg_path, history).await?;
    let app = router(engine.clone());
    let addr: SocketAddr = format!("127.0.0.1:{}", opts.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))?;
    let url = format!("http://{}", listener.local_addr()?);
    crate::ui::info("GUI", format!("interfaz disponible en {url}  (LAN receiver en {})", engine.lan_addr));
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
        .route("/api/state", get(api_state))
        .route("/api/devices", get(api_devices))
        .route("/api/history", get(api_history).delete(api_history_clear))
        .route("/api/offers/{id}/accept", post(api_accept))
        .route("/api/offers/{id}/reject", post(api_reject))
        .route("/api/jobs/{id}/cancel", post(api_job_cancel))
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
        .route("/api/ticket/parse", post(api_ticket_parse))
        .route("/api/ticket/create", post(api_ticket_create))
        .route("/api/ticket/file", get(api_ticket_file))
        .with_state(engine)
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
struct TicketParseReq {
    data: String,
}
async fn api_ticket_parse(Json(r): Json<TicketParseReq>) -> Response {
    res_json(Engine::parse_ticket(&r.data).map(|t| {
        let uri = t.to_uri_compact().unwrap_or_default();
        serde_json::json!({ "ticket": t, "uri": uri, "expired": t.is_expired(), "summary": t.summary() })
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
