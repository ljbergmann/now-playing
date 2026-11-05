//! The Mac settings window: real AppKit controls (⌘,), no browser. Clicks
//! become `Action`s that the window loop picks up with `next()`; `refresh`
//! shows the current state. VinylVault signs in through macOS's own sign-in
//! sheet (ASWebAuthenticationSession) attached to this window.

use std::sync::Mutex;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSButton, NSControlStateValueOff, NSControlStateValueOn, NSFont, NSGridCell,
    NSGridCellPlacement, NSGridRowAlignment, NSGridView, NSPopUpButton, NSStackView, NSTextField,
    NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_authentication_services::{ASWebAuthenticationPresentationContextProviding, ASWebAuthenticationSession};
use objc2_foundation::{NSArray, NSEdgeInsets, NSError, NSPoint, NSRect, NSSize, NSString, NSURL};

use super::settings_ui::{Action, State};
use crate::scene::Theme;

static ACTIONS: Mutex<Vec<Action>> = Mutex::new(Vec::new());
/// Set when the sign-in sheet closes (by hand, or cancelled by us).
static SHEET_CLOSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn push(a: Action) {
    if let Ok(mut q) = ACTIONS.lock() {
        q.push(a);
    }
}

pub fn next() -> Option<Action> {
    let mut q = ACTIONS.lock().ok()?;
    (!q.is_empty()).then(|| q.remove(0))
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NowPlayingSettingsHandler"]
    #[ivars = Retained<NSTextField>]
    struct Handler;

    impl Handler {
        #[unsafe(method(lastfm:))]
        fn lastfm(&self, _sender: &AnyObject) {
            push(Action::LastfmUser(self.ivars().stringValue().to_string()));
        }

        #[unsafe(method(vinylvault:))]
        fn vinylvault(&self, _sender: &AnyObject) {
            push(Action::ConnectVinylVault);
        }

        #[unsafe(method(disconnect:))]
        fn disconnect(&self, _sender: &AnyObject) {
            push(Action::DisconnectVinylVault);
        }

        #[unsafe(method(look:))]
        fn look(&self, sender: &NSPopUpButton) {
            let t = match sender.indexOfSelectedItem() {
                1 => Theme::Poster,
                2 => Theme::Ambient,
                _ => Theme::Vinyl,
            };
            push(Action::Look(t));
        }

        #[unsafe(method(clock:))]
        fn clock(&self, sender: &NSButton) {
            push(Action::TwelveHour(sender.tag() == 12));
        }

        #[unsafe(method(stats:))]
        fn stats(&self, sender: &NSButton) {
            push(Action::Stats(sender.state() == NSControlStateValueOn));
        }

        #[unsafe(method(ownedHint:))]
        fn owned_hint(&self, sender: &NSButton) {
            push(Action::OwnedHint(sender.state() == NSControlStateValueOn));
        }
    }
);

define_class!(
    // SAFETY: as above. Tells the sign-in sheet which window to attach to.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NowPlayingSignInAnchor"]
    #[ivars = Retained<NSWindow>]
    struct Anchor;

    unsafe impl NSObjectProtocol for Anchor {}

    unsafe impl ASWebAuthenticationPresentationContextProviding for Anchor {
        #[unsafe(method_id(presentationAnchorForWebAuthenticationSession:))]
        fn anchor(&self, _session: &ASWebAuthenticationSession) -> Retained<NSWindow> {
            self.ivars().clone()
        }
    }
);

pub struct SettingsWindow {
    window: Retained<NSWindow>,
    _handler: Retained<Handler>,
    lastfm_status: Retained<NSTextField>,
    vv_status: Retained<NSTextField>,
    vv_button: Retained<NSButton>,
    vv_disconnect: Retained<NSButton>,
    look: Retained<NSPopUpButton>,
    clock24: Retained<NSButton>,
    clock12: Retained<NSButton>,
    stats: Retained<NSButton>,
    owned_hint: Retained<NSButton>,
    /// A running sign-in sheet, and the pass that existed before it started.
    sign_in: Option<(Retained<ASWebAuthenticationSession>, Retained<Anchor>, Option<String>)>,
    /// What the controls were last set to (look, 12-hour, stats). They're
    /// only touched when this changes: the window refreshes twice a second,
    /// also while a popup is open, and must not undo a choice being made.
    shown: Option<(Theme, bool, bool, bool)>,
}

fn label(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    NSTextField::labelWithString(&NSString::from_str(text), mtm)
}

fn button(text: &str, handler: &Handler, action: objc2::runtime::Sel, mtm: MainThreadMarker) -> Retained<NSButton> {
    unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(text), Some(handler), Some(action), mtm) }
}

fn row(views: &[&NSView], mtm: MainThreadMarker) -> Retained<NSStackView> {
    let s = NSStackView::stackViewWithViews(&NSArray::from_slice(views), mtm);
    s.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    s.setSpacing(8.0);
    s
}

impl SettingsWindow {
    pub fn new(mtm: MainThreadMarker, current_user: Option<&str>) -> Self {
        let user = NSTextField::textFieldWithString(&NSString::from_str(current_user.unwrap_or("")), mtm);
        user.setPlaceholderString(Some(&NSString::from_str("Last.fm user name")));
        user.widthAnchor().constraintEqualToConstant(200.0).setActive(true);

        let handler: Retained<Handler> = {
            let this = Handler::alloc(mtm).set_ivars(user.clone());
            unsafe { msg_send![super(this), init] }
        };
        let h: &Handler = &handler;
        // Return in the field = Save.
        unsafe {
            user.setTarget(Some(h));
            user.setAction(Some(sel!(lastfm:)));
        }

        let save = button("Save", h, sel!(lastfm:), mtm);
        let lastfm_status = label("", mtm);
        let vv_status = label("", mtm);
        let vv_button = button("Connect with VinylVault…", h, sel!(vinylvault:), mtm);
        let vv_disconnect = button("Disconnect", h, sel!(disconnect:), mtm);

        let look = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
        for t in ["Vinyl", "Poster", "Ambient"] {
            look.addItemWithTitle(&NSString::from_str(t));
        }
        unsafe {
            look.setTarget(Some(h));
            look.setAction(Some(sel!(look:)));
        }
        // Created with a zero frame: give it room for "Ambient" and the arrows.
        look.widthAnchor().constraintGreaterThanOrEqualToConstant(140.0).setActive(true);

        let radio = |text: &str, tag: isize| {
            let b = unsafe { NSButton::radioButtonWithTitle_target_action(&NSString::from_str(text), Some(h), Some(sel!(clock:)), mtm) };
            b.setTag(tag);
            b
        };
        let clock24 = radio("24-hour (21:47)", 24);
        let clock12 = radio("12-hour (9:47 PM)", 12);
        let owned_hint = unsafe {
            NSButton::checkboxWithTitle_target_action(&NSString::from_str("Remind me when I stream a record I own"), Some(h), Some(sel!(ownedHint:)), mtm)
        };
        let stats = unsafe {
            NSButton::checkboxWithTitle_target_action(&NSString::from_str("Show my play counts under the track"), Some(h), Some(sel!(stats:)), mtm)
        };

        let heading = |t: &str| {
            let l = label(t, mtm);
            l.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
            l
        };
        let empty = || NSGridCell::emptyContentView(mtm);
        let lastfm_row = row(&[&user, &save], mtm);
        let vv_buttons = row(&[&vv_button, &vv_disconnect], mtm);
        let clocks = row(&[&clock24, &clock12], mtm);
        let rows: Vec<Retained<NSArray<NSView>>> = vec![
            NSArray::from_slice(&[&*heading("Last.fm") as &NSView, &lastfm_row]),
            NSArray::from_slice(&[&*empty(), &lastfm_status as &NSView]),
            NSArray::from_slice(&[&*heading("VinylVault") as &NSView, &vv_status]),
            NSArray::from_slice(&[&*empty(), &vv_buttons as &NSView]),
            NSArray::from_slice(&[&*empty(), &owned_hint as &NSView]),
            NSArray::from_slice(&[&*heading("Look") as &NSView, &look]),
            NSArray::from_slice(&[&*heading("Clock") as &NSView, &clocks]),
            NSArray::from_slice(&[&*heading("Play counts") as &NSView, &stats]),
        ];
        let row_refs: Vec<&NSArray<NSView>> = rows.iter().map(|r| &**r).collect();
        let grid = NSGridView::gridViewWithViews(&NSArray::from_slice(&row_refs), mtm);
        grid.setRowSpacing(10.0);
        grid.setColumnSpacing(12.0);
        grid.setRowAlignment(NSGridRowAlignment::FirstBaseline);
        grid.columnAtIndex(0).setXPlacement(NSGridCellPlacement::Trailing);
        // A little more air above each section.
        for i in [2, 5] {
            grid.rowAtIndex(i).setTopPadding(10.0);
        }

        let content = NSStackView::stackViewWithViews(&NSArray::from_slice(&[&*grid as &NSView]), mtm);
        content.setEdgeInsets(NSEdgeInsets { top: 20.0, left: 24.0, bottom: 20.0, right: 24.0 });

        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(480.0, 320.0)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str("Now Playing Settings"));
        window.setContentView(Some(&content));
        window.setContentSize(content.fittingSize());
        window.center();

        SettingsWindow {
            window,
            _handler: handler,
            lastfm_status,
            vv_status,
            vv_button,
            vv_disconnect,
            look,
            clock24,
            clock12,
            stats,
            owned_hint,
            sign_in: None,
            shown: None,
        }
    }

    pub fn show(&self, mtm: MainThreadMarker) {
        NSApplication::sharedApplication(mtm).activate();
        self.window.makeKeyAndOrderFront(None);
    }

    pub fn visible(&self) -> bool {
        self.window.isVisible()
    }

    /// Opens macOS's sign-in sheet at `url` (VinylVault's Allow page). The
    /// local setup server receives the answer; `refresh` closes the sheet
    /// once the new pass is saved.
    pub fn sign_in(&mut self, url: &str, mtm: MainThreadMarker) {
        if self.sign_in.is_some() {
            return;
        }
        let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url)) else { return };
        let anchor: Retained<Anchor> = {
            let this = Anchor::alloc(mtm).set_ivars(self.window.clone());
            unsafe { msg_send![super(this), init] }
        };
        // Called when the sheet is closed by hand (or cancelled below).
        let done = RcBlock::new(|_url: *mut NSURL, _err: *mut NSError| {
            SHEET_CLOSED.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        // The older initializer still works everywhere (macOS 10.15+); the
        // newer one needs macOS 14.4.
        #[allow(deprecated)]
        let session = unsafe {
            ASWebAuthenticationSession::initWithURL_callbackURLScheme_completionHandler(
                ASWebAuthenticationSession::alloc(),
                &nsurl,
                Some(&NSString::from_str("nowplaying")),
                RcBlock::as_ptr(&done),
            )
        };
        unsafe {
            session.setPresentationContextProvider(Some(ProtocolObject::from_ref(&*anchor)));
            // Use the browser's existing VinylVault sign-in, if any.
            session.setPrefersEphemeralWebBrowserSession(false);
        }
        if unsafe { session.start() } {
            self.sign_in = Some((session, anchor, crate::settings::get("VINYLVAULT_TOKEN")));
        }
    }

    /// Shows the current state in the controls.
    pub fn refresh(&mut self, s: &State) {
        let set = |l: &NSTextField, t: &str| {
            if l.stringValue().to_string() != t {
                l.setStringValue(&NSString::from_str(t));
            }
        };
        let lastfm = match (&s.lastfm_note, &s.lastfm_user) {
            (Some((_, msg)), _) => msg.clone(),
            (None, Some(_)) => "✓ Connected".into(),
            (None, None) => "Enter your user name and press Save.".into(),
        };
        set(&self.lastfm_status, &lastfm);
        match &s.vinylvault {
            Some(owner) if !owner.is_empty() => set(&self.vv_status, &format!("✓ Connected as {owner}")),
            Some(_) => set(&self.vv_status, "✓ Connected"),
            None if s.shelf_link => set(&self.vv_status, "Using a shared shelf link"),
            None => set(&self.vv_status, "Not connected (optional)"),
        }
        self.vv_button.setTitle(&NSString::from_str(if s.vinylvault.is_some() { "Reconnect…" } else { "Connect with VinylVault…" }));
        self.vv_disconnect.setEnabled(s.vinylvault.is_some());
        let now = (s.theme, s.twelve_hour, s.stats, s.owned_hint);
        if self.shown != Some(now) {
            self.shown = Some(now);
            let i = match s.theme {
                Theme::Vinyl => 0,
                Theme::Poster => 1,
                Theme::Ambient => 2,
            };
            if self.look.indexOfSelectedItem() != i {
                self.look.selectItemAtIndex(i);
            }
            let on_off = |b: bool| if b { NSControlStateValueOn } else { NSControlStateValueOff };
            self.clock24.setState(on_off(!s.twelve_hour));
            self.clock12.setState(on_off(s.twelve_hour));
            self.stats.setState(on_off(s.stats));
            self.owned_hint.setState(on_off(s.owned_hint));
        }

        if SHEET_CLOSED.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.sign_in = None;
        }
        // A new pass arrived: the sign-in is done, close the sheet.
        if let Some((session, _, before)) = &self.sign_in {
            if crate::settings::get("VINYLVAULT_TOKEN") != *before {
                unsafe { session.cancel() };
                self.sign_in = None;
            }
        }
    }

}
