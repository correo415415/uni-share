//! uni-share — hybrid LAN/global file sharing.
//!
//! Library crate exposing every subsystem so the binary (and tests) can reuse them.

pub mod config;
pub mod fsutil;
pub mod hash;
pub mod history;
pub mod logging;
pub mod ui;

pub mod lan;
pub mod global;
pub mod download;
pub mod daemon;
pub mod gui;
pub mod ticket;
pub mod engine;

/// Application name used for config dirs, mDNS service, user-agent…
pub const APP_NAME: &str = "uni-share";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn user_agent() -> String {
    format!("{APP_NAME}/{APP_VERSION} (+https://github.com/correo415415/uni-share)")
}
