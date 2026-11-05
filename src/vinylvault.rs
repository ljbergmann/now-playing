//! VinylVault: which pressing of the playing album do *you* own?
//!
//! Reads a shared VinylVault shelf (`/shelf/{slug}.json`, public and read-only:
//! it only exists while the owner has sharing switched on), refreshes it every
//! 15 minutes, and matches Last.fm's album + artist against it. The vinyl look
//! then draws the record as that pressing: marbled, splatter, clear, ...
//!
//! Albums that aren't on the shelf (or with no shelf set up) fall back to a real
//! pressing of that album from the public VinylVault catalogue (`/search`),
//! picked once per album so it doesn't change between plays.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::Duration;

use serde::Deserialize;

const REFRESH: Duration = Duration::from_secs(15 * 60);
const DEFAULT_BASE: &str = "https://vinylvault.sky-lab.de";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Finish {
    Solid,
    Marbled,
    Splatter,
    Swirl,
    Galaxy,
    PictureDisc,
    HalfAndHalf,
    Clear,
    Glow,
}

impl Finish {
    fn parse(s: &str) -> Finish {
        match s {
            "marbled" => Finish::Marbled,
            "splatter" => Finish::Splatter,
            "swirl" => Finish::Swirl,
            "galaxy" => Finish::Galaxy,
            "picture_disc" => Finish::PictureDisc,
            "half_and_half" => Finish::HalfAndHalf,
            "clear" => Finish::Clear,
            "glow" => Finish::Glow,
            _ => Finish::Solid,
        }
    }
}

/// How a pressing looks, ready for the renderer.
#[derive(Clone, Debug, PartialEq)]
pub struct Pressing {
    pub name: String,
    pub finish: Finish,
    pub colors: Vec<[u8; 3]>,
    pub edition_size: Option<u32>,
    pub is_limited: bool,
    pub is_numbered: bool,
    /// From your shelf (true), or a catalogue pressing of the album (false).
    pub owned: bool,
}

impl Pressing {
    /// True for a plain black record (nothing special to draw).
    pub fn is_plain_black(&self) -> bool {
        self.finish == Finish::Solid && self.colors.iter().all(|c| c.iter().all(|&v| v < 48))
    }

    /// The name to show: Discogs-style names put the official one in brackets,
    /// "Blue Translucent Marbled [Moonstone Blue Marbled] Signed Insert".
    pub fn display_name(&self) -> String {
        let n = self.name.trim();
        match (n.find('['), n.find(']')) {
            (Some(a), Some(b)) if b > a + 1 => n[a + 1..b].trim().to_string(),
            _ if n.is_empty() => "Coloured vinyl".to_string(),
            _ => n.to_string(),
        }
    }

    /// "PINK MARBLED · LTD. 500"
    pub fn caption(&self) -> String {
        let mut parts = vec![self.display_name()];
        match (self.edition_size, self.is_limited, self.is_numbered) {
            (Some(n), _, true) => parts.push(format!("Numbered, {n}")),
            (Some(n), _, false) => parts.push(format!("Ltd. {n}")),
            (None, _, true) => parts.push("Numbered".into()),
            (None, true, _) => parts.push("Limited".into()),
            _ => {}
        }
        parts.join(" · ")
    }
}

#[derive(Clone, Debug)]
pub struct Record {
    title_key: String,
    artist_key: String,
    pub pressing: Option<Pressing>,
}

pub type Shelf = Arc<RwLock<Vec<Record>>>;

// --- wire format (see VinylVault backend: GET /shelf/{slug}.json) -----------

#[derive(Deserialize)]
struct ShelfJson {
    #[serde(default)]
    records: Vec<RecordJson>,
}
// Text fields may be `null` (a copy described only by its owner), so they are Options.
#[derive(Deserialize)]
struct RecordJson {
    release_title: Option<String>,
    artist_name: Option<String>,
    pressing: Option<PressingJson>,
}
#[derive(Deserialize)]
struct PressingJson {
    colourway_name: Option<String>,
    finish: Option<String>,
    #[serde(default)]
    colour_hexes: Option<Vec<String>>,
    edition_size: Option<u32>,
    is_limited: Option<bool>,
    is_numbered: Option<bool>,
    /// Catalogue search results only: use "accepted" rows.
    review_state: Option<String>,
}

impl PressingJson {
    fn into_pressing(self, owned: bool) -> Pressing {
        Pressing {
            name: self.colourway_name.unwrap_or_default(),
            finish: Finish::parse(self.finish.as_deref().unwrap_or("")),
            colors: self.colour_hexes.unwrap_or_default().iter().filter_map(|h| hex(h)).take(6).collect(),
            edition_size: self.edition_size,
            is_limited: self.is_limited.unwrap_or(false),
            is_numbered: self.is_numbered.unwrap_or(false),
            owned,
        }
    }
}

fn hex(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn parse(body: &str) -> Result<Vec<Record>, String> {
    let shelf: ShelfJson = serde_json::from_str(body).map_err(|e| format!("VinylVault: unexpected response ({e})"))?;
    Ok(shelf
        .records
        .into_iter()
        .filter_map(|r| {
            let title = r.release_title.filter(|t| !t.is_empty())?;
            Some((title, r.artist_name.unwrap_or_default(), r.pressing))
        })
        .map(|(title, artist, pressing)| Record {
            title_key: norm_title(&title),
            artist_key: norm(&artist),
            pressing: pressing.map(|p| p.into_pressing(true)),
        })
        .collect())
}

// --- matching --------------------------------------------------------------

/// Lowercase letters and digits only.
fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// Album titles without the noise streaming services add:
/// "(Deluxe Edition)", "[Remastered 2011]", " - Single", "(feat. X)".
fn norm_title(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let lower = out.to_lowercase();
    let cut = [" - single", " - ep", " - deluxe", " - remaster", " - expanded", " - anniversary"]
        .iter()
        .filter_map(|m| lower.find(m))
        .min()
        .unwrap_or(lower.len());
    norm(&lower[..cut])
}

/// The main artist: "Bebe Rexha, YEJI & RYUJIN" → "beberexha".
fn primary_artist(s: &str) -> String {
    let lower = s.to_lowercase();
    let end = [",", " & ", " feat", " ft.", " x ", " with ", " and "]
        .iter()
        .filter_map(|m| lower.find(m))
        .min()
        .unwrap_or(lower.len());
    norm(&lower[..end])
}

fn artist_matches(key: &str, main: &str) -> bool {
    key.is_empty() || main.is_empty() || key.contains(main) || main.contains(key)
}

/// Is this Last.fm album on the shelf?
/// `None` = not on the shelf; `Some(None)` = owned, but plain black (or not
/// described), so draw the normal record; `Some(Some(p))` = owned special pressing.
pub fn lookup(shelf: &Shelf, album: &str, artist: &str) -> Option<Option<Pressing>> {
    let title = norm_title(album);
    if title.is_empty() {
        return None;
    }
    let main = primary_artist(artist);
    let records = shelf.read().ok()?;
    let r = records.iter().filter(|r| r.title_key == title).find(|r| artist_matches(&r.artist_key, &main))?;
    Some(r.pressing.clone().filter(|p| !p.is_plain_black()))
}

// --- catalogue fallback ------------------------------------------------------

#[derive(Deserialize)]
struct SearchJson {
    #[serde(default)]
    items: Vec<SearchItem>,
}
#[derive(Deserialize)]
struct SearchItem {
    variant: Option<PressingJson>,
    release_title: Option<String>,
    artist_name: Option<String>,
}

/// Raw search text: main artist + album without bracketed noise.
fn search_query(album: &str, artist: &str) -> String {
    // Search and slice the *same* string: lowercasing can change byte lengths
    // ("İ" is 2 bytes, "i̇" is 3), so an index into `lower` isn't safe on `artist`.
    let lower = artist.to_lowercase();
    let end = [",", " & ", " feat", " ft.", " x ", " with "].iter().filter_map(|m| lower.find(m)).min().unwrap_or(lower.len());
    let mut title = String::new();
    let mut depth = 0;
    for c in album.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => title.push(c),
            _ => {}
        }
    }
    format!("{} {}", lower[..end].trim(), title.trim())
}

/// Picks a pressing of exactly this album from search results. Strict on both
/// title and artist: the search is fuzzy ("rumours" also finds an orchestra's
/// tribute album and a band called Rumours). Prefers coloured pressings; the
/// pick is fixed per album (seeded by its name), not random per play.
fn pick(items: Vec<SearchItem>, album: &str, artist: &str) -> Option<Pressing> {
    let (title, main) = (norm_title(album), primary_artist(artist));
    let candidates: Vec<Pressing> = items
        .into_iter()
        .filter(|it| it.release_title.as_deref().is_some_and(|t| norm_title(t) == title))
        .filter(|it| {
            let a = norm(it.artist_name.as_deref().unwrap_or(""));
            !a.is_empty() && !main.is_empty() && artist_matches(&a, &main)
        })
        .filter_map(|it| it.variant)
        .filter(|v| v.review_state.as_deref().is_none_or(|s| s == "accepted"))
        .map(|v| v.into_pressing(false))
        .filter(|p| !p.is_plain_black())
        .collect();
    let coloured: Vec<&Pressing> = candidates.iter().filter(|p| !p.colors.is_empty()).collect();
    let pool: Vec<&Pressing> = if coloured.is_empty() { candidates.iter().collect() } else { coloured };
    if pool.is_empty() {
        return None;
    }
    let seed = format!("{main}|{title}").bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3));
    Some(pool[(seed % pool.len() as u64) as usize].clone())
}

/// Looks up a pressing of an album in the public catalogue; one request per
/// album, remembered (including "none" and failures) for the life of the process.
pub struct Catalogue {
    base: String,
    agent: ureq::Agent,
    cache: Mutex<HashMap<String, Option<Pressing>>>,
}

impl Catalogue {
    pub fn new(base: &str) -> Self {
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(6)).user_agent("now-playing-display/0.2").build();
        Catalogue { base: base.trim_end_matches('/').to_string(), agent, cache: Mutex::new(HashMap::new()) }
    }

    pub fn pressing_for(&self, album: &str, artist: &str) -> Option<Pressing> {
        let key = format!("{}|{}", primary_artist(artist), norm_title(album));
        if norm_title(album).is_empty() {
            return None;
        }
        if let Some(hit) = self.cache.lock().ok()?.get(&key) {
            return hit.clone();
        }
        let url = format!("{}/search?q={}", self.base, encode(&search_query(album, artist)));
        let found = match self.agent.get(&url).call() {
            Ok(resp) => {
                let mut body = String::new();
                let ok = std::io::Read::read_to_string(&mut std::io::Read::take(resp.into_reader(), 8 * 1024 * 1024), &mut body).is_ok();
                if ok { serde_json::from_str::<SearchJson>(&body).ok().and_then(|s| pick(s.items, album, artist)) } else { None }
            }
            Err(_) => None, // quietly: the plain black record is a fine fallback
        };
        if let Some(p) = &found {
            eprintln!("VinylVault catalogue: {} — {} on {}", artist, album, p.caption());
        }
        if let Ok(mut c) = self.cache.lock() {
            c.insert(key, found.clone());
        }
        found
    }
}

/// The site behind a shelf setting (so a self-hosted VinylVault works too).
pub fn base_url(shelf_setting: Option<&str>) -> String {
    match shelf_setting.map(str::trim) {
        Some(s) if s.starts_with("http://") || s.starts_with("https://") => match s.find("/shelf/") {
            Some(i) => s[..i].to_string(),
            None => DEFAULT_BASE.into(),
        },
        _ => DEFAULT_BASE.into(),
    }
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// --- fetching --------------------------------------------------------------

/// Accepts a slug ("ab12cd"), a shelf link ("https://…/shelf/ab12cd") or the JSON URL.
pub fn shelf_url(setting: &str) -> String {
    let s = setting.trim().trim_end_matches('/');
    if s.starts_with("http://") || s.starts_with("https://") {
        if s.ends_with(".json") { s.to_string() } else { format!("{s}.json") }
    } else {
        format!("{DEFAULT_BASE}/shelf/{s}.json")
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).user_agent("now-playing-display/0.2").build()
}

fn fetch(agent: &ureq::Agent, url: &str) -> Result<Vec<Record>, String> {
    match agent.get(url).call() {
        // Shelves are small JSON; cap the body anyway.
        Ok(resp) => {
            let mut body = String::new();
            std::io::Read::read_to_string(&mut std::io::Read::take(resp.into_reader(), 32 * 1024 * 1024), &mut body)
                .map_err(|e| e.to_string())?;
            parse(&body)
        }
        Err(ureq::Error::Status(404, _)) => Err("VinylVault: shelf not found (is sharing switched on?)".into()),
        Err(ureq::Error::Status(code, _)) => Err(format!("VinylVault: HTTP {code}")),
        Err(ureq::Error::Transport(t)) => Err(format!("VinylVault: network error: {}", t.kind())),
    }
}

/// One-off load (for `--snapshot`).
pub fn load_once(setting: &str) -> Result<Shelf, String> {
    Ok(Arc::new(RwLock::new(fetch(&agent(), &shelf_url(setting))?)))
}

/// Starts the refresh thread. `changed` is called after each successful load.
pub fn spawn(setting: &str, changed: impl Fn() -> bool + Send + 'static) -> Shelf {
    let shelf: Shelf = Arc::new(RwLock::new(Vec::new()));
    let url = shelf_url(setting);
    let out = shelf.clone();
    thread::spawn(move || {
        let agent = agent();
        loop {
            let result = fetch(&agent, &url);
            match result {
                Ok(records) => {
                    let n = records.len();
                    let colourful = records.iter().filter(|r| r.pressing.as_ref().is_some_and(|p| !p.is_plain_black())).count();
                    if let Ok(mut w) = out.write() {
                        *w = records;
                    }
                    eprintln!("VinylVault: {n} records on the shelf, {colourful} special pressings");
                    if !changed() {
                        return;
                    }
                    thread::sleep(REFRESH);
                }
                Err(e) => {
                    eprintln!("{e}");
                    thread::sleep(Duration::from_secs(120));
                }
            }
        }
    });
    shelf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shelf(json: &str) -> Shelf {
        Arc::new(RwLock::new(parse(json).unwrap()))
    }

    const JSON: &str = r##"{"name":"leon","records":[
        {"release_title":"Escapism. / The Thrill Is Gone.","artist_name":"RAYE","release_year":2023,
         "pressing":{"colourway_name":"Red Marbled","finish":"marbled","colour_hexes":["#CC292E","#F7F7F5"],
                     "edition_size":500,"is_limited":true,"is_numbered":false,"format":"LP","disc_count":1}},
        {"release_title":"Black Album","artist_name":"Someone",
         "pressing":{"colourway_name":"Black","finish":"solid","colour_hexes":["#1A1A1C"]}},
        {"release_title":"No Pressing Info","artist_name":"X","pressing":null}
    ]}"##;

    #[test]
    fn matches_with_noise_in_titles_and_artists() {
        let s = shelf(JSON);
        let p = lookup(&s, "Escapism. / The Thrill Is Gone. (Deluxe Edition)", "RAYE, 070 Shake").unwrap().unwrap();
        assert_eq!(p.finish, Finish::Marbled);
        assert_eq!(p.colors, vec![[0xCC, 0x29, 0x2E], [0xF7, 0xF7, 0xF5]]);
        assert_eq!(p.caption(), "Red Marbled · Ltd. 500");
    }

    #[test]
    fn plain_black_and_unknown_are_not_special() {
        let s = shelf(JSON);
        // Owned, but nothing special: Some(None) — must NOT fall back to the catalogue.
        assert_eq!(lookup(&s, "Black Album", "Someone"), Some(None));
        assert_eq!(lookup(&s, "No Pressing Info", "X"), Some(None));
        // Not on the shelf at all.
        assert_eq!(lookup(&s, "Escapism.", "Different Artist"), None);
    }

    /// Bodies exactly as the VinylVault endpoint's own tests expect them
    /// (backend/tests/test_shelf_json.py), nulls included.
    #[test]
    fn parses_real_endpoint_shapes() {
        let json = r##"{"name":"Shelf Person","records":[
          {"release_title":"Shelf Album","artist_name":"Shelf Act","release_year":2024,
           "pressing":{"colourway_name":"Pink Marbled","finish":"marbled","colour_hexes":["#F599BD","#F7F7F5"],
                       "edition_size":500,"is_limited":true,"is_numbered":false,"format":"12in","disc_count":1}},
          {"release_title":"Loose Record","artist_name":"Someone","release_year":null,
           "pressing":{"colourway_name":"Blue Splatter","finish":"splatter","colour_hexes":["#3D70D6","#1A1A1C"],
                       "edition_size":null,"is_limited":false,"is_numbered":false,"format":"LP","disc_count":null}},
          {"release_title":"Mystery","artist_name":null,"release_year":null,
           "pressing":{"colourway_name":null,"finish":"solid","colour_hexes":[],
                       "edition_size":null,"is_limited":false,"is_numbered":false,"format":null,"disc_count":null}}
        ]}"##;
        let s = shelf(json);
        assert_eq!(s.read().unwrap().len(), 3);
        let p = lookup(&s, "Shelf Album", "Shelf Act").unwrap().unwrap();
        assert_eq!((p.finish, p.caption().as_str()), (Finish::Marbled, "Pink Marbled · Ltd. 500"));
        let p = lookup(&s, "Loose Record", "Someone feat. Other").unwrap().unwrap();
        assert_eq!(p.finish, Finish::Splatter);
        assert_eq!(lookup(&s, "Mystery", "Anyone"), Some(None)); // owned, plain
    }

    /// Shaped like the live `/search?q=fleetwood mac rumours` answer: the fuzzy
    /// search returns tribute albums and other titles that must not be used.
    #[test]
    fn catalogue_pick_is_strict_and_stable() {
        let json = r##"{"items":[
          {"artist_name":"Royal Philharmonic Orchestra","release_title":"Plays Fleetwood Mac's Rumours",
           "variant":{"colourway_name":"Gold","finish":"solid","colour_hexes":["#D4AD4D"],"review_state":"accepted"}},
          {"artist_name":"Fleetwood Mac","release_title":"Rumours In Concert",
           "variant":{"colourway_name":"Burgundy Marbled","finish":"marbled","colour_hexes":["#73192E"],"review_state":"accepted"}},
          {"artist_name":"Fleetwood Mac","release_title":"Rumours",
           "variant":{"colourway_name":"Black","finish":"solid","colour_hexes":[],"review_state":"accepted"}},
          {"artist_name":"Fleetwood Mac","release_title":"Rumours",
           "variant":{"colourway_name":"Clear","finish":"clear","colour_hexes":["#E0EAF0"],"review_state":"accepted"}},
          {"artist_name":"Fleetwood Mac","release_title":"Rumours",
           "variant":{"colourway_name":"Proposed Pink","finish":"solid","colour_hexes":["#F599BD"],"review_state":"proposed"}}
        ]}"##;
        let items = || serde_json::from_str::<SearchJson>(json).unwrap().items;
        let p = pick(items(), "Rumours (Super Deluxe)", "Fleetwood Mac").unwrap();
        assert_eq!(p.name, "Clear");
        assert!(!p.owned);
        assert_eq!(pick(items(), "Rumours", "Fleetwood Mac"), Some(p)); // same pick every time
        assert!(pick(items(), "Tusk", "Fleetwood Mac").is_none());
        assert!(pick(items(), "Rumours", "Rumours").is_none()); // a band called Rumours
    }

    #[test]
    fn official_name_from_brackets() {
        let mut p = Pressing { name: "Blue Translucent Marbled [Moonstone Blue Marbled] Signed Insert".into(),
            finish: Finish::Marbled, colors: vec![], edition_size: None, is_limited: false, is_numbered: false, owned: false };
        assert_eq!(p.caption(), "Moonstone Blue Marbled");
        p.name = "Red".into();
        p.edition_size = Some(300);
        assert_eq!(p.caption(), "Red · Ltd. 300");
    }

    #[test]
    fn query_and_base() {
        assert_eq!(search_query("Sabotage (Deluxe) [2024]", "Bebe Rexha, YEJI"), "bebe rexha Sabotage");
        // Must not panic when lowercasing changes byte lengths.
        assert_eq!(search_query("Album", "İİİ & Friends"), "i\u{307}i\u{307}i\u{307} Album");
        let _ = primary_artist("İİİ, x");
        assert_eq!(base_url(Some("https://vv.example/shelf/ab12")), "https://vv.example");
        assert_eq!(base_url(Some("ab12")), DEFAULT_BASE);
        assert_eq!(base_url(None), DEFAULT_BASE);
    }

    #[test]
    fn shelf_urls() {
        assert_eq!(shelf_url("ab12"), "https://vinylvault.sky-lab.de/shelf/ab12.json");
        assert_eq!(shelf_url("https://x.de/shelf/ab12/"), "https://x.de/shelf/ab12.json");
        assert_eq!(shelf_url("https://x.de/shelf/ab12.json"), "https://x.de/shelf/ab12.json");
    }

    #[test]
    fn title_noise() {
        assert_eq!(norm_title("Sabotage - Single"), "sabotage");
        assert_eq!(norm_title("Abbey Road (Remastered 2019) [Deluxe]"), "abbeyroad");
    }
}
