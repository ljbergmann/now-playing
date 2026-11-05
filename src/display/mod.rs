//! Where frames go: straight to the screen via DRM/KMS on the Pi (no
//! desktop), else a window (Mac, Windows, Linux with a desktop).

use crate::frame::Rect;

pub enum Dirty {
    /// Nothing changed since the last call.
    None,
    /// Everything changed.
    Full,
    /// Only these regions changed (the Mac window always redraws everything).
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Rects(Vec<Rect>),
}

#[cfg(target_os = "linux")]
pub enum Status {
    Continue,
    Quit,
    /// The screen went away or changed (unplugged, swapped). Re-open.
    Lost,
}

#[cfg(target_os = "linux")]
pub trait Display {
    /// Logical size, i.e. after rotation.
    fn size(&self) -> (usize, usize);
    /// Shows `frame` and waits for the next vblank (≈ 16.7 ms at 60 Hz).
    fn present(&mut self, frame: &[u32], dirty: Dirty) -> Result<Status, String>;
}

/// Settings from the environment (see README).
/// For the Pi's direct screen output (a window gets these from the menu).
pub struct Options {
    /// Clockwise rotation in degrees; `None` = auto (panel orientation, else 0).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub rotate: Option<u32>,
    /// Which output to use, e.g. "HDMI-A-1" or "DSI-1"; `None` = first connected.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub connector: Option<String>,
}

#[cfg(target_os = "linux")]
mod kms;
#[cfg(target_os = "linux")]
pub fn open(opts: &Options) -> Result<Box<dyn Display>, String> {
    Ok(Box::new(kms::Kms::open(opts)?))
}

#[cfg(feature = "desktop")]
mod desktop;
#[cfg(all(feature = "desktop", target_os = "macos"))]
mod settings_mac;
#[cfg(feature = "desktop")]
mod settings_ui;
#[cfg(feature = "desktop")]
pub use desktop::run as run_desktop;

/// A desktop to open a window on: always on Mac/Windows, on Linux only
/// inside a graphical session (a Pi with OS Lite has none).
pub fn desktop_available() -> bool {
    if !cfg!(feature = "desktop") {
        return false;
    }
    if cfg!(target_os = "linux") {
        return ["WAYLAND_DISPLAY", "DISPLAY"].iter().any(|v| std::env::var_os(v).is_some_and(|x| !x.is_empty()));
    }
    true
}
