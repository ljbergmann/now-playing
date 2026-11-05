//! Direct-to-screen output on the Pi via DRM/KMS (no X, no Wayland, no browser).
//!
//! Two "dumb" buffers in scanout memory, flipped on vblank:
//! - we draw into the back buffer while the front one is on screen,
//! - `page_flip` swaps them exactly at the screen refresh → no tearing,
//! - waiting for the flip event paces the loop at the display's 60 Hz.
//! Only changed regions are copied, so a frame with just the equalizer moving
//! costs a few hundred pixels of work.

use std::fs::{File, OpenOptions};
use std::os::fd::{AsFd, BorrowedFd};
use std::time::Duration;

use drm::buffer::DrmFourcc;
use drm::control::{
    Device as ControlDevice, Event, Mode, ModeTypeFlags, PageFlipFlags, connector, crtc, dumbbuffer::DumbBuffer,
    framebuffer,
};

use super::{Dirty, Display};
use crate::frame::Rect;

struct Card(File);
impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl drm::Device for Card {}
impl ControlDevice for Card {}

struct Buf {
    db: Option<DumbBuffer>,
    fb: framebuffer::Handle,
    ptr: *mut u8,
    pitch: usize,
    /// Regions this buffer is missing (it was off-screen while they changed).
    pending: Vec<Rect>,
    full: bool,
}

pub struct Kms {
    card: Card,
    crtc: crtc::Handle,
    conn: connector::Handle,
    saved: crtc::Info,
    bufs: [Buf; 2],
    front: usize,
    w: usize,
    h: usize,
}

struct Output {
    card: Card,
    conn: connector::Handle,
    crtc: crtc::Handle,
    mode: Mode,
}

fn find_output() -> Result<Output, String> {
    let mut cards: Vec<_> = std::fs::read_dir("/dev/dri")
        .map_err(|e| format!("/dev/dri: {e} (is the vc4-kms-v3d overlay enabled?)"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("card")))
        .collect();
    cards.sort();

    let mut last_err = "no display connected".to_string();
    for path in cards {
        let file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => f,
            Err(e) => {
                last_err = format!("{}: {e} (is the user in the 'video' group?)", path.display());
                continue;
            }
        };
        let card = Card(file);
        let Ok(res) = card.resource_handles() else { continue }; // render-only node (v3d)
        for &ch in res.connectors() {
            let Ok(con) = card.get_connector(ch, true) else { continue };
            if con.state() != connector::State::Connected || con.modes().is_empty() {
                continue;
            }
            let mode = con
                .modes()
                .iter()
                .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                .copied()
                .unwrap_or(con.modes()[0]);
            let crtc = con
                .current_encoder()
                .and_then(|e| card.get_encoder(e).ok())
                .and_then(|e| e.crtc())
                .or_else(|| {
                    con.encoders().iter().find_map(|&e| {
                        let enc = card.get_encoder(e).ok()?;
                        res.filter_crtcs(enc.possible_crtcs()).first().copied()
                    })
                });
            if let Some(crtc) = crtc {
                eprintln!(
                    "display: {} {:?} {}x{}@{}Hz",
                    path.display(),
                    con.interface(),
                    mode.size().0,
                    mode.size().1,
                    mode.vrefresh()
                );
                return Ok(Output { card, conn: ch, crtc, mode });
            }
        }
    }
    Err(last_err)
}

impl Kms {
    pub fn open() -> Result<Self, String> {
        let Output { card, conn, crtc, mode } = find_output()?;
        let (w, h) = (mode.size().0 as usize, mode.size().1 as usize);
        let saved = card.get_crtc(crtc).map_err(|e| e.to_string())?;

        let make = || -> Result<Buf, String> {
            let mut db = card
                .create_dumb_buffer((w as u32, h as u32), DrmFourcc::Xrgb8888, 32)
                .map_err(|e| format!("create buffer: {e}"))?;
            let fb = card.add_framebuffer(&db, 24, 32).map_err(|e| format!("add framebuffer: {e}"))?;
            let pitch = drm::buffer::Buffer::pitch(&db) as usize;
            let mut map = card.map_dumb_buffer(&mut db).map_err(|e| format!("map buffer: {e}"))?;
            map.as_mut().fill(0);
            let ptr = map.as_mut().as_mut_ptr();
            // Keep the mapping for the life of the program (it's unmapped on exit).
            std::mem::forget(map);
            Ok(Buf { db: Some(db), fb, ptr, pitch, pending: Vec::new(), full: true })
        };
        let bufs = [make()?, make()?];

        card.set_crtc(crtc, Some(bufs[0].fb), (0, 0), &[conn], Some(mode)).map_err(|e| {
            format!("cannot take over the display: {e} (is a desktop or another program using it?)")
        })?;

        Ok(Kms { card, crtc, conn, saved, bufs, front: 0, w, h })
    }

    fn copy_into(&self, b: &Buf, frame: &[u32], r: Rect) {
        for row in r.y..(r.y + r.h).min(self.h) {
            let src = &frame[row * self.w + r.x..row * self.w + (r.x + r.w).min(self.w)];
            // SAFETY: the mapping is pitch * h bytes and rows/columns are clamped to w/h.
            unsafe {
                let dst = b.ptr.add(row * b.pitch + r.x * 4) as *mut u32;
                std::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len());
            }
        }
    }
}

impl Display for Kms {
    fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    fn present(&mut self, frame: &[u32], dirty: Dirty) -> Result<bool, String> {
        match dirty {
            Dirty::None => {}
            Dirty::Full => self.bufs.iter_mut().for_each(|b| b.full = true),
            Dirty::Rects(rects) => self.bufs.iter_mut().for_each(|b| b.pending.extend_from_slice(&rects)),
        }
        let back = 1 - self.front;
        let b = &self.bufs[back];
        if !b.full && b.pending.is_empty() {
            // Nothing to show: idle without spinning.
            std::thread::sleep(Duration::from_millis(16));
            return Ok(!crate::quit_requested());
        }
        if b.full {
            self.copy_into(b, frame, Rect { x: 0, y: 0, w: self.w, h: self.h });
        } else {
            for &r in &b.pending {
                self.copy_into(b, frame, r);
            }
        }
        let b = &mut self.bufs[back];
        b.pending.clear();
        b.full = false;

        self.card
            .page_flip(self.crtc, b.fb, PageFlipFlags::EVENT, None)
            .map_err(|e| format!("page flip: {e}"))?;
        'wait: loop {
            for ev in self.card.receive_events().map_err(|e| format!("events: {e}"))? {
                if let Event::PageFlip(_) = ev {
                    break 'wait;
                }
            }
        }
        self.front = back;
        Ok(!crate::quit_requested())
    }
}

impl Drop for Kms {
    fn drop(&mut self) {
        // Give the screen back to the console the way we found it.
        let _ = self.card.set_crtc(
            self.crtc,
            self.saved.framebuffer(),
            self.saved.position(),
            &[self.conn],
            self.saved.mode(),
        );
        for b in &mut self.bufs {
            let _ = self.card.destroy_framebuffer(b.fb);
            if let Some(db) = b.db.take() {
                let _ = self.card.destroy_dumb_buffer(db);
            }
        }
    }
}
