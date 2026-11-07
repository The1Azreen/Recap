//! Transcript (+ optional summary) to .txt / .srt / .md / .json.

use std::path::{Path, PathBuf};

use crate::model::{fmt_srt, fmt_ts, Summary, Transcript};

pub const FORMATS: [&str; 4] = ["md", "srt", "txt", "json"];

/// A URL that opens the source at `seconds`, when the source is a YouTube video.
pub fn link_at(t: &Transcript, seconds: f64) -> Option<String> {
    t.video_id.as_ref().map(|v| format!("https://www.youtube.com/watch?v={v}&t={}s", seconds as u64))
}

fn who(s: &crate::model::Segment) -> String {
    s.speaker.as_ref().map(|w| format!("{w}: ")).unwrap_or_default()
}

pub fn to_txt(t: &Transcript) -> String {
    let mut out = String::new();
    for s in &t.segments {
        out.push_str(&format!("[{}] {}{}\n", fmt_ts(s.start, true), who(s), s.text));
    }
    out
}

pub fn to_srt(t: &Transcript) -> String {
    let mut blocks = Vec::new();
    for (i, s) in t.segments.iter().enumerate() {
        let end = if s.end > s.start { s.end } else { s.start + 1.0 };
        blocks.push(format!("{}\n{} --> {}\n{}{}\n", i + 1, fmt_srt(s.start), fmt_srt(end), who(s), s.text));
    }
    blocks.join("\n")
}

fn ts_md(t: &Transcript, sec: f64) -> String {
    let stamp = fmt_ts(sec, false);
    match link_at(t, sec) {
        Some(url) => format!("[{stamp}]({url})"),
        None => format!("`{stamp}`"),
    }
}

pub fn to_md(t: &Transcript, s: Option<&Summary>) -> String {
    let title = s.map(|s| s.title.as_str()).filter(|x| !x.is_empty()).or(t.title.as_deref()).unwrap_or("Recap");
    let mut out = vec![format!("# {title}"), String::new()];
    let src = if t.source.starts_with("http") { format!("<{}>", t.source) } else { format!("`{}`", t.source) };
    out.push(format!("*Source:* {src}  "));
    out.push(format!("*Length:* {} · *Transcribed with:* {}", fmt_ts(t.duration, false), t.method));
    out.push(String::new());

    if let Some(s) = s {
        out.extend(["## TL;DR".into(), String::new(), s.tldr.clone(), String::new()]);
        if !s.key_points.is_empty() {
            out.extend(["## Key points".into(), String::new()]);
            out.extend(s.key_points.iter().map(|p| format!("- {p}")));
            out.push(String::new());
        }
        if !s.action_items.is_empty() {
            out.extend(["## Action items".into(), String::new()]);
            out.extend(s.action_items.iter().map(|p| format!("- [ ] {p}")));
            out.push(String::new());
        }
        if !s.chapters.is_empty() {
            out.extend(["## Chapters".into(), String::new()]);
            out.extend(s.chapters.iter().map(|c| format!("- {} **{}**: {}", ts_md(t, c.start), c.title, c.summary)));
            out.push(String::new());
        }
    }
    out.extend(["## Transcript".into(), String::new()]);
    for seg in &t.segments {
        let w = seg.speaker.as_ref().map(|w| format!("**{w}:** ")).unwrap_or_default();
        out.push(format!("{} {w}{}  ", ts_md(t, seg.start), seg.text));
    }
    out.join("\n") + "\n"
}

pub fn to_json(t: &Transcript, s: Option<&Summary>) -> String {
    serde_json::to_string_pretty(&serde_json::json!({ "transcript": t, "summary": s })).unwrap_or_default()
}

pub fn render(fmt: &str, t: &Transcript, s: Option<&Summary>) -> String {
    match fmt {
        "txt" => to_txt(t),
        "srt" => to_srt(t),
        "json" => to_json(t, s),
        _ => to_md(t, s),
    }
}

pub fn write_all(t: &Transcript, s: Option<&Summary>, dir: &Path, stem: &str) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    FORMATS
        .iter()
        .map(|f| {
            let p = dir.join(format!("{stem}.{f}"));
            std::fs::write(&p, render(f, t, s)).map_err(|e| format!("Couldn't write {}: {e}", p.display()))?;
            Ok(p)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Chapter, Segment};

    fn t() -> Transcript {
        Transcript {
            segments: vec![
                Segment { start: 0.0, end: 2.5, text: "Hello there.".into(), speaker: None },
                Segment { start: 2.5, end: 65.25, text: "Second line.".into(), speaker: None },
            ],
            source: "https://youtu.be/dQw4w9WgXcQ".into(),
            source_type: "youtube".into(),
            video_id: Some("dQw4w9WgXcQ".into()),
            ..Default::default()
        }
        .finish()
    }

    #[test]
    fn srt() {
        assert!(to_srt(&t()).starts_with("1\n00:00:00,000 --> 00:00:02,500\nHello there.\n"));
    }

    #[test]
    fn md_links_youtube_times() {
        let s = Summary { title: "T".into(), tldr: "x".into(), chapters: vec![Chapter { start: 2.5, title: "C".into(), summary: "s".into() }], ..Default::default() };
        let md = to_md(&t(), Some(&s));
        assert!(md.contains("## TL;DR"));
        assert!(md.contains("[00:02](https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=2s)"));
    }
}
