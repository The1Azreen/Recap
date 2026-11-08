//! Speech to text with whisper.cpp.
//!
//! Built with Vulkan (scripts/build_whisper_vulkan.ps1), whisper.cpp runs on AMD GPUs on
//! Windows, RX 6800 XT included. faster-whisper only accelerates on NVIDIA, and ROCm on Windows
//! doesn't cover RDNA2. A CPU-only whisper.cpp build works too, just slower.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::model::Segment;
use crate::tools;

pub type Progress<'a> = &'a (dyn Fn(&str, Option<f32>, &str) + Sync);

pub const MODELS: [&str; 11] = [
    "tiny", "tiny.en", "base", "base.en", "small", "small.en", "medium", "medium.en", "large-v2", "large-v3", "large-v3-turbo",
];
pub const DEFAULT_MODEL: &str = "large-v3-turbo";

pub struct Result_ {
    pub segments: Vec<Segment>,
    pub language: Option<String>,
    pub method: String,
}

/// Any audio or video file to 16 kHz mono 16-bit WAV, the format Whisper wants.
pub fn to_wav(src: &Path, dst: &Path) -> Result<(), String> {
    let ffmpeg = tools::ffmpeg().ok_or("ffmpeg not found. Put ffmpeg.exe next to recap.exe or on PATH.")?;
    let out = tools::command(&ffmpeg)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(src)
        .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(dst)
        .output()
        .map_err(|e| format!("Couldn't run ffmpeg: {e}"))?;
    if !out.status.success() || !dst.is_file() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: String = err.trim().chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect();
        return Err(format!("ffmpeg couldn't read {}: {tail}", src.file_name().unwrap_or_default().to_string_lossy()));
    }
    Ok(())
}

/// The ggml model file, downloaded from Hugging Face the first time.
pub fn ensure_model(name: &str, progress: Progress) -> Result<PathBuf, String> {
    if !MODELS.contains(&name) && name != TURNS_MODEL {
        return Err(format!("Unknown whisper model {name}"));
    }
    let path = tools::models_dir().join(format!("ggml-{name}.bin"));
    if path.is_file() {
        return Ok(path);
    }
    std::fs::create_dir_all(tools::models_dir()).map_err(|e| e.to_string())?;
    let repo = if name.contains("tdrz") { "akashmjn/tinydiarize-whisper.cpp" } else { "ggerganov/whisper.cpp" };
    let url = format!("https://huggingface.co/{repo}/resolve/main/ggml-{name}.bin");
    let msg = format!("Downloading whisper model {name}");
    progress("model", Some(0.0), &msg);
    let resp = ureq::get(&url).call().map_err(|e| format!("Couldn't download the {name} model: {e}"))?;
    let total: u64 = resp.header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let part = path.with_extension("part");
    let mut file = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("Model download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if total > 0 {
            progress("model", Some(done as f32 / total as f32), &msg);
        }
    }
    drop(file);
    std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// tinydiarize: a small English model that marks where the speaker changes. It can't tell
/// who is speaking, only that someone else starts, so turns are labelled A, B, A, B...
pub const TURNS_MODEL: &str = "small.en-tdrz";

/// One run of whisper-cli over a 16 kHz WAV. Returns its JSON output and the GPU name it used.
fn run_whisper(cli: &Path, model_path: &Path, wav: &Path, out: &Path, extra: &[&str], language: Option<&str>, label: &str, progress: Progress) -> Result<(serde_json::Value, Option<String>), String> {
    let threads = (std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) / 2).clamp(1, 8);
    let mut child = tools::command(cli)
        .arg("-m")
        .arg(model_path)
        .arg("-f")
        .arg(wav)
        .args(["-oj", "-of"])
        .arg(out)
        .args(["-l", language.unwrap_or("auto"), "-t", &threads.to_string(), "-pp"])
        .args(extra)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't start whisper.cpp: {e}"))?;

    let mut device: Option<String> = None;
    let mut tail: Vec<String> = Vec::new();
    if let Some(err) = child.stderr.take() {
        for line in BufReader::new(err).lines().map_while(Result::ok) {
            // "ggml_vulkan: 0 = AMD Radeon RX 6800 XT (AMD proprietary driver) | uma: 0 | ..."
            if device.is_none() {
                if let Some(rest) = line.split("ggml_vulkan:").nth(1) {
                    if let Some((_, name)) = rest.split_once('=') {
                        let name = name.split(['(', '|']).next().unwrap_or("").trim();
                        if !name.is_empty() {
                            device = Some(name.to_string());
                        }
                    }
                }
            }
            if let Some(p) = line.split("progress =").nth(1) {
                if let Ok(pct) = p.trim().trim_end_matches('%').trim().parse::<f32>() {
                    let on = device.as_ref().map(|d| format!(" on {d}")).unwrap_or_default();
                    progress("transcribe", Some(pct / 100.0), &format!("{label}{on}"));
                }
            }
            tail.push(line);
            if tail.len() > 30 {
                tail.remove(0);
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let json_path = out.with_extension("json");
    if !status.success() || !json_path.is_file() {
        return Err(format!("whisper.cpp failed:\n{}", tail[tail.len().saturating_sub(10)..].join("\n")));
    }
    let raw = std::fs::read(&json_path).map_err(|e| e.to_string())?;
    let data = serde_json::from_str(&String::from_utf8_lossy(&raw)).map_err(|e| format!("whisper.cpp output unreadable: {e}"))?;
    Ok((data, device))
}

fn offsets(item: &serde_json::Value) -> (f64, f64) {
    (item["offsets"]["from"].as_f64().unwrap_or(0.0) / 1000.0, item["offsets"]["to"].as_f64().unwrap_or(0.0) / 1000.0)
}

/// Labels segments "Speaker A" / "Speaker B" from the times where a new speaker starts.
/// A segment belongs to whoever is talking at its midpoint.
pub fn apply_turns(segments: &mut [crate::model::Segment], turns: &[f64]) {
    if turns.is_empty() {
        return;
    }
    for s in segments.iter_mut() {
        let mid = (s.start + s.end) / 2.0;
        let n = turns.iter().filter(|t| **t <= mid).count();
        s.speaker = Some(if n % 2 == 0 { "Speaker A" } else { "Speaker B" }.to_string());
    }
}

pub fn transcribe(src: &Path, model: &str, language: Option<&str>, speaker_turns: bool, progress: Progress) -> Result<Result_, String> {
    let cli = tools::whisper_cli().ok_or(
        "whisper.cpp isn't installed. Run scripts\\build_whisper_vulkan.ps1 (GPU), or put whisper-cli.exe next to recap.exe.",
    )?;
    let model_path = ensure_model(model, progress)?;
    let tmp = tools::TempDir::new().map_err(|e| e.to_string())?;
    progress("extract", None, "Converting audio");
    let wav = tmp.0.join("audio.wav");
    to_wav(src, &wav)?;

    progress("transcribe", Some(0.0), &format!("Transcribing with whisper.cpp ({model})"));
    let (data, device) = run_whisper(&cli, &model_path, &wav, &tmp.0.join("out"), &[], language, "Transcribing", progress)?;

    let mut segments = Vec::new();
    for item in data["transcription"].as_array().into_iter().flatten() {
        let text = item["text"].as_str().unwrap_or("").trim();
        if text.is_empty() || matches!(text, "[BLANK_AUDIO]" | "[ Silence ]" | "[silence]") {
            continue;
        }
        let (from, to) = offsets(item);
        segments.push(Segment { start: from, end: to, text: text.to_string(), speaker: None });
    }
    let lang = data["result"]["language"].as_str().map(str::to_string).or(language.map(str::to_string));
    let gpu = if tools::has_vulkan(&cli) { device.map(|d| format!("Vulkan · {d}")).unwrap_or_else(|| "Vulkan".into()) } else { "CPU".into() };
    let mut method = format!("whisper.cpp {model} ({gpu})");

    // Second, quick pass with the tinydiarize model to find where the speaker changes.
    if speaker_turns && !segments.is_empty() {
        if lang.as_deref().is_some_and(|l| l.starts_with("en")) {
            let turns_model = ensure_model(TURNS_MODEL, progress)?;
            progress("transcribe", Some(0.0), "Finding speaker changes");
            let (data, _) = run_whisper(&cli, &turns_model, &wav, &tmp.0.join("turns"), &["-tdrz"], Some("en"), "Finding speaker changes", progress)?;
            let turns: Vec<f64> = data["transcription"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|i| i["speaker_turn_next"].as_bool() == Some(true))
                .map(|i| offsets(i).1)
                .collect();
            apply_turns(&mut segments, &turns);
            method.push_str(if turns.is_empty() { " · one speaker" } else { " · speaker turns" });
        } else {
            progress("transcribe", None, "Speaker labels only work for English; skipped");
        }
    }
    Ok(Result_ { segments, language: lang, method })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Segment;

    #[test]
    fn turns_alternate_speakers() {
        let seg = |a: f64, b: f64| Segment { start: a, end: b, text: "x".into(), speaker: None };
        let mut s = vec![seg(0.0, 4.0), seg(4.0, 8.0), seg(8.0, 12.0), seg(12.0, 16.0)];
        apply_turns(&mut s, &[7.0, 13.0]);
        let who: Vec<&str> = s.iter().map(|x| x.speaker.as_deref().unwrap()).collect();
        // midpoints 2, 6, 10, 14 s; new speakers start at 7 and 13 s
        assert_eq!(who, ["Speaker A", "Speaker A", "Speaker B", "Speaker A"]);
        let mut one = vec![seg(0.0, 1.0)];
        apply_turns(&mut one, &[]);
        assert_eq!(one[0].speaker, None);
    }
}
