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
//!   otherwise run) is called explicitly after construction — this is not
//!   optional: `api_key_env` is resolved from the live process environment
//!   at `build()` time (a value the earlier `config::load` OAuth check never
//!   saw, since that check only inspected `LoadOptions.env`), so skipping
//!   this call would let a live `sk-ant-oat*` token bypass GP-09's hard
//!   gate.
//! - **`OpenAiCompatConfig.dialect` is parsed by hand** ([`parse_dialect`]),
//!   not via that type's own `serde(rename_all = "snake_case")`
//!   `Deserialize` impl: the facade's documented dialect values
//!   (`"vllm-hermes"`, `"lm-studio"`, `"llamacpp-server"`) are kebab-case,
//!   but `Dialect`'s derived wire form is snake_case (`"vllm_hermes"`, ...),
//!   so a real config file following the documented schema would fail to
//!   deserialize `Dialect` directly.
//! - **The backend map is keyed by each constructed backend's own
//!   `Backend::id()`, not the `backends.<id>` JSON key.**
//!   `AnthropicBackend::id()` unconditionally returns a hardcoded
//!   `BackendId::new("anthropic")` (it has no `id` field to carry a
//!   configured name) — a `conway-backends`-level constraint, out of this
//!   item's file scope to fix at the source. `OpenAiCompatBackend::id()`,
//!   by contrast, faithfully returns the config-provided id (`config.id`,
//!   itself set from the JSON key in [`build_openai_compat`]), so it needs
//!   no equivalent guard. Since `config::merge::validate` checks chain refs
//!   against the JSON key namespace, a mismatched anthropic key would
//!   otherwise pass all config validation and then panic every routed
//!   request in `AttemptEngine::backend_for` (only key `"anthropic"` would
//!   exist in the map); [`build_anthropic`] rejects a non-`"anthropic"` key
//!   with a `ConwayError::Config` at `build()` time instead.
//! - **`config.limits.max_parallel_tools` has no wiring point**: neither
//!   `conway_runtime::runtime::RootSpec` nor `AgentSpec` (which
//!   `Runtime::start_root` builds internally, hardcoding
//!   `DEFAULT_MAX_PARALLEL_TOOLS`) exposes a field this builder or
//!   `Conway::new_session` could set it through. Flagged as a gap for
//!   `MODULE:conway-runtime`, not solved here.
//! - **Facade `ModelMetadataEntry` has no `parallel_tool_calls` or
//!   `structured_output` field** (WI-097's committed schema), so
//!   [`to_capabilities`] defaults both to the most conservative value
//!   (`false` / `StructuredOutput::None`) for every file-derived capability
//!   entry. A role requiring either will only ever be satisfiable via an
//!   injected backend or the optional startup probe overlay, never via
//!   `models.metadata_path` alone.
//! - **Startup capability probing is implemented via
//!   `conway_backends::probe::CapabilityProbe`**, which is `openai-compat`-
//!   feature-gated and only meaningful for `kind = "openai-compat"` backend
//!   entries — there is no equivalent generic mechanism for `anthropic`
//!   entries in this crate (the `Backend::probe()` port method exists but
//!   returns `ProbeReport`, which carries no `max_context_tokens`/capability
//!   data to overlay). `probe_on_startup` therefore only ever affects
//!   `openai-compat` backends; this is disclosed, not silently no-op'd.

#[cfg(any(feature = "anthropic", feature = "openai-compat"))]
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use conway_core::capabilities::{
    CacheMode, Capabilities, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::ids::{BackendId, ModelRef};
use conway_core::ports::{Backend, PermissionGate, Plugin, Router, SessionStore};
use conway_core::routing::ModelOverrides;
use conway_routing::config::HeadroomPolicy;
use conway_routing::{BreakerRegistry, CapabilityIndex, DeclarativeRouter};
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{Runtime, RuntimeDeps};

use crate::agents;
use crate::config::model_metadata::ModelMetadataEntry;
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
#[cfg(feature = "openai-compat")]
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
    warnings: Vec<ConfigWarning>,
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
            warnings: Vec::new(),
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
            warnings,
        } = self;

        // 1. Apply CLI overrides; re-validate. `CliOverrides` has no
        //    `api_key` field, so this can't catch a CLI-supplied oat token --
        //    its real value is catching a pre-existing sk-ant-oat* token in a
        //    config assembled via `from_parts`, which bypasses `load`'s own
        //    OAuth-token check entirely.
        let config = config::merge::apply_cli(&config, &cli_overrides)?;
        let cwd = config.cwd.clone();

        // 2. Load model metadata (facade's local JSON file; missing -> empty).
        let metadata_path = resolve_path(&cwd, &config.models.metadata_path);
        let metadata = config::model_metadata::load(&metadata_path)?;

        // 3+4. Construct config-derived backends, then merge injected ones
        //      over them, keyed by each backend's own `id()`.
        let mut backend_map: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
        for (id, entry) in &config.backends {
            let backend = construct_backend(id, entry, &metadata)?;
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

        // 5. CapabilityIndex from file-derived metadata, optionally
        //    overlaid with a startup probe.
        let mut index_builder = CapabilityIndex::builder();
        for (key, entry) in &metadata.models {
            match key.parse::<ModelRef>() {
                Ok(model_ref) => {
                    index_builder = index_builder.insert(
                        model_ref.backend,
                        model_ref.model,
                        to_capabilities(entry),
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        key = %key,
                        "model metadata key is not a valid 'backend/model' reference; skipping"
                    );
                }
            }
        }
        if config.models.probe_on_startup {
            #[cfg(feature = "openai-compat")]
            {
                index_builder = probe_openai_compat_backends(&config, index_builder);
            }
            #[cfg(not(feature = "openai-compat"))]
            {
                tracing::warn!(
                    "models.probe_on_startup is true but the 'openai-compat' feature is disabled; \
                     no startup probing was performed"
                );
            }
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

        // 10. Plugins: built-ins ++ injected; duplicate manifest ids error.
        let mut resolved_plugins: Vec<Arc<dyn Plugin>> = Vec::new();
        let mut seen_plugin_ids: HashSet<String> = HashSet::new();
        #[cfg(feature = "builtin-tools")]
        {
            for plugin in presets::builtin_plugins() {
                seen_plugin_ids.insert(plugin.manifest().id.clone());
                resolved_plugins.push(plugin);
            }
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

        Ok(Conway::new(rt, config, store, router_explain, warnings))
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
/// Also re-applies GP-09's OAuth-token rejection to whatever `api_key_env`
/// resolves to at `build()` time -- a value `config::merge::validate` never
/// saw, since that check only inspects `LoadOptions.env` at `load()` time.
/// For `kind = "anthropic"` this duplicates `AnthropicConfig::validate()`'s
/// own check (harmless); `kind = "openai-compat"` has no equivalent
/// self-check, so doing it here once, for both kinds, keeps the guard
/// symmetric rather than Anthropic-only.
#[cfg(any(feature = "anthropic", feature = "openai-compat"))]
fn resolve_api_key(id: &str, entry: &BackendEntry) -> Result<String> {
    const OAUTH_TOKEN_PREFIX: &str = "sk-ant-oat";

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
        if resolved.starts_with(OAUTH_TOKEN_PREFIX) {
            return Err(ConwayError::Config {
                path: None,
                message: format!(
                    "backend '{id}': api_key_env '{}' resolves to a Claude subscription OAuth \
                     token (sk-ant-oat*), which is rejected everywhere a direct API key is \
                     required",
                    entry.api_key_env
                ),
            });
        }
        return Ok(resolved);
    }
    Ok(String::new())
}

fn construct_backend(
    id: &str,
    entry: &BackendEntry,
    metadata: &config::model_metadata::ModelMetadata,
) -> Result<Arc<dyn Backend>> {
    match entry.kind {
        BackendKind::Anthropic => build_anthropic(id, entry, metadata),
        BackendKind::OpenaiCompat => build_openai_compat(id, entry, metadata),
    }
}

/// Per-model capability overrides for backend `id`, projected from the
/// facade's loaded `models.json` metadata (keyed `"backend/model"`). The
/// router's T-1 context-fit gate reads `Backend::capabilities`, whose window
/// otherwise falls back to the dialect default — so without wiring the
/// metadata into the backend's own override table here, a `max_context_tokens`
/// set in `models.json` would silently never reach routing (the facade
/// `CapabilityIndex` built from the same metadata is not consulted by that
/// gate).
#[cfg(any(feature = "anthropic", feature = "openai-compat"))]
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
                    reliability_tier: None,
                    parallel_tool_calls: None,
                    min_headroom_tokens: None,
                },
            ))
        })
        .collect()
}

#[cfg(feature = "anthropic")]
fn build_anthropic(
    id: &str,
    entry: &BackendEntry,
    metadata: &config::model_metadata::ModelMetadata,
) -> Result<Arc<dyn Backend>> {
    use conway_backends::anthropic::AnthropicBackend;
    use conway_backends::config::{AnthropicConfig, SecretString};

    // `AnthropicBackend::id()` unconditionally returns `BackendId::new("anthropic")`
    // (it has no `id` field to carry a configured name) -- the backend map in
    // `build()` is keyed by that returned id, not by this JSON key, but
    // `config::merge::validate` checks chain refs (`<backend_id>/<model>`)
    // against the JSON key namespace. A mismatch here would pass all config
    // validation and then panic every routed request in
    // `AttemptEngine::backend_for` (only key "anthropic" would exist in the
    // map). Reject it here instead, at build() time, until
    // `conway-backends` adds an id override for `AnthropicConfig`.
    if id != "anthropic" {
        return Err(ConwayError::Config {
            path: None,
            message: format!(
                "backend '{id}': kind 'anthropic' requires the JSON key to be \
                 'anthropic' (i.e. \"backends\": {{\"anthropic\": {{...}}}}), not '{id}' -- \
                 `AnthropicBackend::id()` always returns the fixed id \"anthropic\" \
                 regardless of the configured key, so any other key would route/backend-lookup \
                 under \"anthropic\" and panic at request time"
            ),
        });
    }

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

#[cfg(not(feature = "anthropic"))]
fn build_anthropic(
    _id: &str,
    _entry: &BackendEntry,
    _metadata: &config::model_metadata::ModelMetadata,
) -> Result<Arc<dyn Backend>> {
    Err(ConwayError::UnsupportedFeature {
        feature: "anthropic",
        message: "backend kind 'anthropic' requires the 'anthropic' cargo feature, which was not \
                  enabled at build time"
            .to_string(),
    })
}

#[cfg(feature = "openai-compat")]
fn build_openai_compat(
    id: &str,
    entry: &BackendEntry,
    metadata: &config::model_metadata::ModelMetadata,
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
    let dialect = parse_dialect(id, dialect_raw)?;
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
        dialect,
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

#[cfg(not(feature = "openai-compat"))]
fn build_openai_compat(
    _id: &str,
    _entry: &BackendEntry,
    _metadata: &config::model_metadata::ModelMetadata,
) -> Result<Arc<dyn Backend>> {
    Err(ConwayError::UnsupportedFeature {
        feature: "openai-compat",
        message: "backend kind 'openai-compat' requires the 'openai-compat' cargo feature, which \
                  was not enabled at build time"
            .to_string(),
    })
}

/// Maps the facade's documented kebab-case dialect strings to
/// `conway_backends::config::Dialect`. Not delegated to that type's own
/// `Deserialize` impl — see the module doc's reconciliation note.
#[cfg(feature = "openai-compat")]
fn parse_dialect(id: &str, raw: &str) -> Result<conway_backends::config::Dialect> {
    use conway_backends::config::Dialect;
    match raw {
        "openai" => Ok(Dialect::OpenAi),
        "ollama" => Ok(Dialect::Ollama),
        "vllm-hermes" => Ok(Dialect::VllmHermes),
        "lm-studio" => Ok(Dialect::LmStudio),
        "llamacpp-server" => Ok(Dialect::LlamaCppServer),
        other => Err(ConwayError::Config {
            path: None,
            message: format!("backend '{id}': unknown dialect '{other}'"),
        }),
    }
}

/// Runs a startup `CapabilityProbe` for every `openai-compat` backend entry,
/// overlaying discovered capabilities over the file-derived ones already in
/// `index_builder`. A backend whose probe observes nothing (`degraded`) or
/// whose entry is missing/invalid config keeps its file-derived metadata
/// unchanged (a `tracing::warn`, never a hard error — probe failure is
/// always a warning per the WI-100 spec).
#[cfg(feature = "openai-compat")]
fn probe_openai_compat_backends(
    config: &ConwayConfig,
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
        let Ok(dialect) = parse_dialect(id, dialect_raw) else {
            tracing::warn!(backend = %id, dialect = %dialect_raw, "probe_on_startup: skipping backend with unknown dialect");
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

        let probe = CapabilityProbe::new(
            base_url,
            dialect,
            auth,
            PROBE_TIMEOUT,
            ModelMetadataStore::defaults(),
            BTreeMap::new(),
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
        for (model_id, caps) in result.capabilities {
            index_builder = index_builder.insert(BackendId::new(id.clone()), model_id, caps);
        }
    }
    index_builder
}

/// Converts the facade's own local `ModelMetadataEntry` (WI-097's JSON
/// schema) into `conway_core`'s `Capabilities`. See the module doc for the
/// disclosed `parallel_tool_calls`/`structured_output` gap.
fn to_capabilities(entry: &ModelMetadataEntry) -> Capabilities {
    Capabilities {
        tool_calling: parse_tool_calling(&entry.tool_calling),
        cache: CacheMode::None,
        parallel_tool_calls: false,
        structured_output: StructuredOutput::None,
        max_context_tokens: entry.max_context_tokens,
        reasoning: entry.reasoning,
        reliability_tier: parse_reliability_tier(&entry.reliability_tier),
    }
}

fn parse_tool_calling(raw: &str) -> ToolCallSupport {
    match raw.to_ascii_lowercase().as_str() {
        "none" => ToolCallSupport::None,
        "non_streaming" | "non_streaming_only" => ToolCallSupport::NonStreamingOnly,
        "streaming" => ToolCallSupport::Streaming { validated: false },
        "streaming_validated" => ToolCallSupport::Streaming { validated: true },
        other => {
            tracing::warn!(
                value = %other,
                "unknown model metadata tool_calling value; treating as no tool-call support"
            );
            ToolCallSupport::None
        }
    }
}

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
#[cfg(any(feature = "jsonl-store", feature = "openai-compat"))]
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

#[cfg(all(test, any(feature = "anthropic", feature = "openai-compat")))]
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
}
