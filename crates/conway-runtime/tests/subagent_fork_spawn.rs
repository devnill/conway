//! Acceptance tests for `impl SubagentHost for Runtime` (WI-084,
//! architecture §4.6, §5.1, §5.2): fork/spawn, inherited context, and
//! session forking.
//!
//! Built entirely from `conway-core`'s fakes plus a local `CountingStore`
//! decorator (mirrors `runtime_api.rs`'s and `agent_loop_e2e.rs`'s own
//! practice of small local test doubles) -- this file does not depend on
//! `conway-plugin-backends` or `conway-tools`.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use conway_core::agent::{
    AgentDefRef, Budget, CancelMode, PermissionDecision, ResultStatus, SubagentMode, SubagentSpec,
    ToolSelector,
};
use conway_core::capabilities::{HeadroomPolicy, RequiredCaps};
use conway_core::config::AgentDef;
use conway_core::content::{ContentBlock, Role, SamplingParams, StopReason, Usage};
use conway_core::error::{RuntimeError, SubagentError, ToolError};
use conway_core::event::Event;
use conway_core::fakes::{
    FakeGate, FakeHealth, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn,
};
use conway_core::ids::{AgentId, BackendId, LogSeq, ModelId, ModelRef, RoleAlias, SessionId};
use conway_core::log::{ForkOrigin, SessionFilter, SessionMeta};
use conway_core::ports::{
    Backend, ContextHook, ContextHookCtx, ContextPayload, LiveOwner, Router, SessionStore,
    SubagentHandle, SubagentHost,
};
use conway_core::provenance::Provenance;
use conway_core::routing::{HealthConfig, MinimalRouter, RoleConfig, RoutingConfig};
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{ResumeSpec, RootSpec, Runtime, RuntimeDeps};
use futures::StreamExt;

// ---------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------

fn text_response(text: &str) -> conway_core::ports::GenerateResponse {
    conway_core::ports::GenerateResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
    }
}

/// A `SessionStore` decorator that counts `fork` calls and otherwise
/// delegates verbatim -- lets tests assert "`SessionStore::fork` exactly
/// once" directly, rather than only inferring it from record counts.
struct CountingStore {
    inner: Arc<dyn SessionStore>,
    fork_calls: AtomicUsize,
}

impl CountingStore {
    fn new(inner: Arc<dyn SessionStore>) -> Self {
        Self {
            inner,
            fork_calls: AtomicUsize::new(0),
        }
    }

    fn fork_call_count(&self) -> usize {
        self.fork_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SessionStore for CountingStore {
    async fn create(&self, meta: SessionMeta) -> Result<SessionId, conway_core::error::StoreError> {
        self.inner.create(meta).await
    }
    async fn append(
        &self,
        sid: &SessionId,
        rec: conway_core::log::LogRecord,
    ) -> Result<LogSeq, conway_core::error::StoreError> {
        self.inner.append(sid, rec).await
    }
    async fn read(
        &self,
        sid: &SessionId,
        range: conway_core::ids::SeqRange,
    ) -> Result<Vec<conway_core::log::LogRecord>, conway_core::error::StoreError> {
        self.inner.read(sid, range).await
    }
    async fn head(&self, sid: &SessionId) -> Result<LogSeq, conway_core::error::StoreError> {
        self.inner.head(sid).await
    }
    async fn fork(
        &self,
        parent: &SessionId,
        at: LogSeq,
        meta: SessionMeta,
    ) -> Result<SessionId, conway_core::error::StoreError> {
        self.fork_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.fork(parent, at, meta).await
    }
    async fn meta(&self, sid: &SessionId) -> Result<SessionMeta, conway_core::error::StoreError> {
        self.inner.meta(sid).await
    }
    async fn children(
        &self,
        sid: &SessionId,
    ) -> Result<Vec<SessionId>, conway_core::error::StoreError> {
        self.inner.children(sid).await
    }
    async fn list(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<SessionMeta>, conway_core::error::StoreError> {
        self.inner.list(filter).await
    }
    async fn remove(&self, sid: &SessionId) -> Result<(), conway_core::error::StoreError> {
        self.inner.remove(sid).await
    }
    async fn set_ephemeral(
        &self,
        sid: &SessionId,
        ephemeral: bool,
    ) -> Result<(), conway_core::error::StoreError> {
        self.inner.set_ephemeral(sid, ephemeral).await
    }

    async fn live_owner(&self) -> Result<Option<LiveOwner>, conway_core::error::StoreError> {
        self.inner.live_owner().await
    }

    async fn touch_live_owner(&self, pid: u32) -> Result<(), conway_core::error::StoreError> {
        self.inner.touch_live_owner(pid).await
    }

    async fn clear_live_owner(&self) -> Result<(), conway_core::error::StoreError> {
        self.inner.clear_live_owner().await
    }
}

/// Builds a runtime whose backend script has `turns` text-only responses
/// queued (one per agent this test expects to complete a single turn) and
/// whose `agent_defs` contains the given defs. Returns the runtime plus the
/// `CountingStore`-wrapped `FakeStore` underneath it.
fn build_runtime(
    turns: usize,
    agent_defs: HashMap<String, AgentDef>,
) -> (Arc<Runtime>, Arc<CountingStore>) {
    let fake = Arc::new(FakeStore::new());
    let store = Arc::new(CountingStore::new(fake));
    let store_dyn: Arc<dyn SessionStore> = store.clone();

    let backend = Arc::new(
        ScriptedBackend::new(
            (0..turns)
                .map(|_| ScriptedTurn::Respond(text_response("ok")))
                .collect(),
        )
        .with_id(BackendId::new("b")),
    );
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);

    let runtime = Runtime::new(RuntimeDeps {
        store: store_dyn,
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs,
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),
    });
    (runtime, store)
}

fn root_spec(prompt: &str) -> RootSpec {
    RootSpec {
        session: None,
        agent_def: None,
        role: Some(RoleAlias::new("planner")),
        tools: None,
        budget: Budget::default(),
        cwd: PathBuf::from("/tmp"),
        root: None,
        prompt: Some(prompt.to_string()),
        keep_alive: false,
        model: None,
    }
}

fn fork_spec(prompt: &str) -> SubagentSpec {
    SubagentSpec::fork(prompt, Budget::default())
}

fn reviewer_def() -> AgentDef {
    AgentDef {
        name: "reviewer".to_string(),
        description: None,
        system_prompt: "You are a careful reviewer.".to_string(),
        role: Some(RoleAlias::new("reviewer")),
        model: None,
        tools: ToolSelector::All,
        skills: Vec::new(),
        max_steps: None,
        result_contract: None,
    }
}

fn session_of(runtime: &Runtime, agent: AgentId) -> SessionId {
    runtime
        .tree()
        .nodes
        .iter()
        .find(|n| n.agent_id == agent)
        .expect("agent present in tree")
        .session
}

async fn wait_for_agent_finished(
    stream: &mut conway_runtime::events::EventStream,
    agent: AgentId,
) -> conway_core::agent::AgentResult {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let envelope = stream.next().await.expect("event stream ended early");
            if envelope.agent == agent {
                if let Event::AgentFinished { result, .. } = envelope.event {
                    return result;
                }
            }
        }
    })
    .await
    .expect("agent never finished")
}

async fn start_and_finish_root(runtime: &Runtime, prompt: &str) -> AgentId {
    let mut stream = runtime.subscribe();
    let root = runtime.start_root(root_spec(prompt)).await.unwrap();
    wait_for_agent_finished(&mut stream, root).await;
    root
}

// ---------------------------------------------------------------------
// Fork: SessionStore::fork exactly once, zero records copied, ForkOrigin
// ---------------------------------------------------------------------

#[tokio::test]
async fn fork_calls_store_fork_exactly_once_and_copies_zero_records() {
    let (runtime, store) = build_runtime(2, HashMap::new());
    let root = start_and_finish_root(&runtime, "investigate").await;
    let parent_session = session_of(&runtime, root);
    let records_before = store.inner.clone();
    let _ = records_before; // keep the inner handle alive for clarity only

    let fake_before = fake_total_records(&store).await;

    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(&*runtime, root, root, fork_spec("look closer"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    assert_eq!(
        store.fork_call_count(),
        1,
        "start(Fork) must call SessionStore::fork exactly once"
    );

    let child_session = session_of(&runtime, child);
    let meta = store.meta(&child_session).await.unwrap();
    let parent_head = store.head(&parent_session).await.unwrap();
    match meta.origin {
        Some(ForkOrigin {
            parent,
            at_seq,
            mode,
        }) => {
            assert_eq!(parent, parent_session);
            assert_eq!(mode, SubagentMode::Fork);
            // `at_seq` was captured as the freeze point at fork time, before
            // the child (or the parent's own continued turns) appended
            // anything further -- it must be <= the parent's current head.
            assert!(at_seq.0 <= parent_head.0);
        }
        None => panic!("fork child header must carry a ForkOrigin"),
    }

    // "copies zero records": the store's total record count only grew by
    // whatever the child itself appended (its ForkDirective, plus one
    // assistant turn) -- nothing from the parent's own file was copied in.
    let fake_after = fake_total_records(&store).await;
    let child_own = store
        .read(&child_session, conway_core::ids::SeqRange::full())
        .await
        .unwrap()
        .len();
    assert_eq!(fake_after, fake_before + child_own);
}

/// Total records across every session, read back through the public
/// `SessionStore` API (not `FakeStore::total_record_count`, which the
/// `CountingStore` wrapper hides) -- summed via `list` + `read`.
async fn fake_total_records(store: &CountingStore) -> usize {
    let mut total = 0;
    for meta in store.list(SessionFilter::default()).await.unwrap() {
        total += store
            .read(&meta.id, conway_core::ids::SeqRange::full())
            .await
            .unwrap()
            .len();
    }
    total
}

// ---------------------------------------------------------------------
// Fork context: InheritedPrefix covers exactly parent's 0..at_seq, then
// ForkDirective
// ---------------------------------------------------------------------

#[tokio::test]
async fn fork_context_contains_inherited_prefix_then_fork_directive() {
    let (runtime, store) = build_runtime(2, HashMap::new());
    let root = start_and_finish_root(&runtime, "investigate the bug").await;
    let parent_session = session_of(&runtime, root);
    let at_seq = store.head(&parent_session).await.unwrap();

    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(&*runtime, root, root, fork_spec("look closer"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    // Not every parent record produces a context segment (e.g.
    // `AgentResultRecord` -- the terminal-result record `finish()` appends
    // -- never does; see `context/builder.rs`'s `record_role_and_content`).
    // The expected count is therefore "how many of the parent's own
    // records, up to at_seq, are a kind the builder maps to a segment",
    // not `at_seq` itself.
    let parent_records = store
        .read(
            &parent_session,
            conway_core::ids::SeqRange::new(LogSeq::ZERO, Some(at_seq)),
        )
        .await
        .unwrap();
    let expected_inherited = parent_records
        .iter()
        .filter(|r| {
            matches!(
                r,
                conway_core::log::LogRecord::UserTurn { .. }
                    | conway_core::log::LogRecord::Assistant { .. }
                    | conway_core::log::LogRecord::ToolResultRecord { .. }
                    | conway_core::log::LogRecord::ForkDirective { .. }
                    | conway_core::log::LogRecord::ParentSteer { .. }
                    | conway_core::log::LogRecord::SystemNote { .. }
            )
        })
        .count();

    let report = runtime.context_report(child).unwrap();
    let inherited: Vec<_> = report
        .segments
        .iter()
        .filter(|e| matches!(e.provenance, Provenance::Inherited { .. }))
        .collect();
    assert_eq!(
        inherited.len(),
        expected_inherited,
        "one Inherited segment per parent record (up to at_seq) that the builder maps to a segment"
    );
    for entry in &inherited {
        if let Provenance::Inherited { from, seq_range } = &entry.provenance {
            assert_eq!(*from, parent_session);
            assert!(seq_range.start.0 < at_seq.0);
        }
    }

    let inherited_idx = report
        .segments
        .iter()
        .position(|e| matches!(e.provenance, Provenance::Inherited { .. }))
        .expect("at least one inherited segment");
    let last_inherited_idx = report
        .segments
        .iter()
        .rposition(|e| matches!(e.provenance, Provenance::Inherited { .. }))
        .unwrap();
    assert_eq!(
        last_inherited_idx - inherited_idx + 1,
        inherited.len(),
        "inherited segments are contiguous, in order"
    );

    let fork_directive = &report.segments[last_inherited_idx + 1];
    match &fork_directive.provenance {
        Provenance::ForkDirective { by } => assert_eq!(*by, root),
        other => {
            panic!("expected ForkDirective immediately after the inherited run, got {other:?}")
        }
    }
}

// ---------------------------------------------------------------------
// Spawn: no Inherited segment; system prompt from agent_def; InvalidSpec
// reconciliation
// ---------------------------------------------------------------------

#[tokio::test]
async fn spawn_context_has_no_inherited_segment_and_uses_agent_def_system_prompt() {
    let mut defs = HashMap::new();
    defs.insert("reviewer".to_string(), reviewer_def());
    let (runtime, _store) = build_runtime(2, defs);
    let root = start_and_finish_root(&runtime, "investigate the bug").await;

    let mut stream = runtime.subscribe();
    let spec = SubagentSpec::spawn(
        "review this diff",
        AgentDefRef("reviewer".to_string()),
        Budget::default(),
    );
    let child = SubagentHost::start(&*runtime, root, root, spec)
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    let report = runtime.context_report(child).unwrap();
    assert!(
        !report
            .segments
            .iter()
            .any(|e| matches!(e.provenance, Provenance::Inherited { .. })),
        "a spawned child's context must contain no Inherited segment"
    );
    assert!(matches!(
        report.segments.first().map(|e| &e.provenance),
        Some(Provenance::AgentDef { name }) if name == "reviewer"
    ));
}

/// **Relaxed (WI-099 superseded):** a `Spawn` without `agent_def` used to be
/// rejected via `SubagentSpec::validate()` (§5.2's original "agent_def
/// required for spawn" rule). A recorded design decision relaxes that: it is
/// now a valid spawn that inherits the PARENT's role (and, transitively, its
/// model routing) exactly like a roleless fork does -- see
/// `subagent.rs::start`'s role-resolution chain (`spec.role -> agent_def.role
/// (skipped, none) -> parent_meta.role -> the literal "default"`). The root
/// here is started with role "planner" (`root_spec`); the spawned child must
/// end up with that same role, not `None` and not the literal `"default"`.
#[tokio::test]
async fn spawn_without_agent_def_inherits_the_parents_role() {
    let (runtime, _store) = build_runtime(2, HashMap::new());
    let root = start_and_finish_root(&runtime, "hi").await;

    let spec = SubagentSpec {
        mode: SubagentMode::Spawn,
        prompt: "do it".into(),
        agent_def: None,
        role: None,
        tools: None,
        budget: Budget::default(),
        result_contract: None,
        keep_alive: false,
        ephemeral: false,
        ask_origin: None,
        cwd: None,
        root: None,
        tag: None,
    };
    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(&*runtime, root, root, spec)
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    let node = runtime
        .tree()
        .nodes
        .into_iter()
        .find(|n| n.agent_id == child)
        .expect("spawned child must be attached to the tree");
    assert_eq!(
        node.role,
        Some(RoleAlias::new("planner")),
        "a spawn with no agent_def must inherit the parent's role"
    );
    assert_eq!(
        node.agent_def, None,
        "no agent_def was given -- none resolved"
    );
}

// ---------------------------------------------------------------------
// AgentSpawned: inherited_upto Some(at_seq)/None, precedes all other events
// ---------------------------------------------------------------------

#[tokio::test]
async fn agent_spawned_carries_inherited_upto_and_precedes_other_events() {
    let mut defs = HashMap::new();
    defs.insert("reviewer".to_string(), reviewer_def());
    let (runtime, store) = build_runtime(3, defs);
    let root = start_and_finish_root(&runtime, "hi").await;
    let parent_session = session_of(&runtime, root);
    let at_seq = store.head(&parent_session).await.unwrap();

    let mut stream = runtime.subscribe();

    let fork_child = SubagentHost::start(&*runtime, root, root, fork_spec("dig in"))
        .await
        .unwrap();
    let spawn_child = SubagentHost::start(
        &*runtime,
        root,
        root,
        SubagentSpec::spawn(
            "review",
            AgentDefRef("reviewer".to_string()),
            Budget::default(),
        ),
    )
    .await
    .unwrap();

    let mut seen_fork = false;
    let mut seen_spawn = false;
    let mut first_event_for = HashMap::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while (!seen_fork || !seen_spawn) && tokio::time::Instant::now() < deadline {
        let envelope = tokio::time::timeout(Duration::from_millis(500), stream.next())
            .await
            .expect("event stream stalled")
            .expect("event stream ended early");
        first_event_for
            .entry(envelope.agent)
            .or_insert_with(|| envelope.event.clone());

        if envelope.agent == fork_child {
            if let Event::AgentSpawned {
                kind,
                inherited_upto,
                ..
            } = &envelope.event
            {
                assert_eq!(*kind, SubagentMode::Fork);
                assert_eq!(*inherited_upto, Some(at_seq));
                seen_fork = true;
            }
        }
        if envelope.agent == spawn_child {
            if let Event::AgentSpawned {
                kind,
                inherited_upto,
                ..
            } = &envelope.event
            {
                assert_eq!(*kind, SubagentMode::Spawn);
                assert_eq!(*inherited_upto, None);
                seen_spawn = true;
            }
        }
    }
    assert!(seen_fork && seen_spawn, "both AgentSpawned events observed");
    assert!(matches!(
        first_event_for.get(&fork_child),
        Some(Event::AgentSpawned { .. })
    ));
    assert!(matches!(
        first_event_for.get(&spawn_child),
        Some(Event::AgentSpawned { .. })
    ));
}

// ---------------------------------------------------------------------
// Immutability: parent appends after fork never change the child's
// resolved inherited context
// ---------------------------------------------------------------------

#[tokio::test]
async fn parent_appends_after_fork_do_not_change_the_childs_inherited_context() {
    let (runtime, store) = build_runtime(3, HashMap::new());
    let root = start_and_finish_root(&runtime, "hi").await;
    let parent_session = session_of(&runtime, root);

    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(&*runtime, root, root, fork_spec("dig in"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    let before = runtime.context_report(child).unwrap();
    let inherited_before: Vec<_> = before
        .segments
        .iter()
        .filter(|e| matches!(e.provenance, Provenance::Inherited { .. }))
        .map(|e| e.segment)
        .collect();

    // The parent keeps talking after the fork.
    for i in 0..5 {
        store
            .append(
                &parent_session,
                conway_core::log::LogRecord::UserTurn {
                    seq: LogSeq(0),
                    ts: chrono::Utc::now(),
                    text: format!("more from parent {i}"),
                    prov: Provenance::UserPrompt,
                },
            )
            .await
            .unwrap();
    }

    // Prompt the child again and let it run a second turn, so its context
    // is re-assembled.
    let mut stream = runtime.subscribe();
    runtime
        .prompt(child, "one more thing".to_string())
        .await
        .unwrap();
    // Wait for a second AgentFinished isn't guaranteed with this fake setup
    // (the loop keeps running turns as long as prompts arrive and the
    // budget allows); instead give the loop a moment to process the new
    // turn boundary and re-assemble context.
    let _ = tokio::time::timeout(Duration::from_millis(300), async {
        loop {
            let envelope = stream.next().await.unwrap();
            if envelope.agent == child {
                if let Event::TurnStarted { .. } = envelope.event {
                    break;
                }
            }
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let after = runtime.context_report(child).unwrap();
    let inherited_after: Vec<_> = after
        .segments
        .iter()
        .filter(|e| matches!(e.provenance, Provenance::Inherited { .. }))
        .map(|e| e.segment)
        .collect();

    assert_eq!(
        inherited_before, inherited_after,
        "the inherited prefix's segment ids must be identical before and after parent appends"
    );
}

// ---------------------------------------------------------------------
// Sibling sharing: 3 forks at the same (parent, at_seq) share one Arc
// ---------------------------------------------------------------------

#[tokio::test]
async fn siblings_forked_at_the_same_point_share_one_inherited_arc() {
    let (runtime, store) = build_runtime(4, HashMap::new());
    let root = start_and_finish_root(&runtime, "hi").await;
    let parent_session = session_of(&runtime, root);
    let at_seq = store.head(&parent_session).await.unwrap();

    let mut ptrs = Vec::new();
    for i in 0..3 {
        let mut stream = runtime.subscribe();
        let child = SubagentHost::start(&*runtime, root, root, fork_spec(&format!("sibling {i}")))
            .await
            .unwrap();
        wait_for_agent_finished(&mut stream, child).await;

        let records = runtime
            .resolver_for_test()
            .peek_prefix(&parent_session, at_seq)
            .expect("resolving a sibling fork must populate (parent_session, at_seq)");
        ptrs.push(records);
    }

    assert_eq!(ptrs.len(), 3);
    assert!(Arc::ptr_eq(&ptrs[0], &ptrs[1]));
    assert!(Arc::ptr_eq(&ptrs[1], &ptrs[2]));
}

// ---------------------------------------------------------------------
// Grandchild forks (depth >= 2): inherited records are the immediate
// parent's FULL effective transcript (root's prefix ++ the parent's own
// records, verbatim, in order -- the D-11 whole-prefix property), the
// ForkDirective names the immediate parent, `Inherited.from` names the
// immediate parent (not root), and same-point sibling grandchildren still
// share one Arc (WI-084 rework, finding S1).
// ---------------------------------------------------------------------

#[tokio::test]
async fn grandchild_fork_inherits_immediate_parents_full_effective_transcript() {
    // root's turn (1) + A's fork turn (1) + A's reprompt turn (1) +
    // B's fork turn (1) + C's fork turn (1).
    let (runtime, store) = build_runtime(5, HashMap::new());
    let root = start_and_finish_root(&runtime, "investigate").await;
    let root_session = session_of(&runtime, root);
    let root_at_seq = store.head(&root_session).await.unwrap();

    // Fork A from root.
    let mut stream = runtime.subscribe();
    let a = SubagentHost::start(&*runtime, root, root, fork_spec("dig in"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, a).await;
    let a_session = session_of(&runtime, a);

    // Give A a second turn, so its own records span more than just the
    // ForkDirective + first assistant reply -- exercising a genuinely
    // multi-record parent prefix at the grandchild boundary.
    let mut stream = runtime.subscribe();
    runtime.prompt(a, "keep going".to_string()).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_millis(300), async {
        loop {
            let envelope = stream.next().await.unwrap();
            if envelope.agent == a {
                if let Event::TurnStarted { .. } = envelope.event {
                    break;
                }
            }
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let a_at_seq = store.head(&a_session).await.unwrap();

    // Independently compute A's full effective transcript straight from
    // the store (not from the resolver under test): root's own records up
    // to the point A was forked, then A's own records up to its current
    // head, verbatim, in order.
    let mut expected_full_transcript = store
        .read(
            &root_session,
            conway_core::ids::SeqRange::new(LogSeq::ZERO, Some(root_at_seq)),
        )
        .await
        .unwrap();
    expected_full_transcript.extend(
        store
            .read(
                &a_session,
                conway_core::ids::SeqRange::new(LogSeq::ZERO, Some(a_at_seq)),
            )
            .await
            .unwrap(),
    );

    // Fork grandchild B from A.
    let mut stream = runtime.subscribe();
    let b = SubagentHost::start(&*runtime, a, a, fork_spec("grandchild look closer"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, b).await;

    let inherited_for_b = runtime
        .resolver_for_test()
        .peek_prefix(&a_session, a_at_seq)
        .expect("resolving grandchild B must populate (a_session, a_at_seq)");
    assert_eq!(
        inherited_for_b.as_ref(),
        expected_full_transcript.as_slice(),
        "grandchild B's inherited records must equal A's FULL effective \
         transcript, verbatim, in order"
    );

    let b_report = runtime.context_report(b).unwrap();
    let last_inherited_idx = b_report
        .segments
        .iter()
        .rposition(|e| matches!(e.provenance, Provenance::Inherited { .. }))
        .expect("at least one inherited segment");

    // The ForkDirective attached to B names A -- the immediate parent B was
    // forked from -- not root.
    match &b_report.segments[last_inherited_idx + 1].provenance {
        Provenance::ForkDirective { by } => assert_eq!(*by, a),
        other => {
            panic!("expected ForkDirective immediately after the inherited run, got {other:?}")
        }
    }

    // `Inherited.from` on every one of B's inherited segments names A (the
    // immediate parent), never root -- the documented semantic (see
    // `subagent.rs`'s "InheritedPrefix::from at fork depth >= 2" and
    // `context/builder.rs`'s `InheritedPrefix::from` field docs).
    for entry in b_report.segments.iter().take(last_inherited_idx + 1) {
        if let Provenance::Inherited { from, .. } = &entry.provenance {
            assert_eq!(
                *from, a_session,
                "Inherited.from must be the immediate parent (A), not the grandparent (root)"
            );
        }
    }

    // A second grandchild forked at the same (A, a_at_seq) point still
    // shares the identical backing allocation -- sibling sharing holds at
    // fork depth >= 2 too, not just depth 1.
    let mut stream = runtime.subscribe();
    let c = SubagentHost::start(&*runtime, a, a, fork_spec("second grandchild"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, c).await;

    let inherited_for_c = runtime
        .resolver_for_test()
        .peek_prefix(&a_session, a_at_seq)
        .expect("resolving grandchild C must populate (a_session, a_at_seq)");
    assert!(
        Arc::ptr_eq(&inherited_for_b, &inherited_for_c),
        "sibling grandchildren forked at the same point must share one Arc"
    );
}

// ---------------------------------------------------------------------
// Budget: SubagentSpec::budget is a required, non-Option Budget (see
// subagent.rs's module doc reconciliation) -- start succeeds with the
// default budget rather than failing an "absent budget" check that the
// committed type makes impossible to construct.
// ---------------------------------------------------------------------

#[tokio::test]
async fn start_succeeds_with_default_budget_every_child_has_a_budget_by_construction() {
    let (runtime, _store) = build_runtime(2, HashMap::new());
    let root = start_and_finish_root(&runtime, "hi").await;

    let spec = SubagentSpec::fork("go", Budget::default());
    assert_eq!(spec.budget, Budget::default());
    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(&*runtime, root, root, spec)
        .await
        .unwrap();
    let result = wait_for_agent_finished(&mut stream, child).await;
    assert_eq!(result.status, ResultStatus::Completed);
}

// ---------------------------------------------------------------------
// await_result: unknown agent -> AgentNotFound; finished agent -> result
// immediately
// ---------------------------------------------------------------------

#[tokio::test]
async fn await_result_unknown_agent_errors_finished_agent_returns_immediately() {
    let (runtime, _store) = build_runtime(2, HashMap::new());
    let root = start_and_finish_root(&runtime, "hi").await;

    let unknown = AgentId::new();
    let err = SubagentHost::await_result(&*runtime, root, unknown)
        .await
        .unwrap_err();
    assert!(matches!(err, RuntimeError::AgentNotFound { agent } if agent == unknown));

    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(&*runtime, root, root, fork_spec("go"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    let result = SubagentHost::await_result(&*runtime, root, child)
        .await
        .unwrap();
    assert_eq!(result.status, ResultStatus::Completed);
}

// ---------------------------------------------------------------------
// await_result: the real blocking path (cycle-2 review F-085 S2). This is
// the mechanism that supersedes the removed, never-populated
// `mailbox::PendingSubagents` map -- see mailbox.rs's module doc.
// ---------------------------------------------------------------------

struct SlowTool;

fn slow_tool_spec() -> conway_core::content::ToolSpec {
    conway_core::content::ToolSpec {
        name: conway_core::ids::ToolName::new("slow"),
        description: "sleeps before returning".into(),
        schema: serde_json::from_value(serde_json::json!({"type": "object"})).unwrap(),
        category: conway_core::content::ToolCategory::Read,
        permission: conway_core::content::PermissionClass::Safe,
    }
}

#[async_trait]
impl conway_core::ports::Tool for SlowTool {
    fn spec(&self) -> conway_core::content::ToolSpec {
        slow_tool_spec()
    }

    async fn invoke(
        &self,
        _call: conway_core::content::ToolCall,
        _ctx: conway_core::ports::ToolCtx,
    ) -> Result<conway_core::ports::ToolOutput, ToolError> {
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok(conway_core::ports::ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "done".into(),
            }],
            is_error: false,
            truncation: conway_core::content::TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

struct SlowPlugin;

impl conway_core::ports::Plugin for SlowPlugin {
    fn manifest(&self) -> conway_core::ports::PluginManifest {
        conway_core::ports::PluginManifest {
            id: "slow".to_string(),
            version: "0.0.0".to_string(),
            tools: vec![slow_tool_spec().name],
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn conway_core::ports::Tool>> {
        vec![Arc::new(SlowTool)]
    }
}

/// Criterion (cycle-2 review F-085 S2): `SubagentHost::await_result`
/// genuinely BLOCKS until the child actually publishes its terminal
/// result -- not merely "returns immediately for an already-finished
/// child", which `await_result_unknown_agent_errors_finished_agent_returns_immediately`
/// above already covers. Two concurrent awaiters both call `await_result`
/// before the child's 150ms tool call has any chance to complete; neither
/// may resolve early, and once the child does finish, both must observe
/// the identical terminal result -- exactly the "resolves ... exactly
/// once" guarantee the removed `mailbox::PendingSubagents` machinery used
/// to claim, now proven against the real mechanism
/// (`AgentTree::await_result`'s `watch` channel, WI-083).
#[tokio::test]
async fn await_result_blocks_until_the_child_actually_finishes_then_resolves_every_awaiter_once() {
    let fake = Arc::new(FakeStore::new());
    let store: Arc<dyn SessionStore> = Arc::new(CountingStore::new(fake));

    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("ok")), // root's single turn
            ScriptedTurn::Respond(conway_core::ports::GenerateResponse {
                content: vec![],
                tool_calls: vec![conway_core::content::ToolCall {
                    call_id: "c1".into(),
                    name: conway_core::ids::ToolName::new("slow"),
                    arguments: serde_json::json!({}),
                }],
                stop: StopReason::ToolUse,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    ..Default::default()
                },
            }),
            ScriptedTurn::Respond(text_response("child done")), // child's follow-up turn
        ])
        .with_id(BackendId::new("b")),
    );
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);

    let runtime = Runtime::new(RuntimeDeps {
        store,
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![Arc::new(SlowPlugin)],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),
    });

    let root = start_and_finish_root(&runtime, "hi").await;
    let child = SubagentHost::start(&*runtime, root, root, fork_spec("go slow"))
        .await
        .unwrap();

    // Two concurrent awaiters, both issued right after `start` returns --
    // well before the child's 150ms tool call has any chance to finish.
    let r1 = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            SubagentHost::await_result(&*runtime, root, child)
                .await
                .unwrap()
        }
    });
    let r2 = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            SubagentHost::await_result(&*runtime, root, child)
                .await
                .unwrap()
        }
    });

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !r1.is_finished(),
        "await_result resolved before the child's terminal result could possibly exist"
    );
    assert!(
        !r2.is_finished(),
        "await_result resolved before the child's terminal result could possibly exist"
    );

    let (result1, result2) = tokio::time::timeout(Duration::from_secs(2), async {
        (r1.await.unwrap(), r2.await.unwrap())
    })
    .await
    .expect("both awaiters must resolve once the child actually finishes");

    assert_eq!(result1.status, ResultStatus::Completed);
    assert_eq!(
        result1, result2,
        "both concurrent awaiters must observe the identical terminal result"
    );
}

// ---------------------------------------------------------------------
// P-1 (board item 01KYT8TS0EBKJHYNJRF6S88NRH): `steer`/`await_result`/
// `cancel` enforce that `caller` may act only within its OWN subtree
// (itself, or any descendant). Driven against the REAL `Runtime`'s tree --
// real `SubagentHost::start`-produced siblings, not a hand-written
// `FakeSubagentHost` fixture (that fake is an intentional pure recorder/
// no-op, see its own module doc) -- because a fixture that does not itself
// enforce the invariant would prove nothing about whether the real trait
// boundary does. This is the same "seam" concern that let two 0.5.0
// security bugs survive: see `crates/conway/tests/root_containment_seam.rs`
// and `permission_pattern_seam.rs`.
// ---------------------------------------------------------------------

/// Forks two children of a fresh root and waits for both to finish,
/// returning `(root, sibling_a, sibling_b)`. Consumes 3 scripted turns
/// (root, `sibling_a`, `sibling_b`), so callers must build their runtime
/// with `build_runtime(3, ..)` (or more, if they also drive further turns).
async fn build_two_siblings(runtime: &Runtime) -> (AgentId, AgentId, AgentId) {
    let root = start_and_finish_root(runtime, "hi").await;
    let mut stream = runtime.subscribe();
    let sibling_a = SubagentHost::start(runtime, root, root, fork_spec("branch a"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, sibling_a).await;
    let sibling_b = SubagentHost::start(runtime, root, root, fork_spec("branch b"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, sibling_b).await;
    (root, sibling_a, sibling_b)
}

#[tokio::test]
async fn steer_rejects_a_sibling_but_the_owning_root_still_succeeds() {
    let (runtime, _store) = build_runtime(3, HashMap::new());
    let (root, sibling_a, sibling_b) = build_two_siblings(&runtime).await;

    // The vulnerability this item fixes: `sibling_a` has seen `sibling_b`'s
    // id (e.g. from tool output/the event stream/`conway_fork`/`conway_spawn`'s own
    // return value) and tries to inject a steering message into it --
    // context-injection with forged parent authority, per this item's own
    // spec.
    let err = SubagentHost::steer(&*runtime, sibling_a, sibling_b, "forged steer".to_string())
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            RuntimeError::AgentNotInSubtree { caller, target }
                if *caller == sibling_a && *target == sibling_b
        ),
        "expected AgentNotInSubtree{{caller: sibling_a, target: sibling_b}}, got {err:?}"
    );

    // The legitimate operator/root path is unaffected: root is an ancestor
    // of `sibling_b` (its own subtree), so this succeeds.
    SubagentHost::steer(&*runtime, root, sibling_b, "legitimate steer".to_string())
        .await
        .unwrap();
}

#[tokio::test]
async fn await_result_rejects_a_sibling_but_the_owning_root_still_succeeds() {
    let (runtime, _store) = build_runtime(3, HashMap::new());
    let (root, sibling_a, sibling_b) = build_two_siblings(&runtime).await;

    let err = SubagentHost::await_result(&*runtime, sibling_a, sibling_b)
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            RuntimeError::AgentNotInSubtree { caller, target }
                if *caller == sibling_a && *target == sibling_b
        ),
        "expected AgentNotInSubtree{{caller: sibling_a, target: sibling_b}}, got {err:?}"
    );

    let result = SubagentHost::await_result(&*runtime, root, sibling_b)
        .await
        .unwrap();
    assert_eq!(result.status, ResultStatus::Completed);
}

#[tokio::test]
async fn cancel_rejects_a_sibling_but_the_owning_root_still_succeeds() {
    let (runtime, _store) = build_runtime(3, HashMap::new());
    let (root, sibling_a, sibling_b) = build_two_siblings(&runtime).await;

    let err = SubagentHost::cancel(
        &*runtime,
        sibling_a,
        sibling_b,
        "destroy their work".to_string(),
        CancelMode::Immediate,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            &err,
            RuntimeError::AgentNotInSubtree { caller, target }
                if *caller == sibling_a && *target == sibling_b
        ),
        "expected AgentNotInSubtree{{caller: sibling_a, target: sibling_b}}, got {err:?}"
    );

    // The legitimate operator/root path is unaffected -- `sibling_b` has
    // already finished, so this is a benign no-op cancel, but the point
    // here is specifically that the subtree check does not also reject the
    // legitimate caller.
    SubagentHost::cancel(
        &*runtime,
        root,
        sibling_b,
        "cleanup".to_string(),
        CancelMode::Immediate,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn steer_allows_any_ancestor_not_only_a_direct_parent() {
    // root -> a -> grandchild: root (a GRANDparent, not `grandchild`'s
    // direct parent) must still be able to steer `grandchild` directly --
    // the check walks the whole ancestor chain, not just one hop.
    let (runtime, _store) = build_runtime(3, HashMap::new());
    let root = start_and_finish_root(&runtime, "hi").await;
    let mut stream = runtime.subscribe();
    let a = SubagentHost::start(&*runtime, root, root, fork_spec("a"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, a).await;
    let grandchild = SubagentHost::start(&*runtime, a, a, fork_spec("gc"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, grandchild).await;

    SubagentHost::steer(
        &*runtime,
        root,
        grandchild,
        "hello from the top".to_string(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn steer_from_a_sibling_of_an_ancestor_is_still_rejected() {
    // root -> a -> grandchild, and root -> b (a sibling of `a`, not of
    // `grandchild`). `b` is a legitimate member of root's own subtree, but
    // `grandchild` is not in ITS subtree -- being "somewhere in the same
    // tree" must not be conflated with "in the caller's own subtree".
    let (runtime, _store) = build_runtime(4, HashMap::new());
    let root = start_and_finish_root(&runtime, "hi").await;
    let mut stream = runtime.subscribe();
    let a = SubagentHost::start(&*runtime, root, root, fork_spec("a"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, a).await;
    let grandchild = SubagentHost::start(&*runtime, a, a, fork_spec("gc"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, grandchild).await;
    let b = SubagentHost::start(&*runtime, root, root, fork_spec("b"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, b).await;

    let err = SubagentHost::steer(&*runtime, b, grandchild, "forged".to_string())
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            RuntimeError::AgentNotInSubtree { caller, target }
                if *caller == b && *target == grandchild
        ),
        "expected AgentNotInSubtree{{caller: b, target: grandchild}}, got {err:?}"
    );
}

/// Criterion: `steer`'s attribution (`LogRecord::ParentSteer::from`) derives
/// from the CALLER, never from `target`'s own tree parent -- the pre-fix
/// behavior this item replaces, which is exactly what let a forged steer
/// look authentic to its recipient (a steer carries parent authority by
/// convention). Proven with a caller that is deliberately NOT `target`'s
/// direct parent (root steers a grandchild whose direct parent is `a`), so
/// a test that accidentally left the old "derive from target's own parent"
/// logic in place would fail here (it would record `from: a`, not `from:
/// root`).
///
/// The grandchild is built `keep_alive: true` with an empty prompt, so it
/// starts IDLE rather than running its own turn (see `SubagentSpec::
/// keep_alive`'s own doc) -- unlike a plain one-shot fork/spawn child,
/// whose task exits for good the instant its own single turn naturally
/// completes (`AgentSpec::keep_alive: false`'s documented behavior,
/// `agent_loop.rs`), an idling `keep_alive` agent's task stays alive to
/// actually drain a queued steer once `Runtime::prompt` wakes it -- the
/// only way this test can observe a persisted `LogRecord::ParentSteer` at
/// all.
#[tokio::test]
async fn steer_attribution_derives_from_the_caller_not_the_targets_own_parent() {
    let (runtime, store) = build_runtime(3, HashMap::new());
    let root = start_and_finish_root(&runtime, "hi").await;
    let mut stream = runtime.subscribe();
    let a = SubagentHost::start(&*runtime, root, root, fork_spec("a"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, a).await;

    let mut keep_alive_spec = SubagentSpec::fork("", Budget::default());
    keep_alive_spec.keep_alive = true;
    let grandchild = SubagentHost::start(&*runtime, a, a, keep_alive_spec)
        .await
        .unwrap();

    // root -- the grandchild's GRANDparent, not its direct parent `a` --
    // steers the grandchild directly, while it is still idling (queued,
    // not yet drained).
    SubagentHost::steer(&*runtime, root, grandchild, "from the top".to_string())
        .await
        .unwrap();

    // Wakes the idling grandchild so it drains its inbox at the top of its
    // first real turn, persisting the queued steer as `LogRecord::
    // ParentSteer` before assembling context for the new prompt.
    runtime
        .prompt(grandchild, "continue".to_string())
        .await
        .unwrap();

    let grandchild_session = session_of(&runtime, grandchild);
    let from = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let records = store
                .read(&grandchild_session, conway_core::ids::SeqRange::full())
                .await
                .unwrap();
            if let Some(from) = records.iter().find_map(|r| match r {
                conway_core::log::LogRecord::ParentSteer { from, .. } => Some(*from),
                _ => None,
            }) {
                return from;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("ParentSteer record never appeared");

    assert_eq!(
        from, root,
        "steer attribution must derive from the CALLER (root), never target's own tree \
         parent (`a`) -- deriving it from the target is what let a forged steer look \
         authentic"
    );
}

// ---------------------------------------------------------------------
// Board item 01KYTP0PGKJ4VCJP5TD39A1WHF: `674bb65` (immediately above) left
// `start`/`ask`/`tree` unguarded -- `start`/`ask` took only `parent` and
// acted on it directly, and `tree` took no caller at all and returned the
// WHOLE runtime-wide tree. Composed, this was cross-tree exfiltration in one
// call: `tree()` to find a sibling's `AgentId`, then `ask(sibling, ..)` to
// fork the sibling's ENTIRE context and read the reply back as plain model
// output. These tests drive the SAME REAL `Runtime` the trio above does --
// see that section's own doc for why a `FakeSubagentHost` fixture proves
// nothing about the real trait boundary.
// ---------------------------------------------------------------------

#[tokio::test]
async fn start_rejects_a_sibling_as_parent_but_the_owning_root_still_succeeds() {
    let (runtime, _store) = build_runtime(4, HashMap::new());
    let (root, sibling_a, sibling_b) = build_two_siblings(&runtime).await;

    // `sibling_a` has seen `sibling_b`'s id and tries to attach a NEW child
    // under it directly -- `caller` (`sibling_a`, correctly its own true
    // identity) does not own `parent` (`sibling_b`), so this must be
    // rejected before any store I/O or child attach happens.
    let err = SubagentHost::start(&*runtime, sibling_a, sibling_b, fork_spec("attach under b"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            RuntimeError::AgentNotInSubtree { caller, target }
                if *caller == sibling_a && *target == sibling_b
        ),
        "expected AgentNotInSubtree{{caller: sibling_a, target: sibling_b}}, got {err:?}"
    );
    // No child was attached under `sibling_b` for the rejected attempt.
    assert!(
        runtime
            .tree()
            .nodes
            .iter()
            .all(|n| n.parent != Some(sibling_b)),
        "no child should have been started under sibling_b for a rejected start"
    );

    // The legitimate operator/root path is unaffected: root owns
    // `sibling_b`'s subtree, so starting a new child under it succeeds.
    SubagentHost::start(&*runtime, root, sibling_b, fork_spec("legitimate attach"))
        .await
        .unwrap();
}

#[tokio::test]
async fn ask_rejects_a_sibling_as_parent_and_the_victims_context_never_comes_back() {
    let (runtime, _store) = build_runtime(4, HashMap::new());
    let (_root, sibling_a, sibling_b) = build_two_siblings(&runtime).await;

    // The exfiltration attack this item closes, verbatim: `sibling_a` (an
    // ordinary, non-privileged agent) has seen `sibling_b`'s id and calls
    // `ask` with itself as the correct, non-forgeable `caller` but
    // `sibling_b` as `parent` -- attempting to fork `sibling_b`'s ENTIRE
    // context (GP-02: a fork inherits everything up to the fork point) and
    // read the reply back as plain model output.
    let err = SubagentHost::ask(
        &*runtime,
        sibling_a,
        sibling_b,
        SubagentSpec::fork("summarize everything above", Budget::default()),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            &err,
            RuntimeError::AgentNotInSubtree { caller, target }
                if *caller == sibling_a && *target == sibling_b
        ),
        "expected AgentNotInSubtree{{caller: sibling_a, target: sibling_b}}, got {err:?}"
    );
    // The victim's context never comes back: no child was ever attached
    // under `sibling_b`, so there is no forked session for its context to
    // have leaked into in the first place.
    assert!(
        runtime
            .tree()
            .nodes
            .iter()
            .all(|n| n.parent != Some(sibling_b)),
        "no ephemeral ask child should have been started under sibling_b for a rejected ask"
    );
}

#[tokio::test]
async fn ask_still_works_when_caller_and_parent_are_the_same_agent() {
    // The ordinary, model-invoked shape (`conway_ask` always passes
    // `ctx.agent_id` as both `caller` and `parent`): an agent asking a fork
    // of ITSELF always succeeds, since an agent's own subtree always
    // contains itself.
    let (runtime, _store) = build_runtime(2, HashMap::new());
    let root = start_and_finish_root(&runtime, "hi").await;

    let outcome = SubagentHost::ask(
        &*runtime,
        root,
        root,
        SubagentSpec::fork("say hi", Budget::default()),
    )
    .await
    .unwrap();
    assert_eq!(outcome.status, ResultStatus::Completed);
}

#[tokio::test]
async fn tree_scopes_to_the_callers_own_subtree_not_the_whole_runtime() {
    // root -> a -> grandchild, and root -> b (a's sibling). `a` calling
    // `tree()` must see ONLY itself and its own descendant (`grandchild`) --
    // never `root` (its own ancestor) or `b` (an unrelated branch) --
    // otherwise `tree()` remains the reconnaissance half of the
    // exfiltration attack this item closes, even after `ask`/`start` are
    // fixed.
    let (runtime, _store) = build_runtime(4, HashMap::new());
    let root = start_and_finish_root(&runtime, "hi").await;
    let mut stream = runtime.subscribe();
    let a = SubagentHost::start(&*runtime, root, root, fork_spec("a"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, a).await;
    let grandchild = SubagentHost::start(&*runtime, a, a, fork_spec("gc"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, grandchild).await;
    let b = SubagentHost::start(&*runtime, root, root, fork_spec("b"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, b).await;

    let a_view = SubagentHost::tree(&*runtime, a);
    let a_view_ids: std::collections::HashSet<AgentId> =
        a_view.nodes.iter().map(|n| n.agent_id).collect();
    assert_eq!(a_view.root, a, "the scoped snapshot's root is the caller");
    assert_eq!(
        a_view_ids,
        std::collections::HashSet::from([a, grandchild]),
        "`a`'s own subtree is exactly itself plus its descendant `grandchild` -- never \
         root (an ancestor) or b (an unrelated sibling branch)"
    );

    // The root's own subtree IS the whole tree, correctly -- no scoping
    // regression for the legitimate operator/root path.
    let root_view = SubagentHost::tree(&*runtime, root);
    let root_view_ids: std::collections::HashSet<AgentId> =
        root_view.nodes.iter().map(|n| n.agent_id).collect();
    assert_eq!(root_view.root, root);
    assert_eq!(
        root_view_ids,
        std::collections::HashSet::from([root, a, grandchild, b]),
        "the root's own subtree is the whole tree"
    );

    // An unrelated third party (a fresh, never-attached id) sees an empty
    // subtree, not an error and not a panic (P-10) -- mirrors
    // `AgentTree::path`'s own "empty for unknown" convention.
    let unknown = AgentId::new();
    let unknown_view = SubagentHost::tree(&*runtime, unknown);
    assert_eq!(unknown_view.root, unknown);
    assert!(unknown_view.nodes.is_empty());
}

// ---------------------------------------------------------------------
// ToolCtx::subagents is BACKED BY this Runtime (via `SubagentHandle`, board
// item C1): a tool that forks through it produces a child visible in this
// same runtime's tree.
// ---------------------------------------------------------------------

struct ForkingTool;

fn forking_tool_spec() -> conway_core::content::ToolSpec {
    conway_core::content::ToolSpec {
        name: conway_core::ids::ToolName::new("test_fork"),
        description: "forks via ToolCtx::subagents".into(),
        schema: serde_json::from_value(serde_json::json!({"type": "object"})).unwrap(),
        category: conway_core::content::ToolCategory::Read,
        permission: conway_core::content::PermissionClass::Safe,
    }
}

#[async_trait]
impl conway_core::ports::Tool for ForkingTool {
    fn spec(&self) -> conway_core::content::ToolSpec {
        forking_tool_spec()
    }

    async fn invoke(
        &self,
        _call: conway_core::content::ToolCall,
        ctx: conway_core::ports::ToolCtx,
    ) -> Result<conway_core::ports::ToolOutput, ToolError> {
        // Board item C1: `ctx.subagents` is now a `SubagentHandle` with
        // this agent's own id already baked in -- no `caller`/`parent`
        // arguments to pass here anymore.
        let child = ctx
            .subagents
            .start(SubagentSpec::fork("nested", Budget::default()))
            .await
            .map_err(|e| ToolError::Internal {
                detail: e.to_string(),
            })?;
        Ok(conway_core::ports::ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: child.to_string(),
            }],
            is_error: false,
            truncation: conway_core::content::TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

struct ForkingPlugin;

impl conway_core::ports::Plugin for ForkingPlugin {
    fn manifest(&self) -> conway_core::ports::PluginManifest {
        conway_core::ports::PluginManifest {
            id: "test".to_string(),
            version: "0.0.0".to_string(),
            tools: vec![forking_tool_spec().name],
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn conway_core::ports::Tool>> {
        vec![Arc::new(ForkingTool)]
    }
}

#[tokio::test]
async fn tool_ctx_subagents_is_the_runtime_itself() {
    let fake = Arc::new(FakeStore::new());
    let store: Arc<dyn SessionStore> = Arc::new(CountingStore::new(fake));

    // Parent's turn calls the tool; the tool's own fork, and the parent's
    // own follow-up text turn, both need a scripted response.
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(conway_core::ports::GenerateResponse {
                content: vec![],
                tool_calls: vec![conway_core::content::ToolCall {
                    call_id: "c1".into(),
                    name: conway_core::ids::ToolName::new("test_fork"),
                    arguments: serde_json::json!({}),
                }],
                stop: StopReason::ToolUse,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    ..Default::default()
                },
            }),
            ScriptedTurn::Respond(text_response("done")),
            ScriptedTurn::Respond(text_response("nested done")),
        ])
        .with_id(BackendId::new("b")),
    );
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);

    let runtime = Runtime::new(RuntimeDeps {
        store,
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![Arc::new(ForkingPlugin)],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),
    });

    let mut stream = runtime.subscribe();
    let root = runtime.start_root(root_spec("use the tool")).await.unwrap();
    let result = wait_for_agent_finished(&mut stream, root).await;
    assert_eq!(result.status, ResultStatus::Completed);

    // The tool ran inside the SAME runtime's task and forked a REAL child
    // through `ctx.subagents` -- that child must show up in this runtime's
    // own tree, proving `ToolCtx::subagents` was backed by this runtime.
    let snapshot = runtime.tree();
    assert_eq!(
        snapshot
            .nodes
            .iter()
            .filter(|n| n.parent == Some(root))
            .count(),
        1,
        "the tool's fork call must have attached exactly one child of root to this runtime's tree"
    );
}

// ---------------------------------------------------------------------
// C1: `SubagentSpec::cwd` scopes a child to its own working directory,
// independent of the parent's -- see `conway_core::agent::SubagentSpec::cwd`
// for the full absolute/relative/nonexistent-path semantics these tests
// prove against the real runtime.
// ---------------------------------------------------------------------

/// Records `ToolCtx::cwd` for whichever agent invokes it -- short of a
/// dedicated runtime inspection API (none exists), this is the only way to
/// observe an `AgentLoop`'s actually-resolved cwd; mirrors this file's own
/// `ForkingTool`/`SlowTool` probe-plugin pattern.
#[derive(Default)]
struct CwdProbe {
    observed: std::sync::Mutex<HashMap<AgentId, PathBuf>>,
}

struct CwdProbeTool {
    probe: Arc<CwdProbe>,
}

fn cwd_probe_tool_spec() -> conway_core::content::ToolSpec {
    conway_core::content::ToolSpec {
        name: conway_core::ids::ToolName::new("cwd_probe"),
        description: "records ctx.cwd for the calling agent".into(),
        schema: serde_json::from_value(serde_json::json!({"type": "object"})).unwrap(),
        category: conway_core::content::ToolCategory::Read,
        permission: conway_core::content::PermissionClass::Safe,
    }
}

#[async_trait]
impl conway_core::ports::Tool for CwdProbeTool {
    fn spec(&self) -> conway_core::content::ToolSpec {
        cwd_probe_tool_spec()
    }

    async fn invoke(
        &self,
        _call: conway_core::content::ToolCall,
        ctx: conway_core::ports::ToolCtx,
    ) -> Result<conway_core::ports::ToolOutput, ToolError> {
        self.probe
            .observed
            .lock()
            .unwrap()
            .insert(ctx.agent_id, ctx.cwd.clone());
        Ok(conway_core::ports::ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "recorded".into(),
            }],
            is_error: false,
            truncation: conway_core::content::TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

struct CwdProbePlugin {
    tool: Arc<CwdProbeTool>,
}

impl conway_core::ports::Plugin for CwdProbePlugin {
    fn manifest(&self) -> conway_core::ports::PluginManifest {
        conway_core::ports::PluginManifest {
            id: "cwd_probe".to_string(),
            version: "0.0.0".to_string(),
            tools: vec![cwd_probe_tool_spec().name],
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn conway_core::ports::Tool>> {
        vec![self.tool.clone()]
    }
}

fn tool_call_response(call_id: &str) -> conway_core::ports::GenerateResponse {
    conway_core::ports::GenerateResponse {
        content: vec![],
        tool_calls: vec![conway_core::content::ToolCall {
            call_id: call_id.into(),
            name: conway_core::ids::ToolName::new("cwd_probe"),
            arguments: serde_json::json!({}),
        }],
        stop: StopReason::ToolUse,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
    }
}

/// Builds a runtime wired with the `cwd_probe` tool and a real (non-`Fake`)
/// backing store so `SessionMeta.cwd` can be read back directly -- the
/// script is supplied by the caller, who interleaves `tool_call_response`/
/// `text_response` turns to script exactly the tool-calling sequence their
/// test needs.
fn build_probe_runtime(
    script: Vec<ScriptedTurn>,
) -> (Arc<Runtime>, Arc<dyn SessionStore>, Arc<CwdProbe>) {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let probe = Arc::new(CwdProbe::default());

    let backend = Arc::new(ScriptedBackend::new(script).with_id(BackendId::new("b")));
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);

    let runtime = Runtime::new(RuntimeDeps {
        store: store.clone(),
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![Arc::new(CwdProbePlugin {
            tool: Arc::new(CwdProbeTool {
                probe: probe.clone(),
            }),
        })],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),
    });
    (runtime, store, probe)
}

/// A second, independent `Runtime` over an ALREADY-populated store -- the
/// same "fresh process, same store" shape `resume_root.rs`'s own harness
/// uses (`build_runtime_over`), needed because `resume_root` re-attaches a
/// session's ORIGINAL `agent_id` into the tree: calling it against the very
/// `Runtime` that already has that agent live (even finished) would collide
/// with "already attached".
fn build_runtime_over(store: Arc<dyn SessionStore>, script: Vec<ScriptedTurn>) -> Arc<Runtime> {
    let backend = Arc::new(ScriptedBackend::new(script).with_id(BackendId::new("b2")));
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);

    Runtime::new(RuntimeDeps {
        store,
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),
    })
}

fn root_spec_with_cwd(prompt: &str, cwd: PathBuf) -> RootSpec {
    RootSpec {
        cwd,
        ..root_spec(prompt)
    }
}

fn spawn_spec_with_cwd(prompt: &str, cwd: Option<PathBuf>) -> SubagentSpec {
    SubagentSpec {
        mode: SubagentMode::Spawn,
        prompt: prompt.to_string(),
        agent_def: None,
        role: None,
        tools: None,
        budget: Budget::default(),
        result_contract: None,
        keep_alive: false,
        ephemeral: false,
        ask_origin: None,
        cwd,
        root: None,
        tag: None,
    }
}

/// (c) Spawn with `cwd: Some(tmp/sub)`: the child's `SessionMeta.cwd` AND
/// its `AgentLoop`'s actually-resolved `ToolCtx.cwd` (probed live via
/// `cwd_probe`) both equal it -- `subagent.rs`'s `child_cwd` is computed
/// once and used at both application sites, and this is the test that would
/// catch the two ever diverging. A sibling spawned with `cwd: None`
/// instead gets the PARENT's cwd, unchanged from before this item.
#[tokio::test]
async fn spawn_cwd_scopes_child_session_meta_and_tool_ctx_sibling_inherits_parent() {
    let root_dir = tempfile::tempdir().unwrap();
    let sub_dir = root_dir.path().join("sub");
    std::fs::create_dir(&sub_dir).unwrap();

    let (runtime, store, probe) = build_probe_runtime(vec![
        ScriptedTurn::Respond(text_response("root turn")), // root's own turn
        ScriptedTurn::Respond(tool_call_response("a1")),   // child A: calls cwd_probe
        ScriptedTurn::Respond(text_response("a done")),    // child A: follow-up finish
        ScriptedTurn::Respond(tool_call_response("b1")),   // child B: calls cwd_probe
        ScriptedTurn::Respond(text_response("b done")),    // child B: follow-up finish
    ]);

    let mut stream = runtime.subscribe();
    let root = runtime
        .start_root(root_spec_with_cwd("hi", root_dir.path().to_path_buf()))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    // Child A: scoped to sub_dir.
    let mut stream = runtime.subscribe();
    let child_a = SubagentHost::start(
        &*runtime,
        root,
        root,
        spawn_spec_with_cwd("scoped child", Some(sub_dir.clone())),
    )
    .await
    .unwrap();
    wait_for_agent_finished(&mut stream, child_a).await;

    let a_session = session_of(&runtime, child_a);
    let a_meta = store.meta(&a_session).await.unwrap();
    assert_eq!(
        a_meta.cwd, sub_dir,
        "child A's SessionMeta.cwd must equal the explicit spec.cwd override"
    );
    assert_eq!(
        probe.observed.lock().unwrap().get(&child_a),
        Some(&sub_dir),
        "child A's ToolCtx.cwd (as seen by its own tool call) must equal the same override"
    );

    // Child B: a sibling of A, spawned with `cwd: None`.
    let mut stream = runtime.subscribe();
    let child_b = SubagentHost::start(
        &*runtime,
        root,
        root,
        spawn_spec_with_cwd("unscoped sibling", None),
    )
    .await
    .unwrap();
    wait_for_agent_finished(&mut stream, child_b).await;

    let b_session = session_of(&runtime, child_b);
    let b_meta = store.meta(&b_session).await.unwrap();
    assert_eq!(
        b_meta.cwd,
        root_dir.path(),
        "a sibling spawned with cwd: None must inherit the PARENT's (root's) cwd, not A's"
    );
    assert_eq!(
        probe.observed.lock().unwrap().get(&child_b),
        Some(&root_dir.path().to_path_buf()),
        "child B's ToolCtx.cwd must equal the parent's cwd too"
    );
}

/// (d) A grandchild spawned with `cwd: None` inherits its IMMEDIATE parent's
/// (possibly-overridden) cwd, not the root's -- the same "immediate parent,
/// not root" rule `grandchild_fork_inherits_immediate_parents_full_effective_transcript`
/// already proves for inherited transcript content, now proven for `cwd`.
#[tokio::test]
async fn grandchild_with_cwd_none_inherits_immediate_parents_cwd_not_roots() {
    let root_dir = tempfile::tempdir().unwrap();
    let child_dir = root_dir.path().join("child_scope");
    std::fs::create_dir(&child_dir).unwrap();

    let (runtime, store, _probe) = build_probe_runtime(vec![
        ScriptedTurn::Respond(text_response("root turn")),
        ScriptedTurn::Respond(text_response("child turn")),
        ScriptedTurn::Respond(text_response("grandchild turn")),
    ]);
    let mut stream = runtime.subscribe();
    let root = runtime
        .start_root(root_spec_with_cwd("hi", root_dir.path().to_path_buf()))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(
        &*runtime,
        root,
        root,
        spawn_spec_with_cwd("scoped child", Some(child_dir.clone())),
    )
    .await
    .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    let mut stream = runtime.subscribe();
    let grandchild = SubagentHost::start(
        &*runtime,
        child,
        child,
        spawn_spec_with_cwd("grandchild, no override", None),
    )
    .await
    .unwrap();
    wait_for_agent_finished(&mut stream, grandchild).await;

    let grandchild_session = session_of(&runtime, grandchild);
    let grandchild_meta = store.meta(&grandchild_session).await.unwrap();
    assert_eq!(
        grandchild_meta.cwd, child_dir,
        "a grandchild with cwd: None must inherit the CHILD's (immediate parent's) cwd"
    );
    assert_ne!(
        grandchild_meta.cwd,
        root_dir.path(),
        "the grandchild's cwd must not be the root's cwd"
    );
}

/// (e) A nonexistent `cwd` fails the spawn fast, with a clear error --
/// mapped through this crate's established `invalid_spec` helper (see
/// `subagent.rs`'s own doc) to `RuntimeError::InvalidSpec`, the same surface
/// an invalid `SubagentSpec` already uses.
#[tokio::test]
async fn spawn_with_nonexistent_cwd_fails_fast_with_a_clear_error() {
    let (runtime, _store) = build_runtime(1, HashMap::new());
    let root = start_and_finish_root(&runtime, "hi").await;

    let missing = tempfile::tempdir().unwrap();
    let missing_path = missing.path().join("does-not-exist");
    // `missing` itself is dropped (and its directory removed) here, but
    // `missing_path` was never created inside it either way -- it never
    // existed.
    drop(missing);

    let err = SubagentHost::start(
        &*runtime,
        root,
        root,
        spawn_spec_with_cwd("doomed", Some(missing_path.clone())),
    )
    .await
    .unwrap_err();

    match &err {
        RuntimeError::InvalidSpec { detail } => {
            assert!(
                detail.contains(&missing_path.display().to_string()),
                "error must name the offending path; got {detail:?}"
            );
        }
        other => panic!("expected RuntimeError::InvalidSpec {{ .. }}, got {other:?}"),
    }
}

/// Not one of C1's five named acceptance tests, but the other half of its
/// settled semantics: "relative: resolved against the PARENT's cwd at spawn
/// time" (`conway_core::agent::SubagentSpec::cwd`'s own doc) is exercised
/// directly here rather than only by construction (every path in the tests
/// above happens to already be absolute).
#[tokio::test]
async fn spawn_with_relative_cwd_resolves_against_the_parents_cwd() {
    let root_dir = tempfile::tempdir().unwrap();
    let sub_dir = root_dir.path().join("relative_sub");
    std::fs::create_dir(&sub_dir).unwrap();

    let (runtime, store, _probe) = build_probe_runtime(vec![
        ScriptedTurn::Respond(text_response("root turn")),
        ScriptedTurn::Respond(text_response("child turn")),
    ]);
    let mut stream = runtime.subscribe();
    let root = runtime
        .start_root(root_spec_with_cwd("hi", root_dir.path().to_path_buf()))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(
        &*runtime,
        root,
        root,
        spawn_spec_with_cwd("relatively scoped", Some(PathBuf::from("relative_sub"))),
    )
    .await
    .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    let child_session = session_of(&runtime, child);
    let child_meta = store.meta(&child_session).await.unwrap();
    assert_eq!(
        child_meta.cwd, sub_dir,
        "a relative cwd override must resolve against the PARENT's cwd, not be stored bare"
    );
}

// ---------------------------------------------------------------------
// S3: root plumbing -- the inheritance algebra (SubagentSpec.root,
// SessionMeta.root, cwd subset-of root, spawn-only narrowing).
// ---------------------------------------------------------------------

fn spawn_spec_with_cwd_and_root(
    prompt: &str,
    cwd: Option<PathBuf>,
    root: Option<PathBuf>,
) -> SubagentSpec {
    SubagentSpec {
        cwd,
        root,
        ..spawn_spec_with_cwd(prompt, None)
    }
}

/// (a) Spawn with a `root` narrower than the parent's own root: accepted,
/// and the child's `SessionMeta.root` equals the (canonicalized) narrower
/// value.
#[tokio::test]
async fn spawn_with_narrower_root_is_accepted_and_child_inherits_it() {
    let root_dir = tempfile::tempdir().unwrap();
    let sub_dir = root_dir.path().join("sub");
    std::fs::create_dir(&sub_dir).unwrap();

    let (runtime, store, _probe) = build_probe_runtime(vec![
        ScriptedTurn::Respond(text_response("root turn")),
        ScriptedTurn::Respond(text_response("child turn")),
    ]);
    let mut stream = runtime.subscribe();
    let root = runtime
        .start_root(root_spec_with_cwd("hi", root_dir.path().to_path_buf()))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(
        &*runtime,
        root,
        root,
        spawn_spec_with_cwd_and_root("narrower", Some(sub_dir.clone()), Some(sub_dir.clone())),
    )
    .await
    .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    let child_session = session_of(&runtime, child);
    let child_meta = store.meta(&child_session).await.unwrap();
    assert_eq!(
        child_meta.root,
        Some(sub_dir.canonicalize().unwrap()),
        "a narrower requested root (the root agent itself is unconfined) must be accepted"
    );
}

/// (b) Spawn with a `root` WIDER than the parent's own (already-confined)
/// root: the spawn FAILS with a typed error naming both roots -- never
/// silently clamped to the parent's root.
#[tokio::test]
async fn spawn_with_wider_root_than_the_parents_fails_naming_both_roots() {
    let root_dir = tempfile::tempdir().unwrap();
    let sub_dir = root_dir.path().join("sub");
    std::fs::create_dir(&sub_dir).unwrap();

    let (runtime, _store, _probe) = build_probe_runtime(vec![
        ScriptedTurn::Respond(text_response("root turn")),
        ScriptedTurn::Respond(text_response("confined child turn")),
    ]);
    let mut stream = runtime.subscribe();
    let root = runtime
        .start_root(root_spec_with_cwd("hi", root_dir.path().to_path_buf()))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    // Confine a first child to `sub_dir`.
    let mut stream = runtime.subscribe();
    let confined = SubagentHost::start(
        &*runtime,
        root,
        root,
        spawn_spec_with_cwd_and_root("confined", Some(sub_dir.clone()), Some(sub_dir.clone())),
    )
    .await
    .unwrap();
    wait_for_agent_finished(&mut stream, confined).await;

    // A grandchild requesting the WIDER `root_dir` (the confined child's
    // own parent's root, but wider than the confined child's own) must
    // fail -- never silently clamped back to `sub_dir`.
    let err = SubagentHost::start(
        &*runtime,
        confined,
        confined,
        spawn_spec_with_cwd_and_root(
            "wider",
            Some(root_dir.path().to_path_buf()),
            Some(root_dir.path().to_path_buf()),
        ),
    )
    .await
    .unwrap_err();

    match &err {
        RuntimeError::InvalidSpec { detail } => {
            assert!(
                detail.contains(
                    &root_dir
                        .path()
                        .canonicalize()
                        .unwrap()
                        .display()
                        .to_string()
                ),
                "error must name the requested (wider) root; got {detail:?}"
            );
            assert!(
                detail.contains(&sub_dir.canonicalize().unwrap().display().to_string()),
                "error must name the parent's (narrower) root; got {detail:?}"
            );
        }
        other => panic!("expected RuntimeError::InvalidSpec {{ .. }}, got {other:?}"),
    }
}

/// (c) Spawn with a `root` disjoint (sideways) from the parent's own
/// (already-confined) root: the spawn FAILS the same way a wider root does.
#[tokio::test]
async fn spawn_with_sideways_root_fails() {
    let root_dir = tempfile::tempdir().unwrap();
    let sub_a = root_dir.path().join("sub_a");
    let sub_b = root_dir.path().join("sub_b");
    std::fs::create_dir(&sub_a).unwrap();
    std::fs::create_dir(&sub_b).unwrap();

    let (runtime, _store, _probe) = build_probe_runtime(vec![
        ScriptedTurn::Respond(text_response("root turn")),
        ScriptedTurn::Respond(text_response("confined child turn")),
    ]);
    let mut stream = runtime.subscribe();
    let root = runtime
        .start_root(root_spec_with_cwd("hi", root_dir.path().to_path_buf()))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    let mut stream = runtime.subscribe();
    let confined = SubagentHost::start(
        &*runtime,
        root,
        root,
        spawn_spec_with_cwd_and_root("confined", Some(sub_a.clone()), Some(sub_a.clone())),
    )
    .await
    .unwrap();
    wait_for_agent_finished(&mut stream, confined).await;

    let err = SubagentHost::start(
        &*runtime,
        confined,
        confined,
        spawn_spec_with_cwd_and_root("sideways", Some(sub_b.clone()), Some(sub_b.clone())),
    )
    .await
    .unwrap_err();

    match &err {
        RuntimeError::InvalidSpec { .. } => {}
        other => panic!("expected RuntimeError::InvalidSpec {{ .. }}, got {other:?}"),
    }
}

/// (d) A spawn whose (inherited or overridden) `cwd` would fall OUTSIDE a
/// newly-narrowed `root` fails the spawn -- "cwd subset of root, always".
#[tokio::test]
async fn spawn_whose_cwd_escapes_a_newly_narrowed_root_fails() {
    let root_dir = tempfile::tempdir().unwrap();
    let sub_dir = root_dir.path().join("sub");
    std::fs::create_dir(&sub_dir).unwrap();

    let (runtime, _store, _probe) =
        build_probe_runtime(vec![ScriptedTurn::Respond(text_response("root turn"))]);
    let mut stream = runtime.subscribe();
    let root = runtime
        .start_root(root_spec_with_cwd("hi", root_dir.path().to_path_buf()))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    // `cwd` is left at `None` (inherits the parent's `root_dir`), but
    // `root` narrows to `sub_dir` -- the inherited cwd now falls outside
    // the child's own confinement.
    let err = SubagentHost::start(
        &*runtime,
        root,
        root,
        spawn_spec_with_cwd_and_root("escaping cwd", None, Some(sub_dir.clone())),
    )
    .await
    .unwrap_err();

    match &err {
        RuntimeError::InvalidSpec { detail } => {
            // The error names the escaping cwd. It is reported raw, and
            // ALSO in canonical form as `(resolved: ...)` when the two
            // differ, so both operands of the containment comparison are
            // displayed on the same footing -- a symlinked cwd shown raw
            // beside an always-canonical root reads as a mismatch between
            // unrelated paths. Asserting on the raw form alone keeps this
            // test agnostic to whether the tempdir path happens to be
            // symlinked (on macOS `/var` -> `/private/var`, so it usually
            // is), which is why it checks `contains` rather than equality.
            assert!(
                detail.contains(&root_dir.path().display().to_string()),
                "error must name the escaping cwd; got {detail:?}"
            );
        }
        other => panic!("expected RuntimeError::InvalidSpec {{ .. }}, got {other:?}"),
    }
}

/// (e) A grandchild spawned with `root: None` inherits its IMMEDIATE
/// parent's (possibly-narrowed) root, not the root agent's own (unconfined)
/// one -- mirrors `grandchild_with_cwd_none_inherits_immediate_parents_cwd_not_roots`.
#[tokio::test]
async fn grandchild_with_root_none_inherits_immediate_parents_root_not_roots() {
    let root_dir = tempfile::tempdir().unwrap();
    let child_dir = root_dir.path().join("child_scope");
    std::fs::create_dir(&child_dir).unwrap();

    let (runtime, store, _probe) = build_probe_runtime(vec![
        ScriptedTurn::Respond(text_response("root turn")),
        ScriptedTurn::Respond(text_response("child turn")),
        ScriptedTurn::Respond(text_response("grandchild turn")),
    ]);
    let mut stream = runtime.subscribe();
    let root = runtime
        .start_root(root_spec_with_cwd("hi", root_dir.path().to_path_buf()))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(
        &*runtime,
        root,
        root,
        spawn_spec_with_cwd_and_root(
            "scoped child",
            Some(child_dir.clone()),
            Some(child_dir.clone()),
        ),
    )
    .await
    .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    let mut stream = runtime.subscribe();
    let grandchild = SubagentHost::start(
        &*runtime,
        child,
        child,
        spawn_spec_with_cwd_and_root("grandchild, no override", None, None),
    )
    .await
    .unwrap();
    wait_for_agent_finished(&mut stream, grandchild).await;

    let grandchild_session = session_of(&runtime, grandchild);
    let grandchild_meta = store.meta(&grandchild_session).await.unwrap();
    assert_eq!(
        grandchild_meta.root,
        Some(child_dir.canonicalize().unwrap()),
        "a grandchild with root: None must inherit the CHILD's (immediate parent's) root"
    );
    assert_ne!(
        grandchild_meta.root, None,
        "the grandchild must NOT fall back to the root agent's own unconfined root"
    );
}

/// (f) A root that does not canonicalize fails the spawn fast, with a clear
/// error -- mirrors `spawn_with_nonexistent_cwd_fails_fast_with_a_clear_error`.
#[tokio::test]
async fn spawn_with_nonexistent_root_fails_fast_with_a_clear_error() {
    let (runtime, _store) = build_runtime(1, HashMap::new());
    let root = start_and_finish_root(&runtime, "hi").await;

    let missing = tempfile::tempdir().unwrap();
    let missing_path = missing.path().join("does-not-exist");
    drop(missing);

    let err = SubagentHost::start(
        &*runtime,
        root,
        root,
        spawn_spec_with_cwd_and_root("doomed", None, Some(missing_path.clone())),
    )
    .await
    .unwrap_err();

    match &err {
        RuntimeError::InvalidSpec { detail } => {
            assert!(
                detail.contains(&missing_path.display().to_string()),
                "error must name the offending root path; got {detail:?}"
            );
        }
        other => panic!("expected RuntimeError::InvalidSpec {{ .. }}, got {other:?}"),
    }
}

/// (f2) Min-1 (P-14): a RELATIVE root carrying a NUL byte is rejected
/// through the SHARED resolution rule (`resolve_like_the_tool_will`) -- the
/// guard the inlined "absolute -> as-is, relative -> join cwd" copies
/// silently dropped until Min-1. Before Min-1 this root would have been
/// joined onto the parent's cwd and handed to `CanonicalRoot::new`, whose
/// failure mode would have been a generic "does not canonicalize" at best;
/// now the typed rejection names the NUL itself (P-10: a typed config
/// error, never a panic).
#[tokio::test]
async fn spawn_with_nul_carrying_relative_root_is_rejected_through_the_shared_resolution_rule() {
    let (runtime, _store) = build_runtime(1, HashMap::new());
    let root = start_and_finish_root(&runtime, "hi").await;

    let err = SubagentHost::start(
        &*runtime,
        root,
        root,
        spawn_spec_with_cwd_and_root("doomed", None, Some(PathBuf::from("nul\0dir"))),
    )
    .await
    .unwrap_err();

    match &err {
        RuntimeError::InvalidSpec { detail } => {
            // Assert on the EXACT typed rejection the shared guard produces,
            // not just "NUL": the OS canonicalize error further down ALSO
            // contains "NUL" ("file name contained an unexpected NUL byte"),
            // so a looser assertion cannot tell the guard from the
            // downstream failure -- the liveness bug break-the-guard caught
            // during Min-1's own verification.
            assert!(
                detail.contains("contains a NUL byte the OS cannot resolve"),
                "the rejection must be the shared rule's typed NUL guard, not the OS \
                 canonicalize error further down; got {detail:?}"
            );
        }
        other => panic!("expected RuntimeError::InvalidSpec {{ .. }}, got {other:?}"),
    }
}

/// (g) `resume_root` preserves a session's persisted `root` unchanged --
/// `ResumeSpec` has no `root` override field at all, so there is no code
/// path that could widen or null it; this proves the header is untouched
/// by resume, not merely that no override was requested.
#[tokio::test]
async fn resume_root_preserves_persisted_root_unchanged() {
    let root_dir = tempfile::tempdir().unwrap();
    let sub_dir = root_dir.path().join("sub");
    std::fs::create_dir(&sub_dir).unwrap();

    let (runtime, store, _probe) = build_probe_runtime(vec![
        ScriptedTurn::Respond(text_response("root turn")),
        ScriptedTurn::Respond(text_response("confined child turn")),
    ]);
    let mut stream = runtime.subscribe();
    let root = runtime
        .start_root(root_spec_with_cwd("hi", root_dir.path().to_path_buf()))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    let mut stream = runtime.subscribe();
    let confined = SubagentHost::start(
        &*runtime,
        root,
        root,
        spawn_spec_with_cwd_and_root("confined", Some(sub_dir.clone()), Some(sub_dir.clone())),
    )
    .await
    .unwrap();
    wait_for_agent_finished(&mut stream, confined).await;

    let confined_session = session_of(&runtime, confined);
    let before = store.meta(&confined_session).await.unwrap();
    assert_eq!(before.root, Some(sub_dir.canonicalize().unwrap()));
    drop(runtime);

    // A fresh `Runtime` over the same store (see `build_runtime_over`'s own
    // doc): `resume_root` re-attaches the ORIGINAL `agent_id`, which the
    // first runtime still has live.
    let runtime2 = build_runtime_over(
        store.clone(),
        vec![ScriptedTurn::Respond(text_response("unused"))],
    );
    runtime2
        .resume_root(ResumeSpec {
            session: confined_session,
            agent_def: None,
            role: None,
            tools: None,
            budget: Budget::default(),
            cwd: None,
        })
        .await
        .unwrap();

    let after = store.meta(&confined_session).await.unwrap();
    assert_eq!(
        after.root, before.root,
        "resume_root must never widen or null a session's persisted root"
    );
}

/// (h) `resume_root`'s `cwd` override is checked against the session's
/// persisted `root`: an override that escapes it fails, mirroring the
/// cwd-subset-of-root invariant `SubagentHost::start` enforces at spawn
/// time.
#[tokio::test]
async fn resume_root_cwd_override_outside_persisted_root_fails() {
    let root_dir = tempfile::tempdir().unwrap();
    let sub_dir = root_dir.path().join("sub");
    let outside_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(&sub_dir).unwrap();

    let (runtime, store, _probe) = build_probe_runtime(vec![
        ScriptedTurn::Respond(text_response("root turn")),
        ScriptedTurn::Respond(text_response("confined child turn")),
    ]);
    let mut stream = runtime.subscribe();
    let root = runtime
        .start_root(root_spec_with_cwd("hi", root_dir.path().to_path_buf()))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    let mut stream = runtime.subscribe();
    let confined = SubagentHost::start(
        &*runtime,
        root,
        root,
        spawn_spec_with_cwd_and_root("confined", Some(sub_dir.clone()), Some(sub_dir.clone())),
    )
    .await
    .unwrap();
    wait_for_agent_finished(&mut stream, confined).await;

    let confined_session = session_of(&runtime, confined);
    let _ = store.meta(&confined_session).await.unwrap();
    drop(runtime);

    let runtime2 = build_runtime_over(
        store.clone(),
        vec![ScriptedTurn::Respond(text_response("unused"))],
    );
    let err = runtime2
        .resume_root(ResumeSpec {
            session: confined_session,
            agent_def: None,
            role: None,
            tools: None,
            budget: Budget::default(),
            cwd: Some(outside_dir.path().to_path_buf()),
        })
        .await
        .unwrap_err();

    match &err {
        RuntimeError::InvalidSpec { detail } => {
            assert!(
                detail.contains(&sub_dir.canonicalize().unwrap().display().to_string()),
                "error must name the session's own root; got {detail:?}"
            );
        }
        other => panic!("expected RuntimeError::InvalidSpec {{ .. }}, got {other:?}"),
    }
}

/// End-to-end spec-rejection test (not just a unit test of the
/// `translate`/`From<SubagentError> for ToolError` mapping in isolation):
/// a REAL `Runtime` (this file's actual `impl SubagentHost`, exercising the
/// genuine `tokio::fs::metadata` check in `subagent.rs`'s `start`), wrapped
/// in the exact `conway_core::ports::SubagentHandle` every `conway-tools`
/// subagent tool's `ToolCtx.subagents` field actually is, started with a
/// spec naming a nonexistent `cwd`. Nothing here is scripted: the
/// `RuntimeError::InvalidSpec` this drives is constructed by the same
/// production code path `spawn_with_nonexistent_cwd_fails_fast_with_a_clear_error`
/// above exercises, and the `SubagentError`/`ToolError` it becomes are
/// produced by `conway_core::ports::subagent::translate` and
/// `From<SubagentError> for ToolError` -- the exact functions
/// `conway-tools`' `host_error` helper calls for every subagent tool. This
/// file deliberately has no `conway-tools` dependency (see the module doc),
/// so this is as close to "through the tool" as it can get without one;
/// `conway-tools`' own `subagent.rs` test suite covers the tool-argument
/// surface (which currently exposes no `cwd`/`root` argument -- GP-04,
/// embedder-only for this slice -- so no tool call can reach this path
/// today; this test proves the PLUMBING is live for when one does).
#[tokio::test]
async fn a_real_spec_rejection_reaches_the_subagent_handle_as_invalid_arguments() {
    let (runtime, _store) = build_runtime(1, HashMap::new());
    let root = start_and_finish_root(&runtime, "hi").await;

    let missing = tempfile::tempdir().unwrap();
    let missing_path = missing.path().join("does-not-exist");
    drop(missing);

    let handle: SubagentHandle = SubagentHandle::new(runtime.clone(), root);
    let spec = spawn_spec_with_cwd("doomed", Some(missing_path.clone()));

    let subagent_err = handle.start(spec).await.unwrap_err();
    let detail = match &subagent_err {
        SubagentError::InvalidSpec { detail } => detail.clone(),
        other => panic!("expected SubagentError::InvalidSpec {{ .. }}, got {other:?}"),
    };
    assert!(
        detail.contains(&missing_path.display().to_string()),
        "error must name the offending path; got {detail:?}"
    );

    match ToolError::from(subagent_err) {
        ToolError::InvalidArguments { detail } => {
            assert!(
                detail.contains(&missing_path.display().to_string()),
                "the InvalidArguments error the model sees must still name the \
                 offending path; got {detail:?}"
            );
        }
        other => panic!(
            "a spec rejection is a model-correctable mistake and must reach the tool \
             boundary as ToolError::InvalidArguments, not {other:?}"
        ),
    }
}

// ---------------------------------------------------------------------
// Decision 01KZHEWXDZWPWMEAQ01XY2RDCB: a fork inherits the parent's own
// `agent_def` (system prompt, tools selector, model pin) when the call site
// left its own `agent_def` unset -- `SubagentHost::start`'s Fork-only
// fallback (`subagent.rs`, right before its `agent_def` resolution) -- but
// NEVER sources a `result_contract` from a def that arrived that way (only
// from a def the call site NAMED). Two dedicated fixtures below: a plugin
// registering TWO tools (one the restricted def allows, one it denies --
// with plugins: vec![] BOTH the broken and fixed paths would offer an empty
// tool list and neither guard below could ever fail on the tool list, per
// this item's own binding notes), and a builder that returns the
// `ScriptedBackend` handle so a test can inspect exactly what a child was
// offered via `ScriptedBackend::calls()`.
// ---------------------------------------------------------------------

fn marker_tool_spec() -> conway_core::content::ToolSpec {
    conway_core::content::ToolSpec {
        name: conway_core::ids::ToolName::new("marker"),
        description: "the tool the restricted def's ToolSelector::Only names".into(),
        schema: serde_json::from_value(serde_json::json!({"type": "object"})).unwrap(),
        category: conway_core::content::ToolCategory::Read,
        permission: conway_core::content::PermissionClass::Safe,
    }
}

fn secret_tool_spec() -> conway_core::content::ToolSpec {
    conway_core::content::ToolSpec {
        name: conway_core::ids::ToolName::new("secret"),
        description: "a tool the restricted def's ToolSelector::Only does NOT name".into(),
        schema: serde_json::from_value(serde_json::json!({"type": "object"})).unwrap(),
        category: conway_core::content::ToolCategory::Read,
        permission: conway_core::content::PermissionClass::Safe,
    }
}

struct InertTool(conway_core::content::ToolSpec);

#[async_trait]
impl conway_core::ports::Tool for InertTool {
    fn spec(&self) -> conway_core::content::ToolSpec {
        self.0.clone()
    }

    async fn invoke(
        &self,
        _call: conway_core::content::ToolCall,
        _ctx: conway_core::ports::ToolCtx,
    ) -> Result<conway_core::ports::ToolOutput, ToolError> {
        Ok(conway_core::ports::ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "noop".into(),
            }],
            is_error: false,
            truncation: conway_core::content::TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

/// Registers TWO tools (`marker`, `secret`) so a `ToolSelector::Only(["marker"])`
/// def is actually discriminating -- an empty registry cannot distinguish
/// "the child got the def's selector" from "the child got nothing at all"
/// (this file's own binding notes call this out as the trap: `build_runtime`
/// above passes `plugins: vec![]`, which would make either guard below
/// vacuous).
struct TwoToolPlugin;

impl conway_core::ports::Plugin for TwoToolPlugin {
    fn manifest(&self) -> conway_core::ports::PluginManifest {
        conway_core::ports::PluginManifest {
            id: "two_tool".to_string(),
            version: "0.0.0".to_string(),
            tools: vec![marker_tool_spec().name, secret_tool_spec().name],
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn conway_core::ports::Tool>> {
        vec![
            Arc::new(InertTool(marker_tool_spec())),
            Arc::new(InertTool(secret_tool_spec())),
        ]
    }
}

/// The model `restricted_def` pins. Non-default on purpose (decision
/// 01KZHEWXDZWPWMEAQ01XY2RDCB: fork-only `agent_def` inheritance covers the
/// system prompt, the tools selector, AND the model pin) -- distinct from
/// `default_model_ref` below, so a router that actually resolves
/// `RouteRequest::pin` (`pin_aware_router`, not `FakeRouter::single`, which
/// ignores it) can tell "the child's pin propagated" from "the child fell
/// back to the role's plain default chain".
///
/// `restricted_def`'s `model` used to be `None` here: that left every
/// existing user of this fixture (`FakeRouter::single`, which ignores
/// `req.pin` unconditionally) unable to distinguish "the pin inherited" from
/// "there was never a pin to inherit". Setting it to a distinguishing value
/// changes nothing for those three existing tests -- none of them read
/// `GenerateRequest.model`, and `FakeRouter::single` still returns the same
/// fixed route regardless of `req.pin` -- but it is what makes the new pin
/// guard below possible without a second, parallel `AgentDef` fixture.
fn pinned_model_ref() -> ModelRef {
    ModelRef {
        backend: BackendId::new("pinned-backend"),
        model: ModelId::new("pinned-model"),
    }
}

/// The "planner" role's plain (unpinned) default under `pin_aware_router` --
/// what a `RouteRequest` with `pin: None` resolves to.
fn default_model_ref() -> ModelRef {
    ModelRef {
        backend: BackendId::new("default-backend"),
        model: ModelId::new("default-model"),
    }
}

fn restricted_def() -> AgentDef {
    AgentDef {
        name: "restricted".to_string(),
        description: None,
        system_prompt: "You are restricted to the marker tool.".to_string(),
        role: None,
        model: Some(pinned_model_ref()),
        tools: ToolSelector::Only(vec!["marker".to_string()]),
        skills: Vec::new(),
        max_steps: None,
        result_contract: None,
    }
}

fn restricted_def_with_contract() -> AgentDef {
    AgentDef {
        result_contract: Some(schema_requiring_summary()),
        ..restricted_def()
    }
}

fn schema_requiring_summary() -> schemars::schema::RootSchema {
    serde_json::from_value(serde_json::json!({
        "type": "object",
        "properties": { "summary": { "type": "string" } },
        "required": ["summary"],
    }))
    .unwrap()
}

/// Mirrors `build_runtime` above, but wires `TwoToolPlugin` (so
/// `ToolSelector::Only`/`Except` are actually discriminating) and returns
/// the `ScriptedBackend` handle -- `build_runtime` drops it, and
/// `ScriptedBackend::calls()` is the only way to inspect exactly what a
/// child's own `GenerateRequest.tools` contained.
fn build_runtime_with_two_tools_and_defs(
    turns: usize,
    agent_defs: HashMap<String, AgentDef>,
) -> (Arc<Runtime>, Arc<ScriptedBackend>) {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());

    let backend = Arc::new(
        ScriptedBackend::new(
            (0..turns)
                .map(|_| ScriptedTurn::Respond(text_response("ok")))
                .collect(),
        )
        .with_id(BackendId::new("b")),
    );
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend.clone());

    let runtime = Runtime::new(RuntimeDeps {
        store,
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![Arc::new(TwoToolPlugin)],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs,
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),
    });
    (runtime, backend)
}

/// Guard 1: byte-for-byte what `conway_fork` (and the TUI's `bare_fork`, and
/// `ForkSpec::from`) build -- `SubagentSpec::fork("go", ..)` leaves
/// `agent_def: None`/`tools: None` -- forked off a root running under a
/// `ToolSelector::Only(["marker"])` def. The discriminating observable is
/// the child's own `GenerateRequest.tools`, captured via
/// `ScriptedBackend::calls()`: this IS literally what the model was offered,
/// which is the escalation itself, not a proxy for it (an error string, a
/// gate-call count, or tree bookkeeping would all be a step removed). Two
/// secondary assertions cover the under-inheritance half: `AgentNode::
/// agent_def` and a `Provenance::AgentDef` segment in the child's own
/// assembled context.
///
/// Break-the-guard expectation (reverting the fill in `subagent.rs::start`):
/// the child's `GenerateRequest.tools` contains BOTH `marker` and `secret`
/// (the full two-tool registry -- `PluginRegistry::specs`'s `selector.
/// is_none_or(..)` "no selector -> everything" fallback, since an unfilled
/// `spec.agent_def` resolves no `agent_def.tools` to narrow against), and
/// the assertion fails on that list directly, not on an error string.
#[tokio::test]
async fn fork_child_inherits_the_parents_agent_def_and_cannot_widen_its_tool_set() {
    let mut defs = HashMap::new();
    defs.insert("restricted".to_string(), restricted_def());
    let (runtime, backend) = build_runtime_with_two_tools_and_defs(2, defs);

    let mut spec = root_spec("investigate");
    spec.agent_def = Some(AgentDefRef("restricted".to_string()));
    let mut stream = runtime.subscribe();
    let root = runtime.start_root(spec).await.unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    // Byte-for-byte what `conway_fork` builds: no `agent_def`, no `tools`.
    let child_spec = SubagentSpec::fork("go", Budget::default());
    assert!(
        child_spec.agent_def.is_none(),
        "this spec must start with no agent_def, or the test proves nothing about inheritance"
    );
    assert!(child_spec.tools.is_none());

    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(&*runtime, root, root, child_spec)
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    // The discriminating observable: exactly what the model was offered.
    let calls = backend.calls();
    let child_call = calls
        .last()
        .expect("the child must have made at least one generate call");
    let offered: Vec<String> = child_call
        .tools
        .iter()
        .map(|t| t.name.0.clone())
        .collect();
    assert_eq!(
        offered,
        vec!["marker".to_string()],
        "the child must be offered EXACTLY the restricted def's tool set (inherited from the \
         parent), not the full two-tool registry -- got {offered:?}"
    );
    assert!(
        !offered.contains(&"secret".to_string()),
        "the child must specifically NOT be offered a tool the inherited def denies"
    );

    // Secondary: the under-inheritance half.
    let child_node = runtime
        .tree()
        .nodes
        .into_iter()
        .find(|n| n.agent_id == child)
        .expect("child attached to the tree");
    assert_eq!(
        child_node.agent_def,
        Some("restricted".to_string()),
        "AgentNode::agent_def must carry the inherited def's name"
    );

    let report = runtime.context_report(child).unwrap();
    assert!(
        report
            .segments
            .iter()
            .any(|e| matches!(&e.provenance, Provenance::AgentDef { name } if name == "restricted")),
        "the child's own context must carry a Provenance::AgentDef segment for the inherited \
         def, got: {:?}",
        report.segments
    );
}

/// A REAL `Router` (not a fake that ignores the field) built solely to make
/// `RouteRequest::pin` discriminating: `MinimalRouter` -- `conway-core`'s
/// production config-only router -- resolves a `Some` pin to a
/// single-element chain naming exactly that model (`chain_for`), and falls
/// back to the "planner" role's configured chain (`default_model_ref`) only
/// when `pin` is `None`. `FakeRouter::single`, used by every other builder
/// in this file (including `build_runtime_with_two_tools_and_defs`, which
/// Guard 1 above uses), returns the same fixed route regardless of
/// `req.pin` -- exactly why it cannot tell "the fork's model-pin fill ran"
/// from "it didn't".
fn pin_aware_router() -> MinimalRouter {
    let mut roles = BTreeMap::new();
    roles.insert(
        "planner".to_string(),
        RoleConfig {
            chain: vec![default_model_ref()],
            required: RequiredCaps::default(),
            params: SamplingParams::default(),
            headroom_tokens: None,
        },
    );
    MinimalRouter::new(RoutingConfig {
        roles,
        health: HealthConfig::default(),
        default_headroom_tokens: 4096,
    })
}

/// Mirrors `build_runtime_with_two_tools_and_defs`, but wires
/// `pin_aware_router` in place of `FakeRouter::single`, and registers ONE
/// `ScriptedBackend` under BOTH `default_model_ref().backend` and
/// `pinned_model_ref().backend` -- `AttemptEngine::backend_for`
/// (`attempt.rs`) looks the resolved route's backend id up in this map, so
/// whichever one the router picks, its calls land on the SAME instance,
/// and `ScriptedBackend::calls()` reports exactly which model the resolved
/// route named regardless of which backend id was chosen.
fn build_runtime_with_pin_aware_router(
    turns: usize,
    agent_defs: HashMap<String, AgentDef>,
) -> (Arc<Runtime>, Arc<ScriptedBackend>) {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(ScriptedBackend::new(
        (0..turns)
            .map(|_| ScriptedTurn::Respond(text_response("ok")))
            .collect(),
    ));
    let router: Arc<dyn Router> = Arc::new(pin_aware_router());
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(default_model_ref().backend, backend.clone());
    backends.insert(pinned_model_ref().backend, backend.clone());

    let runtime = Runtime::new(RuntimeDeps {
        store,
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs,
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),
    });
    (runtime, backend)
}

/// Guard 1b (decision 01KZHEWXDZWPWMEAQ01XY2RDCB): the SAME Fork-only
/// inheritance fill Guard 1 above proves for `tools` also carries the
/// inherited def's MODEL PIN through to the real routing request --
/// `subagent.rs::start`'s `let pin = agent_def.and_then(|d| d.model.clone())`
/// feeds `AgentSpec::pin`, which `agent_loop.rs::route_and_attempt` copies
/// straight into `RouteRequest::pin` on every attempt (`route_req.pin =
/// self.spec.pin.clone()`). The discriminating observable is the child's own
/// `GenerateRequest.model` -- read back off `ScriptedBackend::calls()`,
/// exactly as Guard 1 reads `.tools` -- i.e. the model reference that
/// actually reached the real routing request, not a field copied onto a
/// tree node, and under a router that ACTUALLY resolves `pin`
/// (`pin_aware_router`, not `FakeRouter::single`, which returns the same
/// fixed route whether `pin` is `Some` or `None` and so could never fail
/// here even if the fill were deleted).
///
/// Break-the-guard expectation (reverting the fill in `subagent.rs::start`):
/// the child's `agent_def` resolves to `None`, so `pin` is `None`, and
/// `pin_aware_router` falls back to the "planner" role's plain chain -- the
/// child's `GenerateRequest.model` becomes `default_model_ref().model`, not
/// `pinned_model_ref().model`, and the assertion below fails on that value
/// directly.
#[tokio::test]
async fn fork_child_inherits_the_parents_agent_def_pinned_model() {
    let mut defs = HashMap::new();
    defs.insert("restricted".to_string(), restricted_def());
    let (runtime, backend) = build_runtime_with_pin_aware_router(2, defs);

    let mut spec = root_spec("investigate");
    spec.agent_def = Some(AgentDefRef("restricted".to_string()));
    let mut stream = runtime.subscribe();
    let root = runtime.start_root(spec).await.unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    // Byte-for-byte what `conway_fork` (and the TUI's `bare_fork`, and
    // `ForkSpec::from`) build: no `agent_def`.
    let child_spec = SubagentSpec::fork("go", Budget::default());
    assert!(
        child_spec.agent_def.is_none(),
        "this spec must start with no agent_def, or the test proves nothing about inheritance"
    );

    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(&*runtime, root, root, child_spec)
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    // The discriminating observable: the model reference that actually
    // reached the child's own routing/generate request.
    let calls = backend.calls();
    let child_call = calls
        .last()
        .expect("the child must have made at least one generate call");
    assert_eq!(
        child_call.model,
        pinned_model_ref().model,
        "the forked child must route on the restricted def's inherited model pin, not the \
         role's plain default -- expected {:?}, the role default is {:?}, got {:?}",
        pinned_model_ref().model,
        default_model_ref().model,
        child_call.model
    );
}

/// Characterization test for board item 01KZHET5G0DN7QC0YF5G9XSB1N /
/// decision 01KZHH9N313T5BTDR8281QDWHC: an agent def's (or a call site's)
/// `tools` selects what is announced to the model, it is NOT a capability
/// restriction. `subagent.rs::start` computes the child's announced
/// selector as `spec.tools.clone().or_else(|| agent_def.map(|d|
/// d.tools.clone()))` -- an explicit call-site `tools` *replaces* the
/// inherited def's selector outright, it is never intersected with it. This
/// pins that behavior directly, against the same `restricted` def (`Only(
/// ["marker"])`) Guard 1 above uses, but this time the fork spec supplies
/// its OWN `tools` naming `secret` -- the one tool the inherited def
/// excludes. Reusing Guard 1's two-tool fixture (`TwoToolPlugin`,
/// `build_runtime_with_two_tools_and_defs`, `restricted_def`) rather than a
/// third: a registry with fewer than two tools would make this vacuous, the
/// same trap Guard 1's own doc calls out.
///
/// If this ever asserted `["marker"]` here (or an error/deny), that would
/// mean an intersection had been added somewhere and this item's ruling
/// (Shape B: retract the false "narrowing-only" claims, do not implement
/// enforcement) would need to be revisited, since the four retracted
/// doc-comment claims would have become true.
#[tokio::test]
async fn fork_child_explicit_tools_argument_replaces_rather_than_narrows_the_inherited_defs_selector(
) {
    let mut defs = HashMap::new();
    defs.insert("restricted".to_string(), restricted_def());
    let (runtime, backend) = build_runtime_with_two_tools_and_defs(2, defs);

    let mut spec = root_spec("investigate");
    spec.agent_def = Some(AgentDefRef("restricted".to_string()));
    let mut stream = runtime.subscribe();
    let root = runtime.start_root(spec).await.unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    // An explicit `tools` argument naming a tool the inherited def's
    // `ToolSelector::Only(["marker"])` does NOT include -- the same shape a
    // model's `conway_fork` call, or `conway_ask`'s `AskArgs::tools`, can
    // produce.
    let mut child_spec = SubagentSpec::fork("go", Budget::default());
    child_spec.tools = Some(ToolSelector::Only(vec!["secret".to_string()]));
    assert!(child_spec.agent_def.is_none());

    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(&*runtime, root, root, child_spec)
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    // The discriminating observable: exactly what the model was offered.
    let calls = backend.calls();
    let child_call = calls
        .last()
        .expect("the child must have made at least one generate call");
    let offered: Vec<String> = child_call
        .tools
        .iter()
        .map(|t| t.name.0.clone())
        .collect();
    assert_eq!(
        offered,
        vec!["secret".to_string()],
        "an explicit `tools` argument REPLACES the inherited def's selector rather than \
         intersecting with it -- the child was offered `secret`, a tool the `restricted` def's \
         Only([\"marker\"]) selector excludes. `tools` selects what is announced, it is not a \
         capability boundary -- got {offered:?}"
    );
}

/// Guard 2: the contract rule. Same `restricted` def, but this one also
/// carries a `result_contract` (`schema_requiring_summary`). The fork
/// child's own spec mirrors the TUI's `bare_fork` EXACTLY: `agent_def: None`
/// (inherit), `result_contract: None` (never inherited, per this item's
/// ruling), `tools: Some(Except(["report"]))` (the TUI's own hardcoded
/// keep-alive selector). If a def-declared contract were wrongly sourced
/// from an INHERITED def, this child would be required to call `report` to
/// produce `structured` while simultaneously being denied that very tool --
/// `ContractOutcome::Retry` then `Rejected`, `01KZGX1RR0VXN2YH3P75SBE9SA`'s
/// exact failure shape reproduced in a new path with nobody having typed
/// either half. `ScriptedBackend` never calls `report` at all here (plain
/// text turns only), so a `Some` contract fails validation immediately
/// (`structured` is null) -- three turns are scripted so the BROKEN path
/// (which spends its one retry) still has a turn to consume rather than
/// hitting "scripted backend exhausted" and masking the real assertion.
///
/// Break-the-guard expectation (reverting the `def_was_inherited` carve-out
/// in `subagent.rs::start`'s `result_contract` computation): the child ends
/// `Rejected { missing: [..] }`, not `Completed`.
#[tokio::test]
async fn fork_child_does_not_source_a_result_contract_from_an_inherited_agent_def() {
    let mut defs = HashMap::new();
    defs.insert("restricted".to_string(), restricted_def_with_contract());
    let (runtime, _backend) = build_runtime_with_two_tools_and_defs(3, defs);

    let mut spec = root_spec("investigate");
    spec.agent_def = Some(AgentDefRef("restricted".to_string()));
    let mut stream = runtime.subscribe();
    let root = runtime.start_root(spec).await.unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    // Mirrors the TUI's `bare_fork` (`commands.rs`) exactly: no agent_def,
    // no result_contract, tools narrowed to exclude `report`.
    let mut child_spec = SubagentSpec::fork("go", Budget::default());
    child_spec.tools = Some(ToolSelector::Except(vec!["report".to_string()]));
    assert!(child_spec.agent_def.is_none());
    assert!(child_spec.result_contract.is_none());

    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(&*runtime, root, root, child_spec)
        .await
        .unwrap();
    let result = wait_for_agent_finished(&mut stream, child).await;

    assert_eq!(
        result.status,
        ResultStatus::Completed,
        "a forked child must NOT inherit its def's result_contract merely because the def \
         itself was inherited (never named at the call site) -- got {result:?}"
    );
}

// ---------------------------------------------------------------------
// Guard 3 (decision 01KZHEWXDZWPWMEAQ01XY2RDCB): the fork-only `agent_def`
// inheritance fill is gated on `spec.mode == SubagentMode::Fork`
// (`subagent.rs::start`'s `def_was_inherited` computation) -- a spawn from a
// parent running under an `agent_def` must NOT pick that def up merely
// because it left its own `agent_def` unset. `spawn_without_agent_def_
// inherits_the_parents_role` above already proves the child ends up with
// `agent_def: None`, but its root has NO definition at all
// (`build_runtime(.., HashMap::new())`), so "the child has no agent_def"
// there is equally consistent with "the gate correctly declined" and with
// "there was never anything to decline" -- it cannot fail if the Fork-only
// condition above were loosened to also cover `Spawn`. This guard reuses
// `restricted_def`/`build_runtime_with_two_tools_and_defs` (Guard 1's own
// fixtures) as the SPAWNING parent specifically so there is something real
// to decline: the discriminating observable is the child's own
// `GenerateRequest.tools`, which must be the full two-tool registry
// (`marker` AND `secret`), not narrowed to the restricted def's
// `Only(["marker"])` -- narrowing here would mean the fill had started
// firing for `Spawn` too.
// ---------------------------------------------------------------------

/// Break-the-guard expectation (widening `def_was_inherited`'s mode check
/// in `subagent.rs::start` to fire for `Spawn` as well as `Fork`): the
/// child's `GenerateRequest.tools` narrows to exactly `["marker"]` (the
/// restricted def's selector) instead of the full two-tool registry, and
/// `AgentNode::agent_def` becomes `Some("restricted")` instead of `None`.
#[tokio::test]
async fn spawn_child_declines_the_parents_agent_def_even_though_a_fork_would_inherit_it() {
    let mut defs = HashMap::new();
    defs.insert("restricted".to_string(), restricted_def());
    let (runtime, backend) = build_runtime_with_two_tools_and_defs(2, defs);

    // The spawning parent: a root running under the restricted def, exactly
    // as Guard 1's fork case starts -- the only difference from that guard
    // is the mode of the CHILD spec below.
    let mut spec = root_spec("investigate");
    spec.agent_def = Some(AgentDefRef("restricted".to_string()));
    let mut stream = runtime.subscribe();
    let root = runtime.start_root(spec).await.unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    // A bare spawn, mirroring `spawn_without_agent_def_inherits_the_parents_
    // role`'s own construction: no `agent_def` named at the call site.
    let child_spec = SubagentSpec {
        mode: SubagentMode::Spawn,
        prompt: "do it".into(),
        agent_def: None,
        role: None,
        tools: None,
        budget: Budget::default(),
        result_contract: None,
        keep_alive: false,
        ephemeral: false,
        ask_origin: None,
        cwd: None,
        root: None,
        tag: None,
    };
    assert!(child_spec.agent_def.is_none());

    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(&*runtime, root, root, child_spec)
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    // The discriminating observable: exactly what the model was offered --
    // the full two-tool registry, NOT the restricted def's narrowed set,
    // proving the spawn declined to inherit the parent's `agent_def` rather
    // than merely having nothing to decline.
    let calls = backend.calls();
    let child_call = calls
        .last()
        .expect("the child must have made at least one generate call");
    let mut offered: Vec<String> = child_call
        .tools
        .iter()
        .map(|t| t.name.0.clone())
        .collect();
    offered.sort();
    assert_eq!(
        offered,
        vec!["marker".to_string(), "secret".to_string()],
        "a spawn must NOT inherit the parent's agent_def merely because it left its own unset \
         -- the child should be offered the full two-tool registry, not the restricted def's \
         Only([\"marker\"]) selector -- got {offered:?}"
    );

    let child_node = runtime
        .tree()
        .nodes
        .into_iter()
        .find(|n| n.agent_id == child)
        .expect("child attached to the tree");
    assert_eq!(
        child_node.agent_def, None,
        "a spawn with no agent_def named must resolve none -- even though the parent it spawned \
         from has one, unlike a fork's fill"
    );
}

// ---------------------------------------------------------------------
// Board item 01KZQJ03ZQ22MPM9H2TW1350ZF: `SubagentSpec::tag`, an opaque
// consumer correlation identifier threaded onto `ContextHookCtx::tag` --
// see that field's own doc for the "conway never interprets this" guarantee.
// ---------------------------------------------------------------------

/// Everything one `ContextHook::before_request` call observed, recorded so a
/// test can assert on what the hook actually received (P-15) rather than on
/// an intermediate value. `segments` deliberately captures role/content/
/// provenance -- everything about a segment EXCEPT its own random
/// `SegmentId` -- so two otherwise-identical turns can be compared for
/// structural equality without a random id manufacturing a spurious
/// mismatch.
#[derive(Clone, Debug)]
struct CapturedHookCall {
    agent_id: AgentId,
    turn: u32,
    tag: Option<String>,
    model: Option<ModelRef>,
    estimated_tokens: u32,
    segments: Vec<(Role, Vec<ContentBlock>, Provenance)>,
    tool_names: Vec<String>,
}

struct RecordingContextHook {
    captured: std::sync::Mutex<Vec<CapturedHookCall>>,
}

impl RecordingContextHook {
    fn new() -> Self {
        Self {
            captured: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<CapturedHookCall> {
        self.captured.lock().unwrap().clone()
    }

    /// Every call this hook observed for one specific agent, in the order
    /// they were made -- index `0` is that agent's FIRST turn.
    fn calls_for(&self, agent: AgentId) -> Vec<CapturedHookCall> {
        self.calls()
            .into_iter()
            .filter(|c| c.agent_id == agent)
            .collect()
    }
}

#[async_trait]
impl ContextHook for RecordingContextHook {
    async fn before_request(&self, ctx: &ContextHookCtx, payload: ContextPayload) -> ContextPayload {
        let segments = payload
            .segments
            .iter()
            .map(|s| (s.role, s.content.clone(), s.provenance.clone()))
            .collect();
        let mut tool_names: Vec<String> = payload.tools.iter().map(|t| t.name.0.clone()).collect();
        tool_names.sort();
        self.captured.lock().unwrap().push(CapturedHookCall {
            agent_id: ctx.agent_id,
            turn: ctx.turn,
            tag: ctx.tag.clone(),
            model: ctx.model.clone(),
            estimated_tokens: ctx.estimated_tokens,
            segments,
            tool_names,
        });
        // Pass-through: this hook's job is observing, never transforming.
        payload
    }
}

/// The acceptance criterion that matters most, in the item's own framing:
/// "the first-turn timing IS the defect; a test that reads it back later
/// proves nothing." The hook is registered BEFORE the child is even started
/// -- exactly how a real embedder wires one up once, at startup -- so there
/// is no window in which the child's first turn could run unobserved, and
/// the assertion is against what the hook actually received on that FIRST
/// call, not against `SubagentSpec::tag` still being present on some spec
/// value sitting in a local variable.
#[tokio::test]
async fn consumer_tag_is_readable_from_the_context_hook_on_the_childs_first_turn() {
    let (runtime, _store) = build_runtime(2, HashMap::new());
    let hook = Arc::new(RecordingContextHook::new());
    runtime.set_context_hook(Some(hook.clone()));
    let root = start_and_finish_root(&runtime, "hi").await;

    let mut spec = fork_spec("investigate the ticket");
    spec.tag = Some("ticket-42".to_string());
    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(&*runtime, root, root, spec)
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    let calls = hook.calls_for(child);
    assert!(
        !calls.is_empty(),
        "the registered ContextHook must have been invoked for the child's own turn"
    );
    assert_eq!(
        calls[0].turn, 0,
        "the FIRST captured call for this agent must be turn 0 -- the child's first turn, not \
         a later one"
    );
    assert_eq!(
        calls[0].tag,
        Some("ticket-42".to_string()),
        "the consumer's tag must be readable from ContextHookCtx on the child's first turn, not \
         merely present somewhere afterward"
    );
}

/// **conway never interprets the value, proven rather than asserted**: a
/// tag containing arbitrary content (control characters including a NUL
/// byte, a multi-byte BMP character, and a non-BMP emoji -- deliberately not
/// "just ASCII", the closest a `String`-typed field can get to "arbitrary
/// bytes"), an empty string, and a very long string (100,000 characters) all
/// round-trip through `SubagentSpec::tag` -> `AgentSpec::tag` ->
/// `ContextHookCtx::tag` byte-for-byte unchanged.
#[tokio::test]
async fn consumer_tag_round_trips_arbitrary_content_empty_and_very_long_strings_unchanged() {
    let (runtime, _store) = build_runtime(4, HashMap::new());
    let hook = Arc::new(RecordingContextHook::new());
    runtime.set_context_hook(Some(hook.clone()));
    let root = start_and_finish_root(&runtime, "hi").await;

    let cases: Vec<String> = vec![
        "\0\u{1}\u{7}control-and-multibyte-\u{FFFD}-emoji-\u{1F600}-quote\"-backslash\\-newline\n"
            .to_string(),
        String::new(),
        "x".repeat(100_000),
    ];

    for tag in cases {
        let mut spec = fork_spec("go");
        spec.tag = Some(tag.clone());
        let mut stream = runtime.subscribe();
        let child = SubagentHost::start(&*runtime, root, root, spec)
            .await
            .unwrap();
        wait_for_agent_finished(&mut stream, child).await;

        let calls = hook.calls_for(child);
        assert_eq!(
            calls[0].tag.as_deref(),
            Some(tag.as_str()),
            "a tag of length {} must round-trip byte-for-byte unchanged",
            tag.len()
        );
    }
}

/// Two agents differing ONLY in their tag must take identical paths --
/// proven by comparing what each child's context assembly, routing, budget
/// accounting, and persisted transcript actually produced, not by reasoning
/// about the implementation. `PermissionRequest` carries no `tag` field at
/// all (this item's own scoping decision), so there is structurally nothing
/// for a permission decision to diverge on; the assertions below cover the
/// three axes that DO see a value derived from this agent's spec (routing,
/// context/budget, and the persisted log).
#[tokio::test]
async fn two_agents_differing_only_in_tag_take_identical_routing_context_and_logging_paths() {
    let (runtime, store) = build_runtime(3, HashMap::new());
    let hook = Arc::new(RecordingContextHook::new());
    runtime.set_context_hook(Some(hook.clone()));
    let root = start_and_finish_root(&runtime, "hi").await;

    let mut stream = runtime.subscribe();
    let untagged = SubagentHost::start(&*runtime, root, root, fork_spec("do the work"))
        .await
        .unwrap();
    let untagged_result = wait_for_agent_finished(&mut stream, untagged).await;

    let tag_text = "a very distinctive tag: \u{1F600}\0\t".to_string();
    let mut tagged_spec = fork_spec("do the work");
    tagged_spec.tag = Some(tag_text.clone());
    let mut stream = runtime.subscribe();
    let tagged = SubagentHost::start(&*runtime, root, root, tagged_spec)
        .await
        .unwrap();
    let tagged_result = wait_for_agent_finished(&mut stream, tagged).await;

    assert_eq!(untagged_result.status, ResultStatus::Completed);
    assert_eq!(tagged_result.status, ResultStatus::Completed);

    let untagged_call = hook
        .calls_for(untagged)
        .into_iter()
        .next()
        .expect("untagged child's context hook must have fired");
    let tagged_call = hook
        .calls_for(tagged)
        .into_iter()
        .next()
        .expect("tagged child's context hook must have fired");

    // Different agents, by construction -- and, the point of this test,
    // different tags. A test that could not tell these two calls apart
    // would prove nothing about the comparisons below.
    assert_ne!(untagged_call.agent_id, tagged_call.agent_id);
    assert_eq!(untagged_call.tag, None);
    assert_eq!(tagged_call.tag, Some(tag_text.clone()));

    assert_eq!(
        untagged_call.model, tagged_call.model,
        "routing must not diverge based on the tag alone"
    );
    assert_eq!(
        untagged_call.estimated_tokens, tagged_call.estimated_tokens,
        "context sizing must not diverge based on the tag alone"
    );
    assert_eq!(
        untagged_call.tool_names, tagged_call.tool_names,
        "announced tools must not diverge based on the tag alone"
    );
    assert_eq!(
        untagged_call.segments, tagged_call.segments,
        "the assembled segments (role/content/provenance, deliberately excluding each \
         segment's own random id) must be identical -- the tag must never be rendered into \
         context"
    );

    let nodes = runtime.tree().nodes;
    let untagged_node = nodes
        .iter()
        .find(|n| n.agent_id == untagged)
        .expect("untagged child attached to the tree");
    let tagged_node = nodes
        .iter()
        .find(|n| n.agent_id == tagged)
        .expect("tagged child attached to the tree");
    assert_eq!(
        untagged_node.steps_taken, tagged_node.steps_taken,
        "budget consumption must not diverge based on the tag alone"
    );
    assert_eq!(
        untagged_node.budget, tagged_node.budget,
        "budget must not diverge based on the tag alone"
    );

    // Logging: the tagged child's own persisted transcript never mentions
    // the tag text anywhere -- proof the tag is never written into a log
    // record either, the third behavior class the acceptance criterion
    // names alongside routing and budget.
    let tagged_session = session_of(&runtime, tagged);
    let tagged_records = store
        .read(&tagged_session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    assert!(
        tagged_records
            .iter()
            .all(|r| !format!("{r:?}").contains(&tag_text)),
        "the tag must never appear in a persisted LogRecord"
    );
}
