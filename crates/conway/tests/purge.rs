//! Acceptance tests for `Conway::purge` and `Conway::sweep_stale_modal_asks`
//! (B5): the `/ask` modal's `[esc]` discard fate (and the quit-with-modal
//! fallback), plus the TUI startup crash-residue sweep. Covers the purge
//! guard matrix (unknown agent -> `AgentNotFound`; still-running child ->
//! `NotRemovable`; non-ephemeral child -> `NotRemovable`) and the sweep's
//! `ask_origin` discrimination: `ModalAsk` residue is reaped, `ToolAsk`
//! children and untagged (pre-tag) ephemeral sessions are NEVER touched,
//! and a still-live modal-ask child is skipped.

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::{AskOrigin, Conway, ConwayBuilder, FacadeError, SessionHandle, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::error::{RuntimeError, StoreError};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, SessionId};
use conway_core::log::{SessionFilter, SessionMeta};
use conway_core::ports::{Backend, LiveOwner, SessionStore};
use conway_testkit::{
    text_response, FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn,
};

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
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
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

/// A keep-alive session through one completed `/ask`, returning the handle
/// and the ephemeral child's `(AgentId, SessionId)` (mirrors
/// `pull_in.rs`'s helper: the modal only offers fates for a COMPLETED
/// ask).
async fn live_session_with_completed_ask(conway: &Conway) -> (SessionHandle, AgentId, SessionId) {
    let handle = conway
        .new_session(SessionSpec {
            keep_alive: true,
            ..SessionSpec::default()
        })
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("parent turn").await.expect("prompt");
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

// ---------------------------------------------------------------------
// Conway::purge -- the discard fate.
// ---------------------------------------------------------------------

/// The happy path: a completed ephemeral ask child is purged outright --
/// the session is gone from the store AND from every listing, and the
/// parent's log is untouched (purge merges nothing, unlike pull_in).
#[tokio::test]
async fn purge_discards_a_completed_ask_child_without_touching_the_parent() {
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
    let parent_head_before = store.head(&handle.id()).await.expect("head");

    conway
        .purge(child_agent)
        .await
        .expect("purge of a completed ephemeral ask child must succeed");

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
    assert_eq!(
        store.head(&handle.id()).await.expect("head"),
        parent_head_before,
        "purge must not append anything to the parent's log (that is pull_in)"
    );

    // The child's tree node stays attached (AgentTree never detaches -- the
    // provenance that the ask happened survives the purge).
    assert!(
        handle
            .tree()
            .nodes
            .iter()
            .any(|n| n.agent_id == child_agent),
        "the child's tree node must survive the purge"
    );
}

/// The facade-layer live check: an agent id absent from `Runtime::tree()`
/// is refused with `AgentNotFound` before any store access.
#[tokio::test]
async fn purge_an_unknown_agent_is_refused() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("parent ack"))])
            .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    let unknown = AgentId::new();
    let err = conway
        .purge(unknown)
        .await
        .expect_err("an unknown agent must fail");
    assert!(
        matches!(err, FacadeError::Runtime(RuntimeError::AgentNotFound { agent }) if agent == unknown),
        "unknown agent must be AgentNotFound, got: {err:?}"
    );
}

/// The fates are exclusive: once a child is promoted (B3) it is no longer
/// ephemeral, so the discard fate is refused with `NotRemovable` (B1's
/// store guard, authoritative at the store layer).
#[tokio::test]
async fn purge_a_promoted_child_is_refused() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    let (_handle, child_agent, child_session) = live_session_with_completed_ask(&conway).await;
    conway
        .promote(child_agent)
        .await
        .expect("promote of an ephemeral ask child must succeed");

    let err = conway
        .purge(child_agent)
        .await
        .expect_err("purge of a promoted (non-ephemeral) child must be refused");
    assert!(
        matches!(err, FacadeError::Store(StoreError::NotRemovable { session, .. }) if session == child_session),
        "expected NotRemovable naming the child session, got: {err:?}"
    );
    store
        .meta(&child_session)
        .await
        .expect("the (promoted) child session must survive a refused purge");
}

/// A still-running child cannot be discarded: purging under a live agent
/// loop would orphan its next append (the same still-running guard
/// `pull_in` carries; terminal status is absorbing, so the snapshot check
/// cannot go stale).
#[tokio::test]
async fn purge_a_still_running_child_is_refused() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    // The child's only scripted turn never resolves, so the ask child is
    // deterministically mid-turn (non-terminal) when purge is called.
    let backend =
        Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Pending]).with_id(BackendId::new("fake")));
    let conway = build_conway_with_backend(store.clone(), backend);

    let handle = conway
        .new_session(SessionSpec {
            keep_alive: true,
            ..SessionSpec::default()
        })
        .await
        .expect("new_session should succeed");
    let ask_turn = handle.ask("an ephemeral aside").await.expect("ask");

    // Poll the tree until the ephemeral child node appears in a
    // non-terminal state (attach precedes the turn; the Pending turn keeps
    // it there).
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

    let err = conway
        .purge(child_node.agent_id)
        .await
        .expect_err("purge of a still-running child must be refused");
    assert!(
        matches!(err, FacadeError::Store(StoreError::NotRemovable { .. })),
        "expected NotRemovable for a still-running child, got: {err:?}"
    );
    store
        .meta(&child_node.session)
        .await
        .expect("the child session must survive a refused purge");

    // Do NOT await ask_turn (its scripted answer never resolves); dropping
    // it leaves the runtime task to be torn down with the test runtime.
    drop(ask_turn);
}

// ---------------------------------------------------------------------
// Conway::sweep_stale_modal_asks -- the TUI startup crash-residue sweep.
// ---------------------------------------------------------------------

/// The core sweep criterion: after a "restart" (a fresh `Conway` over the
/// SAME store, so the crashed process's live tree is gone), a leftover
/// MODAL-ask ephemeral session is reaped, but a TOOL-ask ephemeral session
/// and an untagged (pre-`ask_origin`) ephemeral session are NOT.
#[tokio::test]
async fn sweep_reaps_modal_ask_residue_but_never_tool_ask_or_untagged_sessions() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("the ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    // A completed modal-ask child (SessionHandle::ask stamps
    // AskOrigin::ModalAsk -- the precondition assertion below pins that
    // stamping, since the whole discrimination depends on it).
    let (_handle, _child_agent, child_session) = live_session_with_completed_ask(&conway).await;
    let modal_meta = store.meta(&child_session).await.expect("child meta");
    assert_eq!(
        modal_meta.ask_origin,
        Some(AskOrigin::ModalAsk),
        "SessionHandle::ask must stamp AskOrigin::ModalAsk at creation"
    );

    // A tool-ask-shaped ephemeral session, stamped directly at the store
    // layer exactly the way `conway-tools`' `AskTool` has the runtime stamp
    // its children (AskOrigin::ToolAsk).
    let root_session = modal_meta
        .origin
        .as_ref()
        .expect("the child has an origin")
        .parent;
    let root_head = store.head(&root_session).await.expect("head");
    let tool_ask_meta = SessionMeta {
        id: SessionId::new(),
        agent_id: AgentId::new(),
        origin: None, // overridden by `fork`
        agent_def: None,
        role: None,
        created: chrono::Utc::now(),
        cwd: std::path::PathBuf::from("."),
        labels: vec![],
        ephemeral: true,
        ask_origin: Some(AskOrigin::ToolAsk),
        root: None,
        plugin_config: conway_core::ports::PluginConfig::default(),
    };
    let tool_ask_session = store
        .fork(&root_session, root_head, tool_ask_meta)
        .await
        .expect("forking a tool-ask-shaped session should succeed");

    // An untagged ephemeral session (every header written before the tag
    // existed decodes this way) -- also never sweep-eligible.
    let untagged_meta = SessionMeta {
        id: SessionId::new(),
        agent_id: AgentId::new(),
        origin: None, // overridden by `fork`
        agent_def: None,
        role: None,
        created: chrono::Utc::now(),
        cwd: std::path::PathBuf::from("."),
        labels: vec![],
        ephemeral: true,
        ask_origin: None,
        root: None,
        plugin_config: conway_core::ports::PluginConfig::default(),
    };
    let untagged_session = store
        .fork(&root_session, root_head, untagged_meta)
        .await
        .expect("forking an untagged ephemeral session should succeed");

    // The "restart": a fresh Conway over the SAME store. Its runtime's tree
    // is empty -- nothing the crashed process had live is live here, so
    // every leftover is residue. (A fresh ScriptedBackend: the sweep makes
    // no backend calls.)
    let restarted = build_conway_with_backend(
        store.clone(),
        Arc::new(ScriptedBackend::new(vec![]).with_id(BackendId::new("fake"))),
    );

    let purged = restarted
        .sweep_stale_modal_asks(chrono::Duration::seconds(60))
        .await
        .expect("the sweep must succeed");
    assert_eq!(purged, 1, "exactly the modal-ask leftover must be reaped");

    let err = store
        .meta(&child_session)
        .await
        .expect_err("the modal-ask leftover must be gone");
    assert!(matches!(err, StoreError::NotFound { .. }));
    assert_eq!(
        store
            .meta(&tool_ask_session)
            .await
            .expect("a tool-ask session must NEVER be swept")
            .ask_origin,
        Some(AskOrigin::ToolAsk)
    );
    store
        .meta(&untagged_session)
        .await
        .expect("an untagged ephemeral session must NEVER be swept");
}

/// The not-live caution: called on the SAME Conway whose runtime still has
/// the modal-ask child live in its tree, the sweep skips that session
/// rather than purging it out from under a live agent.
#[tokio::test]
async fn sweep_skips_a_modal_ask_child_that_is_still_live_in_the_tree() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("the ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    let (_handle, child_agent, child_session) = live_session_with_completed_ask(&conway).await;
    assert!(
        conway
            .sessions(SessionFilter {
                include_ephemeral: true,
                ..SessionFilter::default()
            })
            .await
            .expect("sessions")
            .iter()
            .any(|m| m.agent_id == child_agent),
        "precondition: the child is present"
    );

    let purged = conway
        .sweep_stale_modal_asks(chrono::Duration::seconds(60))
        .await
        .expect("the sweep must succeed");

    assert_eq!(
        purged, 0,
        "a live modal-ask child must be skipped, not reaped"
    );
    store
        .meta(&child_session)
        .await
        .expect("the live child session must survive the sweep");
}

// ---------------------------------------------------------------------
// Conway::sweep_stale_modal_asks -- cross-process liveness (S1 follow-up).
// ---------------------------------------------------------------------

/// Builds a Conway, drives one completed `/ask` (leaving a `ModalAsk`
/// ephemeral child in `store`), then returns a FRESH "restarted" `Conway`
/// over the SAME store with an empty backend — its runtime tree is empty,
/// so the leftover child is residue to it. This is the "crashed process
/// left a modal-ask child behind, a new TUI starts against the same store"
/// shape, without the time cost of a real restart.
async fn restarted_with_modal_ask_residue(store: Arc<dyn SessionStore>) -> (Conway, SessionId) {
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("the ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway_a = build_conway_with_backend(store.clone(), backend);
    let (_handle, _child_agent, child_session) = live_session_with_completed_ask(&conway_a).await;
    let restarted = build_conway_with_backend(
        store.clone(),
        Arc::new(ScriptedBackend::new(vec![]).with_id(BackendId::new("fake"))),
    );
    (restarted, child_session)
}

/// S1 follow-up, the core new behavior: if the store carries a FRESH
/// liveness marker (another process is actively using this store), the
/// sweep defers entirely — it returns 0 and leaves the other process's
/// modal-ask child untouched, rather than reaping it as "residue" (the
/// not-live check is per-process; without the marker the second TUI would
/// see the first's open child as not-live and purge it).
#[tokio::test]
async fn sweep_defers_when_a_fresh_live_owner_marker_is_present() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (restarted, child_session) = restarted_with_modal_ask_residue(store.clone()).await;

    // Simulate ANOTHER live process owning this store: a fresh marker (pid
    // 999, heartbeat = now). The sweep must read it and defer.
    store
        .touch_live_owner(999)
        .await
        .expect("publishing the other process's marker");

    let purged = restarted
        .sweep_stale_modal_asks(chrono::Duration::seconds(60))
        .await
        .expect("the sweep must succeed");
    assert_eq!(
        purged, 0,
        "a fresh live-owner marker means another process owns this store; the sweep must defer"
    );
    store
        .meta(&child_session)
        .await
        .expect("the other process's modal-ask child must survive the deferred sweep");
}

/// S1 follow-up, crash-recovery: a STALE marker (heartbeat older than the
/// threshold) is read as "no live owner" — the owning process has stopped
/// heartbeating (crashed or exited without clearing), so its modal-ask
/// residue is reaped exactly as on a cold store with no marker.
#[tokio::test]
async fn sweep_reaps_when_the_live_owner_marker_is_stale() {
    // Hold the concrete `Arc<FakeStore>` so the test can inject a STALE
    // heartbeat via the fake's test setter — `touch_live_owner` stamps `now`,
    // and the sweep's decision is time-based (the test must not sleep). The
    // residue helper takes an erased clone of the SAME store.
    let fake = Arc::new(FakeStore::new());
    let store: Arc<dyn SessionStore> = fake.clone();
    let (restarted, child_session) = restarted_with_modal_ask_residue(store.clone()).await;

    // pid 999 but heartbeat 120s ago, past the 60s threshold this test
    // passes explicitly (Stage 2a: the threshold is caller-supplied, not a
    // facade constant -- see the new test immediately below this one for
    // the case where a DIFFERENT, caller-chosen threshold changes the
    // outcome).
    fake.set_live_owner(Some(LiveOwner {
        pid: 999,
        heartbeat: chrono::Utc::now() - chrono::Duration::seconds(120),
    }));

    let purged = restarted
        .sweep_stale_modal_asks(chrono::Duration::seconds(60))
        .await
        .expect("the sweep must succeed");
    assert_eq!(
        purged, 1,
        "a stale marker means no live owner; the modal-ask residue must be reaped"
    );
    let err = store
        .meta(&child_session)
        .await
        .expect_err("the reaped residue must be gone");
    assert!(matches!(err, StoreError::NotFound { .. }));
}

/// **Stage 2a: `live_threshold` is genuinely caller-supplied, not a
/// relocated constant.** The SAME marker (heartbeat 20s old) is "fresh"
/// under the pre-Stage-2a hardcoded default (60s, still exercised by every
/// other test in this file above) and "stale" under a caller-chosen 10s
/// threshold -- a single fixed heartbeat age, two different callers, two
/// different, observed sweep OUTCOMES (deferred vs. reaped), which is what
/// proves the parameter actually drives behavior rather than merely being
/// accepted and ignored. If this test used only one threshold value it
/// could pass even with the argument stored but never consulted.
#[tokio::test]
async fn sweep_stale_modal_asks_threshold_is_caller_supplied_not_a_relocated_constant() {
    let fake = Arc::new(FakeStore::new());
    let store: Arc<dyn SessionStore> = fake.clone();
    let (restarted, child_session) = restarted_with_modal_ask_residue(store.clone()).await;

    // pid 999, heartbeat 20s old -- inside the pre-Stage-2a 60s default,
    // outside a caller-chosen 10s threshold.
    fake.set_live_owner(Some(LiveOwner {
        pid: 999,
        heartbeat: chrono::Utc::now() - chrono::Duration::seconds(20),
    }));

    let purged_under_60s = restarted
        .sweep_stale_modal_asks(chrono::Duration::seconds(60))
        .await
        .expect("the sweep must succeed");
    assert_eq!(
        purged_under_60s, 0,
        "a 20s-old marker is FRESH under a 60s threshold -- the sweep must defer"
    );
    store
        .meta(&child_session)
        .await
        .expect("deferred under the 60s threshold: the residue must still be present");

    let purged_under_10s = restarted
        .sweep_stale_modal_asks(chrono::Duration::seconds(10))
        .await
        .expect("the sweep must succeed");
    assert_eq!(
        purged_under_10s, 1,
        "the SAME 20s-old marker is STALE under a caller-supplied 10s threshold -- the sweep \
         must reap it. A hardcoded-60s implementation (the pre-Stage-2a shape) would defer \
         here too and fail this assertion."
    );
    let err = store
        .meta(&child_session)
        .await
        .expect_err("reaped under the 10s threshold: the residue must be gone");
    assert!(matches!(err, StoreError::NotFound { .. }));
}

/// S1 follow-up, the clear-shutdown path: when the owning process cleared
/// its marker on exit (`clear_live_owner`), there is nothing to read and the
/// sweep reaps normally — the marker's absence is the desired end state, not
/// an error. (This is the same code path as a store that never had a marker.)
#[tokio::test]
async fn sweep_reaps_when_no_live_owner_marker_is_present() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (restarted, child_session) = restarted_with_modal_ask_residue(store.clone()).await;

    // No marker set — as on a cold start against a store whose last owner
    // cleared its marker, or one that never had an owner.
    let purged = restarted
        .sweep_stale_modal_asks(chrono::Duration::seconds(60))
        .await
        .expect("the sweep must succeed");
    assert_eq!(
        purged, 1,
        "with no live owner the modal-ask residue is reaped"
    );
    store
        .meta(&child_session)
        .await
        .expect_err("the reaped residue must be gone");
}
