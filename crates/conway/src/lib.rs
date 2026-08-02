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
pub use conway::{Conway, PermissionLoadReport, RevokeOutcome};
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
pub use conway_core::permission_pattern::{
    PatternOrigin, PatternRule, PermissionFile, Rule, RuleRegistrationError,
    RuleRegistrationReason, Select, Then, When,
};
/// V2b: `conway-cli` reaches `parse_rules` through here (it cannot depend
/// on `conway-core` directly -- `no_forbidden_deps`).
pub mod permission_pattern {
    pub use conway_core::permission_pattern::*;
}
pub use conway_core::content::{ToolCategory, Usage};
pub use conway_core::event::{Envelope, Event};
pub use conway_core::ids::{AgentId, LogSeq, ModelRef, RoleAlias, SegmentId, SessionId, ToolName};
pub use conway_core::log::{AskOrigin, LogRecord, SessionFilter, SessionMeta, SubagentMode};
pub use conway_core::ports::{
    Backend, ContextHook, HealthRegistry, PermissionGate, Plugin, Router, SessionStore, Tool,
};

/// The GP-03 extension surface: every type a crate depending only on
/// `conway` needs to implement [`Plugin`], [`Tool`], and [`ContextHook`]
/// against the public API.
///
/// WHY A MODULE, NOT FLAT ROOT RE-EXPORTS (F8 decide-and-state): the root
/// is already a flat collection of session/config/routing domain types,
/// and twenty-odd extension-authoring names there would bury the signal
/// that this set is the one GP-03 makes a commitment about.
/// `use conway::plugin::...` reads as intent, and the facade already has
/// the curated-submodule precedent (`pub mod permission_pattern` above).
/// The port traits stay flat at the root where they always were; this
/// module is an additional grouped home, not a second location for
/// anything that moved.
///
/// WHY `ContextHook` IS EXPORTED rather than `with_context_hook` made
/// private (F8 decide-and-state): the method is shipped public API on the
/// builder and the capability behind it (masking, tool narrowing,
/// overflow retry) is real and documented; deleting it to avoid exporting
/// one type would remove functionality. Exporting the trait completes the
/// port list instead.
///
/// KNOWN GAP, stated rather than asserted away (GP-14): this surface does
/// NOT yet suffice to re-author every in-tree built-in facade-only. The
/// built-ins that drive capability handles with typed arguments name types
/// no `conway::` path reaches — `conway_ask`/`conway_subagent` construct
/// `SubagentSpec` and match host errors as `RuntimeError`, the report tool
/// constructs `Fact`, the cd tool matches `CwdError`. Widening the surface
/// (or deciding parity is not the goal) is board item
/// 01KYYB2T8AHB4SJFHNG4ZETYN8.
///
/// This is a *curated* re-export, deliberately narrower than
/// `conway_core::ports` (board item F8, `.design/extension-architecture.md`
/// §12 phase 0): the traits above were always re-exported at the crate
/// root, but their method signatures named types this facade never
/// re-exported, so an external crate could name `Tool` and could not write
/// `fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> ...`. Every name here
/// is justified by appearing in one of the three traits' signatures, in a
/// field of a type an implementor must construct (`ToolSpec`, `ToolOutput`,
/// `PluginManifest`, `PromptSegment`), or in a helper signature the
/// built-ins themselves use (`PluginConfig`, `CancellationToken` — see
/// `conway-tools`' `fs/mod.rs` and `subagent/ask.rs`).
///
/// Deliberately NOT here:
///
/// - `CwdHandle`, `EventSinkHandle`, `SubagentHost`, `EventSink` — they
///   appear only as `ToolCtx` *fields* an implementor reads (method calls
///   on `ctx.chdir`/`ctx.events`/`ctx.subagents` never name the type), and
///   the extension design (§13.5) rejects plugin *implementations* of
///   `SubagentHost`/`EventSink` outright. Constructing a `ToolCtx` by hand
///   (what those names are needed for) is test-fixture work, served by
///   `conway-core`'s `fakes` feature, not the authoring surface.
/// - The `SubagentHost`/`EventSink`/`SessionStore`/`Router`/
///   `HealthRegistry`/`Backend` implementation surfaces — §13.5 rejects
///   plugin implementations of those with stated reasons.
/// - `schemars`/`serde_json` — plain data-type crates a plugin author names
///   in their own `Cargo.toml` (version-matched to conway's; the compiler
///   enforces the match loudly). `async_trait` IS re-exported: the three
///   traits are `#[async_trait]`-transformed, and re-exporting the macro is
///   what makes `use conway::plugin::*` sufficient to write an impl.
pub mod plugin {
    pub use async_trait::async_trait;
    pub use conway_core::content::{
        Artifact, ArtifactKind, ContentBlock, PermissionClass, Role, ToolCall, ToolCategory,
        ToolSpec, TruncationPolicy,
    };
    pub use conway_core::error::ToolError;
    pub use conway_core::ids::ToolName;
    pub use conway_core::ports::{
        CancellationToken, ContextHook, ContextHookCtx, ContextPayload, OverflowInfo, PathArgs,
        Plugin, PluginConfig, PluginManifest, RenderKind, Tool, ToolCtx, ToolOutput,
    };
    pub use conway_core::provenance::Provenance;
    pub use conway_core::segment::PromptSegment;
}
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
