//! Sources turn whatever the user gave into a Transcript.
//!
//! * YouTube link: the captions the video already has (via yt-dlp, no download). With no
//!   captions, the audio is downloaded and transcribed.
//! * Other links: audio downloaded with yt-dlp, then transcribed.
//! * Audio / video files: transcribed directly (ffmpeg pulls the audio out of video).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::model::{merge_segments, Segment, Transcript};
use crate::tools;
use crate::transcribe::{self, Progress};

pub const VIDEO_EXTS: &[&str] = &["webm", "mp4", "mov", "mkv", "avi", "m4v", "wmv", "flv", "mpg", "mpeg", "3gp", "ts"];
pub const AUDIO_EXTS: &[&str] = &["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "wma", "aiff", "aif", "amr"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    YouTube,
    Url,
    Audio,
    Video,
}

pub struct Options {
    pub model: String,
    pub language: Option<String>,
    pub force_transcribe: bool,
}

fn valid_id(s: &str) -> Option<String> {
    let id: String = s.chars().take(11).collect();
    (id.len() == 11 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')).then_some(id)
}

/// The 11-character video id of a YouTube URL.
pub fn video_id(url: &str) -> Option<String> {
    if let Some(rest) = url.split("youtu.be/").nth(1) {
        return valid_id(rest);
    }
    let path = url.split("youtube.com/").nth(1).or_else(|| url.split("youtube-nocookie.com/").nth(1))?;
    if let Some(query) = path.strip_prefix("watch?") {
        return query.split('&').find_map(|kv| kv.strip_prefix("v=")).and_then(valid_id);
    }
    ["embed/", "shorts/", "live/", "v/"].iter().find_map(|p| path.strip_prefix(p)).and_then(valid_id)
}

pub fn kind_of(src: &str) -> Result<Kind, String> {
    if video_id(src).is_some() {
        return Ok(Kind::YouTube);
    }
    if src.starts_with("http://") || src.starts_with("https://") {
        return Ok(Kind::Url);
    }
    let ext = Path::new(src).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if VIDEO_EXTS.contains(&ext.as_str()) {
        Ok(Kind::Video)
    } else if AUDIO_EXTS.contains(&ext.as_str()) {
        Ok(Kind::Audio)
    } else {
        Err(format!("Don't know how to read \"{src}\". Give a YouTube link, or an audio or video file."))
    }
}

pub fn is_media_file(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).is_some_and(|e| VIDEO_EXTS.contains(&e.as_str()) || AUDIO_EXTS.contains(&e.as_str()))
}

/// Any source in, one Transcript out. Also returns the local media file to keep for playback,
/// if there is one (the original file, or audio yt-dlp downloaded) and the temp dir holding it.
pub fn load(src: &str, opts: &Options, progress: Progress) -> Result<(Transcript, Option<PathBuf>, Option<tools::TempDir>), String> {
    match kind_of(src)? {
        Kind::YouTube | Kind::Url => load_url(src, opts, progress),
        kind => {
            let path = PathBuf::from(src);
            if !path.is_file() {
                return Err(format!("File not found: {src}"));
            }
            if kind == Kind::Video {
                progress("extract", None, "Extracting audio with ffmpeg");
            }
            let r = transcribe::transcribe(&path, &opts.model, opts.language.as_deref(), progress)?;
            let t = Transcript {
                segments: r.segments,
                source: src.to_string(),
                source_type: if kind == Kind::Video { "video" } else { "audio" }.into(),
                title: path.file_stem().map(|s| s.to_string_lossy().into_owned()),
                language: r.language,
                method: r.method,
                ..Default::default()
            };
            Ok((t.finish(), Some(path), None))
        }
    }
}

fn yt_dlp() -> Result<PathBuf, String> {
    tools::yt_dlp().ok_or_else(|| "yt-dlp not found. Download yt-dlp.exe (github.com/yt-dlp/yt-dlp) and put it next to recap.exe or on PATH.".into())
}

fn load_url(url: &str, opts: &Options, progress: Progress) -> Result<(Transcript, Option<PathBuf>, Option<tools::TempDir>), String> {
    let vid = video_id(url);
    if let (Some(v), false) = (&vid, opts.force_transcribe) {
        progress("captions", None, "Fetching YouTube captions");
        // Captions are only a shortcut: if YouTube refuses them (e.g. HTTP 429 rate limiting),
        // transcribe the audio instead of failing the whole recap.
        if let Ok(Some(t)) = captions(url, v, opts.language.as_deref()) {
            return Ok((t, None, None));
        }
    }
    progress("download", None, if vid.is_some() { "No captions available, downloading audio" } else { "Downloading audio" });
    let tmp = tools::TempDir::new().map_err(|e| e.to_string())?;
    let (audio, title) = download_audio(url, &tmp.0, progress)?;
    let r = transcribe::transcribe(&audio, &opts.model, opts.language.as_deref(), progress)?;
    let t = Transcript {
        segments: r.segments,
        source: url.to_string(),
        source_type: if vid.is_some() { "youtube" } else { "url" }.into(),
        title,
        language: r.language,
        method: r.method,
        video_id: vid,
        ..Default::default()
    };
    Ok((t.finish(), Some(audio), Some(tmp)))
}

/// Picks one caption track from yt-dlp's info: human-made captions in the wanted language first,
/// then auto captions in the video's original language. Asking for every matching track at once
/// (auto-dubbed videos have 20+ `xx-orig` tracks) gets rate limited by YouTube (HTTP 429).
fn pick_track(info: &serde_json::Value, language: Option<&str>) -> Option<String> {
    let keys = |field: &str| -> Vec<String> { info[field].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default() };
    let (manual, auto) = (keys("subtitles"), keys("automatic_captions"));
    let orig = info["language"].as_str().and_then(|l| l.split('-').next()).map(str::to_lowercase);
    let wanted: Vec<String> = match language {
        Some(l) => vec![l.to_lowercase()],
        None => std::iter::once("en".to_string()).chain(orig.clone()).collect(),
    };
    let matches = |k: &str, l: &str| {
        let k = k.to_lowercase();
        k == l || k.starts_with(&format!("{l}-"))
    };
    // human-made captions (not live chat), shortest code first: "en" before "en-GB"
    let mut human: Vec<&String> = manual.iter().filter(|k| *k != "live_chat" && wanted.iter().any(|l| matches(k, l))).collect();
    human.sort_by_key(|k| k.len());
    if let Some(k) = human.first() {
        return Some((*k).clone());
    }
    // auto captions: the original-language track first
    let base = language.map(str::to_lowercase).or(orig).unwrap_or_else(|| "en".into());
    let prefs = [format!("{base}-orig"), base, "en-orig".into(), "en".into()];
    prefs.iter().find(|p| auto.contains(p)).cloned().or_else(|| auto.iter().find(|k| k.ends_with("-orig")).cloned())
}

/// Gets the video's captions without downloading the video: first the video info (one
/// request), then only the single best caption track.
fn captions(url: &str, vid: &str, language: Option<&str>) -> Result<Option<Transcript>, String> {
    let ytdlp = yt_dlp()?;
    let tmp = tools::TempDir::new().map_err(|e| e.to_string())?;
    let info_path = tmp.0.join("v.info.json");
    let out = tools::command(&ytdlp)
        .args(["--skip-download", "--write-info-json", "--no-playlist", "--no-warnings", "--ignore-no-formats-error", "-o"])
        .arg(tmp.0.join("v.%(ext)s"))
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Couldn't run yt-dlp: {e}"))?;
    let info: serde_json::Value = std::fs::read_to_string(&info_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .ok_or_else(|| format!("yt-dlp couldn't read the video: {}", String::from_utf8_lossy(&out.stderr).trim()))?;
    let Some(track) = pick_track(&info, language) else { return Ok(None) };

    // Reuse the info already fetched instead of loading the page again.
    let out = tools::command(&ytdlp)
        .args(["--skip-download", "--write-subs", "--write-auto-subs", "--sub-format", "json3", "--sub-langs", &track])
        .args(["--no-warnings", "--ignore-no-formats-error", "--load-info-json"])
        .arg(&info_path)
        .arg("-o")
        .arg(tmp.0.join("v.%(ext)s"))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Couldn't run yt-dlp: {e}"))?;
    if !out.status.success() {
        return Err(format!("Couldn't fetch captions: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }

    // v.<lang>.json3 files that were written
    let files: Vec<(String, PathBuf)> = std::fs::read_dir(&tmp.0)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let lang = name.strip_prefix("v.")?.strip_suffix(".json3")?.to_string();
            Some((lang, e.path()))
        })
        .collect();
    if files.is_empty() {
        return Ok(None);
    }
    let manual = |l: &str| info["subtitles"].get(l).is_some();
    let rank = |l: &str| -> u8 {
        match (manual(l), l.ends_with("-orig")) {
            (true, _) => 0,
            (false, true) => 1,
            _ => 2,
        }
    };
    let (lang, path) = files.iter().min_by_key(|(l, _)| (rank(l), l.len())).unwrap();
    let data: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;

    let mut raw: Vec<Segment> = Vec::new();
    for ev in data["events"].as_array().into_iter().flatten() {
        let Some(segs) = ev["segs"].as_array() else { continue };
        let text: String = segs.iter().filter_map(|s| s["utf8"].as_str()).collect::<String>().replace('\n', " ");
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let start = ev["tStartMs"].as_f64().unwrap_or(0.0) / 1000.0;
        let dur = ev["dDurationMs"].as_f64().unwrap_or(0.0) / 1000.0;
        raw.push(Segment { start, end: start + dur, text: text.to_string(), speaker: None });
    }
    // Auto captions overlap; end each line where the next begins.
    for i in 0..raw.len().saturating_sub(1) {
        let next = raw[i + 1].start;
        if next > raw[i].start {
            raw[i].end = raw[i].end.min(next);
        }
    }
    let segments = merge_segments(&raw, 150, 12.0);
    if segments.is_empty() {
        return Ok(None);
    }
    let kind = if manual(lang) { "uploaded" } else { "auto-generated" };
    let code = lang.trim_end_matches("-orig").to_string();
    Ok(Some(
        Transcript {
            segments,
            source: url.to_string(),
            source_type: "youtube".into(),
            title: info["title"].as_str().map(str::to_string),
            language: Some(code.clone()),
            method: format!("YouTube captions ({kind}, {code})"),
            video_id: Some(vid.to_string()),
            ..Default::default()
        }
        .finish(),
    ))
}

/// Downloads the best audio stream into `dir`. Returns (file, title).
pub fn download_audio(url: &str, dir: &Path, progress: Progress) -> Result<(PathBuf, Option<String>), String> {
    let ytdlp = yt_dlp()?;
    let mut child = tools::command(&ytdlp)
        .args(["-f", "bestaudio/best", "--no-playlist", "--no-warnings", "--newline", "--progress", "--no-simulate"])
        .args(["--print", "before_dl:TITLE %(title)s", "--print", "after_move:FILE %(filepath)s"])
        .args(["--progress-template", "download:PROG %(progress._percent_str)s"])
        .arg("-o")
        .arg(dir.join("audio.%(ext)s"))
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't run yt-dlp: {e}"))?;
    let mut title = None;
    let mut file = None;
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if let Some(t) = line.strip_prefix("TITLE ") {
                title = Some(t.trim().to_string());
            } else if let Some(f) = line.strip_prefix("FILE ") {
                file = Some(PathBuf::from(f.trim()));
            } else if let Some(p) = line.trim().strip_prefix("PROG ") {
                let pct = p.trim().trim_end_matches('%').trim().parse::<f32>().ok();
                progress("download", pct.map(|p| p / 100.0), "Downloading audio");
            }
        }
    }
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = std::io::Read::read_to_string(&mut e, &mut err);
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    match file.filter(|f| f.is_file()) {
        Some(f) if status.success() => Ok((f, title)),
        _ => Err(format!("Couldn't download {url}: {}", err.lines().last().unwrap_or("yt-dlp failed"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        for (u, want) in [
            ("https://www.youtube.com/watch?v=dQw4w9WgXcQ", Some("dQw4w9WgXcQ")),
            ("https://www.youtube.com/watch?feature=share&v=dQw4w9WgXcQ&t=10", Some("dQw4w9WgXcQ")),
            ("https://youtu.be/dQw4w9WgXcQ?si=abc", Some("dQw4w9WgXcQ")),
            ("https://youtube.com/shorts/dQw4w9WgXcQ", Some("dQw4w9WgXcQ")),
            ("https://www.youtube.com/embed/dQw4w9WgXcQ", Some("dQw4w9WgXcQ")),
            ("https://www.youtube.com/live/dQw4w9WgXcQ", Some("dQw4w9WgXcQ")),
            ("https://vimeo.com/123", None),
        ] {
            assert_eq!(video_id(u).as_deref(), want, "{u}");
        }
    }

    #[test]
    fn kinds() {
        assert_eq!(kind_of("https://youtu.be/dQw4w9WgXcQ"), Ok(Kind::YouTube));
        assert_eq!(kind_of("https://vimeo.com/1"), Ok(Kind::Url));
        assert_eq!(kind_of("clip.WEBM"), Ok(Kind::Video));
        assert_eq!(kind_of("memo.m4a"), Ok(Kind::Audio));
        assert!(kind_of("notes.docx").is_err());
    }

    #[test]
    fn picks_one_caption_track() {
        let dubbed = serde_json::json!({
            "language": "en-US",
            "subtitles": {},
            "automatic_captions": {"ar-orig": [], "de-orig": [], "en": [], "en-orig": [], "fr": [], "ja-orig": []}
        });
        assert_eq!(pick_track(&dubbed, None).as_deref(), Some("en-orig"));
        assert_eq!(pick_track(&dubbed, Some("ja")).as_deref(), Some("ja-orig"));
        let human = serde_json::json!({"language": "fr", "subtitles": {"en-GB": [], "en": [], "live_chat": []}, "automatic_captions": {"fr-orig": []}});
        assert_eq!(pick_track(&human, None).as_deref(), Some("en"));
        let french = serde_json::json!({"language": "fr", "subtitles": {}, "automatic_captions": {"fr-orig": [], "en": []}});
        assert_eq!(pick_track(&french, None).as_deref(), Some("fr-orig"));
        let none = serde_json::json!({"language": "en", "subtitles": {}, "automatic_captions": {}});
        assert_eq!(pick_track(&none, None), None);
    }
}
