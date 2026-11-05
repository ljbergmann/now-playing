//! Per-frame work for the 60 fps loop. Everything here must be cheap:
//! integer math only, written so the compiler can vectorise it (NEON on the Pi).

use crate::scene::EqSpec;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

/// out = from * (1 - t) + to * t, with t in 0..=256. Two channels per multiply
/// (the classic 0x00FF00FF trick), so no per-byte unpacking.
pub fn blend(from: &[u32], to: &[u32], out: &mut [u32], t: u32) {
    let t = t.min(256);
    let it = 256 - t;
    for ((o, &a), &b) in out.iter_mut().zip(from).zip(to) {
        let rb = ((a & 0x00FF_00FF) * it + (b & 0x00FF_00FF) * t) >> 8 & 0x00FF_00FF;
        let g = ((a & 0x0000_FF00) * it + (b & 0x0000_FF00) * t) >> 8 & 0x0000_FF00;
        *o = rb | g;
    }
}

/// Smooth start/stop for the crossfade.
pub fn ease(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

pub fn eq_rect(eq: &EqSpec) -> Rect {
    Rect { x: eq.x, y: eq.y, w: eq.width(), h: eq.h }
}

/// Redraws the three "now playing" bars at time `secs`: restore the region
/// from the static frame, then paint the bars. Touches only a few hundred pixels.
pub fn draw_eq(out: &mut [u32], base: &[u32], stride: usize, eq: &EqSpec, secs: f32) {
    let r = eq_rect(eq);
    let rows = out.len() / stride;
    if r.x + r.w > stride || r.y + r.h > rows {
        return;
    }
    for row in r.y..r.y + r.h {
        let i = row * stride + r.x;
        out[i..i + r.w].copy_from_slice(&base[i..i + r.w]);
    }
    const DELAYS: [f32; 3] = [0.0, 0.4, 0.75];
    for (bar, delay) in DELAYS.iter().enumerate() {
        // 1.1 s period, height between 30% and 100%, eased like a sine.
        let phase = ((secs + delay) / 1.1).fract();
        let s = 0.3 + 0.7 * (0.5 - 0.5 * (phase * std::f32::consts::TAU).cos());
        let bh = ((r.h as f32 * s).round() as usize).clamp(1, r.h);
        let x0 = r.x + bar * (eq.bar_w + eq.gap);
        for row in (r.y + r.h - bh)..(r.y + r.h) {
            let i = row * stride + x0;
            out[i..i + eq.bar_w].fill(eq.color);
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
}
