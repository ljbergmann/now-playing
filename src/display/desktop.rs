//! The desktop window: Mac, Windows, Linux with a desktop. Resizable, or
//! fullscreen on any monitor. Menus: the menu bar on the Mac, a right-click
//! menu on Mac and Windows. Keys everywhere: 1/2/3 look, I clock preview,
//! S settings, F fullscreen, Esc leaves fullscreen / quits.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::monitor::MonitorHandle;
use winit::window::{Fullscreen, Window, WindowId};

use crate::scene::Theme;
use crate::{Command, Player, settings};

const FRAME: Duration = Duration::from_nanos(16_666_667);

/// `open_settings`: show the settings right away (not set up yet).
pub fn run(player: Player, size: (usize, usize), open_settings: bool) -> Result<(), String> {
    let el = EventLoop::new().map_err(|e| e.to_string())?;
    // macOS throttles apps in the background ("App Nap"): timers and network
    // were held back for seconds while Tidal was in front. A display has to
    // react at once, so opt out for as long as the app runs (the Mac may
    // still sleep).
    #[cfg(target_os = "macos")]
    let _awake = {
        use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
        NSProcessInfo::processInfo().beginActivityWithOptions_reason(
            NSActivityOptions::UserInitiatedAllowingIdleSystemSleep | NSActivityOptions::LatencyCritical,
            &NSString::from_str("Showing what's playing"),
        )
    };
    let mut app = App {
        player,
        size,
        window: None,
        surface: None,
        next: Instant::now(),
        last_click: None,
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        menus: None,
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        settings_win: None,
        #[cfg(target_os = "windows")]
        sign_in: None,
        refreshed: Instant::now(),
        open_settings,
        error: None,
    };
    el.run_app(&mut app).map_err(|e| e.to_string())?;
    app.error.map_or(Ok(()), Err)
}

struct App {
    player: Player,
    size: (usize, usize),
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    next: Instant,
    last_click: Option<Instant>,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    menus: Option<menus::Menus>,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    settings_win: Option<NativeSettings>,
    /// Windows: the VinylVault sign-in window, its web view, and the pass
    /// that existed before (a new one means: done, close it).
    #[cfg(target_os = "windows")]
    sign_in: Option<(wry::WebView, Rc<Window>, Option<String>)>,
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    refreshed: Instant,
    open_settings: bool,
    error: Option<String>,
}

#[cfg(target_os = "macos")]
type NativeSettings = super::settings_mac::SettingsWindow;
#[cfg(target_os = "windows")]
type NativeSettings = super::settings_win::SettingsWindow;

/// The saved screen (`SCREEN`), else the one the window is on.
fn pick_monitor(el: &ActiveEventLoop, w: &Window) -> Option<MonitorHandle> {
    let want = settings::get("SCREEN");
    want.and_then(|n| el.available_monitors().find(|m| m.name().as_deref() == Some(n.as_str()))).or_else(|| w.current_monitor())
}

fn fullscreen_saved() -> bool {
    matches!(settings::get("FULLSCREEN").as_deref(), Some("on" | "1" | "true" | "yes"))
}

impl App {
    fn command(&mut self, el: &ActiveEventLoop, cmd: Command) {
        let Some(w) = self.window.clone() else { return };
        match cmd {
            Command::Fullscreen => {
                let on = w.fullscreen().is_none();
                w.set_fullscreen(on.then(|| Fullscreen::Borderless(pick_monitor(el, &w))));
                let _ = settings::save(&[("FULLSCREEN", Some(if on { "on" } else { "off" }))]);
            }
            Command::Screen(name) => {
                let _ = settings::save(&[("SCREEN", Some(&name))]);
                if let Some(m) = el.available_monitors().find(|m| m.name().as_deref() == Some(name.as_str())) {
                    if w.fullscreen().is_some() {
                        w.set_fullscreen(Some(Fullscreen::Borderless(Some(m))));
                    } else {
                        w.set_outer_position(m.position());
                    }
                }
            }
            Command::Quit => el.exit(),
            // Mac and Windows have native settings windows; Linux uses the setup page.
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Command::Settings => self.show_settings(),
            other => self.player.command(other),
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(m) = &self.menus {
            m.sync(self.player.theme(), w.fullscreen().is_some());
        }
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn show_settings(&mut self) {
        let user = settings::get("LASTFM_USER");
        #[cfg(target_os = "macos")]
        {
            let Some(mtm) = objc2::MainThreadMarker::new() else { return };
            let win = self.settings_win.get_or_insert_with(|| NativeSettings::new(mtm, user.as_deref()));
            win.refresh(&super::settings_ui::State::current(self.player.theme()));
            win.show(mtm);
        }
        #[cfg(target_os = "windows")]
        {
            if self.settings_win.is_none() {
                self.settings_win = NativeSettings::new(user.as_deref());
            }
            let Some(win) = self.settings_win.as_mut() else { return };
            win.refresh(&super::settings_ui::State::current(self.player.theme()));
            win.show();
        }
    }

    /// The VinylVault sign-in: macOS's sign-in sheet on the Mac, a small
    /// in-app window with the system web view (WebView2) on Windows.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn sign_in(&mut self, el: &ActiveEventLoop) {
        let Some(url) = self.player.vinylvault_connect_url() else { return };
        #[cfg(target_os = "macos")]
        {
            let _ = el;
            if let (Some(win), Some(mtm)) = (self.settings_win.as_mut(), objc2::MainThreadMarker::new()) {
                win.sign_in(&url, mtm);
            }
        }
        #[cfg(target_os = "windows")]
        {
            if self.sign_in.is_some() {
                return;
            }
            let attrs = Window::default_attributes().with_title("Sign in to VinylVault").with_inner_size(LogicalSize::new(520.0, 700.0));
            let Ok(w) = el.create_window(attrs) else { return };
            let w = Rc::new(w);
            match wry::WebViewBuilder::new().with_url(&url).build(&*w) {
                Ok(view) => self.sign_in = Some((view, w, settings::get("VINYLVAULT_TOKEN"))),
                Err(e) => eprintln!("VinylVault sign-in window: {e}"),
            }
        }
    }

    /// Clicks in the settings window, and keeping it up to date (a sign-in
    /// finishes on its own, so the window has to notice by itself).
    fn settings_tick(&mut self, el: &ActiveEventLoop) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            #[cfg(target_os = "macos")]
            use super::settings_mac::next;
            #[cfg(target_os = "windows")]
            use super::settings_win::next;
            let mut changed = super::settings_ui::tick(&self.player);
            while let Some(a) = next() {
                if let super::settings_ui::Action::ConnectVinylVault = a {
                    self.sign_in(el);
                } else {
                    super::settings_ui::apply(&mut self.player, a);
                }
                changed = true;
            }
            // Windows: a new pass arrived, the sign-in is done.
            #[cfg(target_os = "windows")]
            if self.sign_in.as_ref().is_some_and(|(_, _, before)| settings::get("VINYLVAULT_TOKEN") != *before) {
                self.sign_in = None;
                changed = true;
            }
            let Some(win) = self.settings_win.as_mut() else { return };
            if changed || (win.visible() && self.refreshed.elapsed() > Duration::from_millis(500)) {
                self.refreshed = Instant::now();
                win.refresh(&super::settings_ui::State::current(self.player.theme()));
            }
            if changed {
                if let (Some(m), Some(w)) = (&self.menus, &self.window) {
                    m.sync(self.player.theme(), w.fullscreen().is_some());
                }
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let _ = el;
    }

    fn key(&mut self, el: &ActiveEventLoop, ev: KeyEvent) {
        if ev.state != ElementState::Pressed || ev.repeat {
            return;
        }
        let cmd = match ev.logical_key.as_ref() {
            Key::Character("1") => Command::Look(Theme::Vinyl),
            Key::Character("2") => Command::Look(Theme::Poster),
            Key::Character("3") => Command::Look(Theme::Ambient),
            Key::Character("i" | "I") => Command::IdlePreview,
            Key::Character("s" | "S") => Command::Settings,
            Key::Character("f" | "F") => Command::Fullscreen,
            Key::Named(NamedKey::Escape) => {
                if self.window.as_ref().is_some_and(|w| w.fullscreen().is_some()) {
                    Command::Fullscreen
                } else {
                    Command::Quit
                }
            }
            _ => return,
        };
        self.command(el, cmd);
    }

    fn draw(&mut self) -> Result<(), String> {
        let (Some(w), Some(surface)) = (&self.window, self.surface.as_mut()) else { return Ok(()) };
        let size = w.inner_size();
        let (Some(wd), Some(ht)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return Ok(()) };
        let (sw, sh) = (size.width as usize, size.height as usize);
        if (sw, sh) != self.player.size() {
            self.player.resize(sw, sh);
        }
        self.player.frame();
        surface.resize(wd, ht).map_err(|e| e.to_string())?;
        let mut buf = surface.buffer_mut().map_err(|e| e.to_string())?;
        let out = self.player.pixels();
        if buf.len() == out.len() {
            buf.copy_from_slice(out);
        }
        buf.present().map_err(|e| e.to_string())
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Now Playing")
            .with_inner_size(LogicalSize::new(self.size.0 as f64, self.size.1 as f64));
        let w = match el.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                self.error = Some(e.to_string());
                return el.exit();
            }
        };
        let surface = softbuffer::Context::new(w.clone()).and_then(|c| softbuffer::Surface::new(&c, w.clone()));
        match surface {
            Ok(s) => self.surface = Some(s),
            Err(e) => {
                self.error = Some(e.to_string());
                return el.exit();
            }
        }
        if fullscreen_saved() {
            w.set_fullscreen(Some(Fullscreen::Borderless(pick_monitor(el, &w))));
        } else if let Some(m) = settings::get("SCREEN").and_then(|n| el.available_monitors().find(|m| m.name().as_deref() == Some(n.as_str()))) {
            w.set_outer_position(m.position());
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let names: Vec<String> = el.available_monitors().filter_map(|m| m.name()).collect();
            let m = menus::Menus::new(&names);
            m.sync(self.player.theme(), w.fullscreen().is_some());
            self.menus = Some(m);
        }
        self.window = Some(w);
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if crate::quit_requested() {
            return el.exit();
        }
        if crate::take_setup_request() || std::mem::take(&mut self.open_settings) {
            self.command(el, Command::Settings);
        }
        self.settings_tick(el);
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        while let Some(cmd) = menus::next() {
            self.command(el, cmd);
        }
        let now = Instant::now();
        if now >= self.next {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            // Keep the 60 Hz rhythm, but don't try to catch up after a stall.
            self.next = (self.next + FRAME).max(now);
        }
        el.set_control_flow(ControlFlow::WaitUntil(self.next));
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, ev: WindowEvent) {
        // The Windows sign-in window: only closing matters.
        #[cfg(target_os = "windows")]
        if self.sign_in.as_ref().is_some_and(|(_, w, _)| w.id() == id) {
            if let WindowEvent::CloseRequested = ev {
                self.sign_in = None;
            }
            return;
        }
        if self.window.as_ref().is_some_and(|w| w.id() != id) {
            return;
        }
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.draw() {
                    self.error = Some(e);
                    el.exit();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => self.key(el, event),
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                // Double-click: fullscreen on/off.
                let now = Instant::now();
                if self.last_click.is_some_and(|t| now - t < Duration::from_millis(400)) {
                    self.last_click = None;
                    self.command(el, Command::Fullscreen);
                } else {
                    self.last_click = Some(now);
                }
            }
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                if let (Some(m), Some(w)) = (&self.menus, &self.window) {
                    m.popup(w);
                }
            }
            _ => {}
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod menus {
    use muda::accelerator::{Accelerator, Code, Modifiers};
    #[cfg(target_os = "windows")]
    use muda::ContextMenu;
    use muda::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    use crate::Command;
    use crate::scene::Theme;

    pub struct Menus {
        _bar: Menu,
        context: Submenu,
        looks: [CheckMenuItem; 3],
        looks_ctx: [CheckMenuItem; 3],
        fullscreen: [CheckMenuItem; 2],
    }

    fn look_items() -> [CheckMenuItem; 3] {
        [
            CheckMenuItem::with_id("look:vinyl", "Vinyl", true, false, None),
            CheckMenuItem::with_id("look:poster", "Poster", true, false, None),
            CheckMenuItem::with_id("look:ambient", "Ambient", true, false, None),
        ]
    }

    fn screens(names: &[String]) -> Submenu {
        let sub = Submenu::new("Screen", names.len() > 1);
        for n in names {
            let _ = sub.append(&MenuItem::with_id(format!("screen:{n}"), n, true, None));
        }
        sub
    }

    impl Menus {
        pub fn new(screen_names: &[String]) -> Self {
            let looks = look_items();
            let looks_ctx = look_items();
            let fs = || CheckMenuItem::with_id("fullscreen", "Fullscreen", true, false, None);
            let fullscreen = [fs(), fs()];
            let settings = || MenuItem::with_id("settings", "Settings…", true, Some(Accelerator::new(Modifiers::META, Code::Comma)));
            let idle = || MenuItem::with_id("idle", "Preview Clock Screen", true, None);

            // Menu bar (the Mac shows it; Windows uses only the right-click menu).
            let bar = Menu::new();
            let app = Submenu::new("Now Playing", true);
            let _ = app.append_items(&[&PredefinedMenuItem::about(None, None), &PredefinedMenuItem::separator(), &settings(), &PredefinedMenuItem::separator(), &PredefinedMenuItem::quit(None)]);
            let look = Submenu::new("Look", true);
            let _ = look.append_items(&[&looks[0], &looks[1], &looks[2], &PredefinedMenuItem::separator(), &idle()]);
            let view = Submenu::new("View", true);
            let _ = view.append_items(&[&fullscreen[0], &screens(screen_names)]);
            let _ = bar.append_items(&[&app, &look, &view]);
            #[cfg(target_os = "macos")]
            bar.init_for_nsapp();

            let context = Submenu::new("Now Playing", true);
            let _ = context.append_items(&[
                &settings(),
                &PredefinedMenuItem::separator(),
                &looks_ctx[0],
                &looks_ctx[1],
                &looks_ctx[2],
                &idle(),
                &PredefinedMenuItem::separator(),
                &fullscreen[1],
                &screens(screen_names),
                &PredefinedMenuItem::separator(),
                &MenuItem::with_id("quit", "Quit", true, None),
            ]);
            Menus { _bar: bar, context, looks, looks_ctx, fullscreen }
        }

        /// Ticks next to the current look and fullscreen state.
        pub fn sync(&self, theme: Theme, fullscreen: bool) {
            let i = match theme {
                Theme::Vinyl => 0,
                Theme::Poster => 1,
                Theme::Ambient => 2,
            };
            for set in [&self.looks, &self.looks_ctx] {
                for (k, item) in set.iter().enumerate() {
                    item.set_checked(k == i);
                }
            }
            self.fullscreen.iter().for_each(|f| f.set_checked(fullscreen));
        }

        pub fn popup(&self, w: &Window) {
            let Ok(handle) = w.window_handle() else { return };
            match handle.as_raw() {
                // A context menu blocks whoever opens it. Open it from the main
                // queue after this event returns, so the 60 fps loop keeps running.
                #[cfg(target_os = "macos")]
                RawWindowHandle::AppKit(h) => {
                    let job = Box::new((self.context.clone(), h.ns_view.as_ptr() as usize));
                    unsafe { mac::dispatch_async_f(&mac::_dispatch_main_q, Box::into_raw(job) as *mut _, mac::popup) };
                }
                #[cfg(target_os = "windows")]
                RawWindowHandle::Win32(h) => unsafe {
                    self.context.show_context_menu_for_hwnd(h.hwnd.get(), None);
                },
                _ => {}
            }
        }
    }

    /// A clicked menu item, as a command.
    pub fn next() -> Option<Command> {
        let ev = MenuEvent::receiver().try_recv().ok()?;
        let id = ev.id().as_ref();
        Some(match id {
            "settings" => Command::Settings,
            "idle" => Command::IdlePreview,
            "fullscreen" => Command::Fullscreen,
            "quit" => Command::Quit,
            "look:vinyl" => Command::Look(Theme::Vinyl),
            "look:poster" => Command::Look(Theme::Poster),
            "look:ambient" => Command::Look(Theme::Ambient),
            s => Command::Screen(s.strip_prefix("screen:")?.to_string()),
        })
    }

    #[cfg(target_os = "macos")]
    mod mac {
        use muda::{ContextMenu, Submenu};

        #[repr(C)]
        pub struct DispatchQueue {
            _private: [u8; 0],
        }
        unsafe extern "C" {
            pub static _dispatch_main_q: DispatchQueue;
            pub fn dispatch_async_f(q: *const DispatchQueue, ctx: *mut std::ffi::c_void, f: extern "C" fn(*mut std::ffi::c_void));
        }
        pub extern "C" fn popup(ctx: *mut std::ffi::c_void) {
            // SAFETY: `ctx` is the Box leaked in `Menus::popup`, used exactly once.
            let job = unsafe { Box::from_raw(ctx as *mut (Submenu, usize)) };
            unsafe { job.0.show_context_menu_for_nsview(job.1 as _, None) };
        }
    }
}
