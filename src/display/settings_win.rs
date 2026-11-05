//! The Windows settings window: plain Win32 controls (Ctrl+, / S / menu).
//! Same contents and logic as the Mac one (see settings_ui.rs). Clicks
//! become `Action`s picked up with `next()`; `refresh` shows the state.
//! winit's event loop pumps this window's messages too (same thread).

use std::ffi::c_void;
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{COLOR_BTNFACE, CreateFontIndirectW, GetSysColorBrush, HFONT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, BN_CLICKED, BS_AUTOCHECKBOX, BS_AUTORADIOBUTTON, BS_DEFPUSHBUTTON, BS_PUSHBUTTON,
    CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, CBN_SELCHANGE, CBS_DROPDOWNLIST, CW_USEDEFAULT, CreateWindowExW,
    DefWindowProcW, ES_AUTOHSCROLL, GetWindowTextW, IDC_ARROW, IsWindowVisible, LoadCursorW, NONCLIENTMETRICSW,
    RegisterClassExW, SPI_GETNONCLIENTMETRICS, SW_HIDE, SW_SHOW, SendMessageW, SetForegroundWindow, SetWindowTextW,
    ShowWindow, SystemParametersInfoW, WM_CLOSE, WM_COMMAND, WM_SETFONT, WNDCLASSEXW, WS_BORDER, WS_CAPTION, WS_CHILD,
    WS_EX_CLIENTEDGE, WS_GROUP, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use super::settings_ui::{Action, State};
use crate::scene::Theme;

static ACTIONS: Mutex<Vec<Action>> = Mutex::new(Vec::new());

pub fn next() -> Option<Action> {
    let mut q = ACTIONS.lock().ok()?;
    (!q.is_empty()).then(|| q.remove(0))
}

fn push(a: Action) {
    if let Ok(mut q) = ACTIONS.lock() {
        q.push(a);
    }
}

// Control ids.
const USER: usize = 1;
const SAVE: usize = 2;
const VV_CONNECT: usize = 5;
const VV_DISCONNECT: usize = 6;
const LOOK: usize = 7;
const CLOCK24: usize = 8;
const CLOCK12: usize = 9;
const STATS: usize = 10;

const BST_CHECKED: usize = 1;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The user name field, for reading it when Save is clicked.
static USER_FIELD: Mutex<usize> = Mutex::new(0);

fn text_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let (id, code) = (wparam & 0xFFFF, (wparam >> 16) & 0xFFFF);
            let control = lparam as HWND;
            match (id, code as u32) {
                (SAVE, BN_CLICKED) => {
                    let field = *USER_FIELD.lock().unwrap_or_else(|e| e.into_inner()) as HWND;
                    push(Action::LastfmUser(text_of(field)));
                }
                (VV_CONNECT, BN_CLICKED) => push(Action::ConnectVinylVault),
                (VV_DISCONNECT, BN_CLICKED) => push(Action::DisconnectVinylVault),
                (CLOCK24 | CLOCK12, BN_CLICKED) => push(Action::TwelveHour(id == CLOCK12)),
                (STATS, BN_CLICKED) => {
                    let on = unsafe { SendMessageW(control, BM_GETCHECK, 0, 0) } as usize == BST_CHECKED;
                    push(Action::Stats(on));
                }
                (LOOK, CBN_SELCHANGE) => {
                    let t = match unsafe { SendMessageW(control, CB_GETCURSEL, 0, 0) } {
                        1 => Theme::Poster,
                        2 => Theme::Ambient,
                        _ => Theme::Vinyl,
                    };
                    push(Action::Look(t));
                }
                _ => {}
            }
            0
        }
        // Closing only hides it; it's reopened from the menu.
        WM_CLOSE => {
            unsafe { ShowWindow(hwnd, SW_HIDE) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub struct SettingsWindow {
    hwnd: HWND,
    lastfm_status: HWND,
    vv_status: HWND,
    vv_connect: HWND,
    vv_disconnect: HWND,
    look: HWND,
    clock24: HWND,
    clock12: HWND,
    stats: HWND,
    /// Last values shown (see the Mac window: don't disturb an open dropdown).
    shown: Option<(Theme, bool, bool)>,
    texts: [String; 3],
}

impl SettingsWindow {
    pub fn new(current_user: Option<&str>) -> Option<Self> {
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class = wide("NowPlayingSettings");
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassExW(&wc); // fails harmlessly if already registered

            let scale = GetDpiForSystem() as f32 / 96.0;
            let px = |v: i32| (v as f32 * scale).round() as i32;
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                wide("Now Playing Settings").as_ptr(),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                px(500),
                px(330),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                return None;
            }

            // The system's dialog font, at the right size for this screen.
            let mut ncm: NONCLIENTMETRICSW = std::mem::zeroed();
            ncm.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
            let font: HFONT = if SystemParametersInfoW(SPI_GETNONCLIENTMETRICS, ncm.cbSize, &mut ncm as *mut _ as *mut c_void, 0) != 0 {
                CreateFontIndirectW(&ncm.lfMessageFont)
            } else {
                std::ptr::null_mut()
            };

            let make = |class: &str, text: &str, style: u32, ex: u32, x: i32, y: i32, w: i32, h: i32, id: usize| -> HWND {
                let c = CreateWindowExW(
                    ex,
                    wide(class).as_ptr(),
                    wide(text).as_ptr(),
                    WS_CHILD | WS_VISIBLE | style,
                    px(x),
                    px(y),
                    px(w),
                    px(h),
                    hwnd,
                    id as *mut c_void,
                    instance,
                    std::ptr::null(),
                );
                if !font.is_null() {
                    SendMessageW(c, WM_SETFONT, font as WPARAM, 1);
                }
                c
            };
            let label = |text: &str, x: i32, y: i32, w: i32| make("STATIC", text, 0, 0, x, y, w, 20, 0);
            let (lx, cx) = (20, 130); // label column, control column

            label("Last.fm", lx, 22, 100);
            let user = make("EDIT", current_user.unwrap_or(""), WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32, WS_EX_CLIENTEDGE, cx, 18, 200, 24, USER);
            make("BUTTON", "Save", WS_TABSTOP | BS_DEFPUSHBUTTON as u32, 0, cx + 210, 17, 80, 26, SAVE);
            let lastfm_status = label("", cx, 48, 340);

            label("VinylVault", lx, 86, 100);
            let vv_status = label("", cx, 86, 340);
            let vv_connect = make("BUTTON", "Connect with VinylVault…", WS_TABSTOP | BS_PUSHBUTTON as u32, 0, cx, 110, 200, 28, VV_CONNECT);
            let vv_disconnect = make("BUTTON", "Disconnect", WS_TABSTOP | BS_PUSHBUTTON as u32, 0, cx + 210, 110, 100, 28, VV_DISCONNECT);

            label("Look", lx, 162, 100);
            let look = make("COMBOBOX", "", WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32, 0, cx, 158, 150, 200, LOOK);
            for t in ["Vinyl", "Poster", "Ambient"] {
                SendMessageW(look, CB_ADDSTRING, 0, wide(t).as_ptr() as LPARAM);
            }

            label("Clock", lx, 200, 100);
            let clock24 = make("BUTTON", "24-hour (21:47)", WS_TABSTOP | WS_GROUP | BS_AUTORADIOBUTTON as u32, 0, cx, 198, 150, 22, CLOCK24);
            let clock12 = make("BUTTON", "12-hour (9:47 PM)", BS_AUTORADIOBUTTON as u32, 0, cx + 160, 198, 160, 22, CLOCK12);

            label("Play counts", lx, 236, 100);
            let stats = make("BUTTON", "Show my play counts under the track", WS_TABSTOP | WS_GROUP | BS_AUTOCHECKBOX as u32, 0, cx, 234, 300, 22, STATS);

            *USER_FIELD.lock().unwrap_or_else(|e| e.into_inner()) = user as usize;
            Some(SettingsWindow {
                hwnd,
                lastfm_status,
                vv_status,
                vv_connect,
                vv_disconnect,
                look,
                clock24,
                clock12,
                stats,
                shown: None,
                texts: Default::default(),
            })
        }
    }

    pub fn show(&self) {
        unsafe {
            ShowWindow(self.hwnd, SW_SHOW);
            SetForegroundWindow(self.hwnd);
        }
    }

    pub fn visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd) != 0 }
    }

    pub fn refresh(&mut self, s: &State) {
        let lastfm = match (&s.lastfm_note, &s.lastfm_user) {
            (Some((_, msg)), _) => msg.clone(),
            (None, Some(_)) => "✓ Connected".into(),
            (None, None) => "Enter your user name and click Save.".into(),
        };
        let vv = match &s.vinylvault {
            Some(owner) if !owner.is_empty() => format!("✓ Connected as {owner}"),
            Some(_) => "✓ Connected".into(),
            None if s.shelf_link => "Using a shared shelf link".into(),
            None => "Not connected (optional)".into(),
        };
        let connect = if s.vinylvault.is_some() { "Reconnect…" } else { "Connect with VinylVault…" };
        for (i, (hwnd, text)) in [(self.lastfm_status, lastfm), (self.vv_status, vv), (self.vv_connect, connect.to_string())].into_iter().enumerate() {
            if self.texts[i] != text {
                unsafe { SetWindowTextW(hwnd, wide(&text).as_ptr()) };
                self.texts[i] = text;
            }
        }
        unsafe { EnableWindow(self.vv_disconnect, s.vinylvault.is_some() as i32) };

        let now = (s.theme, s.twelve_hour, s.stats);
        if self.shown != Some(now) {
            self.shown = Some(now);
            let i = match s.theme {
                Theme::Vinyl => 0,
                Theme::Poster => 1,
                Theme::Ambient => 2,
            };
            let check = |b: bool| if b { BST_CHECKED } else { 0 };
            unsafe {
                SendMessageW(self.look, CB_SETCURSEL, i, 0);
                SendMessageW(self.clock24, BM_SETCHECK, check(!s.twelve_hour), 0);
                SendMessageW(self.clock12, BM_SETCHECK, check(s.twelve_hour), 0);
                SendMessageW(self.stats, BM_SETCHECK, check(s.stats), 0);
            }
        }
    }
}
