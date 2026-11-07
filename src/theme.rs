//! The look of https://the1azreen.neocities.org/ : night-sky background, neon-yellow bordered
//! purple windows, teal highlights, magenta taskbar, PixelMplus12 everywhere.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use egui::epaint::{Mesh, Shadow, Vertex, WHITE_UV};
use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Pos2, Rect, Shape, Stroke,
    StrokeKind, TextStyle, TextureId, Ui, Vec2,
};

pub const SKY_TOP: Color32 = Color32::from_rgb(0x0d, 0x08, 0x20);
pub const SKY_MID: Color32 = Color32::from_rgb(0x1d, 0x12, 0x40);
pub const PURPLE: Color32 = Color32::from_rgb(0x2B, 0x1B, 0x4F); // --lilith-inactive-color
pub const INDIGO: Color32 = Color32::from_rgb(0x3C, 0x2E, 0x7B); // --lilith-outer-color
pub const TEAL: Color32 = Color32::from_rgb(0x6D, 0xE0, 0xF6); // --lilith-inner-color
pub const YELLOW: Color32 = Color32::from_rgb(0xF7, 0xC9, 0x48); // --lilith-border-color
pub const INK: Color32 = Color32::from_rgb(0x1B, 0x1B, 0x2F); // --lilith-window-text-back
pub const MAGENTA: Color32 = Color32::from_rgb(0xFF, 0x6F, 0xF2); // --lilith-task-color
pub const WHITE: Color32 = Color32::from_rgb(0xFD, 0xFD, 0xFD);
pub const DIM: Color32 = Color32::from_rgb(0x9A, 0x8C, 0xC8);

pub const BOLD: &str = "pixel-bold";

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD.into()))
}

pub fn pixel(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "pixel".into(),
        Arc::new(FontData::from_static(include_bytes!("../assets/fonts/PixelMplus12-Regular.ttf"))),
    );
    fonts.font_data.insert(
        "pixel-bold".into(),
        Arc::new(FontData::from_static(include_bytes!("../assets/fonts/PixelMplus12-Bold.ttf"))),
    );
    // Chain: pixel font (Latin + kana + 6k kanji) → egui's bundled fonts → system fonts for
    // every other script. Bold only has Latin, so it falls back to the regular pixel font.
    let mut chain = vec!["pixel".to_string()];
    chain.extend(fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
    for (name, data) in system_fallback_fonts() {
        fonts.font_data.insert(name.clone(), Arc::new(data));
        chain.push(name);
    }
    let mut bold_chain = vec!["pixel-bold".to_string()];
    bold_chain.extend(chain.iter().cloned());
    fonts.families.insert(FontFamily::Proportional, chain);
    fonts.families.insert(FontFamily::Name(BOLD.into()), bold_chain);
    ctx.set_fonts(fonts);

    let mut style = (*ctx.style()).clone();
    style.text_styles = [
        (TextStyle::Small, pixel(12.0)),
        (TextStyle::Body, pixel(16.0)),
        (TextStyle::Button, pixel(16.0)),
        (TextStyle::Monospace, FontId::monospace(13.0)),
        (TextStyle::Heading, bold(24.0)),
    ]
    .into();
    style.spacing.item_spacing = Vec2::new(6.0, 4.0);
    style.spacing.button_padding = Vec2::new(6.0, 2.0);
    style.spacing.slider_rail_height = 6.0;

    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.override_text_color = Some(YELLOW);
    v.panel_fill = Color32::TRANSPARENT;
    v.window_fill = PURPLE;
    v.window_stroke = Stroke::new(2.0f32, YELLOW);
    v.window_corner_radius = CornerRadius::same(4);
    v.window_shadow = Shadow { offset: [4, 4], blur: 0, spread: 0, color: Color32::BLACK };
    v.popup_shadow = v.window_shadow;
    v.extreme_bg_color = INK;
    v.faint_bg_color = Color32::from_rgb(0x24, 0x20, 0x40);
    v.code_bg_color = INK;
    v.hyperlink_color = MAGENTA;
    v.selection.bg_fill = INDIGO;
    v.selection.stroke = Stroke::new(1.0f32, TEAL);
    v.slider_trailing_fill = true;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = PURPLE;
    w.noninteractive.bg_stroke = Stroke::new(1.0f32, YELLOW);
    w.noninteractive.fg_stroke = Stroke::new(1.0f32, YELLOW);
    for (state, bg, stroke, fg) in [
        (&mut w.inactive, INK, YELLOW, YELLOW),
        (&mut w.hovered, INDIGO, TEAL, TEAL),
        (&mut w.active, TEAL, YELLOW, INK),
        (&mut w.open, INDIGO, YELLOW, YELLOW),
    ] {
        state.bg_fill = bg;
        state.weak_bg_fill = bg;
        state.bg_stroke = Stroke::new(1.0f32, stroke);
        state.fg_stroke = Stroke::new(1.5f32, fg);
        state.corner_radius = CornerRadius::same(2);
        state.expansion = 0.0;
    }
    // Always dark, whatever the OS theme is.
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_style_of(egui::Theme::Dark, style);
}

/// Fonts the OS already has, for scripts the pixel font lacks (Chinese, Korean, Thai, Arabic,
/// Hebrew, Indic, rare kanji...). They're memory-mapped rather than read, so only the glyphs
/// actually drawn are paged in and the app stays small. Japanese comes first so shared Han
/// characters keep their Japanese shapes.
fn system_fallback_fonts() -> Vec<(String, FontData)> {
    #[cfg(windows)]
    let candidates: &[&str] = &[
        "YuGothM.ttc", "meiryo.ttc", "msgothic.ttc", // Japanese
        "malgun.ttf", "gulim.ttc",                   // Korean
        "msyh.ttc", "simsun.ttc", "msjh.ttc",        // Chinese (simplified, traditional)
        "segoeui.ttf",                               // Cyrillic, Greek, Arabic, Hebrew, Vietnamese
        "LeelawUI.ttf", "Nirmala.ttf", "Nirmala.ttc", // Thai/Lao/Khmer, Indic
        "seguisym.ttf",                              // symbols
    ];
    #[cfg(target_os = "macos")]
    let candidates: &[&str] = &[
        "ヒラギノ角ゴシック W3.ttc", "Hiragino Sans GB.ttc", "AppleSDGothicNeo.ttc",
        "PingFang.ttc", "Supplemental/Arial Unicode.ttf", "Thonburi.ttc", "Kohinoor.ttc",
    ];
    #[cfg(all(unix, not(target_os = "macos")))]
    let candidates: &[&str] = &[
        "opentype/noto/NotoSansCJK-Regular.ttc", "noto-cjk/NotoSansCJK-Regular.ttc",
        "google-noto-cjk/NotoSansCJK-Regular.ttc", "opentype/noto-cjk/NotoSansCJK-Regular.ttc",
        "truetype/droid/DroidSansFallbackFull.ttf", "truetype/wqy/wqy-microhei.ttc",
        "truetype/noto/NotoSans-Regular.ttf", "truetype/dejavu/DejaVuSans.ttf",
        "truetype/noto/NotoSansThai-Regular.ttf", "truetype/noto/NotoSansArabic-Regular.ttf",
    ];

    let dirs: Vec<std::path::PathBuf> = if cfg!(windows) {
        let root = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
        let mut d = vec![std::path::PathBuf::from(root).join("Fonts")];
        // per-user installed fonts
        if let Some(local) = dirs::data_local_dir() {
            d.push(local.join("Microsoft\\Windows\\Fonts"));
        }
        d
    } else if cfg!(target_os = "macos") {
        vec!["/System/Library/Fonts".into(), "/Library/Fonts".into()]
    } else {
        vec!["/usr/share/fonts".into(), "/usr/local/share/fonts".into()]
    };

    candidates
        .iter()
        .filter_map(|name| dirs.iter().map(|d| d.join(name)).find(|p| p.is_file()))
        .filter_map(|path| {
            let file = std::fs::File::open(&path).ok()?;
            // SAFETY: system font files aren't modified while we run; the mapping is leaked so
            // it lives as long as egui needs the bytes.
            let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
            let bytes: &'static [u8] = &Box::leak(Box::new(map))[..];
            let name = format!("sys:{}", path.file_name()?.to_string_lossy());
            Some((name, FontData::from_static(bytes)))
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Night sky (a port of the site's CSS background: five drifting star tiles over an indigo fade)

struct StarLayer {
    at: (f32, f32),
    radius: f32,
    color: Color32,
    tile: (f32, f32),
    /// Pixels travelled per 40 s loop, straight from the site's @keyframes.
    travel: f32,
}

const STARS: [StarLayer; 5] = [
    StarLayer { at: (23.0, 41.0), radius: 1.0, color: WHITE, tile: (211.0, 197.0), travel: 197.0 },
    StarLayer { at: (131.0, 87.0), radius: 1.0, color: YELLOW, tile: (263.0, 229.0), travel: 229.0 },
    StarLayer { at: (77.0, 163.0), radius: 1.0, color: TEAL, tile: (241.0, 251.0), travel: 502.0 },
    StarLayer { at: (181.0, 19.0), radius: 1.2, color: WHITE, tile: (307.0, 283.0), travel: 283.0 },
    StarLayer { at: (41.0, 121.0), radius: 0.8, color: MAGENTA, tile: (173.0, 191.0), travel: 382.0 },
];

pub fn paint_night_sky(painter: &egui::Painter, rect: Rect, time: f64) {
    let mut mesh = Mesh::default();
    let ys = [rect.top(), rect.top() + rect.height() * 0.7, rect.bottom()];
    let cs = [SKY_TOP, SKY_MID, PURPLE];
    for (y, c) in ys.iter().zip(cs) {
        mesh.colored_vertex(Pos2::new(rect.left(), *y), c);
        mesh.colored_vertex(Pos2::new(rect.right(), *y), c);
    }
    for i in 0..2u32 {
        let b = i * 2;
        mesh.add_triangle(b, b + 1, b + 2);
        mesh.add_triangle(b + 1, b + 3, b + 2);
    }
    painter.add(Shape::mesh(mesh));

    for layer in &STARS {
        let shift = ((time / 40.0 * layer.travel as f64) % layer.tile.1 as f64) as f32;
        let cols = (rect.width() / layer.tile.0).ceil() as i32 + 1;
        let rows = (rect.height() / layer.tile.1).ceil() as i32 + 2;
        for cx in 0..cols {
            for cy in -1..rows {
                let p = Pos2::new(
                    rect.left() + cx as f32 * layer.tile.0 + layer.at.0,
                    rect.top() + cy as f32 * layer.tile.1 + layer.at.1 + shift,
                );
                if rect.contains(p) {
                    painter.circle_filled(p, layer.radius + 0.3, layer.color);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Site-style window

/// Draws the little "_ □ x" buttons the site's title bars have.
fn title_buttons(painter: &egui::Painter, right: f32, cy: f32) {
    let fg = YELLOW;
    for i in 0..3 {
        let r = Rect::from_center_size(Pos2::new(right - 10.0 - i as f32 * 16.0, cy), Vec2::splat(12.0));
        painter.rect_filled(r, 1.0, INK);
        painter.rect_stroke(r, 1.0, Stroke::new(1.0f32, fg), StrokeKind::Inside);
        let c = r.center();
        match i {
            0 => {
                painter.line_segment([c + Vec2::new(-3.0, -3.0), c + Vec2::new(3.0, 3.0)], Stroke::new(1.5f32, fg));
                painter.line_segment([c + Vec2::new(3.0, -3.0), c + Vec2::new(-3.0, 3.0)], Stroke::new(1.5f32, fg));
            }
            1 => {
                painter.rect_stroke(Rect::from_center_size(c, Vec2::splat(6.0)), 0.0, Stroke::new(1.0f32, fg), StrokeKind::Inside);
            }
            _ => {
                painter.line_segment([c + Vec2::new(-3.0, 3.0), c + Vec2::new(3.0, 3.0)], Stroke::new(1.5f32, fg));
            }
        }
    }
}

/// A window in the site's style: yellow border, purple chrome, title bar, dark content well.
/// Lights up (indigo body, teal title) on hover like the site does.
pub fn window<R>(ui: &mut Ui, title: &str, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let id = ui.id().with(("site-window", title));
    let hovered = ui
        .ctx()
        .data(|d| d.get_temp::<Rect>(id))
        .is_some_and(|r| ui.rect_contains_pointer(r));
    let (body, title_bg, title_fg) =
        if hovered { (INDIGO, TEAL, INK) } else { (PURPLE, PURPLE, YELLOW) };

    let resp = egui::Frame::new()
        .fill(body)
        .stroke(Stroke::new(2.0f32, YELLOW))
        .corner_radius(4)
        .inner_margin(4)
        .shadow(Shadow { offset: [4, 4], blur: 0, spread: 0, color: Color32::BLACK })
        .show(ui, |ui| {
            let (bar, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), egui::Sense::hover());
            let p = ui.painter();
            p.rect_filled(bar, 0.0, title_bg);
            p.rect_stroke(bar, 0.0, Stroke::new(2.0f32, YELLOW), StrokeKind::Inside);
            // window icon: a tiny transcript page (lines) in a box
            let icon = Rect::from_min_size(bar.min + Vec2::new(5.0, 5.0), Vec2::splat(12.0));
            p.rect_filled(icon, 1.0, YELLOW);
            for (i, w) in [7.0f32, 5.0, 7.0].iter().enumerate() {
                let y = icon.top() + 3.0 + i as f32 * 3.0;
                p.line_segment([Pos2::new(icon.left() + 2.5, y), Pos2::new(icon.left() + 2.5 + w, y)], Stroke::new(1.3f32, INK));
            }
            p.with_clip_rect(bar.shrink(2.0)).text(
                Pos2::new(icon.right() + 6.0, bar.center().y),
                egui::Align2::LEFT_CENTER,
                title,
                bold(16.0),
                title_fg,
            );
            title_buttons(p, bar.right() - 2.0, bar.center().y);
            ui.add_space(2.0);
            egui::Frame::new()
                .fill(INK)
                .stroke(Stroke::new(2.0f32, YELLOW))
                .inner_margin(6)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    add_contents(ui)
                })
                .inner
        });
    glow_rect(ui.painter(), resp.response.rect, 4.0, if hovered { TEAL } else { YELLOW }, 7.0);
    ui.ctx().data_mut(|d| d.insert_temp(id, resp.response.rect));
    resp.inner
}

// ---------------------------------------------------------------------------------------------
// Drawing helpers shared by the skins

/// A ring (or full disc when `inner == 0`) textured with `tex`, rotated by `angle`.
/// `uv_scale` < 1 zooms into the middle of the texture.
pub fn textured_disc(
    painter: &egui::Painter,
    center: Pos2,
    outer: f32,
    inner: f32,
    angle: f32,
    tex: TextureId,
    tint: Color32,
) {
    const SEG: u32 = 72;
    let mut mesh = Mesh::with_texture(tex);
    let uv_at = |rho: f32, a: f32| {
        let k = rho / outer;
        Pos2::new(0.5 + 0.5 * k * (a - angle).cos(), 0.5 + 0.5 * k * (a - angle).sin())
    };
    for i in 0..=SEG {
        let a = i as f32 / SEG as f32 * std::f32::consts::TAU;
        let dir = Vec2::angled(a);
        mesh.vertices.push(Vertex { pos: center + dir * outer, uv: uv_at(outer, a), color: tint });
        mesh.vertices.push(Vertex { pos: center + dir * inner, uv: uv_at(inner, a), color: tint });
    }
    for i in 0..SEG {
        let b = i * 2;
        mesh.add_triangle(b, b + 1, b + 2);
        mesh.add_triangle(b + 1, b + 3, b + 2);
    }
    painter.add(Shape::mesh(mesh));
}

/// A flat-coloured ring between two radii.
pub fn ring(painter: &egui::Painter, center: Pos2, outer: f32, inner: f32, color: Color32) {
    const SEG: u32 = 72;
    let mut mesh = Mesh::default();
    for i in 0..=SEG {
        let dir = Vec2::angled(i as f32 / SEG as f32 * std::f32::consts::TAU);
        mesh.vertices.push(Vertex { pos: center + dir * outer, uv: WHITE_UV, color });
        mesh.vertices.push(Vertex { pos: center + dir * inner, uv: WHITE_UV, color });
    }
    for i in 0..SEG {
        let b = i * 2;
        mesh.add_triangle(b, b + 1, b + 2);
        mesh.add_triangle(b + 1, b + 3, b + 2);
    }
    painter.add(Shape::mesh(mesh));
}

/// A square image drawn with its texture, letterboxed into `rect`.
pub fn image_in(painter: &egui::Painter, rect: Rect, tex: TextureId, tint: Color32) {
    painter.image(tex, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), tint);
}

// ---------------------------------------------------------------------------------------------
// Neon glow ("HDR" in settings). Halos are drawn with zero alpha, which in egui's premultiplied
// pipeline means additive blending: they brighten whatever is underneath, like light bleeding.

static GLOW: AtomicBool = AtomicBool::new(true);

pub fn set_glow(on: bool) {
    GLOW.store(on, Ordering::Relaxed);
}

pub fn glow_on() -> bool {
    GLOW.load(Ordering::Relaxed)
}

fn additive(c: Color32, k: f32) -> Color32 {
    let c = c.gamma_multiply(k.clamp(0.0, 1.0));
    Color32::from_rgba_premultiplied(c.r(), c.g(), c.b(), 0)
}

/// A soft halo outside `rect`'s edge.
pub fn glow_rect(p: &egui::Painter, rect: Rect, radius: f32, color: Color32, spread: f32) {
    if !glow_on() {
        return;
    }
    const LAYERS: i32 = 4;
    for i in 1..=LAYERS {
        let t = i as f32 / LAYERS as f32;
        let w = spread / LAYERS as f32 + 0.5;
        p.rect_stroke(rect.expand(spread * t - w * 0.5), radius + spread * t, Stroke::new(w, additive(color, 0.45 * (1.0 - t) + 0.05)), StrokeKind::Middle);
    }
}

/// A soft halo around a circle's rim.
pub fn glow_circle(p: &egui::Painter, center: Pos2, radius: f32, color: Color32, spread: f32) {
    if !glow_on() {
        return;
    }
    const LAYERS: i32 = 4;
    for i in 1..=LAYERS {
        let t = i as f32 / LAYERS as f32;
        let w = spread / LAYERS as f32 + 0.5;
        p.circle_stroke(center, radius + spread * t - w * 0.5, Stroke::new(w, additive(color, 0.45 * (1.0 - t) + 0.05)));
    }
}

/// Bloom for small lit things (LCD segments, spectrum bars): stacked, growing additive fills.
pub fn glow_fill(p: &egui::Painter, rect: Rect, color: Color32, spread: f32) {
    if !glow_on() {
        return;
    }
    for i in 1..=3 {
        let t = i as f32 / 3.0;
        p.rect_filled(rect.expand(spread * t), spread * t, additive(color, 0.28 * (1.0 - t) + 0.04));
    }
}

/// Glowing text: the same string smeared around itself additively.
pub fn glow_text(p: &egui::Painter, pos: Pos2, align: egui::Align2, text: &str, font: FontId, color: Color32) {
    if !glow_on() {
        return;
    }
    for r in [2.0f32, 4.0] {
        for k in 0..8 {
            let d = Vec2::angled(k as f32 * std::f32::consts::TAU / 8.0) * r;
            p.text(pos + d, align, text, font.clone(), additive(color, if r < 3.0 { 0.22 } else { 0.10 }));
        }
    }
}

/// White at `a`/255 opacity in sRGB space. (`Color32::from_white_alpha` treats `a` as linear,
/// which comes out far brighter than intended for faint highlights.)
pub fn white_a(a: u8) -> Color32 {
    Color32::from_rgba_premultiplied(a, a, a, a)
}

/// egui lays text out left-to-right without shaping. For Arabic / Persian / Hebrew, join the
/// letters into their contextual forms and reorder runs into visual order before drawing.
/// Everything else passes through untouched (and unallocated).
pub fn visual(s: &str) -> std::borrow::Cow<'_, str> {
    let is_rtl = |c: char| matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF);
    if !s.chars().any(is_rtl) {
        return std::borrow::Cow::Borrowed(s);
    }
    let shaped = ar_reshaper::reshape_line(s);
    let bidi = unicode_bidi::BidiInfo::new(&shaped, None);
    let mut out = String::with_capacity(shaped.len());
    for para in &bidi.paragraphs {
        out.push_str(&bidi.reorder_line(para, para.range.clone()));
    }
    std::borrow::Cow::Owned(out)
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()), m(a.a(), b.a()))
}

pub fn fmt_time(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "--:--".into();
    }
    let s = secs as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}
