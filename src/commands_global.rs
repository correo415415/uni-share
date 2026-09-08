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
    let want_ticket = a.ticket.is_some() || a.ticket_qr;
    // BLAKE3 digests for the ticket (computed up-front, cheap compared with the upload).
    let hashes = if want_ticket {
        let sp = ui::spinner(S, "Calculando BLAKE3 para el ticket…");
        let h = uni_share::lan::client::hash_all(&files, |f| sp.set_message(f.to_string())).await?;
        sp.finish_and_clear();
        h
    } else {
        Vec::new()
    };
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

            // Optional .unishare ticket next to the source (or at the given path).
            let mut ticket_path: Option<std::path::PathBuf> = None;
            let mut ticket_uri: Option<String> = None;
            if want_ticket {
                let expires = o.expires_at.as_deref().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.with_timezone(&chrono::Utc));
                let mut t = uni_share::ticket::Ticket::from_upload(&name, &o.url, a.password.as_deref(), &ctx.cfg.device_name, &files, &hashes, expires);
                if let Some(fp) = crate::commands_ticket::maybe_sign(&mut t, ctx.cfg.sign_tickets && !a.no_sign)? {
                    ui::info(S, format!("Ticket firmado (Ed25519, huella {fp})"));
                }
                let out = match a.ticket.as_deref() {
                    Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
                    _ => a.path.parent().map(|d| d.to_path_buf()).unwrap_or_default().join(t.default_filename()),
                };
                t.save(&out)?;
                ticket_uri = Some(t.to_uri_compact()?);
                ticket_path = Some(out);
            }
            if a.json {
                let mut v = serde_json::to_value(&o)?;
                if let Some(p) = &ticket_path {
                    v["ticket"] = serde_json::json!(p);
                    v["ticket_uri"] = serde_json::json!(ticket_uri);
                }
                println!("{}", serde_json::to_string_pretty(&v)?);
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
            if let Some(p) = &ticket_path {
                ui::info(S, format!("Ticket .unishare: {}", style(p.display()).bold()));
            }
            if !a.no_qr {
                match (&ticket_uri, a.ticket_qr) {
                    (Some(u), true) => {
                        ui::info(S, "QR del ticket (incluye contraseña y digests):");
                        ui::print_qr(S, u);
                    }
                    _ => ui::print_qr(S, &o.url),
                }
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
