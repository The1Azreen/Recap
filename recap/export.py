"""Transcript (+ optional summary) to .txt / .srt / .md / .json."""

from __future__ import annotations

import json
from pathlib import Path

from .models import Transcript, fmt_ts

FORMATS = ("txt", "srt", "md", "json")


def link_at(t: Transcript, seconds: float) -> str | None:
    """A URL that opens the source at `seconds`, when the source is a YouTube video."""
    if t.video_id:
        return f"https://www.youtube.com/watch?v={t.video_id}&t={int(seconds)}s"
    return None


def to_txt(t: Transcript) -> str:
    return "\n".join(
        f"[{fmt_ts(s.start, always_hours=True)}] {f'{s.speaker}: ' if s.speaker else ''}{s.text}"
        for s in t.segments
    ) + "\n"


def to_srt(t: Transcript) -> str:
    blocks = []
    for i, s in enumerate(t.segments, 1):
        end = s.end if s.end > s.start else s.start + 1
        text = f"{s.speaker}: {s.text}" if s.speaker else s.text
        blocks.append(f"{i}\n{fmt_ts(s.start, srt=True)} --> {fmt_ts(end, srt=True)}\n{text}\n")
    return "\n".join(blocks)


def _ts_md(t: Transcript, seconds: float) -> str:
    stamp = fmt_ts(seconds)
    url = link_at(t, seconds)
    return f"[{stamp}]({url})" if url else f"`{stamp}`"


def to_md(t: Transcript, summary: dict | None = None) -> str:
    title = (summary or {}).get("title") or t.title or "Recap"
    out = [f"# {title}", ""]
    src = f"<{t.source}>" if t.source.startswith("http") else f"`{t.source}`"
    out += [f"*Source:* {src}  ", f"*Length:* {fmt_ts(t.duration)} · *Transcribed with:* {t.method}", ""]

    if summary:
        out += ["## TL;DR", "", summary.get("tldr", ""), ""]
        if summary.get("key_points"):
            out += ["## Key points", ""] + [f"- {p}" for p in summary["key_points"]] + [""]
        if summary.get("action_items"):
            out += ["## Action items", ""] + [f"- [ ] {a}" for a in summary["action_items"]] + [""]
        if summary.get("chapters"):
            out += ["## Chapters", ""]
            for c in summary["chapters"]:
                out.append(f"- {_ts_md(t, c['start'])} **{c['title']}**: {c['summary']}")
            out.append("")

    out += ["## Transcript", ""]
    for s in t.segments:
        who = f"**{s.speaker}:** " if s.speaker else ""
        out.append(f"{_ts_md(t, s.start)} {who}{s.text}  ")
    return "\n".join(out) + "\n"


def to_json(t: Transcript, summary: dict | None = None) -> str:
    return json.dumps({"transcript": t.to_dict(), "summary": summary}, ensure_ascii=False, indent=2)


def render(fmt: str, t: Transcript, summary: dict | None = None) -> str:
    if fmt == "txt":
        return to_txt(t)
    if fmt == "srt":
        return to_srt(t)
    if fmt == "md":
        return to_md(t, summary)
    if fmt == "json":
        return to_json(t, summary)
    raise ValueError(f"unknown format {fmt!r}")


def write_all(t: Transcript, summary: dict | None, out_dir: Path, stem: str,
              formats: tuple[str, ...] = FORMATS) -> list[Path]:
    out_dir.mkdir(parents=True, exist_ok=True)
    paths = []
    for fmt in formats:
        p = out_dir / f"{stem}.{fmt}"
        p.write_text(render(fmt, t, summary), encoding="utf-8")
        paths.append(p)
    return paths
