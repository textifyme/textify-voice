//! Stage 2 — constrained local LLM parse over the closed schema set.
//! COMMANDS-SPEC §3.1 ("Intent stage 2" row), §3.3, §3.5 #1.
//!
//! No LLM, no model, no network in this crate/phase — this module defines
//! the TRAIT boundary only, plus a deterministic stub implementation for
//! tests. The real backend (Apple FM guided generation / mistral.rs
//! grammar-constrained decode, the same runtime `voice-format` ships) is
//! built elsewhere and plugs in behind [`ConstrainedParser`].

use crate::types::{ActionInstance, IntentResult, MatchStage, RejectReason, SlotValue};

/// A closed-set intent parser. COMMANDS-SPEC §3.5 #1: "Stage 2 cannot
/// emit anything outside the schema registry — constrained decoding, not
/// post-hoc filtering. The worst possible parse error is the *wrong
/// registered action*, never an *arbitrary* one."
///
/// The structural guarantee lives in the shape of [`Stage2Outcome`]: the
/// only way to name a schema is `Emit { index, .. }`, and `index` is an
/// offset into the `registered` slice the CALLER supplies for that one
/// call — there is no variant carrying a free-form string. An
/// implementation cannot fabricate a schema id outside `registered`
/// because nothing in the type lets it hold one; the worst a buggy or
/// adversarially-prompted implementation can do is return an
/// out-of-range index, and [`resolve`] turns that into
/// `Reject { Unsupported }`, never a fabricated `Matched`. This makes
/// "emit an unregistered action" a compile-time-unrepresentable state,
/// not a runtime check that a broken implementation could skip.
pub trait ConstrainedParser {
    /// Parse `utterance` against the closed set `registered` (the schema
    /// ids the live `voice-act` registry currently exposes). MUST NOT use
    /// any network or model download at call time in this phase —
    /// deterministic/local only.
    fn parse(&self, utterance: &str, registered: &[&'static str]) -> Stage2Outcome;
}

/// Raw result from a [`ConstrainedParser`], before bounds-checking
/// against the caller's `registered` slice (see [`resolve`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Stage2Outcome {
    /// Select `registered[index]` as the matched schema, with slots the
    /// parser extracted from the utterance.
    Emit { index: usize, confidence: f32, slots: Vec<SlotValue> },
    Reject { reason: RejectReason },
}

/// Turn a [`Stage2Outcome`] into an [`IntentResult`], defensively
/// bounds-checking `index` against `registered`. This is the enforcement
/// point for "structurally impossible to emit an unregistered action"
/// (COMMANDS-SPEC §3.5 #1): an out-of-range index is treated as a
/// contract violation by the `ConstrainedParser` impl, never a panic —
/// it degrades to `Reject { Unsupported }`, so a hostile or buggy parser
/// can at worst deny a command, never fabricate one.
pub fn resolve(outcome: Stage2Outcome, registered: &[&'static str]) -> IntentResult {
    match outcome {
        Stage2Outcome::Emit { index, confidence, slots } => match registered.get(index) {
            Some(&schema_id) => IntentResult::Matched {
                action: ActionInstance { schema_id, slots },
                stage: MatchStage::LocalLlm,
                confidence,
            },
            None => IntentResult::Reject { reason: RejectReason::Unsupported },
        },
        Stage2Outcome::Reject { reason } => IntentResult::Reject { reason },
    }
}

/// A deterministic stub [`ConstrainedParser`] for tests — a small fixed
/// keyword table, **not** an LLM. Demonstrates the trait boundary; the
/// real constrained-decode backend is out of scope for this crate/phase.
pub struct StubConstrainedParser {
    /// (substring to look for in the lowercased utterance, schema id it
    /// would emit if that schema is currently registered).
    table: &'static [(&'static str, &'static str)],
}

impl StubConstrainedParser {
    pub fn new(table: &'static [(&'static str, &'static str)]) -> Self {
        Self { table }
    }
}

impl ConstrainedParser for StubConstrainedParser {
    fn parse(&self, utterance: &str, registered: &[&'static str]) -> Stage2Outcome {
        let lower = utterance.to_lowercase();
        for (needle, schema_id) in self.table {
            if lower.contains(needle) {
                if let Some(index) = registered.iter().position(|id| id == schema_id) {
                    return Stage2Outcome::Emit { index, confidence: 0.8, slots: Vec::new() };
                }
                // The schema this stub would name isn't in the live
                // registered set for this call — fall through and keep
                // looking rather than emit anything.
            }
        }
        Stage2Outcome::Reject { reason: RejectReason::NotACommand }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGISTERED: &[&str] = &["app.open", "win.tile", "ui.click"];

    #[test]
    fn stub_emits_a_registered_schema_by_index() {
        let parser = StubConstrainedParser::new(&[("open", "app.open")]);
        let outcome = parser.parse("would you please open my email", REGISTERED);
        match outcome {
            Stage2Outcome::Emit { index, .. } => assert_eq!(REGISTERED[index], "app.open"),
            Stage2Outcome::Reject { .. } => panic!("expected Emit"),
        }
    }

    #[test]
    fn resolve_turns_emit_into_matched_local_llm() {
        let parser = StubConstrainedParser::new(&[("open", "app.open")]);
        let outcome = parser.parse("open my email please", REGISTERED);
        let result = resolve(outcome, REGISTERED);
        match result {
            IntentResult::Matched { action, stage, .. } => {
                assert_eq!(action.schema_id, "app.open");
                assert_eq!(stage, MatchStage::LocalLlm);
            }
            IntentResult::Reject { reason } => panic!("expected Matched, got Reject({reason})"),
        }
    }

    #[test]
    fn resolve_passes_through_reject() {
        let parser = StubConstrainedParser::new(&[("open", "app.open")]);
        let outcome = parser.parse("what a nice day", REGISTERED);
        let result = resolve(outcome, REGISTERED);
        assert_eq!(result, IntentResult::Reject { reason: RejectReason::NotACommand });
    }

    #[test]
    fn stub_never_emits_a_schema_absent_from_the_registered_set() {
        // The stub's table names "app.open", but it is not in this call's
        // registered set — the closed set wins, no substitution/fallback.
        let narrow: &[&str] = &["win.tile"];
        let parser = StubConstrainedParser::new(&[("open", "app.open")]);
        let outcome = parser.parse("open the pod bay doors", narrow);
        assert_eq!(outcome, Stage2Outcome::Reject { reason: RejectReason::NotACommand });
    }

    /// A deliberately hostile/buggy parser that always claims an
    /// out-of-range index, as if trying to name a schema outside the
    /// supplied closed set.
    struct HostileParser;

    impl ConstrainedParser for HostileParser {
        fn parse(&self, _utterance: &str, _registered: &[&'static str]) -> Stage2Outcome {
            Stage2Outcome::Emit { index: 9_999, confidence: 1.0, slots: Vec::new() }
        }
    }

    #[test]
    fn out_of_range_index_cannot_fabricate_an_unregistered_action() {
        // The KEY property (COMMANDS-SPEC §3.5 #1): even a parser that
        // tries to point outside the closed set cannot produce a
        // fabricated Matched — resolve() degrades to Reject, never
        // panics, never invents a schema id.
        let outcome = HostileParser.parse("anything at all", REGISTERED);
        let result = resolve(outcome, REGISTERED);
        assert_eq!(result, IntentResult::Reject { reason: RejectReason::Unsupported });
    }

    #[test]
    fn every_possible_emit_index_resolves_to_a_member_of_registered() {
        // Exhaustive structural check: for every index a parser COULD
        // legally emit (0..registered.len()), resolve() names a schema
        // that is literally an element of `registered` — the return type
        // makes anything else unrepresentable.
        for i in 0..REGISTERED.len() {
            let outcome = Stage2Outcome::Emit { index: i, confidence: 0.9, slots: Vec::new() };
            match resolve(outcome, REGISTERED) {
                IntentResult::Matched { action, .. } => {
                    assert!(REGISTERED.contains(&action.schema_id));
                }
                IntentResult::Reject { reason } => panic!("expected Matched, got Reject({reason})"),
            }
        }
    }
}
