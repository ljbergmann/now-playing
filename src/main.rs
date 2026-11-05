//! now-playing: a full-screen "now playing" display for the Raspberry Pi,
//! fed by Last.fm.
//!
//! Threads:
//!   poller  — asks Last.fm every few seconds, downloads cover art
//!   compose — renders a complete static frame whenever something changes
//!   main    — 60 fps loop: crossfades between frames, animates the
//!             equalizer, hands only changed pixels to the display, and
//!             follows screen changes (plug/unplug, resolution, window resize)

mod display;
mod fonts;
mod frame;
mod lastfm;
mod scene;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use display::{Dirty, Status};
use scene::{Scene, View};

const FADE: Duration = Duration::from_millis(450);

static QUIT: AtomicBool = AtomicBool::new(false);
pub fn quit_requested() -> bool {
    QUIT.load(Ordering::Relaxed)
}
extern "C" fn on_signal(_: libc::c_int) {
    QUIT.store(true, Ordering::Relaxed);
}

/// Messages for the compose thread.
enum Msg {
    Lastfm(lastfm::Update),
    Resize(usize, usize),
}

struct Args {
    snapshot: Option<String>,
    size: Option<(usize, usize)>,
    bench: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args { snapshot: None, size: None, bench: false };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--snapshot" => a.snapshot = Some(it.next().ok_or("--snapshot needs a file name")?),
            "--size" => a.size = Some(parse_size(&it.next().ok_or("--size needs WIDTHxHEIGHT")?)?),
            "--bench" => a.bench = true,
            "-h" | "--help" => {
                println!(
                    "now-playing [--size WxH] [--snapshot out.png] [--bench]\n\n\
                     env: LASTFM_USER, LASTFM_API_KEY   (required)\n     \
                     POLL_SECS        seconds between Last.fm checks (default 6)\n     \
                     ROTATE           0, 90, 180 or 270 (default: what the panel reports, else 0)\n     \
                     CONNECTOR        screen to use, e.g. HDMI-A-1 or DSI-1 (default: first connected)\n     \
                     FONT_REGULAR, FONT_BOLD   font file overrides\n\n\
                     On Linux it draws straight to the screen (DRM/KMS); on macOS it opens a window."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(a)
}

fn parse_size(s: &str) -> Result<(usize, usize), String> {
    let (w, h) = s.split_once('x').ok_or("size must be WIDTHxHEIGHT")?;
    let (w, h): (usize, usize) = (w.parse().map_err(|_| "bad width")?, h.parse().map_err(|_| "bad height")?);
    if w < 64 || h < 64 || w > 8192 || h > 8192 {
        return Err("size must be between 64 and 8192 pixels".into());
    }
    Ok((w, h))
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn lastfm_config() -> Result<lastfm::Config, String> {
    Ok(lastfm::Config {
        user: env("LASTFM_USER").ok_or("set LASTFM_USER")?,
        api_key: env("LASTFM_API_KEY").ok_or("set LASTFM_API_KEY (free: https://www.last.fm/api/account/create)")?,
        api_url: env("LASTFM_API_URL").unwrap_or_else(|| "https://ws.audioscrobbler.com/2.0/".into()),
        poll: Duration::from_secs_f32(env("POLL_SECS").and_then(|v| v.parse().ok()).unwrap_or(6.0f32).max(2.0)),
    })
}

fn display_options(size: Option<(usize, usize)>) -> Result<display::Options, String> {
    let rotate = match env("ROTATE").as_deref() {
        None | Some("auto") => None,
        Some(v) => match v.parse::<u32>() {
            Ok(r @ (0 | 90 | 180 | 270)) => Some(r),
            _ => return Err(format!("ROTATE must be 0, 90, 180 or 270 (got {v})")),
        },
    };
    Ok(display::Options { size, rotate, connector: env("CONNECTOR") })
}

fn main() {
    if let Err(e) = run() {
        eprintln!("now-playing: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    unsafe {
        libc::signal(libc::SIGINT, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
    }
    let fonts = fonts::Fonts::discover()?;

    if args.bench {
        bench(&fonts);
        return Ok(());
    }

    let opts = display_options(args.size)?;
    let (msg_tx, msg_rx) = mpsc::channel::<Msg>();
    let tx = msg_tx.clone();
    lastfm::spawn(lastfm_config()?, move |u| tx.send(Msg::Lastfm(u)).is_ok());

    if let Some(path) = args.snapshot {
        return snapshot(&fonts, msg_rx, args.size.unwrap_or((800, 480)), &path);
    }

    let Some(mut display) = wait_for_display(&opts) else { return Ok(()) };
    let (mut w, mut h) = display.size();

    // Compose thread: turns Last.fm updates into finished frames.
    let (scene_tx, scene_rx) = mpsc::channel::<(Scene, bool)>();
    thread::spawn(move || compose_loop(fonts, (w, h), msg_rx, scene_tx));

    let mut base = vec![0u32; w * h]; // current static frame
    let mut from = vec![0u32; w * h]; // previous frame during a crossfade
    let mut out = vec![0u32; w * h]; // what goes to the screen
    let mut eq = None;
    let mut fade_start: Option<Instant> = None;
    let mut dirty = Dirty::Full;
    let start = Instant::now();

    loop {
        // Take the newest finished frame, if any (ignoring ones for an old size).
        if let Some((scene, fade)) = scene_rx.try_iter().filter(|(s, _)| s.pixels.len() == w * h).last() {
            if fade {
                from.copy_from_slice(&out);
                fade_start = Some(Instant::now());
            } else {
                dirty = Dirty::Full;
                out.copy_from_slice(&scene.pixels);
            }
            base = scene.pixels;
            eq = scene.eq;
        }

        if let Some(t0) = fade_start {
            let x = t0.elapsed().as_secs_f32() / FADE.as_secs_f32();
            frame::blend(&from, &base, &mut out, (frame::ease(x) * 256.0) as u32);
            if x >= 1.0 {
                fade_start = None;
                out.copy_from_slice(&base);
            }
            dirty = Dirty::Full;
        }

        if let Some(spec) = &eq {
            frame::draw_eq(&mut out, &base, w, spec, start.elapsed().as_secs_f32());
            if matches!(dirty, Dirty::None) {
                dirty = Dirty::Rects(vec![frame::eq_rect(spec)]);
            }
        }

        let status = display.present(&out, std::mem::replace(&mut dirty, Dirty::None))?;
        match status {
            Status::Continue => continue,
            Status::Quit => break,
            Status::Lost => {
                drop(display);
                let Some(d) = wait_for_display(&opts) else { break };
                display = d;
            }
            Status::Resized => {}
        }
        // New screen or new size: start over with a blank frame at the new size.
        let (nw, nh) = display.size();
        if (nw, nh) != (w, h) {
            (w, h) = (nw, nh);
            base = vec![0u32; w * h];
            from = vec![0u32; w * h];
            out = vec![0u32; w * h];
            eq = None;
            fade_start = None;
            if msg_tx.send(Msg::Resize(w, h)).is_err() {
                break;
            }
        } else {
            out.copy_from_slice(&base);
        }
        dirty = Dirty::Full;
    }
    Ok(())
}

/// Opens the display, waiting (quietly) until a screen is connected.
/// Returns `None` if asked to quit while waiting.
fn wait_for_display(opts: &display::Options) -> Option<Box<dyn display::Display>> {
    let mut last_err = String::new();
    loop {
        match display::open(opts) {
            Ok(d) => return Some(d),
            Err(e) => {
                if e != last_err {
                    eprintln!("waiting for a screen: {e}");
                    last_err = e;
                }
            }
        }
        for _ in 0..20 {
            if quit_requested() {
                return None;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

fn compose_loop(fonts: fonts::Fonts, size: (usize, usize), rx: mpsc::Receiver<Msg>, tx: mpsc::Sender<(Scene, bool)>) {
    let (mut w, mut h) = size;
    let mut track: Option<Option<lastfm::Track>> = None; // None = no answer from Last.fm yet
    let mut error: Option<String> = None;
    let mut shown_key: Option<Option<(String, String, String)>> = None;

    loop {
        let view = match &track {
            None => View::Connecting(error.as_deref()),
            Some(t) => View::Track(t.as_ref()),
        };
        // Crossfade when the song (or kind of screen) changes, swap instantly
        // for small updates such as "14 min ago" → "15 min ago".
        let key = track.as_ref().map(|t| {
            t.as_ref().map(|t| {
                let (a, b, c) = t.key();
                (a.to_string(), b.to_string(), c.to_string())
            })
        });
        let fade = shown_key.is_some() && shown_key != Some(key.clone().flatten());
        shown_key = Some(key.flatten());

        let scene = scene::render(&fonts, w, h, &view, SystemTime::now());
        if tx.send((scene, fade)).is_err() {
            return;
        }

        // Re-render every 30 s so "last played … ago" stays current.
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Msg::Lastfm(lastfm::Update::Track(t))) => {
                track = Some(t);
                error = None;
            }
            Ok(Msg::Lastfm(lastfm::Update::Error(e))) => {
                if track.is_none() {
                    error = Some(e);
                } else {
                    continue; // keep showing the last good state
                }
            }
            Ok(Msg::Resize(nw, nh)) => {
                (w, h) = (nw, nh);
                shown_key = None; // no crossfade from a frame of another size
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn snapshot(fonts: &fonts::Fonts, rx: mpsc::Receiver<Msg>, (w, h): (usize, usize), path: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let track = loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Msg::Lastfm(lastfm::Update::Track(t))) => break t,
            Ok(Msg::Lastfm(lastfm::Update::Error(e))) => return Err(e),
            Ok(Msg::Resize(..)) => {}
            Err(_) => return Err("no answer from Last.fm within 20 s".into()),
        }
    };
    let mut scene = scene::render(fonts, w, h, &View::Track(track.as_ref()), SystemTime::now());
    if let Some(spec) = scene.eq {
        let base = scene.pixels.clone();
        frame::draw_eq(&mut scene.pixels, &base, w, &spec, 0.35);
    }
    let rgb: Vec<u8> = scene.pixels.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    image::save_buffer(path, &rgb, w as u32, h as u32, image::ColorType::Rgb8).map_err(|e| e.to_string())?;
    println!("wrote {path} ({w}x{h})");
    Ok(())
}

/// Times the work that matters for smoothness. The 60 fps budget is 16.7 ms.
fn bench(fonts: &fonts::Fonts) {
    let art = image::RgbaImage::from_fn(770, 770, |x, y| image::Rgba([(x / 3) as u8, (y / 3) as u8, 140, 255]));
    let track = lastfm::Track {
        title: "Ultraviolet Hours (Extended Version)".into(),
        artist: "Elsa Morrow & The Night Shift".into(),
        album: "Signals From the Lake".into(),
        art_url: String::new(),
        art: Some(std::sync::Arc::new(art)),
        playing: true,
        played_at: None,
    };
    println!("{:<11} {:>16} {:>18} {:>16}", "size", "render (bg thr)", "crossfade/frame", "eq/frame");
    for (w, h) in [(480, 320), (800, 480), (1024, 600), (1280, 720), (720, 1280), (720, 720)] {
        let t = Instant::now();
        let n = 3;
        let mut scene = None;
        for _ in 0..n {
            scene = Some(scene::render(fonts, w, h, &View::Track(Some(&track)), SystemTime::now()));
        }
        let render_ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
        let scene = scene.unwrap();

        let from = vec![0x0011_2233u32; w * h];
        let mut out = vec![0u32; w * h];
        let n = 200;
        let t = Instant::now();
        for i in 0..n {
            frame::blend(&from, &scene.pixels, &mut out, (i % 257) as u32);
        }
        let blend_ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;

        let n = 5000;
        let t = Instant::now();
        if let Some(spec) = &scene.eq {
            for i in 0..n {
                frame::draw_eq(&mut out, &scene.pixels, w, spec, i as f32 / 60.0);
            }
        }
        let eq_us = t.elapsed().as_secs_f64() * 1e6 / n as f64;
        println!("{:<11} {:>13.1} ms {:>15.2} ms {:>13.1} µs", format!("{w}x{h}"), render_ms, blend_ms, eq_us);
    }
}
