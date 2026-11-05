//! Where frames go: straight to the screen via DRM/KMS on the Pi,
//! a normal (resizable) window on the Mac for development.

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

pub enum Status {
    Continue,
    Quit,
    /// Same output, new size (Mac window resized). Re-render at `size()`.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Resized,
    /// The screen went away or changed (unplugged, swapped). Re-open.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Lost,
}

pub trait Display {
    /// Logical size, i.e. after rotation.
    fn size(&self) -> (usize, usize);
    /// Shows `frame` and waits for the next vblank (≈ 16.7 ms at 60 Hz).
    fn present(&mut self, frame: &[u32], dirty: Dirty) -> Result<Status, String>;
}

/// Settings from the environment (see README).
pub struct Options {
    /// Window size on the Mac; ignored on the Pi (uses the screen's own mode).
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub size: Option<(usize, usize)>,
    /// Clockwise rotation in degrees; `None` = auto (panel orientation, else 0).
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub rotate: Option<u32>,
    /// Which output to use, e.g. "HDMI-A-1" or "DSI-1"; `None` = first connected.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub connector: Option<String>,
}

#[cfg(target_os = "linux")]
mod kms;
#[cfg(target_os = "linux")]
pub fn open(opts: &Options) -> Result<Box<dyn Display>, String> {
    Ok(Box::new(kms::Kms::open(opts)?))
}

#[cfg(target_os = "macos")]
mod window;
#[cfg(target_os = "macos")]
pub fn open(opts: &Options) -> Result<Box<dyn Display>, String> {
    let (w, h) = opts.size.unwrap_or((800, 480));
    Ok(Box::new(window::Win::open(w, h)?))
}
