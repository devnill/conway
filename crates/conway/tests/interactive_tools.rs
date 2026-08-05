//! Acceptance tests for the interactive-chat "pure and light" tool profile
//! (decision 01KYB0BWY27DWB69NCNK85D56J, board item 01KYB0ESC1YXDEKFT11AZ847NT):
//! `SessionSpec::tools` plumbs straight through `Conway::new_session` into
//! `RootSpec::tools`, so a session built with `ToolSelector::Except(vec![
//! "report".into()])` announces every builtin tool EXCEPT `report` to the
//! backend, while a default `SessionSpec`/`SpawnSpec` (no override) still
//! announces `report` -- exactly the split `conway-cli`'s TUI root/bare
//! keep-alive children (excluded) vs. an autonomous `conway_subagent`-spawned
//! child (default, unaffected) rely on.
//!
//! Gated on the `builtin-tools` feature (like `tests/gates.rs`'s own
//! `presets_builtin_plugins_matches_conway_tools`): with it disabled the
//! crate has no `conway-tools` dependency and `ConwayBuilder::build` (with no
//! plugins injected) registers no tools at all, so there is nothing
//! meaningful to assert inclusion/exclusion of.
#![cfg(feature = "builtin-tools")]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    RoleEntry, RoutingSection, SessionConfig, ToolsConfig, TuiSection,
};
use conway::{Conway, ConwayBuilder, SessionSpec, SpawnSpec, ToolSelector};
use conway_core::agent::PermissionDecision;
use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::fakes::{FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias};
use conway_core::ports::GenerateResponse;

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
        tui: TuiSection::default(),
        tools: ToolsConfig::default(),
    }
}

fn text_response(text: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage::default(),
    }
}

/// Builds a `Conway` with `ScriptedBackend` (records every `GenerateRequest`
/// it receives -- including `tools`, the announced set) and the crate's real
/// built-in plugin set (`ConwayBuilder::build`'s own `builtin-tools`
/// registration, unmodified -- no `.with_plugins` override here).
fn build_conway(backend: Arc<ScriptedBackend>) -> Conway {
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should succeed with every port injected")
}

/// The sorted tool names `req.tools` announced, as plain `String`s.
fn announced_names(req: &conway_core::ports::GenerateRequest) -> Vec<String> {
    let mut names: Vec<String> = req.tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    names
}

#[tokio::test]
async fn default_session_spec_announces_report_and_other_builtin_tools() {
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("hi back"))])
            .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(backend.clone());

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hi").await.expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");

    let calls = backend.calls();
    let names = announced_names(calls.last().expect("backend must have received one call"));
    assert!(
        names.iter().any(|n| n == "report"),
        "a default (no tools override) SessionSpec must still announce `report`, got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "conway_subagent"),
        "a default SessionSpec must announce `conway_subagent` too, got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "read"),
        "a default SessionSpec must announce the `fs` plugin's `read` tool too, got {names:?}"
    );
}

#[tokio::test]
async fn session_spec_tools_except_report_excludes_report_but_keeps_the_rest() {
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("hi back"))])
            .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(backend.clone());

    let spec = SessionSpec {
        tools: Some(ToolSelector::Except(vec!["report".into()])),
        ..SessionSpec::default()
    };
    let handle = conway
        .new_session(spec)
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hi").await.expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");

    let calls = backend.calls();
    let names = announced_names(calls.last().expect("backend must have received one call"));
    assert!(
        !names.iter().any(|n| n == "report"),
        "ToolSelector::Except([\"report\"]) must exclude `report` from the announced set, \
         got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "conway_subagent"),
        "excluding `report` must not exclude `conway_subagent`, got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "read"),
        "excluding `report` must not exclude the `fs` plugin's `read` tool either, got {names:?}"
    );
}

/// Regression guard for spec item 6 ("do NOT change autonomous subagents'
/// toolset"): a `SpawnSpec` built with no `.tools(..)` override (the shape
/// `conway_tools`' own `conway_subagent` tool, and every OTHER facade
/// consumer that has not opted into the interactive exclusion, uses) still
/// announces `report` to its own child turn -- proving this item's plumbing
/// is additive (an explicit opt-in via `SessionSpec::tools`/`SpawnSpec::
/// tools`), never a default-toolset behavior change.
#[tokio::test]
async fn default_spawn_spec_still_announces_report_to_the_child() {
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent turn ack")),
            ScriptedTurn::Respond(text_response("child turn ack")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(backend.clone());

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let parent_turn = handle.prompt("hi").await.expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), parent_turn.result())
        .await
        .expect("parent result() must not hang")
        .expect("parent result() should succeed");

    let root = handle.root();
    let child = handle
        .spawn(root, SpawnSpec::new("please review"))
        .await
        .expect("spawn should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), handle.await_agent(child))
        .await
        .expect("await_agent must not hang")
        .expect("await_agent should succeed");

    let calls = backend.calls();
    let names = announced_names(calls.last().expect("backend must have received one call"));
    assert!(
        names.iter().any(|n| n == "report"),
        "a default (no tools override) SpawnSpec's autonomous child must still announce \
         `report`, got {names:?}"
    );
}

/// The scenario that actually matters for spec item 6 (stronger than
/// [`default_spawn_spec_still_announces_report_to_the_child`] above, which
/// only proves a plain, non-excluded parent's child keeps `report`): a
/// REPORT-EXCLUDED interactive parent (`SessionSpec::tools: Some(Except(
/// ["report"]))`, the exact shape `conway-cli`'s TUI root/bare keep-alive
/// children use) must NOT leak that exclusion into a default `SpawnSpec`
/// child it spawns. Today's code cannot leak it -- `SpawnSpec::tools`
/// defaults `None`, and `SubagentHost::start`'s child resolution never reads
/// the PARENT's own `AgentSpec.tools` for a spawn (clean-slate, per GP-02) --
/// but nothing up to now asserted that specifically from a report-excluded
/// parent; a future change that threaded the parent's tools into spawn
/// child resolution would silently break the "autonomous subagents keep
/// `report`" invariant without this test catching it.
#[tokio::test]
async fn report_excluded_parent_still_gives_an_autonomous_child_report() {
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent turn ack")),
            ScriptedTurn::Respond(text_response("child turn ack")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(backend.clone());

    let spec = SessionSpec {
        tools: Some(ToolSelector::Except(vec!["report".into()])),
        ..SessionSpec::default()
    };
    let handle = conway
        .new_session(spec)
        .await
        .expect("new_session should succeed");
    let parent_turn = handle.prompt("hi").await.expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), parent_turn.result())
        .await
        .expect("parent result() must not hang")
        .expect("parent result() should succeed");

    // Sanity: the parent root itself must NOT announce `report` -- otherwise
    // this test would prove nothing about leakage (there'd be nothing to
    // leak from).
    let calls_after_parent = backend.calls();
    let parent_names = announced_names(
        calls_after_parent
            .last()
            .expect("backend must have received the parent's call"),
    );
    assert!(
        !parent_names.iter().any(|n| n == "report"),
        "the report-excluded parent's own turn must not announce `report`, got {parent_names:?}"
    );

    let root = handle.root();
    let child = handle
        .spawn(root, SpawnSpec::new("please review"))
        .await
        .expect("spawn should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), handle.await_agent(child))
        .await
        .expect("await_agent must not hang")
        .expect("await_agent should succeed");

    let calls_after_child = backend.calls();
    let child_names = announced_names(
        calls_after_child
            .last()
            .expect("backend must have received the child's call"),
    );
    assert!(
        child_names.iter().any(|n| n == "report"),
        "a default SpawnSpec child of a report-excluded parent must STILL announce `report` \
         -- the exclusion must not leak from parent to autonomous child, got {child_names:?}"
    );
}
