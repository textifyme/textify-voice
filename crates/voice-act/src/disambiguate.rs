//! Label matching + near-tie detection for `resolve()`.
//!
//! COMMANDS-SPEC.md §3.3: "click Send" with two Send buttons -> HUD numbers
//! just those candidates; user says the number. Resolution never picks
//! silently among near-ties. Per the repo's RUST RULES we implement the
//! classic edit-distance algorithm ourselves rather than pulling a crate.
//!
//! [`normalize_for_match`] and [`BINDING_FLOOR`] are the single normalize +
//! threshold pair used everywhere a spoken query is matched against a UI
//! label -- both real target binding (`match_candidates`, called from
//! `mock.rs`) and [`crate::escalation`]'s destructive-label check consult
//! this exact pair rather than each carrying its own copy. See
//! `escalation`'s module doc for why that sharing is load-bearing, not
//! incidental.

/// Similarity floor at/above which two labels are considered "the same
/// target". This is *the* threshold real target resolution uses (see
/// `mock.rs`'s `match_candidates` call sites) and the one
/// [`crate::escalation::is_destructive_label`] consults too, so the two can
/// never independently drift apart the way a second, separately-tuned
/// threshold could.
pub const BINDING_FLOOR: f32 = 0.5;

/// Format characters (Unicode general category Cf) that render invisibly.
/// Hand-rolled by block -- no `unicode-general-category`/normalization
/// crate is available in this workspace (RUST RULES) -- covering every
/// block Unicode currently assigns to Cf: bidi controls (LRM/RLM/
/// embeddings/overrides/isolates), zero-width joiners/space/no-break-space,
/// soft hyphen, Arabic number-sign controls, interlinear annotation
/// controls, and the tag/format-control ranges used by a few other blocks.
/// Not claimed exhaustive of some *future* Unicode version, but current as
/// of the version this table was written against.
fn is_format_char(cp: u32) -> bool {
    matches!(cp,
        0x00AD
        | 0x0600..=0x0605
        | 0x061C
        | 0x06DD
        | 0x070F
        | 0x08E2
        | 0x180E
        | 0x200B..=0x200F   // ZWSP, ZWNJ, ZWJ, LRM, RLM
        | 0x202A..=0x202E   // LRE, RLE, PDF, LRO, RLO
        | 0x2060..=0x2064
        | 0x2066..=0x206F
        | 0xFEFF            // ZWNBSP / BOM
        | 0xFFF9..=0xFFFB
        | 0x110BD
        | 0x110CD
        | 0x13430..=0x13438
        | 0x1BCA0..=0x1BCA3
        | 0x1D173..=0x1D17A
        | 0xE0001
        | 0xE0020..=0xE007F
    )
}

/// Nonspacing combining marks (Unicode general category Mn) -- diacritic
/// overlays that attach to the preceding base character with no width of
/// their own, usable to visually clutter a word without changing which base
/// letters a reader sees (e.g. Zalgo-style stacking, or a single stray
/// combining mark dropped into a word to defeat naive equality). Grouped by
/// the combining-mark blocks that carry general-purpose diacritics across
/// scripts; like [`is_format_char`], not claimed exhaustive of every
/// script's marks but covers the blocks that matter for this purpose.
fn is_combining_mark(cp: u32) -> bool {
    matches!(cp,
        0x0300..=0x036F   // Combining Diacritical Marks
        | 0x0483..=0x0489 // Cyrillic combining marks
        | 0x0591..=0x05BD | 0x05BF | 0x05C1 | 0x05C2 | 0x05C4 | 0x05C5 | 0x05C7 // Hebrew points
        | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670 // Arabic marks
        | 0x06D6..=0x06DC | 0x06DF..=0x06E4 | 0x06E7 | 0x06E8 | 0x06EA..=0x06ED // Arabic extended
        | 0x0711 | 0x0730..=0x074A // Syriac
        | 0x07A6..=0x07B0 // Thaana
        | 0x0816..=0x0819 | 0x081B..=0x0823 | 0x0825..=0x0827 | 0x0829..=0x082D // Samaritan
        | 0x0859..=0x085B // Mandaic
        | 0x08E3..=0x0902 | 0x093A | 0x093C | 0x0941..=0x0948 | 0x094D | 0x0951..=0x0957 | 0x0962 | 0x0963 // Devanagari
        | 0x0981 | 0x09BC | 0x09C1..=0x09C4 | 0x09CD | 0x09E2 | 0x09E3 // Bengali
        | 0x1AB0..=0x1AFF // Combining Diacritical Marks Extended
        | 0x1DC0..=0x1DFF // Combining Diacritical Marks Supplement
        | 0x20D0..=0x20FF // Combining Diacritical Marks for Symbols
        | 0xFE00..=0xFE0F // Variation Selectors
        | 0xFE20..=0xFE2F // Combining Half Marks
    )
}

/// True if `c` is invisible/format (Cf) or a nonspacing combining mark
/// (Mn) and should be stripped entirely before comparison -- it carries no
/// base-letter identity of its own, so dropping it (rather than trying to
/// interpret it) is always the safe move.
fn is_stripped(c: char) -> bool {
    let cp = c as u32;
    is_format_char(cp) || is_combining_mark(cp)
}

/// Fullwidth ASCII compatibility forms (U+FF01-U+FF5E) fold to basic Latin
/// at a fixed offset -- this is the same mapping NFKC compatibility
/// decomposition uses for this block, reproduced by hand since no
/// normalization crate is available here.
fn fold_fullwidth(c: char) -> char {
    if ('\u{FF01}'..='\u{FF5E}').contains(&c) {
        char::from_u32(c as u32 - 0xFEE0).unwrap_or(c)
    } else {
        c
    }
}

/// Base codepoint of "A" for each Mathematical Alphanumeric Symbols letter
/// style (bold, italic, bold italic, script, bold script, fraktur,
/// double-struck, bold fraktur, sans-serif, sans-serif bold, sans-serif
/// italic, sans-serif bold italic, monospace). Each style is a contiguous,
/// algorithmically-generated 52-codepoint run: `base..base+26` is A-Z,
/// `base+26..base+52` is a-z, per the Unicode block's published layout.
const MATH_LETTER_STYLE_BASES: [u32; 13] = [
    0x1D400, 0x1D434, 0x1D468, 0x1D49C, 0x1D4D0, 0x1D504, 0x1D538, 0x1D56C, 0x1D5A0, 0x1D5D4,
    0x1D608, 0x1D63C, 0x1D670,
];

/// Base codepoint of digit "0" for each Mathematical Alphanumeric Symbols
/// digit style (bold, double-struck, sans-serif, sans-serif bold,
/// monospace). Each spans 10 contiguous codepoints, 0-9.
const MATH_DIGIT_STYLE_BASES: [u32; 5] = [0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6];

/// Fold a Mathematical Alphanumeric Symbols codepoint (U+1D400-U+1D7FF --
/// bold/italic/script/fraktur/double-struck/sans-serif/monospace styling of
/// plain ASCII letters and digits, as seen in "stylized text" obfuscation)
/// to the ASCII letter/digit it visually renders as. A handful of
/// codepoints in this block are intentionally left unassigned by Unicode
/// (legacy compatibility holes, e.g. italic small h, which uses the
/// pre-existing Letterlike Symbols U+210E instead) -- those exact
/// codepoints can never appear in real text (there is nothing that encodes
/// to an unassigned codepoint), so the per-style arithmetic below needs no
/// special-casing for them.
fn fold_math_alphanumeric(c: char) -> char {
    let cp = c as u32;
    if !(0x1D400..=0x1D7FF).contains(&cp) {
        return c;
    }
    for base in MATH_LETTER_STYLE_BASES {
        if (base..base + 52).contains(&cp) {
            let offset = cp - base;
            let letter =
                if offset < 26 { b'A' + offset as u8 } else { b'a' + (offset - 26) as u8 };
            return letter as char;
        }
    }
    for base in MATH_DIGIT_STYLE_BASES {
        if (base..base + 10).contains(&cp) {
            return (b'0' + (cp - base) as u8) as char;
        }
    }
    c
}

/// Hand-rolled compatibility/confusable folding, standing in for a full
/// Unicode NFKC + confusables table (no crate for either is available in
/// this workspace, per the RUST RULES). Covers fullwidth ASCII
/// compatibility forms (handled separately by [`fold_fullwidth`]) and a
/// curated set of Cyrillic/Greek letters that are glyph-identical (or
/// near-identical) to their Latin lookalikes in common UI fonts, per
/// Unicode's published confusables data. Deliberately conservative -- only
/// entries with well-established visual identity are included, so folding
/// never turns an unrelated label into a false positive on its own (though
/// see `escalation`'s module doc for why the *ratio-based* floor can still
/// do that for short words independent of this table).
const CONFUSABLES: &[(char, char)] = &[
    // Cyrillic -> Latin, lowercase
    ('а', 'a'),
    ('е', 'e'),
    ('о', 'o'),
    ('р', 'p'),
    ('с', 'c'),
    ('х', 'x'),
    ('у', 'y'),
    ('і', 'i'),
    ('ѕ', 's'),
    ('ј', 'j'),
    ('ԁ', 'd'),
    ('һ', 'h'),
    // Cyrillic -> Latin, uppercase
    ('А', 'A'),
    ('В', 'B'),
    ('Е', 'E'),
    ('К', 'K'),
    ('М', 'M'),
    ('Н', 'H'),
    ('О', 'O'),
    ('Р', 'P'),
    ('С', 'C'),
    ('Т', 'T'),
    ('Х', 'X'),
    ('Ѕ', 'S'),
    ('Ј', 'J'),
    // Greek -> Latin, uppercase (glyph-identical in most UI fonts)
    ('Α', 'A'),
    ('Β', 'B'),
    ('Ε', 'E'),
    ('Ζ', 'Z'),
    ('Η', 'H'),
    ('Ι', 'I'),
    ('Κ', 'K'),
    ('Μ', 'M'),
    ('Ν', 'N'),
    ('Ο', 'O'),
    ('Ρ', 'P'),
    ('Τ', 'T'),
    ('Υ', 'Y'),
    ('Χ', 'X'),
    // Greek -> Latin, lowercase
    ('ο', 'o'),
];

fn fold_confusable(c: char) -> char {
    CONFUSABLES.iter().find(|(from, _)| *from == c).map(|(_, to)| *to).unwrap_or(c)
}

/// Fold a single leetspeak digit/symbol substitution to the Latin letter it
/// commonly stands in for: `3->e, 1->l, 0->o, 5->s, 4->a, 7->t, @->a, $->s`.
/// `1` is folded to `l` rather than `i` -- a deliberate simplification for
/// an inherently ambiguous case (leetspeak uses `1` for either). The
/// residual single-character mismatch on the rare word where `1` actually
/// stood for `i` is exactly the kind of one-edit gap [`levenshtein`]'s
/// ratio-based floor already tolerates, so it costs nothing in practice.
fn fold_leet(c: char) -> char {
    match c {
        '3' => 'e',
        '1' => 'l',
        '0' => 'o',
        '5' => 's',
        '4' => 'a',
        '7' => 't',
        '@' => 'a',
        '$' => 's',
        other => other,
    }
}

/// The single normalization pipeline shared by every place a spoken query
/// is compared against a UI label: strip invisible/combining characters,
/// fold fullwidth/mathematical-alphanumeric/confusable/leetspeak characters
/// to the plain Latin letter they visually stand in for, then case-fold and
/// trim. Order matters -- stripping runs first so a combining mark
/// shouldn't itself get folded, and folding runs before the final
/// case/trim pass since folded output is already plain ASCII.
pub fn normalize_for_match(s: &str) -> String {
    s.chars()
        .filter(|c| !is_stripped(*c))
        .map(fold_fullwidth)
        .map(fold_math_alphanumeric)
        .map(fold_confusable)
        .map(fold_leet)
        .collect::<String>()
        .trim()
        .to_lowercase()
}

/// Levenshtein edit distance between two strings, computed over Unicode
/// scalar values (not bytes) so it behaves sanely on non-ASCII labels.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }

    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr: Vec<usize> = vec![0; m + 1];

    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

/// Similarity score in `[0.0, 1.0]`: `1.0` is an exact match after
/// [`normalize_for_match`] (case-insensitive, trimmed, invisible/format
/// characters stripped, fullwidth/mathematical-alphanumeric/confusable/
/// leetspeak characters folded to plain Latin), `0.0` is maximally
/// dissimilar. Used to rank candidates and to decide whether the top
/// candidates are a near-tie -- and, via [`BINDING_FLOOR`], reused verbatim
/// by [`crate::escalation`] to decide whether a label binds to a
/// destructive spoken word.
pub fn label_similarity(query: &str, label: &str) -> f32 {
    let q = normalize_for_match(query);
    let l = normalize_for_match(label);
    let max_len = q.chars().count().max(l.chars().count());
    if max_len == 0 {
        return 1.0;
    }
    let dist = levenshtein(&q, &l);
    1.0 - (dist as f32 / max_len as f32)
}

/// How close two scores must be to count as a "near-tie" rather than a
/// clear winner. Chosen so that e.g. a one-character difference at typical
/// UI-label lengths (6-10 chars) still counts as a tie, while a genuinely
/// distinct label does not.
pub const NEAR_TIE_EPSILON: f32 = 0.05;

/// A scored candidate for a label-based resolve.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredCandidate<'a, T> {
    pub item: &'a T,
    pub score: f32,
}

/// Outcome of matching a query label against a set of candidates.
#[derive(Debug, Clone, PartialEq)]
pub enum MatchOutcome<'a, T> {
    /// Exactly one candidate is clearly best.
    Unique(ScoredCandidate<'a, T>),
    /// Two or more candidates are tied or near-tied at the top score.
    /// `resolve()` MUST turn this into `Resolution::NeedsDisambiguation`,
    /// never pick one silently.
    Tied(Vec<ScoredCandidate<'a, T>>),
    /// No candidate matched at all.
    None,
}

/// Score `query` against every candidate's label (via `label_of`) and
/// classify the result per [`MatchOutcome`]. `min_score` is a floor below
/// which a candidate is not considered a match at all (filters out
/// obviously-unrelated elements rather than forcing a disambiguation
/// dialogue among garbage).
pub fn match_candidates<'a, T>(
    query: &str,
    candidates: &'a [T],
    label_of: impl Fn(&T) -> &str,
    min_score: f32,
) -> MatchOutcome<'a, T> {
    let mut scored: Vec<ScoredCandidate<'a, T>> = candidates
        .iter()
        .map(|item| ScoredCandidate {
            item,
            score: label_similarity(query, label_of(item)),
        })
        .filter(|c| c.score >= min_score)
        .collect();

    if scored.is_empty() {
        return MatchOutcome::None;
    }

    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    let top = scored[0].score;
    let tied: Vec<ScoredCandidate<'a, T>> = scored
        .into_iter()
        .filter(|c| (top - c.score).abs() <= NEAR_TIE_EPSILON)
        .collect();

    if tied.len() == 1 {
        let mut it = tied.into_iter();
        match it.next() {
            Some(candidate) => MatchOutcome::Unique(candidate),
            // Unreachable (len == 1 was just checked); fall back to `None`
            // rather than unwrap on a library-input-reachable path.
            None => MatchOutcome::None,
        }
    } else {
        MatchOutcome::Tied(tied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_matches_known_distances() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("send", "send"), 0);
        assert_eq!(levenshtein("send", "sent"), 1);
    }

    #[test]
    fn label_similarity_exact_match_is_one() {
        assert_eq!(label_similarity("Send", "send"), 1.0);
        assert_eq!(label_similarity("  Send  ", "Send"), 1.0);
    }

    #[test]
    fn exact_duplicate_labels_are_tied() {
        let candidates = vec!["Send".to_string(), "Send".to_string(), "Cancel".to_string()];
        let outcome = match_candidates("Send", &candidates, |s| s.as_str(), 0.5);
        match outcome {
            MatchOutcome::Tied(tied) => {
                assert_eq!(tied.len(), 2, "both Send buttons must tie, Cancel excluded");
                assert!(tied.iter().all(|c| c.item == "Send"));
            }
            other => panic!("expected Tied, got {other:?}"),
        }
    }

    #[test]
    fn near_tie_labels_are_tied_not_guessed() {
        // "Conform" and "Confirms" are both one edit away from "Confirm"
        // (lengths 7 and 8), scoring 0.857 and 0.875 -- within epsilon.
        // "Cancel" is far away and must be excluded.
        let candidates = vec!["Conform".to_string(), "Confirms".to_string(), "Cancel".to_string()];
        let outcome = match_candidates("Confirm", &candidates, |s| s.as_str(), 0.5);
        match outcome {
            MatchOutcome::Tied(tied) => {
                let labels: Vec<&str> = tied.iter().map(|c| c.item.as_str()).collect();
                assert!(labels.contains(&"Conform"));
                assert!(labels.contains(&"Confirms"));
                assert!(!labels.contains(&"Cancel"), "distant candidate must not be pulled into the tie");
            }
            other => panic!("expected Tied near-tie, got {other:?}"),
        }
    }

    #[test]
    fn clear_winner_is_unique() {
        let candidates = vec!["Send".to_string(), "Cancel".to_string(), "Discard".to_string()];
        let outcome = match_candidates("Send", &candidates, |s| s.as_str(), 0.5);
        match outcome {
            MatchOutcome::Unique(c) => assert_eq!(c.item, "Send"),
            other => panic!("expected Unique, got {other:?}"),
        }
    }

    #[test]
    fn no_candidate_above_floor_is_none() {
        let candidates = vec!["Zzyzx".to_string()];
        let outcome = match_candidates("Send", &candidates, |s| s.as_str(), 0.5);
        assert_eq!(outcome, MatchOutcome::None);
    }
}
