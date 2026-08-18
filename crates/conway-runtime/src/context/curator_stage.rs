//! The pre-assembly curator stage (DESIGN-context-path §11.4).
//!
//! Extracted from `AgentLoop::apply_curator` so the stage logic is unit-
//! testable without constructing a full `AgentLoop`: the free function
//! [`apply_curator`] takes the curator `Option`, the path, and a closure
//! that builds a [`CurateCtx`] ONLY when a curator is present -- preserving
//! the zero-cost guarantee (no curator -> the closure is never called, no
//! `CurateCtx` allocated, the path returns unchanged). The
//! `context_golden` 11/11 gate is the load-bearing end-to-end proof of that
//! guarantee; the unit test here is the per-stage proof.
//!
//! **No `GuardedCurator` re-validation layer** -- the `Derivation`-only
//! construction IS the guard (§11.4): [`CurateOutcome::Derived`] can only be
//! built from a `Derivation`, which is already the validated,
//! cost-estimated output of `ValidatedPath::derive`. The
//! unrepresentability lives in the type, not a wrapper.
//!
//! **Failure is fail-open and recorded** (§11.6): a curator returning
//! [`CurateOutcome::Failed`] is logged via `tracing::warn!` -- the SAME
//! non-fatal recording posture a panicking `ToolObserver` uses -- and the
//! turn proceeds on the uncurated path. A curator is an optimization, not a
//! correctness requirement; the consequence of not curating is caught
//! downstream by admission (§2.7).

use std::sync::Arc;

use conway_core::error::RuntimeError;
use conway_core::path::{ResolvedPath, ValidatedPath};
use conway_core::ports::{CurateCtx, CurateOutcome, Curator};

/// Run the pre-assembly curator stage.
///
/// `curator: None` is the **zero-cost pass-through** -- the overwhelmingly
/// common case -- and the load-bearing guarantee: the closure `build_ctx`
/// is NEVER called, no [`CurateCtx`] is allocated, and `path` returns
/// unchanged. This is what keeps `context_golden` 11/11 unregenerated when
/// no curator is installed.
///
/// `curator: Some(c)` builds the [`CurateCtx`] via `build_ctx` (so the
/// caller pays the ctx-construction cost only on the `Some` branch) and
/// delegates to [`run_curator_stage`].
///
/// Returns `Result` so the `try_rt!` call site in `run_inner` composes
/// cleanly; the stage is infallible once a curator is present (a `Failed`
/// outcome returns the original path, not an error -- §11.6 fail-open).
/// The `Option<String>` alongside the path is the §11.6 failure record the
/// caller threads into the turn's `ContextReport`; `None` when no curator
/// ran or the curator succeeded.
pub async fn apply_curator<F>(
    curator: Option<Arc<dyn Curator>>,
    path: ResolvedPath,
    build_ctx: F,
) -> Result<(ResolvedPath, Option<String>), RuntimeError>
where
    F: FnOnce() -> CurateCtx,
{
    // Zero-cost pass-through: the closure is never called.
    let Some(curator) = curator else {
        return Ok((path, None));
    };
    let ctx = build_ctx();
    Ok(run_curator_stage(&curator, &ctx, path).await)
}

/// Run ONE curator against `original` and return the resulting
/// [`ResolvedPath`] plus the failure reason, if the curator failed.
///
/// Infallible by construction: `Unchanged`/`Failed`/panic all return
/// `original`; `Derived` adopts the derivation's path via
/// [`ValidatedPath::into_nodes`] (consuming, no second clone of the
/// `Arc<LogRecord>`s).
///
/// The returned `Option<String>` is the §11.6 record. It is `Some` for BOTH
/// a `Failed` return and a caught panic, and the caller threads it into the
/// turn's `ContextReport` -- §11.6 is explicit that the failure "lands in
/// the context report next to `dropped`", because "fail-open with a silent
/// swallow would be the thing this project actually refuses". A
/// `tracing::warn!` alone would be exactly that silent swallow.
///
/// **Panics are contained** (§11.6: "a curator that errors, *panics*, or
/// returns `Failed` is contained and recorded"). Without this, a curator's
/// `.unwrap()` would unwind through `run_inner` to the `tokio::spawn`
/// boundary and the supervisor would synthesize a panic result for the
/// WHOLE agent -- a far larger blast radius than the uncurated turn §11.6
/// promises. Mirrors the `ToolObserver` containment in `agent_loop.rs`
/// exactly: `catch_unwind(AssertUnwindSafe(..))`, warn, proceed.
pub async fn run_curator_stage(
    curator: &Arc<dyn Curator>,
    ctx: &CurateCtx,
    original: ResolvedPath,
) -> (ResolvedPath, Option<String>) {
    // The clone only happens when a curator is active (after the zero-cost
    // return in `apply_curator`). `default_path` is the tolerant constructor
    // (declares incoherence rather than refusing); a derived base carries
    // no incoherence anyway.
    let base = ValidatedPath::default_path(original.nodes.clone());
    let outcome = match futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
        curator.curate(ctx, &base),
    ))
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => CurateOutcome::Failed {
            reason: "curator panicked".to_string(),
        },
    };
    match outcome {
        CurateOutcome::Unchanged => (original, None),
        CurateOutcome::Derived(derivation) => {
            // `into_nodes` consumes the validated `ValidatedPath` -- no
            // second clone of the `Arc<LogRecord>`s.
            //
            // `derivation.cost` (a `CostEstimate`) is DELIBERATELY dropped
            // here, not read. Part 4 of board item 01M0AP4ADTGJWF3GFMCFWFF1ZQ:
            // **admission (`Backend::admit`, run once per candidate model in
            // `conway_runtime::attempt::AttemptEngine::execute`, just before
            // each generate call) is the ONLY gate over token cost.** A
            // curator reasons about STRUCTURE (`CostEstimate`'s remaining
            // fields: shared prefix length, divergence position/kind,
            // frozen-tier membership) to decide whether a derivation is
            // worth proposing; it does not, and after this item cannot,
            // reason about TOKENS at all -- `CostEstimate` no longer carries
            // a token estimate for a curator to read (see `conway_core::
            // path::CostEstimate`'s own doc for why that field was removed
            // rather than kept "for non-gating use"). The alternative
            // (giving `CurateCtx` a counting capability so a curator could
            // gate on cost too) was considered and rejected: it would be a
            // SECOND gate at a layer that runs BEFORE the real per-model
            // count exists, dragging the per-candidate-model cost problem
            // (routing has not chosen a model yet at curation time) into a
            // stage that today has none of that machinery. One gate, at the
            // layer where the real count exists and already refuses loudly
            // with the exact shortfall named -- see `Backend::admit`'s own
            // doc in `conway-core::ports::backend` -- is simpler and cannot
            // drift from admission's own verdict.
            (
                ResolvedPath {
                    nodes: derivation.path.into_nodes(),
                },
                None,
            )
        }
        CurateOutcome::Failed { reason } => {
            // §11.6: fail-open, recorded. The `tracing::warn!` mirrors the
            // non-fatal posture a panicking `ToolObserver` uses; the
            // returned reason is what reaches the durable context report.
            tracing::warn!(
                agent = %ctx.agent_id,
                session = %ctx.session_id,
                turn = ctx.turn,
                "context curator failed; proceeding on the uncurated path (§11.6): {reason}",
            );
            (original, Some(reason))
        }
    }
}
