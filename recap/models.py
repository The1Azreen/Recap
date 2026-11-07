"""The one transcript format every source returns.

Summarizing and exporting only ever see these types, so they never need to know whether the
text came from YouTube captions, whisper.cpp on the GPU, or faster-whisper on the CPU.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass, field


@dataclass
class Segment:
    start: float  # seconds
    end: float  # seconds
    text: str
    speaker: str | None = None


@dataclass
class Transcript:
    segments: list[Segment]
    source: str  # URL or file path the user gave
    source_type: str  # "youtube" | "url" | "audio" | "video"
    title: str | None = None
    language: str | None = None
    method: str = ""  # how the text was obtained, e.g. "YouTube captions (en)"
    video_id: str | None = None  # YouTube id, for clickable timestamps
    extra: dict = field(default_factory=dict)

    @property
    def text(self) -> str:
        return " ".join(s.text for s in self.segments)

    @property
    def duration(self) -> float:
        return self.segments[-1].end if self.segments else 0.0

    def to_dict(self) -> dict:
        d = asdict(self)
        d["duration"] = self.duration
        return d

    @classmethod
    def from_dict(cls, d: dict) -> "Transcript":
        d = dict(d)
        d.pop("duration", None)
        d["segments"] = [Segment(**s) for s in d.get("segments", [])]
        return cls(**d)


def merge_segments(segments: list[Segment], max_chars: int = 150, max_span: float = 12.0) -> list[Segment]:
    """Join short caption fragments into readable lines.

    YouTube auto-captions arrive as 2 to 5 second fragments with no punctuation. A line ends at
    sentence punctuation (once it has a little length), at `max_chars`, or after `max_span`
    seconds, whichever comes first.
    """
    out: list[Segment] = []
    cur: Segment | None = None
    for s in segments:
        text = s.text.strip()
        if not text:
            continue
        if cur is None:
            cur = Segment(s.start, s.end, text, s.speaker)
            continue
        cur.text = f"{cur.text} {text}"
        cur.end = max(cur.end, s.end)
        ends_sentence = cur.text.endswith((".", "?", "!", "。", "？", "！")) and len(cur.text) >= 40
        if ends_sentence or len(cur.text) >= max_chars or cur.end - cur.start >= max_span:
            out.append(cur)
            cur = None
    if cur is not None:
        out.append(cur)
    return out


def fmt_ts(seconds: float, *, srt: bool = False, always_hours: bool = False) -> str:
    """00:01:02 / 1:02 / 00:01:02,345 (srt)."""
    seconds = max(0.0, float(seconds))
    ms = int(round(seconds * 1000))
    h, rem = divmod(ms, 3_600_000)
    m, rem = divmod(rem, 60_000)
    s, ms = divmod(rem, 1000)
    if srt:
        return f"{h:02}:{m:02}:{s:02},{ms:03}"
    if h or always_hours:
        return f"{h:02}:{m:02}:{s:02}"
    return f"{m:02}:{s:02}"
