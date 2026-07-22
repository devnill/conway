//! `conway`: the embeddable facade over the conway agent harness.
//!
//! Assembles `conway-core`'s ports and domain types with the concrete
//! `conway-runtime`, `conway-backends`, `conway-session`, `conway-routing`,
//! and `conway-tools` implementations behind one stable public API
//! (`ConwayBuilder` -> `Conway` -> `SessionHandle`). This crate is the
//! primary integration surface for embedders (e.g. a Tauri IDE) and for the
//! `conway` CLI.
//!
//! This item (WI-096) establishes the crate skeleton: dependency wiring,
//! the cargo feature flags below, the crate-level [`ConwayError`]/[`Result`],
//! and the curated re-export list from `conway-core`. Every other module
//! named in the facade module's implementation notes (`config`, `agents`,
//! `gates`, `presets`, `builder`, `conway`, `session_handle`,
//! `event_stream`) is added by its own owning work item, each of which
//! appends its own `mod` declaration here.
//!
//! No type from `conway-runtime` is re-exported here: this crate's public
//! surface is defined in terms of `conway-core`'s domain types and port
//! traits, plus the facade's own `ConwayBuilder`/`Conway`/`SessionHandle`
//! wrappers added by later work items.

pub mod agents;
pub mod config;
mod error;
pub mod gates;
pub mod presets;

pub use error::{ConwayError, Result};

pub use conway_core::agent::{AgentResult, AgentTreeSnapshot, Budget, ResultStatus};
pub use conway_core::config::AgentDef;
pub use conway_core::event::{Envelope, Event};
pub use conway_core::ids::{AgentId, LogSeq, ModelRef, RoleAlias, SessionId};
pub use conway_core::log::{LogRecord, SessionFilter, SessionMeta};
pub use conway_core::ports::{
    Backend, HealthRegistry, PermissionGate, Plugin, Router, SessionStore, Tool,
};
pub use conway_core::provenance::{ContextReport, Provenance};
