#!/usr/bin/env python3
"""
swisstransfer_dl.py — Descargador de línea de comandos para SwissTransfer.

Uso:
    python swisstransfer_dl.py https://www.swisstransfer.com/dl/<uuid>
    python swisstransfer_dl.py <enlace> -o ./descargas
    python swisstransfer_dl.py <enlace> -p "contraseña"
    python swisstransfer_dl.py <enlace> --list

Funciona con transferencias de un solo archivo, varios archivos, carpetas
(se recrea la jerarquía de directorios) y enlaces protegidos con contraseña.
"""

from __future__ import annotations

import argparse
import html
import json
import re
import sys
import time
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Optional
from urllib.parse import unquote

import requests
from requests.adapters import HTTPAdapter
from urllib3.util.retry import Retry

try:
    from rich import box
    from rich.console import Console
    from rich.panel import Panel
    from rich.progress import (
        BarColumn,
        DownloadColumn,
        Progress,
        SpinnerColumn,
        TaskProgressColumn,
        TextColumn,
        TimeRemainingColumn,
        TransferSpeedColumn,
    )
    from rich.table import Table
    from rich.text import Text
    from rich.tree import Tree
except ImportError:  # pragma: no cover
    sys.stderr.write(
        "Falta la dependencia 'rich'. Instálala con:  pip install rich requests\n"
    )
    sys.exit(1)

__version__ = "1.0.0"

BASE_URL = "https://www.swisstransfer.com"
USER_AGENT = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36"
)
XSRF_COOKIE = "SWISSTRANSFER-API-XSRF-TOKEN"
CHUNK_SIZE = 1024 * 256  # 256 KiB
UUID_RE = re.compile(
    r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", re.I
)

console = Console(highlight=False)
err_console = Console(stderr=True, highlight=False)


# ─────────────────────────────────────────────────────────────── modelos ────
@dataclass
class TransferFile:
    id: str
    path: str  # ruta relativa tal y como la subió el emisor ("carpeta/sub/a.txt")
    size: int
    mime_type: Optional[str] = None

    @property
    def name(self) -> str:
        return self.path.rsplit("/", 1)[-1]


@dataclass
class Transfer:
    link_id: str
    transfer_id: str
    title: Optional[str]
    message: Optional[str]
    total_size: int
    expires_at: Optional[int]
    files: list[TransferFile] = field(default_factory=list)

    @property
    def is_folder(self) -> bool:
        return any("/" in f.path for f in self.files)


class SwissTransferError(Exception):
    pass


class PasswordRequired(SwissTransferError):
    pass


class WrongPassword(SwissTransferError):
    pass


# ───────────────────────────────────────────────────────────── utilidades ────
def human_size(n: float) -> str:
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if n < 1024 or unit == "TiB":
            return f"{n:.0f} {unit}" if unit == "B" else f"{n:.2f} {unit}"
        n /= 1024
    return f"{n:.2f} TiB"


def extract_link_id(link: str) -> str:
    """Acepta la URL completa o directamente el UUID."""
    m = UUID_RE.search(link)
    if not m:
        raise SwissTransferError(
            f"No se ha encontrado un identificador válido de SwissTransfer en: {link}"
        )
    return m.group(0).lower()


def safe_relpath(path: str) -> Path:
    """Sanea una ruta relativa para evitar path traversal y nombres inválidos."""
    parts: list[str] = []
    for part in re.split(r"[\\/]+", path):
        part = part.strip()
        if part in ("", ".", ".."):
            continue
        # Caracteres prohibidos en Windows + control chars
        part = re.sub(r'[<>:"|?*\x00-\x1f]', "_", part)
        parts.append(part)
    if not parts:
        parts = ["archivo_sin_nombre"]
    return Path(*parts)


def build_session() -> requests.Session:
    s = requests.Session()
    s.headers.update(
        {
            "User-Agent": USER_AGENT,
            "Accept-Language": "es-ES,es;q=0.9,en;q=0.8",
        }
    )
    retry = Retry(
        total=5,
        connect=5,
        read=5,
        backoff_factor=0.8,
        status_forcelist=(429, 500, 502, 503, 504),
        allowed_methods=frozenset({"GET", "HEAD", "POST"}),
        raise_on_status=False,
    )
    adapter = HTTPAdapter(max_retries=retry, pool_maxsize=8)
    s.mount("https://", adapter)
    s.mount("http://", adapter)
    return s


# ─────────────────────────────────────────────────────────────── cliente ────
class SwissTransferClient:
    def __init__(self, session: Optional[requests.Session] = None, timeout: int = 30):
        self.s = session or build_session()
        self.timeout = timeout
        self._inertia_version: Optional[str] = None

    # ---- página Inertia -------------------------------------------------
    def _parse_inertia_page(self, html_text: str) -> dict:
        m = re.search(
            r'<script[^>]+data-page="app"[^>]*type="application/json"[^>]*>(.*?)</script>',
            html_text,
            re.S,
        )
        if not m:
            # Formato antiguo: <div id="app" data-page="{...}">
            m2 = re.search(r'id="app"[^>]*data-page="([^"]+)"', html_text)
            if not m2:
                raise SwissTransferError(
                    "No se ha podido leer la respuesta de SwissTransfer "
                    "(¿ha cambiado el formato de la página?)."
                )
            raw = html.unescape(m2.group(1))
        else:
            raw = m.group(1)
        try:
            return json.loads(raw)
        except json.JSONDecodeError as e:
            raise SwissTransferError(f"Respuesta JSON inválida de SwissTransfer: {e}")

    def _fetch_page(self, link_id: str) -> dict:
        url = f"{BASE_URL}/dl/{link_id}"
        r = self.s.get(url, timeout=self.timeout, allow_redirects=True)
        if r.status_code == 404 or "/not_found" in r.url:
            raise SwissTransferError(
                "Enlace no encontrado: puede haber expirado, haber sido eliminado "
                "o alcanzado el límite de descargas."
            )
        if r.status_code >= 400:
            raise SwissTransferError(
                f"SwissTransfer respondió HTTP {r.status_code} al abrir el enlace."
            )
        page = self._parse_inertia_page(r.text)
        self._inertia_version = page.get("version")
        return page

    def _submit_password(self, link_id: str, password: str) -> dict:
        """Envía la contraseña vía Inertia (POST /dl/{id})."""
        xsrf = self.s.cookies.get(XSRF_COOKIE) or self.s.cookies.get("XSRF-TOKEN")
        headers = {
            "Accept": "text/html, application/xhtml+xml",
            "Content-Type": "application/json",
            "X-Requested-With": "XMLHttpRequest",
            "X-Inertia": "true",
            "Referer": f"{BASE_URL}/dl/{link_id}",
            "Origin": BASE_URL,
        }
        if self._inertia_version:
            headers["X-Inertia-Version"] = self._inertia_version
        if xsrf:
            headers["X-XSRF-TOKEN"] = unquote(xsrf)

        r = self.s.post(
            f"{BASE_URL}/dl/{link_id}",
            json={"password": password},
            headers=headers,
            timeout=self.timeout,
            allow_redirects=True,
        )
        if r.status_code == 409:
            # Versión de assets desactualizada → Inertia pide recargar; reintenta sin versión
            self._inertia_version = None
            headers.pop("X-Inertia-Version", None)
            r = self.s.post(
                f"{BASE_URL}/dl/{link_id}",
                json={"password": password},
                headers=headers,
                timeout=self.timeout,
            )
        if r.status_code in (401, 403, 422):
            raise WrongPassword("Contraseña incorrecta.")
        if r.status_code >= 400:
            raise SwissTransferError(
                f"Error HTTP {r.status_code} al enviar la contraseña."
            )
        ctype = r.headers.get("Content-Type", "")
        if "application/json" in ctype:
            page = r.json()
        else:
            page = self._parse_inertia_page(r.text)
        return page

    # ---- API pública -----------------------------------------------------
    def get_transfer(self, link: str, password: Optional[str] = None) -> Transfer:
        link_id = extract_link_id(link)
        page = self._fetch_page(link_id)
        component = page.get("component", "")
        props = page.get("props", {})

        if component.endswith("password"):
            if not password:
                raise PasswordRequired(
                    "Esta transferencia está protegida con contraseña (usa -p / --password)."
                )
            page = self._submit_password(link_id, password)
            component = page.get("component", "")
            props = page.get("props", {})
            if component.endswith("password"):
                errors = props.get("errors") or {}
                msg = errors.get("password") or "Contraseña incorrecta."
                raise WrongPassword(msg)

        if component.endswith("not-found"):
            raise SwissTransferError("Enlace no encontrado o expirado.")
        if "unavailable" in component or "expired" in component:
            raise SwissTransferError(
                "La transferencia ya no está disponible (expirada o límite alcanzado)."
            )
        if "infected" in component:
            raise SwissTransferError(
                "El antivirus de SwissTransfer ha marcado esta transferencia como infectada."
            )
        if "antivirus" in component or "waiting" in component:
            raise SwissTransferError(
                "La transferencia está pendiente de análisis antivirus. Inténtalo más tarde."
            )

        transfer = props.get("transfer")
        if not transfer:
            raise SwissTransferError(
                f"Respuesta inesperada de SwissTransfer (componente '{component}')."
            )

        files = [
            TransferFile(
                id=f["id"],
                path=f.get("path") or f.get("name") or f["id"],
                size=int(f.get("size") or 0),
                mime_type=f.get("mime_type"),
            )
            for f in transfer.get("files", [])
        ]
        return Transfer(
            link_id=link_id,
            transfer_id=transfer["id"],
            title=transfer.get("title"),
            message=transfer.get("message"),
            total_size=int(transfer.get("total_size") or sum(f.size for f in files)),
            expires_at=transfer.get("expires_at"),
            files=files,
        )

    def get_download_url(self, link_id: str, file_id: str) -> str:
        url = f"{BASE_URL}/api/1/links/{link_id}/files/{file_id}"
        r = self.s.get(
            url,
            headers={
                "Accept": "application/json",
                "Referer": f"{BASE_URL}/dl/{link_id}",
                "X-Requested-With": "XMLHttpRequest",
            },
            timeout=self.timeout,
        )
        if r.status_code == 429:
            raise SwissTransferError("Demasiadas peticiones (429). Espera un momento.")
        if r.status_code >= 400:
            raise SwissTransferError(
                f"No se pudo obtener la URL de descarga (HTTP {r.status_code})."
            )
        data = r.json()
        presigned = (data.get("data") or {}).get("url") if isinstance(data.get("data"), dict) else data.get("data")
        if not presigned or not isinstance(presigned, str):
            raise SwissTransferError("SwissTransfer no devolvió una URL de descarga válida.")
        return presigned

    def download_file(
        self,
        tf: TransferFile,
        link_id: str,
        dest: Path,
        progress: Progress,
        task_id,
        overall_task_id=None,
    ) -> Path:
        dest.parent.mkdir(parents=True, exist_ok=True)
        part = dest.with_name(dest.name + ".part")

        # Reanudación
        resume_from = part.stat().st_size if part.exists() else 0
        if tf.size and resume_from >= tf.size:
            resume_from = 0
            part.unlink(missing_ok=True)

        url = self.get_download_url(link_id, tf.id)
        headers = {}
        if resume_from:
            headers["Range"] = f"bytes={resume_from}-"

        with self.s.get(url, headers=headers, stream=True, timeout=(15, 120)) as r:
            if resume_from and r.status_code != 206:
                # El servidor no aceptó el rango → empezamos de cero
                resume_from = 0
                part.unlink(missing_ok=True)
                r.close()
                r = self.s.get(url, stream=True, timeout=(15, 120))
            if r.status_code >= 400:
                raise SwissTransferError(
                    f"Error HTTP {r.status_code} al descargar «{tf.name}»."
                )
            total = tf.size or (
                int(r.headers.get("Content-Length", 0)) + resume_from
            )
            progress.update(task_id, total=total or None, completed=resume_from)
            if overall_task_id is not None and resume_from:
                progress.advance(overall_task_id, resume_from)

            mode = "ab" if resume_from else "wb"
            with open(part, mode) as fh:
                for chunk in r.iter_content(chunk_size=CHUNK_SIZE):
                    if not chunk:
                        continue
                    fh.write(chunk)
                    progress.advance(task_id, len(chunk))
                    if overall_task_id is not None:
                        progress.advance(overall_task_id, len(chunk))

        final_size = part.stat().st_size
        if tf.size and final_size != tf.size:
            raise SwissTransferError(
                f"Tamaño incorrecto para «{tf.name}»: esperado {tf.size} B, "
                f"obtenido {final_size} B."
            )
        part.replace(dest)
        # Asegura que la barra quede al 100 % (archivos de 0 bytes, etc.)
        progress.update(task_id, completed=final_size, total=final_size or 1)
        return dest


# ────────────────────────────────────────────────────────────── interfaz ────
def print_banner() -> None:
    title = Text.assemble(
        ("Swiss", "bold red"),
        ("Transfer", "bold white"),
        (" Downloader", "bold cyan"),
        (f"  v{__version__}", "dim"),
    )
    console.print(Panel(title, box=box.ROUNDED, border_style="red", expand=False))


def build_tree(transfer: Transfer) -> Tree:
    root_label = transfer.title or (
        "📁 Carpeta" if transfer.is_folder else "📦 Transferencia"
    )
    tree = Tree(f"[bold]{root_label}[/bold]", guide_style="dim")
    nodes: dict[str, Tree] = {"": tree}
    for f in sorted(transfer.files, key=lambda x: x.path.lower()):
        parts = f.path.split("/")
        parent_key = ""
        for d in parts[:-1]:
            key = f"{parent_key}/{d}" if parent_key else d
            if key not in nodes:
                nodes[key] = nodes[parent_key].add(f"[bold blue]📁 {d}[/bold blue]")
            parent_key = key
        nodes[parent_key].add(
            f"📄 {f.name}  [dim]({human_size(f.size)})[/dim]"
        )
    return tree


def print_summary(transfer: Transfer, out_dir: Path) -> None:
    table = Table(box=box.SIMPLE, show_header=False, expand=False, padding=(0, 1))
    table.add_column(style="bold cyan", justify="right")
    table.add_column()
    table.add_row("Enlace", f"[link={BASE_URL}/dl/{transfer.link_id}]{transfer.link_id}[/link]")
    if transfer.title:
        table.add_row("Título", transfer.title)
    if transfer.message:
        table.add_row("Mensaje", transfer.message.strip())
    table.add_row("Archivos", f"{len(transfer.files)}")
    table.add_row("Tamaño total", human_size(transfer.total_size))
    if transfer.expires_at:
        exp = datetime.fromtimestamp(transfer.expires_at)
        remaining = exp - datetime.now()
        days = max(remaining.days, 0)
        table.add_row(
            "Expira",
            f"{exp:%Y-%m-%d %H:%M}  [dim]({days} día{'s' if days != 1 else ''})[/dim]",
        )
    table.add_row("Destino", str(out_dir.resolve()))
    console.print(table)
    console.print(build_tree(transfer))
    console.print()


def run(args: argparse.Namespace) -> int:
    if not args.quiet:
        print_banner()

    client = SwissTransferClient(timeout=args.timeout)

    with console.status("[cyan]Consultando SwissTransfer…", spinner="dots"):
        try:
            transfer = client.get_transfer(args.link, args.password)
        except PasswordRequired:
            transfer = None

    if transfer is None:
        # Pedir contraseña de forma interactiva
        if not sys.stdin.isatty():
            err_console.print("[red]✖ Se requiere contraseña (-p/--password).[/red]")
            return 2
        from getpass import getpass

        for _ in range(3):
            pwd = getpass("🔒 Contraseña: ")
            try:
                with console.status("[cyan]Verificando contraseña…", spinner="dots"):
                    transfer = client.get_transfer(args.link, pwd)
                break
            except WrongPassword:
                err_console.print("[yellow]Contraseña incorrecta, inténtalo de nuevo.[/yellow]")
        if transfer is None:
            err_console.print("[red]✖ Demasiados intentos fallidos.[/red]")
            return 2

    if not transfer.files:
        err_console.print("[yellow]La transferencia no contiene archivos.[/yellow]")
        return 1

    # Directorio de salida: si es carpeta, los archivos ya llevan el prefijo de carpeta
    out_dir = Path(args.output).expanduser()
    if args.subdir:
        out_dir = out_dir / safe_relpath(transfer.title or transfer.link_id)

    print_summary(transfer, out_dir)

    if args.list:
        return 0

    # Filtrar archivos ya existentes (a menos que --force)
    to_download: list[tuple[TransferFile, Path]] = []
    skipped = 0
    for f in transfer.files:
        dest = out_dir / safe_relpath(f.path)
        if dest.exists() and not args.force and (not f.size or dest.stat().st_size == f.size):
            skipped += 1
            continue
        to_download.append((f, dest))

    if skipped:
        console.print(f"[dim]↷ {skipped} archivo(s) ya existente(s), omitido(s). Usa --force para sobrescribir.[/dim]")
    if not to_download:
        console.print("[green]✔ Nada que descargar: todo está ya en disco.[/green]")
        return 0

    total_bytes = sum(f.size for f, _ in to_download)
    ok, failed = 0, []
    t0 = time.monotonic()

    progress = Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        BarColumn(bar_width=None),
        TaskProgressColumn(),
        DownloadColumn(),
        TransferSpeedColumn(),
        TimeRemainingColumn(),
        console=console,
        transient=False,
        expand=True,
    )

    with progress:
        overall = progress.add_task(
            f"[bold]Total ({len(to_download)} archivos)", total=total_bytes or None
        )
        for idx, (f, dest) in enumerate(to_download, 1):
            label = f"[cyan]{idx}/{len(to_download)}[/cyan] {f.name}"
            if len(label) > 48:
                label = label[:45] + "…"
            task = progress.add_task(label, total=f.size or None)
            attempts = 0
            while True:
                attempts += 1
                try:
                    client.download_file(f, transfer.link_id, dest, progress, task, overall)
                    ok += 1
                    progress.update(task, description=f"[green]✔[/green] {f.name}")
                    break
                except (requests.RequestException, SwissTransferError) as e:
                    if attempts < args.retries:
                        progress.update(
                            task,
                            description=f"[yellow]↻ reintento {attempts}[/yellow] {f.name}",
                        )
                        time.sleep(min(2**attempts, 15))
                        continue
                    failed.append((f, str(e)))
                    progress.update(task, description=f"[red]✖[/red] {f.name}")
                    break
        progress.update(overall, completed=progress.tasks[0].total or 0)

    elapsed = time.monotonic() - t0
    console.print()
    if failed:
        tbl = Table(title="Errores", box=box.SIMPLE, show_lines=False, title_style="bold red")
        tbl.add_column("Archivo", style="red")
        tbl.add_column("Motivo")
        for f, why in failed:
            tbl.add_row(f.path, why)
        console.print(tbl)

    speed = (human_size(total_bytes / elapsed) + "/s") if elapsed > 0 else "—"
    status = "green" if not failed else ("yellow" if ok else "red")
    console.print(
        Panel(
            f"[{status}]{'✔' if not failed else '⚠'} {ok}/{len(to_download)} archivos descargados[/{status}]"
            f"  ·  {human_size(total_bytes)} en {elapsed:.1f}s ({speed})\n"
            f"📂 [bold]{out_dir.resolve()}[/bold]",
            border_style=status,
            expand=False,
        )
    )
    return 0 if not failed else 1


def parse_args(argv: Optional[list[str]] = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        prog="swisstransfer_dl.py",
        description="Descarga transferencias de SwissTransfer desde la terminal.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Ejemplos:\n"
            "  python swisstransfer_dl.py https://www.swisstransfer.com/dl/<uuid>\n"
            "  python swisstransfer_dl.py <uuid> -o ~/Descargas -p secreto\n"
            "  python swisstransfer_dl.py <enlace> --list\n"
        ),
    )
    p.add_argument("link", help="URL de SwissTransfer (https://www.swisstransfer.com/dl/…) o UUID")
    p.add_argument("-o", "--output", default=".", help="Directorio de destino (por defecto: actual)")
    p.add_argument("-p", "--password", help="Contraseña de la transferencia (si está protegida)")
    p.add_argument("-l", "--list", action="store_true", help="Solo listar el contenido, sin descargar")
    p.add_argument("-f", "--force", action="store_true", help="Sobrescribir archivos existentes")
    p.add_argument("-s", "--subdir", action="store_true",
                   help="Crear un subdirectorio con el título/UUID de la transferencia")
    p.add_argument("-r", "--retries", type=int, default=3, help="Reintentos por archivo (defecto: 3)")
    p.add_argument("-t", "--timeout", type=int, default=30, help="Timeout de red en segundos (defecto: 30)")
    p.add_argument("-q", "--quiet", action="store_true", help="No mostrar el banner")
    p.add_argument("-V", "--version", action="version", version=f"%(prog)s {__version__}")
    return p.parse_args(argv)


def main(argv: Optional[list[str]] = None) -> int:
    args = parse_args(argv)
    try:
        return run(args)
    except KeyboardInterrupt:
        err_console.print("\n[yellow]⏹ Cancelado por el usuario. Los .part se reanudarán la próxima vez.[/yellow]")
        return 130
    except SwissTransferError as e:
        err_console.print(f"[bold red]✖ {e}[/bold red]")
        return 1
    except requests.RequestException as e:
        err_console.print(f"[bold red]✖ Error de red:[/bold red] {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
