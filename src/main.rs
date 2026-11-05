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
mod settings;
mod setup;
mod themes;
mod vinylvault;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(target_os = "linux")]
use display::Status;
use scene::{Scene, Theme, View};

static QUIT: AtomicBool = AtomicBool::new(false);
pub fn quit_requested() -> bool {
    QUIT.load(Ordering::Relaxed)
}
extern "C" fn on_signal(_: libc::c_int) {
    QUIT.store(true, Ordering::Relaxed);
}
/// SIGUSR1 (`systemctl kill -s USR1 now-playing`) opens the setup page again.
static SETUP: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
extern "C" fn on_setup_signal(_: libc::c_int) {
    SETUP.store(true, Ordering::Relaxed);
}
pub fn take_setup_request() -> bool {
    SETUP.swap(false, Ordering::Relaxed)
}

/// What keys and menus ask for.
pub enum Command {
    Look(Theme),
    IdlePreview,
    Settings,
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    Fullscreen,
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    Screen(String),
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    Quit,
}

/// The 60 fps side: takes finished frames from the compose thread and
/// produces the pixels to show. Driven by the Pi's screen loop or the
/// desktop window.
pub struct Player {
    comp: frame::Compositor,
    size: (usize, usize),
    scene_rx: mpsc::Receiver<(Scene, bool)>,
    msg_tx: mpsc::Sender<Msg>,
    theme: Theme,
    browser: bool,
}

impl Player {
    fn start(fonts: fonts::Fonts, look: Look, size: (usize, usize), msg_tx: mpsc::Sender<Msg>, msg_rx: mpsc::Receiver<Msg>, browser: bool) -> Player {
        let (scene_tx, scene_rx) = mpsc::channel::<(Scene, bool)>();
        let theme = look.theme;
        let tx = msg_tx.clone();
        thread::spawn(move || compose_loop(fonts, look, size, tx, msg_rx, scene_tx));
        Player { comp: frame::Compositor::new(size.0, size.1), size, scene_rx, msg_tx, theme, browser }
    }

    pub fn size(&self) -> (usize, usize) {
        self.size
    }

    pub fn theme(&self) -> Theme {
        self.theme
    }

    pub fn resize(&mut self, w: usize, h: usize) {
        self.size = (w, h);
        self.comp = frame::Compositor::new(w, h);
        let _ = self.msg_tx.send(Msg::Resize(w, h));
    }

    /// Advances one frame; returns what changed.
    pub fn frame(&mut self) -> display::Dirty {
        // Newest finished frame, if any (ignoring ones rendered for an old size).
        if let Some((scene, fade)) = self.scene_rx.try_iter().filter(|(s, _)| self.comp.accepts(s)).last() {
            self.comp.set_scene(scene, fade);
        }
        self.comp.frame()
    }

    /// Everything must be re-sent (the screen was re-opened).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn invalidate(&mut self) {
        self.comp.invalidate();
    }

    pub fn pixels(&self) -> &[u32] {
        &self.comp.out
    }

    /// Opens the browser at a page of the (quietly started) setup server,
    /// e.g. "connect" for Last.fm. No QR code on the display for this.
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub fn open_setup_page(&self, path: &str) {
        if let Err(e) = start_setup(&self.msg_tx) {
            return eprintln!("{e}");
        }
        if let Some(base) = setup::local_url() {
            mac_app::open_browser(&format!("{base}{path}"));
        }
    }

    /// Starts "Connect with VinylVault" for the desktop's sign-in sheet: the
    /// VinylVault address to open; the answer comes back to the (quietly
    /// started) local setup server.
    #[cfg_attr(not(all(feature = "desktop", target_os = "macos")), allow(dead_code))]
    pub fn vinylvault_connect_url(&self) -> Option<String> {
        start_setup(&self.msg_tx).ok()?;
        setup::vinylvault_start(&setup::local_url()?)
    }

    /// Settings changed outside the setup page: re-read them.
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub fn reload(&self) {
        let _ = self.msg_tx.send(Msg::Reload);
    }

    pub fn command(&mut self, cmd: Command) {
        match cmd {
            Command::Look(t) => {
                self.theme = t;
                let _ = settings::save(&[("THEME", Some(t.name()))]);
                let _ = self.msg_tx.send(Msg::Theme(t));
            }
            Command::IdlePreview => {
                let _ = self.msg_tx.send(Msg::ToggleIdlePreview);
            }
            Command::Settings => open_setup(&self.msg_tx, self.browser),
            _ => {}
        }
    }
}

/// Messages for the compose thread.
enum Msg {
    Lastfm(lastfm::Update),
    Resize(usize, usize),
    Theme(Theme),
    ToggleIdlePreview,
    ShelfUpdated,
    /// The setup page saved new settings.
    Reload,
    /// The setup page is open at this address (`None`: closed).
    Setup(Option<String>),
    /// Someone opened the setup page.
    SetupVisited,
}

struct Args {
    snapshot: Option<String>,
    size: Option<(usize, usize)>,
    bench: bool,
    idle: bool,
    setup: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args { snapshot: None, size: None, bench: false, idle: false, setup: false };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--snapshot" => a.snapshot = Some(it.next().ok_or("--snapshot needs a file name")?),
            "--size" => a.size = Some(parse_size(&it.next().ok_or("--size needs WIDTHxHEIGHT")?)?),
            "--bench" => a.bench = true,
            "--idle" => a.idle = true,
            "--setup" => a.setup = true,
            "-h" | "--help" => {
                println!(
                    "now-playing [--size WxH] [--setup] [--snapshot out.png [--idle]] [--bench]\n\n\
                     Not set up yet (or --setup, or SIGUSR1): shows a QR code for the setup page.\n\
                     Settings come from the environment, else from the settings file the page writes.\n\n\
                     LASTFM_USER, LASTFM_API_KEY   your Last.fm account\n     \
                     THEME            vinyl (default), poster or ambient\n     \
                     CLOCK            24h (default) or 12h\n     \
                     VINYLVAULT_SHELF your shared VinylVault shelf link or slug: draws your pressing\n     \
                     VINYLVAULT_CATALOGUE  on (default) or off: else draw a real pressing of the album\n     \
                     PROMO            on (default) or off: the small VinylVault mentions\n     \
                     STATS            on (default) or off: your play counts under the playing track\n     \
                     POLL_SECS        seconds between Last.fm checks (default 2; 1 s near a track's end)\n     \
                     ROTATE           0, 90, 180 or 270 (default: what the panel reports, else 0)\n     \
                     CONNECTOR        screen to use, e.g. HDMI-A-1 or DSI-1 (default: first connected)\n     \
                     FONT_LIGHT, FONT_REGULAR, FONT_BOLD, FONT_BLACK   font file overrides\n\n\
                     On Linux it draws straight to the screen (DRM/KMS); on macOS it opens a\n\
                     window (keys: 1 vinyl, 2 poster, 3 ambient, I preview idle, S setup, Esc quit)."
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
    settings::get(k)
}

/// `None` until a Last.fm user and key are set up.
fn lastfm_config() -> Option<lastfm::Config> {
    Some(lastfm::Config {
        user: env("LASTFM_USER")?,
        api_key: env("LASTFM_API_KEY").or_else(|| lastfm::builtin_key().map(String::from))?,
        api_url: env("LASTFM_API_URL").unwrap_or_else(|| lastfm::API_URL.into()),
        poll: Duration::from_secs_f32(env("POLL_SECS").and_then(|v| v.parse().ok()).unwrap_or(2.0f32).clamp(1.0, 60.0)),
    })
}

fn display_options() -> Result<display::Options, String> {
    let rotate = match env("ROTATE").as_deref() {
        None | Some("auto") => None,
        Some(v) => match v.parse::<u32>() {
            Ok(r @ (0 | 90 | 180 | 270)) => Some(r),
            _ => return Err(format!("ROTATE must be 0, 90, 180 or 270 (got {v})")),
        },
    };
    Ok(display::Options { rotate, connector: env("CONNECTOR") })
}

struct Look {
    theme: Theme,
    twelve_hour: bool,
    promo: bool,
    /// Your play counts under the playing track (STATS=off hides them).
    stats: bool,
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
    let stats = !matches!(env("STATS").as_deref(), Some("off" | "0" | "false" | "no"));
    Ok(Look { theme, twelve_hour, promo, stats })
}

fn main() {
    if let Err(e) = run() {
        eprintln!("now-playing: {e}");
        mac_app::show_error(&e);
        std::process::exit(1);
    }
}

mod mac_app {
    use std::process::Command;

    /// Opened from Finder/Dock: macOS started it (parent is launchd), no terminal.
    pub fn from_finder() -> bool {
        #[cfg(target_os = "macos")]
        return unsafe { libc::getppid() == 1 && libc::isatty(2) == 0 };
        #[cfg(not(target_os = "macos"))]
        false
    }

    /// From Finder there's no terminal to print to: show a dialog.
    pub fn show_error(e: &str) {
        if from_finder() {
            // Passed as an argument, not spliced into the script.
            let _ = Command::new("osascript")
                .args(["-e", "on run argv", "-e", "display alert \"Now Playing\" message (item 1 of argv)", "-e", "end run", e])
                .status();
        }
    }

    pub fn open_browser(url: &str) {
        #[cfg(target_os = "macos")]
        let _ = Command::new("open").arg(url).status();
        #[cfg(target_os = "windows")]
        let _ = Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).status();
        #[cfg(target_os = "linux")]
        let _ = Command::new("xdg-open").arg(url).status();
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGINT, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        libc::signal(libc::SIGUSR1, on_setup_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
    }
    let look = look()?;
    let fonts = fonts::Fonts::discover()?;

    if args.bench {
        bench(&fonts);
        return Ok(());
    }

    let opts = display_options()?;
    let (msg_tx, msg_rx) = mpsc::channel::<Msg>();

    if let Some(path) = args.snapshot {
        let size = args.size.unwrap_or((800, 480));
        if args.setup {
            let view = View::Setup { url: "http://192.168.1.23:8737/k7m2qx9p4a/", connected: false };
            return save_png(&scene::render(&fonts, size.0, size.1, &view, SystemTime::now(), look.theme, false, false), size.0, &path);
        }
        let cfg = lastfm_config().ok_or("Last.fm isn't set up yet: start it without --snapshot and use the setup page")?;
        let tx = msg_tx.clone();
        lastfm::spawn(cfg, Default::default(), move |u| tx.send(Msg::Lastfm(u)).is_ok());
        let shelf = match vinylvault::source() {
            Some(s) => Some(vinylvault::load_once(&s)?),
            None => None,
        };
        let records = Records { shelf, catalogue: catalogue() };
        return snapshot(&fonts, &look, &records, msg_rx, size, args.idle, &path);
    }

    // A window if there's a desktop, else straight to the screen (Pi).
    let desktop = display::desktop_available();
    let needs_setup = lastfm_config().is_none() || args.setup;
    // The Mac opens its own settings window instead; elsewhere: the setup
    // page (QR code on the display, and in the browser on a desktop).
    let native_settings = desktop && cfg!(target_os = "macos");
    if needs_setup && !native_settings {
        open_setup(&msg_tx, desktop);
    }

    #[cfg(feature = "desktop")]
    if desktop {
        let size = args.size.unwrap_or((800, 480));
        let player = Player::start(fonts, look, size, msg_tx, msg_rx, true);
        return display::run_desktop(player, size, needs_setup && native_settings);
    }
    screen_loop(fonts, look, &opts, msg_tx, msg_rx)
}

/// The Pi: draw straight to the screen, re-open it when it's swapped.
#[cfg(target_os = "linux")]
fn screen_loop(fonts: fonts::Fonts, look: Look, opts: &display::Options, msg_tx: mpsc::Sender<Msg>, msg_rx: mpsc::Receiver<Msg>) -> Result<(), String> {
    let Some(mut display) = wait_for_display(opts) else { return Ok(()) };
    let mut player = Player::start(fonts, look, display.size(), msg_tx, msg_rx, false);
    loop {
        let dirty = player.frame();
        match display.present(player.pixels(), dirty)? {
            Status::Continue => {}
            Status::Quit => break,
            Status::Lost => {
                drop(display);
                let Some(d) = wait_for_display(opts) else { break };
                display = d;
                player.invalidate(); // a new screen of a new size is caught below
            }
        }
        if take_setup_request() {
            player.command(Command::Settings);
        }
        let (w, h) = display.size();
        if (w, h) != player.size() {
            player.resize(w, h);
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn screen_loop(_: fonts::Fonts, _: Look, _: &display::Options, _: mpsc::Sender<Msg>, _: mpsc::Receiver<Msg>) -> Result<(), String> {
    Err("this build has no window (built without the \"desktop\" feature)".into())
}

/// Opens the display, waiting (quietly) until a screen is connected.
/// Returns `None` if asked to quit while waiting.
#[cfg(target_os = "linux")]
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

fn start_setup(tx: &mpsc::Sender<Msg>) -> Result<Option<setup::Started>, String> {
    let t = tx.clone();
    setup::start(move |e| {
        let _ = t.send(match e {
            setup::Event::Visited => Msg::SetupVisited,
            setup::Event::Saved => Msg::Reload,
            setup::Event::Closed => Msg::Setup(None),
        });
    })
}

/// Starts the setup page (if it isn't open) and tells the display to show its QR code.
fn open_setup(tx: &mpsc::Sender<Msg>, browser: bool) {
    match start_setup(tx) {
        Ok(Some(s)) => {
            eprintln!("setup page: {}", s.url);
            let _ = tx.send(Msg::Setup(Some(s.url)));
            if browser {
                mac_app::open_browser(&s.local_url);
            }
        }
        Ok(None) => {}
        Err(e) => eprintln!("{e}"),
    }
}

fn catalogue() -> Option<vinylvault::Catalogue> {
    match env("VINYLVAULT_CATALOGUE").as_deref() {
        Some("off" | "0" | "false" | "no") => None,
        _ => Some(vinylvault::Catalogue::new(&vinylvault::site())),
    }
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

/// The running Last.fm poller and the account it polls.
struct Poller {
    account: Option<(String, String)>,
    stop: Arc<AtomicBool>,
}

impl Poller {
    /// (Re)starts polling if the account changed. Returns true if it did.
    fn sync(&mut self, tx: &mpsc::Sender<Msg>) -> bool {
        let cfg = lastfm_config();
        let account = cfg.as_ref().map(|c| (c.user.clone(), c.api_key.clone()));
        if account == self.account {
            return false;
        }
        self.stop.store(true, Ordering::Relaxed);
        self.stop = Arc::new(AtomicBool::new(false));
        self.account = account;
        if let Some(cfg) = cfg {
            let tx = tx.clone();
            lastfm::spawn(cfg, self.stop.clone(), move |u| tx.send(Msg::Lastfm(u)).is_ok());
        }
        true
    }
}

fn spawn_shelf(tx: &mpsc::Sender<Msg>) -> (Option<vinylvault::Source>, Option<vinylvault::Shelf>) {
    let src = vinylvault::source();
    let shelf = src.clone().map(|s| {
        let tx = tx.clone();
        vinylvault::spawn(s, move || tx.send(Msg::ShelfUpdated).is_ok())
    });
    (src, shelf)
}

fn compose_loop(
    fonts: fonts::Fonts,
    mut look: Look,
    size: (usize, usize),
    me: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    tx: mpsc::Sender<(Scene, bool)>,
) {
    let (mut w, mut h) = size;
    let mut poller = Poller { account: None, stop: Arc::new(AtomicBool::new(false)) };
    poller.sync(&me);
    let (mut shelf_setting, shelf) = spawn_shelf(&me);
    let mut records = Records { shelf, catalogue: catalogue() };
    let mut track: Option<Option<lastfm::Track>> = None; // None = no answer from Last.fm yet
    let mut error: Option<String> = None;
    let mut preview_idle = false;
    let mut shown: Option<String> = None;
    let mut setup_url: Option<String> = None;
    let mut setup_visited = false;

    loop {
        let configured = poller.account.is_some();
        let idle = preview_idle || !track.as_ref().is_some_and(|t| t.as_ref().is_some_and(|t| t.playing));
        // Only the vinyl look draws pressings: don't search the catalogue for the others.
        let pressing = match &track {
            Some(Some(t)) if look.theme == Theme::Vinyl => records.pressing(t),
            _ => None,
        };
        // The QR code stays up until the page is opened (or, before the first
        // setup, until Last.fm works), so a new look can be seen right away.
        let view = match (&setup_url, &track) {
            (Some(url), _) if !configured || !setup_visited => View::Setup { url, connected: setup_visited },
            (None, _) if !configured => View::Connecting(Some("Not set up yet")),
            (_, None) => View::Connecting(error.as_deref()),
            (_, Some(t)) => View::Track { track: t.as_ref(), idle, pressing: pressing.as_ref() },
        };
        // Crossfade when what's shown changes in kind (song, playing/idle, theme);
        // swap instantly for small updates such as the clock ticking.
        let key = format!(
            "{:?}|{idle}|{:?}|{:?}|{}",
            look.theme,
            pressing.as_ref().map(|p| &p.name),
            track.as_ref().map(|t| t.as_ref().map(|t| (t.title.clone(), t.artist.clone(), t.album.clone()))),
            matches!(view, View::Setup { .. })
        );
        let fade = shown.as_ref().is_some_and(|s| *s != key);
        shown = Some(key);

        let scene = scene::render(&fonts, w, h, &view, SystemTime::now(), look.theme, look.twelve_hour, look.promo);
        if tx.send((scene, fade)).is_err() {
            return;
        }

        // Wake on the next minute (clock), or on news.
        match rx.recv_timeout(until_next_minute()) {
            Ok(Msg::Lastfm(lastfm::Update::Track(mut t))) => {
                if !look.stats {
                    t.iter_mut().for_each(|t| t.stats = None);
                }
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
            Ok(Msg::ShelfUpdated) => {
                // The pass may have been dropped (display disconnected in VinylVault).
                if vinylvault::source() != shelf_setting {
                    let (setting, shelf) = spawn_shelf(&me);
                    shelf_setting = setting;
                    records = Records { shelf, catalogue: catalogue() };
                }
            }
            Ok(Msg::Reload) => {
                if let Ok(l) = self::look() {
                    look = l;
                }
                if poller.sync(&me) {
                    (track, error) = (None, None);
                }
                if vinylvault::source() != shelf_setting {
                    let (setting, shelf) = spawn_shelf(&me);
                    shelf_setting = setting;
                    records = Records { shelf, catalogue: catalogue() };
                }
            }
            Ok(Msg::Setup(url)) => {
                setup_visited = false;
                setup_url = url;
            }
            Ok(Msg::SetupVisited) => setup_visited = true,
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
    let mut track = loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Msg::Lastfm(lastfm::Update::Track(t))) => break t,
            Ok(Msg::Lastfm(lastfm::Update::Error(e))) => return Err(e),
            Ok(_) => {}
            Err(_) => return Err("no answer from Last.fm within 20 s".into()),
        }
    };
    // Play counts follow a moment later (if Last.fm knows any).
    if look.stats && track.as_ref().is_some_and(|t| t.playing) {
        if let Ok(Msg::Lastfm(lastfm::Update::Track(t))) = rx.recv_timeout(Duration::from_secs(5)) {
            track = t;
        }
    } else {
        track.iter_mut().for_each(|t| t.stats = None);
    }
    let idle = idle || !track.as_ref().is_some_and(|t| t.playing);
    let pressing = track.as_ref().and_then(|t| records.pressing(t));
    if let Some(p) = &pressing {
        println!("{} pressing: {}", if p.owned { "your" } else { "catalogue" }, p.caption());
    }
    let view = View::Track { track: track.as_ref(), idle, pressing: pressing.as_ref() };
    save_png(&scene::render(fonts, w, h, &view, SystemTime::now(), look.theme, look.twelve_hour, look.promo), w, path)
}

fn save_png(scene: &Scene, w: usize, path: &str) -> Result<(), String> {
    let pixels = still(scene, w);
    let h = pixels.len() / w;
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
        stats: Some(lastfm::Stats { loved: true, track_plays: Some(37), artist_plays: Some(1204) }),
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
