//! TOML configuration.
//!
//! Resolution order:
//! 1. `--config <path>` / `UNI_SHARE_CONFIG` env var
//! 2. `./config.toml` in the current directory (dev / portable mode)
//! 3. `~/.config/fileshare/config.toml` (Linux), `%APPDATA%\fileshare\config.toml`
//!    (Windows), `~/Library/Application Support/fileshare/config.toml` (macOS)
//!
//! The file is created with defaults when it does not exist.

use anyhow::{Context, Result};
use directories::{ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const CONFIG_FILE_NAME: &str = "config.toml";
pub const CONFIG_DIR_NAME: &str = "fileshare";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Human-readable device name announced on the LAN.
    pub device_name: String,
    /// Default download directory.
    pub download_dir: PathBuf,
    /// TCP port for the LAN receiver (0 = random free port).
    pub lan_port: u16,
    /// Upload/download rate limit in Mbit/s (0 = unlimited).
    pub rate_limit_mbps: u32,
    /// Automatically accept incoming LAN transfers (used by the daemon).
    pub auto_accept: bool,
    /// Optional pairing PIN (4-6 digits) required from LAN senders.
    pub pin: Option<String>,
    /// Show desktop notifications.
    pub notifications: bool,
    /// Compress folders into .tar.zst before sending (default: keep hierarchy).
    pub compress_folders: bool,
    /// Global backend section.
    pub global: GlobalConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GlobalConfig {
    /// Default backend: "storage_to" or "smash".
    pub backend: String,
    /// storage.to API base URL.
    pub storage_to_api: String,
    /// storage.to optional account bearer token (premium features).
    pub storage_to_token: Option<String>,
    /// Anonymous visitor token (auto generated and persisted here).
    pub storage_to_visitor_token: Option<String>,
    /// Smash API key (Bearer) — required to use the Smash backend.
    pub smash_api_key: Option<String>,
    /// Smash region, e.g. "eu-west-3".
    pub smash_region: String,
    /// Default expiry in days for uploads (1-7 for anonymous storage.to).
    pub expiry_days: u32,
    /// Number of parallel multipart parts.
    pub parallel_parts: usize,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            backend: "storage_to".into(),
            storage_to_api: "https://storage.to/api".into(),
            storage_to_token: None,
            storage_to_visitor_token: None,
            smash_api_key: None,
            smash_region: "eu-west-3".into(),
            expiry_days: 7,
            parallel_parts: 4,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_name: default_device_name(),
            download_dir: default_download_dir(),
            lan_port: 47_820,
            rate_limit_mbps: 0,
            auto_accept: false,
            pin: None,
            notifications: true,
            compress_folders: false,
            global: GlobalConfig::default(),
        }
    }
}

pub fn default_device_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "uni-share-device".to_string())
}

pub fn default_download_dir() -> PathBuf {
    UserDirs::new()
        .and_then(|u| u.download_dir().map(Path::to_path_buf))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// Directory where persistent app data lives (config, sqlite, certificates).
pub fn data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("UNI_SHARE_HOME") {
        return PathBuf::from(p);
    }
    ProjectDirs::from("", "", CONFIG_DIR_NAME)
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".").join(format!(".{CONFIG_DIR_NAME}")))
}

/// Resolve the config file path following the documented precedence.
pub fn resolve_config_path(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Ok(p) = std::env::var("UNI_SHARE_CONFIG") {
        return PathBuf::from(p);
    }
    let local = PathBuf::from(CONFIG_FILE_NAME);
    if local.exists() {
        return local;
    }
    data_dir().join(CONFIG_FILE_NAME)
}

impl Config {
    /// Load from `path`, creating it with defaults when missing.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading config {}", path.display()))?;
            let cfg: Config = toml::from_str(&text)
                .with_context(|| format!("parsing config {}", path.display()))?;
            cfg.validate()?;
            Ok(cfg)
        } else {
            let cfg = Config::default();
            cfg.save(path)?;
            tracing::info!(path = %path.display(), "created default config");
            Ok(cfg)
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
        }
        let text = toml::to_string_pretty(self).context("serializing config")?;
        let header = "# uni-share configuration\n# Docs: https://github.com/correo415415/uni-share\n\n";
        std::fs::write(path, format!("{header}{text}"))
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn validate(&self) -> Result<()> {
        if let Some(pin) = &self.pin {
            anyhow::ensure!(
                (4..=6).contains(&pin.len()) && pin.chars().all(|c| c.is_ascii_digit()),
                "pin must be 4-6 digits"
            );
        }
        anyhow::ensure!(
            matches!(self.global.backend.as_str(), "storage_to" | "smash"),
            "global.backend must be 'storage_to' or 'smash'"
        );
        anyhow::ensure!(
            (1..=7).contains(&self.global.expiry_days) || self.global.storage_to_token.is_some(),
            "global.expiry_days must be 1-7 for anonymous uploads"
        );
        anyhow::ensure!(self.global.parallel_parts >= 1, "parallel_parts must be >= 1");
        Ok(())
    }

    /// Return (and persist if newly generated) the storage.to visitor token.
    pub fn ensure_visitor_token(&mut self, path: &Path) -> Result<String> {
        if let Some(t) = &self.global.storage_to_visitor_token {
            if !t.is_empty() {
                return Ok(t.clone());
            }
        }
        let token = generate_visitor_token();
        self.global.storage_to_visitor_token = Some(token.clone());
        self.save(path)?;
        Ok(token)
    }
}

/// 32 random bytes hex-encoded — same shape as the official CLI token.
pub fn generate_visitor_token() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roundtrip() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn creates_when_missing_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        assert!(!path.exists());
        let cfg = Config::load_or_create(&path).unwrap();
        assert!(path.exists());
        let again = Config::load_or_create(&path).unwrap();
        assert_eq!(cfg, again);
    }

    #[test]
    fn partial_file_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "device_name = \"Test\"\n[global]\nexpiry_days = 3\n").unwrap();
        let cfg = Config::load_or_create(&path).unwrap();
        assert_eq!(cfg.device_name, "Test");
        assert_eq!(cfg.global.expiry_days, 3);
        assert_eq!(cfg.lan_port, Config::default().lan_port);
    }

    #[test]
    fn rejects_bad_pin() {
        let mut cfg = Config::default();
        cfg.pin = Some("12".into());
        assert!(cfg.validate().is_err());
        cfg.pin = Some("1234".into());
        assert!(cfg.validate().is_ok());
        cfg.pin = Some("12a4".into());
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn visitor_token_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::load_or_create(&path).unwrap();
        let t1 = cfg.ensure_visitor_token(&path).unwrap();
        assert_eq!(t1.len(), 64);
        let mut reloaded = Config::load_or_create(&path).unwrap();
        let t2 = reloaded.ensure_visitor_token(&path).unwrap();
        assert_eq!(t1, t2);
    }
}
