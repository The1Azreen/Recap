"""Every recap is saved under RECAP_HOME/library/<id>/ so it can be searched and asked about later.

<id>/transcript.json   the Transcript
<id>/summary.json      the summary, when one was made
<id>/meta.json         title, dates, method, media file name
<id>/media.<ext>       the original upload, for playback in the web UI (optional)
"""

from __future__ import annotations

import json
import re
import shutil
import time
from pathlib import Path

from . import config
from .config import RecapError
from .models import Transcript


def _slug(text: str) -> str:
    s = re.sub(r"[^\w-]+", "-", text.lower(), flags=re.UNICODE).strip("-")
    return s[:40] or "recap"


def _dir(item_id: str) -> Path:
    if not re.fullmatch(r"[\w-]+", item_id):
        raise RecapError("Bad id")
    d = config.LIBRARY_DIR / item_id
    if not d.is_dir():
        raise RecapError(f"No recap with id {item_id}")
    return d


def save(t: Transcript, summary: dict | None, style: str | None, media: Path | None = None,
         move_media: bool = False) -> str:
    item_id = f"{time.strftime('%Y%m%d-%H%M%S')}-{_slug(t.title or 'recap')}"
    d = config.LIBRARY_DIR / item_id
    d.mkdir(parents=True, exist_ok=False)
    media_name = None
    if media and media.is_file():
        media_name = "media" + media.suffix.lower()
        (shutil.move if move_media else shutil.copy2)(str(media), str(d / media_name))
    (d / "transcript.json").write_text(json.dumps(t.to_dict(), ensure_ascii=False), encoding="utf-8")
    if summary:
        (d / "summary.json").write_text(json.dumps(summary, ensure_ascii=False), encoding="utf-8")
    meta = {
        "id": item_id,
        "title": (summary or {}).get("title") or t.title or "Untitled",
        "source": t.source,
        "source_type": t.source_type,
        "video_id": t.video_id,
        "method": t.method,
        "language": t.language,
        "duration": t.duration,
        "style": style,
        "created": time.time(),
        "media": media_name,
    }
    (d / "meta.json").write_text(json.dumps(meta, ensure_ascii=False), encoding="utf-8")
    return item_id


def list_items() -> list[dict]:
    if not config.LIBRARY_DIR.is_dir():
        return []
    items = []
    for d in config.LIBRARY_DIR.iterdir():
        try:
            items.append(json.loads((d / "meta.json").read_text(encoding="utf-8")))
        except (OSError, ValueError):
            continue
    return sorted(items, key=lambda m: m.get("created", 0), reverse=True)


def meta(item_id: str) -> dict:
    return json.loads((_dir(item_id) / "meta.json").read_text(encoding="utf-8"))


def transcript(item_id: str) -> Transcript:
    return Transcript.from_dict(json.loads((_dir(item_id) / "transcript.json").read_text(encoding="utf-8")))


def summary(item_id: str) -> dict | None:
    p = _dir(item_id) / "summary.json"
    return json.loads(p.read_text(encoding="utf-8")) if p.is_file() else None


def set_summary(item_id: str, s: dict, style: str) -> None:
    d = _dir(item_id)
    (d / "summary.json").write_text(json.dumps(s, ensure_ascii=False), encoding="utf-8")
    m = meta(item_id)
    m["style"] = style
    m["title"] = s.get("title") or m["title"]
    (d / "meta.json").write_text(json.dumps(m, ensure_ascii=False), encoding="utf-8")


def media_path(item_id: str) -> Path | None:
    m = meta(item_id)
    return _dir(item_id) / m["media"] if m.get("media") else None


def delete(item_id: str) -> None:
    shutil.rmtree(_dir(item_id))


def search(query: str, limit: int = 50) -> list[dict]:
    """Case-insensitive search across every saved transcript and summary.
    Returns one hit per matching segment: {id, title, start, text}."""
    q = query.strip().lower()
    if not q:
        return []
    hits: list[dict] = []
    for m in list_items():
        try:
            t = transcript(m["id"])
        except (OSError, ValueError, RecapError):
            continue
        title_hit = q in (m.get("title") or "").lower()
        found = False
        for s in t.segments:
            if q in s.text.lower():
                hits.append({"id": m["id"], "title": m["title"], "start": s.start, "text": s.text})
                found = True
                if len(hits) >= limit:
                    return hits
        if title_hit and not found:
            hits.append({"id": m["id"], "title": m["title"], "start": 0, "text": ""})
    return hits
