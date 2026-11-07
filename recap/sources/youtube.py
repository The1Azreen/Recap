"""YouTube links: use the captions the video already has; fall back to downloading the audio.

Other http(s) links go straight to the yt-dlp download path, which covers most video sites.
"""

from __future__ import annotations

import html
import json
import re
import tempfile
import urllib.parse
import urllib.request
from pathlib import Path

from ..config import RecapError
from ..models import Segment, Transcript, merge_segments

_ID = r"([A-Za-z0-9_-]{11})"
_PATTERNS = [
    re.compile(r"(?:youtube\.com|youtube-nocookie\.com)/(?:watch\?(?:.*&)?v=|embed/|shorts/|live/|v/)" + _ID),
    re.compile(r"youtu\.be/" + _ID),
]


def video_id(url: str) -> str | None:
    for p in _PATTERNS:
        m = p.search(url)
        if m:
            return m.group(1)
    return None


def _title(url: str) -> str | None:
    """oEmbed is a single fast request and needs no API key."""
    try:
        q = urllib.parse.urlencode({"url": url, "format": "json"})
        with urllib.request.urlopen(f"https://www.youtube.com/oembed?{q}", timeout=10) as r:
            return json.load(r).get("title")
    except Exception:
        return None


def _captions(vid: str, language: str | None) -> tuple[list[Segment], str, bool] | None:
    """Returns (segments, language_code, is_generated), or None when the video has no captions."""
    from youtube_transcript_api import CouldNotRetrieveTranscript, YouTubeTranscriptApi

    api = YouTubeTranscriptApi()
    try:
        listing = api.list(vid)
    except CouldNotRetrieveTranscript:
        return None

    wanted = [language] if language else ["en", "en-US", "en-GB"]
    available = list(listing)
    if not available:
        return None
    pick = None
    for finder in (listing.find_manually_created_transcript, listing.find_generated_transcript):
        try:
            pick = finder(wanted)
            break
        except CouldNotRetrieveTranscript:
            continue
    if pick is None:
        # Nothing in the preferred language: prefer human-made captions in any language.
        pick = next((t for t in available if not t.is_generated), available[0])

    try:
        fetched = pick.fetch()
    except CouldNotRetrieveTranscript:
        return None

    raw = []
    for s in fetched:
        text = html.unescape(s.text).replace("\n", " ").strip()
        if text:
            raw.append(Segment(s.start, s.start + s.duration, text))
    # Auto-captions overlap; end each one where the next begins.
    for a, b in zip(raw, raw[1:]):
        a.end = min(a.end, b.start) if b.start > a.start else a.end
    return merge_segments(raw), pick.language_code, pick.is_generated


def _download_audio(url: str, dest: Path, progress) -> tuple[Path, dict]:
    import yt_dlp

    def hook(d):
        if d.get("status") == "downloading" and progress:
            total = d.get("total_bytes") or d.get("total_bytes_estimate")
            frac = d.get("downloaded_bytes", 0) / total if total else None
            progress("download", frac, "Downloading audio")

    opts = {
        "format": "bestaudio/best",
        "outtmpl": str(dest / "%(id)s.%(ext)s"),
        "quiet": True,
        "no_warnings": True,
        "noprogress": True,
        "noplaylist": True,
        "progress_hooks": [hook],
    }
    try:
        with yt_dlp.YoutubeDL(opts) as ydl:
            info = ydl.extract_info(url, download=True)
            return Path(ydl.prepare_filename(info)), info
    except yt_dlp.utils.DownloadError as e:
        raise RecapError(f"Couldn't download {url}: {e}") from e


def load(url: str, *, backend="auto", model=None, language=None, progress=None, force_transcribe=False) -> Transcript:
    from . import audio

    vid = video_id(url)
    if vid and not force_transcribe:
        if progress:
            progress("captions", None, "Fetching YouTube captions")
        caps = _captions(vid, language)
        if caps:
            segments, lang, generated = caps
            kind = "auto-generated" if generated else "uploaded"
            return Transcript(
                segments=segments,
                source=url,
                source_type="youtube",
                title=_title(url),
                language=lang,
                method=f"YouTube captions ({kind}, {lang})",
                video_id=vid,
            )

    if progress:
        progress("download", None, "No captions, downloading audio" if vid else "Downloading audio")
    with tempfile.TemporaryDirectory(prefix="recap-") as tmp:
        path, info = _download_audio(url, Path(tmp), progress)
        t = audio.load(path, backend=backend, model=model, language=language, progress=progress)
    t.source = url
    t.source_type = "youtube" if vid else "url"
    t.video_id = vid
    t.title = info.get("title") or t.title
    return t
