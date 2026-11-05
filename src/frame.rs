//! Per-frame work for the 60 fps loop. Everything here must be cheap:
//! integer maths only, written so the compiler can vectorise it (NEON on the Pi),
//! and only the pixels that actually change are touched.

use std::f32::consts::TAU;
use std::time::Instant;

use crate::display::Dirty;
use crate::scene::{EqSpec, Scene, Spinner};

const FADE_SECS: f32 = 0.45;
/// 33⅓ rpm in revolutions per second.
const RPM_33: f32 = 100.0 / 3.0 / 60.0;
/// Spin-up / spin-down time constant (a real platter takes ~1–2 s).
const SPIN_TAU: f32 = 0.8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

/// One pixel: a * (1 - t) + b * t, t in 0..=256 (two channels per multiply).
#[inline(always)]
fn lerp(a: u32, b: u32, t: u32) -> u32 {
    let it = 256 - t;
    let rb = ((a & 0x00FF_00FF) * it + (b & 0x00FF_00FF) * t) >> 8 & 0x00FF_00FF;
    let g = ((a & 0x0000_FF00) * it + (b & 0x0000_FF00) * t) >> 8 & 0x0000_FF00;
    rb | g
}

/// out = from * (1 - t) + to * t for whole frames, t in 0..=256.
pub fn blend(from: &[u32], to: &[u32], out: &mut [u32], t: u32) {
    let t = t.min(256);
    for ((o, &a), &b) in out.iter_mut().zip(from).zip(to) {
        *o = lerp(a, b, t);
    }
}

/// Smooth start/stop.
pub fn ease(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

pub fn eq_rect(eq: &EqSpec) -> Rect {
    Rect { x: eq.x, y: eq.y, w: eq.width(), h: eq.h }
}

/// Redraws the three "now playing" bars at time `secs`: restore the region
/// from `under`, then paint the bars. Touches only a few hundred pixels.
pub fn draw_eq(out: &mut [u32], under: &[u32], stride: usize, eq: &EqSpec, secs: f32) {
    let r = eq_rect(eq);
    let rows = out.len() / stride;
    if r.x + r.w > stride || r.y + r.h > rows || r.h == 0 {
        return;
    }
    for row in r.y..r.y + r.h {
        let i = row * stride + r.x;
        out[i..i + r.w].copy_from_slice(&under[i..i + r.w]);
    }
    const DELAYS: [f32; 3] = [0.0, 0.4, 0.75];
    for (bar, delay) in DELAYS.iter().enumerate() {
        // 1.1 s period, height between 30% and 100%, eased like a sine.
        let phase = ((secs + delay) / 1.1).fract();
        let s = 0.3 + 0.7 * (0.5 - 0.5 * (phase * TAU).cos());
        let bh = ((r.h as f32 * s).round() as usize).clamp(1, r.h);
        let x0 = r.x + bar * (eq.bar_w + eq.gap);
        for row in (r.y + r.h - bh)..(r.y + r.h) {
            let i = row * stride + x0;
            out[i..i + eq.bar_w].fill(eq.color);
        }
    }
}

pub fn spinner_rect(sp: &Spinner) -> Rect {
    Rect { x: sp.x, y: sp.y, w: sp.size, h: sp.size }
}

#[inline(always)]
fn sample(tex: &[u32], s: usize, fx: i32, fy: i32) -> u32 {
    let (x0, y0) = ((fx >> 16) as usize, (fy >> 16) as usize);
    let (x1, y1) = ((x0 + 1).min(s - 1), (y0 + 1).min(s - 1));
    let (tx, ty) = (((fx >> 8) & 0xFF) as u32, ((fy >> 8) & 0xFF) as u32);
    let top = lerp(tex[y0 * s + x0], tex[y0 * s + x1], tx);
    let bot = lerp(tex[y1 * s + x0], tex[y1 * s + x1], tx);
    lerp(top, bot, ty)
}

/// Draws the spinner's texture rotated clockwise by `angle` (radians), with
/// bilinear sampling in 16.16 fixed point. `opacity` 0..=256 fades it over
/// `under` (used during crossfades).
pub fn draw_spinner(out: &mut [u32], under: &[u32], stride: usize, sp: &Spinner, angle: f32, opacity: u32) {
    let s = sp.size;
    if s < 2 || sp.x + s > stride || sp.y + s > out.len() / stride {
        return;
    }
    let half = s as f32 / 2.0;
    let (sin, cos) = angle.sin_cos();
    const ONE: f32 = 65536.0;
    let (c, sn) = ((cos * ONE) as i32, (sin * ONE) as i32);
    let max = ((s - 1) as i32) << 16;
    let opacity = opacity.min(256);
    for j in 0..s {
        let dy = j as f32 + 0.5 - half;
        let dx0 = 0.5 - half;
        // Inverse rotation: where in the texture does this screen pixel come from?
        let mut sx = ((cos * dx0 + sin * dy + half - 0.5) * ONE) as i32;
        let mut sy = ((-sin * dx0 + cos * dy + half - 0.5) * ONE) as i32;
        let row = (sp.y + j) * stride + sp.x;
        let mrow = &sp.mask[j * s..(j + 1) * s];
        for (i, &m) in mrow.iter().enumerate() {
            if m != 0 {
                let px = sample(&sp.label, s, sx.clamp(0, max), sy.clamp(0, max));
                let o = row + i;
                out[o] = if m == 255 && opacity == 256 { px } else { lerp(under[o], px, (m as u32 * opacity) >> 8) };
            }
            sx += c;
            sy -= sn;
        }
    }
}

/// Owns the frame buffers and decides, every frame, what changed.
pub struct Compositor {
    w: usize,
    base: Vec<u32>,
    alt: Option<Vec<u32>>,
    stat: Vec<u32>,
    from: Vec<u32>,
    under: Vec<u32>,
    pub out: Vec<u32>,
    eq: Option<EqSpec>,
    spinner: Option<Spinner>,
    drift_period: f32,
    drift_q: u32,
    fade_start: Option<Instant>,
    angle: f32,
    speed: f32,
    start: Instant,
    last: Instant,
    need_full: bool,
}

impl Compositor {
    pub fn new(w: usize, h: usize) -> Self {
        let now = Instant::now();
        Compositor {
            w,
            base: vec![0; w * h],
            alt: None,
            stat: vec![0; w * h],
            from: vec![0; w * h],
            under: vec![0; w * h],
            out: vec![0; w * h],
            eq: None,
            spinner: None,
            drift_period: 20.0,
            drift_q: u32::MAX,
            fade_start: None,
            angle: 0.0,
            speed: 0.0,
            start: now,
            last: now,
            need_full: true,
        }
    }

    pub fn accepts(&self, scene: &Scene) -> bool {
        scene.pixels.len() == self.out.len()
    }

    pub fn set_scene(&mut self, scene: Scene, fade: bool) {
        if fade {
            self.from.copy_from_slice(&self.out);
            self.fade_start = Some(Instant::now());
        }
        self.base = scene.pixels;
        self.alt = scene.alt;
        self.drift_period = scene.drift_period;
        self.drift_q = u32::MAX;
        self.eq = scene.eq;
        self.spinner = scene.spinner; // angle and speed carry over: no jump
        self.need_full = true;
    }

    /// Everything must be re-sent (e.g. after the display was re-opened).
    pub fn invalidate(&mut self) {
        self.need_full = true;
    }

    /// Advances animations by the time since the last call and updates `out`.
    pub fn frame(&mut self) -> Dirty {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f32().min(0.1);
        self.last = now;
        let t = now.duration_since(self.start).as_secs_f32();

        // Platter physics: ease towards 33⅓ rpm or towards a stop.
        let target = if self.spinner.as_ref().is_some_and(|s| s.spinning) { RPM_33 } else { 0.0 };
        self.speed += (target - self.speed) * (1.0 - (-dt / SPIN_TAU).exp());
        if target == 0.0 && self.speed < 0.002 {
            self.speed = 0.0;
        }
        self.angle = (self.angle + self.speed * dt * TAU) % TAU;

        // Static layer: drift (ambient) and crossfade only redraw when the
        // quantised mix actually changes.
        let mut full = std::mem::take(&mut self.need_full);
        if let Some(alt) = &self.alt {
            let p = 0.5 - 0.5 * (TAU * t / self.drift_period).cos();
            let q = (p * 256.0).round() as u32;
            if q != self.drift_q {
                self.drift_q = q;
                blend(&self.base, alt, &mut self.stat, q);
                full = true;
            }
        }
        let fade = self.fade_start.map(|t0| now.duration_since(t0).as_secs_f32() / FADE_SECS);
        if fade.is_some() || full {
            let src: &[u32] = if self.alt.is_some() { &self.stat } else { &self.base };
            match fade {
                Some(x) => {
                    blend(&self.from, src, &mut self.under, (ease(x) * 256.0) as u32);
                    if x >= 1.0 {
                        self.fade_start = None;
                    }
                }
                None => self.under.copy_from_slice(src),
            }
            self.out.copy_from_slice(&self.under);
            full = true;
        }

        let mut rects = Vec::new();
        if let Some(eq) = &self.eq {
            draw_eq(&mut self.out, &self.under, self.w, eq, t);
            rects.push(eq_rect(eq));
        }
        if let Some(sp) = &self.spinner {
            if full || self.speed > 0.0 {
                let opacity = fade.map_or(256, |x| (ease(x) * 256.0) as u32);
                draw_spinner(&mut self.out, &self.under, self.w, sp, self.angle, opacity);
                rects.push(spinner_rect(sp));
            }
        }

        if full {
            Dirty::Full
        } else if rects.is_empty() {
            Dirty::None
        } else {
            Dirty::Rects(rects)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_endpoints_and_midpoint() {
        let a = [0x00FF_0000u32, 0x0000_0000];
        let b = [0x0000_00FFu32, 0x00FF_FFFF];
        let mut o = [0u32; 2];
        blend(&a, &b, &mut o, 0);
        assert_eq!(o, a);
        blend(&a, &b, &mut o, 256);
        assert_eq!(o, b);
        blend(&a, &b, &mut o, 128);
        assert_eq!(o, [0x007F_007F, 0x007F_7F7F]);
    }

    fn spinner(size: usize) -> Spinner {
        // Left half red, right half blue.
        let label = (0..size * size).map(|i| if i % size < size / 2 { 0x00FF_0000 } else { 0x0000_00FF }).collect();
        Spinner { x: 0, y: 0, size, label, mask: vec![255; size * size], spinning: true }
    }

    #[test]
    fn spinner_at_zero_is_identity_and_half_turn_mirrors() {
        let sp = spinner(8);
        let under = vec![0u32; 64];
        let mut out = vec![0u32; 64];
        draw_spinner(&mut out, &under, 8, &sp, 0.0, 256);
        assert_eq!(out[3 * 8 + 1], 0x00FF_0000);
        assert_eq!(out[3 * 8 + 6], 0x0000_00FF);
        draw_spinner(&mut out, &under, 8, &sp, std::f32::consts::PI, 256);
        assert_eq!(out[3 * 8 + 1], 0x0000_00FF);
        assert_eq!(out[3 * 8 + 6], 0x00FF_0000);
    }

    #[test]
    fn quarter_turn_is_clockwise() {
        // After 90° clockwise, the left (red) half points up.
        let sp = spinner(8);
        let under = vec![0u32; 64];
        let mut out = vec![0u32; 64];
        draw_spinner(&mut out, &under, 8, &sp, std::f32::consts::FRAC_PI_2, 256);
        assert_eq!(out[1 * 8 + 4], 0x00FF_0000, "top should be red");
        assert_eq!(out[6 * 8 + 4], 0x0000_00FF, "bottom should be blue");
    }
}
