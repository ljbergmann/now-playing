//! Renders one complete, static frame for the current state.
//!
//! This is the expensive part (image scaling, text), so it runs on a
//! background thread and only when something changes. The 60 fps loop just
//! copies/blends finished frames and animates the tiny equalizer region.

use std::time::SystemTime;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use image::{RgbaImage, imageops};
use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Paint, Path, PathBuilder, Pixmap, Point, Rect,
    SpreadMode, Transform,
};

use crate::fonts::Fonts;
use crate::lastfm::Track;

/// A finished frame in 0RGB (what both the DRM buffer and the Mac window use).
pub struct Scene {
    pub pixels: Vec<u32>,
    /// Where the animated "now playing" bars go, if they should animate.
    pub eq: Option<EqSpec>,
}

#[derive(Clone, Copy)]
pub struct EqSpec {
    pub x: usize,
    pub y: usize,
    pub bar_w: usize,
    pub gap: usize,
    pub h: usize,
    pub color: u32,
}

impl EqSpec {
    pub fn width(&self) -> usize {
        self.bar_w * 3 + self.gap * 2
    }
}

pub enum View<'a> {
    Connecting(Option<&'a str>),
    Track(Option<&'a Track>),
}

fn bg() -> Color {
    Color::from_rgba8(11, 11, 13, 255)
}

fn clamp(lo: f32, v: f32, hi: f32) -> f32 {
    v.max(lo).min(hi)
}

pub fn render(fonts: &Fonts, w: usize, h: usize, view: &View, now: SystemTime) -> Scene {
    let mut pm = Pixmap::new(w as u32, h as u32).expect("non-zero size");
    pm.fill(bg());
    let mut eq = None;

    match view {
        View::Connecting(err) => {
            centered_message(&mut pm, fonts, "Connecting to Last.fm…", err.unwrap_or("Waiting for the network"))
        }
        View::Track(None) => centered_message(&mut pm, fonts, "Nothing playing", "Start something in Tidal"),
        View::Track(Some(t)) => eq = track(&mut pm, fonts, t, now),
    }

    Scene { pixels: to_xrgb(&pm), eq }
}

// --- layouts ---------------------------------------------------------------

fn centered_message(pm: &mut Pixmap, fonts: &Fonts, title: &str, sub: &str) {
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let vmin = w.min(h) / 100.0;
    let big = clamp(18.0, 5.0 * vmin, 48.0);
    let small = clamp(13.0, 3.2 * vmin, 28.0);
    let tw = text_width(&fonts.bold, big, title, 0.0);
    let sw = text_width(&fonts.regular, small, sub, 0.0);
    let top = h / 2.0 - (big * 1.2 + small * 1.4) / 2.0;
    draw_text(pm, &fonts.bold, big, (w - tw) / 2.0, top, title, WHITE, 0.0);
    draw_text(pm, &fonts.regular, small, (w - sw) / 2.0, top + big * 1.4, sub, DIM, 0.0);
}

fn track(pm: &mut Pixmap, fonts: &Fonts, t: &Track, now: SystemTime) -> Option<EqSpec> {
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let vmin = w.min(h) / 100.0;
    let pad = clamp(16.0, 4.0 * vmin, 48.0);
    let portrait = h > w;

    // Background: blur the cover at 48px (cheap), then stretch it up. Blurring
    // before upscaling keeps it smooth; plain upscaling alone shows blocky blobs.
    if let Some(art) = &t.art {
        let small = imageops::resize(art.as_ref(), 48, 48, imageops::FilterType::Triangle);
        let soft = imageops::blur(&small, 7.0);
        let bw = (w * 1.1) as u32;
        let bh = (h * 1.1) as u32;
        let big = imageops::resize(&soft, bw, bh, imageops::FilterType::CatmullRom);
        if let Some(bp) = to_pixmap(&big) {
            pm.draw_pixmap(
                -(w * 0.05) as i32,
                -(h * 0.05) as i32,
                bp.as_ref(),
                &Default::default(),
                Transform::identity(),
                None,
            );
        }
        let dim = if t.playing { 0.0 } else { 0.12 };
        let shade = LinearGradient::new(
            Point::from_xy(0.0, 0.0),
            Point::from_xy(w, 0.0),
            vec![
                GradientStop::new(0.0, Color::from_rgba(0.0, 0.0, 0.0, 0.35 + dim).unwrap()),
                GradientStop::new(1.0, Color::from_rgba(0.0, 0.0, 0.0, 0.6 + dim).unwrap()),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        );
        if let Some(shader) = shade {
            let paint = Paint { shader, ..Default::default() };
            pm.fill_rect(Rect::from_xywh(0.0, 0.0, w, h).unwrap(), &paint, Transform::identity(), None);
        }
    }

    // Font sizes (same scale as the web version had).
    let title_px = clamp(22.0, 6.4 * vmin, 64.0);
    let artist_px = clamp(15.0, 4.0 * vmin, 38.0);
    let album_px = clamp(12.0, 2.8 * vmin, 26.0);
    let status_px = clamp(10.0, 2.2 * vmin, 18.0);

    // Cover + text column geometry.
    let (cover, text_x, text_w, center) = if portrait {
        let size = (w - 2.0 * pad).min(h * 0.58);
        (((w - size) / 2.0, pad, size), pad, w - 2.0 * pad, true)
    } else {
        let size = (h - 2.0 * pad).min(w * 0.52);
        let tx = pad + size + pad;
        ((pad, (h - size) / 2.0, size), tx, w - tx - pad, false)
    };

    // Text block.
    let status = if t.playing {
        "NOW PLAYING".to_string()
    } else {
        match t.played_at {
            Some(at) => format!("LAST PLAYED · {}", ago(now, at).to_uppercase()),
            None => "PAUSED".to_string(),
        }
    };
    let tracking = 0.14 * status_px;
    let eq_bar = (0.22 * status_px).round().max(2.0);
    let eq_gap = (0.18 * status_px).round().max(1.0);
    let eq_w = eq_bar * 3.0 + eq_gap * 2.0;
    let eq_space = if t.playing { eq_w + 0.6 * status_px } else { 0.0 };

    let title_lines = wrap(&fonts.bold, title_px, &t.title, text_w, 2);
    let artist = ellipsize(&fonts.regular, artist_px, &t.artist, text_w);
    let album = ellipsize(&fonts.regular, album_px, &t.album, text_w);

    let line = |px: f32| px * 1.2;
    let mut block = line(status_px) + 0.7 * status_px + title_lines.len() as f32 * title_px * 1.08;
    block += 0.35 * artist_px + line(artist_px);
    if !album.is_empty() {
        block += 0.25 * album_px + line(album_px);
    }
    let mut y = if portrait {
        let top = cover.1 + cover.2 + pad;
        top + ((h - pad - top) - block).max(0.0) / 2.0
    } else {
        (h - block) / 2.0
    };

    let align = |width: f32| if center { text_x + (text_w - width) / 2.0 } else { text_x };

    // Status line (+ room for the animated bars).
    let status_color = if t.playing { WHITE } else { DIM };
    let sw = text_width(&fonts.bold, status_px, &status, tracking) + eq_space;
    let sx = align(sw);
    draw_text(pm, &fonts.bold, status_px, sx + eq_space, y, &status, status_color, tracking);
    let eq = t.playing.then(|| {
        let eq_h = (status_px * 0.9).round();
        let ascent = fonts.bold.as_scaled(PxScale::from(status_px)).ascent();
        EqSpec {
            x: sx.round() as usize,
            y: (y + ascent - eq_h).round().max(0.0) as usize,
            bar_w: eq_bar as usize,
            gap: eq_gap as usize,
            h: eq_h as usize,
            color: 0xFFFFFF,
        }
    });
    y += line(status_px) + 0.7 * status_px;

    for l in &title_lines {
        draw_text(pm, &fonts.bold, title_px, align(text_width(&fonts.bold, title_px, l, 0.0)), y, l, WHITE, 0.0);
        y += title_px * 1.08;
    }
    y += 0.35 * artist_px;
    draw_text(pm, &fonts.regular, artist_px, align(text_width(&fonts.regular, artist_px, &artist, 0.0)), y, &artist, DIM, 0.0);
    y += line(artist_px);
    if !album.is_empty() {
        y += 0.25 * album_px;
        draw_text(pm, &fonts.regular, album_px, align(text_width(&fonts.regular, album_px, &album, 0.0)), y, &album, FAINT, 0.0);
    }

    // Cover last so its shadow sits on top of the background.
    draw_cover(pm, t, cover, vmin);
    eq
}

fn draw_cover(pm: &mut Pixmap, t: &Track, (x, y, size): (f32, f32, f32), vmin: f32) {
    // Paused: shrink a little and dim, like a record that stopped spinning.
    let (x, y, size) = if t.playing {
        (x, y, size)
    } else {
        let s = size * 0.94;
        (x + (size - s) / 2.0, y + (size - s) / 2.0, s)
    };
    let radius = clamp(6.0, 1.4 * vmin, 14.0);

    // Soft shadow: a few stacked translucent rounded rects.
    for i in (1..=6).rev() {
        let grow = i as f32 * 0.9 * vmin;
        if let Some(p) = rounded(x - grow, y - grow + 1.2 * vmin, size + 2.0 * grow, size + 2.0 * grow, radius + grow) {
            fill(pm, &p, Color::from_rgba(0.0, 0.0, 0.0, 0.07).unwrap());
        }
    }

    let Some(shape) = rounded(x, y, size, size, radius) else { return };
    let px = size.round() as u32;
    match t.art.as_deref() {
        Some(art) if px > 0 => {
            // Pre-scale with a good filter, then draw 1:1 (no sampling artefacts).
            let (aw, ah) = art.dimensions();
            let scale = (px as f32 / aw as f32).max(px as f32 / ah as f32);
            let (sw, sh) = ((aw as f32 * scale).ceil() as u32, (ah as f32 * scale).ceil() as u32);
            let scaled = imageops::resize(art, sw.max(px), sh.max(px), imageops::FilterType::CatmullRom);
            let cropped = imageops::crop_imm(&scaled, (sw.max(px) - px) / 2, (sh.max(px) - px) / 2, px, px).to_image();
            if let Some(cp) = to_pixmap(&cropped) {
                let shader = tiny_skia::Pattern::new(
                    cp.as_ref(),
                    SpreadMode::Pad,
                    tiny_skia::FilterQuality::Nearest,
                    1.0,
                    Transform::from_translate(x.round(), y.round()),
                );
                let paint = Paint { shader, anti_alias: true, ..Default::default() };
                pm.fill_path(&shape, &paint, FillRule::Winding, Transform::identity(), None);
            }
        }
        _ => {
            fill(pm, &shape, Color::from_rgba8(28, 28, 32, 255));
            if let Some(note) = music_note(x, y, size) {
                fill(pm, &note, Color::from_rgba(1.0, 1.0, 1.0, 0.18).unwrap());
            }
        }
    }
    if !t.playing {
        fill(pm, &shape, Color::from_rgba(0.0, 0.0, 0.0, 0.3).unwrap());
    }
}

// --- text ------------------------------------------------------------------

const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const DIM: [f32; 4] = [1.0, 1.0, 1.0, 0.62];
const FAINT: [f32; 4] = [1.0, 1.0, 1.0, 0.47];

fn text_width(font: &FontVec, px: f32, s: &str, tracking: f32) -> f32 {
    let f = font.as_scaled(PxScale::from(px));
    let mut width = 0.0;
    let mut prev = None;
    for c in s.chars() {
        let id = f.glyph_id(c);
        if let Some(p) = prev {
            width += f.kern(p, id);
        }
        width += f.h_advance(id) + tracking;
        prev = Some(id);
    }
    width - if s.is_empty() { 0.0 } else { tracking }
}

/// Draws `s` with its line box starting at `top`.
fn draw_text(pm: &mut Pixmap, font: &FontVec, px: f32, x: f32, top: f32, s: &str, color: [f32; 4], tracking: f32) {
    let f = font.as_scaled(PxScale::from(px));
    let baseline = top + f.ascent() + (px * 1.2 - (f.ascent() - f.descent())) / 2.0;
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let data = pm.data_mut();
    let mut cx = x;
    let mut prev = None;
    for c in s.chars() {
        let id = f.glyph_id(c);
        if let Some(p) = prev {
            cx += f.kern(p, id);
        }
        let glyph = id.with_scale_and_position(PxScale::from(px), point(cx, baseline));
        cx += f.h_advance(id) + tracking;
        prev = Some(id);
        let Some(outline) = font.outline_glyph(glyph) else { continue };
        let b = outline.px_bounds();
        outline.draw(|gx, gy, cov| {
            let (dx, dy) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
            if dx < 0 || dy < 0 || dx >= pw || dy >= ph {
                return;
            }
            let a = (cov.min(1.0) * color[3]).clamp(0.0, 1.0);
            let i = ((dy * pw + dx) * 4) as usize;
            for ch in 0..3 {
                let d = data[i + ch] as f32;
                data[i + ch] = (color[ch] * 255.0 * a + d * (1.0 - a)).round() as u8;
            }
            data[i + 3] = 255;
        });
    }
}

fn ellipsize(font: &FontVec, px: f32, s: &str, max: f32) -> String {
    if text_width(font, px, s, 0.0) <= max {
        return s.to_string();
    }
    let mut chars: Vec<char> = s.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let cand: String = chars.iter().collect::<String>().trim_end().to_string() + "…";
        if text_width(font, px, &cand, 0.0) <= max {
            return cand;
        }
    }
    "…".into()
}

/// Greedy word wrap into at most `max_lines`; the last line gets an ellipsis.
fn wrap(font: &FontVec, px: f32, s: &str, max: f32, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let words: Vec<&str> = s.split_whitespace().collect();
    for (i, word) in words.iter().enumerate() {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if text_width(font, px, &cand, 0.0) <= max || cur.is_empty() {
            cur = cand;
        } else {
            lines.push(std::mem::take(&mut cur));
            if lines.len() == max_lines - 1 {
                let rest = words[i..].join(" ");
                lines.push(ellipsize(font, px, &rest, max));
                return lines;
            }
            cur = word.to_string();
        }
    }
    if !cur.is_empty() {
        lines.push(ellipsize(font, px, &cur, max));
    }
    lines
}

fn ago(now: SystemTime, at: SystemTime) -> String {
    let m = (now.duration_since(at).map(|d| d.as_secs()).unwrap_or(0) as f64 / 60.0).round() as u64;
    match m {
        0 => "just now".into(),
        1..=59 => format!("{m} min ago"),
        _ => {
            let h = (m as f64 / 60.0).round() as u64;
            if h < 24 {
                format!("{h} h ago")
            } else {
                let d = (h as f64 / 24.0).round() as u64;
                if d == 1 { "yesterday".into() } else { format!("{d} days ago") }
            }
        }
    }
}

// --- helpers ---------------------------------------------------------------

fn fill(pm: &mut Pixmap, path: &Path, color: Color) {
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    pm.fill_path(path, &paint, FillRule::Winding, Transform::identity(), None);
}

fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let k = 0.5523 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

/// A simple beamed eighth-note pair, centered in the cover.
fn music_note(x: f32, y: f32, size: f32) -> Option<Path> {
    let s = size * 0.34 / 24.0;
    let ox = x + (size - 24.0 * s) / 2.0;
    let oy = y + (size - 24.0 * s) / 2.0;
    let p = |px: f32, py: f32| (ox + px * s, oy + py * s);
    let mut pb = PathBuilder::new();
    let (cx1, cy1) = p(6.0, 18.5);
    pb.push_circle(cx1, cy1, 3.0 * s);
    let (cx2, cy2) = p(17.0, 15.5);
    pb.push_circle(cx2, cy2, 3.0 * s);
    let (a, b) = p(7.8, 4.6);
    pb.push_rect(Rect::from_xywh(a, b, 1.6 * s, 14.0 * s)?);
    let (a, b) = p(18.8, 2.6);
    pb.push_rect(Rect::from_xywh(a, b, 1.6 * s, 13.0 * s)?);
    let (a1, b1) = p(7.8, 4.6);
    let (a2, b2) = p(20.4, 2.4);
    let (a3, b3) = p(20.4, 5.6);
    let (a4, b4) = p(7.8, 7.8);
    pb.move_to(a1, b1);
    pb.line_to(a2, b2);
    pb.line_to(a3, b3);
    pb.line_to(a4, b4);
    pb.close();
    pb.finish()
}

fn to_pixmap(img: &RgbaImage) -> Option<Pixmap> {
    let mut data = img.as_raw().clone();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u16;
        if a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u16 * a + 127) / 255) as u8;
            }
        }
    }
    Pixmap::from_vec(data, tiny_skia::IntSize::from_wh(img.width(), img.height())?)
}

fn to_xrgb(pm: &Pixmap) -> Vec<u32> {
    pm.data()
        .chunks_exact(4)
        .map(|p| (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32)
        .collect()
}
