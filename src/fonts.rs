//! Font discovery. Uses `FONT_REGULAR` / `FONT_BOLD` if set, otherwise asks
//! fontconfig (`fc-match`) for Inter and lets it fall back to whatever sans
//! is installed. On the Pi the setup script installs `fonts-inter`.

use std::process::Command;

use ab_glyph::{FontVec, VariableFont};

pub struct Fonts {
    pub regular: FontVec,
    pub bold: FontVec,
}

fn fc_match(pattern: &str) -> Option<(String, u32)> {
    let out = Command::new("fc-match").args(["-f", "%{file}|%{index}", pattern]).output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    let (file, index) = s.split_once('|')?;
    (!file.is_empty()).then(|| (file.to_string(), index.trim().parse().unwrap_or(0)))
}

fn load(env: &str, pattern: &str, weight: f32) -> Result<FontVec, String> {
    let (path, index) = match std::env::var(env) {
        Ok(p) if !p.is_empty() => (p, 0),
        _ => fc_match(pattern).ok_or_else(|| {
            format!("no font found for '{pattern}' (install fontconfig + fonts-inter, or set {env})")
        })?,
    };
    let data = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    let mut font = FontVec::try_from_vec_and_index(data, index).map_err(|e| format!("{path}: {e}"))?;
    // Variable fonts (e.g. InterVariable) default to Regular; pick the weight explicitly.
    font.set_variation(b"wght", weight);
    Ok(font)
}

impl Fonts {
    pub fn discover() -> Result<Self, String> {
        Ok(Fonts {
            regular: load("FONT_REGULAR", "Inter:weight=regular", 400.0)?,
            bold: load("FONT_BOLD", "Inter:weight=bold", 700.0)?,
        })
    }
}
