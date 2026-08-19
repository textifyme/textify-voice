//! Bias pipeline layer 3: the constrained local LLM judge-editor.
//!
//! SPEC §3.3 layer 3: "Constrained local LLM judge-editor (residual
//! ambiguous spans only, gated): structured-output call — 'pick the correct
//! term from these candidates or leave unchanged,' never free rewriting.
//! Same local runtime as the formatter (§3.4)."
//!
//! The load-bearing property this module implements is the **guard**: the
//! judge backend is untrusted (it is, in production, model output) and this
//! code must make it structurally impossible for a judge to substitute
//! anything outside the closed candidate list supplied for that span — even
//! if the backend is buggy, adversarial, or returns garbage. The guard lives
//! in [`resolve_ambiguous_span`], not in the backend.

/// A residual span the normalizer/bias-layer-2 pass left ambiguous, plus the
/// **closed** set of candidates it may be replaced with. `candidates` comes
/// from `BiasContext` terms upstream (SPEC §3.3); this crate does not
/// construct it, only consumes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbiguousSpan {
    pub text: String,
    pub candidates: Vec<String>,
}

/// What a [`JudgeBackend`] proposes for a span. Untrusted: `Pick` may name
/// anything, including a value outside `candidates` — see
/// [`resolve_ambiguous_span`] for the enforcement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgeProposal {
    Pick(String),
    LeaveUnchanged,
}

/// The constrained judge backend: "same local runtime as the formatter"
/// (SPEC §3.3). This crate ships no real backend (no mistral.rs, no Apple
/// FM) — only test doubles exercising the guard.
pub trait JudgeBackend {
    fn judge(&self, span: &AmbiguousSpan) -> JudgeProposal;
}

/// Outcome of applying the guard to a backend's proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgeOutcome {
    /// The backend picked a candidate that was verbatim in the closed list —
    /// the only case a replacement is allowed to happen.
    Replaced(String),
    /// Either the backend said to leave it, or it proposed something outside
    /// the closed list and the guard rejected it.
    Unchanged,
}

/// Applies `judge`'s proposal to `span`, enforcing SPEC §3.3 layer 3's
/// closed-candidate-list contract structurally: a proposal is honored only
/// when it is an exact (case-sensitive) match to one of `span.candidates`.
/// Anything else — an out-of-list string, a near-miss, a case variant, or a
/// backend that ignores the schema entirely — is discarded in favor of
/// leaving the span unchanged. The backend is never trusted to have obeyed
/// its own instructions.
pub fn resolve_ambiguous_span<J: JudgeBackend>(judge: &J, span: &AmbiguousSpan) -> JudgeOutcome {
    match judge.judge(span) {
        JudgeProposal::LeaveUnchanged => JudgeOutcome::Unchanged,
        JudgeProposal::Pick(picked) => {
            if span.candidates.iter().any(|c| c == &picked) {
                JudgeOutcome::Replaced(picked)
            } else {
                JudgeOutcome::Unchanged
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedJudge(JudgeProposal);
    impl JudgeBackend for FixedJudge {
        fn judge(&self, _span: &AmbiguousSpan) -> JudgeProposal {
            self.0.clone()
        }
    }

    fn span() -> AmbiguousSpan {
        AmbiguousSpan {
            text: "postgress".to_string(),
            candidates: vec!["Postgres".to_string(), "Postgrest".to_string()],
        }
    }

    #[test]
    fn valid_pick_from_candidate_list_is_applied() {
        let judge = FixedJudge(JudgeProposal::Pick("Postgres".to_string()));
        let outcome = resolve_ambiguous_span(&judge, &span());
        assert_eq!(outcome, JudgeOutcome::Replaced("Postgres".to_string()));
    }

    #[test]
    fn leave_unchanged_proposal_is_honored() {
        let judge = FixedJudge(JudgeProposal::LeaveUnchanged);
        let outcome = resolve_ambiguous_span(&judge, &span());
        assert_eq!(outcome, JudgeOutcome::Unchanged);
    }

    /// The point of this module: a judge backend that ignores its own
    /// closed-list instruction and proposes an arbitrary out-of-list string
    /// (garbage, or an adversarial value like injected imperative text) must
    /// never be substituted into the output. This is the spec's own
    /// acceptance criterion for layer 3.
    #[test]
    fn out_of_list_pick_is_rejected_never_substituted() {
        let judge = FixedJudge(JudgeProposal::Pick("DROP TABLE users;".to_string()));
        let outcome = resolve_ambiguous_span(&judge, &span());
        assert_eq!(outcome, JudgeOutcome::Unchanged);
    }

    #[test]
    fn case_variant_of_a_real_candidate_is_still_rejected() {
        // Exact match only — "postgres" (lowercase) is not literally in the
        // candidate list ["Postgres", "Postgrest"], so a judge that got
        // sloppy about case must not have its pick honored.
        let judge = FixedJudge(JudgeProposal::Pick("postgres".to_string()));
        let outcome = resolve_ambiguous_span(&judge, &span());
        assert_eq!(outcome, JudgeOutcome::Unchanged);
    }

    #[test]
    fn empty_candidate_list_rejects_every_pick() {
        let empty = AmbiguousSpan { text: "whatever".to_string(), candidates: vec![] };
        let judge = FixedJudge(JudgeProposal::Pick("whatever".to_string()));
        let outcome = resolve_ambiguous_span(&judge, &empty);
        assert_eq!(outcome, JudgeOutcome::Unchanged);
    }

    #[test]
    fn substring_of_a_candidate_is_not_treated_as_a_match() {
        // Guards against a lenient "contains" implementation slipping in —
        // the contract is exact membership, not fuzzy containment.
        let judge = FixedJudge(JudgeProposal::Pick("Postgre".to_string()));
        let outcome = resolve_ambiguous_span(&judge, &span());
        assert_eq!(outcome, JudgeOutcome::Unchanged);
    }
}
