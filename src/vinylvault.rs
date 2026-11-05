//! VinylVault: which pressing of the playing album do *you* own?
//!
//! Reads a shared VinylVault shelf (`/shelf/{slug}.json`, public and read-only:
//! it only exists while the owner has sharing switched on), refreshes it every
//! 15 minutes, and matches Last.fm's album + artist against it. The vinyl look
//! then draws the record as that pressing: marbled, splatter, clear, ...

use std::sync::{Arc, RwLock};
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
}

impl Pressing {
    /// True for a plain black record (nothing special to draw).
    pub fn is_plain_black(&self) -> bool {
        self.finish == Finish::Solid && self.colors.iter().all(|c| c.iter().all(|&v| v < 48))
    }

    /// "PINK MARBLED · LTD. 500"
    pub fn caption(&self) -> String {
        let mut parts = vec![if self.name.is_empty() { "Coloured vinyl".to_string() } else { self.name.clone() }];
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
    #[serde(default)]
    is_limited: bool,
    #[serde(default)]
    is_numbered: bool,
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
            pressing: pressing.map(|p| Pressing {
                name: p.colourway_name.unwrap_or_default(),
                finish: Finish::parse(p.finish.as_deref().unwrap_or("")),
                colors: p.colour_hexes.unwrap_or_default().iter().filter_map(|h| hex(h)).take(6).collect(),
                edition_size: p.edition_size,
                is_limited: p.is_limited,
                is_numbered: p.is_numbered,
            }),
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

/// Finds the owned pressing for a Last.fm album/artist, if any.
pub fn lookup(shelf: &Shelf, album: &str, artist: &str) -> Option<Pressing> {
    let title = norm_title(album);
    if title.is_empty() {
        return None;
    }
    let main = primary_artist(artist);
    let records = shelf.read().ok()?;
    records
        .iter()
        .filter(|r| r.title_key == title)
        .find(|r| {
            r.artist_key.is_empty()
                || main.is_empty()
                || r.artist_key.contains(&main)
                || main.contains(&r.artist_key)
        })
        .and_then(|r| r.pressing.clone())
        .filter(|p| !p.is_plain_black())
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
        let p = lookup(&s, "Escapism. / The Thrill Is Gone. (Deluxe Edition)", "RAYE, 070 Shake").unwrap();
        assert_eq!(p.finish, Finish::Marbled);
        assert_eq!(p.colors, vec![[0xCC, 0x29, 0x2E], [0xF7, 0xF7, 0xF5]]);
        assert_eq!(p.caption(), "Red Marbled · Ltd. 500");
    }

    #[test]
    fn plain_black_and_unknown_are_not_special() {
        let s = shelf(JSON);
        assert!(lookup(&s, "Black Album", "Someone").is_none());
        assert!(lookup(&s, "No Pressing Info", "X").is_none());
        assert!(lookup(&s, "Escapism.", "Different Artist").is_none());
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
        let p = lookup(&s, "Shelf Album", "Shelf Act").unwrap();
        assert_eq!((p.finish, p.caption().as_str()), (Finish::Marbled, "Pink Marbled · Ltd. 500"));
        let p = lookup(&s, "Loose Record", "Someone feat. Other").unwrap();
        assert_eq!(p.finish, Finish::Splatter);
        assert!(lookup(&s, "Mystery", "Anyone").is_none()); // plain, no colours
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
