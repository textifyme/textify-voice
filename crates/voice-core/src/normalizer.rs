//! Deterministic normalizer (SPEC.md §3.4 step 1): "the deterministic
//! normalizer always runs (replacements, spacing, bias layer 2)." Runs
//! after `LocalAsr::finalize()`, before the format-gate heuristic decides
//! whether an LLM pass may additionally run.
//!
//! Two kinds of "replacements" happen here, in order:
//! 1. **Literal phrase rules** — fixed multi-word substitutions like
//!    `"cursor dot ai"` → `"cursor.ai"` (SPEC §3.3 layer 2's own example of
//!    "literal rules," distinct from phonetic matching: there's no vowel to
//!    phonetically match against a `.`).
//! 2. **Bias layer 2** — phonetic post-correction against `BiasContext`
//!    ([`crate::bias`]), applied to whatever the literal-rule pass left
//!    alone. Literal-rule replacements are re-emitted at confidence 1.0 so
//!    layer 2 never re-touches them.
//!
//! **Raw paste for AI/coding apps** (SPEC.md line 228, V1.4: "app-kind
//! detection forces raw paste in AI/coding apps"). `format_gate.rs` only
//! suppresses the *separate LLM formatting pass* — it says nothing about
//! this deterministic normalizer, which runs unconditionally before the
//! gate is even consulted. So `ctx.app_kind` is checked here too: when
//! [`AppKind::is_ai_or_coding`] is true, `normalize()` skips the literal
//! rules and bias layer 2 (an editorializing/dictionary-correction pass has
//! no business rewriting a shell command or second-guessing an LLM prompt's
//! wording), and joins words with a single space and no other
//! whitespace/punctuation cleanup beyond what [`verbatim_words`] *may* do —
//! see the next section for when it does and doesn't. Non-AI/coding apps
//! (`General`, `Browser`, `Messaging`, `Email`, `Unknown`, ...) are
//! unaffected and get the full pipeline below.
//!
//! **Two different questions live under "raw paste," and they don't share
//! an answer across all three AI/coding kinds.** (1) "Should our own
//! bias/formatting transforms run?" — SPEC V1.4's raw-paste rule, and the
//! answer is no for all of `Ai`/`Code`/`Terminal`
//! ([`AppKind::is_ai_or_coding`]). (2) "Should whisper's *own* added
//! sentence formatting be undone too?" — a completely separate question
//! whose answer depends on what kind of content is actually being
//! dictated, not on whether it's an "AI/coding" surface. `Terminal` and
//! `Code` mean the dictated content is literally shell/code syntax, where a
//! stray capital or trailing period breaks it. `Ai` means an LLM chat
//! prompt — ordinary prose sent to a model, not code — where a capital
//! letter starting a sentence and a period ending it are exactly what the
//! user wants. Answering (2) "yes" for `Ai` was the V1.4 regression this
//! module now avoids: it turned `"Paris is beautiful in the spring."` into
//! `"paris is beautiful in the spring"`. [`AppKind::wants_shell_verbatim`]
//! is question (2)'s answer — `Terminal`/`Code` only, never `Ai` — and
//! `normalize()` branches on it separately from `is_ai_or_coding()`.
//!
//! **"Raw" means what the user said, not what whisper.cpp printed —
//! but only where the user *meant* shell/code syntax.** whisper.cpp itself
//! applies sentence-style formatting to its output (capitalizes the first
//! word, appends sentence-final punctuation) even though nothing in the
//! audio "said" a capital letter or a period. Simply skipping *our*
//! transforms therefore was not enough to make `--app-kind terminal` raw
//! paste verbatim: `"git status"` decoded by whisper as the two
//! word-tokens `"Git"`, `"status."` and joined untouched still comes out
//! `"Git status."` — capitalized and punctuated, byte-identical to prose
//! mode, and a broken shell command. [`verbatim_words`] undoes exactly
//! those two whisper-isms — trailing sentence-final punctuation on the last
//! word, and a capitalized first word — while leaving everything a coder
//! actually said alone. It runs only when
//! [`AppKind::wants_shell_verbatim`] is true (`Terminal`/`Code`); for `Ai`,
//! `normalize()` joins whisper's tokens as-is, sentence formatting intact,
//! same as prose mode:
//!   * Only the **last** word is checked for trailing punctuation, and only
//!     a *single* `.`/`!`/`?`/`,`/`;`/`:` immediately after an alphanumeric
//!     character is stripped. `"cd .."`'s last word is `".."` — two
//!     punctuation characters with no alphanumeric before the final one — so
//!     it is left alone. `"foo.rs"` and `"a.b.c"` don't end in punctuation
//!     at all (they end in a letter), so they're untouched regardless of
//!     position.
//!   * Only the **first** word's leading character is de-capitalized, and
//!     only when it isn't the pronoun `"I"` and doesn't match a bias/
//!     dictionary term (`ctx.terms` — see [`is_leading_bias_term`]):
//!     dictating `"Docker compose up"` with `"Docker"` in the user's
//!     dictionary keeps the capital `D`, but plain `"Git status"` lowercases
//!     to `"git status"`. `ctx.prev_terms` is deliberately *not* consulted
//!     here even though bias layer 2 treats it as a (lower-weight) term
//!     source: in this codebase `prev_terms` is literally the previous
//!     utterance's own output words (see `voice-cli`'s `dictate.rs`), not a
//!     curated proper-noun list, so treating it as one would preserve
//!     capitals on arbitrary repeated common words instead of only genuine
//!     names. Skipping it is the conservative direction (it only means
//!     *fewer* preserved capitals, never a wrongly-lowered real proper
//!     noun), consistent with "prefer under-transforming."
//!   * Literal phrase rules (`"cursor dot ai"` → `"cursor.ai"`) still do
//!     **not** run in raw mode — that decision is unchanged by this fix.
//!     Whether a coder dictating `"dot rs"` wants literal `".rs"` is a
//!     genuinely separate design question from "undo whisper's sentence
//!     formatting" (this fix's actual scope), and flipping it on would
//!     silently reinterpret words the user said, which is a different kind
//!     of risk than stripping ASR-added formatting the user never said at
//!     all. Left off, conservatively, pending its own decision.
//!   * Nothing else changes: casing/spacing in the middle of the utterance
//!     is passed through exactly as whisper produced it. [`verbatim_words`]
//!     is called only when [`AppKind::wants_shell_verbatim`] is true. For
//!     every other kind — non-AI/coding apps (`General`, `Browser`,
//!     `Messaging`, `Email`, `Unknown`, ...), which get the full pipeline
//!     below with `capitalize_first`/`join_and_clean` unchanged, *and*
//!     `Ai`, which still takes the raw-paste branch but joins whisper's
//!     tokens untouched via `join_raw` — whisper's own sentence formatting
//!     survives exactly as decoded.

use crate::bias::{apply_corrections, correct_spans, Correction, CorrectionThresholds, WordSpan};
use crate::BiasContext;

/// A fixed multi-word→literal substitution, matched case-insensitively on
/// whole spoken words.
#[derive(Debug, Clone, PartialEq)]
pub struct LiteralRule {
    pub spoken: Vec<String>,
    pub replacement: String,
}

impl LiteralRule {
    #[must_use]
    pub fn new(spoken: &[&str], replacement: impl Into<String>) -> Self {
        Self {
            spoken: spoken.iter().map(|s| (*s).to_string()).collect(),
            replacement: replacement.into(),
        }
    }
}

/// The built-in literal rules. SPEC §3.3 names `"cursor dot ai"` →
/// `"cursor.ai"` explicitly as the layer-2 example; `"dot com"` → `".com"`
/// is included as a second, more general instance of the same mechanism.
#[must_use]
pub fn default_literal_rules() -> Vec<LiteralRule> {
    vec![
        LiteralRule::new(&["cursor", "dot", "ai"], "cursor.ai"),
        LiteralRule::new(&["dot", "com"], ".com"),
    ]
}

fn apply_literal_rules(words: &[WordSpan], rules: &[LiteralRule]) -> Vec<WordSpan> {
    let mut sorted_rules: Vec<&LiteralRule> =
        rules.iter().filter(|r| !r.spoken.is_empty()).collect();
    // Longest spoken sequence first, so a longer rule always gets first
    // refusal at a given position over a shorter one that happens to be a
    // prefix of it.
    sorted_rules.sort_by_key(|r| std::cmp::Reverse(r.spoken.len()));

    let mut out = Vec::with_capacity(words.len());
    let mut i = 0usize;
    'outer: while i < words.len() {
        for rule in &sorted_rules {
            let len = rule.spoken.len();
            if i + len > words.len() {
                continue;
            }
            let is_match = words[i..i + len]
                .iter()
                .zip(rule.spoken.iter())
                .all(|(w, s)| w.text.eq_ignore_ascii_case(s));
            if is_match {
                // Confidence 1.0: a literal rule is a deterministic
                // decision, not a guess — bias layer 2 must not reconsider
                // it as a low-confidence span.
                out.push(WordSpan::new(rule.replacement.clone(), 1.0));
                i += len;
                continue 'outer;
            }
        }
        out.push(words[i].clone());
        i += 1;
    }
    out
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Join corrected word tokens into final text: single spaces between words,
/// no space before sentence punctuation, collapsed whitespace, first
/// character capitalized (dictation convention — a full style pass is the
/// local formatter's job, §3.4 step 3, not this deterministic step).
fn join_and_clean(words: &[String]) -> String {
    let mut out = String::new();
    for w in words {
        let trimmed = w.trim();
        if trimmed.is_empty() {
            continue;
        }
        let starts_with_punct = matches!(
            trimmed.chars().next(),
            Some('.' | ',' | '!' | '?' | ';' | ':')
        );
        if !out.is_empty() && !starts_with_punct {
            out.push(' ');
        }
        out.push_str(trimmed);
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    capitalize_first(&collapsed)
}

/// Result of running the deterministic normalizer over one utterance's
/// words: the final cleaned text, plus the bias-layer-2 corrections applied
/// (exposed for telemetry/debugging — SPEC §6 forbids logging transcript
/// *content*, but a caller may still want the correction *count*).
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizeResult {
    pub text: String,
    pub corrections: Vec<Correction>,
}

/// Join word tokens verbatim with a single space, applying none of
/// `join_and_clean`'s transforms (no capitalization, no punctuation-aware
/// spacing, no whitespace collapsing beyond what a plain space-join already
/// gives). This is the AI/coding-app raw-paste path: SPEC.md line 228 (V1.4)
/// requires the dictated text to survive verbatim, because `capitalize_first`
/// alone is enough to turn a working shell command into a broken one
/// (`git status` -> `Git status`).
fn join_raw(words: &[String]) -> String {
    words
        .iter()
        .map(|w| w.trim())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Strip a single ASR-added trailing sentence-final punctuation character
/// from one word, if present. Only strips when the character immediately
/// before it is alphanumeric — so a punctuation-only tail (`".."`, `"..."`)
/// is left alone, since there's no way to tell that apart from genuine
/// shell-command content at this layer, and the spec's example (`cd ..`)
/// is exactly that case. A word that is itself a single bare punctuation
/// character (e.g. a stray `"."` token) is dropped entirely: with no
/// alphanumeric character in the word at all, it cannot be shell content
/// standing alone, and it can only be whisper's sentence-final mark.
fn strip_trailing_sentence_punct(word: &str) -> String {
    let chars: Vec<char> = word.chars().collect();
    let Some(&last) = chars.last() else {
        return word.to_string();
    };
    if !matches!(last, '.' | '!' | '?' | ',' | ';' | ':') {
        return word.to_string();
    }
    if chars.len() == 1 {
        return String::new();
    }
    let prev = chars[chars.len() - 2];
    if prev.is_alphanumeric() {
        chars[..chars.len() - 1].iter().collect()
    } else {
        word.to_string()
    }
}

/// Does `word` (case-insensitively) match the leading spoken word of a
/// bias/dictionary term in `ctx.terms`? Used to decide whether a capitalized
/// first word is a genuine proper noun (`"Docker"`) rather than whisper's
/// ordinary sentence-initial capitalization (`"Git"` in `"Git status"`).
/// Deliberately checks `ctx.terms` only, not `ctx.prev_terms` — see the
/// module doc comment for why the latter is not a reliable proper-noun
/// signal in this codebase.
fn is_leading_bias_term(word: &str, ctx: &BiasContext) -> bool {
    ctx.terms.iter().any(|t| {
        t.text
            .split_whitespace()
            .next()
            .is_some_and(|first| first.eq_ignore_ascii_case(word))
    })
}

/// De-capitalize a leading capital letter that looks like whisper's
/// sentence-initial formatting rather than something the user actually
/// meant capitalized. Conservative by construction: only the pronoun `"I"`
/// and a bias/dictionary-term match are treated as "leave it capitalized";
/// everything else with an uppercase first character gets only its first
/// character lowered (the rest of the word is untouched).
fn decapitalize_if_not_proper_noun(word: &str, ctx: &BiasContext) -> String {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return word.to_string();
    };
    if !first.is_uppercase() {
        return word.to_string();
    }
    if word.eq_ignore_ascii_case("i") || is_leading_bias_term(word, ctx) {
        return word.to_string();
    }
    let mut out: String = first.to_lowercase().collect();
    out.push_str(chars.as_str());
    out
}

/// Undo whisper.cpp's sentence-style formatting on a raw-paste transcript
/// whose content is shell/code syntax (`ctx.app_kind.wants_shell_verbatim()`
/// — `Terminal`/`Code`, never `Ai`): trailing sentence-final punctuation on
/// the last word, and sentence-initial capitalization on the first word
/// (unless it's `"I"` or a bias/dictionary term). See the module doc
/// comment for the full rationale and the specific cases this is
/// conservative about.
fn verbatim_words(words: &[WordSpan], ctx: &BiasContext) -> Vec<String> {
    let mut raw: Vec<String> = words.iter().map(|w| w.text.clone()).collect();
    if let Some(last) = raw.last_mut() {
        *last = strip_trailing_sentence_punct(last);
    }
    if let Some(first) = raw.first_mut() {
        *first = decapitalize_if_not_proper_noun(first, ctx);
    }
    raw
}

/// Run the full deterministic normalizer: literal rules, then bias layer 2,
/// then spacing/punctuation cleanup — UNLESS `ctx.app_kind` is an AI/coding
/// app (SPEC.md line 228, V1.4 "app-kind detection forces raw paste in
/// AI/coding apps"), in which case none of those transforms run and the
/// words are passed through as dictated. Within that raw-paste branch, a
/// *second*, narrower check (`wants_shell_verbatim`) decides whether
/// whisper's own sentence formatting is additionally undone — see the
/// module-level doc comment for why raw mode applies zero editorializing
/// transforms to all three AI/coding kinds, but the sentence-formatting
/// undo to only `Terminal`/`Code`.
#[must_use]
pub fn normalize(
    words: &[WordSpan],
    ctx: &BiasContext,
    literal_rules: &[LiteralRule],
    thresholds: &CorrectionThresholds,
) -> NormalizeResult {
    if ctx.app_kind.is_ai_or_coding() {
        // Two separate decisions live inside this branch — see the module
        // doc comment and `AppKind::wants_shell_verbatim`'s doc comment for
        // the full rationale:
        //   1. Skip literal rules + bias layer 2 (raw paste, SPEC.md V1.4).
        //      True for every `is_ai_or_coding()` kind, `Ai` included.
        //   2. Undo whisper's own sentence-style formatting (leading
        //      capital, trailing sentence punctuation). Only correct where
        //      the dictated content IS shell/code syntax
        //      (`wants_shell_verbatim`) — `Terminal`/`Code`. `Ai` is a chat
        //      prompt: prose, not code, so its capitalization and
        //      punctuation are exactly what the user wants and must be left
        //      alone, same as prose mode.
        let raw_words: Vec<String> = if ctx.app_kind.wants_shell_verbatim() {
            verbatim_words(words, ctx)
        } else {
            words.iter().map(|w| w.text.clone()).collect()
        };
        return NormalizeResult {
            text: join_raw(&raw_words),
            corrections: Vec::new(),
        };
    }
    let after_literal = apply_literal_rules(words, literal_rules);
    let corrections = correct_spans(&after_literal, ctx, thresholds);
    let corrected_words = apply_corrections(&after_literal, &corrections);
    NormalizeResult {
        text: join_and_clean(&corrected_words),
        corrections,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppKind, BiasTerm};

    fn ctx(terms: &[&str]) -> BiasContext {
        BiasContext {
            terms: terms.iter().map(|t| BiasTerm::new(*t)).collect(),
            app_kind: AppKind::General,
            prev_terms: Vec::new(),
        }
    }

    #[test]
    fn spec_example_cursor_dot_ai_literal_rule() {
        let words = vec![
            WordSpan::new("check", 0.99),
            WordSpan::new("out", 0.99),
            WordSpan::new("cursor", 0.99),
            WordSpan::new("dot", 0.99),
            WordSpan::new("ai", 0.99),
        ];
        let result = normalize(
            &words,
            &ctx(&[]),
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "Check out cursor.ai");
    }

    #[test]
    fn bias_layer_2_runs_inside_normalize() {
        let words = vec![
            WordSpan::new("i", 0.99),
            WordSpan::new("use", 0.99),
            WordSpan::new("postgress", 0.35),
        ];
        let result = normalize(
            &words,
            &ctx(&["Postgres"]),
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "I use Postgres");
        assert_eq!(result.corrections.len(), 1);
    }

    #[test]
    fn literal_rule_output_is_not_reconsidered_by_bias_layer_2() {
        // A pathological bias term that happens to phonetically resemble
        // "cursor.ai" must not un-do the literal rule's decision.
        let words = vec![
            WordSpan::new("cursor", 0.99),
            WordSpan::new("dot", 0.99),
            WordSpan::new("ai", 0.99),
        ];
        let result = normalize(
            &words,
            &ctx(&["Curser Eye"]), // deliberately close spelling
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "Cursor.ai");
        assert!(result.corrections.is_empty());
    }

    #[test]
    fn spacing_and_punctuation_cleanup() {
        let words = vec![
            WordSpan::new("hello", 0.99),
            WordSpan::new(",", 0.99),
            WordSpan::new("world", 0.99),
            WordSpan::new(".", 0.99),
        ];
        let result = normalize(&words, &ctx(&[]), &[], &CorrectionThresholds::default());
        assert_eq!(result.text, "Hello, world.");
    }

    #[test]
    fn ai_coding_app_kind_forces_raw_paste_git_status() {
        // SPEC line 228 (V1.4): "app-kind detection forces raw paste in
        // AI/coding apps." `capitalize_first` alone breaks this: "git
        // status" must NOT become "Git status" in a Terminal.
        let words = vec![WordSpan::new("git", 0.99), WordSpan::new("status", 0.99)];
        let mut c = ctx(&[]);
        c.app_kind = AppKind::Terminal;
        let result = normalize(&words, &c, &default_literal_rules(), &CorrectionThresholds::default());
        assert_eq!(result.text, "git status");
    }

    #[test]
    fn ai_coding_app_kind_forces_raw_paste_ls_la() {
        let words = vec![WordSpan::new("ls", 0.99), WordSpan::new("-la", 0.99)];
        let mut c = ctx(&[]);
        c.app_kind = AppKind::Terminal;
        let result = normalize(&words, &c, &default_literal_rules(), &CorrectionThresholds::default());
        assert_eq!(result.text, "ls -la");
    }

    #[test]
    fn ai_app_kind_also_forces_raw_paste() {
        let words = vec![WordSpan::new("print", 0.99), WordSpan::new("hello", 0.99)];
        let mut c = ctx(&[]);
        c.app_kind = AppKind::Ai;
        let result = normalize(&words, &c, &default_literal_rules(), &CorrectionThresholds::default());
        assert_eq!(result.text, "print hello");
    }

    #[test]
    fn code_app_kind_also_forces_raw_paste_and_skips_literal_rules() {
        // Even the "cursor dot ai" literal rule must not fire in raw mode.
        // Whether a coder dictating symbol words like "dot rs" wants them
        // converted is a separate design question from undoing whisper's
        // own sentence-formatting (see the module doc comment) — literal
        // rules stay off in raw mode, conservatively, independent of that.
        let words = vec![
            WordSpan::new("cursor", 0.99),
            WordSpan::new("dot", 0.99),
            WordSpan::new("ai", 0.99),
        ];
        let mut c = ctx(&[]);
        c.app_kind = AppKind::Code;
        let result = normalize(&words, &c, &default_literal_rules(), &CorrectionThresholds::default());
        assert_eq!(result.text, "cursor dot ai");
    }

    #[test]
    fn prose_app_still_normalizes_with_ai_coding_app_present_in_other_tests() {
        // Proves the AI/coding raw-paste path is app-kind-gated, not a
        // global behavior change: General apps still get capitalization,
        // literal rules, and bias correction.
        let words = vec![
            WordSpan::new("git", 0.99),
            WordSpan::new("status", 0.99),
        ];
        let result = normalize(
            &words,
            &ctx(&[]), // AppKind::General
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "Git status");
    }

    #[test]
    fn empty_transcript_normalizes_to_empty_string() {
        let result = normalize(
            &[],
            &ctx(&["Postgres"]),
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "");
        assert!(result.corrections.is_empty());
    }

    // -- Verbatim transform: undoing whisper's own sentence formatting in
    // AI/coding app kinds, using word tokens shaped the way whisper.cpp
    // actually emits them (capitalized first word, sentence-final
    // punctuation glued onto the last word's text) rather than the
    // already-clean lowercase/unpunctuated tokens the tests above use. --

    fn code_ctx(terms: &[&str]) -> BiasContext {
        let mut c = ctx(terms);
        c.app_kind = AppKind::Code;
        c
    }

    #[test]
    fn headline_case_git_status_is_truly_verbatim() {
        // The reproduced blocker: whisper decodes "git status" as word
        // tokens "Git", "status." (capitalized, sentence-final period
        // glued onto the last word). Naive raw-paste (skip only OUR
        // transforms) yields "Git status." -- byte-identical to prose mode.
        // The fix must strip both.
        let words = vec![WordSpan::new("Git", 0.99), WordSpan::new("status.", 0.99)];
        let result = normalize(
            &words,
            &code_ctx(&[]),
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "git status");
    }

    #[test]
    fn verbatim_ls_la_with_whisper_shaped_tokens() {
        let words = vec![WordSpan::new("Ls", 0.99), WordSpan::new("-la.", 0.99)];
        let result = normalize(
            &words,
            &code_ctx(&[]),
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "ls -la");
    }

    #[test]
    fn verbatim_cd_dotdot_survives_trailing_punct_check() {
        // "cd .." must NOT become "cd ." or "cd": the last word is "..",
        // two punctuation characters with no alphanumeric before the final
        // one, so it is content, not an ASR-added sentence-final mark.
        let words = vec![WordSpan::new("Cd", 0.99), WordSpan::new("..", 0.99)];
        let result = normalize(
            &words,
            &code_ctx(&[]),
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "cd ..");
    }

    #[test]
    fn verbatim_npm_run_dev_strips_trailing_period_only() {
        let words = vec![
            WordSpan::new("Npm", 0.99),
            WordSpan::new("run", 0.99),
            WordSpan::new("dev.", 0.99),
        ];
        let result = normalize(
            &words,
            &code_ctx(&[]),
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "npm run dev");
    }

    #[test]
    fn verbatim_cargo_test_workspace_flag_untouched() {
        let words = vec![
            WordSpan::new("Cargo", 0.99),
            WordSpan::new("test", 0.99),
            WordSpan::new("--workspace.", 0.99),
        ];
        let result = normalize(
            &words,
            &code_ctx(&[]),
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "cargo test --workspace");
    }

    #[test]
    fn verbatim_preserves_capital_for_dictionary_proper_noun() {
        // "Docker" is a bias/dictionary term, so its capital must survive
        // even though it's the leading word -- unlike plain "Git".
        let words = vec![
            WordSpan::new("Docker", 0.99),
            WordSpan::new("compose", 0.99),
            WordSpan::new("up.", 0.99),
        ];
        let result = normalize(
            &words,
            &code_ctx(&["Docker"]),
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "Docker compose up");
    }

    #[test]
    fn verbatim_preserves_capital_i_pronoun() {
        let words = vec![
            WordSpan::new("I", 0.99),
            WordSpan::new("formatted", 0.99),
            WordSpan::new("the", 0.99),
            WordSpan::new("file.", 0.99),
        ];
        let result = normalize(
            &words,
            &code_ctx(&[]),
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "I formatted the file");
    }

    #[test]
    fn verbatim_internal_periods_in_filenames_and_paths_are_never_touched() {
        // "foo.rs" and "a.b.c" don't end in punctuation -- they end in a
        // letter -- so the trailing-punct check never even looks at them,
        // regardless of where they sit in the utterance.
        let words = vec![
            WordSpan::new("Open", 0.99),
            WordSpan::new("foo.rs", 0.99),
            WordSpan::new("and", 0.99),
            WordSpan::new("a.b.c", 0.99),
        ];
        let result = normalize(
            &words,
            &code_ctx(&[]),
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "open foo.rs and a.b.c");
    }

    #[test]
    fn verbatim_bare_trailing_punctuation_token_is_dropped() {
        // If whisper ever emits the sentence-final mark as its own token
        // (no leading space merge), it's still ASR formatting, not content,
        // and should be dropped rather than left dangling.
        let words = vec![
            WordSpan::new("Git", 0.99),
            WordSpan::new("status", 0.99),
            WordSpan::new(".", 0.99),
        ];
        let result = normalize(
            &words,
            &code_ctx(&[]),
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "git status");
    }

    #[test]
    fn prose_mode_capitalization_and_punctuation_are_unaffected_by_verbatim_transform() {
        // Regression guard: the verbatim transform must be strictly
        // app-kind-gated. Three ordinary prose sentences, run through
        // General app_kind, must keep whisper's sentence-final punctuation
        // and capitalization exactly as before this fix.
        let cases: &[(&[&str], &str)] = &[
            (&["Hello", "world."], "Hello world."),
            (&["The", "quick", "brown", "fox", "jumps."], "The quick brown fox jumps."),
            (&["I", "am", "testing", "prose", "mode."], "I am testing prose mode."),
        ];
        for (tokens, expected) in cases {
            let words: Vec<WordSpan> =
                tokens.iter().map(|t| WordSpan::new(*t, 0.99)).collect();
            let result = normalize(
                &words,
                &ctx(&[]), // AppKind::General
                &default_literal_rules(),
                &CorrectionThresholds::default(),
            );
            assert_eq!(&result.text, expected);
        }
    }

    // -- fix:verbatim-scope regression: AppKind::Ai is raw-paste (skips
    // literal rules + bias layer 2, per SPEC V1.4) but must NOT run
    // verbatim_words's sentence-formatting undo, because Ai content is
    // prose (an LLM chat prompt), not shell/code syntax. See the module
    // doc comment and `AppKind::wants_shell_verbatim`. --

    fn ai_ctx(terms: &[&str]) -> BiasContext {
        let mut c = ctx(terms);
        c.app_kind = AppKind::Ai;
        c
    }

    #[test]
    fn ai_app_kind_preserves_sentence_capitalization_and_period() {
        // The reproduced blocker, verbatim: whisper decodes this prose
        // sentence with a leading capital and a trailing period, both of
        // which the user wants (it's an LLM chat prompt, not a shell
        // command). Before the fix, verbatim_words ran here and produced
        // "paris is beautiful in the spring" — silently damaging the user's
        // dictated prose.
        let words = vec![
            WordSpan::new("Paris", 0.99),
            WordSpan::new("is", 0.99),
            WordSpan::new("beautiful", 0.99),
            WordSpan::new("in", 0.99),
            WordSpan::new("the", 0.99),
            WordSpan::new("spring.", 0.99),
        ];
        let result = normalize(
            &words,
            &ai_ctx(&[]),
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "Paris is beautiful in the spring.");
    }

    #[test]
    fn ai_app_kind_preserves_capitalized_abbreviation_at_sentence_start() {
        // The second reproduced blocker: before the fix, only the leading
        // character was lowered ("Dr." -> "dr."), which is exactly what
        // decapitalize_if_not_proper_noun does -- confirming this damage
        // came from verbatim_words running in Ai context, not from some
        // other transform.
        let words = vec![
            WordSpan::new("Dr.", 0.99),
            WordSpan::new("Smith", 0.99),
            WordSpan::new("visited", 0.99),
            WordSpan::new("Paris.", 0.99),
        ];
        let result = normalize(
            &words,
            &ai_ctx(&[]),
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "Dr. Smith visited Paris.");
    }

    #[test]
    fn ai_app_kind_preserves_capital_i_pronoun_and_skips_bias_correction() {
        // "I" must stay capitalized (as in prose mode), and — because Ai is
        // still raw-paste — bias layer 2 / literal rules must not run even
        // though the words are joined untouched (no verbatim_words call).
        let words = vec![
            WordSpan::new("I", 0.99),
            WordSpan::new("think", 0.99),
            WordSpan::new("we", 0.99),
            WordSpan::new("should", 0.99),
            WordSpan::new("ship", 0.99),
        ];
        let result = normalize(
            &words,
            &ai_ctx(&[]),
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "I think we should ship");
    }

    #[test]
    fn ai_app_kind_still_skips_literal_rules_and_bias_layer_2() {
        // Confirms the raw-paste half of is_ai_or_coding is untouched by
        // this fix: Ai still gets zero editorializing, only the
        // shell-verbatim undo is now scoped away from it.
        let words = vec![
            WordSpan::new("cursor", 0.99),
            WordSpan::new("dot", 0.99),
            WordSpan::new("ai", 0.99),
        ];
        let result = normalize(
            &words,
            &ai_ctx(&[]),
            &default_literal_rules(),
            &CorrectionThresholds::default(),
        );
        assert_eq!(result.text, "cursor dot ai");
    }
}
