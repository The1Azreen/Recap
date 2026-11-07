"""Input -> transcript -> summary -> saved recap. Shared by the CLI and the web UI."""

from __future__ import annotations

from pathlib import Path
from typing import Callable

from . import library, sources
from .summarize import summarize

Progress = Callable[[str, float | None, str], None]


def run(source: str, *, style: str = "general", do_summary: bool = True, backend: str = "auto",
        model: str | None = None, language: str | None = None, force_transcribe: bool = False,
        keep_media: bool = True, move_media: bool = False,
        progress: Progress | None = None) -> str:
    """Returns the library id of the new recap."""
    t = sources.load(source, backend=backend, model=model, language=language,
                     force_transcribe=force_transcribe, progress=progress)
    s = summarize(t, style, progress=progress) if do_summary else None
    media = Path(source) if keep_media and t.source_type in ("audio", "video") else None
    if progress:
        progress("save", 1.0, "Saving")
    return library.save(t, s, style if s else None, media=media, move_media=move_media)
