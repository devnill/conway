//! The shortest configuration this crate can express today for "route
//! straight to inference: no tools, no agent behaviour, one turn, out" --
//! assembled using only mechanisms a third party also has: `ConwayConfig`
//! fields and `ConwayBuilder` methods, no more. No new entry point and no
//! method that shortcuts the harness were added to build this -- every call
//! below still goes through `ConwayBuilder::build` -> `Conway::new_session`
//! -> `SessionHandle::prompt`, exactly like every other caller (including
//! the interactive TUI).
//!
//! Runs fully offline against fakes (`conway_core::fakes`), like this
//! crate's other example:
//!
//! ```console
//! cargo run -p conway --example bare_inference
//! ```
//!
//! ## What this proves, and what it does not
//!
//! It proves a bare-inference configuration IS reachable through the
//! composition surface conway already ships -- nothing here required a new
//! API. It does not prove that reaching it is *easy*: read
//! `config_with_tools` and `bare_inference_config` below for the ceremony
//! that stood in the way, spelled out where the code itself pays for it
//! (also written up in full in this item's completion report, board item
//! 01M00QGJEF40GGHP6SAD6Z8Z6H).
//!
//! One finding is demonstrated in code, not just asserted, in `main`: it
//! first builds a `Conway` from this crate's OTHER example's exact config
//! shape (`ToolsConfig::default()`, unmodified -- see `minimal_session.rs`'s
//! own `minimal_config()`) and shows the `report` tool IS registered on it,
//! even though that example asks for nothing agent-shaped at all. It then
//! builds this file's actual bare-inference `Conway` -- identical in every
//! other respect -- with `tools.builtin_plugins` set to the empty vec, and
//! shows `report` is NOT registered there. Turning tools off is possible;
//! it is just never the default, and nothing about constructing a
//! `ConwayConfig` by hand forces a caller to know to do it.

use std::collections::BTreeMap;
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionMode, PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig,
    ToolsConfig, TuiSection,
};
use conway::{ConwayBuilder, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::fakes::{FakeBackend, FakeGate, FakeRouter, FakeStore};
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias, ToolName};
use conway_core::ports::SessionStore;

/// The one-role, no-backend-table config shape `ConwayBuilder::from_parts`
/// needs -- byte-for-byte `minimal_session.rs`'s own `minimal_config()`,
/// parameterized over `tools` so `main` can build the same config twice:
/// once with the default `[tools]` section, once with it emptied.
///
/// **This is the first thing that stood in the way.** `ConwayConfig` has no
/// `#[derive(Default)]` (`crate::builder`'s own module doc: `default_role`
/// has no sensible built-in value) even though every one of its OTHER
/// thirteen field types does derive `Default` -- so there is no
/// `..Default::default()` shortcut available, and a caller assembling a
/// bare config by hand states all fourteen fields every time, regardless of
/// how few of them it actually wants to change from their defaults.
fn config_with_tools(tools: ToolsConfig) -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "default".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    ConwayConfig {
        default_role: RoleAlias::new("default"),
        cwd: std::path::PathBuf::from("."),
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        // `permissions.mode = "prompt"` (`PermissionsConfig::default()`'s
        // own value) requires a prompt handler, and no `with_prompt_handler`
        // method exists on `ConwayBuilder` at all (`crate::builder`'s own
        // module doc), so an unmodified default config never builds without
        // a `with_permission_gate` override.
        //
        // The crate's OWN shipped preset for exactly this situation,
        // `presets::default_permissions_for_one_shot` (allow-list mode, an
        // empty `allowed_tools`), looked like the answer -- until it turned
        // out to be a second, sharper blocker instead: `config::merge::
        // validate`'s check 3 hard-rejects `mode = "allowlist"` paired with
        // an EMPTY `allowed_tools` (`"requires a non-empty allowed_tools
        // list"`), and `ConwayBuilder::build` always calls that validator
        // (`config::merge::apply_cli`, its own step 1) even for a config
        // built via `from_parts`. `default_permissions_for_one_shot` and
        // `merge::validate` disagree about whether an empty `allowed_tools`
        // is legal -- so the preset this crate ships specifically for a
        // tool-free build cannot itself build, unconditionally, for any
        // caller who uses it unmodified. Reported, not fixed here (see this
        // item's own report); worked around below with `PermissionMode::
        // Deny` instead, which needs no list and is exactly as inert given
        // zero tools are ever registered.
        permissions: PermissionsConfig {
            mode: PermissionMode::Deny,
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
        },
        backends: BTreeMap::new(),
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig::default(),
        tui: TuiSection::default(),
        tools,
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

/// The actual bare-inference config: `config_with_tools` with `[tools]`
/// emptied -- `tools.builtin_plugins: vec![]` -- so `ConwayBuilder::build`'s
/// plugin-selection step (`PluginSelection::Only(config.tools.
/// builtin_plugins.clone())`, its own default derivation when
/// `with_builtin_plugins` is never called) matches nothing.
///
/// **No `ConwayBuilder::with_builtin_plugins` call is needed.** The empty
/// list alone is enough, and it is a config FIELD, not a builder METHOD --
/// expressible from a `settings.json` a third party writes by hand, not
/// only from Rust code linking this crate directly. That is the one place
/// this composition surface already gets "no tools" right: it does not
/// require a code-level override, only a value the schema already accepts.
fn bare_inference_config() -> ConwayConfig {
    config_with_tools(ToolsConfig {
        builtin_plugins: Vec::new(),
    })
}

#[tokio::main]
async fn main() -> conway::Result<()> {
    let backend = Arc::new(FakeBackend::echo(BackendId::new("fake")));
    let route = ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    };

    // --- Finding, demonstrated: an UNMODIFIED `ToolsConfig::default()` ---
    // (exactly what `minimal_session.rs`'s own `minimal_config()` uses)
    // registers three built-in tool plugins on its own, whether or not the
    // caller wants an agent at all. A permission gate is required here too
    // (`permissions.mode` defaults to "prompt") purely so this comparison
    // build succeeds -- it plays no role in the finding below, which is
    // about `tools`, not `permissions`.
    let with_default_tools = ConwayBuilder::from_parts(config_with_tools(ToolsConfig::default()))
        .with_backend(backend.clone())
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_router(Arc::new(FakeRouter::single(route.clone())))
        .build()?;
    assert!(
        with_default_tools
            .tool_render_kind(&ToolName::new("report"))
            .is_some(),
        "ToolsConfig::default() should register the 'report' tool -- if this \
         assertion fails, the default has changed and this example's finding \
         is stale"
    );
    println!(
        "with ToolsConfig::default() (minimal_session.rs's own config shape): \
         'report' tool IS registered -- an agent-shaped surface exists \
         whether or not anything asked for one"
    );

    // --- The bare-inference build: no tools, one turn, out. ---
    let store = Arc::new(FakeStore::new());
    let bare = ConwayBuilder::from_parts(bare_inference_config())
        .with_backend(backend)
        .with_session_store(store.clone())
        .with_router(Arc::new(FakeRouter::single(route)))
        .build()?;
    assert!(
        bare.tool_render_kind(&ToolName::new("report")).is_none(),
        "bare_inference_config() should register no tools at all"
    );
    println!("with bare_inference_config(): 'report' tool is NOT registered");

    // One turn, out. `SessionSpec::default()`'s `keep_alive: false` already
    // means this session's root agent task exits after this single turn --
    // no separate "single-turn mode" is needed on top of "no tools": with
    // nothing registered to call, the model has nothing to loop on, and the
    // session does not outlive its own first `Completed` turn either.
    let session = bare.new_session(SessionSpec::default()).await?;
    let turn = session.prompt("Hello, conway!").await?;
    println!("prompt -> {}", turn.text().await?);
    let _ = turn.result().await?;

    let head = store.head(&session.id()).await.expect("head read");
    println!(
        "session log head after the one turn: {head:?} -- no ask, no fork, \
         no second prompt: exactly one exchange"
    );

    Ok(())
}
