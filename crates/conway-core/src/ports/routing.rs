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
    /// POST (failure): `RoutingError::NoCandidate` enumerates every
    /// rejection considered.
    ///
    /// T-1: if no candidate's (headroom-adjusted) `max_context_tokens`
    /// covers `req.est_tokens`, `resolve` returns
    /// `RoutingError::ContextTooLarge` naming the shortfall. It must never
    /// return a candidate that cannot fit the request.
    fn resolve(&self, req: &RouteRequest) -> Result<Vec<Route>, RoutingError>;
}

/// Tracks per-endpoint circuit-breaker state, independent of routing policy.
pub trait HealthRegistry: Send + Sync {
    fn state(&self, ep: &EndpointId) -> BreakerState;
    fn record(&self, ep: &EndpointId, obs: Observation);
}
