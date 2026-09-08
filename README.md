# uni-share

Herramienta híbrida de compartición de archivos escrita en **Rust (edition 2024, tokio)** con CLI, GUI web local y aplicación de escritorio nativa (Slint):

- **Modo LAN** — descubrimiento automático por mDNS, transferencia directa cifrada con **TLS 1.3** (certificados auto-firmados generados al vuelo y *pinning* de huella), aceptación explícita del receptor con previsualización, verificación **BLAKE3** por archivo con reintento del archivo fallido, reanudación por offset, límite de velocidad, PIN opcional.
- **Modo Global** — subida a **storage.to** (anónimo, hasta 25 GB, multipart paralelo, carpetas como *collection* preservando la jerarquía); genera link, **QR** y **ticket `.unishare`**, y copia el link al portapapeles. El emisor puede apagarse. (Backend **Smash** presente pero experimental/aparcado.)
- **Tickets `.unishare`** — formato propio de fichero que empaqueta todo lo necesario para descargar (fuentes con contraseña embebida, lista de archivos con tamaño y BLAKE3, caducidad, mensaje). Se comparte como fichero, como URI `unishare:…` o como **QR**; `uni-share download fotos.unishare` verifica cada archivo tras la descarga.
- **Descargas** — desde storage.to (archivos y colecciones, contraseña, reanudación `Range`), **SwissTransfer** (port nativo en Rust de `swisstransfer-dl`, con `--python` para usar el script original) y tickets.
- **Daemon** en segundo plano, **historial SQLite**, **notificaciones** nativas, **GUI web** (`uni-share gui`) y **app nativa** (`uni-share app`, Slint, feature `slint`).

Estado: ver [`TODO.md`](TODO.md). Fases 1-7 implementadas y probadas en vivo (LAN loopback, storage.to, tickets, GUI web); fase 7b (app Slint) en desarrollo.

## Instalación

Requisitos: Rust ≥ 1.85 (edition 2024). En Linux, `libdbus` para notificaciones es opcional (`notify-rust` usa D-Bus puro Rust).

```bash
git clone https://github.com/correo415415/uni-share && cd uni-share
cargo build --release          # un solo comando; sin pasos extra
./target/release/uni-share --help
```

`cargo test` ejecuta 43 tests (unitarios + integración LAN real sobre loopback con TLS).

## Uso

```bash
# Dispositivos en la LAN
uni-share list-devices [--timeout 3] [--json]

# Recibir (pregunta aceptar/rechazar mostrando el árbol de archivos)
uni-share receive [-o DIR] [-p PUERTO] [--auto-accept] [--pin 1234] [--force] [--once]
uni-share receive --qr [--ticket pc-sala.unishare]   # QR de emparejamiento (ip/puerto/huella/PIN)

# Enviar por LAN (interactivo, o --to nombre | ip[:puerto] | ticket de emparejamiento)
uni-share send-lan ~/Videos/proyecto/ [--to PC-Sala] [--pin 1234] [--compress]
uni-share send-lan ~/Videos/proyecto/ --to "unishare:…"   # sin mDNS: huella fijada y PIN incluidos

# Subir a storage.to y obtener link + QR + portapapeles (+ ticket .unishare opcional)
uni-share send-global ~/Videos/proyecto/ [--password xxxx] [--expiry-days 7] [--max-downloads 5] \
                      [--compress] [--ticket [fotos.unishare]] [--ticket-qr] [--json]

# Tickets .unishare (fichero propio con las fuentes, contraseña, lista de archivos y hashes)
uni-share ticket create https://storage.to/c/XXXX --password xxxx --name fotos \
                        [--message "…"] [--verify-from ./fotos] [-o fotos.unishare] [--qr]
uni-share ticket show fotos.unishare [--json]         # verifica la firma Ed25519 si la hay
uni-share ticket qr fotos.unishare [--uri-only]
uni-share ticket save "unishare:…" -o fotos.unishare
uni-share ticket sign fotos.unishare [-o firmado.unishare]   # firma con la clave de este equipo
uni-share ticket identity                              # huella/clave pública de firma de este equipo

# QR de cualquier texto/link (terminal o SVG)
uni-share qr https://storage.to/XXXX [--svg qr.svg]

# Descargar (link, colección, SwissTransfer o ticket)
uni-share download https://storage.to/XXXX [-o DIR] [--password xxxx] [--force] [--list]
uni-share download https://storage.to/c/XXXX --password xxxx
uni-share download https://www.swisstransfer.com/dl/<uuid> [--python]
uni-share download fotos.unishare            # o "unishare:…" — verifica BLAKE3 al terminar

# Historial, daemon, GUIs, config
uni-share history [-n 20] [--json] [--clear]
uni-share daemon start|stop|status
uni-share gui [--port 47900] [--no-open]     # GUI web local
uni-share app [fotos.unishare]               # app nativa Slint (cargo build --features slint)
uni-share app --demo --screenshot app.png [--dialog new|share|settings|fs|confirm] [--select ID[:TAB]] [--light]
uni-share associate [--remove] [--status]    # doble clic en .unishare / links unishare: abren la app
uni-share config [--path]
```

Opciones globales: `--config <ruta>`, `-v/-vv` (debug/trace), `-q`, `RUST_LOG=…`.

### Ejemplo real (LAN, loopback)

```
$ uni-share send-lan ./proj --to 192.168.1.45
[LAN] Conectado a PC-Sala — huella 56EC-D8F3-F330-F460
[LAN] Esperando aceptación de PC-Sala… (2 archivo(s), 19.1 MiB)
[LAN] ████████████████████████████ 100%  19.1 MiB / 19.1 MiB  21.3 MiB/s  0:00:00
[LAN] ✅ Verificación BLAKE3 correcta. 2 archivo(s), 19.1 MiB en 0.9s (21.3 MiB/s)
```

```
$ uni-share receive
[LAN] Escuchando en 0.0.0.0:47820 como PC-Sala
[LAN] Huella TLS: 56EC-D8F3-F330-F460   Destino: /home/user/Downloads
[LAN] Solicitud entrante de Laptop-Maria (192.168.1.78): proj — 2 archivo(s), 19.1 MiB
  proj/readme.txt  (5 B)
  proj/sub/video.bin  (19.1 MiB)
[LAN] ¿Aceptar? [Y/n]
[LAN] ✅ Verificación BLAKE3 correcta. 2 archivo(s) guardados en /home/user/Downloads/proj
```

### Ejemplo real (global)

```
$ uni-share send-global ./proj --password secreto1
[GLOBAL] Subiendo proj (2 archivo(s), 2.9 MiB) a storage.to…
[GLOBAL] ✅ Subida completada.
[GLOBAL] Link: https://storage.to/c/G7pzkDNFy
[GLOBAL] Expira: 2026-09-14T21:27:28+00:00
[GLOBAL] Protegido con contraseña
[GLOBAL] QR: (código QR en terminal)
[GLOBAL] Link copiado al portapapeles.
```

## Configuración

Resolución: `--config` / `UNI_SHARE_CONFIG` → `./config.toml` (si existe, modo portable) → `~/.config/fileshare/config.toml` (Linux), `%APPDATA%\fileshare\config.toml` (Windows), `~/Library/Application Support/fileshare/config.toml` (macOS). Se crea automáticamente. `UNI_SHARE_HOME` cambia el directorio de datos (config, `history.sqlite3`, identidad TLS, pid/log del daemon).

```toml
device_name = "PC-Sala"
download_dir = "/home/user/Downloads"
lan_port = 47820
rate_limit_mbps = 0          # 0 = sin límite
auto_accept = false
# pin = "1234"               # 4-6 dígitos
notifications = true
compress_folders = false     # true → carpetas como .tar.zst
sign_tickets = true          # firmar los tickets con la clave Ed25519 de este equipo (signing.key)

[global]
backend = "storage_to"       # o "smash"
storage_to_api = "https://storage.to/api"
# storage_to_token = "…"     # cuenta storage.to (opcional)
# storage_to_visitor_token = "…"  # se genera y guarda automáticamente
# smash_api_key = "eyJ…"     # necesario para --backend smash
smash_region = "eu-west-3"
expiry_days = 7
parallel_parts = 4
```

## Arquitectura

```
src/
├── main.rs / commands*.rs   CLI (clap) — glue fino sobre la librería
├── lib.rs                   crate `uni_share`
├── config.rs                TOML, rutas por plataforma, visitor token
├── hash.rs                  BLAKE3 streaming (async + blocking)
├── fsutil.rs                walk, safe_join (anti path-traversal), nombres únicos, tar.zst
├── history.rs               SQLite (WAL)
├── ui.rs                    indicatif, console, QR, portapapeles, notificaciones
├── lan/
│   ├── discovery.rs         mDNS/DNS-SD `_unishare._tcp.local.` (TXT: name, fp, ver, pin)
│   ├── tls.rs               identidad rcgen + verificador con pinning (rustls)
│   ├── protocol.rs          manifiesto / offer / upload result (JSON)
│   ├── server.rs            receptor axum HTTPS (ofertas, uploads reanudables, hash)
│   └── client.rs            emisor (offer/poll, streaming, reintento por archivo)
├── global/
│   ├── storage_to.rs        REST client (init/parts/complete/confirm/collection/password…)
│   ├── smash.rs             API Smash (transfer/file/parts/lock)
│   └── upload.rs            orquestación single/multipart/collection + Smash
├── download/
│   ├── http.rs              descarga reanudable (.part + Range) con reintentos
│   ├── storage_to.rs        parser de la página (turbo-stream) + endpoints de descarga
│   ├── swisstransfer.rs     port nativo del flujo Inertia
│   └── ticket.rs            descarga desde ticket (fuentes en orden, verificación BLAKE3)
├── ticket.rs                formato .unishare (contenedor binario / URI / JSON, QR compacto)
├── engine.rs                motor headless compartido por las GUIs (jobs, ofertas, dispositivos)
├── daemon.rs                start/stop/status (pidfile, proceso desacoplado)
├── gui.rs + gui/{index.html,app.css,app.js}   GUI web local (axum sobre el engine, SPA embebida)
└── native.rs                app de escritorio Slint (feature `slint`): puente UI ↔ engine
ui/{theme,widgets,app}.slint app nativa: sistema de diseño propio + ventana principal + diálogos
python/swisstransfer_dl.py   script original (fallback `download --python`)
tests/lan_integration.rs     E2E LAN: TLS pinned, carpeta, duplicados, PIN, rechazo, hash mismatch
```

### Protocolo LAN

```
POST /api/v1/offer {Manifest}         → 202 {transfer_id, status: pending}
GET  /api/v1/offer/{id}               → pending | accepted | rejected{reason}
GET  /api/v1/transfer/{id}/file/{i}   → {received, complete}        (offset para reanudar)
PUT  /api/v1/transfer/{id}/file/{i}?offset=N  <bytes>  (X-Unishare-Blake3)
       → 200 {result: ok, blake3} | 409 {result: hash_mismatch|size_mismatch}
POST /api/v1/transfer/{id}/complete   → 200
```

El receptor escribe en `archivo.part`, hashea mientras escribe y renombra solo si el BLAKE3 coincide; si no, borra el `.part` y el emisor reintenta **ese** archivo (máx. 3). Duplicados → `nombre (1).ext` (o `--force`).

## Decisiones de diseño (y por qué)

| Decisión | Elección | Justificación / trade-offs |
|---|---|---|
| **Descubrimiento LAN** | **mDNS/DNS-SD** con `mdns-sd` | Es el estándar que ya usan Bonjour/Avahi/Windows; `mdns-sd` es Rust puro (sin depender del daemon del sistema) y funciona en Linux/Windows/macOS. Los TXT records transportan nombre, puerto, versión, huella TLS y si requiere PIN — algo que con broadcast UDP tendríamos que reinventar; SSDP está orientado a UPnP y es más verboso. *Trade-off*: algunas redes corporativas filtran multicast → se ofrece `--to ip[:puerto]`. |
| **Transporte LAN** | **HTTP/2 sobre TLS 1.3** (`axum` + `hyper`, `axum-server`) | Streaming nativo de cuerpos grandes (>10 GB sin cargar en memoria), reanudación trivial con offset/`Range`, multiplexación, depurable con `curl`, y el mismo stack sirve para la GUI. QUIC (`quinn`) aporta poco en LAN (sin pérdida ni handover) y complica firewalls (UDP); TCP crudo obliga a diseñar framing, control de flujo y reanudación a mano. |
| **Cifrado** | **TLS 1.3 auto-firmado** (`rcgen`) + **pinning** del SHA-256 del certificado anunciado por mDNS (`rustls` con verificador propio) | Sin CA ni configuración; la huella publicada en mDNS y mostrada en ambas terminales evita MITM. `ring` como proveedor criptográfico (sin `cmake`/C++, compila rápido en cualquier plataforma). Noise habría exigido implementar el transporte completo y no reutiliza HTTP. |
| **Integridad** | **BLAKE3** | 5-10× más rápido que SHA-256, paralelo (`rayon`), seguro. Hash incremental mientras se escribe → sin segunda pasada de lectura. |
| **UI** | **CLI** (`clap` + `indicatif` + `console` + `dialoguer`), **GUI web local** (axum + SPA embebida) y **app nativa Slint** (opcional, feature `slint`) — las tres sobre el mismo **engine** headless | La CLI da barras/prompts; la GUI web funciona en cualquier plataforma sin toolkits; Slint aporta una app de escritorio real (ventana nativa, diálogos de ficheros, bandeja) con un sistema de diseño propio (sin widgets de stock), renderizado por GPU y binario pequeño frente a Electron/Tauri. El engine centraliza jobs, ofertas, descubrimiento y config para que las GUIs sean capas finas. |
| **Formato `.unishare`** | Contenedor binario propio: `UNISHARE` + versión + compresión (zstd) + JSON + BLAKE3; también como URI `unishare:<base64url>` | Un solo artefacto contiene fuentes (con contraseña), lista de archivos con tamaños y hashes, caducidad y mensaje → el receptor descarga y **verifica** sin más datos. La variante URI compacta (sin hashes/lista si hace falta) cabe en un QR. JSON dentro para extensibilidad; el hash final detecta corrupción/truncado. |
| **Historial** | **SQLite** (`rusqlite` *bundled*, WAL) | Escrituras atómicas y lectores concurrentes (daemon + CLI + GUI a la vez), consultas indexadas, sin dependencias del sistema. Un JSON se corrompe con escrituras concurrentes y no escala. |
| **Visitor token storage.to** | 32 bytes aleatorios hex, persistido en `config.toml` (`global.storage_to_visitor_token`); *owner tokens* guardados en el historial (`meta`) | Mismo esquema que el CLI oficial de storage.to; el owner token permite borrar / proteger / cambiar expiración después aunque cambie la IP. |
| **Carpetas en global** | **Collection** con rutas relativas en `filename` (por defecto); `--compress` → un solo `.tar.zst` | Probado: storage.to acepta `sub/dir/a.txt` como nombre y la descarga recrea la jerarquía. Comprimir es opcional (útil para miles de archivos pequeños). |
| **Backend Smash** | **Aparcado** (experimental): opcional, con API key (`Bearer`) en host regional `transfer.<region>.fromsmash.co` | Smash no tiene subida anónima por API; el flujo create transfer → file → PUT parts S3 → lock está implementado pero no se prioriza (storage.to cubre el caso anónimo). |
| **Descarga storage.to** | Parser del *turbo-stream* de React Router de la página (`mint_proof`) + `GET /{id}/download` / `POST /c/{id}/urls`; **fallback a `curl`** si Cloudflare desafía al cliente rustls | storage.to no publica API de descarga; los endpoints del sitio están tras Cloudflare Bot Management que discrimina por huella TLS. `curl` (presente en Linux, macOS y Windows 10+) pasa el filtro; el CDN final acepta `Range` y se descarga con reqwest. |
| **SwissTransfer** | Port nativo en Rust del script Python (solo descarga) + `--python` como fallback | Cumple la restricción de no automatizar subidas a SwissTransfer; el script original se conserva en `python/`. |
| **Errores / async** | `anyhow` + `thiserror`, `tokio` en todo el I/O, sin `unwrap` en rutas de producción | `tokio` es el runtime con más ecosistema (axum, reqwest, hyper). Los `unwrap` quedan solo en tests. |
| **Daemon** | Proceso desacoplado (`setsid` / `DETACHED_PROCESS`) con pidfile y log | Portátil sin integrar con systemd/launchd/servicios de Windows; `daemon status` comprueba liveness real. |

## Seguridad

- TLS 1.3 obligatorio en LAN; el emisor fija la huella del receptor obtenida por mDNS (con `--to ip` se muestra la huella para verificación manual).
- El receptor **siempre** decide (prompt con árbol de archivos, GUI o `--auto-accept` explícito); PIN opcional de 4-6 dígitos (`--pin`).
- Rutas recibidas saneadas (`safe_join`): sin `..`, sin raíces ni letras de unidad, sin caracteres inválidos en Windows.
- Claves/tokens solo en `config.toml` (excluido del repo por `.gitignore`); la clave privada TLS y la de firma se guardan con permisos `0600`.
- **Tickets de emparejamiento LAN** (`receive --qr`): llevan IP, puerto, huella TLS y PIN del receptor; quien lo escanea conecta con la huella fijada desde la primera conexión (sin mDNS). Equivale a compartir el PIN: no publicarlo.
- **Firma Ed25519 de tickets** (opcional, activa por defecto): el ticket incluye la clave pública del emisor y una firma sobre su contenido. `ticket show`, `download`, `send-lan` y las GUIs muestran «firma válida · huella» o rechazan el ticket si la firma no cuadra (manipulado/falsificado). La huella del firmante (`ticket identity`) se puede comparar una vez por otro canal, como una host key de SSH. La versión compacta para QR va sin firma.
- **Reanudación entre ejecuciones**: el receptor guarda registros de transferencias interrumpidas (`transfers/*.json`, 30 días) y, si el mismo emisor vuelve a ofrecer el mismo contenido, reutiliza destino, `.part` y hashes verificados.

## Licencia

MIT
