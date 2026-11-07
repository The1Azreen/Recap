"""Where Recap keeps its models, tools and library. Override the root with RECAP_HOME."""

from __future__ import annotations

import os
import shutil
from pathlib import Path

HOME = Path(os.environ.get("RECAP_HOME", Path.home() / "Recap"))
MODELS_DIR = HOME / "models"
TOOLS_DIR = HOME / "tools"
LIBRARY_DIR = HOME / "library"
UPLOADS_DIR = HOME / "uploads"

WHISPER_CPP_DIR = TOOLS_DIR / "whisper.cpp"

CLAUDE_MODEL = os.environ.get("RECAP_CLAUDE_MODEL", "claude-opus-5-5")
CLAUDE_EFFORT = os.environ.get("RECAP_CLAUDE_EFFORT", "medium")

AUDIO_EXTS = {".mp3", ".wav", ".m4a", ".aac", ".flac", ".ogg", ".opus", ".wma", ".aiff", ".aif", ".amr"}
VIDEO_EXTS = {".webm", ".mp4", ".mov", ".mkv", ".avi", ".m4v", ".wmv", ".flv", ".mpg", ".mpeg", ".3gp", ".ts"}


class RecapError(RuntimeError):
    """An error worth showing to the user as-is."""


def ffmpeg() -> str:
    exe = os.environ.get("RECAP_FFMPEG") or shutil.which("ffmpeg")
    if not exe:
        raise RecapError("ffmpeg was not found. Install it and put it on PATH, or set RECAP_FFMPEG.")
    return exe


def whisper_cli() -> Path | None:
    """The whisper.cpp command-line binary, if one is installed."""
    env = os.environ.get("RECAP_WHISPER_CLI")
    if env and Path(env).is_file():
        return Path(env)
    exe = "whisper-cli.exe" if os.name == "nt" else "whisper-cli"
    local = WHISPER_CPP_DIR / exe
    if local.is_file():
        return local
    found = shutil.which("whisper-cli")
    return Path(found) if found else None


def whisper_cli_has_vulkan(cli: Path) -> bool:
    """True when the whisper.cpp build next to `cli` includes the Vulkan backend."""
    d = cli.parent
    return any(d.glob("ggml-vulkan*")) or (d / "VULKAN").exists()
