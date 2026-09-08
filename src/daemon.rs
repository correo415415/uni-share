//! Background receiver daemon: `daemon start|stop|status`.
//!
//! Cross-platform approach without OS service integration: `start` spawns a
//! detached child process running `daemon run …` (the same binary), stores
//! its PID in `<data_dir>/daemon.pid` and logs to `<data_dir>/daemon.log`.
//! `stop` sends SIGTERM (Unix) / `taskkill` (Windows); `status` checks liveness.

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Stdio;

pub fn pid_file() -> PathBuf {
    crate::config::data_dir().join("daemon.pid")
}
pub fn log_file() -> PathBuf {
    crate::config::data_dir().join("daemon.log")
}

pub fn read_pid() -> Option<u32> {
    std::fs::read_to_string(pid_file()).ok()?.trim().parse().ok()
}

pub fn is_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill -0
        let status = std::process::Command::new("kill").args(["-0", &pid.to_string()]).stdout(Stdio::null()).stderr(Stdio::null()).status();
        status.map(|s| s.success()).unwrap_or(false)
    }
    #[cfg(windows)]
    {
        let out = std::process::Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH"]).output();
        out.map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string())).unwrap_or(false)
    }
}

/// Spawn the detached child. `extra_args` are forwarded to `daemon run`.
pub fn start(extra_args: &[String]) -> Result<u32> {
    if let Some(pid) = read_pid() {
        if is_running(pid) {
            bail!("daemon already running (pid {pid})");
        }
        let _ = std::fs::remove_file(pid_file());
    }
    let exe = std::env::current_exe().context("current exe")?;
    std::fs::create_dir_all(crate::config::data_dir())?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(log_file())?;
    let log_err = log.try_clone()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon").arg("run").args(extra_args).stdin(Stdio::null()).stdout(log).stderr(log_err);
    // Propagate config location.
    for k in ["UNI_SHARE_CONFIG", "UNI_SHARE_HOME", "RUST_LOG"] {
        if let Ok(v) = std::env::var(k) {
            cmd.env(k, v);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // New session so the child survives the parent's terminal closing.
        unsafe {
            cmd.pre_exec(|| {
                libc_setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    let child = cmd.spawn().context("spawning daemon")?;
    let pid = child.id();
    std::fs::write(pid_file(), pid.to_string())?;
    Ok(pid)
}

#[cfg(unix)]
fn libc_setsid() {
    unsafe extern "C" {
        fn setsid() -> i32;
    }
    unsafe {
        setsid();
    }
}

pub fn stop() -> Result<Option<u32>> {
    let Some(pid) = read_pid() else { return Ok(None) };
    if !is_running(pid) {
        let _ = std::fs::remove_file(pid_file());
        return Ok(None);
    }
    #[cfg(unix)]
    {
        std::process::Command::new("kill").args(["-TERM", &pid.to_string()]).status().context("kill")?;
    }
    #[cfg(windows)]
    {
        std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).status().context("taskkill")?;
    }
    for _ in 0..30 {
        if !is_running(pid) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = std::fs::remove_file(pid_file());
    Ok(Some(pid))
}

#[derive(Debug, serde::Serialize)]
pub struct DaemonStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub pid_file: PathBuf,
    pub log_file: PathBuf,
}

pub fn status() -> DaemonStatus {
    let pid = read_pid();
    let running = pid.map(is_running).unwrap_or(false);
    DaemonStatus { running, pid: if running { pid } else { None }, pid_file: pid_file(), log_file: log_file() }
}

/// Write our own PID (used by `daemon run` so status works even if started manually).
pub fn register_self() -> Result<()> {
    std::fs::create_dir_all(crate::config::data_dir())?;
    std::fs::write(pid_file(), std::process::id().to_string())?;
    Ok(())
}

pub fn unregister_self() {
    if read_pid() == Some(std::process::id()) {
        let _ = std::fs::remove_file(pid_file());
    }
}
