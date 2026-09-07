//! CLI subcommand implementations (thin glue over the library).

pub use crate::commands_global::send_global;
pub use crate::commands_lan::{list_devices, receive, send_lan};

use crate::*;
use anyhow::{Result, bail};
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

pub async fn download(_ctx: Ctx, _a: DownloadArgs) -> Result<()> {
    bail!("download: not implemented yet (phase 4)")
}

pub async fn daemon(_ctx: Ctx, _a: DaemonArgs) -> Result<()> {
    bail!("daemon: not implemented yet (phase 5)")
}

pub async fn gui(_ctx: Ctx, _a: GuiArgs) -> Result<()> {
    bail!("gui: not implemented yet (phase 5)")
}
