//! In-process log store behind the "Registro" screens (web `/api/logs`, mobile, Slint,
//! Android). A `tracing` [`Layer`] keeps the last [`CAPACITY`] events in a ring buffer
//! (readable by any UI, with a monotonic sequence number so clients can poll for "new since")
//! and, when a directory is configured, mirrors them to `logs/uni-share.log` with a simple
//! size-based rotation (`uni-share.log.1` … `.N`).
//!
//! Records carry level/target/message only; spans are ignored on purpose (this is a
//! troubleshooting view for users, not a tracing sink).

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// Lines kept in memory.
pub const CAPACITY: usize = 2000;
/// Rotate the file when it grows past this.
pub const FILE_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Rotated generations kept (`uni-share.log.1` … `.FILE_KEEP`).
pub const FILE_KEEP: usize = 5;
pub const FILE_NAME: &str = "uni-share.log";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    /// Monotonic, starts at 1, never reused for the process lifetime.
    pub seq: u64,
    /// Unix ms.
    pub ts: i64,
    /// `error` | `warn` | `info` | `debug` | `trace`.
    pub level: &'static str,
    /// Module path (`uni_share::lan::client`), shortened for display by the UIs.
    pub target: String,
    pub message: String,
}

impl Entry {
    /// `2026-09-09 15:02:07.123  WARN uni_share::lan::client  message`
    pub fn format_line(&self) -> String {
        let ts = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(self.ts)
            .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S%.3f").to_string())
            .unwrap_or_default();
        format!("{ts} {:>5} {}  {}", self.level.to_uppercase(), self.target, self.message)
    }
}

struct FileSink {
    dir: PathBuf,
    file: Option<File>,
    size: u64,
}

impl FileSink {
    fn open(dir: &Path) -> Option<Self> {
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join(FILE_NAME);
        let file = OpenOptions::new().create(true).append(true).open(&path).ok()?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Some(Self { dir: dir.to_path_buf(), file: Some(file), size })
    }

    fn rotate(&mut self) {
        self.file = None;
        let base = self.dir.join(FILE_NAME);
        let _ = std::fs::remove_file(self.dir.join(format!("{FILE_NAME}.{FILE_KEEP}")));
        for i in (1..FILE_KEEP).rev() {
            let _ = std::fs::rename(self.dir.join(format!("{FILE_NAME}.{i}")), self.dir.join(format!("{FILE_NAME}.{}", i + 1)));
        }
        let _ = std::fs::rename(&base, self.dir.join(format!("{FILE_NAME}.1")));
        self.file = OpenOptions::new().create(true).append(true).open(&base).ok();
        self.size = 0;
    }

    fn write_line(&mut self, line: &str) {
        if self.size >= FILE_MAX_BYTES {
            self.rotate();
        }
        if let Some(f) = self.file.as_mut() {
            if f.write_all(line.as_bytes()).and_then(|_| f.write_all(b"\n")).is_ok() {
                self.size += line.len() as u64 + 1;
            }
        }
    }
}

struct Inner {
    ring: VecDeque<Entry>,
    file: Option<FileSink>,
}

struct Store {
    inner: Mutex<Inner>,
    seq: AtomicU64,
    /// Everything appended since startup (also what was dropped from the ring).
    total: AtomicU64,
}

static STORE: OnceLock<Store> = OnceLock::new();

fn store() -> &'static Store {
    STORE.get_or_init(|| Store {
        inner: Mutex::new(Inner { ring: VecDeque::with_capacity(CAPACITY), file: None }),
        seq: AtomicU64::new(0),
        total: AtomicU64::new(0),
    })
}

/// Mirror new lines to `<dir>/uni-share.log` (rotating). Safe to call before or after
/// [`layer`] is installed; the ring buffer content so far is flushed to the file.
pub fn set_file_dir(dir: &Path) -> Option<PathBuf> {
    let mut sink = FileSink::open(dir)?;
    let mut g = store().inner.lock().unwrap_or_else(|p| p.into_inner());
    for e in &g.ring {
        sink.write_line(&e.format_line());
    }
    g.file = Some(sink);
    Some(dir.join(FILE_NAME))
}

/// Path of the current log file, if a directory was configured.
pub fn file_path() -> Option<PathBuf> {
    let g = store().inner.lock().unwrap_or_else(|p| p.into_inner());
    g.file.as_ref().map(|f| f.dir.join(FILE_NAME))
}

/// Append a line produced outside `tracing` (e.g. the Android shell via JNI).
pub fn push(level: Level, target: &str, message: &str) {
    push_entry(level_str(level), target.to_string(), message.to_string());
}

fn level_str(l: Level) -> &'static str {
    match l {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

fn push_entry(level: &'static str, target: String, message: String) {
    let st = store();
    let seq = st.seq.fetch_add(1, Ordering::Relaxed) + 1;
    st.total.fetch_add(1, Ordering::Relaxed);
    let e = Entry { seq, ts: chrono::Utc::now().timestamp_millis(), level, target, message };
    let mut g = st.inner.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(f) = g.file.as_mut() {
        f.write_line(&e.format_line());
    }
    if g.ring.len() >= CAPACITY {
        g.ring.pop_front();
    }
    g.ring.push_back(e);
}

/// Entries with `seq > after`, optionally at or above `min_level` (`warn` → warn+error),
/// newest last, at most `limit`.
pub fn entries(after: u64, min_level: Option<&str>, limit: usize) -> Vec<Entry> {
    let rank = |l: &str| match l {
        "error" => 4,
        "warn" => 3,
        "info" => 2,
        "debug" => 1,
        _ => 0,
    };
    let min = min_level.map(rank).unwrap_or(0);
    let g = store().inner.lock().unwrap_or_else(|p| p.into_inner());
    let mut out: Vec<Entry> = g.ring.iter().rev().filter(|e| e.seq > after && rank(e.level) >= min).take(limit).cloned().collect();
    out.reverse();
    out
}

/// Sequence number of the newest entry (0 when empty).
pub fn last_seq() -> u64 {
    store().seq.load(Ordering::Relaxed)
}

/// Lines ever appended in this process.
pub fn total() -> u64 {
    store().total.load(Ordering::Relaxed)
}

/// Drop the in-memory buffer (the file is left alone) and return how many lines were dropped.
pub fn clear() -> usize {
    let mut g = store().inner.lock().unwrap_or_else(|p| p.into_inner());
    let n = g.ring.len();
    g.ring.clear();
    n
}

/// Whole buffer as text, for "copy"/"share" buttons.
pub fn dump(min_level: Option<&str>) -> String {
    let mut s = String::new();
    for e in entries(0, min_level, CAPACITY) {
        let _ = writeln!(s, "{}", e.format_line());
    }
    s
}

/// The `tracing` layer; add it to the registry next to the console/logcat layer.
pub fn layer() -> BufferLayer {
    let _ = store();
    BufferLayer
}

pub struct BufferLayer;

struct MsgVisitor {
    msg: String,
    rest: String,
}

impl Visit for MsgVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.msg = format!("{value:?}");
        } else {
            let _ = write!(self.rest, " {}={:?}", field.name(), value);
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.msg = value.to_string();
        } else {
            let _ = write!(self.rest, " {}={}", field.name(), value);
        }
    }
}

impl<S: Subscriber> Layer<S> for BufferLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut v = MsgVisitor { msg: String::new(), rest: String::new() };
        event.record(&mut v);
        let mut msg = v.msg;
        if !v.rest.is_empty() {
            msg.push_str(&v.rest);
        }
        let meta = event.metadata();
        push_entry(level_str(*meta.level()), meta.target().to_string(), msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_and_filters() {
        let before = last_seq();
        push(Level::INFO, "t", "hello");
        push(Level::WARN, "t", "careful");
        push(Level::ERROR, "t", "boom");
        let all = entries(before, None, 100);
        assert_eq!(all.iter().map(|e| e.message.as_str()).collect::<Vec<_>>(), ["hello", "careful", "boom"]);
        let warn = entries(before, Some("warn"), 100);
        assert_eq!(warn.len(), 2);
        assert!(warn.iter().all(|e| e.level != "info"));
        assert_eq!(entries(all[1].seq, None, 100).len(), 1);
        assert!(all[0].format_line().contains(" INFO t  hello"));
    }

    #[test]
    fn file_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = FileSink::open(dir.path()).unwrap();
        let line = "x".repeat(1000);
        for _ in 0..(FILE_MAX_BYTES / 1000 + 5) {
            sink.write_line(&line);
        }
        assert!(dir.path().join(format!("{FILE_NAME}.1")).exists());
        assert!(dir.path().join(FILE_NAME).metadata().unwrap().len() < FILE_MAX_BYTES);
    }
}
