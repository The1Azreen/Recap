"""Video files: pull the audio track out with ffmpeg, then transcribe it like any audio file."""

from __future__ import annotations

import tempfile
from pathlib import Path

from ..transcribe import extract_audio
from . import audio


def load(path: Path, *, backend="auto", model=None, language=None, progress=None):
    if progress:
        progress("extract", None, "Extracting audio with ffmpeg")
    with tempfile.TemporaryDirectory(prefix="recap-") as tmp:
        wav = extract_audio(path, Path(tmp) / "audio.wav")
        t = audio.load(wav, backend=backend, model=model, language=language, progress=progress, source_type="video")
    t.source = str(path)
    t.title = path.stem
    return t
