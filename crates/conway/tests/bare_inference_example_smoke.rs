//! Smoke test for the `bare_inference` example's facade flow, mirroring
//! `example_smoke.rs`'s own precedent for `minimal_session`: the example
//! itself is only compile-checked by `cargo build --examples`, so this test
//! exercises the same public-facade path at runtime, pinning the two
//! findings that example demonstrates in code:
//!
//! 1. an unmodified `ToolsConfig::default()` registers the `report` tool
//!    even when nothing agent-shaped was asked for;
//! 2. `tools.builtin_plugins: vec![]` registers none, and a session built
//!    from that config still completes exactly one turn and returns the
//!    echo backend's reply.
//!
//! Every `.await` is wrapped in a short `tokio::time::timeout` so a hang
//! fails the test quickly instead of blocking forever -- the same
//! precaution `example_smoke.rs` takes.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionMode, PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig,
    ToolsConfig,
};
use conway::{Conway, ConwayBuilder, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias, ToolName};
use conway_core::ports::SessionStore;
use conway_testkit::{FakeBackend, FakeGate, FakeRouter, FakeStore};

const T: Duration = Duration::from_secs(5);

/// The same parameterized config the example's `config_with_tools` builds.
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
        // See the example's own doc comment for why this is `Deny`, not the
        // crate's `presets::default_permissions_for_one_shot` (that preset
        // fails `config::merge::validate`'s own check unconditionally).
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
        tools,
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

fn build(config: ConwayConfig, store: Arc<FakeStore>) -> Conway {
    ConwayBuilder::from_parts(config)
        .with_backend(Arc::new(FakeBackend::echo(BackendId::new("fake"))))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_session_store(store)
        .with_router(Arc::new(FakeRouter::single(ModelRef {
            backend: BackendId::new("fake"),
            model: ModelId::new("echo-model"),
        })))
        .build()
        .expect("build with every port injected should succeed")
}

#[tokio::test]
async fn default_tools_config_registers_report_tool() {
    let conway = build(
        config_with_tools(ToolsConfig::default()),
        Arc::new(FakeStore::new()),
    );
    assert!(
        conway.tool_render_kind(&ToolName::new("report")).is_some(),
        "ToolsConfig::default() should register the 'report' tool"
    );
}

#[tokio::test]
async fn bare_inference_config_registers_no_tools_and_still_completes_one_turn() {
    let store = Arc::new(FakeStore::new());
    let conway = build(
        config_with_tools(ToolsConfig {
            builtin_plugins: Vec::new(),
        }),
        store.clone(),
    );
    assert!(
        conway.tool_render_kind(&ToolName::new("report")).is_none(),
        "an empty tools.builtin_plugins should register no tools"
    );

    let session = tokio::time::timeout(T, conway.new_session(SessionSpec::default()))
        .await
        .expect("new_session must not hang")
        .expect("new_session should succeed");

    let turn = tokio::time::timeout(T, session.prompt("Hello, conway!"))
        .await
        .expect("prompt must not hang")
        .expect("prompt should succeed");
    let text = tokio::time::timeout(T, turn.text())
        .await
        .expect("text must not hang")
        .expect("text should succeed");
    assert_eq!(
        text, "Hello, conway!",
        "echo backend returns the prompt text"
    );
    let _ = tokio::time::timeout(T, turn.result())
        .await
        .expect("result must not hang")
        .expect("result should succeed");

    let head = store
        .head(&session.id())
        .await
        .expect("head read should succeed");
    assert!(
        head.0 > 0,
        "the one turn should have appended to the session log"
    );
}
