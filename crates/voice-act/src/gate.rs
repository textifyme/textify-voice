//! The tier gate. COMMANDS-SPEC.md §3.5 #2: T0 execute+undo chip, T1
//! execute+announce, T2 HUD confirm with default-deny on timeout, T3 never.
//!
//! Modeled as pure functions over an explicit `elapsed: Duration` rather
//! than wall-clock time, so the deny-on-timeout path is deterministically
//! unit-tested without sleeping in tests.

use std::time::Duration;

use crate::schema::Tier;

/// COMMANDS-SPEC.md §3.5 #2 / C1.2: T2 confirmations default-deny after 5s.
pub const T2_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);

/// What the tier alone (ignoring confirmation state) prescribes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateAction {
    AutoExecute,
    ExecuteAndAnnounce,
    RequireConfirmation,
    NeverAllowed,
}

/// Pure mapping from tier to gate action. T3 maps to `NeverAllowed`
/// unconditionally -- there is no code path from `T3` to execution.
pub fn gate_for_tier(tier: Tier) -> GateAction {
    match tier {
        Tier::T0 => GateAction::AutoExecute,
        Tier::T1 => GateAction::ExecuteAndAnnounce,
        Tier::T2 => GateAction::RequireConfirmation,
        Tier::T3 => GateAction::NeverAllowed,
    }
}

/// The user's answer to a T2 HUD confirm prompt, if any has arrived yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserResponse {
    Yes,
    No,
}

/// Outcome of evaluating a T2 confirmation at a point in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmOutcome {
    Confirmed,
    Denied,
    Pending,
}

/// Pure state machine for a single T2 confirmation prompt. Holds no clock
/// of its own -- callers pass `elapsed` (time since the prompt opened) and
/// the latest `UserResponse`, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct T2Confirmation {
    pub timeout: Duration,
}

impl Default for T2Confirmation {
    fn default() -> Self {
        Self { timeout: T2_CONFIRM_TIMEOUT }
    }
}

impl T2Confirmation {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    /// Evaluate the confirmation at `elapsed` given the latest response
    /// (`None` if the user hasn't answered yet). Default-deny wins: an
    /// explicit "no" always denies; a "yes" only confirms if it lands
    /// before the timeout; no response at/after the timeout denies.
    pub fn evaluate(&self, elapsed: Duration, response: Option<UserResponse>) -> ConfirmOutcome {
        match response {
            Some(UserResponse::No) => ConfirmOutcome::Denied,
            Some(UserResponse::Yes) if elapsed < self.timeout => ConfirmOutcome::Confirmed,
            // A "yes" that arrives at/after the timeout is too late -- the
            // gate has already closed. Default-deny takes precedence.
            Some(UserResponse::Yes) => ConfirmOutcome::Denied,
            None if elapsed >= self.timeout => ConfirmOutcome::Denied,
            None => ConfirmOutcome::Pending,
        }
    }
}

/// Final decision the pipeline acts on, combining tier + confirmation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    Execute,
    ExecuteAndAnnounce,
    Denied,
    NeverAllowed,
    Pending,
}

/// Decide what to do with a resolved action at `effective_tier`. For `T2`,
/// `confirmation`/`elapsed`/`response` drive the sub-decision; for every
/// other tier they are ignored (in particular `T3` short-circuits to
/// `NeverAllowed` regardless of any confirmation state -- there is no way
/// to "yes" past a T3 refusal).
pub fn decide(
    effective_tier: Tier,
    confirmation: &T2Confirmation,
    elapsed: Duration,
    response: Option<UserResponse>,
) -> GateDecision {
    match gate_for_tier(effective_tier) {
        GateAction::AutoExecute => GateDecision::Execute,
        GateAction::ExecuteAndAnnounce => GateDecision::ExecuteAndAnnounce,
        GateAction::NeverAllowed => GateDecision::NeverAllowed,
        GateAction::RequireConfirmation => match confirmation.evaluate(elapsed, response) {
            ConfirmOutcome::Confirmed => GateDecision::Execute,
            ConfirmOutcome::Denied => GateDecision::Denied,
            ConfirmOutcome::Pending => GateDecision::Pending,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t0_auto_executes() {
        assert_eq!(gate_for_tier(Tier::T0), GateAction::AutoExecute);
    }

    #[test]
    fn t1_executes_and_announces() {
        assert_eq!(gate_for_tier(Tier::T1), GateAction::ExecuteAndAnnounce);
    }

    #[test]
    fn t2_requires_confirmation() {
        assert_eq!(gate_for_tier(Tier::T2), GateAction::RequireConfirmation);
    }

    #[test]
    fn t3_never_allowed() {
        assert_eq!(gate_for_tier(Tier::T3), GateAction::NeverAllowed);
    }

    #[test]
    fn t2_confirm_before_timeout_with_yes_confirms() {
        let c = T2Confirmation::default();
        let outcome = c.evaluate(Duration::from_secs(2), Some(UserResponse::Yes));
        assert_eq!(outcome, ConfirmOutcome::Confirmed);
    }

    #[test]
    fn t2_explicit_no_denies_immediately() {
        let c = T2Confirmation::default();
        let outcome = c.evaluate(Duration::from_millis(1), Some(UserResponse::No));
        assert_eq!(outcome, ConfirmOutcome::Denied);
    }

    #[test]
    fn t2_no_response_before_timeout_is_pending() {
        let c = T2Confirmation::default();
        let outcome = c.evaluate(Duration::from_millis(4999), None);
        assert_eq!(outcome, ConfirmOutcome::Pending);
    }

    #[test]
    fn t2_default_denies_exactly_at_five_second_timeout() {
        let c = T2Confirmation::default();
        assert_eq!(c.timeout, Duration::from_secs(5));
        let outcome = c.evaluate(Duration::from_secs(5), None);
        assert_eq!(outcome, ConfirmOutcome::Denied, "must default-deny at the 5s boundary, not stay pending");
    }

    #[test]
    fn t2_default_denies_after_timeout() {
        let c = T2Confirmation::default();
        let outcome = c.evaluate(Duration::from_secs(30), None);
        assert_eq!(outcome, ConfirmOutcome::Denied);
    }

    #[test]
    fn t2_late_yes_after_timeout_is_still_denied() {
        let c = T2Confirmation::default();
        let outcome = c.evaluate(Duration::from_secs(6), Some(UserResponse::Yes));
        assert_eq!(outcome, ConfirmOutcome::Denied, "a late yes must not override default-deny");
    }

    #[test]
    fn decide_t3_ignores_confirmation_state_entirely() {
        let c = T2Confirmation::default();
        // Even an explicit, timely "yes" must not move a T3 action forward.
        let decision = decide(Tier::T3, &c, Duration::from_millis(1), Some(UserResponse::Yes));
        assert_eq!(decision, GateDecision::NeverAllowed);
    }

    #[test]
    fn decide_t2_full_lifecycle() {
        let c = T2Confirmation::default();
        assert_eq!(decide(Tier::T2, &c, Duration::from_secs(1), None), GateDecision::Pending);
        assert_eq!(
            decide(Tier::T2, &c, Duration::from_secs(1), Some(UserResponse::Yes)),
            GateDecision::Execute
        );
        assert_eq!(
            decide(Tier::T2, &c, Duration::from_secs(1), Some(UserResponse::No)),
            GateDecision::Denied
        );
        assert_eq!(decide(Tier::T2, &c, Duration::from_secs(5), None), GateDecision::Denied);
    }

    #[test]
    fn decide_t0_and_t1_ignore_elapsed_and_response() {
        let c = T2Confirmation::default();
        assert_eq!(decide(Tier::T0, &c, Duration::from_secs(999), Some(UserResponse::No)), GateDecision::Execute);
        assert_eq!(
            decide(Tier::T1, &c, Duration::from_secs(999), Some(UserResponse::No)),
            GateDecision::ExecuteAndAnnounce
        );
    }
}
