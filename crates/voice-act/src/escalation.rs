//! Tier escalation rules. COMMANDS-SPEC.md §3.4/§3.5: tier is declared on
//! the *schema*, but a handful of families escalate the *effective* tier at
//! resolve time based on the bound target's state -- this module is the
//! single place that logic lives, so `resolve()` implementations and tests
//! share one source of truth.
//!
//! ## Governing principle, and why it used to fail
//!
//! COMMANDS-SPEC.md §3.4: "destructive-labeled controls (delete/send/pay
//! lexicon) -> T2". The principle a prior audit stated explicitly: if a
//! label can *bind* to one of these spoken words, it must *escalate*.
//!
//! The prior implementation of that principle was a second matcher: its own
//! invisible-character list, its own confusable table, and a hand-tuned
//! `near_miss` rule (edit distance <= 1) chosen specifically to admit the
//! perturbations that audit had been shown while excluding `"Sendai"` (two
//! edits from `"send"`) as a false positive. Two more audits then found
//! inputs that same rule didn't cover -- LRM/RLM bidi marks, and
//! `"d3l3t3"` (edit distance *3* from `"delete"`, outside the `<=1` rule
//! entirely, yet the *real* binder already matches it at exactly its 0.5
//! floor -- see `disambiguate::BINDING_FLOOR`). A second, independently
//! tuned matcher will always eventually drift from the one that actually
//! governs binding, because nothing keeps the two in sync.
//!
//! [`is_destructive_label`] no longer has its own matcher. It normalizes
//! and scores each candidate word/phrase in the label against the
//! destructive lexicon using [`crate::disambiguate::label_similarity`] --
//! the *exact* function `mock.rs`'s real target binding calls -- at
//! [`crate::disambiguate::BINDING_FLOOR`], the *exact* floor real binding
//! uses. One matcher, one threshold, consulted twice: any perturbation that
//! would make a label bind to a destructive spoken word necessarily crosses
//! this same check, by construction, without either side having to
//! anticipate the perturbation in advance.
//!
//! ### The `"Sendai"` tradeoff
//!
//! This does cost the false-positive guard the old `near_miss` rule bought:
//! `"send"` is 2 insertions / 6 characters from `"sendai"`, which scores
//! `0.667` -- above the 0.5 floor. Since the real binder's own
//! `label_similarity` is the thing being consulted, a label that would
//! genuinely bind to `"send"` under the real binder now escalates too, even
//! when that label is an unrelated word that merely happens to be short and
//! close. Given the choice between reintroducing a bespoke,
//! independently-tuned exception (the exact failure mode two audits already
//! exploited) and accepting an occasional extra T2 confirmation on a
//! coincidentally-close benign label, this module chooses the latter: a
//! needless confirm costs the user one word, a missed one fires a
//! destructive action. See the `previously_benign_labels_now_escalate_*`
//! tests below for exactly what this costs.

use crate::disambiguate::{label_similarity, normalize_for_match, BINDING_FLOOR};
use crate::schema::{ActionSchema, Tier};

/// COMMANDS-SPEC.md §3.4 destructive-label lexicon: single words. Matched
/// against each word-token of a label (see [`tokens`]) via
/// [`label_similarity`] at [`BINDING_FLOOR`] -- not exact/substring
/// equality -- so any perturbation of these words that would bind to the
/// spoken word itself also matches here.
///
/// ## Coverage gap this list closes (adversarial audit, minor finding)
///
/// COMMANDS-SPEC §3.4 names delete/send/pay/remove/discard/purchase/submit/
/// buy/erase/wipe/destroy as *illustrative* examples, but §3.5's governing
/// rule is that tier scales to **blast radius**, not to membership in that
/// literal list. `"click Uninstall"` and `"click Format Disk"` used to
/// resolve at T1 (EXECUTE_AND_ANNOUNCE) purely because "uninstall" and
/// "format" happened not to be spelled out in §3.4 -- a live executor would
/// have auto-clicked a genuinely irreversible control. The words below are
/// added by *category* of blast radius, not as a two-word patch for the two
/// reported cases, each vetted against a realistic benign-label corpus (see
/// `escalation::benign_rate_measurement` tests) and dropped if it produced
/// an unacceptable false-escalation collision:
///
/// - **destructive install/system operations**: `uninstall`, `format`.
///   (`reset`/`restore`/bare `wipe`-adjacent "factory reset" phrasing were
///   considered too -- see the dropped-words note below.)
/// - **irreversible data operations**: `overwrite`, `purge`.
/// - **account/security operations**: `deactivate`, `deauthorize`,
///   `disconnect`, `terminate`.
/// - **publishing/outbound operations**: `publish`, `transfer`, `withdraw`.
///
/// ### Words considered and dropped (unacceptable benign collision)
///
/// Measured against a 108-label realistic-UI-button corpus (see the tests
/// below), each of these produced a literal-word or high-score fuzzy hit
/// against a common, low-blast-radius, high-frequency control, which the
/// baseline's already-accepted tradeoffs (`"Play"~"pay"`, `"Move"~"remove"`)
/// do not have the same magnitude of:
///
/// - `reset` / `clear` / `drop` -- these are also the generic qualifier in
///   ubiquitous, trivially-reversible micro-actions (`"Reset Zoom"`,
///   `"Reset View"`, `"Clear Search"`, `"Clear Filters"`, `"Crop"`,
///   `"Stop"`). Because matching is per-token (see `tokens`'s doc comment),
///   the bare word alone is enough to flag the whole label regardless of
///   what it's qualifying -- every zoom/crop/search-clear action in the app
///   would demand a spoken confirmation. The destructive *senses* of these
///   words are covered instead as phrases below (`"factory reset"`,
///   `"clear all data"`, `"drop table"`, `"drop database"`).
/// - `restore` -- usually names a *recovery* action (`"Restore Purchase"`,
///   `"Restore Backup"`), the opposite of destructive; only its
///   settings-loss sense is covered, as the `"restore defaults"` phrase.
/// - `truncate` -- scores 0.5 (the floor, exactly) against `"Rotate"`, a
///   near-universal, trivially-reversible control in every photo/PDF/
///   document tool. The word is a niche technical term; not worth that
///   collision.
/// - `revoke` -- scores 0.5 against three separate common, zero-consequence
///   controls simultaneously (`"Previous"`, `"Reload"`, `"Redo"`). Its
///   destructive sense is covered as the `"revoke access"` phrase instead.
/// - `unlink` -- scores 0.667 against the bare `"Link"` button (a *connect*
///   action, i.e. the opposite direction of risk from unlinking). Coverage
///   of the disconnect/unlink family is left to `disconnect` (clean) and
///   `deauthorize` (clean).
/// - `post` -- scores >=0.5 against `"Sort"`, `"Paste"`, `"Export"`,
///   `"Import"`, plus a literal hit on `"Post Comment"` (a low-blast-radius,
///   editable/deletable action). `publish` (clean) already covers the
///   high-blast-radius publishing sense this category was after.
/// - `share` -- scores >=0.5 against `"Save"`, `"Save Draft"`, `"Search"`,
///   plus a literal hit on `"Share Screen"`. Most sharing is low/no blast
///   radius and reversible (unshare/delete); colliding with `"Save"` --
///   arguably the single most common, safest button in any UI -- is an
///   unacceptable cost for that benefit. Dropped entirely, no phrase
///   substitute.
/// - `replace` -- scores 0.571 against `"Reply"`, one of the highest-
///   frequency, near-zero-blast-radius buttons in any messaging/email/
///   comment UI. Its higher-blast-radius sense (find-and-replace-all) is
///   covered as the `"replace all"` phrase instead.
/// - `invite` -- scores exactly 0.5 (the floor) against three separate
///   common, zero-consequence controls (`"Unmute"`, `"Minimize"`,
///   `"Favorite"`). Granting access is a real but comparatively low blast
///   radius (reversible by removing the invitee later); not worth three
///   simultaneous collisions with window/media controls.
///
/// ### Accepted new tradeoffs (kept; same shape as the existing `"Sendai"`
/// tradeoff documented in the module doc)
///
/// - `uninstall` scores 0.778 against `"Install"` -- the opposite-risk-
///   direction action. Kept anyway: `uninstall` is one of the two words
///   this finding requires, and the cost is one spoken confirmation on an
///   Install click, the same shape of cost the module doc already accepts
///   for `"Play"~"pay"`.
/// - `format` scores 0.5-0.571 against `"Sort"`/`"Forward"`.
/// - `purge` scores 0.6 against `"Merge"` (itself a non-trivial, often
///   irreversible action, so the extra confirm is cheap).
/// - `deactivate` scores exactly 0.5 against `"Duplicate"`.
///
/// ### Confirmation-of-consequence words need no new entries
///
/// COMMANDS-SPEC's "confirm/proceed/agree/accept" category needs no
/// lexicon addition: because matching is per-token (any token of a label
/// hitting any lexicon entry escalates the whole label), a label like
/// `"Confirm Delete"` or `"Proceed and Send"` already escalates today
/// through the `delete`/`send` token, independent of whether `"confirm"`
/// or `"proceed"` themselves are ever added. Adding those words as bare
/// lexicon entries would instead make *every* confirmation dialog in every
/// app destructive-by-label regardless of what it confirms, which is a
/// clear case of the same over-broad-token problem `reset`/`clear`/`share`
/// were dropped for above.
const DESTRUCTIVE_WORDS: &[&str] = &[
    "delete", "send", "pay", "remove", "discard", "purchase", "submit", "buy", "erase", "wipe", "destroy",
    // destructive install/system operations
    "uninstall", "format",
    // irreversible data operations
    "overwrite", "purge",
    // account/security operations
    "deactivate", "deauthorize", "disconnect", "terminate",
    // publishing/outbound operations
    "publish", "transfer", "withdraw",
];

/// Multi-word destructive phrases that don't reduce to a single lexicon
/// word (e.g. neither "submit" nor "payment" alone is destructive, but the
/// pair together is a confirm-to-pay control). Matched against each
/// adjacent word-count-sized window of a label's tokens.
///
/// The entries below the original four carry the destructive *sense* of a
/// word that [`DESTRUCTIVE_WORDS`]'s doc comment explains was dropped as a
/// bare word because of a benign-collision cost -- phrases are far more
/// specific (more characters to differ on before crossing [`BINDING_FLOOR`])
/// so they add negligible false-escalation cost of their own; none of them
/// produced a new hit against the benign corpus the words above were
/// measured against.
const DESTRUCTIVE_PHRASES: &[&str] = &[
    "submit payment", "buy now", "cancel subscription", "confirm order",
    // destructive install/system operations (dropped bare "reset"/"restore")
    "factory reset", "restore defaults",
    // irreversible data operations (dropped bare "clear"/"drop")
    "empty trash", "clear all data", "replace all", "drop table", "drop database",
    // account/security operations (dropped bare "revoke")
    "revoke access", "close account", "sign out everywhere",
];

/// Split a label into normalized (invisible characters stripped, all the
/// folds [`normalize_for_match`] does applied), punctuation-delimited word
/// tokens, then collapse runs of single-character tokens back into one
/// token. The collapse defeats letter-spaced perturbations like
/// `"D e l e t e"` (which naive splitting would see as six one-letter
/// tokens, none of them `"delete"`) without touching genuinely multi-word
/// labels/phrases like `"Submit Payment"`, whose tokens are each longer
/// than one character. Tokenizing (rather than scoring the whole label as
/// one string, which is what real target *binding* does) is deliberate: a
/// destructive word can appear as one component of an otherwise-unrelated
/// multi-word label (`"Delete Forever"`, `"Please Confirm Order"`), and the
/// whole-label edit-distance ratio would dilute below the floor long before
/// a genuinely destructive control's label would stop reading as
/// destructive to a human.
fn tokens(label: &str) -> Vec<String> {
    let normalized = normalize_for_match(label);
    let raw = normalized.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty());

    let mut out: Vec<String> = Vec::new();
    let mut run = String::new();
    for w in raw {
        if w.chars().count() == 1 {
            run.push_str(w);
        } else {
            if !run.is_empty() {
                out.push(std::mem::take(&mut run));
            }
            out.push(w.to_string());
        }
    }
    if !run.is_empty() {
        out.push(run);
    }
    out
}

/// True if `label`, tokenized and normalized identically to how real target
/// binding sees it, contains a word or adjacent-word phrase that would bind
/// -- per [`label_similarity`] at [`BINDING_FLOOR`], the exact function and
/// floor real binding uses -- to an entry in the destructive lexicon.
pub fn is_destructive_label(label: &str) -> bool {
    let toks = tokens(label);

    let word_hit =
        toks.iter().any(|t| DESTRUCTIVE_WORDS.iter().any(|d| label_similarity(d, t) >= BINDING_FLOOR));
    if word_hit {
        return true;
    }

    DESTRUCTIVE_PHRASES.iter().any(|phrase| {
        let width = phrase.split_whitespace().count();
        width > 0
            && width <= toks.len()
            && toks.windows(width).any(|w| label_similarity(phrase, &w.join(" ")) >= BINDING_FLOOR)
    })
}

/// Runtime facts an escalation rule may need beyond the schema itself.
/// Deliberately a plain struct (not the full `ActionableMap`/`ActionInstance`)
/// so the escalation function stays pure and trivially testable.
#[derive(Debug, Clone, Copy, Default)]
pub struct EscalationContext<'a> {
    pub target_label: Option<&'a str>,
    pub target_has_unsaved_changes: bool,
    pub shortcut_promoted_to_t1: bool,
}

/// Compute the effective tier for one resolved instance of `schema`,
/// applying the §3.4 escalation/demotion rules on top of the schema's
/// declared base tier. The base tier itself is never mutated -- this is a
/// pure function of (schema, context) -> Tier.
pub fn effective_tier(schema: &ActionSchema, ctx: EscalationContext<'_>) -> Tier {
    let mut tier = schema.tier;

    // App lifecycle: "quit-with-unsaved -> T2".
    if schema.id == "app.quit" && ctx.target_has_unsaved_changes {
        tier = tier.max(Tier::T2);
    }

    // UI interaction: destructive-labeled controls -> T2.
    if schema.id.starts_with("ui.") {
        if let Some(label) = ctx.target_label {
            if is_destructive_label(label) {
                tier = tier.max(Tier::T2);
            }
        }
    }

    // Shortcuts bridge: T2 by default, user can promote a *named* shortcut
    // to T1. Promotion only ever lowers T2 -> T1; it cannot demote anything
    // below T1, and it never touches other families.
    if schema.id == "shortcut.run" && ctx.shortcut_promoted_to_t1 && tier == Tier::T2 {
        tier = Tier::T1;
    }

    tier
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Invertibility;

    fn schema(id: &'static str, tier: Tier) -> ActionSchema {
        ActionSchema { id, tier, slots: &[], invertible: Invertibility::None }
    }

    #[test]
    fn destructive_lexicon_is_case_and_punctuation_insensitive() {
        for label in [
            "Delete", "DELETE!!", "Please Delete.", "delete", "  delete  ", "Delete-Forever",
        ] {
            assert!(is_destructive_label(label), "expected destructive: {label:?}");
        }
        for label in ["Send", "Send Now!", "send.", "Pay Invoice", "pay-now", "Remove", "remove!", "Discard", "discard.", "Purchase", "purchase now"] {
            assert!(is_destructive_label(label), "expected destructive: {label:?}");
        }
    }

    #[test]
    fn benign_labels_are_not_destructive() {
        for label in ["Cancel", "OK", "Save", "Close"] {
            assert!(!is_destructive_label(label), "expected benign: {label:?}");
        }
    }

    #[test]
    fn dispatch_named_benign_labels_stay_benign() {
        // The specific benign labels this unit's dispatch named to check
        // the false-escalation rate against.
        for label in ["Cancel", "Back", "Save Draft", "Continue"] {
            assert!(!is_destructive_label(label), "must not false-escalate: {label:?}");
        }
    }

    // --- Label-perturbation regressions ---------------------------------
    // Each of these binds to its spoken destructive word via
    // `disambiguate::match_candidates`'s fuzzy floor even though the old
    // exact-word-equality lexicon check missed it. Per the governing
    // principle ("if it can bind, it must escalate"), every one of these
    // must now be destructive.

    #[test]
    fn soft_hyphen_inside_delete_still_escalates() {
        assert!(is_destructive_label("De\u{00AD}lete"), "a soft hyphen must not hide the word delete");
    }

    #[test]
    fn zero_width_characters_inside_delete_still_escalate() {
        for label in ["De\u{200B}lete", "De\u{200C}lete", "De\u{200D}lete", "De\u{FEFF}lete"] {
            assert!(is_destructive_label(label), "zero-width injection must not hide the word delete: {label:?}");
        }
    }

    #[test]
    fn bidi_directional_marks_inside_delete_still_escalate() {
        // The two confirmed-live bypasses from this unit's dispatch:
        // U+200E LEFT-TO-RIGHT MARK and U+200F RIGHT-TO-LEFT MARK.
        for label in ["De\u{200E}lete", "De\u{200F}lete"] {
            assert!(is_destructive_label(label), "bidi marks must not hide the word delete: {label:?}");
        }
    }

    #[test]
    fn multi_substitution_leetspeak_delete_still_escalates() {
        // The other confirmed-live bypass: "d3l3t3" is edit-distance 3 from
        // "delete" -- outside the old near_miss <=1 rule entirely -- yet
        // the real binder already matches it at exactly its 0.5 floor.
        assert!(is_destructive_label("d3l3t3"), "multi-substitution leetspeak must escalate");
        for label in ["s3nd", "p4y", "r3m0v3", "d1sc4rd", "purch4s3", "5ubm1t", "3r4s3", "w1p3", "d35tr0y"] {
            assert!(is_destructive_label(label), "leetspeak obfuscation must escalate: {label:?}");
        }
    }

    #[test]
    fn cyrillic_homoglyph_pay_still_escalates() {
        // "рау" here is entirely Cyrillic (р U+0440, а U+0430, у U+0443),
        // visually indistinguishable from Latin "pay" in most UI fonts.
        assert!(is_destructive_label("рау"), "Cyrillic homoglyphs of pay must still escalate");
        assert!(is_destructive_label("Ѕеnd"), "partial Cyrillic homoglyph (S, e) of Send must still escalate");
    }

    #[test]
    fn fullwidth_delete_still_escalates() {
        assert!(is_destructive_label("Ｄｅｌｅｔｅ"), "fullwidth-form Delete must still escalate");
    }

    #[test]
    fn letter_spaced_delete_still_escalates() {
        assert!(is_destructive_label("D e l e t e"), "letter-spaced Delete must still escalate");
    }

    #[test]
    fn plural_deletes_still_escalates() {
        assert!(is_destructive_label("Deletes"), "the plural/affixed form Deletes must still escalate");
    }

    #[test]
    fn submit_payment_phrase_escalates() {
        assert!(is_destructive_label("Submit Payment"), "the multi-word phrase Submit Payment must escalate");
    }

    #[test]
    fn buy_now_phrase_escalates() {
        assert!(is_destructive_label("Buy Now"), "the multi-word phrase Buy Now must escalate");
    }

    #[test]
    fn newly_covered_lexicon_words_escalate() {
        // Coverage the previous wave was explicitly told about and missed.
        for label in ["Submit", "Buy", "Erase", "Wipe", "Destroy", "Erase All Data", "Wipe Device"] {
            assert!(is_destructive_label(label), "expected destructive (lexicon coverage): {label:?}");
        }
    }

    #[test]
    fn newly_covered_lexicon_phrases_escalate() {
        for label in ["Cancel Subscription", "Confirm Order", "please confirm order now"] {
            assert!(is_destructive_label(label), "expected destructive (lexicon coverage): {label:?}");
        }
    }

    #[test]
    fn near_miss_typo_of_delete_escalates() {
        // One substitution, same length -- a plausible fat-finger label, not
        // a plural, still must escalate.
        assert!(is_destructive_label("Delote"));
    }

    #[test]
    fn destructive_word_embedded_in_a_longer_label_still_escalates() {
        // A destructive word doesn't have to be the *entire* label -- tokenizing
        // (rather than scoring the whole label as one string) is what catches
        // this; see `tokens`'s doc comment.
        for label in ["Delete Forever", "Please Delete This Item", "Confirm and Send"] {
            assert!(is_destructive_label(label), "expected destructive: {label:?}");
        }
    }

    #[test]
    fn previously_benign_labels_now_escalate_choosing_safety() {
        // "Sendai"/"Sendai Tower" are 2 insertions / 6 characters from
        // "send" -- score 0.667, above BINDING_FLOOR (0.5). The *real*
        // binder's own `label_similarity` would bind a spoken "send" to a
        // button labeled exactly "Sendai" under this same floor, so per the
        // governing principle this module now escalates it too. This is the
        // documented false-escalation tradeoff (see module doc) -- kept as
        // its own test, distinct from `newly_covered_lexicon_words_escalate`
        // above, because the *reason* is different: those are exact lexicon
        // hits, this is a coincidental near-miss the shared floor no longer
        // special-cases away.
        for label in ["Sendai", "Sendai Tower", "Ｓｅｎｄａｉ"] {
            assert!(is_destructive_label(label), "now correctly escalates under binder-derived matching: {label:?}");
        }
    }

    #[test]
    fn words_that_stay_below_the_binding_floor_of_any_lexicon_entry_stay_benign() {
        // The fix must stay at least as permissive as the binder without
        // being *more* permissive than the binder itself would be -- these
        // remain benign because no lexicon word/phrase scores >= the same
        // floor real binding uses against them.
        for label in ["Cancel", "OK", "Save", "Close", "Payment"] {
            assert!(!is_destructive_label(label), "must stay benign (fails closed only for real destructive matches): {label:?}");
        }
    }

    /// Held-out measurement, built from a DIFFERENT generative principle
    /// than the fix uses: instead of hand-picking inputs the fix's own
    /// normalization is known to check for, apply named perturbation
    /// *classes* (confusable scripts including ones this crate's own
    /// confusable table does NOT tabulate, combining diacritics, bidi/
    /// zero-width marks, mixed multi-substitution leetspeak, possessives,
    /// plurals, trailing punctuation, whitespace splitting) programmatically
    /// to base destructive words.
    ///
    /// The invariant under test, per this unit's dispatch: the count of
    /// labels that BIND (i.e. would score >= `BINDING_FLOOR` against the
    /// base word they were derived from, via the exact function real target
    /// binding uses) but do NOT escalate must be zero. A label that fails
    /// to bind at all is not required to escalate -- that's not a gap, it's
    /// the same reason the real binder itself would never route a spoken
    /// destructive word to that label in the first place.
    #[test]
    fn held_out_adversarial_perturbations_bind_implies_escalate() {
        fn stylize(word: &str, base: u32) -> String {
            word.chars()
                .map(|c| {
                    if c.is_ascii_uppercase() {
                        char::from_u32(base + (c as u32 - 'A' as u32)).unwrap_or(c)
                    } else if c.is_ascii_lowercase() {
                        char::from_u32(base + 26 + (c as u32 - 'a' as u32)).unwrap_or(c)
                    } else {
                        c
                    }
                })
                .collect()
        }

        let bases =
            ["delete", "send", "pay", "remove", "discard", "purchase", "submit", "buy", "erase", "wipe", "destroy"];

        let mut cases: Vec<(String, String)> = Vec::new();

        // Mathematical alphanumeric styling: bold, fraktur, double-struck,
        // sans-serif, monospace.
        for base_word in ["delete", "send", "purchase", "destroy", "wipe"] {
            for style_base in [0x1D400u32, 0x1D504, 0x1D538, 0x1D5A0, 0x1D670] {
                cases.push((base_word.to_string(), stylize(base_word, style_base)));
            }
        }

        // Fullwidth styling.
        for base_word in ["delete", "erase", "submit"] {
            let full: String =
                base_word.chars().map(|c| char::from_u32(c as u32 + 0xFEE0).unwrap_or(c)).collect();
            cases.push((base_word.to_string(), full));
        }

        // Combining diacritics stitched after every letter (Zalgo-lite).
        for base_word in ["delete", "send", "pay", "remove"] {
            let marked: String = base_word.chars().flat_map(|c| [c, '\u{0301}']).collect();
            cases.push((base_word.to_string(), marked));
        }

        // Bidi/zero-width marks not already covered by the named
        // regression tests above (those used 00AD/200B-200D/FEFF/200E/200F).
        for base_word in ["discard", "purchase", "buy"] {
            for mark in ['\u{2060}', '\u{180E}', '\u{061C}', '\u{202A}', '\u{FE0F}'] {
                let mid = base_word.chars().count() / 2;
                let mut s = String::new();
                for (i, c) in base_word.chars().enumerate() {
                    if i == mid {
                        s.push(mark);
                    }
                    s.push(c);
                }
                cases.push((base_word.to_string(), s));
            }
        }

        // Confusable scripts, deliberately including letters this crate's
        // own CONFUSABLES table does NOT tabulate (lowercase Greek
        // rho/alpha/epsilon/psi, Armenian letters) alongside a couple it
        // does -- some of these are expected to land below the floor, which
        // is fine; the invariant only requires bind => escalate.
        let script_subs: &[(char, char)] = &[
            ('p', 'ρ'), ('a', 'α'), ('e', 'ε'), ('o', 'ο'), ('y', 'ψ'), // Greek lowercase
            ('n', 'ո'), ('s', 'ս'), ('u', 'ս'), ('r', 'ր'),             // Armenian lowercase
        ];
        for base_word in bases {
            for &(from, to) in script_subs {
                if base_word.contains(from) {
                    cases.push((base_word.to_string(), base_word.replacen(from, &to.to_string(), 1)));
                }
            }
        }

        // Multiple SIMULTANEOUS confusable substitutions per word (harder
        // than the single-substitution cases above), using scripts NOT in
        // this crate's CONFUSABLES table -- these are expected to actually
        // drop below the floor, giving genuine "does not bind" data points
        // rather than every case trivially binding via Levenshtein's
        // single-edit tolerance alone.
        for (base_word, heavy) in
            [("pay", "ραψ"), ("buy", "bսψ"), ("wipe", "աιpε"), ("erase", "ερασε")]
        {
            cases.push((base_word.to_string(), heavy.to_string()));
        }

        // The destructive word embedded inside a longer, otherwise-unrelated
        // surrounding phrase. This exercises the tokenization boundary
        // directly: the whole-string "binds" ground truth against the bare
        // base word is typically well below the floor once padded with
        // extra words, so these mostly demonstrate escalation being a
        // deliberate safety *superset* of raw whole-label binding, not a
        // mirror of it -- see `destructive_word_embedded_in_a_longer_label_
        // still_escalates` above for the same property as a plain assertion.
        for base_word in bases {
            cases.push((base_word.to_string(), format!("Please {base_word} this now")));
            cases.push((base_word.to_string(), format!("{base_word} - confirm to proceed")));
        }

        // Cherokee syllabics substituted for the leading letter.
        for base_word in ["delete", "discard", "destroy"] {
            let mut chars: Vec<char> = base_word.chars().collect();
            chars[0] = 'Ꭰ'; // U+13A0, resembles Latin D
            cases.push((base_word.to_string(), chars.into_iter().collect()));
        }

        // Mixed leetspeak: multiple simultaneous digit/symbol substitutions
        // per word, not just one.
        for (base_word, leet) in [
            ("delete", "d3l3t3"),
            ("send", "53nd"),
            ("pay", "p@y"),
            ("remove", "r3m0v3"),
            ("discard", "d1sc@rd"),
            ("purchase", "purch@53"),
            ("submit", "5ubm17"),
            ("erase", "3r@53"),
            ("wipe", "w1p3"),
            ("destroy", "d357r0y"),
        ] {
            cases.push((base_word.to_string(), leet.to_string()));
        }

        // Possessives, plurals, trailing punctuation, letter-spacing.
        for base_word in bases {
            cases.push((base_word.to_string(), format!("{base_word}'s")));
            cases.push((base_word.to_string(), format!("{base_word}s")));
            cases.push((base_word.to_string(), format!("{base_word}!!!")));
            cases.push((base_word.to_string(), format!("{base_word}...")));
            let spaced =
                base_word.chars().map(|c| c.to_string()).collect::<Vec<_>>().join(" ");
            cases.push((base_word.to_string(), spaced));
        }

        assert!(cases.len() >= 50, "held-out set must have at least 50 cases, has {}", cases.len());

        let mut bind_count = 0usize;
        let mut escalate_count = 0usize;
        let mut bind_but_not_escalate: Vec<(String, String)> = Vec::new();

        for (base_word, label) in &cases {
            let binds = label_similarity(base_word, label) >= BINDING_FLOOR;
            let escalates = is_destructive_label(label);
            if binds {
                bind_count += 1;
            }
            if escalates {
                escalate_count += 1;
            }
            if binds && !escalates {
                bind_but_not_escalate.push((base_word.clone(), label.clone()));
            }
        }

        eprintln!(
            "held-out perturbations: {} cases, {} bind, {} escalate, {} bind-but-not-escalate",
            cases.len(),
            bind_count,
            escalate_count,
            bind_but_not_escalate.len()
        );

        assert!(
            bind_but_not_escalate.is_empty(),
            "invariant violated -- labels that bind but do not escalate: {bind_but_not_escalate:?}"
        );
    }

    // --- Lexicon-coverage additions (adversarial audit minor finding) ---
    // The two reported cases, reproduced at both the pure `is_destructive_
    // label` level and the `effective_tier` level (matching how the dispatch
    // verified against the real binary), plus one test per newly added word
    // and phrase.

    #[test]
    fn reported_uninstall_label_escalates() {
        assert!(is_destructive_label("Uninstall"), "the reported bypass: click Uninstall");
    }

    #[test]
    fn reported_format_disk_label_escalates() {
        assert!(is_destructive_label("Format Disk"), "the reported bypass: click Format Disk");
    }

    #[test]
    fn ui_click_uninstall_escalates_to_t2() {
        let s = schema("ui.click", Tier::T1);
        let ctx = EscalationContext { target_label: Some("Uninstall"), ..Default::default() };
        assert_eq!(effective_tier(&s, ctx), Tier::T2);
    }

    #[test]
    fn ui_click_format_disk_escalates_to_t2() {
        let s = schema("ui.click", Tier::T1);
        let ctx = EscalationContext { target_label: Some("Format Disk"), ..Default::default() };
        assert_eq!(effective_tier(&s, ctx), Tier::T2);
    }

    #[test]
    fn newly_added_destructive_words_escalate() {
        for label in [
            "Uninstall", "Format", "Overwrite", "Purge", "Deactivate", "Deauthorize", "Disconnect",
            "Terminate", "Publish", "Transfer", "Withdraw",
        ] {
            assert!(is_destructive_label(label), "expected destructive (new lexicon word): {label:?}");
        }
    }

    #[test]
    fn newly_added_destructive_words_escalate_embedded_in_a_label() {
        for label in [
            "Format Disk", "Uninstall Plugin", "Overwrite Existing File", "Purge Cache",
            "Deactivate Account", "Deauthorize Device", "Disconnect Bank Account",
            "Terminate Session", "Publish to Everyone", "Transfer Funds", "Withdraw Funds",
        ] {
            assert!(is_destructive_label(label), "expected destructive (new lexicon word, embedded): {label:?}");
        }
    }

    #[test]
    fn newly_added_destructive_phrases_escalate() {
        for label in [
            "Factory Reset", "Restore Defaults", "Empty Trash", "Clear All Data", "Replace All",
            "Drop Table", "Drop Database", "Revoke Access", "Close Account", "Sign Out Everywhere",
        ] {
            assert!(is_destructive_label(label), "expected destructive (new lexicon phrase): {label:?}");
        }
    }

    #[test]
    fn dropped_bare_words_stay_benign_in_their_common_benign_sense() {
        // Words considered and explicitly dropped from DESTRUCTIVE_WORDS
        // (see its doc comment) because the bare word collides with a
        // common, low-blast-radius control. Their destructive *sense* is
        // still covered via the phrase list, exercised separately above.
        //
        // NOTE: "Reset Zoom"/"Reset View" are deliberately excluded from
        // this list -- they already escalate under the *pre-existing*,
        // unrelated "delete"~"Reset" baseline collision (score 0.5), which
        // predates and is independent of this unit's changes. Asserting
        // benign-ness there would be testing a baseline fact this unit
        // didn't create and has no mandate to fix.
        for label in [
            "Clear Search", "Clear Filters", "Crop", "Stop", "Rotate", "Link", "Post Comment",
            "Share Screen", "Reply", "Unmute", "Minimize", "Favorite",
        ] {
            assert!(!is_destructive_label(label), "must stay benign (dropped-word tradeoff): {label:?}");
        }
    }

    #[test]
    fn accepted_new_tradeoffs_documented_in_lexicon_comment() {
        // Same shape as the pre-existing "Sendai" tradeoff: kept because the
        // word itself was required or clearly justified, cost is one extra
        // spoken confirmation on a benign, high-frequency control.
        for label in ["Install", "Sort", "Forward", "Merge", "Duplicate"] {
            assert!(is_destructive_label(label), "documented accepted tradeoff must escalate: {label:?}");
        }
    }

    /// Realistic benign-UI-button-label corpus (108 labels, well over the
    /// dispatch's 60-label minimum) used to measure the false-escalation
    /// rate this unit's lexicon additions cost. The BEFORE rate below is
    /// computed against a hand-copied snapshot of the lexicon as it stood
    /// before this unit's changes (the eleven original `DESTRUCTIVE_WORDS`
    /// and four original `DESTRUCTIVE_PHRASES`) using the *same* shared
    /// `label_similarity`/`BINDING_FLOOR`/`tokens` machinery this file
    /// already exposes -- not a reimplementation -- so the comparison is
    /// apples-to-apples. See this unit's report for the literal before/
    /// after numbers.
    #[test]
    fn benign_rate_measurement_delta_is_bounded() {
        const ORIGINAL_WORDS: &[&str] = &[
            "delete", "send", "pay", "remove", "discard", "purchase", "submit", "buy", "erase",
            "wipe", "destroy",
        ];
        const ORIGINAL_PHRASES: &[&str] =
            &["submit payment", "buy now", "cancel subscription", "confirm order"];

        fn is_destructive_with(label: &str, words: &[&str], phrases: &[&str]) -> bool {
            let toks = tokens(label);
            let word_hit =
                toks.iter().any(|t| words.iter().any(|d| label_similarity(d, t) >= BINDING_FLOOR));
            if word_hit {
                return true;
            }
            phrases.iter().any(|phrase| {
                let width = phrase.split_whitespace().count();
                width > 0
                    && width <= toks.len()
                    && toks.windows(width).any(|w| label_similarity(phrase, &w.join(" ")) >= BINDING_FLOOR)
            })
        }

        let benign: &[&str] = &[
            "Cancel", "OK", "Save", "Close", "Back", "Save Draft", "Continue", "Next", "Previous",
            "Skip", "Done", "Submit Feedback", "Play", "Pause", "Stop", "Mute", "Unmute", "Search",
            "Filter", "Sort", "Refresh", "Reload", "Settings", "Preferences", "Help", "About",
            "Home", "Profile", "Edit", "View", "Copy", "Paste", "Cut", "Undo", "Redo", "Print",
            "Export", "Import", "Download", "Upload", "Link", "Merge", "Crop", "Post Comment",
            "Share Screen", "Zoom In", "Zoom Out", "Rotate", "Select All", "Clear Search",
            "Clear Filters", "Reset Zoom", "Reset View", "Restore Defaults", "Follow", "Unfollow",
            "Like", "Comment", "Reply", "Forward", "Archive", "Mark Read", "Snooze", "Sign In",
            "Sign Up", "Log In", "Log Out", "Register", "Subscribe", "Add", "Remove Filter",
            "Confirm Email", "Verify", "Apply", "OK Got It", "Learn More", "Explore", "Browse",
            "More Info", "Enable", "Disable", "Toggle", "Expand", "Collapse", "Minimize",
            "Maximize", "Restart", "Update", "Install", "Renew", "Extend", "Dismiss", "Ignore",
            "Report", "Flag", "Block", "Unblock", "Pin", "Unpin", "Star", "Favorite", "Bookmark",
            "Tag", "Untag", "Duplicate", "Rename", "Move", "Resume",
        ];
        assert!(benign.len() >= 60, "benign corpus must have at least 60 labels, has {}", benign.len());

        let before_hits: Vec<&str> = benign
            .iter()
            .copied()
            .filter(|l| is_destructive_with(l, ORIGINAL_WORDS, ORIGINAL_PHRASES))
            .collect();
        let after_hits: Vec<&str> =
            benign.iter().copied().filter(|l| is_destructive_label(l)).collect();

        let before_rate = before_hits.len() as f32 / benign.len() as f32;
        let after_rate = after_hits.len() as f32 / benign.len() as f32;

        eprintln!(
            "benign false-escalation rate: BEFORE {}/{} ({:.1}%) -> AFTER {}/{} ({:.1}%)",
            before_hits.len(),
            benign.len(),
            before_rate * 100.0,
            after_hits.len(),
            benign.len(),
            after_rate * 100.0
        );
        eprintln!("before hits: {before_hits:?}");
        eprintln!("after hits: {after_hits:?}");

        let newly_introduced: Vec<&str> =
            after_hits.iter().copied().filter(|l| !before_hits.contains(l)).collect();
        eprintln!("newly introduced by this unit's additions: {newly_introduced:?}");

        // Every newly introduced collision must be one of the tradeoffs
        // documented and accepted in DESTRUCTIVE_WORDS's doc comment -- if
        // the lexicon changes later and a *new*, undocumented collision
        // appears, this must fail rather than silently widen the tradeoff.
        let documented_new_tradeoffs = ["Install", "Sort", "Forward", "Merge", "Duplicate"];
        for label in &newly_introduced {
            assert!(
                documented_new_tradeoffs.contains(label),
                "undocumented new false-escalation introduced by this unit's lexicon change: {label:?}"
            );
        }

        // The delta this unit's additions cost, in absolute percentage
        // points, must stay small relative to the (already-accepted)
        // baseline rate -- a hard ceiling against silent creep.
        assert!(
            after_rate - before_rate <= 0.10,
            "false-escalation rate grew by more than 10 points: {:.1}% -> {:.1}%",
            before_rate * 100.0,
            after_rate * 100.0
        );
    }

    #[test]
    fn fullwidth_benign_label_stays_benign() {
        // Sanity check that fullwidth folding doesn't itself manufacture a
        // false positive on an unrelated word.
        assert!(!is_destructive_label("ＯＫ"));
    }

    #[test]
    fn ui_click_escalates_to_t2_on_destructive_label() {
        let s = schema("ui.click", Tier::T1);
        let ctx = EscalationContext { target_label: Some("Delete"), ..Default::default() };
        assert_eq!(effective_tier(&s, ctx), Tier::T2);
    }

    #[test]
    fn ui_click_stays_t1_on_benign_label() {
        let s = schema("ui.click", Tier::T1);
        let ctx = EscalationContext { target_label: Some("OK"), ..Default::default() };
        assert_eq!(effective_tier(&s, ctx), Tier::T1);
    }

    #[test]
    fn app_quit_escalates_to_t2_with_unsaved_changes() {
        let s = schema("app.quit", Tier::T1);
        let ctx = EscalationContext { target_has_unsaved_changes: true, ..Default::default() };
        assert_eq!(effective_tier(&s, ctx), Tier::T2);
    }

    #[test]
    fn app_quit_stays_t1_without_unsaved_changes() {
        let s = schema("app.quit", Tier::T1);
        let ctx = EscalationContext::default();
        assert_eq!(effective_tier(&s, ctx), Tier::T1);
    }

    #[test]
    fn shortcut_promotion_demotes_t2_to_t1() {
        let s = schema("shortcut.run", Tier::T2);
        let ctx = EscalationContext { shortcut_promoted_to_t1: true, ..Default::default() };
        assert_eq!(effective_tier(&s, ctx), Tier::T1);
    }

    #[test]
    fn shortcut_without_promotion_stays_t2() {
        let s = schema("shortcut.run", Tier::T2);
        let ctx = EscalationContext::default();
        assert_eq!(effective_tier(&s, ctx), Tier::T2);
    }

    #[test]
    fn escalation_never_touches_unrelated_families() {
        let s = schema("win.maximize", Tier::T0);
        let ctx = EscalationContext {
            target_label: Some("Delete"),
            target_has_unsaved_changes: true,
            shortcut_promoted_to_t1: true,
        };
        assert_eq!(effective_tier(&s, ctx), Tier::T0, "win.* must not be escalated by unrelated context flags");
    }
}
