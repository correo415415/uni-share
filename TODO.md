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

## Fase 7 — GUI (web, portable) sobre un *engine* compartido
- [x] `engine.rs`: núcleo sin UI compartido por todos los front-ends — receptor LAN embebido + anuncio mDNS, ofertas pendientes (aceptar con carpeta destino / rechazar), jobs (envío LAN, recepción LAN, subida link, descarga) con progreso por bytes, velocidad, ETA, archivo actual, lista de archivos, log por job, cancelación; descubrimiento periódico; config editable en caliente y persistida; creación/parseo de tickets; listado de directorios para selectores.
- [x] GUI web (`uni-share gui`) reescrita como capa fina sobre el engine: layout tipo qBittorrent (barra de herramientas, panel lateral con filtros y dispositivos LAN, tabla ordenable con barras de progreso, panel de detalles con pestañas General/Archivos/Compartir/Registro/Historial, barra de estado con velocidades ↓↑), diálogo "Nueva transferencia" (LAN / link / descarga / ticket) con vista previa, explorador de archivos propio, diálogo "Compartir" con QR grande (link y ticket) + guardar `.unishare`/SVG, ajustes editables, menú contextual, atajos de teclado, tema claro/oscuro, arrastrar y soltar `.unishare`.
- [ ] SSE en lugar de polling para `/api/state`.
- [ ] Reintentar job fallido desde la UI.

## Fase 7b — GUI nativa de escritorio con **Slint** (`uni-share app`)
Decisión: Slint (Rust puro, renderizado propio con `winit`+`femtovg`/`skia`, sin webview ni Electron; binario único; estilo 100 % definido por nosotros, no por el toolkit). Comparte el `engine` con la GUI web, así que ambas se comportan igual y la lógica no se duplica.
- [ ] Feature de cargo `slint` (opcional, para que `cargo build` siga funcionando sin dependencias gráficas) y subcomando `uni-share app`.
- [ ] Sistema de diseño propio en `.slint`: paleta (grafito + acento teal/violeta), tipografía, radios, sombras, iconografía vectorial propia (Path), sin controles del estilo por defecto (fluent/material): botones, inputs, checkbox, segmented control, tabla, barra de progreso, badges, pestañas, tooltips, menú contextual, diálogos y toasts propios.
- [ ] Ventana principal: barra de herramientas (Nueva / Enviar LAN / Compartir link / Descargar / Ticket / Cancelar / Limpiar / buscar / ajustes), panel lateral (filtros con contadores + dispositivos LAN en vivo + tarjeta "este equipo"), tabla central ordenable y redimensionable con progreso/velocidad/ETA, panel inferior de detalles con pestañas (General / Archivos / Compartir con QR / Registro / Historial), barra de estado con velocidades globales.
- [ ] Ofertas LAN entrantes: banner con árbol de archivos, huella del emisor, aceptar (elegir carpeta) / rechazar; notificación del sistema.
- [ ] Diálogos: Nueva transferencia (4 modos) con selector de archivos nativo (`rfd`) y vista previa; Compartir (QR renderizado en la propia ventana desde `qrcode`, copiar, guardar `.unishare`, guardar QR); Ajustes; confirmaciones.
- [ ] Puente engine↔UI: tokio en hilo aparte, snapshot cada 500 ms → `VecModel` de Slint vía `invoke_from_event_loop`; acciones de la UI → canal mpsc hacia el engine.
- [ ] Arrastrar y soltar (rutas y `.unishare`), atajos de teclado, tema claro/oscuro, bandeja del sistema (`tray-icon`) con "minimizar a bandeja" y menú rápido.
- [ ] Asociación de `.unishare` y `unishare:` al binario (Linux `.desktop` + MIME, Windows registro, macOS Info.plist) → abre la GUI con el ticket cargado.
- [ ] Empaquetado: AppImage/.deb, .msi, .dmg (cargo-dist / cargo-bundle); iconos de la app.

## Fase 8 — Smash (aparcado)
- [ ] Smash queda como backend **experimental**: oculto de la ayuda por defecto, sin más desarrollo hasta nueva orden. La API key nunca se versiona.

## Pendiente / mejoras conocidas
- [ ] Cloudflare puede exigir captcha (Turnstile) en descargas de storage.to según reputación de IP: entonces se muestra un mensaje pidiendo abrir el link en el navegador
- [ ] Descarga de links Smash (requiere token de destinatario del flujo web) — se indica abrir en navegador
- [ ] Reanudación LAN entre ejecuciones distintas del receptor (ahora reanuda dentro de la misma sesión/transfer id)

## Backlog
- [ ] TUI `ratatui`
- [ ] Empaquetado (cargo-dist, .deb, .msi, Homebrew)
