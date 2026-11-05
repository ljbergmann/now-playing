//! Drawing helpers shared by all themes: text, shapes, images, colour, time.
//! Everything here runs on the compose thread (never in the 60 fps loop).

use std::time::{SystemTime, UNIX_EPOCH};

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use image::{RgbaImage, imageops};
use tiny_skia::{Color, FillRule, GradientStop, LinearGradient, Paint, Path, PathBuilder, Pixmap, Point, Rect, SpreadMode, Transform};

pub type Rgba = [f32; 4];

/// `NP_PROFILE=1` prints how long each rendering stage takes (for tuning on the Pi).
pub struct Prof(&'static str, std::time::Instant);
impl Prof {
    pub fn new(label: &'static str) -> Option<Prof> {
        std::env::var_os("NP_PROFILE").map(|_| Prof(label, std::time::Instant::now()))
    }
}
impl Drop for Prof {
    fn drop(&mut self) {
        eprintln!("  {:<18} {:>7.1} ms", self.0, self.1.elapsed().as_secs_f64() * 1000.0);
    }
}

/// Runs `f(row_index, row)` over the rows of `buf` on several threads. Leaves
/// one core free so the 60 fps loop never has to wait while a screen renders.
pub fn par_rows<T: Send>(buf: &mut [T], width: usize, f: impl Fn(usize, &mut [T]) + Sync) {
    if width == 0 {
        return;
    }
    let rows = buf.len() / width;
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).saturating_sub(1).clamp(1, 8);
    if threads == 1 || rows < 32 {
        buf.chunks_mut(width).enumerate().for_each(|(y, row)| f(y, row));
        return;
    }
    let per = rows.div_ceil(threads);
    std::thread::scope(|s| {
        for (i, chunk) in buf.chunks_mut(per * width).enumerate() {
            let f = &f;
            s.spawn(move || chunk.chunks_mut(width).enumerate().for_each(|(j, row)| f(i * per + j, row)));
        }
    });
}

/// The two fixed light reflections on a record, from the offset to its centre.
/// Plain arithmetic (double-angle identities, repeated squaring) instead of
/// atan2/cos/powf: this runs for every pixel of the record.
#[inline]
pub fn sheen(dx: f32, dy: f32) -> f32 {
    let r2 = dx * dx + dy * dy;
    if r2 <= 0.0 {
        return 0.0;
    }
    let (c2, s2) = ((dx * dx - dy * dy) / r2, 2.0 * dx * dy / r2); // cos 2a, sin 2a
    // cos(2a + π/2) = -sin 2a, raised to the 10th.
    let a = (-s2).max(0.0);
    let (a2, a4) = (a * a, a * a * a * a);
    let p10 = a4 * a4 * a2;
    // cos(2a - 0.3π), raised to the 30th.
    let b = (c2 * 0.587_785 + s2 * 0.809_017).max(0.0);
    let (b2, b4) = (b * b, b * b * b * b);
    let (b8, b16) = (b4 * b4, b4 * b4 * b4 * b4);
    p10 + b16 * b8 * b4 * b2 * 0.5
}
pub const WHITE: Rgba = [1.0, 1.0, 1.0, 1.0];
pub const DIM: Rgba = [1.0, 1.0, 1.0, 0.66];
pub const FAINT: Rgba = [1.0, 1.0, 1.0, 0.45];

pub fn rgba(c: [u8; 3], a: f32) -> Rgba {
    [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, a]
}

pub fn clamp(lo: f32, v: f32, hi: f32) -> f32 {
    v.max(lo).min(hi)
}

// --- text ------------------------------------------------------------------

pub fn text_width(font: &FontVec, px: f32, s: &str, tracking: f32) -> f32 {
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

/// Height of a line box for `px` (the y step between lines).
pub fn line_h(px: f32) -> f32 {
    px * 1.2
}

/// Draws `s` with its line box starting at `top`. Returns the drawn width.
pub fn draw_text(pm: &mut Pixmap, font: &FontVec, px: f32, x: f32, top: f32, s: &str, color: Rgba, tracking: f32) -> f32 {
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
            // Premultiplied "over": correct on opaque and on transparent layers.
            for ch in 0..3 {
                let d = data[i + ch] as f32;
                data[i + ch] = (color[ch] * 255.0 * a + d * (1.0 - a)).round() as u8;
            }
            data[i + 3] = (255.0 * a + data[i + 3] as f32 * (1.0 - a)).round() as u8;
        });
    }
    cx - x - if s.is_empty() { 0.0 } else { tracking }
}

/// Horizontal placement inside a column.
#[derive(Clone, Copy, PartialEq)]
pub enum Align {
    Left,
    Center,
}

pub fn draw_aligned(
    pm: &mut Pixmap,
    font: &FontVec,
    px: f32,
    (x, w): (f32, f32),
    top: f32,
    s: &str,
    color: Rgba,
    tracking: f32,
    align: Align,
) {
    let tw = text_width(font, px, s, tracking);
    let x = if align == Align::Center { x + (w - tw) / 2.0 } else { x };
    draw_text(pm, font, px, x, top, s, color, tracking);
}

pub fn ellipsize(font: &FontVec, px: f32, s: &str, max: f32, tracking: f32) -> String {
    if text_width(font, px, s, tracking) <= max {
        return s.to_string();
    }
    let mut chars: Vec<char> = s.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let cand: String = chars.iter().collect::<String>().trim_end().to_string() + "…";
        if text_width(font, px, &cand, tracking) <= max {
            return cand;
        }
    }
    "…".into()
}

/// Greedy word wrap into at most `max_lines`; the last line gets an ellipsis.
pub fn wrap(font: &FontVec, px: f32, s: &str, max: f32, max_lines: usize, tracking: f32) -> Vec<String> {
    let max_lines = max_lines.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let words: Vec<&str> = s.split_whitespace().collect();
    for (i, word) in words.iter().enumerate() {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if cur.is_empty() || text_width(font, px, &cand, tracking) <= max {
            cur = cand;
            continue;
        }
        if lines.len() + 1 == max_lines {
            // Last allowed line: squeeze the rest in and cut it with an ellipsis.
            let last = format!("{cur} {}", words[i..].join(" "));
            lines.push(ellipsize(font, px, &last, max, tracking));
            return lines;
        }
        lines.push(std::mem::replace(&mut cur, word.to_string()));
    }
    if !cur.is_empty() {
        lines.push(ellipsize(font, px, &cur, max, tracking));
    }
    lines
}

/// Largest size ≤ `max_px` at which `s` wraps into ≤ `lines` lines without
/// cutting words; falls back to `min_px` with an ellipsis.
pub fn fit(font: &FontVec, s: &str, width: f32, lines: usize, max_px: f32, min_px: f32, tracking_em: f32) -> (f32, Vec<String>) {
    let mut px = max_px;
    while px > min_px {
        let wrapped = wrap(font, px, s, width, usize::MAX, tracking_em * px);
        if wrapped.len() <= lines && wrapped.iter().all(|l| !l.ends_with('…')) {
            return (px, wrapped);
        }
        px *= 0.93;
    }
    (min_px, wrap(font, min_px, s, width, lines, tracking_em * min_px))
}

// --- shapes ----------------------------------------------------------------

pub fn fill(pm: &mut Pixmap, path: &Path, color: Color) {
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    pm.fill_path(path, &paint, FillRule::Winding, Transform::identity(), None);
}

pub fn fill_rect(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, color: Color) {
    if let Some(r) = Rect::from_xywh(x, y, w, h) {
        let mut paint = Paint::default();
        paint.set_color(color);
        pm.fill_rect(r, &paint, Transform::identity(), None);
    }
}

/// Vertical gradient between two colours over [y0, y1].
pub fn vgradient(pm: &mut Pixmap, y0: f32, y1: f32, c0: Color, c1: Color) {
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    if let Some(shader) = LinearGradient::new(
        Point::from_xy(0.0, y0),
        Point::from_xy(0.0, y1),
        vec![GradientStop::new(0.0, c0), GradientStop::new(1.0, c1)],
        SpreadMode::Pad,
        Transform::identity(),
    ) {
        let paint = Paint { shader, ..Default::default() };
        pm.fill_rect(Rect::from_xywh(0.0, 0.0, w, h).unwrap(), &paint, Transform::identity(), None);
    }
}

pub fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
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

/// A soft drop shadow / glow: stacked translucent rounded rects.
/// A soft glow/shadow around a rounded rect, in one parallel pass: each pixel's
/// strength comes from its distance to the shape (no stacked path fills).
pub fn glow(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, spread: f32, dy: f32, color: [u8; 3], alpha: f32) {
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let y = y + dy;
    let (x0, x1) = (((x - spread).floor() as i32).max(0), ((x + w + spread).ceil() as i32).min(pw));
    let (y0, y1) = (((y - spread).floor() as i32).max(0), ((y + h + spread).ceil() as i32).min(ph));
    let (cx, cy, hx, hy) = (x + w / 2.0, y + h / 2.0, w / 2.0 - r, h / 2.0 - r);
    let col = color.map(|c| c as f32);
    par_rows(pm.data_mut(), pw as usize * 4, |py, row| {
        let py = py as i32;
        if py < y0 || py >= y1 {
            return;
        }
        let qy = ((py as f32 + 0.5 - cy).abs() - hy).max(0.0);
        for px in x0..x1 {
            let qx = ((px as f32 + 0.5 - cx).abs() - hx).max(0.0);
            let dist = (qx * qx + qy * qy).sqrt() - r; // <0 inside the shape
            let t = (1.0 - dist.max(0.0) / spread).clamp(0.0, 1.0);
            let a = alpha * t * t; // fades out smoothly
            if a <= 0.002 {
                continue;
            }
            let i = px as usize * 4;
            for c in 0..3 {
                row[i + c] = (col[c] * a + row[i + c] as f32 * (1.0 - a)) as u8;
            }
            row[i + 3] = (255.0 * a + row[i + 3] as f32 * (1.0 - a)) as u8;
        }
    });
}

/// Darkens/tints the whole pixmap towards `color` by `alpha` (one parallel pass).
pub fn shade(pm: &mut Pixmap, color: [u8; 3], alpha: f32) {
    let w = pm.width() as usize * 4;
    let col = color.map(|c| c as f32 * alpha);
    par_rows(pm.data_mut(), w, |_, row| {
        for p in row.chunks_exact_mut(4) {
            for c in 0..3 {
                p[c] = (col[c] + p[c] as f32 * (1.0 - alpha)) as u8;
            }
        }
    });
}

/// Lays a premultiplied layer of the same size over `dst` (parallel, skips
/// empty pixels). Much cheaper than a general compositing pipeline.
pub fn over(dst: &mut Pixmap, src: &Pixmap) {
    let w = dst.width() as usize * 4;
    let s = src.data();
    par_rows(dst.data_mut(), w, |y, row| {
        let srow = &s[y * w..(y + 1) * w];
        for (d, p) in row.chunks_exact_mut(4).zip(srow.chunks_exact(4)) {
            let a = p[3] as u32;
            if a == 0 {
                continue;
            }
            let ia = 255 - a;
            for c in 0..4 {
                d[c] = (p[c] as u32 + (d[c] as u32 * ia + 127) / 255).min(255) as u8;
            }
        }
    });
}

/// Draws a square image (pre-scaled to `size`) clipped to a rounded rect.
pub fn draw_cover(pm: &mut Pixmap, art: &RgbaImage, x: f32, y: f32, size: f32, radius: f32) {
    let px = size.round().max(1.0) as u32;
    let scaled = square(art, px);
    let Some(cp) = to_pixmap(&scaled) else { return };
    let Some(shape) = rounded(x.round(), y.round(), px as f32, px as f32, radius) else { return };
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

/// Scales with a filter that's good *and* cheap for the direction: area
/// averaging when shrinking (covers are 770 px, screens show them smaller),
/// bilinear when enlarging.
pub fn scale(img: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    if (w, h) == img.dimensions() {
        img.clone()
    } else if w <= img.width() && h <= img.height() {
        imageops::thumbnail(img, w.max(1), h.max(1))
    } else {
        imageops::resize(img, w.max(1), h.max(1), imageops::FilterType::Triangle)
    }
}

/// Centre-crops to a square and scales to `px`.
pub fn square(art: &RgbaImage, px: u32) -> RgbaImage {
    let (aw, ah) = art.dimensions();
    let side = aw.min(ah);
    let sq = imageops::crop_imm(art, (aw - side) / 2, (ah - side) / 2, side, side).to_image();
    scale(&sq, px, px)
}

/// Scales `art` to cover a w×h area (crop the overflow), like CSS object-fit: cover.
/// `focus_y` 0..1 picks which part survives a vertical crop.
pub fn cover_fill(art: &RgbaImage, w: u32, h: u32, focus_y: f32) -> RgbaImage {
    let (aw, ah) = (art.width() as f32, art.height() as f32);
    let s = (w as f32 / aw).max(h as f32 / ah);
    let (cw, ch) = ((w as f32 / s).min(aw), (h as f32 / s).min(ah));
    let x0 = ((aw - cw) / 2.0) as u32;
    let y0 = ((ah - ch) * focus_y.clamp(0.0, 1.0)) as u32;
    let crop = imageops::crop_imm(art, x0, y0, cw as u32, ch as u32).to_image();
    scale(&crop, w, h)
}

/// Blurs at 48 px and scales up: a smooth colour field from the cover.
pub fn blurred(art: &RgbaImage, w: u32, h: u32, sigma: f32) -> RgbaImage {
    let small = imageops::thumbnail(art, 48, 48);
    let soft = imageops::blur(&small, sigma);
    upscale(&soft, w, h)
}

/// Bilinear enlargement of a small, smooth image, in parallel. Plenty for
/// blurred colour fields, and far cheaper than a general resampler.
pub fn upscale(src: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let (w, h) = (w.max(1) as usize, h.max(1) as usize);
    let s = src.as_raw();
    let mut out = vec![0u8; w * h * 4];
    let (fx, fy) = (sw as f32 / w as f32, sh as f32 / h as f32);
    par_rows(&mut out, w * 4, |y, row| {
        let syf = ((y as f32 + 0.5) * fy - 0.5).clamp(0.0, (sh - 1) as f32);
        let (y0, ty) = (syf as usize, syf.fract());
        let y1 = (y0 + 1).min(sh - 1);
        for x in 0..w {
            let sxf = ((x as f32 + 0.5) * fx - 0.5).clamp(0.0, (sw - 1) as f32);
            let (x0, tx) = (sxf as usize, sxf.fract());
            let x1 = (x0 + 1).min(sw - 1);
            for c in 0..4 {
                let p = |xx: usize, yy: usize| s[(yy * sw + xx) * 4 + c] as f32;
                let top = p(x0, y0) + (p(x1, y0) - p(x0, y0)) * tx;
                let bot = p(x0, y1) + (p(x1, y1) - p(x0, y1)) * tx;
                row[x * 4 + c] = (top + (bot - top) * ty) as u8;
            }
        }
    });
    RgbaImage::from_raw(w as u32, h as u32, out).expect("sized buffer")
}

/// Copies an opaque image onto the pixmap at (x, y), clipped (parallel).
pub fn blit(pm: &mut Pixmap, img: &RgbaImage, x: i32, y: i32) {
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    let (x0, x1) = (x.max(0), (x + iw).min(pw));
    if x0 >= x1 {
        return;
    }
    let src = img.as_raw();
    par_rows(pm.data_mut(), pw as usize * 4, |py, row| {
        let (py, sy) = (py as i32, py as i32 - y);
        if sy < 0 || sy >= ih || py >= ph {
            return;
        }
        let s0 = ((sy * iw + (x0 - x)) * 4) as usize;
        let n = ((x1 - x0) * 4) as usize;
        row[(x0 * 4) as usize..(x0 * 4) as usize + n].copy_from_slice(&src[s0..s0 + n]);
        for p in row[(x0 * 4) as usize..(x0 * 4) as usize + n].chunks_exact_mut(4) {
            p[3] = 255; // covers and colour fields are opaque
        }
    });
}

pub fn to_pixmap(img: &RgbaImage) -> Option<Pixmap> {
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

pub fn to_xrgb(pm: &Pixmap) -> Vec<u32> {
    pm.data()
        .chunks_exact(4)
        .map(|p| (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32)
        .collect()
}

/// In-place colour grading of an opaque pixmap: desaturate by `desat` (0..1)
/// and scale brightness by `bright`.
pub fn grade(pm: &mut Pixmap, desat: f32, bright: f32) {
    for p in pm.data_mut().chunks_exact_mut(4) {
        let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
        let l = 0.299 * r + 0.587 * g + 0.114 * b;
        for (i, c) in [r, g, b].into_iter().enumerate() {
            p[i] = ((c + (l - c) * desat) * bright).clamp(0.0, 255.0) as u8;
        }
    }
}

// --- colour ----------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct Palette {
    /// Very dark tone of the cover, for backgrounds.
    pub dark: [u8; 3],
    /// The most vivid colour of the cover, brightened for use on dark.
    pub accent: [u8; 3],
}

impl Default for Palette {
    fn default() -> Self {
        Palette { dark: [14, 12, 18], accent: [232, 92, 64] }
    }
}

pub fn palette(art: Option<&RgbaImage>) -> Palette {
    let Some(art) = art else { return Palette::default() };
    let small = imageops::resize(art, 16, 16, imageops::FilterType::Triangle);
    let (mut sum, mut best, mut best_score) = ([0f32; 3], [255f32, 255.0, 255.0], -1f32);
    for p in small.pixels() {
        let c = [p[0] as f32, p[1] as f32, p[2] as f32];
        for i in 0..3 {
            sum[i] += c[i];
        }
        let max = c[0].max(c[1]).max(c[2]);
        let min = c[0].min(c[1]).min(c[2]);
        let sat = if max > 0.0 { (max - min) / max } else { 0.0 };
        let score = sat * (max / 255.0).powf(0.6) * if max > 50.0 { 1.0 } else { 0.2 };
        if score > best_score {
            best_score = score;
            best = c;
        }
    }
    let n = (small.width() * small.height()) as f32;
    let avg = sum.map(|s| s / n);
    // Lift the accent so it reads on dark backgrounds; grey covers get a warm white.
    let accent = if best_score < 0.12 {
        [236, 228, 216]
    } else {
        let max = best[0].max(best[1]).max(best[2]).max(1.0);
        let k = 235.0 / max;
        best.map(|c| (c * k).min(255.0) as u8)
    };
    Palette { dark: avg.map(|c| (c * 0.16 + 4.0) as u8), accent }
}

fn hsl(h: f32, s: f32, l: f32) -> [f32; 3] {
    let h = h.rem_euclid(360.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [(r + m) * 255.0, (g + m) * 255.0, (b + m) * 255.0]
}

/// Cover art for tracks that have none: a gradient and a sun-like disc, with
/// colours derived from the track name (so each song looks the same every time).
pub fn placeholder_art(seed: &str) -> RgbaImage {
    let hash = seed.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3));
    let hue = (hash % 360) as f32;
    let (cx, cy, r) = (300.0 + (hash >> 9) as f32 % 160.0, 170.0 + (hash >> 17) as f32 % 120.0, 120.0 + (hash >> 25) as f32 % 60.0);
    RgbaImage::from_fn(600, 600, |x, y| {
        let t = (x + y) as f32 / 1200.0;
        let mut c = hsl(hue + 55.0 * t, 0.6, 0.46 - 0.26 * t);
        let (dx, dy) = (x as f32 - cx, y as f32 - cy);
        let d = (dx * dx + dy * dy).sqrt();
        let a = (r - d + 1.0).clamp(0.0, 1.0) * 0.6;
        let sun = hsl(hue + 180.0, 0.7, 0.74);
        for i in 0..3 {
            c[i] = c[i] * (1.0 - a) + sun[i] * a;
        }
        image::Rgba([c[0] as u8, c[1] as u8, c[2] as u8, 255])
    })
}

pub fn color(c: [u8; 3]) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], 255)
}

pub fn xrgb(c: [u8; 3]) -> u32 {
    (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32
}

// --- time ------------------------------------------------------------------

pub struct Clock {
    pub time: String,
    pub date: String,
}

const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// Local wall-clock time (uses the system time zone, like `date`).
pub fn clock(now: SystemTime, twelve_hour: bool) -> Clock {
    let secs = now.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as libc::time_t;
    // SAFETY: localtime_r writes only into the provided struct.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    let time = if twelve_hour {
        format!("{}:{:02}", (tm.tm_hour + 11) % 12 + 1, tm.tm_min)
    } else {
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    };
    let date = format!(
        "{} {} {}",
        DAYS[tm.tm_wday.clamp(0, 6) as usize],
        tm.tm_mday,
        MONTHS[tm.tm_mon.clamp(0, 11) as usize]
    );
    Clock { time, date }
}

pub fn ago(now: SystemTime, at: SystemTime) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// The arithmetic version must match the original trigonometric one.
    #[test]
    fn sheen_matches_trig_version() {
        let trig = |dx: f32, dy: f32| {
            let a = dy.atan2(dx);
            (2.0 * (a + PI * 0.25)).cos().max(0.0).powf(10.0) + (2.0 * (a - PI * 0.15)).cos().max(0.0).powf(30.0) * 0.5
        };
        for k in 0..720 {
            let a = k as f32 * PI / 360.0;
            let (dx, dy) = (a.cos() * 37.0, a.sin() * 37.0);
            assert!((sheen(dx, dy) - trig(dx, dy)).abs() < 1e-3, "angle {k}");
        }
        assert_eq!(sheen(0.0, 0.0), 0.0);
    }

    #[test]
    fn par_rows_visits_every_row_once() {
        let mut buf = vec![0u32; 7 * 1000];
        par_rows(&mut buf, 7, |y, row| row.iter_mut().for_each(|v| *v += y as u32 + 1));
        for (y, row) in buf.chunks(7).enumerate() {
            assert!(row.iter().all(|&v| v == y as u32 + 1));
        }
    }
}
