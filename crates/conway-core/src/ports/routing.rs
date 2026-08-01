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
    /// NOTE, and it is a real gap rather than a simplification: this means
    /// a chain in which every candidate's window is too small, but at least
    /// one ALSO fails another requirement, yields `NoCandidate` -- so the
    /// structured context fields are absent for a request that genuinely
    /// could not fit anywhere. Whether that case should widen to
    /// `ContextTooLarge` is an open question tracked on the board; do not
    /// read the current split as settled design.
    fn resolve(&self, req: &RouteRequest) -> Result<Vec<Route>, RoutingError>;
}

/// Tracks per-endpoint circuit-breaker state, independent of routing policy.
pub trait HealthRegistry: Send + Sync {
    fn state(&self, ep: &EndpointId) -> BreakerState;
    fn record(&self, ep: &EndpointId, obs: Observation);
}
