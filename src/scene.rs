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
    /// size×size 0RGB texture (unrotated).
    pub label: Vec<u32>,
    /// size×size coverage (anti-aliased circle edge).
    pub mask: Vec<u8>,
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
    /// `track` = most recent track (if any); `idle` = show the clock screen.
    Track { track: Option<&'a Track>, idle: bool },
}

/// What a theme needs to draw.
pub struct Ctx<'a> {
    pub fonts: &'a Fonts,
    pub track: Option<&'a Track>,
    pub playing: bool,
    pub now: SystemTime,
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

pub fn render(fonts: &Fonts, w: usize, h: usize, view: &View, now: SystemTime, theme: Theme, twelve_hour: bool) -> Scene {
    let mut pm = Pixmap::new(w as u32, h as u32).expect("non-zero size");
    pm.fill(draw::color([11, 11, 13]));

    let extras = match view {
        View::Connecting(err) => {
            connecting(&mut pm, fonts, err.unwrap_or("Waiting for the network"));
            Extras::default()
        }
        View::Track { track, idle } => {
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
            let ctx = Ctx { fonts, track, playing: !idle, now, twelve_hour };
            match theme {
                Theme::Vinyl => themes::vinyl::render(&mut pm, &ctx),
                Theme::Poster => themes::poster::render(&mut pm, &ctx),
                Theme::Ambient => themes::ambient::render(&mut pm, &ctx),
            }
        }
    };

    let mut pixels = draw::to_xrgb(&pm);
    // Bake the spinner into the static frame too (at angle 0), so snapshots and
    // the first frame already show it.
    if let Some(sp) = &extras.spinner {
        let under = pixels.clone();
        crate::frame::draw_spinner(&mut pixels, &under, w, sp, 0.0, 256);
    }
    Scene {
        pixels,
        alt: extras.alt.as_ref().map(draw::to_xrgb),
        drift_period: extras.drift_period.max(1.0),
        eq: extras.eq,
        spinner: extras.spinner,
    }
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
}

impl<'f> Item<'f> {
    pub fn new(font: &'f FontVec, px: f32, text: impl Into<String>, color: Rgba) -> Self {
        Item { font, px, text: text.into(), color, tracking: 0.0, gap: 0.0, eq: false }
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
        let eq_w = if it.eq { bar * 3.0 + gap * 2.0 + 0.6 * it.px } else { 0.0 };
        let text = draw::ellipsize(it.font, it.px, &it.text, width - eq_w, tracking);
        let tw = draw::text_width(it.font, it.px, &text, tracking) + eq_w;
        let tx = if align == Align::Center { x + (width - tw) / 2.0 } else { x };
        draw::draw_text(pm, it.font, it.px, tx + eq_w, y, &text, it.color, tracking);
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
