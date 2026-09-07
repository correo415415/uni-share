use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use uni_share::config::{Config, resolve_config_path};
use uni_share::history::History;
use uni_share::{logging, ui};

mod commands;

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
    /// Download from a storage.to or SwissTransfer link
    Download(DownloadArgs),
    /// Discover devices on the local network
    ListDevices(ListDevicesArgs),
    /// Show transfer history
    History(HistoryArgs),
    /// Manage the background receiver daemon
    Daemon(DaemonArgs),
    /// Launch the local web GUI
    Gui(GuiArgs),
    /// Show or edit configuration
    Config(ConfigArgs),
}

#[derive(Args, Debug)]
pub struct SendLanArgs {
    /// File or folder to send
    pub path: PathBuf,
    /// Target device name or ip[:port] (skips interactive selection)
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
}

#[derive(Args, Debug)]
pub struct DownloadArgs {
    /// storage.to or SwissTransfer URL
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
    /// For SwissTransfer: delegate to python/swisstransfer_dl.py instead of the native Rust port
    #[arg(long)]
    pub python: bool,
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

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    logging::init(cli.verbose, cli.quiet);
    if let Err(e) = run(cli).await {
        ui::error("ERROR", format!("{e:#}"));
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let cfg_path = resolve_config_path(cli.config.as_deref());
    let cfg = Config::load_or_create(&cfg_path)
        .with_context(|| format!("loading config {}", cfg_path.display()))?;
    let history = History::open(&History::default_path()).context("opening history")?;
    let ctx = Ctx { cfg, cfg_path, history, quiet: cli.quiet };

    match cli.command {
        Command::SendLan(a) => commands::send_lan(ctx, a).await,
        Command::SendGlobal(a) => commands::send_global(ctx, a).await,
        Command::Receive(a) => commands::receive(ctx, a).await,
        Command::Download(a) => commands::download(ctx, a).await,
        Command::ListDevices(a) => commands::list_devices(ctx, a).await,
        Command::History(a) => commands::history(ctx, a).await,
        Command::Daemon(a) => commands::daemon(ctx, a).await,
        Command::Gui(a) => commands::gui(ctx, a).await,
        Command::Config(a) => commands::config(ctx, a).await,
    }
}
