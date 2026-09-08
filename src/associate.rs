//! Register (or remove) the `.unishare` file type and the `unishare:` URL
//! scheme so that double-clicking a ticket — or scanning a QR that contains a
//! `unishare:` URI — opens the desktop app with the ticket loaded.
//!
//! * **Linux** (XDG, per-user, no root): a shared-mime-info XML in
//!   `~/.local/share/mime/packages`, a `.desktop` launcher in
//!   `~/.local/share/applications` whose `MimeType` covers both
//!   `application/x-unishare` and `x-scheme-handler/unishare`, icons, then
//!   `update-mime-database` / `update-desktop-database` / `xdg-mime default`.
//! * **Windows** (per-user, no elevation): `HKCU\Software\Classes` — ProgID
//!   `UniShare.Ticket` with the open command, `.unishare` → ProgID and a
//!   `unishare` URL-protocol key, written through `reg.exe`.
//! * **macOS**: associations live in the app bundle's `Info.plist` (packaging).
//!
//! Idempotent: `associate` twice is fine; `associate --remove` undoes exactly
//! what was created.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const MIME_TYPE: &str = "application/x-unishare";
pub const SCHEME: &str = "unishare";
pub const DESKTOP_ID: &str = "uni-share.desktop";
pub const PROG_ID: &str = "UniShare.Ticket";

/// PNG used as app / file icon (same asset as the Slint window icon).
pub const ICON_PNG: &[u8] = include_bytes!("../ui/icon.png");

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub actions: Vec<String>,
    pub warnings: Vec<String>,
}

impl Report {
    fn did(&mut self, s: impl Into<String>) {
        self.actions.push(s.into());
    }
    fn warn(&mut self, s: impl Into<String>) {
        self.warnings.push(s.into());
    }
}

/// Binary that will be registered as the handler.
pub fn current_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("locating current executable")?;
    Ok(exe.canonicalize().unwrap_or(exe))
}

fn run(report: &mut Report, cmd: &mut Command, what: &str) {
    match cmd.output() {
        Ok(o) if o.status.success() => report.did(what),
        Ok(o) => report.warn(format!("{what}: {}", String::from_utf8_lossy(&o.stderr).trim())),
        Err(e) => report.warn(format!("{what}: {e}")),
    }
}

// ───────────────────────── pure builders (testable everywhere) ─────────────────────────

/// freedesktop `.desktop` entry. `%u` is one URL *or* file; `app` accepts both.
pub fn desktop_entry(exe: &Path) -> String {
    let q = sh_quote(exe);
    format!(
        "[Desktop Entry]\nType=Application\nVersion=1.5\nName=uni-share\nGenericName=File sharing\n\
Comment=Send files over the LAN or share links and .unishare tickets\n\
Exec={q} app %u\nTryExec={q}\nIcon=uni-share\nTerminal=false\n\
Categories=Network;FileTransfer;Utility;\nMimeType={MIME_TYPE};x-scheme-handler/{SCHEME};\n\
StartupNotify=true\nKeywords=share;transfer;lan;ticket;\n"
    )
}

fn sh_quote(p: &Path) -> String {
    let s = p.display().to_string();
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c)) { s } else { format!("\"{}\"", s.replace('"', "\\\"")) }
}

/// shared-mime-info definition: glob + magic (container starts with `UNISHARE`).
pub fn mime_xml() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<mime-info xmlns=\"http://www.freedesktop.org/standards/shared-mime-info\">\n\
  <mime-type type=\"{MIME_TYPE}\">\n\
    <comment>uni-share ticket</comment>\n\
    <comment xml:lang=\"es\">Ticket de uni-share</comment>\n\
    <glob pattern=\"*.unishare\"/>\n\
    <magic priority=\"80\"><match type=\"string\" offset=\"0\" value=\"UNISHARE\"/></magic>\n\
    <icon name=\"application-x-unishare\"/>\n\
  </mime-type>\n\
</mime-info>\n"
    )
}

/// Windows registry operations as `reg.exe` argument lists (also used by tests).
pub fn windows_reg_ops(exe: &Path) -> Vec<Vec<String>> {
    const C: &str = r"HKCU\Software\Classes";
    let exe_s = exe.display().to_string();
    let open = format!("\"{exe_s}\" app \"%1\"");
    let icon = format!("\"{exe_s}\",0");
    let add = |key: String, name: Option<&str>, data: &str| -> Vec<String> {
        let mut v = vec!["add".to_string(), key];
        match name {
            Some(n) => v.extend(["/v".into(), n.into()]),
            None => v.push("/ve".into()),
        }
        v.extend(["/d".into(), data.into(), "/f".into()]);
        v
    };
    vec![
        add(format!(r"{C}\{PROG_ID}"), None, "uni-share ticket"),
        add(format!(r"{C}\{PROG_ID}\DefaultIcon"), None, &icon),
        add(format!(r"{C}\{PROG_ID}\shell\open\command"), None, &open),
        add(format!(r"{C}\.unishare"), None, PROG_ID),
        add(format!(r"{C}\.unishare"), Some("Content Type"), MIME_TYPE),
        add(format!(r"{C}\{SCHEME}"), None, "URL:uni-share ticket"),
        add(format!(r"{C}\{SCHEME}"), Some("URL Protocol"), ""),
        add(format!(r"{C}\{SCHEME}\DefaultIcon"), None, &icon),
        add(format!(r"{C}\{SCHEME}\shell\open\command"), None, &open),
    ]
}

pub fn windows_reg_keys() -> Vec<String> {
    const C: &str = r"HKCU\Software\Classes";
    vec![format!(r"{C}\{PROG_ID}"), format!(r"{C}\.unishare"), format!(r"{C}\{SCHEME}")]
}

// ───────────────────────── platform entry points ─────────────────────────

/// Register the association for the current user.
pub fn install(exe: &Path) -> Result<Report> {
    if cfg!(target_os = "linux") {
        linux::install(exe)
    } else if cfg!(windows) {
        windows::install(exe)
    } else if cfg!(target_os = "macos") {
        bail!("on macOS file associations come from the .app bundle (Info.plist); install the packaged app instead")
    } else {
        bail!("file association is not supported on this platform")
    }
}

/// Remove what `install` created.
pub fn remove() -> Result<Report> {
    if cfg!(target_os = "linux") {
        linux::remove()
    } else if cfg!(windows) {
        windows::remove()
    } else {
        bail!("file association is not supported on this platform")
    }
}

/// Current handler for `.unishare`, if any.
pub fn status() -> Result<Option<String>> {
    if cfg!(target_os = "linux") {
        linux::status()
    } else if cfg!(windows) {
        windows::status()
    } else {
        Ok(None)
    }
}

mod linux {
    use super::*;

    fn data_home() -> PathBuf {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| directories::BaseDirs::new().map(|b| b.home_dir().join(".local/share")))
            .unwrap_or_else(|| PathBuf::from(".local/share"))
    }

    struct Paths {
        mime: PathBuf,
        desktop: PathBuf,
        icon_app: PathBuf,
        icon_mime: PathBuf,
    }

    fn paths() -> Paths {
        let d = data_home();
        Paths {
            mime: d.join("mime/packages/uni-share.xml"),
            desktop: d.join("applications").join(DESKTOP_ID),
            icon_app: d.join("icons/hicolor/64x64/apps/uni-share.png"),
            icon_mime: d.join("icons/hicolor/64x64/mimetypes/application-x-unishare.png"),
        }
    }

    pub fn install(exe: &Path) -> Result<Report> {
        let mut r = Report::default();
        let p = paths();
        let files: [(&Path, Vec<u8>); 4] = [
            (&p.mime, mime_xml().into_bytes()),
            (&p.desktop, desktop_entry(exe).into_bytes()),
            (&p.icon_app, ICON_PNG.to_vec()),
            (&p.icon_mime, ICON_PNG.to_vec()),
        ];
        for (path, data) in files {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(path, data).with_context(|| format!("writing {}", path.display()))?;
            r.did(format!("wrote {}", path.display()));
        }
        let d = data_home();
        run(&mut r, Command::new("update-mime-database").arg(d.join("mime")), "update-mime-database");
        run(&mut r, Command::new("update-desktop-database").arg(d.join("applications")), "update-desktop-database");
        run(&mut r, Command::new("xdg-mime").args(["default", DESKTOP_ID, MIME_TYPE]), "xdg-mime default (file type)");
        run(
            &mut r,
            Command::new("xdg-mime").args(["default", DESKTOP_ID, &format!("x-scheme-handler/{SCHEME}")]),
            "xdg-mime default (unishare: scheme)",
        );
        run(&mut r, Command::new("gtk-update-icon-cache").args(["-q", "-t"]).arg(d.join("icons/hicolor")), "gtk-update-icon-cache");
        Ok(r)
    }

    pub fn remove() -> Result<Report> {
        let mut r = Report::default();
        let p = paths();
        for path in [p.mime, p.desktop, p.icon_app, p.icon_mime] {
            match std::fs::remove_file(&path) {
                Ok(()) => r.did(format!("removed {}", path.display())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => r.warn(format!("{}: {e}", path.display())),
            }
        }
        let d = data_home();
        run(&mut r, Command::new("update-mime-database").arg(d.join("mime")), "update-mime-database");
        run(&mut r, Command::new("update-desktop-database").arg(d.join("applications")), "update-desktop-database");
        Ok(r)
    }

    pub fn status() -> Result<Option<String>> {
        let Ok(out) = Command::new("xdg-mime").args(["query", "default", MIME_TYPE]).output() else { return Ok(None) };
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            return Ok(None);
        }
        let exec = std::fs::read_to_string(paths().desktop)
            .ok()
            .and_then(|t| t.lines().find_map(|l| l.strip_prefix("Exec=").map(str::to_string)))
            .unwrap_or_default();
        Ok(Some(if exec.is_empty() { s } else { format!("{s} → {exec}") }))
    }
}

mod windows {
    use super::*;

    pub fn install(exe: &Path) -> Result<Report> {
        let mut r = Report::default();
        for op in windows_reg_ops(exe) {
            let what = format!("reg {}", op[1]);
            run(&mut r, Command::new("reg.exe").args(&op), &what);
        }
        Ok(r)
    }

    pub fn remove() -> Result<Report> {
        let mut r = Report::default();
        for key in windows_reg_keys() {
            run(&mut r, Command::new("reg.exe").args(["delete", &key, "/f"]), &format!("delete {key}"));
        }
        Ok(r)
    }

    pub fn status() -> Result<Option<String>> {
        let key = format!(r"HKCU\Software\Classes\{PROG_ID}\shell\open\command");
        let Ok(out) = Command::new("reg.exe").args(["query", &key, "/ve"]).output() else { return Ok(None) };
        if !out.status.success() {
            return Ok(None);
        }
        let s = String::from_utf8_lossy(&out.stdout);
        Ok(s.lines().find(|l| l.contains("REG_SZ")).map(|l| l.split("REG_SZ").nth(1).unwrap_or("").trim().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_entry_is_well_formed() {
        let d = desktop_entry(Path::new("/opt/uni share/uni-share"));
        assert!(d.starts_with("[Desktop Entry]\n"));
        assert!(d.contains("Exec=\"/opt/uni share/uni-share\" app %u\n"));
        assert!(d.contains("MimeType=application/x-unishare;x-scheme-handler/unishare;\n"));
        let d2 = desktop_entry(Path::new("/usr/local/bin/uni-share"));
        assert!(d2.contains("Exec=/usr/local/bin/uni-share app %u\n"));
    }

    #[test]
    fn mime_xml_has_glob_and_magic() {
        let x = mime_xml();
        assert!(x.contains("<glob pattern=\"*.unishare\"/>"));
        assert!(x.contains("value=\"UNISHARE\""));
        assert!(x.contains("type=\"application/x-unishare\""));
    }

    #[test]
    fn windows_ops_cover_progid_extension_and_scheme() {
        let ops = windows_reg_ops(Path::new(r"C:\Tools\uni-share.exe"));
        let joined: Vec<String> = ops.iter().map(|o| o.join(" ")).collect();
        assert!(joined.iter().any(|s| s.contains(r"\UniShare.Ticket\shell\open\command") && s.contains(r#""C:\Tools\uni-share.exe" app "%1""#)));
        assert!(joined.iter().any(|s| s.contains(r"\.unishare /ve /d UniShare.Ticket")));
        assert!(joined.iter().any(|s| s.contains(r"\unishare /v URL Protocol")));
        assert_eq!(windows_reg_keys().len(), 3);
    }
}
