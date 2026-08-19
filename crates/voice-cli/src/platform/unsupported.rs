//! Fallback backend for platforms without a port yet.
//!
//! This exists so the crate compiles everywhere and `dictate` fails with a
//! straight answer instead of a link error. `transcribe` and `command` are
//! fully portable and work on any platform today; only the live loop is gated.

use super::PlatformCaps;

pub const CAPS: PlatformCaps = PlatformCaps::NONE;

pub fn unsupported() -> anyhow::Error {
    anyhow::anyhow!(
        "live dictation is macOS-only today. Windows and Linux are on the roadmap \
         (see docs/voice/PORTING.md). `textify-voice transcribe <file>` and \
         `textify-voice command \"...\"` are fully portable and work here now."
    )
}
