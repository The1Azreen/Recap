"""recap: command-line interface."""

from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Optional

import typer
from rich.console import Console
from rich.progress import BarColumn, Progress, TextColumn, TimeElapsedColumn

from . import config, export, library, pipeline
from .config import RecapError
from .models import fmt_ts
from .summarize import STYLES

app = typer.Typer(add_completion=False, no_args_is_help=True,
                  help="Turn YouTube links, audio and video files into transcripts and summaries.")
console = Console()


def _fail(e: Exception) -> None:
    console.print(f"[bold red]Error:[/] {e}")
    raise typer.Exit(1)


@app.command()
def run(
    source: str = typer.Argument(..., help="YouTube/video URL, or an audio/video file."),
    style: str = typer.Option("general", "--style", "-s", help=f"Summary style: {', '.join(STYLES)}."),
    no_summary: bool = typer.Option(False, "--no-summary", help="Transcript only; skip Claude."),
    backend: str = typer.Option("auto", help="auto, whispercpp (Vulkan GPU) or faster-whisper (CPU)."),
    model: Optional[str] = typer.Option(None, "--model", "-m", help="Whisper model, e.g. large-v3-turbo, small."),
    language: Optional[str] = typer.Option(None, "--lang", "-l", help="Spoken language code, e.g. en. Default: detect."),
    transcribe: bool = typer.Option(False, "--transcribe", help="Ignore YouTube captions and transcribe the audio."),
    out: Path = typer.Option(Path("."), "--out", "-o", help="Folder for the exported files."),
    formats: str = typer.Option("txt,srt,md,json", "--formats", "-f", help="Comma-separated: txt,srt,md,json."),
):
    """Transcribe and summarize one source, save it to the library, and export files."""
    fmts = tuple(f.strip() for f in formats.split(",") if f.strip())
    bad = [f for f in fmts if f not in export.FORMATS]
    if bad:
        _fail(RecapError(f"Unknown format(s): {', '.join(bad)}"))

    with Progress(TextColumn("[bold yellow]{task.description}"), BarColumn(), TimeElapsedColumn(),
                  console=console, transient=True) as bar:
        task = bar.add_task("Starting", total=None)

        def progress(stage, frac, msg):
            bar.update(task, description=msg, total=1.0 if frac is not None else None,
                       completed=frac or 0)

        try:
            item_id = pipeline.run(source, style=style, do_summary=not no_summary, backend=backend,
                                   model=model, language=language, force_transcribe=transcribe,
                                   progress=progress)
        except RecapError as e:
            _fail(e)

    t, s, m = library.transcript(item_id), library.summary(item_id), library.meta(item_id)
    paths = export.write_all(t, s, out, item_id, fmts)
    console.print(f"[bold yellow]{m['title']}[/]  [cyan]{fmt_ts(t.duration)}[/] · {t.method}")
    if s:
        console.print(f"\n[bold]TL;DR[/] {s['tldr']}\n")
        for p in s["key_points"]:
            console.print(f"  [magenta]▸[/] {p}")
    console.print(f"\nSaved as [cyan]{item_id}[/]")
    for p in paths:
        console.print(f"  {p}")


@app.command("list")
def list_cmd():
    """Show saved recaps."""
    for m in library.list_items():
        console.print(f"[cyan]{m['id']}[/]  {m['title']}  [dim]{fmt_ts(m.get('duration') or 0)} · {m.get('method')}[/]")


@app.command()
def search(query: str):
    """Search every saved transcript."""
    hits = library.search(query)
    if not hits:
        console.print("No matches.")
    for h in hits:
        console.print(f"[cyan]{h['id']}[/] [yellow]{fmt_ts(h['start'])}[/] {h['text'] or h['title']}")


@app.command()
def ask(item_id: str, question: str):
    """Ask Claude a question about a saved recap."""
    from .summarize import ask as ask_claude
    try:
        console.print(ask_claude(library.transcript(item_id), question))
    except RecapError as e:
        _fail(e)


@app.command("export")
def export_cmd(item_id: str, out: Path = typer.Option(Path("."), "--out", "-o"),
               formats: str = typer.Option("txt,srt,md,json", "--formats", "-f")):
    """Re-export a saved recap."""
    fmts = tuple(f.strip() for f in formats.split(","))
    try:
        for p in export.write_all(library.transcript(item_id), library.summary(item_id), out, item_id, fmts):
            console.print(p)
    except (RecapError, ValueError) as e:
        _fail(e)


@app.command()
def serve(host: str = "127.0.0.1", port: int = 8765, open_browser: bool = typer.Option(True, "--open/--no-open")):
    """Start the web interface."""
    import threading
    import webbrowser

    import uvicorn

    url = f"http://{host}:{port}"
    console.print(f"Recap is running at [cyan]{url}[/]  (Ctrl+C to stop)")
    if open_browser:
        threading.Timer(1.2, lambda: webbrowser.open(url)).start()
    uvicorn.run("recap.web.server:app", host=host, port=port, log_level="warning")


@app.command()
def doctor():
    """Check ffmpeg, the transcriber, the GPU and the Claude API key."""
    import os
    import shutil

    def row(ok, name, detail):
        console.print(f"{'[green]✓' if ok else '[red]✗'}[/] [bold]{name}[/]  {detail}")

    try:
        row(True, "ffmpeg", config.ffmpeg())
    except RecapError as e:
        row(False, "ffmpeg", str(e))

    cli = config.whisper_cli()
    if cli:
        vk = config.whisper_cli_has_vulkan(cli)
        row(vk, "whisper.cpp", f"{cli}  ({'Vulkan GPU build' if vk else 'CPU-only build'})")
    else:
        row(False, "whisper.cpp", "not installed. Run scripts\\build_whisper_vulkan.ps1 for GPU transcription")

    if shutil.which("vulkaninfo"):
        out = subprocess.run(["vulkaninfo", "--summary"], capture_output=True, text=True,
                             encoding="utf-8", errors="replace").stdout
        gpus = [l.split("=", 1)[1].strip() for l in out.splitlines() if "deviceName" in l]
        row(bool(gpus), "Vulkan GPUs", ", ".join(gpus) or "none found")
    else:
        row(False, "Vulkan GPUs", "vulkaninfo not found (update your GPU driver)")

    try:
        import faster_whisper  # noqa: F401
        row(True, "faster-whisper", "installed (CPU fallback)")
    except ImportError:
        console.print("[dim]-[/] [bold]faster-whisper[/]  not installed (optional CPU fallback)")

    key = bool(os.environ.get("ANTHROPIC_API_KEY") or os.environ.get("ANTHROPIC_AUTH_TOKEN"))
    row(key, "Claude API", f"credentials found, model {config.CLAUDE_MODEL}" if key else
        "ANTHROPIC_API_KEY not set (summaries and Q&A need it; `ant auth login` also works)")
    console.print(f"[dim]Data folder: {config.HOME}[/]")


if __name__ == "__main__":
    app()
