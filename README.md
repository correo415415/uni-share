# uni-share

Herramienta híbrida de compartición de archivos escrita en **Rust (edition 2024, tokio)**:

- **Modo LAN** — descubrimiento automático por mDNS, transferencia directa cifrada con TLS 1.3, aceptación explícita del receptor, verificación BLAKE3 y reanudación.
- **Modo Global** — subida a **storage.to** (anónimo, hasta 25 GB, multipart) o **Smash** (con API key); genera link, QR en terminal y lo copia al portapapeles. El emisor puede apagarse.
- **Descargas** — desde storage.to (archivos y colecciones) y **SwissTransfer** (port nativo en Rust del script `swisstransfer-dl`, con fallback a Python).

> Estado: en desarrollo activo. Ver [`TODO.md`](TODO.md).

## Instalación

```bash
cargo build --release
./target/release/uni-share --help
```

## Uso rápido

```bash
uni-share list-devices                 # dispositivos en la LAN
uni-share receive                      # escuchar (acepta/rechaza interactivo)
uni-share send-lan ~/Videos/proyecto/  # enviar por LAN
uni-share send-global ~/Videos/proyecto/ --password 1234
uni-share download https://storage.to/XXXX
uni-share download https://www.swisstransfer.com/dl/<uuid>
uni-share history
uni-share daemon start|stop|status
uni-share gui
```

## Configuración

`./config.toml` (si existe) o `~/.config/fileshare/config.toml` — se crea automáticamente. Ver `uni-share config`.

## Decisiones de diseño

Se documentan con detalle en la sección final del README a medida que se implementan las fases (ver `TODO.md`).

| Decisión | Elección | Por qué |
|---|---|---|
| Descubrimiento LAN | **mDNS/DNS-SD** (`mdns-sd`) | Estándar (Bonjour/Avahi), multiplataforma sin daemon externo (implementación pura Rust), permite TXT records (nombre, puerto, fingerprint TLS). Broadcast UDP requiere protocolo propio; SSDP está pensado para UPnP. |
| Transporte LAN | **HTTP/2 sobre TLS 1.3** (`axum` + `hyper`) | Streaming nativo, `Range`/offset trivial para reanudar, multiplexación, depurable con `curl`. QUIC (`quinn`) aporta poco en LAN y complica el firewall; TCP crudo obliga a diseñar framing propio. |
| Cifrado | **TLS 1.3 auto-firmado** (`rcgen` + `rustls`) con *pinning* del fingerprint SHA-256 anunciado por mDNS | Sin CA, sin configuración; el pin evita MITM en la LAN. Noise exigiría implementar el transporte a mano. |
| UI | **CLI + `indicatif`/`console`/`dialoguer`** + GUI web local (`uni-share gui`) | Barras de progreso, prompts y colores; la GUI web (axum + HTML embebido) da una interfaz gráfica multiplataforma sin toolkits nativos. `ratatui` queda en backlog. |
| Historial | **SQLite** (`rusqlite` bundled) | Escrituras atómicas, lectores concurrentes (daemon + CLI), consultas; sin dependencias del sistema. |
| Visitor token storage.to | 32 bytes aleatorios hex, persistido en `config.toml` (`global.storage_to_visitor_token`); *owner tokens* guardados en el historial | Igual que el CLI oficial; permite borrar/proteger subidas después. |
| Hash | **BLAKE3** | 5-10× más rápido que SHA-256, seguro, paralelo (`rayon`). |

## Licencia

MIT
