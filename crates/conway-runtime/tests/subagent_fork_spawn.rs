//! Acceptance tests for `impl SubagentHost for Runtime` (WI-084,
//! architecture §4.6, §5.1, §5.2): fork/spawn, inherited context, and
//! session forking.
//!
//! Built entirely from `conway-core`'s fakes plus a local `CountingStore`
//! decorator (mirrors `runtime_api.rs`'s and `agent_loop_e2e.rs`'s own
//! practice of small local test doubles) -- this file does not depend on
//! `conway-backends` or `conway-tools`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use conway_core::agent::{
    AgentDefRef, Budget, PermissionDecision, ResultStatus, SubagentMode, SubagentSpec, ToolSelector,
};
use conway_core::config::AgentDef;
use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::error::{RuntimeError, ToolError};
use conway_core::event::Event;
use conway_core::fakes::{
    FakeGate, FakeHealth, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn,
};
use conway_core::ids::{AgentId, BackendId, LogSeq, ModelId, ModelRef, RoleAlias, SessionId};
use conway_core::log::{ForkOrigin, SessionFilter, SessionMeta};
use conway_core::ports::{Backend, Router, SessionStore, SubagentHost};
use conway_core::provenance::Provenance;
use conway_routing::config::HeadroomPolicy;
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{RootSpec, Runtime, RuntimeDeps};
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
        prompt: Some(prompt.to_string()),
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
                if let Event::AgentFinished { result } = envelope.event {
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
    let child = SubagentHost::start(&*runtime, root, fork_spec("look closer"))
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
    let child = SubagentHost::start(&*runtime, root, fork_spec("look closer"))
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
    let child = SubagentHost::start(&*runtime, root, spec).await.unwrap();
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

/// `RuntimeError` has no `InvalidSpec` variant (see `subagent.rs`'s module
/// doc): a `Spawn` without `agent_def` is rejected via
/// `SubagentSpec::validate()`, surfaced here as `RuntimeError::Tool
/// (ToolError::Internal{..})`, the crate's established "closest fit"
/// mapping for a gap shaped like this one.
#[tokio::test]
async fn spawn_without_agent_def_is_rejected() {
    let (runtime, _store) = build_runtime(1, HashMap::new());
    let root = start_and_finish_root(&runtime, "hi").await;

    let spec = SubagentSpec {
        mode: SubagentMode::Spawn,
        prompt: "do it".into(),
        agent_def: None,
        role: None,
        tools: None,
        budget: Budget::default(),
        cache_hint: false,
        result_contract: None,
        await_result: true,
    };
    let err = SubagentHost::start(&*runtime, root, spec)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        RuntimeError::Tool(ToolError::Internal { .. })
    ));
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

    let fork_child = SubagentHost::start(&*runtime, root, fork_spec("dig in"))
        .await
        .unwrap();
    let spawn_child = SubagentHost::start(
        &*runtime,
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
    let child = SubagentHost::start(&*runtime, root, fork_spec("dig in"))
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
        let child = SubagentHost::start(&*runtime, root, fork_spec(&format!("sibling {i}")))
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
    let a = SubagentHost::start(&*runtime, root, fork_spec("dig in"))
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
    let b = SubagentHost::start(&*runtime, a, fork_spec("grandchild look closer"))
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
    let c = SubagentHost::start(&*runtime, a, fork_spec("second grandchild"))
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
    let child = SubagentHost::start(&*runtime, root, spec).await.unwrap();
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
    let err = SubagentHost::await_result(&*runtime, unknown)
        .await
        .unwrap_err();
    assert!(matches!(err, RuntimeError::AgentNotFound { agent } if agent == unknown));

    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(&*runtime, root, fork_spec("go"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    let result = SubagentHost::await_result(&*runtime, child).await.unwrap();
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
    let child = SubagentHost::start(&*runtime, root, fork_spec("go slow"))
        .await
        .unwrap();

    // Two concurrent awaiters, both issued right after `start` returns --
    // well before the child's 150ms tool call has any chance to finish.
    let r1 = tokio::spawn({
        let runtime = runtime.clone();
        async move { SubagentHost::await_result(&*runtime, child).await.unwrap() }
    });
    let r2 = tokio::spawn({
        let runtime = runtime.clone();
        async move { SubagentHost::await_result(&*runtime, child).await.unwrap() }
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
// ToolCtx::subagents IS the Runtime: a tool that forks through it produces
// a child visible in this same runtime's tree.
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
        let child = ctx
            .subagents
            .start(
                ctx.agent_id,
                SubagentSpec::fork("nested", Budget::default()),
            )
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
