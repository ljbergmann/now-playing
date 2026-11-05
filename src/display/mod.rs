//! Where frames go: straight to the screen via DRM/KMS on the Pi,
//! a normal window on the Mac (for development).

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

pub trait Display {
    fn size(&self) -> (usize, usize);
    /// Shows `frame` and waits for the next vblank (≈ 16.7 ms at 60 Hz).
    /// Returns `false` when the program should quit.
    fn present(&mut self, frame: &[u32], dirty: Dirty) -> Result<bool, String>;
}

#[cfg(target_os = "linux")]
mod kms;
#[cfg(target_os = "linux")]
pub fn open(_size: Option<(usize, usize)>) -> Result<Box<dyn Display>, String> {
    Ok(Box::new(kms::Kms::open()?))
}

#[cfg(target_os = "macos")]
mod window;
#[cfg(target_os = "macos")]
pub fn open(size: Option<(usize, usize)>) -> Result<Box<dyn Display>, String> {
    let (w, h) = size.unwrap_or((800, 480));
    Ok(Box::new(window::Win::open(w, h)?))
}
