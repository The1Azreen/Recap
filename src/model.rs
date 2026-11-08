//! The one transcript format every source returns, plus the summary and library metadata.
//! The JSON layout matches the Python version, so both read the same library.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default)]
    pub speaker: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Transcript {
    pub segments: Vec<Segment>,
    pub source: String,
    /// "youtube" | "url" | "audio" | "video"
    pub source_type: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub video_id: Option<String>,
    #[serde(default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub duration: f64,
}

impl Transcript {
    pub fn finish(mut self) -> Self {
        self.duration = self.segments.last().map(|s| s.end).unwrap_or(0.0);
        self
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Chapter {
    pub start: f64,
    pub title: String,
    pub summary: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Summary {
    pub title: String,
    pub tldr: String,
    #[serde(default)]
    pub key_points: Vec<String>,
    #[serde(default)]
    pub action_items: Vec<String>,
    #[serde(default)]
    pub chapters: Vec<Chapter>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Meta {
    pub id: String,
    pub title: String,
    pub source: String,
    pub source_type: String,
    #[serde(default)]
    pub video_id: Option<String>,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub created: f64,
    /// File name (inside the item folder) of audio/video we can play back.
    #[serde(default)]
    pub media: Option<String>,
}

/// Join short caption fragments into readable lines. A line ends at sentence punctuation (once
/// it has a little length), at `max_chars`, or after `max_span` seconds.
pub fn merge_segments(segments: &[Segment], max_chars: usize, max_span: f64) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut cur: Option<Segment> = None;
    for s in segments {
        let text = s.text.trim();
        if text.is_empty() {
            continue;
        }
        match cur.as_mut() {
            None => cur = Some(Segment { start: s.start, end: s.end, text: text.to_string(), speaker: s.speaker.clone() }),
            Some(c) => {
                c.text.push(' ');
                c.text.push_str(text);
                c.end = c.end.max(s.end);
                let ends_sentence = c.text.ends_with(['.', '?', '!', '。', '？', '！']) && c.text.chars().count() >= 40;
                if ends_sentence || c.text.chars().count() >= max_chars || c.end - c.start >= max_span {
                    out.push(cur.take().unwrap());
                }
            }
        }
    }
    out.extend(cur);
    out
}

/// 01:05 / 01:02:05; `always_hours` forces hh:mm:ss.
/// Tidies a title taken from a file name: drops the "[dQw4w9WgXcQ]" video id that yt-dlp adds
/// to downloaded file names, and turns underscores into spaces when there are no spaces at all.
pub fn clean_title(title: &str) -> String {
    let mut t = title.trim();
    if let Some(open) = t.rfind('[') {
        let inner = &t[open + 1..];
        if let Some(id) = inner.strip_suffix(']') {
            if id.len() == 11 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') && open > 0 {
                t = t[..open].trim_end();
            }
        }
    }
    let t = t.trim_end_matches(['-', '_', ' ']).trim();
    let t = if !t.contains(' ') && t.contains('_') { t.replace('_', " ") } else { t.to_string() };
    if t.is_empty() { title.trim().to_string() } else { t }
}

pub fn fmt_ts(seconds: f64, always_hours: bool) -> String {
    let s = seconds.max(0.0) as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 || always_hours {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// 00:01:05,250
pub fn fmt_srt(seconds: f64) -> String {
    let ms = (seconds.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// "[01:02:03]" / "[02:03]" -> seconds.
pub fn parse_ts(s: &str) -> Option<f64> {
    let parts: Vec<&str> = s.split(':').collect();
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|p| p.is_empty() || p.len() > 2 || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    Some(parts.iter().fold(0.0, |acc, p| acc * 60.0 + p.parse::<f64>().unwrap_or(0.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles() {
        assert_eq!(clean_title("Gravity Rush - PlayStation's Forgotten Gem [t2xMG6IA03s]"), "Gravity Rush - PlayStation's Forgotten Gem");
        assert_eq!(clean_title("my_lecture_notes"), "my lecture notes");
        assert_eq!(clean_title("Keep [this part] here"), "Keep [this part] here");
        assert_eq!(clean_title("[t2xMG6IA03s]"), "[t2xMG6IA03s]");
        assert_eq!(clean_title("Plain title"), "Plain title");
    }

    #[test]
    fn merges_fragments() {
        let frags: Vec<Segment> = ["so today", "we will talk", "about", "pixels."]
            .iter()
            .enumerate()
            .map(|(i, t)| Segment { start: i as f64, end: i as f64 + 1.0, text: t.to_string(), speaker: None })
            .collect();
        let m = merge_segments(&frags, 150, 12.0);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].text, "so today we will talk about pixels.");
        assert_eq!((m[0].start, m[0].end), (0.0, 4.0));
    }

    #[test]
    fn splits_long_spans() {
        let frags: Vec<Segment> = (0..10).map(|i| Segment { start: i as f64 * 5.0, end: i as f64 * 5.0 + 5.0, text: "word".into(), speaker: None }).collect();
        let m = merge_segments(&frags, 150, 12.0);
        assert!(m.len() > 1 && m.iter().all(|s| s.end - s.start <= 15.0));
    }

    #[test]
    fn times() {
        assert_eq!(fmt_ts(65.25, false), "01:05");
        assert_eq!(fmt_ts(3725.0, false), "01:02:05");
        assert_eq!(fmt_srt(65.25), "00:01:05,250");
        assert_eq!(parse_ts("01:02:05"), Some(3725.0));
        assert_eq!(parse_ts("1:05"), Some(65.0));
        assert_eq!(parse_ts("ab:cd"), None);
    }

    #[test]
    fn reads_python_json() {
        let j = r#"{"segments":[{"start":0.0,"end":1.5,"text":"hi","speaker":null}],"source":"a.mp3","source_type":"audio","title":"a","language":"en","method":"m","video_id":null,"extra":{},"duration":1.5}"#;
        let t: Transcript = serde_json::from_str(j).unwrap();
        assert_eq!(t.segments[0].text, "hi");
    }
}
