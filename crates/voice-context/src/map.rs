//! The in-memory actionable-element map and the query helpers that turn it
//! into bias terms and label lookups for `voice-act`.
//!
//! COMMANDS-SPEC §3.2/§3.3: "actionable element map"; "click Send" with two
//! Send buttons must surface a near-tie, never pick silently.

use crate::types::{ActionableElement, BiasTerm, Coverage, DegradedReason, ElementRole};

/// The in-memory actionable-element map for the focused window.
/// COMMANDS-SPEC §3.2: "actionable element map (role, label, position,
/// writable, secure)".
///
/// Memory-only — see the invariant documented on [`crate::types::ActionableElement`].
/// This type carries no serialization/persistence impl either; it is a thin
/// `Vec` wrapper plus a [`Coverage`] report.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionableMap {
    elements: Vec<ActionableElement>,
    coverage: Coverage,
}

/// A candidate returned by [`ActionableMap::find_by_label`]: the element's
/// position in the map plus enough identity to render in a HUD numbered
/// list. COMMANDS-SPEC §3.3: "HUD numbers just those candidates and user
/// says the number."
#[derive(Debug, Clone, PartialEq)]
pub struct LabelCandidate {
    pub index: usize,
    pub label: String,
    pub role: ElementRole,
}

/// Result of a label lookup. COMMANDS-SPEC §3.3: "Resolution never picks
/// silently among near-ties; when multiple candidates exist ... HUD numbers
/// just those candidates and user says the number."
#[derive(Debug, Clone, PartialEq)]
pub enum LabelMatch {
    /// Exactly one element matched — safe to act on directly.
    Unique(LabelCandidate),
    /// More than one element matched (case-insensitive label equality) — the
    /// caller (voice-act) must disambiguate, never guess.
    NearTie(Vec<LabelCandidate>),
    /// No element matched.
    NoMatch,
}

impl ActionableMap {
    /// Build a map from an already-read element list plus its coverage report.
    pub fn new(elements: Vec<ActionableElement>, coverage: Coverage) -> Self {
        Self { elements, coverage }
    }

    /// An empty map with `Coverage::Unavailable` — the honest zero value for
    /// "we have not read anything yet" (e.g. before the first async capture
    /// resolves).
    pub fn unavailable(reason: DegradedReason) -> Self {
        Self { elements: Vec::new(), coverage: Coverage::Unavailable { reason } }
    }

    pub fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    pub fn elements(&self) -> &[ActionableElement] {
        &self.elements
    }

    pub fn len(&self) -> usize {
        self.elements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// Element at `index`, as referenced by a [`LabelCandidate`].
    pub fn get(&self, index: usize) -> Option<&ActionableElement> {
        self.elements.get(index)
    }

    /// Find elements by case-insensitive exact label match, reporting a
    /// near-tie when more than one element shares the label (the classic
    /// "two Send buttons" case from COMMANDS-SPEC §3.3).
    pub fn find_by_label(&self, query: &str) -> LabelMatch {
        let mut matches: Vec<LabelCandidate> = self
            .elements
            .iter()
            .enumerate()
            .filter(|(_, el)| el.label.eq_ignore_ascii_case(query))
            .map(|(index, el)| LabelCandidate { index, label: el.label.clone(), role: el.role.clone() })
            .collect();

        match matches.len() {
            0 => LabelMatch::NoMatch,
            1 => LabelMatch::Unique(matches.remove(0)),
            _ => LabelMatch::NearTie(matches),
        }
    }

    /// Turn the map's visible labels into bias terms for the ASR bias
    /// pipeline (SPEC §3.3 layer 1/2 input; COMMANDS-SPEC §3.1 "CommandBias
    /// = ... focused-window AX labels"). Deduplicates labels and skips
    /// secure elements — screen content behind secure input must never leak
    /// into biasing, mirroring the secure-context refusal elsewhere in the
    /// spec family.
    pub fn bias_terms(&self) -> Vec<BiasTerm> {
        let mut seen: Vec<String> = Vec::new();
        let mut terms = Vec::new();
        for el in &self.elements {
            if el.secure || el.label.trim().is_empty() {
                continue;
            }
            let key = el.label.to_ascii_lowercase();
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            terms.push(BiasTerm { text: el.label.clone(), weight: 1.0 });
        }
        terms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Position;

    fn el(role: ElementRole, label: &str, secure: bool) -> ActionableElement {
        ActionableElement {
            role,
            label: label.to_string(),
            position: Position { x: 0.0, y: 0.0, width: 1.0, height: 1.0 },
            writable: false,
            secure,
            enabled: true,
        }
    }

    #[test]
    fn find_by_label_unique_match() {
        let map = ActionableMap::new(
            vec![el(ElementRole::Button, "Send", false), el(ElementRole::Button, "Cancel", false)],
            Coverage::Full,
        );
        match map.find_by_label("send") {
            LabelMatch::Unique(c) => {
                assert_eq!(c.index, 0);
                assert_eq!(c.label, "Send");
            }
            other => panic!("expected Unique, got {other:?}"),
        }
    }

    #[test]
    fn find_by_label_near_tie_never_picks_silently() {
        // COMMANDS-SPEC §3.3's own example: two "Send" buttons.
        let map = ActionableMap::new(
            vec![
                el(ElementRole::Button, "Send", false),
                el(ElementRole::Button, "Reply", false),
                el(ElementRole::Button, "Send", false),
            ],
            Coverage::Full,
        );
        match map.find_by_label("Send") {
            LabelMatch::NearTie(candidates) => {
                assert_eq!(candidates.len(), 2);
                assert_eq!(candidates[0].index, 0);
                assert_eq!(candidates[1].index, 2);
            }
            other => panic!("expected NearTie, got {other:?}"),
        }
    }

    #[test]
    fn find_by_label_no_match() {
        let map = ActionableMap::new(vec![el(ElementRole::Button, "Cancel", false)], Coverage::Full);
        assert_eq!(map.find_by_label("Send"), LabelMatch::NoMatch);
    }

    #[test]
    fn bias_terms_dedupes_and_excludes_secure_elements() {
        let map = ActionableMap::new(
            vec![
                el(ElementRole::Button, "Send", false),
                el(ElementRole::Button, "send", false), // dup, case-insensitive
                el(ElementRole::TextField, "Password", true), // secure — must not leak
                el(ElementRole::TextField, "", false),        // blank label — skipped
            ],
            Coverage::Full,
        );
        let terms = map.bias_terms();
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0].text, "Send");
    }

    #[test]
    fn unavailable_map_is_empty_and_reports_reason() {
        let map = ActionableMap::unavailable(DegradedReason::NoAccessibilityPermission);
        assert!(map.is_empty());
        assert_eq!(map.coverage(), &Coverage::Unavailable { reason: DegradedReason::NoAccessibilityPermission });
    }
}
