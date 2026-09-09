//! Headless application **engine** shared by every GUI front-end (native
//! Slint window, local web UI) and reusable by the daemon.
//!
//! It owns:
//! * the embedded LAN receiver (mDNS announce + TLS server) and its incoming
//!   *offers* waiting for a user decision;
//! * the list of [`Job`]s (LAN send/receive, link upload, download) with
//!   byte-accurate progress, speed/ETA, current file, per-job log and
//!   cancellation;
//! * periodic LAN device discovery;
//! * the live [`Config`] (editable at runtime and persisted).
//!
//! Hot upload/download loops only touch an `AtomicU64`; speed is derived from
//! samples when a front-end calls [`Engine::tick`]. Front-ends never talk to
//! the network layer directly — they call the async action methods here, so
//! the Slint and web UIs behave identically.

use crate::config::Config;
use crate::fsutil::{collect_files, total_size};
use crate::history::{History, Kind, Status};
use crate::lan::client::{Sender, build_manifest, hash_all};
use crate::lan::discovery::{Announcer, Device, discover, parse_target};
use crate::lan::server::{Decision, IncomingOffer, Progress, ServerHandle, ServerOptions};
use crate::lan::tls::Identity;
use crate::signing::SigningKey;
use crate::ticket::{Source, Ticket, TicketFile};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, oneshot};
use tokio::task::AbortHandle;

/// Incoming LAN offer waiting for the user.
#[derive(Clone, Serialize, Debug)]
pub struct PendingOffer {
    pub transfer_id: String,
    pub sender: String,
    pub sender_fingerprint: String,
    pub peer: String,
    pub name: String,
    pub total_size: u64,
    pub compressed: bool,
    pub files: Vec<JobFile>,
    pub received_at: i64,
    /// Bytes already on disk from an earlier, interrupted run (0 = fresh).
    #[serde(default)]
    pub resume_bytes: u64,
    #[serde(default)]
    pub resume_files: usize,
}

#[derive(Clone, Serialize, Deserialize, Default, Debug, PartialEq, Eq)]
pub struct JobFile {
    pub path: String,
    pub size: u64,
}

#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    LanSend,
    LanReceive,
    GlobalUpload,
    Download,
}

impl JobKind {
    pub fn label(self) -> &'static str {
        match self {
            JobKind::LanSend => "Envío LAN",
            JobKind::LanReceive => "Recepción LAN",
            JobKind::GlobalUpload => "Subida (link)",
            JobKind::Download => "Descarga",
        }
    }
    pub fn is_outgoing(self) -> bool {
        matches!(self, JobKind::LanSend | JobKind::GlobalUpload)
    }
}

#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn label(self) -> &'static str {
        match self {
            JobState::Queued => "En cola",
            JobState::Running => "En curso",
            JobState::Completed => "Completada",
            JobState::Failed => "Fallida",
            JobState::Cancelled => "Cancelada",
        }
    }
    pub fn is_active(self) -> bool {
        matches!(self, JobState::Queued | JobState::Running)
    }
}

/// Compact, UI-oriented view of a [`crate::scan::Report`] attached to a job.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct ScanSummary {
    pub severity: crate::scan::Severity,
    pub summary: String,
    /// Engines that ran (`heuristics`, `clamav (…)`).
    pub engines: Vec<String>,
    pub files: usize,
    pub dangers: usize,
    pub warnings: usize,
    pub duration_ms: u64,
    /// Unix ms when the scan finished.
    pub at: i64,
    /// Only files with findings, worst first.
    pub findings: Vec<ScanFileSummary>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct ScanFileSummary {
    pub path: String,
    pub kind: String,
    pub severity: crate::scan::Severity,
    pub findings: Vec<ScanFinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantined: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct ScanFinding {
    pub severity: crate::scan::Severity,
    pub code: String,
    pub message: String,
}

impl ScanSummary {
    pub fn from_report(r: &crate::scan::Report) -> Self {
        let mut findings: Vec<ScanFileSummary> = r
            .files
            .iter()
            .filter(|f| !f.is_clean())
            .map(|f| ScanFileSummary {
                path: f.path.display().to_string(),
                kind: f.kind.clone(),
                severity: f.severity(),
                findings: f
                    .findings
                    .iter()
                    .filter(|x| x.severity != crate::scan::Severity::Info)
                    .map(|x| ScanFinding { severity: x.severity, code: x.code.clone(), message: x.message.clone() })
                    .collect(),
                quarantined: f.quarantined.as_ref().map(|q| q.display().to_string()),
            })
            .collect();
        findings.sort_by(|a, b| b.severity.cmp(&a.severity).then_with(|| a.path.cmp(&b.path)));
        Self {
            severity: r.severity(),
            summary: r.summary(),
            engines: r.engines.clone(),
            files: r.files.len(),
            dangers: r.dangers(),
            warnings: r.warnings(),
            duration_ms: r.duration_ms,
            at: chrono::Utc::now().timestamp_millis(),
            findings,
        }
    }
}

#[derive(Clone, Serialize, Debug)]
pub struct Job {
    pub id: u64,
    pub kind: JobKind,
    pub name: String,
    /// Target device / backend / source URL.
    pub peer: String,
    pub total: u64,
    pub done: u64,
    pub state: JobState,
    pub message: String,
    pub current_file: String,
    pub link: Option<String>,
    pub ticket_uri: Option<String>,
    pub ticket_path: Option<String>,
    pub dest: Option<String>,
    pub files: Vec<JobFile>,
    /// Unix ms.
    pub started: i64,
    pub finished: Option<i64>,
    /// Bytes per second (≈4 s window).
    pub speed: u64,
    /// Seconds.
    pub eta: Option<u64>,
    pub log: Vec<String>,
    /// Outcome of the safety scan (receptions/downloads), once finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan: Option<crate::scan::Severity>,
    /// Structured result of the last safety scan, so the UIs can show *what* was found
    /// (per-file findings) instead of burying it in the job log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_report: Option<ScanSummary>,
    /// Files written to disk by this job (receptions/downloads) — lets the user re-scan later
    /// and lets the Android shell export them to the user's SAF folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    saved: Vec<PathBuf>,
    /// Original request, kept so a failed/cancelled job can be retried.
    #[serde(skip)]
    origin: Option<JobOrigin>,
    /// `true` when the job can be re-launched from the UI (failed/cancelled send/upload/download).
    pub retryable: bool,
    #[serde(skip)]
    samples: VecDeque<(Instant, u64)>,
}

#[derive(Clone, Debug)]
pub enum JobOrigin {
    Lan(SendLanReq),
    Global(SendGlobalReq),
    Download(DownloadReq),
}

impl Job {
    pub fn is_active(&self) -> bool {
        self.state.is_active()
    }
    pub fn percent(&self) -> f32 {
        if self.total == 0 {
            return if self.state == JobState::Completed { 100.0 } else { 0.0 };
        }
        (self.done as f64 / self.total as f64 * 100.0).min(100.0) as f32
    }
}

/// Live handles for a running job (never serialised).
struct JobHandle {
    counter: Arc<AtomicU64>,
    current_file: Arc<std::sync::Mutex<String>>,
    abort: Option<AbortHandle>,
}

/// Shared progress sinks handed to the network layer.
#[derive(Clone)]
pub struct Sink {
    pub counter: Arc<AtomicU64>,
    pub current_file: Arc<std::sync::Mutex<String>>,
}

impl Sink {
    pub fn add(&self, n: u64) {
        self.counter.fetch_add(n, Ordering::Relaxed);
    }
    pub fn set(&self, n: u64) {
        self.counter.store(n, Ordering::Relaxed);
    }
    pub fn file(&self, f: &str) {
        if let Ok(mut g) = self.current_file.lock() {
            if *g != f {
                *g = f.to_string();
            }
        }
    }
}

/// Aggregated snapshot for front-ends (cheap to clone).
#[derive(Clone, Serialize, Debug)]
pub struct Snapshot {
    pub device_name: String,
    pub fingerprint: String,
    pub fingerprint_full: String,
    pub lan_addr: String,
    pub lan_port: u16,
    pub local_ips: Vec<String>,
    /// `unishare:` pairing ticket for this receiver (empty when no LAN address).
    pub pairing_uri: String,
    /// Fingerprint of this device's Ed25519 ticket-signing key.
    pub signer_fingerprint: String,
    pub sign_tickets: bool,
    pub scan_enabled: bool,
    pub scan_clamav: bool,
    pub scan_yara: bool,
    pub scan_on_danger: crate::scan::DangerAction,
    /// `Some(label)` when a ClamAV binary was found on this machine.
    pub clamav: Option<String>,
    /// YARA status: `yara-x (3 regla(s), 1 archivo(s))`, `yara: sin reglas en …`, `yara: no compilado …`.
    pub yara: String,
    /// Where the user drops `*.yar` / `*.yara` files.
    pub yara_rules_dir: PathBuf,
    pub notices: Vec<Notice>,
    pub download_dir: PathBuf,
    pub pin_required: bool,
    pub auto_accept: bool,
    pub pending: Vec<PendingOffer>,
    pub jobs: Vec<Job>,
    pub devices: Vec<Device>,
    pub devices_scanned_ago: Option<u64>,
    pub speed_down: u64,
    pub speed_up: u64,
    pub uptime: u64,
    pub version: &'static str,
}

impl Snapshot {
    /// A realistic, fully populated snapshot used by `uni-share app --demo`
    /// (design reviews and screenshots without any network activity).
    pub fn demo() -> Snapshot {
        use std::net::{IpAddr, Ipv4Addr};
        let now = chrono::Utc::now().timestamp_millis();
        let mk = |id: u64, kind: JobKind, name: &str, peer: &str, total: u64, done: u64, state: JobState, speed: u64, files: Vec<(&str, u64)>| Job {
            id,
            kind,
            name: name.into(),
            peer: peer.into(),
            total,
            done,
            state,
            message: match state {
                JobState::Failed => "El receptor rechazó la transferencia".into(),
                JobState::Completed => "Verificación BLAKE3 correcta".into(),
                _ => String::new(),
            },
            current_file: files.first().map(|f| f.0.to_string()).unwrap_or_default(),
            link: matches!(kind, JobKind::GlobalUpload).then(|| "https://storage.to/c/G7pzkDNFy".to_string()),
            ticket_uri: None,
            ticket_path: None,
            dest: matches!(kind, JobKind::LanReceive | JobKind::Download).then(|| format!("/home/user/Descargas/{name}")),
            files: files.iter().map(|(p, s)| JobFile { path: (*p).into(), size: *s }).collect(),
            started: now - 1000 * (60 * id as i64 + 12),
            finished: (!state.is_active()).then_some(now - 1000 * 20 * id as i64),
            speed,
            eta: (state.is_active() && speed > 0).then(|| (total - done) / speed),
            log: vec![
                "Conectado a PC-Sala — huella 56EC-D8F3-F330-F460".into(),
                "Manifiesto aceptado (3 archivos, 1.9 GiB)".into(),
                format!("Enviando {}…", files.first().map(|f| f.0).unwrap_or("")),
            ],
            scan: (matches!(kind, JobKind::LanReceive | JobKind::Download) && state == JobState::Completed).then_some(if id == 4 { crate::scan::Severity::Warning } else { crate::scan::Severity::Info }),
            scan_report: (matches!(kind, JobKind::LanReceive | JobKind::Download) && state == JobState::Completed).then(|| ScanSummary {
                severity: if id == 4 { crate::scan::Severity::Warning } else { crate::scan::Severity::Info },
                summary: if id == 4 { "Análisis de seguridad: 0 peligroso(s), 1 con avisos de 3 archivo(s) (heuristics, clamav (clamdscan 1.4.1))".into() } else { "Análisis de seguridad: 1 archivo(s) sin hallazgos (heuristics)".into() },
                engines: vec!["heuristics".into(), "clamav (clamdscan 1.4.1)".into()],
                files: if id == 4 { 3 } else { 1 },
                dangers: 0,
                warnings: usize::from(id == 4),
                duration_ms: 1_840,
                at: now - 1000 * 20 * id as i64 + 1_900,
                findings: if id == 4 {
                    vec![ScanFileSummary {
                        path: format!("/home/user/Descargas/{name}/DCIM/instalador-fotos.jpg.exe"),
                        kind: "pe".into(),
                        severity: crate::scan::Severity::Warning,
                        findings: vec![
                            ScanFinding { severity: crate::scan::Severity::Warning, code: "double_extension".into(), message: "Doble extensión: parece una imagen pero es un ejecutable de Windows".into() },
                            ScanFinding { severity: crate::scan::Severity::Warning, code: "magic_mismatch".into(), message: "El contenido (PE) no coincide con la extensión .jpg".into() },
                        ],
                        quarantined: None,
                    }]
                } else {
                    vec![]
                },
            }),
            saved: Vec::new(),
            origin: None,
            retryable: matches!(state, JobState::Failed | JobState::Cancelled) && kind != JobKind::LanReceive,
            samples: VecDeque::new(),
        };
        let gib = 1024u64 * 1024 * 1024;
        let mib = 1024u64 * 1024;
        Snapshot {
            device_name: "Laptop-Maria".into(),
            fingerprint: "9A1C-77E0-B2D4-5F08".into(),
            fingerprint_full: "9A1C77E0B2D45F08".repeat(4),
            lan_addr: "0.0.0.0:47820".into(),
            lan_port: 47820,
            local_ips: vec!["192.168.1.78".into()],
            pairing_uri: Ticket::lan_pairing("Portátil de Ana", "192.168.1.78", 47820, &"3f".repeat(32), None).to_uri().unwrap_or_default(),
            signer_fingerprint: "9c1e:7a40:b2f3:0d58:e6a1:44c9:1b7d:f02e".into(),
            sign_tickets: true,
            scan_enabled: true,
            scan_clamav: true,
            scan_yara: true,
            scan_on_danger: crate::scan::DangerAction::Quarantine,
            clamav: None,
            yara: "yara-x (12 regla(s), 2 archivo(s))".into(),
            yara_rules_dir: PathBuf::from("/home/user/.config/uni-share/rules"),
            notices: vec![],
            download_dir: PathBuf::from("/home/user/Descargas"),
            pin_required: false,
            auto_accept: false,
            pending: vec![PendingOffer {
                transfer_id: "demo-offer".into(),
                sender: "PC-Sala".into(),
                sender_fingerprint: "56EC-D8F3-F330-F460".into(),
                peer: "192.168.1.45".into(),
                name: "fotos-verano".into(),
                total_size: 812 * mib,
                compressed: false,
                files: vec![JobFile { path: "fotos-verano/IMG_2041.jpg".into(), size: 6 * mib }, JobFile { path: "fotos-verano/IMG_2042.jpg".into(), size: 7 * mib }],
                received_at: now,
                resume_bytes: 0,
                resume_files: 0,
            }],
            jobs: vec![
                mk(1, JobKind::LanSend, "proyecto-final", "PC-Sala", 2 * gib, 1_350 * mib, JobState::Running, 48 * mib, vec![("proyecto-final/render.mp4", 1_900 * mib), ("proyecto-final/notas.md", 12_000), ("proyecto-final/assets/logo.svg", 48_000)]),
                mk(2, JobKind::Download, "dataset.tar.zst", "storage.to", 640 * mib, 200 * mib, JobState::Running, 9 * mib, vec![("dataset.tar.zst", 640 * mib)]),
                mk(3, JobKind::GlobalUpload, "entrega-cliente", "storage.to", 320 * mib, 320 * mib, JobState::Completed, 0, vec![("entrega-cliente/informe.pdf", 20 * mib), ("entrega-cliente/anexos.zip", 300 * mib)]),
                mk(4, JobKind::LanReceive, "backup-movil", "Pixel-de-Ana", 5 * gib, 5 * gib, JobState::Completed, 0, vec![("backup-movil/DCIM.tar", 5 * gib)]),
                mk(5, JobKind::LanSend, "video-boda.mov", "TV-Salon", 9 * gib, 400 * mib, JobState::Failed, 0, vec![("video-boda.mov", 9 * gib)]),
                mk(6, JobKind::Download, "swisstransfer-8f2a", "swisstransfer.com", 0, 0, JobState::Queued, 0, vec![]),
                mk(7, JobKind::GlobalUpload, "cv-2026.pdf", "storage.to", 3 * mib, mib, JobState::Cancelled, 0, vec![("cv-2026.pdf", 3 * mib)]),
            ],
            devices: vec![
                Device { name: "PC-Sala".into(), addresses: vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 45))], port: 47820, fingerprint: "56EC-D8F3-F330-F460".into(), version: crate::APP_VERSION.into(), requires_pin: false, online: true },
                Device { name: "Pixel-de-Ana".into(), addresses: vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 90))], port: 47820, fingerprint: "1F0A-3C3C-98B1-E2D7".into(), version: crate::APP_VERSION.into(), requires_pin: true, online: true },
                Device { name: "TV-Salon".into(), addresses: vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 12))], port: 47820, fingerprint: "77AD-10BE-4C40-0E91".into(), version: crate::APP_VERSION.into(), requires_pin: false, online: true },
            ],
            devices_scanned_ago: Some(4),
            speed_down: 9 * mib,
            speed_up: 48 * mib,
            uptime: 3_725,
            version: crate::APP_VERSION,
        }
    }
}

// ───────────────────────── requests ─────────────────────────

#[derive(Deserialize, Debug, Clone, Default)]
pub struct SendLanReq {
    pub path: String,
    /// `ip[:port]` or a discovered device name.
    pub target: String,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub pin: Option<String>,
    #[serde(default)]
    pub compress: bool,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct SendGlobalReq {
    pub path: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub expiry_days: Option<u32>,
    #[serde(default)]
    pub max_downloads: Option<u32>,
    #[serde(default)]
    pub compress: bool,
    /// Also write a .unishare ticket next to the source.
    #[serde(default)]
    pub ticket: bool,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct DownloadReq {
    /// URL, `unishare:` URI or path to a .unishare file.
    pub url: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub dest: Option<String>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct TicketCreateReq {
    pub links: Vec<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub verify_from: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct ConfigPatch {
    pub device_name: Option<String>,
    pub scan_enabled: Option<bool>,
    pub scan_clamav: Option<bool>,
    pub scan_yara: Option<bool>,
    pub scan_on_danger: Option<crate::scan::DangerAction>,
    pub download_dir: Option<String>,
    pub rate_limit_mbps: Option<u32>,
    pub auto_accept: Option<bool>,
    pub pin: Option<String>,
    pub notifications: Option<bool>,
    pub minimize_to_tray: Option<bool>,
    pub compress_folders: Option<bool>,
    pub sign_tickets: Option<bool>,
    pub expiry_days: Option<u32>,
    pub parallel_parts: Option<usize>,
}

/// One-shot message for the GUIs (`kind`: `info` | `ok` | `warn` | `err`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub id: u64,
    pub text: String,
    pub kind: String,
}

#[derive(Serialize, Debug, Clone)]
pub struct TicketCreated {
    pub ticket: Ticket,
    pub path: PathBuf,
    pub uri: String,
}

// ───────────────────────── engine ─────────────────────────

pub struct Engine {
    pub cfg: RwLock<Config>,
    pub cfg_path: PathBuf,
    pub history: History,
    pub identity: Identity,
    /// Ed25519 key used to sign the tickets this device creates.
    pub signing_key: SigningKey,
    pub lan_addr: SocketAddr,
    pub started_at: Instant,
    pending: RwLock<HashMap<String, (PendingOffer, oneshot::Sender<Decision>)>>,
    /// transfer_id → job id for accepted LAN receptions.
    receiving: RwLock<HashMap<String, u64>>,
    jobs: RwLock<Vec<Job>>,
    handles: RwLock<HashMap<u64, JobHandle>>,
    next_job: AtomicU64,
    visitor_token: Mutex<Option<String>>,
    /// Pending user-facing notices (scan results…) drained by the GUIs as toasts.
    notices: Mutex<Vec<Notice>>,
    /// Woken on every state mutation (jobs, offers, notices, devices) → SSE / UI refresh.
    changed: tokio::sync::Notify,
    /// ClamAV detection result at startup (`clamav (clamdscan 1.4.1)`).
    clamav_label: Option<String>,
    devices: RwLock<(Vec<Device>, Option<Instant>)>,
    _announcer: Announcer,
}

impl Engine {
    /// Start the LAN receiver + announcer and the background tasks.
    pub async fn start(cfg: Config, cfg_path: PathBuf, history: History) -> Result<Arc<Self>> {
        let identity = Identity::load_or_generate(&crate::config::data_dir(), &cfg.device_name)?;
        let signing_key = SigningKey::load_or_generate(&crate::config::data_dir())?;
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
                state_dir: Some(crate::config::data_dir()),
            },
        )
        .await?;
        let announcer = Announcer::start(&cfg.device_name, lan.addr.port(), &identity.fingerprint, cfg.pin.is_some())?;

        let eng = Arc::new(Engine {
            cfg_path,
            history,
            identity,
            signing_key,
            lan_addr: lan.addr,
            started_at: Instant::now(),
            pending: RwLock::new(HashMap::new()),
            receiving: RwLock::new(HashMap::new()),
            jobs: RwLock::new(Vec::new()),
            changed: tokio::sync::Notify::new(),
            handles: RwLock::new(HashMap::new()),
            next_job: AtomicU64::new(0),
            visitor_token: Mutex::new(cfg.global.storage_to_visitor_token.clone()),
            notices: Mutex::new(Vec::new()),
            clamav_label: crate::scan::clamav::locate(cfg.scan.clamav_path.as_deref()).map(|b| crate::scan::clamav::describe(&b)),
            devices: RwLock::new((Vec::new(), None)),
            _announcer: announcer,
            cfg: RwLock::new(cfg),
        });

        // Incoming offers → pending map (or auto-accept).
        let e = eng.clone();
        tokio::spawn(async move {
            while let Some(IncomingOffer { transfer_id, manifest, peer, resume, decision }) = lan.offers.recv().await {
                let (auto, dest, notif) = {
                    let c = e.cfg.read().await;
                    (c.auto_accept, c.download_dir.clone(), c.notifications)
                };
                let files: Vec<JobFile> = manifest.files.iter().map(|f| JobFile { path: f.path.clone(), size: f.size }).collect();
                if auto {
                    e.accept_inner(&transfer_id, &manifest.name, &manifest.sender, manifest.total_size, files, dest, decision).await;
                    continue;
                }
                let po = PendingOffer {
                    transfer_id: transfer_id.clone(),
                    sender: manifest.sender.clone(),
                    sender_fingerprint: crate::lan::tls::short_fingerprint(&manifest.sender_fingerprint),
                    peer: peer.ip().to_string(),
                    name: manifest.name.clone(),
                    total_size: manifest.total_size,
                    compressed: manifest.compressed_archive,
                    files,
                    received_at: chrono::Utc::now().timestamp_millis(),
                    resume_bytes: resume.as_ref().map(|r| r.bytes_done).unwrap_or(0),
                    resume_files: resume.as_ref().map(|r| r.files_done).unwrap_or(0),
                };
                if notif {
                    crate::ui::notify("uni-share: solicitud entrante", &format!("{} quiere enviarte {}", manifest.sender, manifest.name));
                }
                e.pending.write().await.insert(transfer_id, (po, decision));
                e.touch();
            }
        });

        // LAN reception progress → job counters.
        let e = eng.clone();
        let mut prx = lan.progress.clone();
        tokio::spawn(async move {
            while prx.changed().await.is_ok() {
                let p: Progress = prx.borrow().clone();
                let jid = e.receiving.read().await.get(&p.transfer_id).copied();
                let Some(jid) = jid else { continue };
                if let Some(sink) = e.sink(jid).await {
                    sink.set(p.received);
                    sink.file(&p.current_file);
                }
                if p.finished {
                    e.receiving.write().await.remove(&p.transfer_id);
                    match &p.error {
                        Some(err) => e.finish_job(jid, JobState::Failed, format!("Error: {err}")).await,
                        None => e.finish_job(jid, JobState::Completed, "Recibido y verificado (BLAKE3)").await,
                    }
                }
            }
        });

        // Completed receptions → history + notification + safety scan.
        let e = eng.clone();
        tokio::spawn(async move {
            while let Some(done) = lan.completed.recv().await {
                if let Ok(id) = e.history.start(Kind::LanReceive, &done.name, done.total, &done.sender, done.files_total as u32) {
                    let _ = e.history.finish(id, if done.error.is_some() { Status::Failed } else { Status::Completed }, done.error.as_deref());
                }
                if e.cfg.read().await.notifications && done.error.is_none() {
                    crate::ui::notify("uni-share: transferencia recibida", &done.name);
                }
                if done.error.is_none() && !done.saved.is_empty() {
                    let jid = e.jobs.read().await.iter().rev().find(|j| j.kind == JobKind::LanReceive && j.name == done.name).map(|j| j.id);
                    e.scan_saved(jid, done.saved.clone()).await;
                }
            }
        });

        // Periodic discovery.
        let e = eng.clone();
        tokio::spawn(async move {
            loop {
                e.refresh_devices().await;
                tokio::time::sleep(Duration::from_secs(12)).await;
            }
        });

        Ok(eng)
    }

    // ───────── jobs ─────────

    async fn new_job(&self, kind: JobKind, name: &str, peer: &str, total: u64, files: Vec<JobFile>) -> (u64, Sink) {
        let id = self.next_job.fetch_add(1, Ordering::Relaxed) + 1;
        let sink = Sink { counter: Arc::new(AtomicU64::new(0)), current_file: Arc::new(std::sync::Mutex::new(String::new())) };
        let job = Job {
            id,
            kind,
            name: name.into(),
            peer: peer.into(),
            total,
            done: 0,
            state: JobState::Running,
            message: String::new(),
            current_file: String::new(),
            link: None,
            ticket_uri: None,
            ticket_path: None,
            dest: None,
            files,
            started: chrono::Utc::now().timestamp_millis(),
            finished: None,
            speed: 0,
            eta: None,
            log: Vec::new(),
            scan: None,
            scan_report: None,
            saved: Vec::new(),
            origin: None,
            retryable: false,
            samples: VecDeque::new(),
        };
        {
            let mut jobs = self.jobs.write().await;
            jobs.insert(0, job);
            jobs.truncate(300);
        }
        self.touch();
        self.handles.write().await.insert(id, JobHandle { counter: sink.counter.clone(), current_file: sink.current_file.clone(), abort: None });
        (id, sink)
    }

    async fn set_abort(&self, id: u64, abort: AbortHandle) {
        if let Some(h) = self.handles.write().await.get_mut(&id) {
            h.abort = Some(abort);
        }
    }

    async fn sink(&self, id: u64) -> Option<Sink> {
        self.handles.read().await.get(&id).map(|h| Sink { counter: h.counter.clone(), current_file: h.current_file.clone() })
    }

    pub async fn update_job(&self, id: u64, f: impl FnOnce(&mut Job)) {
        if let Some(j) = self.jobs.write().await.iter_mut().find(|j| j.id == id) {
            f(j);
        }
        self.touch();
    }

    /// Signal listeners (SSE clients, native UI) that the snapshot changed.
    pub fn touch(&self) {
        self.changed.notify_waiters();
    }

    /// Resolves on the next state change (or after `max`, whichever comes first).
    pub async fn changed(&self, max: std::time::Duration) {
        let _ = tokio::time::timeout(max, self.changed.notified()).await;
    }

    pub async fn log(&self, id: u64, line: impl Into<String>) {
        let line = format!("{} {}", chrono::Local::now().format("%H:%M:%S"), line.into());
        tracing::info!(job = id, "{line}");
        self.update_job(id, |j| {
            j.log.push(line);
            if j.log.len() > 200 {
                j.log.remove(0);
            }
        })
        .await;
    }

    pub async fn push_notice(&self, text: impl Into<String>, kind: &str) {
        let mut n = self.notices.lock().await;
        let id = n.last().map(|x| x.id + 1).unwrap_or(1);
        n.push(Notice { id, text: text.into(), kind: kind.into() });
        if n.len() > 20 {
            n.remove(0);
        }
        drop(n);
        self.touch();
    }

    /// Drop notices the UI has already shown (`up_to` inclusive).
    pub async fn ack_notices(&self, up_to: u64) {
        self.notices.lock().await.retain(|n| n.id > up_to);
        self.touch();
    }

    /// Run the local safety scanner on freshly written paths; results go to the
    /// job log/message (when a job is known), the tracing log and the toast queue.
    pub async fn scan_saved(&self, job: Option<u64>, paths: Vec<PathBuf>) -> Option<crate::scan::Report> {
        if let Some(id) = job {
            let p = paths.clone();
            self.update_job(id, |j| j.saved = p).await;
        }
        let cfg = self.cfg.read().await.scan.clone();
        if !cfg.enabled || paths.is_empty() {
            return None;
        }
        self.scan_with(job, paths, cfg).await
    }

    /// On-demand re-scan of everything a finished job wrote to disk, regardless of the
    /// `[scan] enabled` switch (report-only unless the policy says quarantine/delete).
    pub async fn rescan_job(&self, id: u64) -> Result<crate::scan::Report> {
        let job = self.job(id).await.context("transferencia no encontrada")?;
        if job.is_active() {
            bail!("la transferencia todavía está en curso");
        }
        let mut paths = job.saved.clone();
        if paths.is_empty() {
            // Older jobs / LAN receptions without a recorded file list: fall back to dest + file names.
            if let Some(dest) = job.dest.as_deref() {
                let dest = PathBuf::from(dest);
                paths = job.files.iter().map(|f| dest.join(&f.path)).filter(|p| p.exists()).collect();
                if paths.is_empty() && !job.files.is_empty() && dest.join(&job.name).exists() {
                    paths.push(dest.join(&job.name));
                }
            }
        }
        if paths.is_empty() {
            bail!("no hay archivos guardados que analizar para esta transferencia");
        }
        let mut cfg = self.cfg.read().await.scan.clone();
        cfg.enabled = true;
        self.log(id, "Análisis de seguridad bajo demanda…").await;
        Ok(self.scan_with(Some(id), paths, cfg).await.expect("scan enabled"))
    }

    /// Re-launch a failed/cancelled send/upload/download with its original request.
    /// The old entry is removed from the list; returns the new job id.
    pub async fn retry_job(self: &Arc<Self>, id: u64) -> Result<u64> {
        let job = self.job(id).await.context("transferencia no encontrada")?;
        ensure!(!job.is_active(), "la transferencia todavía está en curso");
        let origin = job.origin.clone().context("esta transferencia no se puede reintentar (recepción LAN o entrada antigua)")?;
        let new = match origin {
            JobOrigin::Lan(r) => self.send_lan(r).await?,
            JobOrigin::Global(r) => self.send_global(r).await?,
            JobOrigin::Download(mut r) => {
                // Resume partial downloads instead of failing on existing files.
                r.force = true;
                self.download(r).await?
            }
        };
        self.remove_job(id).await;
        self.log(new, format!("Reintento de la transferencia #{id}")).await;
        Ok(new)
    }

    async fn scan_with(&self, job: Option<u64>, paths: Vec<PathBuf>, cfg: crate::scan::ScanConfig) -> Option<crate::scan::Report> {
        if let Some(id) = job {
            self.log(id, "Análisis de seguridad en curso…").await;
        }
        let report = crate::scan::scan_paths_async(paths, cfg).await;
        let severity = report.severity();
        let summary = report.summary();
        if let Some(id) = job {
            self.log(id, summary.clone()).await;
            for line in report.detail().lines() {
                self.log(id, line.trim_end()).await;
            }
            let structured = ScanSummary::from_report(&report);
            self.update_job(id, |j| {
                j.scan = Some(severity);
                j.scan_report = Some(structured.clone());
                if severity != crate::scan::Severity::Info {
                    j.message = summary.clone();
                }
            })
            .await;
            // Keep the verdict with the history record too (the record is closed before the scan runs).
            if let Some((kind, name)) = self.jobs.read().await.iter().find(|j| j.id == id).map(|j| (j.kind, j.name.clone())) {
                let hk = match kind {
                    JobKind::LanReceive => Some(Kind::LanReceive),
                    JobKind::Download => Some(Kind::Download),
                    _ => None,
                };
                if let Some(hk) = hk {
                    let patch = serde_json::json!({
                        "scan": severity,
                        "scan_summary": summary,
                        "scan_dangers": structured.dangers,
                        "scan_warnings": structured.warnings,
                        "scan_files": structured.files,
                    });
                    if let Err(e) = self.history.merge_meta_latest(hk, &name, &patch) {
                        tracing::debug!("history meta (scan): {e:#}");
                    }
                }
            }
        }
        match severity {
            crate::scan::Severity::Info => tracing::info!("{summary}"),
            _ => tracing::warn!("{summary}\n{}", report.detail()),
        }
        if severity == crate::scan::Severity::Danger {
            self.push_notice(summary, "err").await;
        } else if severity == crate::scan::Severity::Warning {
            self.push_notice(summary, "warn").await;
        }
        Some(report)
    }

    async fn finish_job(&self, id: u64, state: JobState, message: impl Into<String>) {
        let message = message.into();
        self.update_job(id, |j| {
            j.state = state;
            j.message = message.clone();
            j.retryable = matches!(state, JobState::Failed | JobState::Cancelled) && j.origin.is_some();
            j.finished = Some(chrono::Utc::now().timestamp_millis());
            if state == JobState::Completed {
                j.done = j.total.max(j.done);
            }
            j.speed = 0;
            j.eta = None;
        })
        .await;
        self.log(id, message).await;
        if let Some(h) = self.handles.write().await.get_mut(&id) {
            h.abort = None;
        }
    }

    /// Pull counters into the job list and compute speed/ETA. Call before reading jobs.
    pub async fn tick(&self) {
        let now = Instant::now();
        let handles = self.handles.read().await;
        let mut jobs = self.jobs.write().await;
        for j in jobs.iter_mut().filter(|j| j.is_active()) {
            let Some(h) = handles.get(&j.id) else { continue };
            j.done = h.counter.load(Ordering::Relaxed);
            if let Ok(f) = h.current_file.lock() {
                j.current_file = f.clone();
            }
            j.samples.push_back((now, j.done));
            while j.samples.len() > 2 && now.duration_since(j.samples[0].0) > Duration::from_secs(4) {
                j.samples.pop_front();
            }
            if let (Some(first), Some(last)) = (j.samples.front(), j.samples.back()) {
                let dt = last.0.duration_since(first.0).as_secs_f64();
                if dt > 0.2 {
                    j.speed = ((last.1.saturating_sub(first.1)) as f64 / dt) as u64;
                }
            }
            j.eta = if j.speed > 0 && j.total > j.done { Some((j.total - j.done) / j.speed) } else { None };
        }
    }

    pub async fn jobs(&self) -> Vec<Job> {
        self.jobs.read().await.clone()
    }

    pub async fn job(&self, id: u64) -> Option<Job> {
        self.jobs.read().await.iter().find(|j| j.id == id).cloned()
    }

    pub async fn cancel_job(&self, id: u64) -> bool {
        let abort = self.handles.write().await.get_mut(&id).and_then(|h| h.abort.take());
        match abort {
            Some(a) => {
                a.abort();
                self.finish_job(id, JobState::Cancelled, "Cancelado por el usuario").await;
                true
            }
            None => false,
        }
    }

    pub async fn remove_job(&self, id: u64) -> bool {
        let mut jobs = self.jobs.write().await;
        if let Some(pos) = jobs.iter().position(|j| j.id == id && !j.is_active()) {
            jobs.remove(pos);
            self.handles.write().await.remove(&id);
            self.touch();
            true
        } else {
            false
        }
    }

    pub async fn clear_finished(&self) -> usize {
        let mut jobs = self.jobs.write().await;
        let before = jobs.len();
        let removed: Vec<u64> = jobs.iter().filter(|j| !j.is_active()).map(|j| j.id).collect();
        jobs.retain(|j| j.is_active());
        let mut h = self.handles.write().await;
        for id in removed {
            h.remove(&id);
        }
        let n = before - jobs.len();
        drop(h);
        drop(jobs);
        // Wake the SSE stream right away (otherwise the GUI waits for the 15 s keep-alive).
        self.touch();
        n
    }

    // ───────── offers ─────────

    pub async fn pending(&self) -> Vec<PendingOffer> {
        self.pending.read().await.values().map(|(p, _)| p.clone()).collect()
    }

    #[allow(clippy::too_many_arguments)]
    async fn accept_inner(
        &self,
        transfer_id: &str,
        name: &str,
        sender: &str,
        total: u64,
        files: Vec<JobFile>,
        dest: PathBuf,
        decision: oneshot::Sender<Decision>,
    ) -> u64 {
        let (jid, _) = self.new_job(JobKind::LanReceive, name, sender, total, files).await;
        self.update_job(jid, |j| j.dest = Some(dest.display().to_string())).await;
        self.receiving.write().await.insert(transfer_id.to_string(), jid);
        self.log(jid, format!("Aceptado desde {sender} → {}", dest.display())).await;
        if decision.send(Decision::Accept { dest_dir: dest }).is_err() {
            self.finish_job(jid, JobState::Failed, "El emisor canceló antes de empezar").await;
        }
        jid
    }

    /// Accept a pending offer into `dest` (default: download dir). Returns the job id.
    pub async fn accept_offer(&self, transfer_id: &str, dest: Option<PathBuf>) -> Result<u64> {
        let dest = dest.unwrap_or(self.download_dir().await);
        tokio::fs::create_dir_all(&dest).await.with_context(|| format!("creating {}", dest.display()))?;
        let (p, tx) = self.pending.write().await.remove(transfer_id).context("offer not found")?;
        Ok(self.accept_inner(transfer_id, &p.name, &p.sender, p.total_size, p.files.clone(), dest, tx).await)
    }

    pub async fn reject_offer(&self, transfer_id: &str) -> Result<()> {
        let (p, tx) = self.pending.write().await.remove(transfer_id).context("offer not found")?;
        self.touch();
        let _ = tx.send(Decision::Reject { reason: "rechazada por el usuario".into() });
        if let Ok(rid) = self.history.start(Kind::LanReceive, &p.name, p.total_size, &p.sender, p.files.len() as u32) {
            let _ = self.history.finish(rid, Status::Rejected, None);
        }
        Ok(())
    }

    // ───────── devices / config ─────────

    pub async fn refresh_devices(&self) -> Vec<Device> {
        let list = discover(Duration::from_millis(1800), Some(&self.identity.fingerprint)).await.unwrap_or_default();
        *self.devices.write().await = (list.clone(), Some(Instant::now()));
        self.touch();
        list
    }

    pub async fn devices(&self) -> Vec<Device> {
        self.devices.read().await.0.clone()
    }

    pub async fn download_dir(&self) -> PathBuf {
        self.cfg.read().await.download_dir.clone()
    }

    pub async fn config(&self) -> Config {
        self.cfg.read().await.clone()
    }

    /// Apply and persist a settings patch. Returns `true` when a restart is
    /// needed for the LAN receiver (name / PIN / port).
    pub async fn patch_config(&self, p: ConfigPatch) -> Result<bool> {
        let mut cfg: Config = self.cfg.read().await.clone();
        if let Some(v) = p.device_name.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
            cfg.device_name = v;
        }
        if let Some(v) = p.download_dir.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
            cfg.download_dir = PathBuf::from(v);
        }
        if let Some(v) = p.rate_limit_mbps {
            cfg.rate_limit_mbps = v;
        }
        if let Some(v) = p.auto_accept {
            cfg.auto_accept = v;
        }
        if let Some(v) = p.pin {
            cfg.pin = Some(v.trim().to_string()).filter(|v| !v.is_empty());
        }
        if let Some(v) = p.notifications {
            cfg.notifications = v;
        }
        if let Some(v) = p.minimize_to_tray {
            cfg.minimize_to_tray = v;
        }
        if let Some(v) = p.compress_folders {
            cfg.compress_folders = v;
        }
        if let Some(v) = p.sign_tickets {
            cfg.sign_tickets = v;
        }
        if let Some(v) = p.scan_enabled {
            cfg.scan.enabled = v;
        }
        if let Some(v) = p.scan_clamav {
            cfg.scan.clamav = v;
        }
        if let Some(v) = p.scan_yara {
            cfg.scan.yara = v;
        }
        if let Some(v) = p.scan_on_danger {
            cfg.scan.on_danger = v;
        }
        if let Some(v) = p.expiry_days {
            cfg.global.expiry_days = crate::global::storage_to::clamp_expiry_days(v);
        }
        if let Some(v) = p.parallel_parts {
            cfg.global.parallel_parts = v.clamp(1, 16);
        }
        cfg.validate()?;
        cfg.save(&self.cfg_path)?;
        let restart = {
            let cur = self.cfg.read().await;
            cur.device_name != cfg.device_name || cur.pin != cfg.pin || cur.lan_port != cfg.lan_port
        };
        *self.cfg.write().await = cfg;
        Ok(restart)
    }

    pub async fn snapshot(&self) -> Snapshot {
        self.tick().await;
        let jobs = self.jobs().await;
        let (devices, scanned) = self.devices.read().await.clone();
        let cfg = self.cfg.read().await;
        let (mut down, mut up) = (0u64, 0u64);
        for j in jobs.iter().filter(|j| j.is_active()) {
            if j.kind.is_outgoing() {
                up += j.speed;
            } else {
                down += j.speed;
            }
        }
        Snapshot {
            device_name: cfg.device_name.clone(),
            fingerprint: crate::lan::tls::short_fingerprint(&self.identity.fingerprint),
            fingerprint_full: self.identity.fingerprint.clone(),
            lan_addr: self.lan_addr.to_string(),
            lan_port: self.lan_addr.port(),
            local_ips: local_ips(),
            pairing_uri: local_ips()
                .first()
                // Not signed on purpose: the QR must stay scannable by a phone camera (an Ed25519
                // signature + key adds ~150 bytes ≈ 8 QR versions) and the pinned TLS fingerprint
                // inside the ticket already authenticates the receiver.
                .map(|ip| Ticket::lan_pairing(&cfg.device_name, ip, self.lan_addr.port(), &self.identity.fingerprint, cfg.pin.as_deref()))
                .and_then(|t| t.to_uri().ok())
                .unwrap_or_default(),
            signer_fingerprint: self.signing_key.fingerprint(),
            sign_tickets: cfg.sign_tickets,
            scan_enabled: cfg.scan.enabled,
            scan_clamav: cfg.scan.clamav,
            scan_yara: cfg.scan.yara,
            scan_on_danger: cfg.scan.on_danger,
            clamav: self.clamav_label.clone(),
            yara: crate::scan::yara::status(&cfg.scan.yara_rules_dir()),
            yara_rules_dir: cfg.scan.yara_rules_dir(),
            notices: self.notices.lock().await.clone(),
            download_dir: cfg.download_dir.clone(),
            pin_required: cfg.pin.is_some(),
            auto_accept: cfg.auto_accept,
            pending: self.pending().await,
            jobs,
            devices,
            devices_scanned_ago: scanned.map(|t| t.elapsed().as_secs()),
            speed_down: down,
            speed_up: up,
            uptime: self.started_at.elapsed().as_secs(),
            version: crate::APP_VERSION,
        }
    }

    async fn ensure_visitor_token(&self) -> Result<String> {
        let mut vt = self.visitor_token.lock().await;
        if let Some(t) = vt.clone().filter(|t| !t.is_empty()) {
            return Ok(t);
        }
        let mut cfg = self.cfg.write().await;
        let t = cfg.ensure_visitor_token(&self.cfg_path)?;
        *vt = Some(t.clone());
        Ok(t)
    }

    // ───────── actions ─────────

    /// Send a file/folder to a LAN device. Returns the job id immediately.
    pub async fn send_lan(self: &Arc<Self>, req: SendLanReq) -> Result<u64> {
        let path = PathBuf::from(&req.path);
        ensure!(path.exists(), "path not found: {}", req.path);
        let lan_port = self.cfg.read().await.lan_port;
        let mut fingerprint = req.fingerprint.clone().filter(|f| !f.is_empty());
        let mut pin = req.pin.clone().filter(|p| !p.is_empty());
        let target = req.target.trim().to_string();
        // A LAN pairing ticket (from `receive --qr`) pins the fingerprint and carries the PIN.
        let pairing = if crate::ticket::looks_like_ticket(&target) || target.starts_with('{') {
            let t = Self::parse_ticket(&target).context("reading pairing ticket")?;
            if let crate::signing::SignatureStatus::Invalid { reason } = t.verify_signature() {
                bail!("firma del ticket de emparejamiento inválida ({reason}): ticket manipulado");
            }
            Some(t.lan_endpoint().context("this ticket is a download ticket, not a LAN pairing ticket")?)
        } else {
            None
        };
        let (ip, port) = match &pairing {
            Some(ep) => {
                fingerprint = Some(ep.fingerprint.clone());
                if pin.is_none() {
                    pin = ep.pin.clone();
                }
                (crate::lan::discovery::resolve_host(&ep.host, ep.port).await?, ep.port)
            }
            None => match parse_target(&target, req.port.unwrap_or(lan_port)) {
                Some(t) => t,
                None => {
                    let dev = self
                        .devices()
                        .await
                        .into_iter()
                        .find(|d| d.name.eq_ignore_ascii_case(&target))
                        .context("target must be ip[:port], a discovered device name or a pairing ticket")?;
                    fingerprint.get_or_insert(dev.fingerprint.clone());
                    (dev.best_addr().context("device has no address")?, dev.port)
                }
            },
        };
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let (job, sink) = self.new_job(JobKind::LanSend, &name, &format!("{ip}:{port}"), 0, Vec::new()).await;
        let origin = JobOrigin::Lan(req.clone());
        self.update_job(job, move |j| j.origin = Some(origin)).await;
        let e = self.clone();
        let task = tokio::spawn(async move {
            let res: Result<()> = async {
                let (_tmp, files) = if req.compress && path.is_dir() {
                    e.log(job, "Comprimiendo carpeta (tar.zst)…").await;
                    let (t, f) = crate::lan::client::compress_to_temp(&path).await?;
                    (Some(t), f)
                } else {
                    (None, collect_files(&path)?)
                };
                let total = total_size(&files);
                let jf: Vec<JobFile> = files.iter().map(|f| JobFile { path: f.rel_path.clone(), size: f.size }).collect();
                e.update_job(job, |j| {
                    j.total = total;
                    j.files = jf;
                })
                .await;
                e.log(job, format!("Calculando BLAKE3 de {} archivo(s)…", files.len())).await;
                let s2 = sink.clone();
                let hashes = hash_all(&files, move |f| s2.file(f)).await?;
                let (device_name, rate) = {
                    let c = e.cfg.read().await;
                    (c.device_name.clone(), c.rate_limit_mbps as u64 * 125_000)
                };
                let manifest = build_manifest(&device_name, &e.identity.fingerprint, &name, &files, &hashes);
                let sender = Sender::new(ip, port, fingerprint.as_deref(), pin.clone())?;
                let info = sender.info().await?;
                e.update_job(job, |j| j.peer = format!("{} ({ip})", info.name)).await;
                e.log(job, format!("Oferta enviada a {} — esperando aceptación…", info.name)).await;
                let rid = e.history.start(Kind::LanSend, &name, total, &info.name, files.len() as u32)?;
                let s3 = sink.clone();
                let progress: crate::lan::client::ProgressFn = Arc::new(move |n, f| {
                    s3.add(n);
                    s3.file(f);
                });
                let r = sender.send(&manifest, &files, &hashes, progress, Duration::from_secs(300), rate).await;
                e.history.finish(rid, if r.is_ok() { Status::Completed } else { Status::Failed }, r.as_ref().err().map(|x| format!("{x:#}")).as_deref())?;
                r.map(|_| ())
            }
            .await;
            match res {
                Ok(()) => e.finish_job(job, JobState::Completed, "Enviado — verificación BLAKE3 correcta en el receptor").await,
                Err(err) => e.finish_job(job, JobState::Failed, format!("Error: {err:#}")).await,
            }
        });
        self.set_abort(job, task.abort_handle()).await;
        Ok(job)
    }

    /// Upload to storage.to and (optionally) write a `.unishare` ticket.
    pub async fn send_global(self: &Arc<Self>, req: SendGlobalReq) -> Result<u64> {
        let path = PathBuf::from(&req.path);
        ensure!(path.exists(), "path not found: {}", req.path);
        if let Some(p) = req.password.as_deref().filter(|p| !p.is_empty()) {
            ensure!((4..=100).contains(&p.chars().count()), "password must be 4-100 characters");
        }
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let (job, sink) = self.new_job(JobKind::GlobalUpload, &name, "storage.to", 0, Vec::new()).await;
        let origin = JobOrigin::Global(req.clone());
        self.update_job(job, move |j| j.origin = Some(origin)).await;
        let e = self.clone();
        let task = tokio::spawn(async move {
            let res: Result<()> = async {
                let (_tmp, files) = if req.compress && path.is_dir() {
                    e.log(job, "Comprimiendo carpeta (tar.zst)…").await;
                    let (t, f) = crate::lan::client::compress_to_temp(&path).await?;
                    (Some(t), f)
                } else {
                    (None, collect_files(&path)?)
                };
                let files: Vec<_> = files.into_iter().filter(|f| f.size > 0).collect();
                ensure!(!files.is_empty(), "nothing to upload (empty files are skipped by storage.to)");
                let total = total_size(&files);
                let jf: Vec<JobFile> = files.iter().map(|f| JobFile { path: f.rel_path.clone(), size: f.size }).collect();
                e.update_job(job, |j| {
                    j.total = total;
                    j.files = jf;
                })
                .await;
                let hashes = if req.ticket {
                    e.log(job, "Calculando BLAKE3 para el ticket…").await;
                    let s2 = sink.clone();
                    hash_all(&files, move |f| s2.file(f)).await?
                } else {
                    Vec::new()
                };
                let rid = e.history.start(Kind::GlobalUpload, &name, total, "storage.to", files.len() as u32)?;
                let s3 = sink.clone();
                let progress: crate::global::upload::ProgressFn = Arc::new(move |n| s3.add(n));
                let (api, bearer, expiry, parallel, device_name) = {
                    let c = e.cfg.read().await;
                    (c.global.storage_to_api.clone(), c.global.storage_to_token.clone(), c.global.expiry_days, c.global.parallel_parts, c.device_name.clone())
                };
                let password = req.password.clone().filter(|p| !p.is_empty());
                let opts = crate::global::upload::UploadOptions {
                    expiry_days: Some(crate::global::storage_to::clamp_expiry_days(req.expiry_days.unwrap_or(expiry))),
                    parallel_parts: parallel,
                    password: password.clone(),
                    max_downloads: req.max_downloads.filter(|m| *m > 0),
                };
                let token = e.ensure_visitor_token().await?;
                let client = crate::global::storage_to::Client::new(&api, &token, bearer.as_deref())?;
                e.log(job, format!("Subiendo {} archivo(s) a storage.to…", files.len())).await;
                let s4 = sink.clone();
                let r = crate::global::upload::upload_entries(&client, &files, &opts, progress, move |f| s4.file(f)).await;
                match &r {
                    Ok(o) => {
                        e.history.set_link(rid, &o.url, Some(&serde_json::json!({"owner_token": o.owner_token, "id": o.id, "kind": o.kind}).to_string()))?;
                        e.history.finish(rid, Status::Completed, None)?;
                    }
                    Err(err) => e.history.finish(rid, Status::Failed, Some(&format!("{err:#}")))?,
                }
                let o = r?;
                let mut ticket_uri = None;
                let mut ticket_path = None;
                if req.ticket {
                    let expires = o.expires_at.as_deref().and_then(|x| chrono::DateTime::parse_from_rfc3339(x).ok()).map(|d| d.with_timezone(&chrono::Utc));
                    let mut t = Ticket::from_upload(&name, &o.url, password.as_deref(), &device_name, &files, &hashes, expires);
                    t.message = req.message.clone().filter(|m| !m.trim().is_empty());
                    if e.cfg.read().await.sign_tickets {
                        t.sign(&e.signing_key)?;
                        e.log(job, format!("Ticket firmado (Ed25519, huella {})", e.signing_key.fingerprint())).await;
                    }
                    let out = path.parent().map(Path::to_path_buf).unwrap_or_default().join(t.default_filename());
                    t.save(&out)?;
                    ticket_uri = Some(t.to_uri_compact()?);
                    ticket_path = Some(out.display().to_string());
                    e.log(job, format!("Ticket .unishare guardado en {}", out.display())).await;
                }
                let url = o.url.clone();
                e.update_job(job, |j| {
                    j.link = Some(url);
                    j.ticket_uri = ticket_uri;
                    j.ticket_path = ticket_path;
                })
                .await;
                e.log(job, format!("Link: {}", o.url)).await;
                if e.cfg.read().await.notifications {
                    crate::ui::notify("uni-share: subida completada", &o.url);
                }
                Ok(())
            }
            .await;
            match res {
                Ok(()) => e.finish_job(job, JobState::Completed, "Subida completada").await,
                Err(err) => e.finish_job(job, JobState::Failed, format!("Error: {err:#}")).await,
            }
        });
        self.set_abort(job, task.abort_handle()).await;
        Ok(job)
    }

    /// Download from a link, `unishare:` URI or `.unishare` file.
    pub async fn download(self: &Arc<Self>, req: DownloadReq) -> Result<u64> {
        let src = req.url.trim().to_string();
        ensure!(!src.is_empty(), "empty url");
        let dest = req.dest.clone().filter(|d| !d.trim().is_empty()).map(PathBuf::from).unwrap_or(self.download_dir().await);
        let is_ticket = crate::ticket::looks_like_ticket(&src);
        if is_ticket && Self::parse_ticket(&src).map(|t| t.is_lan()).unwrap_or(false) {
            bail!("es un ticket de emparejamiento LAN (un receptor, no una descarga): úsalo como destino en «Enviar por LAN»");
        }
        let short = if is_ticket { "ticket".to_string() } else { src.clone() };
        let (job, sink) = self.new_job(JobKind::Download, &short, &short, 0, Vec::new()).await;
        let origin = JobOrigin::Download(req.clone());
        self.update_job(job, move |j| j.origin = Some(origin)).await;
        self.update_job(job, |j| j.dest = Some(dest.display().to_string())).await;
        let e = self.clone();
        let task = tokio::spawn(async move {
            let res: Result<(String, Vec<PathBuf>)> = async {
                tokio::fs::create_dir_all(&dest).await?;
                let s2 = sink.clone();
                let progress: crate::download::http::ProgressFn = Arc::new(move |n| s2.add(n));
                let s3 = sink.clone();
                let on_file = move |f: &str| s3.file(f);
                let pw = req.password.clone().filter(|p| !p.is_empty());
                let set_meta = |name: String, total: u64, files: Vec<JobFile>, peer: String| {
                    let e = e.clone();
                    async move {
                        e.update_job(job, |j| {
                            j.name = name;
                            j.total = total;
                            j.files = files;
                            j.peer = peer;
                        })
                        .await
                    }
                };
                if is_ticket {
                    let t = crate::ticket::resolve(&src)?;
                    match t.verify_signature() {
                        crate::signing::SignatureStatus::Invalid { reason } => {
                            bail!("firma Ed25519 del ticket INVÁLIDA ({reason}): ticket manipulado o falsificado, no se descarga");
                        }
                        crate::signing::SignatureStatus::Valid { fingerprint, .. } => {
                            e.log(job, format!("Firma Ed25519 válida · huella del firmante {fingerprint}")).await;
                        }
                        crate::signing::SignatureStatus::Unsigned => e.log(job, "Ticket sin firma (no se puede verificar el remitente)").await,
                    }
                    let jf = t.files.iter().map(|f| JobFile { path: f.path.clone(), size: f.size }).collect();
                    let srcs = t.sources.iter().map(|x| x.label()).collect::<Vec<_>>().join(", ");
                    set_meta(t.name.clone(), t.total_size, jf, format!("ticket · {srcs}")).await;
                    e.log(job, format!("Ticket «{}» de {} — fuentes: {srcs}", t.name, t.sender.as_deref().unwrap_or("?"))).await;
                    let rid = e.history.start(Kind::Download, &t.name, t.total_size, &format!("ticket:{}", t.id), t.files.len() as u32)?;
                    let e2 = e.clone();
                    let r = crate::download::ticket::download_ticket(&t, &dest, req.force, progress, on_file, move |sx, prev| {
                        let e3 = e2.clone();
                        let msg = match prev {
                            Some(err) => format!("Fuente anterior falló ({err:#}); probando {}", sx.label()),
                            None => format!("Descargando desde {}", sx.label()),
                        };
                        tokio::spawn(async move { e3.log(job, msg).await });
                    })
                    .await;
                    e.history.finish(rid, if r.is_ok() { Status::Completed } else { Status::Failed }, r.as_ref().err().map(|x| format!("{x:#}")).as_deref())?;
                    let r = r?;
                    Ok((format!("{} archivo(s) desde {} — BLAKE3 verificado: {}", r.saved.len(), r.source, r.verified), r.saved))
                } else if crate::download::swisstransfer::is_swisstransfer_url(&src) {
                    let mut c = crate::download::swisstransfer::SwissTransferClient::new()?;
                    let t = c.get_transfer(&src, pw.as_deref()).await.map_err(|e| {
                        if e.downcast_ref::<crate::download::swisstransfer::PasswordRequired>().is_some() {
                            anyhow::anyhow!("el link de SwissTransfer requiere contraseña: indícala en «Contraseña» y reintenta")
                        } else if let Some(w) = e.downcast_ref::<crate::download::swisstransfer::WrongPassword>() {
                            anyhow::anyhow!("SwissTransfer rechazó la contraseña ({})", w.0)
                        } else {
                            e
                        }
                    })?;
                    let jf = t.files.iter().map(|f| JobFile { path: f.path.clone(), size: f.size }).collect();
                    let title = t.title.clone().unwrap_or_else(|| t.link_id.clone());
                    set_meta(title.clone(), t.total_size, jf, "SwissTransfer".into()).await;
                    let rid = e.history.start(Kind::Download, &title, t.total_size, &src, t.files.len() as u32)?;
                    let r = c.download_all(&t, &dest, req.force, progress, on_file).await;
                    e.history.finish(rid, if r.is_ok() { Status::Completed } else { Status::Failed }, r.as_ref().err().map(|x| format!("{x:#}")).as_deref())?;
                    let saved = r?;
                    Ok((format!("{} archivo(s) descargados", saved.len()), saved))
                } else if crate::global::storage_to::parse_share_url(&src).is_some() {
                    let d = crate::download::storage_to::StorageDownloader::new()?;
                    let info = d.info(&src).await?;
                    if info.password_protected {
                        d.verify_password(&info, pw.as_deref().context("la compartición requiere contraseña")?).await?;
                    }
                    let title = info.title.clone().unwrap_or_else(|| info.files[0].name.clone());
                    let jf = info.files.iter().map(|f| JobFile { path: f.name.clone(), size: f.size }).collect();
                    set_meta(title.clone(), info.total_size, jf, "storage.to".into()).await;
                    let rid = e.history.start(Kind::Download, &title, info.total_size, &src, info.files.len() as u32)?;
                    let r = d.download_all(&info, &dest, req.force, progress, on_file).await;
                    e.history.finish(rid, if r.is_ok() { Status::Completed } else { Status::Failed }, r.as_ref().err().map(|x| format!("{x:#}")).as_deref())?;
                    let saved = r?;
                    Ok((format!("{} archivo(s) descargados", saved.len()), saved))
                } else if src.starts_with("http://") || src.starts_with("https://") {
                    let mut t = Ticket::new(src.rsplit('/').next().unwrap_or("download"));
                    t.sources.push(Source::Http { url: src.clone(), filename: None });
                    e.update_job(job, |j| j.peer = "HTTP".into()).await;
                    let rid = e.history.start(Kind::Download, &t.name, 0, &src, 1)?;
                    let r = crate::download::ticket::download_ticket(&t, &dest, req.force, progress, on_file, |_, _| {}).await;
                    e.history.finish(rid, if r.is_ok() { Status::Completed } else { Status::Failed }, r.as_ref().err().map(|x| format!("{x:#}")).as_deref())?;
                    let saved = r?.saved;
                    Ok((format!("{} archivo(s) descargados", saved.len()), saved))
                } else {
                    bail!("URL no soportada (storage.to, SwissTransfer, HTTP directo, unishare: o fichero .unishare)")
                }
            }
            .await;
            match res {
                Ok((m, saved)) => {
                    if e.cfg.read().await.notifications {
                        crate::ui::notify("uni-share: descarga completada", &m);
                    }
                    e.finish_job(job, JobState::Completed, m).await;
                    e.scan_saved(Some(job), saved).await;
                }
                Err(err) => e.finish_job(job, JobState::Failed, format!("Error: {err:#}")).await,
            }
        });
        self.set_abort(job, task.abort_handle()).await;
        Ok(job)
    }

    /// Build a `.unishare` ticket from existing links.
    pub async fn create_ticket(&self, r: TicketCreateReq) -> Result<TicketCreated> {
        let links: Vec<String> = r.links.iter().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        ensure!(!links.is_empty(), "at least one link is required");
        for l in &links {
            ensure!(l.starts_with("http://") || l.starts_with("https://"), "not a URL: {l}");
        }
        let local = r.verify_from.clone().filter(|p| !p.trim().is_empty()).map(PathBuf::from);
        let name = r
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .or_else(|| local.as_ref().and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string())))
            .unwrap_or_else(|| links[0].trim_end_matches('/').rsplit('/').next().unwrap_or("share").to_string());
        let mut t = Ticket::new(name);
        t.sender = Some(self.cfg.read().await.device_name.clone());
        t.message = r.message.clone().filter(|m| !m.trim().is_empty());
        let pw = r.password.clone().filter(|p| !p.is_empty());
        for l in &links {
            t.sources.push(Source::from_url(l, pw.clone()));
        }
        if let Some(p) = &local {
            ensure!(p.exists(), "verify_from path not found: {}", p.display());
            let files = collect_files(p)?;
            let hashes = hash_all(&files, |_| {}).await?;
            t.files = files.iter().zip(hashes.iter()).map(|(f, h)| TicketFile { path: f.rel_path.clone(), size: f.size, blake3: Some(h.clone()) }).collect();
            t.total_size = total_size(&files);
        }
        if self.cfg.read().await.sign_tickets {
            t.sign(&self.signing_key)?;
        }
        let out = match r.output.filter(|o| !o.trim().is_empty()) {
            Some(o) => PathBuf::from(o),
            None => self.download_dir().await.join(t.default_filename()),
        };
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        t.save(&out)?;
        let uri = t.to_uri_compact()?;
        Ok(TicketCreated { ticket: t, path: out, uri })
    }

    /// Parse a ticket from a URI, raw JSON or a file path.
    pub fn parse_ticket(data: &str) -> Result<Ticket> {
        let d = data.trim();
        if Path::new(d).is_file() { Ticket::load(Path::new(d)) } else { Ticket::decode(d.as_bytes()) }
    }
}

/// Non-loopback IPv4 addresses of this host (first = default route).
pub fn local_ips() -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(ip) = local_ip_address::local_ip() {
        v.push(ip.to_string());
    }
    if let Ok(list) = local_ip_address::list_afinet_netifas() {
        for (_, ip) in list {
            if ip.is_ipv4() && !ip.is_loopback() && !v.contains(&ip.to_string()) {
                v.push(ip.to_string());
            }
        }
    }
    v
}

/// Open a URL or folder with the platform default handler (best effort).
pub fn open_in_system(target: &str) {
    let t = target.to_string();
    std::thread::spawn(move || {
        #[cfg(target_os = "macos")]
        let mut cmd = {
            let mut c = std::process::Command::new("open");
            c.arg(&t);
            c
        };
        #[cfg(windows)]
        let mut cmd = {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "start", "", &t]);
            c
        };
        #[cfg(not(any(target_os = "macos", windows)))]
        let mut cmd = {
            let mut c = std::process::Command::new("xdg-open");
            c.arg(&t);
            c
        };
        let _ = cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    });
}

/// Directory listing for built-in file pickers.
#[derive(Serialize, Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub path: PathBuf,
    pub dir: bool,
    pub size: u64,
    pub modified: Option<u64>,
}

#[derive(Serialize, Debug, Clone)]
pub struct DirListing {
    pub path: PathBuf,
    pub parent: Option<PathBuf>,
    pub entries: Vec<DirEntry>,
    pub roots: Vec<PathBuf>,
}

pub fn list_dir(path: Option<&Path>, dirs_only: bool, show_hidden: bool) -> Result<DirListing> {
    let path = match path.filter(|p| !p.as_os_str().is_empty()) {
        Some(p) => p.to_path_buf(),
        None => directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from("/")),
    };
    let path = if path.is_dir() { path } else { path.parent().map(Path::to_path_buf).unwrap_or(path) };
    let rd = std::fs::read_dir(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut entries = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        if dirs_only && !md.is_dir() {
            continue;
        }
        entries.push(DirEntry {
            name,
            path: e.path(),
            dir: md.is_dir(),
            size: if md.is_dir() { 0 } else { md.len() },
            modified: md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()),
        });
    }
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(u) = directories::UserDirs::new() {
        roots.push(u.home_dir().to_path_buf());
        for d in [u.desktop_dir(), u.download_dir(), u.document_dir(), u.picture_dir(), u.video_dir()].into_iter().flatten() {
            roots.push(d.to_path_buf());
        }
    }
    #[cfg(windows)]
    for l in b'C'..=b'Z' {
        let d = PathBuf::from(format!("{}:\\", l as char));
        if d.exists() {
            roots.push(d);
        }
    }
    #[cfg(not(windows))]
    {
        roots.push(PathBuf::from("/"));
        for m in ["/media", "/mnt", "/Volumes"] {
            if Path::new(m).exists() {
                roots.push(PathBuf::from(m));
            }
        }
    }
    Ok(DirListing { parent: path.parent().map(Path::to_path_buf), path, entries, roots })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_percent_and_labels() {
        let mut j = Job {
            id: 1,
            kind: JobKind::Download,
            name: "x".into(),
            peer: String::new(),
            total: 200,
            done: 50,
            state: JobState::Running,
            message: String::new(),
            current_file: String::new(),
            link: None,
            ticket_uri: None,
            ticket_path: None,
            dest: None,
            files: vec![],
            started: 0,
            finished: None,
            speed: 0,
            eta: None,
            log: vec![],
            scan: None,
            scan_report: None,
            saved: Vec::new(),
            origin: None,
            retryable: false,
            samples: VecDeque::new(),
        };
        assert_eq!(j.percent(), 25.0);
        j.total = 0;
        assert_eq!(j.percent(), 0.0);
        j.state = JobState::Completed;
        assert_eq!(j.percent(), 100.0);
        assert!(JobKind::LanSend.is_outgoing() && !JobKind::Download.is_outgoing());
        assert_eq!(serde_json::to_string(&JobState::Failed).unwrap(), "\"failed\"");
    }

    #[test]
    fn list_dir_works() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), b"hi").unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join(".hidden"), b"").unwrap();
        let l = list_dir(Some(tmp.path()), false, false).unwrap();
        assert_eq!(l.entries.len(), 2);
        assert!(l.entries[0].dir, "dirs first");
        let d = list_dir(Some(tmp.path()), true, false).unwrap();
        assert_eq!(d.entries.len(), 1);
        assert!(!l.roots.is_empty());
    }
}
