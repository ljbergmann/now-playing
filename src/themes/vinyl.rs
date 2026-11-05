//! Vinyl: a record on a dark platter with the cover as its centre label.
//! Playing: the tonearm sits in the groove and the label turns at 33⅓ rpm.
//! Idle: the arm rests, the record spins down, a big clock takes the text side.
//!
//! A plain black record: only the label rotates per frame (grooves and
//! highlights look the same at every angle). Your own coloured pressing from
//! VinylVault: the whole record rotates under a fixed reflection/arm layer.


use tiny_skia::{Color, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::draw::{self, Align, DIM, FAINT, WHITE, clamp};
use crate::scene::{Ctx, Extras, Item, Spinner, draw_stack, size_to_width, stack_height};
use crate::themes::pressing;
use crate::vinylvault::{Finish, Pressing};

pub fn render(pm: &mut Pixmap, c: &Ctx) -> Extras {
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let vmin = w.min(h) / 100.0;
    let pad = clamp(14.0, 5.0 * vmin, 60.0);
    let art = c.track.and_then(|t| t.art.as_deref());
    let pal = draw::palette(art);
    let stacked = w < h * 1.2;

    // Background: near-black, faintly tinted by the cover.
    let p = draw::Prof::new("vinyl background");
    pm.fill(draw::color([10, 9, 9]));
    if let Some(a) = art {
        draw::blit(pm, &draw::blurred(a, w as u32, h as u32, 8.0), 0, 0);
        draw::shade(pm, [9, 8, 8], 214.0 / 255.0);
    }
    drop(p);

    // Geometry: record left (or top), text right (or below). The arm needs
    // ~0.2 d to the right of the record.
    let (d, cx, cy, col, align) = if stacked {
        let d = ((w - 2.0 * pad) / 1.3).min(h * 0.5);
        let cx = (w - 1.22 * d) / 2.0 + d / 2.0;
        let cy = pad + d / 2.0;
        (d, cx, cy, (pad, w - 2.0 * pad, pad + d + pad * 1.2, h - pad), Align::Center)
    } else {
        let d = (h - 2.0 * pad).min(w * 0.5);
        let tx = pad + d + 0.24 * d;
        (d, pad + d / 2.0, h / 2.0, (tx, w - tx - pad, pad, h - pad), Align::Left)
    };
    let r = d / 2.0;

    // Platter shadow.
    if let Some(p) = disc(cx, cy + 0.02 * d, r * 1.01) {
        draw::fill(pm, &p, Color::from_rgba8(0, 0, 0, 120));
    }

    // The tonearm on its own layer: it must stay on top of a spinning record.
    let p = draw::Prof::new("tonearm");
    let mut arm = Pixmap::new(pm.width(), pm.height()).expect("non-zero size");
    draw_tonearm(&mut arm, cx, cy, d, c.playing);
    drop(p);
    let p = draw::Prof::new("record");

    let mut spinner = match c.pressing {
        // Your pressing: the whole record turns (a marble or splatter visibly
        // rotates); reflections and arm sit on a fixed overlay.
        Some(p) => {
            let size = d.round() as usize;
            let x = (cx - size as f32 / 2.0).round().max(0.0) as usize;
            let y = (cy - size as f32 / 2.0).round().max(0.0) as usize;
            let disc = pressing::disc(p, art, size, 0.34);
            let strength = if p.finish == Finish::Clear { 0.5 } else { 0.3 };
            let overlay = pressing::overlay(size, (x, y), &arm, strength);
            Spinner { x, y, size, label: disc.texture, mask: disc.mask, overlay: Some(overlay), spinning: false, texture_alpha: disc.texture_alpha }
        }
        // Plain black: grooves look the same at every angle, so only the label turns.
        None => {
            draw_record(pm, cx, cy, r);
            label(art, cx, cy, r * 0.34, pal.accent)
        }
    };
    spinner.spinning = c.playing;
    // Draw the arm again straight onto the frame (cheaper than compositing the layer).
    draw_tonearm(pm, cx, cy, d, c.playing);
    drop(p);
    let _p = draw::Prof::new("text");

    // Text side.
    let (cx0, cw, top, bottom) = col;
    let f = c.fonts;
    let mut items = Vec::new();
    if c.playing {
        let t = c.track.expect("playing implies a track");
        let title_px = clamp(20.0, 7.2 * vmin, 76.0);
        let (title_px, lines) = draw::fit(&f.bold, &t.title, cw, 2, title_px, title_px * 0.7, -0.01);
        items.push(Item::new(&f.bold, clamp(10.0, 2.2 * vmin, 18.0), "NOW PLAYING", draw::rgba(pal.accent, 1.0)).track(0.16).eq(true));
        for (i, l) in lines.iter().enumerate() {
            items.push(Item::new(&f.bold, title_px, l.clone(), WHITE).track(-0.01).gap(if i == 0 { 0.5 * title_px } else { -0.12 * title_px }));
        }
        items.push(Item::new(&f.regular, clamp(14.0, 4.2 * vmin, 40.0), t.artist.clone(), DIM).gap(0.2 * title_px));
        if !t.album.is_empty() {
            items.push(Item::new(&f.regular, clamp(11.0, 2.8 * vmin, 26.0), t.album.clone(), FAINT).gap(4.0));
        }
        if let Some(p) = c.pressing {
            let tint = brightest(p).unwrap_or(pal.accent);
            items.push(
                Item::new(&f.bold, clamp(10.0, 2.2 * vmin, 18.0), format!("{} · {}", if p.owned { "YOUR PRESSING" } else { "ON VINYL" }, p.caption().to_uppercase()), draw::rgba(tint, 1.0))
                    .track(0.14)
                    .gap(0.35 * title_px),
            );
            if c.promo {
                let via = if p.owned { "VIA YOUR VINYLVAULT SHELF" } else { "VIA VINYLVAULT" };
                items.push(Item::new(&f.bold, clamp(9.0, 1.7 * vmin, 14.0), via, FAINT).track(0.16).gap(2.0));
            }
        }
    } else {
        let clk = draw::clock(c.now, c.twelve_hour);
        let clock_px = size_to_width(&f.light, "88:88", cw * 0.98, h * if stacked { 0.2 } else { 0.32 }, -0.02);
        items.push(Item::new(&f.light, clock_px, clk.time, WHITE).track(-0.02));
        items.push(Item::new(&f.bold, clamp(10.0, 2.4 * vmin, 22.0), clk.date.to_uppercase(), DIM).track(0.18).gap(-0.05 * clock_px));
        if let Some(t) = c.track {
            let small = clamp(10.0, 2.2 * vmin, 18.0);
            let when = t.played_at.map(|at| format!("LAST PLAYED · {}", draw::ago(c.now, at).to_uppercase())).unwrap_or("LAST PLAYED".into());
            items.push(Item::new(&f.bold, small, when, draw::rgba(pal.accent, 0.9)).track(0.16).gap(0.12 * clock_px));
            items.push(Item::new(&f.regular, clamp(13.0, 3.4 * vmin, 30.0), format!("{} — {}", t.title, t.artist), DIM).gap(2.0));
        }
    }
    let block = stack_height(&items);
    let y = if stacked { top + ((bottom - top) - block).max(0.0) / 2.0 } else { (h - block) / 2.0 };
    let eq = draw_stack(pm, &items, (cx0, cw), y, align);

    Extras { eq, spinner: Some(spinner), ..Default::default() }
}

/// The pressing's most vivid colour, lifted so it reads on a dark background.
/// Falls back to a light colour, then to the cover's accent (black/white records).
fn brightest(p: &Pressing) -> Option<[u8; 3]> {
    let lum = |c: &[u8; 3]| 0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32;
    let sat = |c: &[u8; 3]| {
        let (mx, mn) = (*c.iter().max().unwrap() as f32, *c.iter().min().unwrap() as f32);
        if mx > 0.0 { (mx - mn) / mx } else { 0.0 }
    };
    let c = p
        .colors
        .iter()
        .filter(|c| lum(c) >= 40.0)
        .max_by(|a, b| (sat(a) + lum(a) / 1000.0).total_cmp(&(sat(b) + lum(b) / 1000.0)))?;
    let k = (150.0 / lum(c)).max(1.0);
    Some(c.map(|v| (v as f32 * k).min(255.0) as u8))
}

fn disc(cx: f32, cy: f32, r: f32) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    pb.push_circle(cx, cy, r);
    pb.finish()
}

/// Black vinyl: fine grooves, a few track gaps, a glossy rim and two fixed
/// light reflections that make it read as a shiny record.
fn draw_record(pm: &mut Pixmap, cx: f32, cy: f32, r: f32) {
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let x0 = ((cx - r - 1.0).floor() as i32).max(0);
    let x1 = ((cx + r + 1.0).ceil() as i32).min(pw);
    let y0 = ((cy - r - 1.0).floor() as i32).max(0);
    let y1 = ((cy + r + 1.0).ceil() as i32).min(ph);
    let gaps = [0.52, 0.64, 0.75, 0.86];
    draw::par_rows(pm.data_mut(), pw as usize * 4, |y, row| {
        let y = y as i32;
        if y < y0 || y >= y1 {
            return;
        }
        for x in x0..x1 {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let dist = (dx * dx + dy * dy).sqrt();
            let cov = (r + 0.5 - dist).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            let rn = dist / r;
            let mut l = 17.0f32;
            let grooved = rn > 0.38 && rn < 0.965;
            if grooved {
                // Fine grooves + slightly lighter gaps between "tracks".
                l += 2.2 * (dist * 2.1).sin() + 1.2 * (dist * 0.37).sin();
                if gaps.iter().any(|g| (rn - g).abs() < 0.006) {
                    l += 7.0;
                }
            } else if rn >= 0.965 {
                l = 24.0 + 10.0 * ((rn - 0.965) / 0.035); // rounded edge catching light
            } else {
                l = 13.0; // smooth run-out around the label
            }
            // Two reflections at fixed angles (the room light), strongest on the grooves.
            let sheen = draw::sheen(dx, dy);
            if grooved || rn >= 0.965 {
                l += sheen * 46.0 * (0.75 + 0.25 * (dist * 2.1).sin().abs());
            } else {
                l += sheen * 14.0;
            }
            let (cr, cg, cb) = (l * 1.0, l * 0.985, l * 0.97);
            let i = (x * 4) as usize;
            for (k, v) in [cr, cg, cb].into_iter().enumerate() {
                let dst = row[i + k] as f32;
                row[i + k] = (v.clamp(0.0, 255.0) * cov + dst * (1.0 - cov)) as u8;
            }
        }
    });
}

/// The cover as a round label with a spindle hole, ready to rotate.
fn label(art: Option<&image::RgbaImage>, cx: f32, cy: f32, r: f32, accent: [u8; 3]) -> Spinner {
    let size = (2.0 * r).round().max(8.0) as usize;
    let x = (cx - size as f32 / 2.0).round().max(0.0) as usize;
    let y = (cy - size as f32 / 2.0).round().max(0.0) as usize;
    let tex = art.map(|a| draw::square(a, size as u32));
    let half = size as f32 / 2.0;
    let hole = half * 0.075;
    let mut label = vec![0u32; size * size];
    let mut mask = vec![0u8; size * size];
    for j in 0..size {
        for i in 0..size {
            let (dx, dy) = (i as f32 + 0.5 - half, j as f32 + 0.5 - half);
            let d = (dx * dx + dy * dy).sqrt();
            mask[j * size + i] = ((half - d + 0.5).clamp(0.0, 1.0) * 255.0) as u8;
            let mut c = match &tex {
                Some(t) => {
                    let p = t.get_pixel(i as u32, j as u32);
                    [p[0] as f32, p[1] as f32, p[2] as f32]
                }
                None => {
                    // Plain label in the accent colour with a lighter inner ring.
                    let k = if d < half * 0.55 && d > half * 0.5 { 1.15 } else { 0.8 };
                    accent.map(|v| v as f32 * k)
                }
            };
            // Slight darkening towards the label edge (printed paper on vinyl).
            let edge = ((d / half - 0.9) / 0.1).clamp(0.0, 1.0);
            c = c.map(|v| v * (1.0 - 0.35 * edge));
            // Spindle hole with a soft rim.
            let hole_cov = (hole - d + 0.5).clamp(0.0, 1.0);
            c = c.map(|v| v * (1.0 - hole_cov) + 6.0 * hole_cov);
            label[j * size + i] = draw::xrgb(c.map(|v| v.clamp(0.0, 255.0) as u8));
        }
    }
    Spinner { x, y, size, label, mask, overlay: None, spinning: false, texture_alpha: false }
}

/// Pivot top-right of the record; stylus in the groove when playing, on the
/// rest beside the record when idle.
fn draw_tonearm(pm: &mut Pixmap, cx: f32, cy: f32, d: f32, playing: bool) {
    let pivot = (cx + 0.56 * d, cy - 0.40 * d);
    let groove = (cx + 0.23 * d, cy + 0.26 * d);
    let len = ((groove.0 - pivot.0).powi(2) + (groove.1 - pivot.1).powi(2)).sqrt();
    let stylus = if playing { groove } else { (pivot.0 + 0.02 * d, pivot.1 + len) };
    let dir = ((stylus.0 - pivot.0) / len, (stylus.1 - pivot.1) / len);
    let head = (stylus.0 - dir.0 * 0.075 * d, stylus.1 - dir.1 * 0.075 * d);
    let back = (pivot.0 - dir.0 * 0.11 * d, pivot.1 - dir.1 * 0.11 * d);

    let line = |pm: &mut Pixmap, a: (f32, f32), b: (f32, f32), width: f32, c: Color| {
        let mut pb = PathBuilder::new();
        pb.move_to(a.0, a.1);
        pb.line_to(b.0, b.1);
        if let Some(p) = pb.finish() {
            let mut paint = Paint::default();
            paint.set_color(c);
            paint.anti_alias = true;
            let stroke = Stroke { width, line_cap: LineCap::Round, ..Stroke::default() };
            pm.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
        }
    };
    let circle = |pm: &mut Pixmap, (x, y): (f32, f32), r: f32, c: Color| {
        if let Some(p) = disc(x, y, r) {
            draw::fill(pm, &p, c);
        }
    };

    // Rest post (where the arm sits when idle).
    circle(pm, (pivot.0 + 0.02 * d, pivot.1 + len * 0.82), 0.022 * d, Color::from_rgba8(38, 38, 40, 255));
    // Shadow of the arm on the record.
    let off = (0.012 * d, 0.02 * d);
    line(pm, (pivot.0 + off.0, pivot.1 + off.1), (head.0 + off.0, head.1 + off.1), 0.02 * d, Color::from_rgba8(0, 0, 0, 110));
    // Base plate and counterweight.
    circle(pm, pivot, 0.08 * d, Color::from_rgba8(30, 30, 32, 255));
    circle(pm, pivot, 0.066 * d, Color::from_rgba8(52, 52, 56, 255));
    line(pm, pivot, back, 0.055 * d, Color::from_rgba8(70, 70, 75, 255));
    // Arm tube with a highlight, then the headshell.
    line(pm, pivot, head, 0.018 * d, Color::from_rgba8(176, 176, 182, 255));
    line(pm, pivot, head, 0.006 * d, Color::from_rgba8(232, 232, 236, 255));
    line(pm, head, stylus, 0.045 * d, Color::from_rgba8(120, 120, 126, 255));
    // Finger lift.
    let lift = (stylus.0 + dir.1 * 0.035 * d, stylus.1 - dir.0 * 0.035 * d);
    line(pm, (head.0 + (stylus.0 - head.0) * 0.5, head.1 + (stylus.1 - head.1) * 0.5), lift, 0.008 * d, Color::from_rgba8(150, 150, 156, 255));
    circle(pm, pivot, 0.03 * d, Color::from_rgba8(160, 160, 166, 255));
}
