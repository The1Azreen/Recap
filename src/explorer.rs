//! Transcript Explorer: a file-explorer window over the library, modelled on the project
//! explorer on the1azreen.neocities.org (toolbar with Back / Up / address bar / sort, then a
//! folder tree, a large-icon view and a details pane, and a status bar).
//!
//! Folders are built from the recaps' own metadata, so a new source, month or summary style
//! gets its own folder automatically. The search box looks through titles and the full text
//! of every transcript.

use std::collections::HashMap;
use std::io::Read;
use std::sync::mpsc;

use egui::text::LayoutJob;
use egui::{Align, Align2, Color32, Pos2, Rect, RichText, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};

use crate::library;
use crate::model::{fmt_ts, Meta};
use crate::theme::*;

pub enum Action {
    /// Open this recap, optionally jumping to a time.
    Open(String, Option<f64>),
    ShowInFolder(String),
    Rename(String, String),
    Delete(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Newest,
    Oldest,
    AZ,
    ZA,
    Longest,
}

const SORTS: [(Sort, &str); 5] =
    [(Sort::Newest, "Newest first"), (Sort::Oldest, "Oldest first"), (Sort::AZ, "Name (A-Z)"), (Sort::ZA, "Name (Z-A)"), (Sort::Longest, "Longest first")];

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

struct Node {
    name: String,
    kind: Kind,
}

enum Kind {
    Folders(Vec<Node>),
    /// Indexes into the library.
    Items(Vec<usize>),
}

impl Node {
    fn describe(&self) -> String {
        match &self.kind {
            Kind::Folders(f) => plural(f.len(), "folder", "folders"),
            Kind::Items(i) => plural(i.len(), "transcript", "transcripts"),
        }
    }
}

/// What the selection in the item view points at.
#[derive(Clone, PartialEq)]
enum Sel {
    Folder(String),
    Item(String),
}

struct Hit {
    idx: usize,
    count: usize,
    /// First matching line: (time, text).
    first: Option<(f64, String)>,
}

#[derive(Default)]
pub struct Explorer {
    pub open: bool,
    path: Vec<String>,
    back: Vec<Vec<String>>,
    selected: Option<Sel>,
    query: String,
    sort_idx: usize,
    /// Lower-cased transcript lines per recap id, loaded the first time a search needs them.
    text: HashMap<String, Vec<(f64, String, String)>>,
    tldr: HashMap<String, Option<String>>,
    hits: Option<(String, usize, Vec<Hit>)>,
    /// (recap id, name being typed)
    renaming: Option<(String, String)>,
    /// (recap id, armed until): Delete asks once more before it deletes
    arm_delete: Option<(String, f64)>,
    focus_rename: bool,
    /// YouTube thumbnails per recap id: Some once loaded, None while loading or when unavailable.
    thumbs: HashMap<String, Option<egui::TextureHandle>>,
    thumb_worker: Option<ThumbWorker>,
}

/// One background thread that fetches thumbnails one at a time.
struct ThumbWorker {
    jobs: mpsc::Sender<(String, String)>,
    done: mpsc::Receiver<(String, Option<egui::ColorImage>)>,
}

impl Explorer {
    pub fn new(open: bool) -> Self {
        Explorer { open, ..Default::default() }
    }

    /// Forget cached transcript text and summaries (call after the library changes).
    pub fn invalidate(&mut self) {
        self.text.clear();
        self.tldr.clear();
        self.hits = None;
    }

    fn sort(&self) -> Sort {
        SORTS[self.sort_idx.min(SORTS.len() - 1)].0
    }

    fn go(&mut self, path: Vec<String>) {
        if path != self.path {
            self.back.push(std::mem::replace(&mut self.path, path));
        }
        self.selected = None;
    }

    pub fn show(&mut self, ctx: &egui::Context, library: &[Meta]) -> Option<Action> {
        if !self.open {
            return None;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            // Esc backs out of a rename first, then closes the explorer
            if self.renaming.take().is_none() {
                self.open = false;
                return None;
            }
        }
        let screen = ctx.screen_rect();
        let size = Vec2::new((screen.width() - 80.0).clamp(640.0, 1200.0), (screen.height() - 140.0).clamp(420.0, 760.0));
        let mut action = None;
        egui::Window::new("Transcript Explorer")
            .id(egui::Id::new("transcript-explorer"))
            .title_bar(false)
            .frame(egui::Frame::new())
            .collapsible(false)
            .resizable(false)
            .fixed_size(size)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(screen.center())
            .show(ctx, |ui| {
                window(ui, "Transcript Explorer", |ui| action = self.body(ui, library));
            });
        if matches!(action, Some(Action::Open(..))) {
            self.open = false;
        }
        action
    }

    fn body(&mut self, ui: &mut Ui, library: &[Meta]) -> Option<Action> {
        let tree = build_tree(library);
        // a folder can vanish (e.g. its last recap was deleted): fall back to the root
        if resolve(&tree, &self.path).is_none() {
            self.path.clear();
        }
        let q = self.query.trim().to_lowercase();
        if q.is_empty() {
            self.hits = None;
        } else if self.hits.as_ref().is_none_or(|(hq, n, _)| *hq != q || *n != library.len()) {
            let hits = self.search(library, &q);
            self.hits = Some((q.clone(), library.len(), hits));
        }

        // thumbnails that finished loading since the last frame
        if let Some(w) = &self.thumb_worker {
            for (id, img) in w.done.try_iter() {
                let tex = img.map(|img| ui.ctx().load_texture(format!("thumb-{id}"), img, egui::TextureOptions::LINEAR));
                self.thumbs.insert(id, tex);
            }
        }
        let (area, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
        let tool_r = Rect::from_min_size(area.min, Vec2::new(area.width(), 26.0));
        let status_r = Rect::from_min_max(Pos2::new(area.left(), area.bottom() - 22.0), area.max);
        let panes = Rect::from_min_max(Pos2::new(area.left(), tool_r.bottom() + 6.0), Pos2::new(area.right(), status_r.top() - 6.0));
        let tree_w = (panes.width() * 0.2).clamp(170.0, 230.0);
        let prev_w = (panes.width() * 0.26).clamp(220.0, 300.0);
        let tree_r = Rect::from_min_max(panes.min, Pos2::new(panes.left() + tree_w, panes.bottom()));
        let prev_r = Rect::from_min_max(Pos2::new(panes.right() - prev_w, panes.top()), panes.max);
        let list_r = Rect::from_min_max(Pos2::new(tree_r.right(), panes.top()), Pos2::new(prev_r.left(), panes.bottom()));

        let p = ui.painter();
        p.rect_filled(tree_r, 0.0, PURPLE);
        p.rect_filled(list_r, 0.0, INK);
        p.rect_filled(prev_r, 0.0, PURPLE);
        p.rect_stroke(panes, 0.0, Stroke::new(2.0f32, YELLOW), StrokeKind::Inside);
        for x in [tree_r.right(), prev_r.left()] {
            p.line_segment([Pos2::new(x, panes.top()), Pos2::new(x, panes.bottom())], Stroke::new(2.0f32, YELLOW));
        }

        let mut action = None;
        self.toolbar(&mut child(ui, tool_r), &tree);
        self.tree_pane(&mut child(ui, tree_r.shrink(4.0)), &tree);
        let shown = self.list_pane(&mut child(ui, list_r.shrink(3.0)), &tree, library, &mut action);
        self.preview_pane(&mut child(ui, prev_r.shrink2(Vec2::new(8.0, 6.0))), &tree, library, &mut action);
        self.status(ui, status_r, library, &shown);
        action
    }

    // ------------------------------------------------------------------------------- toolbar

    fn toolbar(&mut self, ui: &mut Ui, tree: &Node) {
        ui.horizontal_centered(|ui| {
            if ui.add_enabled(!self.back.is_empty(), egui::Button::new("◀ Back")).clicked() {
                if let Some(prev) = self.back.pop() {
                    self.path = prev;
                    self.selected = None;
                }
            }
            if ui.add_enabled(!self.path.is_empty(), egui::Button::new("▲ Up")).on_hover_text("Up one folder").clicked() {
                let mut up = self.path.clone();
                up.pop();
                self.go(up);
            }
            let row = Vec2::new(ui.available_width(), ui.available_height());
            ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Close").on_hover_text("Close (Esc)").clicked() {
                    self.open = false;
                }
                let sort_text = SORTS[self.sort_idx.min(SORTS.len() - 1)].1;
                egui::ComboBox::from_id_salt("ex-sort").width(130.0).selected_text(sort_text).show_ui(ui, |ui| {
                    for (i, (_, name)) in SORTS.iter().enumerate() {
                        ui.selectable_value(&mut self.sort_idx, i, *name);
                    }
                });
                let search_w = (ui.available_width() * 0.4).clamp(140.0, 260.0);
                let r = ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search all transcripts…").desired_width(search_w));
                if r.changed() {
                    self.selected = None;
                }
                // address bar fills what's left
                let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
                let p = ui.painter();
                p.rect_filled(rect, 0.0, PURPLE);
                p.rect_stroke(rect, 0.0, Stroke::new(2.0f32, YELLOW), StrokeKind::Inside);
                let mut job = LayoutJob::default();
                job.append("Address ", 0.0, fmt(pixel(14.0), DIM));
                let addr = if self.hits.is_some() {
                    format!("Search results for “{}”", self.query.trim())
                } else {
                    std::iter::once(tree.name.as_str()).chain(self.path.iter().map(String::as_str)).collect::<Vec<_>>().join(" / ")
                };
                job.append(&addr, 0.0, fmt(pixel(14.0), TEAL));
                job.wrap.max_width = rect.width() - 14.0;
                job.wrap.max_rows = 1;
                job.wrap.overflow_character = Some('…');
                let g = ui.painter().layout_job(job);
                ui.painter().galley(Pos2::new(rect.left() + 7.0, rect.center().y - g.size().y / 2.0), g, TEAL);
            });
        });
    }

    // ------------------------------------------------------------------------------- tree

    fn tree_pane(&mut self, ui: &mut Ui, tree: &Node) {
        let mut go_to = None;
        egui::ScrollArea::vertical().id_salt("ex-tree").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            let current = if self.hits.is_some() { None } else { Some(self.path.clone()) };
            tree_rows(ui, tree, &mut Vec::new(), 0, current.as_deref(), &mut go_to);
        });
        if let Some(path) = go_to {
            self.query.clear();
            self.hits = None;
            self.go(path);
        }
    }

    // ------------------------------------------------------------------------------- items

    /// Draws the large-icon view. Returns the recap indexes shown (for the status bar).
    fn list_pane(&mut self, ui: &mut Ui, tree: &Node, library: &[Meta], action: &mut Option<Action>) -> Vec<usize> {
        enum Entry<'a> {
            Folder(&'a Node),
            Item(usize),
        }
        let mut entries: Vec<Entry> = Vec::new();
        let searching = self.hits.is_some();
        let mut hit_times: HashMap<usize, f64> = HashMap::new();
        if let Some((_, _, hits)) = &self.hits {
            let mut idx: Vec<usize> = hits.iter().map(|h| h.idx).collect();
            self.sort_items(&mut idx, library);
            for h in hits {
                if let Some((t, _)) = &h.first {
                    hit_times.insert(h.idx, *t);
                }
            }
            entries.extend(idx.into_iter().map(Entry::Item));
        } else if let Some(node) = resolve(tree, &self.path) {
            match &node.kind {
                Kind::Folders(f) => entries.extend(f.iter().map(Entry::Folder)),
                Kind::Items(i) => {
                    let mut idx = i.clone();
                    self.sort_items(&mut idx, library);
                    entries.extend(idx.into_iter().map(Entry::Item));
                }
            }
        }
        let shown: Vec<usize> = entries.iter().filter_map(|e| if let Entry::Item(i) = e { Some(*i) } else { None }).collect();

        let mut open_folder = None;
        egui::ScrollArea::vertical().id_salt("ex-list").auto_shrink([false, false]).show(ui, |ui| {
            if entries.is_empty() {
                ui.add_space(8.0);
                let msg = if searching {
                    "No transcript mentions that."
                } else if library.is_empty() {
                    "No transcripts yet. Make a recap and it shows up here."
                } else {
                    "This folder is empty."
                };
                ui.label(RichText::new(msg).color(DIM));
                return;
            }
            const TILE: Vec2 = Vec2::new(124.0, 128.0);
            const GAP: f32 = 6.0;
            let w = ui.available_width();
            let cols = (((w - 10.0) + GAP) / (TILE.x + GAP)).floor().max(1.0) as usize;
            let tile_w = ((w - 10.0 - GAP * (cols as f32 - 1.0)) / cols as f32).max(80.0);
            let rows = entries.len().div_ceil(cols);
            let (area, _) = ui.allocate_exact_size(Vec2::new(w, rows as f32 * (TILE.y + GAP) + 10.0), Sense::hover());
            for (i, e) in entries.iter().enumerate() {
                let (r, c) = (i / cols, i % cols);
                let rect = Rect::from_min_size(area.min + Vec2::new(5.0 + c as f32 * (tile_w + GAP), 5.0 + r as f32 * (TILE.y + GAP)), Vec2::new(tile_w, TILE.y));
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let (key, sel) = match e {
                    Entry::Folder(n) => (format!("f:{}", n.name), Sel::Folder(n.name.clone())),
                    Entry::Item(ix) => (format!("i:{}", library[*ix].id), Sel::Item(library[*ix].id.clone())),
                };
                let resp = ui.interact(rect, ui.id().with(("ex-tile", &key)), Sense::click());
                let thumb = match e {
                    Entry::Item(ix) => self.thumb(ui.ctx(), &library[*ix]),
                    Entry::Folder(_) => None,
                };
                let on = self.selected.as_ref() == Some(&sel);
                let p = ui.painter();
                if on {
                    p.rect_filled(rect, 0.0, TEAL);
                    p.rect_stroke(rect, 0.0, Stroke::new(1.0f32, MAGENTA), StrokeKind::Inside);
                } else if resp.hovered() {
                    p.rect_filled(rect, 0.0, INDIGO);
                }
                let icon = Rect::from_center_size(Pos2::new(rect.center().x, rect.top() + 30.0), Vec2::splat(48.0));
                let (name, sub) = match e {
                    Entry::Folder(n) => {
                        folder_icon(p, icon, false);
                        (n.name.clone(), n.describe())
                    }
                    Entry::Item(ix) => {
                        let m = &library[*ix];
                        match &thumb {
                            Some(tex) => thumb_image(p, Rect::from_center_size(icon.center(), Vec2::new((tile_w - 16.0).min(96.0), 54.0)), tex),
                            None => file_icon(p, icon, &m.source_type),
                        }
                        let sub = match self.hits.as_ref().and_then(|(_, _, h)| h.iter().find(|h| h.idx == *ix)) {
                            Some(h) if h.count > 0 => plural(h.count, "match", "matches"),
                            Some(_) => "title match".into(),
                            None => fmt_time(m.duration),
                        };
                        (m.title.clone(), sub)
                    }
                };
                let mut job = LayoutJob::simple(visual(&name).into_owned(), pixel(14.0), if on { PURPLE } else { YELLOW }, tile_w - 8.0);
                job.wrap.max_rows = 3;
                job.wrap.overflow_character = Some('…');
                job.halign = Align::Center;
                let g = ui.painter().layout_job(job);
                let name_h = g.size().y;
                ui.painter().galley(Pos2::new(rect.center().x, rect.top() + 60.0), g, YELLOW);
                ui.painter().text(
                    Pos2::new(rect.center().x, rect.top() + 62.0 + name_h),
                    Align2::CENTER_TOP,
                    sub,
                    pixel(12.0),
                    if on { PURPLE } else { MAGENTA },
                );

                let resp = match e {
                    Entry::Item(ix) => {
                        let m = &library[*ix];
                        let resp = resp.on_hover_text(visual(&m.title));
                        resp.context_menu(|ui| {
                            if ui.button("Open").clicked() {
                                *action = Some(Action::Open(m.id.clone(), hit_times.get(ix).copied()));
                                ui.close_menu();
                            }
                            if ui.button("Rename…").clicked() {
                                self.selected = Some(Sel::Item(m.id.clone()));
                                self.renaming = Some((m.id.clone(), m.title.clone()));
                                self.focus_rename = true;
                                ui.close_menu();
                            }
                            if ui.button("Show in folder").clicked() {
                                *action = Some(Action::ShowInFolder(m.id.clone()));
                                ui.close_menu();
                            }
                            if ui.button("Delete…").clicked() {
                                self.selected = Some(Sel::Item(m.id.clone()));
                                self.arm_delete = Some((m.id.clone(), ui.input(|i| i.time) + 4.0));
                                ui.close_menu();
                            }
                        });
                        resp
                    }
                    Entry::Folder(_) => resp,
                };
                if resp.clicked() {
                    self.selected = Some(sel.clone());
                }
                if resp.double_clicked() {
                    match e {
                        Entry::Folder(n) => open_folder = Some(n.name.clone()),
                        Entry::Item(ix) => *action = Some(Action::Open(library[*ix].id.clone(), hit_times.get(ix).copied())),
                    }
                }
            }
        });
        if let Some(name) = open_folder {
            let mut path = self.path.clone();
            path.push(name);
            self.go(path);
        }
        shown
    }

    fn sort_items(&self, idx: &mut [usize], library: &[Meta]) {
        let title = |i: &usize| library[*i].title.to_lowercase();
        match self.sort() {
            Sort::Newest => idx.sort_by(|a, b| library[*b].created.total_cmp(&library[*a].created)),
            Sort::Oldest => idx.sort_by(|a, b| library[*a].created.total_cmp(&library[*b].created)),
            Sort::AZ => idx.sort_by_key(title),
            Sort::ZA => {
                idx.sort_by_key(title);
                idx.reverse();
            }
            Sort::Longest => idx.sort_by(|a, b| library[*b].duration.total_cmp(&library[*a].duration)),
        }
    }

    // ------------------------------------------------------------------------------- details

    fn preview_pane(&mut self, ui: &mut Ui, tree: &Node, library: &[Meta], action: &mut Option<Action>) {
        egui::ScrollArea::vertical().id_salt("ex-preview").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let item = match &self.selected {
                Some(Sel::Item(id)) => library.iter().position(|m| &m.id == id),
                _ => None,
            };
            if let Some(ix) = item {
                let m = &library[ix];
                match self.thumb(ui.ctx(), m) {
                    Some(tex) => {
                        let w = ui.available_width().min(280.0);
                        let (r, _) = ui.allocate_exact_size(Vec2::new(w, w * 9.0 / 16.0), Sense::hover());
                        thumb_image(ui.painter(), r, &tex);
                    }
                    None => {
                        let (r, _) = ui.allocate_exact_size(Vec2::new(64.0, 64.0), Sense::hover());
                        file_icon(ui.painter(), r, &m.source_type);
                    }
                }
                match self.renaming.as_mut().filter(|(id, _)| *id == m.id) {
                    Some((_, name)) => {
                        let te = ui.add(egui::TextEdit::singleline(name).desired_width(ui.available_width() - 8.0));
                        if std::mem::take(&mut self.focus_rename) {
                            te.request_focus();
                        }
                        let enter = te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let mut done = None;
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() || enter {
                                done = Some(true);
                            }
                            if ui.button("Cancel").clicked() {
                                done = Some(false);
                            }
                        });
                        match done {
                            Some(true) => {
                                *action = Some(Action::Rename(m.id.clone(), name.trim().to_string()));
                                self.renaming = None;
                            }
                            Some(false) => self.renaming = None,
                            None => {}
                        }
                    }
                    None => {
                        ui.add(egui::Label::new(RichText::new(visual(&m.title)).font(bold(18.0)).color(TEAL)).wrap());
                    }
                }
                let (y, mo, d) = ymd(m.created);
                field(ui, "Created", &format!("{d:02}/{mo:02}/{y}"));
                field(ui, "Length", &fmt_time(m.duration));
                field(ui, "Source", source_name(&m.source_type));
                field(ui, "Method", &m.method);
                if let Some(l) = &m.language {
                    field(ui, "Language", l);
                }
                field(ui, "Summary", m.style.as_deref().map(style_label).unwrap_or("None yet"));

                let mut open_at = None;
                if let Some((q, _, hits)) = &self.hits {
                    if let Some(h) = hits.iter().find(|h| h.idx == ix) {
                        ui.add_space(4.0);
                        if let Some((t, text)) = &h.first {
                            ui.label(RichText::new(format!("“{q}” found {}", plural(h.count, "time", "times"))).color(MAGENTA));
                            ui.add(egui::Label::new(RichText::new(format!("{}  {}", fmt_ts(*t, false), visual(text))).color(YELLOW)).wrap());
                            open_at = Some(*t);
                        } else {
                            ui.label(RichText::new("Matches the title").color(MAGENTA));
                        }
                    }
                }
                if open_at.is_none() {
                    let tldr = self.tldr.entry(m.id.clone()).or_insert_with(|| library::summary(&m.id).ok().flatten().map(|s| s.tldr)).clone();
                    ui.add_space(4.0);
                    match tldr {
                        Some(t) if !t.trim().is_empty() => {
                            ui.label(RichText::new("TL;DR").font(bold(14.0)).color(TEAL));
                            ui.add(egui::Label::new(RichText::new(visual(&t)).color(YELLOW)).wrap());
                        }
                        _ => {
                            ui.label(RichText::new("No summary yet.").color(DIM));
                        }
                    }
                }
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    let label = match open_at {
                        Some(t) => format!("Open at {}", fmt_ts(t, false)),
                        None => "Open".into(),
                    };
                    if ui.button(label).clicked() {
                        *action = Some(Action::Open(m.id.clone(), open_at));
                    }
                    if ui.button("Show in folder").clicked() {
                        *action = Some(Action::ShowInFolder(m.id.clone()));
                    }
                    if ui.button("Rename").clicked() {
                        self.renaming = Some((m.id.clone(), m.title.clone()));
                        self.focus_rename = true;
                    }
                    let now = ui.input(|i| i.time);
                    let armed = self.arm_delete.as_ref().is_some_and(|(id, until)| *id == m.id && now < *until);
                    let label = if armed { RichText::new("Sure? Delete").color(MAGENTA) } else { RichText::new("Delete") };
                    let resp = ui.button(label);
                    if resp.clicked() {
                        if armed {
                            *action = Some(Action::Delete(m.id.clone()));
                            self.arm_delete = None;
                            self.selected = None;
                        } else {
                            self.arm_delete = Some((m.id.clone(), now + 4.0));
                        }
                    }
                    if armed {
                        resp.on_hover_text("Click again to delete this recap, its transcript and its audio");
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                    }
                });
                return;
            }
            // a folder, or nothing selected: describe what's being shown
            let folder = match &self.selected {
                Some(Sel::Folder(name)) => resolve(tree, &self.path).and_then(|n| match &n.kind {
                    Kind::Folders(f) => f.iter().find(|c| &c.name == name),
                    Kind::Items(_) => None,
                }),
                _ => None,
            };
            let (r, _) = ui.allocate_exact_size(Vec2::new(64.0, 64.0), Sense::hover());
            folder_icon(ui.painter(), r, folder.is_none());
            match folder {
                Some(n) => {
                    ui.label(RichText::new(&n.name).font(bold(18.0)).color(TEAL));
                    ui.label(RichText::new(n.describe()).color(MAGENTA));
                    ui.label(RichText::new("Double-click to open the folder.").color(DIM));
                }
                None if self.hits.is_some() => {
                    ui.label(RichText::new("Search").font(bold(18.0)).color(TEAL));
                    ui.add(egui::Label::new(RichText::new("Titles and the full text of every transcript are searched. Select a result to see where it matches; double-click to open it there.").color(DIM)).wrap());
                }
                None => {
                    let node = resolve(tree, &self.path).unwrap_or(tree);
                    ui.label(RichText::new(&node.name).font(bold(18.0)).color(TEAL));
                    ui.label(RichText::new(node.describe()).color(MAGENTA));
                    ui.add(egui::Label::new(RichText::new("Select a transcript to see its details. Double-click to open it.").color(DIM)).wrap());
                }
            }
        });
    }

    fn status(&self, ui: &mut Ui, r: Rect, library: &[Meta], shown: &[usize]) {
        let p = ui.painter();
        p.rect_filled(r, 0.0, PURPLE);
        p.rect_stroke(r, 0.0, Stroke::new(2.0f32, YELLOW), StrokeKind::Inside);
        let total: f64 = shown.iter().map(|i| library[*i].duration).sum();
        let left = if shown.is_empty() { String::new() } else { format!("{} · {}", plural(shown.len(), "transcript", "transcripts"), fmt_time(total)) };
        let left = if let Some((_, _, hits)) = &self.hits { format!("{} found", plural(hits.len(), "transcript", "transcripts")) } else { left };
        let p = p.with_clip_rect(r.shrink(2.0));
        p.text(r.left_center() + Vec2::new(8.0, 0.0), Align2::LEFT_CENTER, left, pixel(12.0), YELLOW);
        let right = format!("{} in library", plural(library.len(), "recap", "recaps"));
        p.text(r.right_center() - Vec2::new(8.0, 0.0), Align2::RIGHT_CENTER, right, pixel(12.0), TEAL);
    }

    // ------------------------------------------------------------------------------- thumbnails

    /// The YouTube thumbnail for a recap, if it has one and it's loaded. The first ask queues it:
    /// read from the recap's folder, or downloaded once and saved there.
    fn thumb(&mut self, ctx: &egui::Context, m: &Meta) -> Option<egui::TextureHandle> {
        let vid = m.video_id.as_ref().filter(|v| v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))?;
        if let Some(t) = self.thumbs.get(&m.id) {
            return t.clone();
        }
        self.thumbs.insert(m.id.clone(), None);
        let worker = self.thumb_worker.get_or_insert_with(|| {
            let (jobs, job_rx) = mpsc::channel::<(String, String)>();
            let (done_tx, done) = mpsc::channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                for (id, vid) in job_rx {
                    let img = load_thumb(&id, &vid);
                    if done_tx.send((id, img)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
            ThumbWorker { jobs, done }
        });
        let _ = worker.jobs.send((m.id.clone(), vid.clone()));
        None
    }

    // ------------------------------------------------------------------------------- search

    fn search(&mut self, library: &[Meta], q: &str) -> Vec<Hit> {
        let mut hits = Vec::new();
        for (idx, m) in library.iter().enumerate() {
            let lines = self.text.entry(m.id.clone()).or_insert_with(|| {
                library::transcript(&m.id)
                    .map(|t| t.segments.into_iter().map(|s| (s.start, s.text.to_lowercase(), s.text)).collect())
                    .unwrap_or_default()
            });
            let mut count = 0;
            let mut first = None;
            for (t, low, text) in lines.iter() {
                if low.contains(q) {
                    count += 1;
                    first.get_or_insert_with(|| (*t, text.clone()));
                }
            }
            if count > 0 || m.title.to_lowercase().contains(q) {
                hits.push(Hit { idx, count, first });
            }
        }
        hits
    }
}

// ---------------------------------------------------------------------------------------------
// folders

fn build_tree(library: &[Meta]) -> Node {
    let all: Vec<usize> = (0..library.len()).collect();
    let group = |name: &str, key: &dyn Fn(&Meta) -> (String, String)| -> Node {
        // (sort key, folder name) per recap; folders sorted by key
        let mut folders: Vec<(String, String, Vec<usize>)> = Vec::new();
        for (i, m) in library.iter().enumerate() {
            let (k, n) = key(m);
            match folders.iter_mut().find(|f| f.1 == n) {
                Some(f) => f.2.push(i),
                None => folders.push((k, n, vec![i])),
            }
        }
        folders.sort_by(|a, b| a.0.cmp(&b.0));
        Node { name: name.into(), kind: Kind::Folders(folders.into_iter().map(|(_, n, items)| Node { name: n, kind: Kind::Items(items) }).collect()) }
    };
    let by_source = group("By Source", &|m| {
        let order = match m.source_type.as_str() {
            "youtube" => "0",
            "url" => "1",
            "video" => "2",
            "audio" => "3",
            "subtitles" => "4",
            _ => "5",
        };
        (order.into(), source_name(&m.source_type).into())
    });
    let by_month = group("By Month", &|m| {
        let (y, mo, _) = ymd(m.created);
        // newest month first
        (format!("{:06}", 999_999 - (y * 12 + mo as i64 - 1)), format!("{} {y}", MONTHS[(mo as usize).clamp(1, 12) - 1]))
    });
    let by_summary = group("By Summary", &|m| match m.style.as_deref() {
        Some(s) => (format!("0{s}"), style_label(s).into()),
        None => ("1".into(), "No summary".into()),
    });
    let by_method = group("By Method", &|m| {
        if m.method.starts_with("YouTube") {
            ("0".into(), "YouTube captions".into())
        } else if m.method.contains("Vulkan") {
            ("1".into(), "Whisper (GPU)".into())
        } else if m.method.contains("whisper") {
            ("2".into(), "Whisper (CPU)".into())
        } else if m.method.starts_with("Imported") {
            ("3".into(), "Imported subtitles".into())
        } else {
            ("3".into(), "Other".into())
        }
    });
    Node {
        name: "Transcripts".into(),
        kind: Kind::Folders(vec![Node { name: "All Transcripts".into(), kind: Kind::Items(all) }, by_source, by_month, by_summary, by_method]),
    }
}

fn resolve<'a>(tree: &'a Node, path: &[String]) -> Option<&'a Node> {
    let mut node = tree;
    for seg in path {
        let Kind::Folders(children) = &node.kind else { return None };
        node = children.iter().find(|c| &c.name == seg)?;
    }
    Some(node)
}

fn tree_rows(ui: &mut Ui, node: &Node, path: &mut Vec<String>, depth: usize, current: Option<&[String]>, go_to: &mut Option<Vec<String>>) {
    let on = current == Some(path.as_slice());
    let open = current.is_some_and(|c| c.starts_with(path));
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 20.0), Sense::click());
    let p = ui.painter();
    if on {
        p.rect_filled(rect, 0.0, TEAL);
    } else if resp.hovered() {
        p.rect_filled(rect, 0.0, INDIGO);
    }
    let x = rect.left() + 4.0 + depth as f32 * 12.0;
    if depth > 0 {
        // dotted guide like the site's tree
        let gx = x - 7.0;
        let mut y = rect.top();
        while y < rect.bottom() {
            p.line_segment([Pos2::new(gx, y), Pos2::new(gx, (y + 1.0).min(rect.bottom()))], Stroke::new(1.0f32, YELLOW.gamma_multiply(0.7)));
            y += 3.0;
        }
    }
    folder_icon(p, Rect::from_min_size(Pos2::new(x, rect.center().y - 8.0), Vec2::splat(16.0)), open);
    let text = match &node.kind {
        Kind::Items(i) => format!("{} ({})", node.name, i.len()),
        Kind::Folders(_) => node.name.clone(),
    };
    p.with_clip_rect(rect).text(Pos2::new(x + 20.0, rect.center().y), Align2::LEFT_CENTER, text, pixel(14.0), if on { PURPLE } else { YELLOW });
    if resp.clicked() {
        *go_to = Some(path.clone());
    }
    if let Kind::Folders(children) = &node.kind {
        for c in children {
            path.push(c.name.clone());
            tree_rows(ui, c, path, depth + 1, current, go_to);
            path.pop();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// drawing

fn folder_icon(p: &egui::Painter, r: Rect, open: bool) {
    let s = r.width() / 16.0;
    let at = |x: f32, y: f32| Pos2::new(r.left() + x * s, r.top() + y * s);
    let outline = Stroke::new((s * 0.8).max(1.0), INK);
    // tab, then body
    p.add(Shape::convex_polygon(vec![at(1.0, 3.0), at(6.0, 3.0), at(7.5, 4.5), at(7.5, 6.0), at(1.0, 6.0)], YELLOW, outline));
    p.rect(Rect::from_min_max(at(1.0, 5.0), at(15.0, 14.0)), 0.0, Color32::from_rgb(0xE0, 0xA8, 0x2E), outline, StrokeKind::Middle);
    if open {
        p.add(Shape::convex_polygon(vec![at(3.0, 8.0), at(16.0, 8.0), at(14.5, 14.0), at(1.0, 14.0)], YELLOW, outline));
    } else {
        p.rect(Rect::from_min_max(at(1.0, 6.5), at(15.0, 14.0)), 0.0, YELLOW, outline, StrokeKind::Middle);
    }
}

/// A page with a folded corner and text lines, plus a badge for where the recap came from.
fn file_icon(p: &egui::Painter, r: Rect, source_type: &str) {
    let s = r.width() / 16.0;
    let at = |x: f32, y: f32| Pos2::new(r.left() + x * s, r.top() + y * s);
    let outline = Stroke::new((s * 0.6).max(1.0), INK);
    p.add(Shape::convex_polygon(vec![at(3.0, 1.0), at(10.0, 1.0), at(13.0, 4.0), at(13.0, 15.0), at(3.0, 15.0)], WHITE, outline));
    p.add(Shape::convex_polygon(vec![at(10.0, 1.0), at(13.0, 4.0), at(10.0, 4.0)], DIM, outline));
    for (i, w) in [6.0f32, 7.0, 5.0, 7.0].iter().enumerate() {
        let y = 6.0 + i as f32 * 2.0;
        p.line_segment([at(4.5, y), at(4.5 + w, y)], Stroke::new((s * 0.7).max(1.0), PURPLE));
    }
    let badge = Rect::from_min_max(at(8.0, 10.0), at(16.0, 16.0));
    let c = badge.center();
    match source_type {
        "youtube" => {
            p.rect(badge, 1.5 * s, MAGENTA, outline, StrokeKind::Middle);
            let h = badge.height() * 0.28;
            p.add(Shape::convex_polygon(vec![c + Vec2::new(-h * 0.8, -h), c + Vec2::new(h * 1.1, 0.0), c + Vec2::new(-h * 0.8, h)], WHITE, Stroke::NONE));
        }
        "video" => {
            p.rect(badge, 0.5 * s, TEAL, outline, StrokeKind::Middle);
            for i in 0..3 {
                let x = badge.left() + (1.5 + i as f32 * 2.5) * s;
                p.rect_filled(Rect::from_min_size(Pos2::new(x, badge.top() + 0.8 * s), Vec2::splat(s)), 0.0, INK);
                p.rect_filled(Rect::from_min_size(Pos2::new(x, badge.bottom() - 1.8 * s), Vec2::splat(s)), 0.0, INK);
            }
        }
        "subtitles" => {
            p.rect(badge, 1.0 * s, YELLOW, outline, StrokeKind::Middle);
            p.text(c, Align2::CENTER_CENTER, "CC", bold(4.2 * s), INK);
        }
        "audio" => {
            p.circle(at(12.0, 13.0), 3.4 * s, YELLOW, outline);
            p.circle_filled(at(11.3, 14.0), 1.1 * s, INK);
            p.line_segment([at(12.3, 14.0), at(12.3, 10.6)], Stroke::new((s * 0.7).max(1.0), INK));
        }
        _ => {
            p.circle(at(12.0, 13.0), 3.4 * s, TEAL, outline);
            p.line_segment([at(8.8, 13.0), at(15.2, 13.0)], Stroke::new((s * 0.5).max(1.0), INK));
            p.line_segment([at(12.0, 9.8), at(12.0, 16.2)], Stroke::new((s * 0.5).max(1.0), INK));
        }
    }
}

/// Reads `thumb.jpg` from the recap's folder, downloading it from YouTube the first time.
fn load_thumb(id: &str, vid: &str) -> Option<egui::ColorImage> {
    let path = library::dir(id).ok()?.join("thumb.jpg");
    if !path.is_file() {
        let resp = ureq::get(&format!("https://i.ytimg.com/vi/{vid}/mqdefault.jpg")).timeout(std::time::Duration::from_secs(15)).call().ok()?;
        let mut bytes = Vec::new();
        resp.into_reader().take(4 << 20).read_to_end(&mut bytes).ok()?;
        image::load_from_memory(&bytes).ok()?; // only keep it if it's a real image
        std::fs::write(&path, &bytes).ok()?;
    }
    let img = image::load_from_memory(&std::fs::read(&path).ok()?).ok()?.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()))
}

/// A thumbnail with the site's thick border, cropped to fill `r`.
fn thumb_image(p: &egui::Painter, r: Rect, tex: &egui::TextureHandle) {
    let [w, h] = tex.size().map(|v| v as f32);
    let (want, have) = (r.width() / r.height(), w / h);
    // crop the texture so it fills r without stretching
    let uv = if have > want {
        let k = want / have;
        Rect::from_min_max(Pos2::new(0.5 - k / 2.0, 0.0), Pos2::new(0.5 + k / 2.0, 1.0))
    } else {
        let k = have / want;
        Rect::from_min_max(Pos2::new(0.0, 0.5 - k / 2.0), Pos2::new(1.0, 0.5 + k / 2.0))
    };
    p.image(tex.id(), r, uv, Color32::WHITE);
    p.rect_stroke(r, 0.0, Stroke::new(2.0f32, YELLOW), StrokeKind::Outside);
}

fn field(ui: &mut Ui, label: &str, value: &str) {
    ui.horizontal_top(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::new(70.0, 16.0), Sense::hover());
        ui.painter().text(r.left_top(), Align2::LEFT_TOP, label, pixel(14.0), DIM);
        ui.add(egui::Label::new(RichText::new(visual(value)).font(pixel(14.0)).color(MAGENTA)).wrap());
    });
}

/// A child Ui confined to `rect`; the parent's layout never sees what the child draws.
fn child(ui: &mut Ui, rect: Rect) -> Ui {
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(Align::Min)));
    c.set_clip_rect(rect.intersect(ui.clip_rect()));
    c
}

fn fmt(font: egui::FontId, color: Color32) -> egui::TextFormat {
    egui::TextFormat { font_id: font, color, ..Default::default() }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn source_name(source_type: &str) -> &'static str {
    match source_type {
        "youtube" => "YouTube",
        "url" => "Web links",
        "video" => "Video files",
        "audio" => "Audio files",
        "subtitles" => "Subtitle files",
        _ => "Other",
    }
}

fn style_label(s: &str) -> &'static str {
    match s {
        "meeting" => "Meeting",
        "lecture" => "Lecture",
        "podcast" => "Podcast",
        _ => "General",
    }
}

/// (year, month, day) in the computer's local time zone for a Unix time stamp, so a recap made
/// late in the evening lands on the day it was made.
fn ymd(secs: f64) -> (i64, u32, u32) {
    use chrono::{Datelike, Local, TimeZone};
    match Local.timestamp_opt(secs as i64, 0).earliest() {
        Some(d) => (i64::from(d.year()), d.month(), d.day()),
        None => (1970, 1, 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_dates() {
        use chrono::{Datelike, Local};
        let now = Local::now();
        assert_eq!(ymd(now.timestamp() as f64), (i64::from(now.year()), now.month(), now.day()));
    }

    #[test]
    fn folders_follow_metadata() {
        let m = |id: &str, st: &str, style: Option<&str>| Meta { id: id.into(), title: id.into(), source_type: st.into(), style: style.map(Into::into), ..Default::default() };
        let lib = vec![m("a", "youtube", None), m("b", "video", Some("meeting")), m("c", "youtube", Some("meeting"))];
        let tree = build_tree(&lib);
        let yt = resolve(&tree, &["By Source".into(), "YouTube".into()]).unwrap();
        assert!(matches!(&yt.kind, Kind::Items(i) if i == &vec![0, 2]));
        let meet = resolve(&tree, &["By Summary".into(), "Meeting".into()]).unwrap();
        assert!(matches!(&meet.kind, Kind::Items(i) if i.len() == 2));
        assert!(resolve(&tree, &["By Source".into(), "Audio files".into()]).is_none());
    }
}
