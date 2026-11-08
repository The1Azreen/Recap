//! Summaries and Q&A with the Claude API (raw HTTPS; there's no official Rust SDK).
//!
//! Short transcripts go to Claude in one request. Long ones are split into chunks on segment
//! boundaries; each chunk is condensed into notes ("map"), then the notes are merged into the
//! final summary ("reduce"). Every line carries its [hh:mm:ss] time so chapter times survive.

use std::time::Duration;

use serde_json::{json, Value};

use crate::model::{fmt_ts, Segment, Summary, Transcript};
use crate::transcribe::Progress;

pub const MODEL: &str = "claude-opus-5-5";
const API: &str = "https://api.anthropic.com/v1/messages";
/// ~120k characters is roughly 30k tokens, about two hours of speech.
pub const CHUNK_CHARS: usize = 120_000;
const MAX_PARALLEL: usize = 4;

pub const STYLES: [(&str, &str); 4] = [
    ("general", "Write a clear general-purpose summary."),
    ("meeting", "This is a meeting. Focus on decisions made, open questions, and who agreed to do what. Action items must name an owner and a deadline when the transcript gives one."),
    ("lecture", "This is a lecture or tutorial. Key points should be the concepts taught, with definitions, formulas and worked examples kept precise. Action items are things a student should review or practise."),
    ("podcast", "This is a podcast or interview. Key points should capture each speaker's main arguments, notable stories and memorable quotes. Action items are any books, links, products or recommendations mentioned."),
];

const SYSTEM: &str = "You summarize transcripts of audio and video for the person who recorded or watched them. \
Transcripts come from speech recognition or captions and may contain misheard words; correct obvious ones from \
context and never invent content that isn't there. Write in the transcript's language. Times in the transcript \
are [hh:mm:ss] markers; chapter start values are those times converted to seconds.";

/// The API key lives in the Windows Credential Manager (or the platform's keychain), not in
/// the settings file.
fn key_entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new("Recap", "anthropic-api-key").map_err(|e| e.to_string())
}

/// The stored API key, if there is one.
pub fn load_key() -> Option<String> {
    key_entry().ok()?.get_password().ok().filter(|k| !k.trim().is_empty())
}

/// Stores the API key; an empty key removes it.
pub fn store_key(key: &str) -> Result<(), String> {
    let entry = key_entry()?;
    if key.trim().is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    } else {
        entry.set_password(key.trim()).map_err(|e| e.to_string())
    }
}

pub struct Client {
    pub api_key: String,
    pub effort: String,
}

fn chapter_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "start": {"type": "number", "description": "Start time in seconds, taken from the [hh:mm:ss] markers."},
            "title": {"type": "string"},
            "summary": {"type": "string", "description": "One or two sentences."}
        },
        "required": ["start", "title", "summary"],
        "additionalProperties": false
    })
}

fn summary_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string", "description": "A short descriptive title."},
            "tldr": {"type": "string", "description": "Two to four sentences."},
            "key_points": {"type": "array", "items": {"type": "string"}},
            "action_items": {"type": "array", "items": {"type": "string"}},
            "chapters": {"type": "array", "items": chapter_schema()}
        },
        "required": ["title", "tldr", "key_points", "action_items", "chapters"],
        "additionalProperties": false
    })
}

fn notes_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "notes": {"type": "array", "items": {"type": "string"}, "description": "Dense factual notes, each starting with its [hh:mm:ss] time."},
            "action_items": {"type": "array", "items": {"type": "string"}},
            "chapters": {"type": "array", "items": chapter_schema()}
        },
        "required": ["notes", "action_items", "chapters"],
        "additionalProperties": false
    })
}

pub fn style_guide(style: &str) -> &'static str {
    STYLES.iter().find(|(k, _)| *k == style).map(|(_, g)| *g).unwrap_or(STYLES[0].1)
}

pub fn format_lines(segs: &[Segment]) -> String {
    segs.iter()
        .map(|s| format!("[{}] {}{}", fmt_ts(s.start, true), s.speaker.as_ref().map(|w| format!("{w}: ")).unwrap_or_default(), s.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Greedy split on segment boundaries so no chunk's text exceeds `max_chars`.
pub fn chunk(segs: &[Segment], max_chars: usize) -> Vec<&[Segment]> {
    let mut out = Vec::new();
    let (mut start, mut size) = (0, 0);
    for (i, s) in segs.iter().enumerate() {
        let n = s.text.len() + 12;
        if i > start && size + n > max_chars {
            out.push(&segs[start..i]);
            start = i;
            size = 0;
        }
        size += n;
    }
    if start < segs.len() {
        out.push(&segs[start..]);
    }
    out
}

fn header(t: &Transcript) -> String {
    let mut h = Vec::new();
    if let Some(title) = &t.title {
        h.push(format!("Title: {title}"));
    }
    h.push(format!("Source: {}", t.source_type));
    h.push(format!("Length: {}", fmt_ts(t.duration, true)));
    h.join("\n")
}

impl Client {
    /// One request; returns the response text. Refusals fall back to another model server-side.
    fn call(&self, system: &str, user: &str, schema: Option<Value>, cache_prefix: Option<&str>) -> Result<String, String> {
        if self.api_key.trim().is_empty() {
            return Err("No Claude API key. Set ANTHROPIC_API_KEY or paste a key in Settings.".into());
        }
        let mut content = Vec::new();
        if let Some(p) = cache_prefix {
            content.push(json!({"type": "text", "text": p, "cache_control": {"type": "ephemeral"}}));
        }
        content.push(json!({"type": "text", "text": user}));
        let mut output_config = json!({"effort": self.effort});
        if let Some(s) = schema {
            output_config["format"] = json!({"type": "json_schema", "schema": s});
        }
        let body = json!({
            "model": MODEL,
            "max_tokens": 16000,
            "system": system,
            "messages": [{"role": "user", "content": content}],
            "output_config": output_config,
            "fallbacks": "default"
        });

        let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(20)).timeout_read(Duration::from_secs(600)).build();
        let mut attempt = 0;
        let resp: Value = loop {
            let r = agent
                .post(API)
                .set("x-api-key", self.api_key.trim())
                .set("anthropic-version", "2026-10-21")
                .set("anthropic-beta", "server-side-fallback-2026-07-01")
                .set("content-type", "application/json")
                .send_json(&body);
            match r {
                Ok(r) => break r.into_json().map_err(|e| format!("Unreadable reply from Claude: {e}"))?,
                Err(ureq::Error::Status(code, r)) => {
                    let detail = r.into_json::<Value>().ok().and_then(|v| v["error"]["message"].as_str().map(str::to_string)).unwrap_or_default();
                    // 429 / 5xx / 529 overloaded: retry a couple of times with backoff
                    if (code == 429 || code >= 500) && attempt < 2 {
                        attempt += 1;
                        std::thread::sleep(Duration::from_secs(5 * attempt));
                        continue;
                    }
                    return Err(match code {
                        401 => "Claude API key is invalid. Check it in Settings.".into(),
                        403 => format!("This API key isn't allowed to do that: {detail}"),
                        429 => "Claude API rate limit hit. Wait a minute and try again.".into(),
                        _ => format!("Claude API error {code}: {detail}"),
                    });
                }
                Err(e) => {
                    if attempt < 2 {
                        attempt += 1;
                        std::thread::sleep(Duration::from_secs(3));
                        continue;
                    }
                    return Err(format!("Couldn't reach the Claude API: {e}"));
                }
            }
        };

        match resp["stop_reason"].as_str() {
            Some("refusal") => return Err("Claude declined to process this content.".into()),
            Some("max_tokens") => return Err("Claude's reply was cut off (too long).".into()),
            _ => {}
        }
        let text: String = resp["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect();
        Ok(text)
    }

    fn call_json<T: serde::de::DeserializeOwned>(&self, user: &str, schema: Value) -> Result<T, String> {
        let text = self.call(SYSTEM, user, Some(schema), None)?;
        serde_json::from_str(&text).map_err(|e| format!("Claude returned malformed JSON: {e}"))
    }

    pub fn summarize(&self, t: &Transcript, style: &str, progress: Progress) -> Result<Summary, String> {
        if t.segments.is_empty() {
            return Err("The transcript is empty; nothing to summarize.".into());
        }
        let guide = style_guide(style);
        let chunks = chunk(&t.segments, CHUNK_CHARS);
        if chunks.len() == 1 {
            progress("summarize", None, "Summarizing with Claude");
            let user = format!(
                "{}\n\n<transcript>\n{}\n</transcript>\n\n{guide} Return the summary. Use empty arrays where nothing applies.",
                header(t),
                format_lines(chunks[0])
            );
            return self.call_json(&user, summary_schema());
        }

        // Map: notes per chunk, a few at a time.
        let n = chunks.len();
        progress("summarize", Some(0.0), &format!("Summarizing {n} parts with Claude"));
        let done = std::sync::atomic::AtomicUsize::new(0);
        let mut parts: Vec<Result<Value, String>> = Vec::with_capacity(n);
        for batch in chunks.iter().enumerate().collect::<Vec<_>>().chunks(MAX_PARALLEL) {
            let results: Vec<Result<Value, String>> = std::thread::scope(|s| {
                let handles: Vec<_> = batch
                    .iter()
                    .map(|(i, c)| {
                        let done = &done;
                        s.spawn(move || {
                            let user = format!(
                                "{}\nThis is part {} of {n} ({} to {}).\n\n<transcript>\n{}\n</transcript>\n\n{guide} Take detailed notes on this part only; they will be merged with the other parts' notes into one summary, so keep every fact, name and number that matters.",
                                header(t),
                                i + 1,
                                fmt_ts(c[0].start, true),
                                fmt_ts(c[c.len() - 1].end, true),
                                format_lines(c)
                            );
                            let r = self.call_json::<Value>(&user, notes_schema());
                            let k = done.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                            progress("summarize", Some(k as f32 / (n + 1) as f32), &format!("Summarized part {k} of {n}"));
                            r
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("worker crashed".into()))).collect()
            });
            parts.extend(results);
        }
        let parts: Vec<Value> = parts.into_iter().collect::<Result<_, _>>()?;

        // Reduce.
        progress("summarize", Some(n as f32 / (n + 1) as f32), "Combining parts");
        let notes = parts.iter().enumerate().map(|(i, p)| format!("<part n=\"{}\">\n{p}\n</part>", i + 1)).collect::<Vec<_>>().join("\n\n");
        let user = format!(
            "{}\n\nThe transcript was too long for one pass, so each part was condensed into notes, in order:\n\n{notes}\n\n{guide} Write one summary of the whole recording from these notes. Merge chapters that belong together so there are roughly 4 to 12, and remove duplicate action items. Use empty arrays where nothing applies.",
            header(t)
        );
        self.call_json(&user, summary_schema())
    }

    /// Answer a question about one transcript. The transcript is prompt-cached, so follow-up
    /// questions about the same recording are cheaper and faster.
    pub fn ask(&self, t: &Transcript, question: &str) -> Result<String, String> {
        let prefix = format!("{}\n\n<transcript>\n{}\n</transcript>", header(t), format_lines(&t.segments));
        let system = format!(
            "{SYSTEM} Answer questions about the transcript using only what it says. Cite the [hh:mm:ss] times you rely on, inline, in that same bracket format. If the transcript doesn't cover the question, say so. Answer in plain text without Markdown."
        );
        Ok(self.call(&system, question.trim(), None, Some(&prefix))?.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_respect_budget() {
        let segs: Vec<Segment> = (0..50).map(|i| Segment { start: i as f64, end: i as f64 + 1.0, text: "x".repeat(100), speaker: None }).collect();
        let c = chunk(&segs, 1000);
        assert_eq!(c.iter().map(|c| c.len()).sum::<usize>(), 50);
        assert!(c.iter().all(|c| c.iter().map(|s| s.text.len() + 12).sum::<usize>() <= 1000));
    }

    #[test]
    fn summary_json_shape_parses() {
        let s: Summary = serde_json::from_value(json!({"title":"T","tldr":"x","key_points":["a"],"action_items":[],"chapters":[{"start":1.5,"title":"c","summary":"s"}]})).unwrap();
        assert_eq!(s.chapters[0].start, 1.5);
    }
}

#[cfg(test)]
mod key_tests {
    /// Round-trips a throwaway entry through the real Credential Manager.
    #[test]
    fn credential_store_round_trip() {
        let e = keyring::Entry::new("Recap-test", "round-trip").unwrap();
        e.set_password("sk-test-123").unwrap();
        assert_eq!(e.get_password().unwrap(), "sk-test-123");
        e.delete_credential().unwrap();
        assert!(matches!(e.get_password(), Err(keyring::Error::NoEntry)));
    }
}
