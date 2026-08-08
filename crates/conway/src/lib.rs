//! `conway`: the embeddable facade over the conway agent harness.
//!
//! Assembles `conway-core`'s ports and domain types with the concrete
//! `conway-runtime`, `conway-backends`, `conway-session`, and `conway-tools`
//! implementations behind one stable public API (`ConwayBuilder` -> `Conway`
//! -> `SessionHandle`). This crate is the primary integration surface for
//! embedders (e.g. a Tauri IDE) and for the `conway` CLI. Board item
//! 01KZFC43J1J06BM4CCWKCKHSNV: this crate no longer links a routing engine
//! at all -- `conway-plugin-routing` is an installable first-party plugin,
//! not one of the implementations assembled here; absent it, `build()`
//! compiles `conway_core::routing::MinimalRouter` instead.
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

pub use builder::{ConwayBuilder, PluginSelection};
pub use conway::{Conway, PermissionLoadReport, RevokeOutcome, TrustPermissionReport};
pub use error::{ConwayError, Result};
pub use event_stream::EventStream;
pub use intent::AgentIntent;
pub use session_handle::{SessionHandle, SessionSpec, TurnHandle};
pub use subagent_spec::{ForkSpec, SpawnSpec};

pub use conway_core::agent::{
    AgentResult, AgentTreeSnapshot, Budget, CancelMode, PermissionDecision, PermissionDecisionKind,
    PermissionRequest, PermissionScope, ResultStatus, ToolSelector,
};
pub use conway_core::config::AgentDef;
pub use conway_core::permission_mode::PermissionMode;
pub use conway_core::permission_pattern::{
    PatternOrigin, PatternRule, PermissionFile, Rule, RuleRegistrationError,
    RuleRegistrationReason, Select, Then, When,
};
/// A2: the scope an allow grant covers (`active_structured_allow_rules`
/// surfaces one per rule) -- re-exported so `conway-cli` can label a
/// structured-allow review row without depending on `conway-runtime`
/// (`no_forbidden_deps`).
pub use conway_runtime::permission::GrantScope;
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
    Backend, ContextHook, HealthRegistry, PermissionGate, Plugin, RenderKind, Router,
    RouterBuildContext, RouterBundle, RouterFactory, SessionStore, Tool,
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
/// RESOLVED (GP-14, board item 01KYYB2T8AHB4SJFHNG4ZETYN8): this surface
/// used to name a gap here — the report tool's `Fact`, the cd tool's
/// `CwdError`, and (once `SubagentHandle` landed, C1) `SubagentHandle`'s own
/// `SubagentError` were constructible/matchable only inside this crate's own
/// built-ins, not from a `conway`-only dependent. `Fact`, `CwdError`, and
/// `SubagentError` are exported below and each pinned by name in
/// `crates/conway/tests/plugin_surface.rs` (that file's own rule: an
/// unnamed export is an unguarded one). The stronger, whole-tool proof is
/// `crates/conway/tests/plugin_builtin_parity.rs`, which re-implements the
/// `invoke` logic of all seven subagent/report/cd built-ins against
/// `conway::` paths alone and fails to COMPILE if any of this surface
/// regresses — `plugin_surface.rs` pins the types, that file demonstrates
/// they are sufficient. Deliberately NOT widened alongside
/// them, and this is the closed set (GP-14 cuts both ways — no reach claimed
/// that isn't real):
///
/// - `SubagentSpec` (`conway_core::agent::SubagentSpec`) — a third-party
///   fork/spawn goes through this crate's own `ForkSpec`/`SpawnSpec`
///   instead (GP-02's visibly-distinct-types requirement), which convert
///   into `SubagentSpec` via `From` but are never themselves that type; a
///   plugin author has no reason to construct or match `SubagentSpec`
///   directly.
/// - `RuntimeError` (`conway_core::error::RuntimeError`) — `ToolCtx.
///   subagents: SubagentHandle`'s five fallible methods return
///   `SubagentError`, never `RuntimeError` (see that error's own doc for
///   the translation `SubagentHandle` performs at its boundary), so
///   `RuntimeError` has no reachable call site from this facade's surface
///   at all.
/// - `SubagentHost`/`CwdHandle`/`EventSinkHandle`/`SubagentHandle`/
///   `EventSink` (the host-capability traits and handle types themselves)
///   — see "Deliberately NOT here" below; unchanged by this resolution.
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
/// - `CwdHandle`, `EventSinkHandle`, `SubagentHandle`, `SubagentHost`,
///   `EventSink` — they appear only as `ToolCtx` *fields* an implementor
///   reads (method calls on `ctx.chdir`/`ctx.events`/`ctx.subagents` never
///   name the type — a tool calls `ctx.subagents.start(spec)` by method
///   dispatch, exactly like the pre-existing `ctx.chdir.set(..)` precedent),
///   and the extension design (§13.5) rejects plugin *implementations* of
///   `SubagentHost`/`EventSink` outright. Constructing a `ToolCtx` by hand
///   (what those names are needed for) is test-fixture work, served by
///   `conway-core`'s `fakes` feature, not the authoring surface. Re-checked
///   for `SubagentHandle` specifically when it landed (C1, board item
///   01KZ59SXNQ3BRXP49V4JW10N72): no concrete call site names it either.
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
    pub use conway_core::agent::Fact;
    pub use conway_core::content::{
        Artifact, ArtifactKind, ContentBlock, PermissionClass, Role, ToolCall, ToolCategory,
        ToolSpec, TruncationPolicy,
    };
    pub use conway_core::error::{ArtifactWriteError, CwdError, SubagentError, ToolError};
    pub use conway_core::ids::ToolName;
    pub use conway_core::ports::{
        ArtifactWriteHandle, ArtifactWriter, CancellationToken, ContextHook, ContextHookCtx,
        ContextPayload, OverflowInfo, PathArgs, Plugin, PluginConfig, PluginManifest, RenderKind,
        Tool, ToolCtx, ToolOutput,
    };
    pub use conway_core::provenance::Provenance;
    pub use conway_core::segment::PromptSegment;
}
pub use conway_core::provenance::{ContextReport, Provenance};
pub use conway_core::routing::{AttemptFailure, BreakerKind, BreakerState, RoutingReason};

// WI-116 (CARRIED F-111-1), amended by board item 01KZFC1KNGQ51TZ0BG7P7RAY9H:
// `routes explain` needs `Conway::explain_routing`'s return type.
// `ExplainReport` (and its own public field types -- `ExplainEntry`,
// `EntryOutcome`, `CapabilitySummary`, `BreakerSnapshot`) used to be defined
// in `conway_plugin_routing`; they now live in `conway_core::routing`, so
// any `Router` supplied from outside that crate (`ConwayBuilder::with_router`)
// can still produce one (see `conway_core::routing::MinimalRouter`) instead
// of a fabricated-empty report. Re-exported here from `conway_core` --
// same five names, same shapes -- so this facade's public surface is
// unchanged.
pub use conway_core::routing::{
    BreakerSnapshot, CapabilitySummary, EntryOutcome, ExplainEntry, ExplainReport,
};

// Board item 01KZFC2MD1FVNA674YJ9A19T8E, GP-03/P-6: `RouterFactory` joins the
// extension surface above, so the field types of what its `build` receives
// must be nameable by a crate depending only on `conway`. Without these two,
// a third-party factory could read `ctx.routing.roles` and call
// `ctx.headroom.resolve(..)` but could not write a helper taking
// `&RoutingConfig` or `&HeadroomPolicy` as a parameter -- it would have to
// depend on `conway-core` directly, which is exactly the privileged-interface
// asymmetry P-6 forbids. A port whose context cannot be spelled through the
// public facade is only half-installed.
pub use conway_core::capabilities::HeadroomPolicy;
pub use conway_core::routing::RoutingConfig;
