#![deny(missing_debug_implementations)]

//! Conway-core: domain types and port traits for the conway agent harness.
//!
//! Every public type is `Serialize + Deserialize`. Implementations of the
//! port traits live in dedicated crates.
//!
//! **FORWARD DECLARATION — this crate does NOT yet perform no I/O.** One
//! module breaks it: [`containment`] calls `std::fs::canonicalize` at
//! `CanonicalRoot::new` and again in its walk-up loop. That is the whole of
//! the exception today, pinned by the `t2_core_io_is_confined_to_the_one_
//! known_file` guard in `crates/conway/tests/architecture_invariants.rs`, so
//! a second offender fails CI.
//! ("Retire the harness-level confinement root once conway.fs enforces its
//! own", under Stage 1.5) closes it by moving confinement out of this crate,
//! and **must delete this label when it lands.**

pub mod agent;
pub mod capabilities;
pub mod config;
pub mod containment;
pub mod content;
pub mod error;
pub mod event;
pub mod event_name;
pub mod failure;
pub mod hook;
pub mod ids;
pub mod log;
pub mod permission_mode;
pub mod permission_pattern;
pub mod ports;
pub mod provenance;
pub mod routing;
pub mod segment;
pub mod text;

pub mod prelude {
    pub use crate::{
        agent::*, capabilities::*, config::*, content::*, error::*, event::*, ids::*, log::*,
        ports::*, provenance::*, routing::*, segment::*,
    };
}
