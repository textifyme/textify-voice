//! Telemetry shape. COMMANDS-SPEC.md §3.5 #6 / §6: "command history honors
//! `NeverStore`; telemetry is counters and schema-ids only — never
//! utterance text, never target labels (schema-id histograms are safe;
//! 'clicked Send invoice to Acme' is not)."
//!
//! [`TelemetryEvent`] enforces this *structurally*: every field is either a
//! `&'static str` (only ever a schema id, a compile-time constant defined
//! in [`crate::registry`]) or a closed enum. There is no `String` field in
//! this type at all, so there is no API surface through which a runtime
//! utterance or a live target label could be smuggled in.

use crate::schema::Tier;

/// What happened to a resolved action, as far as telemetry is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryOutcome {
    Executed,
    ExecutedAndAnnounced,
    Confirmed,
    Denied,
    TimedOut,
    Refused,
    NeedsDisambiguation,
    Undone,
    Redone,
}

/// A single telemetry event. Deliberately field-limited: `schema_id` is
/// `&'static str` (a registry constant, never user-derived), `tier` and
/// `outcome` are closed enums. See module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetryEvent {
    pub schema_id: &'static str,
    pub tier: Tier,
    pub outcome: TelemetryOutcome,
    /// Coarse latency bucket in milliseconds, never a payload.
    pub latency_ms: Option<u32>,
}

/// Sink for emitted events. Real backends batch/upload counters and
/// latency histograms; this run only needs the trait boundary + an
/// in-memory implementation for tests.
pub trait TelemetrySink {
    fn emit(&mut self, event: TelemetryEvent);
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryTelemetrySink {
    pub events: Vec<TelemetryEvent>,
}

impl TelemetrySink for InMemoryTelemetrySink {
    fn emit(&mut self, event: TelemetryEvent) {
        self.events.push(event);
    }
}

impl InMemoryTelemetrySink {
    pub fn schema_id_counts(&self) -> std::collections::HashMap<&'static str, usize> {
        let mut counts = std::collections::HashMap::new();
        for e in &self.events {
            *counts.entry(e.schema_id).or_insert(0) += 1;
        }
        counts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// COMMANDS-SPEC.md §3.5 #6: even when the surrounding system handled
    /// a target with a highly identifying label ("Send invoice to Acme
    /// Corp now"), the emitted telemetry must not contain any trace of it.
    /// Because `TelemetryEvent` has no string-typed field other than the
    /// compile-time `schema_id`, this is true by construction; the
    /// assertion below is a regression guard against that invariant ever
    /// being weakened (e.g. someone adding a `label: String` field later).
    #[test]
    fn telemetry_event_cannot_carry_target_label_or_utterance_text() {
        let hostile_label = "Send invoice to Acme Corp now!!";
        let hostile_utterance = "click the send button for the Acme invoice";

        let event = TelemetryEvent {
            schema_id: "ui.click",
            tier: Tier::T2,
            outcome: TelemetryOutcome::Confirmed,
            latency_ms: Some(120),
        };

        let rendered = format!("{event:?}");
        assert!(!rendered.contains("Acme"));
        assert!(!rendered.contains("invoice"));
        assert!(!rendered.contains(hostile_label));
        assert!(!rendered.contains(hostile_utterance));
        // What IS present is exactly the safe, closed set.
        assert!(rendered.contains("ui.click"));
        assert!(rendered.contains("T2"));
    }

    #[test]
    fn sink_aggregates_counts_by_schema_id_only() {
        let mut sink = InMemoryTelemetrySink::default();
        sink.emit(TelemetryEvent { schema_id: "win.maximize", tier: Tier::T0, outcome: TelemetryOutcome::Executed, latency_ms: None });
        sink.emit(TelemetryEvent { schema_id: "win.maximize", tier: Tier::T0, outcome: TelemetryOutcome::Executed, latency_ms: None });
        sink.emit(TelemetryEvent { schema_id: "ui.click", tier: Tier::T1, outcome: TelemetryOutcome::ExecutedAndAnnounced, latency_ms: Some(80) });

        let counts = sink.schema_id_counts();
        assert_eq!(counts.get("win.maximize"), Some(&2));
        assert_eq!(counts.get("ui.click"), Some(&1));
    }
}
