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
- [x] Asociación de extensión `.unishare` y esquema `unishare:` (doble clic abre la GUI): `uni-share associate [--remove|--status|--exe]` — Linux XDG (MIME XML con glob+magic, `.desktop`, iconos, `xdg-mime default`), Windows HKCU (`reg.exe`); la app acepta `file://` y previsualiza el ticket al abrir.
- [x] Ticket de emparejamiento LAN (`Source::Lan{host,port,fingerprint,pin,name}`): `receive --qr [--ticket FILE]` imprime QR/URI; `send-lan --to <ticket>` y el campo "dirección manual" de las GUIs lo aceptan (huella TLS fijada desde la primera conexión, PIN prellenado, sin mDNS). Botón "QR de emparejamiento" en la tarjeta del equipo (web y nativa). Los caminos de descarga rechazan tickets de emparejamiento con pista. Test E2E.
- [x] Firma opcional Ed25519 (`ring`, sin crates nuevos): clave persistente `signing.key`, `Ticket.signer/signature` sobre el JSON canónico, `verify_signature()` → sin firma / válida (huella del firmante) / **inválida**; `sign_tickets` en config (por defecto sí), `ticket create --sign/--no-sign`, `send-global --no-sign`, `ticket sign`, `ticket identity`, `ticket show` verifica; `download`/`send-lan`/engine rechazan firmas inválidas; GUIs muestran huella de firma, ajuste y estado en la previsualización. La forma compacta para QR va sin firma (el `.unishare` completo la conserva).

## Fase 7 — GUI (web, portable) sobre un *engine* compartido
- [x] `engine.rs`: núcleo sin UI compartido por todos los front-ends — receptor LAN embebido + anuncio mDNS, ofertas pendientes (aceptar con carpeta destino / rechazar), jobs (envío LAN, recepción LAN, subida link, descarga) con progreso por bytes, velocidad, ETA, archivo actual, lista de archivos, log por job, cancelación; descubrimiento periódico; config editable en caliente y persistida; creación/parseo de tickets; listado de directorios para selectores.
- [x] GUI web (`uni-share gui`) reescrita como capa fina sobre el engine: layout tipo qBittorrent (barra de herramientas, panel lateral con filtros y dispositivos LAN, tabla ordenable con barras de progreso, panel de detalles con pestañas General/Archivos/Compartir/Registro/Historial, barra de estado con velocidades ↓↑), diálogo "Nueva transferencia" (LAN / link / descarga / ticket) con vista previa, explorador de archivos propio, diálogo "Compartir" con QR grande (link y ticket) + guardar `.unishare`/SVG, ajustes editables, menú contextual, atajos de teclado, tema claro/oscuro, arrastrar y soltar `.unishare`.
- [x] GUI web con el mismo sistema de diseño «Graphite» que la GUI Slint: tokens idénticos (superficies, acento azul único, ámbar saliente, semánticos desaturados, radios 4/5/8, toolbar 46 / fila 32 / control 30, escala tipográfica), botones planos, badges rectangulares, barras de progreso 6 px, barra lateral con marcador de 2 px, contadores mono, diálogos atenuados sin blur ni gradientes, glifo de marca.
- [x] SSE: `GET /api/events` (`text/event-stream`, evento `state` con el mismo JSON que `/api/state`) emitido en cada cambio del engine (`Engine::touch`/`changed` con `tokio::sync::Notify`, coalescido 60 ms) y cada 700 ms mientras hay transferencias activas; keep-alive 15 s. La web usa `EventSource` y solo vuelve al polling si el stream cae; la GUI nativa también se despierta con `changed()`.
- [x] Reintentar job fallido/cancelado desde la UI: el engine guarda la petición original (`Job.origin`), `Engine::retry_job` relanza (descargas con `force` para reanudar) y quita la entrada vieja; `POST /api/jobs/{id}/retry`; web: menú contextual «Reintentar» + enlace ↻ en Detalles; nativa: botón «Reintentar» junto al estado. Campo `retryable` en el snapshot.

## Fase 7b — GUI nativa de escritorio con **Slint** (`uni-share app`)
Decisión: Slint (Rust puro, renderizado propio con `winit`+`femtovg`/`skia`, sin webview ni Electron; binario único; estilo 100 % definido por nosotros, no por el toolkit). Comparte el `engine` con la GUI web, así que ambas se comportan igual y la lógica no se duplica.
- [x] Feature de cargo `slint` (opcional, para que `cargo build` siga funcionando sin dependencias gráficas) y subcomando `uni-share app`.
- [x] Sistema de diseño propio en `.slint`: paleta (grafito + acento teal/violeta), tipografía, radios, sombras, iconografía vectorial propia (Path), sin controles del estilo por defecto (fluent/material): botones, inputs, checkbox, segmented control, tabla, barra de progreso, badges, pestañas, tooltips, menú contextual, diálogos y toasts propios.
- [x] Ventana principal: barra de herramientas (Nueva / Enviar LAN / Compartir link / Descargar / Ticket / Cancelar / Limpiar / buscar / ajustes), panel lateral (filtros con contadores + dispositivos LAN en vivo + tarjeta "este equipo"), tabla central ordenable y redimensionable con progreso/velocidad/ETA, panel inferior de detalles con pestañas (General / Archivos / Compartir con QR / Registro / Historial), barra de estado con velocidades globales.
- [x] Ofertas LAN entrantes: banner con árbol de archivos, huella del emisor, aceptar (elegir carpeta) / rechazar; notificación del sistema.
- [x] Diálogos: Nueva transferencia (4 modos) con selector de archivos nativo (`rfd`) y vista previa; Compartir (QR renderizado en la propia ventana desde `qrcode`, copiar, guardar `.unishare`, guardar QR); Ajustes; confirmaciones.
- [x] Puente engine↔UI: tokio en hilo aparte, snapshot cada 500 ms → `VecModel` de Slint vía `invoke_from_event_loop`; acciones de la UI → canal mpsc hacia el engine.
- [x] Arrastrar y soltar nativo: eventos winit `HoveredFile`/`DroppedFile` vía `WinitWindowAccessor` (feature `unstable-winit-030`; Slint 1.13 no expone drops externos). Overlay «Suelta para enviar por LAN» mientras se arrastra; un archivo/carpeta abre «Enviar LAN» con la ruta; varios del mismo directorio → la carpeta; un `.unishare` abre «Descargar» (o el emparejamiento si es ticket LAN). `app --demo --drag-over` + captura `drop` en CI.
- [x] Bandeja del sistema (Slint 1.17 `SystemTrayIcon`, feature `system-tray`): icono + tooltip con transferencias activas, menú Mostrar/Ocultar · Enviar por LAN · Compartir link · Descargar · Abrir ticket · Salir; clic izquierdo alterna la ventana. Ajuste «minimizar a la bandeja al cerrar» (`minimize_to_tray`, por defecto off): la ventana se oculta y el motor sigue recibiendo; sin host StatusNotifierItem (GNOME sin extensión) se cierra normalmente.
- [x] Asociación de `.unishare` y `unishare:` al binario (Linux `.desktop` + MIME, Windows registro) → abre la GUI con el ticket cargado (los de emparejamiento abren «Enviar por LAN»). Pendiente macOS Info.plist (requiere bundle).
- [ ] Empaquetado: AppImage/.deb, .msi, .dmg (cargo-dist / cargo-bundle); iconos de la app.

- [x] Atajos de teclado (N/L/U/D/T/,/Supr/flechas/Esc) y tema claro/oscuro.
- [x] Compilación verificada en CI (self-hosted), sin warnings: errores de `.slint` corregidos (Icon como caja, `tint`, columnas por `horizontal-stretch`), puente `Send` corregido.
- [x] `uni-share app --demo` (snapshot realista sin red) + `--screenshot out.png` + `--dialog new|share|settings|fs|confirm` para revisar el diseño; el CI sube capturas a la release nightly.
- [x] Revisión visual con las capturas del CI (10 vistas: principal, detalles ×4, diálogos ×5): iconos centrados, badges sin recorte, columnas proporcionales, tarjeta del equipo.
- [x] Panic de zbus ("no reactor running") al arrancar con accesibilidad AT-SPI: `rfd` sin la feature `tokio` de zbus; `notify()` en hilo propio.
- [x] Layout principal adaptable al tamaño de la ventana: la columna central declara `horizontal-stretch: 1; min-width: 0` (tabla, Flickables, detalles) y el estado vacío ya no fija su ancho preferido (antes, sin transferencias, la tabla quedaba a ~220 px y el resto de la ventana vacío). `--demo --empty --size WxH` + capturas `empty`/`empty-wide` en CI para vigilarlo.
- [x] Captura del tema claro en CI (`--light`, 3 vistas) y paleta "Graphite" con contrastes revisados (acento único, sin degradados, bordes finos).
- [x] Restyle profesional: tokens en `theme.slint` (radios 4/5/8, toolbar 46, fila 32, control 30), iconos vectoriales centrados (`swap`, `sun`, `gear` redibujado), badges rectangulares, columnas proporcionales.

## Fase 7c — CI/CD en runner local
- [x] Workflow `.github/workflows/build.yml` en `self-hosted`: fmt (aviso) → clippy `-D warnings` → tests → `cargo build --release` (default) → `--features slint` (target-dir separado).
- [x] Sin caché ni artefactos de Actions (sin espacio): el runner conserva `~/.cargo` y `target/`; los binarios van a una **release** rodante `nightly-<rama>` (prerelease, assets sobrescritos, tag movido al HEAD) y a releases normales en tags `v*`.
- [x] Empaqueta `uni-share-<target>`, `uni-share-app-<target>` (con Slint), `uni-share-<ver>-<target>.tar.gz` (binarios + docs) y `SHA256SUMS`.
- [ ] Matriz Windows/macOS cuando haya runners de esas plataformas (el workflow ya detecta `runner.os`/`runner.arch`).

## Fase 8 — Smash (aparcado)
- [ ] Smash queda como backend **experimental**: oculto de la ayuda por defecto, sin más desarrollo hasta nueva orden. La API key nunca se versiona.

## Fase 9 — Android

Objetivo: el mismo engine Rust en el móvil sin reescribir la lógica. Decisión final (frente a la propuesta inicial UniFFI + Compose): **la app Android es una cáscara Kotlin mínima que muestra la GUI web** (`src/gui/*`, ya con el sistema de diseño Graphite y responsive) servida por el propio core en `127.0.0.1:<puerto aleatorio>`. Cero duplicación de UI, misma API `/api/*` que el escritorio, y la integración con el sistema (share sheet, intents, servicio en primer plano) se hace en ~400 líneas de Kotlin.

- [x] **Etapa 1 — el crate compila para Android**: `crate-type = ["rlib","cdylib"]`; `arboard`, `notify-rust`, `rustls-platform-verifier` pasan a `[target.'cfg(not(target_os = "android"))']`; en Android `webpki-roots` para TLS saliente (storage.to) y stubs de portapapeles/notificaciones (`ui.rs`). Scripts `android/build-android.sh|.bat` (cargo-ndk → `jniLibs/{arm64-v8a,armeabi-v7a,x86_64}`, `--apk` debug, `--release` con keystore local autogenerado).
- [x] **Etapa 2 — JNI** (`src/android.rs`, solo `cfg(target_os = "android")`): `dev.unishare.app.Native.{start(dataDir, downloadDir, deviceName) → puerto, stop(), port(), version()}`. `start` fija `UNI_SHARE_HOME=dataDir`, carga/crea `config.toml`, abre el historial SQLite, arranca `Engine` + `gui::router` en un runtime tokio propio y devuelve el puerto; `tracing` → logcat (tag `uni-share`).
- [x] **Etapa 3 — cáscara Kotlin/Gradle** (`android/`): `MainActivity` (WebView sobre la GUI web, splash/error con reintento, back = historial), `EngineService` (foreground `dataSync` con notificación «uni-share activo» y acción Detener), `App` (canal de notificaciones, `filesDir/uni-share`, `getExternalFilesDir(Download)`, nombre del dispositivo), intents `ACTION_SEND`/`SEND_MULTIPLE` (copia a `cache/inbox` → `openNew('lan',{path})`), `ACTION_VIEW` `unishare:` y `.unishare` (→ `openNew('download',{url})`), `network_security_config` (cleartext solo loopback), icono adaptativo + legacy desde `assets/logo-*.png`, tema Graphite, firma release desde `UNISHARE_KEYSTORE_PASS`/`UNISHARE_KEY_ALIAS`, `versionName` leído de `Cargo.toml`.
- [x] Etapa 4 — pulido móvil: `MulticastLock` para mDNS (`EngineService`); puente `window.Android` (`Bridge.kt`, `@JavascriptInterface`) con **carpeta de descargas vía SAF** (`ACTION_OPEN_DOCUMENT_TREE` + `takePersistableUriPermission`; el motor sigue escribiendo en la carpeta privada y, al completarse una recepción/descarga, la página llama a `exportJob(id, saved[], name)` que copia los archivos con `DocumentFile` respetando subcarpetas — `Job.saved` ahora se serializa en el snapshot), **escáner QR** (`zxing-android-embedded` `ScanContract`, permiso `CAMERA` en tiempo de ejecución → `openScanned(texto)`: ticket LAN → «Enviar LAN» con el ticket como destino, ticket/link → «Descargar», ip[:puerto] → destino manual), **compartir nativo** (`shareText`/`shareFile` por `FileProvider` desde el diálogo Compartir) y `androidEvent(kind, payload)` para toasts/carpeta/exportación. Botón «Escanear QR» y fila «Carpeta de descargas (Android)» solo aparecen si existe `window.Android`.
- [x] Etapa 4b — notificación de oferta entrante con acciones **Aceptar/Rechazar** (`OfferWatcher` sondea `/api/state` cada 3 s mientras vive el servicio; canal «Transferencias entrantes» de prioridad alta; `OfferReceiver` llama a `/api/offers/{id}/accept|reject` por loopback y la notificación desaparece cuando la oferta se resuelve, también si se contestó desde la WebView).
- [x] Etapa 4c — permiso `POST_NOTIFICATIONS` explicado: diálogo previo (motor en segundo plano + avisos Aceptar/Rechazar, sin publicidad) con «Permitir / Ahora no»; se pregunta una sola vez.
- [x] **Etapa 6 — GUI móvil propia desde cero** (`src/gui/mobile/{index.html,m.css,m.js}`, servida en `/m` y cargada por el WebView de la app; misma identidad «Graphite»: superficies, acento azul único, ámbar saliente, radios 4/5/8/14, Inter/JetBrains Mono, badges rectangulares, sin degradados). Pensada para pulgar y pantallas 360-430 px, claro/oscuro (sistema + conmutador), *safe areas*, animaciones de 150 ms, `prefers-reduced-motion`.
  - Decisión técnica: **(b) capa web móvil separada** sobre el mismo engine/API (un solo puente Kotlin `window.Android`, sin coste extra de compilación en CI, listas y teclado nativos del WebView). La variante Slint-Android queda descartada mientras `backend-android-activity` no aporte ventajas claras.
  - [x] Apartados (barra inferior de 5 pestañas + FAB «Nueva»): **Inicio** (hero de estado, accesos rápidos Enviar LAN/Recibir/Escanear/Descargar, ofertas entrantes como tarjeta, en curso, recientes), **Transferencias** (buscador, chips de filtro con contadores, filas con progreso, ficha de detalle con badges, progreso grande, acciones, archivos, registro, menú «más»), **Dispositivos** (radar animado, buscar de nuevo, menú por dispositivo: enviar/copiar dirección/copiar huella, emparejar por QR), **Compartir** (QR de emparejamiento + pantalla completa, subir y crear link, ticket desde links, enviar LAN, links recientes con hoja QR/link/ticket), **Ajustes** (interruptores y valores editables con hojas, carpeta SAF en Android, tema; subpantallas Seguridad y análisis, Historial agrupado por día, Acerca de).
  - [x] Flujos: hoja «Nueva transferencia» (segmentos LAN · Link · Descargar · Ticket) con selector de archivos del sistema en Android (`OpenMultipleDocuments` → caché → `androidEvent('picked')`) o navegador de rutas del engine (`/api/fs`) en escritorio, cuadrícula de dispositivos, dirección/ticket manual con botón de escáner, PIN, contraseña/caducidad/mensaje, previsualización de tickets (`/api/ticket/parse`: firma, caducidad, archivos); hojas de confirmación, menú y *prompt*; ofertas con Aceptar/Rechazar (+ vibración); botón *Atrás* de Android = cerrar hoja / volver (sincronía con `history`); reconexión SSE → sondeo; toasts con acción; exportación a SAF al completar recepciones/descargas.
  - [x] Calidad: prueba de humo con jsdom (todas las pantallas, detalle, hojas, selector, envío, aceptar oferta, ajustes, tema) sin errores; `uni-share gui --demo` (estado `Snapshot::demo()` sin motor) + capturas en CI con Chromium headless a 390×844 (12 vistas: pestañas, detalle, hojas Nueva/Descargar/Recibir/Compartir, Seguridad, tema claro; **verificadas** en la release nightly); etiquetas ARIA (`role=switch`, `aria-label`, `aria-live`), objetivos ≥ 48 px, contraste AA de los tokens compartidos.
  - [ ] Pendiente menor: *pull to refresh* en Dispositivos, deslizar filas para cancelar/eliminar, i18n (`es` fijo por ahora), capturas 412×915 → **movido a Fase 12**.
- [x] Etapa 5 — CI: paso «Android» en `build.yml` que solo actúa si el runner tiene SDK+NDK (`ANDROID_HOME`/`ANDROID_NDK_HOME` o `~/Android/Sdk`): instala `cargo-ndk` y los targets, ejecuta `android/build-android.sh --apk` y publica `uni-share-<ver>-android-debug.apk` en la release nightly; si no hay SDK se omite con un aviso. **Verificado en el runner** (NDK 28, AGP 8.7): `libuni_share.so` para arm64-v8a/armeabi-v7a/x86_64 y `uni-share-0.1.0-android-debug.apk` (39 MB) publicada en la release nightly.
- Limitaciones asumidas: sin daemon permanente (recepción solo con la app o el servicio en primer plano); mismo puerto LAN 47820 → misma Wi-Fi sin AP isolation; carpetas como `.tar.zst`; análisis de seguridad solo heurístico (sin ClamAV).

## Fase 10 — Análisis de seguridad local (`src/scan.rs`)
- [x] Heurísticas 100 % locales: *magic bytes* vs. extensión (ejecutables disfrazados = peligro), extensiones peligrosas, nombres engañosos (doble extensión, RTLO, ancho cero, relleno, reservados), tamaño ≠ declarado, ZIP/OOXML/JAR/APK por directorio central (bombas, traversal, anidados, cifrados, `vbaProject.bin`), tar/tar.zst por cabeceras con presupuesto de 8 GiB (traversal, setuid, enlaces fuera), PDF (JS/Launch/embebidos/acciones), OLE (macros/Ole10Native/DDE), SVG/HTML (scripts, iframes, meta refresh), `.desktop`/`.url`, shebangs. Sin crates nuevos.
- [x] ClamAV opcional: detección de `clamdscan` (`--fdpass`) / `clamscan` en PATH y rutas típicas, `--no-summary --infected`, *timeout* 120 s, exit 1 → firma; `clamav_path` explícito; `max_file_mib`.
- [x] Política ante peligro: cuarentena (`*.unishare-quarantine`, 0600, sufijo numerado), solo avisar, eliminar. `Report` con `summary()`/`detail()`/JSON.
- [x] Integración: `[scan]` en config; `Progress.saved` en el servidor LAN; `receive` (CLI) y engine (recepciones y descargas) → registro del job, `Job.scan`, `Notice` → toasts; `download`/ticket (CLI); `uni-share scan` (exit 0/1/2, `--json`, `--no-clamav`, `--quarantine|--delete`); `POST /api/scan`; ajustes «Seguridad» en GUI web y Slint con estado de ClamAV; badge 🛡 en tabla/detalle.
- [x] Tests: sniffing, ejecutables disfrazados y doble extensión, trucos de nombre, ZIP (bomba/traversal/anidado/cifrado/macros/truncado/OOXML), tar + tar.zst (setuid/traversal/enlace/exec), PDF/OLE/SVG/HTML/.desktop, informe + cuarentena/eliminación + JSON, tamaño, ClamAV ausente y falso `clamscan` que reporta EICAR.
- [x] Escaneo bajo demanda: `Engine::rescan_job` (recuerda `Job.saved`; fallback a `dest` + lista de archivos), `POST /api/jobs/{id}/scan` → `{severity, summary, detail, report}`; web: menú contextual «Analizar de nuevo» + enlace ↻ en Detalles; nativa: botón 🛡 junto al estado. Funciona aunque `[scan] enabled = false`.
- [ ] YARA opcional (crate `yara-x`) con reglas del usuario en `<data_dir>/rules/`.
- [x] Recorrer `.tgz`/`.tar.gz` con `flate2` (`rust_backend`, ya estaba en el árbol vía reqwest — sin dependencias nativas nuevas): mismas comprobaciones de traversal/setuid/enlaces/bomba que `.tar`/`.tar.zst`. Test.

## Fase 12 — Incidencias de uso real (Android + PC) y apartado de registro
Reportadas tras probar la app en el teléfono y el escritorio (09-2026). Cada punto lleva el diagnóstico hecho sobre el código.

- [x] **Parpadeos en la app Android.** La GUI móvil re-renderiza toda la página (`page.innerHTML = …`) con cada frame SSE (cada 700 ms con transferencias activas) y cada elemento nuevo lleva `animation: fadein 160ms` (`.page>*`) → destello en cada refresco. Arreglo: quitar la animación de entrada en re-renders (solo animar al cambiar de pestaña/vista), y actualizar *in place* barras/porcentajes/velocidades de las filas existentes (`data-job`) en vez de reconstruir el DOM cuando no cambia el conjunto de trabajos; preservar `scrollTop` y foco. Mismo criterio para la web de escritorio si aplica.
- [x] **«os error 22 (Invalid argument)» al compartir por LAN desde Android.** Candidatos: (1) el `Content-Length` del PUT se calcula con `entry.size` del manifiesto pero el archivo se abrió/copió desde un `content://` a la caché y su tamaño real difiere → el stream se corta y el error se propaga como `io::Error` EINVAL; (2) `seek(offset)` con `offset > len` en reanudación; (3) `tempfile::tempdir()` usa `/tmp` (no existe en Android) al comprimir carpetas → hay que fijar `TMPDIR` a `cacheDir` desde `android.rs`/`App.kt`. Arreglo: fijar `TMPDIR`, recalcular tamaños con `metadata()` justo antes de enviar y devolver un error legible («el archivo cambió de tamaño»), tests con archivo truncado.
- [x] **Android: no se puede elegir la carpeta `Descargas` del dispositivo.** El selector SAF (`ACTION_OPEN_DOCUMENT_TREE`) no permite seleccionar la raíz de `Download` en Android 11+ (restricción del sistema: solo subcarpetas) y la app no explica nada. Arreglo: (a) por defecto, sin pedir nada, guardar en `Descargas/uni-share` con `MediaStore.Downloads` (API 29+, sin permiso) o `Environment.DIRECTORY_DOWNLOADS` + `WRITE_EXTERNAL_STORAGE` (≤ API 28, con `maxSdkVersion="28"`) → los archivos aparecen en la app Archivos/Descargas; (b) mantener el selector SAF como opción para otra carpeta, pre-abierto en `Download` (`EXTRA_INITIAL_URI`) y con aviso «elige o crea una subcarpeta»; (c) mostrar en Ajustes la ruta efectiva.
- [x] **«Quitar transferencias terminadas» no se refleja o tarda.** `Engine::clear_finished` no llama a `touch()` → el SSE no emite hasta el siguiente *keep-alive* (15 s). Arreglo: `touch()` en `clear_finished` (y revisar `cancel_job`, `retry_job`, `reject_offer`, `ack_notices`); la web/móvil aplican además el resultado localmente al instante.
- [x] **Escáner QR: forzar vertical y mejorar la lectura.** `ScanOptions.setOrientationLocked(false)` deja la `CaptureActivity` de zxing en horizontal; arreglo: `setOrientationLocked(true)` + `CaptureActivity` propia en el manifiesto con `screenOrientation="portrait"`, y para la lectura: `setDesiredBarcodeFormats(QR_CODE)` ya está; añadir `setCameraId(0)`, `setBeepEnabled(false)`, tiempo de espera 0 e **inverted scan / TRY_HARDER** (`DecodeHintType.TRY_HARDER`) — los QR de tickets son densos (versión 20-30): además **reducir la densidad**: el ticket de emparejamiento y el QR de tickets deben codificar la forma compacta (`to_uri_compact`) y usar nivel de corrección L; en la GUI mostrar el QR a mayor tamaño (pantalla completa por defecto al «Mostrar QR»).
- [x] **PC (Slint): en «Emparejar con este dispositivo» el texto se sale del diálogo.** El `Input` de solo lectura con el ticket no tiene `min-width: 0px` dentro del `HorizontalLayout` y el `Dialog` toma el ancho del contenido (`dialog-width: 820px` pero el texto empuja). Arreglo: `min-width: 0px; horizontal-stretch: 1` en el Input y en su columna, `TextInput` con `wrap: no-wrap` + recorte (`clip: true`) y ancho del diálogo acotado al de la ventana (`min(820px, root.width - 48px)`); misma revisión en la web (`#sh-cp` con `word-break`).
- [x] **No se pueden ver los resultados del análisis de seguridad de un archivo recibido.** El engine vuelca el informe en `Job.log` y `Job.scan`, pero (web/móvil/Slint) el detalle solo muestra el badge y, si la severidad es *info*, ni siquiera el mensaje. Arreglo: guardar el informe estructurado en el trabajo (`Job.scan_report: Option<ScanSummary{severity, summary, detail, files:[{path, findings}]}>`), sección «Análisis de seguridad» en la ficha de detalle (web, móvil, Slint) con el resumen, el detalle por archivo y botón «Analizar de nuevo»; mostrarlo también en el historial (`Record.meta`).
- [x] **Apartado «Registro» (logs) en PC y Android.** Nuevo `tracing` *layer* en memoria (`src/logbuf.rs`: anillo de 2 000 líneas con nivel, hora, destino y mensaje; también a fichero rotativo `<data_dir>/logs/uni-share.log`, 5 × 2 MiB) + `GET /api/logs?after=<seq>&level=` y `DELETE /api/logs`; GUI web: pestaña/diálogo «Registro» con filtro por nivel, búsqueda, autoscroll, copiar y «Guardar…»; GUI móvil: Ajustes → «Registro» (vista con filtro por nivel, búsqueda, compartir con la hoja del sistema); Slint: diálogo «Registro» con las mismas opciones; Android: además el `logcat` de la cáscara Kotlin (`Log.*`) se enruta al mismo anillo vía JNI (`uni_share_log`).
- [ ] Pendientes menores de la GUI móvil (de la etapa 6): *pull to refresh* en Dispositivos, deslizar filas para cancelar/eliminar, preparación i18n (tabla `T{}` de cadenas en `m.js`), capturas 412×915 en CI.

## Pendiente / mejoras conocidas
- [ ] Cloudflare puede exigir captcha (Turnstile) en descargas de storage.to según reputación de IP: entonces se muestra un mensaje pidiendo abrir el link en el navegador
- [ ] Descarga de links Smash (requiere token de destinatario del flujo web) — se indica abrir en navegador
- [x] Reanudación LAN entre ejecuciones distintas del receptor: registros JSON por transferencia en `<data_dir>/transfers/<clave>.json` (clave = BLAKE3 de huella del emisor + manifiesto); al re-ofrecer se reutilizan destino, `.part` y hashes ya verificados; se purgan a los 30 días. Aviso en CLI/GUI. Test E2E.

## Backlog
- [ ] TUI `ratatui`
- [ ] Empaquetado (cargo-dist, .deb, .msi, Homebrew)
