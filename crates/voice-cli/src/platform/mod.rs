//! The platform boundary — everything `dictate` needs from an operating system,
//! expressed as capabilities rather than as APIs.
//!
//! macOS is the only implementation today. Windows and Linux are on the roadmap
//! (see `docs/voice/PORTING.md`), and the point of this module is that they can
//! land without touching `dictate.rs`: the loop above this line talks about
//! "hold events", "an indicator" and "cues", never about `CGEventTap`,
//! `NSPanel` or `NSSound`.
//!
//! Three decisions are baked in here because they are cheap now and expensive
//! later. Each comes from a real constraint on a platform we do not yet ship:
//!
//! 1. **Every capability is optional and declared.** `PlatformCaps` says what
//!    this OS can actually do. Windows cannot `SendInput` into an elevated
//!    process (UIPI); GNOME/Wayland has no `layer-shell`, so there may be no
//!    overlay at all. The product must degrade *visibly* rather than appear to
//!    work — a dictation tool that silently drops text is worse than one that
//!    says it cannot type here.
//! 2. **Hold-to-talk is not universally expressible.** macOS gets it from a
//!    `CGEventTap`; Windows from a `WH_KEYBOARD_LL` hook; but on Wayland the
//!    XDG `GlobalShortcuts` portal binds *chords*, and whether a lone modifier
//!    can be bound at all is compositor-dependent. So `HoldKeySource` reports
//!    whether true press-and-hold is available, and the loop falls back to
//!    tap-to-toggle where it is not. That fallback exists in the type system
//!    from day one so no platform has to bolt it on.
//! 3. **Insertion is "get text into the focused field", not "press ⌘V".**
//!    The strategies are genuinely different in kind — synthetic keystroke
//!    (macOS/Windows), portal-mediated injection via libei (Wayland), or
//!    committing directly as an input method (IBus, which is how CJK input
//!    works and needs no injection permission at all). Naming the operation
//!    after the keystroke would have quietly excluded the IME path, which is
//!    the *cleanest* option on Linux.

pub mod caps;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(not(target_os = "macos"))]
pub mod unsupported;

pub use caps::PlatformCaps;

/// What the running platform supports. The single place that answers the
/// question, so no call site has to reason about `cfg` itself.
pub fn current_caps() -> PlatformCaps {
    #[cfg(target_os = "macos")]
    {
        macos::CAPS
    }
    #[cfg(not(target_os = "macos"))]
    {
        unsupported::CAPS
    }
}

/// Which bare modifier arms dictation.
///
/// Defined here rather than in the macOS backend so the CLI surface is the same
/// shape on every platform — a flag that exists on one OS and vanishes on
/// another makes documentation and scripts platform-specific for no reason.
/// Keycodes are the backend's problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum HoldKey {
    /// Left Option/Alt — the default. The right-hand Option is the one more
    /// often used for typing special characters.
    LeftOption,
    RightOption,
    /// Either Option key.
    EitherOption,
    /// The `fn` / globe key. macOS may claim this for its own dictation.
    Fn,
    RightCommand,
    LeftControl,
    RightControl,
}

impl HoldKey {
    pub fn describe(self) -> &'static str {
        match self {
            HoldKey::LeftOption => "left Option (⌥)",
            HoldKey::RightOption => "right Option (⌥)",
            HoldKey::EitherOption => "either Option (⌥)",
            HoldKey::Fn => "fn / globe",
            HoldKey::RightCommand => "right Command (⌘)",
            HoldKey::LeftControl => "left Control (⌃)",
            HoldKey::RightControl => "right Control (⌃)",
        }
    }
}

/// What the input backend observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoldEvent {
    /// The hold key went down — start capturing.
    Down,
    /// The hold key came up — endpoint and transcribe.
    Up,
    /// Abandon the in-flight utterance without transcribing. The user was
    /// typing a special character or firing a shortcut, not dictating.
    Cancel(&'static str),
    /// The OS stopped delivering events. Recoverable, but never silently:
    /// on macOS the system disables a slow event tap, and the symptom is a
    /// hold key that simply stops working forever.
    SourceDisabled,
}

/// A source of hold-key events.
///
/// Implementations must be non-blocking: the caller polls this from the same
/// loop that drives the indicator, and a blocked poll freezes the UI and, on
/// macOS, starves the event tap into being disabled.
pub trait HoldKeySource {
    /// Drain everything observed since the last call. Never blocks.
    fn poll(&self) -> Vec<HoldEvent>;

    /// Re-arm after `SourceDisabled`.
    fn re_arm(&self) {}

    /// Whether this backend can distinguish key-down from key-up for a bare
    /// modifier. When false the caller must use tap-to-toggle: a source that
    /// only reports "activated" cannot express push-to-talk.
    fn supports_hold(&self) -> bool {
        true
    }
}

/// The listening indicator.
///
/// Optional by design — on GNOME/Wayland there may be no way to place an
/// always-on-top overlay, and dictation must still work without one.
pub trait Indicator {
    fn show_listening(&mut self);
    fn show_transcribing(&mut self);
    fn hide(&mut self);
    /// Advance one animation frame. `level` is capture RMS in 0.0..=1.0.
    fn tick(&mut self, level: f32);
}

/// The press/release audio cues.
///
/// The waveform synthesis itself is portable (`crate::sound::synth`); only
/// playback is platform-specific, so a port implements this trait and reuses
/// the generated PCM unchanged.
pub trait Cues {
    fn press(&self);
    fn release(&self);
}

/// A no-op indicator, used when the platform has no overlay or the user passed
/// `--no-hud`. Keeps `dictate` free of `Option<Box<dyn Indicator>>` branching
/// at every call site.
pub struct NullIndicator;

impl Indicator for NullIndicator {
    fn show_listening(&mut self) {}
    fn show_transcribing(&mut self) {}
    fn hide(&mut self) {}
    fn tick(&mut self, _level: f32) {}
}

/// Silent cues, for `--no-sound` or a platform without an audio-out path.
pub struct NullCues;

impl Cues for NullCues {
    fn press(&self) {}
    fn release(&self) {}
}

/// Everything the menu-bar / system-tray status item can be showing.
///
/// Deliberately its own type rather than a re-export of `crate::menubar
/// ::MenuBarState` — that type is a macOS/AppKit-specific detail (an
/// `NSStatusItem`'s icon and its "Status:" row), and a Windows tray or
/// Linux `StatusNotifierItem` port needs a state to render without
/// depending on any AppKit type. `platform::macos::MacStatusUi` maps
/// between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusUiState {
    /// Armed and waiting for the hold key.
    Idle,
    /// Hold key is down, capturing audio.
    Listening,
    /// Hold key released, ASR is running.
    Transcribing,
    /// The last utterance failed (capture, ASR, or insertion).
    Error,
    /// Microphone and/or Accessibility has not been granted.
    PermissionsMissing,
}

/// What the status UI observed. Pushed from whatever platform-specific
/// input mechanism the tray/menu uses, drained non-blockingly the same way
/// `HoldKeySource::poll` is — see `poll_events` below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusUiEvent {
    /// The user asked to pause/resume dictation without quitting.
    ToggleArmed,
    /// The user asked to open the Settings window.
    OpenSettings,
    /// The user asked to quit the app.
    Quit,
    /// The user clicked "Check for Updates…". What this means depends on
    /// the last known update state (up to date -> check now; available ->
    /// download; ready to relaunch -> install and relaunch) -- see
    /// `dictate::run_agent_macos`'s handling of this event, which is the
    /// only place that state is tracked.
    CheckForUpdates,
}

/// A menu-bar / system-tray status item.
///
/// Optional, same shape as [`Indicator`]/[`Cues`]: `PlatformCaps::
/// can_show_status_ui` says whether this platform has a host for one at
/// all, and [`NullStatusUi`] keeps the agent loop free of
/// `Option<Box<dyn StatusUi>>` branching when it does not. This struct
/// owns *display*, not behavior — flipping "Dictation Armed" here does not
/// itself pause capture; it only emits [`StatusUiEvent::ToggleArmed`] for
/// the caller (the agent loop, which owns the real armed/hold-key state)
/// to act on and report back via `set_armed`.
pub trait StatusUi {
    /// Update the visible state (icon + any "current status" row).
    fn set_state(&mut self, state: StatusUiState);
    /// Update the "hold key" row to whatever `HoldKey::describe()` returns.
    fn set_hold_key(&mut self, description: &str);
    /// Reflect the real armed/unarmed state (e.g. a checkmark).
    fn set_armed(&mut self, armed: bool);
    /// Update whatever row shows update status (e.g. "up to date",
    /// "update available: 0.2.0", "downloading update (1234/5678
    /// bytes)"). Default no-op so a platform with no update UI yet (or
    /// [`NullStatusUi`]) needs no explicit implementation -- only
    /// `platform::macos::MacStatusUi` overrides this today.
    fn set_update_text(&mut self, _text: &str) {}
    /// Drain everything observed since the last call. Never blocks.
    fn poll_events(&self) -> Vec<StatusUiEvent>;
}

/// No status UI at all — used when the platform has no tray/menu-bar host
/// (`can_show_status_ui: false`) or the status item failed to construct at
/// runtime. The agent loop still runs; it just has no chrome.
pub struct NullStatusUi;

impl StatusUi for NullStatusUi {
    fn set_state(&mut self, _state: StatusUiState) {}
    fn set_hold_key(&mut self, _description: &str) {}
    fn set_armed(&mut self, _armed: bool) {}
    fn poll_events(&self) -> Vec<StatusUiEvent> {
        Vec::new()
    }
}
