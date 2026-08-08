//! The `Router` and `HealthRegistry` ports (architecture §4.5).
//!
//! Strict separation (decision 6): `Router` owns *policy* (which model
//! should serve this role); `HealthRegistry` owns *state* (is this endpoint
//! usable now). The router only reads breaker state as a filter. No
//! classifiers, no embeddings, no request-content inspection — there is no
//! code path reachable through these traits that can see prompt text.

use std::sync::Arc;

use crate::capabilities::HeadroomPolicy;
use crate::error::{ConwayError, RoutingError};
use crate::ids::EndpointId;
use crate::routing::{BreakerState, ExplainReport, Observation, Route, RouteRequest, RoutingConfig};

use super::Backend;

/// Resolves a routing role to an ordered list of candidates.
pub trait Router: Send + Sync {
    /// MUST be pure with respect to request content: `RouteRequest` carries
    /// no prompt text by construction (GP-07), so purity here is a
    /// consequence of the input type, not just a convention. MUST NOT
    /// mutate breaker state — `resolve` only reads it via `HealthRegistry`.
    ///
    /// POST (success): the returned `Vec` is never empty, and every element
    /// carries a `RoutingReason` explaining why it was chosen or where it
    /// falls in the fallback chain.
    /// POST (failure): either `RoutingError::ContextTooLarge` (see T-1) or
    /// `RoutingError::NoCandidate`, whose `considered` enumerates every
    /// rejection.
    ///
    /// T-1: `resolve` MUST NOT return a candidate that cannot fit the
    /// request. When every candidate was rejected and *each one solely*
    /// because its headroom-adjusted `max_context_tokens` does not cover
    /// `req.est_tokens`, `resolve` returns `RoutingError::ContextTooLarge`
    /// naming the input size, the resolved headroom, and the largest window
    /// among the rejected candidates (P-9's "typed error naming input
    /// tokens, headroom, and the largest window").
    ///
    /// A rejection that is NOT attributable to context size alone --
    /// an unindexed model, a health-open breaker, or a candidate failing
    /// the headroom gate *and* some other requirement -- makes the outcome
    /// `NoCandidate` instead, with the per-candidate shortfall carried as
    /// prose inside `considered`.
    ///
    /// This split is SETTLED DESIGN, not a simplification awaiting a fix
    /// (P-9 as amended 2026-08-01; decision `01KYY4D6R5KH1S02XWAJKP7531`).
    /// A chain in which every candidate's window is too small, but at least
    /// one ALSO fails another requirement, deliberately yields
    /// `NoCandidate`. Reporting it as `ContextTooLarge` would attribute the
    /// failure purely to size and hand the operator remediation advice --
    /// shrink the turn, raise the headroom -- that cannot work when a
    /// capability was also missing. `considered` names every reason, which
    /// is the honest answer when size was not the whole story.
    ///
    /// Known consequence, recorded so it is not rediscovered as a surprise:
    /// `AgentLoop` invokes `ContextHook::on_overflow` only on
    /// `ContextTooLarge`, so a hook cannot intervene in the mixed case even
    /// though shrinking the context might bring the request under the window
    /// of a candidate that failed only on headroom. Widening that hook's
    /// trigger -- not widening this error -- is the change to consider if it
    /// ever matters; it exposes the seam without moving routing policy into
    /// core (GP-11).
    fn resolve(&self, req: &RouteRequest) -> Result<Vec<Route>, RoutingError>;
}

/// Tracks per-endpoint circuit-breaker state, independent of routing policy.
pub trait HealthRegistry: Send + Sync {
    fn state(&self, ep: &EndpointId) -> BreakerState;
    fn record(&self, ep: &EndpointId, obs: Observation);
}

/// Produces the "why did this model run, and why not the others" answer for
/// a `RouteRequest` -- see `ExplainReport`. Deliberately a separate port from
/// `Router` rather than a new method on it (board item
/// 01KZFC1KNGQ51TZ0BG7P7RAY9H): `Router` has exactly one method, `resolve`,
/// and every existing `.with_router(..)` call site across this workspace
/// supplies a trait object that only ever needed to answer that one
/// question. Implemented by `conway_routing::RoutingExplain` (a capability-
/// and health-filtered projection of a concrete `DeclarativeRouter`) and by
/// `crate::routing::MinimalRouter` (an honestly degenerate answer needing
/// nothing but a `RoutingConfig`) -- both build the one `ExplainReport`
/// shape, so a caller never sees two answers to "why" that could disagree
/// about which report fields exist.
pub trait RoutingExplainer: Send + Sync {
    fn explain(&self, req: &RouteRequest) -> ExplainReport;
}

// ---------------------------------------------------------------------
// `RouterFactory` (board item 01KZFC2MD1FVNA674YJ9A19T8E): names a router
// KIND up front (so it can appear in `[plugins].install`, resolved before
// backends/capabilities exist) and defers actual construction to a later,
// fallible step. Settled decision 01KZF15KSWVD689HPBNNATFP8C, cited rather
// than relitigated: `Router` gains NO `id()` method -- selection (naming a
// kind) must precede construction (a fallible step needing backends and a
// capability picture), and `Backend::id()` is a CONFIGURED INSTANCE
// identity while a router id names a KIND, so the two are not the same
// question answered twice.
// ---------------------------------------------------------------------

/// What a [`RouterFactory::build`] hands back, together: the constructed
/// `Router`, the `HealthRegistry` it reads/records breaker state through,
/// and -- optionally -- a matching `RoutingExplainer`.
///
/// Construction is the only moment a supplied router can hand over an
/// explainer that is guaranteed to agree with it about "why" (built from
/// the exact same internal state the router itself resolves against) --
/// discovering one separately, after the fact, risks the two silently
/// drifting apart, which is why `explain` rides along here rather than
/// being a second, independently-timed call. `explain: None` is honest
/// when a factory's router has no richer answer to give than
/// [`crate::routing::MinimalRouter`] already provides -- `Conway::
/// explain_routing` falls back to that degenerate answer in exactly the
/// same way it already does for a router injected via `ConwayBuilder::
/// with_router` (GP-14: no report claims more than it can back).
///
/// `health` REPLACES whatever `HealthRegistry` the caller would otherwise
/// have constructed -- the router and the runtime it serves MUST continue
/// to share exactly ONE breaker registry, so this bundle is the sole
/// channel a factory has to supply one at all.
pub struct RouterBundle {
    pub router: Arc<dyn Router>,
    pub health: Arc<dyn HealthRegistry>,
    pub explain: Option<Arc<dyn RoutingExplainer>>,
}

/// Manual, opaque-placeholder `Debug` (mirrors `ports::plugin::ToolCtx`'s own
/// precedent for a struct holding `Arc<dyn Trait>` fields): none of `Router`/
/// `HealthRegistry`/`RoutingExplainer` requires `Debug` of its implementors,
/// so this crate's `#![deny(missing_debug_implementations)]` is satisfied
/// without imposing that requirement on every factory's router/health/
/// explainer implementation.
impl std::fmt::Debug for RouterBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouterBundle")
            .field("router", &"<dyn Router>")
            .field("health", &"<dyn HealthRegistry>")
            .field(
                "explain",
                &self.explain.as_ref().map(|_| "<dyn RoutingExplainer>"),
            )
            .finish()
    }
}

/// Everything a [`RouterFactory::build`] genuinely needs, and nothing more.
///
/// Every field type here is already defined in `conway-core` itself
/// (`RoutingConfig`, `HeadroomPolicy`, [`Backend`]) -- this deliberately
/// does NOT carry a `conway_core::ports::CapabilityIndex` (or any other
/// sibling-crate type), so this port stays constructible with no new
/// dependency from `conway-core` onto `conway-routing` (C-04). A factory
/// that wants a capability picture builds one from `backends` itself,
/// exactly as `conway_routing::CapabilityIndex::from_backends` already does
/// for the compiled path.
pub struct RouterBuildContext<'a> {
    /// The role → fallback-chain routing policy resolved from
    /// `[routing]`/`[roles]`.
    pub routing: RoutingConfig,
    /// The resolved reserved-output-token policy (`[routing]
    /// default_headroom_tokens` plus any per-role override).
    pub headroom: HeadroomPolicy,
    /// Every backend `ConwayBuilder::build` has already constructed
    /// (config-derived, then injected via `with_backend`, merged by id) --
    /// a factory reads `Backend::capabilities()` off these directly, never
    /// a second, independently-recomputed notion of what a backend can do
    /// (P-14).
    pub backends: &'a [Arc<dyn Backend>],
}

/// Manual, opaque-placeholder `Debug` for the `backends` slice's `dyn
/// Backend` elements -- same reasoning as [`RouterBundle`]'s own manual
/// impl immediately above.
impl std::fmt::Debug for RouterBuildContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouterBuildContext")
            .field("routing", &self.routing)
            .field("headroom", &self.headroom)
            .field("backends", &format!("<{} backend(s)>", self.backends.len()))
            .finish()
    }
}

/// Carries a router's IDENTITY up front, so it can be named in
/// `[plugins].install` before the router itself can be built -- building a
/// real router needs backends and a capability picture that do not exist
/// until much later in startup, well after `[plugins].install` is read
/// (board item 01KZFC2MD1FVNA674YJ9A19T8E).
///
/// `build` is DEFERRED (invoked only once `ctx` can actually be assembled)
/// and FALLIBLE, returning [`ConwayError`] -- `conway-core`'s own existing
/// crate-level error enum, deliberately reused rather than a new one
/// invented for this port: a router factory's construction failure is
/// exactly the shape `ConwayError::Config`/`ConwayError::Parse` already
/// exist to describe (an operator-supplied name/config the factory could
/// not turn into a working router), and every other fallible port
/// constructor in this crate already reuses an existing typed error rather
/// than growing a bespoke one per port.
pub trait RouterFactory: Send + Sync {
    /// This factory's own identity -- the id an operator names in
    /// `[plugins].install` to select it. A KIND, not a configured
    /// instance's identity (contrast `Backend::id()`): stable across every
    /// `Router` this factory might construct.
    fn id(&self) -> &str;

    /// Builds the router this factory names, the health registry it shares
    /// state with, and (optionally) a matching explainer -- see
    /// [`RouterBundle`]'s own doc. Called at most once per build, after
    /// every backend this build needs has already been constructed.
    fn build(&self, ctx: RouterBuildContext<'_>) -> Result<RouterBundle, ConwayError>;
}
