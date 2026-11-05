//! Last.fm polling. Runs on its own thread, downloads artwork there too,
//! and only sends a `Track` when something actually changed.

use std::io::Read;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use image::RgbaImage;
use serde::Deserialize;

/// Last.fm's grey-star placeholder image. Treated as "no artwork".
const PLACEHOLDER: &str = "2a96cbd8b46e442fc41c2b86b821562f";

#[derive(Clone)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: String,
    pub art: Option<std::sync::Arc<RgbaImage>>,
    pub playing: bool,
    /// When it was scrobbled (only set when not playing).
    pub played_at: Option<SystemTime>,
}

impl Track {
    /// Identity for "is this a different song" (drives the crossfade).
    pub fn key(&self) -> (&str, &str, &str) {
        (&self.title, &self.artist, &self.album)
    }
}

pub enum Update {
    Track(Option<Track>),
    Error(String),
}

pub struct Config {
    pub user: String,
    pub api_key: String,
    pub api_url: String,
    pub poll: Duration,
}

// --- Last.fm JSON (only the fields we use) --------------------------------

#[derive(Deserialize)]
struct Resp {
    recenttracks: Option<Recent>,
    error: Option<i64>,
    message: Option<String>,
}
#[derive(Deserialize)]
struct Recent {
    #[serde(default, deserialize_with = "one_or_many")]
    track: Vec<RawTrack>,
}
#[derive(Deserialize)]
struct RawTrack {
    #[serde(default)]
    name: String,
    artist: Option<Text>,
    album: Option<Text>,
    #[serde(default)]
    image: Vec<Image>,
    #[serde(rename = "@attr")]
    attr: Option<Attr>,
    date: Option<Date>,
}
#[derive(Deserialize)]
struct Text {
    #[serde(rename = "#text", default)]
    text: String,
}
#[derive(Deserialize)]
struct Image {
    #[serde(rename = "#text", default)]
    url: String,
}
#[derive(Deserialize)]
struct Attr {
    nowplaying: Option<String>,
}
#[derive(Deserialize)]
struct Date {
    uts: String,
}

/// Last.fm returns an object instead of an array when there's a single track.
fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<RawTrack>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        Many(Vec<RawTrack>),
        One(RawTrack),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::Many(v) => v,
        OneOrMany::One(t) => vec![t],
    })
}

/// Last.fm only lists 300px images, but its CDN serves larger sizes by path.
fn big_art_url(images: &[Image]) -> String {
    let Some(url) = images.last().map(|i| i.url.as_str()) else { return String::new() };
    if url.is_empty() || url.contains(PLACEHOLDER) {
        return String::new();
    }
    match url.find("/i/u/") {
        Some(i) => {
            let rest = &url[i + 5..];
            match rest.find('/') {
                Some(j) => format!("{}/i/u/770x0/{}", &url[..i], &rest[j + 1..]),
                None => url.to_string(),
            }
        }
        None => url.to_string(),
    }
}

fn parse(body: &str) -> Result<Option<Track>, String> {
    let resp: Resp = serde_json::from_str(body).map_err(|e| format!("bad JSON: {e}"))?;
    if let Some(code) = resp.error {
        return Err(format!("Last.fm error {code}: {}", resp.message.unwrap_or_default()));
    }
    let Some(t) = resp.recenttracks.and_then(|r| r.track.into_iter().next()) else {
        return Ok(None);
    };
    let playing = t.attr.and_then(|a| a.nowplaying).as_deref() == Some("true");
    let played_at = if playing {
        None
    } else {
        t.date
            .and_then(|d| d.uts.parse::<u64>().ok())
            .map(|s| UNIX_EPOCH + Duration::from_secs(s))
    };
    Ok(Some(Track {
        title: t.name,
        artist: t.artist.map(|a| a.text).unwrap_or_default(),
        album: t.album.map(|a| a.text).unwrap_or_default(),
        art_url: big_art_url(&t.image),
        art: None,
        playing,
        played_at,
    }))
}

fn get(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    let resp = agent.get(url).call().map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(20 * 1024 * 1024)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

fn fetch_art(agent: &ureq::Agent, url: &str) -> Option<RgbaImage> {
    if url.is_empty() {
        return None;
    }
    // Fall back to the standard 300px size if the big one isn't available.
    let fallback = url.replace("/770x0/", "/300x300/");
    for u in [url, fallback.as_str()] {
        if let Ok(bytes) = get(agent, u) {
            if let Ok(img) = image::load_from_memory(&bytes) {
                return Some(img.to_rgba8());
            }
        }
        if u == fallback {
            break;
        }
    }
    None
}

pub fn spawn(cfg: Config, tx: Sender<Update>) {
    thread::spawn(move || {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(10))
            .user_agent("now-playing-display/0.1")
            .build();
        let url = format!(
            "{}?method=user.getrecenttracks&user={}&api_key={}&format=json&limit=1",
            cfg.api_url,
            encode(&cfg.user),
            encode(&cfg.api_key)
        );
        let mut last: Option<(String, String, String, String, bool, Option<SystemTime>)> = None;
        let mut art_cache: Option<(String, Option<std::sync::Arc<RgbaImage>>)> = None;
        let mut failures: u32 = 0;

        loop {
            let result = get(&agent, &url)
                .and_then(|b| String::from_utf8(b).map_err(|e| e.to_string()))
                .and_then(|s| parse(&s));
            match result {
                Ok(track) => {
                    failures = 0;
                    let sig = track.as_ref().map(|t| {
                        (t.title.clone(), t.artist.clone(), t.album.clone(), t.art_url.clone(), t.playing, t.played_at)
                    });
                    if sig != last {
                        last = sig;
                        let track = track.map(|mut t| {
                            let cached = art_cache.as_ref().filter(|(u, _)| *u == t.art_url);
                            t.art = match cached {
                                Some((_, art)) => art.clone(),
                                None => {
                                    let art = fetch_art(&agent, &t.art_url).map(std::sync::Arc::new);
                                    art_cache = Some((t.art_url.clone(), art.clone()));
                                    art
                                }
                            };
                            t
                        });
                        if tx.send(Update::Track(track)).is_err() {
                            return;
                        }
                    }
                }
                Err(e) => {
                    failures += 1;
                    if failures == 1 || failures % 20 == 0 {
                        eprintln!("Last.fm poll failed: {e}");
                    }
                    if tx.send(Update::Error(e)).is_err() {
                        return;
                    }
                }
            }
            // Back off gently while the network or API is unhappy.
            let wait = if failures == 0 {
                cfg.poll
            } else {
                (cfg.poll * 2u32.saturating_pow(failures.min(5))).min(Duration::from_secs(60))
            };
            thread::sleep(wait);
        }
    });
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_now_playing_and_upgrades_art() {
        let json = r##"{"recenttracks":{"track":[{"name":"Song","artist":{"#text":"Band"},
            "album":{"#text":"LP"},"image":[{"#text":"https://lastfm.freetls.fastly.net/i/u/34s/abc.jpg"},
            {"#text":"https://lastfm.freetls.fastly.net/i/u/300x300/abc.jpg"}],"@attr":{"nowplaying":"true"}}]}}"##;
        let t = parse(json).unwrap().unwrap();
        assert!(t.playing);
        assert_eq!(t.title, "Song");
        assert_eq!(t.art_url, "https://lastfm.freetls.fastly.net/i/u/770x0/abc.jpg");
    }

    #[test]
    fn single_object_track_and_placeholder() {
        let json = r##"{"recenttracks":{"track":{"name":"Old","artist":{"#text":"X"},"album":{"#text":""},
            "image":[{"#text":"https://x/i/u/300x300/2a96cbd8b46e442fc41c2b86b821562f.png"}],"date":{"uts":"1700000000"}}}}"##;
        let t = parse(json).unwrap().unwrap();
        assert!(!t.playing);
        assert!(t.art_url.is_empty());
        assert_eq!(t.played_at, Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000)));
    }

    #[test]
    fn api_error() {
        assert!(parse(r#"{"error":10,"message":"Invalid API key"}"#).is_err());
    }
}
