//! What the native settings windows show and do, independent of the
//! platform's controls (AppKit on the Mac, Win32 on Windows). No browser:
//! Last.fm needs only a user name (the app has its own key), VinylVault
//! signs in through the system's sign-in sheet.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::scene::Theme;
use crate::{Command, Player, lastfm, settings};

pub enum Action {
    LastfmUser(String),
    ConnectVinylVault,
    DisconnectVinylVault,
    Look(Theme),
    TwelveHour(bool),
    Stats(bool),
    OwnedHint(bool),
}

pub struct State {
    pub lastfm_user: Option<String>,
    /// Result of the last user name check: (ok, message).
    pub lastfm_note: Option<(bool, String)>,
    /// Connected with "Connect with VinylVault": the owner's name (may be empty).
    pub vinylvault: Option<String>,
    pub shelf_link: bool,
    pub theme: Theme,
    pub twelve_hour: bool,
    pub stats: bool,
    pub owned_hint: bool,
}

/// The user name check runs on its own thread (it asks Last.fm); its
/// result lands here.
static NOTE: Mutex<Option<(bool, String)>> = Mutex::new(None);
static CHECKING: AtomicBool = AtomicBool::new(false);
static SAVED: AtomicBool = AtomicBool::new(false);

fn on(key: &str, default: bool) -> bool {
    match settings::get(key).as_deref() {
        Some("on" | "1" | "true" | "yes" | "12h" | "12") => true,
        Some(_) => false,
        None => default,
    }
}

impl State {
    pub fn current(theme: Theme) -> State {
        State {
            lastfm_user: settings::get("LASTFM_USER"),
            lastfm_note: NOTE.lock().ok().and_then(|n| n.clone()),
            vinylvault: settings::get("VINYLVAULT_TOKEN").map(|_| settings::get("VINYLVAULT_OWNER").unwrap_or_default()),
            shelf_link: settings::get("VINYLVAULT_SHELF").is_some(),
            theme,
            twelve_hour: on("CLOCK", false),
            stats: on("STATS", true),
            owned_hint: on("OWNED_HINT", true),
        }
    }
}

fn note(ok: bool, msg: String) {
    if let Ok(mut n) = NOTE.lock() {
        *n = Some((ok, msg));
    }
}

/// Checks the name with Last.fm, then saves it.
fn check_user(name: String) {
    if CHECKING.swap(true, Ordering::SeqCst) {
        return;
    }
    note(true, "Checking…".into());
    std::thread::spawn(move || {
        let key = settings::get("LASTFM_API_KEY").or_else(|| lastfm::builtin_key().map(String::from));
        let result = match key {
            None => Err("This build has no Last.fm key.".to_string()),
            Some(api_key) => {
                let api_url = settings::get("LASTFM_API_URL").unwrap_or_else(|| lastfm::API_URL.into());
                let cfg = lastfm::Config { user: name.clone(), api_key, api_url, poll: std::time::Duration::from_secs(2) };
                lastfm::check(&cfg)
            }
        };
        match result.and_then(|t| settings::save(&[("LASTFM_USER", Some(&name))]).map(|_| t)) {
            Ok(Some(t)) if t.playing => note(true, format!("✓ Playing now: {} – {}", t.title, t.artist)),
            Ok(Some(t)) => note(true, format!("✓ Last played: {} – {}", t.title, t.artist)),
            Ok(None) => note(true, "✓ Connected. Play something and it shows up.".into()),
            Err(e) => note(false, e),
        }
        SAVED.store(true, Ordering::SeqCst);
        CHECKING.store(false, Ordering::SeqCst);
    });
}

/// Called every frame by the window loop: picks up a finished check.
pub fn tick(player: &Player) -> bool {
    if SAVED.swap(false, Ordering::SeqCst) {
        player.reload();
        return true;
    }
    false
}

/// Everything except `ConnectVinylVault`, which needs the platform's sign-in sheet.
pub fn apply(player: &mut Player, a: Action) {
    match a {
        Action::LastfmUser(name) => {
            let name = name.trim().to_string();
            if name.is_empty() || name.len() > 64 {
                note(false, "Enter your Last.fm user name.".into());
            } else {
                check_user(name);
            }
        }
        Action::ConnectVinylVault => {}
        Action::DisconnectVinylVault => {
            let _ = settings::save(&[("VINYLVAULT_TOKEN", None), ("VINYLVAULT_TOKEN_SITE", None), ("VINYLVAULT_OWNER", None)]);
            player.reload();
        }
        Action::Look(t) => player.command(Command::Look(t)),
        Action::TwelveHour(b) => {
            let _ = settings::save(&[("CLOCK", Some(if b { "12h" } else { "24h" }))]);
            player.reload();
        }
        Action::OwnedHint(b) => {
            let _ = settings::save(&[("OWNED_HINT", Some(if b { "on" } else { "off" }))]);
            player.reload();
        }
        Action::Stats(b) => {
            let _ = settings::save(&[("STATS", Some(if b { "on" } else { "off" }))]);
            player.reload();
        }
    }
}
