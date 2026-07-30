#![deny(missing_debug_implementations)]

//! conway-core: domain types and port traits for the conway agent harness.
//!
//! This crate performs no I/O. Every public type is `Serialize + Deserialize`.
//! Implementations of the port traits live in dedicated crates; the only
//! implementations permitted here are test fakes behind `feature = "fakes"`.

pub mod agent;
pub mod capabilities;
pub mod config;
pub mod containment;
pub mod content;
pub mod error;
pub mod event;
#[cfg(feature = "fakes")]
pub mod fakes;
pub mod ids;
pub mod log;
pub mod permission_mode;
pub mod permission_pattern;
pub mod ports;
pub mod provenance;
pub mod routing;
pub mod segment;

pub mod prelude {
    pub use crate::{
        agent::*, capabilities::*, config::*, content::*, error::*, event::*, ids::*, log::*,
        ports::*, provenance::*, routing::*, segment::*,
    };
}
