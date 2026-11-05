//! Fonts: Inter is built into the app (assets/fonts, SIL Open Font License),
//! so it looks the same on every machine. `FONT_LIGHT` / `FONT_REGULAR` /
//! `FONT_BOLD` / `FONT_BLACK` can point to other font files instead.

use ab_glyph::{FontVec, VariableFont};

const INTER: &[u8] = include_bytes!("../assets/fonts/InterVariable.ttf");

pub struct Fonts {
    pub light: FontVec,
    pub regular: FontVec,
    pub bold: FontVec,
    /// Extra-bold/black, for poster headlines.
    pub black: FontVec,
}

fn load(env: &str, weight: f32) -> Result<FontVec, String> {
    let mut font = match crate::settings::get(env) {
        Some(path) => {
            let data = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
            FontVec::try_from_vec(data).map_err(|e| format!("{path}: {e}"))?
        }
        None => FontVec::try_from_vec(INTER.to_vec()).map_err(|e| format!("built-in font: {e}"))?,
    };
    // Variable fonts default to Regular; pick the weight explicitly.
    font.set_variation(b"wght", weight);
    Ok(font)
}

impl Fonts {
    pub fn discover() -> Result<Self, String> {
        Ok(Fonts {
            light: load("FONT_LIGHT", 300.0)?,
            regular: load("FONT_REGULAR", 400.0)?,
            bold: load("FONT_BOLD", 700.0)?,
            black: load("FONT_BLACK", 900.0)?,
        })
    }
}
