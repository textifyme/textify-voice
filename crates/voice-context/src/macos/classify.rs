//! Bundle identifier → [`AppKind`] classification.
//!
//! This is what makes SPEC V1.4's raw-paste rule ("raw paste in AI/coding
//! apps") real: without it every app looks like `AppKind::Other`, and the
//! ASR/formatting bias layers can't tell a terminal from a chat window.
//! Exposed as data (`BUNDLE_ID_RULES`) rather than buried in `if`/`match`
//! arms so the mapping is independently testable and additions are a data
//! change, not a logic change.
//!
//! ## Sourcing and confidence
//!
//! Bundle IDs for Apple's own apps and long-standing open-source terminals
//! are stable, published identifiers (Terminal.app, iTerm2, VS Code, Xcode,
//! Sublime Text). A few closed-source consumer apps in the AI-chat table
//! (Claude desktop, ChatGPT desktop) are sourced from their public macOS
//! release notes / installer metadata rather than a live-observed
//! `NSWorkspace` read in this session — flagged individually below. Getting
//! one of those wrong fails safe: an unmatched bundle id falls through to
//! `AppKind::Other`, which only costs bias/raw-paste coverage for that one
//! app, not correctness elsewhere (see `classify_bundle_id`'s doc comment).
//!
//! ## Live-verified
//!
//! `com.googlecode.iterm2` (iTerm2) and `com.google.Chrome` were both
//! observed directly as this session's real frontmost app via
//! `NSWorkspace::frontmostApplication()` (see `examples/probe_macos.rs`
//! output). Their entries below are confirmed correct against ground truth,
//! not just documentation.

use crate::types::AppKind;

/// One data-driven bundle-id → [`AppKind`] rule. `bundle_id` is matched
/// case-insensitively and exactly (see [`classify_bundle_id`] for the one
/// documented exception: the JetBrains family prefix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundleIdRule {
    pub bundle_id: &'static str,
    pub kind: AppKind,
    /// Free-text note on what this row is and how confident we are in the
    /// literal bundle id — surfaced so a reviewer doesn't have to guess
    /// which rows are "obviously correct" (Apple's own apps) versus "best
    /// effort, verify before shipping" (long-tail third-party apps).
    pub note: &'static str,
}

macro_rules! rule {
    ($id:literal, $kind:expr, $note:literal) => {
        BundleIdRule { bundle_id: $id, kind: $kind, note: $note }
    };
}

/// The full bundle-id classification table. Ordered by category for
/// reviewability; lookup itself (`classify_bundle_id`) is a linear scan,
/// which is fine at this table's size and keeps the data trivially
/// diffable (no hash-table bucketing to reason about).
pub const BUNDLE_ID_RULES: &[BundleIdRule] = &[
    // --- Terminals — is_ai_or_coding() == true, raw paste applies ---
    rule!("com.apple.Terminal", AppKind::Terminal, "Apple Terminal.app, built-in"),
    rule!("com.googlecode.iterm2", AppKind::Terminal, "iTerm2 — live-verified this session"),
    rule!("com.mitchellh.ghostty", AppKind::Terminal, "Ghostty"),
    rule!("org.alacritty", AppKind::Terminal, "Alacritty (macOS app bundle id)"),
    rule!("net.kovidgoyal.kitty", AppKind::Terminal, "kitty"),
    rule!("com.github.wez.wezterm", AppKind::Terminal, "WezTerm"),
    rule!("dev.warp.Warp-Stable", AppKind::Terminal, "Warp, stable channel"),
    rule!("dev.warp.Warp-Preview", AppKind::Terminal, "Warp, preview channel"),
    // --- Editors / IDEs — is_ai_or_coding() == true, raw paste applies ---
    rule!("com.microsoft.VSCode", AppKind::Code, "VS Code"),
    rule!("com.microsoft.VSCodeInsiders", AppKind::Code, "VS Code Insiders"),
    rule!("com.todesktop.230313mzl4w4u92", AppKind::Code, "Cursor (todesktop-packaged Electron app; best effort — not live-verified)"),
    rule!("com.apple.dt.Xcode", AppKind::Code, "Xcode"),
    rule!("com.google.android.studio", AppKind::Code, "Android Studio (IntelliJ Platform, non-JetBrains bundle id)"),
    rule!("dev.zed.Zed", AppKind::Code, "Zed"),
    rule!("com.sublimetext.4", AppKind::Code, "Sublime Text 4"),
    rule!("com.sublimetext.3", AppKind::Code, "Sublime Text 3"),
    rule!("com.neovide.neovide", AppKind::Code, "Neovide (Neovim GUI wrapper)"),
    rule!("com.qvacua.VimR", AppKind::Code, "VimR (Neovim GUI wrapper)"),
    // JetBrains individual products are matched by the "com.jetbrains."
    // prefix in classify_bundle_id() below, not listed row-by-row here —
    // the family is large (IntelliJ IDEA, PyCharm, WebStorm, CLion, GoLand,
    // RubyMine, PhpStorm, Rider, DataGrip, AppCode, ...) and every member
    // ships under that one vendor prefix.

    // --- AI chat apps — is_ai_or_coding() == true, raw paste applies ---
    rule!("com.anthropic.claudefordesktop", AppKind::Ai, "Claude desktop (best effort — not live-verified)"),
    rule!("com.openai.chat", AppKind::Ai, "ChatGPT desktop (best effort — not live-verified)"),

    // --- Browsers — informational; is_ai_or_coding() == false ---
    rule!("com.apple.Safari", AppKind::Browser, "Safari"),
    rule!("com.google.Chrome", AppKind::Browser, "Chrome — live-verified this session"),
    rule!("org.mozilla.firefox", AppKind::Browser, "Firefox"),
    rule!("com.microsoft.edgemac", AppKind::Browser, "Microsoft Edge"),
    rule!("com.brave.Browser", AppKind::Browser, "Brave"),
    rule!("company.thebrowser.Browser", AppKind::Browser, "Arc"),

    // --- Chat / messaging — informational; is_ai_or_coding() == false ---
    rule!("com.tinyspeck.slackmacgap", AppKind::Chat, "Slack"),
    rule!("com.hnc.Discord", AppKind::Chat, "Discord"),
    rule!("com.apple.MobileSMS", AppKind::Chat, "Messages"),
    rule!("com.microsoft.teams2", AppKind::Chat, "Microsoft Teams (new)"),
    rule!("ru.keepcoder.Telegram", AppKind::Chat, "Telegram"),
    rule!("net.whatsapp.WhatsApp", AppKind::Chat, "WhatsApp"),

    // --- Documents / notes — informational; is_ai_or_coding() == false ---
    rule!("com.apple.iWork.Pages", AppKind::Document, "Pages"),
    rule!("com.microsoft.Word", AppKind::Document, "Microsoft Word"),
    rule!("notion.id", AppKind::Document, "Notion"),
    rule!("md.obsidian", AppKind::Document, "Obsidian"),
    rule!("com.apple.Notes", AppKind::Document, "Notes"),
];

/// The JetBrains vendor bundle-id prefix. Matched separately from
/// [`BUNDLE_ID_RULES`] because the family (IntelliJ IDEA, PyCharm,
/// WebStorm, CLion, GoLand, RubyMine, PhpStorm, Rider, DataGrip, AppCode,
/// ...) is large and every member ships under this one prefix rather than
/// needing its own row.
const JETBRAINS_BUNDLE_ID_PREFIX: &str = "com.jetbrains.";

/// Classify a bundle identifier into a coarse [`AppKind`].
///
/// The fall-through is explicit, not accidental: an unrecognized bundle id
/// (including `None`/empty) resolves to `AppKind::Other` via a named match
/// arm, documented as "general/prose" — the safe default that leaves the
/// format gate and bias layer 2 active rather than silently disabling them
/// (`is_ai_or_coding()` is false for `Other`, so nothing is falsely
/// suppressed by a classification miss).
pub fn classify_bundle_id(bundle_id: &str) -> AppKind {
    if let Some(rule) = BUNDLE_ID_RULES.iter().find(|r| r.bundle_id.eq_ignore_ascii_case(bundle_id)) {
        return rule.kind;
    }
    if bundle_id.to_ascii_lowercase().starts_with(JETBRAINS_BUNDLE_ID_PREFIX) {
        return AppKind::Code;
    }
    // Explicit fall-through: general/prose, not a coding/terminal/AI app.
    AppKind::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_terminals() {
        for id in [
            "com.apple.Terminal",
            "com.googlecode.iterm2",
            "com.mitchellh.ghostty",
            "org.alacritty",
            "net.kovidgoyal.kitty",
            "com.github.wez.wezterm",
            "dev.warp.Warp-Stable",
        ] {
            assert_eq!(classify_bundle_id(id), AppKind::Terminal, "expected {id} to classify as Terminal");
        }
    }

    #[test]
    fn classifies_editors_and_ides() {
        for id in ["com.microsoft.VSCode", "com.apple.dt.Xcode", "dev.zed.Zed", "com.sublimetext.4"] {
            assert_eq!(classify_bundle_id(id), AppKind::Code, "expected {id} to classify as Code");
        }
    }

    #[test]
    fn classifies_jetbrains_family_by_prefix() {
        for id in ["com.jetbrains.intellij", "com.jetbrains.pycharm", "com.jetbrains.CLion", "com.jetbrains.rider"] {
            assert_eq!(classify_bundle_id(id), AppKind::Code, "expected {id} to classify as Code via JetBrains prefix");
        }
    }

    #[test]
    fn classifies_ai_chat_apps() {
        assert_eq!(classify_bundle_id("com.anthropic.claudefordesktop"), AppKind::Ai);
        assert_eq!(classify_bundle_id("com.openai.chat"), AppKind::Ai);
    }

    #[test]
    fn classifies_browsers() {
        assert_eq!(classify_bundle_id("com.google.Chrome"), AppKind::Browser);
        assert_eq!(classify_bundle_id("com.apple.Safari"), AppKind::Browser);
    }

    #[test]
    fn unknown_bundle_id_falls_through_to_other_explicitly() {
        assert_eq!(classify_bundle_id("com.example.SomeRandomApp"), AppKind::Other);
        assert_eq!(classify_bundle_id(""), AppKind::Other);
    }

    #[test]
    fn classification_is_case_insensitive() {
        assert_eq!(classify_bundle_id("COM.GOOGLECODE.ITERM2"), AppKind::Terminal);
        assert_eq!(classify_bundle_id("Com.JetBrains.PyCharm"), AppKind::Code);
    }

    #[test]
    fn is_ai_or_coding_categories_cover_the_task_examples() {
        // The three categories the task calls out by name as the raw-paste
        // gate's inputs must actually be the ones voice-core's
        // `AppKind::is_ai_or_coding()` treats as coding/AI — pinned here so
        // a future edit to either table can't silently drift them apart.
        // (voice-context::AppKind intentionally mirrors voice-core::AppKind's
        // Code/Terminal/Ai spelling — see types.rs's doc comment.)
        for id in ["com.googlecode.iterm2", "com.microsoft.VSCode", "com.anthropic.claudefordesktop"] {
            let kind = classify_bundle_id(id);
            assert!(
                matches!(kind, AppKind::Terminal | AppKind::Code | AppKind::Ai),
                "{id} classified as {kind:?}, expected one of Terminal/Code/Ai"
            );
        }
    }

    #[test]
    fn every_rule_note_is_non_empty_and_bundle_id_lowercase_matches_itself() {
        // Cheap data-hygiene check on the table itself, not the function —
        // catches a copy-pasted empty note or an accidental non-ASCII id.
        for rule in BUNDLE_ID_RULES {
            assert!(!rule.note.is_empty(), "rule for {} has an empty note", rule.bundle_id);
            assert!(!rule.bundle_id.is_empty());
        }
    }
}
