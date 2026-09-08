//! `ticket create|show|qr|save` and `qr`: `.unishare` tickets and QR sharing.

use crate::*;
use anyhow::{Context, Result};
use console::style;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uni_share::download::ticket::download_ticket;
use uni_share::fsutil::{collect_files, human_bytes};
use uni_share::history::{Kind, Status};
use uni_share::lan::client::hash_all;
use uni_share::ticket::{Source, Ticket, TicketFile};

const S: &str = "TICKET";

pub async fn ticket(ctx: Ctx, a: TicketArgs) -> Result<()> {
    match a.action {
        TicketAction::Create(c) => create(ctx, c).await,
        TicketAction::Show { ticket, json } => {
            let t = uni_share::ticket::resolve(&ticket)?;
            let sig = t.verify_signature();
            if json {
                let mut v = serde_json::to_value(&t)?;
                v["signature_status"] = serde_json::to_value(&sig)?;
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                print_ticket(&t);
            }
            if sig.is_invalid() {
                anyhow::bail!("la firma del ticket NO es válida: {}", sig.label());
            }
            Ok(())
        }
        TicketAction::Sign { ticket, output } => {
            let mut t = uni_share::ticket::resolve(&ticket)?;
            let key = signing_key()?;
            t.sign(&key)?;
            let out = output.unwrap_or_else(|| {
                if uni_share::ticket::is_uri(&ticket) { PathBuf::from(t.default_filename()) } else { PathBuf::from(&ticket) }
            });
            t.save(&out)?;
            ui::ok(S, format!("Ticket firmado ({}) → {}", style(key.fingerprint()).bold(), style(out.display()).bold()));
            println!("{}", t.to_uri()?);
            Ok(())
        }
        TicketAction::Identity => {
            let key = signing_key()?;
            ui::info(S, format!("Huella de firma: {}", style(key.fingerprint()).bold()));
            ui::info(S, format!("Clave pública (Ed25519, base64url): {}", key.public_b64()));
            ui::info(S, format!("Fichero: {}", uni_share::signing::SigningKey::key_path(&uni_share::config::data_dir()).display()));
            ui::info(S, "Comparte la huella por otro canal: quien reciba tus tickets podrá comprobar que los firmaste tú.");
            Ok(())
        }
        TicketAction::Qr { ticket, uri_only } => {
            let t = uni_share::ticket::resolve(&ticket)?;
            let uri = t.to_uri_compact()?;
            if !uri_only {
                if uri.len() < t.to_uri()?.len() {
                    ui::warn(S, "ticket demasiado grande para un QR: se omiten los detalles de archivos en el QR (el .unishare completo sí los conserva)");
                }
                ui::print_qr(S, &uri);
            }
            println!("{uri}");
            Ok(())
        }
        TicketAction::Save { uri, output } => {
            let t = Ticket::from_uri(&uri)?;
            let out = output.unwrap_or_else(|| PathBuf::from(t.default_filename()));
            t.save(&out)?;
            ui::ok(S, format!("Ticket guardado en {}", style(out.display()).bold()));
            Ok(())
        }
    }
}

/// This device's Ed25519 ticket-signing key (created on first use).
pub fn signing_key() -> Result<uni_share::signing::SigningKey> {
    uni_share::signing::SigningKey::load_or_generate(&uni_share::config::data_dir()).context("loading signing key")
}

/// Sign `t` when the CLI flags / config ask for it. Returns the signer fingerprint.
pub fn maybe_sign(t: &mut Ticket, want: bool) -> Result<Option<String>> {
    if !want {
        return Ok(None);
    }
    let key = signing_key()?;
    t.sign(&key)?;
    Ok(Some(key.fingerprint()))
}

async fn create(ctx: Ctx, a: TicketCreateArgs) -> Result<()> {
    let first = a.links.first().context("at least one link is required")?;
    let name = a.name.clone().unwrap_or_else(|| {
        a.verify_from
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| first.trim_end_matches('/').rsplit('/').next().unwrap_or("share").to_string())
    });
    let mut t = Ticket::new(name);
    t.sender = Some(ctx.cfg.device_name.clone());
    t.message = a.message.clone();
    for l in &a.links {
        anyhow::ensure!(l.starts_with("http://") || l.starts_with("https://"), "not a URL: {l}");
        t.sources.push(Source::from_url(l, a.password.clone()));
    }
    if let Some(local) = &a.verify_from {
        anyhow::ensure!(local.exists(), "path not found: {}", local.display());
        let files = collect_files(local)?;
        let sp = ui::spinner(S, "Calculando BLAKE3…");
        let hashes = hash_all(&files, |f| sp.set_message(f.to_string())).await?;
        sp.finish_and_clear();
        t.files = files
            .iter()
            .zip(hashes.iter())
            .map(|(f, h)| TicketFile { path: f.rel_path.clone(), size: f.size, blake3: Some(h.clone()) })
            .collect();
        t.total_size = files.iter().map(|f| f.size).sum();
    }
    let want_sign = if a.sign { true } else if a.no_sign { false } else { ctx.cfg.sign_tickets };
    maybe_sign(&mut t, want_sign)?;
    let out = a.output.clone().unwrap_or_else(|| PathBuf::from(t.default_filename()));
    t.save(&out)?;
    ui::ok(S, format!("Ticket creado: {}  ({})", style(out.display()).bold(), t.summary()));
    if a.qr {
        ui::print_qr(S, &t.to_uri_compact()?);
    }
    println!("{}", out.display());
    Ok(())
}

pub fn print_ticket(t: &Ticket) {
    eprintln!("{} {}", ui::tag(S), style(&t.name).bold());
    eprintln!("    id: {}   creado: {}   por: {}", t.id, t.created.format("%Y-%m-%d %H:%M"), t.sender.as_deref().unwrap_or("-"));
    if let Some(e) = &t.expires {
        eprintln!("    expira: {}{}", e.format("%Y-%m-%d %H:%M"), if t.is_expired() { "  (EXPIRADO)" } else { "" });
    }
    if let Some(m) = &t.message {
        eprintln!("    mensaje: {m}");
    }
    eprintln!("    tamaño: {}   archivos: {}", human_bytes(t.total_size), if t.files.is_empty() { "?".into() } else { t.files.len().to_string() });
    match t.verify_signature() {
        uni_share::signing::SignatureStatus::Unsigned => eprintln!("    firma: {}", style("ninguna").dim()),
        uni_share::signing::SignatureStatus::Valid { fingerprint, .. } => eprintln!("    firma: {} Ed25519 válida · huella del firmante {}", style("✓").green(), style(fingerprint).bold()),
        uni_share::signing::SignatureStatus::Invalid { reason } => eprintln!("    firma: {} INVÁLIDA — {} (ticket manipulado o falsificado)", style("✗").red().bold(), reason),
    }
    eprintln!("    fuentes:");
    for (i, s) in t.sources.iter().enumerate() {
        match s.lan_endpoint() {
            Some(ep) => eprintln!(
                "      {}. [LAN] {} en {}  huella {}{}   → uni-share send-lan <ruta> --to <ticket>",
                i + 1,
                ep.name.as_deref().unwrap_or("receptor"),
                ep.addr(),
                uni_share::lan::tls::short_fingerprint(&ep.fingerprint),
                if ep.pin.is_some() { "  (PIN incluido)" } else { "" }
            ),
            None => eprintln!("      {}. [{}] {}{}", i + 1, s.label(), s.url(), if s.password().is_some() { "  (con contraseña)" } else { "" }),
        }
    }
    if !t.files.is_empty() {
        eprintln!("    contenido:");
        for f in t.files.iter().take(30) {
            eprintln!("      {:<50} {:>10}  {}", f.path, human_bytes(f.size), f.blake3.as_deref().map(|h| &h[..12]).unwrap_or("-"));
        }
        if t.files.len() > 30 {
            eprintln!("      … y {} más", t.files.len() - 30);
        }
    }
}

/// `download <ticket>` path.
pub async fn download_from_ticket(ctx: Ctx, a: DownloadArgs, dest: PathBuf) -> Result<()> {
    let t = uni_share::ticket::resolve(&a.url)?;
    print_ticket(&t);
    if a.list {
        return Ok(());
    }
    if t.is_lan() && t.sources.iter().all(Source::is_lan) {
        anyhow::bail!("es un ticket de emparejamiento LAN (un receptor, no una descarga): usa `uni-share send-lan <ruta> --to '{}'`", a.url);
    }
    if t.verify_signature().is_invalid() {
        anyhow::bail!("la firma Ed25519 del ticket no es válida: el ticket ha sido manipulado o falsificado. No se descarga nada.");
    }
    if t.is_expired() {
        ui::warn(S, "el ticket indica que la compartición ha expirado; se intentará igualmente");
    }
    let rid = ctx.history.start(Kind::Download, &t.name, t.total_size, &format!("ticket:{}", t.id), t.files.len() as u32)?;
    let bar = ui::bytes_bar("DOWNLOAD", t.total_size, ctx.quiet);
    let b2 = bar.clone();
    let b3 = bar.clone();
    let b4 = bar.clone();
    let res = download_ticket(
        &t,
        &dest,
        a.force,
        Arc::new(move |n| b2.inc(n)),
        move |f| b3.set_message(f.to_string()),
        move |src, prev| {
            if let Some(e) = prev {
                b4.println(format!("{} {} fuente anterior falló ({e:#}); probando {}", ui::tag(S), style("⚠").yellow(), src.label()));
            }
        },
    )
    .await;
    match res {
        Ok(r) => {
            bar.finish_and_clear();
            ctx.history.finish(rid, Status::Completed, None)?;
            ui::ok(
                "DOWNLOAD",
                format!(
                    "Completado desde {}. {} archivo(s) en {} — BLAKE3 verificado: {}{}",
                    r.source,
                    r.saved.len(),
                    style(dest.display()).bold(),
                    r.verified,
                    if r.unverified > 0 { format!(" (sin digest: {})", r.unverified) } else { String::new() }
                ),
            );
            if ctx.cfg.notifications {
                ui::notify("uni-share: descarga completada", &t.name);
            }
            crate::commands_download::scan_saved(&ctx, r.saved).await;
            Ok(())
        }
        Err(e) => {
            bar.abandon();
            ctx.history.finish(rid, Status::Failed, Some(&format!("{e:#}")))?;
            Err(e)
        }
    }
}

/// `qr <data>`: any text; tickets are converted to their compact URI.
pub async fn qr(_ctx: Ctx, a: QrArgs) -> Result<()> {
    let data = if uni_share::ticket::looks_like_ticket(&a.data) && Path::new(&a.data).exists() {
        Ticket::load(Path::new(&a.data))?.to_uri_compact()?
    } else {
        a.data.clone()
    };
    if let Some(svg) = &a.svg {
        std::fs::write(svg, ui::qr_svg(&data).context("data too long for a QR code")?)?;
        ui::ok("QR", format!("SVG guardado en {}", style(svg.display()).bold()));
        return Ok(());
    }
    ui::print_qr("QR", &data);
    Ok(())
}
