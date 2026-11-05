//! macOS development window (minifb). Not used on the Pi.
//! Resizable, so layouts for other screen shapes can be tried live.

use minifb::{Key, KeyRepeat, ScaleMode, Window, WindowOptions};

use super::{Dirty, Display, Status};

pub struct Win {
    win: Window,
    w: usize,
    h: usize,
}

impl Win {
    pub fn open(w: usize, h: usize) -> Result<Self, String> {
        let opts = WindowOptions { resize: true, scale_mode: ScaleMode::UpperLeft, ..WindowOptions::default() };
        let mut win = Window::new("Now Playing", w, h, opts).map_err(|e| e.to_string())?;
        win.set_target_fps(60);
        Ok(Win { win, w, h })
    }
}

impl Display for Win {
    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    fn present(&mut self, frame: &[u32], _dirty: Dirty) -> Result<Status, String> {
        if frame.len() == self.w * self.h {
            self.win.update_with_buffer(frame, self.w, self.h).map_err(|e| e.to_string())?;
        } else {
            self.win.update();
        }
        if !self.win.is_open() || self.win.is_key_down(Key::Escape) || crate::quit_requested() {
            return Ok(Status::Quit);
        }
        let (w, h) = self.win.get_size();
        if (w, h) != (self.w, self.h) && w > 0 && h > 0 {
            (self.w, self.h) = (w, h);
            return Ok(Status::Resized);
        }
        Ok(Status::Continue)
    }

    fn key(&mut self) -> Option<char> {
        self.win.get_keys_pressed(KeyRepeat::No).into_iter().find_map(|k| match k {
            Key::Key1 => Some('1'),
            Key::Key2 => Some('2'),
            Key::Key3 => Some('3'),
            Key::I => Some('i'),
            _ => None,
        })
    }
}
