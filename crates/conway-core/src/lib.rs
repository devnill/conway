#![deny(missing_debug_implementations)]

//! conway-core: domain types and port traits for the conway agent harness.
//!
//! This crate performs no I/O. Every public type is `Serialize + Deserialize`.
//! Implementations of the port traits live in dedicated crates; the only
//! implementations permitted here are test fakes behind `feature = "fakes"`.

pub mod agent;
pub mod capabilities;
pub mod config;
pub mod content;
pub mod error;
pub mod ids;
pub mod log;
pub mod provenance;
pub mod routing;
pub mod segment;
// pub mod event;        // WI-006
// pub mod ports;        // WI-007
// #[cfg(feature = "fakes")]
// pub mod fakes;        // WI-008

pub mod prelude {
    pub use crate::{
        agent::*, capabilities::*, config::*, content::*, error::*, ids::*, log::*, provenance::*,
        routing::*, segment::*,
    };
}
