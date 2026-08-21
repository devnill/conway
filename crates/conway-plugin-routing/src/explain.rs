//! `RoutingExplain`: the "why did this model run, and why not the others"
//! answer (amended for headroom). Implemented **solely** as a
//! projection of `DeclarativeRouter::evaluate` (`router.rs`) plus a
//! per-candidate health/capability snapshot -- it must never re-implement
//! filtering, which is the specific bug this structure prevents.
//!
//! **The report shape itself moved to `conway-core`** -- `BreakerSnapshot`, `CapabilitySummary`,
//! `EntryOutcome`, `ExplainEntry`, and `ExplainReport` are now defined in
//! `conway_core::routing` and re-exported from this crate's `lib.rs` for
//! source compatibility. This module now also implements
//! `conway_core::ports::RoutingExplainer` for [`RoutingExplain`], delegating
//! to the existing inherent `explain` method below so the two never diverge.
//!
//! Divergence note (flagged, not worked around): the binding plan
//! specifies each
//! entry's `breaker` field as `BreakerSnapshot { transport: BreakerState,
//! probe: BreakerState }` -- an independent read of each of the two breaker
//! kinds. `conway_core::ports::HealthRegistry` (the only handle this crate's
//! `DeclarativeRouter` holds on health state) exposes just one method,
//! `state(&EndpointId) -> BreakerState`, which returns the *merged* view
//! (`BreakerRegistry::merged_state`); the transport/probe split is an
//! inherent method on `BreakerRegistry` itself (`kind_state`), not part of
//! the port trait, so it is unreachable through `Arc<dyn HealthRegistry>`.
//! `BreakerSnapshot` below therefore carries a single merged `state` field
//! instead of the specified `{transport, probe}` pair. Coordinator-approved
//! (option (a) of the scoping blocker): a `HealthRegistry::kind_state`
//! trait addition is queued for the refinement phase; this note is the
//! breadcrumb back to the original spec shape.
//!
//! Second divergence note: the plan's implementation notes say
//! `generated_at` "is injected via the router's `Clock`", matching an
//! earlier draft. `DeclarativeRouter` (as landed, commit 0a38c42)
//! carries no `Clock` field -- only `BreakerRegistry` does, and it is not
//! reachable from here (see above). `generated_at` is instead read directly
//! from `chrono::Utc::now()` at explain time. This does not affect
//! determinism of the golden-file criterion: `render_text` never emits
//! `generated_at`.
//!
//! Third note: `explain` is infallible (`-> ExplainReport`, not `Result`),
//! but `DeclarativeRouter::evaluate` can fail with
//! `RoutingError::UnknownRole` for a non-pinned request naming an
//! unconfigured role. In that case `explain` returns a report with
//! `entries: vec![]` and `headroom_tokens: 0` rather than propagating the
//! error -- the router's per-role headroom resolution
//! (`DeclarativeRouter::effective_headroom`) is private and not part of the
//! two coordinator-approved accessors (`health`, `capability_index`), so no
//! better value is reachable here without widening that approval. This is a
//! deliberate, documented limitation, not a silent swallow: `resolve` still
//! surfaces `UnknownRole` as an error to callers that need it.

use chrono::Utc;

use conway_core::ports::RoutingExplainer;
use conway_core::routing::{
    BreakerSnapshot, CapabilitySummary, EntryOutcome, ExplainEntry, ExplainReport, RouteRequest,
};

use crate::router::{endpoint_of, DeclarativeRouter, EvalOutcome};

/// Builds `ExplainReport`s as a pure projection of `DeclarativeRouter`'s
/// evaluation -- the "why did this model run" answer, sharing its filtering
/// logic with `resolve` by construction rather than duplicating it.
pub struct RoutingExplain<'a> {
    router: &'a DeclarativeRouter,
}

impl<'a> RoutingExplain<'a> {
    pub fn new(router: &'a DeclarativeRouter) -> RoutingExplain<'a> {
        RoutingExplain { router }
    }

    /// Synchronous, no I/O: reads `router.evaluate(req)` (no filtering logic
    /// of its own) plus one health-registry read and one capability-index
    /// lookup per candidate -- never calls `HealthRegistry::record`.
    pub fn explain(&self, req: &RouteRequest) -> ExplainReport {
        let generated_at = Utc::now();

        match self.router.evaluate(req) {
            Ok(evaluation) => {
                let entries = evaluation
                    .entries
                    .into_iter()
                    .map(|entry| {
                        let model_ref = entry.model_ref.clone();
                        let capabilities = self
                            .router
                            .capability_index()
                            .get(&model_ref)
                            .map(CapabilitySummary::from);
                        // Board item 01M0ASX466G3PW3SJJS3KGNS55: the
                        // operator-visible surface for
                        // `Backend::token_fidelity`. Keyed by backend id
                        // alone (fidelity is a `Backend`-level declaration,
                        // not per-model), same index `capabilities` above
                        // already reads.
                        let token_fidelity = self
                            .router
                            .capability_index()
                            .token_fidelity(&model_ref.backend);
                        let breaker = BreakerSnapshot {
                            state: self.router.health().state(&endpoint_of(&model_ref)),
                        };
                        let outcome = match entry.outcome {
                            EvalOutcome::Selected(reason) => EntryOutcome::Selected { reason },
                            // The headroom-only window (used by `resolve`'s
                            // T-1 aggregate decision, see `router.rs`) isn't
                            // part of the explain surface -- it already has
                            // the full `RoutingReason::CapabilitySkip`
                            // detail, and `explain` never re-derives
                            // `ContextTooLarge` (see this file's "Third
                            // note").
                            EvalOutcome::Skipped(reason, _headroom_only_window) => {
                                EntryOutcome::Skipped { reason }
                            }
                        };
                        ExplainEntry {
                            model_ref,
                            chain_position: entry.chain_position,
                            outcome,
                            capabilities,
                            breaker,
                            token_fidelity,
                        }
                    })
                    .collect();

                ExplainReport {
                    role: req.role.clone(),
                    pin: req.pin.clone(),
                    est_tokens: req.est_tokens,
                    required: self.router.effective_required(&req.role, req),
                    headroom_tokens: evaluation.headroom_tokens,
                    entries,
                    generated_at,
                }
            }
            // See the module-level "Third note": UnknownRole is the one
            // evaluate() error, reachable only for a non-pinned request
            // naming an unconfigured role.
            Err(_unknown_role) => ExplainReport {
                role: req.role.clone(),
                pin: req.pin.clone(),
                est_tokens: req.est_tokens,
                required: req.required.clone(),
                headroom_tokens: 0,
                entries: Vec::new(),
                generated_at,
            },
        }
    }
}

impl RoutingExplainer for RoutingExplain<'_> {
    fn explain(&self, req: &RouteRequest) -> ExplainReport {
        RoutingExplain::explain(self, req)
    }
}

/// Lets an owned `Arc<DeclarativeRouter>` (as `ConwayBuilder::build` holds
/// once it compiles one) stand in
/// directly as an `Arc<dyn RoutingExplainer>` for `Conway`'s
/// `router_explain` slot. `RoutingExplain<'a>` above borrows a `&'a
/// DeclarativeRouter` and so cannot itself be boxed/arc'd as a `'static`
/// trait object; this impl delegates to the exact same
/// `RoutingExplain::new(self).explain(req)` path, so it adds no second
/// filtering implementation to keep in sync with this module's own
/// top-of-file note ("it must never re-implement filtering").
impl RoutingExplainer for DeclarativeRouter {
    fn explain(&self, req: &RouteRequest) -> ExplainReport {
        RoutingExplain::new(self).explain(req)
    }
}
