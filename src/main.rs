use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use uni_share::config::{Config, resolve_config_path};
use uni_share::history::History;
use uni_share::{logging, ui};

mod commands;
mod commands_lan;
mod commands_global;
mod commands_download;
mod commands_daemon;
mod commands_ticket;

#[derive(Parser, Debug)]
#[command(
    name = "uni-share",
    version,
    about = "Hybrid file sharing: LAN (mDNS + TLS 1.3) and global links (storage.to / Smash)",
    long_about = None,
    propagate_version = true
)]
pub struct Cli {
    /// Path to config.toml (default: ./config.toml or ~/.config/fileshare/config.toml)
    #[arg(long, global = true, env = "UNI_SHARE_CONFIG")]
    pub config: Option<PathBuf>,

    /// Increase verbosity (-v debug, -vv trace)
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Suppress non-error output
    #[arg(short, long, global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Send a file or folder to a device on the local network
    SendLan(SendLanArgs),
    /// Upload a file or folder to storage.to (or Smash) and get a share link
    SendGlobal(SendGlobalArgs),
    /// Listen for incoming LAN transfers
    Receive(ReceiveArgs),
    /// Download from a storage.to / SwissTransfer link or a .unishare ticket
    Download(DownloadArgs),
    /// Create, inspect or share .unishare tickets (portable download descriptors)
    Ticket(TicketArgs),
    /// Render a QR code in the terminal for any text/URL/ticket
    Qr(QrArgs),
    /// Discover devices on the local network
    ListDevices(ListDevicesArgs),
    /// Show transfer history
    History(HistoryArgs),
    /// Manage the background receiver daemon
    Daemon(DaemonArgs),
    /// Launch the local web GUI (browser)
    Gui(GuiArgs),
    /// Launch the native desktop app (Slint). Optionally open a link / .unishare ticket
    App(AppArgs),
    /// Show or edit configuration
    Config(ConfigArgs),
    /// Register the .unishare file type and unishare: links to open the desktop app (per user)
    Associate(AssociateArgs),
    /// Run the local safety scanner on files or folders (heuristics + ClamAV when installed)
    Scan(ScanArgs),
}

#[derive(Args, Debug)]
pub struct ScanArgs {
    /// Files or folders to analyse (folders are walked recursively)
    #[arg(required = true, value_name = "PATH")]
    pub paths: Vec<PathBuf>,
    /// Print the full report as JSON (stdout)
    #[arg(long)]
    pub json: bool,
    /// Do not use ClamAV even if it is installed
    #[arg(long)]
    pub no_clamav: bool,
    /// Only report: never rename or delete dangerous files (default for this command)
    #[arg(long, conflicts_with_all = ["quarantine", "delete"])]
    pub report: bool,
    /// Rename dangerous files to *.unishare-quarantine
    #[arg(long, conflicts_with = "delete")]
    pub quarantine: bool,
    /// Delete dangerous files
    #[arg(long)]
    pub delete: bool,
    /// Show clean files too
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(Args, Debug)]
pub struct SendLanArgs {
    /// File or folder to send
    pub path: PathBuf,
    /// Target: device name, ip[:port], or a LAN pairing ticket (`unishare:` URI / .unishare file
    /// from `receive --qr`) — the ticket pins the receiver's fingerprint and supplies the PIN
    #[arg(long)]
    pub to: Option<String>,
    /// Pairing PIN if the receiver requires one
    #[arg(long)]
    pub pin: Option<String>,
    /// Compress folders into .tar.zst before sending
    #[arg(long)]
    pub compress: bool,
    /// Discovery timeout in seconds
    #[arg(long, default_value_t = 3)]
    pub timeout: u64,
}

#[derive(Args, Debug)]
pub struct SendGlobalArgs {
    /// File or folder to upload
    pub path: PathBuf,
    /// Backend: storage_to | smash (default from config)
    #[arg(long)]
    pub backend: Option<String>,
    /// Protect the share with a password (4-100 chars)
    #[arg(long)]
    pub password: Option<String>,
    /// Expiry in days (1-7 anonymous)
    #[arg(long)]
    pub expiry_days: Option<u32>,
    /// Maximum downloads before auto-delete (1-1000)
    #[arg(long)]
    pub max_downloads: Option<u32>,
    /// Compress folders into a single .tar.zst instead of a collection
    #[arg(long)]
    pub compress: bool,
    /// Do not copy the link to the clipboard
    #[arg(long)]
    pub no_clipboard: bool,
    /// Do not print the QR code
    #[arg(long)]
    pub no_qr: bool,
    /// Print machine-readable JSON result
    #[arg(long)]
    pub json: bool,
    /// Also write a .unishare ticket (link + password + BLAKE3 digests) next to the source,
    /// or to the given path
    #[arg(long, value_name = "FILE", num_args = 0..=1, default_missing_value = "")]
    pub ticket: Option<PathBuf>,
    /// Print the QR of the ticket instead of the plain link (implies --ticket)
    #[arg(long)]
    pub ticket_qr: bool,
    /// Do not sign the ticket with this device's Ed25519 key (config `sign_tickets`)
    #[arg(long)]
    pub no_sign: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ReceiveArgs {
    /// Destination directory (default: config download_dir)
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Port to listen on (default: config lan_port)
    #[arg(short, long)]
    pub port: Option<u16>,
    /// Accept every incoming transfer without asking
    #[arg(long)]
    pub auto_accept: bool,
    /// Require this PIN from senders
    #[arg(long)]
    pub pin: Option<String>,
    /// Overwrite existing files instead of renaming
    #[arg(long)]
    pub force: bool,
    /// Exit after the first completed transfer
    #[arg(long)]
    pub once: bool,
    /// Print a pairing ticket as QR (ip/port/fingerprint/PIN) so senders can
    /// target this receiver without mDNS discovery
    #[arg(long)]
    pub qr: bool,
    /// Also save the pairing ticket to this .unishare file (implies --qr)
    #[arg(long, value_name = "FILE")]
    pub ticket: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct DownloadArgs {
    /// storage.to / SwissTransfer URL, `unishare:` URI or path to a .unishare file
    pub url: String,
    /// Destination directory
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Password for protected shares
    #[arg(short, long)]
    pub password: Option<String>,
    /// Overwrite existing files
    #[arg(long)]
    pub force: bool,
    /// Only list contents, don't download
    #[arg(long)]
    pub list: bool,
}

#[derive(Args, Debug)]
pub struct TicketArgs {
    #[command(subcommand)]
    pub action: TicketAction,
}

#[derive(Subcommand, Debug)]
pub enum TicketAction {
    /// Build a ticket from one or more existing share links
    Create(TicketCreateArgs),
    /// Show the contents of a ticket (file or unishare: URI) and verify its signature
    Show {
        /// Path to .unishare file or unishare: URI
        ticket: String,
        /// JSON output
        #[arg(long)]
        json: bool,
    },
    /// Sign an existing ticket with this device's Ed25519 key (in place or to --output)
    Sign {
        /// Path to .unishare file or unishare: URI
        ticket: String,
        /// Output file (default: overwrite the input file / <name>.unishare for URIs)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Show this device's ticket-signing identity (Ed25519 public key + fingerprint)
    Identity,
    /// Print a ticket as a QR code / unishare: URI to share it
    Qr {
        /// Path to .unishare file or unishare: URI
        ticket: String,
        /// Only print the URI (no QR)
        #[arg(long)]
        uri_only: bool,
    },
    /// Convert a unishare: URI into a .unishare file
    Save {
        /// unishare: URI
        uri: String,
        /// Output file (default: <name>.unishare)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

#[derive(Args, Debug)]
pub struct TicketCreateArgs {
    /// Share links (storage.to, SwissTransfer, direct HTTP). First one is preferred.
    #[arg(required = true, num_args = 1..)]
    pub links: Vec<String>,
    /// Title of the share (default: derived from the first link)
    #[arg(short, long)]
    pub name: Option<String>,
    /// Password embedded in the ticket for protected links
    #[arg(short, long)]
    pub password: Option<String>,
    /// Free-text message for the recipient
    #[arg(short, long)]
    pub message: Option<String>,
    /// Local copy of the shared file/folder: adds sizes and BLAKE3 digests so the
    /// recipient can verify integrity
    #[arg(long, value_name = "PATH")]
    pub verify_from: Option<PathBuf>,
    /// Output file (default: <name>.unishare in the current directory)
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Also print a QR code of the ticket
    #[arg(long)]
    pub qr: bool,
    /// Sign the ticket with this device's Ed25519 key (default from config `sign_tickets`)
    #[arg(long, overrides_with = "no_sign")]
    pub sign: bool,
    /// Do not sign the ticket
    #[arg(long, overrides_with = "sign")]
    pub no_sign: bool,
}

#[derive(Args, Debug)]
pub struct QrArgs {
    /// Text, URL, unishare: URI or path to a .unishare file
    pub data: String,
    /// Write the QR as SVG to this file instead of printing it
    #[arg(long, value_name = "FILE")]
    pub svg: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct ListDevicesArgs {
    /// Seconds to wait for mDNS answers
    #[arg(long, default_value_t = 3)]
    pub timeout: u64,
    /// JSON output
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct HistoryArgs {
    /// Number of entries
    #[arg(short = 'n', long, default_value_t = 20)]
    pub limit: usize,
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Delete all history
    #[arg(long)]
    pub clear: bool,
}

#[derive(Args, Debug)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub action: DaemonAction,
}

#[derive(Subcommand, Debug)]
pub enum DaemonAction {
    /// Start the receiver in the background
    Start(ReceiveArgs),
    /// Stop the background receiver
    Stop,
    /// Show daemon status
    Status,
    /// (internal) run in foreground as the daemon child
    #[command(hide = true)]
    Run(ReceiveArgs),
}

#[derive(Args, Debug)]
pub struct GuiArgs {
    /// Port for the local web UI
    #[arg(long, default_value_t = 47_900)]
    pub port: u16,
    /// Do not open the browser automatically
    #[arg(long)]
    pub no_open: bool,
    /// Serve a realistic fake state without starting the engine (design reviews, screenshots)
    #[arg(long)]
    pub demo: bool,
}

#[derive(Args, Debug)]
pub struct AppArgs {
    /// Link, unishare: URI or .unishare file to open on start
    pub open: Option<String>,
    /// Demo mode: realistic fake data, no network (design review)
    #[arg(long)]
    pub demo: bool,
    /// Render once, save a PNG screenshot and quit
    #[arg(long, value_name = "PNG")]
    pub screenshot: Option<PathBuf>,
    /// Dialog to open on start (new, share, settings, fs, confirm)
    #[arg(long, value_name = "NAME")]
    pub dialog: Option<String>,
    /// Pre-select job ID and details tab, e.g. `1:1` (tabs: 0 general, 1 files, 2 share, 3 log, 4 history)
    #[arg(long, value_name = "ID[:TAB]")]
    pub select: Option<String>,
    /// Start with the light theme
    #[arg(long)]
    pub light: bool,
    /// With --demo: no jobs/offers/devices (fresh-install look, empty-state layout)
    #[arg(long)]
    pub empty: bool,
    /// Initial window size, e.g. `1024x600` (layout review at small sizes)
    #[arg(long, value_name = "WxH")]
    pub size: Option<String>,
    /// Show the drag & drop overlay (design review)
    #[arg(long)]
    pub drag_over: bool,
}

#[derive(Args, Debug)]
pub struct AssociateArgs {
    /// Remove the association instead of creating it
    #[arg(long)]
    pub remove: bool,
    /// Only show the current handler
    #[arg(long)]
    pub status: bool,
    /// Binary to register (default: this executable)
    #[arg(long, value_name = "PATH")]
    pub exe: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct ConfigArgs {
    /// Print the path of the active config file
    #[arg(long)]
    pub path: bool,
}

/// Shared runtime context.
pub struct Ctx {
    pub cfg: Config,
    pub cfg_path: PathBuf,
    pub history: History,
    pub quiet: bool,
}

impl Ctx {
    /// storage.to visitor token, generated and persisted on first use.
    pub fn ensure_visitor_token(&mut self) -> Result<String> {
        let path = self.cfg_path.clone();
        self.cfg.ensure_visitor_token(&path)
    }
}

fn main() {
    let cli = Cli::parse();
    logging::init(cli.verbose, cli.quiet);
    // The native GUI event loop must own the *main* thread (winit requirement
    // on macOS), so it runs outside tokio; the engine gets its own runtime.
    let res = if matches!(cli.command, Command::App(_)) {
        load_ctx(&cli).and_then(|ctx| match cli.command {
            Command::App(a) => commands::app(ctx, a),
            _ => unreachable!(),
        })
    } else {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("tokio runtime")
            .and_then(|rt| rt.block_on(run(cli)))
    };
    if let Err(e) = res {
        ui::error("ERROR", format!("{e:#}"));
        std::process::exit(1);
    }
}

fn load_ctx(cli: &Cli) -> Result<Ctx> {
    let cfg_path = resolve_config_path(cli.config.as_deref());
    let cfg = Config::load_or_create(&cfg_path)
        .with_context(|| format!("loading config {}", cfg_path.display()))?;
    let history = History::open(&History::default_path()).context("opening history")?;
    Ok(Ctx { cfg, cfg_path, history, quiet: cli.quiet })
}

async fn run(cli: Cli) -> Result<()> {
    let ctx = load_ctx(&cli)?;

    match cli.command {
        Command::SendLan(a) => commands::send_lan(ctx, a).await,
        Command::SendGlobal(a) => commands::send_global(ctx, a).await,
        Command::Receive(a) => commands::receive(ctx, a).await,
        Command::Download(a) => commands::download(ctx, a).await,
        Command::Ticket(a) => commands::ticket(ctx, a).await,
        Command::Qr(a) => commands::qr(ctx, a).await,
        Command::ListDevices(a) => commands::list_devices(ctx, a).await,
        Command::History(a) => commands::history(ctx, a).await,
        Command::Daemon(a) => commands::daemon(ctx, a).await,
        Command::Gui(a) => commands::gui(ctx, a).await,
        Command::App(_) => unreachable!("handled in main"),
        Command::Config(a) => commands::config(ctx, a).await,
        Command::Associate(a) => commands::associate(ctx, a).await,
        Command::Scan(a) => commands::scan(ctx, a).await,
    }
}
