"""Audio files: voice memos, meetings, podcasts."""

from __future__ import annotations

from pathlib import Path

from ..models import Transcript
from ..transcribe import transcribe


def load(path: Path, *, backend="auto", model=None, language=None, progress=None, source_type="audio") -> Transcript:
    result = transcribe(path, backend=backend, model=model, language=language, progress=progress)
    return Transcript(
        segments=result.segments,
        source=str(path),
        source_type=source_type,
        title=path.stem,
        language=result.language,
        method=result.method,
    )
