//! The sole path from a `Resolution` to something `ActionExecutor::execute`
//! will accept. COMMANDS-SPEC.md §3.5 #2: T0 auto-execute, T1
//! execute+announce, T2 confirm-with-default-deny-on-timeout, T3 never.
//!
//! Before this module existed, `gate::decide` was reachable from exactly
//! one place in the whole workspace -- a test assertion -- and
//! `execute()`/`guarded_execute()` accepted a bare `Resolution` plus
//! caller-supplied `is_secure`/`never_allowed` booleans that nothing forced
//! any caller to actually derive from the gate. A T2 `Bound` (or a
//! hand-forged T3 one) would execute immediately, with no confirmation,
//! because nothing in the type system required one.
//!
//! [`Authorized`] makes that unrepresentable. It has no public fields, no
//! `Default`, and no `Clone`/`Copy` impl, and its only constructor is
//! [`authorize`], which computes tier and secure-ness *from the
//! `Resolution` itself* (never from a caller-supplied flag) and runs them
//! through [`crate::gate::decide`]. `ActionExecutor::execute` takes
//! `&Authorized`, not `&Resolution` -- so there is no way to call it for a
//! `T2` action without a confirmation that actually arrived within the
//! timeout, and no way to call it at all for `T3` (`gate::decide` never
//! returns an `Execute*` outcome for `T3`, so [`authorize`] never returns
//! `Ok` for one -- see the `t3_has_no_ok_path_under_any_confirmation_state`
//! test below for that claim exercised exhaustively).

use std::time::Duration;

use crate::gate::{self, GateDecision, T2Confirmation, UserResponse};
use crate::resolution::Resolution;

/// Proof that a `Resolution` has been run through [`gate::decide`] and
/// cleared for execution. The only way to obtain one is [`authorize`] --
/// there is no public field to populate by hand, no `Default::default()`,
/// and no `Clone`/`Copy` impl that could launder a token minted for one
/// tier/confirmation state into standing in for another. A token borrows
/// the `Resolution` it was authorized against, so it cannot outlive it and
/// cannot be swapped for a different `Resolution` after the fact.
#[derive(Debug)]
pub struct Authorized<'a> {
    resolution: &'a Resolution,
}

impl<'a> Authorized<'a> {
    /// The `Resolution` (always `Bound`, and never for a secure target --
    /// [`authorize`] refuses both cases before a token can be minted) this
    /// token authorizes execution of.
    pub fn resolution(&self) -> &'a Resolution {
        self.resolution
    }
}

/// Why [`authorize`] declined to mint an [`Authorized`] token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizeError {
    /// `resolution` was `NeedsDisambiguation` or `Refused` -- there is no
    /// bound target and therefore no tier to gate at all.
    NotBound,
    /// The bound target is a secure-context element. Derived from the
    /// `Resolution` itself (`BoundTarget::secure`), never from a
    /// caller-supplied flag -- see `resolution::BoundTarget::secure`'s doc.
    SecureContext,
    /// `effective_tier` is `T3`: never allowed, unconditionally, regardless
    /// of any confirmation state.
    NeverAllowed,
    /// A `T2` confirmation was explicitly denied, or arrived at/after the
    /// timeout (default-deny).
    Denied,
    /// A `T2` confirmation has neither been answered nor timed out yet.
    Pending,
}

/// The only function in this crate that can construct an [`Authorized`].
/// Derives `effective_tier` and secure-ness from `resolution` itself (never
/// from a caller-supplied boolean), and defers the tier/confirmation
/// decision entirely to [`gate::decide`] -- this function does not
/// reimplement or duplicate any part of the gate's logic, it only adapts
/// `gate::decide`'s [`GateDecision`] into a mint-or-refuse outcome.
pub fn authorize<'a>(
    resolution: &'a Resolution,
    confirmation: &T2Confirmation,
    elapsed: Duration,
    response: Option<UserResponse>,
) -> Result<Authorized<'a>, AuthorizeError> {
    let (tier, secure) = match resolution {
        Resolution::Bound { effective_tier, target, .. } => (*effective_tier, target.secure),
        Resolution::NeedsDisambiguation { .. } | Resolution::Refused { .. } => {
            return Err(AuthorizeError::NotBound)
        }
    };
    if secure {
        return Err(AuthorizeError::SecureContext);
    }
    match gate::decide(tier, confirmation, elapsed, response) {
        GateDecision::Execute | GateDecision::ExecuteAndAnnounce => Ok(Authorized { resolution }),
        GateDecision::Denied => Err(AuthorizeError::Denied),
        GateDecision::NeverAllowed => Err(AuthorizeError::NeverAllowed),
        GateDecision::Pending => Err(AuthorizeError::Pending),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::resolution::BoundTarget;
    use crate::schema::{ActionInstance, Tier};

    fn bound(tier: Tier) -> Resolution {
        Resolution::Bound {
            instance: ActionInstance::new("ui.click", vec![]),
            target: BoundTarget { element_id: Some("btn".into()), label: Some("OK".into()), secure: false },
            effective_tier: tier,
        }
    }

    fn secure_bound(tier: Tier) -> Resolution {
        Resolution::Bound {
            instance: ActionInstance::new("ui.click", vec![]),
            target: BoundTarget { element_id: Some("pw".into()), label: Some("Password".into()), secure: true },
            effective_tier: tier,
        }
    }

    // --- Finding 1, invariant 1: a T2 Bound cannot reach an Authorized
    // token without confirmation ------------------------------------------

    #[test]
    fn t2_without_any_response_does_not_authorize() {
        let r = bound(Tier::T2);
        let c = T2Confirmation::default();
        let result = authorize(&r, &c, Duration::from_secs(1), None);
        assert_eq!(result.err(), Some(AuthorizeError::Pending), "no confirmation yet must not authorize");
    }

    #[test]
    fn t2_with_explicit_no_does_not_authorize() {
        let r = bound(Tier::T2);
        let c = T2Confirmation::default();
        let result = authorize(&r, &c, Duration::from_millis(1), Some(UserResponse::No));
        assert_eq!(result.err(), Some(AuthorizeError::Denied));
    }

    #[test]
    fn t2_with_timely_yes_authorizes() {
        let r = bound(Tier::T2);
        let c = T2Confirmation::default();
        let authorized = authorize(&r, &c, Duration::from_secs(1), Some(UserResponse::Yes))
            .expect("a timely explicit yes must authorize a T2 action");
        // The token really does carry the same resolution through, not a
        // detached copy or a stand-in.
        assert_eq!(authorized.resolution(), &r);
    }

    // --- Finding 1, invariant 2: timeout means deny, not authorize --------

    #[test]
    fn t2_timeout_means_deny_not_authorize() {
        let r = bound(Tier::T2);
        let c = T2Confirmation::default();
        // At/after the 5s default timeout with no response.
        let result = authorize(&r, &c, Duration::from_secs(5), None);
        assert_eq!(result.err(), Some(AuthorizeError::Denied), "must default-deny at the timeout boundary");
        let result = authorize(&r, &c, Duration::from_secs(30), None);
        assert_eq!(result.err(), Some(AuthorizeError::Denied));
    }

    #[test]
    fn t2_late_yes_after_timeout_does_not_authorize() {
        let r = bound(Tier::T2);
        let c = T2Confirmation::default();
        let result = authorize(&r, &c, Duration::from_secs(6), Some(UserResponse::Yes));
        assert_eq!(result.err(), Some(AuthorizeError::Denied), "a late yes must not override default-deny");
    }

    // --- Finding 1, invariant 3: T3 has no execution path that returns Ok -

    #[test]
    fn t3_has_no_ok_path_under_any_confirmation_state() {
        let r = bound(Tier::T3);
        let c = T2Confirmation::default();
        // Exhaustively try every response/elapsed combination a caller
        // could construct, including an explicit, timely "yes" -- the
        // strongest case for accidentally authorizing. None of them may
        // ever produce Ok.
        for response in [None, Some(UserResponse::Yes), Some(UserResponse::No)] {
            for elapsed_secs in [0, 1, 4, 5, 6, 30, 999] {
                let result = authorize(&r, &c, Duration::from_secs(elapsed_secs), response);
                assert_eq!(
                    result.err(),
                    Some(AuthorizeError::NeverAllowed),
                    "T3 must never authorize (response={response:?}, elapsed={elapsed_secs}s)"
                );
            }
        }
    }

    #[test]
    fn t0_and_t1_authorize_regardless_of_confirmation_state() {
        let c = T2Confirmation::default();
        for tier in [Tier::T0, Tier::T1] {
            let r = bound(tier);
            // Even an explicit "no" and a huge elapsed time must not block
            // a T0/T1 action -- confirmation state is irrelevant below T2.
            assert!(authorize(&r, &c, Duration::from_secs(999), Some(UserResponse::No)).is_ok());
            assert!(authorize(&r, &c, Duration::ZERO, None).is_ok());
        }
    }

    // --- Finding 1, invariant 4: gate::decide is the sole determiner ------

    #[test]
    fn authorize_outcome_always_matches_gate_decide_directly() {
        // Property check across every tier x every confirmation state:
        // authorize()'s Ok/Err split must be *exactly* what calling
        // gate::decide with the same inputs says, for a non-secure Bound.
        // This is the executable form of "gate::decide is on the only
        // path" -- authorize() adds no extra logic of its own that could
        // diverge from the gate.
        let c = T2Confirmation::default();
        let tiers = [Tier::T0, Tier::T1, Tier::T2, Tier::T3];
        let responses = [None, Some(UserResponse::Yes), Some(UserResponse::No)];
        let elapsed_values = [0u64, 1, 4, 5, 6, 30];

        for tier in tiers {
            let r = bound(tier);
            for response in responses {
                for elapsed_secs in elapsed_values {
                    let elapsed = Duration::from_secs(elapsed_secs);
                    let direct = gate::decide(tier, &c, elapsed, response);
                    let via_authorize = authorize(&r, &c, elapsed, response);
                    match direct {
                        GateDecision::Execute | GateDecision::ExecuteAndAnnounce => {
                            assert!(via_authorize.is_ok(), "gate::decide said execute for {tier:?}/{response:?}/{elapsed_secs}s but authorize() refused: {via_authorize:?}");
                        }
                        GateDecision::Denied => {
                            assert_eq!(via_authorize.err(), Some(AuthorizeError::Denied));
                        }
                        GateDecision::NeverAllowed => {
                            assert_eq!(via_authorize.err(), Some(AuthorizeError::NeverAllowed));
                        }
                        GateDecision::Pending => {
                            assert_eq!(via_authorize.err(), Some(AuthorizeError::Pending));
                        }
                    }
                }
            }
        }
    }

    // --- Finding 1: secure-ness is derived from the Resolution, never a
    // caller-supplied flag --------------------------------------------------

    #[test]
    fn secure_bound_never_authorizes_at_any_tier() {
        let c = T2Confirmation::default();
        for tier in [Tier::T0, Tier::T1, Tier::T2] {
            let r = secure_bound(tier);
            let result = authorize(&r, &c, Duration::from_secs(1), Some(UserResponse::Yes));
            assert_eq!(result.err(), Some(AuthorizeError::SecureContext), "{tier:?} secure target must refuse even with an affirmative confirmation");
        }
    }

    // --- Finding 1: non-Bound resolutions never authorize -----------------

    #[test]
    fn needs_disambiguation_and_refused_never_authorize() {
        use crate::resolution::RefusalReason;
        let c = T2Confirmation::default();
        let instance = ActionInstance::new("ui.click", vec![]);

        let needs_disambig = Resolution::NeedsDisambiguation { instance: instance.clone(), candidates: vec![] };
        assert_eq!(
            authorize(&needs_disambig, &c, Duration::ZERO, Some(UserResponse::Yes)).err(),
            Some(AuthorizeError::NotBound)
        );

        let refused = Resolution::Refused { instance, reason: RefusalReason::NotFound };
        assert_eq!(
            authorize(&refused, &c, Duration::ZERO, Some(UserResponse::Yes)).err(),
            Some(AuthorizeError::NotBound)
        );
    }
}
