//! conway-plugin-routing: declarative role -> ordered-candidate resolution
//! (`DeclarativeRouter`), per-endpoint circuit breakers (`BreakerRegistry`)
//! plus a background health prober (`HealthProber`) that is defined but not
//! yet wired into production (see `prober`'s module doc comment and board
//! item `01KZ802GSF692EKYKQ2TTVCJB8`), the router's own capability
//! predicate (`satisfies`, `capability.rs`), and the "why did this model
//! run" report (`RoutingExplain`).
//!
//! **First-party plugin, not a `conway` built-in (board item
//! 01KZFC43J1J06BM4CCWKCKHSNV).** `conway`'s own `builder.rs` used to
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
//! source compatibility but no longer defined in this crate (board item
//! 01KZFBZHTWDF11TH7G0H613ERE moved them to `conway_core::ports` --
//! "the backend side" -- since `from_backends` reads directly off
//! `Backend::capabilities` and nothing about the type is routing-policy
//! specific).
//!
//! `BreakerSnapshot`/`CapabilitySummary`/`EntryOutcome`/`ExplainEntry`/
//! `ExplainReport` are, likewise, re-exported here for source compatibility
//! but no longer defined in this crate (board item 01KZFC1KNGQ51TZ0BG7P7RAY9H
//! moved them to `conway_core::routing`, so a `Router` supplied from outside
//! this crate can still produce one via `conway_core::routing::MinimalRouter`
//! -- see that type's doc for why).
//!
//! `conway-core` owns the port traits (`Router`, `HealthRegistry`) and the
//! content-free request/response/config types this crate operates on; this
//! crate provides the implementations. See `ARCHITECTURE.md` for the
//! whole-system picture, and `docs/routing.md` for how the resulting
//! behavior looks from the outside (WI-031 through WI-036).
//!
//! No classifier, embedding model, or other learned component may be linked
//! into this crate, at MVP or ever, absent an explicit decision reversal
//! (GP-07).

mod breaker;
mod capability;
pub mod config;
mod explain;
mod factory;
mod prober;
mod router;

/// Not yet implemented / not wired (GP-14 forward declaration): no
/// production code constructs a [`HealthProber`] or calls
/// [`HealthProber::spawn`] — see `prober`'s module doc comment for why, and
/// board item `01KZ802GSF692EKYKQ2TTVCJB8` for the deferred wiring.
pub use prober::{HealthProber, ProberHandle};

// The crate's re-export block is authored incrementally by the work items
// that implement each type (WI-032 .. WI-036); each lands its own line.
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
