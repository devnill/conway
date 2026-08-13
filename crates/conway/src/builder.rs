//! `ConwayBuilder`: assembles a validated [`crate::config::ConwayConfig`]
//! plus optional injected ports into a live [`crate::conway::Conway`]
//! (WI-100). This is the wiring layer — it contains no agent logic.
//!
//! ## Reconciliations against the binding spec (disclosed, not worked around)
//!
//! - **`build(self) -> Result<Conway>` is synchronous** (the WI-100 golden
//!   end-to-end criterion chains `.build()?.new_session(..).await?` — `?`
//!   with no `.await` on `build()`), but the default session store
//!   (`conway_session::JsonlSessionStore::open`) is an `async fn` that
//!   performs real I/O. `build()` bridges this by running that one `async`
//!   call to completion on a fresh OS thread with its own throwaway
//!   current-thread `tokio` runtime ([`block_on`]) rather than via
//!   `Handle::current().block_on(..)`, which panics when `build()` is (as
//!   it commonly will be) invoked from inside an existing `tokio` task.
//!   This still briefly blocks whichever thread calls `build()` when it
//!   needs to construct a real store; embedders that call `build()` from an
//!   async context and care about that should do so via `spawn_blocking`.
//!   This is a load-bearing, disclosed deviation forced by the sync/async
//!   mismatch between the golden criterion and the lower crates' committed
//!   `async` signatures — not an oversight. (Board item
//!   01KZHF270T3W8GZ7NM6DSNQ4MM: the optional startup capability probe used
//!   to be the OTHER caller of this same bridge, directly in this module;
//!   `conway_plugin_backends::OpenAiCompatBackendFactory::
//!   probe_capabilities` now runs its own probe behind its own,
//!   independently-maintained bridge — see that method's own doc — so this
//!   module's `block_on` is used by [`build_default_store`] alone today.)
//! - **No `with_prompt_handler` method exists** (the criteria list
//!   `ConwayBuilder`'s methods "exactly", and that list has no such
//!   method), so `gates::from_config` is always called with `prompt_handler:
//!   None`. Since `permissions.mode` defaults to `"prompt"`, an embedder
//!   using an unmodified default config and no `with_permission_gate`
//!   override will get `ConwayError::Config` from `build()` — flagged as a
//!   gap in this item's own public surface (the CLI or a future item should
//!   likely add a way to supply a prompt handler) rather than silently
//!   adding an undocumented method.
//! - **Backend construction, dialect/profile resolution, and startup
//!   capability probing are `conway_plugin_backends`'s concern, not this
//!   module's** (board item 01KZHF270T3W8GZ7NM6DSNQ4MM): `resolve_backend_
//!   factory` resolves a `[backends.<id>]` entry's `kind` against every
//!   registered [`BackendFactory`] ONLY (no compiled-in fallback), and
//!   [`build_backend_context`] resolves the [`BackendBuildContext`] a
//!   matching factory's own `build`/`probe_capabilities` receives -- what
//!   `AnthropicConfig`/`OpenAiCompatConfig` construction, `Profile`
//!   resolution (the three historically kebab-case dialect strings
//!   translated to their snake_case built-in profile ids), and
//!   `CapabilityProbe`'s own HTTP round trip do with those resolved fields
//!   is entirely `conway_plugin_backends::factory`'s own implementation now
//!   -- this facade compiles neither dialect in, and its own
//!   `resolve_backend_factory` names neither `"anthropic"` nor
//!   `"openai-compat"`: `kind` resolves only against whichever
//!   `BackendFactory`s a caller registered.
//! - **The backend map is keyed by each constructed backend's own
//!   `Backend::id()`.** `config::merge::validate` checks chain refs
//!   (`<backend_id>/<model>`) against that same key namespace, so the two
//!   agree by construction -- this is what lets an Anthropic-compatible
//!   third-party endpoint be named for what it is (`kimi`) and coexist with
//!   a real `anthropic` backend, rather than every such config having to
//!   squat the key `"anthropic"`.
//! - **`config.limits.max_parallel_tools` has no wiring point**: neither
//!   `conway_runtime::runtime::RootSpec` nor `AgentSpec` (which
//!   `Runtime::start_root` builds internally, hardcoding
//!   `DEFAULT_MAX_PARALLEL_TOOLS`) exposes a field this builder or
//!   `Conway::new_session` could set it through. Flagged as a gap for
//!   `MODULE:conway-runtime`, not solved here.
//! - **The router's `CapabilityIndex` is built from `Backend::capabilities()`,
//!   not from a second `models.json` → `Capabilities` conversion** (WI-123):
//!   [`models_overrides_for`] projects `models.json`'s `max_context_tokens`
//!   and `reliability_tier` into each backend's `ModelOverrides` table
//!   *before* backends are constructed, and step 5 below then calls
//!   `CapabilityIndex::from_backends` on those already-constructed backends
//!   — so the router reads exactly what `Backend::capabilities()` (and
//!   therefore `conway_runtime::attempt::AttemptEngine`'s T-1 gate) would
//!   return for the same pair, never an independently-recomputed value. One
//!   consequence: `parallel_tool_calls`/`structured_output` for a
//!   file-derived model now resolve to the dialect's default rather than
//!   always `false`/`None` (the prior `to_capabilities` conversion's
//!   conservative fallback, since the facade's `ModelMetadataEntry` schema
//!   has no field for either). `models.json`'s `tool_calling` and
//!   `reasoning` fields, however, still reach neither the router nor
//!   `Backend::capabilities()`: `ModelOverrides` (owned by `conway-core`)
//!   has no field for them, and extending it is outside this item's file
//!   scope — see `conway_plugin_backends::capabilities`'s module doc and
//!   this item's scope-boundary note.
//! - **Startup capability probing is a per-kind opt-in
//!   ([`BackendFactory::probe_capabilities`]'s own doc), and only
//!   `conway_plugin_backends::OpenAiCompatBackendFactory` implements it** —
//!   the Anthropic wire format has no server-side model-listing endpoint
//!   this facade's plugin speaks (`Backend::probe()`'s own `ProbeReport`
//!   carries no `max_context_tokens`/capability data to overlay either).
//!   `probe_on_startup` therefore only ever affects `"openai-compat"`-kind
//!   backends; this is disclosed, not silently no-op'd, and is unchanged
//!   from before this item's relocation. **The RESTRICT eligibility
//!   filter — a probed pair only overlays the router's `CapabilityIndex`
//!   when its key already appears in that entry's own `BackendBuildContext
//!   ::models` (i.e. `models.json` already declared it for this backend) —
//!   is enforced HERE, in [`ConwayBuilder::build`]'s own step 5, identically
//!   for every kind's discovered map**, not delegated to each
//!   `BackendFactory::probe_capabilities` implementation to get right on its
//!   own (no opaque auto-selection in the core — a model becoming
//!   routable because a server mentioned it, with no operator declaration
//!   behind it, is not a "route" a user could have predicted from
//!   `models.json` alone). A pair a factory's probe observed but
//!   `models.json` never listed is silently dropped, not inserted, and not
//!   surfaced as a hard error, but it IS logged at `debug` so an operator
//!   who enabled `probe_on_startup` and expected discovery to pick up an
//!   undeclared model has a signal for why it never became routable.
//!   `ctx.models` (`models_overrides_for(id, metadata)`, the same map the
//!   backend itself was built with, not an independently-derived one) wins
//!   outright over a probed value in both directions for every model it
//!   lists — a probed window can neither mask a smaller operator-declared
//!   one nor be masked by a larger one the operator explicitly widened past
//!   what the probe observed — which makes the overlay a verified no-op for
//!   every `models.json`-listed pair: equal inputs (`ctx.models`) reach
//!   `build_capabilities` on both the probe side and the
//!   `Backend::capabilities()` side, so equal outputs mean the router's
//!   index ends up exactly what `Backend::capabilities()` — and therefore
//!   the T-1 gate — would return for the same pair.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use conway_core::capabilities::{HeadroomPolicy, ReliabilityTier};
use conway_core::ids::{BackendId, ModelRef};
use conway_core::ports::CapabilityIndex;
use conway_core::ports::{
    Backend, BackendBuildContext, BackendFactory, ContextHook, HealthRegistry, HookRunner,
    PermissionGate, Plugin, Router, RouterBuildContext, RouterBundle, RouterFactory,
    RoutingExplainer, SessionStore,
};
use conway_core::routing::{AlwaysClosedHealthRegistry, MinimalRouter, ModelOverrides};
use conway_runtime::events::EventBus;
use conway_runtime::permission::PreToolUseHookSpec;
use conway_runtime::runtime::{Runtime, RuntimeDeps};

use crate::agents;
use crate::config::schema::{BackendEntry, ConwayConfig};
use crate::config::{self, CliOverrides, ConfigWarning, LoadOptions};
use crate::conway::Conway;
use crate::error::{ConwayError, Result};
use crate::gates;
#[cfg(feature = "builtin-tools")]
use crate::presets;

/// The capacity of the runtime's broadcast event bus. ASSUMPTION: no
/// criterion pins this value and `conway-runtime` exports no default
/// constant; picked generously (matching the order of magnitude
/// `conway-runtime`'s own tests use for a long-lived bus) rather than
/// inventing a config surface this item has no mandate to add.
const EVENT_BUS_CAPACITY: usize = 1024;

/// Which built-in plugins [`ConwayBuilder::build`] auto-registers, filtered
/// by each candidate's own `PluginManifest::id` (board item: bash ships on
/// by default and cannot be declined).
///
/// **This is a generic, id-keyed predicate over a *bundle* of candidate
/// plugins -- it is not bash-specific and carries no built-in-vs-third-party
/// distinction of its own** ("a third-party plugin and a built-in
/// must be selectable the same way"). `build()` applies it to exactly one
/// bundle today -- `presets::builtin_plugins()`'s four candidates -- but
/// nothing about the type restricts it to that bundle: an embedder shipping
/// their own *set* of related third-party plugins (as opposed to one ad hoc
/// `with_plugin` call) can filter that set through this same enum before
/// handing survivors to `with_plugin`, one at a time, exactly as `build()`
/// does internally for built-ins.
///
/// **Plugins injected via [`ConwayBuilder::with_plugin`] are never filtered
/// by this type.** Calling `with_plugin` IS already the explicit,
/// per-plugin declaration the one extension mechanism requires of a third
/// party -- nothing about
/// that call is privileged or automatic. What this item corrects is the
/// other direction: conway's own built-ins were the ONE bundle that
/// installed itself with no equivalent declaration, `bash` included. This
/// type extends the SAME "explicit declaration" requirement to built-ins
/// (letting three of the four opt back in by default, purely as a matter of
/// today's chosen default -- see [`crate::config::schema::ToolsConfig`]'s
/// doc), not the reverse: an already-explicit `with_plugin` call gains no
/// new hoop to jump through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginSelection {
    /// Every candidate in the bundle.
    All,
    /// No candidate in the bundle.
    None,
    /// Exactly the named manifest ids.
    Only(Vec<String>),
    /// Every candidate EXCEPT the named manifest ids.
    AllExcept(Vec<String>),
}

impl PluginSelection {
    /// With the `builtin-tools` feature disabled there is no candidate
    /// bundle to filter (`presets::builtin_plugins()` does not exist), so
    /// this is never called -- `allow(dead_code)` rather than dropping the
    /// method under that same `cfg`, which would make it unavailable to an
    /// embedder's own third-party-bundle use of this type (this type's own
    /// doc) purely as a side effect of THIS crate's built-ins being off.
    #[cfg_attr(not(feature = "builtin-tools"), allow(dead_code))]
    fn allows(&self, id: &str) -> bool {
        match self {
            Self::All => true,
            Self::None => false,
            Self::Only(ids) => ids.iter().any(|i| i == id),
            Self::AllExcept(ids) => !ids.iter().any(|i| i == id),
        }
    }
}

/// Assembles a [`Conway`] from a [`ConwayConfig`] plus optional injected
/// ports. See the module doc for disclosed reconciliations.
pub struct ConwayBuilder {
    config: ConwayConfig,
    cli_overrides: CliOverrides,
    backends: Vec<Arc<dyn Backend>>,
    plugins: Vec<Arc<dyn Plugin>>,
    gate: Option<Arc<dyn PermissionGate>>,
    store: Option<Arc<dyn SessionStore>>,
    router: Option<Arc<dyn Router>>,
    /// Board item 01KZFC2MD1FVNA674YJ9A19T8E. `None` (the default) means
    /// `build()`'s router step falls through to compiling its own
    /// `DeclarativeRouter`, exactly as it did before this field existed --
    /// see [`Self::with_router_factory`]'s own doc for the full precedence.
    router_factory: Option<Arc<dyn RouterFactory>>,
    /// Board item 01KZHF0RBKJZZC68F7GPFB347Q. Empty (the default) means
    /// `build()`'s backend step is byte-for-byte what it was before this
    /// field existed -- config-derived backends merged with `backends`
    /// (above), nothing more -- see [`Self::with_backend_factory`]'s own doc
    /// for the full precedence and duplicate-kind rules.
    backend_factories: Vec<Arc<dyn BackendFactory>>,
    /// Board item 01KZHF2W8Y1KBM7PJH7R4QQJA0. Empty (the default) means
    /// nothing changes from before this field existed -- see
    /// [`Self::with_declined_backend_kinds`]'s own doc for what a non-empty
    /// value does (purely diagnostic; it never removes, blocks, or replaces
    /// a registered [`BackendFactory`]).
    declined_backend_kinds: Vec<String>,
    /// WI-126. `None` (the default) means `build()` never calls
    /// `Runtime::set_context_hook` at all, leaving every agent's
    /// `context_hook` at the `Runtime`-constructed default of `None` --
    /// i.e. today's behavior, unchanged.
    context_hook: Option<Arc<dyn ContextHook>>,
    /// Board item 01KZS00JP5QNBJSSHNFP9C47GM. `None` (the default) means
    /// `build()` never calls `Runtime::set_hook_runner` at all, leaving
    /// `PermissionBroker::decide`'s `pre_tool_use` hook-check step at the
    /// `PermissionBroker`-constructed default of `None` -- a byte-for-byte
    /// no-op, i.e. today's behavior, unchanged, REGARDLESS of whatever
    /// `[hooks].rules[]` a loaded config declares (see
    /// [`Self::with_hook_runner`]'s own doc).
    hook_runner: Option<Arc<dyn HookRunner>>,
    /// Board item (bash ships on by default and cannot be declined).
    /// `None` (the default) means `build()` derives the effective
    /// [`PluginSelection`] from `config.tools.builtin_plugins` instead --
    /// see [`Self::with_builtin_plugins`]'s doc.
    builtin_selection: Option<PluginSelection>,
    warnings: Vec<ConfigWarning>,
    /// Board item 01KYTMH9JX21CGSE2Y6E2KP8SJ: an operator-set confinement
    /// root, applied to every root agent this `Conway` starts (see
    /// [`Self::with_root`]'s own doc). Deliberately NOT a `ConwayConfig`
    /// field: `ConwayConfig` has no `#[derive(Default)]` (`default_role` has
    /// no sensible built-in value), so every one of its existing struct-
    /// literal call sites across the workspace would have to name a new
    /// field the moment one was added -- a blast radius with no relationship
    /// to this item's own scope. `Conway`/`ConwayBuilder` are constructed
    /// exclusively through this builder's own methods (never struct-
    /// literaled by a caller), so a field here costs nothing outside this
    /// file and `conway.rs`.
    root: Option<PathBuf>,
}

impl ConwayBuilder {
    /// Loads config from an explicit path (bypassing discovery), still
    /// layered under XDG/env/CLI precedence.
    pub fn from_config(path: impl AsRef<Path>) -> Result<Self> {
        let options = LoadOptions {
            explicit_path: Some(path.as_ref().to_path_buf()),
            ..LoadOptions::default()
        };
        let outcome = config::load(options)?;
        Ok(Self::from_parts(outcome.config).with_warnings(outcome.warnings))
    }

    /// Loads config via the standard five-source discovery/precedence chain
    /// (`config::load` with `LoadOptions::default()`, whose own `cwd`
    /// defaults to `std::env::current_dir()`).
    pub fn discover() -> Result<Self> {
        let outcome = config::load(LoadOptions::default())?;
        Ok(Self::from_parts(outcome.config).with_warnings(outcome.warnings))
    }

    /// Builds directly from an already-validated config, bypassing `load`
    /// entirely (no discovery, no env, no warnings).
    pub fn from_parts(config: ConwayConfig) -> Self {
        Self {
            config,
            cli_overrides: CliOverrides::default(),
            backends: Vec::new(),
            plugins: Vec::new(),
            gate: None,
            store: None,
            router: None,
            router_factory: None,
            backend_factories: Vec::new(),
            declined_backend_kinds: Vec::new(),
            context_hook: None,
            hook_runner: None,
            builtin_selection: None,
            warnings: Vec::new(),
            root: None,
        }
    }

    fn with_warnings(mut self, warnings: Vec<ConfigWarning>) -> Self {
        self.warnings = warnings;
        self
    }

    /// Read-only access to the config this builder currently holds --
    /// board item 01KZDC3JQ7W4DY1MG6MBCVB2DV's answer to "how does a caller
    /// decide which first-party (or third-party) plugin to `with_plugin`
    /// before `build()`?" `config().plugins.install`
    /// ([`crate::config::schema::PluginsConfig`]) is the intended read: a
    /// binary that links a given plugin crate (this crate itself never
    /// does -- see that field's own doc) checks whether its manifest id
    /// appears in this list and, if so, attaches it -- exactly what
    /// `crates/conway-cli/src/first_party_plugins.rs` does before calling
    /// `build()`.
    ///
    /// Reflects any `with_cli_overrides` call made so far but NOT
    /// `config::merge::apply_cli` (`build()`'s own step 1) -- CLI overrides
    /// are only fully applied and re-validated inside `build()` itself, so
    /// a caller reading `config()` beforehand sees the loaded/`from_parts`
    /// config, not the final post-override one.
    pub fn config(&self) -> &ConwayConfig {
        &self.config
    }

    /// Injects a backend. Takes precedence over any `[backends.<id>]`
    /// entry's factory-built backend with the same `Backend::id()` -- see
    /// [`Self::with_backend_factory`]'s own doc for the full precedence
    /// order between both sources.
    pub fn with_backend(mut self, backend: Arc<dyn Backend>) -> Self {
        self.backends.push(backend);
        self
    }

    /// Registers a [`BackendFactory`] (board item
    /// 01KZHF0RBKJZZC68F7GPFB347Q): a provider-adapter KIND, named up front,
    /// whose actual construction is deferred to `build()`'s own backend
    /// step -- the [`Self::with_router_factory`] pattern, one layer over.
    /// Read that method's own doc first; this restates it for a SET rather
    /// than a singleton, since (unlike routing) a build has many backends,
    /// not one.
    ///
    /// **Precedence, exact, extending [`Self::with_backend`]'s own
    /// "takes precedence" doc to name both sources by construction order:**
    /// `build()`'s backend step constructs one backend per `[backends.<id>]`
    /// entry FIRST (resolving `entry.kind` against the registered
    /// factories, below -- there is no separate "config-derived" backend
    /// construction distinct from this: the temporary two-adapter fallback
    /// that once made one is GONE, see the open-name paragraph below), then
    /// merges every `with_backend`-injected backend LAST -- each step keyed
    /// into the same `HashMap<BackendId, Arc<dyn Backend>>` by
    /// `Backend::id()`, so a later step's entry OVERWRITES an earlier
    /// step's entry sharing the same id. Concretely: an injected backend
    /// wins over a `[backends.<id>]`-entry-built backend sharing its id --
    /// and the loser is genuinely discarded, never merely shadowed (its
    /// constructor still ran, so a factory with side effects in `build`
    /// still sees them, but the `Arc<dyn Backend>` it returned is dropped,
    /// never wired into the runtime).
    ///
    /// **Two registered factories reporting the same [`BackendFactory::id`]
    /// (a duplicate KIND, not a duplicate instance) is a hard `build()`
    /// error** naming both -- mirroring the duplicate-manifest-id check
    /// `build()`'s own plugin step already makes (below) rather than
    /// inventing a second convention. Checked BEFORE any registered
    /// factory's `build` is invoked (a dedicated first pass over every
    /// registered id), so a duplicate never leaves one factory's `build`
    /// side effects to have run while the whole call still fails.
    ///
    /// **A factory whose `build` returns `Err` fails the whole `build()`
    /// call as [`crate::ConwayError::Build`], naming this factory's own
    /// [`BackendFactory::id`] and the underlying message** -- never silently
    /// swallowed, never a fallback that drops the kind and proceeds with
    /// whatever other backends exist (a registered factory that
    /// silently produced nothing would be a configuration key an operator
    /// set and got nothing for).
    ///
    /// **Registering a factory whose kind no `[backends.<id>]` entry ever
    /// names is fine, not an error or a warning** -- kind resolution is
    /// PER ENTRY (see the open-name paragraph below): `build()` invokes a
    /// registered factory's `build` only for a `[backends.<id>]` entry
    /// whose `kind` names it, never unconditionally for every registered
    /// factory. Nothing about registering a factory promises any
    /// particular `[backends.<id>]` entry will ever select it.
    ///
    /// **`[backends.<id>].kind` is now an open name** (board item
    /// 01KZHF1E85MS1VF4YH8CDNCP9Z, decision 01KZHRPZ010R37411R3W1XR5TF): for
    /// every `[backends.<id>]` entry, `build()`'s own `resolve_backend_
    /// factory` resolves `entry.kind` against every registered factory's own
    /// [`BackendFactory::id`] -- and against nothing else. The temporary
    /// fallback to two compiled-in adapters is GONE (board item
    /// 01KZHF270T3W8GZ7NM6DSNQ4MM): this facade no longer links either
    /// dialect, so an unregistered kind is an unknown-kind error, not a
    /// silent built-in. See `resolve_backend_factory`'s own doc for the
    /// exact resolution order and the error shape. A matching
    /// factory's `build` is invoked with a [`BackendBuildContext`] resolved
    /// from THAT entry: `id` is the entry's own `[backends.<id>]` JSON key,
    /// `base_url`/`dialect` are copied verbatim, and `api_key` is resolved
    /// the same centralized way every config-derived backend's key already
    /// is (literal `api_key` wins, else `api_key_env` read from the process
    /// environment, else `None`) -- see [`BackendBuildContext`]'s own doc
    /// for why resolving it once, here, is the point of that shape.
    /// **This makes a registered factory's `build` re-invocable once PER
    /// MATCHING entry**, not once per `ConwayBuilder::build()` call: two
    /// `[backends.<id>]` entries naming the same kind invoke that kind's
    /// factory twice, with two different contexts -- exactly the "one
    /// installed kind, many configured instances" cardinality this method's
    /// own doc above already promises (the "kimi" example), now actually
    /// reachable for a third-party kind and not just the two built-in ones.
    /// **Registering a factory whose kind no entry names is still fine, not
    /// an error** -- its `build` is simply never invoked, the literal case
    /// the paragraph above already covers. **Not called at all**, however,
    /// is no longer a benign default: with no factory registered there is
    /// nothing left for a `kind` to resolve against, so every
    /// `[backends.<id>]` entry fails with an unknown-kind error and the
    /// build reaches no model at all. The `conway` CLI avoids this by
    /// installing `conway-plugin-backends`' two factories from
    /// `[plugins].default_backends`; a library embedder linking this facade
    /// alone must register a factory itself.
    pub fn with_backend_factory(mut self, factory: Arc<dyn BackendFactory>) -> Self {
        self.backend_factories.push(factory);
        self
    }

    /// Declares which `BackendFactory` KIND ids this caller *knows about but
    /// chose not to attach* (board item 01KZHF2W8Y1KBM7PJH7R4QQJA0 -- the
    /// operator-facing decline mechanism for the two dialects
    /// `conway_plugin_backends` ships, `[plugins].default_backends`'s own
    /// doc, `crate::config::schema::PluginsConfig`). Replaces any prior call
    /// wholesale, the same "whole value, not additive" contract
    /// [`Self::with_cli_overrides`] already has.
    ///
    /// **Purely diagnostic -- changes no attach behavior at all.** Whether a
    /// kind is attached is, and remains, entirely a function of
    /// [`Self::with_backend_factory`] calls; this list is never consulted to
    /// skip, block, or filter one. Its only effect is on the MESSAGE
    /// [`build()`](Self::build) raises when a `[backends.<id>]` entry names a
    /// `kind` no registered factory claims: a kind in this list gets a
    /// **declined-kind** error naming it as such, distinguishable from the
    /// **unknown-kind** error every other unresolved `kind` still gets
    /// (nothing may claim to be reached that isn't
    /// -- an operator who deliberately declined a dialect deserves that
    /// diagnosis, not "conway has never heard of this," which is a different,
    /// worse-fitting claim about what happened). Not called at all (the
    /// default, empty list) means every unresolved `kind` is an unknown-kind
    /// error exactly as before this method existed -- unchanged.
    ///
    /// **`conway` (this binary's CLI) is the one caller wired today**:
    /// `crates/conway-cli/src/first_party_plugins.rs`'s `install` computes
    /// this as `conway_plugin_backends`'s published kind ids MINUS
    /// `wanted_ids` (`[plugins].install` unioned with
    /// `[plugins].default_backends`) -- i.e. every first-party dialect this
    /// binary links but this build did not select -- and calls this method
    /// with that list before `build()`. A library embedder linking
    /// `conway_plugin_backends` (or any other kind bundle) directly can call
    /// this the same way to get the same accurate diagnosis for its own
    /// declined kinds; nothing about this method is CLI-specific.
    pub fn with_declined_backend_kinds(mut self, kinds: Vec<String>) -> Self {
        self.declined_backend_kinds = kinds;
        self
    }

    /// Injects a plugin. `build()` errors if its manifest id collides with a
    /// built-in's (or another injected plugin's).
    pub fn with_plugin(mut self, plugin: Arc<dyn Plugin>) -> Self {
        self.plugins.push(plugin);
        self
    }

    /// Overrides `permissions.mode`-derived gate selection entirely.
    pub fn with_permission_gate(mut self, gate: Arc<dyn PermissionGate>) -> Self {
        self.gate = Some(gate);
        self
    }

    /// Registers a [`ContextHook`] (WI-126): invoked before every LLM
    /// request (mask/system-prompt/tool-announcement curation) and, on a
    /// T-1 `ContextTooLarge`, for a bounded overflow-reassembly retry. No
    /// call to this method (the default) means `build()` never touches
    /// `Runtime::set_context_hook` at all -- every agent's assembly,
    /// routing, and overflow handling stays exactly as it was before this
    /// item, with a hard `ContextTooLarge` on overflow.
    pub fn with_context_hook(mut self, hook: Arc<dyn ContextHook>) -> Self {
        self.context_hook = Some(hook);
        self
    }

    /// Registers a [`HookRunner`] (board item 01KZS00JP5QNBJSSHNFP9C47GM):
    /// the dispatcher `conway_runtime::permission::PermissionBroker::decide`
    /// invokes, at its deny tier, for every enabled `[hooks].rules[]` entry
    /// whose `event` is `"pre_tool_use"`. Mirrors [`Self::with_permission_
    /// gate`]/[`Self::with_context_hook`]'s own shape exactly (a
    /// third party supplies a runner on the identical surface a built-in
    /// uses) -- this facade never constructs a concrete `HookRunner`
    /// itself; `conway_tools::hook_runner::ProcessHookRunner` is the one
    /// this workspace ships, and a binary that wants it attaches it here
    /// (`conway`, this crate's own CLI, is the intended caller -- see that
    /// binary's own startup wiring).
    ///
    /// **No call to this method (the default) means `build()` never calls
    /// `Runtime::set_hook_runner` at all** -- `PermissionBroker::decide`'s
    /// hook-check step stays a byte-for-byte no-op, REGARDLESS of whatever
    /// `[hooks].rules[]` a loaded config declares: a `pre_tool_use` rule
    /// with no runner ever injected parses, validates, and is silently
    /// never consulted (see [`crate::config::schema::HooksConfig`]'s own
    /// doc for the full disclosure of that precondition). This is
    /// deliberately the same shape as every other optional port on this
    /// builder, not a special case: an embedder who wants `[hooks].rules[]`
    /// enforcement opts in explicitly, exactly like every other capability
    /// here.
    pub fn with_hook_runner(mut self, runner: Arc<dyn HookRunner>) -> Self {
        self.hook_runner = Some(runner);
        self
    }

    /// Convenience wrapper around [`Self::with_hook_runner`] that supplies
    /// this workspace's own in-tree default -- `conway_tools::hook_runner::
    /// ProcessHookRunner` -- rather than requiring every caller to name and
    /// construct that type itself (board item 01KZVTTP492R3BDY33FAGYWDNW).
    ///
    /// **Not a second injection mechanism.** This method does nothing
    /// `with_hook_runner` could not already do; it just fills in the one
    /// argument a caller wanting the shipped default would otherwise repeat
    /// verbatim everywhere. Contrast the general port itself, which stays
    /// exactly as general as before: a third party wanting its OWN
    /// `HookRunner` still calls `with_hook_runner` directly, on the
    /// identical surface a built-in uses, since a built-in gets no privileged
    /// API -- this method is not
    /// where that capability lives, and the two are deliberately kept
    /// separate rather than collapsed into one (calling this method twice,
    /// or this method then `with_hook_runner`, behaves exactly like calling
    /// `with_hook_runner` twice: last write wins, no special-casing here).
    ///
    /// Gated on the `builtin-tools` feature, mirroring `crate::presets`'
    /// own built-in-plugin methods: with that feature disabled, this crate
    /// has no `conway-tools` dependency to construct a `ProcessHookRunner`
    /// from, so this method does not exist rather than existing and
    /// panicking or silently no-opping.
    ///
    /// `conway-cli`'s `build_conway` is the intended caller (board item
    /// 01KZVTTP492R3BDY33FAGYWDNW): the CLI itself never depends on
    /// `conway-tools` directly (`crates/conway-cli/tests/cli_surface.rs::
    /// no_forbidden_deps` forbids that edge outright), so this facade
    /// method is what lets the CLI obtain the workspace's default runner
    /// without naming `conway-tools` at all -- the same shape
    /// `presets::builtin_plugins` already establishes for built-in tool
    /// plugins (this file's own `build()`, around the `builtin-tools` cfg
    /// block for `resolved_plugins`).
    #[cfg(feature = "builtin-tools")]
    pub fn with_default_hook_runner(self) -> Self {
        self.with_hook_runner(Arc::new(conway_tools::hook_runner::ProcessHookRunner::new()))
    }

    /// Overrides which built-in plugins `build()` auto-registers (board
    /// item: bash ships on by default and cannot be declined). See
    /// [`PluginSelection`]'s own doc for why this is a generic, id-keyed
    /// mechanism rather than a bash-specific switch.
    ///
    /// **Not called at all (the default)** means `build()` derives the
    /// selection from `config.tools.builtin_plugins`
    /// ([`crate::config::schema::ToolsConfig`]) instead -- itself defaulting
    /// to every built-in EXCEPT `conway.shell` (bash). Either way, obtaining
    /// bash is a deliberate act: call this method with a selection that
    /// names `"conway.shell"` (or `PluginSelection::All`), or add
    /// `"conway.shell"` to a loaded config's `tools.builtin_plugins` array.
    pub fn with_builtin_plugins(mut self, selection: PluginSelection) -> Self {
        self.builtin_selection = Some(selection);
        self
    }

    /// Overrides the default `JsonlSessionStore`.
    pub fn with_session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Overrides the default router (board item 01KZFC43J1J06BM4CCWKCKHSNV:
    /// `conway_core::routing::MinimalRouter`, the config-only core resolver
    /// `build()` compiles when neither this nor `with_router_factory` is
    /// called -- see that method's own doc). `Conway::explain_routing` falls
    /// back to `MinimalRouter` (an honestly degenerate report -- no
    /// capability/health filtering, one entry per configured chain
    /// candidate) when this is set, since a `RoutingExplainer` for an
    /// arbitrary injected `Router` trait object does not exist.
    pub fn with_router(mut self, router: Arc<dyn Router>) -> Self {
        self.router = Some(router);
        self
    }

    /// Registers a [`RouterFactory`] (board item 01KZFC2MD1FVNA674YJ9A19T8E):
    /// a router KIND, named up front, whose actual construction is deferred
    /// to `build()`'s own router step -- once backends and a resolved
    /// routing/headroom policy actually exist for the factory to build
    /// against.
    ///
    /// **Precedence, exact:** an injected [`Self::with_router`] wins
    /// UNCONDITIONALLY over this -- it is never wrapped, inspected, or
    /// validated, and a factory set here is then never even invoked (not
    /// merely ignored: `RouterFactory::build` is not called at all, so a
    /// factory with side effects in `build` sees none). Absent an injected
    /// router, a factory set here is invoked with the assembled
    /// `RouterBuildContext`; absent both, `build()` falls through to
    /// `conway_core::routing::MinimalRouter` -- the config-only core
    /// resolver (board item 01KZFC43J1J06BM4CCWKCKHSNV: `conway` no longer
    /// links a capability-/health-filtering router engine at all; that is
    /// exactly what this method installs). A factory whose `build` returns
    /// `Err` fails the whole `build()` call as `ConwayError::Build`, naming
    /// this factory's own `RouterFactory::id()` and the underlying message
    /// -- never silently swallowed, never a fallback to `MinimalRouter`.
    ///
    /// When the factory path IS taken, its returned `RouterBundle::health`
    /// replaces the `HealthRegistry` `build()` would otherwise have
    /// constructed (the honestly degenerate `AlwaysClosedHealthRegistry`,
    /// absent a factory) -- the router and the runtime it serves continue to
    /// share exactly one registry -- and its `RouterBundle::explain`, when
    /// present, becomes what `Conway::explain_routing` projects through
    /// (absent, `explain_routing` falls back to `MinimalRouter`, the same
    /// honest degenerate answer an injected `with_router` already falls back
    /// to).
    ///
    /// **Not called at all (the default)** changes nothing: `build()`'s
    /// router step behaves exactly as it did before this method existed --
    /// `MinimalRouter` over `[roles]`/`[routing]`, no capability or health
    /// filtering. `crates/conway-plugin-routing` is the first-party plugin
    /// that installs the richer `DeclarativeRouter` engine instead, either
    /// via this method directly or by naming its `ROUTER_ID` in
    /// `[plugins].install` (see `docs/routing.md`).
    pub fn with_router_factory(mut self, factory: Arc<dyn RouterFactory>) -> Self {
        self.router_factory = Some(factory);
        self
    }

    /// Sets CLI-sourced overrides, applied (and fully re-validated,
    /// including OAuth-token rejection) at `build()` time.
    pub fn with_cli_overrides(mut self, cli: CliOverrides) -> Self {
        self.cli_overrides = cli;
        self
    }

    /// Sets this `Conway`'s confinement root (board item
    /// 01KYTMH9JX21CGSE2Y6E2KP8SJ): every root agent
    /// [`crate::Conway::new_session`] starts afterward is confined to it,
    /// via `conway_runtime::runtime::RootSpec::root` -- the same S3/S5
    /// primitive (`AgentRoot`, `PermissionBroker::check_root`) that already
    /// confines a spawned child, now finally reachable for the agent an
    /// operator actually talks to.
    ///
    /// **Not called at all (the default)** means every root agent this
    /// `Conway` starts stays `Unconfined`, byte-for-byte identical to every
    /// invocation before this method existed -- this is deliberately NOT the
    /// default `build()` picks on its own; an operator opts in explicitly
    /// (`conway-cli`'s `--root`).
    ///
    /// **`cwd` was never the security boundary** (S0's own charter) -- this
    /// is a distinct setting from `ConwayConfig::cwd`/`SessionSpec::cwd`, not
    /// an inference from either. A relative `root` resolves against the
    /// SESSION's own `cwd` at `new_session` time (`RootSpec::root`'s own
    /// doc), which must itself already fall inside it -- `new_session`
    /// returns a typed error rather than starting an agent whose own working
    /// directory sits outside its own confinement.
    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = Some(root.into());
        self
    }

    /// Assembles the `Conway`. See the module doc for the full
    /// construction-order rationale and disclosed reconciliations.
    pub fn build(self) -> Result<Conway> {
        let ConwayBuilder {
            config,
            cli_overrides,
            backends,
            plugins,
            gate,
            store,
            router,
            router_factory,
            backend_factories,
            declined_backend_kinds,
            context_hook,
            hook_runner,
            builtin_selection,
            warnings,
            root,
        } = self;
        let declined_backend_kinds: HashSet<String> = declined_backend_kinds.into_iter().collect();

        // 1. Apply CLI overrides; re-validate. This is what catches an
        //    invalid override in a config assembled via `from_parts`, which
        //    bypasses `load`'s own validation entirely.
        let config = config::merge::apply_cli(&config, &cli_overrides)?;
        let cwd = config.cwd.clone();

        // 2. Load model metadata (facade's local JSON file; missing -> empty).
        let metadata_path = resolve_path(&cwd, &config.models.metadata_path);
        let metadata = config::model_metadata::load(&metadata_path)?;

        // 2b. Declarative provider profiles (board item 01KZHF270T3W8GZ7NM6DSNQ4MM):
        //     this facade no longer parses/merges `.conway/profiles.toml`
        //     itself -- that is now `conway_plugin_backends`'s own
        //     `OpenAiCompatBackendFactory::resolve_profile_store` concern
        //     (a kind with no "dialect" notion, like `"anthropic"`, simply
        //     never reads this field). What stays here is discovering WHICH
        //     files exist to read at all (project then global —
        //     `config::discovery::provider_profile_file_paths`, unchanged),
        //     resolved once so every `[backends.<id>]` entry's
        //     `BackendBuildContext` (construction below, and startup
        //     probing, step 5) carries the identical path list.
        let env: HashMap<String, String> = std::env::vars().collect();
        let profile_file_paths = config::discovery::provider_profile_file_paths(&cwd, &env);

        // 3+3b+4. Duplicate-kind check over every registered factory FIRST
        //         (before any factory's own `build` runs, regardless of
        //         whether a `[backends.<id>]` entry ever names it -- a
        //         dedicated pass, not "insert-then-error-on-the-second-one",
        //         so a duplicate never leaves an earlier factory's `build`
        //         side effects to have run while the whole call still
        //         fails). Then construct one backend per `[backends.<id>]`
        //         entry, resolving `entry.kind` against the registered
        //         factories ONLY (board item 01KZHF270T3W8GZ7NM6DSNQ4MM
        //         removed the temporary compiled-in fallback board item
        //         01KZHF1E85MS1VF4YH8CDNCP9Z left standing -- see
        //         `resolve_backend_factory`'s own doc). Then merge injected
        //         ones over all of that -- each step keyed into the same map
        //         by each backend's own `id()`, so a later step overwrites
        //         an earlier step's entry sharing an id. See
        //         `ConwayBuilder::with_backend_factory`'s own doc for the
        //         full precedence/duplicate-kind rules this step implements.
        //         Each entry's already-resolved `BackendBuildContext` is
        //         also kept (`probe_targets`) so step 5's optional startup
        //         probe can reuse it rather than re-resolving the same
        //         `api_key`/`profile_file_paths` a second time.
        let mut seen_factory_kinds: HashSet<&str> = HashSet::new();
        for factory in &backend_factories {
            if !seen_factory_kinds.insert(factory.id()) {
                return Err(ConwayError::Build {
                    message: format!(
                        "duplicate backend factory kind '{}': two factories registered via \
                         ConwayBuilder::with_backend_factory report the same BackendFactory::id()",
                        factory.id()
                    ),
                });
            }
        }
        let factories_by_kind: HashMap<&str, &Arc<dyn BackendFactory>> =
            backend_factories.iter().map(|f| (f.id(), f)).collect();

        let mut backend_map: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
        let mut probe_targets: Vec<(String, Arc<dyn BackendFactory>, BackendBuildContext)> =
            Vec::new();
        for (id, entry) in &config.backends {
            let factory =
                resolve_backend_factory(id, entry, &factories_by_kind, &declined_backend_kinds)?;
            let ctx = build_backend_context(id, entry, &metadata, &profile_file_paths)?;
            if config.models.probe_on_startup {
                probe_targets.push((id.clone(), factory.clone(), ctx.clone()));
            }
            let backend = factory.build(ctx).map_err(|e| ConwayError::Build {
                message: format!(
                    "backend '{id}': factory for kind '{}' failed to build: {e}",
                    entry.kind
                ),
            })?;
            backend_map.insert(backend.id(), backend);
        }
        for backend in backends {
            backend_map.insert(backend.id(), backend);
        }
        if backend_map.is_empty() {
            return Err(ConwayError::Build {
                message: "no backends configured: add a [backends.<id>] entry to config or call \
                          ConwayBuilder::with_backend"
                    .to_string(),
            });
        }

        // 5. CapabilityIndex, read directly from each constructed backend's
        //    own `Backend::capabilities()` (WI-123: the single accessor this
        //    index and the runtime's T-1 gate both read — see
        //    `CapabilityIndex::from_backends`'s doc) for every
        //    `(backend, model)` pair `models.json` declares. Optionally
        //    overlaid with a startup probe -- board item
        //    01KZHF270T3W8GZ7NM6DSNQ4MM relocated the probing mechanism
        //    itself into `conway_plugin_backends::OpenAiCompatBackendFactory
        //    ::probe_capabilities` (this facade's own resolution path no
        //    longer names `CapabilityProbe` or "openai-compat" at all); the
        //    RESTRICT eligibility filter below -- overlay only a pair
        //    `models.json` already declared for this backend, per
        //    `BackendFactory::probe_capabilities`'s own doc -- stays here,
        //    applied identically to every kind's discovered map, not
        //    delegated to each kind to get right on its own.
        let model_refs: Vec<ModelRef> = metadata
            .models
            .keys()
            .filter_map(|key| match key.parse::<ModelRef>() {
                Ok(model_ref) => Some(model_ref),
                Err(_) => {
                    tracing::warn!(
                        key = %key,
                        "model metadata key is not a valid 'backend/model' reference; skipping"
                    );
                    None
                }
            })
            .collect();
        let all_backends: Vec<Arc<dyn Backend>> = backend_map.values().cloned().collect();
        let mut index_builder =
            CapabilityIndex::from_backends(&all_backends, &model_refs).into_builder();
        for (id, factory, ctx) in &probe_targets {
            for (model_id, caps) in factory.probe_capabilities(ctx) {
                if !ctx.models.contains_key(model_id.as_str()) {
                    tracing::debug!(
                        backend = %id,
                        model = %model_id,
                        "probe_on_startup: server reported a model with no models.json entry \
                         for this backend; not admitting it (models.json is the sole source of \
                         routable models)"
                    );
                    continue;
                }
                index_builder = index_builder.insert(BackendId::new(id.clone()), model_id, caps);
            }
        }
        let capability_index = index_builder.build();

        // 6. Resolve routing/headroom config. Board item
        //    01KZFC43J1J06BM4CCWKCKHSNV: `conway` itself no longer links a
        //    circuit-breaker implementation (that engine moved to the
        //    `conway-plugin-routing` first-party plugin), so the default
        //    `HealthRegistry` -- absent an installed router factory -- is
        //    the honestly degenerate `AlwaysClosedHealthRegistry`: no
        //    breaker ever opens, `record` is a no-op. A factory's own
        //    `RouterBundle::health` REPLACES this below when one is taken.
        let routing_config = config.routing().map_err(|message| ConwayError::Config {
            path: None,
            message,
        })?;
        let headroom_policy = HeadroomPolicy::from_routing_config(&routing_config);
        let health: Arc<dyn HealthRegistry> = Arc::new(AlwaysClosedHealthRegistry);

        // 7. Router: injected `router` wins UNCONDITIONALLY over everything
        //    below and is never wrapped, inspected, or validated; else a
        //    `RouterFactory` (`with_router_factory`), when set, is invoked
        //    with the build context assembled from the preceding steps;
        //    else `conway_core::routing::MinimalRouter` -- the config-only
        //    core resolver `conway` compiles with no plugin installed (board
        //    item 01KZFC43J1J06BM4CCWKCKHSNV: this replaces the
        //    `DeclarativeRouter` `build()` used to compile in directly).
        //    Whichever explainer the taken branch produces (`None` for an
        //    injected router, the factory's own `RouterBundle::explain`, or
        //    `MinimalRouter` itself) is kept alongside the type-erased
        //    `Router` so `Conway::explain_routing` can still project
        //    through it. When the factory path is taken, its returned
        //    `health` REPLACES the `AlwaysClosedHealthRegistry` built
        //    immediately above -- the router and the runtime must continue
        //    to share exactly ONE registry, so `health` is reassigned
        //    below, never both kept alive.
        //    The three outcomes are spelled as a `RouterBundle` rather than a
        //    bare triple: that type already IS this exact shape (router,
        //    health, explain), because it is what a `RouterFactory` hands
        //    back. Naming it here keeps the three arms visibly agreeing on
        //    one contract instead of on tuple position, and is what the
        //    factory arm below unwraps into anyway.
        let RouterBundle {
            router,
            health,
            explain: router_explain,
        } = if let Some(router) = router {
            RouterBundle {
                router,
                health,
                explain: None,
            }
        } else if let Some(factory) = router_factory {
            let ctx = RouterBuildContext {
                routing: routing_config,
                headroom: headroom_policy.clone(),
                backends: &all_backends,
                capability_index,
            };
            factory.build(ctx).map_err(|e| ConwayError::Build {
                message: format!("router factory '{}' failed to build: {e}", factory.id()),
            })?
        } else {
            let compiled = Arc::new(MinimalRouter::new(routing_config));
            RouterBundle {
                router: compiled.clone() as Arc<dyn Router>,
                health,
                explain: Some(compiled as Arc<dyn RoutingExplainer>),
            }
        };

        // 8. Store: injected, else JsonlSessionStore::open (jsonl-store
        //    feature), else a Build error. NOTE the feature decides only
        //    whether THIS default is available -- `conway-session` is linked
        //    either way, via `conway-runtime`'s unconditional dependency on
        //    it (forward declaration, board 01KZVYVTVWRH20R6VJ6G3SWTJ6; see
        //    `Cargo.toml`'s own comment on the feature).
        let store: Arc<dyn SessionStore> = match store {
            Some(store) => store,
            None => build_default_store(&cwd, &config.session.root)?,
        };

        // 9. Gate: injected, else selected from config.permissions.
        let gate: Arc<dyn PermissionGate> = match gate {
            Some(gate) => gate,
            None => gates::from_config(&config.permissions, None)?,
        };

        // 10. Plugins: built-ins (filtered by `selection`) ++ injected;
        //     duplicate manifest ids error. Board item: bash ships on by
        //     default and cannot be declined -- `selection` is what makes
        //     `presets::builtin_plugins()`'s four candidates no longer an
        //     unconditional install; injected `plugins` (below) are never
        //     filtered by it (`with_plugin` is already an explicit
        //     declaration -- see `PluginSelection`'s own doc).
        let selection = builtin_selection
            .unwrap_or_else(|| PluginSelection::Only(config.tools.builtin_plugins.clone()));
        let mut resolved_plugins: Vec<Arc<dyn Plugin>> = Vec::new();
        let mut seen_plugin_ids: HashSet<String> = HashSet::new();
        #[cfg(feature = "builtin-tools")]
        {
            for plugin in presets::builtin_plugins() {
                let id = plugin.manifest().id.clone();
                if selection.allows(&id) {
                    seen_plugin_ids.insert(id);
                    resolved_plugins.push(plugin);
                }
            }
        }
        #[cfg(not(feature = "builtin-tools"))]
        {
            // No candidate bundle exists to filter without this feature;
            // `selection` is still computed above (from `config.tools`, or
            // an explicit `with_builtin_plugins` call) so it is consumed
            // either way rather than triggering an unused-variable warning.
            let _ = &selection;
        }
        for plugin in plugins {
            let id = plugin.manifest().id.clone();
            if !seen_plugin_ids.insert(id.clone()) {
                return Err(ConwayError::Build {
                    message: format!("duplicate plugin id: '{id}'"),
                });
            }
            resolved_plugins.push(plugin);
        }

        // 11. Agent defs.
        let agents_dir = resolve_path(&cwd, &config.agents.dir);
        let agent_defs = agents::load_agent_defs(&agents_dir)?;

        // 12. Runtime::new.
        let event_bus = EventBus::new(EVENT_BUS_CAPACITY);
        let rt = Runtime::new(RuntimeDeps {
            store: store.clone(),
            router,
            health,
            backends: backend_map,
            plugins: resolved_plugins,
            gate,
            agent_defs,
            event_bus,
            headroom: Arc::new(headroom_policy),
        });
        // WI-126: `RuntimeDeps` has no `context_hook` field (out of that
        // item's file scope to add -- see `conway_runtime::runtime`'s
        // module doc), so registration happens post-construction via this
        // dedicated setter. `context_hook: None` (no `with_context_hook`
        // call) sets the runtime's hook to `None`, identical to never
        // calling this method at all.
        rt.set_context_hook(context_hook);
        // Board item 01KZS00JP5QNBJSSHNFP9C47GM: mirrors the `context_hook`
        // wiring immediately above -- `hook_runner: None` (no
        // `with_hook_runner` call) sets the broker's runner to `None`,
        // identical to never calling `Runtime::set_hook_runner` at all
        // (`PermissionBroker::decide`'s hook-check step stays a no-op).
        // `pre_tool_use_specs` is computed unconditionally either way (an
        // empty `hook_runner` makes it inert regardless of what it
        // contains -- `PermissionBroker::pre_tool_use_hook_denial`'s own
        // doc), filtering `[hooks].rules[]` to exactly the entries this
        // item's own `HooksConfig` doc names as dispatched: `event ==
        // "pre_tool_use"` and `enabled`.
        let pre_tool_use_specs: Vec<PreToolUseHookSpec> = config
            .hooks
            .rules
            .iter()
            .filter(|rule| rule.enabled && rule.event == "pre_tool_use")
            .map(|rule| PreToolUseHookSpec {
                id: rule.id.clone(),
                command: rule.command.clone(),
                timeout_ms: rule.timeout_ms,
            })
            .collect();
        rt.set_hook_runner(hook_runner);
        rt.set_pre_tool_use_hooks(pre_tool_use_specs);

        Ok(Conway::new(
            rt,
            config,
            store,
            router_explain,
            warnings,
            metadata,
            root,
        ))
    }
}

/// Resolves `p` against `cwd` when relative; returns `p` unchanged when
/// already absolute. Mirrors `config::merge`'s own (private)
/// `resolve_metadata_path` helper.
fn resolve_path(cwd: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

/// Resolves a backend's effective API key: the literal `api_key` if set,
/// else the value named by `api_key_env` read from the live process
/// environment (a named-but-unset var is a `ConwayError::Config`), else an
/// empty string (no key configured). `merge::validate` already established
/// the two are mutually exclusive.
///
/// The key's *shape* is never inspected. An `api_key_env` that names an
/// unset variable is a configuration mistake conway can describe exactly,
/// so that stays a hard error; what the resolved value looks like is the
/// provider's judgment to make, not conway's.
fn resolve_api_key(id: &str, entry: &BackendEntry) -> Result<String> {
    if !entry.api_key.is_empty() {
        return Ok(entry.api_key.clone());
    }
    if !entry.api_key_env.is_empty() {
        let resolved = std::env::var(&entry.api_key_env).map_err(|_| ConwayError::Config {
            path: None,
            message: format!(
                "backend '{id}': api_key_env '{}' is not set in the environment",
                entry.api_key_env
            ),
        })?;
        return Ok(resolved);
    }
    Ok(String::new())
}

/// Resolves one `[backends.<id>]` entry's `kind` against every registered
/// [`BackendFactory`] (board item 01KZHF1E85MS1VF4YH8CDNCP9Z: `kind` is an
/// open name, not a closed enum) -- ONLY against registered factories, with
/// no compiled-in fallback: board item 01KZHF270T3W8GZ7NM6DSNQ4MM removed
/// the temporary two-adapter fallback this function (then named
/// `construct_backend`) used to fall through to (`"anthropic"`,
/// `"openai-compat"` compiled directly into this facade), the deliberate,
/// disclosed gap that item's own predecessor left standing so its slice
/// could ship alone. Every kind this facade resolves today, including
/// those two, is therefore a registered factory -- see
/// `conway_plugin_backends::factory`'s own module doc for what makes both
/// attach by default with no `[plugins].install`/`with_backend_factory` call
/// an operator has to write by hand.
///
/// A `kind` no registered factory claims is a hard, named
/// [`ConwayError::Config`] listing every kind this build actually
/// recognises -- the same disclosure shape
/// `crates/conway-cli/src/first_party_plugins.rs`'s unknown-id error already
/// produces for plugin ids (a silently ignored `kind` is exactly the
/// failure that check exists to prevent).
///
/// **Two distinct diagnoses for that same failure** (board item
/// 01KZHF2W8Y1KBM7PJH7R4QQJA0), chosen by whether `entry.kind` appears in
/// `declined` ([`ConwayBuilder::with_declined_backend_kinds`]):
/// - present -> a **declined-kind** error: this build recognises the kind by
///   name but a caller deliberately did not attach a factory for it.
/// - absent -> the pre-existing **unknown-kind** error, unchanged: this
///   build has never heard of the kind at all.
///
/// Both are the identical hard `build()`-time [`ConwayError::Config`] this
/// function always raised -- neither timing nor severity changes, only the
/// message an operator reads, so declining a shipped dialect and forgetting
/// a `[backends.<id>]` entry that still names it fails the whole `build()`
/// exactly as an unknown kind always has (see `PluginsConfig::
/// default_backends`'s own doc for why a build with zero backends is never
/// an acceptable silent outcome to fall back to instead).
fn resolve_backend_factory<'a>(
    id: &str,
    entry: &BackendEntry,
    factories: &'a HashMap<&str, &Arc<dyn BackendFactory>>,
    declined: &HashSet<String>,
) -> Result<&'a Arc<dyn BackendFactory>> {
    factories.get(entry.kind.as_str()).copied().ok_or_else(|| {
        let mut known: Vec<String> = factories.keys().map(|k| k.to_string()).collect();
        known.sort();
        known.dedup();
        if declined.contains(entry.kind.as_str()) {
            ConwayError::Config {
                path: None,
                message: format!(
                    "backend '{id}': kind '{}' was declined, not installed for this build. This \
                     is a DIFFERENT diagnosis than a kind this build has never heard of at all: \
                     '{}' is a recognised dialect that plugins.default_backends/plugins.install \
                     no longer names (or that an embedder chose not to attach via \
                     ConwayBuilder::with_backend_factory). Installed kinds: [{}]. Add '{}' back \
                     to plugins.default_backends (or plugins.install), or call \
                     ConwayBuilder::with_backend_factory for it, before build().",
                    entry.kind,
                    entry.kind,
                    known.join(", "),
                    entry.kind
                ),
            }
        } else {
            ConwayError::Config {
                path: None,
                message: format!(
                    "backend '{id}': unknown kind '{}'; recognised kinds: [{}]. A third-party \
                     kind is installed with ConwayBuilder::with_backend_factory before build().",
                    entry.kind,
                    known.join(", ")
                ),
            }
        }
    })
}

/// Resolves the [`BackendBuildContext`] a registered [`BackendFactory`]
/// receives for one `[backends.<id>]` entry naming its kind: `id` is the
/// entry's own JSON key, `base_url`/`dialect` copied verbatim, `api_key`
/// resolved through the same centralized [`resolve_api_key`] every
/// config-derived backend's credential already goes through, `models` the
/// same per-backend `models.json` overrides [`models_overrides_for`]
/// projects (WI-123's single-source guarantee, extended to every registered
/// kind rather than left a built-ins-only privilege), `profile_file_paths`
/// copied verbatim from [`ConwayBuilder::build`]'s own step 2b resolution --
/// see that field's own doc ([`conway_core::ports::BackendBuildContext`]) for
/// why every kind receives the identical list whether or not it reads it --
/// and `extra` cloned verbatim from this same `entry`'s own
/// [`BackendEntry::extra`] (board item 01KZMM8ABQJQGHTDTP5S29P88C), never
/// from anywhere else: this is the ONLY place that map is read out of the
/// loaded config and handed onward, closing the gap where it was previously
/// captured at load time and then discarded before any factory saw it.
fn build_backend_context(
    id: &str,
    entry: &BackendEntry,
    metadata: &config::model_metadata::ModelMetadata,
    profile_file_paths: &[PathBuf],
) -> Result<BackendBuildContext> {
    let api_key = resolve_api_key(id, entry)?;
    Ok(BackendBuildContext {
        id: BackendId::new(id),
        base_url: entry.base_url.clone(),
        api_key: if api_key.is_empty() {
            None
        } else {
            Some(api_key)
        },
        dialect: entry.dialect.clone(),
        models: models_overrides_for(id, metadata),
        profile_file_paths: profile_file_paths.to_vec(),
        extra: entry.extra.clone(),
    })
}

/// Per-model capability overrides for backend `id`, projected from the
/// facade's loaded `models.json` metadata (keyed `"backend/model"`). This is
/// the *only* channel `models.json` has into `Backend::capabilities()`
/// (called directly by the T-1 gate in `conway_runtime::attempt`, and
/// indirectly by the router's `CapabilityIndex` — see step 5 of
/// [`ConwayBuilder::build`]) — so without wiring the metadata into the
/// backend's own override table here, a `max_context_tokens` or
/// `reliability_tier` set in `models.json` would silently never reach
/// routing.
///
/// Only `max_context_tokens` and `reliability_tier` are projected:
/// `ModelOverrides` (`conway_core::routing`) has no field for
/// `tool_calling`/`reasoning`, so those two `models.json` fields currently
/// have no effect here — see this module's doc for the scope-boundary note.
fn models_overrides_for(
    id: &str,
    metadata: &config::model_metadata::ModelMetadata,
) -> BTreeMap<String, ModelOverrides> {
    metadata
        .models
        .iter()
        .filter_map(|(key, m)| {
            let model_ref = key.parse::<ModelRef>().ok()?;
            if model_ref.backend.as_str() != id {
                return None;
            }
            Some((
                model_ref.model.as_str().to_string(),
                ModelOverrides {
                    stream_tools: None,
                    max_context_tokens: Some(m.max_context_tokens),
                    reliability_tier: Some(parse_reliability_tier(&m.reliability_tier)),
                    parallel_tool_calls: None,
                    min_headroom_tokens: None,
                },
            ))
        })
        .collect()
}

/// Parses the facade's `models.json` `reliability_tier` string (WI-097's
/// JSON schema). Used both by [`models_overrides_for`] (the
/// `Backend::capabilities()`/router-`CapabilityIndex` channel) — any value
/// other than `"verified"`/`"community"` is `Unknown`, never a hard error:
/// `models.json` is user-editable data, not a validated config surface.
fn parse_reliability_tier(raw: &str) -> ReliabilityTier {
    match raw.to_ascii_lowercase().as_str() {
        "verified" => ReliabilityTier::Verified,
        "community" => ReliabilityTier::Community,
        _ => ReliabilityTier::Unknown,
    }
}

#[cfg(feature = "jsonl-store")]
fn build_default_store(cwd: &Path, root: &Path) -> Result<Arc<dyn SessionStore>> {
    let root = resolve_path(cwd, root);
    let store = block_on(conway_session::JsonlSessionStore::open(root))?;
    Ok(Arc::new(store))
}

/// The `jsonl-store`-off arm. Reached when the feature is disabled and no
/// store was injected -- note this is the ONLY thing the feature changes.
/// `conway-session` is still linked (via `conway-runtime`); what is gone is
/// this crate's ability to name `conway_session::JsonlSessionStore` and wire
/// it by default. Forward declaration, board 01KZVYVTVWRH20R6VJ6G3SWTJ6.
#[cfg(not(feature = "jsonl-store"))]
fn build_default_store(_cwd: &Path, _root: &Path) -> Result<Arc<dyn SessionStore>> {
    Err(ConwayError::Build {
        message: "no session store configured: enable the 'jsonl-store' feature or call \
                  ConwayBuilder::with_session_store"
            .to_string(),
    })
}

/// Runs `fut` to completion on a fresh OS thread with its own throwaway
/// current-thread `tokio` runtime, so an `async` lower-crate API can be
/// called from `build()`'s synchronous signature without panicking when
/// `build()` is itself invoked from inside an already-running `tokio` task
/// (`Handle::current().block_on` panics in that situation; a brand new
/// thread + runtime does not). See the module doc's top-level reconciliation
/// note.
fn block_on<F>(fut: F) -> F::Output
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build temporary tokio runtime for ConwayBuilder::build's sync/async bridge")
                    .block_on(fut)
            })
            .join()
            .expect("ConwayBuilder::build's blocking-bridge thread panicked")
    })
}

#[cfg(test)]
mod models_overrides_tests {
    use super::*;
    use crate::config::model_metadata::{ModelMetadata, ModelMetadataEntry};

    fn entry(max_context_tokens: u32) -> ModelMetadataEntry {
        ModelMetadataEntry {
            max_context_tokens,
            tool_calling: "streaming".to_string(),
            reasoning: true,
            reliability_tier: "verified".to_string(),
        }
    }

    #[test]
    fn projects_only_the_matching_backends_models_with_the_configured_window() {
        let mut m = ModelMetadata::empty();
        m.models
            .insert("ollama_cloud/glm-5.2".to_string(), entry(1_000_000));
        m.models
            .insert("other_backend/foo".to_string(), entry(4_096));

        let ov = models_overrides_for("ollama_cloud", &m);

        // Only this backend's model is projected; the window from models.json
        // is carried through as a per-model override (the value the router's
        // T-1 context-fit check reads via Backend::capabilities).
        assert_eq!(ov.len(), 1);
        assert_eq!(ov["glm-5.2"].max_context_tokens, Some(1_000_000));
        assert!(!ov.contains_key("foo"));
    }

    #[test]
    fn skips_keys_that_are_not_valid_backend_slash_model_refs() {
        let mut m = ModelMetadata::empty();
        m.models.insert("not-a-ref".to_string(), entry(100));
        assert!(models_overrides_for("ollama_cloud", &m).is_empty());
    }

    #[test]
    fn reliability_tier_is_projected_alongside_max_context_tokens() {
        let mut m = ModelMetadata::empty();
        m.models.insert(
            "ollama_cloud/glm-5.2".to_string(),
            ModelMetadataEntry {
                reliability_tier: "community".to_string(),
                ..entry(1_000)
            },
        );
        let ov = models_overrides_for("ollama_cloud", &m);
        assert_eq!(
            ov["glm-5.2"].reliability_tier,
            Some(ReliabilityTier::Community)
        );
    }

    /// Declarative provider profiles: `resolve_profile`'s own coverage
    /// (every documented dialect string, both plain and the three
    /// kebab-case spellings; a brand-new built-in profile resolved by name
    /// with no special-casing; a user-supplied profile resolved with no
    /// recompile; an unknown name rejected with a named, typed error rather
    /// than a panic) moved with the function itself to
    /// `conway_plugin_backends::factory` (board item 01KZHF270T3W8GZ7NM6DSNQ4MM)
    /// -- see that crate's `src/factory.rs` test module for the ported
    /// tests, unchanged in what they check.
    ///
    /// WI-123's core proof: `models.json` has exactly one predictable
    /// routing effect. The value `Backend::capabilities()` returns (what
    /// `conway_runtime::attempt::AttemptEngine`'s T-1 gate reads directly)
    /// and the value the router's `CapabilityIndex` resolves for the same
    /// pair (built via `CapabilityIndex::from_backends`, step 5 of
    /// `ConwayBuilder::build`) must be identical -- not two independently
    /// recomputed values that can silently drift apart.
    #[test]
    fn models_json_drives_both_backend_capabilities_and_router_index_identically() {
        use conway_core::ids::ModelId;
        use conway_plugin_backends::config::{Dialect, OpenAiCompatConfig};
        use conway_plugin_backends::openai_compat::OpenAiCompatBackend;

        let mut m = ModelMetadata::empty();
        m.models.insert(
            "ollama_cloud/glm-5.2".to_string(),
            ModelMetadataEntry {
                max_context_tokens: 1_000_000,
                tool_calling: "non_streaming".to_string(),
                reasoning: false,
                reliability_tier: "community".to_string(),
            },
        );

        let cfg = OpenAiCompatConfig {
            id: BackendId::new("ollama_cloud"),
            base_url: url::Url::parse("http://localhost:11434").unwrap(),
            api_key: None,
            profile: Dialect::Ollama.profile(),
            timeout: None,
            metadata_path: None,
            models: models_overrides_for("ollama_cloud", &m),
        };
        let backend: Arc<dyn Backend> =
            Arc::new(OpenAiCompatBackend::new(cfg).expect("valid config must construct"));
        let model = ModelId::new("glm-5.2");

        // What the runtime's T-1 gate reads directly (attempt.rs, WI-122;
        // out of this item's file scope, but this is its accessor).
        let direct = backend.capabilities(&model);
        assert_eq!(
            direct.max_context_tokens, 1_000_000,
            "ollama's 32K dialect default must be overridden by models.json"
        );
        assert_eq!(direct.reliability_tier, ReliabilityTier::Community);

        // What the router's CapabilityIndex resolves for the same pair.
        let model_ref: ModelRef = "ollama_cloud/glm-5.2".parse().unwrap();
        let index = CapabilityIndex::from_backends(&[backend], std::slice::from_ref(&model_ref));
        let via_index = index.get(&model_ref).expect("model present in index");
        assert_eq!(
            *via_index, direct,
            "router CapabilityIndex must agree exactly with Backend::capabilities() -- \
             no divergent projection"
        );
    }
}
