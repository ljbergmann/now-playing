//! Ambient: the cover floats on a slowly shifting colour field made from its
//! own colours, with small, quiet type. More lamp than screen.
//! Idle: the field keeps drifting (slower), a thin clock sits in the middle.
//!
//! The drift is two pre-rendered frames whose backgrounds differ (the cover's
//! colour field and its mirror image); the 60 fps loop crossfades between them.

use image::{RgbaImage, imageops};
use tiny_skia::{Color, Pixmap};

use crate::draw::{self, Align, DIM, FAINT, WHITE, clamp};
use crate::scene::{self, Ctx, Extras, Item, draw_stack, size_to_width, stack_height};

pub fn render(pm: &mut Pixmap, c: &Ctx) -> Extras {
    let (w, h) = (pm.width(), pm.height());
    let art = c.track.and_then(|t| t.art.as_deref());
    let pal = draw::palette(art);

    let mut alt = pm.clone();
    let p = draw::Prof::new("ambient fields");
    field(pm, art, false, pal);
    field(&mut alt, art, true, pal);
    drop(p);
    // Cover, glow and text are the same on both frames: draw them once on a
    // transparent layer and lay that over each field.
    let _p = draw::Prof::new("ambient fg");
    let mut fg = Pixmap::new(w, h).expect("non-zero size");
    let eq = foreground(&mut fg, c, pal);
    draw::over(pm, &fg);
    draw::over(&mut alt, &fg);

    Extras { eq, alt: Some(alt), drift_period: if c.playing { 16.0 } else { 30.0 }, ..Default::default() }
}

/// The colour field: the cover blurred into soft blobs (or its mirror image).
fn field(pm: &mut Pixmap, art: Option<&RgbaImage>, mirrored: bool, pal: draw::Palette) {
    let (w, h) = (pm.width(), pm.height());
    match art {
        Some(a) => {
            let src = if mirrored { imageops::rotate180(a) } else { a.clone() };
            let mut b = draw::blurred(&src, w, h, 9.0);
            // Richer colour, lower brightness: glow, not glare.
            for p in b.pixels_mut() {
                let (r, g, bb) = (p[0] as f32, p[1] as f32, p[2] as f32);
                let l = 0.299 * r + 0.587 * g + 0.114 * bb;
                for (i, v) in [r, g, bb].into_iter().enumerate() {
                    p[i] = ((l + (v - l) * 1.5) * 0.62).clamp(0.0, 255.0) as u8;
                }
            }
            draw::blit(pm, &b, 0, 0);
        }
        None => {
            let a = Color::from_rgba8(pal.accent[0] / 3, pal.accent[1] / 3, pal.accent[2] / 3, 255);
            let d = draw::color(pal.dark);
            let (c0, c1) = if mirrored { (d, a) } else { (a, d) };
            draw::vgradient(pm, 0.0, h as f32, c0, c1);
        }
    }
}

fn foreground(pm: &mut Pixmap, c: &Ctx, pal: draw::Palette) -> Option<crate::scene::EqSpec> {
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let vmin = w.min(h) / 100.0;
    let pad = clamp(16.0, 6.0 * vmin, 72.0);
    let f = c.fonts;
    let col = (pad, w - 2.0 * pad);
    let art = c.track.and_then(|t| t.art.as_deref());

    if c.playing {
        let t = c.track.expect("playing implies a track");
        let mut items = vec![
            Item::new(&f.regular, clamp(16.0, 4.6 * vmin, 44.0), t.title.clone(), WHITE).eq(true),
            Item::new(&f.light, clamp(13.0, 3.4 * vmin, 32.0), t.artist.clone(), DIM).gap(2.0),
        ];
        if c.owned {
            let px = clamp(9.0, 2.0 * vmin, 16.0);
            items.push(scene::own_hint(&f.bold, px, 0.16, col.1, draw::rgba(pal.accent, 1.0)).gap(0.6 * px));
        }
        let layout = |items: &[Item]| {
            let text_h = stack_height(items);
            let size = (h - 2.0 * pad - text_h - pad * 0.8).min(w * 0.62).max(40.0);
            (text_h, size, (h - (size + pad * 0.8 + text_h)) / 2.0)
        };
        let (mut text_h, mut size, mut top) = layout(&items);
        // The play-count line arrives a moment after the track. It goes under
        // the artist, into the bottom margin if there's room (the cover
        // doesn't shrink when it appears); on small screens it joins the text.
        let mut late = None;
        if let Some(it) = scene::stats_item(&f.light, clamp(10.0, 2.4 * vmin, 22.0), t, FAINT) {
            let it = it.gap(0.15 * pad);
            if it.gap + draw::line_h(it.px) <= 0.6 * top {
                late = Some(it);
            } else {
                items.push(it.gap(2.0));
                (text_h, size, top) = layout(&items);
            }
        }
        let x = (w - size) / 2.0;
        let r = size * 0.025;
        draw::glow(pm, x, top, size, size, r, size * 0.09, size * 0.02, pal.accent, 0.42);
        draw::glow(pm, x, top, size, size, r, size * 0.07, size * 0.03, [0, 0, 0], 0.32);
        match art {
            Some(a) => draw::draw_cover(pm, a, x, top, size, r),
            None => {
                if let Some(p) = draw::rounded(x, top, size, size, r) {
                    draw::fill(pm, &p, Color::from_rgba8(255, 255, 255, 20));
                }
            }
        }
        let eq = draw_stack(pm, &items, col, top + size + pad * 0.8, Align::Center);
        if let Some(it) = late {
            draw_stack(pm, &[it], col, top + size + pad * 0.8 + text_h, Align::Center);
        }
        eq
    } else {
        let clk = draw::clock(c.now, c.twelve_hour);
        let clock_px = size_to_width(&f.light, "88:88", col.1 * 0.62, h * 0.34, -0.02);
        let mut items = vec![
            Item::new(&f.light, clock_px, clk.time, WHITE).track(-0.02),
            Item::new(&f.regular, clamp(11.0, 2.6 * vmin, 24.0), clk.date.to_lowercase(), DIM).track(0.08).gap(-0.06 * clock_px),
        ];
        let thumb = clamp(28.0, 9.0 * vmin, 96.0);
        let last = c.track.map(|t| format!("{} · {}", t.title, t.artist));
        if last.is_some() {
            items.push(Item::new(&f.light, 1.0, "", FAINT).gap(thumb + 0.5 * pad));
        }
        let block = stack_height(&items);
        let top = (h - block) / 2.0;
        draw_stack(pm, &items, col, top, Align::Center);

        // Small "last played" row under the clock: thumbnail + text.
        if let (Some(t), Some(line)) = (c.track, last) {
            let px = clamp(11.0, 2.6 * vmin, 22.0);
            let text = draw::ellipsize(&f.light, px, &line, col.1 - thumb - pad, 0.0);
            let tw = draw::text_width(&f.light, px, &text, 0.0);
            let row_w = thumb + 0.4 * pad + tw;
            let x = (w - row_w) / 2.0;
            let y = top + block - thumb;
            if let Some(a) = t.art.as_deref() {
                draw::draw_cover(pm, a, x, y, thumb, thumb * 0.06);
                // Dim it a touch so the clock stays the main thing.
                if let Some(p) = draw::rounded(x, y, thumb, thumb, thumb * 0.06) {
                    draw::fill(pm, &p, Color::from_rgba8(0, 0, 0, 70));
                }
            }
            draw::draw_text(pm, &f.light, px, x + thumb + 0.4 * pad, y + (thumb - px * 1.2) / 2.0, &text, FAINT, 0.0);
        }
        None
    }
}
