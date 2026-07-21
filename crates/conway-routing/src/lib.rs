//! conway-routing: declarative role -> ordered-candidate resolution
//! (`DeclarativeRouter`), per-endpoint circuit breakers and background
//! health probing (`BreakerRegistry`, `HealthProber`), capability filtering
//! (`CapabilityIndex`), and the "why did this model run" report
//! (`RoutingExplain`).
//!
//! `conway-core` owns the port traits (`Router`, `HealthRegistry`) and the
//! content-free request/response/config types this crate operates on; this
//! crate provides the implementations. See `docs/plan/architecture.md`,
//! "Module: conway-routing", for the module contract, and
//! `docs/plan/wi-conway-routing*.md` for the work-item breakdown.
//!
//! No classifier, embedding model, or other learned component may be linked
//! into this crate, at MVP or ever, absent an explicit decision reversal
//! (GP-07).

mod breaker;
mod capability;
pub mod config;
mod explain;
pub mod failure;
mod prober;
mod router;

// The crate's re-export block (`DeclarativeRouter`, `BreakerRegistry`,
// `HealthProber`, `ProberHandle`, `CapabilityIndex`, `RoutingExplain`,
// `ExplainReport`) is authored incrementally by the work items that
// implement each type (WI-032 .. WI-036), landing as a single `pub use`
// block once `router.rs` (the last structural dependency, WI-034) exists.
// Until then the placeholder modules above contain no public items to
// re-export.
