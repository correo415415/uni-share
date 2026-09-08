//! Native desktop window (Slint) — `uni-share app`.
//!
//! Architecture: the Slint event loop owns the main thread; a dedicated tokio
//! runtime thread owns the [`Engine`]. Every 500 ms the runtime takes a
//! [`Snapshot`] and pushes it into the window models via
//! `slint::invoke_from_event_loop`; UI callbacks send [`Cmd`]s through an
//! unbounded channel to the runtime. The window therefore never blocks and
//! never touches the network layer directly — it renders state and emits
//! intents, exactly like the web front-end.

use crate::config::Config;
use crate::engine::{ConfigPatch, DownloadReq, Engine, JobKind, JobState, SendGlobalReq, SendLanReq, Snapshot, TicketCreateReq};
use crate::fsutil::human_bytes;
use crate::history::History;
use anyhow::{Context, Result};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

slint::include_modules!();

/// Intents from the UI thread to the engine thread.
enum Cmd {
    SendLan(SendLanReq),
    SendGlobal(SendGlobalReq),
    Download(DownloadReq),
    CreateTicket(TicketCreateReq),
    Cancel(u64),
    Remove(u64),
    ClearFinished,
    Rescan,
    Accept(String, Option<PathBuf>),
    Reject(String),
    PatchConfig(ConfigPatch),
    LoadHistory,
}

/// Results from the engine thread to the UI thread.
enum Evt {
    Snapshot(Snapshot),
    Toast(String, &'static str),
    Error(String),
    TicketCreated { name: String, uri: String, path: String, link: String },
    History(Vec<crate::history::Record>),
    Config(Config),
}

struct UiState {
    filter: String,
    query: String,
    sort_key: String,
    sort_desc: bool,
    selected_job: Option<u64>,
    dark: bool,
    toasts: Vec<(i32, String, &'static str)>,
    next_toast: i32,
    last_states: std::collections::HashMap<u64, JobState>,
    last_pending: usize,
    snapshot: Option<Snapshot>,
}

#[derive(Clone)]
struct UiCtx {
    win: Weak<MainWindow>,
    tx: tokio::sync::mpsc::UnboundedSender<Cmd>,
    /// Engine→UI event sender; also handed to helper threads (file dialogs) so
    /// they can report back without touching the non-`Send` UI state.
    etx: std::sync::mpsc::Sender<Evt>,
    st: Rc<std::cell::RefCell<UiState>>,
}

impl UiCtx {
    fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }
    fn toast(&self, text: impl Into<String>, kind: &'static str) {
        let mut st = self.st.borrow_mut();
        let id = st.next_toast;
        st.next_toast += 1;
        st.toasts.push((id, text.into(), kind));
        if st.toasts.len() > 5 {
            st.toasts.remove(0);
        }
        drop(st);
        self.render_toasts();
        let me = self.clone();
        slint::Timer::single_shot(Duration::from_millis(5000), move || {
            me.st.borrow_mut().toasts.retain(|t| t.0 != id);
            me.render_toasts();
        });
    }
    fn render_toasts(&self) {
        if let Some(w) = self.win.upgrade() {
            let rows: Vec<ToastRow> = self.st.borrow().toasts.iter().map(|(id, t, k)| ToastRow { id: *id, text: t.into(), kind: (*k).into() }).collect();
            w.set_toasts(ModelRc::new(VecModel::from(rows)));
        }
    }
}

pub struct AppOptions {
    /// Ticket or URL to open at start (file association / `unishare:` handler).
    pub open: Option<String>,
    /// Feed the UI with `Snapshot::demo()` instead of starting the engine.
    pub demo: bool,
    /// Render the window once, save it as PNG and quit (design review / CI).
    pub screenshot: Option<PathBuf>,
    /// Dialog to open before the screenshot ("new", "share", "settings", "fs", "confirm").
    pub dialog: Option<String>,
    /// Pre-select a job id and details tab (0 general, 1 files, 2 share, 3 log, 4 history).
    pub select: Option<(u64, i32)>,
    /// Start with the light theme.
    pub light: bool,
}

/// Run the native window. Blocks until the window is closed.
pub fn run(cfg: Config, cfg_path: PathBuf, history: History, opts: AppOptions) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Cmd>();
    let (etx, erx) = std::sync::mpsc::channel::<Evt>();

    // ── engine thread ──
    let cfg2 = cfg.clone();
    let cfg_path2 = cfg_path.clone();
    let etx2 = etx.clone();
    if opts.demo {
        // No network, no engine: a static snapshot that ticks its fake progress.
        std::thread::Builder::new()
            .name("uni-share-demo".into())
            .spawn(move || {
                let mut snap = Snapshot::demo();
                let _ = etx2.send(Evt::Config(cfg2));
                loop {
                    let _ = etx2.send(Evt::Snapshot(snap.clone()));
                    for j in snap.jobs.iter_mut().filter(|j| j.state == JobState::Running) {
                        j.done = (j.done + j.speed / 2).min(j.total);
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            })
            .context("spawning demo thread")?;
    } else {
    std::thread::Builder::new()
        .name("uni-share-engine".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
            rt.block_on(async move {
                let engine = match Engine::start(cfg2, cfg_path2, history).await {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = etx2.send(Evt::Error(format!("No se pudo iniciar el receptor LAN: {e:#}")));
                        return;
                    }
                };
                let _ = etx2.send(Evt::Config(engine.config().await));
                let mut tick = tokio::time::interval(Duration::from_millis(500));
                loop {
                    tokio::select! {
                        _ = tick.tick() => { let _ = etx2.send(Evt::Snapshot(engine.snapshot().await)); }
                        cmd = rx.recv() => {
                            let Some(cmd) = cmd else { break };
                            handle_cmd(&engine, cmd, &etx2).await;
                        }
                    }
                }
            });
        })
        .context("spawning engine thread")?;
    }

    // ── window ──
    let win = MainWindow::new().context("creating window")?;
    win.set_version(format!("v{}", crate::APP_VERSION).into());
    win.set_download_dir(cfg.download_dir.display().to_string().into());
    win.set_f_dest(cfg.download_dir.display().to_string().into());
    win.set_f_expiry(cfg.global.expiry_days.to_string().into());
    let st = Rc::new(std::cell::RefCell::new(UiState {
        filter: "all".into(),
        query: String::new(),
        sort_key: "started".into(),
        sort_desc: true,
        selected_job: opts.select.map(|(id, _)| id),
        dark: !opts.light,
        toasts: Vec::new(),
        next_toast: 1,
        last_states: Default::default(),
        last_pending: 0,
        snapshot: None,
    }));
    win.global::<Theme>().set_dark(!opts.light);
    let ctx = UiCtx { win: win.as_weak(), tx: tx.clone(), etx: etx.clone(), st: st.clone() };
    wire_callbacks(&win, &ctx);
    set_filters(&win, None);

    if let Some(open) = opts.open {
        // Launched by the OS for a `.unishare` file or a `unishare:` link (see
        // `associate`) or with a plain URL: pre-fill the download dialog and
        // show the ticket preview straight away.
        let open = open.strip_prefix("file://").map(|p| percent_decode(p)).unwrap_or(open);
        let is_pairing = Engine::parse_ticket(&open).map(|t| t.is_lan()).unwrap_or(false);
        win.set_preview_text(preview_ticket(&open).into());
        if is_pairing {
            // A LAN pairing ticket is a *target*, not a download: open "Enviar por LAN"
            // with the ticket as manual address.
            win.set_new_mode(0);
            win.set_f_device(-1);
            win.set_f_target(open.into());
        } else {
            win.set_new_mode(2);
            win.set_f_url(open.into());
        }
        win.set_dialog("new".into());
    }
    if let Some((_, tab)) = opts.select {
        win.set_tab(tab);
    }
    if let Some(d) = &opts.dialog {
        if d == "fs" {
            win.set_fs_dirs_only(false);
            fs_load(&win, &cfg.download_dir.display().to_string(), false);
        }
        if d == "share" {
            let data = "https://storage.to/c/G7pzkDNFy";
            win.set_share_what("link".into());
            win.set_qr_caption(data.into());
            win.set_qr_image(qr_image(data));
        }
        win.set_dialog(d.as_str().into());
    }
    // Kept alive until the event loop returns.
    let shot = slint::Timer::default();
    if let Some(path) = opts.screenshot.clone() {
        // Give the pump a few ticks so the first snapshot is rendered, then grab it.
        let w = win.as_weak();
        shot.start(slint::TimerMode::SingleShot, Duration::from_millis(1500), move || {
            let Some(w) = w.upgrade() else { return };
            if let Some(w) = w.window().take_snapshot().ok() {
                let img = image::RgbaImage::from_raw(w.width(), w.height(), w.as_bytes().to_vec());
                match img.map(|i| i.save(&path)) {
                    Some(Ok(())) => tracing::info!("screenshot saved to {}", path.display()),
                    Some(Err(e)) => tracing::error!("saving screenshot: {e}"),
                    None => tracing::error!("snapshot buffer size mismatch"),
                }
            }
            let _ = slint::quit_event_loop();
        });
    }

    // ── event pump: engine → UI ──
    let ctx2 = ctx.clone();
    let pump = slint::Timer::default();
    pump.start(slint::TimerMode::Repeated, Duration::from_millis(100), move || {
        while let Ok(evt) = erx.try_recv() {
            handle_evt(&ctx2, evt);
        }
    });

    win.run().context("running event loop")?;
    drop(shot);
    Ok(())
}

async fn handle_cmd(engine: &Arc<Engine>, cmd: Cmd, etx: &std::sync::mpsc::Sender<Evt>) {
    let r: Result<Option<Evt>> = match cmd {
        Cmd::SendLan(r) => engine.send_lan(r).await.map(|_| Some(Evt::Toast("Envío LAN iniciado".into(), "ok"))),
        Cmd::SendGlobal(r) => engine.send_global(r).await.map(|_| Some(Evt::Toast("Subida iniciada".into(), "ok"))),
        Cmd::Download(r) => engine.download(r).await.map(|_| Some(Evt::Toast("Descarga iniciada".into(), "ok"))),
        Cmd::CreateTicket(r) => engine.create_ticket(r).await.map(|t| {
            Some(Evt::TicketCreated {
                name: t.ticket.name.clone(),
                uri: t.uri,
                path: t.path.display().to_string(),
                link: t.ticket.sources.first().map(|s| s.url().to_string()).unwrap_or_default(),
            })
        }),
        Cmd::Cancel(id) => Ok(if engine.cancel_job(id).await { Some(Evt::Toast("Cancelada".into(), "info")) } else { None }),
        Cmd::Remove(id) => {
            engine.remove_job(id).await;
            Ok(None)
        }
        Cmd::ClearFinished => Ok(Some(Evt::Toast(format!("{} eliminada(s)", engine.clear_finished().await), "info"))),
        Cmd::Rescan => {
            engine.refresh_devices().await;
            Ok(None)
        }
        Cmd::Accept(id, dest) => engine.accept_offer(&id, dest).await.map(|_| Some(Evt::Toast("Recibiendo…".into(), "ok"))),
        Cmd::Reject(id) => engine.reject_offer(&id).await.map(|_| None),
        Cmd::PatchConfig(p) => match engine.patch_config(p).await {
            Ok(restart) => {
                let _ = etx.send(Evt::Config(engine.config().await));
                Ok(Some(Evt::Toast(
                    if restart { "Guardado. Nombre/PIN/puerto se aplican al reiniciar.".into() } else { "Ajustes guardados".into() },
                    "ok",
                )))
            }
            Err(e) => Err(e),
        },
        Cmd::LoadHistory => engine.history.list(200).map(|l| Some(Evt::History(l))),
    };
    match r {
        Ok(Some(e)) => {
            let _ = etx.send(e);
        }
        Ok(None) => {}
        Err(e) => {
            let _ = etx.send(Evt::Error(format!("{e:#}")));
        }
    }
    let _ = etx.send(Evt::Snapshot(engine.snapshot().await));
}

// ───────────────────────── rendering ─────────────────────────

fn fmt_time(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).map(|d| d.with_timezone(&chrono::Local).format("%H:%M:%S").to_string()).unwrap_or_else(|| "—".into())
}
fn fmt_eta(s: Option<u64>) -> String {
    match s {
        None => "—".into(),
        Some(s) if s < 60 => format!("{s}s"),
        Some(s) if s < 3600 => format!("{}m {}s", s / 60, s % 60),
        Some(s) => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}

fn set_filters(win: &MainWindow, snap: Option<&Snapshot>) {
    let count = |f: &dyn Fn(&crate::engine::Job) -> bool| snap.map(|s| s.jobs.iter().filter(|j| f(j)).count()).unwrap_or(0) as i32;
    let rows = vec![
        FilterRow { id: "all".into(), label: "Todas".into(), icon: "list".into(), count: count(&|_| true) },
        FilterRow { id: "active".into(), label: "Activas".into(), icon: "clock".into(), count: count(&|j| j.is_active()) },
        FilterRow { id: "receiving".into(), label: "Recibiendo".into(), icon: "arrow-down".into(), count: count(&|j| j.kind == JobKind::LanReceive) },
        FilterRow { id: "sending".into(), label: "Enviando LAN".into(), icon: "arrow-up".into(), count: count(&|j| j.kind == JobKind::LanSend) },
        FilterRow { id: "uploads".into(), label: "Subidas (links)".into(), icon: "globe".into(), count: count(&|j| j.kind == JobKind::GlobalUpload) },
        FilterRow { id: "downloads".into(), label: "Descargas".into(), icon: "download".into(), count: count(&|j| j.kind == JobKind::Download) },
        FilterRow { id: "completed".into(), label: "Completadas".into(), icon: "check".into(), count: count(&|j| j.state == JobState::Completed) },
        FilterRow { id: "failed".into(), label: "Fallidas".into(), icon: "x-circle".into(), count: count(&|j| matches!(j.state, JobState::Failed | JobState::Cancelled)) },
    ];
    win.set_filters(ModelRc::new(VecModel::from(rows)));
}

fn state_str(s: JobState) -> &'static str {
    match s {
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::Completed => "completed",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
    }
}
fn kind_str(k: JobKind) -> &'static str {
    match k {
        JobKind::LanSend => "lan_send",
        JobKind::LanReceive => "lan_receive",
        JobKind::GlobalUpload => "global_upload",
        JobKind::Download => "download",
    }
}

fn visible_jobs<'a>(st: &UiState, snap: &'a Snapshot) -> Vec<&'a crate::engine::Job> {
    let q = st.query.trim().to_lowercase();
    let mut list: Vec<&crate::engine::Job> = snap
        .jobs
        .iter()
        .filter(|j| match st.filter.as_str() {
            "active" => j.is_active(),
            "receiving" => j.kind == JobKind::LanReceive,
            "sending" => j.kind == JobKind::LanSend,
            "uploads" => j.kind == JobKind::GlobalUpload,
            "downloads" => j.kind == JobKind::Download,
            "completed" => j.state == JobState::Completed,
            "failed" => matches!(j.state, JobState::Failed | JobState::Cancelled),
            _ => true,
        })
        .filter(|j| q.is_empty() || format!("{} {} {} {}", j.name, j.peer, j.link.as_deref().unwrap_or(""), j.kind.label()).to_lowercase().contains(&q))
        .collect();
    list.sort_by(|a, b| {
        let o = match st.sort_key.as_str() {
            "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            "kind" => kind_str(a.kind).cmp(kind_str(b.kind)),
            "size" => a.total.cmp(&b.total),
            "progress" => a.percent().partial_cmp(&b.percent()).unwrap_or(std::cmp::Ordering::Equal),
            "state" => state_str(a.state).cmp(state_str(b.state)),
            "speed" => a.speed.cmp(&b.speed),
            "eta" => a.eta.unwrap_or(u64::MAX).cmp(&b.eta.unwrap_or(u64::MAX)),
            "peer" => a.peer.to_lowercase().cmp(&b.peer.to_lowercase()),
            _ => a.started.cmp(&b.started),
        };
        if st.sort_desc { o.reverse() } else { o }
    });
    list
}

fn render(ctx: &UiCtx) {
    let Some(win) = ctx.win.upgrade() else { return };
    let st = ctx.st.borrow();
    let Some(snap) = st.snapshot.as_ref() else { return };
    win.set_device_name(snap.device_name.clone().into());
    win.set_fingerprint(snap.fingerprint.clone().into());
    win.set_fingerprint_full(snap.fingerprint_full.clone().into());
    win.set_lan_addr(snap.lan_port.to_string().into());
    win.set_local_ip(snap.local_ips.first().cloned().unwrap_or_default().into());
    win.set_pairing_uri(snap.pairing_uri.clone().into());
    win.set_download_dir(snap.download_dir.display().to_string().into());
    win.set_pin_required(snap.pin_required);
    win.set_auto_accept(snap.auto_accept);
    win.set_speed_down(format!("{}/s", human_bytes(snap.speed_down)).into());
    win.set_speed_up(format!("{}/s", human_bytes(snap.speed_up)).into());
    win.set_active_count(snap.jobs.iter().filter(|j| j.state == JobState::Running).count() as i32);
    set_filters(&win, Some(snap));

    let list = visible_jobs(&st, snap);
    let mut sel_idx = -1i32;
    let rows: Vec<JobRow> = list
        .iter()
        .enumerate()
        .map(|(i, j)| {
            if Some(j.id) == st.selected_job {
                sel_idx = i as i32;
            }
            JobRow {
                id: j.id as i32,
                kind: kind_str(j.kind).into(),
                kind_label: j.kind.label().into(),
                name: j.name.clone().into(),
                peer: j.peer.clone().into(),
                total: 0,
                size_text: if j.total > 0 { human_bytes(j.total) } else { "—".into() }.into(),
                percent: j.percent(),
                state: state_str(j.state).into(),
                state_label: j.state.label().into(),
                speed_text: if j.state == JobState::Running && j.speed > 0 { format!("{}/s", human_bytes(j.speed)) } else { "—".into() }.into(),
                eta_text: if j.state == JobState::Running { fmt_eta(j.eta) } else { "—".into() }.into(),
                started_text: fmt_time(j.started).into(),
                message: j.message.clone().into(),
                current_file: j.current_file.clone().into(),
                link: j.link.clone().unwrap_or_default().into(),
                ticket_uri: j.ticket_uri.clone().unwrap_or_default().into(),
                ticket_path: j.ticket_path.clone().unwrap_or_default().into(),
                dest: j.dest.clone().unwrap_or_default().into(),
                done_text: human_bytes(j.done).into(),
                files_count: j.files.len() as i32,
                finished_text: j.finished.map(fmt_time).unwrap_or_else(|| "—".into()).into(),
                indeterminate: j.state == JobState::Running && j.total == 0,
            }
        })
        .collect();
    win.set_jobs(ModelRc::new(VecModel::from(rows)));
    win.set_selected(sel_idx);
    if let Some(j) = st.selected_job.and_then(|id| snap.jobs.iter().find(|j| j.id == id)) {
        let files: Vec<FileRow> = j.files.iter().take(3000).map(|f| FileRow { path: f.path.clone().into(), size_text: human_bytes(f.size).into() }).collect();
        win.set_sel_files(ModelRc::new(VecModel::from(files)));
        win.set_sel_log(j.log.join("\n").into());
    } else {
        win.set_sel_files(ModelRc::new(VecModel::from(Vec::<FileRow>::new())));
        win.set_sel_log("".into());
    }
    let devs: Vec<DeviceRow> = snap
        .devices
        .iter()
        .map(|d| DeviceRow {
            name: d.name.clone().into(),
            addr: format!("{}:{}", d.best_addr().map(|a| a.to_string()).unwrap_or_else(|| "?".into()), d.port).into(),
            fingerprint: d.fingerprint.clone().into(),
            pin: d.requires_pin,
        })
        .collect();
    win.set_devices(ModelRc::new(VecModel::from(devs)));
    let offers: Vec<OfferRow> = snap
        .pending
        .iter()
        .map(|o| OfferRow {
            id: o.transfer_id.clone().into(),
            sender: o.sender.clone().into(),
            name: o.name.clone().into(),
            size_text: human_bytes(o.total_size).into(),
            files_count: o.files.len() as i32,
            peer: o.peer.clone().into(),
            fingerprint: o.sender_fingerprint.clone().into(),
            compressed: o.compressed,
        })
        .collect();
    win.set_offers(ModelRc::new(VecModel::from(offers)));
}

fn handle_evt(ctx: &UiCtx, evt: Evt) {
    match evt {
        Evt::Snapshot(snap) => {
            // Transition notifications.
            let mut notes: Vec<(String, &'static str)> = Vec::new();
            {
                let mut st = ctx.st.borrow_mut();
                for j in &snap.jobs {
                    if let Some(prev) = st.last_states.get(&j.id) {
                        if *prev != j.state && matches!(j.state, JobState::Completed | JobState::Failed) {
                            notes.push((
                                format!("{} «{}»: {}{}", j.kind.label(), j.name, j.state.label(), if j.state == JobState::Failed { format!(" — {}", j.message) } else { String::new() }),
                                if j.state == JobState::Completed { "ok" } else { "err" },
                            ));
                        }
                    }
                    st.last_states.insert(j.id, j.state);
                }
                if snap.pending.len() > st.last_pending {
                    notes.push(("Solicitud de transferencia entrante".into(), "info"));
                }
                st.last_pending = snap.pending.len();
                st.snapshot = Some(snap);
            }
            for (t, k) in notes {
                ctx.toast(t, k);
            }
            render(ctx);
        }
        Evt::Toast(t, k) => ctx.toast(t, k),
        Evt::Error(e) => {
            if let Some(w) = ctx.win.upgrade() {
                if w.get_dialog().is_empty() {
                    ctx.toast(e, "err");
                } else {
                    w.set_dialog_error(e.into());
                }
            }
        }
        Evt::TicketCreated { name, uri, path, link } => {
            if let Some(w) = ctx.win.upgrade() {
                w.set_dialog_error("".into());
                w.set_qr_caption(uri.clone().into());
                w.set_qr_image(qr_image(&uri));
                w.set_share_what("ticket".into());
                w.set_dialog("share".into());
                ctx.toast(format!("Ticket «{name}» guardado en {path}"), "ok");
                let _ = link;
            }
        }
        Evt::History(list) => {
            if let Some(w) = ctx.win.upgrade() {
                let rows: Vec<HistoryRow> = list
                    .iter()
                    .map(|r| HistoryRow {
                        when: r.timestamp.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string().into(),
                        kind: r.kind.as_str().into(),
                        name: r.name.clone().into(),
                        size_text: human_bytes(r.size).into(),
                        status: r.status.as_str().into(),
                        peer: r.peer_or_link.clone().into(),
                    })
                    .collect();
                w.set_history(ModelRc::new(VecModel::from(rows)));
            }
        }
        Evt::Config(c) => {
            if let Some(w) = ctx.win.upgrade() {
                w.set_s_name(c.device_name.clone().into());
                w.set_s_pin(c.pin.clone().unwrap_or_default().into());
                w.set_s_dir(c.download_dir.display().to_string().into());
                w.set_s_rate(c.rate_limit_mbps.to_string().into());
                w.set_s_parallel(c.global.parallel_parts.to_string().into());
                w.set_s_expiry(c.global.expiry_days.to_string().into());
                w.set_s_auto(c.auto_accept);
                w.set_s_notif(c.notifications);
                w.set_s_compress(c.compress_folders);
                w.set_f_dest(c.download_dir.display().to_string().into());
            }
        }
    }
}

/// Render a QR code into a Slint image (1 module = 1 px, scaled by the UI with nearest filtering).
fn qr_image(data: &str) -> slint::Image {
    let Ok(code) = qrcode::QrCode::new(data.as_bytes()) else {
        return slint::Image::default();
    };
    let w = code.width();
    let margin = 2usize;
    let size = (w + margin * 2) as u32;
    let mut buf = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(size, size);
    let px = buf.make_mut_slice();
    for p in px.iter_mut() {
        *p = slint::Rgb8Pixel { r: 255, g: 255, b: 255 };
    }
    let colors = code.to_colors();
    for y in 0..w {
        for x in 0..w {
            if colors[y * w + x] == qrcode::Color::Dark {
                let i = (y + margin) * size as usize + (x + margin);
                px[i] = slint::Rgb8Pixel { r: 0, g: 0, b: 0 };
            }
        }
    }
    slint::Image::from_rgb8(buf)
}

fn nonempty(s: SharedString) -> Option<String> {
    let s = s.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn fs_load(win: &MainWindow, path: &str, dirs_only: bool) {
    let p = if path.is_empty() { None } else { Some(PathBuf::from(path)) };
    let p = p.map(|p| p.canonicalize().unwrap_or(p));
    match crate::engine::list_dir(p.as_deref(), dirs_only, false) {
        Ok(l) => {
            win.set_fs_path(l.path.display().to_string().into());
            win.set_fs_roots(ModelRc::new(VecModel::from(l.roots.iter().map(|r| SharedString::from(r.display().to_string())).collect::<Vec<_>>())));
            let rows: Vec<FsEntry> = l
                .entries
                .iter()
                .map(|e| FsEntry { name: e.name.clone().into(), path: e.path.display().to_string().into(), dir: e.dir, size_text: if e.dir { "".into() } else { human_bytes(e.size).into() } })
                .collect();
            win.set_fs_entries(ModelRc::new(VecModel::from(rows)));
        }
        Err(e) => win.set_dialog_error(format!("{e:#}").into()),
    }
}

/// Report the outcome of a native save dialog back to the UI thread (toast).
fn report_save(etx: &std::sync::mpsc::Sender<Evt>, what: &str, res: Option<Result<PathBuf>>) {
    match res {
        Some(Ok(p)) => {
            let _ = etx.send(Evt::Toast(format!("{what} {}", p.display()), "ok"));
        }
        Some(Err(e)) => {
            let _ = etx.send(Evt::Error(format!("{e:#}")));
        }
        None => {}
    }
}

fn wire_callbacks(win: &MainWindow, ctx: &UiCtx) {
    let c = ctx.clone();
    win.on_select_job(move |i| {
        let Some(w) = c.win.upgrade() else { return };
        let id = if i >= 0 { w.get_jobs().row_data(i as usize).map(|r| r.id as u64) } else { None };
        c.st.borrow_mut().selected_job = id;
        render(&c);
    });
    let c = ctx.clone();
    win.on_sort_by(move |k| {
        {
            let mut st = c.st.borrow_mut();
            if st.sort_key == k.as_str() {
                st.sort_desc = !st.sort_desc;
            } else {
                st.sort_desc = k == "started";
                st.sort_key = k.to_string();
            }
            if let Some(w) = c.win.upgrade() {
                w.set_sort_key(k);
                w.set_sort_desc(st.sort_desc);
            }
        }
        render(&c);
    });
    let c = ctx.clone();
    win.on_set_filter(move |f| {
        c.st.borrow_mut().filter = f.to_string();
        if let Some(w) = c.win.upgrade() {
            w.set_filter(f);
        }
        render(&c);
    });
    let c = ctx.clone();
    win.on_set_query(move |q| {
        c.st.borrow_mut().query = q.to_string();
        render(&c);
    });
    let c = ctx.clone();
    win.on_set_tab(move |t| {
        if let Some(w) = c.win.upgrade() {
            w.set_tab(t);
            w.set_details_collapsed(false);
        }
    });
    let c = ctx.clone();
    win.on_cancel_job(move |id| c.send(Cmd::Cancel(id as u64)));
    let c = ctx.clone();
    win.on_remove_job(move |id| c.send(Cmd::Remove(id as u64)));
    let c = ctx.clone();
    win.on_clear_finished(move || c.send(Cmd::ClearFinished));
    let c = ctx.clone();
    win.on_rescan(move || c.send(Cmd::Rescan));
    let c = ctx.clone();
    win.on_accept_offer(move |id, dest| c.send(Cmd::Accept(id.to_string(), nonempty(dest).map(PathBuf::from))));
    let c = ctx.clone();
    win.on_reject_offer(move |id| c.send(Cmd::Reject(id.to_string())));
    win.on_open_path(|p| crate::engine::open_in_system(&p));
    win.on_open_url(|u| crate::engine::open_in_system(&u));
    let c = ctx.clone();
    win.on_copy_text(move |t| {
        if crate::ui::copy_to_clipboard(&t) {
            c.toast("Copiado al portapapeles", "ok");
        } else {
            c.toast("Portapapeles no disponible", "err");
        }
    });
    let c = ctx.clone();
    win.on_toggle_theme(move || {
        let Some(w) = c.win.upgrade() else { return };
        let mut st = c.st.borrow_mut();
        st.dark = !st.dark;
        w.global::<Theme>().set_dark(st.dark);
    });
    let c = ctx.clone();
    win.on_load_history(move || c.send(Cmd::LoadHistory));
    let c = ctx.clone();
    win.on_dismiss_toast(move |id| {
        c.st.borrow_mut().toasts.retain(|t| t.0 != id);
        c.render_toasts();
    });
    let c = ctx.clone();
    win.on_open_settings(move || {
        if let Some(w) = c.win.upgrade() {
            w.set_dialog_error("".into());
            w.set_dialog("settings".into());
        }
    });
    let c = ctx.clone();
    win.on_open_share(move |i, what| {
        let Some(w) = c.win.upgrade() else { return };
        let Some(j) = w.get_jobs().row_data(i.max(0) as usize) else { return };
        let data = if what == "ticket" { j.ticket_uri.to_string() } else { j.link.to_string() };
        if data.is_empty() {
            return;
        }
        c.st.borrow_mut().selected_job = Some(j.id as u64);
        w.set_selected(i);
        w.set_share_what(what);
        w.set_qr_caption(data.clone().into());
        w.set_qr_image(qr_image(&data));
        w.set_dialog("share".into());
    });
    let c = ctx.clone();
    win.on_open_pairing(move || {
        let Some(w) = c.win.upgrade() else { return };
        let data = w.get_pairing_uri().to_string();
        if data.is_empty() {
            return c.toast("Sin dirección LAN: no se puede generar el ticket de emparejamiento", "err");
        }
        w.set_share_what("pair".into());
        w.set_qr_caption(data.clone().into());
        w.set_qr_image(qr_image(&data));
        w.set_dialog("share".into());
    });
    let c = ctx.clone();
    win.on_save_ticket(move |uri| {
        let etx = c.etx.clone();
        let t = match crate::ticket::Ticket::from_uri(&uri) {
            Ok(t) => t,
            Err(e) => return c.toast(format!("{e:#}"), "err"),
        };
        let default = t.default_filename();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new().set_file_name(&default).add_filter("uni-share ticket", &["unishare"]).save_file();
            let res = picked.map(|p| t.save(&p).map(|_| p));
            report_save(&etx, "Ticket guardado en", res);
        });
    });
    let c = ctx.clone();
    win.on_save_qr(move |data| {
        let etx = c.etx.clone();
        let data = data.to_string();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new().set_file_name("qr.svg").add_filter("SVG", &["svg"]).save_file();
            let res = picked.map(|p| crate::ui::qr_svg(&data).context("data too long for a QR").and_then(|svg| std::fs::write(&p, svg).map(|_| p).map_err(Into::into)));
            report_save(&etx, "QR guardado en", res);
        });
    });
    let c = ctx.clone();
    win.on_fs_open(move |p| {
        if let Some(w) = c.win.upgrade() {
            fs_load(&w, &p, w.get_fs_dirs_only());
        }
    });
    let c = ctx.clone();
    win.on_fs_pick(move |p| {
        let Some(w) = c.win.upgrade() else { return };
        let target = w.get_fs_target_field().to_string();
        let p = p.to_string();
        match target.as_str() {
            "f-path" => {
                w.set_f_path(p.clone().into());
                w.set_dialog("new".into());
                w.set_preview_text(preview_path(&p).into());
            }
            "f-url" => {
                w.set_f_url(p.clone().into());
                w.set_dialog("new".into());
                w.set_preview_text(preview_ticket(&p).into());
            }
            "f-dest" => {
                w.set_f_dest(p.into());
                w.set_dialog("new".into());
            }
            "s-dir" => {
                w.set_s_dir(p.into());
                w.set_dialog("settings".into());
            }
            t if t.starts_with("offer:") => {
                c.send(Cmd::Accept(t[6..].to_string(), Some(PathBuf::from(p))));
                w.set_dialog("".into());
            }
            _ => w.set_dialog("".into()),
        }
    });
    let c = ctx.clone();
    win.on_preview_path(move |p| {
        if let Some(w) = c.win.upgrade() {
            w.set_preview_text(preview_path(&p).into());
        }
    });
    let c = ctx.clone();
    win.on_preview_ticket(move |p| {
        if let Some(w) = c.win.upgrade() {
            w.set_preview_text(preview_ticket(&p).into());
        }
    });
    let c = ctx.clone();
    win.on_confirm_yes(move || {
        // Only one confirm flow today: cancel selected job.
        let Some(w) = c.win.upgrade() else { return };
        let sel = w.get_selected();
        if sel >= 0 {
            if let Some(j) = w.get_jobs().row_data(sel as usize) {
                c.send(Cmd::Cancel(j.id as u64));
            }
        }
    });
    let c = ctx.clone();
    win.on_submit_settings(move || {
        let Some(w) = c.win.upgrade() else { return };
        let patch = ConfigPatch {
            device_name: nonempty(w.get_s_name()),
            download_dir: nonempty(w.get_s_dir()),
            rate_limit_mbps: w.get_s_rate().trim().parse().ok(),
            auto_accept: Some(w.get_s_auto()),
            pin: Some(w.get_s_pin().to_string()),
            notifications: Some(w.get_s_notif()),
            compress_folders: Some(w.get_s_compress()),
            expiry_days: w.get_s_expiry().trim().parse().ok(),
            parallel_parts: w.get_s_parallel().trim().parse().ok(),
        };
        w.set_dialog("".into());
        c.send(Cmd::PatchConfig(patch));
    });
    let c = ctx.clone();
    win.on_submit_new(move || {
        let Some(w) = c.win.upgrade() else { return };
        w.set_dialog_error("".into());
        let err = |m: &str| w.set_dialog_error(m.into());
        match w.get_new_mode() {
            0 => {
                let Some(path) = nonempty(w.get_f_path()) else { return err("Indica el archivo o carpeta") };
                let dev_i = w.get_f_device();
                let dev = if dev_i >= 0 { w.get_devices().row_data(dev_i as usize) } else { None };
                let target = nonempty(w.get_f_target()).or_else(|| dev.as_ref().map(|d| d.addr.to_string()));
                let Some(target) = target else { return err("Elige un dispositivo o escribe una dirección") };
                c.send(Cmd::SendLan(SendLanReq { path, target, fingerprint: dev.map(|d| d.fingerprint.to_string()), port: None, pin: nonempty(w.get_f_pin()), compress: w.get_f_compress() }));
            }
            1 => {
                let Some(path) = nonempty(w.get_f_path()) else { return err("Indica el archivo o carpeta") };
                c.send(Cmd::SendGlobal(SendGlobalReq {
                    path,
                    password: nonempty(w.get_f_password()),
                    expiry_days: w.get_f_expiry().trim().parse().ok(),
                    max_downloads: w.get_f_max().trim().parse().ok(),
                    compress: w.get_f_compress(),
                    ticket: w.get_f_ticket(),
                    message: nonempty(w.get_f_message()),
                }));
            }
            2 => {
                let Some(url) = nonempty(w.get_f_url()) else { return err("Pega un link o ticket") };
                c.send(Cmd::Download(DownloadReq { url, password: nonempty(w.get_f_password()), dest: nonempty(w.get_f_dest()), force: w.get_f_force() }));
            }
            _ => {
                let links: Vec<String> = w.get_f_links().split_whitespace().map(str::to_string).collect();
                if links.is_empty() {
                    return err("Añade al menos un link");
                }
                c.send(Cmd::CreateTicket(TicketCreateReq { links, name: nonempty(w.get_f_name()), password: nonempty(w.get_f_password()), message: nonempty(w.get_f_message()), verify_from: nonempty(w.get_f_path()), output: None }));
                return; // dialog switches to "share" on TicketCreated
            }
        }
        w.set_dialog("".into());
        c.st.borrow_mut().filter = "active".into();
        w.set_filter("active".into());
    });
    let _ = win.on_shortcut(|_| {});
}

fn preview_path(p: &str) -> String {
    let path = Path::new(p);
    if !path.exists() {
        return String::new();
    }
    match crate::fsutil::collect_files(path) {
        Ok(files) => {
            let total = crate::fsutil::total_size(&files);
            let mut s = format!("{} archivo(s) · {}\n", files.len(), human_bytes(total));
            for f in files.iter().take(60) {
                s.push_str(&format!("{}  ({})\n", f.rel_path, human_bytes(f.size)));
            }
            if files.len() > 60 {
                s.push_str(&format!("… y {} más", files.len() - 60));
            }
            s
        }
        Err(e) => format!("{e:#}"),
    }
}

/// Minimal `%XX` decoding for `file://` URIs handed over by desktop launchers.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn preview_ticket(v: &str) -> String {
    let v = v.trim();
    if !(crate::ticket::looks_like_ticket(v) || v.starts_with('{')) {
        return String::new();
    }
    match Engine::parse_ticket(v) {
        Ok(t) => {
            let mut s = format!("🎫 {}{}\n", t.summary(), if t.is_expired() { "  (EXPIRADO)" } else { "" });
            for src in &t.sources {
                match src.lan_endpoint() {
                    Some(ep) => s.push_str(&format!(
                        "[LAN] {} · {} · huella {}{}\n",
                        ep.name.as_deref().unwrap_or("receptor"),
                        ep.addr(),
                        crate::lan::tls::short_fingerprint(&ep.fingerprint),
                        if ep.pin.is_some() { " · PIN incluido" } else { "" }
                    )),
                    None => s.push_str(&format!("[{}] {}{}\n", src.label(), src.url(), if src.password().is_some() { " 🔒" } else { "" })),
                }
            }
            for f in t.files.iter().take(40) {
                s.push_str(&format!("{}  ({}){}\n", f.path, human_bytes(f.size), if f.blake3.is_some() { " ✓" } else { "" }));
            }
            s
        }
        Err(e) => format!("{e:#}"),
    }
}
