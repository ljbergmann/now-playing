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
    random(16, b"abcdefghijkmnpqrstuvwxyz23456789")
}

/// `n` random characters from `alphabet` (whose length divides 256, so no bias).
fn random(n: usize, alphabet: &[u8]) -> std::io::Result<String> {
    let mut b = vec![0u8; n];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|x| alphabet[*x as usize % alphabet.len()] as char).collect())
}

const URL_SAFE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// PKCE (S256): base64url, no padding, of SHA-256(verifier).
fn challenge(verifier: &str) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(verifier.as_bytes());
    let mut out = String::new();
    for chunk in d.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(URL_SAFE[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// A query parameter, percent-decoded.
fn param(query: &str, name: &str) -> Option<String> {
    let raw = query.split('&').find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='))?;
    let b = raw.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                out.push(u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).ok()?, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Shown in VinylVault's list of connected displays.
fn display_name() -> String {
    let mut buf = [0u8; 64];
    let host = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) } == 0;
    let host = if host { String::from_utf8_lossy(&buf[..buf.iter().position(|&b| b == 0).unwrap_or(0)]).to_string() } else { String::new() };
    let host = host.split('.').next().unwrap_or("").to_string(); // "Mac.home" → "Mac"
    let name = if host.is_empty() { "Now Playing".to_string() } else { format!("Now Playing · {host}") };
    name.chars().take(60).collect()
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Swaps VinylVault's one-time code for the display's read-only pass.
fn vinylvault_token(site: &str, code: &str, verifier: &str) -> Result<(String, String), String> {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).user_agent("now-playing-display/0.3").build();
    let resp = agent
        .post(&format!("{site}/displays/token"))
        .set("Content-Type", "application/json")
        .send_string(&serde_json::json!({ "code": code, "verifier": verifier }).to_string())
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("VinylVault said no (HTTP {code})"),
            ureq::Error::Transport(t) => format!("can't reach VinylVault ({})", t.kind()),
        })?;
    let mut body = String::new();
    resp.into_reader().take(64 * 1024).read_to_string(&mut body).map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let token = v["token"].as_str().filter(|t| t.starts_with("vvd_")).ok_or("no pass in VinylVault's answer")?;
    Ok((token.to_string(), v["name"].as_str().unwrap_or("").to_string()))
}

/// This machine's address on the local network (no packet is sent).
fn lan_ip() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then(|| ip.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn pkce_challenge_is_base64url_sha256() {
        // Expected values from Python: base64.urlsafe_b64encode(sha256(v)).rstrip("=")
        assert_eq!(super::challenge("dBjftJeZ4CVP-mJ92oSl3nDvuSu3yZsfmD-YC2Q47c0"), "ZPfezKB-pMKjuwE3GECW9VKl2qPsD8T-y6XPZnm7YN0");
        assert_eq!(super::challenge("dBjftJeZ4CVP-mJ92oSl3nDvuSu3yZsfmD-YC2Q47cM"), "30KIVHMaSrqbuLlgSm5yY5jebdK7E7q4GVq_xIZTzBU");
    }

    #[test]
    fn query_params_are_decoded() {
        assert_eq!(super::param("code=a%2Fb+c&state=x", "code").as_deref(), Some("a/b c"));
        assert_eq!(super::param("xcode=1&code=2", "code").as_deref(), Some("2"));
        assert_eq!(super::param("code=%4", "code").as_deref(), Some("%4"));
        assert_eq!(super::param("state=x", "code"), None);
    }
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
    // "Connect with VinylVault" in progress: (verifier, state, site).
    let mut pending: Option<(String, String, String)> = None;
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
            // "Connect with VinylVault": like Last.fm, plus PKCE so a stolen
            // code is useless without the verifier only this display knows.
            (Method::Get, "/vinylvault/connect") => {
                let site = vinylvault::site();
                let secure = site.starts_with("https://") || site.starts_with("http://127.0.0.1") || site.starts_with("http://localhost");
                let reply = match (host(&req), random(64, URL_SAFE), random(32, URL_SAFE)) {
                    (Some(h), Ok(verifier), Ok(state)) if secure => {
                        let back = format!("http://{h}{base}/vinylvault/back");
                        let to = format!(
                            "{}/connect?name={}&cb={}&challenge={}&state={}",
                            site,
                            urlencode(&display_name()),
                            urlencode(&back),
                            challenge(&verifier),
                            state
                        );
                        pending = Some((verifier, state, site));
                        Response::empty(302).with_header(header("Location", &to))
                    }
                    _ => Response::empty(500),
                };
                let _ = req.respond(reply);
            }
            (Method::Get, "/vinylvault/back") => {
                let result = match (&pending, param(&query, "state"), param(&query, "code")) {
                    (_, _, _) if param(&query, "error").is_some() => Err("denied".to_string()),
                    (Some((verifier, state, site)), Some(s), Some(code)) if s == *state => vinylvault_token(site, &code, verifier)
                        .and_then(|(token, owner)| {
                            settings::save(&[
                                ("VINYLVAULT_TOKEN", Some(&token)),
                                ("VINYLVAULT_TOKEN_SITE", Some(site)),
                                ("VINYLVAULT_OWNER", Some(&owner)),
                            ])
                        }),
                    _ => Err("this link doesn't belong to this setup".to_string()),
                };
                pending = None;
                let to = match result {
                    Ok(()) => {
                        on_event(Event::Saved);
                        format!("{base}/?vv-connected")
                    }
                    Err(e) => {
                        eprintln!("VinylVault connect: {e}");
                        format!("{base}/?vv-failed")
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
        "vv_owner": settings::get("VINYLVAULT_TOKEN").map(|_| settings::get("VINYLVAULT_OWNER").unwrap_or_default()),
        "vv_site": vinylvault::site(),
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
        vinylvault::check_link(shelf).map_err(|_| "That VinylVault shelf link doesn't work. Copy it again from the app?")?;
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
