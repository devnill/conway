//! The CLI's exit-code contract.
//!
//! [`ExitCode`] is the single vocabulary every command path reports
//! through: `main` never returns early with a bare `std::process::exit`
//! call for anything reachable from a live run, it always produces one of
//! these variants and converts it to a process exit status in one place.
//!
//! Every declared variant is reachable from a live `conway -p` invocation,
//! and `tests/oneshot.rs` proves it by driving the real compiled binary and
//! asserting the observed process exit status -- a unit test of the mapping
//! functions below is not evidence a code is live. Two entries in
//! that contract history are worth recording here because they shaped this
//! module:
//!
//! 1. **There is no permission-denied code, deliberately.** Exit code 3
//!    (`PermissionDenied`) was declared for "a `ConwayError` whose terminal
//!    cause is a permission denial" and removed rather than wired, because
//!    the premise is wrong for one-shot mode: a permission denial (either
//!    `PermissionDecision::Deny` or `::DenyWithFeedback`) collapses to a
//!    model-visible `ToolOutcome::error` fed back into the agent's own turn
//!    (`conway-runtime/src/permission.rs`'s `PermissionOutcome::from`,
//!    `conway-runtime/src/tools/runner.rs`'s `execute_one`) -- it is a tool
//!    result, not a terminal condition. The agent may recover (pick another
//!    tool, or finish without it) and the run legitimately continues, so
//!    terminating the process over it would kill runs that complete
//!    successfully. `ToolError::Denied`, the one core variant whose name
//!    suggests a terminal denial, is constructed nowhere in the workspace;
//!    inventing a terminal path for it would change agent *behavior*, not
//!    classification, which no exit-code item may do. Code 3 is simply
//!    unassigned.
//! 2. **`NoHealthyBackend` (4) is wired through `from_result`, not
//!    `from_error`.** A routing failure mid-turn never propagates out of
//!    `oneshot::run` as a `ConwayError`: `AgentLoop::run_inner`'s generic
//!    `Err` path folds every `RuntimeError` -- `RuntimeError::Routing`
//!    included -- into `ResultStatus::Failed { error: err.to_string() }`
//!    (`conway-runtime/src/agent_loop.rs`'s `finish_error`: "everything
//!    else maps to `Failed`"). That is why 4 was unreachable while only
//!    `from_error` classified it, and why the fix lives in `from_result`'s
//!    `Failed` arm below rather than in any runtime change: the `Failed`
//!    string still carries the `RoutingError`'s `Display` wording verbatim
//!    through every wrapping layer (`RuntimeError::Routing` only prepends
//!    `"routing error: "`), so the same classifier serves both entry
//!    points.
//!
//! **The classification mechanism itself (disclosed):** the `conway`
//! facade re-exports `ConwayError` but not the
//! `conway_core::error::{RoutingError, RuntimeError}` types nested inside
//! its `Routing`/`Runtime` variants (see `crates/conway/src/lib.rs`'s
//! re-export list) -- and this crate's manifest is machine-checked to
//! depend on no other workspace crate (`tests/cli_surface.rs::no_forbidden_deps`),
//! so neither `from_error` nor `from_result` can name those inner types to
//! match on them directly. `classify_runtime_or_routing` therefore
//! matches on `Display` substrings rather than types. Every substring it
//! looks for is pinned by a `conway-core/src/error.rs` test (named at the
//! match site), so a wording change upstream fails a test upstream instead
//! of silently reclassifying here. A future facade change adding
//! classifier methods on `ConwayError` itself (e.g. `is_routing_rejection()`)
//! would let this module drop the string match; flagged as a follow-up,
//! not fixed here (out of scope).
//!
//! **SIGINT precedence (130):** `AgentResult` carries no SIGINT flag --
//! that state lives in the caller's `signal::SigintWatch`. `from_result`
//! alone (its signature takes only `&AgentResult`) cannot see it, so it
//! always reports `AgentFailed` for a bare `Cancelled`.
//! [`ExitCode::from_result_with_sigint`] is the one call that already
//! implements the "SIGINT outranks a status the runtime produced *because
//! of* the interrupt" precedence rule, and `oneshot::run` is the one place
//! that knows both facts at once.

use conway::{AgentResult, ConwayError, ResultStatus};

/// The CLI's process exit status vocabulary. Discriminants are the
/// contract: `code()` casts `self` directly to `i32`, so these values are
/// load-bearing, not incidental. Every variant is constructed by production
/// code on a live path (see the module doc); code 3 is deliberately
/// unassigned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitCode {
    Completed = 0,
    AgentFailed = 1,
    Usage = 2,
    NoHealthyBackend = 4,
    BudgetExceeded = 5,
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
    pub fn from_result(r: &AgentResult) -> ExitCode {
        match &r.status {
            ResultStatus::Completed => ExitCode::Completed,
            ResultStatus::BudgetExceeded { .. } => ExitCode::BudgetExceeded,
            // A routing rejection surfaces HERE, not via `from_error` --
            // `finish_error` folded it into this bare string (module doc,
            // entry 2), which still carries the `RoutingError`'s `Display`
            // wording verbatim.
            ResultStatus::Failed { error } => classify_runtime_or_routing(error),
            ResultStatus::Rejected { .. } | ResultStatus::Cancelled { .. } => ExitCode::AgentFailed,
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
    /// notes). The caller (`oneshot::run`) is the one place that knows both
    /// facts at once.
    pub fn from_result_with_sigint(r: &AgentResult, sigint_seen: bool) -> ExitCode {
        if sigint_seen && matches!(r.status, ResultStatus::Cancelled { .. }) {
            ExitCode::Interrupted
        } else {
            Self::from_result(r)
        }
    }

    /// Maps a terminal `ConwayError` to an exit code. Routing failures a
    /// live `-p` run can trigger do not reach this function (they arrive as
    /// `ResultStatus::Failed` -- module doc, entry 2); this arm still
    /// classifies them correctly for any caller that does produce one, so
    /// both entry points agree.
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

/// Substring classification over a `ConwayError::{Routing,Runtime}`'s or a
/// `ResultStatus::Failed`'s `Display` text -- see this module's doc comment
/// for why a string match is the only mechanism available without a new
/// dependency, and for the wiring that makes routing failures reach it.
///
/// All three `RoutingError` variants share [`ExitCode::NoHealthyBackend`]:
/// an unknown role, no admissible candidate, and a context too large for
/// every candidate's window are the same outcome from a script's side --
/// routing could not supply any model for the turn, so nothing could
/// proceed. (`DeclarativeRouter` distinguishes `ContextTooLarge` from
/// `NoCandidate` only for mixed rejections, since core surfaces a refusal
/// rather than routing around it; that split is
/// about error *detail*, and both remain routing rejections here.)
/// `RuntimeError::ForkContextOverflow` shares `ContextTooLarge`'s exact
/// `Display` wording and is the same outcome at the fork boundary, so it
/// classifies the same way for free.
///
/// Each needle is pinned by a `conway-core/src/error.rs` test so an
/// upstream wording change fails there instead of silently reclassifying
/// here:
///
/// - `"no candidate for role"` -- `no_candidate_display_names_role_count_and_zero_reasons`
/// - `"unknown role alias"` -- `routing_rejection_display_wordings_pin_the_cli_exit_classifier`
/// - `"context rejected:"` -- same test (`ContextTooLarge` and
///   `ForkContextOverflow` share this prefix)
fn classify_runtime_or_routing(display: &str) -> ExitCode {
    const ROUTING_REJECTIONS: &[&str] = &[
        "no candidate for role",
        "unknown role alias",
        "context rejected:",
    ];
    if ROUTING_REJECTIONS
        .iter()
        .any(|needle| display.contains(needle))
    {
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
    fn failed_unclassified_cause_is_one() {
        let r = result(ResultStatus::Failed {
            error: "boom".into(),
        });
        assert_eq!(ExitCode::from_result(&r).code(), 1);
    }

    /// The `from_result` routing classification (module doc, entry 2):
    /// these build the REAL `RuntimeError`/`RoutingError` values and feed
    /// `finish_error`'s exact `err.to_string()` output through as the
    /// `Failed` string, so the test exercises the same text a live turn
    /// produces -- the liveness proof that this arm is ever reached at all
    /// is in `tests/oneshot.rs`, not here.
    #[test]
    fn failed_no_candidate_is_four() {
        let err = RuntimeError::Routing(RoutingError::NoCandidate {
            role: RoleAlias::new("coder"),
            considered: Vec::new(),
        });
        let r = result(ResultStatus::Failed {
            error: err.to_string(),
        });
        assert_eq!(ExitCode::from_result(&r).code(), 4);
    }

    #[test]
    fn failed_unknown_role_is_four() {
        let err = RuntimeError::Routing(RoutingError::UnknownRole {
            role: RoleAlias::new("doesnotexist"),
        });
        let r = result(ResultStatus::Failed {
            error: err.to_string(),
        });
        assert_eq!(ExitCode::from_result(&r).code(), 4);
    }

    #[test]
    fn failed_context_too_large_is_four() {
        let err = RuntimeError::Routing(RoutingError::ContextTooLarge {
            role: RoleAlias::new("default"),
            model: conway_core::ids::ModelRef {
                backend: conway_core::ids::BackendId::new("mock"),
                model: conway_core::ids::ModelId::new("tiny"),
            },
            est_tokens: 30_000,
            headroom_tokens: 4_000,
            required_tokens: 34_000,
            max_context_tokens: 1_024,
            shortfall_tokens: 32_976,
        });
        let r = result(ResultStatus::Failed {
            error: err.to_string(),
        });
        assert_eq!(ExitCode::from_result(&r).code(), 4);
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
        // Mirrors `conway-runtime/src/attempt.rs`'s wrapping: the router's
        // `RoutingError::NoCandidate` boxed inside `RuntimeError::Routing`,
        // then `ConwayError::Runtime`.
        let routing_err = RoutingError::NoCandidate {
            role: RoleAlias::new("coder"),
            considered: Vec::new(),
        };
        let e = ConwayError::Runtime(RuntimeError::Routing(routing_err));
        assert_eq!(ExitCode::from_error(&e).code(), 4);
    }

    #[test]
    fn unknown_role_via_routing_variant_is_four() {
        let e = ConwayError::Routing(RoutingError::UnknownRole {
            role: RoleAlias::new("doesnotexist"),
        });
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
        // 3 is deliberately unassigned -- see the module doc, entry 1.
        assert_eq!(ExitCode::NoHealthyBackend.code(), 4);
        assert_eq!(ExitCode::BudgetExceeded.code(), 5);
        assert_eq!(ExitCode::Interrupted.code(), 130);
    }
}
