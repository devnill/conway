//! `01M03KZXR1KF77YRAW4W4GE6KK`: the two siblings of the `result_contract`
//! facade-fork bug -- `ForkSpec::keep_alive` and `ForkSpec::plugin_config`
//! were silently dropped on `Conway::fork_from` (the facade path), while both
//! were honored on the model-triggered `SessionHandle::fork` path (via
//! `From<ForkSpec> for SubagentSpec`). These are the three behavioural
//! acceptance proofs the spec requires, each proven BEHAVIOURALLY rather than
//! by asserting a field round-tripped:
//!
//! 1. `fork_from_keep_alive_child_persists_for_a_second_turn` -- a child
//!    forked via `fork_from` with `keep_alive(true)` runs a GENUINE second
//!    turn in the same process (a second backend call), instead of
//!    terminating on its first completed turn.
//! 2. `fork_from_with_narrowed_conway_fs_root_refuses_a_read_outside_it` -- a
//!    child forked via `fork_from` with a narrowed `conway.fs` root is
//!    REFUSED a `read` outside it, the same shape of proof
//!    `01M0321414SVRD60HEP074AFHG` used for resume.
//! 3. `fork_from_with_a_conway_fs_root_wider_than_the_parent_is_refused` -- a
//!    forked child can NEVER end up with a root wider than its parent imposed:
//!    the fork itself fails with a typed error rather than persisting a
//!    widened config.
//!
//! Mirrors `resume_plugin_config.rs` (real `FsPlugin`, a real agent turn end
//! to end, every assertion on the persisted `ToolResultRecord`) and
//! `keep_alive.rs` (the `ScriptedBackend` call log is the ground truth for a
//! second turn, not just "the record was appended").
#![cfg(feature = "builtin-tools")]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::{Conway, ConwayBuilder, ForkSpec, PluginSelection, SessionHandle, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::content::{ContentBlock, StopReason, ToolCall, ToolResult, Usage};
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias};
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
        name: conway_core::ids::ToolName::new("read"),
        arguments: serde_json::json!({ "path": path }),
    }
}

/// A single turn that issues two `read` calls -- one inside, one outside a
/// confinement root -- so one `ToolResultRecord` proves the boundary and the
/// other proves the inside still works. Mirrors `resume_plugin_config.rs`'s
/// identical `double_read_turn`.
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
/// per-agent root check, not the operator gate. Mirrors
/// `resume_plugin_config.rs`'s identical `AllowGate`.
struct AllowGate;

#[async_trait::async_trait]
impl PermissionGate for AllowGate {
    async fn check(&self, _req: conway_core::agent::PermissionRequest) -> PermissionDecision {
        PermissionDecision::AllowOnce
    }
}

/// Build a `Conway` with `conway.fs` installed (the proving consumer of
/// `conway.fs.root`), a caller-chosen root, and a caller-chosen cwd. The
/// cwd MUST fall inside `root` (when set) -- `start_root` validates
/// `cwd ⊆ root`, so a test whose parent session is confined to `root` must
/// set the parent's cwd inside it. Mirrors `resume_plugin_config.rs`'s
/// `build_conway_with_plugins`.
fn build_conway(
    store: Arc<dyn conway_core::ports::SessionStore>,
    script: Vec<ScriptedTurn>,
    cwd: &Path,
    root: Option<&Path>,
) -> Conway {
    let backend = Arc::new(ScriptedBackend::new(script).with_id(BackendId::new("fake")));
    let mut config = base_config();
    config.cwd = cwd.to_path_buf();
    let mut builder = ConwayBuilder::from_parts(config)
        .with_backend(backend as Arc<dyn Backend>)
        .with_session_store(store)
        .with_permission_gate(Arc::new(AllowGate))
        .with_router(fake_router())
        .with_builtin_plugins(PluginSelection::All);
    if let Some(root) = root {
        builder = builder.with_root(root);
    }
    builder
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
// ACCEPTANCE bullet 1: a child forked via `fork_from` with `keep_alive(true)`
// PERSISTS -- proven by a genuine second backend call in the same process,
// not by asserting the field round-tripped. Before this item, the child's
// task terminated on its first completed turn (`resume_root` hardcoded
// `keep_alive: false`), so the second `prompt` would have appended a
// `UserTurn` nobody ever read and `turn2.text()` would time out.
// ---------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_from_keep_alive_child_persists_for_a_second_turn() {
    let store: Arc<dyn conway_core::ports::SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("child turn 1")),
            ScriptedTurn::Respond(text_response("child turn 2")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = ConwayBuilder::from_parts(base_config())
        .with_backend(backend.clone() as Arc<dyn Backend>)
        .with_session_store(store.clone())
        .with_permission_gate(Arc::new(AllowGate))
        .with_router(fake_router())
        .build()
        .expect("build should succeed");

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle
        .prompt("parent turn text")
        .await
        .expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("parent result() must not hang")
        .expect("parent result() should succeed");
    let at = store.head(&handle.id()).await.expect("head should succeed");

    // The fork_from child with keep_alive(true). Before this item, the
    // `keep_alive` flag was silently dropped on this facade path.
    let child = conway
        .fork_from(
            handle.id(),
            at,
            ForkSpec::new("picking up").keep_alive(true),
        )
        .await
        .expect("fork_from should succeed");

    // Let the forked child's gated loop reach its idle-await before the
    // first prompt races it -- mirrors `keep_alive.rs`'s SETTLE idiom.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let child_turn1 = child
        .prompt("child turn 1 text")
        .await
        .expect("first prompt on fork_from child must succeed");
    let text1 = tokio::time::timeout(Duration::from_secs(5), child_turn1.text())
        .await
        .expect("first turn's text() must not hang")
        .expect("first turn's text() should succeed");
    assert_eq!(text1, "child turn 1");
    assert_eq!(
        backend.calls().len(),
        2,
        "parent (1) + child turn 1 (1) = 2 so far"
    );

    // THE PROOF: a SECOND prompt on the SAME live child drives a genuine
    // second backend call. Pre-fix, the child's task would have terminated
    // on `child turn 1`'s natural completion, so this second `prompt` would
    // either error (no task to notify) or silently append a `UserTurn`
    // nobody reads -- and `text()` would time out waiting for a
    // `TurnFinished` that never comes.
    let child_turn2 = child
        .prompt("child turn 2 text")
        .await
        .expect("second prompt on the SAME live fork_from child should succeed");
    let text2 = tokio::time::timeout(Duration::from_secs(5), child_turn2.text())
        .await
        .expect(
            "second turn's text() must not hang -- this is exactly the fix: a keep_alive \
             fork_from child's task must still be alive to run a genuine second turn",
        )
        .expect("second turn's text() should succeed");
    assert_eq!(text2, "child turn 2");
    assert_eq!(
        backend.calls().len(),
        3,
        "the second explicit prompt must ALSO have driven a new, third backend call -- proving a \
         real second turn ran, not just that the UserTurn record was appended: {:?}",
        backend.calls()
    );
}

// ---------------------------------------------------------------------
// ACCEPTANCE bullet 2: a child forked via `fork_from` with a narrowed
// `conway.fs` root is REFUSED a read outside it -- the same shape of proof
// `01M0321414SVRD60HEP074AFHG` used for resume. Before this item,
// `ForkSpec::plugin_config` was silently dropped on this path, so the child
// inherited the parent's (unconfined) config and the outside read would have
// SUCCEEDED.
// ---------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_from_with_narrowed_conway_fs_root_refuses_a_read_outside_it() {
    let tmp = TempDir::new().unwrap();
    let narrow_root = tmp.path().join("narrow");
    let outside_root = tmp.path().join("outside");
    std::fs::create_dir(&narrow_root).unwrap();
    std::fs::create_dir(&outside_root).unwrap();
    std::fs::write(narrow_root.join("mine.txt"), b"inside narrow").unwrap();
    std::fs::write(outside_root.join("theirs.txt"), b"outside narrow").unwrap();

    let store: Arc<dyn conway_core::ports::SessionStore> = Arc::new(FakeStore::new());
    // Parent: unconfined (no root) so the fork's narrowing is a first-time
    // narrowing (unbounded -> bounded), always accepted by `PluginConfig::
    // narrow` -- the simplest parent that still lets the child's confinement
    // be proven.
    let conway = build_conway(
        store.clone(),
        vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            // The child's single turn issues two reads: one inside its
            // narrowed root, one outside it.
            ScriptedTurn::Respond(double_read_turn(
                "inside",
                "mine.txt",
                "outside",
                &outside_root.join("theirs.txt").display().to_string(),
            )),
            ScriptedTurn::Respond(text_response("child done")),
        ],
        &narrow_root,
        None,
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle
        .prompt("parent turn text")
        .await
        .expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("parent result() must not hang")
        .expect("parent result() should succeed");
    let at = store.head(&handle.id()).await.expect("head should succeed");

    // Fork the child with a narrowed conway.fs root. Before this item, this
    // request was silently dropped and the child inherited the parent's
    // unconfined config.
    let child = conway
        .fork_from(
            handle.id(),
            at,
            ForkSpec::new("narrowed fork").plugin_config(fs_root_config(&narrow_root)),
        )
        .await
        .expect("fork_from should succeed");

    tokio::time::sleep(Duration::from_millis(50)).await;

    let child_turn = child
        .prompt("probe the boundary")
        .await
        .expect("prompt on fork_from child must succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), child_turn.result())
        .await
        .expect("child turn must not hang")
        .expect("child turn should succeed");
    let records = child
        .transcript(child.root())
        .await
        .expect("transcript should resolve");

    let inside = tool_result_for(&records, "inside");
    assert!(
        !inside.is_error,
        "the forked child reading its own narrowed root's file must succeed: {:?}",
        blocks_text(&inside.blocks)
    );
    assert!(blocks_text(&inside.blocks).contains("inside narrow"));

    let outside = tool_result_for(&records, "outside");
    assert!(
        outside.is_error,
        "the forked child reading OUTSIDE its narrowed conway.fs root must be REFUSED -- if this \
         passes, `ForkSpec::plugin_config` was silently dropped on the facade fork path and the \
         child inherited the parent's unconfined config"
    );
}

// ---------------------------------------------------------------------
// ACCEPTANCE bullet 3: a forked child CANNOT end up with a root wider than
// its parent imposed -- the fork itself FAILS with a typed error naming the
// widening, rather than persisting a widened config. The parent is confined
// to `narrow_root` (via `ConwayBuilder::with_root`, so its persisted
// `plugin_config` carries `conway.fs.root = narrow_root`); the fork requests
// `outside_root`, which is disjoint from (wider than) `narrow_root`, so
// `PluginConfig::narrow` refuses it with `WouldWiden`.
// ---------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_from_with_a_conway_fs_root_wider_than_the_parent_is_refused() {
    let tmp = TempDir::new().unwrap();
    let narrow_root = tmp.path().join("narrow");
    let outside_root = tmp.path().join("outside");
    std::fs::create_dir(&narrow_root).unwrap();
    std::fs::create_dir(&outside_root).unwrap();

    let store: Arc<dyn conway_core::ports::SessionStore> = Arc::new(FakeStore::new());
    // Parent: confined to `narrow_root` via `with_root`, so its persisted
    // `SessionMeta::plugin_config` carries `conway.fs.root = narrow_root`.
    let conway = build_conway(
        store.clone(),
        vec![ScriptedTurn::Respond(text_response("parent ack"))],
        &narrow_root,
        Some(&narrow_root),
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle
        .prompt("parent turn text")
        .await
        .expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("parent result() must not hang")
        .expect("parent result() should succeed");
    let at = store.head(&handle.id()).await.expect("head should succeed");

    // Fork requesting a root DISJOINT from (wider than) the parent's own
    // `narrow_root` -- a genuine widening attempt. The fork itself must
    // fail; no child is ever created. Before this item, this request was
    // silently dropped and the child inherited the parent's `narrow_root`
    // (fail-safe, but by accident of the drop, not by design). With the
    // field honored, the narrowing is re-validated and the widening is
    // refused explicitly -- the only outcome that is never silently wrong in
    // either direction.
    let err = expect_session_err(
        conway
            .fork_from(
                handle.id(),
                at,
                ForkSpec::new("try to widen").plugin_config(fs_root_config(&outside_root)),
            )
            .await,
        "a fork_from requesting a conway.fs root wider than the parent's own must fail the fork",
    );
    let message = err.to_string();
    assert!(
        message.contains("conway.fs.root") || message.contains("plugin_config"),
        "the fork failure should name the plugin_config/root mismatch: {message}"
    );
}
