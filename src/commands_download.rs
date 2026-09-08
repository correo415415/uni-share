//! `download <url>`: storage.to (file/collection) or SwissTransfer.

use crate::*;
use anyhow::{Result, bail};
use console::style;
use std::sync::Arc;
use uni_share::download::storage_to::StorageDownloader;
use uni_share::download::swisstransfer::{SwissTransferClient, is_swisstransfer_url, run_python_fallback};
use uni_share::fsutil::human_bytes;
use uni_share::global::storage_to::parse_share_url;
use uni_share::history::{Kind, Status};

const S: &str = "DOWNLOAD";

pub async fn download(ctx: Ctx, a: DownloadArgs) -> Result<()> {
    let dest = a.output.clone().unwrap_or_else(|| ctx.cfg.download_dir.clone());
    tokio::fs::create_dir_all(&dest).await?;

    if uni_share::ticket::looks_like_ticket(&a.url) {
        return crate::commands_ticket::download_from_ticket(ctx, a, dest).await;
    }
    if is_swisstransfer_url(&a.url) {
        if a.python {
            ui::info(S, "Delegando a swisstransfer-dl (Python)…");
            let rid = ctx.history.start(Kind::Download, "swisstransfer", 0, &a.url, 0)?;
            let r = run_python_fallback(&a.url, &dest, a.password.as_deref(), a.force, a.list).await;
            ctx.history.finish(rid, if r.is_ok() { Status::Completed } else { Status::Failed }, r.as_ref().err().map(|e| e.to_string()).as_deref())?;
            return r.map(|_| ui::ok(S, "Completado."));
        }
        return download_swisstransfer(ctx, a, dest).await;
    }
    if parse_share_url(&a.url).is_some() {
        return download_storage_to(ctx, a, dest).await;
    }
    if uni_share::global::smash::parse_share_url(&a.url).is_some() {
        bail!("Smash links must be downloaded from the browser (the Smash download API requires the recipient token); open {}", a.url);
    }
    bail!("unsupported URL: {} (expected storage.to, swisstransfer.com, a unishare: URI or a .unishare file)", a.url)
}

async fn download_storage_to(ctx: Ctx, a: DownloadArgs, dest: std::path::PathBuf) -> Result<()> {
    let dl = StorageDownloader::new()?;
    let sp = ui::spinner(S, "Consultando storage.to…");
    let info = dl.info(&a.url).await?;
    sp.finish_and_clear();
    ui::info(
        S,
        format!(
            "{} «{}» — {} archivo(s), {}{}",
            if info.kind == "collection" { "Colección" } else { "Archivo" },
            style(info.title.clone().unwrap_or_else(|| info.files[0].name.clone())).bold(),
            info.files.len(),
            human_bytes(info.total_size),
            info.expires_at.as_ref().map(|e| format!(", expira {e}")).unwrap_or_default()
        ),
    );
    if a.list {
        for f in &info.files {
            eprintln!("  {}  ({})", f.name, human_bytes(f.size));
        }
        return Ok(());
    }
    if info.password_protected {
        let pw = match &a.password {
            Some(p) => p.clone(),
            None => tokio::task::spawn_blocking(|| dialoguer::Password::new().with_prompt("Contraseña").interact()).await??,
        };
        dl.verify_password(&info, &pw).await?;
    }
    let rid = ctx.history.start(Kind::Download, info.title.clone().unwrap_or_else(|| info.files[0].name.clone()).as_str(), info.total_size, &a.url, info.files.len() as u32)?;
    let bar = ui::bytes_bar(S, info.total_size, ctx.quiet);
    let b2 = bar.clone();
    let b3 = bar.clone();
    let res = dl.download_all(&info, &dest, a.force, Arc::new(move |n| b2.inc(n)), move |f| b3.set_message(f.to_string())).await;
    finish(&ctx, rid, bar, res, &dest).await
}

async fn download_swisstransfer(ctx: Ctx, a: DownloadArgs, dest: std::path::PathBuf) -> Result<()> {
    let mut st = SwissTransferClient::new()?;
    let sp = ui::spinner(S, "Consultando SwissTransfer…");
    let t = st.get_transfer(&a.url, a.password.as_deref()).await?;
    sp.finish_and_clear();
    ui::info(
        S,
        format!("Transferencia {} — {} archivo(s), {}", style(t.title.clone().unwrap_or_else(|| t.link_id.clone())).bold(), t.files.len(), human_bytes(t.total_size)),
    );
    if let Some(m) = &t.message {
        ui::info(S, format!("Mensaje: {m}"));
    }
    if a.list {
        for f in &t.files {
            eprintln!("  {}  ({})", f.path, human_bytes(f.size));
        }
        return Ok(());
    }
    let rid = ctx.history.start(Kind::Download, t.title.clone().unwrap_or_else(|| t.link_id.clone()).as_str(), t.total_size, &a.url, t.files.len() as u32)?;
    let bar = ui::bytes_bar(S, t.total_size, ctx.quiet);
    let b2 = bar.clone();
    let b3 = bar.clone();
    let res = st.download_all(&t, &dest, a.force, Arc::new(move |n| b2.inc(n)), move |f| b3.set_message(f.to_string())).await;
    finish(&ctx, rid, bar, res, &dest).await
}

async fn finish(ctx: &Ctx, rid: i64, bar: indicatif::ProgressBar, res: Result<Vec<std::path::PathBuf>>, dest: &std::path::Path) -> Result<()> {
    match res {
        Ok(paths) => {
            bar.finish_and_clear();
            ctx.history.finish(rid, Status::Completed, None)?;
            ui::ok(S, format!("Completado. {} archivo(s) guardados en {}", paths.len(), style(dest.display()).bold()));
            if ctx.cfg.notifications {
                ui::notify("uni-share: descarga completada", &format!("{} archivo(s) en {}", paths.len(), dest.display()));
            }
            scan_saved(ctx, paths).await;
            Ok(())
        }
        Err(e) => {
            bar.abandon();
            ctx.history.finish(rid, Status::Failed, Some(&format!("{e:#}")))?;
            Err(e)
        }
    }
}

/// Local safety scan of freshly downloaded files (config `[scan]`).
pub async fn scan_saved(ctx: &Ctx, paths: Vec<std::path::PathBuf>) {
    if !ctx.cfg.scan.enabled || paths.is_empty() {
        return;
    }
    let sp = ui::spinner(S, "Análisis de seguridad…");
    let report = uni_share::scan::scan_paths_async(paths, ctx.cfg.scan.clone()).await;
    sp.finish_and_clear();
    ui::scan_report(S, &report);
}
