//! `01KZDC0269171BZDB3HH00179B`: per-agent plugin configuration, with
//! `conway.fs`'s root as the proving consumer -- narrowing-only down the
//! fork/spawn tree, and reachable from `ToolCtx` per-agent rather than
//! per-process.
//!
//! Mirrors `root_containment_seam.rs`'s own shape (real `FsPlugin`, real
//! `ToolRunner`/`PermissionBroker`, a real agent turn end to end) for the
//! identical reason: a hand-built `ToolCtx`/`PluginConfig` fixture proves
//! nothing about whether the real fork/spawn -> `SubagentHost::start` ->
//! `AgentLoop` -> tool-dispatch pipeline actually threads a per-agent
//! config through. Every assertion below is on the persisted
//! `ToolResultRecord` (the observable outcome), never on an intermediate
//! call count -- P-15.
#![cfg(feature = "builtin-tools")]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::{Conway, ConwayBuilder, PluginSelection, SessionHandle, SessionSpec, SpawnSpec};
use conway_core::agent::PermissionDecision;
use conway_core::content::{ContentBlock, StopReason, ToolCall, ToolResult, Usage};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, ToolName};
use conway_core::log::LogRecord;
use conway_core::ports::{Backend, GenerateResponse, PermissionGate, PluginConfig};
use conway_testkit::{text_response, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use tempfile::TempDir;

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }))
}

fn read_call(call_id: &str, path: &str) -> ToolCall {
    ToolCall {
        call_id: call_id.to_string(),
        name: ToolName::new("read"),
        arguments: serde_json::json!({ "path": path }),
    }
}

/// A single assistant turn proposing BOTH reads at once (an in-root one and
/// an out-of-root one) -- one script entry covers a whole sibling's probe,
/// keeping the shared `ScriptedBackend` queue's ordering trivially
/// deterministic (`spawn_and_await` awaits each sibling fully before the
/// next is spawned).
fn double_read_turn(
    call_id_a: &str,
    path_a: &str,
    call_id_b: &str,
    path_b: &str,
) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![read_call(call_id_a, path_a), read_call(call_id_b, path_b)],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
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
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

/// Always answers `AllowOnce` -- this file is about `conway.fs`'s OWN
/// per-agent root check, not the operator gate, so the gate is never the
/// discriminating factor in any assertion here.
struct AllowGate;

#[async_trait::async_trait]
impl PermissionGate for AllowGate {
    async fn check(&self, _req: conway_core::agent::PermissionRequest) -> PermissionDecision {
        PermissionDecision::AllowOnce
    }
}

fn build_conway(script: Vec<ScriptedTurn>) -> Conway {
    let backend = Arc::new(ScriptedBackend::new(script).with_id(BackendId::new("fake")));
    let store = Arc::new(FakeStore::new());
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend as Arc<dyn Backend>)
        .with_session_store(store)
        .with_permission_gate(Arc::new(AllowGate))
        .with_router(fake_router())
        .with_builtin_plugins(PluginSelection::All)
        .build()
        .expect("build should succeed with the real builtin fs tools registered")
}

/// `{"conway.fs.root": "<root>"}` -- the already-prefixed key
/// `ToolCtx::config` carries `conway.fs`'s narrowable root under
/// (`conway_tools::fs`'s own module doc).
fn fs_root_config(root: &Path) -> PluginConfig {
    let mut values = serde_json::Map::new();
    values.insert(
        "conway.fs.root".to_string(),
        serde_json::json!(root.display().to_string()),
    );
    PluginConfig { values }
}

async fn spawn_and_await(handle: &SessionHandle, parent: AgentId, spec: SpawnSpec) -> AgentId {
    let child = handle
        .spawn(parent, spec)
        .await
        .expect("spawn should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(10), handle.await_agent(child))
        .await
        .expect("child turn must not hang")
        .expect("await_agent should resolve Ok");
    child
}

fn tool_result_for<'a>(records: &'a [LogRecord], call_id: &str) -> &'a ToolResult {
    records
        .iter()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. } if result.call_id == call_id => Some(result),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a ToolResultRecord for call_id {call_id}"))
}

fn blocks_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------
// The VERIFICATION ANCHOR: two siblings spawned with different
// `conway.fs` roots, each reading successfully inside its own and
// refused outside it.
// ---------------------------------------------------------------------
#[tokio::test]
async fn two_siblings_with_different_conway_fs_roots_each_read_inside_own_and_refused_outside() {
    let tmp = TempDir::new().unwrap();
    let root_a = tmp.path().join("a");
    let root_b = tmp.path().join("b");
    std::fs::create_dir(&root_a).unwrap();
    std::fs::create_dir(&root_b).unwrap();
    std::fs::write(root_a.join("mine.txt"), b"a's own file").unwrap();
    std::fs::write(root_b.join("mine.txt"), b"b's own file").unwrap();

    let conway = build_conway(vec![
        // Sibling A's one turn: reads its own file (inside) and B's file
        // (outside, by absolute path) at once.
        ScriptedTurn::Respond(double_read_turn(
            "in_a",
            "mine.txt",
            "out_a",
            &root_b.join("mine.txt").display().to_string(),
        )),
        ScriptedTurn::Respond(text_response("a done")),
        // Sibling B's one turn: the mirror image.
        ScriptedTurn::Respond(double_read_turn(
            "in_b",
            "mine.txt",
            "out_b",
            &root_a.join("mine.txt").display().to_string(),
        )),
        ScriptedTurn::Respond(text_response("b done")),
    ]);

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");

    let sibling_a = spawn_and_await(
        &handle,
        handle.root(),
        SpawnSpec::new("probe from A")
            .cwd(&root_a)
            .plugin_config(fs_root_config(&root_a)),
    )
    .await;
    let records_a = handle
        .transcript(sibling_a)
        .await
        .expect("transcript should resolve");

    let inside_a = tool_result_for(&records_a, "in_a");
    assert!(
        !inside_a.is_error,
        "A reading its own file must succeed: {:?}",
        blocks_text(&inside_a.blocks)
    );
    assert!(blocks_text(&inside_a.blocks).contains("a's own file"));

    let outside_a = tool_result_for(&records_a, "out_a");
    assert!(
        outside_a.is_error,
        "A reading B's file must be refused by A's own conway.fs root"
    );

    let sibling_b = spawn_and_await(
        &handle,
        handle.root(),
        SpawnSpec::new("probe from B")
            .cwd(&root_b)
            .plugin_config(fs_root_config(&root_b)),
    )
    .await;
    let records_b = handle
        .transcript(sibling_b)
        .await
        .expect("transcript should resolve");

    let inside_b = tool_result_for(&records_b, "in_b");
    assert!(
        !inside_b.is_error,
        "B reading its own file must succeed: {:?}",
        blocks_text(&inside_b.blocks)
    );
    assert!(blocks_text(&inside_b.blocks).contains("b's own file"));

    let outside_b = tool_result_for(&records_b, "out_b");
    assert!(
        outside_b.is_error,
        "B reading A's file must be refused by B's own conway.fs root"
    );
}

// ---------------------------------------------------------------------
// A child attempting to WIDEN its parent's already-narrowed `conway.fs`
// root is rejected outright, with the spawn itself failing -- never
// silently clamped to the parent's root and never silently honored.
// ---------------------------------------------------------------------
#[tokio::test]
async fn a_childs_attempt_to_widen_its_parents_conway_fs_root_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let narrow_root = tmp.path().join("narrow");
    let sideways_root = tmp.path().join("sideways");
    std::fs::create_dir(&narrow_root).unwrap();
    std::fs::create_dir(&sideways_root).unwrap();

    let conway = build_conway(vec![]);
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");

    // The root session spawns a first child, narrowed to `narrow_root` --
    // accepted (the root session itself has no per-agent `conway.fs.root`
    // yet, so this is a first-time narrowing, never a widening).
    let narrowed_child = handle
        .spawn(
            handle.root(),
            SpawnSpec::new("narrow first")
                .cwd(&narrow_root)
                .plugin_config(fs_root_config(&narrow_root)),
        )
        .await
        .expect("first narrowing must be accepted");

    // That child now attempts to spawn a GRANDCHILD whose requested root
    // (`sideways_root`) is disjoint from its own already-narrowed root --
    // a genuine widening (well, sideways move) attempt. The spawn itself
    // must fail; no grandchild is ever created.
    let err = handle
        .spawn(
            narrowed_child,
            SpawnSpec::new("try to widen")
                .cwd(&sideways_root)
                .plugin_config(fs_root_config(&sideways_root)),
        )
        .await
        .expect_err("a plugin_config root disjoint from the parent's own must fail the spawn");
    let message = err.to_string();
    assert!(
        message.contains("conway.fs.root") || message.contains("plugin_config"),
        "the spawn failure should name the plugin_config/root mismatch: {message}"
    );
}

// ---------------------------------------------------------------------
// A child attempting to introduce a plugin_config key its parent -- or any
// installed plugin -- never declared narrowable is rejected the same way.
// ---------------------------------------------------------------------
#[tokio::test]
async fn a_childs_attempt_to_introduce_an_undeclared_plugin_config_key_is_rejected() {
    let tmp = TempDir::new().unwrap();

    let conway = build_conway(vec![]);
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");

    let mut values = serde_json::Map::new();
    values.insert("acme.undeclared".to_string(), serde_json::json!("anything"));
    let undeclared = PluginConfig { values };

    let err = handle
        .spawn(
            handle.root(),
            SpawnSpec::new("introduce an undeclared key")
                .cwd(tmp.path())
                .plugin_config(undeclared),
        )
        .await
        .expect_err("a key no installed plugin declared narrowable must fail the spawn");
    let message = err.to_string();
    assert!(
        message.contains("acme.undeclared"),
        "the spawn failure should name the offending key: {message}"
    );
}
