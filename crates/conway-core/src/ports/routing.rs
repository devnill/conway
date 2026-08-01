//! The `Router` and `HealthRegistry` ports (architecture §4.5).
//!
//! Strict separation (decision 6): `Router` owns *policy* (which model
//! should serve this role); `HealthRegistry` owns *state* (is this endpoint
//! usable now). The router only reads breaker state as a filter. No
//! classifiers, no embeddings, no request-content inspection — there is no
//! code path reachable through these traits that can see prompt text.

use crate::error::RoutingError;
use crate::ids::EndpointId;
use crate::routing::{BreakerState, Observation, Route, RouteRequest};

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
