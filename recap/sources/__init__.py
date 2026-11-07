"""Sources turn whatever the user gave into a Transcript. See `load`."""

from __future__ import annotations

from pathlib import Path
from typing import Callable

from ..config import AUDIO_EXTS, VIDEO_EXTS, RecapError
from ..models import Transcript
from . import audio, video, youtube

# progress(stage, fraction 0..1 or None, message)
Progress = Callable[[str, float | None, str], None]


def _noop(stage: str, frac: float | None, msg: str) -> None:
    pass


def kind_of(source: str) -> str:
    if youtube.video_id(source):
        return "youtube"
    if source.startswith(("http://", "https://")):
        return "url"
    ext = Path(source).suffix.lower()
    if ext in VIDEO_EXTS:
        return "video"
    if ext in AUDIO_EXTS:
        return "audio"
    raise RecapError(f"Don't know how to read {source!r}. Give a YouTube link, or an audio or video file.")


def load(
    source: str,
    *,
    backend: str = "auto",
    model: str | None = None,
    language: str | None = None,
    force_transcribe: bool = False,
    progress: Progress | None = None,
) -> Transcript:
    """Any source in, one Transcript out."""
    progress = progress or _noop
    kind = kind_of(source)
    opts = dict(backend=backend, model=model, language=language, progress=progress)
    if kind in ("youtube", "url"):
        return youtube.load(source, force_transcribe=force_transcribe, **opts)
    path = Path(source)
    if not path.is_file():
        raise RecapError(f"File not found: {source}")
    if kind == "video":
        return video.load(path, **opts)
    return audio.load(path, **opts)
