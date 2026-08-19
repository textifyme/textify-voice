//! Bias pipeline **layer 2**: deterministic phonetic post-correction
//! (SPEC.md §3.3). Runs inside the normalizer, every engine, ~0 ms.
//!
//! Gate: a word (or multi-word span) is only a *candidate* for correction if
//! EVERY word in it has ASR confidence below
//! [`CorrectionThresholds::confidence_ceiling`] — "leave high-confidence
//! spans untouched" is enforced here, not left to chance, and enforced per
//! word so a single confident word can't be dragged into a rewrite by an
//! uncertain neighbour (e.g. "Sarah" at 0.95 next to "conor" at 0.30 must
//! not have the whole two-word span replaced just because the span's
//! *minimum* confidence clears the gate). A candidate is only actually
//! corrected if it clears **both**
//! gates: its Double Metaphone code matches a `BiasContext` term's code
//! ([`crate::metaphone`]), *and* the case-insensitive edit distance
//! ([`crate::edit_distance`]) between the span and the term is within a
//! length-scaled threshold. Phonetic match alone is not enough — see the
//! `kafka_style_phonetic_match_but_edit_distance_too_far_is_rejected` test,
//! which is exactly the "near-miss below threshold" case this run's spec
//! calls out.

use crate::edit_distance::damerau_levenshtein_ci;
use crate::metaphone::double_metaphone;
use crate::{BiasContext, BiasTerm};

/// One ASR-output word plus its per-word confidence — the unit layer 2
/// operates on. Deliberately not `crate::asr::WordConfidence` (that type is
/// the ASR engine's output contract; this one additionally treats the list
/// as a positionally-addressable span sequence for multi-word matching).
#[derive(Debug, Clone, PartialEq)]
pub struct WordSpan {
    pub text: String,
    pub confidence: f32,
}

impl WordSpan {
    #[must_use]
    pub fn new(text: impl Into<String>, confidence: f32) -> Self {
        Self {
            text: text.into(),
            confidence,
        }
    }
}

/// Tunable gates for layer 2. Defaults are deliberately conservative: they
/// favor leaving ambiguous output alone over over-correcting, matching
/// SPEC's framing of layer 2 as the *deterministic* (i.e. high-precision)
/// layer — the judge-editor (layer 3) is where genuinely ambiguous spans
/// get resolved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CorrectionThresholds {
    /// Words at or above this confidence are never touched, regardless of
    /// how well a bias term matches.
    pub confidence_ceiling: f32,
    /// Max allowed edit distance as a fraction of the bias term's
    /// character length (rounded, minimum 1) — e.g. 0.34 allows roughly
    /// one edit per three characters, which comfortably covers a single
    /// inserted/dropped/substituted letter ("postgres" -> "postgress") while
    /// rejecting spans that are only vaguely phonetically similar.
    pub max_relative_edit: f32,
    /// Bias terms with more words than this are never considered as a
    /// single correction span (keeps the sliding-window search bounded).
    pub max_term_words: usize,
}

impl Default for CorrectionThresholds {
    fn default() -> Self {
        Self {
            confidence_ceiling: 0.80,
            max_relative_edit: 0.34,
            max_term_words: 4,
        }
    }
}

impl CorrectionThresholds {
    fn max_edit_for(&self, term_len_chars: usize) -> usize {
        ((term_len_chars as f32) * self.max_relative_edit)
            .round()
            .max(1.0) as usize
    }
}

/// One accepted correction: replace `words[span_start..span_start+span_len]`
/// with `replacement`.
#[derive(Debug, Clone, PartialEq)]
pub struct Correction {
    pub span_start: usize,
    pub span_len: usize,
    pub replacement: String,
    pub matched_term: String,
}

/// Effective bias terms for matching purposes: `ctx.terms` plus `prev_terms`
/// wrapped at a lower weight (previous-utterance continuity, per SPEC §3.3's
/// mention of "prior-utterance terms" as a `BiasContext` input) — but only
/// for names not already present in `ctx.terms`, so an explicit term's
/// weight is never diluted by its own carry-over echo.
fn effective_terms(ctx: &BiasContext) -> Vec<BiasTerm> {
    let mut terms = ctx.terms.clone();
    let known: std::collections::HashSet<&str> =
        ctx.terms.iter().map(|t| t.text.as_str()).collect();
    for prev in &ctx.prev_terms {
        if !known.contains(prev.as_str()) {
            terms.push(BiasTerm::weighted(prev.clone(), 0.5));
        }
    }
    terms
}

/// Find every accepted correction in `words` against `ctx`. Returns
/// non-overlapping corrections, longest span first at each position so a
/// multi-word bias term (e.g. a two-word proper noun) wins over a
/// single-word coincidental match.
#[must_use]
pub fn correct_spans(
    words: &[WordSpan],
    ctx: &BiasContext,
    thresholds: &CorrectionThresholds,
) -> Vec<Correction> {
    if words.is_empty() {
        return Vec::new();
    }
    let terms = effective_terms(ctx);
    if terms.is_empty() {
        return Vec::new();
    }

    let mut corrections: Vec<Correction> = Vec::new();
    let mut consumed = vec![false; words.len()];

    let mut start = 0usize;
    while start < words.len() {
        if consumed[start] {
            start += 1;
            continue;
        }

        let max_len = thresholds.max_term_words.min(words.len() - start);
        let mut best: Option<(usize, usize, &BiasTerm)> = None; // (span_len, edit_distance, term)

        for span_len in (1..=max_len).rev() {
            if consumed[start..start + span_len].iter().any(|&c| c) {
                continue;
            }
            let span = &words[start..start + span_len];
            let max_confidence = span
                .iter()
                .map(|w| w.confidence)
                .fold(f32::NEG_INFINITY, f32::max);
            if max_confidence >= thresholds.confidence_ceiling {
                // SPEC 3.3 layer 2: "preserve high-confidence spans; touch
                // only uncertain ones." Using the span's MINIMUM confidence
                // here (the old behavior) let a single low-confidence word
                // drag high-confidence neighbours into rewriting — e.g.
                // "Sarah" at 0.95 next to "conor" at 0.30 would have the
                // whole two-word span replaced despite the ASR being 95%
                // sure about "Sarah". Requiring every word in the span to
                // be below the ceiling means a span containing ANY
                // confident word is left alone entirely; eligibility for
                // multi-word rewriting is therefore restricted to spans
                // that are uncertain throughout, not just somewhere.
                continue;
            }

            let candidate_text = span
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let candidate_code = double_metaphone(&candidate_text);

            for term in &terms {
                let term_word_count = term.text.split_whitespace().count().max(1);
                if term_word_count != span_len {
                    continue;
                }
                let term_code = double_metaphone(&term.text);
                if !candidate_code.matches(&term_code) {
                    continue;
                }
                let dist = damerau_levenshtein_ci(&candidate_text, &term.text);
                let max_allowed = thresholds.max_edit_for(term.text.chars().count());
                if dist > max_allowed {
                    continue;
                }
                let better = match &best {
                    None => true,
                    Some((best_len, best_dist, best_term)) => {
                        // Prefer: longer span, then lower edit distance,
                        // then higher weight.
                        span_len > *best_len
                            || (span_len == *best_len && dist < *best_dist)
                            || (span_len == *best_len
                                && dist == *best_dist
                                && term.weight > best_term.weight)
                    }
                };
                if better {
                    best = Some((span_len, dist, term));
                }
            }
        }

        if let Some((span_len, _dist, term)) = best {
            for slot in consumed.iter_mut().skip(start).take(span_len) {
                *slot = true;
            }
            corrections.push(Correction {
                span_start: start,
                span_len,
                replacement: term.text.clone(),
                matched_term: term.text.clone(),
            });
            start += span_len;
        } else {
            start += 1;
        }
    }

    corrections
}

/// Apply `corrections` (as returned by [`correct_spans`]) to `words`,
/// producing the corrected word sequence. Corrections are expected to be
/// non-overlapping, as `correct_spans` guarantees; overlapping input is
/// handled defensively by letting the first one encountered win rather than
/// panicking.
#[must_use]
pub fn apply_corrections(words: &[WordSpan], corrections: &[Correction]) -> Vec<String> {
    let mut out = Vec::with_capacity(words.len());
    let mut i = 0usize;
    let mut sorted = corrections.to_vec();
    sorted.sort_by_key(|c| c.span_start);

    let mut next_correction = 0usize;
    while i < words.len() {
        if let Some(c) = sorted.get(next_correction) {
            if c.span_start == i {
                out.push(c.replacement.clone());
                i += c.span_len.max(1);
                next_correction += 1;
                continue;
            }
        }
        out.push(words[i].text.clone());
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppKind;

    fn ctx(terms: &[&str], app_kind: AppKind) -> BiasContext {
        BiasContext {
            terms: terms.iter().map(|t| BiasTerm::new(*t)).collect(),
            app_kind,
            prev_terms: Vec::new(),
        }
    }

    #[test]
    fn spec_example_postgress_corrects_to_postgres() {
        let words = vec![WordSpan::new("postgress", 0.4)];
        let corrections = correct_spans(
            &words,
            &ctx(&["Postgres"], AppKind::Code),
            &CorrectionThresholds::default(),
        );
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].replacement, "Postgres");
        assert_eq!(
            apply_corrections(&words, &corrections),
            vec!["Postgres".to_string()]
        );
    }

    #[test]
    fn high_confidence_words_are_never_touched() {
        // Same misspelling, but the ASR is confident about it — must be
        // left alone per SPEC layer 2's "leave high-confidence spans
        // untouched."
        let words = vec![WordSpan::new("postgress", 0.95)];
        let corrections = correct_spans(
            &words,
            &ctx(&["Postgres"], AppKind::Code),
            &CorrectionThresholds::default(),
        );
        assert!(
            corrections.is_empty(),
            "high-confidence span must not be corrected"
        );
    }

    #[test]
    fn phonetic_match_but_edit_distance_too_far_is_rejected() {
        // Hand-verified in metaphone.rs: "Kafka" and "Kaaaaafka" share the
        // primary code KFK, but their edit distance (4) blows past the
        // length-scaled threshold for a 5-char term — the classic
        // "near-miss below threshold" case.
        let words = vec![WordSpan::new("kaaaaafka", 0.3)];
        let corrections = correct_spans(
            &words,
            &ctx(&["Kafka"], AppKind::General),
            &CorrectionThresholds::default(),
        );
        assert!(
            corrections.is_empty(),
            "phonetic match alone must not be sufficient without the edit-distance gate"
        );
    }

    #[test]
    fn terms_absent_from_bias_context_leave_low_confidence_words_untouched() {
        let words = vec![WordSpan::new("postgress", 0.2)];
        let corrections = correct_spans(
            &words,
            &ctx(&[], AppKind::General),
            &CorrectionThresholds::default(),
        );
        assert!(corrections.is_empty());

        let corrections_unrelated = correct_spans(
            &words,
            &ctx(&["Kubernetes"], AppKind::General),
            &CorrectionThresholds::default(),
        );
        assert!(
            corrections_unrelated.is_empty(),
            "no matching term should mean no correction"
        );
    }

    #[test]
    fn multi_word_span_matches_multi_word_bias_term() {
        let words = vec![WordSpan::new("sarah", 0.3), WordSpan::new("conor", 0.3)];
        let corrections = correct_spans(
            &words,
            &ctx(&["Sara Connor"], AppKind::General),
            &CorrectionThresholds::default(),
        );
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].span_len, 2);
        assert_eq!(corrections[0].replacement, "Sara Connor");
        assert_eq!(
            apply_corrections(&words, &corrections),
            vec!["Sara Connor".to_string()]
        );
    }

    #[test]
    fn mixed_confidence_span_does_not_rewrite_the_confident_word() {
        // MINOR regression: ASR emits "Sarah" at 0.95 (confident) and
        // "conor" at 0.30 (uncertain). The old min-confidence gate let the
        // uncertain neighbour drag the whole span into a rewrite even
        // though the ASR was 95% sure about "Sarah". The span must now be
        // ineligible entirely (no correction at all), because "Sarah" alone
        // is above the ceiling and there's no matching single-word term for
        // "conor" by itself.
        let words = vec![WordSpan::new("sarah", 0.95), WordSpan::new("conor", 0.30)];
        let corrections = correct_spans(
            &words,
            &ctx(&["Sara Connor"], AppKind::General),
            &CorrectionThresholds::default(),
        );
        assert!(
            corrections.is_empty(),
            "a span containing a high-confidence word must not be rewritten: {corrections:?}"
        );
    }

    #[test]
    fn multi_word_term_preferred_over_single_word_coincidental_match() {
        // "conor" alone might phonetically resemble some single-word term,
        // but the two-word span should win when both are eligible.
        let words = vec![WordSpan::new("sarah", 0.3), WordSpan::new("conor", 0.3)];
        let corrections = correct_spans(
            &words,
            &ctx(&["Sara Connor", "Conner"], AppKind::General),
            &CorrectionThresholds::default(),
        );
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].span_len, 2);
        assert_eq!(corrections[0].replacement, "Sara Connor");
    }

    #[test]
    fn prev_terms_are_used_as_lower_weight_fallback() {
        let mut c = ctx(&[], AppKind::General);
        c.prev_terms = vec!["Postgres".to_string()];
        let words = vec![WordSpan::new("postgress", 0.3)];
        let corrections = correct_spans(&words, &c, &CorrectionThresholds::default());
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].replacement, "Postgres");
    }

    #[test]
    fn empty_words_and_empty_context_are_handled_without_panicking() {
        assert!(correct_spans(
            &[],
            &ctx(&["Postgres"], AppKind::General),
            &CorrectionThresholds::default()
        )
        .is_empty());
        let words = vec![WordSpan::new("postgress", 0.1)];
        assert!(correct_spans(
            &words,
            &ctx(&[], AppKind::General),
            &CorrectionThresholds::default()
        )
        .is_empty());
    }
}
