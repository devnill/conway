//! THE CLOSING TEST (board item 01M0989GZ0PQAW0TN7APY1PHYW): the exact same
//! acceptance shape as `tests/memory_end_to_end.rs`'s
//! `a_foreign_labelled_sessions_record_reaches_a_different_sessions_
//! assembled_context`, except session A is created and populated entirely
//! through the FACADE -- `conway.new_session(SessionSpec { labels, .. })`
//! then `.prompt()` -- instead of being seeded directly against the store.
//! That file's own module doc explains why it had to seed directly: as of
//! this item, `SessionSpec::labels` actually reaches `SessionMeta.labels`
//! (`conway::Conway::new_session` -> `RootSpec::labels` -> `start_root`'s
//! `SessionMeta` literal, `conway-runtime`'s `runtime/root.rs`), so the
//! production path this test exercises now exists and this workaround is
//! no longer necessary for a NEW test that specifically wants to prove it.
//!
//! Same fixtures/shape as `memory_end_to_end.rs` (`build_conway`,
//! `base_config`, `text_response`, the `ScriptedBackend`/`FakeStore`
//! family) -- read that file first; duplicated here rather than shared
//! because Rust integration test files are independent binaries.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::{Conway, ConwayBuilder, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias};
use conway_core::ports::GenerateResponse;
use conway_testkit::{FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};

use conway_plugin_memory::{MemoryConfig, MemoryPlugin, DEFAULT_LABEL};

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

/// A real, fully-faked `Conway` (no network, no live provider) with
/// [`MemoryPlugin`] attached exactly the way a library embedder would --
/// mirrors `memory_end_to_end.rs`'s own `build_conway`.
fn build_conway(backend: Arc<ScriptedBackend>, store: Arc<FakeStore>) -> Conway {
    let gate: Arc<dyn conway::PermissionGate> =
        Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let router: Arc<dyn conway::Router> = Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }));
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(router)
        .with_plugin(Arc::new(MemoryPlugin::new(MemoryConfig::default())))
        .build()
        .expect("build should succeed with every port injected")
}

/// **THE CLOSING TEST.** Session A is created through the FACADE with
/// `SessionSpec { labels: vec![DEFAULT_LABEL.into()], .. }` and takes one
/// real prompt/reply turn through a real `Conway`/`SessionHandle` -- not
/// seeded directly against the store. Session B is a SEPARATE root session
/// (not forked from A, not a descendant of A) that takes its own real turn;
/// the assertion is on the ACTUAL `GenerateRequest` the fake backend
/// received for B's turn: A's own content, produced entirely through the
/// facade, is present as rendered segments.
#[tokio::test]
async fn a_facade_created_labelled_sessions_record_reaches_a_different_sessions_assembled_context()
{
    let store = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            // A's own turn (the reply `MemoryCurator` should later recall).
            ScriptedTurn::Respond(text_response(
                "Noted: the deploy secret lives in vault path secret/data/prod-deploy.",
            )),
            // B's own turn.
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(backend.clone(), store);

    // Session A: created and populated entirely through the FACADE, labelled
    // via `SessionSpec::labels` -- the write path this item adds.
    let session_a = conway
        .new_session(SessionSpec {
            labels: vec![DEFAULT_LABEL.to_string()],
            ..Default::default()
        })
        .await
        .expect("new_session for A should succeed");
    let turn_a = session_a
        .prompt("Remember this: the deploy secret lives in a vault path.")
        .await
        .expect("prompt A");
    tokio::time::timeout(Duration::from_secs(5), turn_a.result())
        .await
        .expect("A's result() must not hang")
        .expect("A's result() should succeed");

    // Session B: an UNRELATED root session -- no fork/spawn relationship to
    // A whatsoever, and not itself labelled.
    let session_b = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session for B should succeed");
    let turn_b = session_b
        .prompt("What should I check before deploying?")
        .await
        .expect("prompt B");
    tokio::time::timeout(Duration::from_secs(5), turn_b.result())
        .await
        .expect("B's result() must not hang")
        .expect("B's result() should succeed");

    // The assertion: B's OWN request to the backend carries A's content.
    let calls = backend.calls();
    let b_request = calls
        .last()
        .expect("the backend must have received B's own request");

    let mut all_text = String::new();
    for segment in &b_request.segments {
        for block in &segment.content {
            if let ContentBlock::Text { text } = block {
                all_text.push_str(text);
                all_text.push('\n');
            }
        }
    }

    assert!(
        all_text.contains("Remember this: the deploy secret lives in a vault path."),
        "session A's own user turn (created via the FACADE) must reach session B's assembled \
         request; got segments: {all_text:?}"
    );
    assert!(
        all_text.contains("Noted: the deploy secret lives in vault path secret/data/prod-deploy."),
        "session A's own assistant reply (produced via the FACADE) must ALSO reach session B's \
         assembled request; got segments: {all_text:?}"
    );
}
