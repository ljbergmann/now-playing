//! Coloured vinyl: turns a VinylVault pressing (finish + colours) into a
//! record texture that the 60 fps loop rotates, plus the fixed layer on top
//! (light reflections, tonearm) that must *not* rotate with it.
//!
//! Runs on the compose thread, once per track. Patterns are seeded from the
//! pressing's name, so the same record always looks the same.

use std::f32::consts::{PI, TAU};

use image::RgbaImage;
use tiny_skia::Pixmap;

use crate::draw;
use crate::vinylvault::{Finish, Pressing};

// --- small noise toolkit -----------------------------------------------------

fn hash(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(374_761_393) ^ (y as u32).wrapping_mul(668_265_263) ^ seed.wrapping_mul(2_246_822_519);
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    (h ^ (h >> 16)) as f32 / u32::MAX as f32
}

fn vnoise(x: f32, y: f32, seed: u32) -> f32 {
    let (xi, yi) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let a = hash(xi, yi, seed) + (hash(xi + 1, yi, seed) - hash(xi, yi, seed)) * sx;
    let b = hash(xi, yi + 1, seed) + (hash(xi + 1, yi + 1, seed) - hash(xi, yi + 1, seed)) * sx;
    a + (b - a) * sy
}

fn fbm(x: f32, y: f32, seed: u32) -> f32 {
    let (mut v, mut amp, mut f) = (0.0, 0.5, 1.0);
    for o in 0..4 {
        v += amp * vnoise(x * f, y * f, seed.wrapping_add(o * 101));
        amp *= 0.5;
        f *= 2.03;
    }
    v / 0.9375
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

type C = [f32; 3];

fn mix(a: C, b: C, t: f32) -> C {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// Walks through the colours as t goes 0→1, with soft-but-visible edges.
fn bands(cs: &[C], t: f32, sharp: f32) -> C {
    let n = cs.len();
    let pos = t.rem_euclid(1.0) * n as f32;
    let i = pos.floor() as usize % n;
    let f = pos - pos.floor();
    let w = 0.5 / sharp;
    mix(cs[i], cs[(i + 1) % n], smooth(0.5 - w, 0.5 + w, f))
}

/// The colours to work with, filled in sensibly when the catalogue gave too few.
fn palette(p: &Pressing) -> Vec<C> {
    let mut cs: Vec<C> = p.colors.iter().map(|c| [c[0] as f32, c[1] as f32, c[2] as f32]).collect();
    if cs.is_empty() {
        cs.push(match p.finish {
            Finish::Clear => [224.0, 234.0, 240.0],
            Finish::Glow => [184.0, 250.0, 153.0],
            _ => [26.0, 26.0, 28.0],
        });
    }
    let two_tone = matches!(p.finish, Finish::Marbled | Finish::Swirl | Finish::HalfAndHalf | Finish::Galaxy);
    if cs.len() == 1 && two_tone {
        let c = cs[0];
        let lum = 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
        cs.push(if lum > 140.0 { mix(c, [20.0, 20.0, 22.0], 0.75) } else { mix(c, [245.0, 245.0, 240.0], 0.7) });
    }
    cs
}

/// The two fixed light reflections (shared with the black record).
pub fn sheen(dx: f32, dy: f32) -> f32 {
    let a = dy.atan2(dx);
    (2.0 * (a + PI * 0.25)).cos().max(0.0).powf(10.0) + (2.0 * (a - PI * 0.15)).cos().max(0.0).powf(30.0) * 0.5
}

pub struct Disc {
    pub texture: Vec<u32>,
    pub mask: Vec<u8>,
}

/// The whole record as a size×size texture: pattern, grooves, rim, label, hole.
pub fn disc(p: &Pressing, art: Option<&RgbaImage>, size: usize, label_frac: f32) -> Disc {
    let seed = p.name.bytes().fold(2_166_136_261u32, |h, b| (h ^ b as u32).wrapping_mul(16_777_619));
    let cs = palette(p);
    let half = size as f32 / 2.0;
    let cover = art.map(|a| draw::square(a, size as u32));
    let label_px = (2.0 * half * label_frac).round().max(4.0) as u32;
    let label = art.map(|a| draw::square(a, label_px));

    // Splatter: a fixed set of blobs, mostly small, a few big.
    let splat_colors: Vec<C> = if cs.len() > 1 { cs[1..].to_vec() } else { vec![[18.0, 18.0, 20.0], [244.0, 244.0, 240.0]] };
    let splats: Vec<(f32, f32, f32, C)> = (0..110)
        .map(|k| {
            let (a, r) = (hash(k, 1, seed) * TAU, hash(k, 2, seed).sqrt() * 0.95);
            let rad = 0.008 + hash(k, 3, seed).powf(3.0) * 0.06;
            (r * a.cos(), r * a.sin(), rad, splat_colors[k as usize % splat_colors.len()])
        })
        .collect();

    let mut texture = vec![0u32; size * size];
    let mut mask = vec![0u8; size * size];
    for j in 0..size {
        for i in 0..size {
            let (dx, dy) = (i as f32 + 0.5 - half, j as f32 + 0.5 - half);
            let d = (dx * dx + dy * dy).sqrt();
            let cov = (half - d + 0.5).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            let (u, v) = (dx / half, dy / half); // -1..1 across the record
            let rn = d / half;

            // 1. The plastic.
            let mut c = match p.finish {
                Finish::Solid | Finish::Glow => cs[0],
                Finish::Clear => cs[0],
                Finish::HalfAndHalf => mix(cs[0], cs[1], smooth(-0.004, 0.004, u)),
                Finish::Marbled => {
                    let (x, y) = (u * 2.2, v * 2.2);
                    let (qx, qy) = (fbm(x, y, seed), fbm(x + 5.2, y + 1.3, seed));
                    let t = fbm(x + 3.5 * qx, y + 3.5 * qy, seed ^ 0x9E37);
                    bands(&cs, t * 1.6, 3.0)
                }
                Finish::Swirl => {
                    let a = v.atan2(u) / TAU;
                    let t = a * 2.0 + rn * 1.7 + 0.35 * fbm(u * 2.0, v * 2.0, seed);
                    bands(&cs, t, 2.5)
                }
                Finish::Galaxy => {
                    let base = mix(cs[0], [6.0, 6.0, 10.0], 0.55);
                    let cloud = fbm(u * 2.4 + 0.6 * fbm(u * 3.0, v * 3.0, seed), v * 2.4, seed ^ 0x51);
                    let mut g = mix(base, cs[1 % cs.len()], smooth(0.48, 0.82, cloud) * 0.85);
                    if hash(i as i32, j as i32, seed ^ 0xABCD) > 0.9965 {
                        g = mix(g, [255.0, 255.0, 250.0], 0.85);
                    }
                    g
                }
                Finish::Splatter => {
                    let mut g = cs[0];
                    for &(sx, sy, r, col) in &splats {
                        let (ex, ey) = (u - sx, v - sy);
                        if ex.abs() > r * 1.5 || ey.abs() > r * 1.5 {
                            continue;
                        }
                        let rough = r * (0.75 + 0.5 * vnoise(u * 60.0, v * 60.0, seed ^ 0x77));
                        let e = (ex * ex + ey * ey).sqrt();
                        g = mix(g, col, smooth(rough + 0.004, rough - 0.004, e));
                    }
                    g
                }
                Finish::PictureDisc => match &cover {
                    Some(img) => {
                        let px = img.get_pixel(i as u32, j as u32);
                        [px[0] as f32, px[1] as f32, px[2] as f32]
                    }
                    None => cs[0],
                },
            };

            // 2. Grooves, track gaps, rim, smooth run-out by the label.
            let grooved = rn > label_frac + 0.04 && rn < 0.965;
            let depth = match p.finish {
                Finish::PictureDisc => 0.025,
                Finish::Clear => 0.10,
                _ => 0.06,
            };
            let mut shade = 1.0;
            if grooved && p.finish == Finish::Clear {
                // Clear vinyl: grooves catch light rather than darken.
                shade += 0.25 * (0.5 + 0.5 * (d * 2.1).sin());
            } else if grooved {
                shade -= depth * (0.5 + 0.5 * (d * 2.1).sin());
                if [0.52, 0.64, 0.75, 0.86].iter().any(|g| (rn - g).abs() < 0.006) {
                    shade -= 0.10;
                }
            } else if rn >= 0.965 {
                shade = 1.12;
            }
            if p.finish == Finish::Glow {
                shade *= 1.08;
            }
            c = c.map(|x| x * shade);

            // 3. Label + spindle hole (a picture disc has no separate label).
            let mut alpha = cov;
            if p.finish == Finish::Clear && rn > label_frac {
                // Mostly see-through; the rounded edge and track gaps show more.
                let gap = [0.52, 0.64, 0.75, 0.86].iter().any(|g| (rn - g).abs() < 0.006);
                alpha *= if rn >= 0.965 { 0.6 } else if gap { 0.22 } else { 0.07 + 0.07 * (0.5 + 0.5 * (d * 2.1).sin()) };
            }
            if p.finish != Finish::PictureDisc && rn <= label_frac {
                if let Some(l) = &label {
                    let lx = ((u / label_frac * 0.5 + 0.5) * label_px as f32).clamp(0.0, label_px as f32 - 1.0) as u32;
                    let ly = ((v / label_frac * 0.5 + 0.5) * label_px as f32).clamp(0.0, label_px as f32 - 1.0) as u32;
                    let px = l.get_pixel(lx, ly);
                    let edge = ((rn / label_frac - 0.9) / 0.1).clamp(0.0, 1.0);
                    let lc = [px[0] as f32, px[1] as f32, px[2] as f32].map(|x| x * (1.0 - 0.35 * edge));
                    let lcov = (label_frac * half - d + 0.5).clamp(0.0, 1.0);
                    c = mix(c, lc, lcov);
                    alpha = alpha.max(lcov);
                }
            }
            let hole = (label_frac * half * 0.075 - d + 0.5).clamp(0.0, 1.0);
            c = mix(c, [6.0, 6.0, 6.0], hole);

            texture[j * size + i] = draw::xrgb(c.map(|x| x.clamp(0.0, 255.0) as u8));
            mask[j * size + i] = (alpha * 255.0) as u8;
        }
    }
    Disc { texture, mask }
}

/// The fixed layer over the spinning record: reflections, then whatever is
/// drawn in `top` (the tonearm and its shadow), as ARGB.
pub fn overlay(size: usize, at: (usize, usize), top: &Pixmap, strength: f32) -> Vec<u32> {
    let half = size as f32 / 2.0;
    let (tw, th) = (top.width() as usize, top.height() as usize);
    let tdata = top.data();
    let mut out = vec![0u32; size * size];
    for j in 0..size {
        for i in 0..size {
            let (dx, dy) = (i as f32 + 0.5 - half, j as f32 + 0.5 - half);
            let rn = (dx * dx + dy * dy).sqrt() / half;
            // Reflection: premultiplied white.
            let s = if rn <= 1.0 { (sheen(dx, dy) * strength).min(0.6) } else { 0.0 };
            let mut p = [255.0 * s, 255.0 * s, 255.0 * s, s];
            // Arm and shadow on top (premultiplied "over").
            let (x, y) = (at.0 + i, at.1 + j);
            if x < tw && y < th {
                let k = (y * tw + x) * 4;
                let ta = tdata[k + 3] as f32 / 255.0;
                if ta > 0.0 {
                    for ch in 0..3 {
                        p[ch] = tdata[k + ch] as f32 + p[ch] * (1.0 - ta);
                    }
                    p[3] = ta + p[3] * (1.0 - ta);
                }
            }
            if p[3] > 0.0 {
                let a = p[3];
                let c = [p[0] / a, p[1] / a, p[2] / a].map(|v| v.clamp(0.0, 255.0) as u32);
                out[j * size + i] = ((a * 255.0) as u32) << 24 | c[0] << 16 | c[1] << 8 | c[2];
            }
        }
    }
    out
}
