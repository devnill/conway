//! conway-routing: declarative role -> ordered-candidate resolution
//! (`DeclarativeRouter`), per-endpoint circuit breakers (`BreakerRegistry`)
//! plus a background health prober (`HealthProber`) that is defined but not
//! yet wired into production (see `prober`'s module doc comment and board
//! item `01KZ802GSF692EKYKQ2TTVCJB8`), the router's own capability
//! predicate (`satisfies`, `capability.rs`), and the "why did this model
//! run" report (`RoutingExplain`).
//!
//! `CapabilityIndex`/`CapabilityIndexBuilder` are re-exported here for
//! source compatibility but no longer defined in this crate (board item
//! 01KZFBZHTWDF11TH7G0H613ERE moved them to `conway_core::ports` --
//! "the backend side" -- since `from_backends` reads directly off
//! `Backend::capabilities` and nothing about the type is routing-policy
//! specific).
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
pub use explain::{
    BreakerSnapshot, CapabilitySummary, EntryOutcome, ExplainEntry, ExplainReport, RoutingExplain,
};
pub use router::DeclarativeRouter;
