//! Renders one complete, static frame for the current state, in the chosen theme.
//!
//! This is the expensive part (image scaling, text), so it runs on a
//! background thread and only when something changes. The 60 fps loop
//! (see `frame.rs`) only blends finished frames and animates small parts:
//! the equalizer bars, the spinning record label, the ambient colour drift.

use std::time::SystemTime;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use tiny_skia::Pixmap;

use crate::draw::{self, Align, DIM, Rgba, WHITE};
use crate::fonts::Fonts;
use crate::lastfm::Track;
use crate::themes;
use crate::vinylvault::Pressing;

/// A finished frame in 0RGB (what both the DRM buffer and the Mac window use).
pub struct Scene {
    pub pixels: Vec<u32>,
    /// Second frame to drift towards and back (ambient theme).
    pub alt: Option<Vec<u32>>,
    /// Seconds for one full drift cycle.
    pub drift_period: f32,
    /// Where the animated "now playing" bars go, if they should animate.
    pub eq: Option<EqSpec>,
    /// A rotating disc (the record label in the vinyl theme).
    pub spinner: Option<Spinner>,
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

/// A circular texture that the 60 fps loop rotates in place.
pub struct Spinner {
    pub x: usize,
    pub y: usize,
    pub size: usize,
    /// size×size texture (unrotated). 0RGB, or ARGB when `texture_alpha`.
    pub label: Vec<u32>,
    /// size×size coverage of the round shape (anti-aliased edges). Fixed on
    /// screen, so it must be the same at every angle.
    pub mask: Vec<u8>,
    /// The texture's top byte is an opacity that turns with it (clear
    /// vinyl with splatter, swirls, ...), multiplied with `mask`.
    pub texture_alpha: bool,
    /// Optional size×size ARGB layer that stays put on top of the rotating
    /// texture: light reflections, the tonearm and its shadow.
    pub overlay: Option<Vec<u32>>,
    /// Whether it should be turning (it spins up and down smoothly).
    pub spinning: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Theme {
    Vinyl,
    Poster,
    Ambient,
}

impl Theme {
    pub fn parse(s: &str) -> Option<Theme> {
        match s.to_ascii_lowercase().as_str() {
            "vinyl" | "record" => Some(Theme::Vinyl),
            "poster" => Some(Theme::Poster),
            "ambient" | "glow" => Some(Theme::Ambient),
            _ => None,
        }
    }
}

pub enum View<'a> {
    Connecting(Option<&'a str>),
    /// QR code for the setup page; `connected` once a phone opened it.
    Setup { url: &'a str, connected: bool },
    /// `track` = most recent track (if any); `idle` = show the clock screen;
    /// `pressing` = the copy you own of this album (VinylVault), if any.
    Track { track: Option<&'a Track>, idle: bool, pressing: Option<&'a Pressing> },
}

/// What a theme needs to draw.
pub struct Ctx<'a> {
    pub fonts: &'a Fonts,
    pub track: Option<&'a Track>,
    pub playing: bool,
    pub pressing: Option<&'a Pressing>,
    pub now: SystemTime,
    /// Show the small VinylVault mentions (PROMO=off hides them).
    pub promo: bool,
    pub twelve_hour: bool,
}

/// Animated extras a theme can hand to the 60 fps loop.
#[derive(Default)]
pub struct Extras {
    pub eq: Option<EqSpec>,
    pub spinner: Option<Spinner>,
    pub alt: Option<Pixmap>,
    pub drift_period: f32,
}

pub fn render(fonts: &Fonts, w: usize, h: usize, view: &View, now: SystemTime, theme: Theme, twelve_hour: bool, promo: bool) -> Scene {
    let mut pm = Pixmap::new(w as u32, h as u32).expect("non-zero size");
    pm.fill(draw::color([11, 11, 13]));

    let extras = match view {
        View::Connecting(err) => {
            connecting(&mut pm, fonts, err.unwrap_or("Waiting for the network"));
            Extras::default()
        }
        View::Setup { url, connected } => {
            setup(&mut pm, fonts, url, *connected);
            Extras::default()
        }
        View::Track { track, idle, pressing } => {
            // No cover on Last.fm? Use generated art so every look stays complete.
            let filled;
            let track = match track {
                Some(t) if t.art.is_none() => {
                    let mut t2 = (*t).clone();
                    t2.art = Some(std::sync::Arc::new(draw::placeholder_art(&format!("{}{}", t.artist, t.title))));
                    filled = t2;
                    Some(&filled)
                }
                other => *other,
            };
            let ctx = Ctx { fonts, track, playing: !idle, pressing: *pressing, now, twelve_hour, promo };
            let mut extras = match theme {
                Theme::Vinyl => themes::vinyl::render(&mut pm, &ctx),
                Theme::Poster => themes::poster::render(&mut pm, &ctx),
                Theme::Ambient => themes::ambient::render(&mut pm, &ctx),
            };
            if *idle && promo {
                brand(&mut pm, fonts);
                if let Some(alt) = extras.alt.as_mut() {
                    brand(alt, fonts);
                }
            }
            extras
        }
    };

    let p = draw::Prof::new("to_xrgb + bake");
    let mut pixels = draw::to_xrgb(&pm);
    // Bake the spinner into the static frame too (at angle 0), so snapshots and
    // the first frame already show it.
    if let Some(sp) = &extras.spinner {
        let under = pixels.clone();
        crate::frame::draw_spinner(&mut pixels, &under, w, sp, 0.0, 256);
    }
    drop(p);
    Scene {
        pixels,
        alt: extras.alt.as_ref().map(draw::to_xrgb),
        drift_period: extras.drift_period.max(1.0),
        eq: extras.eq,
        spinner: extras.spinner,
    }
}

/// A small, quiet VinylVault mark for the clock screen: a record icon, the
/// name and a tagline, tucked into the bottom-right corner.
fn brand(pm: &mut Pixmap, fonts: &Fonts) {
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let vmin = w.min(h) / 100.0;
    let pad = draw::clamp(12.0, 4.5 * vmin, 52.0);
    let px = draw::clamp(10.0, 2.3 * vmin, 20.0);
    let small = px * 0.8;
    let (name, tag) = ("VinylVault", "your records, every pressing");
    let name_w = draw::text_width(&fonts.bold, px, name, 0.02 * px);
    let tag_w = draw::text_width(&fonts.regular, small, tag, 0.0);
    let block_h = draw::line_h(px) + draw::line_h(small) - 0.15 * px;
    let icon = block_h * 0.9;
    let gap = 0.5 * px;
    let right = w - pad;
    let top = h - pad - block_h;
    let text_x = right - name_w.max(tag_w);
    // Record icon: disc, label, spindle hole.
    let (cx, cy, r) = (text_x - gap - icon / 2.0, top + block_h / 2.0, icon / 2.0);
    let circle = |r: f32| {
        let mut pb = tiny_skia::PathBuilder::new();
        pb.push_circle(cx, cy, r);
        pb.finish()
    };
    if let Some(p) = circle(r) {
        draw::fill(pm, &p, tiny_skia::Color::from_rgba8(255, 255, 255, 120));
    }
    if let Some(p) = circle(r * 0.4) {
        draw::fill(pm, &p, tiny_skia::Color::from_rgba8(0, 0, 0, 150));
    }
    if let Some(p) = circle(r * 0.08) {
        draw::fill(pm, &p, tiny_skia::Color::from_rgba8(255, 255, 255, 140));
    }
    draw::draw_text(pm, &fonts.bold, px, text_x, top, name, [1.0, 1.0, 1.0, 0.62], 0.02 * px);
    draw::draw_text(pm, &fonts.regular, small, text_x, top + draw::line_h(px) - 0.15 * px, tag, [1.0, 1.0, 1.0, 0.4], 0.0);
}

/// QR code on a white tile, with a few words next to it (below it on portrait screens).
fn setup(pm: &mut Pixmap, fonts: &Fonts, url: &str, connected: bool) {
    const ACCENT: draw::Rgba = [1.0, 0.48, 0.27, 1.0];
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let vmin = w.min(h) / 100.0;
    let pad = draw::clamp(16.0, 7.0 * vmin, 80.0);
    let portrait = h > w * 1.1;
    let (status, hint) = if connected {
        ("On your phone now", "Finish setting up there.")
    } else {
        ("Scan to set up", "Point your phone's camera at the code.")
    };
    let items = vec![
        Item::new(&fonts.bold, draw::clamp(10.0, 2.4 * vmin, 18.0), "NOW PLAYING · SETUP", ACCENT).track(0.12),
        Item::new(&fonts.bold, draw::clamp(20.0, 7.0 * vmin, 64.0), status, WHITE).gap(1.5 * vmin),
        Item::new(&fonts.regular, draw::clamp(13.0, 3.6 * vmin, 30.0), hint, DIM).gap(0.6 * vmin),
        Item::new(&fonts.regular, draw::clamp(11.0, 2.8 * vmin, 22.0), "Last.fm  ·  VinylVault (optional)  ·  Look", draw::FAINT).gap(4.0 * vmin),
        Item::new(&fonts.regular, draw::clamp(10.0, 2.4 * vmin, 18.0), url, draw::FAINT).gap(5.0 * vmin),
    ];
    let q = if portrait { (w * 0.62).min(h * 0.45) } else { (h * 0.66).min(w * 0.42) };
    let text_h = stack_height(&items);
    let (qx, qy) = if portrait { ((w - q) / 2.0, ((h - q - pad - text_h) / 2.0).max(pad)) } else { (pad * 1.4, (h - q) / 2.0) };

    if let Some(p) = draw::rounded(qx, qy, q, q, q * 0.05) {
        draw::fill(pm, &p, tiny_skia::Color::WHITE);
    }
    if let Ok(code) = qrcode::QrCode::new(url.as_bytes()) {
        let n = code.width();
        let quiet = 2.0;
        let m = q / (n as f32 + 2.0 * quiet);
        let dark = tiny_skia::Color::from_rgba8(14, 13, 16, 255);
        for (i, c) in code.to_colors().iter().enumerate() {
            if *c == qrcode::Color::Dark {
                let (x, y) = ((i % n) as f32, (i / n) as f32);
                // Snap to whole pixels so neighbouring modules leave no seams.
                let x0 = (qx + (x + quiet) * m).round();
                let y0 = (qy + (y + quiet) * m).round();
                let x1 = (qx + (x + 1.0 + quiet) * m).round();
                let y1 = (qy + (y + 1.0 + quiet) * m).round();
                draw::fill_rect(pm, x0, y0, x1 - x0, y1 - y0, dark);
            }
        }
    }

    let (col, top, align) = if portrait {
        ((pad, w - 2.0 * pad), qy + q + pad, Align::Center)
    } else {
        let x = qx + q + pad * 1.2;
        ((x, w - x - pad), 0.0, Align::Left)
    };
    let top = if portrait { top } else { (h - text_h) / 2.0 };
    draw_stack(pm, &items, col, top, align);
}

fn connecting(pm: &mut Pixmap, fonts: &Fonts, sub: &str) {
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let vmin = w.min(h) / 100.0;
    let big = draw::clamp(18.0, 5.0 * vmin, 48.0);
    let small = draw::clamp(13.0, 3.2 * vmin, 28.0);
    let top = h / 2.0 - (big * 1.2 + small * 1.4) / 2.0;
    draw::draw_aligned(pm, &fonts.bold, big, (0.0, w), top, "Connecting to Last.fm…", WHITE, 0.0, Align::Center);
    let sub = draw::ellipsize(&fonts.regular, small, sub, w * 0.9, 0.0);
    draw::draw_aligned(pm, &fonts.regular, small, (0.0, w), top + big * 1.4, &sub, DIM, 0.0, Align::Center);
}

// --- a tiny text-stack layout used by the themes ---------------------------

pub struct Item<'f> {
    pub font: &'f FontVec,
    pub px: f32,
    pub text: String,
    pub color: Rgba,
    /// Letter spacing in em.
    pub tracking: f32,
    /// Extra space above this line, in px.
    pub gap: f32,
    /// Put the animated equalizer bars in front of this line.
    pub eq: bool,
    /// Draw a small heart in front of this line (for fonts without ♥).
    pub heart: bool,
}

impl<'f> Item<'f> {
    pub fn new(font: &'f FontVec, px: f32, text: impl Into<String>, color: Rgba) -> Self {
        Item { font, px, text: text.into(), color, tracking: 0.0, gap: 0.0, eq: false, heart: false }
    }
    pub fn track(mut self, em: f32) -> Self {
        self.tracking = em;
        self
    }
    pub fn gap(mut self, px: f32) -> Self {
        self.gap = px;
        self
    }
    pub fn eq(mut self, on: bool) -> Self {
        self.eq = on;
        self
    }
}

pub fn stack_height(items: &[Item]) -> f32 {
    items.iter().map(|i| i.gap + draw::line_h(i.px)).sum()
}

/// Draws the lines from `top` down inside the column (x, width).
/// Returns where the equalizer bars should go, if any line asked for them.
pub fn draw_stack(pm: &mut Pixmap, items: &[Item], (x, width): (f32, f32), top: f32, align: Align) -> Option<EqSpec> {
    let mut y = top;
    let mut eq = None;
    for it in items {
        y += it.gap;
        let tracking = it.tracking * it.px;
        let (bar, gap) = ((0.22 * it.px).round().max(2.0), (0.18 * it.px).round().max(1.0));
        let eq_w = if it.eq { bar * 3.0 + gap * 2.0 + 0.6 * it.px } else if it.heart { heart_w(it.px) } else { 0.0 };
        let text = draw::ellipsize(it.font, it.px, &it.text, width - eq_w, tracking);
        let tw = draw::text_width(it.font, it.px, &text, tracking) + eq_w;
        let tx = if align == Align::Center { x + (width - tw) / 2.0 } else { x };
        draw::draw_text(pm, it.font, it.px, tx + eq_w, y, &text, it.color, tracking);
        if it.heart {
            let ascent = it.font.as_scaled(PxScale::from(it.px)).ascent();
            let baseline = y + ascent + (it.px * 1.2 - (ascent - it.font.as_scaled(PxScale::from(it.px)).descent())) / 2.0;
            draw_heart(pm, tx, baseline, it.px, it.color);
        }
        if it.eq {
            let eq_h = (it.px * 0.9).round();
            let ascent = it.font.as_scaled(PxScale::from(it.px)).ascent();
            let baseline = y + ascent + (it.px * 1.2 - (ascent - it.font.as_scaled(PxScale::from(it.px)).descent())) / 2.0;
            eq = Some(EqSpec {
                x: tx.round().max(0.0) as usize,
                y: (baseline - eq_h).round().max(0.0) as usize,
                bar_w: bar as usize,
                gap: gap as usize,
                h: eq_h as usize,
                color: draw::xrgb(it.color.map(|c| (c * 255.0) as u8)[..3].try_into().unwrap()),
            });
        }
        y += draw::line_h(it.px);
    }
    eq
}

/// Font size at which `sample` fills `width` (capped at `max_px`).
pub fn size_to_width(font: &FontVec, sample: &str, width: f32, max_px: f32, tracking_em: f32) -> f32 {
    let at100 = draw::text_width(font, 100.0, sample, tracking_em * 100.0).max(1.0);
    (100.0 * width / at100).min(max_px)
}

/// The quiet play-count line under the playing track, if Last.fm told us any.
pub fn stats_item<'f>(font: &'f FontVec, px: f32, track: &crate::lastfm::Track, color: Rgba) -> Option<Item<'f>> {
    let line = track.stats.as_ref()?.line(&track.artist)?;
    // Inter and San Francisco have ♥; other fallback fonts may not: draw one.
    let heart = crate::lastfm::HEART;
    match line.strip_prefix(heart) {
        Some(rest) if font.glyph_id(heart).0 == 0 => {
            let mut it = Item::new(font, px, rest.to_string(), color);
            it.heart = true;
            Some(it)
        }
        _ => Some(Item::new(font, px, line, color)),
    }
}

/// Room the drawn heart takes before the text (its own width; the text starts
/// with " · " after it).
fn heart_w(px: f32) -> f32 {
    0.62 * px
}

/// A small heart sitting on the baseline, about as tall as the x-height.
fn draw_heart(pm: &mut Pixmap, x: f32, baseline: f32, px: f32, color: Rgba) {
    let s = 0.62 * px; // width
    let (cx, top, bottom) = (x + s / 2.0, baseline - 0.62 * px, baseline - 0.02 * px);
    let mid = top + 0.3 * (bottom - top);
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(cx, bottom);
    pb.cubic_to(cx - 0.2 * s, bottom - 0.25 * (bottom - top), x, mid + 0.25 * (bottom - top), x, mid);
    pb.cubic_to(x, top - 0.02 * px, cx - 0.04 * s, top - 0.04 * px, cx, top + 0.22 * (bottom - top));
    pb.cubic_to(cx + 0.04 * s, top - 0.04 * px, x + s, top - 0.02 * px, x + s, mid);
    pb.cubic_to(x + s, mid + 0.25 * (bottom - top), cx + 0.2 * s, bottom - 0.25 * (bottom - top), cx, bottom);
    pb.close();
    if let Some(p) = pb.finish() {
        let c = color.map(|v| (v * 255.0).round() as u8);
        draw::fill(pm, &p, tiny_skia::Color::from_rgba8(c[0], c[1], c[2], c[3]));
    }
}
