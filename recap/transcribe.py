"""Speech to text.

Two backends, picked automatically:

* ``whispercpp``: whisper.cpp built with Vulkan. This is how Recap runs on AMD GPUs on Windows.
  faster-whisper (CTranslate2) only accelerates on NVIDIA CUDA, and AMD's ROCm PyTorch builds
  for Windows don't cover RDNA2 cards like the RX 6800 XT, but Vulkan runs on any recent
  AMD, NVIDIA or Intel GPU. Build it with ``scripts/build_whisper_vulkan.ps1``.
* ``faster-whisper``: CPU (int8) or NVIDIA CUDA. Install with ``pip install recap[cpu]``.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import threading
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

from . import config
from .config import RecapError
from .models import Segment

Progress = Callable[[str, float | None, str], None]

WHISPER_CPP_MODELS = [
    "tiny", "tiny.en", "base", "base.en", "small", "small.en", "medium", "medium.en",
    "large-v2", "large-v3", "large-v3-turbo",
]
GPU_DEFAULT_MODEL = "large-v3-turbo"  # ~1.6 GB, fast on a 16 GB card, close to large-v3 quality
CPU_DEFAULT_MODEL = "small"
_HF = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{}.bin"


@dataclass
class Result:
    segments: list[Segment]
    language: str | None
    method: str


def _noop(stage: str, frac: float | None, msg: str) -> None:
    pass


def extract_audio(src: Path, dst: Path) -> Path:
    """Any audio or video file to 16 kHz mono 16-bit WAV, the format Whisper wants."""
    cmd = [config.ffmpeg(), "-nostdin", "-hide_banner", "-loglevel", "error", "-y",
           "-i", str(src), "-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le", str(dst)]
    proc = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace")
    if proc.returncode != 0 or not dst.is_file():
        raise RecapError(f"ffmpeg couldn't read {src.name}: {proc.stderr.strip()[-400:]}")
    return dst


def ensure_ggml_model(name: str, progress: Progress = _noop) -> Path:
    if name not in WHISPER_CPP_MODELS:
        raise RecapError(f"Unknown whisper model {name!r}. Choose from: {', '.join(WHISPER_CPP_MODELS)}")
    path = config.MODELS_DIR / f"ggml-{name}.bin"
    if path.is_file():
        return path
    path.parent.mkdir(parents=True, exist_ok=True)
    part = path.with_suffix(".part")
    progress("model", 0.0, f"Downloading whisper model {name}")
    with urllib.request.urlopen(_HF.format(name), timeout=60) as r, open(part, "wb") as f:
        total = int(r.headers.get("Content-Length") or 0)
        done = 0
        while chunk := r.read(1 << 20):
            f.write(chunk)
            done += len(chunk)
            if total:
                progress("model", done / total, f"Downloading whisper model {name}")
    part.replace(path)
    return path


def available_backend() -> str:
    if config.whisper_cli():
        return "whispercpp"
    try:
        import faster_whisper  # noqa: F401
        return "faster-whisper"
    except ImportError:
        pass
    raise RecapError(
        "No transcriber installed. For the GPU (AMD/NVIDIA/Intel via Vulkan) run "
        "scripts\\build_whisper_vulkan.ps1; for CPU only run: pip install faster-whisper"
    )


def transcribe(path: Path, *, backend: str = "auto", model: str | None = None,
               language: str | None = None, progress: Progress | None = None) -> Result:
    progress = progress or _noop
    if backend == "auto":
        backend = available_backend()
    if backend == "whispercpp":
        return _whispercpp(path, model or GPU_DEFAULT_MODEL, language, progress)
    if backend in ("faster-whisper", "cpu"):
        return _faster_whisper(path, model or CPU_DEFAULT_MODEL, language, progress)
    raise RecapError(f"Unknown backend {backend!r} (use auto, whispercpp or faster-whisper)")


# --------------------------------------------------------------------------------------------
# whisper.cpp (Vulkan GPU)

_PROGRESS_RE = re.compile(r"progress\s*=\s*(\d+)%")
_DEVICE_RE = re.compile(r"ggml_vulkan:\s*\d+\s*=\s*([^|(]+)")


def _whispercpp(path: Path, model: str, language: str | None, progress: Progress) -> Result:
    import tempfile

    cli = config.whisper_cli()
    if not cli:
        raise RecapError("whisper.cpp isn't installed. Run scripts\\build_whisper_vulkan.ps1")
    model_path = ensure_ggml_model(model, progress)

    with tempfile.TemporaryDirectory(prefix="recap-") as tmp:
        tmp = Path(tmp)
        progress("extract", None, "Converting audio")
        wav = extract_audio(path, tmp / "audio.wav")
        out = tmp / "out"
        threads = max(1, min(8, (os.cpu_count() or 4) // 2))
        cmd = [str(cli), "-m", str(model_path), "-f", str(wav), "-oj", "-of", str(out),
               "-l", language or "auto", "-t", str(threads), "-pp"]
        progress("transcribe", 0.0, f"Transcribing with whisper.cpp ({model})")
        proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                                text=True, encoding="utf-8", errors="replace",
                                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        device: list[str] = []
        tail: list[str] = []

        def read_stderr():
            for line in proc.stderr:
                tail.append(line)
                del tail[:-30]
                if m := _DEVICE_RE.search(line):
                    device.append(m.group(1).strip())
                if m := _PROGRESS_RE.search(line):
                    where = f" on {device[0]}" if device else ""
                    progress("transcribe", int(m.group(1)) / 100, f"Transcribing{where}")

        reader = threading.Thread(target=read_stderr, daemon=True)
        reader.start()
        code = proc.wait()
        reader.join(timeout=5)
        if code != 0 or not (out.with_suffix(".json")).is_file():
            raise RecapError("whisper.cpp failed:\n" + "".join(tail[-12:]))
        data = json.loads(out.with_suffix(".json").read_text(encoding="utf-8", errors="replace"))

    segments = []
    for item in data.get("transcription", []):
        text = item.get("text", "").strip()
        if not text or text in ("[BLANK_AUDIO]", "[ Silence ]", "[silence]"):
            continue
        off = item.get("offsets", {})
        segments.append(Segment(off.get("from", 0) / 1000, off.get("to", 0) / 1000, text))
    lang = (data.get("result") or {}).get("language") or language
    where = f"Vulkan · {device[0]}" if device else "CPU"
    return Result(segments, lang, f"whisper.cpp {model} ({where})")


# --------------------------------------------------------------------------------------------
# faster-whisper (CPU / CUDA)

def _faster_whisper(path: Path, model: str, language: str | None, progress: Progress) -> Result:
    try:
        from faster_whisper import WhisperModel
    except ImportError as e:
        raise RecapError("faster-whisper isn't installed: pip install faster-whisper") from e

    device, compute = "cpu", "int8"
    try:
        import ctranslate2
        if ctranslate2.get_cuda_device_count() > 0:
            device, compute = "cuda", "float16"
    except Exception:
        pass

    # Decode with our ffmpeg rather than faster-whisper's PyAV, which breaks across `av` releases.
    import tempfile
    import wave

    import numpy as np

    progress("extract", None, "Converting audio")
    with tempfile.TemporaryDirectory(prefix="recap-") as tmp:
        wav = extract_audio(path, Path(tmp) / "audio.wav")
        with wave.open(str(wav), "rb") as w:
            pcm = w.readframes(w.getnframes())
    audio = np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0

    os.environ.setdefault("HF_HUB_DISABLE_SYMLINKS_WARNING", "1")
    progress("model", None, f"Loading faster-whisper {model} ({device})")
    wm = WhisperModel(model, device=device, compute_type=compute)
    seg_iter, info = wm.transcribe(audio, language=language, vad_filter=True)
    segments = []
    for s in seg_iter:
        segments.append(Segment(s.start, s.end, s.text.strip()))
        if info.duration:
            progress("transcribe", min(1.0, s.end / info.duration), f"Transcribing ({device})")
    return Result(segments, info.language, f"faster-whisper {model} ({device.upper()})")
