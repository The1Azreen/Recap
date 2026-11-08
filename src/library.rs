//! Every recap is saved under <home>/library/<id>/ so it can be searched and asked about later.
//!
//!   transcript.json   the Transcript
//!   summary.json      the summary, when one was made
//!   meta.json         title, dates, method, media file name
//!   media.<ext>       audio/video kept for playback (optional)

use std::path::{Path, PathBuf};

use crate::model::{clean_title, Meta, Summary, Transcript};
use crate::tools;

fn slug(text: &str) -> String {
    let mut s = String::new();
    for c in text.to_lowercase().chars() {
        if c.is_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    let s: String = s.trim_matches('-').chars().take(40).collect();
    if s.is_empty() { "recap".into() } else { s.trim_end_matches('-').to_string() }
}

fn now_secs() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// UTC time stamp for ids, e.g. 20261107-233502. It only needs to sort and be readable.
fn stamp() -> String {
    let secs = now_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem / 60 % 60, rem % 60)
}

pub fn dir(id: &str) -> Result<PathBuf, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return Err("Bad id".into());
    }
    let d = tools::library_dir().join(id);
    if d.is_dir() { Ok(d) } else { Err(format!("No recap with id {id}")) }
}

fn write_json<T: serde::Serialize>(path: &Path, v: &T) -> Result<(), String> {
    let s = serde_json::to_string(v).map_err(|e| e.to_string())?;
    std::fs::write(path, s).map_err(|e| format!("Couldn't write {}: {e}", path.display()))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    serde_json::from_str(&s).map_err(|e| format!("{} is damaged: {e}", path.display()))
}

/// Saves a new recap; `media` is copied in for playback. Returns its id.
pub fn save(t: &Transcript, summary: Option<&Summary>, style: Option<&str>, media: Option<&Path>) -> Result<String, String> {
    let base = format!("{}-{}", stamp(), slug(t.title.as_deref().unwrap_or("recap")));
    let mut id = base.clone();
    let mut n = 2;
    while tools::library_dir().join(&id).exists() {
        id = format!("{base}-{n}");
        n += 1;
    }
    let d = tools::library_dir().join(&id);
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;

    let mut media_name = None;
    if let Some(src) = media.filter(|p| p.is_file()) {
        let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("bin").to_ascii_lowercase();
        let name = format!("media.{ext}");
        std::fs::copy(src, d.join(&name)).map_err(|e| format!("Couldn't keep a copy of the media: {e}"))?;
        media_name = Some(name);
    }
    write_json(&d.join("transcript.json"), t)?;
    if let Some(s) = summary {
        write_json(&d.join("summary.json"), s)?;
    }
    let meta = Meta {
        id: id.clone(),
        title: summary.map(|s| s.title.clone()).filter(|s| !s.is_empty()).or_else(|| t.title.clone()).unwrap_or_else(|| "Untitled".into()),
        source: t.source.clone(),
        source_type: t.source_type.clone(),
        video_id: t.video_id.clone(),
        method: t.method.clone(),
        language: t.language.clone(),
        duration: t.duration,
        style: summary.and(style.map(str::to_string)),
        created: now_secs(),
        media: media_name,
    };
    write_json(&d.join("meta.json"), &meta)?;
    Ok(id)
}

pub fn list() -> Vec<Meta> {
    let Ok(rd) = std::fs::read_dir(tools::library_dir()) else { return Vec::new() };
    let mut items: Vec<Meta> = rd.flatten().filter_map(|e| read_json(&e.path().join("meta.json")).ok()).map(tidy).collect();
    items.sort_by(|a, b| b.created.total_cmp(&a.created));
    items
}

pub fn meta(id: &str) -> Result<Meta, String> {
    read_json(&dir(id)?.join("meta.json")).map(tidy)
}

/// Older recaps saved file-name titles like "Talk [dQw4w9WgXcQ]"; show them tidied.
fn tidy(mut m: Meta) -> Meta {
    m.title = clean_title(&m.title);
    m
}

/// Renames a recap. Returns the updated metadata.
pub fn set_title(id: &str, title: &str) -> Result<Meta, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("The name can't be empty".into());
    }
    let d = dir(id)?;
    let mut m: Meta = read_json(&d.join("meta.json"))?;
    m.title = title.to_string();
    write_json(&d.join("meta.json"), &m)?;
    Ok(m)
}

pub fn transcript(id: &str) -> Result<Transcript, String> {
    read_json(&dir(id)?.join("transcript.json"))
}

pub fn summary(id: &str) -> Result<Option<Summary>, String> {
    let p = dir(id)?.join("summary.json");
    if p.is_file() { read_json(&p).map(Some) } else { Ok(None) }
}

pub fn set_summary(id: &str, s: &Summary, style: &str) -> Result<Meta, String> {
    let d = dir(id)?;
    write_json(&d.join("summary.json"), s)?;
    let mut m = meta(id)?;
    m.style = Some(style.to_string());
    if !s.title.is_empty() {
        m.title = s.title.clone();
    }
    write_json(&d.join("meta.json"), &m)?;
    Ok(m)
}

/// Records a media file already placed inside the item's folder.
pub fn set_media(id: &str, file_name: &str) -> Result<Meta, String> {
    let d = dir(id)?;
    let mut m = meta(id)?;
    m.media = Some(file_name.to_string());
    write_json(&d.join("meta.json"), &m)?;
    Ok(m)
}

pub fn media_path(m: &Meta) -> Option<PathBuf> {
    let p = dir(&m.id).ok()?.join(m.media.as_ref()?);
    p.is_file().then_some(p)
}

pub fn delete(id: &str) -> Result<(), String> {
    std::fs::remove_dir_all(dir(id)?).map_err(|e| e.to_string())
}
