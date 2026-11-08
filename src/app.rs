//! Recap's window: the same look as JAMP (night sky, neon-yellow windows, magenta taskbar).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

use egui::{Align, Align2, Color32, Pos2, Rect, RichText, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};
use serde::{Deserialize, Serialize};

use crate::audio::Player;
use crate::claude::{self, Client};
use crate::model::{fmt_ts, parse_ts, Meta, Summary, Transcript};
use crate::sources::{self, Kind};
use crate::theme::*;
use crate::explorer::{self, Explorer};
use crate::{export, library, tools, transcribe};

/// Transcription saturates the GPU, so recaps run one at a time.
static GPU: Mutex<()> = Mutex::new(());

const STYLE_NAMES: [(&str, &str); 4] = [("general", "General"), ("meeting", "Meeting"), ("lecture", "Lecture"), ("podcast", "Podcast")];

fn style_name(s: &str) -> &'static str {
    STYLE_NAMES.iter().find(|(k, _)| *k == s).map(|(_, n)| *n).unwrap_or("General")
}

#[derive(Default)]
pub struct Options {
    pub source: Option<String>,
    pub open_id: Option<String>,
    pub screenshot: Option<PathBuf>,
    pub explorer: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    style: String,
    /// Plain text in the settings file; ANTHROPIC_API_KEY takes precedence when set.
    api_key: String,
    model: String,
    language: String,
    effort: String,
    summarize: bool,
    force_transcribe: bool,
    glow: bool,
    volume: f32,
}

impl Default for Saved {
    fn default() -> Self {
        Saved {
            style: "general".into(),
            api_key: String::new(),
            model: transcribe::DEFAULT_MODEL.into(),
            language: String::new(),
            effort: "medium".into(),
            summarize: true,
            force_transcribe: false,
            glow: true,
            volume: 0.7,
        }
    }
}

enum Msg {
    Progress { frac: Option<f32>, msg: String },
    Warn(String),
    RecapDone(Result<String, String>),
    Summarized { id: String, result: Result<(Summary, Meta), String> },
    Answer { id: String, question: String, result: Result<String, String> },
    AudioReady { id: String, result: Result<Meta, String> },
    Playlist(Result<Vec<String>, String>),
}

struct OpenItem {
    meta: Meta,
    transcript: Transcript,
    summary: Option<Summary>,
}

struct Hit {
    id: String,
    title: String,
    start: f64,
    text: String,
}

struct Shot {
    path: PathBuf,
    at: f64,
    waiting: bool,
}

pub struct App {
    saved: Saved,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    start: std::time::Instant,

    url: String,
    file: Option<PathBuf>,

    library: Vec<Meta>,
    lib_query: String,
    hits: Option<Vec<Hit>>,
    search_cache: HashMap<String, Transcript>,
    arm_delete: Option<(String, f64)>,

    open: Option<OpenItem>,
    tr_filter: String,
    follow: bool,
    active: Option<usize>,
    scroll_to_active: bool,

    player: Player,
    player_for: Option<String>,

    busy: bool,
    /// Sources waiting to be recapped, one after another.
    queue: VecDeque<String>,
    /// The source being recapped right now.
    current: Option<String>,
    /// (finished, failed) in the current run of the queue
    batch: (usize, usize),
    side_busy: Option<String>,
    status: String,
    status_err: bool,
    frac: Option<f32>,

    question: String,
    answer: Option<(String, String)>,

    show_settings: bool,
    explorer: Explorer,
    engine: (String, bool),
    shot: Option<Shot>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: Options) -> Self {
        install(&cc.egui_ctx);
        tools::update_yt_dlp_in_background();
        let saved: Saved = cc.storage.and_then(|s| eframe::get_value(s, "recap")).unwrap_or_default();
        set_glow(saved.glow);
        let (tx, rx) = channel();
        let mut player = Player::new();
        player.set_volume(saved.volume);
        let mut app = App {
            saved,
            tx,
            rx,
            start: std::time::Instant::now(),
            url: String::new(),
            file: None,
            library: library::list(),
            lib_query: String::new(),
            hits: None,
            search_cache: HashMap::new(),
            arm_delete: None,
            open: None,
            tr_filter: String::new(),
            follow: true,
            active: None,
            scroll_to_active: false,
            player,
            player_for: None,
            busy: false,
            queue: VecDeque::new(),
            current: None,
            batch: (0, 0),
            side_busy: None,
            status: "Ready".into(),
            status_err: false,
            frac: None,
            question: String::new(),
            answer: None,
            show_settings: false,
            explorer: Explorer::new(opts.explorer),
            engine: engine(),
            shot: opts.screenshot.map(|path| Shot { path, at: 2.5, waiting: false }),
        };
        if let Some(src) = opts.source {
            if PathBuf::from(&src).is_file() { app.file = Some(src.into()) } else { app.url = src }
        }
        if let Some(id) = opts.open_id.or_else(|| app.library.first().map(|m| m.id.clone())) {
            app.open_item(&id, None);
        }
        if app.api_key().is_empty() {
            app.set_status("No Claude API key: transcripts work, summaries need a key (Settings)", true);
        }
        app
    }

    fn time(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    fn api_key(&self) -> String {
        std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.trim().is_empty()).unwrap_or_else(|| self.saved.api_key.trim().to_string())
    }

    fn client(&self) -> Client {
        Client { api_key: self.api_key(), effort: self.saved.effort.clone() }
    }

    fn set_status(&mut self, s: impl Into<String>, err: bool) {
        self.status = s.into();
        self.status_err = err;
        self.frac = None;
    }

    // ------------------------------------------------------------------------------- jobs

    fn progress_fn(&self, ctx: &egui::Context) -> impl Fn(&str, Option<f32>, &str) + Sync + Send + 'static {
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        move |_stage: &str, frac: Option<f32>, msg: &str| {
            let _ = tx.send(Msg::Progress { frac, msg: msg.to_string() });
            ctx.request_repaint();
        }
    }

    /// "Recap it" / "Add to queue": takes the link or file from the New Recap window.
    fn start_recap(&mut self, ctx: &egui::Context) {
        let source = match (&self.file, self.url.trim()) {
            (Some(f), _) => f.to_string_lossy().into_owned(),
            (None, u) if !u.is_empty() => u.to_string(),
            _ => {
                self.set_status("Paste a link or choose a file first", true);
                return;
            }
        };
        if sources::is_playlist(&source) {
            self.url.clear();
            self.expand_playlist(ctx, source);
            return;
        }
        if let Err(e) = sources::kind_of(&source) {
            self.set_status(e, true);
            return;
        }
        self.file = None;
        self.url.clear();
        self.enqueue(ctx, vec![source]);
    }

    /// Adds sources to the queue and starts the first one if nothing is running.
    fn enqueue(&mut self, ctx: &egui::Context, sources: Vec<String>) {
        let n = sources.len();
        self.queue.extend(sources);
        if self.busy {
            let what = if n == 1 { "1 item".to_string() } else { format!("{n} items") };
            self.set_status(format!("Added {what} to the queue ({} waiting)", self.queue.len()), false);
        } else {
            self.batch = (0, 0);
            self.start_next(ctx);
        }
    }

    fn start_next(&mut self, ctx: &egui::Context) {
        if let Some(source) = self.queue.pop_front() {
            self.run_source(ctx, source);
        }
    }

    fn expand_playlist(&mut self, ctx: &egui::Context, url: String) {
        let (tx, ctx2) = (self.tx.clone(), ctx.clone());
        self.side_busy = Some("Reading the playlist…".into());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Playlist(sources::playlist_entries(&url)));
            ctx2.request_repaint();
        });
    }

    fn run_source(&mut self, ctx: &egui::Context, source: String) {
        self.current = Some(source.clone());
        let summarize = self.saved.summarize && !self.api_key().is_empty();
        let opts = sources::Options {
            model: self.saved.model.clone(),
            language: Some(self.saved.language.trim().to_string()).filter(|l| !l.is_empty() && l != "auto"),
            force_transcribe: self.saved.force_transcribe,
        };
        let style = self.saved.style.clone();
        let client = self.client();
        let (tx, ctx2, progress) = (self.tx.clone(), ctx.clone(), self.progress_fn(ctx));
        self.busy = true;
        self.set_status("Starting…", false);
        std::thread::spawn(move || {
            let _gpu = GPU.lock().unwrap_or_else(|e| e.into_inner());
            let result = run_recap(&source, &opts, summarize.then_some((client, style)), &progress, &tx);
            let _ = tx.send(Msg::RecapDone(result));
            ctx2.request_repaint();
        });
    }

    fn start_summary(&mut self, ctx: &egui::Context) {
        let Some(open) = &self.open else { return };
        if self.side_busy.is_some() {
            return;
        }
        let (id, t, style, client) = (open.meta.id.clone(), open.transcript.clone(), self.saved.style.clone(), self.client());
        let (tx, ctx2, progress) = (self.tx.clone(), ctx.clone(), self.progress_fn(ctx));
        self.side_busy = Some(format!("Summarizing as {} with Claude…", style_name(&style)));
        std::thread::spawn(move || {
            let result = client.summarize(&t, &style, &progress).and_then(|s| library::set_summary(&id, &s, &style).map(|m| (s, m)));
            let _ = tx.send(Msg::Summarized { id, result });
            ctx2.request_repaint();
        });
    }

    fn start_ask(&mut self, ctx: &egui::Context) {
        let q = self.question.trim().to_string();
        let Some(open) = &self.open else { return };
        if q.is_empty() || self.side_busy.is_some() {
            return;
        }
        let (id, t, client, tx, ctx2) = (open.meta.id.clone(), open.transcript.clone(), self.client(), self.tx.clone(), ctx.clone());
        self.side_busy = Some("Asking Claude…".into());
        self.answer = Some((q.clone(), String::new()));
        std::thread::spawn(move || {
            let result = client.ask(&t, &q);
            let _ = tx.send(Msg::Answer { id, question: q, result });
            ctx2.request_repaint();
        });
    }

    fn start_download_audio(&mut self, ctx: &egui::Context) {
        let Some(open) = &self.open else { return };
        if self.side_busy.is_some() {
            return;
        }
        let (id, url) = (open.meta.id.clone(), open.meta.source.clone());
        let (tx, ctx2, progress) = (self.tx.clone(), ctx.clone(), self.progress_fn(ctx));
        self.side_busy = Some("Downloading audio…".into());
        std::thread::spawn(move || {
            let result = (|| {
                let dir = library::dir(&id)?;
                let tmp = tools::TempDir::new().map_err(|e| e.to_string())?;
                let (file, _) = sources::download_audio(&url, &tmp.0, &progress)?;
                let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("webm").to_string();
                let name = format!("media.{ext}");
                std::fs::copy(&file, dir.join(&name)).map_err(|e| e.to_string())?;
                library::set_media(&id, &name)
            })();
            let _ = tx.send(Msg::AudioReady { id, result });
            ctx2.request_repaint();
        });
    }

    fn pump(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Progress { frac, msg } => {
                    self.status = if self.queue.is_empty() { msg } else { format!("{msg} · {} more queued", self.queue.len()) };
                    self.status_err = false;
                    self.frac = frac;
                }
                Msg::Warn(w) => self.set_status(w, true),
                Msg::RecapDone(r) => {
                    self.busy = false;
                    let source = self.current.take().unwrap_or_default();
                    match r {
                        Ok(id) => {
                            self.batch.0 += 1;
                            if !self.status_err {
                                self.set_status("Done", false);
                                self.frac = Some(1.0);
                            }
                            self.refresh_library();
                            self.open_item(&id, None);
                        }
                        Err(e) => {
                            self.batch.1 += 1;
                            let many = self.batch.0 + self.batch.1 + self.queue.len() > 1;
                            self.set_status(if many { format!("{}: {e}", source_label(&source)) } else { e }, true);
                            // a single failed recap: give the input back so it can be retried
                            if self.queue.is_empty() && self.batch == (0, 1) && self.url.is_empty() && self.file.is_none() {
                                if source.starts_with("http") {
                                    self.url = source;
                                } else {
                                    self.file = Some(PathBuf::from(source));
                                }
                            }
                        }
                    }
                    if !self.queue.is_empty() {
                        self.start_next(ctx);
                    } else if self.batch.0 + self.batch.1 > 1 {
                        let (ok, bad) = self.batch;
                        let msg = format!("Queue finished: {ok} done{}", if bad > 0 { format!(", {bad} failed") } else { String::new() });
                        if bad > 0 && self.status_err {
                            self.status = format!("{msg} · last error: {}", self.status);
                        } else {
                            self.set_status(msg, bad > 0);
                        }
                    }
                }
                Msg::Playlist(r) => {
                    self.side_busy = None;
                    match r {
                        Ok(urls) => self.enqueue(ctx, urls),
                        Err(e) => self.set_status(e, true),
                    }
                }
                Msg::Summarized { id, result } => {
                    self.side_busy = None;
                    match result {
                        Ok((s, m)) => {
                            if let Some(o) = self.open.as_mut().filter(|o| o.meta.id == id) {
                                o.summary = Some(s);
                                o.meta = m;
                            }
                            self.set_status("Summary updated", false);
                            self.refresh_library();
                        }
                        Err(e) => self.set_status(e, true),
                    }
                }
                Msg::Answer { id, question, result } => {
                    self.side_busy = None;
                    if self.open.as_ref().is_some_and(|o| o.meta.id == id) {
                        match result {
                            Ok(a) => {
                                self.answer = Some((question, a));
                                self.question.clear();
                                self.set_status("Answered", false);
                            }
                            Err(e) => {
                                self.answer = Some((question, e.clone()));
                                self.set_status(e, true);
                            }
                        }
                    }
                }
                Msg::AudioReady { id, result } => {
                    self.side_busy = None;
                    match result {
                        Ok(m) => {
                            if let Some(o) = self.open.as_mut().filter(|o| o.meta.id == id) {
                                o.meta = m;
                            }
                            self.set_status("Audio downloaded: click any timestamp to play", false);
                            self.refresh_library();
                        }
                        Err(e) => self.set_status(e, true),
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------------------- library

    fn refresh_library(&mut self) {
        self.library = library::list();
        self.search_cache.clear();
        self.explorer.invalidate();
        if self.hits.is_some() {
            self.run_search();
        }
    }

    fn run_search(&mut self) {
        let q = self.lib_query.trim().to_lowercase();
        if q.is_empty() {
            self.hits = None;
            return;
        }
        let mut hits = Vec::new();
        'outer: for m in &self.library {
            if !self.search_cache.contains_key(&m.id) {
                if let Ok(t) = library::transcript(&m.id) {
                    self.search_cache.insert(m.id.clone(), t);
                }
            }
            let Some(t) = self.search_cache.get(&m.id) else { continue };
            let mut found = false;
            for s in &t.segments {
                if s.text.to_lowercase().contains(&q) {
                    hits.push(Hit { id: m.id.clone(), title: m.title.clone(), start: s.start, text: s.text.clone() });
                    found = true;
                    if hits.len() >= 200 {
                        break 'outer;
                    }
                }
            }
            if !found && m.title.to_lowercase().contains(&q) {
                hits.push(Hit { id: m.id.clone(), title: m.title.clone(), start: 0.0, text: String::new() });
            }
        }
        self.hits = Some(hits);
    }

    fn open_item(&mut self, id: &str, at: Option<f64>) {
        let loaded = (|| Ok::<_, String>(OpenItem { meta: library::meta(id)?, transcript: library::transcript(id)?, summary: library::summary(id)? }))();
        match loaded {
            Ok(o) => {
                if self.player_for.as_deref() != Some(id) {
                    self.player.stop();
                    self.player_for = None;
                }
                self.open = Some(o);
                self.active = None;
                self.tr_filter.clear();
                self.answer = None;
                if let Some(t) = at {
                    self.jump(t);
                }
            }
            Err(e) => self.set_status(e, true),
        }
    }

    fn delete_item(&mut self, id: &str) {
        if self.player_for.as_deref() == Some(id) {
            self.player.stop();
            self.player_for = None;
        }
        match library::delete(id) {
            Ok(()) => {
                if self.open.as_ref().is_some_and(|o| o.meta.id == id) {
                    self.open = None;
                }
                self.set_status("Deleted", false);
            }
            Err(e) => self.set_status(e, true),
        }
        self.refresh_library();
    }

    // ------------------------------------------------------------------------------- playback

    fn media(&self) -> Option<PathBuf> {
        self.open.as_ref().and_then(|o| library::media_path(&o.meta))
    }

    /// Jump to `t`: plays from there when we have the audio, and always highlights the line.
    fn jump(&mut self, t: f64) {
        if let (Some(path), Some(o)) = (self.media(), &self.open) {
            let id = o.meta.id.clone();
            let r = if self.player_for.as_deref() == Some(&id) && self.player.is_loaded() {
                self.player.seek(t);
                self.player.set_paused(false);
                Ok(())
            } else {
                self.player.load(&path, t, false)
            };
            match r {
                Ok(()) => self.player_for = Some(id),
                Err(e) => self.set_status(format!("Can't play this file: {e}"), true),
            }
        }
        self.active = self.segment_at(t);
        self.scroll_to_active = true;
    }

    fn segment_at(&self, t: f64) -> Option<usize> {
        let segs = &self.open.as_ref()?.transcript.segments;
        let idx = segs.partition_point(|s| s.start <= t + 0.05);
        idx.checked_sub(1)
    }

    fn toggle_play(&mut self) {
        let mine = self.open.as_ref().is_some_and(|o| self.player_for.as_deref() == Some(o.meta.id.as_str()));
        if mine && self.player.is_loaded() {
            let p = self.player.is_paused();
            self.player.set_paused(!p);
        } else if self.media().is_some() {
            self.jump(0.0);
        } else if self.open.is_some() {
            self.set_status("No local audio for this recap. Use “Download audio” in the Summary window.", true);
        }
    }

    // ------------------------------------------------------------------------------- panels

    fn header(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let (r, _) = ui.allocate_exact_size(Vec2::new(176.0, 52.0), Sense::hover());
            let p = ui.painter();
            p.text(r.left_center() + Vec2::new(3.0, 3.0), Align2::LEFT_CENTER, "RECAP", bold(48.0), MAGENTA);
            glow_text(p, r.left_center(), Align2::LEFT_CENTER, "RECAP", bold(48.0), MAGENTA);
            p.text(r.left_center(), Align2::LEFT_CENTER, "RECAP", bold(48.0), YELLOW);
            ui.vertical(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new("Transcribe &").color(TEAL));
                ui.label(RichText::new("Summarize").color(TEAL));
            });
            ui.add_space(18.0);
            for (key, _) in STYLE_NAMES {
                if style_icon(ui, key, self.saved.style == key).clicked() {
                    self.saved.style = key.into();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                let (name, gpu) = self.engine.clone();
                let label = if gpu { "GPU: Vulkan" } else if name.contains("CPU") { "CPU only" } else { "No transcriber" };
                let col = if gpu { TEAL } else if name.contains("CPU") { YELLOW } else { MAGENTA };
                let g = ui.painter().layout_no_wrap(label.to_string(), pixel(16.0), col);
                let (r, resp) = ui.allocate_exact_size(g.size() + Vec2::new(12.0, 4.0), Sense::click());
                let p = ui.painter();
                p.rect_filled(r, 2.0, INDIGO);
                p.rect_stroke(r, 2.0, Stroke::new(1.0f32, col), StrokeKind::Inside);
                if gpu {
                    glow_rect(p, r, 2.0, TEAL, 5.0);
                }
                p.galley(r.min + Vec2::new(6.0, 2.0), g, col);
                if resp.on_hover_text(format!("Transcription: {name}\nSummaries: {}\nClick for settings", claude::MODEL)).clicked() {
                    self.show_settings = true;
                }
                if ui.add(egui::Button::new("Settings").selected(self.show_settings)).clicked() {
                    self.show_settings = !self.show_settings;
                }
                if ui.button("Open file").clicked() {
                    self.pick_file(&ui.ctx().clone());
                }
                if ui.add(egui::Button::new("Transcripts").selected(self.explorer.open)).on_hover_text("Browse and search all your transcripts").clicked() {
                    self.explorer.open = !self.explorer.open;
                }
            });
        });
    }

    fn pick_file(&mut self, ctx: &egui::Context) {
        let mut exts: Vec<&str> = sources::AUDIO_EXTS.to_vec();
        exts.extend_from_slice(sources::VIDEO_EXTS);
        exts.extend_from_slice(sources::SUBTITLE_EXTS);
        if let Some(mut files) = rfd::FileDialog::new().add_filter("Audio, video and subtitles", &exts).add_filter("Subtitles (.srt, .vtt)", sources::SUBTITLE_EXTS).pick_files() {
            if files.len() == 1 {
                self.file = files.pop();
                self.url.clear();
            } else if !files.is_empty() {
                // several files at once go straight into the queue
                self.enqueue(ctx, files.into_iter().map(|p| p.to_string_lossy().into_owned()).collect());
            }
        }
    }

    fn new_recap(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        ui.label(RichText::new("YouTube or video link").small().color(TEAL));
        let r = ui.add(egui::TextEdit::singleline(&mut self.url).hint_text("https://youtu.be/…").desired_width(f32::INFINITY));
        if r.changed() && !self.url.is_empty() {
            self.file = None;
        }
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.start_recap(ctx);
        }
        // "──── or ────"
        let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 20.0), Sense::hover());
        let p = ui.painter();
        let line = Stroke::new(1.0f32, YELLOW.gamma_multiply(0.5));
        p.line_segment([r.left_center(), r.center() - Vec2::new(14.0, 0.0)], line);
        p.line_segment([r.center() + Vec2::new(14.0, 0.0), r.right_center()], line);
        p.text(r.center(), Align2::CENTER_CENTER, "or", pixel(12.0), DIM);

        // drop zone / file picker
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 64.0), Sense::click());
        let hover = resp.hovered() || ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        let p = ui.painter();
        let col = if hover || self.file.is_some() { TEAL } else { YELLOW };
        p.rect_filled(rect, 0.0, if self.file.is_some() || hover { INDIGO.gamma_multiply(0.6) } else { Color32::TRANSPARENT });
        let pts = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom(), rect.left_top()];
        p.extend(Shape::dashed_line(&pts, Stroke::new(1.5f32, col), 6.0, 4.0));
        let (main, sub) = match &self.file {
            Some(f) => (f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), "click to choose a different file".to_string()),
            None => ("Drop audio / video here".into(), "or subtitles (.srt) · click to browse".into()),
        };
        p.with_clip_rect(rect.shrink(4.0)).text(rect.center() - Vec2::new(0.0, 9.0), Align2::CENTER_CENTER, visual(&main), bold(16.0), col);
        p.text(rect.center() + Vec2::new(0.0, 12.0), Align2::CENTER_CENTER, sub, pixel(12.0), DIM);
        if resp.clicked() {
            self.pick_file(ctx);
        }

        ui.add_space(6.0);
        let has_key = !self.api_key().is_empty();
        ui.add_enabled_ui(has_key, |ui| {
            ui.checkbox(&mut self.saved.summarize, format!("Summarize ({})", style_name(&self.saved.style)))
                .on_disabled_hover_text("Add a Claude API key in Settings to summarize");
        });
        ui.checkbox(&mut self.saved.force_transcribe, "Ignore YouTube captions");
        ui.add_space(6.0);
        let has_input = self.file.is_some() || !self.url.trim().is_empty();
        let label = if !self.busy { "▶ Recap it" } else if has_input { "+ Add to queue" } else { "Working…" };
        if big_button(ui, label, !self.busy || has_input).clicked() {
            self.start_recap(ctx);
        }
        if !self.queue.is_empty() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Up next ({})", self.queue.len())).font(bold(14.0)).color(TEAL));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("Clear").on_hover_text("Remove everything waiting in the queue").clicked() {
                        self.queue.clear();
                    }
                });
            });
            let mut remove = None;
            for (i, s) in self.queue.iter().enumerate().take(5) {
                ui.horizontal(|ui| {
                    if ui.small_button("✕").on_hover_text("Remove from the queue").clicked() {
                        remove = Some(i);
                    }
                    ui.add(egui::Label::new(RichText::new(visual(&source_label(s))).small().color(YELLOW)).truncate()).on_hover_text(s);
                });
            }
            if self.queue.len() > 5 {
                ui.label(RichText::new(format!("…and {} more", self.queue.len() - 5)).small().color(DIM));
            }
            if let Some(i) = remove {
                self.queue.remove(i);
            }
        }
    }

    fn library_view(&mut self, ui: &mut Ui) {
        let r = ui.add(egui::TextEdit::singleline(&mut self.lib_query).hint_text("Search all transcripts…").desired_width(f32::INFINITY));
        if r.changed() {
            self.run_search();
        }
        ui.add_space(4.0);
        let now = self.time();
        let mut open: Option<(String, Option<f64>)> = None;
        let mut delete = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let w = ui.available_width();
            if let Some(hits) = &self.hits {
                if hits.is_empty() {
                    ui.label(RichText::new("No matches.").small().color(DIM));
                }
                for (i, h) in hits.iter().enumerate() {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 38.0), Sense::click());
                    row_bg(ui, rect, i, resp.hovered(), false);
                    let p = ui.painter().with_clip_rect(rect.shrink(2.0));
                    p.text(rect.left_top() + Vec2::new(6.0, 3.0), Align2::LEFT_TOP, visual(&h.title), pixel(16.0), YELLOW);
                    let line = if h.text.is_empty() { "title match".to_string() } else { format!("{}  {}", fmt_ts(h.start, false), h.text) };
                    p.text(rect.left_top() + Vec2::new(6.0, 21.0), Align2::LEFT_TOP, visual(&line), pixel(12.0), WHITE);
                    if resp.clicked() {
                        open = Some((h.id.clone(), Some(h.start)));
                    }
                }
                return;
            }
            if self.library.is_empty() {
                ui.label(RichText::new("Nothing here yet.").small().color(DIM));
            }
            for (i, m) in self.library.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 38.0), Sense::click());
                let on = self.open.as_ref().is_some_and(|o| o.meta.id == m.id);
                row_bg(ui, rect, i, resp.hovered(), on);
                let p = ui.painter().with_clip_rect(rect.shrink(2.0));
                let mut x = rect.left() + 6.0;
                if on {
                    p.text(Pos2::new(x, rect.top() + 3.0), Align2::LEFT_TOP, "▸", pixel(16.0), MAGENTA);
                    x += 12.0;
                }
                p.text(Pos2::new(x, rect.top() + 3.0), Align2::LEFT_TOP, visual(&m.title), pixel(16.0), YELLOW);
                let mut sub = format!("{} · {}", fmt_time(m.duration), method_short(&m.method));
                if let Some(s) = &m.style {
                    sub.push_str(&format!(" · {}", style_name(s)));
                }
                p.text(Pos2::new(x, rect.top() + 21.0), Align2::LEFT_TOP, sub, pixel(12.0), TEAL);
                if resp.clicked() {
                    open = Some((m.id.clone(), None));
                }
                let armed = self.arm_delete.as_ref().is_some_and(|(id, until)| *id == m.id && now < *until);
                resp.context_menu(|ui| {
                    if ui.button("Show in folder").clicked() {
                        if let Ok(d) = library::dir(&m.id) {
                            tools::open_in_file_manager(&d);
                        }
                        ui.close_menu();
                    }
                    let label = if armed { RichText::new("Sure? Delete").color(MAGENTA) } else { RichText::new("Delete…") };
                    if ui.button(label).clicked() {
                        if armed {
                            delete = Some(m.id.clone());
                            ui.close_menu();
                        } else {
                            self.arm_delete = Some((m.id.clone(), now + 4.0));
                        }
                    }
                });
            }
        });
        if let Some(id) = delete {
            self.arm_delete = None;
            self.delete_item(&id);
        }
        if let Some((id, at)) = open {
            self.open_item(&id, at);
        }
    }

    fn summary_view(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let Some(o) = &self.open else {
            empty_art(ui);
            return;
        };
        let mut jump = None;
        let mut act: Option<&str> = None;
        let busy = self.side_busy.is_some();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let m = &o.meta;
            ui.label(RichText::new(visual(&m.title)).font(bold(22.0)).color(YELLOW));
            let src = if m.source_type == "youtube" { "YouTube".to_string() } else if m.source.starts_with("http") { "link".to_string() } else {
                PathBuf::from(&m.source).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            };
            let lang = m.language.as_ref().map(|l| format!(" · {l}")).unwrap_or_default();
            ui.label(RichText::new(format!("{src} · {} · {}{lang}", fmt_time(m.duration), m.method)).small().color(TEAL));
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Export…").on_hover_text("Save .md, .srt, .txt and .json to a folder").clicked() {
                    act = Some("export");
                }
                if ui.button("Copy Markdown").clicked() {
                    act = Some("copy");
                }
                let label = format!("Summarize as {}", style_name(&self.saved.style));
                if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                    act = Some("summarize");
                }
                if m.media.is_none() && m.source.starts_with("http") && ui.add_enabled(!busy, egui::Button::new("Download audio")).on_hover_text("Fetch the audio with yt-dlp so timestamps play here").clicked() {
                    act = Some("audio");
                }
                if m.video_id.is_some() && ui.button("Open on YouTube").on_hover_text("Opens in your default browser").clicked() {
                    act = Some("youtube");
                }
                if m.source_type == "video" && PathBuf::from(&m.source).is_file() && ui.button("Open video").on_hover_text("Opens the original file in your default player").clicked() {
                    act = Some("video");
                }
            });
            let Some(s) = &o.summary else {
                ui.add_space(10.0);
                let msg = if self.api_key().is_empty() { "No summary. Add a Claude API key in Settings, then press “Summarize”." } else { "No summary yet. Pick a style in the top bar and press “Summarize”." };
                ui.label(RichText::new(msg).color(DIM));
                return;
            };
            heading(ui, "TL;DR");
            ui.label(RichText::new(visual(&s.tldr)).color(WHITE));
            if !s.key_points.is_empty() {
                heading(ui, "Key points");
                for k in &s.key_points {
                    bullet(ui, "▸", MAGENTA, k);
                }
            }
            if !s.action_items.is_empty() {
                heading(ui, "Action items");
                for k in &s.action_items {
                    bullet(ui, "☐", TEAL, k);
                }
            }
            if !s.chapters.is_empty() {
                heading(ui, "Chapters");
                for c in &s.chapters {
                    ui.horizontal_top(|ui| {
                        if ts_button(ui, c.start) {
                            jump = Some(c.start);
                        }
                        ui.vertical(|ui| {
                            ui.label(RichText::new(visual(&c.title)).font(bold(16.0)).color(YELLOW));
                            ui.label(RichText::new(visual(&c.summary)).small().color(DIM));
                        });
                    });
                    ui.add_space(2.0);
                }
            }
        });
        if let Some(t) = jump {
            self.jump(t);
        }
        match act {
            Some("export") => self.export(),
            Some("copy") => {
                let o = self.open.as_ref().unwrap();
                ctx.copy_text(export::to_md(&o.transcript, o.summary.as_ref()));
                self.set_status("Markdown copied", false);
            }
            Some("summarize") => {
                if self.api_key().is_empty() {
                    self.set_status("Add a Claude API key in Settings first", true);
                    self.show_settings = true;
                } else {
                    self.start_summary(ctx);
                }
            }
            Some("audio") => self.start_download_audio(ctx),
            Some("youtube") => {
                let o = self.open.as_ref().unwrap();
                if let Some(u) = export::link_at(&o.transcript, self.player_pos_or_active()) {
                    tools::open_url(&u);
                }
            }
            Some("video") => {
                let src = PathBuf::from(&self.open.as_ref().unwrap().meta.source);
                tools::open_in_file_manager(&src);
            }
            _ => {}
        }
    }

    fn player_pos_or_active(&self) -> f64 {
        let o = self.open.as_ref();
        if o.is_some_and(|o| self.player_for.as_deref() == Some(o.meta.id.as_str())) && self.player.is_loaded() {
            return self.player.position();
        }
        self.active.and_then(|i| o.map(|o| o.transcript.segments[i].start)).unwrap_or(0.0)
    }

    fn export(&mut self) {
        let Some(o) = &self.open else { return };
        let Some(dir) = rfd::FileDialog::new().set_title("Export recap to folder").pick_folder() else { return };
        match export::write_all(&o.transcript, o.summary.as_ref(), &dir, &o.meta.id) {
            Ok(files) => self.set_status(format!("Exported {} files to {}", files.len(), dir.display()), false),
            Err(e) => self.set_status(e, true),
        }
    }

    fn ask_view(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let mut jump = None;
        let h = (ui.available_height() - 30.0).max(30.0);
        egui::ScrollArea::vertical().id_salt("answer").max_height(h).auto_shrink([false, true]).show(ui, |ui| {
            match &self.answer {
                None => {
                    let msg = match &self.open {
                        Some(o) => format!("Ask anything about “{}”, e.g. what did they say about pricing?", o.meta.title),
                        None => "Open a recap to ask questions about it.".into(),
                    };
                    ui.label(RichText::new(msg).color(DIM));
                }
                Some((q, a)) => {
                    ui.label(RichText::new(format!("> {q}")).color(TEAL));
                    if a.is_empty() {
                        ui.label(RichText::new("Thinking…").color(DIM));
                    } else if let Some(t) = rich_answer(ui, a) {
                        jump = Some(t);
                    }
                }
            }
        });
        if let Some(t) = jump {
            self.jump(t);
        }
        ui.horizontal(|ui| {
            let enabled = self.open.is_some() && self.side_busy.is_none();
            let r = ui.add_enabled(enabled, egui::TextEdit::singleline(&mut self.question).hint_text("Ask about this recap…").desired_width(ui.available_width() - 50.0));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.add_enabled(enabled, egui::Button::new("Ask")).clicked() || enter {
                if self.api_key().is_empty() {
                    self.set_status("Add a Claude API key in Settings first", true);
                    self.show_settings = true;
                } else {
                    self.start_ask(ctx);
                }
            }
        });
    }

    fn transcript_view(&mut self, ui: &mut Ui) {
        // Lay out right-to-left so the search box takes exactly what the checkbox leaves; a row
        // wider than the panel would make the resizable panel grow a little every frame. The row
        // gets a fixed height: a bare right_to_left layout would claim the whole panel height.
        let row = Vec2::new(ui.available_width(), ui.spacing().interact_size.y + 4.0);
        ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(Align::Center), |ui| {
            ui.checkbox(&mut self.follow, "Follow");
            let margin = ui.spacing().button_padding.x * 2.0;
            ui.add(egui::TextEdit::singleline(&mut self.tr_filter).hint_text("Find in transcript…").desired_width((ui.available_width() - margin).max(40.0)));
        });
        let Some(o) = &self.open else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("The transcript shows up here").color(DIM)));
            return;
        };
        let (hdr, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
        ui.painter().rect_filled(hdr, 0.0, PURPLE);
        ui.painter().text(hdr.left_center() + Vec2::new(6.0, 0.0), Align2::LEFT_CENTER, "Time", bold(14.0), YELLOW);
        ui.painter().text(hdr.left_center() + Vec2::new(TIME_W, 0.0), Align2::LEFT_CENTER, "Text", bold(14.0), YELLOW);

        let q = self.tr_filter.trim().to_lowercase();
        let playing_here = self.player_for.as_deref() == Some(o.meta.id.as_str()) && self.player.is_loaded();
        let mut clicked = None;
        let scroll = std::mem::take(&mut self.scroll_to_active);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let w = ui.available_width();
            let text_w = (w - TIME_W - 8.0).max(60.0);
            if o.transcript.segments.is_empty() {
                ui.label(RichText::new("No speech found.").color(DIM));
            }
            for (i, s) in o.transcript.segments.iter().enumerate() {
                if !q.is_empty() && !s.text.to_lowercase().contains(&q) {
                    continue;
                }
                let on = self.active == Some(i);
                let text = match &s.speaker {
                    Some(sp) => format!("{sp}: {}", s.text),
                    None => s.text.clone(),
                };
                let galley = ui.painter().layout(visual(&text).into_owned(), pixel(16.0), YELLOW, text_w);
                let h = galley.size().y + 6.0;
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
                if ui.is_rect_visible(rect) {
                    row_bg(ui, rect, i, resp.hovered(), on);
                    let p = ui.painter();
                    let tm = if on { format!("▸ {}", fmt_ts(s.start, false)) } else { fmt_ts(s.start, false) };
                    p.text(rect.left_top() + Vec2::new(6.0, 3.0), Align2::LEFT_TOP, tm, pixel(16.0), if on { MAGENTA } else { TEAL });
                    p.galley_with_override_text_color(rect.left_top() + Vec2::new(TIME_W, 3.0), galley, if on { TEAL } else { YELLOW });
                }
                if on && scroll {
                    resp.scroll_to_me(Some(Align::Center));
                }
                if resp.clicked() {
                    clicked = Some(s.start);
                }
                resp.on_hover_text_at_pointer(if playing_here || o.meta.media.is_some() { "Play from here" } else { "Highlight (download the audio to play)" });
            }
        });
        if let Some(t) = clicked {
            self.jump(t);
        }
    }

    fn taskbar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            let s = Vec2::new(30.0, 26.0);
            let mine = self.open.as_ref().is_some_and(|o| self.player_for.as_deref() == Some(o.meta.id.as_str())) && self.player.is_loaded();
            let playing = mine && self.player.is_playing();
            if icon_button(ui, s, if playing { Icon::Pause } else { Icon::Play }).on_hover_text("Play / pause").clicked() {
                self.toggle_play();
            }
            if icon_button(ui, s, Icon::Stop).on_hover_text("Stop").clicked() {
                self.player.stop();
                self.player_for = None;
            }
            vline(ui);

            let dur = self.open.as_ref().map(|o| o.meta.duration).unwrap_or(0.0);
            let pos = if mine { self.player.position() } else { 0.0 };
            let time = format!("{} / {}", fmt_time(pos), fmt_time(dur));
            let (rail_box, _) = ui.allocate_exact_size(Vec2::new((ui.available_width() * 0.3).max(120.0), 26.0), Sense::hover());
            let r = Rect::from_center_size(rail_box.center(), Vec2::new(rail_box.width(), 8.0));
            ui.painter().rect_filled(r.expand(2.0), 2.0, INK);
            let frac = if dur > 0.0 { (pos / dur) as f32 } else { 0.0 };
            if let Some(f) = seek_rail(ui, r, frac, INK, YELLOW, WHITE) {
                self.jump(f as f64 * dur);
            }
            ui.label(RichText::new(time).color(INK));
            let (vr, _) = ui.allocate_exact_size(Vec2::new(70.0, 26.0), Sense::hover());
            let vr = Rect::from_center_size(vr.center(), Vec2::new(vr.width(), 6.0));
            ui.painter().rect_filled(vr.expand(2.0), 2.0, INK);
            if let Some(v) = seek_rail(ui, vr, self.player.volume(), INK, TEAL, WHITE) {
                self.player.set_volume(v);
                self.saved.volume = v;
            }
            vline(ui);

            // right: library size; middle: job status
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let total: f64 = self.library.iter().map(|m| m.duration).sum();
                let n = self.library.len();
                slot(ui, |ui| {
                    ui.label(RichText::new(format!("{n} recap{} · {}", if n == 1 { "" } else { "s" }, fmt_time(total))).color(YELLOW));
                });
                let busy = self.busy || self.side_busy.is_some();
                let (msg, err) = match (&self.side_busy, self.busy) {
                    (Some(s), false) => (s.clone(), false),
                    _ => (self.status.clone(), self.status_err),
                };
                let w = ui.available_width();
                slot(ui, |ui| {
                    ui.set_width(w - 18.0);
                    ui.horizontal_centered(|ui| {
                        let (dot, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                        let blink = busy && (self.time() * 2.0) as i64 % 2 == 0;
                        let c = if err { MAGENTA } else if busy { if blink { YELLOW } else { INK } } else { TEAL };
                        ui.painter().rect_filled(dot.shrink(1.0), 0.0, c);
                        let bar_w = if self.frac.is_some() && busy { 120.0 } else { 0.0 };
                        let label_w = ui.available_width() - bar_w - 8.0;
                        ui.add_sized([label_w, 20.0], egui::Label::new(RichText::new(&msg).color(if err { MAGENTA } else { YELLOW })).truncate()).on_hover_text(&msg);
                        if let (Some(f), true) = (self.frac, busy) {
                            let (b, _) = ui.allocate_exact_size(Vec2::new(bar_w - 8.0, 10.0), Sense::hover());
                            ui.painter().rect_stroke(b, 0.0, Stroke::new(1.0f32, YELLOW), StrokeKind::Inside);
                            let fill = Rect::from_min_size(b.min, Vec2::new(b.width() * f.clamp(0.0, 1.0), b.height()));
                            ui.painter().rect_filled(fill.shrink(1.0), 0.0, YELLOW);
                        }
                    });
                });
            });
        });
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        egui::Window::new(RichText::new("Settings").font(bold(18.0)))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .show(ctx, |ui| {
                let heading = |ui: &mut Ui, text: &str| {
                    ui.add_space(6.0);
                    ui.label(RichText::new(text).font(bold(16.0)).color(TEAL));
                    ui.separator();
                };
                heading(ui, "Claude (summaries and Q&A)");
                let env = std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.trim().is_empty()).is_some();
                ui.horizontal(|ui| {
                    ui.label("API key");
                    ui.add_enabled(!env, egui::TextEdit::singleline(&mut self.saved.api_key).password(true).hint_text("sk-ant-…").desired_width(300.0));
                });
                ui.label(RichText::new(if env { "Using ANTHROPIC_API_KEY from the environment." } else { "Saved in plain text in your settings file. Or set ANTHROPIC_API_KEY instead." }).small().color(DIM));
                ui.horizontal(|ui| {
                    ui.label(format!("Model: {}   Effort", claude::MODEL));
                    egui::ComboBox::from_id_salt("effort").selected_text(&self.saved.effort).show_ui(ui, |ui| {
                        for e in ["low", "medium", "high"] {
                            ui.selectable_value(&mut self.saved.effort, e.to_string(), e);
                        }
                    });
                });

                heading(ui, "Transcription");
                ui.horizontal(|ui| {
                    ui.label("Whisper model");
                    egui::ComboBox::from_id_salt("model").selected_text(&self.saved.model).show_ui(ui, |ui| {
                        for m in transcribe::MODELS {
                            ui.selectable_value(&mut self.saved.model, m.to_string(), m);
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.label("Spoken language");
                    ui.add(egui::TextEdit::singleline(&mut self.saved.language).hint_text("auto").desired_width(60.0));
                    ui.label(RichText::new("code like en, ja; blank = detect").small().color(DIM));
                });
                let tool = |ui: &mut Ui, name: &str, p: Option<PathBuf>, extra: &str| {
                    let (txt, c) = match &p {
                        Some(p) => (format!("✓ {name}: {}{extra}", p.display()), YELLOW),
                        None => (format!("✗ {name}: not found"), MAGENTA),
                    };
                    ui.label(RichText::new(txt).small().color(c));
                };
                let cli = tools::whisper_cli();
                let vk = cli.as_ref().map(|c| if tools::has_vulkan(c) { "  (Vulkan GPU)" } else { "  (CPU build)" }).unwrap_or("");
                tool(ui, "whisper.cpp", cli.clone(), vk);
                tool(ui, "ffmpeg", tools::ffmpeg(), "");
                tool(ui, "yt-dlp", tools::yt_dlp(), "");
                ui.horizontal(|ui| {
                    if ui.button("Open tools folder").clicked() {
                        let _ = std::fs::create_dir_all(tools::tools_dir());
                        tools::open_in_file_manager(&tools::tools_dir());
                    }
                    if ui.button("Re-check").clicked() {
                        self.engine = engine();
                    }
                });
                ui.label(RichText::new("Tools are found next to recap.exe, in the tools folder, or on PATH.").small().color(DIM));

                heading(ui, "Look");
                if ui.checkbox(&mut self.saved.glow, "Neon glow (HDR-style bloom)").changed() {
                    set_glow(self.saved.glow);
                }
                heading(ui, "Data");
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tools::home().display().to_string()).small().color(DIM));
                    if ui.button("Open").clicked() {
                        let _ = std::fs::create_dir_all(tools::home());
                        tools::open_in_file_manager(&tools::home());
                    }
                });
            });
        self.show_settings = open;
    }

    fn take_screenshot(&mut self, ctx: &egui::Context) {
        let now = self.time();
        let Some(shot) = &mut self.shot else { return };
        let mut img = None;
        ctx.input(|i| {
            for e in &i.raw.events {
                if let egui::Event::Screenshot { image, .. } = e {
                    img = Some(image.clone());
                }
            }
        });
        if let (Some(img), true) = (img, shot.waiting) {
            let _ = image::save_buffer(&shot.path, img.as_raw(), img.width() as u32, img.height() as u32, image::ColorType::Rgba8);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if !shot.waiting && now >= shot.at {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            shot.waiting = true;
        }
    }
}

pub struct CliArgs {
    pub source: String,
    pub style: String,
    pub summarize: bool,
    pub force_transcribe: bool,
    pub model: String,
    pub language: Option<String>,
}

/// `recap --cli <source>`: the same pipeline without the window, progress on stdout.
/// The Claude key comes from ANTHROPIC_API_KEY.
pub fn cli(a: CliArgs) -> i32 {
    let key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
    let opts = sources::Options { model: a.model, language: a.language, force_transcribe: a.force_transcribe };
    if a.summarize && key.trim().is_empty() {
        println!("note: ANTHROPIC_API_KEY not set, skipping the summary");
    }
    let summary = (a.summarize && !key.trim().is_empty()).then(|| (Client { api_key: key, effort: "medium".into() }, a.style));
    let source = a.source;
    let (tx, rx) = channel();
    let progress = |_: &str, f: Option<f32>, m: &str| match f {
        Some(f) => println!("[{:>3.0}%] {m}", f * 100.0),
        None => println!("[ .. ] {m}"),
    };
    let r = run_recap(&source, &opts, summary, &progress, &tx);
    for m in rx.try_iter() {
        if let Msg::Warn(w) = m {
            println!("warning: {w}");
        }
    }
    match r {
        Ok(id) => {
            println!("saved {id}");
            0
        }
        Err(e) => {
            println!("error: {e}");
            1
        }
    }
}

/// Runs on a worker thread: source -> transcript -> (summary) -> library.
fn run_recap(source: &str, opts: &sources::Options, summary: Option<(Client, String)>, progress: transcribe::Progress, tx: &Sender<Msg>) -> Result<String, String> {
    let kind = sources::kind_of(source)?;
    let (t, media, _tmp) = sources::load(source, opts, progress)?;

    // Keep something playable. Video: just the audio track (no re-encode). Audio: the file.
    let tmp_audio = tools::TempDir::new().map_err(|e| e.to_string())?;
    let media = match (kind, media) {
        // video (also a video found next to imported subtitles): keep only its audio track
        (_, Some(v)) if sources::kind_of(&v.to_string_lossy()) == Ok(Kind::Video) => {
            progress("save", None, "Saving audio for playback");
            let out = tmp_audio.0.join("media.mka");
            let ok = tools::ffmpeg()
                .and_then(|ff| {
                    tools::command(&ff)
                        .args(["-nostdin", "-loglevel", "error", "-y", "-i"])
                        .arg(&v)
                        .args(["-vn", "-map", "0:a:0", "-c:a", "copy"])
                        .arg(&out)
                        .status()
                        .ok()
                })
                .is_some_and(|s| s.success());
            ok.then_some(out)
        }
        (_, m) => m,
    };

    let mut s = None;
    if let Some((client, style)) = &summary {
        match client.summarize(&t, style, progress) {
            Ok(x) => s = Some(x),
            Err(e) => {
                let _ = tx.send(Msg::Warn(format!("Transcript saved, but the summary failed: {e}")));
            }
        }
    }
    progress("save", Some(1.0), "Saving");
    library::save(&t, s.as_ref(), summary.as_ref().map(|(_, st)| st.as_str()), media.as_deref())
}

fn engine() -> (String, bool) {
    match tools::whisper_cli() {
        Some(c) if tools::has_vulkan(&c) => ("whisper.cpp · Vulkan GPU".into(), true),
        Some(_) => ("whisper.cpp · CPU".into(), false),
        None => ("whisper.cpp not installed".into(), false),
    }
}

/// A short name for a queued source: the file name, or the link.
fn source_label(src: &str) -> String {
    if src.starts_with("http") {
        return src.to_string();
    }
    std::path::Path::new(src).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| src.to_string())
}

fn method_short(m: &str) -> String {
    if m.starts_with("YouTube") {
        return "YouTube captions".into();
    }
    let base = m.split(" (").next().unwrap_or(m).to_string();
    if m.contains("Vulkan") { format!("{base} · GPU") } else { base }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = self.time();
        self.pump(ctx);

        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        let mut dropped: Vec<PathBuf> = dropped.into_iter().filter(|p| sources::is_media_file(p)).collect();
        if dropped.len() == 1 {
            self.file = dropped.pop();
            self.url.clear();
        } else if dropped.len() > 1 {
            // several files at once go straight into the queue
            dropped.sort();
            self.enqueue(ctx, dropped.into_iter().map(|p| p.to_string_lossy().into_owned()).collect());
        }

        // keep the highlighted line in step with playback
        if let Some(o) = &self.open {
            if self.player_for.as_deref() == Some(o.meta.id.as_str()) && self.player.is_playing() {
                let a = self.segment_at(self.player.position());
                if a != self.active {
                    self.active = a;
                    self.scroll_to_active = self.follow;
                }
            }
        }

        paint_night_sky(&ctx.layer_painter(egui::LayerId::background()), ctx.screen_rect(), now);

        egui::TopBottomPanel::top("header").frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 6))).show(ctx, |ui| self.header(ui));
        egui::TopBottomPanel::bottom("taskbar")
            .exact_height(40.0)
            .frame(egui::Frame::new().fill(MAGENTA).inner_margin(egui::Margin::symmetric(6, 4)))
            .show(ctx, |ui| {
                let r = ui.max_rect().expand2(Vec2::new(6.0, 4.0));
                ui.painter().line_segment([r.left_top(), r.right_top()], Stroke::new(2.0f32, WHITE));
                self.taskbar(ui);
            });

        let pad = egui::Frame::new().inner_margin(egui::Margin::same(10));
        egui::SidePanel::left("left").resizable(true).default_width(300.0).width_range(240.0..=440.0).frame(pad).show(ctx, |ui| {
            window(ui, "New Recap", |ui| self.new_recap(ui, ctx));
            ui.add_space(12.0);
            let h = ui.available_height();
            fill_window(ui, "Library", h, |ui| self.library_view(ui));
        });
        // Cap at half the window so the Summary/Ask column always keeps room (this also pulls
        // back a width that was saved while the panel was creeping).
        let screen_w = ctx.screen_rect().width();
        let right_w = (screen_w * 0.4).clamp(320.0, 720.0);
        let right_max = (screen_w * 0.5).max(300.0);
        egui::SidePanel::right("transcript").resizable(true).default_width(right_w.min(right_max)).width_range(280.0..=right_max).frame(pad).show(ctx, |ui| {
            let h = ui.available_height();
            fill_window(ui, "Transcript", h, |ui| self.transcript_view(ui));
        });
        egui::CentralPanel::default().frame(pad).show(ctx, |ui| {
            let ask_h = 170.0;
            let h = ui.available_height() - ask_h - 12.0;
            fill_window(ui, "Summary", h, |ui| self.summary_view(ui, ctx));
            ui.add_space(12.0);
            fill_window(ui, "Ask", ask_h, |ui| self.ask_view(ui, ctx));
        });

        self.settings_window(ctx);
        match self.explorer.show(ctx, &self.library) {
            Some(explorer::Action::Open(id, at)) => self.open_item(&id, at),
            Some(explorer::Action::ShowInFolder(id)) => {
                if let Ok(d) = library::dir(&id) {
                    tools::open_in_file_manager(&d);
                }
            }
            Some(explorer::Action::Rename(id, title)) => match library::set_title(&id, &title) {
                Ok(m) => {
                    if let Some(o) = self.open.as_mut().filter(|o| o.meta.id == id) {
                        o.meta = m;
                    }
                    self.set_status("Renamed", false);
                    self.refresh_library();
                }
                Err(e) => self.set_status(e, true),
            },
            Some(explorer::Action::Delete(id)) => self.delete_item(&id),
            None => {}
        }
        self.take_screenshot(ctx);

        let busy = self.busy || self.side_busy.is_some();
        if self.player.is_playing() || busy || self.shot.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(if busy && !self.player.is_playing() { 250 } else { 33 }));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(66));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "recap", &self.saved);
    }
}

// ---------------------------------------------------------------------------------------------
// widgets

const TIME_W: f32 = 74.0;

/// A site window that fills `height` (minus room for its drop shadow).
/// The parent only ever sees the fixed rect, so content that overflows can't widen a side panel.
fn fill_window(ui: &mut Ui, title: &str, height: f32, add: impl FnOnce(&mut Ui)) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, (height - 6.0).max(80.0)), Sense::hover());
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(*ui.layout()));
    window(&mut child, title, |ui| {
        ui.set_min_height(ui.available_height());
        add(ui)
    });
}

fn row_bg(ui: &Ui, rect: Rect, i: usize, hovered: bool, on: bool) {
    let p = ui.painter();
    let bg = if hovered { Color32::from_rgb(0x2a, 0x22, 0x50) } else if i % 2 == 1 { Color32::from_rgb(0x22, 0x20, 0x3a) } else { Color32::TRANSPARENT };
    p.rect_filled(rect, 0.0, bg);
    if on {
        p.rect_stroke(rect.shrink(0.5), 0.0, Stroke::new(1.5f32, MAGENTA), StrokeKind::Inside);
        glow_rect(p, rect, 0.0, MAGENTA, 4.0);
    }
}

fn heading(ui: &mut Ui, text: &str) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(text).font(bold(16.0)).color(TEAL));
        let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 16.0), Sense::hover());
        ui.painter().line_segment([r.left_center(), r.right_center()], Stroke::new(1.0f32, YELLOW.gamma_multiply(0.6)));
    });
    ui.add_space(2.0);
}

fn bullet(ui: &mut Ui, mark: &str, c: Color32, text: &str) {
    ui.horizontal_top(|ui| {
        ui.label(RichText::new(mark).color(c));
        ui.add(egui::Label::new(RichText::new(visual(text)).color(YELLOW)).wrap());
    });
}

fn ts_button(ui: &mut Ui, t: f64) -> bool {
    let text = fmt_ts(t, false);
    let g = ui.painter().layout_no_wrap(text.clone(), pixel(12.0), TEAL);
    let (rect, resp) = ui.allocate_exact_size(g.size() + Vec2::new(10.0, 6.0), Sense::click());
    let p = ui.painter();
    let (bg, fg) = if resp.hovered() { (TEAL, INK) } else { (INK, TEAL) };
    p.rect_filled(rect, 1.0, bg);
    p.rect_stroke(rect, 1.0, Stroke::new(1.0f32, TEAL), StrokeKind::Inside);
    if resp.hovered() {
        glow_rect(p, rect, 1.0, TEAL, 4.0);
    }
    p.text(rect.center(), Align2::CENTER_CENTER, text, pixel(12.0), fg);
    resp.on_hover_text("Jump here").clicked()
}

/// Answer text with its [hh:mm:ss] citations turned into jump buttons.
fn rich_answer(ui: &mut Ui, text: &str) -> Option<f64> {
    let mut jump = None;
    for para in text.split('\n') {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let mut rest = para;
            while !rest.is_empty() {
                let next = rest.find('[').and_then(|a| rest[a..].find(']').map(|b| (a, a + b)));
                match next.and_then(|(a, b)| parse_ts(&rest[a + 1..b]).map(|t| (a, b, t))) {
                    Some((a, b, t)) => {
                        if a > 0 {
                            ui.label(RichText::new(visual(&rest[..a])).color(WHITE));
                        }
                        ui.add_space(2.0);
                        if ts_button(ui, t) {
                            jump = Some(t);
                        }
                        ui.add_space(2.0);
                        rest = &rest[b + 1..];
                    }
                    None => {
                        // no citation left (or a bracket that isn't one): print up to just past it
                        let cut = next.map(|(_, b)| b + 1).unwrap_or(rest.len());
                        ui.label(RichText::new(visual(&rest[..cut])).color(WHITE));
                        rest = &rest[cut..];
                    }
                }
            }
        });
    }
    jump
}

fn big_button(ui: &mut Ui, text: &str, enabled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), if enabled { Sense::click() } else { Sense::hover() });
    // leave room bottom-right for the hard drop shadow
    let r = Rect::from_min_max(rect.min + Vec2::splat(2.0), rect.max - Vec2::splat(4.0));
    let pressed = resp.is_pointer_button_down_on();
    let r = if pressed { r.translate(Vec2::splat(2.0)) } else { r };
    let p = ui.painter();
    let fill = if !enabled { YELLOW.gamma_multiply(0.45) } else if resp.hovered() { TEAL } else { YELLOW };
    if !pressed {
        p.rect_filled(r.translate(Vec2::splat(4.0)), 0.0, Color32::BLACK);
    }
    p.rect_filled(r, 0.0, fill);
    p.rect_stroke(r, 0.0, Stroke::new(2.0f32, INK), StrokeKind::Inside);
    if enabled {
        glow_rect(p, r, 0.0, fill, 6.0);
    }
    p.text(r.center(), Align2::CENTER_CENTER, text, bold(16.0), INK);
    resp
}

fn slot(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new().fill(INK).stroke(Stroke::new(2.0f32, YELLOW)).inner_margin(egui::Margin::symmetric(6, 2)).show(ui, add);
}

fn vline(ui: &mut Ui) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(4.0, 28.0), Sense::hover());
    ui.painter().rect_filled(Rect::from_center_size(r.center(), Vec2::new(2.0, 26.0)), 0.0, YELLOW);
    ui.painter().rect_filled(Rect::from_center_size(r.center() + Vec2::new(2.0, 0.0), Vec2::new(1.0, 26.0)), 0.0, WHITE);
}

#[derive(Clone, Copy, PartialEq)]
enum Icon {
    Play,
    Pause,
    Stop,
}

fn icon_button(ui: &mut Ui, size: Vec2, icon: Icon) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    let (bg, fg, border) = if resp.is_pointer_button_down_on() {
        (TEAL, INK, YELLOW)
    } else if resp.hovered() {
        (INDIGO, TEAL, TEAL)
    } else {
        (INK, YELLOW, YELLOW)
    };
    p.rect_filled(rect, 2.0, bg);
    p.rect_stroke(rect, 2.0, Stroke::new(1.5f32, border), StrokeKind::Inside);
    let r = rect.shrink(4.0);
    let s = r.height().min(r.width()) * 0.5;
    let c = r.center();
    match icon {
        Icon::Play => {
            p.add(Shape::convex_polygon(vec![c + Vec2::new(-s * 0.42, -s * 0.55), c + Vec2::new(s * 0.58, 0.0), c + Vec2::new(-s * 0.42, s * 0.55)], fg, Stroke::NONE));
        }
        Icon::Pause => {
            for dx in [-0.28, 0.28] {
                p.rect_filled(Rect::from_center_size(c + Vec2::new(dx * s, 0.0), Vec2::new(s * 0.3, s * 1.1)), 0.0, fg);
            }
        }
        Icon::Stop => {
            p.rect_filled(Rect::from_center_size(c, Vec2::splat(s * 0.95)), 0.0, fg);
        }
    }
    resp
}

/// A horizontal progress/seek rail. Returns the fraction clicked or dragged to.
fn seek_rail(ui: &mut Ui, rect: Rect, frac: f32, track: Color32, fill: Color32, knob: Color32) -> Option<f32> {
    let resp = ui.interact(rect.expand2(Vec2::new(0.0, 4.0)), ui.id().with(("rail", rect.min.x as i32, rect.min.y as i32)), Sense::click_and_drag());
    let p = ui.painter();
    p.rect_filled(rect, rect.height() * 0.5, track);
    let mut f = frac.clamp(0.0, 1.0);
    if let Some(pos) = resp.interact_pointer_pos() {
        f = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    }
    let filled = Rect::from_min_max(rect.min, Pos2::new(rect.left() + rect.width() * f, rect.bottom()));
    p.rect_filled(filled, rect.height() * 0.5, fill);
    p.circle_filled(Pos2::new(filled.right(), rect.center().y), rect.height() * 0.9 + 1.0, knob);
    (resp.drag_stopped() || resp.clicked()).then_some(f)
}

/// A pixel icon with a label under it, like JAMP's skin picker.
fn style_icon(ui: &mut Ui, style: &str, active: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(62.0, 56.0), Sense::click());
    let p = ui.painter();
    if active || resp.hovered() {
        p.rect_filled(rect, 3.0, if active { INDIGO } else { white_a(10) });
        p.rect_stroke(rect, 3.0, Stroke::new(1.0f32, if active { YELLOW } else { TEAL }), StrokeKind::Inside);
    }
    let ic = Rect::from_center_size(rect.center_top() + Vec2::new(0.0, 19.0), Vec2::splat(30.0));
    let c = ic.center();
    let px = |x: f32, y: f32, w: f32, h: f32, col: Color32| p.rect_filled(Rect::from_min_size(ic.min + Vec2::new(x, y), Vec2::new(w, h)), 0.0, col);
    match style {
        "general" => {
            px(5.0, 1.0, 20.0, 28.0, WHITE);
            px(5.0, 1.0, 20.0, 6.0, TEAL);
            for (i, w) in [14.0, 14.0, 9.0].iter().enumerate() {
                px(8.0, 11.0 + i as f32 * 5.0, *w, 2.0, if i == 1 { MAGENTA } else { INK });
            }
        }
        "meeting" => {
            px(3.0, 4.0, 8.0, 8.0, YELLOW);
            px(19.0, 4.0, 8.0, 8.0, TEAL);
            px(0.0, 13.0, 14.0, 10.0, YELLOW);
            px(16.0, 13.0, 14.0, 10.0, TEAL);
            px(1.0, 24.0, 28.0, 4.0, WHITE);
        }
        "lecture" => {
            px(0.0, 3.0, 30.0, 19.0, YELLOW);
            px(2.0, 5.0, 26.0, 15.0, Color32::from_rgb(0x1d, 0x4a, 0x3a));
            px(5.0, 8.0, 12.0, 2.0, WHITE);
            px(5.0, 12.0, 17.0, 2.0, WHITE);
            px(5.0, 16.0, 8.0, 2.0, TEAL);
            px(7.0, 22.0, 3.0, 7.0, YELLOW);
            px(20.0, 22.0, 3.0, 7.0, YELLOW);
        }
        _ => {
            px(10.0, 0.0, 10.0, 16.0, MAGENTA);
            for y in [3.0, 7.0, 11.0] {
                px(12.0, y, 6.0, 1.5, INK);
            }
            px(6.0, 11.0, 2.0, 7.0, WHITE);
            px(22.0, 11.0, 2.0, 7.0, WHITE);
            px(8.0, 18.0, 14.0, 2.0, WHITE);
            px(14.0, 20.0, 2.0, 5.0, WHITE);
            px(9.0, 26.0, 12.0, 2.0, WHITE);
        }
    }
    let _ = c;
    let fg = if active { YELLOW } else if resp.hovered() { TEAL } else { WHITE };
    p.text(rect.center_bottom() - Vec2::new(0.0, 3.0), Align2::CENTER_BOTTOM, style_name(style), pixel(14.0), fg);
    let tip = match style {
        "meeting" => "Meeting notes: decisions, owners, action items",
        "lecture" => "Lecture notes: concepts and things to review",
        "podcast" => "Podcast recap: arguments, quotes, recommendations",
        _ => "General summary",
    };
    resp.on_hover_text(tip)
}

/// The empty Summary window: a little pixel sunset, like JAMP's placeholder cover.
fn empty_art(ui: &mut Ui) {
    let avail = ui.available_rect_before_wrap();
    let art = Rect::from_center_size(avail.center() - Vec2::new(0.0, 30.0), Vec2::new(200.0, 130.0));
    let p = ui.painter();
    let mut mesh = egui::epaint::Mesh::default();
    for (y, col) in [(art.top(), INDIGO), (art.top() + art.height() * 0.62, MAGENTA)] {
        mesh.colored_vertex(Pos2::new(art.left(), y), col);
        mesh.colored_vertex(Pos2::new(art.right(), y), col);
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    p.add(Shape::mesh(mesh));
    let horizon = art.top() + art.height() * 0.62;
    let sun = Pos2::new(art.center().x, horizon);
    p.with_clip_rect(Rect::from_min_max(art.min, Pos2::new(art.right(), horizon))).circle_filled(sun, 36.0, YELLOW);
    for k in 0..4 {
        let y = horizon - 6.0 - k as f32 * 7.0;
        p.rect_filled(Rect::from_min_size(Pos2::new(sun.x - 40.0, y), Vec2::new(80.0, 2.0 + (3 - k) as f32 * 0.6)), 0.0, MAGENTA);
    }
    let ground = Rect::from_min_max(Pos2::new(art.left(), horizon), art.max);
    p.rect_filled(ground, 0.0, INK);
    let gp = p.with_clip_rect(ground);
    for i in -6..=6 {
        gp.line_segment([Pos2::new(sun.x + i as f32 * 6.0, horizon), Pos2::new(sun.x + i as f32 * 40.0, art.bottom())], Stroke::new(1.0f32, TEAL.gamma_multiply(0.8)));
    }
    for k in 1..5 {
        let y = horizon + (k * k) as f32 * 3.0;
        gp.line_segment([Pos2::new(art.left(), y), Pos2::new(art.right(), y)], Stroke::new(1.0f32, TEAL.gamma_multiply(0.8)));
    }
    p.rect_stroke(art, 0.0, Stroke::new(2.0f32, YELLOW), StrokeKind::Outside);
    glow_rect(p, art, 0.0, YELLOW, 6.0);
    let blink = if (ui.input(|i| i.time) * 1.0) as i64 % 2 == 0 { "_" } else { " " };
    p.text(art.center_bottom() + Vec2::new(0.0, 22.0), Align2::CENTER_TOP, "Paste a link or drop a file", pixel(16.0), DIM);
    p.text(art.center_bottom() + Vec2::new(0.0, 42.0), Align2::CENTER_TOP, format!("to make your first recap{blink}"), pixel(16.0), DIM);
}
