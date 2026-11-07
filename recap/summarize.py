"""Summaries and Q&A with Claude.

Short transcripts go to Claude in one request. Long ones are split into chunks on segment
boundaries; each chunk is condensed into notes ("map"), then the notes are merged into the
final summary ("reduce"). Every transcript line carries its [hh:mm:ss] start time, so chapter
times survive both steps.
"""

from __future__ import annotations

import json
from concurrent.futures import ThreadPoolExecutor
from typing import Callable

from . import config
from .config import RecapError
from .models import Segment, Transcript, fmt_ts

Progress = Callable[[str, float | None, str], None]

# Claude's context is far larger than this; chunking keeps each request quick and lets long
# recordings be summarized in parallel. ~120k characters is roughly 30k tokens.
DEFAULT_CHUNK_CHARS = 120_000
MAX_PARALLEL = 4

STYLES: dict[str, str] = {
    "general": "Write a clear general-purpose summary.",
    "meeting": (
        "This is a meeting. Focus on decisions made, open questions, and who agreed to do what. "
        "Action items must name an owner and a deadline when the transcript gives one."
    ),
    "lecture": (
        "This is a lecture or tutorial. Key points should be the concepts taught, with definitions, "
        "formulas and worked examples kept precise. Action items are things a student should review "
        "or practise."
    ),
    "podcast": (
        "This is a podcast or interview. Key points should capture each speaker's main arguments, "
        "notable stories and memorable quotes. Action items are any books, links, products or "
        "recommendations mentioned."
    ),
}

_CHAPTER = {
    "type": "object",
    "properties": {
        "start": {"type": "number", "description": "Start time in seconds, taken from the [hh:mm:ss] markers."},
        "title": {"type": "string"},
        "summary": {"type": "string", "description": "One or two sentences."},
    },
    "required": ["start", "title", "summary"],
    "additionalProperties": False,
}

SUMMARY_SCHEMA = {
    "type": "object",
    "properties": {
        "title": {"type": "string", "description": "A short descriptive title."},
        "tldr": {"type": "string", "description": "Two to four sentences."},
        "key_points": {"type": "array", "items": {"type": "string"}},
        "action_items": {"type": "array", "items": {"type": "string"}},
        "chapters": {"type": "array", "items": _CHAPTER},
    },
    "required": ["title", "tldr", "key_points", "action_items", "chapters"],
    "additionalProperties": False,
}

NOTES_SCHEMA = {
    "type": "object",
    "properties": {
        "notes": {"type": "array", "items": {"type": "string"},
                  "description": "Dense factual notes, each starting with its [hh:mm:ss] time."},
        "action_items": {"type": "array", "items": {"type": "string"}},
        "chapters": {"type": "array", "items": _CHAPTER},
    },
    "required": ["notes", "action_items", "chapters"],
    "additionalProperties": False,
}

_SYSTEM = (
    "You summarize transcripts of audio and video for the person who recorded or watched them. "
    "Transcripts come from speech recognition or captions and may contain misheard words; "
    "correct obvious ones from context and never invent content that isn't there. "
    "Write in the transcript's language. Times in the transcript are [hh:mm:ss] markers; "
    "chapter start values are those times converted to seconds."
)


def _client():
    import anthropic
    return anthropic.Anthropic()


def _call(system: str, user: str, schema: dict | None, effort: str | None = None,
          cache_prefix: str | None = None) -> str:
    """One streamed request; returns the response text. Refusals fall back server-side."""
    import anthropic

    content: list[dict] = []
    if cache_prefix:
        content.append({"type": "text", "text": cache_prefix, "cache_control": {"type": "ephemeral"}})
    content.append({"type": "text", "text": user})
    output_config: dict = {"effort": effort or config.CLAUDE_EFFORT}
    if schema:
        output_config["format"] = {"type": "json_schema", "schema": schema}

    try:
        with _client().beta.messages.stream(
            model=config.CLAUDE_MODEL,
            max_tokens=32000,
            betas=["server-side-fallback-2026-07-01"],
            fallbacks="default",
            output_config=output_config,
            system=system,
            messages=[{"role": "user", "content": content}],
        ) as stream:
            msg = stream.get_final_message()
    except anthropic.AuthenticationError as e:
        raise RecapError("Claude API key missing or invalid. Set ANTHROPIC_API_KEY.") from e
    except anthropic.RateLimitError as e:
        raise RecapError("Claude API rate limit hit. Wait a minute and try again.") from e
    except anthropic.APIStatusError as e:
        raise RecapError(f"Claude API error {e.status_code}: {e.message}") from e
    except anthropic.APIConnectionError as e:
        raise RecapError("Couldn't reach the Claude API. Check your connection.") from e

    if msg.stop_reason == "refusal":
        raise RecapError("Claude declined to summarize this content.")
    if msg.stop_reason == "max_tokens":
        raise RecapError("The summary was cut off. Try a smaller --chunk-chars.")
    return "".join(b.text for b in msg.content if b.type == "text")


def format_lines(segments: list[Segment]) -> str:
    return "\n".join(
        f"[{fmt_ts(s.start, always_hours=True)}] {f'{s.speaker}: ' if s.speaker else ''}{s.text}"
        for s in segments
    )


def chunk_segments(segments: list[Segment], max_chars: int = DEFAULT_CHUNK_CHARS) -> list[list[Segment]]:
    """Greedy split on segment boundaries so no chunk's text exceeds max_chars."""
    chunks: list[list[Segment]] = []
    cur: list[Segment] = []
    size = 0
    for s in segments:
        n = len(s.text) + 12  # + the timestamp prefix
        if cur and size + n > max_chars:
            chunks.append(cur)
            cur, size = [], 0
        cur.append(s)
        size += n
    if cur:
        chunks.append(cur)
    return chunks


def _header(t: Transcript) -> str:
    bits = [f"Title: {t.title}" if t.title else None, f"Source: {t.source_type}",
            f"Length: {fmt_ts(t.duration, always_hours=True)}"]
    return "\n".join(b for b in bits if b)


def summarize(t: Transcript, style: str = "general", *, chunk_chars: int = DEFAULT_CHUNK_CHARS,
              progress: Progress | None = None) -> dict:
    if style not in STYLES:
        raise RecapError(f"Unknown style {style!r}. Choose from: {', '.join(STYLES)}")
    if not t.segments:
        raise RecapError("The transcript is empty; nothing to summarize.")
    progress = progress or (lambda *a: None)
    guide = STYLES[style]
    chunks = chunk_segments(t.segments, chunk_chars)

    if len(chunks) == 1:
        progress("summarize", None, "Summarizing with Claude")
        user = (f"{_header(t)}\n\n<transcript>\n{format_lines(chunks[0])}\n</transcript>\n\n"
                f"{guide} Return the summary. Use empty arrays where nothing applies.")
        return json.loads(_call(_SYSTEM, user, SUMMARY_SCHEMA))

    # Map: notes per chunk, in parallel.
    done = 0

    def note(i_chunk):
        nonlocal done
        i, chunk = i_chunk
        user = (f"{_header(t)}\nThis is part {i + 1} of {len(chunks)} "
                f"({fmt_ts(chunk[0].start, always_hours=True)} to {fmt_ts(chunk[-1].end, always_hours=True)}).\n\n"
                f"<transcript>\n{format_lines(chunk)}\n</transcript>\n\n"
                f"{guide} Take detailed notes on this part only; they will be merged with the other "
                f"parts' notes into one summary, so keep every fact, name and number that matters.")
        out = json.loads(_call(_SYSTEM, user, NOTES_SCHEMA))
        done += 1
        progress("summarize", done / (len(chunks) + 1), f"Summarized part {done} of {len(chunks)}")
        return out

    progress("summarize", 0.0, f"Summarizing {len(chunks)} parts with Claude")
    with ThreadPoolExecutor(MAX_PARALLEL) as pool:
        parts = list(pool.map(note, enumerate(chunks)))

    # Reduce: one summary from all the notes.
    progress("summarize", len(chunks) / (len(chunks) + 1), "Combining parts")
    notes = "\n\n".join(f"<part n=\"{i + 1}\">\n{json.dumps(p, ensure_ascii=False)}\n</part>"
                        for i, p in enumerate(parts))
    user = (f"{_header(t)}\n\nThe transcript was too long for one pass, so each part was condensed "
            f"into notes, in order:\n\n{notes}\n\n{guide} Write one summary of the whole recording "
            f"from these notes. Merge chapters that belong together so there are roughly 4 to 12, and "
            f"remove duplicate action items. Use empty arrays where nothing applies.")
    return json.loads(_call(_SYSTEM, user, SUMMARY_SCHEMA))


def ask(t: Transcript, question: str) -> str:
    """Answer a question about one transcript. The transcript is prompt-cached, so follow-up
    questions about the same recording are cheaper and faster."""
    if not question.strip():
        raise RecapError("Ask a question.")
    prefix = f"{_header(t)}\n\n<transcript>\n{format_lines(t.segments)}\n</transcript>"
    system = (_SYSTEM + " Answer questions about the transcript using only what it says. Cite the "
              "[hh:mm:ss] times you rely on, inline, in that same bracket format. If the transcript "
              "doesn't cover the question, say so. Answer in plain text without Markdown headings.")
    return _call(system, question.strip(), None, cache_prefix=prefix).strip()
