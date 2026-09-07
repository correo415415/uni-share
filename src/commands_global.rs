//! `send-global`: upload to storage.to (default) or Smash.

use crate::*;
use anyhow::{Context, Result, bail};
use console::style;
use std::sync::Arc;
use uni_share::fsutil::{collect_files, human_bytes, total_size};
use uni_share::global::smash::SmashClient;
use uni_share::global::storage_to::Client as StorageClient;
use uni_share::global::upload::{UploadOptions, UploadOutcome, upload_entries, upload_entries_smash};
use uni_share::history::{Kind, Status};
use uni_share::lan::client::compress_to_temp;

const S: &str = "GLOBAL";

pub async fn send_global(mut ctx: Ctx, a: SendGlobalArgs) -> Result<()> {
    anyhow::ensure!(a.path.exists(), "path not found: {}", a.path.display());
    let backend = a.backend.clone().unwrap_or_else(|| ctx.cfg.global.backend.clone());
    if !matches!(backend.as_str(), "storage_to" | "smash") {
        bail!("unknown backend '{backend}' (use storage_to or smash)");
    }
    if let Some(p) = &a.password {
        anyhow::ensure!((4..=100).contains(&p.chars().count()), "password must be 4-100 characters");
    }

    // Collect files (optionally compress folders).
    let compress = a.compress || (ctx.cfg.compress_folders && a.path.is_dir());
    let (_tmp, files) = if compress && a.path.is_dir() {
        let sp = ui::spinner(S, "Comprimiendo carpeta (tar.zst)…");
        let r = compress_to_temp(&a.path).await?;
        sp.finish_and_clear();
        (Some(r.0), r.1)
    } else {
        (None, collect_files(&a.path)?)
    };
    let files: Vec<_> = files.into_iter().filter(|f| f.size > 0).collect();
    anyhow::ensure!(!files.is_empty(), "nothing to upload (empty files are skipped by storage.to)");
    let total = total_size(&files);
    let name = a.path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "upload".into());

    let opts = UploadOptions {
        expiry_days: a.expiry_days.or(Some(ctx.cfg.global.expiry_days)),
        parallel_parts: ctx.cfg.global.parallel_parts,
        password: a.password.clone(),
        max_downloads: a.max_downloads,
    };
    let backend_label = if backend == "smash" { "Smash" } else { "storage.to" };
    if !a.json {
        ui::info(S, format!("Subiendo {} ({} archivo(s), {}) a {}…", style(&name).bold(), files.len(), human_bytes(total), backend_label));
    }
    let rid = ctx.history.start(Kind::GlobalUpload, &name, total, backend_label, files.len() as u32)?;

    let bar = ui::bytes_bar(S, total, ctx.quiet || a.json);
    let b2 = bar.clone();
    let progress: uni_share::global::upload::ProgressFn = Arc::new(move |n| b2.inc(n));
    let b3 = bar.clone();
    let on_file = move |f: &str| b3.set_message(f.to_string());

    let outcome: Result<UploadOutcome> = if backend == "smash" {
        let key = ctx.cfg.global.smash_api_key.clone().filter(|k| !k.is_empty()).context(
            "Smash backend needs an API key: set global.smash_api_key in config.toml (https://api.fromsmash.com)",
        )?;
        let client = SmashClient::new(&key, &ctx.cfg.global.smash_region)?;
        upload_entries_smash(&client, &files, &name, &opts, progress, on_file).await
    } else {
        let token = ctx.ensure_visitor_token()?;
        let client = StorageClient::new(&ctx.cfg.global.storage_to_api, &token, ctx.cfg.global.storage_to_token.as_deref())?;
        upload_entries(&client, &files, &opts, progress, on_file).await
    };

    match outcome {
        Ok(o) => {
            bar.finish_and_clear();
            let meta = serde_json::json!({ "owner_token": o.owner_token, "id": o.id, "kind": o.kind, "backend": backend }).to_string();
            ctx.history.set_link(rid, &o.url, Some(&meta))?;
            ctx.history.finish(rid, Status::Completed, None)?;
            if a.json {
                println!("{}", serde_json::to_string_pretty(&o)?);
                return Ok(());
            }
            ui::ok(S, "Subida completada.");
            eprintln!("{} Link: {}", ui::tag(S), style(&o.url).green().bold().underlined());
            if let Some(exp) = &o.expires_at {
                ui::info(S, format!("Expira: {exp}"));
            }
            if o.password_protected {
                ui::info(S, "Protegido con contraseña");
            }
            if let Some(m) = a.max_downloads {
                ui::info(S, format!("Máximo de descargas: {m}"));
            }
            if !a.no_qr {
                ui::print_qr(S, &o.url);
            }
            if !a.no_clipboard {
                if ui::copy_to_clipboard(&o.url) {
                    ui::info(S, "Link copiado al portapapeles.");
                } else {
                    ui::warn(S, "no se pudo acceder al portapapeles (sin entorno gráfico)");
                }
            }
            if ctx.cfg.notifications {
                ui::notify("uni-share: subida completada", &o.url);
            }
            // Print the URL on stdout too so scripts can capture it.
            println!("{}", o.url);
            Ok(())
        }
        Err(e) => {
            bar.abandon();
            ctx.history.finish(rid, Status::Failed, Some(&format!("{e:#}")))?;
            Err(e)
        }
    }
}
