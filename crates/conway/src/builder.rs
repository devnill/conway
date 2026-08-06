//! `ConwayBuilder`: assembles a validated [`crate::config::ConwayConfig`]
//! plus optional injected ports into a live [`crate::conway::Conway`]
//! (WI-100). This is the wiring layer — it contains no agent logic.
//!
//! ## Reconciliations against the binding spec (disclosed, not worked around)
//!
//! - **`build(self) -> Result<Conway>` is synchronous** (the WI-100 golden
//!   end-to-end criterion chains `.build()?.new_session(..).await?` — `?`
//!   with no `.await` on `build()`), but the default session store
//!   (`conway_session::JsonlSessionStore::open`) and the optional startup
//!   capability probe (`conway_backends::probe::CapabilityProbe::discover_result`)
//!   are both `async fn`s that perform real I/O. `build()` bridges this by
//!   running that one `async` call to completion on a fresh OS thread with
//!   its own throwaway current-thread `tokio` runtime ([`block_on`]) rather
//!   than via `Handle::current().block_on(..)`, which panics when `build()`
//!   is (as it commonly will be) invoked from inside an existing `tokio`
//!   task. This still briefly blocks whichever thread calls `build()` when
//!   it needs to construct a real store or run a probe; embedders that call
//!   `build()` from an async context and care about that should do so via
//!   `spawn_blocking`. This is a load-bearing, disclosed deviation forced by
//!   the sync/async mismatch between the golden criterion and the lower
//!   crates' committed `async` signatures — not an oversight.
//! - **No `with_prompt_handler` method exists** (the criteria list
//!   `ConwayBuilder`'s methods "exactly", and that list has no such
//!   method), so `gates::from_config` is always called with `prompt_handler:
//!   None`. Since `permissions.mode` defaults to `"prompt"`, an embedder
//!   using an unmodified default config and no `with_permission_gate`
//!   override will get `ConwayError::Config` from `build()` — flagged as a
//!   gap in this item's own public surface (the CLI or a future item should
//!   likely add a way to supply a prompt handler) rather than silently
//!   adding an undocumented method.
//! - **`AnthropicBackend`/`OpenAiCompatBackend` construction bypasses the
//!   private `AnthropicConfigRaw`/serde `TryFrom` path**: every field on
//!   both `conway_backends::config::{AnthropicConfig, OpenAiCompatConfig}`
//!   is `pub`, so this module builds each directly via a struct literal
//!   instead of round-tripping through a synthesized JSON document.
//!   `AnthropicConfig::validate()` (which the private `TryFrom` path would
//!   otherwise run) is called explicitly after construction: `api_key_env`
//!   is resolved from the live process environment at `build()` time, a
//!   value the earlier `config::load` never saw (it only inspected
//!   `LoadOptions.env`).
//! - **`OpenAiCompatConfig.profile` is resolved by hand**
//!   ([`resolve_profile`]), not via `Profile`'s own `Deserialize` impl: a
//!   backend entry's `dialect` string names a *profile id* (built-in or
//!   loaded from `.conway/profiles.toml` — declarative provider profiles
//!   item), resolved against a `conway_backends::profile::ProfileStore`
//!   this module assembles once per `build()` call
//!   ([`load_provider_profiles`]). The facade's three historically
//!   kebab-case dialect strings (`"vllm-hermes"`, `"lm-studio"`,
//!   `"llamacpp-server"`) are translated to their snake_case built-in
//!   profile ids (`"vllm_hermes"`, `"lm_studio"`, `"llama_cpp_server"`)
//!   before the `ProfileStore` lookup, preserving every existing config
//!   file unchanged; every other string (`"openai"`, `"ollama"`, `"kimi"`,
//!   or any id a user-supplied profile file declares) is looked up
//!   verbatim — this is what makes a new provider selectable with no
//!   recompile.
//! - **The backend map is keyed by each constructed backend's own
//!   `Backend::id()`, which both adapters set from the `backends.<id>` JSON
//!   key.** `config::merge::validate` checks chain refs
//!   (`<backend_id>/<model>`) against that same key namespace, so the two
//!   agree by construction. `AnthropicConfig` gained an `id` field for
//!   this: an Anthropic-compatible third-party endpoint can be named for
//!   what it is (`kimi`) and coexist with a real `anthropic` backend,
//!   rather than every such config having to squat the key `"anthropic"`.
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
//!   scope — see `conway_backends::capabilities`'s module doc and this
//!   item's scope-boundary note.
//! - **Startup capability probing is implemented via
//!   `conway_backends::probe::CapabilityProbe`**, which is only meaningful
//!   for `kind = "openai-compat"` backend entries — there is no equivalent
//!   generic mechanism for `anthropic`
//!   entries in this crate (the `Backend::probe()` port method exists but
//!   returns `ProbeReport`, which carries no `max_context_tokens`/capability
//!   data to overlay). `probe_on_startup` therefore only ever affects
//!   `openai-compat` backends; this is disclosed, not silently no-op'd.
//!   [`probe_openai_compat_backends`] constructs each backend's
//!   `CapabilityProbe` with the *same* `models_overrides_for(id, metadata)`
//!   map the backend itself is built with (not an empty one): `models.json`
//!   wins outright, in both directions, for every model it lists — a probed
//!   window can neither mask a smaller operator-declared one nor be masked
//!   by a larger one the operator explicitly widened past what the probe
//!   observed. This makes the `CapabilityIndexBuilder::insert` overlay a
//!   verified no-op for `models.json`-listed pairs: `build_capabilities`
//!   (`conway-backends`) is fed byte-identical `overrides` on both the probe
//!   side and the `Backend::capabilities()` side, so equal inputs yield
//!   equal outputs and the router's index ends up exactly what
//!   `Backend::capabilities()` — and therefore the T-1 gate — would return
//!   for the same pair, restoring the single-source guarantee the bullet
//!   above already describes for the unprobed path.
//! - **The probe may confirm a declared model; it may never introduce one**
//!   (RESTRICT, DECIDED — see [`probe_openai_compat_backends`]'s own
//!   comment for the full reasoning). A server can report models
//!   `models.json` never named at all; [`probe_openai_compat_backends`]'s
//!   overlay loop drops every such pair rather than inserting it into
//!   `index_builder`, so `models.json` stays the sole source of which
//!   `(backend, model)` pairs are routable at all — `probe_on_startup`
//!   narrows/confirms capabilities for declared models, it never expands
//!   the declared set.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use conway_core::capabilities::ReliabilityTier;
use conway_core::ids::{BackendId, ModelRef};
use conway_core::ports::{Backend, ContextHook, PermissionGate, Plugin, Router, SessionStore};
use conway_core::routing::ModelOverrides;
use conway_routing::config::HeadroomPolicy;
use conway_routing::{BreakerRegistry, CapabilityIndex, DeclarativeRouter};
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{Runtime, RuntimeDeps};

use crate::agents;
use crate::config::schema::{BackendEntry, BackendKind, ConwayConfig};
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

/// Per-discovery-request timeout for the optional startup capability probe.
/// Mirrors `conway_backends::probe::DISCOVERY_TIMEOUT` (private to that
/// crate) rather than importing it.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Which built-in plugins [`ConwayBuilder::build`] auto-registers, filtered
/// by each candidate's own `PluginManifest::id` (board item: bash ships on
/// by default and cannot be declined).
///
/// **This is a generic, id-keyed predicate over a *bundle* of candidate
/// plugins -- it is not bash-specific and carries no built-in-vs-third-party
/// distinction of its own** (GP-03: "a third-party plugin and a built-in
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
/// per-plugin declaration GP-03 requires of a third party -- nothing about
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
    /// WI-126. `None` (the default) means `build()` never calls
    /// `Runtime::set_context_hook` at all, leaving every agent's
    /// `context_hook` at the `Runtime`-constructed default of `None` --
    /// i.e. today's behavior, unchanged.
    context_hook: Option<Arc<dyn ContextHook>>,
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
            context_hook: None,
            builtin_selection: None,
            warnings: Vec::new(),
            root: None,
        }
    }

    fn with_warnings(mut self, warnings: Vec<ConfigWarning>) -> Self {
        self.warnings = warnings;
        self
    }

    /// Injects a backend. Takes precedence over any config-derived backend
    /// with the same `Backend::id()`.
    pub fn with_backend(mut self, backend: Arc<dyn Backend>) -> Self {
        self.backends.push(backend);
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

    /// Overrides the default `DeclarativeRouter`. `Conway::explain_routing`
    /// falls back to a degraded (empty) report when this is set, since
    /// `conway_routing::RoutingExplain` only projects a concrete
    /// `DeclarativeRouter`, not the `Router` trait object.
    pub fn with_router(mut self, router: Arc<dyn Router>) -> Self {
        self.router = Some(router);
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
            context_hook,
            builtin_selection,
            warnings,
            root,
        } = self;

        // 1. Apply CLI overrides; re-validate. This is what catches an
        //    invalid override in a config assembled via `from_parts`, which
        //    bypasses `load`'s own validation entirely.
        let config = config::merge::apply_cli(&config, &cli_overrides)?;
        let cwd = config.cwd.clone();

        // 2. Load model metadata (facade's local JSON file; missing -> empty).
        let metadata_path = resolve_path(&cwd, &config.models.metadata_path);
        let metadata = config::model_metadata::load(&metadata_path)?;

        // 2b. Declarative provider profiles: built-ins layered under any
        //     discovered `.conway/profiles.toml` (project then global —
        //     `config::discovery::provider_profile_file_paths`). Resolved
        //     once here so every `openai-compat` backend entry (and startup
        //     probing, step 5) sees the same loaded set.
        let profiles = load_provider_profiles(&cwd)?;

        // 3+4. Construct config-derived backends, then merge injected ones
        //      over them, keyed by each backend's own `id()`.
        let mut backend_map: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
        for (id, entry) in &config.backends {
            let backend = construct_backend(id, entry, &metadata, &profiles)?;
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
        //    overlaid with a startup probe.
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
        if config.models.probe_on_startup {
            index_builder =
                probe_openai_compat_backends(&config, &profiles, &metadata, index_builder);
        }
        let capability_index = index_builder.build();

        // 6. BreakerRegistry from config.health (via ConwayConfig::routing()).
        let routing_config = config.routing().map_err(|message| ConwayError::Config {
            path: None,
            message,
        })?;
        let headroom_policy = HeadroomPolicy::from_routing_config(&routing_config);
        let health: Arc<dyn conway_core::ports::HealthRegistry> =
            BreakerRegistry::new(routing_config.health);

        // 7. Router: injected, else a freshly compiled DeclarativeRouter.
        //    The concrete instance is kept alongside the type-erased one so
        //    `Conway::explain_routing` can still project through it.
        let (router, router_explain): (Arc<dyn Router>, Option<Arc<DeclarativeRouter>>) =
            match router {
                Some(router) => (router, None),
                None => {
                    let compiled = Arc::new(
                        DeclarativeRouter::new(
                            routing_config,
                            headroom_policy.clone(),
                            health.clone(),
                            capability_index,
                        )
                        .map_err(|issues| ConwayError::Build {
                            message: format!("routing config invalid: {issues:?}"),
                        })?,
                    );
                    (compiled.clone() as Arc<dyn Router>, Some(compiled))
                }
            };

        // 8. Store: injected, else JsonlSessionStore::open (jsonl-store
        //    feature), else a Build error.
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

fn construct_backend(
    id: &str,
    entry: &BackendEntry,
    metadata: &config::model_metadata::ModelMetadata,
    profiles: &conway_backends::profile::ProfileStore,
) -> Result<Arc<dyn Backend>> {
    match entry.kind {
        BackendKind::Anthropic => build_anthropic(id, entry, metadata),
        BackendKind::OpenaiCompat => build_openai_compat(id, entry, metadata, profiles),
    }
}

/// Declarative provider profiles: built-ins ([`conway_backends::profile::ProfileStore::built_ins`])
/// layered under any discovered `.conway/profiles.toml` file(s) — project
/// then global (`config::discovery::provider_profile_file_paths`). Reads
/// the live process environment directly (`std::env::vars()`), matching
/// [`resolve_api_key`]'s own precedent of touching real env for exactly
/// this kind of build()-time resolution rather than threading an injected
/// map through every caller.
fn load_provider_profiles(cwd: &Path) -> Result<conway_backends::profile::ProfileStore> {
    use conway_backends::profile::ProfileStore;

    let env: HashMap<String, String> = std::env::vars().collect();
    let mut store = ProfileStore::built_ins();
    for path in config::discovery::provider_profile_file_paths(cwd, &env) {
        store = store.merge_file(&path).map_err(|e| ConwayError::Config {
            path: Some(path.clone()),
            message: format!(
                "failed to load provider profiles from {}: {e}",
                path.display()
            ),
        })?;
    }
    Ok(store)
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

fn build_anthropic(
    id: &str,
    entry: &BackendEntry,
    metadata: &config::model_metadata::ModelMetadata,
) -> Result<Arc<dyn Backend>> {
    use conway_backends::anthropic::AnthropicBackend;
    use conway_backends::config::{AnthropicConfig, SecretString};

    // The configured JSON key becomes the backend's own id, so a chain ref
    // (`<backend_id>/<model>`) resolves against the same namespace
    // `config::merge::validate` checked. This is what lets an
    // Anthropic-compatible third-party endpoint be named for what it is
    // (`kimi`) and coexist with a real `anthropic` backend.
    let api_key = resolve_api_key(id, entry)?;
    let base_url = if entry.base_url.is_empty() {
        url::Url::parse("https://api.anthropic.com")
            .expect("hardcoded default Anthropic base URL must be valid")
    } else {
        url::Url::parse(&entry.base_url).map_err(|e| ConwayError::Config {
            path: None,
            message: format!("backend '{id}': invalid base_url: {e}"),
        })?
    };

    let cfg = AnthropicConfig {
        api_key: SecretString::new(api_key),
        // The JSON key is the backend's identity, so a chain ref resolves
        // against the namespace `config::merge::validate` already checked.
        id: BackendId::new(id),
        base_url,
        // Not exposed by the facade schema; mirrors
        // `conway_backends::config`'s own (private) default literal.
        anthropic_version: "2023-06-01".to_string(),
        timeout: None,
        models: models_overrides_for(id, metadata),
    };
    cfg.validate().map_err(|e| ConwayError::Config {
        path: None,
        message: format!("backend '{id}': {e}"),
    })?;

    let backend = AnthropicBackend::new(cfg).map_err(|e| ConwayError::Config {
        path: None,
        message: format!("backend '{id}': {e}"),
    })?;
    Ok(Arc::new(backend))
}

fn build_openai_compat(
    id: &str,
    entry: &BackendEntry,
    metadata: &config::model_metadata::ModelMetadata,
    profiles: &conway_backends::profile::ProfileStore,
) -> Result<Arc<dyn Backend>> {
    use conway_backends::config::{OpenAiCompatConfig, SecretString};
    use conway_backends::openai_compat::OpenAiCompatBackend;

    let dialect_raw = entry
        .dialect
        .as_deref()
        .ok_or_else(|| ConwayError::Config {
            path: None,
            message: format!("backend '{id}': kind 'openai-compat' requires 'dialect'"),
        })?;
    let profile = resolve_profile(id, dialect_raw, profiles)?;
    let api_key = resolve_api_key(id, entry)?;
    let base_url = url::Url::parse(&entry.base_url).map_err(|e| ConwayError::Config {
        path: None,
        message: format!("backend '{id}': invalid base_url: {e}"),
    })?;

    let cfg = OpenAiCompatConfig {
        id: BackendId::new(id.to_string()),
        base_url,
        api_key: if api_key.is_empty() {
            None
        } else {
            Some(SecretString::new(api_key))
        },
        profile,
        timeout: None,
        metadata_path: None,
        models: models_overrides_for(id, metadata),
    };

    let backend = OpenAiCompatBackend::new(cfg).map_err(|e| ConwayError::Config {
        path: None,
        message: format!("backend '{id}': {e}"),
    })?;
    Ok(Arc::new(backend))
}

/// Resolves the facade's `backends.<id>.dialect` string to a
/// [`conway_backends::profile::Profile`] against `profiles` (declarative
/// provider profiles item). The three dialects whose documented facade
/// spelling is kebab-case (`"vllm-hermes"`, `"lm-studio"`,
/// `"llamacpp-server"`) are translated to their snake_case built-in profile
/// ids first — preserving every existing config file unchanged — then
/// looked up verbatim; every other string (`"openai"`, `"ollama"`,
/// `"kimi"`, or any id a `.conway/profiles.toml` file declares) is looked
/// up as-is. This is what lets a new provider be selected by name with no
/// recompile: adding it to `profiles` is enough, no change to this
/// function is ever required.
fn resolve_profile(
    id: &str,
    raw: &str,
    profiles: &conway_backends::profile::ProfileStore,
) -> Result<conway_backends::profile::Profile> {
    let canonical = match raw {
        "vllm-hermes" => "vllm_hermes",
        "lm-studio" => "lm_studio",
        "llamacpp-server" => "llama_cpp_server",
        other => other,
    };
    profiles
        .get(canonical)
        .cloned()
        .ok_or_else(|| ConwayError::Config {
            path: None,
            message: format!(
                "backend '{id}': unknown dialect/profile '{raw}' (no built-in or loaded profile \
                 named '{canonical}')"
            ),
        })
}

/// Runs a startup `CapabilityProbe` for every `openai-compat` backend entry,
/// overlaying discovered capabilities over the file-derived ones already in
/// `index_builder`. A backend whose probe observes nothing (`degraded`) or
/// whose entry is missing/invalid config keeps its file-derived metadata
/// unchanged (a `tracing::warn`, never a hard error — probe failure is
/// always a warning per the WI-100 spec).
///
/// `metadata` is the facade's already-loaded `models.json` (step 2 of
/// [`ConwayBuilder::build`]); [`models_overrides_for`] projects it into the
/// exact same `BTreeMap<String, ModelOverrides>` shape each backend's own
/// config is built with (see `build_openai_compat`'s `models:
/// models_overrides_for(id, metadata)` field). Passing that same map into
/// `CapabilityProbe::new` here — rather than an empty one — is what makes
/// the probe's own merge precedence (this module's doc, `probe.rs`'s: config
/// `ModelOverrides` > `ModelMetadata` entry > probed server value >
/// `DialectDefaults`) agree with `Backend::capabilities()`'s: for every
/// `models.json`-listed model, `build_capabilities` is fed byte-identical
/// inputs on both sides, so the overlay below becomes a verified no-op
/// wherever `models.json` already has an opinion — see the module doc's
/// `CapabilityIndex`/`Backend::capabilities()` reconciliation note.
fn probe_openai_compat_backends(
    config: &ConwayConfig,
    profiles: &conway_backends::profile::ProfileStore,
    metadata: &config::model_metadata::ModelMetadata,
    mut index_builder: conway_routing::CapabilityIndexBuilder,
) -> conway_routing::CapabilityIndexBuilder {
    use conway_backends::config::SecretString;
    use conway_backends::model_metadata::ModelMetadataStore;
    use conway_backends::probe::CapabilityProbe;

    for (id, entry) in &config.backends {
        if !matches!(entry.kind, BackendKind::OpenaiCompat) {
            continue;
        }
        let Some(dialect_raw) = entry.dialect.as_deref() else {
            tracing::warn!(backend = %id, "probe_on_startup: skipping backend with no 'dialect'");
            continue;
        };
        let Ok(profile) = resolve_profile(id, dialect_raw, profiles) else {
            tracing::warn!(backend = %id, dialect = %dialect_raw, "probe_on_startup: skipping backend with unknown dialect/profile");
            continue;
        };
        let Ok(base_url) = url::Url::parse(&entry.base_url) else {
            tracing::warn!(backend = %id, "probe_on_startup: skipping backend with invalid base_url");
            continue;
        };
        let auth = resolve_api_key(id, entry)
            .ok()
            .filter(|key| !key.is_empty())
            .map(SecretString::new);

        // Bound so the admission filter below can reuse the exact same
        // backend-scoped, `ModelRef`-normalized key set the probe itself
        // was constructed with, rather than re-deriving it (and risking a
        // second, subtly different notion of "declared for this backend").
        let overrides = models_overrides_for(id, metadata);
        let probe = CapabilityProbe::new(
            base_url,
            profile,
            auth,
            PROBE_TIMEOUT,
            // Matches the backend's own store (`openai_compat/mod.rs`'s
            // `metadata_path: None`) — the facade's `models.json` reaches
            // the probe exclusively through `overrides` below, not through
            // this store.
            ModelMetadataStore::defaults(),
            overrides.clone(),
        );
        let result = block_on(probe.discover_result());
        if result.degraded {
            tracing::warn!(
                backend = %id,
                "probe_on_startup: capability discovery observed no models; keeping file-derived \
                 metadata"
            );
            continue;
        }
        // RESTRICT (DECIDED, operator direction 2026-08-06: "Keep
        // configuration something done by hand and have the probe confirm
        // that the model works, nothing else."): only overlay a pair
        // `models.json` already declares for this backend. `probe.rs`'s own
        // module doc states the merge precedence as config `ModelOverrides`
        // > `ModelMetadata` entry > probed server value > `DialectDefaults`,
        // and that discovery may only *narrow* `max_context_tokens` — never
        // raise `tool_calling`, never set `reliability_tier` to `Verified`.
        // Inserting a pair for a model `models.json` never named is the
        // largest possible raise the probe could make: from not-routable-
        // at-all to routable, on the strength of the server's own say-so
        // alone. That is exactly the opaque, server-driven admission
        // `probe.rs`'s contract already forbids in every other direction,
        // and exactly what GP-07 ("no opaque auto-selection in the core")
        // rules out — a model becoming routable because a server mentioned
        // it, with no operator declaration behind it, is not a "route" a
        // user could have predicted from `models.json` alone. `models.json`
        // is the sole hand-written source of truth (same principle as prior
        // decision 01KZ50GM85GF0TPNBYCNXYAS9Z: for a *listed* pair,
        // `models.json` wins outright over the probe in both directions —
        // this is that principle at its boundary, where no declaration
        // means nothing for the probe to confirm). A pair the probe
        // observed but `models.json` never listed is silently dropped, not
        // inserted, and not surfaced as a hard error — `discover_result`
        // itself treats absent-server-observation the same way (a probe
        // failure is always a warning, never fatal, per `CapabilityProbe`'s
        // own doc) — but it IS logged at `debug` below so an operator who
        // enabled `probe_on_startup` and expected discovery to pick up an
        // undeclared model has a signal for why it never became routable.
        for (model_id, caps) in result.capabilities {
            if !overrides.contains_key(model_id.as_str()) {
                tracing::debug!(
                    backend = %id,
                    model = %model_id,
                    "probe_on_startup: server reported a model with no models.json entry for \
                     this backend; not admitting it (models.json is the sole source of \
                     routable models)"
                );
                continue;
            }
            index_builder = index_builder.insert(BackendId::new(id.clone()), model_id, caps);
        }
    }
    index_builder
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

    /// Declarative provider profiles: `resolve_profile` accepts every
    /// existing documented dialect string (both plain and the three
    /// kebab-case spellings), resolves a brand-new built-in profile
    /// (`kimi`) by name with no special-casing, resolves a user-supplied
    /// profile id with no recompile, and rejects an unknown name with a
    /// named, typed error rather than a panic.
    #[test]
    fn resolve_profile_accepts_every_documented_dialect_string_and_new_built_ins() {
        use conway_backends::profile::ProfileStore;

        let profiles = ProfileStore::built_ins();
        for (raw, expected_id) in [
            ("openai", "openai"),
            ("ollama", "ollama"),
            ("vllm-hermes", "vllm_hermes"),
            ("lm-studio", "lm_studio"),
            ("llamacpp-server", "llama_cpp_server"),
            ("kimi", "kimi"),
        ] {
            let profile = resolve_profile("test", raw, &profiles)
                .unwrap_or_else(|e| panic!("'{raw}' must resolve: {e}"));
            assert_eq!(profile.id, expected_id);
        }
    }

    #[test]
    fn resolve_profile_resolves_a_user_supplied_profile_with_no_recompile() {
        use conway_backends::profile::ProfileStore;

        let dir = std::env::temp_dir().join(format!(
            "conway-builder-resolve-profile-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("profiles.toml");
        std::fs::write(
            &path,
            r#"
            [[profile]]
            id = "my-vendor"
            chat_path = "/chat/completions"
            "#,
        )
        .unwrap();

        let profiles = ProfileStore::built_ins().merge_file(&path).unwrap();
        let profile = resolve_profile("test", "my-vendor", &profiles).unwrap();
        assert_eq!(profile.id, "my-vendor");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_profile_names_the_unknown_dialect_in_a_typed_error() {
        use conway_backends::profile::ProfileStore;

        let profiles = ProfileStore::built_ins();
        let err = resolve_profile("mybackend", "totally-unknown", &profiles)
            .expect_err("an unknown dialect/profile must be rejected");
        match err {
            ConwayError::Config { message, .. } => {
                assert!(message.contains("mybackend"), "{message}");
                assert!(message.contains("totally-unknown"), "{message}");
            }
            other => panic!("expected ConwayError::Config, got {other:?}"),
        }
    }

    /// WI-123's core proof: `models.json` has exactly one predictable
    /// routing effect. The value `Backend::capabilities()` returns (what
    /// `conway_runtime::attempt::AttemptEngine`'s T-1 gate reads directly)
    /// and the value the router's `CapabilityIndex` resolves for the same
    /// pair (built via `CapabilityIndex::from_backends`, step 5 of
    /// `ConwayBuilder::build`) must be identical -- not two independently
    /// recomputed values that can silently drift apart.
    #[test]
    fn models_json_drives_both_backend_capabilities_and_router_index_identically() {
        use conway_backends::config::{Dialect, OpenAiCompatConfig};
        use conway_backends::openai_compat::OpenAiCompatBackend;
        use conway_core::ids::ModelId;

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
