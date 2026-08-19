//! `textify-voice command "<utterance>"` — DRY RUN ONLY. Demonstrates the
//! whole Command Mode spine (voice-intent's stage-1 grammar -> voice-act's
//! resolve() -> the tier gate) safely: no OS action is ever taken. This
//! module never constructs anything that could execute -- there is no
//! `Authorized` token minted here, only `gate::decide`, which is a pure
//! function over (tier, confirmation-state) and performs no action itself.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;

use voice_act::mock::MockDesktopExecutor;
use voice_act::target::{ActionableElement, ActionableMap, ElementRole};
use voice_act::undo::{NoopPersistence, UndoJournal};
use voice_act::{gate, ActionExecutor, Resolution};
use voice_intent::{match_utterance, CommandContext, IntentResult};

use crate::common::split_bias_terms;

#[derive(Args, Debug)]
pub struct CommandArgs {
    /// The utterance to run through the command spine, e.g. "open Slack".
    pub utterance: String,

    /// Comma-separated app names the utterance's AppRef slot may resolve
    /// against (stands in for `voice-context`'s real installed-app list).
    #[arg(long, value_delimiter = ',')]
    pub apps: Vec<String>,

    /// Comma-separated on-screen element labels the utterance's ElementRef
    /// slot may resolve against (stands in for the real focused-window
    /// actionable-element map).
    #[arg(long, value_delimiter = ',')]
    pub labels: Vec<String>,

    /// Comma-separated user Shortcut names the utterance's ShortcutName
    /// slot may resolve against.
    #[arg(long, value_delimiter = ',')]
    pub shortcuts: Vec<String>,
}

pub fn run(args: CommandArgs) -> Result<()> {
    println!("=== DRY RUN -- no application, window, or OS state will be changed ===");
    println!("utterance : {:?}", args.utterance);

    let apps = split_bias_terms(&args.apps);
    let labels = split_bias_terms(&args.labels);
    let shortcuts = split_bias_terms(&args.shortcuts);

    println!("known_apps      : {apps:?}");
    println!("known_elements  : {labels:?}");
    println!("known_shortcuts : {shortcuts:?}");
    println!();

    let ctx = CommandContext {
        known_apps: apps.clone(),
        known_elements: labels.clone(),
        known_shortcuts: shortcuts.clone(),
    };

    let intent = match_utterance(&args.utterance, &ctx);
    let action = match intent {
        IntentResult::Reject { reason } => {
            println!("stage 1 (grammar) : REJECTED -- reason = {reason}");
            println!();
            println!("result: REJECTED. Nothing was matched, nothing was resolved, nothing was executed.");
            return Ok(());
        }
        IntentResult::Matched { action, stage, confidence } => {
            println!("stage 1 (grammar) : MATCHED");
            println!("  schema_id  : {}", action.schema_id);
            println!("  match stage: {stage:?}");
            println!("  confidence : {confidence:.2}");
            action
        }
    };

    let act_instance = convert_instance(&action).context(
        "this dry-run CLI cannot express one of the parsed slot values in voice-act's slot \
         vocabulary (see command.rs's Direction conversion note)",
    )?;

    let mut elements = Vec::new();
    for (i, app) in apps.iter().enumerate() {
        elements.push(ActionableElement::new(format!("app-{i}"), ElementRole::App, app.clone()));
    }
    for (i, label) in labels.iter().enumerate() {
        elements.push(ActionableElement::new(format!("el-{i}"), ElementRole::Button, label.clone()));
    }
    for (i, sc) in shortcuts.iter().enumerate() {
        elements.push(ActionableElement::new(format!("sc-{i}"), ElementRole::Shortcut, sc.clone()));
    }
    let map = ActionableMap::new(elements);

    let journal = Rc::new(RefCell::new(UndoJournal::new(NoopPersistence)));
    let executor = MockDesktopExecutor::new(journal);
    let resolution = executor.resolve(&act_instance, &map);

    println!();
    match &resolution {
        Resolution::Bound { target, effective_tier, .. } => {
            println!("resolve()  : BOUND");
            println!(
                "  target     : id={:?} label={:?} secure={}",
                target.element_id, target.label, target.secure
            );
            println!("  effective tier: {effective_tier:?}");

            let confirmation = gate::T2Confirmation::default();
            // Dry run: zero elapsed time, no user response yet -- exactly
            // the instant a real HUD would just be showing the prompt (or,
            // for T0/T1, the instant it would already be done).
            let decision = gate::decide(*effective_tier, &confirmation, Duration::ZERO, None);
            println!();
            print_decision(decision);
        }
        Resolution::NeedsDisambiguation { candidates, .. } => {
            println!("resolve()  : NEEDS DISAMBIGUATION ({} near-tied candidates)", candidates.len());
            for c in candidates {
                println!("  - {:?}  role={:?}  id={}", c.label, c.role, c.element_id);
            }
            println!();
            println!(
                "gate decision: PENDING (HUD would list exactly these candidates and wait for a pick) -- NOTHING EXECUTED."
            );
        }
        Resolution::Refused { reason, .. } => {
            println!("resolve()  : REFUSED -- reason = {reason:?}");
            println!();
            println!("gate decision: REFUSED -- NOTHING EXECUTED.");
        }
    }

    println!();
    println!("=== dry run complete -- no application, window, or OS state was changed ===");
    Ok(())
}

fn print_decision(decision: gate::GateDecision) {
    match decision {
        gate::GateDecision::Execute => {
            println!("gate decision: EXECUTE (T0 auto-execute, or a T2 confirmed within the timeout)");
            println!("  [dry run -- no Authorized token was minted, nothing was executed]");
        }
        gate::GateDecision::ExecuteAndAnnounce => {
            println!("gate decision: EXECUTE_AND_ANNOUNCE (T1 -- disruptive-but-recoverable)");
            println!("  [dry run -- no Authorized token was minted, nothing was executed]");
        }
        gate::GateDecision::Pending => {
            println!("gate decision: REQUIRE_CONFIRM (T2 -- consequential)");
            println!(
                "  a real run would show a HUD confirm prompt (\"say yes / no\") and default-deny \
                 after {:?} with no response -- NOTHING EXECUTED.",
                gate::T2_CONFIRM_TIMEOUT
            );
        }
        gate::GateDecision::Denied => {
            println!("gate decision: DENIED -- NOTHING EXECUTED.");
        }
        gate::GateDecision::NeverAllowed => {
            println!("gate decision: NEVER_ALLOWED (T3) -- NOTHING EXECUTED, unconditionally.");
        }
    }
}

/// `voice-intent::Direction` has three variants (`Top`/`Bottom`/`Back`) that
/// `voice-act::Direction` does not, because the *only* Direction-slotted
/// schema in the live registry (`nav.scroll`) only ever produces `Up`/`Down`
/// from the grammar's own `UP_DOWN` table (see `voice-intent`'s grammar
/// module) -- `Top`/`Bottom` bind to the separate NO_SLOTS `nav.scroll_to_top`
/// schema instead, and nothing in the closed registry uses `Back`. This
/// conversion is total over what the grammar can actually produce; the
/// `None` arm exists only so a future grammar/registry change that *did*
/// start emitting one of these can't silently misconvert into the wrong
/// direction -- it fails loudly here instead.
fn convert_direction(d: voice_intent::Direction) -> Option<voice_act::Direction> {
    use voice_act::Direction as Act;
    use voice_intent::Direction as Intent;
    match d {
        Intent::Up => Some(Act::Up),
        Intent::Down => Some(Act::Down),
        Intent::Left => Some(Act::Left),
        Intent::Right => Some(Act::Right),
        Intent::Next => Some(Act::Next),
        Intent::Previous => Some(Act::Previous),
        Intent::Top | Intent::Bottom | Intent::Back => None,
    }
}

fn convert_slot(s: &voice_intent::SlotValue) -> Result<voice_act::SlotValue> {
    use voice_act::SlotValue as Act;
    use voice_intent::SlotValue as Intent;
    Ok(match s {
        Intent::AppRef(v) => Act::AppRef(v.clone()),
        Intent::ElementRef(v) => Act::ElementRef(v.clone()),
        Intent::Ordinal(v) => Act::Ordinal(*v),
        Intent::Percentage(v) => Act::Percentage(*v),
        Intent::ShortcutName(v) => Act::ShortcutName(v.clone()),
        Intent::Direction(d) => Act::Direction(
            convert_direction(*d)
                .ok_or_else(|| anyhow::anyhow!("direction {d:?} has no voice-act equivalent"))?,
        ),
    })
}

fn convert_instance(a: &voice_intent::ActionInstance) -> Result<voice_act::ActionInstance> {
    let slots = a.slots.iter().map(convert_slot).collect::<Result<Vec<_>>>()?;
    Ok(voice_act::ActionInstance::new(a.schema_id, slots))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn convert_direction_maps_every_voice_act_direction() {
        use voice_act::Direction as Act;
        use voice_intent::Direction as Intent;
        assert_eq!(convert_direction(Intent::Up), Some(Act::Up));
        assert_eq!(convert_direction(Intent::Down), Some(Act::Down));
        assert_eq!(convert_direction(Intent::Left), Some(Act::Left));
        assert_eq!(convert_direction(Intent::Right), Some(Act::Right));
        assert_eq!(convert_direction(Intent::Next), Some(Act::Next));
        assert_eq!(convert_direction(Intent::Previous), Some(Act::Previous));
    }

    #[test]
    fn convert_direction_has_no_equivalent_for_top_bottom_back() {
        use voice_intent::Direction as Intent;
        assert_eq!(convert_direction(Intent::Top), None);
        assert_eq!(convert_direction(Intent::Bottom), None);
        assert_eq!(convert_direction(Intent::Back), None);
    }

    #[test]
    fn convert_instance_carries_schema_id_and_slots_through() {
        let intent_instance = voice_intent::ActionInstance {
            schema_id: "app.open",
            slots: vec![voice_intent::SlotValue::AppRef("Slack".to_string())],
        };
        let act_instance = convert_instance(&intent_instance).expect("AppRef must convert");
        assert_eq!(act_instance.schema_id, "app.open");
        assert_eq!(act_instance.slots, vec![voice_act::SlotValue::AppRef("Slack".to_string())]);
    }

    #[test]
    fn convert_instance_errors_on_unmappable_direction_rather_than_panicking() {
        let intent_instance = voice_intent::ActionInstance {
            schema_id: "nav.scroll",
            slots: vec![voice_intent::SlotValue::Direction(voice_intent::Direction::Top)],
        };
        assert!(convert_instance(&intent_instance).is_err());
    }
}
