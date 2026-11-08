//! Sources turn whatever the user gave into a Transcript.
//!
//! * YouTube link: the captions the video already has (via yt-dlp, no download). With no
//!   captions, the audio is downloaded and transcribed.
//! * Other links: audio downloaded with yt-dlp, then transcribed.
//! * Audio / video files: transcribed directly (ffmpeg pulls the audio out of video).
//! * Subtitle files (.srt / .vtt): read as they are, no transcription.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::model::{clean_title, merge_segments, Segment, Transcript};
use crate::tools;
use crate::transcribe::{self, Progress};

pub const VIDEO_EXTS: &[&str] = &["webm", "mp4", "mov", "mkv", "avi", "m4v", "wmv", "flv", "mpg", "mpeg", "3gp", "ts"];
pub const AUDIO_EXTS: &[&str] = &["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "wma", "aiff", "aif", "amr"];
pub const SUBTITLE_EXTS: &[&str] = &["srt", "vtt"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    YouTube,
    Url,
    Audio,
    Video,
    Subtitles,
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
    } else if SUBTITLE_EXTS.contains(&ext.as_str()) {
        Ok(Kind::Subtitles)
    } else {
        Err(format!("Don't know how to read \"{src}\". Give a YouTube link, an audio or video file, or an .srt / .vtt subtitle file."))
    }
}

/// True for a link to a whole YouTube playlist (not a single video that happens to be in one).
pub fn is_playlist(url: &str) -> bool {
    let yt = url.contains("youtube.com/") || url.contains("youtu.be/");
    yt && (url.contains("/playlist?") || (url.contains("list=") && video_id(url).is_none()))
}

/// The video links in a playlist, read without downloading anything.
pub fn playlist_entries(url: &str) -> Result<Vec<String>, String> {
    let out = tools::command(&yt_dlp()?)
        .args(["--flat-playlist", "--no-warnings", "--print", "url"])
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Couldn't run yt-dlp: {e}"))?;
    let urls: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::trim).filter(|l| l.starts_with("http")).map(str::to_string).collect();
    if urls.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("Couldn't read the playlist: {}", err.lines().last().unwrap_or("no videos found")));
    }
    Ok(urls)
}

/// Anything Recap can make a recap from: audio, video or a subtitle file.
pub fn is_media_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| VIDEO_EXTS.contains(&e.as_str()) || AUDIO_EXTS.contains(&e.as_str()) || SUBTITLE_EXTS.contains(&e.as_str()))
}

/// Any source in, one Transcript out. Also returns the local media file to keep for playback,
/// if there is one (the original file, or audio yt-dlp downloaded) and the temp dir holding it.
pub fn load(src: &str, opts: &Options, progress: Progress) -> Result<(Transcript, Option<PathBuf>, Option<tools::TempDir>), String> {
    match kind_of(src)? {
        Kind::YouTube | Kind::Url => load_url(src, opts, progress),
        Kind::Subtitles => {
            let path = PathBuf::from(src);
            progress("subtitles", None, "Reading subtitles");
            let text = std::fs::read(&path).map_err(|e| format!("Couldn't read {src}: {e}"))?;
            let raw = parse_subtitles(&String::from_utf8_lossy(&text));
            if raw.is_empty() {
                return Err(format!("No subtitle lines found in {src}"));
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("srt").to_ascii_lowercase();
            let t = Transcript {
                segments: merge_segments(&raw, 150, 12.0),
                source: src.to_string(),
                source_type: "subtitles".into(),
                title: path.file_stem().map(|s| clean_title(strip_lang(&s.to_string_lossy()))),
                method: format!("Imported subtitles (.{ext})"),
                ..Default::default()
            };
            // A video or audio file with the same name next to the subtitles is kept for playback.
            Ok((t.finish(), sibling_media(&path), None))
        }
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
                title: path.file_stem().map(|s| clean_title(&s.to_string_lossy())),
                language: r.language,
                method: r.method,
                ..Default::default()
            };
            Ok((t.finish(), Some(path), None))
        }
    }
}

/// "en", "pt-BR", "en-orig": a 2-3 letter language code with an optional suffix.
fn is_lang_code(s: &str) -> bool {
    let (code, rest) = s.split_once('-').unwrap_or((s, ""));
    (2..=3).contains(&code.len()) && code.chars().all(|c| c.is_ascii_alphabetic()) && rest.len() <= 8 && rest.chars().all(|c| c.is_ascii_alphanumeric())
}

/// "Talk.en-orig" -> "Talk": subtitle files are often named with their language.
fn strip_lang(stem: &str) -> &str {
    match stem.rsplit_once('.') {
        Some((base, lang)) if !base.is_empty() && is_lang_code(lang) => base,
        _ => stem,
    }
}

/// "Talk.mp4" next to "Talk.srt" (or "Talk.en.srt").
fn sibling_media(subs: &Path) -> Option<PathBuf> {
    let dir = subs.parent()?;
    let stem = subs.file_stem()?.to_string_lossy().into_owned();
    // "Talk.en" -> also try "Talk"
    let stems = [stem.clone(), strip_lang(&stem).to_string()];
    stems.iter().filter(|s| !s.is_empty()).find_map(|s| {
        VIDEO_EXTS.iter().chain(AUDIO_EXTS).map(|e| dir.join(format!("{s}.{e}"))).find(|p| p.is_file())
    })
}

/// Parses SRT or WebVTT text into lines. Styling tags are dropped, and repeated lines (as in
/// YouTube's rolling auto captions) are kept once.
pub fn parse_subtitles(text: &str) -> Vec<Segment> {
    fn time(s: &str) -> Option<f64> {
        // 01:02:03,456 | 01:02:03.456 | 02:03.456
        let s = s.trim().replace(',', ".");
        let (hms, frac) = s.split_once('.').unwrap_or((&s, "0"));
        let parts: Vec<f64> = hms.split(':').map(|p| p.trim().parse::<f64>()).collect::<Result<_, _>>().ok()?;
        let secs = match parts.as_slice() {
            [h, m, s] => h * 3600.0 + m * 60.0 + s,
            [m, s] => m * 60.0 + s,
            _ => return None,
        };
        let frac: f64 = format!("0.{}", frac.chars().take_while(char::is_ascii_digit).collect::<String>()).parse().unwrap_or(0.0);
        Some(secs + frac)
    }
    fn clean(line: &str) -> String {
        let mut out = String::new();
        let mut depth = 0i32;
        for c in line.chars() {
            match c {
                '<' | '{' => depth += 1,
                '>' | '}' if depth > 0 => depth -= 1,
                _ if depth == 0 => out.push(c),
                _ => {}
            }
        }
        out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ").split_whitespace().collect::<Vec<_>>().join(" ")
    }

    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n").replace('\r', "\n");
    let mut segs: Vec<Segment> = Vec::new();
    let (mut prev, mut prev_end): (Vec<String>, f64) = (Vec::new(), 0.0);
    for block in text.split("\n\n") {
        let mut lines = block.lines().map(str::trim).filter(|l| !l.is_empty());
        // skip the cue number / id, find the timing line
        let Some(timing) = lines.by_ref().find(|l| l.contains("-->")) else { continue };
        let (a, b) = timing.split_once("-->").unwrap_or_default();
        let b = b.split_whitespace().next().unwrap_or(""); // VTT settings follow the end time
        let (Some(start), Some(end)) = (time(a), time(b)) else { continue };
        let cue: Vec<String> = lines.map(clean).filter(|l| !l.is_empty()).collect();
        // Rolling captions (YouTube auto captions) show the previous line again above the new
        // one, and grow a line word by word: keep only what wasn't on screen in the last cue.
        let rolling = start < prev_end + 0.5;
        let mut fresh: Vec<&str> = Vec::new();
        for line in &cue {
            if prev.contains(line) {
                continue;
            }
            match prev.iter().find(|p| rolling && line.starts_with(p.as_str())) {
                Some(p) => fresh.push(line[p.len()..].trim()),
                None => fresh.push(line),
            }
        }
        let body = fresh.into_iter().filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
        prev = cue;
        prev_end = end;
        if !body.is_empty() {
            segs.push(Segment { start, end: end.max(start), text: body, speaker: None });
        }
    }
    segs.sort_by(|a, b| a.start.total_cmp(&b.start));
    segs
}

fn yt_dlp() -> Result<PathBuf, String> {
    tools::wait_for_yt_dlp_update();
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
    fn srt_and_vtt() {
        let srt = "\u{feff}1\r\n00:00:01,000 --> 00:00:03,500\r\n<i>Hello</i> there\r\n\r\n2\r\n00:00:04,000 --> 00:00:06,000\r\nGeneral &amp; Kenobi\r\nsecond line\r\n";
        let s = parse_subtitles(srt);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].start, s[0].end, s[0].text.as_str()), (1.0, 3.5, "Hello there"));
        assert_eq!(s[1].text, "General & Kenobi second line");

        let vtt = "WEBVTT\nKind: captions\n\nNOTE a comment\n\n00:01.000 --> 00:02.000 align:start position:0%\nwe were<00:01.500><c> talking</c>\n\n00:02.000 --> 00:03.000\nwe were talking\n\n00:02.010 --> 00:04.000\nwe were talking about it\n\n1:00:00.250 --> 1:00:01.000\n{\\an8}late\n";
        let s = parse_subtitles(vtt);
        let texts: Vec<&str> = s.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(texts, vec!["we were talking", "about it", "late"]);
        assert_eq!(s[2].start, 3600.25);
    }

    #[test]
    fn youtube_rolling_vtt() {
        let vtt = "WEBVTT

00:00:00.160 --> 00:00:02.000

Ratchet<00:00:00.400><c> and</c><00:00:00.800><c> Clank</c>

00:00:02.000 --> 00:00:02.010
Ratchet and Clank
 

00:00:02.010 --> 00:00:04.000
Ratchet and Clank
is<00:00:02.500><c> back</c>

00:00:04.000 --> 00:00:04.010
is back
 

00:00:04.010 --> 00:00:06.000
is back
to being dormant
";
        let texts: Vec<String> = parse_subtitles(vtt).into_iter().map(|s| s.text).collect();
        assert_eq!(texts, vec!["Ratchet and Clank", "is back", "to being dormant"]);
        assert_eq!(strip_lang("Talk [DIp3iT-Raig].en-orig"), "Talk [DIp3iT-Raig]");
        assert_eq!(strip_lang("Lecture 3.final cut"), "Lecture 3.final cut");
        assert_eq!(strip_lang("Lecture.final"), "Lecture.final");
        assert_eq!(strip_lang("Talk.pt-BR"), "Talk");
    }

    #[test]
    fn playlists() {
        assert!(is_playlist("https://www.youtube.com/playlist?list=PL1234567890"));
        assert!(!is_playlist("https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PL1234567890"));
        assert!(!is_playlist("https://youtu.be/dQw4w9WgXcQ"));
        assert!(!is_playlist("https://example.com/watch?list=1"));
    }

    #[test]
    fn kinds() {
        assert_eq!(kind_of("https://youtu.be/dQw4w9WgXcQ"), Ok(Kind::YouTube));
        assert_eq!(kind_of("https://vimeo.com/1"), Ok(Kind::Url));
        assert_eq!(kind_of("clip.WEBM"), Ok(Kind::Video));
        assert_eq!(kind_of("memo.m4a"), Ok(Kind::Audio));
        assert_eq!(kind_of("talk.en.SRT"), Ok(Kind::Subtitles));
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
