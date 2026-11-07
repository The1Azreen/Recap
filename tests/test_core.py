import json

import pytest

from recap import export, library, summarize
from recap.models import Segment, Transcript, fmt_ts, merge_segments
from recap.sources import kind_of
from recap.sources.youtube import video_id


def _t(**kw):
    segs = [Segment(0, 2.5, "Hello there."), Segment(2.5, 65.25, "Second line, with a comma.")]
    return Transcript(segments=segs, source="https://youtu.be/dQw4w9WgXcQ", source_type="youtube",
                      title="Test", video_id="dQw4w9WgXcQ", method="test", **kw)


@pytest.mark.parametrize("url,vid", [
    ("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "dQw4w9WgXcQ"),
    ("https://www.youtube.com/watch?feature=share&v=dQw4w9WgXcQ&t=10", "dQw4w9WgXcQ"),
    ("https://youtu.be/dQw4w9WgXcQ?si=abc", "dQw4w9WgXcQ"),
    ("https://youtube.com/shorts/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
    ("https://www.youtube.com/embed/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
    ("https://www.youtube.com/live/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
    ("https://vimeo.com/123", None),
])
def test_video_id(url, vid):
    assert video_id(url) == vid


def test_kind_of():
    assert kind_of("https://youtu.be/dQw4w9WgXcQ") == "youtube"
    assert kind_of("https://vimeo.com/1") == "url"
    assert kind_of("talk.MP4") == "video"
    assert kind_of("clip.webm") == "video"
    assert kind_of("memo.m4a") == "audio"


def test_fmt_ts():
    assert fmt_ts(65.25) == "01:05"
    assert fmt_ts(3725) == "01:02:05"
    assert fmt_ts(65.25, srt=True) == "00:01:05,250"
    assert fmt_ts(5, always_hours=True) == "00:00:05"


def test_merge_segments_joins_fragments():
    frags = [Segment(i, i + 1, w) for i, w in enumerate(["so today", "we will talk", "about", "pixels."])]
    merged = merge_segments(frags)
    assert len(merged) == 1
    assert merged[0].text == "so today we will talk about pixels."
    assert (merged[0].start, merged[0].end) == (0, 4)


def test_merge_segments_splits_long_spans():
    frags = [Segment(i * 5, i * 5 + 5, "word") for i in range(10)]
    merged = merge_segments(frags)
    assert all(s.end - s.start <= 15 for s in merged)
    assert len(merged) > 1


def test_chunk_segments_respects_budget():
    segs = [Segment(i, i + 1, "x" * 100) for i in range(50)]
    chunks = summarize.chunk_segments(segs, max_chars=1000)
    assert sum(len(c) for c in chunks) == 50
    assert all(sum(len(s.text) + 12 for s in c) <= 1000 for c in chunks)


def test_srt_and_md():
    t = _t()
    srt = export.to_srt(t)
    assert srt.startswith("1\n00:00:00,000 --> 00:00:02,500\nHello there.\n")
    md = export.to_md(t, {"title": "T", "tldr": "short", "key_points": ["a"], "action_items": [],
                          "chapters": [{"start": 2.5, "title": "C", "summary": "s"}]})
    assert "## TL;DR" in md
    assert "[00:02](https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=2s)" in md


def test_transcript_roundtrip():
    t = _t()
    assert Transcript.from_dict(json.loads(json.dumps(t.to_dict()))) == t


def test_library_save_search(tmp_path, monkeypatch):
    monkeypatch.setattr(library.config, "LIBRARY_DIR", tmp_path)
    item_id = library.save(_t(), None, None)
    assert library.meta(item_id)["title"] == "Test"
    hits = library.search("comma")
    assert hits and hits[0]["start"] == 2.5
    assert library.search("nothing like this") == []
    library.delete(item_id)
    assert library.list_items() == []


def test_summarize_map_reduce(monkeypatch):
    calls = []

    def fake_call(system, user, schema, effort=None, cache_prefix=None):
        calls.append(schema)
        if schema is summarize.NOTES_SCHEMA:
            return json.dumps({"notes": ["[00:00:01] n"], "action_items": [], "chapters": []})
        return json.dumps({"title": "T", "tldr": "x", "key_points": [], "action_items": [], "chapters": []})

    monkeypatch.setattr(summarize, "_call", fake_call)
    segs = [Segment(i, i + 1, "y" * 100) for i in range(30)]
    t = Transcript(segments=segs, source="a.mp3", source_type="audio")
    out = summarize.summarize(t, "meeting", chunk_chars=1000)
    assert out["title"] == "T"
    assert calls.count(summarize.NOTES_SCHEMA) > 1 and calls[-1] is summarize.SUMMARY_SCHEMA
