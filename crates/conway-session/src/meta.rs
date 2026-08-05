//! Session metadata types.
//!
//! `conway-core::log` is the authoritative definition of every type
//! re-exported here (architecture §5.1, §4.4). `conway-session` adds no
//! fields and no behavior of its own — this module exists so that
//! `conway_session::{SessionMeta, ForkOrigin, SessionFilter}` is a stable,
//! crate-root-reachable path (per `lib.rs`'s re-export list) without every
//! downstream module reaching into `conway_core::log` directly.
//!
//! Do not redefine these types here. `LogRecord::Header` (conway-core)
//! already embeds `SessionMeta` and its wire form (the `session`/`agent`
//! rename, the `origin` field, etc.) is fixed there.

pub use conway_core::log::{ForkOrigin, SessionFilter, SessionMeta};
