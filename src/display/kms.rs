//! Direct-to-screen output on the Pi via DRM/KMS (no X, no Wayland, no browser).
//!
//! Two "dumb" buffers in scanout memory, flipped on vblank:
//! - we draw into the back buffer while the front one is on screen,
//! - `page_flip` swaps them exactly at the screen refresh → no tearing,
//! - waiting for the flip event paces the loop at the display's refresh rate.
//! Only changed regions are copied, so a frame with just the equalizer moving
//! costs a few hundred pixels of work.
//!
//! Adapts to the attached screen: uses its preferred mode, honours the panel's
//! reported mounting orientation (or ROTATE), and notices unplug/replug.

use std::fs::{File, OpenOptions};
use std::os::fd::{AsFd, BorrowedFd};
use std::time::{Duration, Instant};

use drm::buffer::DrmFourcc;
use drm::control::{
    Device as ControlDevice, Event, Mode, ModeTypeFlags, PageFlipFlags, connector, crtc, dumbbuffer::DumbBuffer,
    framebuffer, property,
};

use super::{Dirty, Display, Options, Status};
use crate::frame::Rect;

const HOTPLUG_CHECK: Duration = Duration::from_secs(2);

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
    mode: Mode,
    saved: crtc::Info,
    bufs: [Buf; 2],
    front: usize,
    /// Physical (scanout) size.
    pw: usize,
    ph: usize,
    /// Clockwise rotation applied to the image: 0, 90, 180, 270.
    rot: u32,
    row: Vec<u32>,
    last_check: Instant,
}

struct Output {
    card: Card,
    name: String,
    conn: connector::Handle,
    crtc: crtc::Handle,
    mode: Mode,
    panel_rotation: Option<u32>,
}

fn connector_name(con: &connector::Info) -> String {
    format!("{}-{}", con.interface().as_str(), con.interface_id())
}

fn preferred_mode(con: &connector::Info) -> Option<Mode> {
    let modes = con.modes();
    modes
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| modes.first())
        .copied()
}

/// Reads the connector's "panel orientation" property (set by some DSI panel
/// drivers) and turns it into the clockwise rotation that makes the image upright.
fn panel_rotation(card: &Card, conn: connector::Handle) -> Option<u32> {
    let props = card.get_properties(conn).ok()?;
    let (ids, vals) = props.as_props_and_values();
    for (id, val) in ids.iter().zip(vals) {
        let info = card.get_property(*id).ok()?;
        if info.name().to_bytes() != b"panel orientation" {
            continue;
        }
        let property::ValueType::Enum(e) = info.value_type() else { return None };
        let (raw, names) = e.values();
        let idx = raw.iter().position(|r| r == val)?;
        return match names[idx].name().to_bytes() {
            b"Upside Down" => Some(180),
            b"Left Side Up" => Some(270),
            b"Right Side Up" => Some(90),
            _ => Some(0),
        };
    }
    None
}

fn find_output(want: Option<&str>) -> Result<Output, String> {
    let mut cards: Vec<_> = std::fs::read_dir("/dev/dri")
        .map_err(|e| format!("/dev/dri: {e} (is the vc4-kms-v3d overlay enabled?)"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("card")))
        .collect();
    cards.sort();

    let mut seen = Vec::new();
    let mut last_err = None;
    for path in cards {
        let file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => f,
            Err(e) => {
                last_err = Some(format!("{}: {e} (is the user in the 'video' group?)", path.display()));
                continue;
            }
        };
        let card = Card(file);
        let Ok(res) = card.resource_handles() else { continue }; // render-only node (v3d)
        for &ch in res.connectors() {
            let Ok(con) = card.get_connector(ch, true) else { continue };
            let name = connector_name(&con);
            let connected = con.state() == connector::State::Connected;
            seen.push(format!("{name}{}", if connected { " (connected)" } else { "" }));
            if !connected || want.is_some_and(|w| !w.eq_ignore_ascii_case(&name)) {
                continue;
            }
            let Some(mode) = preferred_mode(&con) else { continue };
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
                let panel_rotation = panel_rotation(&card, ch);
                return Ok(Output { card, name, conn: ch, crtc, mode, panel_rotation });
            }
        }
    }
    Err(last_err.unwrap_or_else(|| match want {
        Some(w) => format!("screen {w} not connected (outputs: {})", seen.join(", ")),
        None if seen.is_empty() => "no display outputs found".into(),
        None => format!("no screen connected (outputs: {})", seen.join(", ")),
    }))
}

impl Kms {
    pub fn open(opts: &Options) -> Result<Self, String> {
        let Output { card, name, conn, crtc, mode, panel_rotation } = find_output(opts.connector.as_deref())?;
        let (pw, ph) = (mode.size().0 as usize, mode.size().1 as usize);
        let rot = opts.rotate.or(panel_rotation).unwrap_or(0);
        eprintln!(
            "screen: {name} {pw}x{ph}@{}Hz, rotation {rot}°{}",
            mode.vrefresh(),
            match (opts.rotate, panel_rotation) {
                (Some(_), _) => " (ROTATE)",
                (None, Some(_)) => " (panel orientation)",
                _ => "",
            }
        );
        let saved = card.get_crtc(crtc).map_err(|e| e.to_string())?;

        let make = || -> Result<Buf, String> {
            let mut db = card
                .create_dumb_buffer((pw as u32, ph as u32), DrmFourcc::Xrgb8888, 32)
                .map_err(|e| format!("create buffer: {e}"))?;
            let fb = card.add_framebuffer(&db, 24, 32).map_err(|e| format!("add framebuffer: {e}"))?;
            let pitch = drm::buffer::Buffer::pitch(&db) as usize;
            let mut map = card.map_dumb_buffer(&mut db).map_err(|e| format!("map buffer: {e}"))?;
            map.as_mut().fill(0);
            let ptr = map.as_mut().as_mut_ptr();
            // Keep the mapping for the life of the process (the kernel cleans up on exit).
            std::mem::forget(map);
            Ok(Buf { db: Some(db), fb, ptr, pitch, pending: Vec::new(), full: true })
        };
        let bufs = [make()?, make()?];

        card.set_crtc(crtc, Some(bufs[0].fb), (0, 0), &[conn], Some(mode)).map_err(|e| {
            format!("cannot take over the display: {e} (is a desktop or another program using it?)")
        })?;

        Ok(Kms {
            card,
            crtc,
            conn,
            mode,
            saved,
            bufs,
            front: 0,
            pw,
            ph,
            rot,
            row: Vec::with_capacity(pw.max(ph)),
            last_check: Instant::now(),
        })
    }

    /// Logical rect → physical rect for the current rotation.
    fn to_physical(&self, r: Rect) -> Rect {
        let (lw, lh) = self.size();
        match self.rot {
            90 => Rect { x: lh - r.y - r.h, y: r.x, w: r.h, h: r.w },
            180 => Rect { x: lw - r.x - r.w, y: lh - r.y - r.h, w: r.w, h: r.h },
            270 => Rect { x: r.y, y: lw - r.x - r.w, w: r.h, h: r.w },
            _ => r,
        }
    }

    /// Copies logical rect `r` of `frame` into buffer `bi`, rotating on the way.
    /// Writes go out row by row in scanout order (scanout memory is write-combined,
    /// so sequential writes matter far more than read order).
    fn copy_rect(&mut self, bi: usize, frame: &[u32], r: Rect) {
        let (lw, lh) = self.size();
        let r = Rect { x: r.x.min(lw), y: r.y.min(lh), w: r.w.min(lw - r.x.min(lw)), h: r.h.min(lh - r.y.min(lh)) };
        if r.w == 0 || r.h == 0 {
            return;
        }
        let p = self.to_physical(r);
        let (ptr, pitch) = (self.bufs[bi].ptr, self.bufs[bi].pitch);
        for py in p.y..p.y + p.h {
            let src: &[u32] = if self.rot == 0 {
                &frame[py * lw + p.x..py * lw + p.x + p.w]
            } else {
                self.row.clear();
                for px in p.x..p.x + p.w {
                    let (lx, ly) = match self.rot {
                        90 => (py, lh - 1 - px),
                        180 => (lw - 1 - px, lh - 1 - py),
                        _ => (lw - 1 - py, px), // 270
                    };
                    self.row.push(frame[ly * lw + lx]);
                }
                &self.row
            };
            // SAFETY: the mapping is pitch * ph bytes; p is clamped inside pw x ph.
            unsafe {
                let dst = ptr.add(py * pitch + p.x * 4) as *mut u32;
                std::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len());
            }
        }
    }

    /// Is the screen still there, with the same preferred mode?
    fn still_connected(&self) -> bool {
        match self.card.get_connector(self.conn, false) {
            Ok(con) => {
                con.state() == connector::State::Connected
                    && preferred_mode(&con).is_some_and(|m| m.size() == self.mode.size())
            }
            Err(_) => false,
        }
    }
}

impl Display for Kms {
    fn size(&self) -> (usize, usize) {
        if self.rot == 90 || self.rot == 270 { (self.ph, self.pw) } else { (self.pw, self.ph) }
    }

    fn present(&mut self, frame: &[u32], dirty: Dirty) -> Result<Status, String> {
        if crate::quit_requested() {
            return Ok(Status::Quit);
        }
        if self.last_check.elapsed() >= HOTPLUG_CHECK {
            self.last_check = Instant::now();
            if !self.still_connected() {
                eprintln!("screen disconnected or changed");
                return Ok(Status::Lost);
            }
        }
        let (lw, lh) = self.size();
        if frame.len() != lw * lh {
            std::thread::sleep(Duration::from_millis(16));
            return Ok(Status::Continue);
        }

        match dirty {
            Dirty::None => {}
            Dirty::Full => self.bufs.iter_mut().for_each(|b| b.full = true),
            Dirty::Rects(rects) => self.bufs.iter_mut().for_each(|b| b.pending.extend_from_slice(&rects)),
        }
        let back = 1 - self.front;
        if !self.bufs[back].full && self.bufs[back].pending.is_empty() {
            // Nothing to show: idle without spinning.
            std::thread::sleep(Duration::from_millis(16));
            return Ok(Status::Continue);
        }
        if self.bufs[back].full {
            self.copy_rect(back, frame, Rect { x: 0, y: 0, w: lw, h: lh });
        } else {
            let pending = std::mem::take(&mut self.bufs[back].pending);
            for r in &pending {
                self.copy_rect(back, frame, *r);
            }
            self.bufs[back].pending = pending;
        }
        self.bufs[back].pending.clear();
        self.bufs[back].full = false;

        if let Err(e) = self.card.page_flip(self.crtc, self.bufs[back].fb, PageFlipFlags::EVENT, None) {
            eprintln!("page flip failed: {e}");
            return Ok(Status::Lost);
        }
        'wait: loop {
            for ev in self.card.receive_events().map_err(|e| format!("events: {e}"))? {
                if let Event::PageFlip(_) = ev {
                    break 'wait;
                }
            }
        }
        self.front = back;
        Ok(Status::Continue)
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

#[cfg(test)]
mod tests {
    // Rotation maths, checked independently of any DRM device.
    fn map(rot: u32, lw: usize, lh: usize, px: usize, py: usize) -> (usize, usize) {
        match rot {
            90 => (py, lh - 1 - px),
            180 => (lw - 1 - px, lh - 1 - py),
            270 => (lw - 1 - py, px),
            _ => (px, py),
        }
    }

    #[test]
    fn rotation_is_a_bijection() {
        let (lw, lh) = (5, 3);
        for rot in [0, 90, 180, 270] {
            let (pw, ph) = if rot % 180 == 90 { (lh, lw) } else { (lw, lh) };
            let mut seen = vec![false; lw * lh];
            for py in 0..ph {
                for px in 0..pw {
                    let (lx, ly) = map(rot, lw, lh, px, py);
                    assert!(lx < lw && ly < lh, "rot {rot}");
                    assert!(!seen[ly * lw + lx], "rot {rot} hits a pixel twice");
                    seen[ly * lw + lx] = true;
                }
            }
        }
    }

    #[test]
    fn rot90_puts_logical_top_left_at_physical_top_right() {
        // Clockwise: the image's top-left corner ends up at the screen's top-right.
        let (lw, lh) = (5, 3);
        let pw = lh;
        assert_eq!(map(90, lw, lh, pw - 1, 0), (0, 0));
    }
}
