//! macOS backend: `CGEventTap` for the hold key, an AppKit panel for the
//! indicator, `NSSound` for the cues.
//!
//! Thin adapters only — the real implementations live in `crate::holdkey`,
//! `crate::hud` and `crate::sound`. Keeping the adapters separate is what lets
//! a Windows or Linux backend be added beside this file rather than inside it.

use super::{Cues, HoldEvent, HoldKey, HoldKeySource, Indicator, PlatformCaps, StatusUi, StatusUiEvent, StatusUiState};

pub const CAPS: PlatformCaps = PlatformCaps {
    can_hold_bare_modifier: true,
    can_inject_keystroke: true,
    // macOS has input methods, but we insert via clipboard + ⌘V today.
    can_commit_via_ime: false,
    can_overlay: true,
    // Both now real: `dictate.rs` wires a live `voice_context::
    // MacosContextProvider` (NSWorkspace + AXUIElement) into the bias/
    // app-kind path (`can_read_focused_app`) and into
    // `CliInsertionBackend::current_target()`'s secure-field refusal
    // (`can_detect_secure_field`). See `platform/caps.rs`'s
    // `todays_macos_reports_zero_gaps_now_that_ax_context_is_wired` test,
    // which pins this.
    can_read_focused_app: true,
    can_detect_secure_field: true,
    // NSStatusBar is always present on macOS -- see `MacStatusUi` below,
    // which wraps `crate::menubar::MenuBar`.
    can_show_status_ui: true,
};

pub struct MacHoldKey(crate::holdkey::HoldKeyTap);

impl MacHoldKey {
    pub fn install(key: HoldKey) -> anyhow::Result<Self> {
        Ok(Self(crate::holdkey::HoldKeyTap::install(key)?))
    }
}

impl HoldKeySource for MacHoldKey {
    fn poll(&self) -> Vec<HoldEvent> {
        self.0.poll()
    }
    fn re_arm(&self) {
        self.0.re_enable();
    }
    fn supports_hold(&self) -> bool {
        true
    }
}

pub struct MacIndicator(crate::hud::Hud);

impl MacIndicator {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self(crate::hud::Hud::new()?))
    }
}

impl Indicator for MacIndicator {
    fn show_listening(&mut self) {
        self.0.show_listening();
    }
    fn show_transcribing(&mut self) {
        self.0.show_transcribing();
    }
    fn hide(&mut self) {
        self.0.hide();
    }
    fn tick(&mut self, level: f32) {
        self.0.tick(level);
    }
}

pub struct MacCues(crate::sound::Tones);

impl MacCues {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self(crate::sound::Tones::new()?))
    }
}

impl Cues for MacCues {
    fn press(&self) {
        self.0.press();
    }
    fn release(&self) {
        self.0.release();
    }
}

/// Adapts `crate::menubar::MenuBar` (the real `NSStatusItem` -- see that
/// module for the objc2 wiring) to the platform-agnostic `StatusUi` trait,
/// exactly the same thin-wrapper shape as `MacIndicator`/`MacCues` above.
pub struct MacStatusUi(crate::menubar::MenuBar);

impl MacStatusUi {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self(crate::menubar::MenuBar::new()?))
    }

    /// Read back the current state/armed flag from the underlying
    /// `MenuBar`, rather than the caller having to track its own copy of
    /// what it last pushed via `set_state`/`set_armed`. `dictate.rs`'s
    /// agent loop currently keeps its own local `armed`/state as the
    /// source of truth and only ever pushes into `StatusUi` (never reads
    /// back), so these are not on the `StatusUi` trait itself and not yet
    /// called from there -- but they are real, working accessors over
    /// `MenuBar::state`/`MenuBar::armed` (which otherwise have no caller
    /// anywhere in the crate), not decorative.
    #[allow(dead_code)]
    pub fn state(&self) -> StatusUiState {
        map_state_back(self.0.state())
    }

    #[allow(dead_code)]
    pub fn armed(&self) -> bool {
        self.0.armed()
    }
}

impl StatusUi for MacStatusUi {
    fn set_state(&mut self, state: StatusUiState) {
        self.0.set_state(map_state(state));
    }
    fn set_hold_key(&mut self, description: &str) {
        self.0.set_hold_key(description);
    }
    fn set_armed(&mut self, armed: bool) {
        self.0.set_armed(armed);
    }
    fn set_update_text(&mut self, text: &str) {
        self.0.set_update_text(text);
    }
    fn poll_events(&self) -> Vec<StatusUiEvent> {
        self.0.poll_events().into_iter().map(map_event).collect()
    }
}

fn map_state(state: StatusUiState) -> crate::menubar::MenuBarState {
    match state {
        StatusUiState::Idle => crate::menubar::MenuBarState::Idle,
        StatusUiState::Listening => crate::menubar::MenuBarState::Listening,
        StatusUiState::Transcribing => crate::menubar::MenuBarState::Transcribing,
        StatusUiState::Error => crate::menubar::MenuBarState::Error,
        StatusUiState::PermissionsMissing => crate::menubar::MenuBarState::PermissionsMissing,
    }
}

fn map_state_back(state: crate::menubar::MenuBarState) -> StatusUiState {
    match state {
        crate::menubar::MenuBarState::Idle => StatusUiState::Idle,
        crate::menubar::MenuBarState::Listening => StatusUiState::Listening,
        crate::menubar::MenuBarState::Transcribing => StatusUiState::Transcribing,
        crate::menubar::MenuBarState::Error => StatusUiState::Error,
        crate::menubar::MenuBarState::PermissionsMissing => StatusUiState::PermissionsMissing,
    }
}

fn map_event(event: crate::menubar::MenuEvent) -> StatusUiEvent {
    match event {
        crate::menubar::MenuEvent::ToggleArmed => StatusUiEvent::ToggleArmed,
        crate::menubar::MenuEvent::OpenSettings => StatusUiEvent::OpenSettings,
        crate::menubar::MenuEvent::Quit => StatusUiEvent::Quit,
        crate::menubar::MenuEvent::CheckForUpdates => StatusUiEvent::CheckForUpdates,
    }
}
