//! Edit distance, implemented in-crate rather than pulled in as a dependency.
//!
//! Feeds bias pipeline layer 2 (SPEC.md §3.3) as the second gate after a
//! Double Metaphone code match: "matched against `BiasContext` via Double
//! Metaphone + edit-distance threshold."
//!
//! This is Damerau-Levenshtein restricted to adjacent transpositions (the
//! "optimal string alignment" variant): insertions, deletions, substitutions,
//! and single adjacent-character transpositions, each costing 1. That is the
//! standard practical choice for phonetic-correction pipelines (it catches
//! the common ASR/typo pattern of two adjacent letters swapped, e.g.
//! "tehram" vs "terham", without the extra bookkeeping true unrestricted
//! Damerau-Levenshtein needs for non-adjacent transpositions, which do not
//! occur in single-word ASR misrecognition).

/// Compute the OSA (optimal string alignment) Damerau-Levenshtein distance
/// between `a` and `b`, comparing Unicode scalar values directly (callers
/// that want case-insensitive comparison should lowercase both inputs first).
#[must_use]
pub fn damerau_levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (la, lb) = (a.len(), b.len());

    if la == 0 {
        return lb;
    }
    if lb == 0 {
        return la;
    }

    // d[i][j] = distance between a[..i] and b[..j]
    let mut d = vec![vec![0usize; lb + 1]; la + 1];
    for (i, row) in d.iter_mut().enumerate().take(la + 1) {
        row[0] = i;
    }
    for j in 0..=lb {
        d[0][j] = j;
    }

    for i in 1..=la {
        for j in 1..=lb {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let deletion = d[i - 1][j] + 1;
            let insertion = d[i][j - 1] + 1;
            let substitution = d[i - 1][j - 1] + cost;
            let mut best = deletion.min(insertion).min(substitution);

            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                let transposition = d[i - 2][j - 2] + 1;
                best = best.min(transposition);
            }
            d[i][j] = best;
        }
    }

    d[la][lb]
}

/// Case-insensitive convenience wrapper (ASCII + Unicode simple case-folding
/// via `char::to_lowercase`), used throughout the bias pipeline where
/// transcripts and bias terms differ only in casing.
#[must_use]
pub fn damerau_levenshtein_ci(a: &str, b: &str) -> usize {
    let a_lower: String = a.chars().flat_map(char::to_lowercase).collect();
    let b_lower: String = b.chars().flat_map(char::to_lowercase).collect();
    damerau_levenshtein(&a_lower, &b_lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_strings_have_zero_distance() {
        assert_eq!(damerau_levenshtein("postgres", "postgres"), 0);
    }

    #[test]
    fn empty_strings() {
        assert_eq!(damerau_levenshtein("", ""), 0);
        assert_eq!(damerau_levenshtein("abc", ""), 3);
        assert_eq!(damerau_levenshtein("", "abc"), 3);
    }

    #[test]
    fn single_insertion() {
        // postgres -> postgress: one inserted 's'.
        assert_eq!(damerau_levenshtein("postgres", "postgress"), 1);
    }

    #[test]
    fn single_substitution() {
        assert_eq!(damerau_levenshtein("cat", "cot"), 1);
    }

    #[test]
    fn adjacent_transposition_costs_one() {
        // "ab" -> "ba" is a plain substitution-based distance of 2 under
        // pure Levenshtein but 1 under Damerau-Levenshtein.
        assert_eq!(damerau_levenshtein("ab", "ba"), 1);
        assert_eq!(damerau_levenshtein("teh", "the"), 1);
    }

    #[test]
    fn repeated_insertions_scale_with_count() {
        // Hand-verified fixture shared with metaphone.rs's near-miss test:
        // "Kafka" vs "Kaaaaafka" — same phonetic code, distance 4.
        assert_eq!(damerau_levenshtein("kafka", "kaaaaafka"), 4);
    }

    #[test]
    fn case_insensitive_wrapper() {
        assert_eq!(damerau_levenshtein_ci("Postgres", "postgres"), 0);
        assert_eq!(damerau_levenshtein_ci("POSTGRES", "postgress"), 1);
    }

    #[test]
    fn distance_is_symmetric() {
        assert_eq!(
            damerau_levenshtein("kitten", "sitting"),
            damerau_levenshtein("sitting", "kitten")
        );
        assert_eq!(damerau_levenshtein("kitten", "sitting"), 3);
    }
}
