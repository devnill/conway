//! The VERIFICATION ANCHOR: a real subprocess plugin, discovered by
//! spawning a fixture Python script that imports nothing from this
//! workspace, called through a real agent turn -- not a unit test of the
//! codec (`tests/mechanism.rs` is that unit-level coverage; this file is
//! the end-to-end proof the item's own VERIFICATION ANCHOR asks for:
//! "calls its tool through a real agent turn, and asserts the result").
//!
//! Written the same way `conway-plugin-skeleton`'s own
//! `tests/skeleton_end_to_end.rs` is: `ConwayBuilder`, the credential-free
//! `ScriptedBackend`/`FakeGate`/`FakeRouter`/`FakeStore` family, and the
//! plugin attached via `ConwayBuilder::with_plugin` -- exactly the call a
//! library embedder makes, and exactly what `conway-cli`'s own
//! `subprocess_plugins::install` (the config-driven wiring this item also
//! adds) does internally after `SubprocessPlugin::discover` succeeds.
//!
//! `tool_is_callable_end_to_end_through_a_real_turn` is the positive proof;
//! `removing_the_configured_command_removes_the_tool` is the negative half
//! the item's own VERIFICATION ANCHOR names explicitly: "shown to fail when
//! the `[plugins]` entry naming it is removed" -- at THIS layer that is "a
//! `Conway` built with no `with_plugin(subprocess_plugin)` call has no such
//! tool", the identical shape `conway-plugin-skeleton`'s own
//! `tool_absent_by_default_present_once_installed` proves for an in-process
//! plugin. `crates/conway-cli/tests/subprocess_plugins.rs` proves the SAME
//! property one layer up, against the real compiled binary and an actual
//! `settings.json` edit.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::{Conway, ConwayBuilder, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::content::{ContentBlock, StopReason, ToolCall, Usage};
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias, SeqRange, ToolName};
use conway_core::log::LogRecord;
use conway_core::ports::{GenerateResponse, SessionStore};
use conway_plugin_subprocess::SubprocessPlugin;
use conway_testkit::{
    text_response, FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn,
};

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }))
}

fn base_config() -> ConwayConfig {
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
        permissions: PermissionsConfig::default(),
        backends: BTreeMap::new(),
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig::default(),
        tools: ToolsConfig::default(),
        // Proving the config-driven `[plugins].subprocess` wiring belongs
        // to `conway-cli`, not this test: this stays default (empty), and
        // the plugin is attached the library-embedder way below, exactly
        // like `conway-plugin-skeleton`'s own fixture does for `[plugins]`
        // generally.
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

fn tool_call_response(call_id: &str, tool: &str, arguments: serde_json::Value) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: ToolName::new(tool),
            arguments,
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

fn build_conway(
    backend: Arc<ScriptedBackend>,
    plugin: Option<SubprocessPlugin>,
) -> (Conway, Arc<FakeStore>) {
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let builder = ConwayBuilder::from_parts(base_config())
        .with_backend(backend)
        .with_session_store(store.clone())
        .with_permission_gate(gate)
        .with_router(fake_router());
    let builder = match plugin {
        Some(plugin) => builder.with_plugin(Arc::new(plugin)),
        None => builder,
    };
    let conway = builder
        .build()
        .expect("build should succeed with every port injected");
    (conway, store)
}

/// **The positive VERIFICATION ANCHOR.** `SubprocessPlugin::discover` spawns
/// the real Python fixture, `ConwayBuilder::with_plugin` attaches the result
/// exactly like any other plugin, and a `ScriptedBackend` turn calls
/// `greet` -- the runtime dispatches through `SubprocessTool::invoke`,
/// which re-spawns the SAME fixture for the actual call, and the persisted
/// `ToolResultRecord` carries the exact reply text the REAL subprocess
/// produced, not a stubbed one.
///
/// **Ordering, disclosed (the item's own "built before, authored after"
/// acceptance criterion).** The `conway`/`conway-runtime`/`conway-testkit`
/// binaries this test links were compiled with zero knowledge of `greet` --
/// `common::GREET_PLUGIN` is written to a fresh temp dir at THIS TEST'S OWN
/// runtime, after every one of those crates has already finished
/// compiling. The only thing that makes this tool reachable is the
/// `SubprocessPluginSpec` naming the freshly-written script's path -- the
/// literal wire-level analogue of "a settings.json change" for a library
/// embedder, since this crate carries no config-file parser of its own
/// (`crates/conway-cli/tests/subprocess_plugins.rs` proves the identical
/// property through an actual `settings.json` edit against the real
/// compiled binary).
#[tokio::test]
async fn tool_is_callable_end_to_end_through_a_real_turn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for_warmed(dir.path(), "greet.py", common::GREET_PLUGIN).await;
    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery against the real fixture must succeed");

    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response(
                "call-1",
                "greet",
                serde_json::json!({ "name": "world" }),
            )),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let (conway, store) = build_conway(backend, Some(plugin));
    let store: Arc<dyn SessionStore> = store;

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("greet the world").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");

    let records = store
        .read(&handle.id(), SeqRange::full())
        .await
        .expect("read should succeed");
    let tool_result = records
        .iter()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == "greet" => {
                Some(result)
            }
            _ => None,
        })
        .expect("the session must have actually invoked the subprocess-backed tool");

    assert!(
        !tool_result.is_error,
        "the greet call must succeed, not error: {tool_result:?}"
    );
    let text: String = tool_result
        .blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text, "hello, world",
        "the real subprocess's own tool/1 reply must reach the persisted log verbatim, proving \
         the runtime dispatched through SubprocessTool::invoke and re-spawned the real fixture, \
         not merely announced its name"
    );
}

/// **The negative VERIFICATION ANCHOR:** a `Conway` built with no
/// `with_plugin(subprocess_plugin)` call at all has no such tool -- the
/// SAME "absent by default, present once installed" shape
/// `conway-plugin-skeleton`'s own `tool_absent_by_default_present_once_
/// installed` proves for an in-process plugin, applied here to prove the
/// subprocess host adds nothing uninvited: this is what "removed from
/// config" collapses to once the config-parsing layer (`conway-cli`) is
/// stripped away, and `crates/conway-cli/tests/subprocess_plugins.rs`
/// proves the literal config-removal version at that layer.
#[tokio::test]
async fn tool_is_absent_without_the_plugin_attached() {
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("hi"))])
            .with_id(BackendId::new("fake")),
    );
    let (conway, _store) = build_conway(backend, None);
    assert_eq!(
        conway.tool_render_kind(&ToolName::new("greet")),
        None,
        "a Conway built without the subprocess plugin attached must not have registered 'greet'"
    );
}
