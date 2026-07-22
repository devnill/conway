//! The CLI's exit-code contract (WI-111).
//!
//! [`ExitCode`] is the single vocabulary every command path reports
//! through: `main` never returns early with a bare `std::process::exit`
//! call for anything reachable from a live run, it always produces one of
//! these variants and converts it to a process exit status in one place.
//!
//! **Reconciliation, disclosed:** the binding implementation notes' exit
//! code table has two rows this module cannot classify precisely against
//! the currently-committed `conway`/`conway-core` types, and a third that
//! this module's *pure functions* cannot resolve alone. All three are
//! explained here rather than worked around with a silent approximation:
//!
//! 1. **"`RoutingError::NoCandidate{..}` or fallback chain exhausted... ->
//!    4."** The `conway` facade re-exports `ConwayError` but not the
//!    `conway_core::error::{RoutingError, RuntimeError}` types nested
//!    inside its `Routing`/`Runtime` variants (see `crates/conway/src/lib.rs`'s
//!    re-export list) -- and this crate's manifest is machine-checked to
//!    depend on no other workspace crate (`tests/cli_surface.rs::no_forbidden_deps`),
//!    so `from_error` cannot name those inner types to match on them
//!    directly. Both live construction sites for this row
//!    (`conway-routing/src/router.rs`'s direct `NoCandidate`, and
//!    `conway-runtime/src/attempt.rs:324`'s fallback-exhaustion path, which
//!    wraps the same `RoutingError::NoCandidate` in `RuntimeError::Routing`)
//!    preserve the `Display` substring `"no candidate for role"` through
//!    every wrapping layer (some layers add a prefix -- e.g.
//!    `RuntimeError::Routing` is `#[error("routing error: {0}")]` -- but the
//!    inner wording is always carried through intact).
//!    `classify_runtime_or_routing` below matches on that substring (a
//!    `contains`, not an exact match) rather than the type, which is the
//!    only classification
//!    mechanism available without adding a new dependency or reaching
//!    outside this item's file scope into `crates/conway/src/error.rs`. A
//!    future facade change adding classifier methods on `ConwayError`
//!    itself (e.g. `is_no_candidate()`) would let this module drop the
//!    string match; flagged as a follow-up, not fixed here (out of scope).
//! 2. **"`ConwayError` whose terminal cause is a permission denial (hard
//!    `Deny`, not `DenyWithFeedback`) -> 3."** Traced through
//!    `conway-runtime/src/permission.rs`'s `PermissionOutcome::from` (both
//!    `PermissionDecision::Deny` and `::DenyWithFeedback` collapse to the
//!    same `PermissionOutcome::Deny`) and `conway-runtime/src/tools/runner.rs:263-264`
//!    (every `PermissionOutcome::Deny` becomes a model-visible
//!    `ToolOutcome::error`, fed back into the agent's own turn -- never a
//!    terminal `ConwayError`): under the currently-committed runtime, a
//!    permission denial of either kind cannot reach `from_error` at all.
//!    `ToolError::Denied` (the one variant whose name suggests this case)
//!    is declared in `conway-core` but constructed nowhere in the workspace.
//!    This row is therefore unreachable from any live code path today, not
//!    merely hard to classify; `from_error`'s `ConwayError::Runtime(_)`
//!    default (`AgentFailed`) is what every actually-reachable `Runtime`
//!    cause maps to. Producing `PermissionDenied` requires either the
//!    runtime to add a terminal permission-escalation path or the table to
//!    be reconciled against what the architecture actually does -- an
//!    architectural decision outside a CLI work item's remit, flagged for
//!    the module owner rather than papered over with a speculative,
//!    untestable string match.
//! 3. **"`ResultStatus::Cancelled{..}` and a SIGINT was observed -> 130."**
//!    `AgentResult` carries no SIGINT flag -- that state lives in the
//!    caller's `signal::SigintWatch` (WI-112, not yet built). `from_result`
//!    alone (its signature fixed by this item's own criteria to take only
//!    `&AgentResult`) cannot see it, so it always reports `AgentFailed` for
//!    a bare `Cancelled`. [`ExitCode::from_result_with_sigint`] is one
//!    small addition beyond the three criterion-listed methods, added
//!    specifically so (a) WI-112's `oneshot::run` has one call that already
//!    implements the "SIGINT outranks a status the runtime produced
//!    *because of* the interrupt" precedence rule instead of reimplementing
//!    it inline, and (b) this row has something unit-testable to assert
//!    against, per this item's own "one assertion per row" criterion.
//!
//! Row "(non-permission cause)" on `ResultStatus::Failed{error}` is
//! similarly disclosed as vacuous under the committed type: `error` is a
//! bare `String` with no structured cause, so there is no mechanism to
//! split out a permission sub-cause here either -- every `Failed` maps to
//! `AgentFailed`.

use conway::{AgentResult, ConwayError, ResultStatus};

/// The CLI's process exit status vocabulary. Discriminants are the
/// contract: `code()` casts `self` directly to `i32`, so these values are
/// load-bearing, not incidental.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitCode {
    // Only `AgentFailed` and `Usage` are constructed by production code
    // today (every WI-111 stub returns `Usage`; `main`'s `from_error`
    // fallback is `AgentFailed`). The rest are consumed by WI-112's
    // streaming renderers and SIGINT handling and WI-113's per-code
    // integration tests, not yet landed.
    #[allow(dead_code)]
    Completed = 0,
    AgentFailed = 1,
    Usage = 2,
    #[allow(dead_code)]
    PermissionDenied = 3,
    NoHealthyBackend = 4,
    #[allow(dead_code)]
    BudgetExceeded = 5,
    #[allow(dead_code)]
    Interrupted = 130,
}

impl ExitCode {
    /// The process exit status this variant represents.
    pub fn code(self) -> i32 {
        self as i32
    }

    /// Maps a finished agent's terminal status to an exit code. Does not
    /// (cannot) know whether a `Cancelled` status was caused by an observed
    /// SIGINT -- see [`Self::from_result_with_sigint`] for the caller that
    /// does.
    ///
    /// Not yet called from production code: every WI-111 stub returns
    /// `ExitCode::Usage` directly rather than driving a real `AgentResult`.
    /// WI-112's `oneshot::run` is the first real caller.
    #[allow(dead_code)]
    pub fn from_result(r: &AgentResult) -> ExitCode {
        match &r.status {
            ResultStatus::Completed => ExitCode::Completed,
            ResultStatus::BudgetExceeded { .. } => ExitCode::BudgetExceeded,
            ResultStatus::Failed { .. }
            | ResultStatus::Rejected { .. }
            | ResultStatus::Cancelled { .. } => ExitCode::AgentFailed,
            // `ResultStatus` is `#[non_exhaustive]`: fail into the same
            // bucket as every other unclassified terminal cause rather than
            // refusing to compile on a future variant.
            _ => ExitCode::AgentFailed,
        }
    }

    /// [`Self::from_result`], with one override: a `Cancelled` status
    /// combined with `sigint_seen == true` reports `Interrupted` (130)
    /// rather than the default `AgentFailed` (1) -- "SIGINT outranks a
    /// status the runtime produced because of the interrupt" (module
    /// notes). The caller (WI-112's `oneshot::run`) is the one place that
    /// knows both facts at once.
    #[allow(dead_code)]
    pub fn from_result_with_sigint(r: &AgentResult, sigint_seen: bool) -> ExitCode {
        if sigint_seen && matches!(r.status, ResultStatus::Cancelled { .. }) {
            ExitCode::Interrupted
        } else {
            Self::from_result(r)
        }
    }

    /// Maps a terminal `ConwayError` to an exit code. See the module doc
    /// comment for the two rows this cannot classify precisely given the
    /// facade's current re-export surface, and why.
    pub fn from_error(e: &ConwayError) -> ExitCode {
        match e {
            ConwayError::Config { .. }
            | ConwayError::AgentDef { .. }
            | ConwayError::Build { .. }
            | ConwayError::UnsupportedFeature { .. } => ExitCode::Usage,
            ConwayError::Routing(_) | ConwayError::Runtime(_) => {
                classify_runtime_or_routing(&e.to_string())
            }
            ConwayError::Io(_) | ConwayError::Backend(_) | ConwayError::Store(_) => {
                ExitCode::AgentFailed
            }
            // `ConwayError` is `#[non_exhaustive]`.
            _ => ExitCode::AgentFailed,
        }
    }
}

/// Substring classification over a `ConwayError::{Routing,Runtime}`'s
/// `Display` text -- see this module's doc comment, reconciliation (1), for
/// why this is the only mechanism available without a new dependency.
/// Depends on `RoutingError::NoCandidate`'s `Display` wording
/// (`conway-core/src/error.rs`) never dropping the phrase `"no candidate
/// for role"`.
fn classify_runtime_or_routing(display: &str) -> ExitCode {
    if display.contains("no candidate for role") {
        ExitCode::NoHealthyBackend
    } else {
        ExitCode::AgentFailed
    }
}

#[cfg(test)]
mod tests {
    use conway::{AgentId, SessionId};
    use conway_core::error::{RoutingError, RuntimeError};
    use conway_core::ids::RoleAlias;

    use super::*;

    fn result(status: ResultStatus) -> AgentResult {
        AgentResult::new(AgentId::new(), SessionId::new(), status, "")
    }

    #[test]
    fn completed_is_zero() {
        assert_eq!(
            ExitCode::from_result(&result(ResultStatus::Completed)).code(),
            0
        );
    }

    #[test]
    fn failed_non_permission_is_one() {
        let r = result(ResultStatus::Failed {
            error: "boom".into(),
        });
        assert_eq!(ExitCode::from_result(&r).code(), 1);
    }

    #[test]
    fn rejected_is_one() {
        let r = result(ResultStatus::Rejected {
            missing: vec!["fact".into()],
        });
        assert_eq!(ExitCode::from_result(&r).code(), 1);
    }

    #[test]
    fn config_load_error_is_two() {
        let e = ConwayError::Config {
            path: None,
            message: "bad config".into(),
        };
        assert_eq!(ExitCode::from_error(&e).code(), 2);
    }

    #[test]
    fn unknown_role_agent_def_error_is_two() {
        let e = ConwayError::AgentDef {
            path: "reviewer.toml".into(),
            message: "unknown role".into(),
        };
        assert_eq!(ExitCode::from_error(&e).code(), 2);
    }

    #[test]
    fn no_candidate_via_bare_routing_variant_is_four() {
        let routing_err = RoutingError::NoCandidate {
            role: RoleAlias::new("coder"),
            considered: Vec::new(),
        };
        let e = ConwayError::Routing(routing_err);
        assert_eq!(ExitCode::from_error(&e).code(), 4);
    }

    #[test]
    fn no_candidate_via_fallback_chain_exhaustion_is_four() {
        // Mirrors `conway-runtime/src/attempt.rs:324`'s wrapping: the
        // router's `RoutingError::NoCandidate` boxed inside
        // `RuntimeError::Routing`, then `ConwayError::Runtime`.
        let routing_err = RoutingError::NoCandidate {
            role: RoleAlias::new("coder"),
            considered: Vec::new(),
        };
        let e = ConwayError::Runtime(RuntimeError::Routing(routing_err));
        assert_eq!(ExitCode::from_error(&e).code(), 4);
    }

    #[test]
    fn budget_exceeded_is_five() {
        let r = result(ResultStatus::BudgetExceeded {
            limit: "max_steps".into(),
        });
        assert_eq!(ExitCode::from_result(&r).code(), 5);
    }

    #[test]
    fn cancelled_with_sigint_is_130() {
        let r = result(ResultStatus::Cancelled {
            reason: "sigint".into(),
        });
        assert_eq!(ExitCode::from_result_with_sigint(&r, true).code(), 130);
    }

    #[test]
    fn cancelled_without_sigint_is_one() {
        let r = result(ResultStatus::Cancelled {
            reason: "sigint".into(),
        });
        assert_eq!(ExitCode::from_result_with_sigint(&r, false).code(), 1);
        assert_eq!(ExitCode::from_result(&r).code(), 1);
    }

    #[test]
    fn discriminants_match_the_contract() {
        assert_eq!(ExitCode::Completed.code(), 0);
        assert_eq!(ExitCode::AgentFailed.code(), 1);
        assert_eq!(ExitCode::Usage.code(), 2);
        assert_eq!(ExitCode::PermissionDenied.code(), 3);
        assert_eq!(ExitCode::NoHealthyBackend.code(), 4);
        assert_eq!(ExitCode::BudgetExceeded.code(), 5);
        assert_eq!(ExitCode::Interrupted.code(), 130);
    }
}
