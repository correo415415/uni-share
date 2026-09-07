# uni-share — TODO / Roadmap

Leyenda: `[x]` hecho · `[~]` en progreso · `[ ]` pendiente

## Fase 0 — Investigación y decisiones
- [x] API de **storage.to** (docs + CLI Go oficial + pruebas en vivo): `POST /api/upload/init` → `PUT` a R2 (single <50 MB, multipart 32 MiB/parte) → `POST /api/upload/confirm`; collections (`/api/collection`) preservan rutas relativas en `filename`; descarga vía `GET /{id}/download` con cabecera `x-mint-proof` (extraída de la página) y `POST /c/{id}/urls` para colecciones; el CDN soporta `Range`.
- [x] **Smash** (`@smash-sdk/transfer`): requiere API key (Bearer) en `https://transfer.<region>.fromsmash.co` → backend opcional.
- [x] `swisstransfer-dl` (Python): flujo Inertia (`data-page`), `POST /dl/{id}` con password, `/api/1/links/{link}/files/{file}` → URL S3 pre-firmada.
- [x] Decisiones técnicas documentadas en `README.md`.

## Fase 1 — Esqueleto y núcleo
- [~] `Cargo.toml` (edition 2024, tokio, clap, axum, reqwest/rustls, rcgen, mdns-sd, blake3, rusqlite, indicatif…)
- [ ] `config.toml` (TOML) con creación automática (`./config.toml` en dev, `~/.config/fileshare/config.toml` en prod)
- [ ] Logging `tracing` (`RUST_LOG`, `-v/-vv`)
- [ ] Hash BLAKE3 en streaming + tests
- [ ] Utilidades FS: walk recursivo, saneado anti path-traversal, nombres únicos `archivo (1).ext`, `--force`
- [ ] Historial SQLite + comando `history`

## Fase 2 — LAN
- [ ] Descubrimiento mDNS/DNS-SD (`_unishare._tcp.local.`) con TXT: nombre, versión, fingerprint TLS
- [ ] `list-devices`
- [ ] TLS 1.3 auto-firmado (`rcgen`) + pin del fingerprint anunciado por mDNS
- [ ] Protocolo HTTP/2: `POST /offer` → aceptar/rechazar → `PUT /upload/{id}/{idx}` streaming con offset (reanudación) → `POST /complete`
- [ ] Verificación BLAKE3 por archivo; reintento solo del archivo fallido
- [ ] Progreso (bytes, velocidad, ETA)
- [ ] `receive` (aceptar s/n, `--auto-accept`, `--pin`)
- [ ] `send-lan <path>` (selección interactiva o `--to`)
- [ ] Notificaciones nativas
- [ ] Límite de velocidad
- [ ] Carpetas preservando jerarquía; `--compress` opcional (tar.zst)

## Fase 3 — Global
- [ ] Visitor token persistido + owner tokens en historial
- [ ] Upload single y multipart (paralelo, reintentos, abort)
- [ ] Carpetas → collection (o `--compress`)
- [ ] Link + QR + portapapeles + `--password` + `--expiry-days` + `--max-downloads`
- [ ] Backend Smash (`--backend smash`)

## Fase 4 — Descargas
- [ ] `download` storage.to (archivo y colección) con reanudación `Range`
- [ ] `download` SwissTransfer (port Rust) + fallback Python
- [ ] Duplicados y `--force`

## Fase 5 — Daemon, GUI, tests
- [ ] `daemon start|stop|status`
- [ ] GUI web local (`uni-share gui`)
- [ ] Tests: hash, config, fs, protocolo, integración LAN loopback
- [ ] README final

## Backlog
- [ ] TUI `ratatui`
- [ ] Empaquetado (cargo-dist, .deb, .msi, Homebrew)
