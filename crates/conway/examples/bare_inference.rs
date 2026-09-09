//! The shortest configuration this crate can express today for "route
//! straight to inference: no tools, no agent behaviour, one turn, out" --
//! assembled using only mechanisms a third party also has: `ConwayConfig`
//! fields and `ConwayBuilder` methods, no more. No new entry point and no
//! method that shortcuts the harness were added to build this -- every call
//! below still goes through `ConwayBuilder::build` -> `Conway::new_session`
//! -> `SessionHandle::prompt`, exactly like every other caller (including
//! the interactive TUI).
//!
//! Runs fully offline against fakes (`conway_testkit`), like this
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
//!
//! **Board item 01M00QGYR1M8F71HTAA1S3PEKS's own finding, on top of this
//! one:** the fourteen-field ceremony below is real for `ConwayBuilder::
//! from_parts` -- the entry point this file (deliberately) uses to isolate
//! exactly what a hand-built `ConwayConfig` costs -- but it is NOT the only
//! entry point. `ConwayBuilder::discover()` already existed when this file
//! was written and was never used here: it layers a documented, built-in
//! default over every one of these same fourteen fields
//! (`config::merge::default_document`), reachable with zero struct-literal
//! ceremony. See `examples/discover_getting_started.rs` for that same
//! "bare inference" shape reached through `discover()` instead, in well
//! under half the lines (measured directly against an equivalent scratch
//! crate outside this workspace -- see that example's own doc for the
//! numbers) -- and `docs/embedding.md`, which now opens with it.
//! This file's own finding stands: `ConwayConfig` still has no `Default`,
//! and a caller who genuinely needs `from_parts` (no filesystem, no
//! ambient environment -- an embedded target, a test fixture) still pays
//! this in full.

use std::collections::BTreeMap;
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::{ConwayBuilder, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias, ToolName};
use conway_core::ports::SessionStore;
use conway_testkit::{FakeBackend, FakeGate, FakeRouter, FakeStore};

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
        // Board item 01M1YVP3FDPHY4WZ72SXMWAN2D: `PermissionsConfig` no
        // longer carries a gate selection at all (`default_mode` is a
        // different concept -- the STARTING mode of an interactive TUI
        // session, irrelevant to this one-shot, no-tools example). The
        // gate itself is supplied explicitly below via
        // `with_permission_gate` regardless of what this field holds, so
        // the plain default is correct here.
        permissions: PermissionsConfig::default(),
        backends: BTreeMap::new(),
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig::default(),
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
    // full literal: `ToolsConfig` has exactly one field, emptied here -- see
    // this function's own doc for why.
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
    // (`ConwayBuilder::build`'s own step 9 fallback, `GateMode::Prompt` by
    // default, needs a prompt handler this example does not supply)
    // purely so this comparison build succeeds -- it plays no role in the
    // finding below, which is about `tools`, not `permissions`.
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
