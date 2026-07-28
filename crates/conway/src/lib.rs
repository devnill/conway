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
mod builder;
pub mod config;
mod conway;
mod error;
mod event_stream;
mod fork_child;
pub mod gates;
mod intent;
pub mod presets;
mod session_handle;
mod subagent_spec;

pub use builder::ConwayBuilder;
pub use conway::Conway;
pub use error::{ConwayError, Result};
pub use event_stream::EventStream;
pub use intent::AgentIntent;
pub use session_handle::{SessionHandle, SessionSpec, TurnHandle};
pub use subagent_spec::{ForkSpec, SpawnSpec};

pub use conway_core::agent::{
    AgentResult, AgentTreeSnapshot, Budget, PermissionDecision, PermissionDecisionKind,
    PermissionRequest, PermissionScope, ResultStatus, ToolSelector,
};
pub use conway_core::config::AgentDef;
pub use conway_core::permission_mode::PermissionMode;
pub use conway_core::permission_pattern::{PatternRule, PermissionFile};
pub use conway_core::content::{ToolCategory, Usage};
pub use conway_core::event::{Envelope, Event};
pub use conway_core::ids::{AgentId, LogSeq, ModelRef, RoleAlias, SegmentId, SessionId, ToolName};
pub use conway_core::log::{AskOrigin, LogRecord, SessionFilter, SessionMeta, SubagentMode};
pub use conway_core::ports::{
    Backend, HealthRegistry, PermissionGate, Plugin, Router, SessionStore, Tool,
};
pub use conway_core::provenance::{ContextReport, Provenance};
pub use conway_core::routing::{AttemptFailure, BreakerKind, BreakerState, RoutingReason};

// WI-116 (CARRIED F-111-1): `routes explain` needs `Conway::explain_routing`'s
// return type, and this is the one type in this crate's re-export list drawn
// from `conway_routing` rather than `conway_core` -- `ExplainReport` is
// defined in that crate (see `conway.rs`'s own `use conway_routing::{..,
// ExplainReport, ..}`), not duplicated here. Its own public field types
// (`ExplainEntry`, `EntryOutcome`, `CapabilitySummary`, `BreakerSnapshot`)
// are re-exported alongside it so a consumer can name every type reachable
// by field access without reaching past this facade into `conway_routing`
// directly.
pub use conway_routing::{
    BreakerSnapshot, CapabilitySummary, EntryOutcome, ExplainEntry, ExplainReport,
};
