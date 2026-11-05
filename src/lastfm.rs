//! Last.fm polling. Runs on its own thread, downloads artwork there too,
//! and only sends a `Track` when something actually changed.

use std::io::Read;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use image::RgbaImage;
use serde::Deserialize;

/// Last.fm's grey-star placeholder image. Treated as "no artwork".
const PLACEHOLDER: &str = "2a96cbd8b46e442fc41c2b86b821562f";

pub const API_URL: &str = "https://ws.audioscrobbler.com/2.0/";

/// The app's own API key, built in by `pi/build.sh` / `mac/bundle.sh` from
/// ~/.config/now-playing/build.env (never in the repo). With it, people only
/// need their Last.fm user name. A key in the settings still wins.
pub fn builtin_key() -> Option<&'static str> {
    option_env!("LASTFM_APP_KEY").map(str::trim).filter(|k| !k.is_empty())
}

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
    /// Your play counts for it; arrives a moment after the track itself.
    pub stats: Option<Stats>,
}

/// Your Last.fm numbers for the playing track.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub loved: bool,
    /// Your scrobbles of this track so far (the playing one isn't counted yet).
    pub track_plays: Option<u64>,
    /// Your scrobbles of this artist so far.
    pub artist_plays: Option<u64>,
}

impl Stats {
    /// One quiet line, e.g. "♥ · 38th play · 1,204 plays of Ariana Grande".
    /// Parts that aren't known are left out; `None` when nothing is.
    pub fn line(&self, artist: &str) -> Option<String> {
        let mut parts = Vec::new();
        if self.loved {
            parts.push(HEART.to_string());
        }
        if let Some(n) = self.track_plays {
            parts.push(if n == 0 { "first play".into() } else { format!("{} play", ordinal(n + 1)) });
        }
        // Like the track's number, the artist's counts the play that's on now
        // (Last.fm hasn't scrobbled it yet), so it's never below the track's.
        // No artist count before the first play of them: "first play" says it.
        if let Some(n) = self.artist_plays.filter(|&n| n > 0).map(|n| n + 1) {
            let plays = if n == 1 { "play" } else { "plays" };
            parts.push(if artist.is_empty() { format!("{} {plays}", thousands(n)) } else { format!("{} {plays} of {artist}", thousands(n)) });
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

pub const HEART: char = '♥';

/// 1st, 2nd, 3rd, 4th, 11th, 12th, 13th, 21st, 112th, ...
fn ordinal(n: u64) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{}{suffix}", thousands(n))
}

/// 1204 → "1,204".
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}


pub enum Update {
    Track(Option<Track>),
    Error(String),
}

#[derive(Clone)]
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
        stats: None,
    }))
}

/// Errors never include the URL: it contains the API key, and these messages
/// end up in the system log and on screen.
fn get(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    let resp = match agent.get(url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, resp)) => {
            // Last.fm explains API errors (bad key, unknown user) in the body.
            let msg = resp
                .into_string()
                .ok()
                .and_then(|b| serde_json::from_str::<Resp>(&b).ok())
                .and_then(|r| r.message);
            return Err(match msg {
                Some(m) => format!("Last.fm: {m} (HTTP {code})"),
                None => format!("HTTP {code}"),
            });
        }
        Err(ureq::Error::Transport(t)) => {
            return Err(match t.message() {
                Some(m) => format!("network error: {} ({m})", t.kind()),
                None => format!("network error: {}", t.kind()),
            });
        }
    };
    let mut buf = Vec::new();
    resp.into_reader()
        .take(20 * 1024 * 1024)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

/// Decodes cover art with hard limits, so a huge or oddly shaped image
/// can't eat the Pi's memory. Real covers are square and ≤ 1200 px.
fn decode_art(bytes: &[u8]) -> Option<RgbaImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(96 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let (w, h) = (img.width(), img.height());
    if w < 16 || h < 16 || w.max(h) > 4 * w.min(h) {
        return None;
    }
    Some(img.to_rgba8())
}

fn fetch_art(agent: &ureq::Agent, url: &str) -> Option<RgbaImage> {
    if url.is_empty() {
        return None;
    }
    // Fall back to the standard 300px size if the big one isn't available.
    let fallback = url.replace("/770x0/", "/300x300/");
    for u in [url, fallback.as_str()] {
        if let Some(img) = get(agent, u).ok().and_then(|b| decode_art(&b)) {
            return Some(img);
        }
        if u == fallback {
            break;
        }
    }
    None
}

/// Polls until `send` returns false or `stop` is set (e.g. the account changed).
pub fn spawn(cfg: Config, stop: Arc<AtomicBool>, send: impl Fn(Update) -> bool + Send + 'static) {
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
        // The playing track as first seen, and its length, for timing the polls.
        let mut current: Option<(String, String)> = None;
        let mut since = std::time::Instant::now();
        let mut duration: Option<Duration> = None;
        let mut stats: Option<Stats> = None;

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
                        let playing = track.as_ref().filter(|t| t.playing).map(|t| (t.artist.clone(), t.title.clone()));
                        // Same song, something else changed: keep its stats.
                        let track = track.map(|mut t| {
                            if playing.is_some() && playing == current {
                                t.stats = stats.clone();
                            }
                            t
                        });
                        let sent = track.clone();
                        if !send(Update::Track(track)) {
                            return;
                        }
                        // After sending, so the track shows at once: its length (for
                        // timing the next polls) and your play counts (a follow-up).
                        if playing != current {
                            since = std::time::Instant::now();
                            let info = playing.as_ref().and_then(|(a, t)| track_info(&agent, &cfg, a, t));
                            duration = info.as_ref().and_then(duration_of);
                            stats = playing.as_ref().and_then(|(a, _)| {
                                let s = Stats { artist_plays: artist_plays(&agent, &cfg, a), ..info.as_ref().map(track_stats).unwrap_or_default() };
                                (s != Stats::default()).then_some(s)
                            });
                            current = playing;
                            if let (Some(s), Some(mut t)) = (&stats, sent) {
                                t.stats = Some(s.clone());
                                if !send(Update::Track(Some(t))) {
                                    return;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    failures += 1;
                    if failures == 1 || failures % 20 == 0 {
                        eprintln!("Last.fm poll failed: {e}");
                    }
                    if !send(Update::Error(e)) {
                        return;
                    }
                }
            }
            // Back off gently while the network or API is unhappy.
            let wait = if failures == 0 {
                let d = if current.is_some() { duration } else { None };
                next_wait(cfg.poll, since.elapsed(), d)
            } else {
                (cfg.poll * 2u32.saturating_pow(failures.min(5))).min(Duration::from_secs(60))
            };
            let until = std::time::Instant::now() + wait;
            while std::time::Instant::now() < until {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_millis(100).min(until.saturating_duration_since(std::time::Instant::now())));
            }
            if stop.load(Ordering::Relaxed) {
                return;
            }
        }
    });
}

/// `track.getInfo` for the playing track, with your play count and loved
/// flag (`username`). Also gives its length, for timing the polls.
fn track_info(agent: &ureq::Agent, cfg: &Config, artist: &str, title: &str) -> Option<serde_json::Value> {
    let url = format!(
        "{}?method=track.getInfo&artist={}&track={}&username={}&api_key={}&format=json",
        cfg.api_url,
        encode(artist),
        encode(title),
        encode(&cfg.user),
        encode(&cfg.api_key)
    );
    let body = get(agent, &url).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Your scrobbles of an artist (`artist.getInfo` with `username`).
fn artist_plays(agent: &ureq::Agent, cfg: &Config, artist: &str) -> Option<u64> {
    let url = format!(
        "{}?method=artist.getInfo&artist={}&username={}&api_key={}&format=json",
        cfg.api_url,
        encode(artist),
        encode(&cfg.user),
        encode(&cfg.api_key)
    );
    let body = get(agent, &url).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&body).ok()?;
    count(&v["artist"]["stats"]["userplaycount"])
}

/// Last.fm sends numbers as strings ("37"), sometimes as numbers.
fn count(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::String(s) => s.trim().parse().ok(),
        serde_json::Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

/// Track length in Last.fm's catalogue (often missing, then `None`).
fn duration_of(v: &serde_json::Value) -> Option<Duration> {
    let ms = count(&v["track"]["duration"])?;
    (ms >= 20_000 && ms <= 3 * 3_600_000).then(|| Duration::from_millis(ms))
}

/// Your play count and loved flag from a `track.getInfo` answer.
fn track_stats(v: &serde_json::Value) -> Stats {
    let t = &v["track"];
    Stats { loved: count(&t["userloved"]) == Some(1), track_plays: count(&t["userplaycount"]), artist_plays: None }
}

/// How long to wait before asking Last.fm again.
///
/// Track changes almost always happen when a track ends (or on a skip). We
/// first see a track a little after it really started, so its end comes a
/// little after `elapsed == duration`: around that moment, ask every second.
/// Otherwise use the base interval.
fn next_wait(base: Duration, elapsed: Duration, duration: Option<Duration>) -> Duration {
    const FAST: Duration = Duration::from_secs(1);
    let Some(d) = duration else { return base };
    let window_start = d.saturating_sub(Duration::from_secs(6));
    let window_end = d + Duration::from_secs(25); // paused or long gap: stop hurrying
    if elapsed >= window_start && elapsed <= window_end {
        FAST.min(base)
    } else if elapsed < window_start {
        // Don't sleep past the start of the window.
        base.min(window_start - elapsed).max(Duration::from_millis(500))
    } else {
        base
    }
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// One request to see whether a user name + key work. Returns the most
/// recent track (without artwork) or a message fit for the setup page.
pub fn check(cfg: &Config) -> Result<Option<Track>, String> {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(8)).user_agent("now-playing-display/0.1").build();
    let url = format!(
        "{}?method=user.getrecenttracks&user={}&api_key={}&format=json&limit=1",
        cfg.api_url,
        encode(&cfg.user),
        encode(&cfg.api_key)
    );
    let body = get(&agent, &url).map_err(|e| {
        if e.contains("User not found") {
            "Last.fm doesn't know that user name.".to_string()
        } else if e.contains("Invalid API key") || e.contains("Invalid API Key") {
            "Last.fm doesn't accept that API key.".to_string()
        } else if e.starts_with("network error") {
            "Can't reach Last.fm. Is the display online?".to_string()
        } else {
            e
        }
    })?;
    parse(&String::from_utf8(body).map_err(|e| e.to_string())?)
}

// --- "Connect with Last.fm" ------------------------------------------------
// Last.fm's web login: send the browser to `auth_url`, Last.fm sends it back
// with a token, and `session_user` swaps that for the account name. Needs the
// app's key and secret (built in, like the key; see `builtin_key`).

pub fn builtin_secret() -> Option<&'static str> {
    option_env!("LASTFM_APP_SECRET").map(str::trim).filter(|k| !k.is_empty())
}

pub fn auth_url(api_key: &str, back_to: &str) -> String {
    format!("https://www.last.fm/api/auth/?api_key={}&cb={}", encode(api_key), encode(back_to))
}

/// Last.fm's request signature: parameters sorted by name, name+value
/// concatenated, then the secret, MD5 as hex. `format` is never signed.
fn signature(params: &[(&str, &str)], secret: &str) -> String {
    let mut p = params.to_vec();
    p.sort();
    let mut s: String = p.iter().map(|(k, v)| format!("{k}{v}")).collect();
    s.push_str(secret);
    format!("{:x}", md5::compute(s.as_bytes()))
}

/// The user name that approved the login. The session key itself isn't
/// kept: reading what someone plays needs only their name.
pub fn session_user(api_url: &str, api_key: &str, secret: &str, token: &str) -> Result<String, String> {
    let params = [("api_key", api_key), ("method", "auth.getSession"), ("token", token)];
    let url = format!(
        "{api_url}?method=auth.getSession&api_key={}&token={}&api_sig={}&format=json",
        encode(api_key),
        encode(token),
        signature(&params, secret)
    );
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(8)).user_agent("now-playing-display/0.1").build();
    let body = get(&agent, &url)?;
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    v["session"]["name"].as_str().filter(|n| !n.is_empty()).map(String::from).ok_or_else(|| "Last.fm didn't confirm the login.".into())
}

#[cfg(test)]
mod tests {

    #[test]
    fn signature_sorts_and_appends_secret() {
        let p = [("token", "t"), ("method", "auth.getSession"), ("api_key", "k")];
        assert_eq!(super::signature(&p, "s"), "4b1fe62eb056c03672443be528ff2633");
    }
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
    fn polls_fast_around_the_end_of_a_track() {
        let s = Duration::from_secs;
        let base = s(2);
        let len = Some(s(200));
        assert_eq!(next_wait(base, s(10), None), base); // unknown length
        assert_eq!(next_wait(base, s(10), len), base); // mid-track
        assert_eq!(next_wait(base, s(193), len), s(1)); // stops at the window start
        assert_eq!(next_wait(base, s(194), len), s(1)); // inside the window
        assert_eq!(next_wait(base, s(215), len), s(1)); // a bit late: still fast
        assert_eq!(next_wait(base, s(240), len), base); // paused: back to normal
        assert_eq!(next_wait(s(5), s(190), len), s(4)); // never sleeps into the window
    }

    #[test]
    fn parses_track_and_artist_stats() {
        let v: serde_json::Value = serde_json::from_str(r#"{"track":{"name":"S","duration":"215000","userplaycount":"37","userloved":"1"}}"#).unwrap();
        assert_eq!(track_stats(&v), Stats { loved: true, track_plays: Some(37), artist_plays: None });
        assert_eq!(duration_of(&v), Some(Duration::from_millis(215_000)));
        let v: serde_json::Value = serde_json::from_str(r#"{"track":{"name":"S","userplaycount":0,"userloved":"0"}}"#).unwrap();
        assert_eq!(track_stats(&v), Stats { loved: false, track_plays: Some(0), artist_plays: None });
        // No username in the answer (or an error): nothing known.
        let v: serde_json::Value = serde_json::from_str(r#"{"error":6,"message":"Track not found"}"#).unwrap();
        assert_eq!(track_stats(&v), Stats::default());
        let v: serde_json::Value = serde_json::from_str(r#"{"artist":{"name":"A","stats":{"listeners":"9","playcount":"99","userplaycount":"1204"}}}"#).unwrap();
        assert_eq!(count(&v["artist"]["stats"]["userplaycount"]), Some(1204));
    }

    #[test]
    fn ordinals_and_separators() {
        let got: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 101, 111, 112, 1001].into_iter().map(ordinal).collect();
        assert_eq!(got, ["1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "23rd", "101st", "111th", "112th", "1,001st"]);
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1204), "1,204");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn stats_line() {
        let s = |loved, t, a| Stats { loved, track_plays: t, artist_plays: a };
        assert_eq!(s(true, Some(37), Some(1204)).line("Ariana Grande").as_deref(), Some("♥ · 38th play · 1,205 plays of Ariana Grande"));
        assert_eq!(s(false, Some(0), Some(0)).line("X").as_deref(), Some("first play"));
        assert_eq!(s(false, Some(0), Some(5)).line("X").as_deref(), Some("first play · 6 plays of X"));
        assert_eq!(s(false, Some(1), Some(1)).line("X").as_deref(), Some("2nd play · 2 plays of X"));
        assert_eq!(s(true, None, None).line("X").as_deref(), Some("♥"));
        assert_eq!(s(false, None, Some(2000)).line("X").as_deref(), Some("2,001 plays of X"));
        assert_eq!(s(false, None, Some(3)).line("").as_deref(), Some("4 plays"));
        assert_eq!(s(false, None, None).line("X"), None);
    }

    #[test]
    fn api_error() {
        assert!(parse(r#"{"error":10,"message":"Invalid API key"}"#).is_err());
    }
}
