//! Fake Last.fm for development: `cargo run --example mock_lastfm`
//! then run the display with LASTFM_API_URL=http://localhost:9911/2.0/
//!
//! Switch scenarios with: curl -X POST localhost:9911/mode/<playing|stopped|noart|long>
//!
//! MOCK_ART_DELAY_MS slows down cover downloads (e.g. 2000).
//!
//! MOCK_ARTIST / MOCK_ALBUM / MOCK_TITLE override the "playing" track (e.g. to
//! try the VinylVault catalogue fallback with a real album).
//!
//! Fake VinylVault "Connect" (run the display with VINYLVAULT_URL=http://127.0.0.1:9911):
//! /connect approves at once, /displays/token checks the PKCE verifier,
//! /me/shelf.json needs the pass. `curl -X POST localhost:9911/revoke` = disconnected.
//!
//! track.getInfo / artist.getInfo answer with your play counts, per scenario:
//! playing = loved, 37 plays, 1,203 of the artist; long = not loved, never
//! played, 12,345 of the artist; noart = unknown (no line at all).
//!
//! Also a fake VinylVault shelf that owns the test album:
//!   VINYLVAULT_SHELF=http://localhost:9911/shelf/test
//!   curl -X POST localhost:9911/finish/<solid|marbled|splatter|swirl|galaxy|picture_disc|half_and_half|clear|glow>

use std::io::Cursor;
use std::time::{SystemTime, UNIX_EPOCH};

use tiny_http::{Header, Method, Response, Server};

fn art(hue: f32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(600, 600, |x, y| {
        let t = (x + y) as f32 / 1200.0;
        let (r, g, b) = hsl((hue + 60.0 * t) % 360.0, 0.65, 0.55 - 0.3 * t);
        let (dx, dy) = (x as f32 - 210.0, y as f32 - 370.0);
        if dx * dx + dy * dy < 130.0 * 130.0 {
            let (r2, g2, b2) = hsl((hue + 180.0) % 360.0, 0.8, 0.85);
            image::Rgb([((r + r2) / 2.0) as u8, ((g + g2) / 2.0) as u8, ((b + b2) / 2.0) as u8])
        } else {
            image::Rgb([r as u8, g as u8, b as u8])
        }
    });
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn hsl(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    ((r + m) * 255.0, (g + m) * 255.0, (b + m) * 255.0)
}


fn main() {
    let server = Server::http("127.0.0.1:9911").unwrap();
    println!("mock Last.fm on http://localhost:9911/2.0/");
    let mut mode = String::from("playing");
    let mut finish = String::from("marbled");
    let mut challenge = String::new();
    let mut revoked = false;
    for mut req in server.incoming_requests() {
        let url = req.url().to_string();
        if url.starts_with("/connect?") {
            let q = &url[9..];
            let get = |k: &str| q.split('&').find_map(|kv| kv.strip_prefix(k)?.strip_prefix('=')).unwrap_or("").to_string();
            challenge = get("challenge");
            let cb = decode(&get("cb"));
            let to = format!("{cb}?code=mockcode&state={}", get("state"));
            let _ = req.respond(Response::empty(302).with_header(Header::from_bytes("Location", to).unwrap()));
            continue;
        }
        if *req.method() == Method::Post && url == "/displays/token" {
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let ok = v["code"] == "mockcode" && pkce(v["verifier"].as_str().unwrap_or("")) == challenge;
            challenge.clear(); // codes work once
            revoked = false;
            let resp = if ok {
                Response::from_string(r#"{"token":"vvd_mock","name":"Mock Collector","display_name":"x"}"#)
            } else {
                Response::from_string(r#"{"detail":"bad code or verifier"}"#).with_status_code(400)
            };
            let _ = req.respond(resp);
            continue;
        }
        if *req.method() == Method::Post && url == "/revoke" {
            revoked = true;
            let _ = req.respond(Response::from_string("revoked"));
            continue;
        }
        if url == "/me/shelf.json" {
            let auth = req.headers().iter().find(|h| h.field.equiv("Authorization")).map(|h| h.value.to_string());
            if revoked || auth.as_deref() != Some("Bearer vvd_mock") {
                let _ = req.respond(Response::from_string(r#"{"detail":"unauthorized"}"#).with_status_code(401));
                continue;
            }
        }
        if *req.method() == Method::Post && url.starts_with("/mode/") {
            mode = url[6..].to_string();
            let _ = req.respond(Response::from_string(mode.clone()));
            continue;
        }
        if *req.method() == Method::Post && url.starts_with("/finish/") {
            finish = url[8..].to_string();
            let _ = req.respond(Response::from_string(finish.clone()));
            continue;
        }
        // "Connect with Last.fm": any token with a signature logs in as "mockuser".
        if url.contains("method=auth.getSession") {
            let body = if url.contains("api_sig=") { r#"{"session":{"name":"mockuser","key":"k","subscriber":0}}"# } else { r#"{"error":13,"message":"Invalid method signature supplied"}"# };
            let _ = req.respond(Response::from_string(body));
            continue;
        }
        if (url.starts_with("/shelf/") && url.ends_with(".json")) || url == "/me/shelf.json" {
            let (name, hexes) = match finish.as_str() {
                "solid" => ("Translucent Blue", r##"["#3D70D6"]"##),
                "splatter" => ("Yellow w/ Black & Red Splatter", r##"["#F5D140","#1A1A1C","#CC292E"]"##),
                "swirl" => ("Purple, Pink & Cyan Swirl", r##"["#824DBF","#F599BD","#4DC7DB"]"##),
                "galaxy" => ("Black Galaxy", r##"["#1A1A1C","#824DBF"]"##),
                "picture_disc" => ("Picture Disc", "[]"),
                "half_and_half" => ("Orange / Black Half & Half", r##"["#F28726","#1A1A1C"]"##),
                "clear" => ("Clear", "[]"),
                "glow" => ("Glow In The Dark", "[]"),
                _ => ("Red Marbled", r##"["#CC292E","#F7F7F5"]"##),
            };
            let body = format!(
                r#"{{"name":"test","records":[{{"release_title":"Signals From the Lake (Deluxe Edition)","artist_name":"Elsa Morrow","release_year":2024,"pressing":{{"colourway_name":"{name}","finish":"{finish}","colour_hexes":{hexes},"edition_size":500,"is_limited":true,"is_numbered":false,"format":"LP","disc_count":1}}}}]}}"#
            );
            let json = Header::from_bytes("Content-Type", "application/json").unwrap();
            let _ = req.respond(Response::from_string(body).with_header(json));
            continue;
        }
        if url.starts_with("/i/u/") {
            // MOCK_ART_DELAY_MS: a slow image server.
            if let Some(ms) = std::env::var("MOCK_ART_DELAY_MS").ok().and_then(|v| v.parse().ok()) {
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
            // Track-specific art; the 770px size exists, like on the real CDN.
            let hue = if url.contains("b.png") { 20.0 } else { 280.0 };
            let png = Header::from_bytes("Content-Type", "image/png").unwrap();
            let _ = req.respond(Response::from_data(art(hue)).with_header(png));
            continue;
        }
        if url.contains("method=track.getInfo") || url.contains("method=artist.getInfo") {
            let track = url.contains("method=track.getInfo");
            let body = match (mode.as_str(), track) {
                ("noart", _) => r#"{"error":6,"message":"Track not found"}"#.to_string(),
                ("long", true) => r#"{"track":{"name":"x","duration":"0","userplaycount":"0","userloved":"0"}}"#.to_string(),
                ("long", false) => r#"{"artist":{"name":"x","stats":{"listeners":"10","playcount":"99999","userplaycount":"12345"}}}"#.to_string(),
                (_, true) => r#"{"track":{"name":"x","duration":"0","userplaycount":"37","userloved":"1"}}"#.to_string(),
                (_, false) => r#"{"artist":{"name":"x","stats":{"listeners":"10","playcount":"99999","userplaycount":"1203"}}}"#.to_string(),
            };
            let json = Header::from_bytes("Content-Type", "application/json").unwrap();
            let _ = req.respond(Response::from_string(body).with_header(json));
            continue;
        }
        let base = "http://localhost:9911/i/u/300x300/";
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let (name, artist, img, extra) = match mode.as_str() {
            "stopped" => ("Slow Burn", "Kacey Lane", format!("{base}b.png"), format!(r#","date":{{"uts":"{}"}}"#, now - 14 * 60)),
            "noart" => ("Untitled Demo", "Unknown Artist", format!("{base}2a96cbd8b46e442fc41c2b86b821562f.png"), r#","@attr":{"nowplaying":"true"}"#.into()),
            "long" => ("A Remarkably Long Song Title That Keeps Going Well Past Two Lines Of Text On Small Screens", "Orchestra of the Very Long Name and Friends", format!("{base}a.png"), r#","@attr":{"nowplaying":"true"}"#.into()),
            _ => ("Ultraviolet Hours (Extended Version)", "Elsa Morrow & The Night Shift", format!("{base}a.png"), r#","@attr":{"nowplaying":"true"}"#.into()),
        };
        let custom = mode == "playing";
        let env = |k: &str, d: &str| if custom { std::env::var(k).unwrap_or_else(|_| d.to_string()) } else { d.to_string() };
        let (name, artist, album) = (env("MOCK_TITLE", name), env("MOCK_ARTIST", artist), env("MOCK_ALBUM", "Signals From the Lake"));
        let body = format!(
            r##"{{"recenttracks":{{"track":[{{"name":"{name}","artist":{{"#text":"{artist}"}},"album":{{"#text":"{album}"}},"image":[{{"size":"small","#text":"{img}"}},{{"size":"extralarge","#text":"{img}"}}]{extra}}}]}}}}"##
        );
        let json = Header::from_bytes("Content-Type", "application/json").unwrap();
        let _ = req.respond(Response::from_string(body).with_header(json));
    }
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'?'));
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn pkce(verifier: &str) -> String {
    use sha2::Digest;
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let d = sha2::Sha256::digest(verifier.as_bytes());
    let mut out = String::new();
    for c in d.chunks(3) {
        let n = c.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..=c.len() {
            out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}
