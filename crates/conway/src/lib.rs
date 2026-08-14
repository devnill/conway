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
//! the cargo feature flags below, the crate-level [`ConwayError`]/[`Result`],
//! and the curated re-export list from `conway-core`. Every other module
//! named in the facade module's implementation notes (`config`, `agents`,
//! `gates`, `presets`, `builder`, `conway`, `session_handle`,
//! `event_stream`) is added by its own owning work item, each of which
//! appends its own `mod` declaration here.
//!
//! This crate's public surface is defined in terms of `conway-core`'s domain
//! types and port traits, plus the facade's own `ConwayBuilder`/`Conway`/
//! `SessionHandle` wrappers added by later work items.
//!
//! **FORWARD DECLARATION — one `conway-runtime` type IS re-exported here.**
//! This paragraph used to state flatly that none was. `pub use
//! conway_runtime::permission::GrantScope;` appears about forty lines below,
//! and it is load-bearing: `conway-cli` names it at eight sites to label and
//! revoke a structured-allow rule in `/settings`, and cannot reach it any
//! other way (`no_forbidden_deps` in `crates/conway-cli/tests/cli_surface.
//! rs` forbids that crate depending on `conway-runtime`). Exactly one such
//! re-export exists, pinned by the `t6_facade_has_exactly_the_one_known_
//! runtime_reexport` guard in `crates/conway/tests/architecture_invariants.
//! rs`, so a second fails CI.
//!
//! (Stage 2b) closes it, together with
//!, which owns the consumer half — deleting the
//! re-export alone breaks the build, so a replacement type carrying the
//! `AgentId` has to land in the same change. **Those items must delete this label.**

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
pub use conway::{
    Conway, HookRuleView, PermissionLoadReport, RevokeOutcome, TrustPermissionReport,
};
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
///
/// **FORWARD DECLARATION — this is the one re-export the crate root doc's
/// label names, and it is the whole of that contradiction.** It is not
/// vestigial: eight `conway-cli` sites name it through the `conway::` path
/// (`tui/input.rs`, `tui/state.rs`, `tui/view/settings.rs`), and
/// `conway::PermissionScope` is not a substitute -- it is a bare
/// `Session`/`Agent`/`AgentSubtree` enum, while this type is `Session |
/// Agent(AgentId) | Subtree(AgentId)`, and the review row exists to tell an
/// operator WHICH agent a per-agent grant covers.
/// (Stage 2b) and
/// (its consumer half) replace it with a type this facade owns and **must
/// delete this label**; records the
/// evidence and the condition that would reopen it.
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
    Backend, BackendBuildContext, BackendFactory, ContextHook, HealthRegistry, PermissionGate,
    Plugin, RenderKind, Router, RouterBuildContext, RouterBundle, RouterFactory, SessionStore,
    Tool,
};

/// The shared error type
/// [`RouterFactory::build`] and [`BackendFactory::build`] both return.
/// Re-exported under this name, not `ConwayError` (already this crate's own
/// root error type, [`crate::error::ConwayError`], returned by every OTHER
/// fallible public API here) -- the two are deliberately distinct types with
/// the same short name at different crate depths (`conway_core::error::
/// ConwayError` vs. `conway::error::ConwayError`), so re-exporting the
/// former under the latter's own name at this SAME root would shadow one
/// with the other. `CoreConwayError` names which one a factory
/// implementation must actually return -- `RouterFactory::build`'s own
/// signature already committed to this type (`crates/conway-core/src/ports/
/// routing.rs`) before this item existed; this re-export is what finally
/// makes that signature spellable from a crate depending only on `conway`,
/// closing a latent gap `RouterFactory` alone never had a compile-guarded
/// test to catch (`crates/conway/tests/backend_parity.rs`'s extension,
/// this item's own, is the first such test either factory port has had).
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
/// - `SubagentHost`/`CwdHandle`/`EventSinkHandle`/`SubagentHandle`/
///   `EventSink` (the host-capability traits and handle types themselves)
///   — see "Deliberately NOT here" below; unchanged by this resolution.
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
/// - `CwdHandle`, `EventSinkHandle`, `SubagentHandle`, `SubagentHost`,
///   `EventSink` — they appear only as `ToolCtx` *fields* an implementor
///   reads (method calls on `ctx.chdir`/`ctx.events`/`ctx.subagents` never
///   name the type — a tool calls `ctx.subagents.start(spec)` by method
///   dispatch, exactly like the pre-existing `ctx.chdir.set(..)` precedent).
///   `SubagentHost`/`EventSink` have no builder injection point at all — no
///   `pub fn with_*` in `crates/conway/src/builder.rs` accepts either — which
///   is the actual, IN-PROCESS reason they stay absent here. (§13.5 is a
///   non-goals list for the OUT-OF-PROCESS subprocess transport and was
///   never authority for this in-process question — the extension design's
///   own 2026-08-09 dated status note said so.)
///   Constructing a `ToolCtx` by hand (what those names are needed for) is
///   test-fixture work, not the authoring surface. NOTE, corrected
///   2026-08-10: this paragraph
///   used to say that work was "served by `conway-core`'s `fakes`
///   feature". That is true INSIDE this workspace and false for a third
///   party — `crates/conway/Cargo.toml` takes `conway-core` with
///   `features = ["fakes"]` only under `[dev-dependencies]`, so a crate
///   depending on `conway` cannot reach `FakeSubagentHost` or
///   `CollectingEventSink` at all. `ToolCtx` therefore carries the same
///   construction tax `ContextHookCtx` carried until
///   `ArtifactWriteHandle::noop` closed it, and no equivalent exists yet
///   for `SubagentHandle`/`EventSinkHandle`. Re-checked for `SubagentHandle`
///   specifically when it landed: no concrete call site names it either.
/// - The `SessionStore`/`HealthRegistry` implementation surfaces.
///   `SessionStore` because `SeqRange`/`StoreError` — needed to spell
///   `SessionStore::append`'s own signature — are not re-exported anywhere;
///   `HealthRegistry` because, like `SubagentHost`/`EventSink` above, no
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
    pub use conway_core::agent::Fact;
    pub use conway_core::content::{
        Artifact, ArtifactKind, ContentBlock, PermissionClass, Role, ToolCall, ToolCategory,
        ToolSpec, TruncationPolicy,
    };
    pub use conway_core::error::{
        ArtifactWriteError, CwdError, HookFailure, SubagentError, ToolError,
    };
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
    pub use conway_core::ids::ToolName;
    /// [`Plugin::events`]'s own
    /// return type -- a plugin author constructs one of these per custom
    /// event it declares, the SAME `name`+`summary` shape [`CommandSpec`]
    /// establishes for commands, plus `carries_tool_name` (whether a
    /// `[hooks].rules[]` entry may pair this event with `match`).
    pub use conway_core::ports::EventDecl;
    pub use conway_core::ports::{
        ArtifactWriteHandle, ArtifactWriter, CancellationToken, Command, CommandCtx,
        CommandOutcome, CommandSpec, ContextHook, ContextHookCtx, ContextPayload, HookRunner,
        ObservedCall, ObserverAnswer, ObserverCtx, ObserverNote, OverflowInfo, PathArgs, Plugin,
        PluginConfig, PluginEventHandle, PluginManifest, RegisteredObserver, RenderKind, Tool,
        ToolCtx, ToolObserver, ToolOutput,
    };
    pub use conway_core::provenance::Provenance;
    pub use conway_core::segment::PromptSegment;
}

/// The `Backend` authoring surface:
/// every type a crate depending only on `conway` needs to write `impl
/// conway::Backend for MyBackend`. `Backend`'s trait itself has been
/// re-exported at this crate's root since an earlier item, but nothing its five
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
        StreamChunk,
    };
    pub use conway_core::segment::PromptSegment;
}

pub use conway_core::provenance::{ContextReport, Provenance};
pub use conway_core::routing::{AttemptFailure, BreakerKind, BreakerState, RoutingReason};

// an earlier item (CARRIED), amended by:
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

//, same reasoning one line
// up: `BackendFactory` joins the extension surface too, and
// `BackendBuildContext::models` names `ModelOverrides` in its own field
// type -- without this re-export, a third-party factory could read
// `ctx.models.get("some-model")` but could not write a helper taking
// `&ModelOverrides` as a parameter, the identical gap `RoutingConfig`/
// `HeadroomPolicy` closed for `RouterBuildContext` above.
pub use conway_core::routing::ModelOverrides;
