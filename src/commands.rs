//! CLI subcommand implementations (thin glue over the library).

pub use crate::commands_daemon::daemon;
pub use crate::commands_download::download;
pub use crate::commands_global::send_global;
pub use crate::commands_lan::{list_devices, receive, send_lan};
pub use crate::commands_ticket::{qr, ticket};

use crate::*;
use anyhow::Result;
use console::style;
use uni_share::fsutil::human_bytes;

pub async fn config(ctx: Ctx, a: ConfigArgs) -> Result<()> {
    if a.path {
        println!("{}", ctx.cfg_path.display());
        return Ok(());
    }
    println!("# {}", ctx.cfg_path.display());
    print!("{}", toml::to_string_pretty(&ctx.cfg)?);
    Ok(())
}

pub async fn associate(_ctx: Ctx, a: AssociateArgs) -> Result<()> {
    use uni_share::associate as assoc;
    const S: &str = "ASSOC";
    if a.status {
        match assoc::status()? {
            Some(h) => ui::info(S, format!(".unishare → {h}")),
            None => ui::warn(S, ".unishare no está asociado a ninguna aplicación"),
        }
        return Ok(());
    }
    let report = if a.remove {
        assoc::remove()?
    } else {
        let exe = match a.exe {
            Some(p) => p.canonicalize().unwrap_or(p),
            None => assoc::current_exe()?,
        };
        anyhow::ensure!(exe.exists(), "binary not found: {}", exe.display());
        assoc::install(&exe)?
    };
    for line in &report.actions {
        ui::info(S, line);
    }
    for w in &report.warnings {
        ui::warn(S, w);
    }
    if a.remove {
        ui::ok(S, "Asociación eliminada");
    } else {
        ui::ok(S, "Doble clic en un .unishare (o un enlace unishare:) abrirá uni-share");
        if cfg!(target_os = "linux") {
            ui::info(S, "Si el gestor de archivos no lo refleja, cierra sesión y vuelve a entrar.");
        }
    }
    Ok(())
}

pub async fn history(ctx: Ctx, a: HistoryArgs) -> Result<()> {
    if a.clear {
        let n = ctx.history.clear()?;
        ui::ok("HISTORY", format!("deleted {n} entries"));
        return Ok(());
    }
    let list = ctx.history.list(a.limit)?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(());
    }
    if list.is_empty() {
        ui::info("HISTORY", "no transfers yet");
        return Ok(());
    }
    println!(
        "{:<4} {:<17} {:<14} {:<10} {:<28} {:<9} {}",
        style("ID").bold(),
        style("DATE").bold(),
        style("TYPE").bold(),
        style("SIZE").bold(),
        style("NAME").bold(),
        style("STATUS").bold(),
        style("PEER / LINK").bold()
    );
    for r in list {
        let status = match r.status {
            uni_share::history::Status::Completed => style("completed").green(),
            uni_share::history::Status::Failed => style("failed").red(),
            uni_share::history::Status::Rejected => style("rejected").yellow(),
            uni_share::history::Status::InProgress => style("running").cyan(),
        };
        let name = if r.file_count > 1 {
            format!("{} ({} files)", r.name, r.file_count)
        } else {
            r.name.clone()
        };
        println!(
            "{:<4} {:<17} {:<14} {:<10} {:<28} {:<9} {}",
            r.id,
            r.timestamp.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M"),
            r.kind.as_str(),
            human_bytes(r.size),
            truncate(&name, 28),
            status,
            r.peer_or_link
        );
        if let Some(e) = r.error {
            println!("     {}", style(format!("↳ {e}")).red().dim());
        }
    }
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n - 1).collect();
        format!("{cut}…")
    }
}

pub async fn gui(ctx: Ctx, a: GuiArgs) -> Result<()> {
    uni_share::gui::run(ctx.cfg, ctx.cfg_path, ctx.history, uni_share::gui::GuiOptions { port: a.port, open_browser: !a.no_open }).await
}

/// Native desktop window. Runs on the main thread (no tokio here; the engine
/// owns its own runtime thread).
#[cfg(feature = "slint")]
pub fn app(ctx: Ctx, a: AppArgs) -> Result<()> {
    use anyhow::Context as _;
    let Ctx { cfg, cfg_path, history, .. } = ctx;
    let select = match a.select.as_deref() {
        None => None,
        Some(s) => {
            let (id, tab) = s.split_once(':').unwrap_or((s, "0"));
            Some((id.parse::<u64>().context("--select: invalid job id")?, tab.parse::<i32>().context("--select: invalid tab")?))
        }
    };
    uni_share::native::run(cfg, cfg_path, history, uni_share::native::AppOptions { open: a.open, demo: a.demo, screenshot: a.screenshot, dialog: a.dialog, select, light: a.light })
}

#[cfg(not(feature = "slint"))]
pub fn app(_ctx: Ctx, _a: AppArgs) -> Result<()> {
    anyhow::bail!("this build has no native GUI: rebuild with `cargo build --release --features slint` (or use `uni-share gui` for the browser UI)")
}
