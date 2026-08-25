//! `01M0321414SVRD60HEP074AFHG`: a resumed session's per-agent plugin
//! config -- `conway.fs`'s narrowed root, the same proving consumer
//! `crates/conway/tests/per_agent_plugin_config.rs` uses -- must survive the
//! store round-trip a resume performs, instead of silently reverting to the
//! unconfined global default.
//!
//! Mirrors both `per_agent_plugin_config.rs` (real `FsPlugin`, a real agent
//! turn end to end, every assertion on the persisted `ToolResultRecord`
//! rather than an intermediate call count -- P-15) and `resume.rs`'s "a
//! SECOND `Conway`/`Runtime` sharing the same backing store" harness for
//! simulating a process restart. Every test here resumes a session over a
//! FRESH `Conway`, never the one that spawned it -- the same reason
//! `resume.rs`'s own tests give: `Runtime::resume_root` re-attaches the
//! session's original `agent_id`, and attaching an id already live in the
//! SAME tree errors.
#![cfg(feature = "builtin-tools")]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::test_support::{scripted_backend, test_builder};
use conway::{Conway, PluginSelection, SessionHandle, SessionSpec, SpawnSpec};
use conway_core::agent::PermissionDecision;
use conway_core::content::{ContentBlock, StopReason, ToolCall, ToolResult, Usage};
use conway_core::ids::{AgentId, RoleAlias, SessionId, ToolName};
use conway_core::log::LogRecord;
use conway_core::ports::{GenerateResponse, PermissionGate, PluginConfig};
use conway_testkit::{text_response, FakeStore, ScriptedTurn};
use tempfile::TempDir;

fn read_call(call_id: &str, path: &str) -> ToolCall {
    ToolCall {
        call_id: call_id.to_string(),
        name: ToolName::new("read"),
        arguments: serde_json::json!({ "path": path }),
    }
}

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

/// Always answers `AllowOnce` -- these tests are about `conway.fs`'s OWN
/// per-agent root check surviving a resume, not the operator gate.
struct AllowGate;

#[async_trait::async_trait]
impl PermissionGate for AllowGate {
    async fn check(&self, _req: conway_core::agent::PermissionRequest) -> PermissionDecision {
        PermissionDecision::AllowOnce
    }
}

/// Like `per_agent_plugin_config.rs`'s own `build_conway`, but with a
/// caller-chosen `plugins` selection -- the "not-installed" test below needs
/// a SECOND `Conway` built WITHOUT `conway.fs` at all.
/// A `Conway` over `base_config()` with an explicit builtin-plugin
/// selection -- the one axis these tests vary.
fn conway_with_plugin_selection(
    store: Arc<dyn conway_core::ports::SessionStore>,
    script: Vec<ScriptedTurn>,
    plugins: PluginSelection,
) -> Conway {
    test_builder(base_config())
        .with_backend(scripted_backend(script))
        .with_session_store(store)
        .with_permission_gate(Arc::new(AllowGate))
        .with_builtin_plugins(plugins)
        .build()
        .expect("build should succeed with the requested plugin selection")
}

/// `{"conway.fs.root": "<root>"}` -- see `per_agent_plugin_config.rs`'s own
/// identical helper.
fn fs_root_config(root: &Path) -> PluginConfig {
    let mut values = serde_json::Map::new();
    values.insert(
        "conway.fs.root".to_string(),
        serde_json::json!(root.display().to_string()),
    );
    PluginConfig { values }
}

/// `SessionHandle` deliberately does not derive `Debug` (see `resume.rs`'s
/// own identical helper's doc), so `Result::expect_err` cannot be used
/// directly on a `Result<SessionHandle, _>` here.
fn expect_session_err(
    result: Result<SessionHandle, conway::FacadeError>,
    msg: &str,
) -> conway::FacadeError {
    match result {
        Err(err) => err,
        Ok(_) => panic!("{msg}"),
    }
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

/// This agent's own session id, resolved via the live tree snapshot -- the
/// same `session_of` pattern `conway-runtime/tests/resume_root.rs` and
/// `subagent_fork_spawn.rs` both already use.
fn session_of(handle: &SessionHandle, agent: AgentId) -> SessionId {
    handle
        .tree()
        .nodes
        .iter()
        .find(|n| n.agent_id == agent)
        .expect("agent present in tree")
        .session
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
// VERIFICATION ANCHOR + ACCEPTANCE bullet 1: two siblings, each narrowed to
// a different `conway.fs` root, BOTH resumed over a fresh `Conway`/`Runtime`
// sharing the same store, and BOTH still independently confined -- proven by
// driving a real tool call post-resume that is refused, never by asserting a
// field round-tripped.
// ---------------------------------------------------------------------
#[tokio::test]
async fn two_narrowed_siblings_resumed_together_stay_independently_confined() {
    let tmp = TempDir::new().unwrap();
    let root_a = tmp.path().join("a");
    let root_b = tmp.path().join("b");
    std::fs::create_dir(&root_a).unwrap();
    std::fs::create_dir(&root_b).unwrap();
    std::fs::write(root_a.join("mine.txt"), b"a's own file").unwrap();
    std::fs::write(root_b.join("mine.txt"), b"b's own file").unwrap();

    let store: Arc<dyn conway_core::ports::SessionStore> = Arc::new(FakeStore::new());
    let (session_a, session_b) = {
        // First "process": spawn both narrowed siblings, each completing an
        // uneventful first turn -- nothing about their eventual confinement
        // is exercised yet, only that they exist, narrowed, in the store.
        let conway1 = conway_with_plugin_selection(
            store.clone(),
            vec![
                ScriptedTurn::Respond(text_response("a spawned")),
                ScriptedTurn::Respond(text_response("b spawned")),
            ],
            PluginSelection::All,
        );
        let handle1 = conway1
            .new_session(SessionSpec::default())
            .await
            .expect("new_session");
        let sibling_a = spawn_and_await(
            &handle1,
            handle1.root(),
            SpawnSpec::new("hello from A")
                .cwd(&root_a)
                .plugin_config(fs_root_config(&root_a)),
        )
        .await;
        let sibling_b = spawn_and_await(
            &handle1,
            handle1.root(),
            SpawnSpec::new("hello from B")
                .cwd(&root_b)
                .plugin_config(fs_root_config(&root_b)),
        )
        .await;
        (
            session_of(&handle1, sibling_a),
            session_of(&handle1, sibling_b),
        )
        // `conway1`/`handle1` drop here -- only `store` survives, simulating
        // a process restart.
    };

    // Second "process": a fresh `Conway` over the SAME store. Both siblings
    // are resumed into it and each is driven through one more turn that
    // probes both an in-root and an out-of-root (the OTHER sibling's own
    // root) read at once.
    let conway2 = conway_with_plugin_selection(
        store.clone(),
        vec![
            ScriptedTurn::Respond(double_read_turn(
                "in_a",
                "mine.txt",
                "out_a",
                &root_b.join("mine.txt").display().to_string(),
            )),
            ScriptedTurn::Respond(text_response("a done")),
            ScriptedTurn::Respond(double_read_turn(
                "in_b",
                "mine.txt",
                "out_b",
                &root_a.join("mine.txt").display().to_string(),
            )),
            ScriptedTurn::Respond(text_response("b done")),
        ],
        PluginSelection::All,
    );

    let resumed_a = conway2
        .resume(session_a)
        .await
        .expect("resuming sibling A must succeed");
    let turn_a = resumed_a
        .prompt("probe from resumed A")
        .await
        .expect("prompt on resumed A must succeed");
    tokio::time::timeout(Duration::from_secs(5), turn_a.result())
        .await
        .expect("A's resumed turn must not hang")
        .expect("A's resumed turn must succeed");
    let records_a = resumed_a
        .transcript(resumed_a.root())
        .await
        .expect("transcript should resolve");

    let inside_a = tool_result_for(&records_a, "in_a");
    assert!(
        !inside_a.is_error,
        "resumed A reading its own file must still succeed: {:?}",
        blocks_text(&inside_a.blocks)
    );
    assert!(blocks_text(&inside_a.blocks).contains("a's own file"));

    let outside_a = tool_result_for(&records_a, "out_a");
    assert!(
        outside_a.is_error,
        "resumed A reading B's file must still be refused by A's own persisted \
         conway.fs root -- if this passes, the resumed agent silently reverted to \
         the unconfined global default"
    );

    let resumed_b = conway2
        .resume(session_b)
        .await
        .expect("resuming sibling B must succeed");
    let turn_b = resumed_b
        .prompt("probe from resumed B")
        .await
        .expect("prompt on resumed B must succeed");
    tokio::time::timeout(Duration::from_secs(5), turn_b.result())
        .await
        .expect("B's resumed turn must not hang")
        .expect("B's resumed turn must succeed");
    let records_b = resumed_b
        .transcript(resumed_b.root())
        .await
        .expect("transcript should resolve");

    let inside_b = tool_result_for(&records_b, "in_b");
    assert!(
        !inside_b.is_error,
        "resumed B reading its own file must still succeed: {:?}",
        blocks_text(&inside_b.blocks)
    );
    assert!(blocks_text(&inside_b.blocks).contains("b's own file"));

    let outside_b = tool_result_for(&records_b, "out_b");
    assert!(
        outside_b.is_error,
        "resumed B reading A's file must still be refused by B's own persisted \
         conway.fs root"
    );
}

// ---------------------------------------------------------------------
// The not-installed / no-longer-narrowable case: resuming a session whose
// persisted plugin_config carries a key ("conway.fs.root") that NO
// currently-installed plugin declares narrowable any more must refuse the
// resume outright -- never silently drop the narrowing (which would come
// back wider, unconfined, with no signal at all) and never silently keep a
// value nothing enforces (a trap: the header would still show a "root" no
// tool ever checks, since `conway.fs` -- and thus `check_root` -- is not
// even registered).
// ---------------------------------------------------------------------
#[tokio::test]
async fn resuming_a_session_whose_narrowed_plugin_is_no_longer_installed_refuses_to_resume() {
    let tmp = TempDir::new().unwrap();
    let narrow_root = tmp.path().join("narrow");
    std::fs::create_dir(&narrow_root).unwrap();

    let store: Arc<dyn conway_core::ports::SessionStore> = Arc::new(FakeStore::new());
    let session = {
        let conway1 = conway_with_plugin_selection(
            store.clone(),
            vec![ScriptedTurn::Respond(text_response("spawned"))],
            PluginSelection::All,
        );
        let handle1 = conway1
            .new_session(SessionSpec::default())
            .await
            .expect("new_session");
        let child = spawn_and_await(
            &handle1,
            handle1.root(),
            SpawnSpec::new("hello")
                .cwd(&narrow_root)
                .plugin_config(fs_root_config(&narrow_root)),
        )
        .await;
        session_of(&handle1, child)
    };

    // Second "process": a fresh `Conway` built WITHOUT `conway.fs` at all
    // (`PluginSelection::AllExcept` names its manifest id) -- the exact
    // "the plugin that declared this key narrowable was uninstalled since
    // this session was created" scenario.
    let conway2 = conway_with_plugin_selection(
        store,
        vec![],
        PluginSelection::AllExcept(vec!["conway.fs".to_string()]),
    );
    let err = expect_session_err(
        conway2.resume(session).await,
        "resuming a session whose narrowed plugin is gone must be refused",
    );
    let message = err.to_string();
    assert!(
        message.contains("conway.fs.root") || message.contains("plugin_config"),
        "the refusal should name the key/mechanism that could not be re-validated: {message}"
    );
}
