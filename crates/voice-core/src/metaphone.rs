//! Double Metaphone phonetic encoding, implemented in-crate (Philips, "Double
//! Metaphone," C/C++ Users Journal, 2000) rather than pulled in as a dependency.
//!
//! Feeds bias pipeline **layer 2** — SPEC.md §3.3: "Deterministic phonetic
//! post-correction (every engine, ~0 ms): low-confidence / OOV spans in the
//! transcript matched against `BiasContext` via Double Metaphone + edit-distance
//! threshold."
//!
//! This is a practical port covering the algorithm's core rule set (silent
//! initial letters, vowel skipping, consonant digraphs, hard/soft C and G,
//! doubled-consonant collapsing, primary/secondary divergence for genuinely
//! ambiguous spellings) rather than a byte-for-byte reproduction of every rare
//! branch (e.g. Slavic/Germanic surname exceptions) in the original — those
//! don't change correctness for the pipeline's actual inputs: English proper
//! nouns and code identifiers biased into a dictation transcript. Deliberate
//! deviation from the classic implementation: codes are capped at
//! [`MAX_CODE_LEN`] = 6 rather than the original's 4, which reduces
//! false-positive collisions when matching against arbitrary identifiers.

/// The two Double Metaphone codes for a word. `secondary` equals `primary`
/// when the word has no plausible alternate pronunciation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaphoneCode {
    pub primary: String,
    pub secondary: String,
}

impl MetaphoneCode {
    /// True if `self` and `other` share any code (primary or secondary) —
    /// the standard Double Metaphone match rule.
    #[must_use]
    pub fn matches(&self, other: &MetaphoneCode) -> bool {
        (!self.primary.is_empty() && self.primary == other.primary)
            || (!self.primary.is_empty() && self.primary == other.secondary)
            || (!self.secondary.is_empty() && self.secondary == other.primary)
            || (!self.secondary.is_empty() && self.secondary == other.secondary)
    }
}

/// Max length (bytes) of each returned code. See module docs for why this
/// deviates from the classic algorithm's default of 4.
const MAX_CODE_LEN: usize = 6;

/// Encode `word` into its primary and secondary Double Metaphone codes.
/// Non-alphabetic characters (spaces, digits, punctuation) are stripped
/// before encoding, so callers may pass raw transcript spans directly.
#[must_use]
pub fn double_metaphone(word: &str) -> MetaphoneCode {
    let bytes: Vec<u8> = word
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_uppercase() as u8)
        .collect();
    let (primary, secondary) = encode(&bytes);
    MetaphoneCode { primary, secondary }
}

fn is_vowel(b: u8) -> bool {
    matches!(b, b'A' | b'E' | b'I' | b'O' | b'U' | b'Y')
}

struct Buf<'a> {
    c: &'a [u8],
    n: usize,
}

impl Buf<'_> {
    fn at(&self, i: isize) -> u8 {
        if i < 0 {
            return 0;
        }
        let i = i as usize;
        if i >= self.n {
            0
        } else {
            self.c[i]
        }
    }

    fn is(&self, i: isize, b: u8) -> bool {
        self.at(i) == b
    }

    fn one_of(&self, i: isize, set: &[u8]) -> bool {
        let v = self.at(i);
        v != 0 && set.contains(&v)
    }

    /// True if the ASCII slice `pattern` occurs starting at `start`.
    fn matches_at(&self, start: isize, pattern: &[u8]) -> bool {
        if start < 0 {
            return false;
        }
        let start = start as usize;
        let len = pattern.len();
        if start + len > self.n {
            return false;
        }
        &self.c[start..start + len] == pattern
    }

    fn matches_any_at(&self, start: isize, options: &[&[u8]]) -> bool {
        options.iter().any(|p| self.matches_at(start, p))
    }
}

/// One or two output bytes to append; `None` means "append nothing" (silent).
struct Emit {
    primary: Option<&'static [u8]>,
    secondary: Option<&'static [u8]>,
}

fn both(s: &'static [u8]) -> Emit {
    Emit {
        primary: Some(s),
        secondary: Some(s),
    }
}

fn silent() -> Emit {
    Emit {
        primary: None,
        secondary: None,
    }
}

fn encode(chars: &[u8]) -> (String, String) {
    let n = chars.len();
    if n == 0 {
        return (String::new(), String::new());
    }
    let buf = Buf { c: chars, n };

    let mut pri: Vec<u8> = Vec::with_capacity(MAX_CODE_LEN);
    let mut sec: Vec<u8> = Vec::with_capacity(MAX_CODE_LEN);

    let mut i: isize = 0;

    // Initial silent-letter combinations (SPEC layer 2's job is to catch what
    // an ASR engine already mis-decoded, so getting these right matters for
    // proper nouns like "Knuth" or "Wright").
    if buf.matches_any_at(0, &[b"GN", b"KN", b"PN", b"WR", b"PS"]) {
        i = 1;
    } else if buf.at(0) == b'X' {
        // "Xavier" -> starts with an S sound.
        pri.push(b'S');
        sec.push(b'S');
        i = 1;
    }

    while i < n as isize && pri.len() < MAX_CODE_LEN && sec.len() < MAX_CODE_LEN {
        let ch = buf.at(i);
        let (emit, advance) = match ch {
            b'A' | b'E' | b'I' | b'O' | b'U' | b'Y' => {
                if i == 0 {
                    (both(b"A"), 1)
                } else {
                    (silent(), 1)
                }
            }
            b'B' => (both(b"P"), if buf.is(i + 1, b'B') { 2 } else { 1 }),
            b'C' => handle_c(&buf, i),
            b'D' => handle_d(&buf, i),
            b'F' => (both(b"F"), if buf.is(i + 1, b'F') { 2 } else { 1 }),
            b'G' => handle_g(&buf, i),
            b'H' => handle_h(&buf, i),
            b'J' => (both(b"J"), if buf.is(i + 1, b'J') { 2 } else { 1 }),
            b'K' => (both(b"K"), if buf.is(i + 1, b'K') { 2 } else { 1 }),
            b'L' => (both(b"L"), if buf.is(i + 1, b'L') { 2 } else { 1 }),
            b'M' => (both(b"M"), if buf.is(i + 1, b'M') { 2 } else { 1 }),
            b'N' => (both(b"N"), if buf.is(i + 1, b'N') { 2 } else { 1 }),
            b'P' => handle_p(&buf, i),
            b'Q' => (both(b"K"), if buf.is(i + 1, b'Q') { 2 } else { 1 }),
            b'R' => (both(b"R"), if buf.is(i + 1, b'R') { 2 } else { 1 }),
            b'S' => handle_s(&buf, i),
            b'T' => handle_t(&buf, i),
            b'V' => (both(b"F"), if buf.is(i + 1, b'V') { 2 } else { 1 }),
            b'W' => handle_w(&buf, i),
            b'X' => (both(b"KS"), if buf.one_of(i + 1, b"XZ") { 2 } else { 1 }),
            b'Z' => (both(b"S"), if buf.is(i + 1, b'Z') { 2 } else { 1 }),
            _ => (silent(), 1),
        };
        if let Some(p) = emit.primary {
            pri.extend_from_slice(p);
        }
        if let Some(s) = emit.secondary {
            sec.extend_from_slice(s);
        }
        i += advance;
    }

    pri.truncate(MAX_CODE_LEN);
    sec.truncate(MAX_CODE_LEN);
    // SAFETY-free: all pushed bytes are ASCII by construction.
    (
        String::from_utf8(pri).unwrap_or_default(),
        String::from_utf8(sec).unwrap_or_default(),
    )
}

fn handle_c(buf: &Buf, i: isize) -> (Emit, isize) {
    if buf.matches_at(i, b"CIA") {
        return (both(b"X"), 3);
    }
    if buf.matches_at(i, b"CH") {
        // Ambiguous: "chip" (X) vs "chorus"/"chemist" (K). Both kept as
        // alternates rather than guessing.
        return (
            Emit {
                primary: Some(b"X"),
                secondary: Some(b"K"),
            },
            2,
        );
    }
    if buf.matches_at(i, b"CK") {
        return (both(b"K"), 2);
    }
    if buf.one_of(i + 1, b"IEY") {
        return (both(b"S"), 1);
    }
    (both(b"K"), if buf.is(i + 1, b'C') { 2 } else { 1 })
}

fn handle_d(buf: &Buf, i: isize) -> (Emit, isize) {
    if buf.matches_at(i, b"DGE") || buf.matches_at(i, b"DGI") || buf.matches_at(i, b"DGY") {
        return (both(b"J"), 2);
    }
    (both(b"T"), if buf.is(i + 1, b'D') { 2 } else { 1 })
}

fn handle_g(buf: &Buf, i: isize) -> (Emit, isize) {
    if buf.matches_at(i, b"GH") {
        return if is_vowel(buf.at(i + 2)) {
            (both(b"K"), 2)
        } else {
            (silent(), 2) // "though", "light" — silent GH
        };
    }
    if buf.is(i + 1, b'N') {
        let word_end = i + 2 >= buf.n as isize;
        if word_end || buf.matches_at(i + 2, b"ED") {
            return (silent(), 2); // "sign", "resigned"
        }
    }
    if buf.one_of(i + 1, b"EIY") {
        return (both(b"J"), 1);
    }
    (both(b"K"), if buf.is(i + 1, b'G') { 2 } else { 1 })
}

fn handle_h(buf: &Buf, i: isize) -> (Emit, isize) {
    let after_vowel_or_start = i == 0 || is_vowel(buf.at(i - 1));
    if after_vowel_or_start && is_vowel(buf.at(i + 1)) {
        (both(b"H"), 1)
    } else {
        (silent(), 1)
    }
}

fn handle_p(buf: &Buf, i: isize) -> (Emit, isize) {
    if buf.is(i + 1, b'H') {
        return (both(b"F"), 2);
    }
    (both(b"P"), if buf.one_of(i + 1, b"PB") { 2 } else { 1 })
}

fn handle_s(buf: &Buf, i: isize) -> (Emit, isize) {
    if buf.matches_at(i, b"SCH") {
        return (
            Emit {
                primary: Some(b"X"),
                secondary: Some(b"SK"),
            },
            3,
        );
    }
    if buf.matches_at(i, b"SH") {
        return (both(b"X"), 2);
    }
    if buf.matches_at(i, b"SIO") || buf.matches_at(i, b"SIA") {
        return (both(b"X"), 3);
    }
    (both(b"S"), if buf.one_of(i + 1, b"SZ") { 2 } else { 1 })
}

fn handle_t(buf: &Buf, i: isize) -> (Emit, isize) {
    if buf.matches_at(i, b"TCH") {
        return (both(b"X"), 3);
    }
    if buf.matches_at(i, b"TIA") || buf.matches_at(i, b"TIO") {
        return (both(b"X"), 3);
    }
    if buf.matches_at(i, b"TH") {
        // '0' stands in for theta, matching the classic algorithm's alphabet;
        // secondary 'T' covers the common mishearing/loan-word collapse.
        return (
            Emit {
                primary: Some(b"0"),
                secondary: Some(b"T"),
            },
            2,
        );
    }
    (both(b"T"), if buf.is(i + 1, b'T') { 2 } else { 1 })
}

fn handle_w(buf: &Buf, i: isize) -> (Emit, isize) {
    if buf.matches_at(i, b"WH") {
        return (both(b"W"), 2);
    }
    if is_vowel(buf.at(i + 1)) {
        return (both(b"W"), 1);
    }
    (silent(), 1) // W before a consonant, mid-word: silent
}

#[cfg(test)]
mod tests {
    use super::*;

    fn primary(word: &str) -> String {
        double_metaphone(word).primary
    }

    #[test]
    fn spec_example_postgres_matches_misheard_postgress() {
        // SPEC.md §3.3 layer 2's own example.
        let a = double_metaphone("Postgres");
        let b = double_metaphone("postgress");
        assert!(a.matches(&b), "{a:?} should match {b:?}");
        assert_eq!(primary("Postgres"), "PSTKRS");
        assert_eq!(primary("postgress"), "PSTKRS");
    }

    #[test]
    fn doubled_consonants_collapse() {
        // "SUMMER" should not double-emit M.
        assert_eq!(primary("SUMMER"), primary("SUMER"));
    }

    #[test]
    fn silent_initial_letters_are_dropped() {
        // WRIGHT: silent W (initial WR), GH silent before T.
        let wright = double_metaphone("Wright");
        let rite = double_metaphone("Rite");
        assert!(
            wright.matches(&rite),
            "{wright:?} should match {rite:?} (classic Metaphone homophone test)"
        );
    }

    #[test]
    fn unrelated_words_do_not_collide() {
        let a = double_metaphone("Postgres");
        let b = double_metaphone("Kubernetes");
        assert!(!a.matches(&b), "{a:?} should not match {b:?}");
    }

    #[test]
    fn empty_input_yields_empty_codes() {
        let code = double_metaphone("");
        assert_eq!(code.primary, "");
        assert_eq!(code.secondary, "");
        let code = double_metaphone("1234"); // no alphabetic content
        assert_eq!(code.primary, "");
    }

    #[test]
    fn ch_digraph_has_primary_and_secondary_alternates() {
        let code = double_metaphone("chip");
        assert_eq!(code.primary, "XP");
        assert_eq!(code.secondary, "KP");
    }

    #[test]
    fn vowel_repetition_does_not_change_the_code() {
        // Establishes the hand-verified fixture used by bias.rs's near-miss
        // test: identical phonetic code, very different spelling.
        assert_eq!(primary("Kafka"), "KFK");
        assert_eq!(primary("Kaaaaafka"), "KFK");
    }

    #[test]
    fn code_length_is_capped() {
        let code = double_metaphone("internationalization");
        assert!(code.primary.len() <= MAX_CODE_LEN);
    }
}
