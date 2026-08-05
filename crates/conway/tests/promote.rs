//! Acceptance tests for `Conway::promote` (B3): the atomic
//! ephemeral→persistent flip — durable session-header rewrite
//! (`SessionStore::set_ephemeral`), live-tree flag flip
//! (`AgentTree::set_ephemeral`), and exactly one `Event::AgentPromoted`,
//! in that failure order. Also covers the guard matrix: non-ephemeral
//! refused (`NotPromotable`), double promote refused, unknown agent
//! refused (`AgentNotFound`, the facade-layer live check).
//!
//! The on-disk byte-level assertions (verbatim record preservation,
//! `index.jsonl` projection, reopen persistence) live in
//! `crates/conway-session/tests/promote_tests.rs` — this crate runs
//! against `FakeStore`, so here "the header flipped" is observed through
//! `store.meta` and the default `sessions()` listing.

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    RoleEntry, RoutingSection, SessionConfig, ToolsConfig, TuiSection,
};
use conway::{Conway, ConwayBuilder, ConwayError, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::content::ContentBlock;
use conway_core::error::{RuntimeError, StoreError};
use conway_core::event::Event;
use conway_core::fakes::{FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, SessionId};
use conway_core::log::SessionFilter;
use conway_core::ports::{Backend, GenerateResponse, SessionStore};
use futures_core::Stream as _;

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }))
}

fn text_response(text: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        tool_calls: vec![],
        stop: conway_core::content::StopReason::EndTurn,
        usage: conway_core::content::Usage::default(),
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
        tui: TuiSection::default(),
        tools: ToolsConfig::default(),
    }
}

fn build_conway_with_backend(store: Arc<dyn SessionStore>, backend: Arc<dyn Backend>) -> Conway {
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should succeed with every port injected")
}

/// Drives a session with one parent turn plus one completed `/ask`,
/// returning the handle and the ephemeral child's `(AgentId, SessionId)`.
async fn session_with_completed_ask(conway: &Conway) -> (conway::SessionHandle, AgentId, SessionId) {
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("parent turn").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("parent result must not hang")
        .expect("parent result should succeed");

    let ask_turn = handle.ask("an ephemeral aside").await.expect("ask");
    let _ = tokio::time::timeout(Duration::from_secs(5), ask_turn.result())
        .await
        .expect("ask result must not hang")
        .expect("ask result should succeed");

    let child_meta = conway
        .sessions(SessionFilter {
            include_ephemeral: true,
            ..SessionFilter::default()
        })
        .await
        .expect("sessions() should succeed")
        .into_iter()
        .find(|m| m.id != handle.id())
        .expect("the ephemeral ask child must be present");
    (handle, child_meta.agent_id, child_meta.id)
}

// ---------------------------------------------------------------------
// Happy path: all three flips, in order, observable from every view.
// ---------------------------------------------------------------------

#[tokio::test]
async fn promote_flips_header_tree_and_listing_and_emits_agent_promoted() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("parent turn").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("parent result must not hang")
        .expect("parent result should succeed");

    let ask_turn = handle.ask("an ephemeral aside").await.expect("ask");
    let _ = tokio::time::timeout(Duration::from_secs(5), ask_turn.result())
        .await
        .expect("ask result must not hang")
        .expect("ask result should succeed");

    let child_meta = conway
        .sessions(SessionFilter {
            include_ephemeral: true,
            ..SessionFilter::default()
        })
        .await
        .expect("sessions() should succeed")
        .into_iter()
        .find(|m| m.id != handle.id())
        .expect("the ephemeral ask child must be present");
    let (child_agent, child_session) = (child_meta.agent_id, child_meta.id);
    assert!(child_meta.ephemeral, "precondition: the child is ephemeral");
    assert_eq!(
        conway
            .sessions(SessionFilter::default())
            .await
            .expect("sessions()")
            .len(),
        1,
        "precondition: the ephemeral child is hidden from the default listing"
    );

    // Subscribe BEFORE promote, on the PARENT-scoped stream: `AgentPromoted`
    // is stamped under the child's own session, so this also proves the
    // `EventStream::accept` lifecycle-style passthrough for it.
    let mut events = handle.events();

    let returned = conway
        .promote(child_agent)
        .await
        .expect("promote of an ephemeral ask child must succeed");
    assert_eq!(
        returned, child_session,
        "promote must return the promoted agent's own (unchanged) session id"
    );

    // 1. The durable header flipped.
    let meta = store
        .meta(&child_session)
        .await
        .expect("meta should succeed");
    assert!(!meta.ephemeral, "the session header must show ephemeral: false");
    assert_eq!(
        meta.origin.as_ref().map(|o| o.parent),
        Some(handle.id()),
        "P-2: promotion preserves the record — origin/provenance untouched"
    );

    // ... so the default (exclude-ephemeral) listing now surfaces it.
    let listing = conway
        .sessions(SessionFilter::default())
        .await
        .expect("sessions() should succeed");
    assert!(
        listing.iter().any(|m| m.id == child_session),
        "the promoted child must now appear in the default listing"
    );

    // 2. The live tree flipped.
    let node = handle
        .tree()
        .nodes
        .into_iter()
        .find(|n| n.agent_id == child_agent)
        .expect("the child must still be attached");
    assert!(
        !node.ephemeral,
        "the tree snapshot must show the flipped flag"
    );
    assert_eq!(
        node.parent,
        Some(handle.root()),
        "B3: promotion is a flag flip only — no re-parenting"
    );

    // 3. Exactly one `AgentPromoted`, naming the child, reached the
    // parent-scoped stream.
    let envelope = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope = std::future::poll_fn(|cx| std::pin::Pin::new(&mut events).poll_next(cx))
                .await
                .expect("event stream open");
            if matches!(envelope.event, Event::AgentPromoted { .. }) {
                break envelope;
            }
        }
    })
    .await
    .expect("timed out waiting for AgentPromoted");
    assert_eq!(envelope.agent, child_agent);
    assert_eq!(envelope.session, child_session);

    // A double promote is refused by the store guard (the session is no
    // longer ephemeral), not silently accepted.
    let err = conway
        .promote(child_agent)
        .await
        .expect_err("a double promote must fail");
    assert!(
        matches!(err, ConwayError::Store(StoreError::NotPromotable { .. })),
        "double promote must be NotPromotable, got: {err:?}"
    );
}

// ---------------------------------------------------------------------
// Guard matrix
// ---------------------------------------------------------------------

/// Promoting a non-ephemeral agent (the session's own root) is refused by
/// the store guard — and the failure ordering guarantees NOTHING else
/// happened: no event, no tree change.
#[tokio::test]
async fn promote_a_non_ephemeral_agent_is_refused() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("parent ack"))])
            .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("parent turn").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("parent result must not hang")
        .expect("parent result should succeed");

    let mut events = handle.events();
    let err = conway
        .promote(handle.root())
        .await
        .expect_err("promoting a non-ephemeral session must fail");
    assert!(
        matches!(err, ConwayError::Store(StoreError::NotPromotable { session, .. }) if session == handle.id()),
        "non-ephemeral promote must be NotPromotable for the root's own session, got: {err:?}"
    );

    // No `AgentPromoted` was emitted (the header rewrite failed first, so
    // the tree flip and the event never happened — the binding failure
    // ordering). Drain the stream briefly: only the root's own turn
    // traffic may appear, never a promote event.
    let saw_promoted = tokio::time::timeout(Duration::from_millis(300), async {
        loop {
            let envelope = std::future::poll_fn(|cx| std::pin::Pin::new(&mut events).poll_next(cx))
                .await
                .expect("event stream open");
            if matches!(envelope.event, Event::AgentPromoted { .. }) {
                break true;
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        !saw_promoted,
        "a refused promote must never emit AgentPromoted"
    );
}

/// The facade-layer live check: an agent id that is not present in
/// `Runtime::tree()` is refused with `AgentNotFound` BEFORE any store
/// access — even when (as here) no such session exists at all.
#[tokio::test]
async fn promote_an_unknown_agent_is_refused() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("parent ack"))])
            .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    let (handle, _child_agent, _child_session) = session_with_completed_ask(&conway).await;
    let _ = handle;

    let unknown = AgentId::new();
    let err = conway
        .promote(unknown)
        .await
        .expect_err("an unknown agent must fail");
    assert!(
        matches!(err, ConwayError::Runtime(RuntimeError::AgentNotFound { agent }) if agent == unknown),
        "unknown agent must be AgentNotFound, got: {err:?}"
    );
}
