//! Poster: the cover fills the screen, big heavy type sits on a dark fade at
//! the bottom like a gig poster, with an accent colour taken from the art.
//! Idle: the cover goes dark and colourless and a huge clock takes over.

use tiny_skia::{Color, Pixmap};

use crate::draw::{self, Align, DIM, WHITE, clamp};
use crate::scene::{Ctx, Extras, Item, draw_stack, size_to_width, stack_height};

pub fn render(pm: &mut Pixmap, c: &Ctx) -> Extras {
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let vmin = w.min(h) / 100.0;
    let pad = clamp(16.0, 6.0 * vmin, 72.0);
    let art = c.track.and_then(|t| t.art.as_deref());
    let pal = draw::palette(art);
    let accent = draw::rgba(pal.accent, 1.0);
    let stacked = w < h * 1.2;

    // Picture.
    pm.fill(draw::color(pal.dark));
    let pic_h = if stacked { w.min(h * 0.62) } else { h };
    if let Some(a) = art {
        draw::blit(pm, &draw::cover_fill(a, w as u32, pic_h as u32, 0.3), 0, 0);
    }
    if !c.playing {
        draw::grade(pm, 0.85, 0.42);
    }
    // Fade into the dark band the type sits on.
    let dark = |a: f32| Color::from_rgba8(pal.dark[0] / 2, pal.dark[1] / 2, pal.dark[2] / 2, (a * 255.0) as u8);
    if stacked {
        draw::vgradient(pm, pic_h * 0.55, pic_h, dark(0.0), dark(1.0));
        draw::fill_rect(pm, 0.0, pic_h, w, h - pic_h, dark(1.0));
    } else {
        draw::vgradient(pm, h * 0.28, h, dark(0.0), dark(0.94));
    }

    let f = c.fonts;
    let col = (pad, w - 2.0 * pad);
    let mut items = Vec::new();
    if c.playing {
        let t = c.track.expect("playing implies a track");
        let max_px = if stacked { h * 0.075 } else { h * 0.19 };
        let (px, lines) = draw::fit(&f.black, &t.title.to_uppercase(), col.1, 2, max_px, max_px * 0.45, -0.02);
        items.push(Item::new(&f.bold, clamp(10.0, 2.3 * vmin, 20.0), "NOW PLAYING", accent).track(0.2).eq(true));
        for (i, l) in lines.iter().enumerate() {
            items.push(Item::new(&f.black, px, l.clone(), WHITE).track(-0.02).gap(if i == 0 { 0.3 * px } else { -0.2 * px }));
        }
        items.push(Item::new(&f.bold, clamp(12.0, 3.6 * vmin, 34.0), t.artist.to_uppercase(), DIM).track(0.14).gap(0.15 * px));
    } else {
        let clk = draw::clock(c.now, c.twelve_hour);
        let clock_px = size_to_width(&f.black, "88:88", col.1 * if stacked { 0.9 } else { 0.62 }, h * 0.42, -0.03);
        items.push(Item::new(&f.bold, clamp(10.0, 2.5 * vmin, 22.0), clk.date.to_uppercase(), accent).track(0.2));
        items.push(Item::new(&f.black, clock_px, clk.time, WHITE).track(-0.03).gap(-0.02 * clock_px));
        if let Some(t) = c.track {
            let when = t.played_at.map(|at| format!(" {}", draw::ago(c.now, at).to_uppercase())).unwrap_or_default();
            let line = format!("LAST PLAYED{when} — {} · {}", t.title.to_uppercase(), t.artist.to_uppercase());
            items.push(Item::new(&f.bold, clamp(10.0, 2.2 * vmin, 18.0), line, DIM).track(0.12).gap(0.02 * clock_px));
        }
    }

    // Bottom-aligned block with an accent rule on top.
    let block = stack_height(&items);
    let rule_h = clamp(3.0, 0.7 * vmin, 6.0);
    let y = h - pad - block;
    draw::fill_rect(pm, col.0, y - rule_h - 0.8 * pad, clamp(40.0, 14.0 * vmin, 160.0), rule_h, draw::color(pal.accent));
    let eq = draw_stack(pm, &items, col, y, Align::Left);

    Extras { eq, ..Default::default() }
}
