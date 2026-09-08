//! Terminal UI helpers: coloured tags, progress bars, QR codes, clipboard,
//! desktop notifications.

use console::style;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::time::Duration;

pub fn tag(section: &str) -> String {
    style(format!("[{section}]")).cyan().bold().to_string()
}

pub fn info(section: &str, msg: impl AsRef<str>) {
    eprintln!("{} {}", tag(section), msg.as_ref());
}

pub fn ok(section: &str, msg: impl AsRef<str>) {
    eprintln!("{} {} {}", tag(section), style("✅").green(), msg.as_ref());
}

pub fn warn(section: &str, msg: impl AsRef<str>) {
    eprintln!("{} {} {}", tag(section), style("⚠").yellow(), style(msg.as_ref()).yellow());
}

pub fn error(section: &str, msg: impl AsRef<str>) {
    eprintln!("{} {} {}", tag(section), style("❌").red(), style(msg.as_ref()).red());
}

/// Byte-oriented progress bar with speed and ETA.
pub fn bytes_bar(section: &str, total: u64, hidden: bool) -> ProgressBar {
    let pb = if hidden {
        ProgressBar::with_draw_target(Some(total), ProgressDrawTarget::hidden())
    } else {
        ProgressBar::new(total)
    };
    let tpl = format!(
        "{} {{bar:28.cyan/blue}} {{percent:>3}}%  {{bytes}} / {{total_bytes}}  {{bytes_per_sec}}  {{eta_precise}}",
        tag(section)
    );
    pb.set_style(
        ProgressStyle::with_template(&tpl)
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("█▓░"),
    );
    pb.enable_steady_tick(Duration::from_millis(120));
    pb
}

pub fn spinner(section: &str, msg: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    let tpl = format!("{} {{spinner:.cyan}} {{msg}}", tag(section));
    pb.set_style(ProgressStyle::with_template(&tpl).unwrap_or_else(|_| ProgressStyle::default_spinner()));
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

/// Render a QR code as UTF-8 half-blocks (2 rows per line) for the terminal.
pub fn qr_string(data: &str) -> Option<String> {
    let code = qrcode::QrCode::new(data.as_bytes()).ok()?;
    let w = code.width();
    let colors = code.to_colors();
    let dark = |x: isize, y: isize| -> bool {
        if x < 0 || y < 0 || x >= w as isize || y >= w as isize {
            return false; // quiet zone
        }
        colors[y as usize * w + x as usize] == qrcode::Color::Dark
    };
    let margin = 2isize;
    let mut out = String::new();
    let mut y = -margin;
    while y < w as isize + margin {
        for x in -margin..(w as isize + margin) {
            let top = dark(x, y);
            let bottom = dark(x, y + 1);
            out.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        out.push('\n');
        y += 2;
    }
    Some(out)
}

/// Render a QR code as a standalone SVG (used by the GUI and `qr --svg`).
pub fn qr_svg(data: &str) -> Option<String> {
    let code = qrcode::QrCode::new(data.as_bytes()).ok()?;
    let w = code.width();
    let colors = code.to_colors();
    let margin = 4usize;
    let size = w + margin * 2;
    let mut path = String::new();
    for y in 0..w {
        for x in 0..w {
            if colors[y * w + x] == qrcode::Color::Dark {
                path.push_str(&format!("M{} {}h1v1h-1z", x + margin, y + margin));
            }
        }
    }
    Some(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {size} {size}\" shape-rendering=\"crispEdges\">\
<rect width=\"100%\" height=\"100%\" fill=\"#fff\"/><path d=\"{path}\" fill=\"#000\"/></svg>"
    ))
}

pub fn print_qr(section: &str, data: &str) {
    match qr_string(data) {
        Some(q) => {
            eprintln!("{} QR:", tag(section));
            for line in q.lines() {
                eprintln!("    {line}");
            }
        }
        None => warn(section, "could not render QR code"),
    }
}

/// Copy text to system clipboard. Returns false when no clipboard is available
/// (headless server, SSH…).
pub fn copy_to_clipboard(text: &str) -> bool {
    match arboard::Clipboard::new() {
        Ok(mut cb) => cb.set_text(text.to_string()).is_ok(),
        Err(e) => {
            tracing::debug!("clipboard unavailable: {e}");
            false
        }
    }
}

/// Best-effort desktop notification.
/// Show a desktop notification without blocking the caller: D-Bus round-trips
/// can take hundreds of ms and the caller may be a UI thread.
pub fn notify(summary: &str, body: &str) {
    let n = notify_rust::Notification::new()
        .appname(crate::APP_NAME)
        .summary(summary)
        .body(body)
        .timeout(notify_rust::Timeout::Milliseconds(6000))
        .finalize();
    let spawned = std::thread::Builder::new().name("uni-share-notify".into()).spawn(move || {
        if let Err(e) = n.show() {
            tracing::debug!("notification failed: {e}");
        }
    });
    if let Err(e) = spawned {
        tracing::debug!("notification thread failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_renders() {
        let q = qr_string("https://storage.to/abc123").unwrap();
        assert!(q.lines().count() > 10);
        assert!(q.contains('█'));
    }
}
