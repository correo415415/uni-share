//! Android entry points (JNI).
//!
//! The Kotlin shell (`dev.unishare.app.Native`) loads `libuni_share.so` and calls
//! `start(dataDir, downloadDir, deviceName)`. We spin up the shared [`Engine`] plus the
//! web GUI router bound to `127.0.0.1:<random port>` on a dedicated tokio runtime; the
//! activity then shows it inside a WebView. Logs go to logcat (tag `uni-share`).
#![cfg(target_os = "android")]

use std::ffi::CString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::{jint, jstring};
use tokio::sync::oneshot;

use crate::config::Config;
use crate::engine::Engine;
use crate::history::History;

struct Running {
    port: u16,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

// ---------------------------------------------------------------- logcat --

unsafe extern "C" {
    fn __android_log_write(prio: i32, tag: *const std::ffi::c_char, text: *const std::ffi::c_char) -> i32;
}

fn logcat(prio: i32, text: &str) {
    let tag = c"uni-share";
    let text = text.replace('\0', " ");
    if let Ok(text) = CString::new(text) {
        // SAFETY: both pointers are valid NUL-terminated C strings for the duration of the call.
        unsafe {
            __android_log_write(prio, tag.as_ptr(), text.as_ptr());
        }
    }
}

struct LogcatWriter;

impl std::io::Write for LogcatWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        let prio = if text.contains("ERROR") {
            6
        } else if text.contains("WARN") {
            5
        } else if text.contains("DEBUG") || text.contains("TRACE") {
            3
        } else {
            4
        };
        logcat(prio, text.trim_end());
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn init_logging() {
    use tracing_subscriber::prelude::*;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,uni_share=debug"));
    let fmt = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .without_time()
        .with_writer(|| LogcatWriter);
    // Ignore the error if a subscriber is already installed (start() called twice).
    let _ = tracing_subscriber::registry().with(filter).with(fmt).with(crate::logbuf::layer()).try_init();
}

// ---------------------------------------------------------------- engine --

fn jstr(env: &mut JNIEnv, s: &JString) -> String {
    env.get_string(s).map(|s| s.into()).unwrap_or_default()
}

async fn boot(
    data_dir: PathBuf,
    download_dir: PathBuf,
    device_name: String,
) -> Result<(std::sync::Arc<Engine>, tokio::net::TcpListener)> {
    std::fs::create_dir_all(&data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    std::fs::create_dir_all(&download_dir).with_context(|| format!("creating {}", download_dir.display()))?;
    let cfg_path = data_dir.join("config.toml");
    let mut cfg = Config::load_or_create(&cfg_path)?;
    let mut dirty = false;
    if !device_name.is_empty() && cfg.device_name != device_name {
        cfg.device_name = device_name;
        dirty = true;
    }
    if cfg.download_dir != download_dir {
        cfg.download_dir = download_dir;
        dirty = true;
    }
    if dirty {
        cfg.save(&cfg_path)?;
    }
    let history = History::open(&History::default_path())?;
    let engine = Engine::start(cfg, cfg_path, history).await?;
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
    Ok((engine, listener))
}

fn start(data_dir: PathBuf, download_dir: PathBuf, device_name: String) -> Result<u16> {
    let mut guard = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(r) = guard.as_ref() {
        return Ok(r.port);
    }
    // Every path under the app sandbox: config/history/keys live in `data_dir`.
    // Android has no `/tmp`: point TMPDIR (std::env::temp_dir, tempfile) into our data dir
    // so compressing folders / staging tickets does not fail with EINVAL/ENOENT.
    let tmp = data_dir.join("tmp");
    let _ = std::fs::create_dir_all(&tmp);
    // SAFETY: called from the UI thread before any other thread of ours exists.
    unsafe {
        std::env::set_var("UNI_SHARE_HOME", &data_dir);
        if std::env::var_os("TMPDIR").is_none_or(|t| !Path::new(&t).is_dir()) {
            std::env::set_var("TMPDIR", &tmp);
        }
    }
    crate::logbuf::set_file_dir(&data_dir.join("logs"));

    let (port_tx, port_rx) = std::sync::mpsc::channel::<Result<u16>>();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let thread = std::thread::Builder::new()
        .name("uni-share-engine".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = port_tx.send(Err(e.into()));
                    return;
                }
            };
            rt.block_on(async move {
                let (engine, listener) = match boot(data_dir, download_dir, device_name).await {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = port_tx.send(Err(e));
                        return;
                    }
                };
                let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
                tracing::info!(port, lan = %engine.lan_addr, "uni-share engine running");
                let _ = port_tx.send(Ok(port));
                let app = crate::gui::router(engine);
                if let Err(e) = axum::serve(listener, app)
                    .with_graceful_shutdown(async {
                        let _ = stop_rx.await;
                    })
                    .await
                {
                    tracing::error!(error = %e, "web gui server failed");
                }
            });
        })
        .context("spawning engine thread")?;

    let port = port_rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .context("engine did not start in time")??;
    *guard = Some(Running { port, stop: Some(stop_tx), thread: Some(thread) });
    Ok(port)
}

fn stop() {
    let mut guard = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(mut r) = guard.take() {
        if let Some(tx) = r.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = r.thread.take() {
            let _ = t.join();
        }
    }
}

fn port() -> Option<u16> {
    RUNNING.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map(|r| r.port)
}

// ------------------------------------------------------------------- JNI --

/// `Native.start(dataDir, downloadDir, deviceName): Int` → local web GUI port, `-1` on error.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_unishare_app_Native_start(
    mut env: JNIEnv,
    _class: JClass,
    data_dir: JString,
    download_dir: JString,
    device_name: JString,
) -> jint {
    init_logging();
    let data_dir = PathBuf::from(jstr(&mut env, &data_dir));
    let download_dir = PathBuf::from(jstr(&mut env, &download_dir));
    let device_name = jstr(&mut env, &device_name);
    match start(data_dir, download_dir, device_name) {
        Ok(port) => jint::from(port),
        Err(e) => {
            logcat(6, &format!("start failed: {e:#}"));
            -1
        }
    }
}

/// `Native.stop()` — graceful shutdown of the engine and the local web server.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_unishare_app_Native_stop(_env: JNIEnv, _class: JClass) {
    stop();
}

/// `Native.port(): Int` — current local web GUI port, `0` when not running.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_unishare_app_Native_port(_env: JNIEnv, _class: JClass) -> jint {
    port().map(jint::from).unwrap_or(0)
}

/// `Native.log(level, tag, message)`: the Kotlin shell appends its own lines (scanner,
/// SAF export, permissions) to the shared log buffer so the "Registro" screen shows both sides.
/// `level`: 2 verbose · 3 debug · 4 info · 5 warn · 6 error (android.util.Log constants).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_unishare_app_Native_log(mut env: JNIEnv, _class: JClass, level: jint, tag: JString, msg: JString) {
    let lvl = match level {
        6 => tracing::Level::ERROR,
        5 => tracing::Level::WARN,
        4 => tracing::Level::INFO,
        3 => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    };
    let tag = jstr(&mut env, &tag);
    let msg = jstr(&mut env, &msg);
    crate::logbuf::push(lvl, &format!("android::{tag}"), &msg);
}

/// `Native.version(): String`
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_unishare_app_Native_version(env: JNIEnv, _class: JClass) -> jstring {
    match env.new_string(crate::APP_VERSION) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}
