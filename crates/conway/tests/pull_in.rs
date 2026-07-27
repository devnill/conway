//! Acceptance tests for `Conway::pull_in` (B4): an ephemeral `/ask` child's
//! turns merged into the parent's log VERBATIM — the child's `ForkDirective`
//! head record materialized as a `UserTurn` re-stamped
//! `Provenance::MergedAsk { from: child_session }`, its `Assistant` records
//! passed through untouched (P-10) — followed by the child's purge via
//! `SessionStore::remove` (B1). Also covers the guard matrix: parent not
//! live (`AgentNotLive`), child has children (`NotRemovable`), non-ephemeral
//! child (`NotRemovable`), unknown child (`AgentNotFound`) — each refusing
//! BEFORE the parent's log is mutated.

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    RoleEntry, RoutingSection, SessionConfig, TuiSection,
};
use conway::{Conway, ConwayBuilder, ConwayError, Provenance, SessionHandle, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::content::ContentBlock;
use conway_core::error::{RuntimeError, StoreError};
use conway_core::fakes::{FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use conway_core::ids::{AgentId, BackendId, LogSeq, ModelId, ModelRef, RoleAlias, SeqRange, SessionId};
use conway_core::log::{LogRecord, SessionFilter, SessionMeta, SessionStatus};
use conway_core::ports::{Backend, GenerateResponse, SessionStore};

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

/// Drives a KEEP-ALIVE session (so the parent stays live — non-terminal
/// `AgentStatus` — for `pull_in`'s guard) through one parent turn plus one
/// completed `/ask`, returning the handle and the ephemeral child's
/// `(AgentId, SessionId)`.
async fn live_session_with_completed_ask(
    conway: &Conway,
) -> (SessionHandle, AgentId, SessionId) {
    let handle = conway
        .new_session(SessionSpec {
            keep_alive: true,
            ..SessionSpec::default()
        })
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("parent turn").await.expect("prompt");
    // keep_alive: `result()` would hang (no AgentFinished until the session
    // ends); `text()` resolves on the turn's own TurnFinished.
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.text())
        .await
        .expect("parent turn must not hang")
        .expect("parent turn should succeed");

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

async fn read_all(store: &Arc<dyn SessionStore>, sid: SessionId) -> Vec<LogRecord> {
    let head = store.head(&sid).await.expect("head should succeed");
    store
        .read(&sid, SeqRange::new(LogSeq::ZERO, Some(head)))
        .await
        .expect("read should succeed")
}

// ---------------------------------------------------------------------
// Happy path: verbatim merge, contiguous re-sequencing, child purged.
// ---------------------------------------------------------------------

#[tokio::test]
async fn pull_in_merges_the_ask_child_verbatim_then_purges_it() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("the ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    let (handle, child_agent, child_session) = live_session_with_completed_ask(&conway).await;

    // Capture the child's own records pre-merge as the verbatim baseline.
    let child_records = read_all(&store, child_session).await;
    let child_assistants: Vec<&LogRecord> = child_records
        .iter()
        .filter(|r| matches!(r, LogRecord::Assistant { .. }))
        .collect();
    assert_eq!(
        child_assistants.len(),
        1,
        "precondition: the scripted ask child produced exactly one Assistant record, got: {child_records:?}"
    );
    assert!(
        matches!(child_records.first(), Some(LogRecord::ForkDirective { .. })),
        "precondition (the B2 amendment): the child is ForkDirective-headed, got: {child_records:?}"
    );

    let parent_head_before = store.head(&handle.id()).await.expect("head");

    conway
        .pull_in(child_agent)
        .await
        .expect("pull_in of a completed ephemeral ask child must succeed");

    // The parent log gained exactly the merge set: the question (as a
    // UserTurn) plus the one Assistant record.
    let parent_records = read_all(&store, handle.id()).await;
    let before = parent_head_before.0 as usize;
    assert_eq!(
        parent_records.len(),
        before + 2,
        "expected the question + the answer appended, got: {parent_records:?}"
    );

    // The store re-sequenced the appended records: seqs are contiguous
    // across the whole parent log (no child-local seqs leaked through).
    for (i, record) in parent_records.iter().enumerate() {
        assert_eq!(
            record.seq(),
            Some(LogSeq(i as u64)),
            "parent log seqs must be contiguous after the merge"
        );
    }

    // The question landed as a UserTurn re-stamped MergedAsk, naming the
    // (now purged) child session.
    match &parent_records[before] {
        LogRecord::UserTurn { text, prov, .. } => {
            assert_eq!(text, "an ephemeral aside");
            assert_eq!(
                *prov,
                Provenance::MergedAsk {
                    from: child_session
                },
                "the merged question must carry MergedAsk provenance (P-2/GP-10)"
            );
        }
        other => panic!("expected the merged question as a UserTurn, got: {other:?}"),
    }

    // The answer landed VERBATIM: every untrusted model-produced field
    // (P-10) — content, model, route_reason, usage, stop — plus ts is
    // field-for-field the child's original record; only the seq moved.
    let (
        merged_content,
        merged_model,
        merged_route,
        merged_usage,
        merged_stop,
        merged_ts,
    ) = match &parent_records[before + 1] {
        LogRecord::Assistant {
            content,
            model,
            route_reason,
            usage,
            stop,
            ts,
            ..
        } => (content, model, route_reason, usage, stop, ts),
        other => panic!("expected the merged answer as an Assistant record, got: {other:?}"),
    };
    match child_assistants[0] {
        LogRecord::Assistant {
            content,
            model,
            route_reason,
            usage,
            stop,
            ts,
            ..
        } => {
            assert_eq!(merged_content, content);
            assert_eq!(merged_model, model);
            assert_eq!(merged_route, route_reason);
            assert_eq!(merged_usage, usage);
            assert_eq!(merged_stop, stop);
            assert_eq!(merged_ts, ts);
        }
        other => unreachable!("filtered to Assistant above, got: {other:?}"),
    }

    // The child session is gone — from the store AND from every listing,
    // ephemeral-inclusive ones included.
    let err = store
        .meta(&child_session)
        .await
        .expect_err("the child's meta must be gone");
    assert!(matches!(err, StoreError::NotFound { session } if session == child_session));
    let remaining = conway
        .sessions(SessionFilter {
            include_ephemeral: true,
            ..SessionFilter::default()
        })
        .await
        .expect("sessions() should succeed");
    assert_eq!(
        remaining.len(),
        1,
        "only the parent's session may remain, got: {remaining:?}"
    );

    // A second remove of the child is NotFound (the purge was real, not a
    // flag flip).
    let err = store
        .remove(&child_session)
        .await
        .expect_err("remove of the purged child must be NotFound");
    assert!(matches!(err, StoreError::NotFound { session } if session == child_session));

    // The child's tree node stays attached (AgentTree never detaches — the
    // provenance that the ask happened survives the purge, P-2).
    assert!(
        handle
            .tree()
            .nodes
            .iter()
            .any(|n| n.agent_id == child_agent),
        "the child's tree node must survive the purge"
    );
}

// ---------------------------------------------------------------------
// Guard matrix — each refusal happens BEFORE the parent's log is mutated.
// ---------------------------------------------------------------------

/// Parent not live: a non-keep-alive parent is `Finished` once its first
/// turn completes, and merging into a log nothing will ever read again is
/// refused with `AgentNotLive` — and neither the merge nor the purge
/// happens.
#[tokio::test]
async fn pull_in_refuses_when_the_parent_is_not_live() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    // NOT keep_alive: the root agent task terminates on its first
    // Completed turn, leaving the tree node `Finished`.
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
        .expect("sessions()")
        .into_iter()
        .find(|m| m.id != handle.id())
        .expect("the ask child must be present");

    let parent_head_before = store.head(&handle.id()).await.expect("head");

    let err = conway
        .pull_in(child_meta.agent_id)
        .await
        .expect_err("pull_in with a finished parent must be refused");
    assert!(
        matches!(err, ConwayError::Runtime(RuntimeError::AgentNotLive { agent }) if agent == handle.root()),
        "expected AgentNotLive naming the parent, got: {err:?}"
    );

    // Failure ordering: the parent's log is untouched and the child was NOT
    // purged.
    assert_eq!(
        store.head(&handle.id()).await.expect("head"),
        parent_head_before,
        "a refused pull_in must not mutate the parent's log"
    );
    store
        .meta(&child_meta.id)
        .await
        .expect("the child session must survive a refused pull_in");
}

/// B1's guards apply: a child that has children of its own (even ephemeral
/// ones) is refused with `NotRemovable` — the merge would orphan the
/// grandchild's provenance.
#[tokio::test]
async fn pull_in_refuses_when_the_child_has_children() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    let (handle, child_agent, child_session) = live_session_with_completed_ask(&conway).await;

    // Give the child a child of its own, directly at the store layer (B1's
    // guard reads `list(parent: child, include_ephemeral: true)`, so a
    // store-level grandchild exercises exactly what `remove` will check).
    let child_head = store.head(&child_session).await.expect("head");
    let grandchild_meta = SessionMeta {
        id: SessionId::new(),
        agent_id: AgentId::new(),
        origin: None, // overridden by `fork`
        agent_def: None,
        role: None,
        created: chrono::Utc::now(),
        cwd: std::path::PathBuf::from("."),
        labels: vec![],
        status: SessionStatus::Active,
        ephemeral: false,
        ask_origin: None,
    };
    store
        .fork(&child_session, child_head, grandchild_meta)
        .await
        .expect("forking the ask child should succeed");

    let parent_head_before = store.head(&handle.id()).await.expect("head");

    let err = conway
        .pull_in(child_agent)
        .await
        .expect_err("pull_in of a child that has children must be refused");
    assert!(
        matches!(err, ConwayError::Store(StoreError::NotRemovable { session, .. }) if session == child_session),
        "expected NotRemovable naming the child session, got: {err:?}"
    );

    assert_eq!(
        store.head(&handle.id()).await.expect("head"),
        parent_head_before,
        "a refused pull_in must not mutate the parent's log"
    );
    store
        .meta(&child_session)
        .await
        .expect("the child session must survive a refused pull_in");
}

/// B1's guards apply: a non-ephemeral child (here: an ask child that was
/// first promoted, B3) is refused with `NotRemovable` — pull_in merges only
/// ephemeral scratchpads.
#[tokio::test]
async fn pull_in_refuses_a_non_ephemeral_child() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    let (handle, child_agent, child_session) = live_session_with_completed_ask(&conway).await;

    // Promote flips the child to persistent — after which pull_in must
    // refuse it (the two fates are mutually exclusive).
    conway
        .promote(child_agent)
        .await
        .expect("promote of an ephemeral ask child must succeed");

    let parent_head_before = store.head(&handle.id()).await.expect("head");

    let err = conway
        .pull_in(child_agent)
        .await
        .expect_err("pull_in of a non-ephemeral child must be refused");
    assert!(
        matches!(err, ConwayError::Store(StoreError::NotRemovable { session, .. }) if session == child_session),
        "expected NotRemovable naming the child session, got: {err:?}"
    );

    assert_eq!(
        store.head(&handle.id()).await.expect("head"),
        parent_head_before,
        "a refused pull_in must not mutate the parent's log"
    );
    store
        .meta(&child_session)
        .await
        .expect("the (promoted) child session must survive a refused pull_in");
}

/// The facade-layer live check: an agent id absent from `Runtime::tree()`
/// is refused with `AgentNotFound` before any store access.
#[tokio::test]
async fn pull_in_an_unknown_child_is_refused() {
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

    let unknown = AgentId::new();
    let err = conway
        .pull_in(unknown)
        .await
        .expect_err("an unknown child must fail");
    assert!(
        matches!(err, ConwayError::Runtime(RuntimeError::AgentNotFound { agent }) if agent == unknown),
        "unknown child must be AgentNotFound, got: {err:?}"
    );
}

/// The child must be TERMINAL: a still-running child is refused BEFORE any
/// merge (cycle-5 B4 review, significant finding 1). Merging mid-turn would
/// read a partial child log and then purge the session under a live agent
/// loop (its next append would fail `NotFound`). Terminal status is
/// absorbing, so the snapshot check can never go stale.
#[tokio::test]
async fn pull_in_refuses_a_still_running_child() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    // The child's only scripted turn never resolves, so the ask child is
    // deterministically mid-turn (non-terminal) when pull_in is called.
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Pending]).with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    // keep_alive root, NO parent prompt: the parent is live (idle), and the
    // child turn is the only backend call.
    let handle = conway
        .new_session(SessionSpec {
            keep_alive: true,
            ..SessionSpec::default()
        })
        .await
        .expect("new_session should succeed");
    let ask_turn = handle.ask("an ephemeral aside").await.expect("ask");

    // Poll the tree until the ephemeral child node appears in a non-terminal
    // state (attach precedes the turn; the Pending turn keeps it there).
    let mut child_node = None;
    for _ in 0..100 {
        if let Some(n) = handle
            .tree()
            .nodes
            .iter()
            .find(|n| n.parent == Some(handle.root()) && n.ephemeral)
        {
            child_node = Some(n.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let child_node = child_node.expect("the ask child must attach within 5s");
    assert!(
        !matches!(
            child_node.status,
            conway_core::agent::AgentStatus::Finished
                | conway_core::agent::AgentStatus::Failed
                | conway_core::agent::AgentStatus::Cancelled
        ),
        "precondition: the child must be non-terminal, got {:?}",
        child_node.status
    );

    let parent_head_before = store.head(&handle.id()).await.expect("head");

    let err = conway
        .pull_in(child_node.agent_id)
        .await
        .expect_err("pull_in of a still-running child must be refused");
    assert!(
        matches!(err, ConwayError::Store(StoreError::NotRemovable { .. })),
        "expected NotRemovable for a still-running child, got: {err:?}"
    );

    // Failure ordering: the parent's log is untouched and the child was NOT
    // purged.
    assert_eq!(
        store.head(&handle.id()).await.expect("head"),
        parent_head_before,
        "a refused pull_in must not mutate the parent's log"
    );
    store
        .meta(&child_node.session)
        .await
        .expect("the child session must survive a refused pull_in");

    // Do NOT await ask_turn (its scripted answer is 60s out); dropping it
    // leaves the runtime task to be torn down with the test runtime.
    drop(ask_turn);
}
