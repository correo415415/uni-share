//! LAN subcommands: list-devices, receive, send-lan.

use crate::*;
use anyhow::{Context, Result, bail};
use console::style;
use std::sync::Arc;
use std::time::Duration;
use uni_share::fsutil::{collect_files, human_bytes, total_size, tree_preview};
use uni_share::history::{Kind, Status};
use uni_share::lan::client::{Sender, build_manifest, compress_to_temp, hash_all};
use uni_share::lan::discovery::{Announcer, Device, discover, parse_target, resolve_host};
use uni_share::lan::server::{Decision, ServerOptions, start};
use uni_share::lan::tls::{Identity, short_fingerprint};

const S: &str = "LAN";

fn identity(ctx: &Ctx) -> Result<Identity> {
    Identity::load_or_generate(&uni_share::config::data_dir(), &ctx.cfg.device_name)
}

pub async fn list_devices(ctx: Ctx, a: ListDevicesArgs) -> Result<()> {
    let me = identity(&ctx).ok().map(|i| i.fingerprint);
    let sp = (!a.json && !ctx.quiet).then(|| ui::spinner(S, "Buscando dispositivos…"));
    let devices = discover(Duration::from_secs(a.timeout), me.as_deref()).await?;
    if let Some(sp) = sp {
        sp.finish_and_clear();
    }
    if a.json {
        println!("{}", serde_json::to_string_pretty(&devices)?);
        return Ok(());
    }
    print_devices(&devices);
    Ok(())
}

fn print_devices(devices: &[Device]) {
    if devices.is_empty() {
        ui::warn(S, "no se encontraron dispositivos (¿está `uni-share receive` en ejecución en la otra máquina?)");
        return;
    }
    ui::info(S, format!("{} dispositivo(s) encontrado(s):", devices.len()));
    for (i, d) in devices.iter().enumerate() {
        let addr = d.best_addr().map(|a| a.to_string()).unwrap_or_else(|| "?".into());
        let state = if d.online { style("online").green() } else { style("offline").red() };
        let pin = if d.requires_pin { style(" 🔒PIN").yellow().to_string() } else { String::new() };
        eprintln!(
            "  {}. {} ({}:{})  {}  v{}  {}{}",
            i + 1,
            style(&d.name).bold(),
            addr,
            d.port,
            state,
            d.version,
            style(short_fingerprint(&d.fingerprint)).dim(),
            pin
        );
    }
}

pub async fn receive(ctx: Ctx, a: ReceiveArgs) -> Result<()> {
    let id = identity(&ctx)?;
    let dest = a.output.clone().unwrap_or_else(|| ctx.cfg.download_dir.clone());
    tokio::fs::create_dir_all(&dest).await.with_context(|| format!("creating {}", dest.display()))?;
    let pin = a.pin.clone().or_else(|| ctx.cfg.pin.clone());
    let auto_accept = a.auto_accept || ctx.cfg.auto_accept;
    let opts = ServerOptions {
        device_name: ctx.cfg.device_name.clone(),
        port: a.port.unwrap_or(ctx.cfg.lan_port),
        pin: pin.clone(),
        force_overwrite: a.force,
        dest_dir: dest.clone(),
        rate_limit_mbps: ctx.cfg.rate_limit_mbps,
        state_dir: Some(uni_share::config::data_dir()),
    };
    let mut server = start(id.clone(), opts).await?;
    let _announcer = Announcer::start(&ctx.cfg.device_name, server.addr.port(), &id.fingerprint, pin.is_some())?;
    ui::info(S, format!("Escuchando en {} como {}", style(server.addr).bold(), style(&ctx.cfg.device_name).bold()));
    ui::info(S, format!("Huella TLS: {}   Destino: {}", short_fingerprint(&id.fingerprint), dest.display()));
    if pin.is_some() {
        ui::info(S, "PIN requerido para enviar a este dispositivo");
    }
    if auto_accept {
        ui::warn(S, "auto-aceptación activada");
    }
    if a.qr || a.ticket.is_some() {
        let ips = uni_share::engine::local_ips();
        let host = ips.first().cloned().context("no LAN address found for the pairing ticket")?;
        let mut t = uni_share::ticket::Ticket::lan_pairing(&ctx.cfg.device_name, &host, server.addr.port(), &id.fingerprint, pin.as_deref());
        crate::commands_ticket::maybe_sign(&mut t, ctx.cfg.sign_tickets)?;
        let uri = t.to_uri()?;
        ui::info(S, format!("Ticket de emparejamiento ({}): envía con `uni-share send-lan <ruta> --to '<ticket>'`", style(&host).bold()));
        ui::print_qr(S, &uri);
        eprintln!("    {uri}");
        if pin.is_some() {
            ui::warn(S, "el ticket incluye el PIN: compártelo solo con quien deba enviarte archivos");
        }
        if let Some(p) = &a.ticket {
            t.save(p)?;
            ui::ok(S, format!("Ticket guardado en {}", style(p.display()).bold()));
        }
    }

    let history = ctx.history.clone();
    let notifications = ctx.cfg.notifications;
    let scan_cfg = ctx.cfg.scan.clone();
    let quiet = ctx.quiet;
    let mut progress_rx = server.progress.clone();
    let mut bar: Option<indicatif::ProgressBar> = None;
    let mut record_id: Option<i64> = None;

    loop {
        tokio::select! {
            Some(offer) = server.offers.recv() => {
                let m = &offer.manifest;
                ui::info(S, format!(
                    "Solicitud entrante de {} ({}): {} — {} archivo(s), {}",
                    style(&m.sender).bold(), offer.peer.ip(), style(&m.name).bold(), m.files.len(), human_bytes(m.total_size)
                ));
                if !m.sender_fingerprint.is_empty() {
                    ui::info(S, format!("Huella del emisor: {}", short_fingerprint(&m.sender_fingerprint)));
                }
                if let Some(r) = &offer.resume {
                    ui::info(S, format!(
                        "Transferencia interrumpida anteriormente: se reanuda ({} archivo(s) y {} ya en {})",
                        r.files_done, human_bytes(r.bytes_done), r.dest_dir.display()
                    ));
                }
                if m.files.len() > 1 {
                    let preview: Vec<uni_share::fsutil::FileEntry> = m.files.iter().map(|f| uni_share::fsutil::FileEntry {
                        rel_path: f.path.clone(), size: f.size, abs_path: Default::default() }).collect();
                    eprintln!("{}", tree_preview(&preview, 12));
                }
                if notifications {
                    ui::notify("uni-share: solicitud entrante", &format!("{} quiere enviarte {} ({})", m.sender, m.name, human_bytes(m.total_size)));
                }
                let accept = if auto_accept {
                    true
                } else {
                    let q = format!("{} ¿Aceptar?", ui::tag(S));
                    tokio::task::spawn_blocking(move || {
                        dialoguer::Confirm::new().with_prompt(q).default(true).interact().unwrap_or(false)
                    }).await.unwrap_or(false)
                };
                if accept {
                    let rid = history.start(Kind::LanReceive, &m.name, m.total_size, &m.sender, m.files.len() as u32)?;
                    record_id = Some(rid);
                    let _ = offer.decision.send(Decision::Accept { dest_dir: dest.clone() });
                    ui::info(S, "Transferencia aceptada. Recibiendo…");
                    if !quiet {
                        bar = Some(ui::bytes_bar(S, m.total_size, false));
                    }
                } else {
                    let _ = offer.decision.send(Decision::Reject { reason: "rechazada por el usuario".into() });
                    let rid = history.start(Kind::LanReceive, &m.name, m.total_size, &m.sender, m.files.len() as u32)?;
                    history.finish(rid, Status::Rejected, None)?;
                    ui::warn(S, "Transferencia rechazada");
                }
            }
            Ok(()) = progress_rx.changed() => {
                let p = progress_rx.borrow().clone();
                if let Some(b) = &bar {
                    b.set_position(p.received.min(p.total));
                    if !p.current_file.is_empty() { b.set_message(p.current_file.clone()); }
                }
            }
            Some(done) = server.completed.recv() => {
                if let Some(b) = bar.take() { b.set_position(done.total); b.finish_and_clear(); }
                let where_ = done.dest_dir.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                ui::ok(S, format!("Verificación BLAKE3 correcta. {} archivo(s) guardados en {}", done.files_total, style(&where_).bold()));
                if let Some(rid) = record_id.take() { history.finish(rid, Status::Completed, None)?; }
                if scan_cfg.enabled && !done.saved.is_empty() {
                    let sp = ui::spinner(S, "Análisis de seguridad…");
                    let report = uni_share::scan::scan_paths_async(done.saved.clone(), scan_cfg.clone()).await;
                    sp.finish_and_clear();
                    ui::scan_report(S, &report);
                }
                if notifications {
                    ui::notify("uni-share: transferencia completada", &format!("{} de {} guardado en {}", done.name, done.sender, where_));
                }
                if a.once { break; }
            }
            _ = tokio::signal::ctrl_c() => {
                eprintln!();
                ui::info(S, "Saliendo…");
                if let Some(rid) = record_id.take() { let _ = history.finish(rid, Status::Failed, Some("interrupted")); }
                break;
            }
        }
    }
    server.shutdown().await;
    Ok(())
}

pub async fn send_lan(ctx: Ctx, a: SendLanArgs) -> Result<()> {
    anyhow::ensure!(a.path.exists(), "path not found: {}", a.path.display());
    let id = identity(&ctx)?;

    // 1. Resolve target device.
    let mut pin = a.pin.clone();
    let (ip, port, fingerprint, target_name) = match &a.to {
        Some(t) if uni_share::ticket::looks_like_ticket(t) => {
            let ticket = uni_share::ticket::resolve(t).context("reading pairing ticket")?;
            let sig = ticket.verify_signature();
            if sig.is_invalid() {
                bail!("la firma del ticket de emparejamiento no es válida ({}) — ticket manipulado", sig.label());
            }
            if let uni_share::signing::SignatureStatus::Valid { fingerprint, .. } = &sig {
                ui::info(S, format!("Ticket firmado · huella del firmante {}", style(fingerprint).bold()));
            }
            let ep = ticket.lan_endpoint().context("the ticket is a download ticket, not a LAN pairing ticket (use `download`)")?;
            let ip = resolve_host(&ep.host, ep.port).await?;
            if pin.is_none() {
                pin = ep.pin.clone();
            }
            let name = ep.name.clone().unwrap_or_else(|| ticket.name.clone());
            ui::info(S, format!("Ticket de emparejamiento: {} en {} (huella fijada)", style(&name).bold(), ep.addr()));
            (ip, ep.port, Some(ep.fingerprint.clone()), name)
        }
        Some(t) => match parse_target(t, ctx.cfg.lan_port) {
            Some((ip, port)) => (ip, port, None, t.clone()),
            None => {
                let sp = ui::spinner(S, &format!("Buscando «{t}»…"));
                let devices = discover(Duration::from_secs(a.timeout), Some(&id.fingerprint)).await?;
                sp.finish_and_clear();
                let d = devices
                    .iter()
                    .find(|d| d.name.eq_ignore_ascii_case(t))
                    .ok_or_else(|| anyhow::anyhow!("device '{t}' not found on the LAN"))?;
                let ip = d.best_addr().context("device has no address")?;
                (ip, d.port, Some(d.fingerprint.clone()), d.name.clone())
            }
        },
        None => {
            let sp = ui::spinner(S, "Buscando dispositivos…");
            let devices = discover(Duration::from_secs(a.timeout), Some(&id.fingerprint)).await?;
            sp.finish_and_clear();
            if devices.is_empty() {
                bail!("no devices found; run `uni-share receive` on the target or use --to ip[:port]");
            }
            print_devices(&devices);
            let idx = if devices.len() == 1 {
                0
            } else {
                let n = devices.len();
                let q = format!("{} Selecciona destino (1-{n})", ui::tag(S));
                tokio::task::spawn_blocking(move || -> Result<usize> {
                    let v: usize = dialoguer::Input::new()
                        .with_prompt(q)
                        .validate_with(|s: &String| s.parse::<usize>().ok().filter(|v| (1..=n).contains(v)).map(|_| ()).ok_or("número inválido"))
                        .interact_text()?
                        .parse()?;
                    Ok(v - 1)
                })
                .await??
            };
            let d = &devices[idx];
            let ip = d.best_addr().context("device has no address")?;
            (ip, d.port, Some(d.fingerprint.clone()), d.name.clone())
        }
    };
    if fingerprint.is_none() {
        ui::warn(S, "conexión por IP directa: no hay huella mDNS para fijar; la huella del receptor se mostrará para verificación manual");
    }

    // 2. Collect + hash files.
    let compress = a.compress || (ctx.cfg.compress_folders && a.path.is_dir());
    let (_tmp, files) = if compress && a.path.is_dir() {
        let sp = ui::spinner(S, "Comprimiendo carpeta (tar.zst)…");
        let r = compress_to_temp(&a.path).await?;
        sp.finish_and_clear();
        (Some(r.0), r.1)
    } else {
        (None, collect_files(&a.path)?)
    };
    let total = total_size(&files);
    let sp = ui::spinner(S, "Calculando BLAKE3…");
    let hashes = hash_all(&files, |f| sp.set_message(format!("BLAKE3 {f}"))).await?;
    sp.finish_and_clear();
    let name = a.path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "transfer".into());
    let mut manifest = build_manifest(&ctx.cfg.device_name, &id.fingerprint, &name, &files, &hashes);
    manifest.compressed_archive = compress && a.path.is_dir();

    // 3. Connect + offer.
    let sender = Sender::new(ip, port, fingerprint.as_deref(), pin.clone())?;
    let info = sender.info().await.with_context(|| format!("cannot reach {target_name} at {ip}:{port}"))?;
    ui::info(S, format!("Conectado a {} — huella {}", style(&info.name).bold(), short_fingerprint(&info.fingerprint)));
    if info.requires_pin && pin.is_none() {
        bail!("{} requiere PIN: usa --pin", info.name);
    }
    let rid = ctx.history.start(Kind::LanSend, &name, total, &info.name, files.len() as u32)?;
    ui::info(S, format!("Esperando aceptación de {}… ({} archivo(s), {})", info.name, files.len(), human_bytes(total)));

    let bar = ui::bytes_bar(S, total, ctx.quiet);
    let bar2 = bar.clone();
    let progress: uni_share::lan::client::ProgressFn = Arc::new(move |n, f| {
        bar2.inc(n);
        bar2.set_message(f.to_string());
    });
    let rate_bps = (ctx.cfg.rate_limit_mbps as u64) * 1_000_000 / 8;
    let started = std::time::Instant::now();
    let res = sender.send(&manifest, &files, &hashes, progress, Duration::from_secs(300), rate_bps).await;
    match res {
        Ok(report) => {
            bar.finish_and_clear();
            let secs = started.elapsed().as_secs_f64().max(0.001);
            ui::ok(
                S,
                format!(
                    "Verificación BLAKE3 correcta. {} archivo(s), {} en {:.1}s ({}){}",
                    report.files,
                    human_bytes(report.bytes),
                    secs,
                    uni_share::fsutil::human_rate(report.bytes as f64 / secs),
                    if report.retries > 0 { format!(" — {} reintento(s)", report.retries) } else { String::new() }
                ),
            );
            ctx.history.finish(rid, Status::Completed, None)?;
            if ctx.cfg.notifications {
                ui::notify("uni-share: envío completado", &format!("{name} enviado a {}", info.name));
            }
            Ok(())
        }
        Err(e) => {
            bar.abandon();
            ctx.history.finish(rid, Status::Failed, Some(&format!("{e:#}")))?;
            Err(e)
        }
    }
}
