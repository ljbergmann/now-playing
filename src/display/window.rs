//! macOS development window (minifb). Not used on the Pi.

use minifb::{Key, Window, WindowOptions};

use super::{Dirty, Display};

pub struct Win {
    win: Window,
    w: usize,
    h: usize,
}

impl Win {
    pub fn open(w: usize, h: usize) -> Result<Self, String> {
        let mut win = Window::new("Now Playing", w, h, WindowOptions::default()).map_err(|e| e.to_string())?;
        win.set_target_fps(60);
        Ok(Win { win, w, h })
    }
}

impl Display for Win {
    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    fn present(&mut self, frame: &[u32], _dirty: Dirty) -> Result<bool, String> {
        self.win.update_with_buffer(frame, self.w, self.h).map_err(|e| e.to_string())?;
        Ok(self.win.is_open() && !self.win.is_key_down(Key::Escape) && !crate::quit_requested())
    }
}
