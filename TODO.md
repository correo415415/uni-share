# uni-share — TODO / Roadmap

Leyenda: `[x]` hecho · `[~]` en progreso · `[ ]` pendiente

## Fase 0 — Investigación y decisiones
- [x] API de **storage.to** (docs + CLI Go oficial + pruebas en vivo): `POST /api/upload/init` → `PUT` a R2 (single <50 MB, multipart 32 MiB/parte) → `POST /api/upload/confirm`; collections (`/api/collection`) preservan rutas relativas en `filename`; descarga vía `GET /{id}/download` con cabecera `x-mint-proof` (extraída de la página) y `POST /c/{id}/urls` para colecciones; el CDN soporta `Range`.
- [x] **Smash** (`@smash-sdk/transfer`): requiere API key (Bearer) en `https://transfer.<region>.fromsmash.co` → backend opcional.
- [x] `swisstransfer-dl` (Python): flujo Inertia (`data-page`), `POST /dl/{id}` con password, `/api/1/links/{link}/files/{file}` → URL S3 pre-firmada.
- [x] Decisiones técnicas documentadas en `README.md`.

## Fase 1 — Esqueleto y núcleo
- [x] `Cargo.toml` (edition 2024, tokio, clap, axum, reqwest/rustls, rcgen, mdns-sd, blake3, rusqlite, indicatif…)
- [x] `config.toml` (TOML) con creación automática (`./config.toml` en dev, `~/.config/fileshare/config.toml` en prod)
- [x] Logging `tracing` (`RUST_LOG`, `-v/-vv`)
- [x] Hash BLAKE3 en streaming + tests
- [x] Utilidades FS: walk recursivo, saneado anti path-traversal, nombres únicos `archivo (1).ext`, `--force`
- [x] Historial SQLite + comando `history`

## Fase 2 — LAN
- [x] Descubrimiento mDNS/DNS-SD (`_unishare._tcp.local.`) con TXT: nombre, versión, fingerprint TLS
- [x] `list-devices`
- [x] TLS 1.3 auto-firmado (`rcgen`) + pin del fingerprint anunciado por mDNS
- [x] Protocolo HTTP/2: `POST /offer` → aceptar/rechazar → `PUT /upload/{id}/{idx}` streaming con offset (reanudación) → `POST /complete`
- [x] Verificación BLAKE3 por archivo; reintento solo del archivo fallido
- [x] Progreso (bytes, velocidad, ETA)
- [x] `receive` (aceptar s/n, `--auto-accept`, `--pin`)
- [x] `send-lan <path>` (selección interactiva o `--to`)
- [x] Notificaciones nativas
- [x] Límite de velocidad
- [x] Carpetas preservando jerarquía; `--compress` opcional (tar.zst)

## Fase 3 — Global
- [x] Visitor token persistido + owner tokens en historial
- [x] Upload single y multipart (paralelo, reintentos, abort)
- [x] Carpetas → collection (o `--compress`)
- [x] Link + QR + portapapeles + `--password` + `--expiry-days` + `--max-downloads`
- [x] Backend Smash (`--backend smash`)

## Fase 4 — Descargas
- [x] `download` storage.to (archivo y colección) con reanudación `Range`
- [x] `download` SwissTransfer (port Rust) + fallback Python
- [x] Duplicados y `--force`

## Fase 5 — Daemon, GUI, tests
- [x] `daemon start|stop|status`
- [x] GUI web local (`uni-share gui`)
- [x] Tests: hash, config, fs, protocolo, integración LAN loopback
- [x] README final

## Fase 6 — Formato `.unishare` (ticket) y QR
- [x] Módulo `ticket`: contenedor binario (`UNISHARE` magic + versión + zstd + BLAKE3 del payload), JSON interno con fuentes múltiples (storage.to / SwissTransfer / HTTP), contraseña embebida, lista de archivos con tamaños y digests BLAKE3, remitente, mensaje, caducidad. Tests (roundtrip, corrupción, URI, compactación para QR).
- [x] Forma URI `unishare:<base64url>` (para chats/QR) + versión compacta que cabe en un QR.
- [x] `download` acepta ticket (fichero o URI): prueba las fuentes en orden y verifica BLAKE3 al terminar.
- [x] `ticket create|show|qr|save`; `send-global --ticket [FILE]` y `--ticket-qr`.
- [x] Comando `qr <texto|url|ticket>` (terminal y `--svg`).
- [ ] Asociación de extensión `.unishare` (doble clic abre la GUI) en Linux (`.desktop` + MIME) y Windows (registro).
- [ ] Ticket para transferencias LAN (`send-lan --ticket`: ip/puerto/fingerprint/PIN en QR para emparejar sin descubrimiento).
- [ ] Firma opcional del ticket (Ed25519) para verificar remitente.

## Fase 7 — GUI de escritorio profesional (estilo qBittorrent, identidad propia)
- [ ] Backend GUI: jobs con velocidad/ETA/archivo actual/lista de archivos, pausar/cancelar, reintentar, eliminar; SSE o WebSocket en lugar de polling.
- [ ] Backend GUI: explorador de ficheros del sistema (`/api/fs`), ajustes editables (`/api/config` GET/PUT), QR SVG (`/api/qr`), tickets (`/api/ticket/create|parse|download`), abrir carpeta de destino.
- [ ] Frontend: layout tipo qBittorrent — barra de herramientas, panel lateral con filtros (Todas / Recibiendo / Enviando / Subidas / Descargas / Completadas / Fallidas / Dispositivos LAN / Tickets), tabla central ordenable con barras de progreso, panel inferior de detalles con pestañas (General / Archivos / Fuentes / Log), barra de estado con velocidades globales.
- [ ] Frontend: estilo único (tema oscuro/claro, tipografía y paleta propias, iconografía SVG inline), atajos de teclado, arrastrar y soltar rutas/tickets, notificaciones en la app.
- [ ] Frontend: diálogo "Nueva transferencia" (LAN / Global / Descarga / Ticket) con vista previa de árbol de archivos; diálogo "Compartir" con link, QR grande, copiar, guardar `.unishare`.
- [ ] Frontend: aceptar/rechazar ofertas LAN entrantes con previsualización del árbol y elección de carpeta destino.
- [ ] Ventana nativa (webview vía `tao`/`wry` o `tauri`) opcional en vez de abrir el navegador.

## Fase 8 — Smash (aparcado)
- [ ] Smash queda como backend **experimental**: oculto de la ayuda por defecto, sin más desarrollo hasta nueva orden. La API key nunca se versiona.

## Pendiente / mejoras conocidas
- [ ] Cloudflare puede exigir captcha (Turnstile) en descargas de storage.to según reputación de IP: entonces se muestra un mensaje pidiendo abrir el link en el navegador
- [ ] Descarga de links Smash (requiere token de destinatario del flujo web) — se indica abrir en navegador
- [ ] Reanudación LAN entre ejecuciones distintas del receptor (ahora reanuda dentro de la misma sesión/transfer id)

## Backlog
- [ ] TUI `ratatui`
- [ ] Empaquetado (cargo-dist, .deb, .msi, Homebrew)
