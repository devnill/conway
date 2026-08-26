//! `conway`: the embeddable facade over the conway agent harness.
//!
//! Assembles `conway-core`'s ports and domain types with the concrete
//! `conway-runtime`, `conway-session`, and `conway-tools` implementations
//! behind one stable public API (`ConwayBuilder` -> `Conway` ->
//! `SessionHandle`). This crate is the primary integration surface for
//! embedders (e.g. a Tauri IDE) and for the `conway` CLI.
//!: this crate no longer links a routing engine
//! at all -- `conway-plugin-routing` is an installable first-party plugin,
//! not one of the implementations assembled here; absent it, `build()`
//! compiles `conway_core::routing::MinimalRouter` instead.
//!: the same is now true of both provider-adapter
//! dialects -- `conway-plugin-backends` is a first-party plugin too, and no
//! production resolution path in this crate names either `"anthropic"` or
//! `"openai-compat"`: `resolve_backend_factory` matches a
//! `[backends.<id>]` entry's `kind` only against whichever
//! `BackendFactory`s a caller registered, and absent one, `build()` fails
//! naming every kind it does recognise.
//!
//! This item establishes the crate skeleton: dependency wiring,
//! the cargo feature flags below, the crate-level [`FacadeError`]/[`Result`],
//! and the curated re-export list from `conway-core`. Every other module
//! named in the facade module's implementation notes (`config`, `agents`,
//! `gates`, `presets`, `builder`, `conway`, `session_handle`,
//! `event_stream`) is added by its own owning work item, each of which
//! appends its own `mod` declaration here.
//!
//! This crate's public surface is defined in terms of `conway-core`'s domain
//! types and port traits, plus the facade's own `ConwayBuilder`/`Conway`/
//! `SessionHandle` wrappers added by later work items.

pub mod agents;
mod builder;
pub mod config;
mod conway;
mod discovery_host;
mod error;
mod event_stream;
mod fork_child;
pub mod gates;
mod host_caps;
mod intent;
pub mod memory;
mod output_schema;
mod permissions;
pub mod presets;
mod session_handle;
pub mod skills;
mod subagent_spec;

pub use builder::{ConwayBuilder, PluginSelection};
pub use config::trust::TrustStatus;
pub use conway::{Conway, HookRuleView, DENY_CAPABLE_EVENTS};
pub use error::{FacadeError, Result};
pub use event_stream::EventStream;
pub use host_caps::HostCaps;
pub use intent::AgentIntent;
pub use output_schema::compile_output_schema;
pub use permissions::{PermissionLoadReport, RevokeOutcome, TrustPermissionReport, TrustPreview};
pub use session_handle::{SessionHandle, SessionSpec, TurnHandle};
pub use subagent_spec::{ForkSpec, SpawnSpec};

pub use conway_core::agent::{
    AgentResult, AgentTreeSnapshot, Budget, CancelMode, GrantScope, PermissionDecision,
    PermissionDecisionKind, PermissionRequest, PermissionScope, ResultStatus, ToolSelector,
};
pub use conway_core::config::AgentDef;
/// Re-exported so a crate depending only on `conway` (the facade -- the
/// surface a third-party plugin author gets, per GP-03) can name the type
/// `conway::skills::load_skill_defs` returns in its own signature. A plugin
/// that consumes skill defs (e.g. `conway-plugin-skills`' progressive-
/// disclosure hook/tool) needs to name `SkillDef` to construct itself from
/// a loaded skills map; without this re-export it would have to depend on
/// `conway-core` directly, the exact shortcut the plugin tier's "facade
/// only" discipline exists to avoid.
pub use conway_core::config::SkillDef;
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
pub use conway_core::ids::{
    AgentId, LogSeq, MemoryId, ModelRef, RoleAlias, SegmentId, SessionId, ToolName,
};
pub use conway_core::log::{AskOrigin, LogRecord, SessionFilter, SessionMeta, SubagentMode};
pub use conway_core::ports::{
    Backend, BackendBuildContext, BackendFactory, ContextHook, HealthRegistry, PermissionGate,
    Plugin, RenderKind, Router, RouterBuildContext, RouterBundle, RouterFactory, SessionStore,
    Tool,
};

/// The shared error type [`RouterFactory::build`] and [`BackendFactory::
/// build`] both return -- `conway_core::error::ConwayError`, distinct from
/// this crate's own umbrella [`error::FacadeError`] (board item CON-3
/// renamed the latter so the two no longer share a bare name). Re-exported
/// under `CoreConwayError` so a factory implementation can spell
/// `RouterFactory`/`BackendFactory`'s own signature type from a crate
/// depending only on `conway`, without a direct `conway-core` dependency.
pub use conway_core::error::ConwayError as CoreConwayError;

/// The extension surface -- there is exactly one extension mechanism, and
/// this is it: every type a crate depending only on
/// `conway` needs to implement [`Plugin`], [`Tool`], [`ContextHook`], and
/// [`plugin::HookRunner`] against the
/// public API.
///
/// WHY A MODULE, NOT FLAT ROOT RE-EXPORTS (F8 decide-and-state): the root
/// is already a flat collection of session/config/routing domain types,
/// and twenty-odd extension-authoring names there would bury the signal
/// that this set is the one the extension mechanism makes a commitment about.
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
/// RESOLVED: this surface
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
/// them, and this is the closed set (the rule cuts both ways — no reach claimed
/// that isn't real):
///
/// - `SubagentSpec` (`conway_core::agent::SubagentSpec`) — a third-party
///   fork/spawn goes through this crate's own `ForkSpec`/`SpawnSpec`
///   instead (fork and spawn stay visibly distinct types), which convert
///   into `SubagentSpec` via `From` but are never themselves that type; a
///   plugin author has no reason to construct or match `SubagentSpec`
///   directly.
/// - `RuntimeError` (`conway_core::error::RuntimeError`) — `ToolCtx.
///   subagents: SubagentHandle`'s five fallible methods return
///   `SubagentError`, never `RuntimeError` (see that error's own doc for
///   the translation `SubagentHandle` performs at its boundary), so
///   `RuntimeError` has no reachable call site from this facade's surface
///   at all.
/// - `SubagentHost`/`CwdHandle`/`SubagentHandle` (the host-capability
///   traits and handle types that remain unreachable through this module)
///   — see "Deliberately NOT here" below; unchanged by this resolution.
///   `EventSinkHandle`/`EventSink` are NOT grouped with those three: a real
///   production injection point exists for them, so they are exported, not
///   absent — see the same section for why.
///
/// This is a *curated* re-export, deliberately narrower than
/// `conway_core::ports` (F8, the extension design phase 0): the traits above were always re-exported at the crate
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
/// - `CwdHandle`, `SubagentHandle`, `SubagentHost` — they appear only as
///   `ToolCtx` *fields* an implementor reads (method calls on
///   `ctx.chdir`/`ctx.subagents` never name the type — a tool calls
///   `ctx.subagents.start(spec)` by method dispatch, exactly like the
///   pre-existing `ctx.chdir.set(..)` precedent).
///
///   `SubagentHost` has no builder injection point at all, and none is
///   coming: this is a decided design, not an unfilled seam. Fork and spawn
///   are mechanism with exactly one implementation, and the runtime that
///   keeps the log is the only thing that may fork it (INTENT.md §7 —
///   *"if it wants them" means uncalled, not replaced*); a second
///   `SubagentHost` would be a second authority over what a session's
///   ancestry means. No `pub fn with_*` in `crates/conway/src/builder.rs`
///   accepts one. `HostCaps::from_config`
///   (`crates/conway/src/host_caps.rs`) states the identical ruling from
///   the capability-advertisement side: the host always offers
///   `HostCapability::Subagent` because the runtime always provides the
///   implementation, never because an embedder chose to supply one.
///   `crates/conway-core/src/ports/subagent.rs`'s own module doc ("Exactly
///   one intended implementor") states the same invariant a third time, at
///   the port itself (INTENT.md §8.6 — an invariant belongs to the seam,
///   not to its call sites). (§13.5 is a non-goals list for the OUT-OF-PROCESS
///   subprocess transport and was never authority for this in-process
///   question — the extension design's own 2026-08-09 dated status note
///   said so.)
///
///   `CwdHandle`/`SubagentHandle` stay absent for a DIFFERENT reason now
///   than they used to (see the dated note below): assembling them by hand
///   is no longer what a third party has to do at all.
///   Constructing a `ToolCtx` by hand (what those names used to be needed
///   for) is test-fixture work, not the authoring surface.
///
///   NOTE, corrected 2026-08-10, again 2026-08-15 (board item
///   01KZVYWNA24EYMPVW3NPGBW51M, "Extract conway-testkit"), and again
///   2026-08-15 (board item 01KZQ3AZWG3NNJNZEJFX21MDJT, "ToolCtx carries the
///   same construction tax `ContextHookCtx` just shed"): the 2026-08-10
///   correction said hand-constructing a `ToolCtx` was "served by
///   `conway-core`'s `fakes` feature" inside this workspace and unreachable
///   for a third party, because `crates/conway/Cargo.toml` took
///   `conway-core` with `features = ["fakes"]` only under
///   `[dev-dependencies]`. 01KZVYWNA24EYMPVW3NPGBW51M closed that half: the
///   doubles moved to `conway-testkit`, a crate of its own, forwarded here
///   behind this crate's own `testkit` feature (`pub mod testkit`, below
///   `pub mod backend`) — `FakeSubagentHost`/`CollectingEventSink` are
///   reachable as `conway::testkit::{FakeSubagentHost,
///   CollectingEventSink}`. What it left open was the other half:
///   `SubagentHandle`/`EventSinkHandle` themselves — the types
///   `ToolCtx.subagents`/`.events` actually hold — were still not nameable
///   through this facade, so wrapping one of the now-reachable doubles into
///   a working `ToolCtx` still required a type this module did not export.
///   `ToolCtx` therefore still carried PART of the construction tax
///   `ContextHookCtx` carried until `ArtifactWriteHandle::noop` closed it.
///
///   01KZQ3AZWG3NNJNZEJFX21MDJT closes the rest — WITHOUT exporting either
///   type, and without re-litigating the settled choices above (the
///   doubles' home and how they are forwarded). `ToolCtx::for_test`
///   (`conway_core::ports::ToolCtx::for_test`, re-exported here as an
///   associated function on `ToolCtx` itself, so nothing new appears in
///   this list) builds the `chdir`/`subagents` fields internally from an
///   `AgentId` and a `cwd`, taking only `subagents`/`events` as `Arc<dyn
///   SubagentHost>`/`Arc<dyn EventSink>` parameters — coercion targets, not
///   named types, so passing `Arc::new(FakeSubagentHost::new(agent_id))` /
///   `Arc::new(CollectingEventSink::new())` needs neither trait nor handle
///   type named at the call site. That is a *different* answer than
///   `ArtifactWriteHandle::noop`'s for the identical-looking problem, on
///   purpose: unlike a `ContextHookCtx` fixture for a hook that never
///   writes, a `Tool::invoke` test usually wants to assert a subagent
///   started or an event fired, so silently defaulting both to no-ops would
///   make the common case unwritable — `for_test` takes them as required
///   parameters instead of defaulting them. See that constructor's own doc
///   (`crates/conway-core/src/ports/plugin.rs`) for the full reasoning, and
///   `conway_core::ports`'s own module doc for why this is a second,
///   no-longer-unprecedented "kind 2" test-fixture constructor rather than
///   the builder/`#[non_exhaustive]` combination `ToolCtx`'s own doc
///   rejects as disproportionate for ordinary struct-literal construction.
///
///   `EventSinkHandle`/`EventSink` are NOT part of this closed list, and an
///   earlier version of this doc was wrong to say they were: it grouped
///   them with `SubagentHost` as sharing "no builder injection point at
///   all," and that stopped being true the moment `Plugin::observe_sink`
///   (`conway_core::ports::Plugin::observe_sink`) shipped. A plugin
///   author implements `EventSink` on their own type and returns
///   `Some(handle)` from `observe_sink` to receive a copy of the host's
///   live event stream as an observer; `ConwayBuilder::build`
///   (`crates/conway/src/builder.rs`) collects every installed plugin's
///   `observe_sink` and spawns the forwarding task that wires it in — a
///   real, production, in-process injection point, not a theoretical one.
///   `EventSink`/`EventSinkHandle` are exported from this module for
///   exactly the reason `ContextHook` is above: the capability is real and
///   shipped, so naming the type completes the port list instead of
///   leaving it authorable only in theory. What IS still true of
///   `EventSink`, and is a narrower claim than "no injection point at
///   all": no `ConwayBuilder::with_*` method lets an embedder replace what
///   `ToolCtx.events` itself holds (that field is always the runtime's own
///   internal sink, unlike the separate `observe_sink` path above) — but
///   that narrower absence is not why `EventSink` is unreachable through
///   this module, because it isn't unreachable.
/// - The `SessionStore`/`HealthRegistry` implementation surfaces.
///   `SessionStore` because *implementing* it means spelling
///   `SessionStore::append`'s own signature, and the full set that requires
///   is not re-exported. Note the narrower *calling* surface IS reachable as
///   of D1-8: `CurateCtx` hands a curator a live `Arc<dyn SessionStore>` and
///   its module doc advertises `ctx.store.read(...)`, so `SeqRange` and
///   `StoreError` are re-exported from `plugin` below — without them that
///   advertised read surface would not compile from a facade-only crate.
///   Implementing the port from outside remains out of scope;
///   `HealthRegistry` because, like `SubagentHost` above, no
///   `ConwayBuilder::with_*` method injects a replacement. Both checked by
///   compiling a facade-only scratch crate against each claim, not by
///   reading.
///   `Backend` used to be named alongside these here; a later change added
///   `pub mod backend` (below, a second
///   curated module beside this one — see its own doc for why it is
///   separate) specifically to make third-party `Backend` implementations
///   possible, so `Backend` is no longer part of this closed list.
///
///   `Router` stays on this list for a narrower reason than it used to: a
///   wholly new `impl Router`
///   still needs `RouteRequest`/`Route`/`RoutingError`, none of which this
///   facade re-exports, so *authoring* a new routing algorithm is unchanged
///   and this curated module still names none of those three types. But
///   *installing* one, once built — whether built inside this workspace or
///   by a crate willing to take the `conway-core` dependency that authoring
///   still requires — now has a real, tested, facade-only mechanism this
///   module does not carry:
///   [`RouterFactory`]/[`RouterBuildContext`]/[`RouterBundle`], re-exported
///   a few dozen lines above this doc comment, and
///   `ConwayBuilder::with_router_factory`. See `docs/embedding.md`'s "What's
///   reachable from the library, and what isn't" table and its "Installing
///   a router" section for the full authoring-vs-installing distinction, and
///   `crates/conway/tests/router_factory.rs` for the installation path
///   exercised end to end.
/// - `schemars`/`serde_json` — plain data-type crates a plugin author names
///   in their own `Cargo.toml` (version-matched to conway's; the compiler
///   enforces the match loudly). `async_trait` IS re-exported: the three
///   traits are `#[async_trait]`-transformed, and re-exporting the macro is
///   what makes `use conway::plugin::*` sufficient to write an impl.
pub mod plugin {
    pub use async_trait::async_trait;
    pub use conway_core::agent::{Fact, ResultStatus};
    pub use conway_core::content::{
        Artifact, ArtifactKind, ContentBlock, PermissionClass, Role, ToolCall, ToolCategory,
        ToolSpec, TruncationPolicy,
    };
    /// The other half of the §11.5 read surface. `CurateCtx::store` is a live
    /// `Arc<dyn SessionStore>`, and the ONLY way to call its `read` is
    /// `ctx.store.read(&sid, SeqRange::full())` — so a curator that cannot
    /// name `SeqRange` cannot use the cross-session read the port exists to
    /// provide, and one that cannot name `StoreError` cannot handle its
    /// failure. Both are re-exported for *calling* `SessionStore`, which is
    /// distinct from *implementing* it (still out of scope — see the
    /// `forbidden`-types discussion on this module's parent).
    /// `crates/conway/tests/plugin_surface.rs` compiles a facade-only
    /// curator against exactly this surface so the claim is checked rather
    /// than asserted.
    pub use conway_core::error::StoreError;
    pub use conway_core::error::{
        ArtifactWriteError, CwdError, HookFailure, MemoryStoreError, SubagentError, ToolError,
    };
    pub use conway_core::event::Event;
    /// The SAME core-vs-plugin
    /// namespace rule `conway_core::event_name::validate_event_name`
    /// already enforces for plugin-declared events, reused (not
    /// reinvented) for plugin-declared TUI command names -- see that
    /// function's own doc for why both share one implementation.
    /// `conway-cli`'s `CommandRegistry::build` is this function's one
    /// caller today; re-exported here so a THIRD-PARTY plugin/embedder
    /// building its own command registry gets the identical rule, not a
    /// private one `conway-cli` alone can reach (production code outside
    /// this facade cannot depend on `conway-core` directly --
    /// `crates/conway-cli/tests/cli_surface.rs`'s `no_forbidden_deps`
    /// guard).
    pub use conway_core::event_name::validate_command_name;
    /// The domain types one
    /// `HookRunner::run` invocation carries in and out -- `HookInvocation`
    /// is the argument, `HookEvent` is its `event` field, and
    /// `HookAnswer`/`HookPermissionVerdict` together are what a
    /// `pre_tool_use` implementor returns (see `HookPermissionVerdict`'s
    /// own doc for why it has no `Allow` variant).
    pub use conway_core::hook::{HookAnswer, HookEvent, HookInvocation, HookPermissionVerdict};
    pub use conway_core::ids::SeqRange;
    pub use conway_core::ids::ToolName;
    /// [`Plugin::events`]'s own
    /// return type -- a plugin author constructs one of these per custom
    /// event it declares, the SAME `name`+`summary` shape [`CommandSpec`]
    /// establishes for commands, plus `carries_tool_name` (whether a
    /// `[hooks].rules[]` entry may pair this event with `match`).
    pub use conway_core::ports::EventDecl;
    /// [`Plugin::instructions`]'s own return-type element -- a plugin
    /// author constructs one of these per instruction fragment it
    /// declares, the SAME "declare, host attributes/checks it" shape
    /// [`EventDecl`]/[`CommandSpec`] establish immediately above and below.
    pub use conway_core::ports::InstructionFragment;
    /// [`Plugin::description`]'s own return type -- see that method's own
    /// doc for why this is a distinct type from [`InstructionFragment`],
    /// argued rather than assumed (two audiences, two cardinalities).
    pub use conway_core::ports::PluginDescription;
    pub use conway_core::ports::{
        ArtifactWriteHandle, ArtifactWriter, CancellationToken, Command, CommandCtx,
        CommandOutcome, CommandSpec, ContextHook, ContextHookCtx, ContextPayload, CurateCtx,
        CurateOutcome, Curator, EventSink, EventSinkHandle, HookRunner, HostCapability, Memory,
        MemoryProvenance, MemoryStore, ObservedCall, ObserverAnswer, ObserverCtx, ObserverNote,
        OverflowInfo, PathArgs, Plugin, PluginConfig, PluginEventHandle, PluginManifest,
        PluginPermissionRule, PluginPermissionVerdict, PluginStatusContribution,
        RegisteredObserver, RenderKind, Tool, ToolCtx, ToolObserver, ToolOutput,
    };
    /// Edge B's plugin -> plugin capability CALL channel (board item
    /// `01M0WWNHQQYN1EVTH8WPZ33EBF`,
    /// `docs/vision/DESIGN-plugin-dependencies.md` §2;
    /// `conway_core::ports::capability`'s own module doc has the full
    /// design). `CapabilityProvider` is what a `Plugin::capabilities`
    /// implementor implements; `CapabilityRegistration` pairs one with the
    /// `HostCapability` name it answers for; `CapabilityError` is what a
    /// provider returns on failure; `CapabilityCallError`/`CapabilityHost`/
    /// `CapabilityRegistry`/`CapabilityCallHandle` are the caller-facing
    /// dispatch machinery (`ToolCtx::capabilities` is a
    /// `CapabilityCallHandle`, re-exported above already via `ToolCtx`
    /// itself needing no separate name). Re-exported here for the SAME
    /// reason every other plugin-facing type in this module is: a
    /// third-party plugin implementing `Plugin::capabilities` needs to
    /// name every one of these without depending on `conway-core` directly.
    pub use conway_core::ports::{
        CapabilityCallError, CapabilityCallHandle, CapabilityError, CapabilityHost,
        CapabilityProvider, CapabilityRegistration, CapabilityRegistry,
    };
    pub use conway_core::provenance::Provenance;
    pub use conway_core::segment::PromptSegment;
    /// The memoised effective-transcript resolver a [`CurateCtx`] carries
    /// (DESIGN §11.5): a curator may resolve any session's transcript via
    /// `ctx.resolver.resolve(&ctx.store, &sid)`. Re-exported here because
    /// `CurateCtx`'s `pub resolver: Arc<TranscriptResolver>` field would
    /// otherwise be unspellable from a crate depending only on `conway`.
    pub use conway_core::transcript::TranscriptResolver;

    /// The Drop-time / timeout-time process-group SIGKILL a `Tool` or
    /// `ContextHook` that spawns a child process needs so the child does
    /// not outlive its spawner: SIGTERM the whole group, give it a grace
    /// period, SIGKILL and reap if it hasn't exited.
    ///
    /// **Why this landed here (board item `01M0EKVR1BEXXS75NV2JC4HZZ9`).**
    /// The identical ~15-line sequence was hand-copied five times across
    /// three crates -- `conway-tools` (behind a private module), and
    /// `conway-plugin-subprocess`/`conway-plugin-mcp` (each copied it
    /// because the plugin tier may not depend on `conway-tools` directly,
    /// and a private module gave them no other way to reach it). That is
    /// not a tidiness problem, it is a gap in the extension surface: conway
    /// asks a plugin author to spawn and reap child processes and gave
    /// them no supported way to do the reaping. Three ways to close it were
    /// weighed, not assumed: publish `conway-tools` itself (cheapest, and
    /// wrong -- it stays an internal engine crate `conway-cli`'s own
    /// `no_forbidden_deps` guard exists to keep plugin-tier code out of);
    /// leave it duplicated and document the split (legitimate in general,
    /// but five SILENT copies were an accident, not a decision, and this
    /// primitive is exactly the kind of thing every other plugin-facing
    /// capability in this module already reaches through this same
    /// facade); or re-export it here, alongside everything else a plugin
    /// author needs. This module took the third option: `Tool`/
    /// `ContextHook` are already only reachable through `conway::plugin`,
    /// and a plugin that spawns a child is exercising the exact same kind
    /// of capability `ToolCtx`'s other handles already broker -- putting
    /// one more process primitive next to them is consistent, not a
    /// widening of what this facade is for.
    ///
    /// Gated on `builtin-tools` (default-on), UNLIKE `Tool`/`ContextHook`
    /// themselves (those come from `conway_core::ports`, unconditionally):
    /// this implementation lives in `conway_tools::process` (see that
    /// module's own doc for the five-way diff and the one behavioral
    /// difference it resolved), the same optional, default-on crate
    /// `ConwayBuilder`'s own built-in-tools wiring depends on
    /// (`crates/conway/src/builder.rs`'s `#[cfg(feature = "builtin-tools")]`
    /// items). A binary that disables `conway`'s default features loses
    /// this the same way it loses every other `conway-tools`-sourced name
    /// in this crate; it still owns its own reaping in that configuration,
    /// exactly as it would if built-in tools had never existed.
    #[cfg(all(unix, feature = "builtin-tools"))]
    pub use conway_tools::process::unix::{kill_group, TERM_GRACE};

    /// The shared child-process SESSION lifecycle (spawn once, an
    /// id-correlated NDJSON round trip, a per-call timeout, and fail-closed
    /// teardown) `conway_plugin_mcp::session::McpSession` and
    /// `conway_plugin_subprocess::session::PersistentSession` each build
    /// their OWN wire dialect on top of (board item
    /// `01M0TV7ZDS8X4F4TEJPRZB9P6T`).
    ///
    /// **Extends this facade's existing route, does not invent a third.**
    /// [`ChildSession`] is re-exported from `conway_tools::process::
    /// child_session` the SAME way [`kill_group`] immediately above already
    /// is, for the SAME reason: it calls `conway_tools::process::unix::
    /// kill_group` directly on its own timeout path, so it needs exactly
    /// `kill_group`'s own dependency (`conway-tools`' nix-backed
    /// process-group signalling), present only when the optional,
    /// default-on `builtin-tools` feature pulls `conway-tools` in. Gated
    /// identically: `cfg(all(unix, feature = "builtin-tools"))`.
    ///
    /// **What moved here, and what deliberately did not.** The pending-table
    /// and its dead/death-reason bookkeeping, the long-lived reader task,
    /// the write-then-await round trip, the graceful timeout kill, and the
    /// synchronous `Drop`-time SIGKILL -- the mechanics every child-process
    /// session in this workspace needs, and a SAFETY property (fail-closed
    /// on child death/timeout/malformed frame), not a stylistic choice.
    /// Each wire dialect's OWN request/response shapes, version negotiation,
    /// and per-point refuse-vs-degrade rules stay in their owning crate
    /// (INTENT §8.10: "similar is not duplicate") -- see
    /// `conway_tools::process::child_session`'s own module doc for the full
    /// argument and the divergence this extraction preserves rather than
    /// collapses (`NotificationRoute`'s two variants).
    ///
    /// Each crate's own public error enum (`McpPluginError`/
    /// `SubprocessPluginError`) is UNCHANGED by this -- same variants, same
    /// `Display` text -- by implementing [`ChildSessionError`] as a thin,
    /// one-line-per-variant mapping onto its own type.
    #[cfg(all(unix, feature = "builtin-tools"))]
    pub use conway_tools::process::child_session::{
        ChildSession, ChildSessionError, NotificationRoute, PendingGuard,
    };

    /// Applied when a plugin-host spec (`conway_plugin_mcp::McpPluginSpec`,
    /// `conway_plugin_subprocess::SubprocessPluginSpec`) does not name its
    /// own `timeout_ms`: long enough for a typical local plugin process
    /// (MCP server or subprocess tool) to answer, short enough that a hung
    /// child cannot silently stall an agent turn indefinitely.
    ///
    /// **One authority, not two (board item `01M0TV6E2K6QF9VXP6C7TFH06X`).**
    /// `conway-plugin-mcp` and `conway-plugin-subprocess` each used to
    /// declare their own `pub const` naming the same 5000ms value and
    /// carry a "must match" doc comment as the only enforcement -- nothing
    /// actually checked the two literals agreed. This constant is now
    /// defined ONCE, here; both crates re-export it (`pub use
    /// conway::plugin::DEFAULT_TIMEOUT_MS`) rather than restating it, so
    /// their old public paths still resolve but there is exactly one place
    /// the value can be edited.
    ///
    /// **A third caller draws from the same authority now, by the same
    /// route (board item `01M0TX5EB6WDK6W4WKZJ29AD9F`).**
    /// `crates/conway/src/config/schema.rs`'s `default_hook_timeout_ms`
    /// backs `HookEntry::timeout_ms`, `SubprocessPluginEntry::timeout_ms`,
    /// and `McpPluginEntry::timeout_ms` -- a hook callout, a subprocess
    /// plugin call, and an MCP round trip are three shapes of the identical
    /// underlying risk (an operator-configured local child process this
    /// crate spawned and must not let stall a turn forever), so that item
    /// answered "same knowledge" and had that function RETURN this constant
    /// instead of restating the literal a second time in this crate --
    /// `default_hook_timeout_ms` no longer states "the identical value" in
    /// prose, it IS this value, structurally. Both are still free to diverge
    /// in a LATER item that argues a concrete reason (e.g. hooks running
    /// operator scripts of unpredictable length); nothing here forecloses
    /// that, it only removes the "must match, nothing enforces it" defect
    /// while they happen to agree.
    ///
    /// **Why here, not sourced from `conway-tools` like [`kill_group`]
    /// immediately above.** `kill_group` needs `conway-tools`' `nix`-backed
    /// process-group signalling, which is genuinely unix-only and only
    /// exists when the optional, default-on `builtin-tools` feature pulls
    /// `conway-tools` in -- so its re-export is gated
    /// `cfg(all(unix, feature = "builtin-tools"))`, matching where the
    /// dependency is actually only sometimes present. This constant has no
    /// such dependency: it is plain data, needed unconditionally by
    /// `McpPluginSpec::new`/`SubprocessPluginSpec::new`, which compile and
    /// run on every platform regardless of `builtin-tools` (that feature
    /// governs conway's OWN built-in `Tool` implementations, not whether a
    /// plugin host may construct a spec with a default timeout). Gating
    /// this constant the same way `kill_group` is gated would break both
    /// constructors on a non-unix target or with default features off --
    /// the facade would have to learn a constraint (unix-only,
    /// builtin-tools-only) this value has no reason to carry. So it is
    /// declared directly here, ungated, rather than routed through
    /// `conway-tools`.
    pub const DEFAULT_TIMEOUT_MS: u64 = 5000;
}

/// The `Backend` authoring surface:
/// every type a crate depending only on `conway` needs to write `impl
/// conway::Backend for MyBackend`. `Backend`'s trait itself has been
/// re-exported at this crate's root for some time, but nothing its five
/// methods' signatures name was, so a facade-only crate could name the
/// trait and could not implement it — full stop, not "mostly" (the
/// preceding item's own compile evidence,
/// the compile evidence: 17 unresolved-name
/// errors against a scratch crate that named every one of the trait's own
/// types through `conway::` paths). This list was derived the same way —
/// by compiling, not by reading the trait — and `crates/conway/tests/
/// backend_parity.rs` is that same compile check kept alive as a
/// permanent regression guard.
///
/// WHY A SEPARATE MODULE, NOT FOLDED INTO `pub mod plugin` ABOVE (F8
/// decide-and-state, the same choice `pub mod plugin` states for itself):
/// `Backend` is not a `Tool`/`Plugin`/`ContextHook`. It is selected by
/// `backends.<id>.kind` in configuration — one adapter per LLM provider
/// dialect — not registered
/// in-process alongside a session's tools, and its authoring surface is
/// twenty names deep on its own. Folding it into `pub mod plugin` would
/// bury that module's own stated promise ("the set an in-process
/// tool/plugin author needs") under names a tool author never touches;
/// keeping them apart keeps both promises narrow and true. `use
/// conway::backend::...` reads as intent the same way `use
/// conway::plugin::...` already does.
///
/// This module sits inside the shape
/// settled: both shipped adapters
/// will install by default through a new declarative key (not folded into
/// `[plugins].install`), the adapter crate is being renamed
/// `conway-plugin-backends`, and `backends.<id>.kind` is becoming an open
/// name rather than a closed enum. None of that is this module's job —
/// later chain items own the installation story — this module only makes
/// the Rust trait nameable and implementable, independent of how an
/// implementation eventually gets installed.
///
/// Every name below is justified by appearing in `Backend`'s own method
/// signatures (`crates/conway-core/src/ports/backend.rs`) or in a public field
/// of one of those signatures' types — the no-unreached-claims rule cuts both
/// ways, so see "Deliberately NOT here" below for the names one level further
/// down that are NOT duplicated a third time:
///
/// - `Backend` — already re-exported at this crate's root; duplicated here
///   (matching `Tool`/`Plugin`/`ContextHook`'s identical dual export inside
///   `pub mod plugin` above) so `use conway::backend::*` alone names both
///   the trait and everything its impl block needs.
/// - `BackendId` — `Backend::id`'s return type.
/// - `ModelId` — `Backend::capabilities`'s `model` parameter and
///   `GenerateRequest::model`.
/// - `Capabilities` — `Backend::capabilities`'s return type.
/// - `ToolCallSupport`, `CacheMode`, `StructuredOutput`, `ReliabilityTier`
///   — `Capabilities`'s own fields; a `capabilities()` implementation
///   cannot construct its return value without them.
/// - `GenerateRequest` — `Backend::generate`/`::stream`/`::admit`'s shared
///   request parameter.
/// - `SamplingParams`, `PrefixKey` — `GenerateRequest`'s own fields, with
///   no existing re-export anywhere else in this facade before this item.
/// - `PromptSegment`, `ToolSpec` — `GenerateRequest::segments`/`::tools`'
///   element types; a caller building a request (this module's own parity
///   test, standing in for `conway-runtime`'s real one) must construct
///   these.
/// - `GenerateResponse` — `Backend::generate`'s `Ok` type, and
///   `StreamChunk::Done`'s payload.
/// - `ContentBlock`, `ToolCall` — `GenerateResponse`'s own fields; a
///   `generate()` implementation cannot construct its return value without
///   them.
/// - `StopReason` — `GenerateResponse::stop`'s type, with no existing
///   re-export anywhere else in this facade before this item.
/// - `Usage` — `GenerateResponse::usage`'s type. Already re-exported at
///   this crate's root (`conway::Usage`); duplicated here so this module
///   is self-sufficient on its own, the same choice already made for
///   `Provenance` (re-exported at both the root and inside `pub mod
///   plugin` above).
/// - `StreamChunk`, `BoxStream` — `Backend::stream`'s item and return
///   types.
/// - `ProbeReport` — `Backend::probe`'s `Ok` type.
/// - `BackendError` — every method's `Err` type.
/// - `Admission`, `check_admission` — `Backend::admit`'s `Ok` type and the
///   ONE arithmetic implementation every override — including this
///   module's own parity test's — MUST call rather than restating
///   `est_tokens + headroom_tokens <= max_context_tokens` itself.
///   `check_admission` is not decoration: an author who cannot name it
///   cannot honour `Backend::admit`'s contract.
/// - `TokenCountFidelity` — `Backend::token_fidelity`'s return type (board
///   item 01M0AP4ADTGJWF3GFMCFWFF1ZQ): a third-party `Backend` overriding
///   `admit` with a real dialect-aware estimator cannot declare what it
///   achieved without naming this type, the same way it cannot honour
///   `admit` without naming `check_admission` above.
/// - `async_trait` — `Backend` is `#[async_trait]`-transformed, the same
///   reason `pub mod plugin` re-exports the macro for its own three
///   traits.
///
/// Deliberately NOT here:
///
/// - `CacheTtl` — reachable only through `CacheMode::ExplicitBreakpoints`,
///   a single variant of one `Capabilities` field, and no signature or
///   field this module curates requires that specific variant.
///   `backend_parity.rs`'s own stub declares `CacheMode::ImplicitPrefix`
///   instead (an equally real dialect shape — OpenAI-compatible servers
///   use it), so nothing here needs `CacheTtl`. A real Anthropic-shaped
///   adapter that does need it lives inside this repository already
///   (`conway-plugin-backends`' `AnthropicBackend`), the same asymmetry this
///   item's own preceding investigation flagged as a decision for a later chain
///   item, not this one.
/// - `Role`, `Provenance`, `ToolCategory`, `PermissionClass`, `ToolName` —
///   needed one level further down, to construct a `PromptSegment`/
///   `ToolSpec` literal, but already reachable without duplicating them a
///   third time: `Provenance`/`ToolCategory`/`ToolName` at this crate's
///   root, `Role`/`PermissionClass` through `pub mod plugin` above.
///   `backend_parity.rs` imports them from wherever they already live, the
///   same way a real third-party `Backend` crate would.
pub mod backend {
    pub use async_trait::async_trait;
    pub use conway_core::capabilities::{
        CacheMode, Capabilities, ProbeReport, ReliabilityTier, StructuredOutput, ToolCallSupport,
    };
    pub use conway_core::content::{
        ContentBlock, SamplingParams, StopReason, ToolCall, ToolSpec, Usage,
    };
    pub use conway_core::error::BackendError;
    pub use conway_core::ids::{BackendId, ModelId, PrefixKey};
    pub use conway_core::ports::{
        check_admission, Admission, Backend, BoxStream, GenerateRequest, GenerateResponse,
        StreamChunk, TokenCountFidelity,
    };
    pub use conway_core::segment::PromptSegment;
}

pub use conway_core::provenance::{ContextReport, Provenance};
pub use conway_core::routing::{AttemptFailure, BreakerKind, BreakerState, RoutingReason};

/// Canonical JSON bytes — recursively sort object keys, serialize without
/// insignificant whitespace. Re-exported from `conway-core` (DESIGN-context-path
/// §2.3) so a crate depending only on this facade (e.g. a first-party plugin
/// that holds the "facade-only" discipline, like `conway-plugin-stepguard`)
/// can hash through the one canonicalizer the workspace uses, rather than
/// carrying a third copy. Pure, no policy.
pub use conway_core::canon::canonical_json_bytes;

/// The first-class context path vocabulary + refusing constructors
/// (DESIGN-context-path §2.1–§2.9, §4.1–§4.2): pure value types, the
/// model-free `SelectionKey`, `ValidatedPath` and its `derive`/
/// `derive_reordered` constructors, and `Derivation`. Re-exported so a
/// facade-only crate can name `RecordRef`/`PathNode`/`PathSelection`/
/// `SelectionKey`/`PathOp`/`CostEstimate`/`PathError`/`ValidatedPath`/
/// `Derivation` without depending on `conway-core` directly.
///
/// **`PathStore` is deliberately NOT among these re-exports, and not in
/// `pub mod plugin` either (board item `01M0EMCK55628YJXGBQY8YGXHE`).**
/// Every other port trait this facade curates is either re-exported for a
/// third party to *implement* or reached indirectly through a ctx field
/// whose concrete `Arc<dyn Trait>` needs no import to call (`CurateCtx`'s
/// `store: Arc<dyn SessionStore>` is the latter). `PathStore` is neither:
/// `CurateCtx` hands a curator `store` and `resolver`, never a path store,
/// and `01M0EMAC4CCDQ8QJYM21RXPKRY`'s real curator (`conway-plugin-trim`)
/// confirmed that the §11.5 read surface those two fields provide is
/// sufficient — it names ops and lets the engine derive; it never stores or
/// fetches a `PathSelection` directly. See
/// [`conway_core::ports::PathStore`]'s own doc for why: a second
/// implementation would put the write-once, content-addressed guarantee the
/// retention index depends on in a third party's hands rather than the
/// engine's, for a capability nothing outside the engine's own context
/// assembly currently exercises (`resolve_default_path` in `conway-runtime`
/// is `.get`'s only production caller; nothing calls `.put` outside
/// `FsPathStore`'s own construction). The tolerant constructor
/// (`default_path`) and head resolution land in later sub-units and stay
/// engine-internal for the same reason.
pub use conway_core::path::{
    CostEstimate, Derivation, DivergenceKind, HarnessDrop, NodeProvenance, NodeStamp, OpLabel,
    Orphan, PathError, PathNode, PathOp, PathSelection, RecordRef, SelectionKey, Selector,
    ValidatedPath,
};

/// The cross-session discovery vocabulary (board item
/// `01M0PS8J3AK7Z7253Z3E3RD3GY`): pure value types a `Tool` builds a
/// [`conway_core::ports::SessionSearchQuery`] from and reads a
/// [`conway_core::ports::SessionSearchResult`] back into -- re-exported at
/// the root for the SAME reason the context-path vocabulary immediately
/// above is: a facade-only crate constructs and reads these without
/// depending on `conway-core` directly. `SessionDiscoveryHost`/
/// `SessionDiscoveryHandle` are deliberately NOT re-exported (mirroring
/// `ContextPathHost`/`ContextPathHandle`'s own precedent, `conway_core::
/// ports::ContextPathHandle`'s doc): `ToolCtx::session_discovery`'s methods
/// are reachable by dispatch alone, never by naming the handle/host types.
pub use conway_core::ports::{
    MatchedRecord, SessionMatch, SessionSearchQuery, SessionSearchResult, SessionSearchScope,
};

// Amended by:
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

// Board item 01M0ASX466G3PW3SJJS3KGNS55: `ExplainEntry::token_fidelity` is a
// public field of a type re-exported above, so this facade must let a
// caller name its type too -- the identical reasoning `RoutingConfig`/
// `HeadroomPolicy`/`ModelOverrides` below already establish for
// `RouterBuildContext`/`BackendBuildContext`'s own field types. Already
// re-exported inside `pub mod backend` (for a third-party `Backend`
// implementing `token_fidelity()`); this is a second, root-level re-export
// for the unrelated reason that `conway routes explain`'s own caller needs
// to name it without depending on `conway-core` directly, the same
// dual-export shape `Provenance` already has (root, and `pub mod plugin`).
pub use conway_core::ports::TokenCountFidelity;

// `RouterFactory` joins the
// extension surface above, so the field types of what its `build` receives
// must be nameable by a crate depending only on `conway`. Without these two,
// a third-party factory could read `ctx.routing.roles` and call
// `ctx.headroom.resolve(..)` but could not write a helper taking
// `&RoutingConfig` or `&HeadroomPolicy` as a parameter -- it would have to
// depend on `conway-core` directly, which is exactly the privileged-interface
// asymmetry the no-privileged-built-ins rule forbids. A port whose context
// cannot be spelled through the
// public facade is only half-installed.
pub use conway_core::capabilities::HeadroomPolicy;
pub use conway_core::routing::RoutingConfig;

// By the same reasoning one line
// up: `BackendFactory` joins the extension surface too, and
// `BackendBuildContext::models` names `ModelOverrides` in its own field
// type -- without this re-export, a third-party factory could read
// `ctx.models.get("some-model")` but could not write a helper taking
// `&ModelOverrides` as a parameter, the identical gap `RoutingConfig`/
// `HeadroomPolicy` closed for `RouterBuildContext` above.
pub use conway_core::routing::ModelOverrides;

/// Test doubles for every `conway-core` port trait (`FakeBackend`,
/// `FakeStore`, `FakeGate`, `FakeRouter`, `FakeHealth`, `FakeSubagentHost`,
/// `CollectingEventSink`, ...) -- forwarded from `conway-testkit`, a crate
/// of its own, behind this facade's own `testkit` feature (off by default:
/// a testkit in every production build is dead weight).
///
/// Board item 01KZVYWNA24EYMPVW3NPGBW51M: this is the fix for the gap
/// `pub mod plugin`'s own doc used to describe -- `conway-core`'s doubles
/// used to be feature-gated INSIDE that crate, and this facade enabled
/// that gate only under `[dev-dependencies]`, so a crate depending on
/// `conway` could never reach `FakeSubagentHost`/`CollectingEventSink` at
/// all. Enabling `testkit` now reaches the identical doubles this
/// workspace's own tests always could.
#[cfg(feature = "testkit")]
pub mod testkit {
    pub use conway_testkit::*;
}

/// Facade-level test scaffolding -- the one place a `Conway` is assembled
/// for a test -- behind the non-default `test-support` feature.
///
/// `conway::testkit` (above) forwards the port doubles; this forwards the
/// *wiring* of those doubles into a built `Conway`, which no core-only
/// crate can offer because none of them can see `ConwayBuilder`. Board
/// item 01M0TV8MSFRHHQ5BNZV3NHZCEW: a local `build_conway` helper was
/// hand-rolled in 46 test files across seven crates for exactly that
/// reason.
///
/// Off by default for the same reason `testkit` is, and it implies
/// `testkit`: it is built out of those doubles.
#[cfg(feature = "test-support")]
pub mod test_support;
