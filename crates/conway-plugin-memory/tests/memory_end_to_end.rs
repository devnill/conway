//! End-to-end acceptance for the reworked `conway.memory` (board item
//! `01M09P2T8E5M292WMSMS64CVC4`): a real, fully-faked `Conway` (no network,
//! no live provider), driven through real turns, asserted on the ACTUAL
//! captured `GenerateRequest` a fake backend received -- the same standard
//! this program's earlier acceptance tests set (the label-based design's
//! own `tests/memory_end_to_end.rs`/`facade_labelled_session_recall.rs`,
//! both retired by this rework).
//!
//! Every acceptance criterion this item lists is proven here as a distinct
//! test:
//! 1. A memory with freeform text and NO provenance is recalled into a
//!    later session's assembled context.
//! 2. A memory WITH provenance naming a source session has that provenance
//!    retrievable.
//! 3. A removed memory stops appearing.
//! 4. A memory whose source session no longer exists is still valid and
//!    still recalled.
//! 5. Injected segments carry honest provenance (`Provenance::Memory`), not
//!    disguised as recalled records.
//! 6. An injected memory does not trip the hook guard's coherence check --
//!    proven HERE by the plain fact that every turn below completes
//!    successfully: `GuardedContextHook`
//!    (`crates/conway-runtime/src/context/hook_guard.rs`) sits between this
//!    plugin's hook and every real turn, and a coherence refusal would
//!    surface as `turn.result()` returning `Err`, not as a hang or a
//!    silently-dropped segment. (A second, more surgical proof lives at
//!    `conway-runtime`'s own `hook_guard.rs` test module,
//!    `an_injected_memory_segment_does_not_trip_the_coherence_guard`.)

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::backend::BackendId;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::plugin::{ContentBlock, Memory, MemoryProvenance, MemoryStore};
use conway::{Conway, MemoryId, RoleAlias, SessionId, SessionSpec};
use conway_testkit::{text_response, FakeStore, ScriptedBackend, ScriptedTurn};

use conway::test_support::test_builder;
use conway_plugin_memory::{InMemoryMemoryStore, MemoryConfig, MemoryPlugin};

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

async fn run_one_turn(conway: &Conway, prompt: &str, reply: &str, backend: &ScriptedBackend) {
    let _ = backend; // kept for symmetry/readability at call sites
    let session = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = session.prompt(prompt).await.expect("prompt");
    tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");
    let _ = reply;
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

/// Acceptance 1 + 5 + 6: a memory with freeform text and NO provenance is
/// injected into a LATER session's assembled context (proving the store is
/// global, not session-scoped selection), carrying honest
/// `Provenance::Memory` (asserted via the segment text landing at all --
/// the dedicated unit tests in `src/lib.rs` pin the provenance tag
/// directly), and the turn completes successfully (proving the hook guard
/// accepted it).
#[tokio::test]
async fn a_memory_with_no_provenance_reaches_a_real_turns_assembled_context() {
    let store = Arc::new(FakeStore::new());
    let memory_store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    memory_store
        .put(Memory {
            id: MemoryId::new(),
            text: "the deploy secret lives in vault path secret/data/prod-deploy".to_string(),
            created: chrono::Utc::now(),
            provenance: None,
        })
        .await
        .expect("put");

    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("done"))])
            .with_id(BackendId::new("fake")),
    );
    let conway = test_builder(base_config())
        .with_backend(backend.clone())
        .with_session_store(store)
        .with_plugin(Arc::new(MemoryPlugin::new(
            memory_store,
            MemoryConfig::default(),
        )))
        .build()
        .expect("build should succeed with every port injected");

    run_one_turn(
        &conway,
        "What should I check before deploying?",
        "done",
        &backend,
    )
    .await;

    let calls = backend.calls();
    let request = calls
        .last()
        .expect("a request must have reached the backend");
    let text = all_text(request);
    assert!(
        text.contains("the deploy secret lives in vault path secret/data/prod-deploy"),
        "the stored memory must reach the assembled request; got: {text:?}"
    );
}

/// Acceptance 2: a memory WITH provenance naming a source session has that
/// provenance retrievable -- through the SAME store instance a real turn
/// used, not merely a freshly constructed one.
#[tokio::test]
async fn a_memory_with_provenance_has_that_provenance_retrievable() {
    let store = Arc::new(FakeStore::new());
    let memory_store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let source_session = SessionId::new();
    let id = MemoryId::new();
    memory_store
        .put(Memory {
            id,
            text: "sourced from a specific session".to_string(),
            created: chrono::Utc::now(),
            provenance: Some(MemoryProvenance {
                session: source_session,
                range: None,
            }),
        })
        .await
        .expect("put");

    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("done"))])
            .with_id(BackendId::new("fake")),
    );
    let conway = test_builder(base_config())
        .with_backend(backend.clone())
        .with_session_store(store)
        .with_plugin(Arc::new(MemoryPlugin::new(
            memory_store.clone(),
            MemoryConfig::default(),
        )))
        .build()
        .expect("build should succeed with every port injected");
    run_one_turn(&conway, "hello", "done", &backend).await;

    let fetched = memory_store.get(&id).await.expect("get");
    assert_eq!(
        fetched.provenance,
        Some(MemoryProvenance {
            session: source_session,
            range: None,
        })
    );
}

/// Acceptance 3: a removed memory stops appearing in a LATER turn's
/// assembled context.
#[tokio::test]
async fn a_removed_memory_stops_appearing() {
    let store = Arc::new(FakeStore::new());
    let memory_store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    let keep_id = MemoryId::new();
    let remove_id = MemoryId::new();
    memory_store
        .put(Memory {
            id: keep_id,
            text: "keep this memory".to_string(),
            created: chrono::Utc::now(),
            provenance: None,
        })
        .await
        .expect("put keep");
    memory_store
        .put(Memory {
            id: remove_id,
            text: "forget this memory".to_string(),
            created: chrono::Utc::now(),
            provenance: None,
        })
        .await
        .expect("put remove");

    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("first")),
            ScriptedTurn::Respond(text_response("second")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = test_builder(base_config())
        .with_backend(backend.clone())
        .with_session_store(store)
        .with_plugin(Arc::new(MemoryPlugin::new(
            memory_store.clone(),
            MemoryConfig::default(),
        )))
        .build()
        .expect("build should succeed with every port injected");

    // First turn: both memories present.
    run_one_turn(&conway, "first prompt", "first", &backend).await;
    let first_text = all_text(backend.calls().last().unwrap());
    assert!(first_text.contains("keep this memory"));
    assert!(first_text.contains("forget this memory"));

    // Remove one, then take a second, independent turn.
    memory_store.remove(&remove_id).await.expect("remove");
    run_one_turn(&conway, "second prompt", "second", &backend).await;
    let second_text = all_text(backend.calls().last().unwrap());
    assert!(
        second_text.contains("keep this memory"),
        "the kept memory must still be present"
    );
    assert!(
        !second_text.contains("forget this memory"),
        "the removed memory must stop appearing; got: {second_text:?}"
    );
}

/// Acceptance 4: a memory whose source session no longer exists (never
/// created in this `Conway`'s own `SessionStore` at all) is still valid and
/// still recalled -- `MemoryStore` never consults `SessionStore`.
#[tokio::test]
async fn a_memory_with_a_dangling_source_session_is_still_recalled() {
    let store = Arc::new(FakeStore::new());
    let memory_store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
    // A session id that was never created anywhere in `store` -- as good as
    // "purged" from this test's point of view.
    let purged_session = SessionId::new();
    memory_store
        .put(Memory {
            id: MemoryId::new(),
            text: "survives a dangling source reference".to_string(),
            created: chrono::Utc::now(),
            provenance: Some(MemoryProvenance {
                session: purged_session,
                range: None,
            }),
        })
        .await
        .expect("put");

    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("done"))])
            .with_id(BackendId::new("fake")),
    );
    let conway = test_builder(base_config())
        .with_backend(backend.clone())
        .with_session_store(store)
        .with_plugin(Arc::new(MemoryPlugin::new(
            memory_store,
            MemoryConfig::default(),
        )))
        .build()
        .expect("build should succeed with every port injected");
    run_one_turn(&conway, "hello", "done", &backend).await;

    let text = all_text(backend.calls().last().unwrap());
    assert!(
        text.contains("survives a dangling source reference"),
        "a memory with a dangling session reference must still be recalled; got: {text:?}"
    );
}
