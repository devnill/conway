//! `ConwayBuilder`: assembles a validated [`crate::config::ConwayConfig`]
//! plus optional injected ports into a live [`crate::conway::Conway`]
//!. This is the wiring layer — it contains no agent logic.
//!
//! ## Reconciliations against the binding spec (disclosed, not worked around)
//!
//! - **`build(self) -> Result<Conway>` is synchronous** (the golden
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
//!   `async` signatures — not an oversight. The optional startup capability
//!   probe used to be the OTHER caller of this same bridge, directly in this
//!   module;
//!   `conway_plugin_backends::OpenAiCompatBackendFactory::
//!   probe_capabilities` now runs its own probe behind its own,
//!   independently-maintained bridge — see that method's own doc — so this
//!   module's `block_on` is used by [`build_default_store`] alone today.)
//! - **`with_prompt_handler` now exists** (board item
//!   01M00QGYR1M8F71HTAA1S3PEKS closed the gap this bullet used to disclose
//!   as unresolved): `gates::from_config` is called with whatever handler
//!   [`ConwayBuilder::with_prompt_handler`] supplied, `None` when it was
//!   never called. Since `permissions.mode` defaults to `"prompt"`
//!   (`config::merge::default_document`), an embedder using an unmodified
//!   default config and neither `with_prompt_handler` nor
//!   `with_permission_gate` still gets a named `FacadeError::Config` from
//!   `build()` — unchanged, and deliberately so (see that method's own doc):
//!   the fix is a direct path to the one closure a host almost always
//!   already has, not a silent default gate choice.
//! - **Backend construction, dialect/profile resolution, and startup
//!   capability probing are `conway_plugin_backends`'s concern, not this
//!   module's**: `resolve_backend_
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
//!   not from a second `models.json` → `Capabilities` conversion**:
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
use conway_core::error::PluginError;
use conway_core::event_name::EVENT_NAMESPACE_SEPARATOR;
use conway_core::hook::HookOrigin;
use conway_core::ids::{BackendId, ModelRef};
use conway_core::permission_pattern::{PatternOrigin, Rule, Select, Then, When};
use conway_core::ports::CapabilityIndex;
use conway_core::ports::{
    Backend, BackendBuildContext, BackendFactory, CapabilityRegistration, CapabilityRegistry,
    ContextHook, CurateOutcome, Curator, HealthRegistry, HookRunner, PathArgs, PathStore,
    PermissionGate, Plugin, PluginHookRule, PluginManifest, PluginPermissionRule,
    PluginPermissionVerdict, PluginStatusContribution, RenderKind, Router, RouterBuildContext,
    RouterBundle, RouterFactory, RoutingExplainer, SessionStore,
};
use conway_core::routing::{AlwaysClosedHealthRegistry, MinimalRouter, ModelOverrides};
use conway_runtime::context::PluginInstruction;
use conway_runtime::events::EventBus;
use conway_runtime::hook_dispatch::{declared_plugin_events, HookSpec, DISPATCHED_EVENTS};
use conway_runtime::permission::PreToolUseHookSpec;
use conway_runtime::runtime::{Runtime, RuntimeDeps};

use crate::agents;
use crate::config::schema::{BackendEntry, ConwayConfig, HookEntry};
use crate::config::{self, CliOverrides, ConfigWarning, LoadOptions, WarningCode};
use crate::conway::Conway;
use crate::discovery_host;
use crate::error::{FacadeError, Result};
use crate::gates;
use crate::host_caps::HostCaps;
#[cfg(feature = "builtin-tools")]
use crate::presets;
use crate::skills;

/// The capacity of the runtime's broadcast event bus. ASSUMPTION: no
/// criterion pins this value and `conway-runtime` exports no default
/// constant; picked generously (matching the order of magnitude
/// `conway-runtime`'s own tests use for a long-lived bus) rather than
/// inventing a config surface this item has no mandate to add.
const EVENT_BUS_CAPACITY: usize = 1024;

/// Which built-in plugins [`ConwayBuilder::build`] auto-registers, filtered
/// by each candidate's own `PluginManifest::id` (bash ships on
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
    /// The handler [`Self::build`]'s step 9 passes to `gates::from_config`
    /// when `permissions.mode = "prompt"` and no [`Self::with_permission_gate`]
    /// override is set. `None` (the default) is unchanged from before this
    /// field existed: an unmodified default config (`permissions.mode`
    /// defaults to `"prompt"` -- `config::merge::default_document`) with
    /// neither this nor `with_permission_gate` set still fails `build()`
    /// with a named `FacadeError::Config` naming exactly that, rather than
    /// silently choosing `AllowAlways`/`DenyAll` on a caller's behalf -- see
    /// [`Self::with_prompt_handler`]'s own doc for what setting this closes.
    prompt_handler: Option<gates::PromptHandler>,
    store: Option<Arc<dyn SessionStore>>,
    /// `None` (the default) means [`Self::build`]'s step 8 co-locates a
    /// `conway_session::FsPathStore` with the default `store` at
    /// `config.session.root` (D1-3d-wire: `RuntimeDeps::path_store`'s own
    /// doc), exactly mirroring `store`'s own injected-else-default
    /// precedence immediately above.
    path_store: Option<Arc<dyn PathStore>>,
    router: Option<Arc<dyn Router>>,
    /// `None` (the default) means
    /// `build()`'s router step falls through to compiling its own
    /// `DeclarativeRouter`, exactly as it did before this field existed --
    /// see [`Self::with_router_factory`]'s own doc for the full precedence.
    router_factory: Option<Arc<dyn RouterFactory>>,
    /// Empty (the default) means
    /// `build()`'s backend step is byte-for-byte what it was before this
    /// field existed -- config-derived backends merged with `backends`
    /// (above), nothing more -- see [`Self::with_backend_factory`]'s own doc
    /// for the full precedence and duplicate-kind rules.
    backend_factories: Vec<Arc<dyn BackendFactory>>,
    /// Empty (the default) means
    /// nothing changes from before this field existed -- see
    /// [`Self::with_declined_backend_kinds`]'s own doc for what a non-empty
    /// value does (purely diagnostic; it never removes, blocks, or replaces
    /// a registered [`BackendFactory`]).
    declined_backend_kinds: Vec<String>,
    /// . `None` (the default) means `build()` never calls
    /// `Runtime::set_context_hook` at all, leaving every agent's
    /// `context_hook` at the `Runtime`-constructed default of `None` --
    /// i.e. today's behavior, unchanged.
    context_hook: Option<Arc<dyn ContextHook>>,
    /// An embedder-injected [`Curator`] (DESIGN §11.4). `None` (the default)
    /// means `build()` contributes no curator of its own -- it still composes
    /// any plugin-contributed curators, and if BOTH are `None` the runtime's
    /// `context_curator` stays `None`, leaving the pre-assembly stage a
    /// zero-cost pass-through (the `context_golden` 11/11 gate's
    /// load-bearing guarantee).
    context_curator: Option<Arc<dyn Curator>>,
    /// `None` (the default) means
    /// `build()` never calls `Runtime::set_hook_runner` at all, leaving
    /// `PermissionBroker::decide`'s `pre_tool_use` hook-check step at the
    /// `PermissionBroker`-constructed default of `None` -- a byte-for-byte
    /// no-op, i.e. today's behavior, unchanged, REGARDLESS of whatever
    /// `[hooks].rules[]` a loaded config declares (see
    /// [`Self::with_hook_runner`]'s own doc).
    hook_runner: Option<Arc<dyn HookRunner>>,
    /// (bash ships on by default and cannot be declined).
    /// `None` (the default) means `build()` derives the effective
    /// [`PluginSelection`] from `config.tools.builtin_plugins` instead --
    /// see [`Self::with_builtin_plugins`]'s doc.
    builtin_selection: Option<PluginSelection>,
    warnings: Vec<ConfigWarning>,
    /// An operator-set confinement
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
    /// Empty (the default) means [`Self::build`]'s agent-def step reads
    /// exactly `config.agents.dir`, unchanged from before this field
    /// existed. A non-empty list is folded in AFTER `config.agents.dir`
    /// (which therefore always wins a name collision against every entry
    /// here — `agents::load_agent_defs_from_roots`'s own precedence rule),
    /// each entry resolved against `cwd` the same way `config.agents.dir`
    /// is, in the order [`Self::with_extra_agent_dir`] was called. See
    /// [`crate::config::schema::AgentsConfig`]'s own doc for why this lives
    /// here rather than as a config field.
    extra_agent_dirs: Vec<PathBuf>,
    /// The skills-side twin of `extra_agent_dirs` immediately above — same
    /// empty-by-default/fold-in-after/resolved-against-`cwd` contract, over
    /// the fixed `.conway/skills` operator root [`Self::build`] always
    /// reads first instead of a config field (skills has never had one —
    /// see `skills::load_skill_defs_from_roots`'s own doc).
    extra_skill_dirs: Vec<PathBuf>,
}

impl ConwayBuilder {
    /// Loads config from an explicit path (bypassing discovery), still
    /// layered under user/env/CLI precedence.
    ///
    /// **This still reads the ambient user layer**
    /// (`$CONWAY_CONFIG_DIR/settings.json`, or `~/.conway/settings.json`)
    /// unconditionally, before `path` — exactly as documented above, and
    /// unchanged by. A caller that
    /// wants `path` to be the *only* config file read — a test fixture, or
    /// an embedder that wants to use its own configuration rather than
    /// whatever is in the invoking user's home directory — wants
    /// [`Self::from_config_only`] instead.
    pub fn from_config(path: impl AsRef<Path>) -> Result<Self> {
        let options = LoadOptions {
            explicit_path: Some(path.as_ref().to_path_buf()),
            ..LoadOptions::default()
        };
        let outcome = config::load(options)?;
        Ok(Self::from_parts(outcome.config).with_warnings(outcome.warnings))
    }

    /// Loads config from an explicit path, ignoring the ambient user/user
    /// layer entirely — the merge
    /// this method drives is `default < path < env < CLI`, four sources
    /// instead of [`Self::from_config`]'s five.
    ///
    /// **`env` is deliberately NOT suppressed** — `CONWAY_*` environment
    /// variables are how CI and container entrypoints hand a specific
    /// invocation its credentials and overrides, a caller-supplied input to
    /// *this* invocation, not ambient state left over from someone else's.
    /// See [`crate::config::merge::load_ignoring_user_config`]'s own doc for the
    /// full reasoning. A caller that also wants an env-free load already
    /// has the tool for that: [`Self::from_parts`] with a manually
    /// assembled [`ConwayConfig`], or `config::load_ignoring_user_config` with a
    /// hand-built (possibly empty) `env` map.
    ///
    /// The second consumer this seam serves, beyond test isolation: a host
    /// application embedding `conway` as a library dependency has, until
    /// this method, had no way to say "use my configuration" without first
    /// discovering, and being at the mercy of, whatever happens to sit at
    /// `~/.conway/settings.json` on the machine it runs on.
    pub fn from_config_only(path: impl AsRef<Path>) -> Result<Self> {
        let options = LoadOptions {
            explicit_path: Some(path.as_ref().to_path_buf()),
            ..LoadOptions::default()
        };
        let outcome = config::load_ignoring_user_config(options)?;
        Ok(Self::from_parts(outcome.config).with_warnings(outcome.warnings))
    }

    /// Loads config via the standard five-source discovery/precedence chain
    /// (`config::load` with `LoadOptions::default()`, whose own `cwd`
    /// defaults to `std::env::current_dir()`).
    pub fn discover() -> Result<Self> {
        let outcome = config::load(LoadOptions::default())?;
        Ok(Self::from_parts(outcome.config).with_warnings(outcome.warnings))
    }

    /// [`Self::discover`]/[`Self::from_config`], but from CALLER-SUPPLIED
    /// `options` rather than `LoadOptions::default()` -- the seam none of
    /// `discover`/`from_config`/`from_config_only` have: each hard-codes
    /// `cwd: std::env::current_dir()`/`env: std::env::vars()`, this
    /// PROCESS's real ambient values, with no way for an in-process caller
    /// to supply its own instead. Board item `01M0QK9GRM8HSNWRAR414TCX42`
    /// is what surfaced the gap: `[session].root`'s central-default
    /// resolution happens INSIDE `config::load` itself, using
    /// `LoadOptions.cwd`/`.env` directly, so a caller that needs THAT
    /// resolved against something other than this process's real
    /// environment (a fixture's own isolated `CONWAY_CONFIG_DIR`/`cwd`, the
    /// case every in-process test building a `Conway` against a temp-dir
    /// fixture is in) previously had no way to get it -- a LATER
    /// `CliOverrides.cwd`/`with_cli_overrides` fix-up, applied at `build()`
    /// time, is too late for a resolution that already happened inside
    /// `load`. Still the full five-source chain (`default < user < project
    /// < env < CLI`) -- `options.env`'s own `CONWAY_CONFIG_DIR` still
    /// decides whether the "user" layer means a real `~/.conway/
    /// settings.json` or an isolated fixture directory with none;
    /// [`Self::from_options_ignoring_user_config`] is the sibling that
    /// drops that layer entirely, mirroring `from_config_only`.
    pub fn from_options(options: LoadOptions) -> Result<Self> {
        let outcome = config::load(options)?;
        Ok(Self::from_parts(outcome.config).with_warnings(outcome.warnings))
    }

    /// [`Self::from_options`]'s `from_config_only`-shaped sibling: the merge
    /// is `default < project < env < CLI` (four sources, `options.explicit_
    /// path`/`options.cwd`-discovered project layer, no user layer), from
    /// CALLER-SUPPLIED `options` rather than `LoadOptions::default()`.
    pub fn from_options_ignoring_user_config(options: LoadOptions) -> Result<Self> {
        let outcome = config::load_ignoring_user_config(options)?;
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
            prompt_handler: None,
            store: None,
            path_store: None,
            router: None,
            router_factory: None,
            backend_factories: Vec::new(),
            declined_backend_kinds: Vec::new(),
            context_hook: None,
            context_curator: None,
            hook_runner: None,
            builtin_selection: None,
            warnings: Vec::new(),
            root: None,
            extra_agent_dirs: Vec::new(),
            extra_skill_dirs: Vec::new(),
        }
    }

    fn with_warnings(mut self, warnings: Vec<ConfigWarning>) -> Self {
        self.warnings = warnings;
        self
    }

    /// Appends one [`ConfigWarning`] to whatever [`Self::from_config`]/
    /// [`Self::discover`]/etc already loaded via the crate-private
    /// `with_warnings` -- the public, additive counterpart that a
    /// plugin-installation step running BEFORE [`Self::build`] (the four
    /// async tiers `main.rs::
    /// build_conway` chains: `first_party_plugins`, `subprocess_plugins`,
    /// `mcp_plugins`, `claude_compat_plugins`) can call to make a non-fatal
    /// discovery problem visible on [`crate::Conway::warnings`] rather than
    /// only on whatever stderr channel that caller happens to also write to.
    ///
    /// **Board item `01M1AMSDE035HAG23TE6XPEF9R`'s own reason to exist**:
    /// an MCP server this host tried to discover -- either an operator-
    /// authored `[plugins].mcp[]` entry or one translated from a
    /// `[plugins].claude_compat[]` directory's own `.mcp.json` -- can fail
    /// for reasons entirely outside conway's control (missing runtime, a
    /// failed first-launch build, a bad path, an upstream bug). An MCP
    /// server contributes tools ONLY (`conway_plugin_mcp::McpPlugin`'s own
    /// `Plugin` impl has no `hooks`/`permission_evaluator` override), so a
    /// server that never came up narrows what the model can call -- it does
    /// not silently drop or misapply a permission rule (the one thing
    /// conway's fail-closed rule for deny/prompt permission rules actually
    /// targets).
    /// That is what makes this the DEGRADE-and-announce shape rather than
    /// the hard `FacadeError::Build` every OTHER discovery failure in those
    /// same four tiers still raises (a directory or file this host cannot
    /// read at all is a different, harder failure this method does not
    /// soften). See `crates/conway-cli/src/claude_compat_plugins.rs`'s own
    /// `install` for the one caller that uses this today.
    pub fn with_warning(mut self, warning: ConfigWarning) -> Self {
        self.warnings.push(warning);
        self
    }

    /// Read-only access to the config this builder currently holds --
    ///'s answer to "how does a caller
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

    /// Registers a [`BackendFactory`] -- a provider-adapter KIND, named up front,
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
    /// call as [`crate::FacadeError::Build`], naming this factory's own
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
    /// **`[backends.<id>].kind` is now an open name** -- for
    /// every `[backends.<id>]` entry, `build()`'s own `resolve_backend_
    /// factory` resolves `entry.kind` against every registered factory's own
    /// [`BackendFactory::id`] -- and against nothing else. The temporary
    /// fallback to two compiled-in adapters is GONE -- this facade no longer links either
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
    /// chose not to attach* ( -- the
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

    /// Read-only access to every plugin injected via [`Self::with_plugin`]
    /// (or [`Self::install_selected`], which calls it internally) SO FAR --
    /// [`Self::config`]'s plugin-side counterpart, added so a caller
    /// composing a `ConwayBuilder` across several steps (as
    /// `crates/conway-cli/src/claude_compat_plugins.rs`'s `install` does,
    /// one `[plugins].claude_compat[]` entry at a time) can inspect what it
    /// has already attached without re-deriving it independently.
    ///
    /// **Does NOT include built-ins.** Those are resolved later, inside
    /// [`Self::build`] itself (`presets::builtin_plugins()`, filtered by
    /// [`PluginSelection`]) -- there is no built-in candidate list to see
    /// before that point, so this can only ever report what a caller
    /// explicitly injected, never conway's own bundled tools.
    pub fn plugins(&self) -> &[Arc<dyn Plugin>] {
        &self.plugins
    }

    /// Read-only access to every [`ConfigWarning`] this builder carries SO
    /// FAR -- both what [`Self::from_config`]/[`Self::discover`]/etc loaded
    /// (headroom-vs-context-window, a stale `[tui]` section) and every one
    /// [`Self::with_warning`] appended since. [`Self::plugins`]'s own
    /// sibling accessor, added for the identical reason: a caller composing
    /// a `ConwayBuilder` across several async plugin-installation steps
    /// (`claude_compat_plugins::install` above all -- board item
    /// `01M1AMSDE035HAG23TE6XPEF9R`) can inspect what it has pushed so far
    /// without waiting for [`Self::build`] to hand back a live [`crate::
    /// Conway`] first.
    pub fn warnings(&self) -> &[ConfigWarning] {
        &self.warnings
    }

    /// Overrides `permissions.mode`-derived gate selection entirely.
    pub fn with_permission_gate(mut self, gate: Arc<dyn PermissionGate>) -> Self {
        self.gate = Some(gate);
        self
    }

    /// Supplies the handler `gates::from_config` needs when `permissions.mode`
    /// resolves to `"prompt"` -- the config default (`config::merge::
    /// default_document`), and therefore what `ConwayBuilder::discover()`
    /// hands a host that changed nothing about permissions.
    ///
    /// **This closes a gap this module's own doc used to disclose rather than
    /// silently paper over**: before this method existed, the ONLY way to
    /// satisfy an unmodified default config's `permissions.mode = "prompt"`
    /// was [`Self::with_permission_gate`] -- which requires implementing the
    /// whole [`PermissionGate`] trait (`check`'s full signature: tool name,
    /// arguments, render kind, scope) just to answer one async question, "may
    /// this tool call proceed?" A host embedding conway to ask ITS OWN user
    /// (a dialog box, a terminal prompt, a chat UI's inline approval) almost
    /// always has exactly that one closure, not a reason to hand-roll a
    /// gate. This method takes it directly: `Arc<dyn Fn(PermissionRequest) ->
    /// BoxFuture<'static, PermissionDecision> + Send + Sync>`
    /// ([`gates::PromptHandler`]), the same handler shape
    /// [`gates::PromptingGate`] has always wrapped -- this method is the
    /// missing builder-level path to it, not a new gate implementation.
    ///
    /// **Precedence: [`Self::with_permission_gate`] wins unconditionally over
    /// this.** If both are called, `build()`'s gate step (9) never even
    /// constructs a `PromptingGate` from this handler -- the injected gate is
    /// used outright, exactly as it always has been when `permissions.mode`
    /// is something this handler is irrelevant to (`"deny"`/`"allowlist"`).
    /// Calling only this method, with `permissions.mode` resolving to
    /// anything other than `"prompt"`, is harmless: the handler is simply
    /// never invoked, since `gates::from_config`'s `"deny"`/`"allowlist"`
    /// arms never read it.
    ///
    /// **Not called at all (the default, unchanged from before this method
    /// existed):** `permissions.mode = "prompt"` with no
    /// `with_permission_gate` override still fails `build()` with a named
    /// [`FacadeError::Config`] stating exactly that ("permissions.mode =
    /// \"prompt\" requires a prompt handler to be supplied") -- never a
    /// silent `AllowAlways`/`DenyAll` substitute. A host that wants the
    /// friendliest default (ask, rather than deny or blanket-allow) to
    /// actually build now has a direct path to it; a host that never calls
    /// this (and never overrides `permissions.mode` some other way) keeps
    /// getting exactly the same named refusal it always has.
    pub fn with_prompt_handler(mut self, handler: gates::PromptHandler) -> Self {
        self.prompt_handler = Some(handler);
        self
    }

    /// Registers a [`ContextHook`]: invoked before every LLM
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

    /// Registers a standalone [`Curator`] -- the pre-assembly selection-layer
    /// curation capability (DESIGN-context-path §11.4). Mirrors
    /// [`Self::with_context_hook`]'s own shape exactly: an embedder with a
    /// standalone curator and no plugin still uses it directly, but a plugin
    /// can ALSO contribute curators through [`Plugin::curators`] on the SAME
    /// `with_plugin`/`install_selected` surface -- no privileged channel a
    /// plugin cannot also reach. `build()` composes this injected curator first, then each
    /// plugin's `curators()` in install order, into the single
    /// `Runtime::set_context_curator` call the runtime reads.
    ///
    /// No call to this method (the default) AND no curating plugin installed
    /// means `build()` never calls `Runtime::set_context_curator` at all --
    /// the pre-assembly stage is a zero-cost pass-through, byte-identical to
    /// a build without this port (the `context_golden` 11/11 gate's
    /// load-bearing guarantee).
    pub fn with_curator(mut self, curator: Arc<dyn Curator>) -> Self {
        self.context_curator = Some(curator);
        self
    }

    /// Registers a [`HookRunner`]:
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
    /// construct that type itself.
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
    /// `conway-cli`'s `build_conway` is the intended caller -- the CLI itself never depends on
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

    /// Overrides the default `FsPathStore` (co-located with the session
    /// store at `config.session.root` when neither this nor
    /// [`Self::with_session_store`]'s implicit default applies).
    ///
    /// **`PathStore` itself is not re-exported through `conway::plugin`**
    /// (board item `01M0EMCK55628YJXGBQY8YGXHE`, decided: engine-internal —
    /// see `conway_core::ports::PathStore`'s own doc for the full
    /// reasoning). This method exists for parity with
    /// [`Self::with_session_store`] and for callers already depending on
    /// `conway-core` directly; a facade-only caller (the intended shape for
    /// a `Tool`/`Curator`/`ContextHook` author) cannot name this method's
    /// parameter type and is not expected to need to.
    pub fn with_path_store(mut self, path_store: Arc<dyn PathStore>) -> Self {
        self.path_store = Some(path_store);
        self
    }

    /// Overrides the default router (:
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

    /// Registers a [`RouterFactory`]:
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
    /// resolver (: `conway` no longer
    /// links a capability-/health-filtering router engine at all; that is
    /// exactly what this method installs). A factory whose `build` returns
    /// `Err` fails the whole `build()` call as `FacadeError::Build`, naming
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

    /// Installs the id-selected subset of three CALLER-SUPPLIED bundles
    /// against `self.config().plugins` in one pass -- the facade's own
    /// version of the ~70-line resolution `crates/conway-cli/src/
    /// first_party_plugins.rs`'s `install` used to hand-roll (///; this method itself lands under
    ///), now reachable by any embedder, not only
    /// this workspace's own CLI binary.
    ///
    /// **The facade still depends on no plugin crate -- `plugins`,
    /// `router_factories`, and `backend_factories` are CALLER-SUPPLIED,
    /// already-constructed values.** This method matches each bundle
    /// entry's own identity (`Plugin::manifest().id`, `RouterFactory::id()`,
    /// `BackendFactory::id()`) against a configured id string; it never maps
    /// an id to a crate itself, and never could -- `crates/conway/Cargo.toml`
    /// names no plugin crate, and the workspace's `no_forbidden_deps`/
    /// architecture guards exist specifically to keep that true (this class
    /// of shortcut -- resolving an id to a crate from inside the facade --
    /// has been tried and reverted before; see `crates/conway-cli/tests/
    /// cli_surface.rs`'s own `no_forbidden_deps` comment for the record of
    /// it). A caller that wants a third-party OR first-party plugin/router/
    /// backend still has to name and construct it and hand it into ONE of
    /// the three `Vec`s below -- this method is a resolution convenience
    /// over [`Self::with_plugin`]/[`Self::with_router_factory`]/
    /// [`Self::with_backend_factory`] (which it calls internally), not a
    /// fourth injection mechanism with different rules.
    ///
    /// **The ids resolved are `self.config().plugins.install` UNIONED with
    /// `self.config().plugins.default_backends`**, deduplicated,
    /// order-preserving (an id present in both appears once, at `install`'s
    /// position) -- see `[plugins].install`'s own doc and
    /// `PluginsConfig::default_backends`'s own doc
    /// ([`crate::config::schema::PluginsConfig`]) for why the union exists:
    /// it is what lets an operator's `[backends.<id>]` entries keep
    /// resolving against a shipped dialect kind id with no `[plugins]`
    /// section in `settings.json` at all, while every other kind of id stays
    /// opt-in through `install` alone.
    ///
    /// **The three installable shapes stay distinct -- never flattened into
    /// one -- and are matched, per id, in this order:**
    /// 1. `Plugin`/`Tool`: an id matching a `plugins` entry's own
    ///    `PluginManifest::id` is installed via [`Self::with_plugin`].
    /// 2. `RouterFactory`: an id matching a `router_factories` entry's own
    ///    `RouterFactory::id()` is installed via
    ///    [`Self::with_router_factory`]. **At most one id may resolve to a
    ///    router factory** -- a build has exactly one router, so a second
    ///    router-factory id is a hard error naming both, never a silent
    ///    "last one wins" -- router SELECTION (naming a kind here) must
    ///    precede router CONSTRUCTION (`build()`'s own later step), which is
    ///    the reason this is a `RouterFactory`, not a `Router`, in the first
    ///    place.
    /// 3. `BackendFactory`: an id matching a `backend_factories` entry's own
    ///    `BackendFactory::id()` is installed via
    ///    [`Self::with_backend_factory`]. No cardinality limit -- a build
    ///    has a SET of backends, not one.
    ///
    /// An id present in more than one bundle under the same string resolves
    /// to whichever of the three comes first in that order.
    ///
    /// **Also calls [`Self::with_declined_backend_kinds`] unconditionally,
    /// before anything else below**, naming every id in `backend_factories`
    /// the resolved id set does NOT select -- purely diagnostic (that
    /// method's own doc): it changes no attach behavior, only which of the
    /// two messages `build()` raises for a `[backends.<id>]` entry naming an
    /// unresolved `kind` later (**declined** vs **unknown**).
    ///
    /// **An id resolving to nothing in any of the three bundles is a hard
    /// [`FacadeError::Config`], never a silent no-op** -- an id in the
    /// config that resolves to nothing is user-facing configuration that
    /// lies (nothing may claim to be reached that isn't). The error names
    /// the offending id and lists every id the three supplied bundles
    /// actually carry, so a caller can tell a typo from a bundle that
    /// genuinely does not include what they named -- matching the
    /// unknown-id diagnosis `first_party_plugins::install` already gave the
    /// CLI, extended to name whichever bundles THIS caller supplied rather
    /// than assuming the CLI's own first-party set.
    ///
    /// **The resolved id set being empty is not itself an error** -- this
    /// method returns `Ok(self)` right after the `with_declined_backend_
    /// kinds` call above, with none of the three bundles consulted further
    /// (an empty `[plugins].install` and an operator-emptied
    /// `[plugins].default_backends` together are a legitimate, if unusual,
    /// configuration). `build()` itself no longer fails on an empty backend
    /// map at all (board item `01M163T1KGX3HTCC2YMDPT655J`) -- a
    /// `[backends.<id>]` entry naming a kind this call's own resolved id set
    /// does not cover is still a hard, named error (the declined/unknown
    /// split above), unrelated to this method's own return.
    ///
    /// **Also validates the `PluginManifest::requires` graph among what
    /// this call can see** (every plugin already on `self`, plus every
    /// `plugins` bundle entry `wanted` selects) -- a cycle there is a hard
    /// [`FacadeError::Build`], since a cycle is unsatisfiable no matter
    /// what `build()` later adds. **This validation is topological; the
    /// `with_plugin` calls below are not** -- they still run in plain
    /// `wanted` order (== `[plugins].install` order), unchanged, because
    /// that order is `Plugin::instructions()`'s own injection-precedence
    /// authority. See `PluginManifest::requires`'s own doc for the full
    /// disclosure of what this method's own visibility can and cannot
    /// check (it cannot see built-ins, so a MISSING required dependency is
    /// deferred to `build()`'s later, full-set pass rather than risking a
    /// false positive here).
    pub fn install_selected(
        mut self,
        plugins: Vec<Arc<dyn Plugin>>,
        router_factories: Vec<Arc<dyn RouterFactory>>,
        backend_factories: Vec<Arc<dyn BackendFactory>>,
    ) -> Result<Self> {
        // [plugins].install UNIONED with [plugins].default_backends,
        // deduplicated, order-preserving -- see this method's own doc.
        let mut seen: HashSet<&str> = HashSet::new();
        let wanted: Vec<String> = self
            .config
            .plugins
            .install
            .iter()
            .chain(self.config.plugins.default_backends.iter())
            .filter(|id| seen.insert(id.as_str()))
            .cloned()
            .collect();

        // every supplied
        // backend-factory id `wanted` does NOT name is a DECLINED kind, not
        // an unknown one -- computed and handed to the builder before the
        // early return below, so the diagnosis is accurate even when
        // `wanted` is empty (declining every supplied dialect at once).
        let declined_backend_kinds: Vec<String> = backend_factories
            .iter()
            .map(|f| f.id().to_string())
            .filter(|id| !wanted.iter().any(|w| w == id))
            .collect();
        self = self.with_declined_backend_kinds(declined_backend_kinds);

        if wanted.is_empty() {
            return Ok(self);
        }

        // Dependency-graph validation (board item
        // `01M0WWJMYK0KDC2X7B7MR46FRR`), topological -- but see
        // `PluginManifest::requires`'s own doc for why this call NEVER
        // reorders the `with_plugin` calls the loop below still makes in
        // plain `wanted` (== `[plugins].install`) order: install order is
        // `Plugin::instructions()`'s own precedence authority, and this
        // step exists to validate the dependency graph, not to choose an
        // injection order. Scoped to what THIS call can see -- every
        // plugin already installed on `self` plus every `plugins` bundle
        // entry `wanted` is about to select -- which does NOT include
        // built-ins (those are resolved later, in `build()`, which is why
        // this step checks a cycle only: a cycle among visible ids is
        // already unsatisfiable no matter what `build()` later adds, but a
        // missing-required id here might yet turn out to be a built-in, so
        // that check is deferred to `build()`'s authoritative, full-set
        // pass (`PluginManifest::requires`'s own doc, "Enforced at
        // ConwayBuilder::build, not at registration order").
        let visible_manifests: Vec<PluginManifest> = self
            .plugins
            .iter()
            .map(|p| p.manifest())
            .chain(
                wanted
                    .iter()
                    .filter_map(|id| plugins.iter().find(|p| &p.manifest().id == id))
                    .map(|p| p.manifest()),
            )
            .collect();
        detect_required_dependency_cycle(&visible_manifests).map_err(|err| FacadeError::Build {
            message: err.to_string(),
        })?;

        let mut router_factory_installed: Option<String> = None;
        for id in &wanted {
            if let Some(plugin) = plugins.iter().find(|p| &p.manifest().id == id) {
                self = self.with_plugin(plugin.clone());
                continue;
            }
            if let Some(factory) = router_factories.iter().find(|f| f.id() == id.as_str()) {
                if let Some(already) = &router_factory_installed {
                    return Err(FacadeError::Config {
                        path: None,
                        message: format!(
                            "plugins.install names more than one router factory ('{already}' \
                             and '{id}'); a build has exactly one router, so at most one \
                             router-factory id may appear in plugins.install."
                        ),
                    });
                }
                router_factory_installed = Some(id.clone());
                self = self.with_router_factory(factory.clone());
                continue;
            }
            if let Some(factory) = backend_factories.iter().find(|f| f.id() == id.as_str()) {
                self = self.with_backend_factory(factory.clone());
                continue;
            }
            let known_plugins: Vec<String> = plugins.iter().map(|p| p.manifest().id).collect();
            let known_routers: Vec<String> = router_factories
                .iter()
                .map(|f| f.id().to_string())
                .collect();
            let known_backends: Vec<String> = backend_factories
                .iter()
                .map(|f| f.id().to_string())
                .collect();
            return Err(FacadeError::Config {
                path: None,
                message: format!(
                    "plugins.install names unknown id '{id}'; linked plugins: [{}]; linked \
                     router factories: [{}]; linked backend factories: [{}]. A plugin, router, \
                     or backend not among these caller-supplied bundles is installed directly, \
                     before build(), via ConwayBuilder::with_plugin/with_router_factory/\
                     with_backend_factory.",
                    known_plugins.join(", "),
                    known_routers.join(", "),
                    known_backends.join(", ")
                ),
            });
        }
        Ok(self)
    }

    /// Sets CLI-sourced overrides, applied (and fully re-validated,
    /// including OAuth-token rejection) at `build()` time.
    pub fn with_cli_overrides(mut self, cli: CliOverrides) -> Self {
        self.cli_overrides = cli;
        self
    }

    /// Sets this `Conway`'s confinement root -- every root agent
    /// [`crate::Conway::new_session`] starts afterward is confined to it.
    ///
    /// **This is the ergonomic surface a prior ruling required stay reachable
    /// through "Retire the harness-level confinement root once
    /// `conway.fs` enforces its own":** a harness-level pre-gate check used
    /// to be the ONLY thing this method's `root` fed; that check is retired.
    /// `with_root` now feeds TWO things from the SAME single, once-resolved,
    /// once-canonicalized `root` value: (1) `conway_runtime::runtime::
    /// RootSpec::root`, unchanged, which still confines the artifact-writer
    /// path (`conway_runtime::artifact_store::AgentArtifactWriter`); and (2)
    /// a derived `conway.fs.root` per-agent plugin-config entry
    /// (`conway_runtime::permission::derive_fs_root_config`, applied inside
    /// `Runtime::start_root`), which is what `conway.fs` itself now reads to
    /// confine `read`/`write`/`edit`/`cd`/`glob`/`grep` -- open-relative,
    /// inside the tool, closing a TOCTOU gap the harness-level check could
    /// not (see `conway_tools::fs::beneath`'s own doc). A spawned child's
    /// `SubagentSpec::root` (`SpawnSpec::root`/`ForkSpec` inheritance)
    /// receives the identical treatment at spawn time
    /// (`conway_runtime::subagent::SubagentHost::start`), so a subtree
    /// confined via this method stays confined for ordinary tool calls at
    /// every depth, not only at the agent an operator directly started.
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
    ///
    /// **`bash` remains outside this boundary entirely** (a different
    /// plugin, with no root-enforcing mechanism `conway.fs`'s relocation
    /// could give it) -- excluding it from the tool set is the actual
    /// guarantee an operator relying on `--root` needs, not this method
    /// alone. See `docs/tools.md` and `docs/plugins/trust-and-security.md`.
    /// This is no longer prose alone: [`Self::build`] itself checks it --
    /// a root set here alongside `bash` (`conway.shell`) among the final
    /// installed tools earns exactly one [`crate::config::ConfigWarning`]
    /// (`crate::config::WarningCode::RootWithUnconfinableTool`) on
    /// [`crate::Conway::warnings()`], surfaced on startup the same way
    /// every other build-time warning is (`conway-cli`'s `diag::warn`, or
    /// the TUI's own transcript).
    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = Some(root.into());
        self
    }

    /// Appends one additional agent-definition root. [`Self::build`] folds
    /// it in AFTER `config.agents.dir` (the operator's own root, which
    /// therefore always wins a name collision against it —
    /// `agents::load_agent_defs_from_roots`'s own precedence rule), resolved
    /// against `cwd` the same way `config.agents.dir` is. Call multiple
    /// times, in the order roots should take precedence over one another,
    /// to add more than one — mirrors [`Self::with_plugin`]'s own
    /// repeat-to-add shape rather than taking a `Vec` up front.
    ///
    /// Deliberately NOT a `ConwayConfig` field — see
    /// [`crate::config::schema::AgentsConfig`]'s own doc for why (the same
    /// blast-radius reasoning [`Self::with_root`]'s own doc gives for that
    /// field). This is the seam a Claude Code compat layer (or any other
    /// embedder) calls to hand a plugin's own `agents/` directory to a real
    /// build, rather than requiring an operator to hand-edit `settings.json`
    /// first.
    pub fn with_extra_agent_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.extra_agent_dirs.push(dir.into());
        self
    }

    /// The skills-side twin of [`Self::with_extra_agent_dir`] — identical
    /// add-order/precedence contract, over
    /// `skills::load_skill_defs_from_roots` instead of the agent-def loader.
    /// [`Self::build`] always reads the fixed `.conway/skills` operator root
    /// first (skills has never had a `dir` config field to override that
    /// default with — see that function's own doc); a root appended here is
    /// folded in after it, and after any earlier-appended extra root, in
    /// call order.
    pub fn with_extra_skill_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.extra_skill_dirs.push(dir.into());
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
            prompt_handler,
            store,
            path_store,
            router,
            router_factory,
            backend_factories,
            declined_backend_kinds,
            context_hook,
            context_curator,
            hook_runner,
            builtin_selection,
            warnings,
            root,
            extra_agent_dirs,
            extra_skill_dirs,
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

        // 2b. Declarative provider profiles:
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

        // 2c. Session discovery (board item `01M0PS8J3AK7Z7253Z3E3RD3GY`):
        //     the SAME `env` immediately above, reused (not re-read) for the
        //     identical central-config-directory resolution `session.root`'s
        //     own central-default branch performs -- pure, no I/O, just
        //     naming where the central sessions root WOULD be. Step 8 below
        //     builds the real `FsSessionDiscoveryHost` from these once
        //     `store` exists.
        let discovery_project_key =
            config::discovery::encode_project_key(&config::discovery::normalize_lexically(&cwd));
        let discovery_central_root = config::discovery::user_config_path(&env)
            .and_then(|p| p.parent().map(|d| d.join("sessions")));

        // 3+3b+4. Duplicate-kind check over every registered factory FIRST
        //         (before any factory's own `build` runs, regardless of
        //         whether a `[backends.<id>]` entry ever names it -- a
        //         dedicated pass, not "insert-then-error-on-the-second-one",
        //         so a duplicate never leaves an earlier factory's `build`
        //         side effects to have run while the whole call still
        //         fails). Then construct one backend per `[backends.<id>]`
        //         entry, resolving `entry.kind` against the registered
        //         factories ONLY (
        //         removed the temporary compiled-in fallback
        // left standing -- see
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
                return Err(FacadeError::Build {
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
            let ctx = build_backend_context(id, entry, &metadata, &profile_file_paths);
            if config.models.probe_on_startup {
                probe_targets.push((id.clone(), factory.clone(), ctx.clone()));
            }
            let backend = factory.build(ctx).map_err(|e| FacadeError::Build {
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
        // Board item `01M163T1KGX3HTCC2YMDPT655J`: an empty `backend_map`
        // here -- no `[backends.<id>]` entry at all, and no `with_backend`
        // injection -- is no longer a hard error. Everything downstream
        // already tolerates it: `CapabilityIndex::from_backends` is
        // `.get()`-based over an empty map, `MinimalRouter`/
        // `DeclarativeRouter` both return a typed `RoutingError::
        // NoCandidate`/`UnknownRole` rather than panicking when a role's
        // chain has nothing to offer, and `AttemptEngine::execute` already
        // has a dedicated (previously unreachable in production, per its
        // own comment) `NoCandidate` arm for exactly an empty `req.routes`.
        // The built-in default config's own `roles.default.chain = []`
        // means an unmodified default with zero configured backends now
        // reaches that same named, typed failure the moment a turn is
        // attempted, instead of refusing to start at all -- see
        // `crates/conway-cli/src/first_run.rs`'s guided-setup decline path
        // for why leaving the app open is the point: "no thanks, I'll
        // configure it later" must be a real option. `ConwayBuilder::
        // with_backend`/`.with_backend_factory` remain exactly as useful as
        // before for a caller that DOES want one wired in; this removal
        // only stops `build()` from insisting on it.

        // 5. CapabilityIndex, read directly from each constructed backend's
        //    own `Backend::capabilities()` (the single accessor this
        //    index and the runtime's T-1 gate both read — see
        //    `CapabilityIndex::from_backends`'s doc) for every
        //    `(backend, model)` pair `models.json` declares. Optionally
        //    overlaid with a startup probe
        // relocated the probing mechanism
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

        // 6. Resolve routing/headroom config.
        //: `conway` itself no longer links a
        //    circuit-breaker implementation (that engine moved to the
        //    `conway-plugin-routing` first-party plugin), so the default
        //    `HealthRegistry` -- absent an installed router factory -- is
        //    the honestly degenerate `AlwaysClosedHealthRegistry`: no
        //    breaker ever opens, `record` is a no-op. A factory's own
        //    `RouterBundle::health` REPLACES this below when one is taken.
        let routing_config = config.routing().map_err(|message| FacadeError::Config {
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
        //    item: this replaces the
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
            factory.build(ctx).map_err(|e| FacadeError::Build {
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
        //    feature), else a Build error. `config.session.root` is `Some`
        //    for any config that reached here through `config::load`/
        //    `load_ignoring_user_config` -- the central-default resolution
        //    (board item `01M0QK9GRM8HSNWRAR414TCX42`) happens THERE, using
        //    the load-scoped `env`/`cwd` this function has no seam of its
        //    own to receive (see `SessionConfig`'s own doc). A config
        //    assembled directly via `ConwayBuilder::from_parts`, bypassing
        //    `load` entirely, can still reach here with `root` unresolved
        //    (`None`) -- rather than reading THIS PROCESS's ambient
        //    environment here too (a strictly larger blast radius than the
        //    already-disclosed ambient read three steps up,
        //    `provider_profile_file_paths`: that one only ever looks for an
        //    optional file, this one would go on to CREATE a directory),
        //    an unresolved `root` falls back to the exact fixed default
        //    `session.root` always had before this item existed,
        //    `.conway/sessions` relative to `cwd` -- byte-identical
        //    behavior for every existing `from_parts` caller (this crate's
        //    own test suite included) that never named a `session.root` of
        //    its own.
        let effective_session_root = config
            .session
            .root
            .clone()
            .unwrap_or_else(|| Path::new(".conway/sessions").to_path_buf());
        let store: Arc<dyn SessionStore> = match store {
            Some(store) => store,
            None => build_default_store(&cwd, &effective_session_root)?,
        };
        // 8a2. Session discovery (board item `01M0PS8J3AK7Z7253Z3E3RD3GY`):
        //      built from `store` above (whichever it is -- injected or
        //      the default just constructed) plus the pre-resolved
        //      project key/central root from step 2c. Not injectable via a
        //      builder method the way `store`/`path_store` are: nothing in
        //      this crate's public surface names `SessionDiscoveryHost`
        //      (T4 keeps it out of `conway-runtime`, and this facade has
        //      not yet had a reason to widen `with_*` to it) -- an embedder
        //      needing a different discovery implementation depends on
        //      `conway-core`/`conway-runtime` directly and builds a
        //      `RuntimeDeps` of their own, the same escape hatch every
        //      facade-only limitation here has.
        let session_discovery: Arc<dyn conway_core::ports::SessionDiscoveryHost> =
            Arc::new(discovery_host::FsSessionDiscoveryHost::new(
                store.clone(),
                discovery_project_key,
                discovery_central_root,
            ));
        // 8b. Path store: injected, else `FsPathStore::open`, co-located as
        //     a SIBLING of the effective session root the session store
        //     just resolved against -- see `build_default_path_store`'s own
        //     doc for exactly where (a sibling of the root itself, not of
        //     its parent, since this item's central default nests the root
        //     one level deeper than the fixed default/an explicit value
        //     ever did).
        let path_store: Arc<dyn PathStore> = match path_store {
            Some(path_store) => path_store,
            None => build_default_path_store(&cwd, &effective_session_root)?,
        };

        // 9. Gate: injected, else selected from config.permissions --
        //    `prompt_handler` (Self::with_prompt_handler) is what lets a
        //    "permissions.mode = prompt" config (the default) build at all
        //    without an injected gate; see that method's own doc for the
        //    precedence between the two.
        let gate: Arc<dyn PermissionGate> = match gate {
            Some(gate) => gate,
            None => gates::from_config(&config.permissions, prompt_handler)?,
        };

        // 10. Plugins: built-ins (filtered by `selection`) ++ injected;
        //     duplicate manifest ids error. bash ships on by
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
                return Err(FacadeError::Build {
                    message: format!("duplicate plugin id: '{id}'"),
                });
            }
            resolved_plugins.push(plugin);
        }

        // 10a. Host-capability gate (board item
        //      `01M03VJXARFHSDAGHFXGCWKJTY`): each installed plugin's
        //      `PluginManifest::required_host_caps` is compared against what
        //      THIS host offers (`HostCaps::from_config`), right where the
        //      duplicate-plugin-id check above already runs -- the manifest-
        //      validation seam. A cap the host does NOT offer is a
        //      `PluginError::MissingHostCapability` naming both the plugin
        //      and the cap, surfaced as a build error (mirroring the
        //      duplicate-id error's `FacadeError::Build` shape one step above).
        //      Empty `required_host_caps` (the common case -- "needs nothing
        //      the host might lack") is always satisfied. The check lives in
        //      the builder, NOT in `PluginRegistry::from_plugins`, so
        //      `conway-core`'s surface is unchanged.
        //
        //      `PluginManifest::optional_host_caps` (board item
        //      `01M0WWKA8K1E7JPK87J6RRQMZF`, that field's own doc) rides
        //      the SAME per-plugin loop, right after the mandatory check:
        //      a cap this host does NOT offer never fails the build -- the
        //      plugin loads degraded, and the degradation is announced on
        //      the SAME two channels `10a2`'s missing-optional-DEPENDENCY
        //      loop below uses for the identical idea one edge over
        //      (`tracing::warn!` plus a `ConfigWarning`).
        let host_caps = HostCaps::from_config(&config);
        let mut warnings = warnings;
        for plugin in &resolved_plugins {
            let manifest = plugin.manifest();
            host_caps
                .check_manifest(&manifest)
                .map_err(|err| FacadeError::Build {
                    message: err.to_string(),
                })?;
            for cap in host_caps.missing_optional(&manifest) {
                tracing::warn!(
                    plugin = %manifest.id,
                    capability = %cap,
                    "plugin's optional host capability is not offered by this host; loading \
                     degraded"
                );
                warnings.push(ConfigWarning {
                    code: WarningCode::OptionalHostCapabilityMissing,
                    message: format!(
                        "plugin '{}' optionally uses host capability '{cap}', which this host \
                         does not offer; '{}' will load degraded",
                        manifest.id, manifest.id
                    ),
                });
            }
        }

        // 10a2. Plugin-to-plugin dependency gate (board item
        //       `01M0WWJMYK0KDC2X7B7MR46FRR`, `PluginManifest::requires`/
        //       `::optional`'s own docs): the authoritative pass, run here
        //       (not inside `install_selected`) because `resolved_plugins`
        //       is the FIRST point with the FULL final installed set --
        //       built-ins ++ everything `install_selected`/`with_plugin`
        //       added -- in view; `install_selected`'s own earlier cycle
        //       check (its own doc explains why) cannot see built-ins at
        //       all. Required-edge cycle first (structurally unsatisfiable
        //       regardless of what is or isn't missing), then presence:
        //       a missing REQUIRED dependency is a hard `FacadeError::
        //       Build` naming both the dependent and the missing id
        //       (mirroring the host-capability gate immediately above); a
        //       missing OPTIONAL dependency never fails the build -- the
        //       dependent loads degraded, and the degradation is announced
        //       on two channels so no host is left with no way to notice
        //       it (`tracing::warn!`, for a host with no reason to read
        //       `Conway::warnings()` at all, plus a `ConfigWarning` on that
        //       same accessor for a host that does).
        //
        //       Extended by board item `01M0WWNHQQYN1EVTH8WPZ33EBF` (Edge B's
        //       capability CALL channel, `conway_core::ports::capability`'s
        //       own module doc) to union each `requires`/`optional` entry
        //       against `provided_caps` -- the capability names some
        //       installed plugin's `Plugin::capabilities()` registers a
        //       runtime provider for -- alongside the plugin-id set already
        //       checked here, "one vocabulary, not two" applied to the SAME
        //       `requires`/`optional` fields rather than a second, parallel
        //       pair of capability-only lists.
        let installed_manifests: Vec<PluginManifest> =
            resolved_plugins.iter().map(|p| p.manifest()).collect();
        let provided_caps = provided_capability_names(&resolved_plugins);
        detect_required_dependency_cycle(&installed_manifests).map_err(|err| {
            FacadeError::Build {
                message: err.to_string(),
            }
        })?;
        missing_required_dependency(&installed_manifests, &provided_caps).map_err(|err| {
            FacadeError::Build {
                message: err.to_string(),
            }
        })?;
        // `warnings` was already rebound `mut` above, at the host-capability
        // gate (10a), so the optional-DEPENDENCY loop below can push onto
        // the SAME `Vec` the optional-host-capability loop above already
        // does.
        for (plugin, dependency) in
            missing_optional_dependencies(&installed_manifests, &provided_caps)
        {
            tracing::warn!(
                plugin = %plugin,
                dependency = %dependency,
                "plugin's optional dependency is not installed; loading degraded"
            );
            warnings.push(ConfigWarning {
                code: WarningCode::OptionalPluginDependencyMissing,
                message: format!(
                    "plugin '{plugin}' optionally depends on '{dependency}', which is not \
                     installed; '{plugin}' will load degraded"
                ),
            });
        }

        // 10a2b. Board item (harness gap review 2026-09-01, finding 10):
        //        `with_root`/`--root` confines PATH ARGUMENTS only. A tool
        //        whose `Tool::path_args` declares `PathArgs::Unconfinable`
        //        AND whose `Tool::render_kind` declares
        //        `RenderKind::ShellCommand` hands its call straight to a
        //        shell, which can reach any path the root would otherwise
        //        confine (`bash`'s own `path_args` doc pairs exactly this
        //        pair of facts, for exactly this reason). Computed here
        //        from that STRUCTURAL pair on each installed tool -- never
        //        `if manifest.id == "conway.shell"` or a bare-name check on
        //        `bash` -- so a future tool making the identical two claims
        //        about itself earns the identical warning with no edit to
        //        this call site (safety is a mechanism, not an opinion).
        //        `report` also declares `Unconfinable` (its artifact path
        //        is nested, outside `PathArgs::Named`'s vocabulary) but is
        //        excluded here for the same structural reason, not a name
        //        check either: its `render_kind` is `Structured`, never a
        //        shell command (`report`'s own `path_args`/`render_kind`
        //        docs). Runs after `resolved_plugins` is the FULL final
        //        installed set (10a2's own comment), so a plugin
        //        `with_plugin`-injected after `with_builtin_plugins` is
        //        seen exactly like a built-in.
        if root.is_some() {
            let unconfinable_shell_tool = resolved_plugins.iter().find_map(|plugin| {
                let manifest = plugin.manifest();
                plugin.tools().into_iter().find_map(|tool| {
                    let unconfinable = matches!(tool.path_args(), PathArgs::Unconfinable { .. });
                    let shell_command = tool.render_kind() == RenderKind::ShellCommand;
                    if unconfinable && shell_command {
                        Some((tool.spec().name.as_str().to_string(), manifest.id.clone()))
                    } else {
                        None
                    }
                })
            });
            if let Some((tool_name, plugin_id)) = unconfinable_shell_tool {
                warnings.push(ConfigWarning {
                    code: WarningCode::RootWithUnconfinableTool,
                    message: format!(
                        "--root confines path arguments, but tool {tool_name} ({plugin_id}) \
                         runs shell commands the root cannot confine; remove {plugin_id} from \
                         tools.builtin_plugins for a real boundary"
                    ),
                });
            }
        }

        // 10a3. The runtime CALL half of Edge B (board item
        //       `01M0XXWV3BVDM6Y646WMEBTYT1`; `conway_core::ports::capability`'s
        //       own module doc): build the REAL `CapabilityRegistry`, ONCE,
        //       here, from the SAME `Plugin::capabilities()` registrations
        //       every installed plugin offers -- the runtime counterpart of
        //       `provided_caps` immediately above, which only kept the
        //       capability NAMES for the static requires/optional check and
        //       discarded the providers themselves. Paired with the
        //       declaring plugin's own id (`capability_owners`) so a
        //       duplicate-provider refusal below can name BOTH offending
        //       plugins, not just the capability name
        //       `DuplicateCapabilityProvider` itself carries -- `manifest.id`
        //       for every plugin was already computed once, above, as
        //       `installed_manifests`, but that Vec is not keyed by plugin,
        //       so this re-derives the id per plugin directly from
        //       `resolved_plugins` rather than re-zipping the two Vecs.
        let capability_registrations: Vec<(String, CapabilityRegistration)> = resolved_plugins
            .iter()
            .flat_map(|p| {
                let plugin_id = p.manifest().id;
                p.capabilities()
                    .into_iter()
                    .map(move |registration| (plugin_id.clone(), registration))
            })
            .collect();
        let capability_owners: Vec<(String, String)> = capability_registrations
            .iter()
            .map(|(plugin_id, registration)| {
                (
                    plugin_id.clone(),
                    registration.capability.as_wire_str().to_string(),
                )
            })
            .collect();
        // The refusal `CapabilityRegistry::from_registrations` returns on a
        // duplicate provider MUST reach `build()` as a real error -- an
        // `.unwrap_or_default()` or an ignored `Err` here would silently
        // resolve to one arbitrary provider, which is worse than the no-op
        // this item replaces (see that method's own doc: fail closed, never
        // "last one wins").
        let capability_registry = CapabilityRegistry::from_registrations(
            capability_registrations
                .into_iter()
                .map(|(_, registration)| registration),
        )
        .map_err(|dup| {
            let mut owners: Vec<&str> = capability_owners
                .iter()
                .filter(|(_, capability)| capability == &dup.capability)
                .map(|(plugin_id, _)| plugin_id.as_str())
                .collect();
            owners.sort_unstable();
            owners.dedup();
            FacadeError::Build {
                message: format!(
                    "capability '{}' has more than one provider: {}",
                    dup.capability,
                    owners.join(", ")
                ),
            }
        })?;

        // 10b. Every installed plugin's own declared custom events (board
        //      item, `PHILOSOPHY.md` §5's open
        //      vocabulary: "A plugin declares the events it emits...
        //      Those events sit at the same level as the ones conway
        //      emits") -- namespaced and validated
        //      (`conway_runtime::hook_dispatch::declared_plugin_events`,
        //      the SAME shared `validate_event_name` the [hooks] event-
        //      shape check above already uses). Computed here, borrowing
        //      `resolved_plugins`, BEFORE it is moved into `RuntimeDeps`
        //      below. A malformed declaration (an empty bare name, or two
        //      events landing on the same full name) is a build-time error
        //      naming the offender -- "an event a plugin declares and
        //      never fires is the same defect as a tool that does
        //      nothing" starts with the declaration itself being
        //      well-formed.
        let plugin_events = declared_plugin_events(&resolved_plugins)
            .map_err(|message| FacadeError::Build { message })?;

        // 11. Agent defs. `AgentsConfig::dir` is the operator's own root
        // (strict: a malformed file here is a loud build error, unchanged
        // from before multi-root support existed); `extra_agent_dirs` (this
        // builder's own field, destructured from `self` above -- board item
        // `01M0X1EH2GW5DKY9XD1EZ78S3F` first added a config-field version of
        // this, `01M0XRE2N96ATHEXJ1617E133P` moved it here -- see
        // `crate::config::schema::AgentsConfig`'s own doc for why) is zero
        // or more ADDITIONAL roots, each resolved against the same
        // `cwd`, that shadow-lose to `dir` and to each other in call order
        // on a name collision, and whose own malformed files are skipped
        // rather than failing the build -- see
        // `agents::load_agent_defs_from_roots`'s own doc for the exact
        // contract. Nothing calls `with_extra_agent_dir` in this crate
        // itself yet: wiring a Claude Code compat plugin's own directories
        // into it is a sibling item's job.
        let agents_dir = resolve_path(&cwd, &config.agents.dir);
        let mut agent_roots = vec![agents_dir];
        agent_roots.extend(extra_agent_dirs.iter().map(|dir| resolve_path(&cwd, dir)));
        let agent_defs = agents::load_agent_defs_from_roots(&agent_roots)?;

        // 11b. Skill defs (board item `01M03GKZ3MGZK3ETP6R27E2M9Y` produced
        // the loader; `01M0XRE2N96ATHEXJ1617E133P` wired it to a caller).
        // No `[skills]` config section exists (or is needed) -- unlike
        // `AgentsConfig::dir`, skill *selection* is already fully
        // established by `AgentDef.skills`' name list, so a configurable
        // directory would add config surface neither loader needs (see
        // `AgentsConfig`'s own doc: THIS is the reason `extra_agent_dirs`
        // moved off `ConwayConfig` too, so both loaders end symmetric).
        // `.conway/skills`, resolved against the same `cwd` as every other
        // `.conway/`-relative path here, mirrors `AgentsConfig::dir`'s own
        // default (`.conway/agents`) and `docs/vision/CATALOGUE.md` entry
        // 2's proposed layout, and is always the first (operator-own,
        // strict) root -- exactly like `agents_dir` above.
        // `extra_skill_dirs` (this builder's own field, destructured from
        // `self` above) is the skills-side twin of `extra_agent_dirs`: zero
        // or more ADDITIONAL roots, in call order,
        // each resolved against `cwd`, shadow-losing to the operator's own
        // root and to each other on a name collision, with malformed files
        // in them skipped rather than failing the build -- see
        // `skills::load_skill_defs_from_roots`'s own doc for the exact
        // contract. Nothing calls `with_extra_skill_dir` in this crate
        // itself yet either, for the same reason `agent_roots` above names.
        let skills_dir = resolve_path(&cwd, Path::new(".conway/skills"));
        let mut skill_roots = vec![skills_dir];
        skill_roots.extend(extra_skill_dirs.iter().map(|dir| resolve_path(&cwd, dir)));
        let skill_defs = skills::load_skill_defs_from_roots(&skill_roots)?;

        // 12. Runtime::new.
        //
        // Collect each installed plugin's own `Plugin::context_hooks()`
        // contributions BEFORE `resolved_plugins` is moved into
        // `RuntimeDeps` below. Composed with any
        // `with_context_hook`-injected hook after construction -- see the
        // `set_context_hook` call below for the composition order.
        let plugin_context_hooks: Vec<Arc<dyn ContextHook>> = resolved_plugins
            .iter()
            .flat_map(|p| p.context_hooks())
            .collect();
        // Collect each installed plugin's own `Plugin::curators()`
        // contributions BEFORE `resolved_plugins` is moved into `RuntimeDeps`
        // below -- the SAME collect-before-move, install-after-construct
        // shape `plugin_context_hooks` immediately above establishes for
        // context hooks. Composed with any `with_curator`-injected curator
        // after construction -- see the `set_context_curator` call below.
        let plugin_curators: Vec<Arc<dyn Curator>> =
            resolved_plugins.iter().flat_map(|p| p.curators()).collect();
        // Collect each installed plugin's own `Plugin::permission_rules()`
        // contributions BEFORE `resolved_plugins` is moved into `RuntimeDeps`
        // below (board item `01M03VKJG7JJ0JEKY265WA7MJ7`). Installed into the
        // broker as `PatternOrigin::Plugin` deny/prompt rules AFTER
        // `Runtime::new` -- the SAME collect-before-move, install-after-construct
        // shape `plugin_context_hooks` immediately above establishes for
        // context hooks. Narrowing-only by type construction
        // (`PluginPermissionVerdict` has no `Allow` variant), so a plugin
        // can never widen what the operator authorized; the operator's own
        // `permissions.json`/`PermissionMode` STILL wins (a plugin `Deny` is
        // checked at the same deny tier; a plugin `Prompt` forces the gate
        // but the operator's `Deny`/plan-mode refusal fires first).
        let plugin_permission_rules: Vec<PluginPermissionRule> = resolved_plugins
            .iter()
            .flat_map(|p| p.permission_rules())
            .collect();
        // Collect each installed plugin's own `Plugin::hooks()` contributions
        // BEFORE `resolved_plugins` is moved into `RuntimeDeps` below (board
        // item `01M129QW0GV90QTQS6B3BY3DAR` -- the seam `ConwayBuilder::
        // config_mut`'s own doc named as missing: a plugin registers a hook
        // rule the SAME way it registers a tool, rather than reaching for a
        // whole-config escape hatch). The SAME collect-before-move shape
        // `plugin_permission_rules` immediately above establishes -- paired
        // with its declaring plugin's own manifest id, both for the
        // namespacing and for the provenance attribution below.
        let plugin_hook_rules: Vec<(String, PluginHookRule)> = resolved_plugins
            .iter()
            .flat_map(|p| {
                let plugin_id = p.manifest().id;
                p.hooks()
                    .into_iter()
                    .map(move |rule| (plugin_id.clone(), rule))
            })
            .collect();
        // Fold `plugin_hook_rules` in beside `config.hooks.rules` into ONE
        // combined, ORIGIN-TAGGED list -- the single source both the
        // `pre_tool_use_specs` and `observation_specs` steps below read,
        // rather than each re-implementing its own "config rules plus
        // plugin rules" merge (P-14: one implementation of the classification
        // logic, not two that could drift). Every config-declared rule is
        // tagged [`HookOrigin::Operator`], unchanged from every hook rule
        // that existed before this item; every plugin-declared rule is
        // tagged [`HookOrigin::Plugin`] naming its declaring plugin, and its
        // bare `id` is host-prefixed with that plugin's own manifest id --
        // this item's own decided answer to "should provenance be
        // structural": an author never picks their own namespace, the SAME
        // rule `declared_plugin_events`/`CommandRegistry::build` already
        // enforce for event/command names -- so a plugin can never claim an
        // id an operator might also have written, and the resulting id is
        // what makes a plugin-registered hook distinguishable from an
        // operator-authored one wherever `id` is later read (a denial
        // message, `Conway::active_deny_capable_hook_rules`'s review list).
        //
        // A collision -- an empty bare id, or a namespaced id already taken
        // by a `[hooks].rules[]` entry or another plugin's own hook -- is a
        // hard `FacadeError::Build` naming the offender, mirroring the
        // duplicate-plugin-id / duplicate-instruction-fragment-name checks
        // above: an ambiguous hook id is not a cosmetic problem, it is
        // exactly the "which rule does 'foo' refer to" ambiguity check 9 of
        // `config::merge::validate`'s own hooks check already refuses to
        // load for `[hooks].rules[]` alone.
        let mut seen_hook_ids: HashSet<String> =
            config.hooks.rules.iter().map(|r| r.id.clone()).collect();
        // The third element is `PluginHookRule::spawn_only` (board item
        // `01M129Y98V4C1050QBPPMY37X0`) -- carried BESIDE `HookEntry` rather
        // than folded into it: that field has no `HookEntry` counterpart on
        // purpose (see its own doc), so `HookEntry` itself, and every
        // operator-facing surface built on it (`[hooks].rules[]` TOML,
        // `config::merge::validate`), stays exactly as it was. Every
        // `[hooks].rules[]` entry gets `false` here -- an operator has no
        // way to set this field at all, unchanged from before it existed.
        let mut effective_hook_rules: Vec<(HookOrigin, HookEntry, bool)> = config
            .hooks
            .rules
            .iter()
            .cloned()
            .map(|rule| (HookOrigin::Operator, rule, false))
            .collect();
        for (plugin_id, rule) in plugin_hook_rules {
            if rule.id.is_empty() {
                return Err(FacadeError::Build {
                    message: format!(
                        "plugin '{plugin_id}' registered a Plugin::hooks() rule with an empty \
                         id; every hook rule must have a non-empty id"
                    ),
                });
            }
            let namespaced_id = format!("{plugin_id}{EVENT_NAMESPACE_SEPARATOR}{}", rule.id);
            if !seen_hook_ids.insert(namespaced_id.clone()) {
                return Err(FacadeError::Build {
                    message: format!(
                        "duplicate hook id '{namespaced_id}': plugin '{plugin_id}' registered a \
                         Plugin::hooks() rule whose namespaced id collides with an existing \
                         [hooks].rules[] entry or another plugin's own hook -- rename one of them"
                    ),
                });
            }
            let spawn_only = rule.spawn_only;
            effective_hook_rules.push((
                HookOrigin::Plugin(plugin_id),
                HookEntry {
                    id: namespaced_id,
                    event: rule.event,
                    match_tool: rule.match_tool,
                    command: rule.command,
                    timeout_ms: rule.timeout_ms,
                    enabled: rule.enabled,
                    on_failure: rule.on_failure,
                },
                spawn_only,
            ));
        }
        // Collect each installed plugin's own `Plugin::instructions()`
        // contributions BEFORE `resolved_plugins` is moved into `RuntimeDeps`
        // below (board item `01M0K5MD59YZRSHE31JKZKFRMY`) -- the SAME
        // collect-before-move shape `plugin_context_hooks`/`plugin_curators`/
        // `plugin_permission_rules` establish above. Each fragment is paired
        // with its declaring plugin's own `PluginManifest::id` here and
        // nowhere else (the SAME "an author never picks their own
        // namespace" attribution `Runtime::new`'s `observers` collection
        // already performs for `Plugin::observers()`).
        //
        // Only ONE check happens here, and it is deliberately NOT the
        // reachability check: a duplicate fragment `name` across every
        // installed plugin is a plain authoring bug -- unlike reachability,
        // it does not depend on what `plugins.install` resolved to for THIS
        // operator (the SAME set of names is either unique or is not,
        // regardless of which tools happen to be installed), so it is
        // exactly the kind of build-time, configuration-INDEPENDENT fact
        // this method already refuses to build on (mirrors the duplicate-
        // plugin-id check above). Reachability itself is checked once per
        // turn, in `conway_runtime::context::builder::ContextBuilder::build`,
        // against that turn's own resolved tool set -- see
        // `conway_core::ports::plugin::Plugin::instructions`'s own doc for
        // the full argument for why it lives there instead of here or in CI.
        // Keyed by fragment name -> the id of the plugin that declared it
        // FIRST, rather than a bare set of names: resolving a collision means
        // editing one of the two declarations, so the operator needs BOTH
        // ids. A set can only name the plugin being processed when the clash
        // is detected, leaving the other side as "some earlier plugin" for
        // the reader to hunt down by hand.
        let mut seen_instruction_names: HashMap<String, String> = HashMap::new();
        let mut plugin_instructions: Vec<PluginInstruction> = Vec::new();
        for plugin in &resolved_plugins {
            let plugin_id = plugin.manifest().id;
            for fragment in plugin.instructions() {
                if let Some(first_plugin_id) = seen_instruction_names.get(&fragment.name).cloned() {
                    return Err(FacadeError::Build {
                        message: format!(
                            "duplicate instruction fragment name '{}': plugins '{first_plugin_id}' \
                             and '{plugin_id}' both declare a Plugin::instructions() fragment with \
                             this name. Fragment names are global -- rename one of them",
                            fragment.name
                        ),
                    });
                }
                seen_instruction_names.insert(fragment.name.clone(), plugin_id.clone());
                plugin_instructions.push(PluginInstruction {
                    plugin_id: plugin_id.clone(),
                    name: fragment.name,
                    text: fragment.text,
                    tool_ids: fragment.tool_ids,
                });
            }
        }
        // Collect each installed plugin's own `Plugin::status_contributions()`
        // and `Plugin::observe_sink()` contributions BEFORE `resolved_plugins`
        // is moved into `RuntimeDeps` below (board item
        // `01M03VKQ738DTGHHK2C4RWXC0E`). The status contributions are a
        // build-time SNAPSHOT (collected at session-open, before any
        // `status/1` notifications have arrived -- typically empty); kept
        // for `Conway::plugin_status_contributions` exactly as before. This
        // snapshot is no longer the ONLY reachable record, though --
        // `live_plugins` immediately below is the live one (board item
        // `01M0Y3A8MYKKE0GMYKZE1K0QTD`, see that field's own doc). The
        // observe sinks are installed as `EventBus` subscribers: one forwarding
        // task per sink drives a `bus.subscribe()` stream and calls
        // `sink.emit(envelope.event)` for each envelope, so a persistent
        // subprocess plugin that engaged `observe/1` receives matching `Event`s
        // as notifications on its stdin. Lossy-with-notice by construction
        // (the bus's broadcast buffer drops for a slow subscriber, surfacing
        // `Event::Lagged`; the sink's own bounded channel drops+warns too).
        let plugin_status_contributions: Vec<PluginStatusContribution> = resolved_plugins
            .iter()
            .flat_map(|p| p.status_contributions())
            .collect();
        // Board item `01M0Y3A8MYKKE0GMYKZE1K0QTD`: a LIVE handle to every
        // installed plugin, retained so a caller can re-invoke
        // `Plugin::status_contributions()` after `build()` returns and see
        // whatever a plugin's own background refresh loop has produced
        // since -- the missing piece `DESIGN-plugin-dependencies.md` §7c
        // was left open on (see that section's own revision entry for the
        // argument). `Arc::clone` per element (a refcount bump, not a deep
        // copy) -- `resolved_plugins` itself is moved into `RuntimeDeps.
        // plugins` a few lines down and consumed there:
        // `PluginRegistry::from_plugins` extracts each plugin's `Tool`s and
        // manifest id but drops the `Arc<dyn Plugin>` handle itself at the
        // end of its own construction loop, so without THIS clone, take
        // now, there would be nothing left anywhere to poll -- the snapshot
        // above is a `Vec<PluginStatusContribution>`, not a handle, and
        // cannot substitute.
        //
        // Deliberately NOT threaded through `RuntimeDeps` the way the
        // capability registry (`RuntimeDeps::capabilities`) is: that
        // channel is reached from deep inside a tool call's own dispatch
        // path (`conway_runtime::agent_loop`'s `LoopDeps`, `conway_runtime::
        // tools::runner`'s `ToolBatchCtx`) because a capability CALL
        // genuinely happens mid-turn, synchronously with tool execution.
        // A status-line poll has no such need -- its only consumer is the
        // TUI's own render loop, which already holds a `Conway` clone
        // directly (`conway-cli`'s `App`) -- so this rides the SAME facade
        // surface `plugin_status_contributions` above already does, as a
        // sibling field, at zero cost to `RuntimeDeps`/`LoopDeps`/
        // `ToolBatchCtx` and every one of their existing construction
        // sites.
        let live_plugins: Vec<Arc<dyn Plugin>> = resolved_plugins.clone();
        // Collected here, beside the status snapshot above, for the same
        // reason and at the same moment: `PluginRegistry` consumes
        // `resolved_plugins` a few lines down, so this is the last point at
        // which `Plugin::permission_modes()` is reachable at all. Each entry
        // is paired with its declaring plugin's manifest id, because a name
        // collision between two plugins must be reported naming BOTH of them
        // (`ModeCycle::build`'s own contract) and the id is not recoverable
        // from the `PluginDeclaredMode` afterwards.
        let declared_permission_modes: Vec<(String, conway_core::ports::PluginDeclaredMode)> =
            resolved_plugins
                .iter()
                .flat_map(|p| {
                    let plugin_id = p.manifest().id;
                    p.permission_modes()
                        .into_iter()
                        .map(move |mode| (plugin_id.clone(), mode))
                })
                .collect();
        let observe_sinks: Vec<conway_core::ports::EventSinkHandle> = resolved_plugins
            .iter()
            .filter_map(|p| p.observe_sink())
            .collect();
        let event_bus = EventBus::new(EVENT_BUS_CAPACITY);
        // Spawn one forwarding task per observe sink BEFORE the `event_bus` Arc
        // is moved into `RuntimeDeps` (a clone is taken for the task(s)). The
        // tasks are spawned on the CURRENT tokio runtime if one is running
        // (`Handle::try_current`); if `build()` is called outside a runtime
        // (a library embedder not yet inside `tokio::main`), the forwarding is
        // SKIPPED -- the plugins load normally and serve `tool/1`, but receive
        // no `observe/1` notifications. That is an honest degradation, not a
        // panic: `tokio::spawn` would panic outside a runtime, so the guard
        // avoids forcing a runtime on every embedder. The CLI and every
        // `#[tokio::test]` run inside a runtime, so the common path engages.
        // The spawned tasks are DETACHED (their `JoinHandle`s are dropped).
        // Each task DROPS its cloned `Arc<EventBus>` ref right after
        // `subscribe()` (the returned `EventStream` holds only a
        // `broadcast::Receiver`, not a borrow of the `Arc`), so the task does
        // NOT pin the `EventBus` (its `broadcast::Sender`) alive for its own
        // lifetime. The stream therefore ends -- and the task exits -- when the
        // `EventBus` is dropped (the `Runtime` is dropped, when the last
        // `Arc<Runtime>` held by every `Conway` clone goes away and the last
        // `Sender` goes with it): the task is bounded by the runtime's lifetime,
        // not leaked. (Holding the `Arc` for the task's whole life would keep
        // the `Sender` alive, the channel would never close, the stream would
        // never end, and the task would leak for the runtime's lifetime.)
        if !observe_sinks.is_empty() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                for sink in observe_sinks {
                    let bus_clone = event_bus.clone();
                    handle.spawn(async move {
                        // `subscribe()` borrows `bus_clone` and yields a
                        // `Receiver`-only stream; drop our strong `Arc` ref
                        // immediately so we do not keep the `EventBus` (its
                        // `Sender`) alive for the task's whole life (see the
                        // comment above the loop for the leak that would
                        // otherwise result).
                        let mut stream = bus_clone.subscribe();
                        drop(bus_clone);
                        use tokio_stream::StreamExt;
                        while let Some(envelope) = stream.next().await {
                            sink.emit(envelope.event);
                        }
                    });
                }
            } else {
                tracing::warn!(
                    "ConwayBuilder::build was called outside a tokio runtime; {} plugin observe \
                     sink(s) will NOT receive Event notifications (degrade: plugins load and serve \
                     tool/1 but observe/1 forwarding is skipped)",
                    observe_sinks.len()
                );
            }
        }
        let rt = Runtime::new(RuntimeDeps {
            store: store.clone(),
            path_store,
            router,
            health,
            backends: backend_map,
            plugins: resolved_plugins,
            gate,
            agent_defs,
            instructions: plugin_instructions,
            skills: skill_defs,
            event_bus,
            headroom: Arc::new(headroom_policy),
            session_discovery,
            capabilities: Arc::new(capability_registry)
                as Arc<dyn conway_core::ports::CapabilityHost>,
        });
        // `RuntimeDeps` has no `context_hook` field (out of that
        // item's file scope to add -- see `conway_runtime::runtime`'s
        // module doc), so registration happens post-construction via this
        // dedicated setter.
        //
        // The single hook the runtime accepts is composed here from TWO
        // sources, in this order: (1) an embedder's explicit
        // `with_context_hook`-injected hook (if any), then (2) every
        // installed plugin's own `Plugin::context_hooks()` contributions,
        // in `with_plugin`/`install_selected` install order. `None` overall
        // (no injected hook AND no plugin contributed one) sets the
        // runtime's hook to `None`, identical to never calling this method
        // at all -- the zero-cost default `Plugin::context_hooks`'s empty
        // default preserves for every plugin that does not opt in.
        let mut composed: Vec<Arc<dyn ContextHook>> = Vec::new();
        if let Some(injected) = context_hook {
            composed.push(injected);
        }
        composed.extend(plugin_context_hooks);
        rt.set_context_hook(compose_context_hooks(composed));
        // Mirrors the `context_hook` wiring immediately above: the single
        // curator the runtime accepts is composed here from TWO sources, in
        // this order -- (1) an embedder's explicit `with_curator`-injected
        // curator (if any), then (2) every installed plugin's own
        // `Plugin::curators()` contributions, in `with_plugin`/
        // `install_selected` install order. `None` overall (no injected
        // curator AND no plugin contributed one) sets the runtime's curator
        // to `None`, identical to never calling this method at all -- the
        // zero-cost default `Plugin::curators`'s empty default preserves for
        // every plugin that does not opt in, and the pre-assembly stage is a
        // pass-through (the `context_golden` 11/11 gate's load-bearing
        // guarantee).
        let mut composed_curators: Vec<Arc<dyn Curator>> = Vec::new();
        if let Some(injected) = context_curator {
            composed_curators.push(injected);
        }
        composed_curators.extend(plugin_curators);
        rt.set_context_curator(compose_curators(composed_curators));
        // mirrors the `context_hook`
        // wiring immediately above -- `hook_runner: None` (no
        // `with_hook_runner` call) sets the broker's runner to `None`,
        // identical to never calling `Runtime::set_hook_runner` at all
        // (`PermissionBroker::decide`'s hook-check step stays a no-op).
        // `pre_tool_use_specs` is computed unconditionally either way (an
        // empty `hook_runner` makes it inert regardless of what it
        // contains -- `PermissionBroker::pre_tool_use_hook_denial`'s own
        // doc), filtering `effective_hook_rules` (`[hooks].rules[]` PLUS
        // every plugin-declared rule folded in above) to exactly the
        // entries this item's own `HooksConfig` doc names as dispatched:
        // `event == "pre_tool_use"` and `enabled`. A plugin-registered
        // `pre_tool_use` rule lands in this SAME `Vec` a config-declared one
        // does, so it reaches `PermissionBroker::decide`'s hook-check step
        // at the IDENTICAL tier -- before the mode gate, the cache, pattern
        // allows, and `AutoAllow` -- by construction, not by a second,
        // parallel dispatch path (board item `01M129QW0GV90QTQS6B3BY3DAR`
        // acceptance 2).
        let pre_tool_use_specs: Vec<PreToolUseHookSpec> = effective_hook_rules
            .iter()
            .filter(|(_, rule, _)| rule.enabled && rule.event == "pre_tool_use")
            .map(|(origin, rule, _)| PreToolUseHookSpec {
                id: rule.id.clone(),
                command: rule.command.clone(),
                timeout_ms: rule.timeout_ms,
                // carried through
                // unchanged -- `PermissionBroker::pre_tool_use_hook_denial`
                // is where `None` vs `Some` actually decides anything.
                matcher: rule.match_tool.clone(),
                // carried through unchanged -- `HookEntry::on_failure`
                // already defaults to `HookOnFailure::Deny`, so an existing
                // rule that never sets it keeps denying on outage
                // byte-for-byte (board item `01M0X1AH44SNMK5TZ507K30QNP`).
                on_failure: rule.on_failure,
                origin: origin.clone(),
            })
            .collect();
        // The observation and deny-only events, and: the
        // same shape for every event dispatched outside the permission
        // broker, grouped by event name. `post_tool_use`, `session_starting`,
        // `child_spawned`, `request_assembled`, `child_reported`, and every
        // ACTUALLY-DECLARED plugin event (`plugin_events`, computed at step
        // 10b above) observe; `prompt_submitted` may deny but never modify.
        // A namespaced `event` naming no installed plugin's declared event
        // still parses, validates, and does nothing -- the SAME tolerance a
        // typo'd core event name has always had (see `schema::HooksConfig`'s
        // own per-event reachability doc for the exhaustive dispatched
        // list).
        //
        // The SAME runner feeds both tiers, so an embedder that injects one
        // gets every dispatched event rather than having to opt in twice.
        // Reads `effective_hook_rules`, exactly like `pre_tool_use_specs`
        // immediately above -- one classification pass over one combined
        // list, not a second copy of this loop for plugin-declared rules.
        let mut observation_specs: BTreeMap<String, Vec<HookSpec>> = BTreeMap::new();
        for (origin, rule, spawn_only) in effective_hook_rules.iter().filter(|(_, r, _)| r.enabled)
        {
            let plugin_decl = plugin_events.get(&rule.event);
            if !DISPATCHED_EVENTS.contains(&rule.event.as_str()) && plugin_decl.is_none() {
                continue;
            }
            // the plugin-event
            // extension of check 10's own rule (`merge::validate`, core
            // events only -- that function has no access to the resolved
            // plugin set). A `match` on a plugin event whose OWN
            // declaration says its payload carries no tool name is the
            // identical typed error, just discoverable only here, once the
            // plugin set is known.
            if let (Some(_matcher), Some(decl)) = (&rule.match_tool, plugin_decl) {
                if !decl.carries_tool_name {
                    return Err(FacadeError::Build {
                        message: format!(
                            "hooks.rules[]: rule '{}' sets \"match\" on event \"{}\", whose \
                             declaration says its payload carries no tool name -- \"match\" \
                             only applies to an event whose payload names one",
                            rule.id, rule.event
                        ),
                    });
                }
            }
            observation_specs
                .entry(rule.event.clone())
                .or_default()
                .push(HookSpec {
                    id: rule.id.clone(),
                    command: rule.command.clone(),
                    timeout_ms: rule.timeout_ms,
                    // carried
                    // through unchanged -- only meaningful for
                    // `post_tool_use` and a `carries_tool_name` plugin
                    // event (`HookSpec::matcher`'s own doc); `merge::
                    // validate` and the check immediately above together
                    // refuse to load/build a config pairing `match` with
                    // any toolless event, core or plugin.
                    matcher: rule.match_tool.clone(),
                    origin: origin.clone(),
                    // `PluginHookRule::spawn_only`, carried through
                    // `effective_hook_rules`'s own third element (that
                    // field's own doc: no `HookEntry` counterpart) --
                    // `false` for every `[hooks].rules[]` entry, an
                    // operator has no way to set it.
                    spawn_only: *spawn_only,
                });
        }

        rt.set_hook_runner(hook_runner.clone());
        rt.set_pre_tool_use_hooks(pre_tool_use_specs);
        rt.set_observation_hook_runner(hook_runner);
        rt.set_observation_hooks(observation_specs);

        // Install each plugin's `permission_rules()` contributions as
        // `PatternOrigin::Plugin` deny/prompt rules in the broker (board
        // item `01M03VKJG7JJ0JEKY265WA7MJ7`). A `Deny` verdict -> a deny
        // rule at the broker's deny tier (step 2 of `PermissionBroker::
        // decide`'s ordering -- before plan-mode, the cache, pattern-allow,
        // and `AutoAllow`); a `Prompt` verdict -> a prompt rule (step 4 --
        // sets `must_reach_gate`, forcing the operator's gate); an `Abstain`
        // verdict installs nothing. `When::Always` means the rule matches
        // every call to the named tool regardless of rendered args, so
        // `base` (the `PathsUnder` canonicalization root) is never
        // consulted -- the placeholder `/` is inert, the same way
        // `remember_pattern`'s own `debug_assert!` pins it. There is no
        // `Allow` path here (a plugin cannot contribute one -- see
        // `Plugin::permission_rules`'s trait doc), so `remember_pattern_rule`'s
        // `PatternOrigin::Plugin` allow-rejection guard is never even
        // reached: the narrowing-only verdict type makes widening
        // structurally impossible, and the operator's own
        // `permissions.json`/`PermissionMode` still wins.
        let broker = rt.permission_broker();
        for rule in &plugin_permission_rules {
            let structured = Rule {
                select: Select::Tools(vec![rule.tool.clone()]),
                when: When::Always,
                then: match rule.verdict {
                    PluginPermissionVerdict::Deny => Then::Deny,
                    PluginPermissionVerdict::Prompt => Then::Prompt,
                    PluginPermissionVerdict::Abstain => continue,
                },
            };
            match rule.verdict {
                PluginPermissionVerdict::Deny => {
                    broker.remember_deny_rule(
                        structured,
                        PatternOrigin::Plugin,
                        std::path::Path::new("/"),
                    );
                }
                PluginPermissionVerdict::Prompt => {
                    broker.remember_prompt_rule(
                        structured,
                        PatternOrigin::Plugin,
                        std::path::Path::new("/"),
                    );
                }
                PluginPermissionVerdict::Abstain => {}
            }
        }

        Ok(Conway::new(
            rt,
            config,
            store,
            router_explain,
            warnings,
            metadata,
            root,
            plugin_status_contributions,
            declared_permission_modes,
            live_plugins,
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

/// Detects a cycle in the `PluginManifest::requires` graph, restricted to
/// ids present in `manifests` -- an edge to an id NOT present here is
/// simply absent from this graph (missing-ness is a distinct, separately
/// enforced check: [`missing_required_dependency`]). Returns
/// [`PluginError::DependencyCycle`] naming one full cycle (ids in
/// traversal order, the starting id repeated at both ends, e.g.
/// `"a -> b -> a"`) on the first cycle found.
///
/// Iterative three-color DFS over indices into `manifests` (never over
/// plugin-id string references), so it terminates on any input, including
/// a large or pathological dependency graph, with no recursion-depth
/// concern.
fn detect_required_dependency_cycle(
    manifests: &[PluginManifest],
) -> std::result::Result<(), PluginError> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Color {
        White,
        Gray,
        Black,
    }

    let index_of: HashMap<&str, usize> = manifests
        .iter()
        .enumerate()
        .map(|(i, m)| (m.id.as_str(), i))
        .collect();
    let mut color = vec![Color::White; manifests.len()];

    for start in 0..manifests.len() {
        if color[start] != Color::White {
            continue;
        }
        // (node, next-`requires`-index-to-visit) -- an explicit stack, not
        // a recursive call, so depth is bounded only by heap, not the
        // process's call stack.
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        color[start] = Color::Gray;
        while let Some(&top) = stack.last() {
            // Copy the frame out (both fields are `usize`, `Copy`) so
            // nothing below holds a live borrow of `stack` while it is
            // also pushed/popped/mutated.
            let (node, edge_idx) = top;
            let deps = &manifests[node].requires;
            if edge_idx >= deps.len() {
                color[node] = Color::Black;
                stack.pop();
                continue;
            }
            stack
                .last_mut()
                .expect("stack non-empty: just matched above")
                .1 += 1;
            let Some(&dep) = index_of.get(deps[edge_idx].as_str()) else {
                // Not among the manifests THIS call can see -- membership,
                // not cycle, and checked elsewhere.
                continue;
            };
            match color[dep] {
                Color::White => {
                    color[dep] = Color::Gray;
                    stack.push((dep, 0));
                }
                Color::Gray => {
                    // `dep` is on the current path -- everything from its
                    // own stack position onward, plus `dep` again, IS the
                    // cycle.
                    let cycle_start = stack
                        .iter()
                        .position(|&(n, _)| n == dep)
                        .expect("dep is Gray, so it is on the current stack");
                    let mut cycle: Vec<String> = stack[cycle_start..]
                        .iter()
                        .map(|&(n, _)| manifests[n].id.clone())
                        .collect();
                    cycle.push(manifests[dep].id.clone());
                    return Err(PluginError::DependencyCycle {
                        cycle: cycle.join(" -> "),
                    });
                }
                Color::Black => {}
            }
        }
    }
    Ok(())
}

/// Checks every manifest's `PluginManifest::requires` against the id set
/// `manifests` itself carries, UNION the set of capability names some
/// installed plugin's `Plugin::capabilities()` provides (`provided_caps`) --
/// Edge B (`docs/vision/DESIGN-plugin-dependencies.md` §2): a `requires`
/// entry is satisfied by EITHER an installed plugin id OR a provided
/// capability name, "one vocabulary, not two" applied to the SAME field
/// rather than a second, parallel `requires_capability` list. A plain
/// membership test, no ordering question. Returns the FIRST missing
/// required dependency as [`PluginError::MissingDependency`], naming both
/// the dependent and the missing id/capability. Called at
/// `ConwayBuilder::build` with the FINAL installed set (built-ins ++
/// everything `install_selected`/`with_plugin` added), which is the only
/// point this crate has full visibility into that set -- see
/// `PluginManifest::requires`'s own doc.
///
/// This is the "does anything installed actually provide this name" check
/// `crate::event_name`'s own doc records as missing one layer down (§16.6
/// point 2), built here for capabilities: a `requires` naming a capability
/// nothing provides now fails the SAME way a `requires` naming an absent
/// plugin id already did, rather than resolving to silence.
fn missing_required_dependency(
    manifests: &[PluginManifest],
    provided_caps: &HashSet<String>,
) -> std::result::Result<(), PluginError> {
    let ids: HashSet<&str> = manifests.iter().map(|m| m.id.as_str()).collect();
    for manifest in manifests {
        for dep in &manifest.requires {
            if !ids.contains(dep.as_str()) && !provided_caps.contains(dep.as_str()) {
                return Err(PluginError::MissingDependency {
                    plugin: manifest.id.clone(),
                    dependency: dep.clone(),
                });
            }
        }
    }
    Ok(())
}

/// The `optional` counterpart of [`missing_required_dependency`]: every
/// `(dependent id, missing dependency-or-capability id)` pair for a
/// `PluginManifest::optional` entry absent from the final installed set AND
/// from `provided_caps` (see that function's own doc for the union rule).
/// Never an error -- an optional dependency's absence degrades rather than
/// refuses (`PluginManifest::optional`'s own doc) -- the caller
/// (`ConwayBuilder::build`) turns each pair into a `tracing::warn!` and a
/// `ConfigWarning` rather than failing the build.
fn missing_optional_dependencies(
    manifests: &[PluginManifest],
    provided_caps: &HashSet<String>,
) -> Vec<(String, String)> {
    let ids: HashSet<&str> = manifests.iter().map(|m| m.id.as_str()).collect();
    let mut missing = Vec::new();
    for manifest in manifests {
        for dep in &manifest.optional {
            if !ids.contains(dep.as_str()) && !provided_caps.contains(dep.as_str()) {
                missing.push((manifest.id.clone(), dep.clone()));
            }
        }
    }
    missing
}

/// The set of capability names (`HostCapability::as_wire_str`) some
/// installed plugin's `Plugin::capabilities()` registers a provider for --
/// the STATIC input [`missing_required_dependency`]/
/// [`missing_optional_dependencies`] union against each manifest's own id
/// set. Deliberately takes `&[Arc<dyn Plugin>]`, not `&[PluginManifest]`: a
/// provided capability is a RUNTIME registration (`Plugin::capabilities`'s
/// own doc: the runtime half of a declaration, mirroring `Plugin::tools`
/// vs `PluginManifest::tools`), not manifest data, so it cannot be read
/// from a manifest alone.
fn provided_capability_names(plugins: &[Arc<dyn Plugin>]) -> HashSet<String> {
    plugins
        .iter()
        .flat_map(|p| p.capabilities())
        .map(|registration| registration.capability.as_wire_str().to_string())
        .collect()
}

#[cfg(test)]
mod plugin_dependency_resolution_tests {
    //! Unit coverage for the four free functions above
    //! ([`detect_required_dependency_cycle`], [`missing_required_dependency`],
    //! [`missing_optional_dependencies`], [`provided_capability_names`])
    //! directly, at the graph-algorithm level -- distinct from
    //! `crates/conway/tests/install_selected.rs`'s (`::build`) end-to-end
    //! coverage of the plugin-id case through the real facade,
    //! `crates/conway/tests/capability_channel.rs`'s equivalent end-to-end
    //! coverage of the Edge B capability case, and from
    //! `crates/conway/tests/builder.rs`'s own
    //! `a_requires_edge_does_not_reorder_instruction_fragment_precedence`
    //! (the injection-order/resolution-order separation this graph exists
    //! to serve, without itself deciding).

    use super::*;

    fn manifest(id: &str, requires: &[&str], optional: &[&str]) -> PluginManifest {
        PluginManifest {
            id: id.to_string(),
            version: "0.0.0".to_string(),
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: requires.iter().map(|s| s.to_string()).collect(),
            optional: optional.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn no_caps() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn no_edges_is_never_a_cycle_or_missing() {
        let manifests = vec![manifest("a", &[], &[]), manifest("b", &[], &[])];
        assert!(detect_required_dependency_cycle(&manifests).is_ok());
        assert!(missing_required_dependency(&manifests, &no_caps()).is_ok());
        assert!(missing_optional_dependencies(&manifests, &no_caps()).is_empty());
    }

    #[test]
    fn a_satisfied_requires_edge_is_neither_a_cycle_nor_missing() {
        let manifests = vec![
            manifest("dependent", &["base"], &[]),
            manifest("base", &[], &[]),
        ];
        assert!(detect_required_dependency_cycle(&manifests).is_ok());
        assert!(missing_required_dependency(&manifests, &no_caps()).is_ok());
    }

    #[test]
    fn detect_required_dependency_cycle_finds_a_two_node_cycle() {
        let manifests = vec![manifest("a", &["b"], &[]), manifest("b", &["a"], &[])];
        let err = detect_required_dependency_cycle(&manifests)
            .expect_err("a requires b requires a must be refused as a cycle");
        match err {
            PluginError::DependencyCycle { cycle } => {
                assert!(cycle.contains('a'), "{cycle}");
                assert!(cycle.contains('b'), "{cycle}");
                assert!(cycle.contains("->"), "{cycle}");
            }
            other => panic!("expected DependencyCycle, got {other:?}"),
        }
    }

    #[test]
    fn detect_required_dependency_cycle_finds_a_self_loop() {
        let manifests = vec![manifest("a", &["a"], &[])];
        let err = detect_required_dependency_cycle(&manifests)
            .expect_err("a requiring itself must be refused as a cycle");
        assert!(matches!(err, PluginError::DependencyCycle { .. }));
    }

    #[test]
    fn detect_required_dependency_cycle_ignores_optional_only_edges() {
        // a optionally depends on b, b optionally depends on a: no REQUIRES
        // edge exists at all, so this is not a cycle -- a mutual optional
        // relationship is a perfectly ordinary, harmless configuration
        // (each simply checks whether the other happens to be installed).
        let manifests = vec![manifest("a", &[], &["b"]), manifest("b", &[], &["a"])];
        assert!(detect_required_dependency_cycle(&manifests).is_ok());
    }

    #[test]
    fn detect_required_dependency_cycle_ignores_edges_to_ids_absent_from_the_set() {
        // "ghost" is not among `manifests` at all -- absence is a
        // membership question (`missing_required_dependency`'s own job),
        // never a cycle question.
        let manifests = vec![manifest("a", &["ghost"], &[])];
        assert!(detect_required_dependency_cycle(&manifests).is_ok());
    }

    #[test]
    fn missing_required_dependency_names_both_sides() {
        let manifests = vec![manifest("dependent", &["missing.base"], &[])];
        let err = missing_required_dependency(&manifests, &no_caps())
            .expect_err("a requires edge to an absent id must be refused");
        match err {
            PluginError::MissingDependency { plugin, dependency } => {
                assert_eq!(plugin, "dependent");
                assert_eq!(dependency, "missing.base");
            }
            other => panic!("expected MissingDependency, got {other:?}"),
        }
    }

    #[test]
    fn missing_optional_dependencies_lists_every_missing_pair_never_erroring() {
        let manifests = vec![
            manifest("a", &[], &["missing.one"]),
            manifest("b", &[], &["missing.two"]),
        ];
        let missing = missing_optional_dependencies(&manifests, &no_caps());
        assert_eq!(missing.len(), 2);
        assert!(missing.contains(&("a".to_string(), "missing.one".to_string())));
        assert!(missing.contains(&("b".to_string(), "missing.two".to_string())));
        // Never an error -- optional absence degrades, it never refuses.
        assert!(detect_required_dependency_cycle(&manifests).is_ok());
        assert!(missing_required_dependency(&manifests, &no_caps()).is_ok());
    }

    #[test]
    fn a_present_optional_dependency_is_not_reported_missing() {
        let manifests = vec![manifest("a", &[], &["b"]), manifest("b", &[], &[])];
        assert!(missing_optional_dependencies(&manifests, &no_caps()).is_empty());
    }

    // -----------------------------------------------------------------
    // Edge B: a `requires`/`optional` entry satisfied by a PROVIDED
    // capability rather than a plugin id (board item
    // `01M0WWNHQQYN1EVTH8WPZ33EBF`, acceptance 2/3).
    // -----------------------------------------------------------------

    fn caps(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_requires_edge_satisfied_by_a_provided_capability_is_not_missing() {
        // "dependent" requires "acme.ui.checkbox" -- no PLUGIN named that,
        // but SOME installed plugin's `Plugin::capabilities()` provides it.
        let manifests = vec![manifest("dependent", &["acme.ui.checkbox"], &[])];
        assert!(
            missing_required_dependency(&manifests, &caps(&["acme.ui.checkbox"])).is_ok(),
            "a provided capability satisfies a requires entry exactly as an installed plugin id does"
        );
    }

    #[test]
    fn missing_required_dependency_names_the_unprovided_capability() {
        // Nothing installed is named "acme.ui.checkbox" AND no installed
        // plugin provides a capability by that name -- must fail the SAME
        // way a `requires` naming an absent plugin id already does, not
        // resolve to silence.
        let manifests = vec![manifest("dependent", &["acme.ui.checkbox"], &[])];
        let err = missing_required_dependency(&manifests, &no_caps())
            .expect_err("a requires entry naming an unprovided capability must be refused");
        match err {
            PluginError::MissingDependency { plugin, dependency } => {
                assert_eq!(plugin, "dependent");
                assert_eq!(dependency, "acme.ui.checkbox");
            }
            other => panic!("expected MissingDependency, got {other:?}"),
        }
    }

    #[test]
    fn an_optional_capability_nothing_provides_degrades_not_errors() {
        let manifests = vec![manifest("dependent", &[], &["acme.ui.checkbox"])];
        let missing = missing_optional_dependencies(&manifests, &no_caps());
        assert_eq!(
            missing,
            vec![("dependent".to_string(), "acme.ui.checkbox".to_string())]
        );
        assert!(missing_required_dependency(&manifests, &no_caps()).is_ok());
    }

    #[test]
    fn an_optional_capability_something_provides_is_not_reported_missing() {
        let manifests = vec![manifest("dependent", &[], &["acme.ui.checkbox"])];
        assert!(missing_optional_dependencies(&manifests, &caps(&["acme.ui.checkbox"])).is_empty());
    }

    /// A minimal fixture `Plugin` that provides one or more capabilities --
    /// used only to exercise [`provided_capability_names`] itself, which
    /// (unlike the two functions above) reads live `Plugin::capabilities()`
    /// registrations rather than manifest data.
    struct ProvidingPlugin {
        id: &'static str,
        provides: Vec<&'static str>,
    }

    struct EchoProvider;

    #[async_trait::async_trait]
    impl conway_core::ports::CapabilityProvider for EchoProvider {
        async fn call(
            &self,
            payload: serde_json::Value,
        ) -> std::result::Result<serde_json::Value, conway_core::ports::CapabilityError> {
            Ok(payload)
        }
    }

    impl Plugin for ProvidingPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                id: self.id.to_string(),
                version: "0.0.0".to_string(),
                tools: vec![],
                required_host_caps: vec![],
                optional_host_caps: vec![],
                requires: vec![],
                optional: vec![],
            }
        }

        fn tools(&self) -> Vec<Arc<dyn conway_core::ports::Tool>> {
            vec![]
        }

        fn capabilities(&self) -> Vec<conway_core::ports::CapabilityRegistration> {
            self.provides
                .iter()
                .map(|name| {
                    conway_core::ports::CapabilityRegistration::new(
                        conway_core::ports::HostCapability::named(*name).unwrap(),
                        "1.0.0",
                        Arc::new(EchoProvider) as Arc<dyn conway_core::ports::CapabilityProvider>,
                    )
                })
                .collect()
        }
    }

    #[test]
    fn provided_capability_names_collects_every_installed_plugins_registrations() {
        let plugins: Vec<Arc<dyn Plugin>> = vec![
            Arc::new(ProvidingPlugin {
                id: "acme.ui",
                provides: vec!["acme.ui.checkbox", "acme.ui.select"],
            }),
            Arc::new(ProvidingPlugin {
                id: "acme.other",
                provides: vec!["acme.other.thing"],
            }),
            Arc::new(ProvidingPlugin {
                id: "acme.silent",
                provides: vec![],
            }),
        ];
        let names = provided_capability_names(&plugins);
        assert_eq!(
            names,
            caps(&["acme.ui.checkbox", "acme.ui.select", "acme.other.thing"])
        );
    }
}

/// Composes zero or more plugin-/embedder-contributed [`ContextHook`]s into
/// the single `Option<Arc<dyn ContextHook>>` the runtime accepts.
///
/// - Empty -> `None`: the runtime's hook stays unset, byte-identical to
///   never calling `with_context_hook` / no curating plugin installed.
/// - One -> that hook directly: no wrapper, so `GuardedContextHook` wraps
///   exactly the one hook the caller contributed (the common case -- e.g.
///   a single `conway.skills` install).
/// - More than one -> a [`ChainedContextHook`] that runs them in order,
///   feeding each hook's returned payload to the next (`before_request`),
///   and on `on_overflow` runs each in order returning the first `Some`.
///
/// This is the composition `Plugin::context_hooks`'s own doc names: an
/// embedder's `with_context_hook` hook first, then each plugin's hooks in
/// install order. It keeps the `with_context_hook` surface working for a
/// standalone hook while letting plugins contribute curation through the
/// SAME `with_plugin`/`install_selected` surface -- no privileged channel.
fn compose_context_hooks(hooks: Vec<Arc<dyn ContextHook>>) -> Option<Arc<dyn ContextHook>> {
    match hooks.len() {
        0 => None,
        1 => Some(hooks.into_iter().next().expect("len == 1")),
        _ => Some(Arc::new(ChainedContextHook::new(hooks))),
    }
}

/// Runs a chain of [`ContextHook`]s in order, feeding each hook's
/// `before_request` output to the next. `on_overflow` runs each hook in
/// order on the payload and returns the first `Some` (a hook that returns
/// `None` defers to the next; the final `None` falls through to the hard
/// `ContextTooLarge`, exactly as a single hook's default does).
///
/// Only constructed by [`compose_context_hooks`] when more than one hook is
/// present; the one-hook case installs that hook directly, so this type's
/// chaining logic is exercised only when an embedder AND a plugin (or two
/// plugins) each contribute a hook.
struct ChainedContextHook {
    hooks: Vec<Arc<dyn ContextHook>>,
}

impl ChainedContextHook {
    fn new(hooks: Vec<Arc<dyn ContextHook>>) -> Self {
        Self { hooks }
    }
}

#[async_trait::async_trait]
impl ContextHook for ChainedContextHook {
    async fn before_request(
        &self,
        ctx: &conway_core::ports::ContextHookCtx,
        mut payload: conway_core::ports::ContextPayload,
    ) -> conway_core::ports::ContextPayload {
        for hook in &self.hooks {
            payload = hook.before_request(ctx, payload).await;
        }
        payload
    }

    async fn on_overflow(
        &self,
        ctx: &conway_core::ports::ContextHookCtx,
        mut payload: conway_core::ports::ContextPayload,
        overflow: conway_core::ports::OverflowInfo,
    ) -> Option<conway_core::ports::ContextPayload> {
        // Each hook gets a chance to shrink the payload; a hook that
        // returns `None` (the default -- "I can't help") simply defers to
        // the next, which may shrink different segments. The chain returns
        // `Some` iff at least one hook shrank, else `None` (the hard
        // `ContextTooLarge`, exactly as a single hook's default does).
        let mut shrunk = false;
        for hook in &self.hooks {
            // `on_overflow` takes `payload` by value and returns `Option`,
            // so a `None` ("I can't help") would consume the payload and
            // leave nothing for the next hook. Clone for each call so a
            // hook that abstains does not foreclose on a later sibling
            // that could shrink different segments -- the rare multi-hook
            // overflow path pays one `ContextPayload` clone per hook,
            // acceptable for a path that only runs on a T-1 rejection.
            let result = hook.on_overflow(ctx, payload.clone(), overflow).await;
            if let Some(transformed) = result {
                payload = transformed;
                shrunk = true;
            }
        }
        if shrunk {
            Some(payload)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod compose_context_hooks_tests {
    //! Covers [`compose_context_hooks`]'s 0/1/2+ branches and
    //! [`ChainedContextHook`]'s chaining, which nothing else in the tree
    //! drives -- see this item's own filing (board item
    //! `01M090HJEJBK24SX70Z9E25PZ4`): `grep -rln "compose_context_hooks\|
    //! ChainedContextHook" crates/` matched only this file before these
    //! tests existed.
    //!
    //! **Characterization, not specification.** These tests pin the
    //! existing behavior of `compose_context_hooks`/`ChainedContextHook`;
    //! they do not change it. Where a finding below reads like a defect,
    //! it is reported as one (see the worker's completion report for this
    //! item), not silently "fixed" here.
    //!
    //! **The asymmetry with [`compose_curators_tests`], confirmed by
    //! reading the actual contract first** (`conway_core::ports::plugin`'s
    //! `ContextHook` doc, and `conway_runtime::context::hook_guard`'s
    //! `GuardedContextHook`), not assumed from `Curator`'s shape:
    //!
    //! - `ContextHook` has NO `Failed`/refusal variant at all.
    //!   `before_request` returns a bare `ContextPayload`, not a
    //!   `Result`/enum a hook could use to signal failure, so
    //!   `ChainedContextHook::before_request` has nothing to branch on --
    //!   every hook in the chain always runs, in order, on the previous
    //!   hook's output. `on_overflow`'s `None` means "I can't help", not an
    //!   error, and (unlike `Curator`'s `Failed`) does NOT stop the chain --
    //!   every remaining hook still gets a turn. This is the load-bearing
    //!   divergence from `compose_curators`' Failed-stops-the-chain rule,
    //!   and is asserted directly below
    //!   (`on_overflow_runs_every_hook_even_after_an_earlier_one_already_shrank`).
    //! - A `ContextHook`'s edit is a rewrite the harness re-validates via
    //!   `GuardedContextHook`, not a construction-time-guaranteed
    //!   `Derivation` the way a `Curator`'s is (`Curator` needed no such
    //!   wrapper -- see `compose_curators`'s own doc). Read
    //!   `Runtime::set_context_hook` (`conway_runtime::runtime`):  it wraps
    //!   whatever `compose_context_hooks` returns -- the single hook
    //!   directly, or the whole `ChainedContextHook` -- in exactly ONE
    //!   `GuardedContextHook`. So coherence is re-validated once, on the
    //!   chain's FINAL output, never on an intermediate hook's output: a
    //!   hook that orphans a tool call and a later hook that repairs it
    //!   would never be refused, and only a hook that leaves the chain's
    //!   last output incoherent trips the guard. That is worth pinning, but
    //!   `GuardedContextHook::before_request`/`on_overflow` are
    //!   `pub(crate)` to `conway-runtime` (see that type's own doc) and
    //!   this item's blast radius is `crates/conway/` only, so it is
    //!   recorded here as a documented finding rather than exercised by a
    //!   test in this module -- `conway_runtime::context::hook_guard`'s own
    //!   `context_hook_wrapping_tests` module is where that guard's
    //!   behavior is actually driven.
    //! - No retry loop lives in `ChainedContextHook` itself: each hook's
    //!   `on_overflow` runs exactly once per call to the composed hook.
    //!   The bounded re-attempt loop the doc for `on_overflow`'s retry
    //!   path might suggest (`MAX_OVERFLOW_ATTEMPTS`) is `AgentLoop::
    //!   route_and_attempt`'s concern (`conway-runtime`, out of this
    //!   item's scope), which simply calls the SAME composed (guarded)
    //!   hook's `on_overflow` again on a subsequent attempt -- nothing
    //!   about that requires `ChainedContextHook` itself to retry
    //!   anything.

    use super::*;
    use conway_core::content::{ContentBlock, Role};
    use conway_core::ids::{AgentId, SessionId};
    use conway_core::ports::{ArtifactWriteHandle, ContextHookCtx, ContextPayload, OverflowInfo};
    use conway_core::provenance::Provenance;
    use conway_core::segment::PromptSegment;

    fn hook_ctx() -> ContextHookCtx {
        let agent_id = AgentId::new();
        ContextHookCtx {
            agent_id,
            agent_path: vec![agent_id],
            session_id: SessionId::new(),
            turn: 1,
            model: None,
            estimated_tokens: 100,
            artifacts: ArtifactWriteHandle::noop(agent_id),
            tag: None,
        }
    }

    fn segment(text: &str) -> PromptSegment {
        PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            Provenance::UserPrompt,
        )
    }

    fn payload(segments: Vec<PromptSegment>) -> ContextPayload {
        ContextPayload {
            segments,
            tools: Vec::new(),
        }
    }

    fn overflow() -> OverflowInfo {
        OverflowInfo {
            max_context_tokens: 100,
            headroom_tokens: 10,
            required_tokens: 200,
            shortfall_tokens: 100,
        }
    }

    /// Appends one marker segment in `before_request`, recording how many
    /// segments it saw on entry -- the analogue of `compose_curators_tests`'
    /// `OmitAt`, which records `base_len_seen`.
    struct AppendsMarker {
        marker: &'static str,
        segments_seen: std::sync::Mutex<Option<usize>>,
    }

    impl AppendsMarker {
        fn new(marker: &'static str) -> Arc<Self> {
            Arc::new(Self {
                marker,
                segments_seen: std::sync::Mutex::new(None),
            })
        }
    }

    #[async_trait::async_trait]
    impl ContextHook for AppendsMarker {
        async fn before_request(
            &self,
            _ctx: &ContextHookCtx,
            mut payload: ContextPayload,
        ) -> ContextPayload {
            *self.segments_seen.lock().unwrap() = Some(payload.segments.len());
            payload.segments.push(segment(self.marker));
            payload
        }
    }

    /// Shrinks an overflowing payload to `keep` segments if it currently
    /// has more than that; otherwise declines (`None`) -- exactly the
    /// "I can't help, defer to the next hook" case the port doc names.
    /// Records the segment count it was CALLED with, so a chain test can
    /// prove the second hook saw the first's output rather than the
    /// original payload.
    struct ShrinksOnOverflow {
        keep: usize,
        segments_seen: std::sync::Mutex<Option<usize>>,
    }

    impl ShrinksOnOverflow {
        fn new(keep: usize) -> Arc<Self> {
            Arc::new(Self {
                keep,
                segments_seen: std::sync::Mutex::new(None),
            })
        }
    }

    #[async_trait::async_trait]
    impl ContextHook for ShrinksOnOverflow {
        async fn before_request(
            &self,
            _ctx: &ContextHookCtx,
            payload: ContextPayload,
        ) -> ContextPayload {
            payload
        }

        async fn on_overflow(
            &self,
            _ctx: &ContextHookCtx,
            payload: ContextPayload,
            _overflow: OverflowInfo,
        ) -> Option<ContextPayload> {
            *self.segments_seen.lock().unwrap() = Some(payload.segments.len());
            if payload.segments.len() > self.keep {
                let mut shrunk = payload;
                shrunk.segments.truncate(self.keep);
                Some(shrunk)
            } else {
                None
            }
        }
    }

    /// Records whether it ran at all, regardless of what an earlier hook in
    /// the chain returned -- proves the on_overflow chain does NOT stop
    /// early the way `ComposedCurator` stops on `Failed`.
    struct RecordsThatItRan {
        ran: std::sync::Mutex<bool>,
    }

    #[async_trait::async_trait]
    impl ContextHook for RecordsThatItRan {
        async fn before_request(
            &self,
            _ctx: &ContextHookCtx,
            payload: ContextPayload,
        ) -> ContextPayload {
            payload
        }

        async fn on_overflow(
            &self,
            _ctx: &ContextHookCtx,
            _payload: ContextPayload,
            _overflow: OverflowInfo,
        ) -> Option<ContextPayload> {
            *self.ran.lock().unwrap() = true;
            // Declines -- the point is that it ran at all, not that it
            // shrinks anything.
            None
        }
    }

    #[test]
    fn compose_of_none_is_none_so_the_runtimes_hook_stays_unset() {
        assert!(compose_context_hooks(Vec::new()).is_none());
    }

    #[tokio::test]
    async fn compose_of_one_installs_it_directly_without_a_chained_wrapper() {
        let only: Arc<dyn ContextHook> = AppendsMarker::new("only");
        let composed =
            compose_context_hooks(vec![only.clone()]).expect("one hook composes to Some");
        // The SAME Arc, not a ChainedContextHook wrapping it.
        assert!(Arc::ptr_eq(&composed, &only));

        // And it behaves exactly like calling the hook directly -- no
        // wrapper changes what `before_request` returns.
        let out = composed
            .before_request(&hook_ctx(), payload(vec![segment("base")]))
            .await;
        assert_eq!(out.segments.len(), 2);
    }

    #[tokio::test]
    async fn chained_before_request_feeds_the_first_hooks_output_to_the_second() {
        let first = AppendsMarker::new("first");
        let second = AppendsMarker::new("second");
        let composed = compose_context_hooks(vec![first.clone(), second.clone()])
            .expect("two hooks compose to Some");

        let out = composed
            .before_request(&hook_ctx(), payload(vec![segment("base")]))
            .await;

        assert_eq!(first.segments_seen.lock().unwrap().unwrap(), 1);
        // The load-bearing assertion: the second hook's input is the
        // FIRST's output (2 segments: base + "first"), not the original
        // 1-segment payload.
        assert_eq!(second.segments_seen.lock().unwrap().unwrap(), 2);
        assert_eq!(
            out.segments.len(),
            3,
            "base + first's marker + second's marker"
        );
    }

    #[tokio::test]
    async fn chained_on_overflow_feeds_the_first_hooks_shrunk_payload_to_the_second() {
        let first = ShrinksOnOverflow::new(2);
        let second = ShrinksOnOverflow::new(1);
        let composed = compose_context_hooks(vec![first.clone(), second.clone()])
            .expect("two hooks compose to Some");

        let three_segments = payload(vec![segment("a"), segment("b"), segment("c")]);
        let out = composed
            .on_overflow(&hook_ctx(), three_segments, overflow())
            .await;

        assert_eq!(first.segments_seen.lock().unwrap().unwrap(), 3);
        // The load-bearing assertion: the second hook saw the FIRST's
        // shrunk-to-2 output, not the original 3-segment payload.
        assert_eq!(second.segments_seen.lock().unwrap().unwrap(), 2);
        let out = out.expect("at least one hook shrank -- Some");
        assert_eq!(out.segments.len(), 1);
    }

    #[tokio::test]
    async fn a_hook_that_declines_defers_the_unshrunk_payload_to_the_next() {
        // `first` keeps up to 10 -- a 2-segment payload never exceeds that,
        // so it declines (`None`) and the ORIGINAL payload passes to
        // `second` untouched.
        let first = ShrinksOnOverflow::new(10);
        let second = ShrinksOnOverflow::new(1);
        let composed = compose_context_hooks(vec![first.clone(), second.clone()])
            .expect("two hooks compose to Some");

        let two_segments = payload(vec![segment("a"), segment("b")]);
        let out = composed
            .on_overflow(&hook_ctx(), two_segments, overflow())
            .await;

        assert_eq!(first.segments_seen.lock().unwrap().unwrap(), 2);
        assert_eq!(
            second.segments_seen.lock().unwrap().unwrap(),
            2,
            "second sees the ORIGINAL 2-segment payload, since first declined rather than shrinking"
        );
        assert_eq!(out.expect("second shrank -- Some").segments.len(), 1);
    }

    #[tokio::test]
    async fn on_overflow_is_none_when_every_hook_in_the_chain_declines() {
        // Mirrors a single hook's default `on_overflow`: no hook shrank
        // anything, so the composed result is `None` -- the runtime's hard
        // `ContextTooLarge`, exactly as if no hook (or a single
        // never-shrinks hook) were registered.
        let composed =
            compose_context_hooks(vec![ShrinksOnOverflow::new(10), ShrinksOnOverflow::new(10)])
                .expect("two hooks compose to Some");

        let two_segments = payload(vec![segment("a"), segment("b")]);
        let out = composed
            .on_overflow(&hook_ctx(), two_segments, overflow())
            .await;

        assert!(out.is_none());
    }

    #[tokio::test]
    async fn on_overflow_runs_every_hook_even_after_an_earlier_one_already_shrank() {
        // The divergence from `ComposedCurator`, which stops on `Failed`:
        // `ContextHook` has no failure/refusal concept at all, so nothing
        // in the chain ever short-circuits. `first` shrinks (returns
        // `Some`) and `second` still gets its turn.
        let first = ShrinksOnOverflow::new(1);
        let second = Arc::new(RecordsThatItRan {
            ran: std::sync::Mutex::new(false),
        });
        let composed =
            compose_context_hooks(vec![first, second.clone()]).expect("two hooks compose to Some");

        let two_segments = payload(vec![segment("a"), segment("b")]);
        let _ = composed
            .on_overflow(&hook_ctx(), two_segments, overflow())
            .await;

        assert!(
            *second.ran.lock().unwrap(),
            "every hook in the chain runs, regardless of what an earlier hook returned"
        );
    }
}

/// Composes zero or more plugin-/embedder-contributed [`Curator`]s into the
/// single `Option<Arc<dyn Curator>>` the runtime accepts. Mirrors
/// [`compose_context_hooks`] exactly:
///
/// - Empty -> `None`: the runtime's curator stays unset, and the
///   pre-assembly stage is a zero-cost pass-through, byte-identical to no
///   curator installed (the `context_golden` 11/11 gate's load-bearing
///   guarantee).
/// - One -> that curator directly: no wrapper (the common case -- e.g. a
///   single `conway.memory` install).
/// - More than one -> a [`ComposedCurator`] that chains them: curator B's
///   base is curator A's derived `ValidatedPath`; `Unchanged` passes the
///   current base through; `Failed` stops the chain and returns `Failed`;
///   all-`Unchanged` -> `Unchanged`.
///
/// This is the composition `Plugin::curators`'s own doc names: an
/// embedder's `with_curator` curator first, then each plugin's curators in
/// install order. It keeps the `with_curator` surface working for a
/// standalone curator while letting plugins contribute curation through the
/// SAME `with_plugin`/`install_selected` surface -- no privileged channel
/// (GP-03).
///
/// **No `GuardedCurator` re-validation layer** -- the `Derivation`-only
/// construction IS the guard (DESIGN §11.4): `CurateOutcome::Derived` can
/// only be built from a `Derivation`, which is already the validated,
/// cost-estimated output of `ValidatedPath::derive`. The unrepresentability
/// lives in the type, not a wrapper, so the seam does not need a second
/// guard the way `ContextHook` needed `GuardedContextHook`.
fn compose_curators(curators: Vec<Arc<dyn Curator>>) -> Option<Arc<dyn Curator>> {
    match curators.len() {
        0 => None,
        1 => Some(curators.into_iter().next().expect("len == 1")),
        _ => Some(Arc::new(ComposedCurator::new(curators))),
    }
}

/// Runs a chain of [`Curator`]s in order, feeding each curator's derived
/// `ValidatedPath` to the next as its base. Only constructed by
/// [`compose_curators`] when more than one curator is present; the
/// one-curator case installs that curator directly, so this type's
/// chaining logic is exercised only when an embedder AND a plugin (or two
/// plugins) each contribute a curator.
struct ComposedCurator {
    curators: Vec<Arc<dyn Curator>>,
}

impl ComposedCurator {
    fn new(curators: Vec<Arc<dyn Curator>>) -> Self {
        Self { curators }
    }
}

#[async_trait::async_trait]
impl Curator for ComposedCurator {
    async fn curate(
        &self,
        ctx: &conway_core::ports::CurateCtx,
        base: &conway_core::path::ValidatedPath,
    ) -> CurateOutcome {
        // Start from the base; each curator sees the previous curator's
        // derived path (or the original base if it returned `Unchanged`).
        let mut current: conway_core::path::ValidatedPath = base.clone();
        let mut last_derivation: Option<conway_core::path::Derivation> = None;
        for curator in &self.curators {
            match curator.curate(ctx, &current).await {
                CurateOutcome::Unchanged => {
                    // Pass the current base through to the next curator.
                }
                CurateOutcome::Derived(derivation) => {
                    // Curator B's base is curator A's derived path. Clone
                    // the path into `current` (cheap `Arc<LogRecord>`
                    // clones) so `derivation` stays whole for the return.
                    current = derivation.path.clone();
                    last_derivation = Some(derivation);
                }
                CurateOutcome::Failed { reason } => {
                    // A failing curator stops the chain -- §11.6 -- and the
                    // composed outcome is `Failed` (recorded non-fatally by
                    // the stage, same as a single-curator failure).
                    return CurateOutcome::Failed { reason };
                }
            }
        }
        // If any curator derived, `last_derivation.path == current` by
        // construction (the last `Derived` set both). If none did, every
        // curator returned `Unchanged` and the composed outcome is
        // `Unchanged` -- the stage uses the original path.
        match last_derivation {
            Some(derivation) => CurateOutcome::Derived(derivation),
            None => CurateOutcome::Unchanged,
        }
    }
}

#[cfg(test)]
mod compose_curators_tests {
    //! Covers [`compose_curators`]'s 0/1/2+ branches and [`ComposedCurator`]'s
    //! chaining, which nothing else in the tree drives: the runtime's
    //! `curator_stage` tests exercise ONE curator through the stage, and the
    //! composition that turns N plugin-contributed curators into the single
    //! `Arc<dyn Curator>` the runtime accepts lives here, in this crate,
    //! behind a private fn. Without these, the `Failed`-discards-earlier-
    //! derivations rule and the `last_derivation`-is-the-final-path invariant
    //! could regress silently.

    use super::*;
    use conway_core::ids::{AgentId, LogSeq, ModelId, SessionId};
    use conway_core::log::LogRecord;
    use conway_core::path::{
        NodeProvenance, NodeStamp, PathNode, PathOp, RecordRef, Selector, ValidatedPath,
    };
    use conway_core::ports::CurateCtx;
    use conway_core::provenance::Provenance;
    use conway_core::transcript::TranscriptResolver;
    use conway_testkit::FakeStore;

    fn ts() -> chrono::DateTime<chrono::Utc> {
        "2026-07-20T00:00:00Z".parse().unwrap()
    }

    fn sid() -> SessionId {
        "01ARZ3NDEKTSV4RRFFQ69G5FBV".parse().unwrap()
    }

    /// A two-node base path, both nodes plain `UserTurn`s so `derive`'s
    /// tool-call coherence rules (§4.1) never refuse an `Omit`.
    fn base_path() -> ValidatedPath {
        let nodes = (0..2)
            .map(|i| {
                (
                    PathNode {
                        record: RecordRef {
                            session: sid(),
                            seq: LogSeq(i),
                        },
                        stamp: NodeStamp::Head,
                        prov: NodeProvenance {
                            selected_by: Selector::DefaultRule,
                            at: ts(),
                        },
                    },
                    Arc::new(LogRecord::UserTurn {
                        seq: LogSeq(i),
                        ts: ts(),
                        text: format!("turn {i}"),
                        prov: Provenance::UserPrompt,
                    }),
                )
            })
            .collect();
        ValidatedPath::default_path(nodes)
    }

    fn ctx() -> CurateCtx {
        CurateCtx {
            agent_id: AgentId::new(),
            session_id: sid(),
            turn: 1,
            model: Some(ModelId::new("unrouted")),
            store: Arc::new(FakeStore::new()),
            resolver: Arc::new(TranscriptResolver::new(64)),
        }
    }

    /// Omits the base node at `index`, recording the base length it saw.
    struct OmitAt {
        index: usize,
        base_len_seen: std::sync::Mutex<Option<usize>>,
    }

    impl OmitAt {
        fn new(index: usize) -> Arc<Self> {
            Arc::new(Self {
                index,
                base_len_seen: std::sync::Mutex::new(None),
            })
        }
    }

    #[async_trait::async_trait]
    impl Curator for OmitAt {
        async fn curate(&self, _ctx: &CurateCtx, base: &ValidatedPath) -> CurateOutcome {
            *self.base_len_seen.lock().unwrap() = Some(base.nodes().count());
            let Some(target) = base.nodes().nth(self.index).map(|(n, _)| n.record) else {
                return CurateOutcome::Failed {
                    reason: format!("no node at {}", self.index),
                };
            };
            match base.derive(&[PathOp::Omit { node: target }]) {
                Ok(derivation) => CurateOutcome::Derived(derivation),
                Err(err) => CurateOutcome::Failed {
                    reason: format!("derive refused: {err}"),
                },
            }
        }
    }

    struct Noop;
    #[async_trait::async_trait]
    impl Curator for Noop {
        async fn curate(&self, _ctx: &CurateCtx, _base: &ValidatedPath) -> CurateOutcome {
            CurateOutcome::Unchanged
        }
    }

    struct AlwaysFails;
    #[async_trait::async_trait]
    impl Curator for AlwaysFails {
        async fn curate(&self, _ctx: &CurateCtx, _base: &ValidatedPath) -> CurateOutcome {
            CurateOutcome::Failed {
                reason: "synthetic".into(),
            }
        }
    }

    /// Records whether it ran at all -- proves the chain STOPS on `Failed`.
    struct RecordsThatItRan {
        ran: std::sync::Mutex<bool>,
    }
    #[async_trait::async_trait]
    impl Curator for RecordsThatItRan {
        async fn curate(&self, _ctx: &CurateCtx, _base: &ValidatedPath) -> CurateOutcome {
            *self.ran.lock().unwrap() = true;
            CurateOutcome::Unchanged
        }
    }

    #[test]
    fn compose_of_none_is_none_so_the_stage_stays_a_zero_cost_passthrough() {
        assert!(compose_curators(Vec::new()).is_none());
    }

    #[test]
    fn compose_of_one_installs_it_directly_without_a_wrapper() {
        let only: Arc<dyn Curator> = Arc::new(Noop);
        let composed = compose_curators(vec![only.clone()]).expect("one curator composes to Some");
        // The SAME Arc, not a ComposedCurator wrapping it.
        assert!(Arc::ptr_eq(&composed, &only));
    }

    #[tokio::test]
    async fn chained_second_curator_sees_the_firsts_derived_path_as_its_base() {
        let first = OmitAt::new(1);
        let second = OmitAt::new(0);
        let composed = compose_curators(vec![first.clone(), second.clone()])
            .expect("two curators compose to Some");

        let outcome = composed.curate(&ctx(), &base_path()).await;

        assert_eq!(first.base_len_seen.lock().unwrap().unwrap(), 2);
        // The load-bearing assertion: the second curator's base is the
        // FIRST's derived path (1 node), not the original 2-node base.
        assert_eq!(second.base_len_seen.lock().unwrap().unwrap(), 1);
        match outcome {
            CurateOutcome::Derived(derivation) => {
                assert_eq!(
                    derivation.path.nodes().count(),
                    0,
                    "both omissions applied, so the final derivation is empty"
                );
            }
            other => panic!("expected Derived, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn all_unchanged_composes_to_unchanged() {
        let composed = compose_curators(vec![Arc::new(Noop), Arc::new(Noop)]).expect("Some");
        assert!(matches!(
            composed.curate(&ctx(), &base_path()).await,
            CurateOutcome::Unchanged
        ));
    }

    #[tokio::test]
    async fn a_derive_followed_by_unchanged_keeps_the_derivation() {
        let first = OmitAt::new(1);
        let composed = compose_curators(vec![first, Arc::new(Noop)]).expect("Some");
        match composed.curate(&ctx(), &base_path()).await {
            CurateOutcome::Derived(derivation) => {
                assert_eq!(
                    derivation.path.nodes().count(),
                    1,
                    "the derivation survives"
                );
            }
            other => panic!("expected Derived, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_failure_after_a_derive_discards_the_derivation_and_fails_open() {
        // §11.6: the composed outcome is `Failed`, NOT a half-applied
        // curation -- the stage then proceeds on the ORIGINAL path.
        let composed = compose_curators(vec![OmitAt::new(1), Arc::new(AlwaysFails)]).expect("Some");
        match composed.curate(&ctx(), &base_path()).await {
            CurateOutcome::Failed { reason } => assert_eq!(reason, "synthetic"),
            other => panic!("expected Failed (no half-applied curation), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_failure_stops_the_chain_before_later_curators_run() {
        let later = Arc::new(RecordsThatItRan {
            ran: std::sync::Mutex::new(false),
        });
        let composed = compose_curators(vec![Arc::new(AlwaysFails), later.clone()]).expect("Some");
        let _ = composed.curate(&ctx(), &base_path()).await;
        assert!(
            !*later.ran.lock().unwrap(),
            "a curator after a failing one must not run"
        );
    }
}

/// Resolves a backend's effective API key: the literal `api_key` if set,
/// else the value named by `api_key_env` read from the live process
/// environment, else an empty string (no key configured). `merge::validate`
/// already established the two are mutually exclusive.
///
/// **An `api_key_env` naming an unset variable no longer fails `build()`**
/// (board item `01M163T1KGX3HTCC2YMDPT655J`, correcting `01M163TZTM9BF
/// 40769FRRVXJ33`'s finding that this was a SECOND gate producing the same
/// operator-visible "declining first-run setup still exits" outcome as the
/// empty-backend-map check this function's sibling used to enforce): it
/// resolves to an empty string, exactly as an entry declaring no credential
/// at all already does, and a `tracing::warn!` names the unset variable so
/// the gap is never silent. `crate::backend_usability::classify_entry` is
/// the pre-flight surface an operator actually sees this same condition
/// through (the first-run flow's own trigger, checked BEFORE `build()` ever
/// runs); this function's job is only to stop `build()` itself from
/// refusing over it, so a backend that is not finished being configured
/// still takes its place in a working app rather than taking the whole
/// process down with it. The resulting backend is registered, not silently
/// treated as working: the missing credential fails loud the moment a turn
/// actually reaches it (an ordinary `BackendError::Auth` from the wire, or
/// an equivalent provider-specific rejection -- never a panic, never a
/// silently empty response).
///
/// The key's *shape* is never inspected, same as before.
fn resolve_api_key(id: &str, entry: &BackendEntry) -> String {
    if !entry.api_key.is_empty() {
        return entry.api_key.clone();
    }
    if !entry.api_key_env.is_empty() {
        return std::env::var(&entry.api_key_env).unwrap_or_else(|_| {
            tracing::warn!(
                backend = %id,
                variable = %entry.api_key_env,
                "backend '{id}': api_key_env '{}' is not set in the environment; \
                 registering the backend anyway with no credential -- it will fail \
                 at the wire, naming the failure, the first time a turn reaches it",
                entry.api_key_env,
            );
            String::new()
        });
    }
    String::new()
}

/// Resolves one `[backends.<id>]` entry's `kind` against every registered
/// [`BackendFactory`] (: `kind` is an
/// open name, not a closed enum) -- ONLY against registered factories, with
/// no compiled-in fallback: removed
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
/// [`FacadeError::Config`] listing every kind this build actually
/// recognises -- the same disclosure shape
/// `crates/conway-cli/src/first_party_plugins.rs`'s unknown-id error already
/// produces for plugin ids (a silently ignored `kind` is exactly the
/// failure that check exists to prevent).
///
/// **Two distinct diagnoses for that same failure** , chosen by whether `entry.kind` appears in
/// `declined` ([`ConwayBuilder::with_declined_backend_kinds`]):
/// - present -> a **declined-kind** error: this build recognises the kind by
///   name but a caller deliberately did not attach a factory for it.
/// - absent -> the pre-existing **unknown-kind** error, unchanged: this
///   build has never heard of the kind at all.
///
/// Both are the identical hard `build()`-time [`FacadeError::Config`] this
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
            FacadeError::Config {
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
            FacadeError::Config {
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
/// projects (the single-source guarantee, extended to every registered
/// kind rather than left a built-ins-only privilege), `profile_file_paths`
/// copied verbatim from [`ConwayBuilder::build`]'s own step 2b resolution --
/// see that field's own doc ([`conway_core::ports::BackendBuildContext`]) for
/// why every kind receives the identical list whether or not it reads it --
/// and `extra` cloned verbatim from this same `entry`'s own
/// [`BackendEntry::extra`], never
/// from anywhere else: this is the ONLY place that map is read out of the
/// loaded config and handed onward, closing the gap where it was previously
/// captured at load time and then discarded before any factory saw it.
fn build_backend_context(
    id: &str,
    entry: &BackendEntry,
    metadata: &config::model_metadata::ModelMetadata,
    profile_file_paths: &[PathBuf],
) -> BackendBuildContext {
    let api_key = resolve_api_key(id, entry);
    BackendBuildContext {
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
    }
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

/// Parses the facade's `models.json` `reliability_tier` string (the
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
/// store was injected -- `conway-session` is unlinked entirely in this
/// configuration (it is this crate's only edge to it), so there is no
/// default store to fall back to.
#[cfg(not(feature = "jsonl-store"))]
fn build_default_store(_cwd: &Path, _root: &Path) -> Result<Arc<dyn SessionStore>> {
    Err(FacadeError::Build {
        message: "no session store configured: enable the 'jsonl-store' feature or call \
                  ConwayBuilder::with_session_store"
            .to_string(),
    })
}

/// [`build_default_store`]'s counterpart for the path store (D1-3d-wire):
/// `FsPathStore::open`, in a `paths/` directory ALONGSIDE the session root
/// rather than inside it.
///
/// **Why not co-located in the session root, which was the first attempt.**
/// `FsPathStore` writes `paths-index.jsonl` at the top of whatever root it is
/// given, and `JsonlSessionStore`'s own filenames (`<sid>.jsonl`,
/// `index.jsonl`) do not collide with it -- so sharing looked safe on a
/// filename analysis. It is not, for a reason a filename analysis cannot see:
/// **the session directory is an operator-visible artifact with its own
/// readers.** `conway sessions list`/`show`/`export` enumerate it, and
/// `PHILOSOPHY.md` §1 promises the log is "one file per session ... so will
/// anything else you point at a line-delimited JSON file". A non-session file
/// sitting in there breaks that promise for every reader, not just ours --
/// which is exactly what eight `sessions_*` tests caught by asserting the
/// directory holds exactly one session file.
///
/// A sibling directory keeps both stores' invariants intact and costs
/// nothing.
///
/// **Sibling of `sessions_root` ITSELF, not of its parent** (board item
/// `01M0QK9GRM8HSNWRAR414TCX42` -- a correction to this function's own
/// first cut, caught before landing by actually running it against the
/// real central default rather than only a project-local fixture). The
/// original formula -- `sessions_root.parent().join("paths")` -- silently
/// assumed `sessions_root`'s parent is ALREADY project-exclusive, true of
/// the old fixed default (`<cwd>/.conway/sessions`, parent `<cwd>/.conway`)
/// and of an operator's own explicit `session.root`, but false of the new
/// central, project-keyed default: `~/.conway/sessions/<project-key>/`'s
/// parent is `~/.conway/sessions/`, the ONE directory shared by every
/// project. Every project would have collided on the identical
/// `~/.conway/sessions/paths/` -- confirmed live, not hypothetically: an
/// in-process test that reached this function without isolating
/// `CONWAY_CONFIG_DIR` created exactly that directory under this
/// machine's own real `~/.conway/`. Keying off `sessions_root`'s own file
/// name instead of its parent's fixes the central case and leaves the
/// fixed-default/explicit cases merely relocated (`<cwd>/.conway/
/// sessions-paths` instead of `<cwd>/.conway/paths`) -- a safe, silent
/// rename with no practical migration cost: nothing in this workspace's
/// production code writes through `PathStore` yet (`RuntimeDeps::
/// path_store`'s own doc), so no operator has real data sitting in the old
/// location to lose.
#[cfg(feature = "jsonl-store")]
fn build_default_path_store(cwd: &Path, root: &Path) -> Result<Arc<dyn PathStore>> {
    let sessions_root = resolve_path(cwd, root);
    let paths_root = match sessions_root.file_name() {
        Some(name) => {
            let mut paths_name = std::ffi::OsString::from(name);
            paths_name.push("-paths");
            sessions_root.with_file_name(paths_name)
        }
        // A root with no file name (e.g. `/`) is pathological; fall back to
        // a nested `paths/` so we still never write a stray file into the
        // session directory itself.
        None => sessions_root.join("paths"),
    };
    let path_store = block_on(conway_session::FsPathStore::open(paths_root))?;
    Ok(Arc::new(path_store))
}

/// The `jsonl-store`-off arm, mirroring [`build_default_store`]'s own: no
/// default path store to fall back to once `conway-session` is unlinked.
///
/// **Does not repeat [`build_default_store`]'s "or call
/// `with_session_store`" shape.** That advice is actionable there because
/// `SessionStore` is re-exported through the facade (`conway::SessionStore`).
/// `PathStore` is not (board item `01M0EMCK55628YJXGBQY8YGXHE`, decided
/// deliberately -- see [`ConwayBuilder::with_path_store`]'s own doc), so a
/// facade-only caller cannot name `with_path_store`'s parameter type at all.
/// Naming an escape hatch the reader cannot reach is worse than a plain
/// refusal, so this message states the real constraint instead: with
/// `jsonl-store` off, there is currently no way for a facade-only caller
/// (one depending only on the `conway` crate) to supply a path store, even
/// alongside an injected `with_session_store`. Enabling `jsonl-store` is
/// the only lever such a caller has; `with_path_store` remains real, just
/// reachable only by a caller that also depends on `conway-core` directly.
#[cfg(not(feature = "jsonl-store"))]
fn build_default_path_store(_cwd: &Path, _root: &Path) -> Result<Arc<dyn PathStore>> {
    Err(FacadeError::Build {
        message: "no path store configured: this configuration (jsonl-store disabled) has \
                  no default path store and no way for a facade-only caller to supply one -- \
                  `PathStore` is engine-internal and not re-exported through the `conway` \
                  facade (board item 01M0EMCK55628YJXGBQY8YGXHE). Enable the 'jsonl-store' \
                  feature to get the default FsPathStore; `ConwayBuilder::with_path_store` is \
                  only reachable by a caller that also depends on conway-core directly."
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
    /// `conway_plugin_backends::factory`
    /// -- see that crate's `src/factory.rs` test module for the ported
    /// tests, unchanged in what they check.
    ///
    /// the core proof: `models.json` has exactly one predictable
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

        // What the runtime's T-1 gate reads directly (attempt.rs;
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
