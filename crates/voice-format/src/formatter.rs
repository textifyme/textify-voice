//! The `Formatter` trait, a deterministic passthrough stub, and the
//! formatting-gate integration point.
//!
//! SPEC §3.4: local formatting runs after the deterministic normalizer, only
//! when "a heuristic gate decides if an LLM pass would change the output —
//! forced off in AI/coding apps (paste raw, matching Wispr's confirmed
//! behavior)". Real backends (Apple Foundation Models, TextRewriter/Phi
//! Silica, mistral.rs + Qwen3.5/Phi-4-mini) are out of scope for this run —
//! see [`PassthroughFormatter`] for the deterministic in-memory stand-in.

use crate::types::{FormatRequest, FormatResponse};

/// A local text formatter. Implementations may run an on-device model; this
/// crate ships only [`PassthroughFormatter`], a no-op stand-in that lets
/// callers (and their tests) exercise the gate/wiring without any native
/// runtime.
pub trait Formatter {
    fn format(&self, request: &FormatRequest) -> FormatResponse;
}

/// Deterministic stub: returns `request.text` unchanged. No mistral.rs, no
/// Apple Foundation Models, no network — matches the run's native-dependency
/// ban.
#[derive(Debug, Default, Clone, Copy)]
pub struct PassthroughFormatter;

impl Formatter for PassthroughFormatter {
    fn format(&self, request: &FormatRequest) -> FormatResponse {
        FormatResponse { formatted_text: request.text.clone() }
    }
}

/// Why the formatting gate is closed for this request. SPEC §3.4: "forced
/// off in AI/coding apps."
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateClosedReason {
    /// `app_kind` is AI/coding — paste raw, per SPEC §3.4.
    AiOrCodingApp,
    /// User preference / privacy setting disabled local formatting.
    UserDisabled,
    /// Any other policy reason the caller wants to record.
    Other(String),
}

/// The formatting-gate decision, computed upstream (by the normalizer /
/// app-kind detection this crate does not own) and passed in as input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    Open,
    Closed(GateClosedReason),
}

/// Wraps a [`Formatter`] with the gate integration point: when the gate is
/// closed, this no-ops — the inner formatter is never invoked, and the
/// request's raw text is returned unchanged (SPEC §3.4 "paste raw").
pub struct GatedFormatter<F: Formatter> {
    inner: F,
}

impl<F: Formatter> GatedFormatter<F> {
    pub fn new(inner: F) -> Self {
        Self { inner }
    }

    /// Applies the gate decision. `Open` delegates to the inner formatter;
    /// `Closed` never calls it and returns the input text unchanged.
    pub fn format(&self, request: &FormatRequest, gate: &GateDecision) -> FormatResponse {
        match gate {
            GateDecision::Open => self.inner.format(request),
            GateDecision::Closed(_) => FormatResponse { formatted_text: request.text.clone() },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct CountingFormatter {
        calls: Cell<u32>,
    }

    impl CountingFormatter {
        fn new() -> Self {
            Self { calls: Cell::new(0) }
        }
    }

    impl Formatter for CountingFormatter {
        fn format(&self, request: &FormatRequest) -> FormatResponse {
            self.calls.set(self.calls.get() + 1);
            // Deliberately transform the text so a leak through the gate
            // would be visible, not just "unchanged by coincidence".
            FormatResponse { formatted_text: format!("[[formatted]] {}", request.text) }
        }
    }

    fn req(text: &str) -> FormatRequest {
        FormatRequest { text: text.to_string(), style: None, app_kind: None }
    }

    #[test]
    fn passthrough_formatter_is_identity() {
        let f = PassthroughFormatter;
        let resp = f.format(&req("hello there"));
        assert_eq!(resp.formatted_text, "hello there");
    }

    #[test]
    fn gate_open_delegates_to_inner_formatter() {
        let inner = CountingFormatter::new();
        let gated = GatedFormatter::new(inner);
        let resp = gated.format(&req("hello"), &GateDecision::Open);
        assert_eq!(resp.formatted_text, "[[formatted]] hello");
        assert_eq!(gated.inner.calls.get(), 1);
    }

    #[test]
    fn gate_closed_is_a_true_noop_inner_never_called() {
        let inner = CountingFormatter::new();
        let gated = GatedFormatter::new(inner);
        let gate = GateDecision::Closed(GateClosedReason::AiOrCodingApp);
        let resp = gated.format(&req("fn main() {}"), &gate);
        // Raw pass-through, not merely "same text by chance": the inner
        // formatter (which would prepend a marker) must not have run.
        assert_eq!(resp.formatted_text, "fn main() {}");
        assert_eq!(gated.inner.calls.get(), 0, "formatter must not be invoked when the gate is closed");
    }
}
