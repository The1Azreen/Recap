#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
#[allow(dead_code)]
mod audio;
mod claude;
mod explorer;
mod export;
mod library;
mod model;
mod sources;
#[allow(dead_code)]
mod theme;
mod tools;
mod transcribe;

use std::path::PathBuf;

const USAGE: &str = "\
recap [FILE|URL] [options]

  --open <id>            open this saved recap
  --explorer             start with the Transcripts explorer open
  --size <WxH>           window size, e.g. 1400x900
  --screenshot <file>    save a screenshot after start-up and quit
  -h, --help             this text

recap --cli <FILE|URL> [--style general|meeting|lecture|podcast] [--no-summary]
                       [--transcribe] [--speakers] [--model large-v3-turbo] [--lang en]
  Run without the window; the recap is saved to the library.
  Summaries use ANTHROPIC_API_KEY.";

fn main() -> eframe::Result {
    let all: Vec<String> = std::env::args().skip(1).collect();
    if all.first().map(String::as_str) == Some("--cli") {
        std::process::exit(run_cli(&all[1..]));
    }
    let mut opts = app::Options::default();
    let mut size = [1440.0f32, 900.0];
    let mut args = all.into_iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--open" => opts.open_id = args.next(),
            "--explorer" => opts.explorer = true,
            "--screenshot" => opts.screenshot = args.next().map(PathBuf::from),
            "--size" => {
                if let Some((w, h)) = args.next().as_deref().and_then(|s| s.split_once('x')) {
                    size = [w.parse().unwrap_or(size[0]), h.parse().unwrap_or(size[1])];
                }
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            _ => opts.source = Some(a),
        }
    }

    let icon = image::load_from_memory(include_bytes!("../assets/icon.png"))
        .map(|i| i.to_rgba8())
        .ok()
        .map(|i| egui::IconData { width: i.width(), height: i.height(), rgba: i.into_raw() });
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Recap")
        .with_app_id("recap")
        .with_inner_size(size)
        .with_min_inner_size([980.0, 620.0])
        .with_drag_and_drop(true);
    if opts.screenshot.is_some() {
        viewport = viewport.with_mouse_passthrough(true).with_active(false);
    }
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let native = eframe::NativeOptions {
        viewport,
        persistence_path: Some(tools::home().join("settings.ron")),
        ..Default::default()
    };
    eframe::run_native("recap", native, Box::new(|cc| Ok(Box::new(app::App::new(cc, opts)))))
}

fn run_cli(args: &[String]) -> i32 {
    attach_console();
    let mut a = app::CliArgs {
        source: String::new(),
        style: "general".into(),
        summarize: true,
        force_transcribe: false,
        speaker_turns: false,
        model: transcribe::DEFAULT_MODEL.into(),
        language: None,
    };
    let mut it = args.iter();
    while let Some(x) = it.next() {
        match x.as_str() {
            "--style" => a.style = it.next().cloned().unwrap_or(a.style),
            "--model" => a.model = it.next().cloned().unwrap_or(a.model),
            "--lang" => a.language = it.next().cloned(),
            "--no-summary" => a.summarize = false,
            "--transcribe" => a.force_transcribe = true,
            "--speakers" => a.speaker_turns = true,
            _ => a.source = x.clone(),
        }
    }
    if a.source.is_empty() {
        println!("{USAGE}");
        return 2;
    }
    app::cli(a)
}

/// The release build is a GUI program; borrow the terminal it was started from so --cli output shows.
fn attach_console() {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn AttachConsole(pid: u32) -> i32;
        }
        // SAFETY: plain Win32 call; ATTACH_PARENT_PROCESS = -1. Failing (no parent console) is fine.
        unsafe {
            AttachConsole(u32::MAX);
        }
    }
}
