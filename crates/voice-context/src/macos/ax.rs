//! Raw macOS Accessibility (`AXUIElement`) and `NSWorkspace` reads.
//!
//! This is the ONLY module in the crate that touches `objc2*` types — the
//! parent module (`super`) and its state machine work with plain owned Rust
//! values (`RawFrontmost`, `RawFocusedElement`, `AxReadError`). That keeps
//! the platform boundary a real module boundary, matching PORTING.md's rule
//! that macOS-specific code stays behind the existing platform boundary
//! rather than sprinkled inline.
//!
//! ## The undocumented-constants gotcha
//!
//! `objc2-application-services` does NOT export the `kAX*Attribute` /
//! `kAX*Subrole` string constants as linkable Rust symbols. Apple declares
//! them as C preprocessor macros in the SDK headers — e.g.
//! `#define kAXRoleAttribute CFSTR("AXRole")` in `AXAttributeConstants.h` —
//! not `extern CFStringRef` symbols, so header-translator has nothing to
//! bind against (the generated `AXAttributeConstants.rs` / `AXRoleConstants.rs`
//! files are empty stubs). Callers must hand-declare the literal string
//! values themselves, as every other AX Rust wrapper does. The values below
//! were checked byte-for-byte against the SDK headers and then exercised
//! live against real focused elements (iTerm2's `AXTextArea`, Chrome's
//! `AXWebArea`) via a throwaway probe binary before landing here — see
//! `examples/probe_macos.rs` for the in-crate equivalent.
//!
//! ## Why the system-wide element is not used
//!
//! `AXUIElementCreateSystemWide()` plus `AXFocusedUIElement` reliably
//! returned `kAXErrorCannotComplete` in manual testing, even with a real,
//! already-focused window. Going through the frontmost app's own
//! `AXUIElementCreateApplication(pid)` and asking *that* element for its
//! `AXFocusedUIElement` worked correctly. This module always takes the
//! per-app path for that reason.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_app_kit::NSWorkspace;
use objc2_application_services::{AXError, AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{CFBoolean, CFRetained, CFString, CFType, CGPoint, CGSize};

const AX_FOCUSED_UI_ELEMENT_ATTRIBUTE: &str = "AXFocusedUIElement";
const AX_ROLE_ATTRIBUTE: &str = "AXRole";
const AX_SUBROLE_ATTRIBUTE: &str = "AXSubrole";
const AX_VALUE_ATTRIBUTE: &str = "AXValue";
const AX_TITLE_ATTRIBUTE: &str = "AXTitle";
const AX_DESCRIPTION_ATTRIBUTE: &str = "AXDescription";
const AX_ENABLED_ATTRIBUTE: &str = "AXEnabled";
const AX_POSITION_ATTRIBUTE: &str = "AXPosition";
const AX_SIZE_ATTRIBUTE: &str = "AXSize";

/// SDK value: `AXRoleConstants.h` `kAXSecureTextFieldSubrole`. This is the
/// one string constant the rest of the crate needs by name (secure-field
/// detection), so it is exposed rather than kept private to this module.
pub const AX_SECURE_TEXT_FIELD_SUBROLE: &str = "AXSecureTextField";

/// Whether this process currently holds the macOS Accessibility TCC grant.
/// Distinguishing "no permission" from "app exposes nothing" (task point 4)
/// starts here: when this is `false`, every AX read below is *expected* to
/// fail, and callers should report `DegradedReason::NoAccessibilityPermission`
/// rather than blaming the target application.
pub fn ax_is_trusted() -> bool {
    unsafe { objc2_application_services::AXIsProcessTrusted() }
}

/// Frontmost application identity, as read via `NSWorkspace`.
///
/// This is a WindowServer/Dock query, not a call into the target app's own
/// process — it carries none of the "can hang on an unresponsive app" risk
/// the AX reads below do, and needs no Accessibility permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrontmost {
    pub bundle_id: Option<String>,
    pub name: Option<String>,
    pub pid: i32,
}

pub fn read_frontmost() -> Option<RawFrontmost> {
    let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
    let bundle_id = app.bundleIdentifier().map(|s| s.to_string());
    let name = app.localizedName().map(|s| s.to_string());
    let pid = app.processIdentifier();
    Some(RawFrontmost { bundle_id, name, pid })
}

/// Everything read off the frontmost app's focused `AXUIElement`, as plain
/// owned values — the caller (`super::mod`) turns this into an
/// `ActionableElement` without needing to know objc2 exists.
#[derive(Debug, Clone, PartialEq)]
pub struct RawFocusedElement {
    pub role: Option<String>,
    pub subrole: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    /// `AXValue`'s "is settable" bit — this crate's definition of `writable`.
    pub value_settable: bool,
    /// `None` when the element doesn't expose `AXEnabled` at all (common —
    /// e.g. iTerm2's focused `AXTextArea` in manual testing), which is
    /// deliberately distinct from `Some(false)`. Absence is treated as "no
    /// signal", not as disabled, by the caller.
    pub enabled: Option<bool>,
    pub position: Option<(f64, f64)>,
    pub size: Option<(f64, f64)>,
}

/// Why a focused-element read failed, collapsed to the handful of cases the
/// caller needs to distinguish (see `DegradedReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxReadError {
    /// `kAXErrorAPIDisabled` (or `ax_is_trusted()` was already false) — no
    /// Accessibility permission.
    NoPermission,
    /// `kAXErrorNoValue` — the app has no focused element right now (a real,
    /// honest state: no window focused, or the app doesn't participate in
    /// AX at all for this element).
    NoFocusedElement,
    /// `kAXErrorCannotComplete` — the exact failure mode SPEC 3.1 warns an
    /// unresponsive app can produce.
    CannotComplete,
    /// Any other `AXError` code, preserved for diagnostics.
    Other(i32),
}

impl From<AXError> for AxReadError {
    fn from(err: AXError) -> Self {
        match err {
            AXError::APIDisabled => AxReadError::NoPermission,
            AXError::NoValue => AxReadError::NoFocusedElement,
            AXError::CannotComplete => AxReadError::CannotComplete,
            other => AxReadError::Other(other.0),
        }
    }
}

/// Copy an attribute value off an `AXUIElement` as a `CFType`, if any.
///
/// # Safety
/// `el` must be a live `AXUIElement`. This performs the underlying
/// `AXUIElementCopyAttributeValue` C call, which can take real wall-clock
/// time talking to another process — callers on a latency-sensitive path
/// must run this behind their own timeout (this module does not enforce
/// one itself; `super::read_focused_element_with_timeout` does).
unsafe fn copy_attr(el: &AXUIElement, attr: &str) -> Result<CFRetained<CFType>, AXError> {
    let cf_attr = CFString::from_str(attr);
    let mut out: *const CFType = std::ptr::null();
    let Some(ptr) = NonNull::new(&mut out as *mut *const CFType) else {
        return Err(AXError::Failure);
    };
    let err = unsafe { el.copy_attribute_value(&cf_attr, ptr) };
    if err != AXError::Success {
        return Err(err);
    }
    match NonNull::new(out.cast_mut()) {
        // AXError::Success but a null value pointer would be an API
        // contract violation on Apple's side; treat it as "no value"
        // rather than panicking on attacker/OS-controlled input.
        None => Err(AXError::NoValue),
        Some(nn) => Ok(unsafe { CFRetained::from_raw(nn) }),
    }
}

unsafe fn attr_as_string(el: &AXUIElement, attr: &str) -> Option<String> {
    let v = unsafe { copy_attr(el, attr) }.ok()?;
    let s = v.downcast::<CFString>().ok()?;
    Some(s.to_string())
}

unsafe fn attr_as_bool(el: &AXUIElement, attr: &str) -> Option<bool> {
    let v = unsafe { copy_attr(el, attr) }.ok()?;
    let b = v.downcast::<CFBoolean>().ok()?;
    Some(b.as_bool())
}

unsafe fn attr_as_point(el: &AXUIElement, attr: &str) -> Option<(f64, f64)> {
    let v = unsafe { copy_attr(el, attr) }.ok()?;
    let axv = v.downcast::<AXValue>().ok()?;
    let mut point = CGPoint { x: 0.0, y: 0.0 };
    let ptr = NonNull::new(&mut point as *mut CGPoint as *mut c_void)?;
    let ok = unsafe { axv.value(AXValueType::CGPoint, ptr) };
    ok.then_some((point.x, point.y))
}

unsafe fn attr_as_size(el: &AXUIElement, attr: &str) -> Option<(f64, f64)> {
    let v = unsafe { copy_attr(el, attr) }.ok()?;
    let axv = v.downcast::<AXValue>().ok()?;
    let mut size = CGSize { width: 0.0, height: 0.0 };
    let ptr = NonNull::new(&mut size as *mut CGSize as *mut c_void)?;
    let ok = unsafe { axv.value(AXValueType::CGSize, ptr) };
    ok.then_some((size.width, size.height))
}

unsafe fn is_settable(el: &AXUIElement, attr: &str) -> Result<bool, AXError> {
    let cf_attr = CFString::from_str(attr);
    let mut settable: u8 = 0; // objc2_core_foundation::Boolean == c_uchar
    let Some(ptr) = NonNull::new(&mut settable as *mut u8) else {
        return Err(AXError::Failure);
    };
    let err = unsafe { el.is_attribute_settable(&cf_attr, ptr) };
    if err != AXError::Success {
        return Err(err);
    }
    Ok(settable != 0)
}

/// Read the focused element of the application identified by `pid`.
///
/// Deliberately synchronous and unbounded in time on its own — the FFI call
/// inside can genuinely block on an unresponsive target process (SPEC 3.1's
/// documented risk). Timeout enforcement is the caller's job
/// (`super::read_focused_element_with_timeout` runs this on its own thread
/// and gives up after a budget), so this function stays a straight,
/// testable translation of the AX calls with no timing policy baked in.
pub fn read_focused_element(pid: i32) -> Result<RawFocusedElement, AxReadError> {
    if !ax_is_trusted() {
        return Err(AxReadError::NoPermission);
    }

    let app_el = unsafe { AXUIElement::new_application(pid) };
    let focused_cf = unsafe { copy_attr(&app_el, AX_FOCUSED_UI_ELEMENT_ATTRIBUTE) }.map_err(AxReadError::from)?;
    let focused = focused_cf.downcast::<AXUIElement>().map_err(|_| AxReadError::Other(-1))?;

    let role = unsafe { attr_as_string(&focused, AX_ROLE_ATTRIBUTE) };
    let subrole = unsafe { attr_as_string(&focused, AX_SUBROLE_ATTRIBUTE) };
    let title = unsafe { attr_as_string(&focused, AX_TITLE_ATTRIBUTE) };
    let description = unsafe { attr_as_string(&focused, AX_DESCRIPTION_ATTRIBUTE) };
    let value_settable = unsafe { is_settable(&focused, AX_VALUE_ATTRIBUTE) }.unwrap_or(false);
    let enabled = unsafe { attr_as_bool(&focused, AX_ENABLED_ATTRIBUTE) };
    let position = unsafe { attr_as_point(&focused, AX_POSITION_ATTRIBUTE) };
    let size = unsafe { attr_as_size(&focused, AX_SIZE_ATTRIBUTE) };

    Ok(RawFocusedElement { role, subrole, title, description, value_settable, enabled, position, size })
}
