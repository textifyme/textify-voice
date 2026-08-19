//! `voice-context` — on-screen context capture for Textify Voice.
//!
//! Owns the in-memory actionable-element map (COMMANDS-SPEC §3.2) and the
//! async, best-effort context-capture contract (SPEC §3.1, §3.3). The core
//! types (`types`, `map`, `provider`) carry zero unconditional native
//! platform dependency (no `objc2`, no `windows-rs`, no AX/UIA calls in the
//! base `[dependencies]` table — see `Cargo.toml`), and are exercised
//! through [`provider::FixtureContextProvider`], a deterministic in-memory
//! implementation used by tests and by other crates.
//!
//! A real platform backend now sits ADDITIONALLY behind the same
//! [`provider::ContextProvider`] trait: [`macos::MacosContextProvider`] on
//! macOS (`NSWorkspace` + `AXUIElement`, target-gated dependencies only —
//! see `Cargo.toml`'s `[target.'cfg(target_os = "macos")'.dependencies]`).
//! The crate still compiles, and the fixture provider still works
//! unchanged, on every other target; `cargo test --workspace` on a non-macOS
//! host simply never sees the `macos` module at all. Windows (UIAutomation)
//! is future scope behind the same trait, per `docs/voice/PORTING.md`.
//!
//! Independent of `voice-core` / `voice-intent` / `voice-act` / `voice-format`
//! by design for this phase — see each module for the local types this crate
//! defines rather than importing from a sibling crate.

#[cfg(target_os = "macos")]
pub mod macos;
pub mod map;
pub mod provider;
pub mod types;

#[cfg(target_os = "macos")]
pub use macos::{classify_bundle_id, BundleIdRule, MacosContextProvider, BUNDLE_ID_RULES, DEFAULT_AX_TIMEOUT};
pub use map::{ActionableMap, LabelCandidate, LabelMatch};
pub use provider::{ContextCapture, ContextProvider, ContextSnapshot, FixtureContextProvider, PendingContext, PollResult};
pub use types::{
    ActionableElement, AppInfo, AppKind, BiasTerm, Coverage, DegradedReason, ElementRole, Position,
    SecureFieldStatus,
};
