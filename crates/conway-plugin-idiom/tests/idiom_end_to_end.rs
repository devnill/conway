//! End-to-end acceptance for `conway.idiom` (board item
//! `01M0VR3BKW5N3V3WS28H7FV8ZK`): a real, fully-faked `Conway` (no network,
//! no live provider) with `IdiomPlugin` attached exactly the way a library
//! embedder would (`ConwayBuilder::with_plugin`) -- the same call
//! `crates/conway-cli/src/first_party_plugins.rs` makes internally once
//! `conway.idiom` is named in `[plugins].install`.
//!
//! Drives the fragment through a REAL model turn (`ScriptedBackend`),
//! asserting on the ACTUAL wire request the turn sends -- proving the
//! fragment reaches the model, not merely that `Plugin::instructions()`
//! returns the right value in isolation.

use std::collections::BTreeMap;
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::plugin::ContentBlock;
use conway::test_support::test_builder;
use conway::{Conway, ForkSpec, RoleAlias, SessionSpec, SpawnSpec};
use conway_plugin_idiom::{IdiomPlugin, FRAGMENT_TEXT, INSTRUCTION_NAME, PLUGIN_ID};
use conway_testkit::{text_response, FakeStore, ScriptedBackend, ScriptedTurn};

fn base_config(cwd: std::path::PathBuf) -> ConwayConfig {
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
        cwd,
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

/// A real, fully-faked `Conway` with `IdiomPlugin` attached, mirroring
/// `conway-plugin-path`'s own `path_conway` precedent.
fn idiom_conway(
    cwd: std::path::PathBuf,
    backend: Arc<ScriptedBackend>,
    store: Arc<FakeStore>,
) -> Conway {
    test_builder(base_config(cwd))
        .with_backend(backend)
        .with_session_store(store)
        .with_plugin(Arc::new(IdiomPlugin))
        .build()
        .expect("build should succeed with every port injected")
}

fn all_text(req: &conway::backend::GenerateRequest) -> String {
    let mut out = String::new();
    for segment in &req.segments {
        for block in &segment.content {
            if let ContentBlock::Text { text } = block {
                out.push_str(text);
                out.push('\n');
            }
        }
    }
    out
}

#[test]
fn manifest_id_matches_the_published_constant() {
    use conway::plugin::Plugin as _;
    assert_eq!(IdiomPlugin.manifest().id, PLUGIN_ID);
}

/// Acceptance 2/9: installing `conway.idiom` on a bare session (no
/// `agent_def`, no `system_prompt_override` -- the exact shape
/// `App::session_spec` builds, and this item's premise section confirms
/// carries no `[0] SystemPrompt` segment at all) makes the fragment's own
/// text reach the wire request a real turn sends -- proof it lands in the
/// assembled context, not merely that `Plugin::instructions()` returns the
/// right value in isolation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fragment_reaches_a_bare_sessions_wire_request() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response(
            "hello from the model",
        ))])
        .with_id(conway::backend::BackendId::new("fake")),
    );
    let conway = idiom_conway(tmp.path().to_path_buf(), backend.clone(), store);

    // The bare interactive-session shape: no `agent_def`, no
    // `system_prompt_override` -- `SessionSpec::default()` sets neither.
    let session = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = session.prompt("hi").await.expect("prompt");
    turn.result().await.expect("turn completes");

    let calls = backend.calls();
    let request = calls.last().expect("at least one call recorded");
    let text = all_text(request);
    assert!(
        text.contains("Fork vs spawn"),
        "the idiom fragment must be part of the wire request's context: {text}"
    );
    assert!(
        text.contains("This reaches every agent"),
        "the fragment's own subagent-reach disclosure must ship with it: {text}"
    );
}

/// Board item `01M0VSKA76NSEHDSH25XJGJ2J5`: the fragment reaches a forked
/// AND a spawned child too, not the root alone -- driven through the real
/// facade (`SessionHandle::fork`/`::spawn`), asserting on each child's own
/// ACTUAL wire request, exactly like [`fragment_reaches_a_bare_sessions_wire_request`]
/// does for root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fragment_reaches_a_forked_and_a_spawned_childs_wire_request() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("root turn")),
            ScriptedTurn::Respond(text_response("fork child turn")),
            ScriptedTurn::Respond(text_response("spawn child turn")),
        ])
        .with_id(conway::backend::BackendId::new("fake")),
    );
    let conway = idiom_conway(tmp.path().to_path_buf(), backend.clone(), store);

    let session = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = session.prompt("hi").await.expect("prompt");
    turn.result().await.expect("root turn completes");
    let root = session.root();

    let forked = session
        .fork(root, ForkSpec::new("investigate further"))
        .await
        .expect("fork should succeed");
    session
        .await_agent(forked)
        .await
        .expect("forked child should complete");

    let fork_calls = backend.calls();
    let fork_text = all_text(fork_calls.last().expect("fork child made a call"));
    assert!(
        fork_text.contains("Fork vs spawn"),
        "the idiom fragment must reach a forked child's wire request: {fork_text}"
    );

    let spawned = session
        .spawn(root, SpawnSpec::new("do the review"))
        .await
        .expect("spawn should succeed");
    session
        .await_agent(spawned)
        .await
        .expect("spawned child should complete");

    let spawn_calls = backend.calls();
    let spawn_text = all_text(spawn_calls.last().expect("spawn child made a call"));
    assert!(
        spawn_text.contains("Fork vs spawn"),
        "the idiom fragment must reach a spawned child's wire request too, not the root/fork \
         only: {spawn_text}"
    );
}

/// The fragment's own line/word budget, checked against the exact constant
/// the plugin ships (acceptance 3) -- redundant with the unit test in
/// `src/lib.rs`, deliberately: that one guards the constant in isolation,
/// this one is the end-to-end suite's own record of the number, so a
/// reviewer scanning this file alone still sees the budget enforced.
#[test]
fn fragment_text_is_within_the_forty_line_four_hundred_word_budget() {
    let lines = FRAGMENT_TEXT.lines().count();
    let words = FRAGMENT_TEXT.split_whitespace().count();
    assert!(lines <= 40, "fragment has {lines} lines, over budget");
    assert!(words <= 400, "fragment has {words} words, over budget");
}

/// `INSTRUCTION_NAME` is exported so a caller (or this test) can name the
/// fragment without re-deriving it -- checked here rather than merely
/// assumed by the plugin's own doc comment.
#[test]
fn instruction_name_is_the_published_constant() {
    use conway::plugin::Plugin as _;
    let instructions = IdiomPlugin.instructions();
    assert_eq!(instructions[0].name, INSTRUCTION_NAME);
}
