//! Font discovery. Uses `FONT_REGULAR` / `FONT_BOLD` if set, otherwise asks
//! fontconfig (`fc-match`) for Inter and lets it fall back to whatever sans
//! is installed. On the Pi the setup script installs `fonts-inter`. On macOS,
//! when Inter isn't installed, the system font (San Francisco) is used.

use std::process::Command;

use ab_glyph::{FontVec, VariableFont};

pub struct Fonts {
    pub light: FontVec,
    pub regular: FontVec,
    pub bold: FontVec,
    /// Extra-bold/black, for poster headlines.
    pub black: FontVec,
}

/// `fc-match`, also where Homebrew puts it: apps opened from Finder don't
/// get the shell's PATH.
fn fc_match(pattern: &str) -> Option<(String, String, u32)> {
    let out = ["fc-match", "/opt/homebrew/bin/fc-match", "/usr/local/bin/fc-match"]
        .iter()
        .find_map(|bin| Command::new(bin).args(["-f", "%{family}|%{file}|%{index}", pattern]).output().ok())?;
    let s = String::from_utf8(out.stdout).ok()?;
    let mut parts = s.splitn(3, '|');
    let (family, file, index) = (parts.next()?, parts.next()?, parts.next()?);
    (!file.is_empty()).then(|| (family.to_string(), file.to_string(), index.trim().parse().unwrap_or(0)))
}

const MAC_SYSTEM_FONT: &str = "/System/Library/Fonts/SFNS.ttf";

/// Inter if fontconfig has it; on a Mac without Inter, San Francisco (a
/// variable font, so every weight works); otherwise fontconfig's fallback.
fn find(pattern: &str) -> Option<(String, u32)> {
    let found = fc_match(pattern);
    if let Some((family, file, index)) = &found {
        if family.contains("Inter") {
            return Some((file.clone(), *index));
        }
    }
    if cfg!(target_os = "macos") && std::path::Path::new(MAC_SYSTEM_FONT).exists() {
        return Some((MAC_SYSTEM_FONT.to_string(), 0));
    }
    found.map(|(_, file, index)| (file, index))
}

fn load(env: &str, pattern: &str, weight: f32) -> Result<FontVec, String> {
    let (path, index) = match std::env::var(env) {
        Ok(p) if !p.is_empty() => (p, 0),
        _ => find(pattern).ok_or_else(|| {
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
            light: load("FONT_LIGHT", "Inter:weight=light", 300.0)?,
            regular: load("FONT_REGULAR", "Inter:weight=regular", 400.0)?,
            bold: load("FONT_BOLD", "Inter:weight=bold", 700.0)?,
            black: load("FONT_BLACK", "Inter:weight=black", 900.0)?,
        })
    }
}
