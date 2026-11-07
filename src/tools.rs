//! Where Recap keeps its data, and the external programs it drives (ffmpeg, yt-dlp, whisper.cpp).
//!
//! Every program is looked up next to recap.exe first, then in the data folder's `tools`
//! directory, then on PATH, so a portable folder can carry its own copies.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Root data folder: RECAP_HOME, or `recap-data` next to the exe (portable), or ~/Recap.
pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("RECAP_HOME") {
        return PathBuf::from(h);
    }
    if let Some(d) = exe_dir().map(|d| d.join("recap-data")).filter(|d| d.is_dir()) {
        return d;
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join("Recap")
}

pub fn library_dir() -> PathBuf {
    home().join("library")
}
pub fn models_dir() -> PathBuf {
    home().join("models")
}
pub fn tools_dir() -> PathBuf {
    home().join("tools")
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

fn exe_name(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn on_path(name: &str) -> Option<PathBuf> {
    let file = exe_name(name);
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(&file)).find(|p| p.is_file())
}

/// Finds `name` next to the exe, in `tools/` (also `tools/<subdir>/`), or on PATH.
pub fn find(name: &str, subdir: Option<&str>) -> Option<PathBuf> {
    let file = exe_name(name);
    let mut candidates = Vec::new();
    if let Some(d) = exe_dir() {
        candidates.push(d.join(&file));
        if let Some(s) = subdir {
            candidates.push(d.join(s).join(&file));
        }
    }
    let t = tools_dir();
    candidates.push(t.join(&file));
    if let Some(s) = subdir {
        candidates.push(t.join(s).join(&file));
    }
    candidates.into_iter().find(|p| p.is_file()).or_else(|| on_path(name))
}

pub fn ffmpeg() -> Option<PathBuf> {
    find("ffmpeg", None)
}
pub fn yt_dlp() -> Option<PathBuf> {
    find("yt-dlp", None)
}
pub fn whisper_cli() -> Option<PathBuf> {
    find("whisper-cli", Some("whisper.cpp"))
}

/// True when the whisper.cpp build next to `cli` includes the Vulkan backend.
pub fn has_vulkan(cli: &Path) -> bool {
    let Some(dir) = cli.parent() else { return false };
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().any(|e| e.file_name().to_string_lossy().to_ascii_lowercase().starts_with("ggml-vulkan")))
        .unwrap_or(false)
}

/// A command for an absolute program path that doesn't flash a console window.
pub fn command(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

/// A fresh, empty scratch folder that deletes itself when dropped.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new() -> std::io::Result<Self> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let p = std::env::temp_dir().join(format!("recap-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p)?;
        Ok(TempDir(p))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn open_in_file_manager(p: &Path) {
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(p).spawn();
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = Command::new("xdg-open").arg(p).spawn();
}

/// Opens a URL in the user's default browser. Only ever called from an explicit button.
pub fn open_url(url: &str) {
    #[cfg(windows)]
    let _ = Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = Command::new("xdg-open").arg(url).spawn();
}
