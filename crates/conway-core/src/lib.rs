#![deny(missing_debug_implementations)]

//! conway-core: domain types and port traits for the conway agent harness.
//!
//! This crate performs no I/O. Every public type is `Serialize + Deserialize`.
//! Implementations of the port traits live in dedicated crates; the only
//! implementations permitted here are test fakes behind `feature = "fakes"`.

pub mod capabilities;
pub mod content;
pub mod error;
pub mod ids;
pub mod log;
pub mod routing;
// pub mod provenance;   // WI-003
// pub mod segment;      // WI-003
// pub mod agent;        // WI-005
// pub mod config;       // WI-005
// pub mod event;        // WI-006
// pub mod ports;        // WI-007
// #[cfg(feature = "fakes")]
// pub mod fakes;        // WI-008

pub mod prelude {
    pub use crate::{capabilities::*, content::*, error::*, ids::*, log::*, routing::*};
}
