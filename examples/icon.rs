//! Draws the app icon: a record on a dark macOS-style tile, a warm label
//! with three "now playing" bars, and a tonearm in the groove.
//!
//!     cargo run --release --example icon -- icon.png [size]
//!
//! Plain per-pixel shading with 4×4 supersampling, so no assets are needed.

const S: f32 = 1024.0; // design grid (Apple's: 824 px tile centred on 1024)

type Rgba = [f32; 4];

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn hex(c: u32) -> [f32; 3] {
    [((c >> 16) & 255) as f32 / 255.0, ((c >> 8) & 255) as f32 / 255.0, (c & 255) as f32 / 255.0]
}

/// Paint `c` with coverage `a` over `dst`.
fn over(dst: &mut [f32; 3], c: [f32; 3], a: f32) {
    *dst = mix(*dst, c, a.clamp(0.0, 1.0));
}

/// Distance from p to segment ab.
fn seg(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    let (bx, by) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * bx + (p.1 - a.1) * by) / (bx * bx + by * by)).clamp(0.0, 1.0);
    let (dx, dy) = (p.0 - a.0 - bx * t, p.1 - a.1 - by * t);
    ((dx * dx + dy * dy).sqrt(), t)
}

// Layout on the 1024 grid.
const TILE: (f32, f32, f32) = (512.0, 512.0, 412.0); // centre, half size
const REC: (f32, f32) = (462.0, 548.0);
const R: f32 = 318.0;
const LABEL: f32 = 112.0;
const PIVOT: (f32, f32) = (812.0, 236.0);
const ELBOW: (f32, f32) = (790.0, 548.0);
const HEAD: (f32, f32) = (706.0, 652.0);

fn sample(x: f32, y: f32) -> Rgba {
    // Squircle tile (superellipse, n = 5).
    let (u, v) = ((x - TILE.0) / TILE.2, (y - TILE.1) / TILE.2);
    if u.abs().powf(5.0) + v.abs().powf(5.0) > 1.0 {
        return [0.0; 4];
    }
    // Background: warm near-black, a little lighter at the top.
    let mut c = mix(hex(0x2a2530), hex(0x0c0b0e), smooth(100.0, 924.0, y));
    // Faint warm light behind the record.
    let gl = ((x - REC.0).hypot(y - REC.1) - R).max(0.0);
    over(&mut c, hex(0xff7a45), 0.10 * (1.0 - smooth(0.0, 220.0, gl)));

    // Record shadow.
    let ds = (x - REC.0 - 10.0).hypot(y - REC.1 - 26.0);
    over(&mut c, [0.0; 3], 0.55 * (1.0 - smooth(R - 30.0, R + 40.0, ds)));

    let d = (x - REC.0).hypot(y - REC.1);
    if d < R {
        let mut r = hex(0x141316);
        if d > LABEL + 14.0 && d < R - 10.0 {
            // Grooves, with a couple of gaps between "tracks".
            let g = 0.5 + 0.5 * (d * 1.15).sin();
            let gap = [178.0, 236.0].iter().any(|&t| (d - t).abs() < 3.0);
            r = mix(r, hex(0x2b2a2f), if gap { 0.0 } else { 0.35 * g });
            // Light catching the grooves: two opposite wedges.
            let a = (y - REC.1).atan2(x - REC.0);
            let s = (2.0 * (a + 0.75)).cos().max(0.0).powf(5.0);
            r = mix(r, hex(0xb9b3c2), 0.42 * s * (0.6 + 0.4 * g));
        }
        // Lip of the record.
        r = mix(r, hex(0x3a383f), smooth(R - 7.0, R - 1.0, d) * 0.9);
        let edge = 1.0 - smooth(R - 1.0, R, d);
        over(&mut c, r, edge);

        if d < LABEL {
            // Label: amber → coral, slightly darker toward the edge.
            let t = smooth(REC.1 - LABEL, REC.1 + LABEL, y + (x - REC.0) * 0.35);
            let mut l = mix(hex(0xffb347), hex(0xff4f6d), t);
            l = mix(l, l.map(|v| v * 0.82), smooth(LABEL - 40.0, LABEL, d));
            // Three "now playing" bars.
            for (i, hgt) in [64.0f32, 104.0, 46.0].iter().enumerate() {
                let bx = REC.0 - 34.0 + 34.0 * i as f32;
                let (dd, _) = seg((x, y), (bx, REC.1 + 52.0 - hgt), (bx, REC.1 + 52.0));
                l = mix(l, hex(0xfff6ee), 1.0 - smooth(10.0, 11.5, dd));
            }
            over(&mut c, l, 1.0 - smooth(LABEL - 1.0, LABEL, d));
        }
    }

    // Tonearm: shadow, arm, headshell, pivot.
    let p = (x, y);
    let sh = (x - 12.0, y - 18.0);
    let arm_sh = seg(sh, PIVOT, ELBOW).0.min(seg(sh, ELBOW, HEAD).0);
    over(&mut c, [0.0; 3], 0.45 * (1.0 - smooth(8.0, 30.0, arm_sh)));
    for (a, b) in [(PIVOT, ELBOW), (ELBOW, HEAD)] {
        let (dd, _) = seg(p, a, b);
        // Cylinder shading across the arm.
        let n = (dd / 11.0).clamp(0.0, 1.0);
        let metal = mix(hex(0xf1eff4), hex(0x8d8a95), n);
        over(&mut c, metal, 1.0 - smooth(10.0, 11.5, dd));
    }
    let hs = seg(p, (HEAD.0 + 28.0, HEAD.1 - 34.0), (HEAD.0 - 18.0, HEAD.1 + 20.0)).0;
    over(&mut c, mix(hex(0xd9d6df), hex(0x6e6b77), smooth(0.0, 22.0, hs)), 1.0 - smooth(21.0, 22.5, hs));
    let dp = (x - PIVOT.0).hypot(y - PIVOT.1);
    over(&mut c, [0.0; 3], 0.4 * (1.0 - smooth(44.0, 70.0, (x - PIVOT.0 - 8.0).hypot(y - PIVOT.1 - 14.0))));
    over(&mut c, mix(hex(0xe8e5ee), hex(0x77737f), smooth(0.0, 48.0, dp + (y - PIVOT.1) * 0.4)), 1.0 - smooth(46.0, 47.5, dp));
    over(&mut c, hex(0x2a282e), 1.0 - smooth(17.0, 18.5, dp));

    [c[0], c[1], c[2], 1.0]
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "icon.png".into());
    let size: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1024);
    let k = S / size as f32;
    const SS: u32 = 4;
    let img = image::RgbaImage::from_fn(size, size, |px, py| {
        let mut acc = [0.0f32; 4];
        for sy in 0..SS {
            for sx in 0..SS {
                let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) * k;
                let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) * k;
                let s = sample(x, y);
                // Premultiplied accumulation.
                for i in 0..3 {
                    acc[i] += s[i] * s[3];
                }
                acc[3] += s[3];
            }
        }
        let n = (SS * SS) as f32;
        let a = acc[3] / n;
        let un = |v: f32| if a > 0.0 { ((v / n / a).clamp(0.0, 1.0) * 255.0).round() as u8 } else { 0 };
        image::Rgba([un(acc[0]), un(acc[1]), un(acc[2]), (a * 255.0).round() as u8])
    });
    img.save(&out).expect("write png");
    eprintln!("wrote {out} ({size}×{size})");
}
