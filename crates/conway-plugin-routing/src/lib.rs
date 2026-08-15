//! Conway-plugin-routing: declarative role -> ordered-candidate resolution
//! (`DeclarativeRouter`), a per-endpoint circuit breaker (`BreakerRegistry`),
//! the router's own capability predicate (`satisfies`, `capability.rs`), and
//! the "why did this model run" report (`RoutingExplain`).
//!
//! **The periodic health prober was retired, not wired.** A prober type used to live here,
//! feeding an independent `Probe` breaker from periodic liveness checks
//! decoupled from request traffic. It had no production call site — the
//! Transport breaker alone already handles recovery (a clock read takes it
//! half-open; the next real request retries), so wiring the prober would
//! only have shaved latency off the first request after an outage, an
//! optimization this project gates on a measured baseline that neither
//! existed nor was scheduled. Retiring it (rather than leaving it as a
//! forward declaration indefinitely) is the operator's decision, not a
//! default. See `docs/routing.md`'s "Health and failover" section for the
//! current, single-breaker shape.
//!
//! **First-party plugin, not a `conway` built-in.** `conway`'s own `builder.rs` used to
//! compile a `DeclarativeRouter` in unconditionally; that Cargo edge is now
//! cut, and this entire engine (everything this crate contains) is
//! installed instead, by naming [`ROUTER_ID`] in `[plugins].install`
//! (`factory.rs`'s `RoutingRouterFactory`) or by handing a
//! `RoutingRouterFactory` to `ConwayBuilder::with_router_factory` directly.
//! Absent that, `ConwayBuilder::build` falls through to
//! `conway_core::routing::MinimalRouter` -- the honest, config-only core
//! resolver that needs no filtering logic at all. See `docs/routing.md`
//! for what changes (and what doesn't) between the two configurations.
//!
//! `CapabilityIndex`/`CapabilityIndexBuilder` are re-exported here for
//! source compatibility but no longer defined in this crate (a later item
//! moved them to `conway_core::ports` --
//! "the backend side" -- since `from_backends` reads directly off
//! `Backend::capabilities` and nothing about the type is routing-policy
//! specific).
//!
//! `BreakerSnapshot`/`CapabilitySummary`/`EntryOutcome`/`ExplainEntry`/
//! `ExplainReport` are, likewise, re-exported here for source compatibility
//! but no longer defined in this crate (a later item
//! moved them to `conway_core::routing`, so a `Router` supplied from outside
//! this crate can still produce one via `conway_core::routing::MinimalRouter`
//! -- see that type's doc for why).
//!
//! `conway-core` owns the port traits (`Router`, `HealthRegistry`) and the
//! content-free request/response/config types this crate operates on; this
//! crate provides the implementations. See `ARCHITECTURE.md` for the
//! whole-system picture, and `docs/routing.md` for how the resulting
//! behavior looks from the outside (through earlier work).
//!
//! No classifier, embedding model, or other learned component may be linked
//! into this crate, at MVP or ever, absent an explicit decision reversal
//! predictable and answerable.

mod breaker;
mod capability;
pub mod config;
mod explain;
mod factory;
mod router;

// The crate's re-export block is authored incrementally by the work items
// that implement each type; each lands its own line.
#[cfg(any(test, feature = "test-clock"))]
pub use breaker::TestClock;
pub use breaker::{BreakerRegistry, Clock, SystemClock};
pub use capability::satisfies;
pub use conway_core::ports::{CapabilityIndex, CapabilityIndexBuilder};
pub use conway_core::routing::{
    BreakerSnapshot, CapabilitySummary, EntryOutcome, ExplainEntry, ExplainReport,
};
pub use explain::RoutingExplain;
pub use factory::{RoutingRouterFactory, ROUTER_ID};
pub use router::DeclarativeRouter;
