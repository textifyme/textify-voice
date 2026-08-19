//! The menu-bar status item — the app's only visible chrome.
//!
//! `textify-voice` runs `Accessory` (no Dock icon, no app-switcher entry;
//! `hud.rs` sets that policy) precisely so dictation targets keep focus. That
//! means the status item is the *only* place the user can see "is this thing
//! armed, listening, or broken" without switching away from what they're
//! typing into — so it has to be legible at a glance, in both menu-bar
//! appearances, without ever stealing focus itself.
//!
//! Three details here are load-bearing:
//!
//! 1. **Template images.** `NSStatusBarButton` images rendered with
//!    `setTemplate(true)` are recolored by AppKit from their alpha channel
//!    alone — the RGB values are thrown away. That is *why* a template image
//!    survives a light/dark menu bar without any theme-detection code here:
//!    draw a black silhouette on a transparent ground and the OS does the
//!    rest. It also means distinct states must be distinct *silhouettes*
//!    (different SF Symbols / different drawn shapes), not different colors
//!    — a colored dot would look identical to the user in both appearances.
//! 2. **Menu actions never block the CFRunLoop.** `dictate.rs` pumps its run
//!    loop at ~60 Hz on the main thread and must never stall; the objc2
//!    `define_class!` target here only ever pushes an `MenuEvent` onto an
//!    `mpsc` channel from inside its action method (mirrors
//!    `holdkey.rs`'s `TapState`/`Sender` pattern exactly). `poll_events`
//!    drains it the same non-blocking way `HoldKeySource::poll` does.
//! 3. **This struct owns display, not behavior.** Clicking "Dictation
//!    Armed" does not flip the checkmark itself — it only emits
//!    `MenuEvent::ToggleArmed`. The checkmark updates only when the caller
//!    (whichever unit owns the real armed/hold-key state) calls
//!    `set_armed`/`set_hold_key`/`set_state` back. That keeps this file free
//!    of any opinion about what "armed" actually does, which is exactly the
//!    boundary the dispatch for this unit draws.
//!
//! The `define_class!` target/action wiring below follows the pattern
//! proven to actually dispatch (not just compile) via
//! `NSApplication.sendAction:to:from:` in this wave's objc2 recon MWE —
//! see that probe's `main.rs` for the executed proof this is modeled on.
//!
//! NOT VERIFIED: nobody has looked at the menu bar. A launched process that
//! constructs a `MenuBar` without crashing is the furthest this environment
//! can check — see this crate's notes for exactly what was and wasn't run.

use std::sync::mpsc::{self, Receiver, Sender};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSControlStateValueOff, NSControlStateValueOn, NSImage, NSMenu,
    NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{ns_string, MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

/// Everything the status item can be showing at once.
///
/// Deliberately flat rather than `(armed: bool, busy: bool, ...)` — the
/// dispatch calls for a state distinguishable "at a glance", and a small
/// closed enum is what makes the image-mapping below exhaustive and
/// impossible to leave a state unhandled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuBarState {
    /// Armed and waiting for the hold key. The common resting state.
    Idle,
    /// Hold key is down, capturing audio.
    Listening,
    /// Hold key released, whisper.cpp is running.
    Transcribing,
    /// The last utterance failed (capture, ASR, or insertion).
    Error,
    /// Microphone and/or Accessibility has not been granted; dictation
    /// cannot run at all.
    PermissionsMissing,
}

impl MenuBarState {
    /// Human-readable label for the "current state" menu row.
    pub fn label(self) -> &'static str {
        match self {
            MenuBarState::Idle => "Idle",
            MenuBarState::Listening => "Listening…",
            MenuBarState::Transcribing => "Transcribing…",
            MenuBarState::Error => "Error",
            MenuBarState::PermissionsMissing => "Permissions needed",
        }
    }

    /// SF Symbol name whose silhouette reads as this state on its own —
    /// see the module docs on why silhouette (not color) is what has to
    /// differ. All five are stock symbols shipped since macOS 11.
    fn sf_symbol_name(self) -> &'static str {
        match self {
            MenuBarState::Idle => "mic",
            MenuBarState::Listening => "waveform",
            MenuBarState::Transcribing => "arrow.triangle.2.circlepath",
            MenuBarState::Error => "exclamationmark.triangle.fill",
            MenuBarState::PermissionsMissing => "lock.fill",
        }
    }
}

/// What the menu observed. Pushed from the objc2 action target, drained by
/// `MenuBar::poll_events` on whatever cadence the caller's loop runs at.
///
/// Behavior-free by design (see module docs point 3): this only says *what
/// happened in the menu*, never what it should cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuEvent {
    /// The "Dictation Armed" row was clicked.
    ToggleArmed,
    /// "Settings…" was clicked.
    OpenSettings,
    /// "Quit" was clicked.
    Quit,
    /// "Check for Updates…" was clicked.
    CheckForUpdates,
}

/// Icon canvas size in points. 18pt is the conventional macOS menu-bar
/// glyph size — big enough to read, small enough not to crowd neighbors.
const ICON_SIDE: f64 = 18.0;

// NSMenuItem tags used to route the single shared action selector back to
// a `MenuEvent` (see `MenuBarTarget::menu_action` below). Plain integers,
// not an enum discriminant cast, because `tag()` crosses the objc boundary
// as `NSInteger` and we want the mapping visible in one place
// (`event_for_tag`) rather than relied on to stay in sync by position.
const TAG_TOGGLE_ARMED: objc2::ffi::NSInteger = 1;
const TAG_OPEN_SETTINGS: objc2::ffi::NSInteger = 2;
const TAG_QUIT: objc2::ffi::NSInteger = 3;
const TAG_CHECK_UPDATES: objc2::ffi::NSInteger = 4;

fn event_for_tag(tag: objc2::ffi::NSInteger) -> Option<MenuEvent> {
    match tag {
        TAG_TOGGLE_ARMED => Some(MenuEvent::ToggleArmed),
        TAG_OPEN_SETTINGS => Some(MenuEvent::OpenSettings),
        TAG_QUIT => Some(MenuEvent::Quit),
        TAG_CHECK_UPDATES => Some(MenuEvent::CheckForUpdates),
        _ => None,
    }
}

/// Ivars for the objc2 target object. Just the channel half the action
/// method needs to push into — mirrors `holdkey.rs`'s `TapState { tx, .. }`.
struct MenuTargetIvars {
    tx: Sender<MenuEvent>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; MenuBarTarget has
    // no Drop impl that would conflict with objc's own deallocation.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuTargetIvars]
    struct MenuBarTarget;

    unsafe impl NSObjectProtocol for MenuBarTarget {}

    impl MenuBarTarget {
        /// The one action selector every actionable menu item is wired to.
        /// Which `MenuEvent` it means is read off the sender's `tag`
        /// (`event_for_tag`) rather than needing one Rust method per item —
        /// see `action_item` below for where the tag is set.
        #[unsafe(method(menuAction:))]
        fn menu_action(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            // SAFETY: every sender wired to this selector is one of this
            // module's own `NSMenuItem`s (see `action_item`), which responds
            // to plain-property `tag` like any `NSMenuItem`.
            let tag: objc2::ffi::NSInteger = unsafe { msg_send![sender, tag] };
            if let Some(event) = event_for_tag(tag) {
                // A full channel or a disconnected receiver both just mean
                // "nobody is listening right now" -- never a reason to panic
                // from inside an AppKit callback.
                let _ = self.ivars().tx.send(event);
            }
        }
    }
);

impl MenuBarTarget {
    fn new(mtm: MainThreadMarker, tx: Sender<MenuEvent>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(MenuTargetIvars { tx });
        // SAFETY: NSObject's `init` has the correct signature and every
        // objc2 object is initialized this way.
        unsafe { msg_send![super(this), init] }
    }
}

/// A disabled, non-actionable row used for the "current state" and "hold
/// key" lines -- informational only, so it must never be clickable.
fn info_item(mtm: MainThreadMarker, title: &str) -> Retained<NSMenuItem> {
    // SAFETY: `initWithTitle:action:keyEquivalent:` requires the selector
    // (if any) to be valid; we pass `None`, which is always valid.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            None,
            ns_string!(""),
        )
    };
    item.setEnabled(false);
    item
}

/// A clickable row wired to `target`'s shared `menuAction:` selector,
/// tagged so the handler can tell which row fired.
fn action_item(
    mtm: MainThreadMarker,
    title: &str,
    target: &MenuBarTarget,
    tag: objc2::ffi::NSInteger,
) -> Retained<NSMenuItem> {
    let action = sel!(menuAction:);
    // SAFETY: `menuAction:` is a real selector defined on `MenuBarTarget`
    // above, with a signature `NSMenuItem`'s action dispatch matches.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            ns_string!(""),
        )
    };
    let target_any: &AnyObject = target;
    // SAFETY: `target_any` outlives `item` (`MenuBar` holds both for its
    // whole lifetime) and responds to `menuAction:`.
    unsafe { item.setTarget(Some(target_any)) };
    item.setTag(tag);
    item
}

/// Draw a small filled/stroked silhouette for `state` when the SF Symbol
/// catalog can't resolve a name (headless environment, missing resource) --
/// item 1 of the dispatch calls for this fallback explicitly. Each state
/// gets a distinct shape (not just a color -- see module docs point 1).
///
/// `lockFocus`/`unlockFocus` are deprecated in favor of the block-based
/// `imageWithSize:flipped:drawingHandler:`, which needs the `block2` crate
/// this workspace does not otherwise depend on. Given this path only runs
/// as a fallback of a fallback (SF Symbol lookup failing is not expected on
/// any supported macOS version), the deprecated pair is the lower-risk
/// choice today; revisit if `block2` ever becomes a real dependency here.
#[allow(deprecated)]
fn drawn_fallback_image(state: MenuBarState) -> Retained<NSImage> {
    let size = NSSize::new(ICON_SIDE, ICON_SIDE);
    let image = NSImage::initWithSize(NSImage::alloc(), size);
    image.lockFocus();

    NSColor::blackColor().setFill();
    NSColor::blackColor().setStroke();

    match state {
        MenuBarState::Idle => {
            // A hollow ring: armed and waiting, nothing happening yet.
            let ring = NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
                NSPoint::new(3.0, 3.0),
                NSSize::new(12.0, 12.0),
            ));
            ring.setLineWidth(2.0);
            ring.stroke();
        }
        MenuBarState::Listening => {
            // A solid disc: capturing audio right now.
            let disc = NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
                NSPoint::new(2.0, 2.0),
                NSSize::new(14.0, 14.0),
            ));
            disc.fill();
        }
        MenuBarState::Transcribing => {
            // A solid square: visibly different silhouette from "listening"
            // for "working on it, not hearing you".
            let square = NSBezierPath::new();
            square.appendBezierPathWithRect(NSRect::new(
                NSPoint::new(3.0, 3.0),
                NSSize::new(12.0, 12.0),
            ));
            square.fill();
        }
        MenuBarState::Error => {
            // A filled triangle -- the universal "warning" silhouette.
            let triangle = NSBezierPath::new();
            triangle.moveToPoint(NSPoint::new(9.0, 15.0));
            triangle.lineToPoint(NSPoint::new(2.0, 3.0));
            triangle.lineToPoint(NSPoint::new(16.0, 3.0));
            triangle.closePath();
            triangle.fill();
        }
        MenuBarState::PermissionsMissing => {
            // A ring with a diagonal slash through it -- "blocked".
            let ring = NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
                NSPoint::new(3.0, 3.0),
                NSSize::new(12.0, 12.0),
            ));
            ring.setLineWidth(2.0);
            ring.stroke();
            let slash = NSBezierPath::new();
            slash.setLineWidth(2.0);
            slash.moveToPoint(NSPoint::new(3.0, 3.0));
            slash.lineToPoint(NSPoint::new(15.0, 15.0));
            slash.stroke();
        }
    }

    image.unlockFocus();
    image.setTemplate(true);
    image
}

/// The image for `state`: the real SF Symbol when the catalog resolves it,
/// the drawn fallback otherwise. Either way the result is a template image
/// (see module docs point 1).
fn state_image(state: MenuBarState) -> Retained<NSImage> {
    let name = NSString::from_str(state.sf_symbol_name());
    match NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &name,
        Some(&NSString::from_str(state.label())),
    ) {
        Some(image) => {
            image.setTemplate(true);
            image
        }
        None => drawn_fallback_image(state),
    }
}

/// The menu-bar status item. Construct once on the main thread; call
/// `set_state` / `set_hold_key` / `set_armed` whenever those change, and
/// drain `poll_events` from the same loop that pumps `dictate.rs`'s
/// `CFRunLoop` (never blocks -- see module docs point 2).
pub struct MenuBar {
    mtm: MainThreadMarker,
    status_item: Retained<NSStatusItem>,
    // Kept alive for the item's lifetime -- `NSStatusItem::setMenu` does not
    // retain beyond what ARC-equivalent `Retained` ownership already needs,
    // but holding it here keeps ownership visible and matches how `menu`'s
    // rows (`state_item` etc.) are held below.
    _menu: Retained<NSMenu>,
    state_item: Retained<NSMenuItem>,
    hold_key_item: Retained<NSMenuItem>,
    armed_item: Retained<NSMenuItem>,
    update_item: Retained<NSMenuItem>,
    // Kept alive: `setTarget` does not retain the callee across the
    // objc bridge in every AppKit version, and this Rust owner is what
    // actually keeps `MenuBarTarget` from being deallocated out from under
    // the menu items pointing at it.
    _target: Retained<MenuBarTarget>,
    rx: Receiver<MenuEvent>,
    state: MenuBarState,
    armed: bool,
}

impl MenuBar {
    /// Build the status item and its menu. Must run on the main thread --
    /// like `Hud::new` and `Tones::new`, this refuses rather than corrupting
    /// AppKit state silently if called elsewhere (item 4 of the dispatch).
    pub fn new() -> anyhow::Result<Self> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow::anyhow!("the menu bar must be created on the main thread"))?;

        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(NSVariableStatusItemLength);

        let (tx, rx) = mpsc::channel();
        let target = MenuBarTarget::new(mtm, tx);

        let menu = NSMenu::new(mtm);

        let initial_state = MenuBarState::Idle;
        let state_item = info_item(mtm, &format!("Status: {}", initial_state.label()));
        menu.addItem(&state_item);

        let hold_key_item = info_item(mtm, "Hold key: (not set)");
        menu.addItem(&hold_key_item);

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let armed_item = action_item(mtm, "Dictation Armed", &target, TAG_TOGGLE_ARMED);
        armed_item.setState(NSControlStateValueOff);
        menu.addItem(&armed_item);

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let settings_item = action_item(mtm, "Settings…", &target, TAG_OPEN_SETTINGS);
        menu.addItem(&settings_item);

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        // Informational row, updated by `set_update_text` -- the click
        // that actually checks/downloads/installs is the item right
        // below it. See `dictate::run_agent_macos`'s handling of
        // `MenuEvent::CheckForUpdates` for what a click does, which
        // depends on this row's last-pushed text.
        let update_item = info_item(mtm, "Update: not checked yet");
        menu.addItem(&update_item);

        let check_updates_item = action_item(mtm, "Check for Updates…", &target, TAG_CHECK_UPDATES);
        menu.addItem(&check_updates_item);

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let quit_item = action_item(mtm, "Quit", &target, TAG_QUIT);
        menu.addItem(&quit_item);

        status_item.setMenu(Some(&menu));

        // Item 5 of the dispatch: degrade rather than panic when the status
        // bar has no visible surface (headless / no window server). The
        // menu, channel and every setter below still work; only the icon
        // itself is silently skipped rather than unwrapped into a panic.
        if let Some(button) = status_item.button(mtm) {
            let image = state_image(initial_state);
            button.setImage(Some(&image));
        }

        Ok(Self {
            mtm,
            status_item,
            _menu: menu,
            state_item,
            hold_key_item,
            armed_item,
            update_item,
            _target: target,
            rx,
            state: initial_state,
            armed: false,
        })
    }

    /// Update the visible state: the "Status:" row's text and the status
    /// item's icon. Cheap enough to call on every `dictate.rs` loop
    /// transition (listening/transcribing/hide).
    pub fn set_state(&mut self, state: MenuBarState) {
        if self.state == state {
            return;
        }
        self.state = state;
        self.state_item.setTitle(&NSString::from_str(&format!("Status: {}", state.label())));
        if let Some(button) = self.status_item.button(self.mtm) {
            let image = state_image(state);
            button.setImage(Some(&image));
        }
    }

    /// Update the "Hold key:" row. Pass whatever `HoldKey::describe()`
    /// returns (this module does not depend on `platform::HoldKey` itself,
    /// so it stays usable from a non-macOS caller's test code too).
    pub fn set_hold_key(&mut self, description: &str) {
        self.hold_key_item.setTitle(&NSString::from_str(&format!("Hold key: {description}")));
    }

    /// Reflect the real armed/unarmed state as the row's checkmark.
    /// Deliberately does not flip itself when `ToggleArmed` is polled --
    /// see module docs point 3.
    pub fn set_armed(&mut self, armed: bool) {
        self.armed = armed;
        self.armed_item.setState(if armed { NSControlStateValueOn } else { NSControlStateValueOff });
    }

    /// Update the "Update:" row -- whatever `update::UpdateState`'s own
    /// `Display` impl renders ("up to date", "update available: 0.2.0",
    /// "downloading update (1234/5678 bytes)", "update ready -- relaunch
    /// to apply", or a failure message), plus this crate's own
    /// "not checked yet"/"checking for updates..." transient strings.
    /// Purely display -- see module docs point 3; the click that acts on
    /// whatever this currently says is `MenuEvent::CheckForUpdates`.
    pub fn set_update_text(&mut self, text: &str) {
        self.update_item.setTitle(&NSString::from_str(&format!("Update: {text}")));
    }

    pub fn state(&self) -> MenuBarState {
        self.state
    }

    pub fn armed(&self) -> bool {
        self.armed
    }

    /// Drain everything the menu has observed since the last call. Never
    /// blocks -- safe to call every tick of `dictate.rs`'s ~60 Hz loop
    /// alongside `HoldKeySource::poll`.
    pub fn poll_events(&self) -> Vec<MenuEvent> {
        self.rx.try_iter().collect()
    }
}

impl Drop for MenuBar {
    fn drop(&mut self) {
        // Good citizenship: remove the icon from the status bar rather than
        // leaving a dead item behind if the process exits without an
        // explicit Quit-menu path (e.g. Ctrl-C in the terminal during dev).
        NSStatusBar::systemStatusBar().removeStatusItem(&self.status_item);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn every_state_has_a_non_empty_label_and_symbol() {
        for state in [
            MenuBarState::Idle,
            MenuBarState::Listening,
            MenuBarState::Transcribing,
            MenuBarState::Error,
            MenuBarState::PermissionsMissing,
        ] {
            assert!(!state.label().is_empty(), "{state:?} has an empty label");
            assert!(!state.sf_symbol_name().is_empty(), "{state:?} has an empty symbol name");
        }
    }

    #[test]
    fn every_state_has_a_distinct_symbol_silhouette() {
        // Module docs point 1: a template image only differs by silhouette,
        // never color, so two states sharing a symbol would be
        // indistinguishable to the user regardless of appearance.
        let states = [
            MenuBarState::Idle,
            MenuBarState::Listening,
            MenuBarState::Transcribing,
            MenuBarState::Error,
            MenuBarState::PermissionsMissing,
        ];
        let mut symbols: Vec<&str> = states.iter().map(|s| s.sf_symbol_name()).collect();
        let before = symbols.len();
        symbols.sort_unstable();
        symbols.dedup();
        assert_eq!(symbols.len(), before, "two states share an SF Symbol name");
    }

    #[test]
    fn menu_item_tags_round_trip_to_their_events() {
        assert_eq!(event_for_tag(TAG_TOGGLE_ARMED), Some(MenuEvent::ToggleArmed));
        assert_eq!(event_for_tag(TAG_OPEN_SETTINGS), Some(MenuEvent::OpenSettings));
        assert_eq!(event_for_tag(TAG_QUIT), Some(MenuEvent::Quit));
        assert_eq!(event_for_tag(TAG_CHECK_UPDATES), Some(MenuEvent::CheckForUpdates));
    }

    #[test]
    fn an_unrecognized_tag_maps_to_nothing() {
        // Anything not wired by `action_item` (or a stray tag from a future
        // item someone forgets to add here) must be silently ignored, never
        // panic or fire the wrong event.
        assert_eq!(event_for_tag(0), None);
        assert_eq!(event_for_tag(999), None);
        assert_eq!(event_for_tag(-1), None);
    }

    #[test]
    fn the_four_tags_are_pairwise_distinct() {
        let tags = [TAG_TOGGLE_ARMED, TAG_OPEN_SETTINGS, TAG_QUIT, TAG_CHECK_UPDATES];
        for i in 0..tags.len() {
            for j in (i + 1)..tags.len() {
                assert_ne!(tags[i], tags[j], "tags[{i}] and tags[{j}] collide");
            }
        }
    }

    #[test]
    fn constructing_off_the_main_thread_errors_instead_of_panicking() {
        // The Rust test harness runs each #[test] body on a worker thread,
        // never the process's actual main thread -- so this exercises the
        // exact "no main thread" guard `MenuBar::new` takes (item 4 of the
        // dispatch) for real, without needing a window server at all. If
        // this ever started panicking instead of returning `Err`, that
        // would be the regression this test exists to catch.
        let result = MenuBar::new();
        assert!(result.is_err(), "MenuBar::new() must not succeed off the main thread");
    }
}
