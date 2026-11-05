//! Setup from a phone: a tiny web page served by the display itself, reached
//! by the QR code on screen. The address holds a random code, so others on
//! the network can't change the display. Closes after "Done" or 20 idle minutes.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tiny_http::{Header, Method, Response, Server};

use crate::{lastfm, settings, vinylvault};

const PAGE: &str = include_str!("setup.html");
static RUNNING: AtomicBool = AtomicBool::new(false);

pub enum Event {
    /// The page was opened on a phone/browser.
    Visited,
    /// Settings changed: re-read them.
    Saved,
    Closed,
}

pub struct Started {
    /// For the QR code: reachable from other devices on the network.
    pub url: String,
    /// For a browser on this machine.
    pub local_url: String,
}

/// Starts the setup page unless it's already running.
pub fn start(on_event: impl Fn(Event) + Send + 'static) -> Result<Option<Started>, String> {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Ok(None);
    }
    let port: u16 = settings::get("SETUP_PORT").and_then(|p| p.parse().ok()).unwrap_or(8737);
    let server = Server::http(("0.0.0.0", port))
        .or_else(|_| Server::http(("0.0.0.0", 0)))
        .map_err(|e| {
            RUNNING.store(false, Ordering::SeqCst);
            format!("setup page: {e}")
        })?;
    let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(port);
    let token = token().map_err(|e| {
        RUNNING.store(false, Ordering::SeqCst);
        format!("setup page: no random numbers ({e})")
    })?;
    let ip = lan_ip().unwrap_or_else(|| "127.0.0.1".into());
    let started = Started {
        url: format!("http://{ip}:{port}/{token}/"),
        local_url: format!("http://127.0.0.1:{port}/{token}/"),
    };
    std::thread::spawn(move || {
        serve(&server, &token, &on_event);
        RUNNING.store(false, Ordering::SeqCst);
        on_event(Event::Closed);
    });
    Ok(Some(started))
}

/// 16 random characters from a 32-letter alphabet (80 bits), from the OS's
/// random source. No fallback: without it, no setup page.
fn token() -> std::io::Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    const A: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    Ok(b.iter().map(|x| A[*x as usize % A.len()] as char).collect())
}

/// This machine's address on the local network (no packet is sent).
fn lan_ip() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then(|| ip.to_string())
}

#[derive(Deserialize)]
struct Form {
    user: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    shelf: String,
    theme: String,
    clock: String,
}

fn serve(server: &Server, token: &str, on_event: &impl Fn(Event)) {
    let base = format!("/{token}");
    let mut last = Instant::now();
    let mut visited = false;
    loop {
        let req = match server.recv_timeout(Duration::from_secs(1)) {
            Ok(Some(r)) => r,
            Ok(None) => {
                if last.elapsed() > Duration::from_secs(20 * 60) && settings::get("LASTFM_USER").is_some() {
                    return;
                }
                continue;
            }
            Err(_) => return,
        };
        let (url, query) = match req.url().split_once('?') {
            Some((u, q)) => (u.to_string(), q.to_string()),
            None => (req.url().to_string(), String::new()),
        };
        let Some(path) = url.strip_prefix(&base) else {
            let _ = req.respond(Response::from_string("Not found").with_status_code(404));
            continue;
        };
        last = Instant::now();
        let method = req.method().clone();
        match (method, path) {
            (Method::Get, "") => {
                let to = format!("{base}/");
                let _ = req.respond(Response::empty(302).with_header(header("Location", &to)));
            }
            (Method::Get, "/") => {
                if !visited {
                    visited = true;
                    on_event(Event::Visited);
                }
                let _ = req.respond(Response::from_string(PAGE).with_header(header("Content-Type", "text/html; charset=utf-8")));
            }
            (Method::Get, "/state") => {
                let _ = req.respond(json(&state()));
            }
            (Method::Post, "/save") => {
                let mut req = req;
                let mut body = String::new();
                let ok = req.as_reader().take(8 * 1024).read_to_string(&mut body).is_ok();
                let result = if ok { save(&body) } else { Err("Couldn't read the form.".into()) };
                let reply = match result {
                    Ok(note) => {
                        on_event(Event::Saved);
                        serde_json::json!({ "ok": true, "note": note })
                    }
                    Err(e) => serde_json::json!({ "ok": false, "error": e }),
                };
                let _ = req.respond(json(&reply));
            }
            // "Connect with Last.fm": off to Last.fm, which sends the browser
            // back to /back with a token.
            (Method::Get, "/connect") => {
                let back = host(&req).map(|h| format!("http://{h}{base}/back"));
                let reply = match (lastfm::builtin_key(), lastfm::builtin_secret(), back) {
                    (Some(key), Some(_), Some(back)) => Response::empty(302).with_header(header("Location", &lastfm::auth_url(key, &back))),
                    _ => Response::empty(404),
                };
                let _ = req.respond(reply);
            }
            (Method::Get, "/back") => {
                let token = query.split('&').find_map(|kv| kv.strip_prefix("token=")).unwrap_or("");
                let result = match (lastfm::builtin_key(), lastfm::builtin_secret()) {
                    (Some(key), Some(secret)) if !token.is_empty() && token.len() < 128 => {
                        let api = settings::get("LASTFM_API_URL").unwrap_or_else(|| lastfm::API_URL.into());
                        lastfm::session_user(&api, key, secret, token).and_then(|name| settings::save(&[("LASTFM_USER", Some(&name))]))
                    }
                    _ => Err("no login".into()),
                };
                let to = match result {
                    Ok(()) => {
                        on_event(Event::Saved);
                        format!("{base}/?connected")
                    }
                    Err(e) => {
                        eprintln!("Last.fm login failed: {e}");
                        format!("{base}/?login-failed")
                    }
                };
                let _ = req.respond(Response::empty(302).with_header(header("Location", &to)));
            }
            (Method::Post, "/done") => {
                let _ = req.respond(json(&serde_json::json!({ "ok": true })));
                return;
            }
            _ => {
                let _ = req.respond(Response::from_string("Not found").with_status_code(404));
            }
        }
    }
}

/// The address the browser used to reach us (so Last.fm can send it back
/// there), if it looks like a plain host[:port].
fn host(req: &tiny_http::Request) -> Option<String> {
    let h = req.headers().iter().find(|h| h.field.equiv("Host"))?.value.as_str().to_string();
    let ok = !h.is_empty() && h.len() < 100 && h.bytes().all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b));
    ok.then_some(h)
}

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k, v).expect("valid header")
}

fn json(v: &serde_json::Value) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(v.to_string())
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Cache-Control", "no-store"))
}

/// What the page shows. The API key itself is never sent back.
fn state() -> serde_json::Value {
    serde_json::json!({
        "user": settings::get("LASTFM_USER").unwrap_or_default(),
        "has_key": settings::get("LASTFM_API_KEY").is_some(),
        "builtin_key": lastfm::builtin_key().is_some(),
        "connect": lastfm::builtin_key().is_some() && lastfm::builtin_secret().is_some(),
        "shelf": settings::get("VINYLVAULT_SHELF").unwrap_or_default(),
        "theme": settings::get("THEME").unwrap_or_else(|| "vinyl".into()),
        "clock": settings::get("CLOCK").unwrap_or_else(|| "24h".into()),
    })
}

/// Checks what was entered, then saves it. Returns a short note for the page.
fn save(body: &str) -> Result<String, String> {
    let f: Form = serde_json::from_str(body).map_err(|_| "Couldn't read the form.")?;
    let (user, key, shelf) = (f.user.trim(), f.key.trim(), f.shelf.trim());
    if !matches!(f.theme.as_str(), "vinyl" | "poster" | "ambient") || !matches!(f.clock.as_str(), "24h" | "12h") {
        return Err("Unknown look or clock.".into());
    }
    if user.is_empty() || user.len() > 64 {
        return Err("Enter your Last.fm user name.".into());
    }
    // Empty key field = keep the saved one, else the built-in one.
    let own = if key.is_empty() { settings::get("LASTFM_API_KEY") } else { Some(key.to_string()) };
    let Some(key) = own.clone().or_else(|| lastfm::builtin_key().map(String::from)) else {
        return Err("Enter your Last.fm API key.".into());
    };
    let cfg = lastfm::Config {
        user: user.into(),
        api_key: key.clone(),
        api_url: settings::get("LASTFM_API_URL").unwrap_or_else(|| lastfm::API_URL.into()),
        poll: Duration::from_secs(2),
    };
    let note = match lastfm::check(&cfg)? {
        Some(t) if t.playing => format!("Connected. Playing now: {} – {}", t.title, t.artist),
        Some(t) => format!("Connected. Last played: {} – {}", t.title, t.artist),
        None => "Connected. Play something and it shows up on the display.".into(),
    };
    if !shelf.is_empty() {
        vinylvault::load_once(shelf).map_err(|_| "That VinylVault shelf link doesn't work. Copy it again from the app?")?;
    }
    settings::save(&[
        ("LASTFM_USER", Some(user)),
        ("LASTFM_API_KEY", own.as_deref()),
        ("VINYLVAULT_SHELF", (!shelf.is_empty()).then_some(shelf)),
        ("THEME", Some(&f.theme)),
        ("CLOCK", Some(&f.clock)),
    ])?;
    Ok(note)
}
