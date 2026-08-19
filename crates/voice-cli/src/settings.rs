//! `textify-voice`'s persisted settings + the Settings window (SPEC WP-V0.4
//! / this unit's dispatch section B/C).
//!
//! # What's here
//!
//! - [`Settings`]: hold key, PTT/toggle mode, model tier, paste-vs-clipboard,
//!   HUD/sound toggles, and whether the menu-bar agent checks for updates
//!   in the background -- every field [`Settings::default`] gives a value,
//!   every field this crate's other subcommands already have their own
//!   `clap::ValueEnum` type for (see below), so this module never
//!   re-defines what "the hold key" or "the model" *means* -- only how a
//!   chosen value is persisted and edited.
//! - A small hand-rolled `key = value` text config (see "Why not serde"
//!   below) that round-trips, defaults every field a missing/corrupt file
//!   can't supply, and never drops a key it doesn't recognize (forward
//!   compatibility -- see [`merge_and_render`]).
//! - [`open_settings_window`] (macOS only): a real `NSWindow` with live
//!   controls for every field above, plus a permissions panel that
//!   re-checks on demand. See that function's doc comment for the
//!   activation-policy handling this unit's dispatch calls out by name.
//!
//! # Why not serde
//!
//! This unit's dispatch asks for "serde, defaults for every field,
//! forward-compatible parsing." `voice-cli`'s `Cargo.toml` -- which this
//! unit's dispatch does not permit editing (owned files are only
//! `settings.rs` and `onboarding.rs`) -- has no serialization crate as a
//! dependency, and `Cargo.lock` confirms `serde` does not resolve anywhere
//! in this workspace today (only `serde_core`/`serde_derive` show up,
//! pulled in transitively by something else, not usable as `serde` itself
//! without the crate being a direct dependency). Rather than silently
//! doing something structurally different from what was asked, or leaving
//! settings unimplemented, this follows the precedent `crate::dictionary`
//! already set in this exact crate for the identical constraint (see that
//! module's own doc comment): hand-roll a small, greppable text format and
//! keep the three properties serde would have given "for free" —
//! defaults, round-tripping, and forward compatibility — as explicit,
//! directly-tested code instead. If `dirs`/`serde` are ever added as
//! direct `voice-cli` dependencies, [`default_path`] and the
//! parse/render pair below are the only things that would need to change;
//! [`Settings`] and the window would not.
//!
//! # Reusing the CLI's own enums
//!
//! [`Settings::hold_key`], [`Settings::mode`], and [`Settings::model`] are
//! `crate::platform::HoldKey`, `crate::dictate::DictateMode`, and
//! `crate::common::ModelArg` themselves -- not parallel copies -- so a
//! persisted setting and a `--hold-key`/`--mode`/`--model` flag can never
//! drift apart, and the config file's on-disk spelling
//! (`hold_key = left-option`) is generated from `clap::ValueEnum`'s own
//! possible-value names, i.e. exactly what a user would type on the
//! command line for the same choice. See [`enum_to_config_value`] /
//! [`enum_from_config_value`].

// See the identical note in `crate::onboarding`: this module's `pub` API
// (settings persistence + `open_settings_window`) is consumed by a
// not-yet-built unit in this same wave (a future menu-bar app), so it is
// correctly `dead_code` by this binary crate's own call graph today --
// allowed at the module level rather than per-item so a genuinely unused
// item is not lost in the noise.
#![allow(dead_code)]

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use clap::ValueEnum;

use crate::common::ModelArg;
use crate::dictate::DictateMode;
use crate::platform::HoldKey;

/// Overrides [`default_path`] entirely when set to a non-empty value --
/// mirrors `crate::dictionary::DICTIONARY_PATH_ENV_VAR`'s convention.
pub const SETTINGS_PATH_ENV_VAR: &str = "TEXTIFY_VOICE_SETTINGS_PATH";

/// Every `HoldKey` variant, in a fixed display/persistence order. `HoldKey`
/// itself has no `Default`/enumeration of its own (see `crate::platform`),
/// so the Settings window and this module's tests both need a concrete
/// list -- defined once, here.
pub const HOLD_KEYS: [HoldKey; 7] = [
    HoldKey::LeftOption,
    HoldKey::RightOption,
    HoldKey::EitherOption,
    HoldKey::Fn,
    HoldKey::RightCommand,
    HoldKey::LeftControl,
    HoldKey::RightControl,
];

pub const MODES: [DictateMode; 2] = [DictateMode::Ptt, DictateMode::Toggle];

pub const MODELS: [ModelArg; 2] = [ModelArg::TinyEn, ModelArg::BaseEn];

// ---------------------------------------------------------------------
// Pure model
// ---------------------------------------------------------------------

/// Whether `dictate` should synthesize a ⌘V paste after writing the
/// transcript to the clipboard, or leave it clipboard-only. Mirrors
/// `dictate::DictateArgs`'s `--paste` / `--clipboard-only` pair as a
/// single persisted choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InsertionMode {
    /// Write to the clipboard, then synthesize the paste keystroke. The
    /// default for the app: "release and the text lands where you were typing"
    /// is the entire product promise, and clipboard-only quietly turns that
    /// into "release, then go press Cmd-V yourself".
    ///
    /// Safe to default to now that secure-field refusal is wired and fails
    /// closed, and the clipboard is still written first, so a paste that fails
    /// leaves the text recoverable.
    #[default]
    Paste,
    /// Copy only; you press Cmd-V. Still the default for the terminal
    /// `dictate` subcommand, where a command you just typed synthesizing
    /// keystrokes into another window is more surprising than helpful.
    ClipboardOnly,
}

impl InsertionMode {
    fn as_config_str(self) -> &'static str {
        match self {
            InsertionMode::Paste => "paste",
            InsertionMode::ClipboardOnly => "clipboard-only",
        }
    }

    fn from_config_str(s: &str) -> Option<Self> {
        match s {
            "paste" => Some(InsertionMode::Paste),
            "clipboard-only" => Some(InsertionMode::ClipboardOnly),
            _ => None,
        }
    }
}

/// Everything the Settings window edits. Every field has a default (see
/// [`Settings::default`]) -- there is no "unset" state for a user who has
/// never opened Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub hold_key: HoldKey,
    pub mode: DictateMode,
    pub model: ModelArg,
    pub insertion: InsertionMode,
    pub hud_enabled: bool,
    pub sound_enabled: bool,
    /// Whether the menu-bar agent checks `crate::update`'s appcast in the
    /// background (see `dictate::run_agent_macos`'s `spawn_update_checker`
    /// wiring). Defaults to `true`: unlike diagnostics upload (which can
    /// carry crash-report content -- see `crate::diagnostics`'s own
    /// off-by-default setting), an update check sends nothing about this
    /// user or machine -- it is one HTTPS GET of a static, public JSON
    /// file to learn the latest version number. Leaving it on by default
    /// is what makes an update mechanism actually reach a beta user who
    /// never opens Settings, which this unit's dispatch names as the top
    /// priority; a user who wants to opt out entirely still can here.
    /// (Not to be confused with diagnostics upload -- a separate,
    /// off-by-default setting the Settings window also edits, persisted
    /// by `crate::diagnostics` itself, not this struct.)
    pub update_check_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        // Matches `dictate::DictateArgs`'s own `default_value_t`s for hold
        // key, mode and model, and both cues on by default.
        //
        // Insertion deliberately DIVERGES from the CLI default: the app pastes,
        // the `dictate` subcommand does not. See `InsertionMode`.
        Settings {
            hold_key: HoldKey::LeftOption,
            mode: DictateMode::Ptt,
            model: ModelArg::BaseEn,
            insertion: InsertionMode::Paste,
            hud_enabled: true,
            sound_enabled: true,
            update_check_enabled: true,
        }
    }
}

/// Render `v`'s `clap::ValueEnum` possible-value name -- the exact string
/// a `--flag <value>` on this CLI would accept -- as an owned `String` (the
/// borrow inside `PossibleValue` does not outlive the temporary
/// `to_possible_value()` returns, so this cannot hand back a plain `&str`).
fn enum_to_config_value<T: ValueEnum>(v: T) -> String {
    v.to_possible_value().map(|pv| pv.get_name().to_string()).unwrap_or_default()
}

fn enum_from_config_value<T: ValueEnum>(s: &str) -> Option<T> {
    T::from_str(s, true).ok()
}

// ---------------------------------------------------------------------
// Text format: parse / render / merge
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsParseError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for SettingsParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// The result of loading (or defaulting) settings: the effective
/// [`Settings`] (every field either parsed from disk or defaulted),
/// whether a file was found at all, every key this build did not
/// recognize (forward compatibility -- see [`merge_and_render`]), and
/// every recognized key whose value this build could not parse (a corrupt
/// file degrades field-by-field, never wholesale).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadResult {
    pub settings: Settings,
    pub found: bool,
    pub unknown_keys: Vec<String>,
    pub errors: Vec<SettingsParseError>,
}

impl LoadResult {
    fn defaulted(found: bool) -> Self {
        LoadResult { settings: Settings::default(), found, unknown_keys: Vec::new(), errors: Vec::new() }
    }
}

const KNOWN_KEYS: [&str; 7] = [
    "hold_key",
    "mode",
    "model",
    "insertion",
    "hud_enabled",
    "sound_enabled",
    "update_check_enabled",
];

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Parse `content` on top of [`Settings::default`]: every recognized,
/// valid key overrides its field; every recognized key with an
/// unparseable value is reported in [`LoadResult::errors`] and leaves that
/// one field at its default; every unrecognized key is reported in
/// [`LoadResult::unknown_keys`] and otherwise ignored. Never fails, never
/// panics, never resets a field it *could* parse just because a sibling
/// field could not.
fn parse(content: &str) -> LoadResult {
    let mut settings = Settings::default();
    let mut unknown_keys = Vec::new();
    let mut errors = Vec::new();

    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            errors.push(SettingsParseError {
                line: line_no,
                message: format!("expected `key = value`, found {raw_line:?}"),
            });
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        match key {
            "hold_key" => match enum_from_config_value::<HoldKey>(value) {
                Some(v) => settings.hold_key = v,
                None => errors.push(bad_value(line_no, key, value)),
            },
            "mode" => match enum_from_config_value::<DictateMode>(value) {
                Some(v) => settings.mode = v,
                None => errors.push(bad_value(line_no, key, value)),
            },
            "model" => match enum_from_config_value::<ModelArg>(value) {
                Some(v) => settings.model = v,
                None => errors.push(bad_value(line_no, key, value)),
            },
            "insertion" => match InsertionMode::from_config_str(value) {
                Some(v) => settings.insertion = v,
                None => errors.push(bad_value(line_no, key, value)),
            },
            "hud_enabled" => match parse_bool(value) {
                Some(v) => settings.hud_enabled = v,
                None => errors.push(bad_value(line_no, key, value)),
            },
            "sound_enabled" => match parse_bool(value) {
                Some(v) => settings.sound_enabled = v,
                None => errors.push(bad_value(line_no, key, value)),
            },
            "update_check_enabled" => match parse_bool(value) {
                Some(v) => settings.update_check_enabled = v,
                None => errors.push(bad_value(line_no, key, value)),
            },
            other => unknown_keys.push(other.to_string()),
        }
    }

    LoadResult { settings, found: true, unknown_keys, errors }
}

fn bad_value(line: usize, key: &str, value: &str) -> SettingsParseError {
    SettingsParseError { line, message: format!("`{key}` = {value:?} is not a recognized value") }
}

fn config_line(key: &str, value: &str) -> String {
    format!("{key} = {value}\n")
}

fn settings_value(settings: &Settings, key: &str) -> String {
    match key {
        "hold_key" => enum_to_config_value(settings.hold_key),
        "mode" => enum_to_config_value(settings.mode),
        "model" => enum_to_config_value(settings.model),
        "insertion" => settings.insertion.as_config_str().to_string(),
        "hud_enabled" => settings.hud_enabled.to_string(),
        "sound_enabled" => settings.sound_enabled.to_string(),
        _ => settings.update_check_enabled.to_string(),
    }
}

/// Render `settings` as a fresh, canonical `key = value` file (all six
/// known keys). Used for a brand-new file; [`merge_and_render`] is used
/// for every subsequent save so an unrecognized line already on disk
/// survives.
fn render(settings: &Settings) -> String {
    let mut out = String::from(
        "# textify-voice settings -- edit directly or use the Settings window.\n\
         # Changes here take effect the next time `dictate` starts.\n\n",
    );
    for key in KNOWN_KEYS {
        out.push_str(&config_line(key, &settings_value(settings, key)));
    }
    out
}

/// Merge `settings` into `existing` raw file content, preserving every
/// line this build does not recognize (comments, blank lines, and any
/// `key = value` outside [`KNOWN_KEYS`]) **verbatim**, upserting every
/// known key in place, and appending any known key missing from
/// `existing`. This is the forward-compatibility guarantee this unit's
/// dispatch asks for: a newer build's extra key, re-saved by this build
/// after the user changes one field in the Settings window, is not wiped.
fn merge_and_render(existing: &str, settings: &Settings) -> String {
    let mut written: Vec<&str> = Vec::new();
    let mut out = String::new();

    for raw_line in existing.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out.push_str(raw_line);
            out.push('\n');
            continue;
        }
        let Some((key, _)) = trimmed.split_once('=') else {
            out.push_str(raw_line);
            out.push('\n');
            continue;
        };
        let key = key.trim();
        if let Some(known) = KNOWN_KEYS.iter().find(|k| **k == key) {
            out.push_str(&config_line(known, &settings_value(settings, known)));
            written.push(known);
        } else {
            // Unrecognized key -- preserve verbatim (forward compatibility).
            out.push_str(raw_line);
            out.push('\n');
        }
    }

    for key in KNOWN_KEYS {
        if !written.contains(&key) {
            out.push_str(&config_line(key, &settings_value(settings, key)));
        }
    }

    out
}

// ---------------------------------------------------------------------
// Path resolution + persisted state (thin I/O shell)
// ---------------------------------------------------------------------

#[derive(Debug)]
pub enum SettingsError {
    NoConfigDir,
    Io(io::Error),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SettingsError::NoConfigDir => write!(
                f,
                "could not resolve a platform data directory for settings; \
                 set {SETTINGS_PATH_ENV_VAR} or pass an explicit path"
            ),
            SettingsError::Io(e) => write!(f, "settings I/O error: {e}"),
        }
    }
}

impl std::error::Error for SettingsError {}

impl From<io::Error> for SettingsError {
    fn from(e: io::Error) -> Self {
        SettingsError::Io(e)
    }
}

/// Resolve the default settings path: [`SETTINGS_PATH_ENV_VAR`] if set to
/// a non-empty value, else the platform data directory -- concretely
/// `~/Library/Application Support/textify/settings.txt` on macOS, right
/// next to `crate::dictionary`'s `dictionary.txt` per this unit's dispatch
/// ("next to the dictionary, same convention"). Reimplements the same
/// per-OS resolution `dictionary::default_path` does -- see this module's
/// top doc comment on why there is no shared helper to call instead.
pub fn default_path() -> Result<PathBuf, SettingsError> {
    if let Ok(p) = std::env::var(SETTINGS_PATH_ENV_VAR) {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    Ok(platform_data_dir()?.join("textify").join("settings.txt"))
}

#[cfg(target_os = "macos")]
fn platform_data_dir() -> Result<PathBuf, SettingsError> {
    let home = std::env::var_os("HOME").ok_or(SettingsError::NoConfigDir)?;
    Ok(PathBuf::from(home).join("Library").join("Application Support"))
}

#[cfg(target_os = "windows")]
fn platform_data_dir() -> Result<PathBuf, SettingsError> {
    let appdata = std::env::var_os("APPDATA").ok_or(SettingsError::NoConfigDir)?;
    Ok(PathBuf::from(appdata))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_data_dir() -> Result<PathBuf, SettingsError> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg));
        }
    }
    let home = std::env::var_os("HOME").ok_or(SettingsError::NoConfigDir)?;
    Ok(PathBuf::from(home).join(".local").join("share"))
}

/// Load from [`default_path`]. A missing file is the normal first-run
/// state, not an error -- returns [`Settings::default`] with
/// [`LoadResult::found`] `false`.
pub fn load() -> Result<LoadResult, SettingsError> {
    load_from(&default_path()?)
}

pub fn load_from(path: &Path) -> Result<LoadResult, SettingsError> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(parse(&content)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(LoadResult::defaulted(false)),
        Err(e) => Err(e.into()),
    }
}

/// Persist `settings` to [`default_path`], preserving any unrecognized
/// line already on disk (see [`merge_and_render`]).
pub fn save(settings: &Settings) -> Result<(), SettingsError> {
    save_to(&default_path()?, settings)
}

pub fn save_to(path: &Path, settings: &Settings) -> Result<(), SettingsError> {
    let existing = fs::read_to_string(path).unwrap_or_default();
    let rendered =
        if existing.trim().is_empty() { render(settings) } else { merge_and_render(&existing, settings) };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, rendered)?;
    Ok(())
}

// ---------------------------------------------------------------------
// The Settings window.
// ---------------------------------------------------------------------

/// Open the Settings window and block until it is closed (either via the
/// "Done" button or the standard red close button -- both paths save
/// nothing extra on close because every control already saves on change,
/// see [`open`](appkit::open)'s module doc comment).
///
/// # The activation-policy dance (this unit's dispatch calls this out
/// explicitly)
///
/// `dictate`'s live loop runs the app as `NSApplicationActivationPolicy::Accessory`
/// permanently (see `crate::hud::Hud::new`): no Dock icon, no menu bar, and
/// critically the app is never the foreground app, because dictation's
/// whole insertion path depends on focus staying on whatever the user was
/// typing into. Settings is the deliberate, sole exception: it is a real
/// window with real text fields and popup buttons, and AppKit will not
/// reliably bring an unbundled Accessory-policy process's window to the
/// front over other running apps without an explicit activation. So this
/// function:
///
/// 1. Calls `NSApplication::activate()` right before showing the window --
///    the modern (non-deprecated) replacement for
///    `activateIgnoringOtherApps(true)`, bringing this process, and the
///    Settings window with it, frontmost and key.
/// 2. Leaves the activation *policy* itself at `Accessory` throughout --
///    activating is a focus/frontmost concept, not a Dock-icon concept;
///    flipping the policy to `Regular` and back is unnecessary and would
///    briefly flash a Dock icon for no reason.
/// 3. Calls `NSApplication::deactivate()` (the direct, non-deprecated
///    counterpart to `activate()`) once the window closes -- by either
///    path (see `windowWillClose:` on `SettingsController`, and the
///    "Done" button, both funnel into the same `deactivate_and_stop`).
///
/// Get step 3 wrong and the *next* dictation's paste silently lands in the
/// Settings window (or wherever focus was left) instead of the user's
/// actual target app -- exactly the failure mode `crate::hud`'s own module
/// docs warn about for the HUD panel, for the identical reason.
///
/// **NOT VERIFIED**: this environment has no window server session this
/// task can observe, so no part of this window's actual on-screen
/// behavior (layout, whether `activate()` really steals focus, whether
/// `windowWillClose:` really fires for the red-button path) has been
/// watched happen. Every AppKit call used here is real (not a stub) and,
/// where of a kind this wave's AppKit probe exercised
/// (`define_class!`-based target/action dispatch, `NSWindow`/`NSPanel`
/// construction, `NSApplication` activation-policy calls), was proven to
/// actually dispatch by that probe's executed `sendAction:to:from:` test
/// -- see this task's context for that run's output. What is new here
/// (`NSPopUpButton`, `NSWindowDelegate`, `runModalForWindow:`) compiles
/// and type-checks against the real `objc2-app-kit` bindings but has not
/// itself been executed.
#[cfg(target_os = "macos")]
pub fn open_settings_window() -> anyhow::Result<()> {
    let mtm = objc2::MainThreadMarker::new()
        .ok_or_else(|| anyhow::anyhow!("the settings window must be opened from the main thread"))?;
    appkit::open(mtm)
}

#[cfg(not(target_os = "macos"))]
pub fn open_settings_window() -> anyhow::Result<()> {
    anyhow::bail!("the settings window is only implemented for macOS in this build")
}

#[cfg(target_os = "macos")]
mod appkit {
    use super::{save, InsertionMode, LoadResult, Settings, HOLD_KEYS, MODELS, MODES};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, ProtocolObject};
    use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSBackingStoreType, NSButton, NSControlStateValueOff,
        NSControlStateValueOn, NSPopUpButton, NSTextField, NSWindow, NSWindowDelegate,
        NSWindowStyleMask, NSWorkspace,
    };
    use objc2_foundation::{ns_string, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

    const ROW_H: f64 = 30.0;
    const WIN_W: f64 = 480.0;
    const WIN_H: f64 = 490.0;
    const LABEL_X: f64 = 20.0;
    const CONTROL_X: f64 = 170.0;
    const CONTROL_W: f64 = 290.0;

    struct ControllerIvars {
        hold_key_popup: Retained<NSPopUpButton>,
        mode_popup: Retained<NSPopUpButton>,
        model_popup: Retained<NSPopUpButton>,
        paste_checkbox: Retained<NSButton>,
        hud_checkbox: Retained<NSButton>,
        sound_checkbox: Retained<NSButton>,
        update_check_checkbox: Retained<NSButton>,
        // Not part of `Settings` -- persisted separately by
        // `crate::diagnostics::save_setting` (see `control_changed` and
        // `crate::diagnostics::DiagnosticsSetting`'s own doc comment for
        // why upload stays that module's single source of truth rather
        // than a duplicated field here).
        diagnostics_upload_checkbox: Retained<NSButton>,
        mic_status_label: Retained<NSTextField>,
        ax_status_label: Retained<NSTextField>,
        dictionary_path_owned: std::path::PathBuf,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements; SettingsController
        // has no Drop impl, and every ivars field is itself a `Retained<T>` or
        // plain owned data -- no interior aliasing hazards.
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = ControllerIvars]
        struct SettingsController;

        unsafe impl NSObjectProtocol for SettingsController {}

        unsafe impl NSWindowDelegate for SettingsController {
            #[unsafe(method(windowWillClose:))]
            fn window_will_close(&self, _notification: &NSNotification) {
                self.stop_modal();
            }
        }

        impl SettingsController {
            #[unsafe(method(controlChanged:))]
            fn control_changed(&self, _sender: Option<&AnyObject>) {
                let settings = self.read_settings();
                let _ = save(&settings);
                // Diagnostics upload is not a `Settings` field (see
                // `ControllerIvars::diagnostics_upload_checkbox`'s doc
                // comment) -- persisted directly via `crate::diagnostics`
                // on every control change, same trigger as every other
                // checkbox here. Writing the same value it already had is
                // harmless; this only actually changes anything when the
                // diagnostics checkbox itself was the one clicked.
                let upload_enabled =
                    self.ivars().diagnostics_upload_checkbox.state() == NSControlStateValueOn;
                let _ = crate::diagnostics::save_setting(crate::diagnostics::DiagnosticsSetting {
                    upload_enabled,
                });
            }

            #[unsafe(method(revealDictionary:))]
            fn reveal_dictionary(&self, _sender: Option<&AnyObject>) {
                let path_str = self.ivars().dictionary_path_owned.to_string_lossy().to_string();
                let ns_path = NSString::from_str(&path_str);
                let workspace = NSWorkspace::sharedWorkspace();
                workspace.selectFile_inFileViewerRootedAtPath(Some(&ns_path), ns_string!(""));
            }

            #[unsafe(method(recheckPermissions:))]
            fn recheck_permissions(&self, _sender: Option<&AnyObject>) {
                self.refresh_permission_labels();
            }

            #[unsafe(method(openMicSettings:))]
            fn open_mic_settings(&self, _sender: Option<&AnyObject>) {
                if let Some(url) = crate::onboarding::OnboardingStep::Microphone.deep_link_url() {
                    let _ = crate::onboarding::open_deep_link(url);
                }
            }

            #[unsafe(method(openAccessibilitySettings:))]
            fn open_accessibility_settings(&self, _sender: Option<&AnyObject>) {
                if let Some(url) = crate::onboarding::OnboardingStep::Accessibility.deep_link_url() {
                    let _ = crate::onboarding::open_deep_link(url);
                }
            }

            #[unsafe(method(doneClicked:))]
            fn done_clicked(&self, _sender: Option<&AnyObject>) {
                self.stop_modal();
            }
        }
    );

    impl SettingsController {
        fn new(mtm: MainThreadMarker, ivars: ControllerIvars) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(ivars);
            // SAFETY: NSObject's `init` has the correct signature.
            unsafe { msg_send![super(this), init] }
        }

        /// Read the four editable controls back into a [`Settings`] value.
        /// Called on every control's action -- see `control_changed`.
        fn read_settings(&self) -> Settings {
            let ivars = self.ivars();
            let hold_key = HOLD_KEYS
                .get(usize_from_index(ivars.hold_key_popup.indexOfSelectedItem()))
                .copied()
                .unwrap_or_default_hold_key();
            let mode = MODES
                .get(usize_from_index(ivars.mode_popup.indexOfSelectedItem()))
                .copied()
                .unwrap_or(crate::dictate::DictateMode::Ptt);
            let model = MODELS
                .get(usize_from_index(ivars.model_popup.indexOfSelectedItem()))
                .copied()
                .unwrap_or_default_model();
            let insertion = if ivars.paste_checkbox.state() == NSControlStateValueOn {
                InsertionMode::Paste
            } else {
                InsertionMode::ClipboardOnly
            };
            Settings {
                hold_key,
                mode,
                model,
                insertion,
                hud_enabled: ivars.hud_checkbox.state() == NSControlStateValueOn,
                sound_enabled: ivars.sound_checkbox.state() == NSControlStateValueOn,
                update_check_enabled: ivars.update_check_checkbox.state() == NSControlStateValueOn,
            }
        }

        fn refresh_permission_labels(&self) {
            let report = crate::permissions::check();
            let mic_ok = report.mic == voice_audio::MicPermission::Authorized;
            let mic_text =
                format!("Microphone: {}", if mic_ok { "granted" } else { "not granted" });
            let ax_text = format!(
                "Accessibility: {}",
                if report.accessibility_trusted { "granted" } else { "not granted" }
            );
            self.ivars().mic_status_label.setStringValue(&NSString::from_str(&mic_text));
            self.ivars().ax_status_label.setStringValue(&NSString::from_str(&ax_text));
        }

        fn stop_modal(&self) {
            let app = NSApplication::sharedApplication(self.mtm());
            app.stopModal();
        }
    }

    /// `NSPopUpButton::indexOfSelectedItem()` returns `NSInteger` (`-1` if
    /// nothing is selected, which should not happen here since every
    /// popup is always populated and given an initial selection, but a
    /// negative index must never be handed to `[T]::get` regardless).
    fn usize_from_index(index: isize) -> usize {
        usize::try_from(index).unwrap_or(0)
    }

    trait DefaultHoldKey {
        fn unwrap_or_default_hold_key(self) -> super::HoldKey;
    }
    impl DefaultHoldKey for Option<super::HoldKey> {
        fn unwrap_or_default_hold_key(self) -> super::HoldKey {
            self.unwrap_or(super::HoldKey::LeftOption)
        }
    }

    trait DefaultModel {
        fn unwrap_or_default_model(self) -> super::ModelArg;
    }
    impl DefaultModel for Option<super::ModelArg> {
        fn unwrap_or_default_model(self) -> super::ModelArg {
            self.unwrap_or(super::ModelArg::BaseEn)
        }
    }

    fn label(mtm: MainThreadMarker, text: &str, frame: NSRect) -> Retained<NSTextField> {
        let field = NSTextField::labelWithString(&NSString::from_str(text), mtm);
        field.setFrame(frame);
        field
    }

    pub fn open(mtm: MainThreadMarker) -> anyhow::Result<()> {
        let loaded: LoadResult = super::load().unwrap_or_else(|_| LoadResult::defaulted(false));
        let settings = loaded.settings;
        // Diagnostics upload is `crate::diagnostics`'s own setting, not a
        // `Settings` field -- see `ControllerIvars::diagnostics_upload_
        // checkbox`'s doc comment.
        let diagnostics_upload_enabled = crate::diagnostics::is_upload_enabled();
        let dictionary_path =
            crate::dictionary::default_path().unwrap_or_else(|_| std::path::PathBuf::from("dictionary.txt"));

        let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIN_W, WIN_H));
        let style =
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
        // SAFETY: standard, documented NSWindow designated initializer.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                content_rect,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: this window is not owned by a window controller, so
        // AppKit must not auto-release it when it closes.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(ns_string!("Textify Voice Settings"));
        window.center();

        let content_view = window
            .contentView()
            .ok_or_else(|| anyhow::anyhow!("the settings window has no content view"))?;

        let mut y = WIN_H - 40.0;

        content_view.addSubview(&label(mtm, "Hold key:", NSRect::new(NSPoint::new(LABEL_X, y), NSSize::new(140.0, 20.0))));
        let hold_key_popup = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            NSRect::new(NSPoint::new(CONTROL_X, y - 4.0), NSSize::new(CONTROL_W, 26.0)),
            false,
        );
        for key in HOLD_KEYS {
            hold_key_popup.addItemWithTitle(&NSString::from_str(key.describe()));
        }
        if let Some(idx) = HOLD_KEYS.iter().position(|k| *k == settings.hold_key) {
            hold_key_popup.selectItemAtIndex(idx as isize);
        }
        content_view.addSubview(&hold_key_popup);
        y -= ROW_H;

        content_view.addSubview(&label(mtm, "Mode:", NSRect::new(NSPoint::new(LABEL_X, y), NSSize::new(140.0, 20.0))));
        let mode_popup = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            NSRect::new(NSPoint::new(CONTROL_X, y - 4.0), NSSize::new(CONTROL_W, 26.0)),
            false,
        );
        for mode in MODES {
            let title = match mode {
                crate::dictate::DictateMode::Ptt => "Push to talk",
                crate::dictate::DictateMode::Toggle => "Toggle",
            };
            mode_popup.addItemWithTitle(&NSString::from_str(title));
        }
        if let Some(idx) = MODES.iter().position(|m| *m == settings.mode) {
            mode_popup.selectItemAtIndex(idx as isize);
        }
        content_view.addSubview(&mode_popup);
        y -= ROW_H;

        content_view.addSubview(&label(mtm, "Model:", NSRect::new(NSPoint::new(LABEL_X, y), NSSize::new(140.0, 20.0))));
        let model_popup = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            NSRect::new(NSPoint::new(CONTROL_X, y - 4.0), NSSize::new(CONTROL_W, 26.0)),
            false,
        );
        for m in MODELS {
            let title = match m {
                crate::common::ModelArg::TinyEn => "tiny.en (fastest)",
                crate::common::ModelArg::BaseEn => "base.en (default)",
            };
            model_popup.addItemWithTitle(&NSString::from_str(title));
        }
        if let Some(idx) = MODELS.iter().position(|m| *m == settings.model) {
            model_popup.selectItemAtIndex(idx as isize);
        }
        content_view.addSubview(&model_popup);
        y -= ROW_H + 6.0;

        // SAFETY: standard NSButton convenience constructor.
        let paste_checkbox = unsafe {
            NSButton::checkboxWithTitle_target_action(
                ns_string!("Automatically paste after transcription"),
                None,
                None,
                mtm,
            )
        };
        paste_checkbox.setFrame(NSRect::new(NSPoint::new(LABEL_X, y), NSSize::new(420.0, 22.0)));
        paste_checkbox
            .setState(if settings.insertion == InsertionMode::Paste { NSControlStateValueOn } else { NSControlStateValueOff });
        content_view.addSubview(&paste_checkbox);
        y -= ROW_H;

        let hud_checkbox = unsafe {
            NSButton::checkboxWithTitle_target_action(ns_string!("Show the listening waveform (HUD)"), None, None, mtm)
        };
        hud_checkbox.setFrame(NSRect::new(NSPoint::new(LABEL_X, y), NSSize::new(420.0, 22.0)));
        hud_checkbox.setState(if settings.hud_enabled { NSControlStateValueOn } else { NSControlStateValueOff });
        content_view.addSubview(&hud_checkbox);
        y -= ROW_H;

        let sound_checkbox = unsafe {
            NSButton::checkboxWithTitle_target_action(ns_string!("Play press/release sounds"), None, None, mtm)
        };
        sound_checkbox.setFrame(NSRect::new(NSPoint::new(LABEL_X, y), NSSize::new(420.0, 22.0)));
        sound_checkbox.setState(if settings.sound_enabled { NSControlStateValueOn } else { NSControlStateValueOff });
        content_view.addSubview(&sound_checkbox);
        y -= ROW_H;

        let update_check_checkbox = unsafe {
            NSButton::checkboxWithTitle_target_action(
                ns_string!("Automatically check for updates"),
                None,
                None,
                mtm,
            )
        };
        update_check_checkbox.setFrame(NSRect::new(NSPoint::new(LABEL_X, y), NSSize::new(420.0, 22.0)));
        update_check_checkbox.setState(
            if settings.update_check_enabled { NSControlStateValueOn } else { NSControlStateValueOff },
        );
        content_view.addSubview(&update_check_checkbox);
        y -= ROW_H;

        // Off by default (see `crate::diagnostics`'s own doc comment) --
        // and, today, a no-op even when turned on: no third-party SDK or
        // network client is wired up anywhere in this build
        // (`crate::diagnostics::UnconfiguredTransmitter` always errors),
        // so this checkbox only records the user's preference for
        // whenever that changes. The label says exactly that rather than
        // implying reports are already going anywhere.
        let diagnostics_upload_checkbox = unsafe {
            NSButton::checkboxWithTitle_target_action(
                ns_string!("Automatically send crash reports (not yet connected to a server)"),
                None,
                None,
                mtm,
            )
        };
        diagnostics_upload_checkbox
            .setFrame(NSRect::new(NSPoint::new(LABEL_X, y), NSSize::new(420.0, 22.0)));
        diagnostics_upload_checkbox.setState(
            if diagnostics_upload_enabled { NSControlStateValueOn } else { NSControlStateValueOff },
        );
        content_view.addSubview(&diagnostics_upload_checkbox);
        y -= ROW_H + 10.0;

        content_view.addSubview(&label(
            mtm,
            "Dictionary:",
            NSRect::new(NSPoint::new(LABEL_X, y), NSSize::new(90.0, 20.0)),
        ));
        content_view.addSubview(&label(
            mtm,
            &dictionary_path.display().to_string(),
            NSRect::new(NSPoint::new(LABEL_X + 90.0, y), NSSize::new(260.0, 20.0)),
        ));
        let reveal_button = unsafe {
            NSButton::buttonWithTitle_target_action(ns_string!("Reveal in Finder"), None, None, mtm)
        };
        reveal_button.setFrame(NSRect::new(NSPoint::new(LABEL_X + 360.0, y - 4.0), NSSize::new(100.0, 24.0)));
        content_view.addSubview(&reveal_button);
        y -= ROW_H + 10.0;

        let mic_status_label = label(mtm, "Microphone: checking...", NSRect::new(NSPoint::new(LABEL_X, y), NSSize::new(220.0, 20.0)));
        content_view.addSubview(&mic_status_label);
        let open_mic_button = unsafe {
            NSButton::buttonWithTitle_target_action(ns_string!("Open Settings"), None, None, mtm)
        };
        open_mic_button.setFrame(NSRect::new(NSPoint::new(LABEL_X + 240.0, y - 4.0), NSSize::new(120.0, 24.0)));
        content_view.addSubview(&open_mic_button);
        y -= ROW_H;

        let ax_status_label = label(mtm, "Accessibility: checking...", NSRect::new(NSPoint::new(LABEL_X, y), NSSize::new(220.0, 20.0)));
        content_view.addSubview(&ax_status_label);
        let open_ax_button = unsafe {
            NSButton::buttonWithTitle_target_action(ns_string!("Open Settings"), None, None, mtm)
        };
        open_ax_button.setFrame(NSRect::new(NSPoint::new(LABEL_X + 240.0, y - 4.0), NSSize::new(120.0, 24.0)));
        content_view.addSubview(&open_ax_button);
        y -= ROW_H;

        let recheck_button = unsafe {
            NSButton::buttonWithTitle_target_action(ns_string!("Recheck Permissions"), None, None, mtm)
        };
        recheck_button.setFrame(NSRect::new(NSPoint::new(LABEL_X, y - 4.0), NSSize::new(160.0, 24.0)));
        content_view.addSubview(&recheck_button);

        let done_button = unsafe { NSButton::buttonWithTitle_target_action(ns_string!("Done"), None, None, mtm) };
        done_button.setFrame(NSRect::new(NSPoint::new(WIN_W - 100.0, 16.0), NSSize::new(80.0, 28.0)));
        content_view.addSubview(&done_button);

        let controller = SettingsController::new(
            mtm,
            ControllerIvars {
                hold_key_popup: hold_key_popup.clone(),
                mode_popup: mode_popup.clone(),
                model_popup: model_popup.clone(),
                paste_checkbox: paste_checkbox.clone(),
                hud_checkbox: hud_checkbox.clone(),
                sound_checkbox: sound_checkbox.clone(),
                update_check_checkbox: update_check_checkbox.clone(),
                diagnostics_upload_checkbox: diagnostics_upload_checkbox.clone(),
                mic_status_label: mic_status_label.clone(),
                ax_status_label: ax_status_label.clone(),
                dictionary_path_owned: dictionary_path,
            },
        );
        let target: &AnyObject = &controller;

        // SAFETY: `target`/`action` are the standard AppKit target/action
        // pair; the target outlives the controls (both are rooted in this
        // function's locals until `runModalForWindow` returns).
        unsafe {
            hold_key_popup.setTarget(Some(target));
            hold_key_popup.setAction(Some(sel!(controlChanged:)));
            mode_popup.setTarget(Some(target));
            mode_popup.setAction(Some(sel!(controlChanged:)));
            model_popup.setTarget(Some(target));
            model_popup.setAction(Some(sel!(controlChanged:)));
            paste_checkbox.setTarget(Some(target));
            paste_checkbox.setAction(Some(sel!(controlChanged:)));
            hud_checkbox.setTarget(Some(target));
            hud_checkbox.setAction(Some(sel!(controlChanged:)));
            sound_checkbox.setTarget(Some(target));
            sound_checkbox.setAction(Some(sel!(controlChanged:)));
            update_check_checkbox.setTarget(Some(target));
            update_check_checkbox.setAction(Some(sel!(controlChanged:)));
            diagnostics_upload_checkbox.setTarget(Some(target));
            diagnostics_upload_checkbox.setAction(Some(sel!(controlChanged:)));
            reveal_button.setTarget(Some(target));
            reveal_button.setAction(Some(sel!(revealDictionary:)));
            recheck_button.setTarget(Some(target));
            recheck_button.setAction(Some(sel!(recheckPermissions:)));
            open_mic_button.setTarget(Some(target));
            open_mic_button.setAction(Some(sel!(openMicSettings:)));
            open_ax_button.setTarget(Some(target));
            open_ax_button.setAction(Some(sel!(openAccessibilitySettings:)));
            done_button.setTarget(Some(target));
            done_button.setAction(Some(sel!(doneClicked:)));
        }

        window.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
        controller.refresh_permission_labels();

        let app = NSApplication::sharedApplication(mtm);
        app.activate();
        window.makeKeyAndOrderFront(None);
        // Blocks until `stop_modal` (Done button, or the red close button
        // via `windowWillClose:`) is called.
        app.runModalForWindow(&window);
        window.orderOut(None);
        app.deactivate();

        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // -- defaults --

    #[test]
    fn default_settings_match_dictate_args_own_defaults() {
        let s = Settings::default();
        assert_eq!(s.hold_key, HoldKey::LeftOption);
        assert_eq!(s.mode, DictateMode::Ptt);
        assert_eq!(s.model, ModelArg::BaseEn);
        assert!(s.hud_enabled);
        assert!(s.sound_enabled);
    }

    #[test]
    fn update_checking_defaults_on() {
        // See `Settings::update_check_enabled`'s own doc comment: unlike
        // diagnostics upload, a check sends no personal data, so leaving
        // it on by default is what actually gets a fix to a beta user who
        // never opens Settings -- this unit's dispatch's stated top
        // priority.
        assert!(Settings::default().update_check_enabled);
    }

    #[test]
    fn the_app_pastes_by_default_unlike_the_dictate_subcommand() {
        // Deliberate divergence from `dictate`'s clipboard-only default, and
        // the reason is the whole product promise: the app exists so that
        // releasing the key puts text where you were typing. Defaulting to
        // clipboard-only silently turned that into "now go press Cmd-V", which
        // reads as the app not working at all.
        //
        // If this ever flips back, dictation stops appearing to do anything.
        assert_eq!(Settings::default().insertion, InsertionMode::Paste);
    }

    #[test]
    fn loading_missing_content_area_defaults_and_reports_not_found_via_load_from() {
        let dir = std::env::temp_dir().join(format!("textify-settings-test-{}-missing", std::process::id()));
        let path = dir.join("settings.txt");
        let result = load_from(&path).unwrap();
        assert_eq!(result.settings, Settings::default());
        assert!(!result.found);
        assert!(result.unknown_keys.is_empty());
        assert!(result.errors.is_empty());
    }

    // -- enum <-> config value mapping --

    #[test]
    fn every_hold_key_round_trips_through_config_value() {
        for key in HOLD_KEYS {
            let s = enum_to_config_value(key);
            assert_eq!(enum_from_config_value::<HoldKey>(&s), Some(key), "hold key {key:?} -> {s:?} did not round-trip");
        }
    }

    #[test]
    fn every_mode_round_trips_through_config_value() {
        for mode in MODES {
            let s = enum_to_config_value(mode);
            assert_eq!(enum_from_config_value::<DictateMode>(&s), Some(mode));
        }
    }

    #[test]
    fn every_model_round_trips_through_config_value() {
        for model in MODELS {
            let s = enum_to_config_value(model);
            assert_eq!(enum_from_config_value::<ModelArg>(&s), Some(model));
        }
    }

    #[test]
    fn hold_key_config_values_match_the_cli_flag_spelling() {
        // Not incidental: `crate::dictate::DictateArgs::hold_key` is parsed
        // by the same `clap::ValueEnum`, so the config file's on-disk
        // spelling for this field is required to be exactly what a user
        // would type after `--hold-key` on the command line.
        assert_eq!(enum_to_config_value(HoldKey::LeftOption), "left-option");
        assert_eq!(enum_to_config_value(HoldKey::Fn), "fn");
        assert_eq!(enum_to_config_value(ModelArg::TinyEn), "tiny.en");
        assert_eq!(enum_to_config_value(ModelArg::BaseEn), "base.en");
    }

    // -- parse / render round trip --

    #[test]
    fn settings_round_trip_through_render_and_parse() {
        let s = Settings {
            hold_key: HoldKey::RightControl,
            mode: DictateMode::Toggle,
            model: ModelArg::TinyEn,
            insertion: InsertionMode::Paste,
            hud_enabled: false,
            sound_enabled: false,
            update_check_enabled: false,
        };
        let text = render(&s);
        let result = parse(&text);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(result.unknown_keys.is_empty());
        assert_eq!(result.settings, s);
    }

    #[test]
    fn default_settings_also_round_trip() {
        let s = Settings::default();
        let result = parse(&render(&s));
        assert_eq!(result.settings, s);
    }

    // -- corrupt file --

    #[test]
    fn a_corrupt_file_does_not_crash_and_recovers_field_by_field() {
        let content = "\
hold_key = right-command
mode = not-a-real-mode
this line has no equals sign at all
model = tiny.en
hud_enabled = 47
sound_enabled = false
";
        let result = parse(content);
        assert_eq!(result.errors.len(), 3, "{:?}", result.errors); // mode, the malformed line, hud_enabled
        // Everything parseable still landed:
        assert_eq!(result.settings.hold_key, HoldKey::RightCommand);
        assert_eq!(result.settings.model, ModelArg::TinyEn);
        assert!(!result.settings.sound_enabled);
        // The unparseable fields fell back to defaults rather than
        // poisoning the whole load:
        assert_eq!(result.settings.mode, DictateMode::Ptt);
        assert!(result.settings.hud_enabled);
    }

    #[test]
    fn an_unparseable_update_check_enabled_falls_back_to_the_default_and_is_reported() {
        let result = parse("update_check_enabled = maybe\n");
        assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
        assert!(result.settings.update_check_enabled, "should fall back to the true default");
    }

    #[test]
    fn an_empty_file_is_settings_default_with_no_diagnostics() {
        let result = parse("");
        assert_eq!(result.settings, Settings::default());
        assert!(result.errors.is_empty());
        assert!(result.unknown_keys.is_empty());
    }

    // -- forward compatibility --

    #[test]
    fn unknown_keys_are_reported_and_do_not_error() {
        let content = "hold_key = fn\nfuture_field = something-this-build-has-never-heard-of\n";
        let result = parse(content);
        assert!(result.errors.is_empty());
        assert_eq!(result.unknown_keys, vec!["future_field".to_string()]);
        assert_eq!(result.settings.hold_key, HoldKey::Fn);
    }

    #[test]
    fn unknown_keys_from_a_newer_version_survive_a_save_from_this_build() {
        let existing = "\
# a future build's field this build has never heard of
future_field = something-new

hold_key = left-option
mode = ptt
model = base.en
insertion = clipboard-only
hud_enabled = true
sound_enabled = true
";
        // Change one field, like the Settings window would.
        let s = Settings { hold_key: HoldKey::Fn, ..Settings::default() };

        let merged = merge_and_render(existing, &s);
        assert!(
            merged.contains("future_field = something-new"),
            "an unrecognized key must survive a save verbatim:\n{merged}"
        );
        assert!(merged.contains("hold_key = fn"), "the changed field must actually update:\n{merged}");

        let reparsed = parse(&merged);
        assert!(reparsed.errors.is_empty(), "{:?}", reparsed.errors);
        assert_eq!(reparsed.settings.hold_key, HoldKey::Fn);
    }

    #[test]
    fn merge_appends_known_keys_missing_from_an_existing_file() {
        let existing = "hold_key = fn\n";
        let s = Settings::default();
        let merged = merge_and_render(existing, &s);
        for key in KNOWN_KEYS {
            assert!(merged.contains(key), "merged output missing `{key}`:\n{merged}");
        }
    }

    // -- I/O shell: save_to / load_from --

    #[test]
    fn save_then_load_round_trips_and_creates_parent_dirs() {
        let dir = std::env::temp_dir().join(format!("textify-settings-test-{}-roundtrip", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("settings.txt");

        let s = Settings { mode: DictateMode::Toggle, sound_enabled: false, ..Settings::default() };
        save_to(&path, &s).unwrap();

        let loaded = load_from(&path).unwrap();
        assert!(loaded.found);
        assert_eq!(loaded.settings, s);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_save_preserves_a_hand_added_unknown_line() {
        let dir = std::env::temp_dir().join(format!("textify-settings-test-{}-preserve", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("settings.txt");

        save_to(&path, &Settings::default()).unwrap();
        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push_str("hand_edited_field = kept-me\n");
        std::fs::write(&path, &content).unwrap();

        let s = Settings { hold_key: HoldKey::EitherOption, ..Settings::default() };
        save_to(&path, &s).unwrap();

        let final_content = std::fs::read_to_string(&path).unwrap();
        assert!(final_content.contains("hand_edited_field = kept-me"), "{final_content}");
        assert!(final_content.contains("hold_key = either-option"), "{final_content}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_path_respects_the_env_var_override() {
        std::env::set_var(SETTINGS_PATH_ENV_VAR, "/tmp/custom-settings.txt");
        let p = default_path().unwrap();
        assert_eq!(p, PathBuf::from("/tmp/custom-settings.txt"));
        std::env::remove_var(SETTINGS_PATH_ENV_VAR);
    }
}
