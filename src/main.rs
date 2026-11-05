//! now-playing: a full-screen "now playing" display for the Raspberry Pi,
//! fed by Last.fm, in one of three looks (vinyl, poster, ambient).
//!
//! Threads:
//!   poller  — asks Last.fm every few seconds, downloads cover art
//!   compose — renders a complete static frame whenever something changes
//!   main    — 60 fps loop (`frame::Compositor`): crossfades, spinning label,
//!             colour drift, equalizer; hands only changed pixels to the
//!             display and follows screen changes (plug/unplug, resize)

mod display;
mod draw;
mod fonts;
mod frame;
mod lastfm;
mod scene;
mod themes;
mod vinylvault;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use display::Status;
use scene::{Scene, Theme, View};

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
    Theme(Theme),
    ToggleIdlePreview,
    ShelfUpdated,
}

struct Args {
    snapshot: Option<String>,
    size: Option<(usize, usize)>,
    bench: bool,
    idle: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args { snapshot: None, size: None, bench: false, idle: false };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--snapshot" => a.snapshot = Some(it.next().ok_or("--snapshot needs a file name")?),
            "--size" => a.size = Some(parse_size(&it.next().ok_or("--size needs WIDTHxHEIGHT")?)?),
            "--bench" => a.bench = true,
            "--idle" => a.idle = true,
            "-h" | "--help" => {
                println!(
                    "now-playing [--size WxH] [--snapshot out.png [--idle]] [--bench]\n\n\
                     env: LASTFM_USER, LASTFM_API_KEY   (required)\n     \
                     THEME            vinyl (default), poster or ambient\n     \
                     CLOCK            24h (default) or 12h\n     \
                     VINYLVAULT_SHELF your shared VinylVault shelf link or slug: draws your pressing\n     \
                     VINYLVAULT_CATALOGUE  on (default) or off: else draw a real pressing of the album\n     \
                     PROMO            on (default) or off: the small VinylVault mentions\n     \
                     POLL_SECS        seconds between Last.fm checks (default 6)\n     \
                     ROTATE           0, 90, 180 or 270 (default: what the panel reports, else 0)\n     \
                     CONNECTOR        screen to use, e.g. HDMI-A-1 or DSI-1 (default: first connected)\n     \
                     FONT_LIGHT, FONT_REGULAR, FONT_BOLD, FONT_BLACK   font file overrides\n\n\
                     On Linux it draws straight to the screen (DRM/KMS); on macOS it opens a\n\
                     window (keys: 1 vinyl, 2 poster, 3 ambient, I preview idle, Esc quit)."
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

struct Look {
    theme: Theme,
    twelve_hour: bool,
    promo: bool,
}

fn look() -> Result<Look, String> {
    let theme = match env("THEME") {
        None => Theme::Vinyl,
        Some(t) => Theme::parse(&t).ok_or(format!("THEME must be vinyl, poster or ambient (got {t})"))?,
    };
    let twelve_hour = match env("CLOCK").as_deref() {
        None | Some("24h") | Some("24") => false,
        Some("12h") | Some("12") => true,
        Some(v) => return Err(format!("CLOCK must be 24h or 12h (got {v})")),
    };
    let promo = !matches!(env("PROMO").as_deref(), Some("off" | "0" | "false" | "no"));
    Ok(Look { theme, twelve_hour, promo })
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
    let look = look()?;
    let fonts = fonts::Fonts::discover()?;

    if args.bench {
        bench(&fonts);
        return Ok(());
    }

    let opts = display_options(args.size)?;
    let (msg_tx, msg_rx) = mpsc::channel::<Msg>();
    let tx = msg_tx.clone();
    lastfm::spawn(lastfm_config()?, move |u| tx.send(Msg::Lastfm(u)).is_ok());

    let shelf_setting = env("VINYLVAULT_SHELF");
    let catalogue = match env("VINYLVAULT_CATALOGUE").as_deref() {
        Some("off" | "0" | "false" | "no") => None,
        _ => Some(vinylvault::Catalogue::new(&vinylvault::base_url(shelf_setting.as_deref()))),
    };
    if let Some(path) = args.snapshot {
        let shelf = match &shelf_setting {
            Some(s) => Some(vinylvault::load_once(s)?),
            None => None,
        };
        let records = Records { shelf, catalogue };
        return snapshot(&fonts, &look, &records, msg_rx, args.size.unwrap_or((800, 480)), args.idle, &path);
    }
    let shelf = shelf_setting.map(|s| {
        let tx = msg_tx.clone();
        vinylvault::spawn(&s, move || tx.send(Msg::ShelfUpdated).is_ok())
    });
    let records = Records { shelf, catalogue };

    let Some(mut display) = wait_for_display(&opts) else { return Ok(()) };
    let (mut w, mut h) = display.size();

    let (scene_tx, scene_rx) = mpsc::channel::<(Scene, bool)>();
    thread::spawn(move || compose_loop(fonts, look, records, (w, h), msg_rx, scene_tx));

    let mut comp = frame::Compositor::new(w, h);
    loop {
        // Newest finished frame, if any (ignoring ones rendered for an old size).
        if let Some((scene, fade)) = scene_rx.try_iter().filter(|(s, _)| comp.accepts(s)).last() {
            comp.set_scene(scene, fade);
        }
        let dirty = comp.frame();
        match display.present(&comp.out, dirty)? {
            Status::Continue => {}
            Status::Quit => break,
            Status::Lost => {
                drop(display);
                let Some(d) = wait_for_display(&opts) else { break };
                display = d;
                comp.invalidate();
            }
            Status::Resized => {}
        }
        if let Some(k) = display.key() {
            let msg = match k {
                '1' => Msg::Theme(Theme::Vinyl),
                '2' => Msg::Theme(Theme::Poster),
                '3' => Msg::Theme(Theme::Ambient),
                _ => Msg::ToggleIdlePreview,
            };
            let _ = msg_tx.send(msg);
        }
        let (nw, nh) = display.size();
        if (nw, nh) != (w, h) {
            (w, h) = (nw, nh);
            comp = frame::Compositor::new(w, h);
            if msg_tx.send(Msg::Resize(w, h)).is_err() {
                break;
            }
        }
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

/// Time until just after the next full minute (so the clock flips on time).
fn until_next_minute() -> Duration {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    Duration::from_millis(60_000 - ms % 60_000 + 50)
}

/// Where pressings come from: your shelf first, then the public catalogue.
struct Records {
    shelf: Option<vinylvault::Shelf>,
    catalogue: Option<vinylvault::Catalogue>,
}

impl Records {
    fn pressing(&self, t: &lastfm::Track) -> Option<vinylvault::Pressing> {
        // On your shelf decides it, even when it's plain black: never dress your
        // own black copy up as somebody else's coloured one.
        if let Some(on_shelf) = self.shelf.as_ref().and_then(|s| vinylvault::lookup(s, &t.album, &t.artist)) {
            return on_shelf;
        }
        self.catalogue.as_ref().and_then(|c| c.pressing_for(&t.album, &t.artist))
    }
}

fn compose_loop(
    fonts: fonts::Fonts,
    mut look: Look,
    records: Records,
    size: (usize, usize),
    rx: mpsc::Receiver<Msg>,
    tx: mpsc::Sender<(Scene, bool)>,
) {
    let (mut w, mut h) = size;
    let mut track: Option<Option<lastfm::Track>> = None; // None = no answer from Last.fm yet
    let mut error: Option<String> = None;
    let mut preview_idle = false;
    let mut shown: Option<String> = None;

    loop {
        let idle = preview_idle || !track.as_ref().is_some_and(|t| t.as_ref().is_some_and(|t| t.playing));
        // Only the vinyl look draws pressings: don't search the catalogue for the others.
        let pressing = match &track {
            Some(Some(t)) if look.theme == Theme::Vinyl => records.pressing(t),
            _ => None,
        };
        let view = match &track {
            None => View::Connecting(error.as_deref()),
            Some(t) => View::Track { track: t.as_ref(), idle, pressing: pressing.as_ref() },
        };
        // Crossfade when what's shown changes in kind (song, playing/idle, theme);
        // swap instantly for small updates such as the clock ticking.
        let key = format!(
            "{:?}|{idle}|{:?}|{:?}",
            look.theme,
            pressing.as_ref().map(|p| &p.name),
            track.as_ref().map(|t| t.as_ref().map(|t| (t.title.clone(), t.artist.clone(), t.album.clone())))
        );
        let fade = shown.as_ref().is_some_and(|s| *s != key);
        shown = Some(key);

        let scene = scene::render(&fonts, w, h, &view, SystemTime::now(), look.theme, look.twelve_hour, look.promo);
        if tx.send((scene, fade)).is_err() {
            return;
        }

        // Wake on the next minute (clock), or on news.
        match rx.recv_timeout(until_next_minute()) {
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
                shown = None; // no crossfade from a frame of another size
            }
            Ok(Msg::Theme(t)) => look.theme = t,
            Ok(Msg::ToggleIdlePreview) => preview_idle = !preview_idle,
            Ok(Msg::ShelfUpdated) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn snapshot(
    fonts: &fonts::Fonts,
    look: &Look,
    records: &Records,
    rx: mpsc::Receiver<Msg>,
    (w, h): (usize, usize),
    idle: bool,
    path: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let track = loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Msg::Lastfm(lastfm::Update::Track(t))) => break t,
            Ok(Msg::Lastfm(lastfm::Update::Error(e))) => return Err(e),
            Ok(_) => {}
            Err(_) => return Err("no answer from Last.fm within 20 s".into()),
        }
    };
    let idle = idle || !track.as_ref().is_some_and(|t| t.playing);
    let pressing = track.as_ref().and_then(|t| records.pressing(t));
    if let Some(p) = &pressing {
        println!("{} pressing: {}", if p.owned { "your" } else { "catalogue" }, p.caption());
    }
    let view = View::Track { track: track.as_ref(), idle, pressing: pressing.as_ref() };
    let scene = scene::render(fonts, w, h, &view, SystemTime::now(), look.theme, look.twelve_hour, look.promo);
    let pixels = still(&scene, w);
    let rgb: Vec<u8> = pixels.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    image::save_buffer(path, &rgb, w as u32, h as u32, image::ColorType::Rgb8).map_err(|e| e.to_string())?;
    println!("wrote {path} ({w}x{h})");
    Ok(())
}

/// A representative still of an animated scene (mid-drift, label turned a bit).
fn still(scene: &Scene, w: usize) -> Vec<u32> {
    let mut px = match &scene.alt {
        Some(alt) => {
            let mut o = vec![0; scene.pixels.len()];
            frame::blend(&scene.pixels, alt, &mut o, 128);
            o
        }
        None => scene.pixels.clone(),
    };
    let under = px.clone();
    if let Some(eq) = &scene.eq {
        frame::draw_eq(&mut px, &under, w, eq, 0.35);
    }
    if let Some(sp) = &scene.spinner {
        frame::draw_spinner(&mut px, &under, w, sp, 0.5, 256);
    }
    px
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
    println!("Per-frame budget at 60 fps: 16.7 ms. 'worst frame' = the most work any single frame does.");
    println!("{:<8} {:<10} {:>14} {:>13} {:>13}", "theme", "size", "render (bg)", "steady frame", "worst frame");
    let marbled = vinylvault::Pressing {
        name: "Red Marbled".into(),
        finish: vinylvault::Finish::Marbled,
        colors: vec![[204, 41, 46], [247, 247, 245]],
        edition_size: Some(500),
        is_limited: true,
        is_numbered: false,
        owned: true,
    };
    let cases = [
        ("vinyl", Theme::Vinyl, None),
        ("pressing", Theme::Vinyl, Some(&marbled)),
        ("poster", Theme::Poster, None),
        ("ambient", Theme::Ambient, None),
    ];
    for (label, theme, pressing) in cases {
        for (w, h) in [(480, 320), (800, 480), (1280, 720), (720, 1280)] {
            let t = Instant::now();
            let view = View::Track { track: Some(&track), idle: false, pressing };
            let scene = scene::render(fonts, w, h, &view, SystemTime::now(), theme, false, true);
            let render_ms = t.elapsed().as_secs_f64() * 1000.0;

            // Steady state: whatever the theme animates every frame.
            let mut comp = frame::Compositor::new(w, h);
            comp.set_scene(scene, false);
            comp.frame();
            let n = 300;
            let t = Instant::now();
            for _ in 0..n {
                let _ = comp.frame();
            }
            let steady_ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;

            // Worst case: a crossfade frame (full blend + overlays).
            let scene2 = scene::render(fonts, w, h, &view, SystemTime::now(), theme, false, true);
            comp.set_scene(scene2, true);
            let n = 30;
            let t = Instant::now();
            for _ in 0..n {
                let _ = comp.frame();
            }
            let worst_ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
            println!(
                "{:<8} {:<10} {:>11.0} ms {:>10.2} ms {:>10.2} ms",
                label,
                format!("{w}x{h}"),
                render_ms,
                steady_ms,
                worst_ms
            );
        }
    }
}
