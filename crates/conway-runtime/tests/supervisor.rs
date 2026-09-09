//! Acceptance tests for `AgentTree` and the supervisor (architecture
//! §7): attachment/structural lookups, cancellation propagation, and the
//! guarantee that `await_result` always terminates -- panic containment,
//! budget-deadline synthesis, and hard cancellation.
//!
//! These tests exercise `AgentTree`/`supervisor::supervise` directly against
//! bare mock tasks (not a real `AgentLoop` -- `agent_loop.rs`/`subagent.rs`
//! wiring is the job) so the supervision guarantee itself is proven
//! independent of any particular agent implementation.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use conway_core::agent::{AgentResult, AgentStatus, Budget, ResultStatus, SubagentMode};
use conway_core::error::{HookFailure, RuntimeError};
use conway_core::event::{Envelope, Event};
use conway_core::hook::{HookAnswer, HookInvocation, HookOrigin};
use conway_core::ids::{AgentId, RoleAlias, SessionId};
use conway_core::log::SessionMeta;
use conway_core::ports::{HookRunner, PluginConfig, SessionStore};
use conway_runtime::events::EventBus;
use conway_runtime::hook_dispatch::{HookDispatcher, HookSpec, CHILD_REPORTED};
use conway_runtime::supervisor::{self, SuperviseArgs};
use conway_runtime::tree::{AgentNode, AgentTree};
use conway_testkit::FakeStore;
use futures::future::FutureExt;
use futures::StreamExt;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// A fresh, empty `SessionStore` -- every test in this file that does not
/// itself care about durable persistence (most of them: this file's own
/// module doc says it exercises the supervision guarantee "independent of
/// any particular agent implementation") passes one of these to
/// `SuperviseArgs::store` so the struct literal compiles. A `Synthesized`
/// branch's `persist_agent_result` call against a session this store never
/// `create`d simply logs a `NotFound` and continues (best-effort, mirroring
/// `AgentLoop::finish`'s identical failure handling) -- see
/// `a_synthesized_panic_persists_a_terminal_agent_result_as_the_logs_last_
/// record`, below, for the one test that DOES seed a real session and
/// asserts on what actually landed in it.
fn fake_store() -> Arc<dyn SessionStore> {
    Arc::new(FakeStore::new())
}

/// Creates `session` in `store` with a minimal, filler `SessionMeta` --
/// enough for `persist_agent_result`'s `head`/`append` couplet to succeed
/// against it. Mirrors `conway-runtime/src/agent_loop.rs`'s own
/// `seeded_session` test helper (byte-for-byte the same shape), duplicated
/// here rather than shared across a src/tests boundary neither crate
/// exposes a seam for.
async fn seed_session(store: &dyn SessionStore, agent: AgentId, session: SessionId) {
    store
        .create(SessionMeta {
            id: session,
            agent_id: agent,
            origin: None,
            agent_def: None,
            role: None,
            created: Utc::now(),
            cwd: PathBuf::from("/tmp"),
            labels: vec![],
            ephemeral: false,
            ask_origin: None,
            root: None,
            plugin_config: PluginConfig::default(),
        })
        .await
        .expect("fresh session id never collides");
}

/// An unwired dispatcher (no runner installed) is a byte-for-byte no-op,
/// matching every other observation-tier default -- the right default for
/// every test in this file that is not itself about `child_reported`.
fn no_hooks() -> Arc<HookDispatcher> {
    Arc::new(HookDispatcher::new())
}

/// A `HookRunner` that records every event name it saw and always succeeds
/// -- the `child_reported` tests below need to know THAT it ran, not to
/// prove failure propagation (already proven for the observation tier by
/// `crates/conway-runtime/tests/hook_dispatch.rs`).
#[derive(Debug, Default)]
struct RecordingRunner {
    seen: Mutex<Vec<String>>,
}

impl RecordingRunner {
    fn count(&self, event: &str) -> usize {
        self.seen
            .lock()
            .expect("seen lock poisoned")
            .iter()
            .filter(|n| n.as_str() == event)
            .count()
    }
}

#[async_trait]
impl HookRunner for RecordingRunner {
    async fn run(&self, invocation: &HookInvocation) -> Result<HookAnswer, HookFailure> {
        self.seen
            .lock()
            .expect("seen lock poisoned")
            .push(invocation.event.name.clone());
        Ok(HookAnswer::default())
    }
}

fn child_reported_hooks(runner: Arc<RecordingRunner>) -> Arc<HookDispatcher> {
    let hooks = Arc::new(HookDispatcher::new());
    hooks.set_runner(Some(runner));
    hooks.set_hooks(BTreeMap::from([(
        CHILD_REPORTED.to_string(),
        vec![HookSpec {
            id: "watcher".to_string(),
            command: vec!["/bin/true".to_string()],
            timeout_ms: 1_000,
            matcher: None,
            origin: HookOrigin::Operator,
            spawn_only: false,
        }],
    )]));
    hooks
}

/// Polls (cooperatively yielding, never a fixed sleep) until `predicate` is
/// `true` or 2s elapse -- the `Synthesized` branch's `child_reported`
/// dispatch happens inside the SAME `tokio::spawn`'d supervising task that
/// publishes the result `await_result` resolves from, in program order
/// AFTER the publish (`supervisor.rs`'s own module doc) -- this removes any
/// dependence on exactly how the executor happens to interleave the two
/// tasks once `await_result` resolves.
async fn wait_until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("condition was not met within 2s");
}

/// Builds a minimal [`AgentNode`] for a test; `role`/`agent_def` are fixed
/// filler since no criterion in this file exercises them.
fn mk_node(
    id: AgentId,
    parent: Option<AgentId>,
    session: SessionId,
    budget: Budget,
    cancel: CancellationToken,
    kind: Option<SubagentMode>,
) -> AgentNode {
    AgentNode {
        id,
        parent,
        session,
        kind,
        agent_def: None,
        role: Some(RoleAlias::new("worker")),
        budget,
        cancel,
        inherited_upto: None,
        ephemeral: false,
    }
}

// ---------------------------------------------------------------------
// AgentTree: attach / snapshot / path
// ---------------------------------------------------------------------

#[tokio::test]
async fn attach_populates_snapshot_fields() {
    let tree = AgentTree::new(EventBus::new(16));
    let agent = AgentId::new();
    let session = SessionId::new();
    let budget = Budget {
        max_steps: 7,
        ..Budget::default()
    };

    tree.attach(mk_node(
        agent,
        None,
        session,
        budget.clone(),
        CancellationToken::new(),
        None,
    ))
    .unwrap();

    let snapshot = tree.snapshot();
    assert_eq!(snapshot.nodes.len(), 1);
    let node = &snapshot.nodes[0];
    assert_eq!(node.agent_id, agent);
    assert_eq!(node.session, session);
    assert_eq!(node.parent, None);
    assert_eq!(node.status, AgentStatus::Running);
    assert_eq!(node.steps_taken, 0);
    assert_eq!(node.budget, budget);
    assert_eq!(snapshot.root, agent);
}

#[tokio::test]
async fn attach_unknown_parent_errors_agent_not_found() {
    let tree = AgentTree::new(EventBus::new(16));
    let agent = AgentId::new();
    let unknown_parent = AgentId::new();

    let err = tree
        .attach(mk_node(
            agent,
            Some(unknown_parent),
            SessionId::new(),
            Budget::default(),
            CancellationToken::new(),
            None,
        ))
        .unwrap_err();

    assert!(matches!(err, RuntimeError::AgentNotFound { agent } if agent == unknown_parent));
}

#[tokio::test]
async fn attach_duplicate_id_errors() {
    let tree = AgentTree::new(EventBus::new(16));
    let agent = AgentId::new();
    let session = SessionId::new();

    tree.attach(mk_node(
        agent,
        None,
        session,
        Budget::default(),
        CancellationToken::new(),
        None,
    ))
    .unwrap();

    let err = tree
        .attach(mk_node(
            agent,
            None,
            session,
            Budget::default(),
            CancellationToken::new(),
            None,
        ))
        .unwrap_err();
    // No dedicated "duplicate agent" `RuntimeError` variant exists
    // (`tree.rs`'s `already_attached` doc comment) -- assert only that it
    // errors, not the exact variant.
    assert!(format!("{err}").contains(&agent.to_string()));
}

#[tokio::test]
async fn path_returns_root_to_agent_chain_and_empty_for_unknown() {
    let tree = AgentTree::new(EventBus::new(16));
    let root = AgentId::new();
    let mid = AgentId::new();
    let leaf = AgentId::new();
    let session = SessionId::new();

    tree.attach(mk_node(
        root,
        None,
        session,
        Budget::default(),
        CancellationToken::new(),
        None,
    ))
    .unwrap();
    tree.attach(mk_node(
        mid,
        Some(root),
        session,
        Budget::default(),
        CancellationToken::new(),
        Some(SubagentMode::Fork),
    ))
    .unwrap();
    tree.attach(mk_node(
        leaf,
        Some(mid),
        session,
        Budget::default(),
        CancellationToken::new(),
        Some(SubagentMode::Fork),
    ))
    .unwrap();

    assert_eq!(tree.path(leaf), vec![root, mid, leaf]);
    assert_eq!(tree.path(root), vec![root]);
    assert!(tree.path(AgentId::new()).is_empty());
}

// ---------------------------------------------------------------------
// publish_result: set-once
// ---------------------------------------------------------------------

#[tokio::test]
async fn publish_result_is_set_once() {
    let tree = AgentTree::new(EventBus::new(16));
    let agent = AgentId::new();
    let session = SessionId::new();
    tree.attach(mk_node(
        agent,
        None,
        session,
        Budget::default(),
        CancellationToken::new(),
        None,
    ))
    .unwrap();

    let first = AgentResult::new(agent, session, ResultStatus::Completed, "first");
    let second = AgentResult::new(
        agent,
        session,
        ResultStatus::Failed {
            error: "x".to_string(),
        },
        "second",
    );

    assert!(tree.publish_result(agent, first.clone()).unwrap());
    assert!(!tree.publish_result(agent, second).unwrap());

    let observed = tree.await_result(agent).await.unwrap();
    assert_eq!(observed, first);
}

#[tokio::test]
async fn publish_result_unknown_agent_errors() {
    let tree = AgentTree::new(EventBus::new(16));
    let agent = AgentId::new();
    let err = tree
        .publish_result(
            agent,
            AgentResult::new(agent, SessionId::new(), ResultStatus::Completed, ""),
        )
        .unwrap_err();
    assert!(matches!(err, RuntimeError::AgentNotFound { .. }));
}

// ---------------------------------------------------------------------
// Supervisor: panic containment, budget synthesis, hard cancel
// ---------------------------------------------------------------------

#[tokio::test]
async fn panic_in_task_resolves_failed_mentioning_panic_within_1s() {
    let bus = EventBus::new(64);
    let tree = Arc::new(AgentTree::new(bus.clone()));
    let agent = AgentId::new();
    let session = SessionId::new();
    let cancel = CancellationToken::new();

    tree.attach(mk_node(
        agent,
        None,
        session,
        Budget::default(),
        cancel.clone(),
        None,
    ))
    .unwrap();

    let task: JoinHandle<AgentResult> = tokio::spawn(async { panic!("boom") });
    supervisor::supervise(SuperviseArgs {
        tree: tree.clone(),
        bus: bus.clone(),
        agent,
        session,
        cancel,
        deadline: None,
        grace: Duration::from_millis(50),
        task,
        hooks: no_hooks(),
        parent: None,
        store: fake_store(),
    });

    let result = tokio::time::timeout(Duration::from_secs(1), tree.await_result(agent))
        .await
        .expect("await_result did not resolve within 1s")
        .expect("await_result errored");

    match &result.status {
        ResultStatus::Failed { error } => assert!(
            error.contains("panic"),
            "expected the Failed error to mention the panic, got {error:?}"
        ),
        other => panic!("expected Failed, got {other:?}"),
    }
    // Exactly one terminal result was ever published; the underlying task
    // (already finished) cannot race a second publish here.
    assert!(!tree.publish_result(agent, result).unwrap());
}

/// Board item A5.3: the durable-persistence half of the panic-containment
/// guarantee above. `panic_in_task_resolves_failed_mentioning_panic_within_1s`
/// only proves the LIVE `AgentTree` sees a real `Failed` result -- it says
/// nothing about the session's own persisted log, which (before this item)
/// a `Synthesized` branch never wrote to at all: only `AgentLoop::finish`
/// (a method the panicking task never reaches) ever appended a terminal
/// record. This test seeds a REAL session in a real `SessionStore`, drives
/// the identical panic through `supervise`, then reads the log back and
/// asserts on the observable outcome this item's own acceptance criteria
/// name: the LAST RECORD in the log, not an intermediate signal. A store
/// that never persisted anything would leave `read` returning zero records
/// for this session; a store that persisted the wrong thing (a stale
/// earlier record, or a status this panic never produced) fails the field
/// assertions below -- neither failure mode is masked by only checking
/// "`await_result` returned without erroring".
///
/// Shown to fail: before this item's `supervisor.rs` change (no
/// `persist_agent_result` call on the `Synthesized` branch), `store.read`
/// below returns an empty `Vec`, and `records.last()` panics with "a
/// synthesized panic must leave at least one persisted record" -- confirmed
/// by temporarily reverting that one call during development.
#[tokio::test]
async fn a_synthesized_panic_persists_a_terminal_agent_result_as_the_logs_last_record() {
    let bus = EventBus::new(64);
    let tree = Arc::new(AgentTree::new(bus.clone()));
    let agent = AgentId::new();
    let session = SessionId::new();
    let cancel = CancellationToken::new();
    let store = fake_store();

    seed_session(store.as_ref(), agent, session).await;
    tree.attach(mk_node(
        agent,
        None,
        session,
        Budget::default(),
        cancel.clone(),
        None,
    ))
    .unwrap();

    let task: JoinHandle<AgentResult> = tokio::spawn(async { panic!("boom") });
    supervisor::supervise(SuperviseArgs {
        tree: tree.clone(),
        bus: bus.clone(),
        agent,
        session,
        cancel,
        deadline: None,
        grace: Duration::from_millis(50),
        task,
        hooks: no_hooks(),
        parent: None,
        store: store.clone(),
    });

    // Wait for the LIVE result first (the supervisor's own documented
    // guarantee): `supervise`'s `Synthesized` branch persists BEFORE it
    // publishes to the tree (`supervisor.rs`'s own ordering), so by the
    // time `await_result` resolves the durable write has already happened
    // and this read below is not racing it.
    let live_result = tokio::time::timeout(Duration::from_secs(1), tree.await_result(agent))
        .await
        .expect("await_result did not resolve within 1s")
        .expect("await_result errored");

    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .expect("session was seeded, read must succeed");
    let last = records
        .last()
        .expect("a synthesized panic must leave at least one persisted record");
    match last {
        conway_core::log::LogRecord::AgentResultRecord { result, .. } => {
            assert_eq!(
                result.status, live_result.status,
                "the persisted terminal record must match what a live awaiter saw"
            );
            match &result.status {
                ResultStatus::Failed { error } => assert!(
                    error.contains("panic"),
                    "persisted Failed error must mention the panic, got {error:?}"
                ),
                other => panic!("expected a persisted Failed status, got {other:?}"),
            }
        }
        other => {
            panic!("the log's LAST record must be the terminal AgentResultRecord, got {other:?}")
        }
    }
}

/// Board item A5.3, sibling of the test above: the "double writer" hazard
/// this item's spec explicitly warns about ("a second writer drifts and
/// silently loses a field") would show up here as TWO
/// `AgentResultRecord`s in one session's log -- one from the supervisor's
/// synthesis, one from the task's own (losing) `AgentLoop::finish` call, if
/// both persisted unconditionally. This file's mock tasks cannot reach a
/// real `AgentLoop::finish` (module doc: "not a real `AgentLoop`"), so this
/// test instead proves the NARROWER, still load-bearing property fully
/// within this module's own reach: a `Synthesized` branch that LOSES the
/// `publish_result` CAS (because something else already published first --
/// standing in for the real race's winning side) must not persist at all.
/// Combined with `AgentLoop::finish`'s own identical `is_first` gate (see
/// `agent_loop.rs`'s `finish`, which this test cannot exercise directly),
/// this is the other half of "at most one `AgentResultRecord` ever lands in
/// a session's log".
#[tokio::test]
async fn a_synthesized_result_that_loses_the_publish_race_does_not_persist() {
    let bus = EventBus::new(64);
    let tree = Arc::new(AgentTree::new(bus.clone()));
    let agent = AgentId::new();
    let session = SessionId::new();
    let cancel = CancellationToken::new();
    let store = fake_store();

    seed_session(store.as_ref(), agent, session).await;
    tree.attach(mk_node(
        agent,
        None,
        session,
        Budget::default(),
        cancel.clone(),
        None,
    ))
    .unwrap();

    // Wins the CAS before the supervisor's own synthesis ever gets a
    // chance to -- standing in for a real `AgentLoop::finish` call that
    // outraced its own `abort()` (see `supervisor.rs`'s "double-
    // AgentFinished race" module doc for the real interleaving this
    // stands in for).
    let already_published = AgentResult::new(agent, session, ResultStatus::Completed, "won");
    assert!(tree
        .publish_result(agent, already_published.clone())
        .unwrap());

    let task: JoinHandle<AgentResult> = tokio::spawn(async { panic!("boom") });
    supervisor::supervise(SuperviseArgs {
        tree: tree.clone(),
        bus: bus.clone(),
        agent,
        session,
        cancel,
        deadline: None,
        grace: Duration::from_millis(50),
        task,
        hooks: no_hooks(),
        parent: None,
        store: store.clone(),
    });

    // The supervisor still resolves `await_result` (the live result is
    // whatever already won, unaffected by the losing synthesis) --
    // `panic_in_task_resolves_failed_mentioning_panic_within_1s` already
    // proves liveness for the winning case; this test's own assertion is
    // about the LOG, below.
    let live_result = tokio::time::timeout(Duration::from_secs(1), tree.await_result(agent))
        .await
        .expect("await_result did not resolve within 1s")
        .expect("await_result errored");
    assert_eq!(live_result.status, ResultStatus::Completed);

    // Give the losing `Synthesized` branch a moment to run (it still
    // completes its own `tokio::spawn`'d task fully, it just must not
    // persist) before asserting the log stayed empty.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .expect("session was seeded, read must succeed");
    assert!(
        records.is_empty(),
        "a Synthesized branch that LOST the publish race must not persist its own \
         result -- got {} record(s): {records:?}",
        records.len()
    );
}

// ---------------------------------------------------------------------
// `child_reported` on the `Synthesized` branch
// ---------------------------------------------------------------------

/// ACCEPTANCE: "`child_reported` fires ... for one that is cancelled or
/// panics, since the supervisor synthesizes a terminal result in those
/// cases." A caught panic is exactly the `Outcome::Synthesized` case this
/// module's own doc names -- the task never reaches `AgentLoop::finish`'s
/// OWN dispatch, so this module's `won`-gated dispatch is the ONLY place
/// this terminal result is ever reported. Shown to fail by removing the
/// dispatch call from the `Synthesized` branch in `supervisor.rs`.
#[tokio::test]
async fn synthesized_panic_dispatches_child_reported_when_a_parent_exists() {
    let bus = EventBus::new(64);
    let tree = Arc::new(AgentTree::new(bus.clone()));
    let parent = AgentId::new();
    let agent = AgentId::new();
    let session = SessionId::new();

    tree.attach(mk_node(
        parent,
        None,
        session,
        Budget::default(),
        CancellationToken::new(),
        None,
    ))
    .unwrap();
    tree.attach(mk_node(
        agent,
        Some(parent),
        session,
        Budget::default(),
        CancellationToken::new(),
        Some(SubagentMode::Fork),
    ))
    .unwrap();

    let runner = Arc::new(RecordingRunner::default());
    let hooks = child_reported_hooks(runner.clone());

    let task: JoinHandle<AgentResult> = tokio::spawn(async { panic!("boom") });
    supervisor::supervise(SuperviseArgs {
        tree: tree.clone(),
        bus: bus.clone(),
        agent,
        session,
        cancel: CancellationToken::new(),
        deadline: None,
        grace: Duration::from_millis(50),
        task,
        hooks,
        parent: Some(parent),
        store: fake_store(),
    });

    tokio::time::timeout(Duration::from_secs(1), tree.await_result(agent))
        .await
        .expect("await_result did not resolve within 1s")
        .expect("await_result errored");
    wait_until(|| runner.count(CHILD_REPORTED) >= 1).await;

    assert_eq!(
        runner.count(CHILD_REPORTED),
        1,
        "child_reported must fire exactly once for a synthesized (panicked) result"
    );
}

/// Sibling of the test above: a synthesized result for an agent with NO
/// parent (`parent: None`, the shape `Runtime::start_root`'s own
/// `SuperviseArgs` construction always passes) never dispatches
/// `child_reported` -- a root has no parent for a result to cross back to.
#[tokio::test]
async fn synthesized_panic_does_not_dispatch_child_reported_with_no_parent() {
    let bus = EventBus::new(64);
    let tree = Arc::new(AgentTree::new(bus.clone()));
    let agent = AgentId::new();
    let session = SessionId::new();

    tree.attach(mk_node(
        agent,
        None,
        session,
        Budget::default(),
        CancellationToken::new(),
        None,
    ))
    .unwrap();

    let runner = Arc::new(RecordingRunner::default());
    let hooks = child_reported_hooks(runner.clone());

    let task: JoinHandle<AgentResult> = tokio::spawn(async { panic!("boom") });
    supervisor::supervise(SuperviseArgs {
        tree: tree.clone(),
        bus: bus.clone(),
        agent,
        session,
        cancel: CancellationToken::new(),
        deadline: None,
        grace: Duration::from_millis(50),
        task,
        hooks,
        parent: None,
        store: fake_store(),
    });

    tokio::time::timeout(Duration::from_secs(1), tree.await_result(agent))
        .await
        .expect("await_result did not resolve within 1s")
        .expect("await_result errored");

    assert_eq!(
        runner.count(CHILD_REPORTED),
        0,
        "a root (no parent) must never dispatch child_reported"
    );
}

#[tokio::test]
async fn deadline_elapsed_while_blocked_resolves_budget_exceeded() {
    let bus = EventBus::new(64);
    let tree = Arc::new(AgentTree::new(bus.clone()));
    let agent = AgentId::new();
    let session = SessionId::new();
    let cancel = CancellationToken::new();

    tree.attach(mk_node(
        agent,
        None,
        session,
        Budget::default(),
        cancel.clone(),
        None,
    ))
    .unwrap();

    // Simulates being blocked in a tool call: never completes, ignores
    // cancellation entirely.
    let task: JoinHandle<AgentResult> = tokio::spawn(std::future::pending::<AgentResult>());
    let deadline = Utc::now() + chrono::Duration::milliseconds(30);

    supervisor::supervise(SuperviseArgs {
        tree: tree.clone(),
        bus: bus.clone(),
        agent,
        session,
        cancel,
        deadline: Some(deadline),
        grace: Duration::from_millis(50),
        task,
        hooks: no_hooks(),
        parent: None,
        store: fake_store(),
    });

    let result = tokio::time::timeout(Duration::from_secs(2), tree.await_result(agent))
        .await
        .expect("await_result did not resolve")
        .expect("await_result errored");
    assert!(matches!(result.status, ResultStatus::BudgetExceeded { .. }));
}

/// Board item A5.6, "when a child is terminated by a budget while a tool
/// call is IN FLIGHT, the synthesized result names the interrupted call".
///
/// **Shown failing against the pre-fix code first** (see this test's own
/// completion notes) -- before A5.6, `supervisor::budget_exceeded` took no
/// `in_flight` argument at all and its `detail` was the bare literal
/// `"budget exceeded: deadline elapsed"`, with no way to name what was
/// running. This test seeds `AgentTree::mark_tools_started` -- the exact
/// call `agent_loop.rs` makes immediately before dispatching a real tool
/// batch -- so the mock "blocked forever" task here stands in for a real
/// `AgentLoop` that is genuinely still awaiting `ToolRunner::run_batch` when
/// the deadline elapses and `grace` runs out with no response, forcing this
/// module to `abort()` the task and synthesize a result with no `AgentLoop`
/// left alive to ask.
#[tokio::test]
async fn a_budget_deadline_synthesis_names_the_tool_call_that_was_in_flight() {
    let bus = EventBus::new(64);
    let tree = Arc::new(AgentTree::new(bus.clone()));
    let agent = AgentId::new();
    let session = SessionId::new();
    let cancel = CancellationToken::new();

    tree.attach(mk_node(
        agent,
        None,
        session,
        Budget::default(),
        cancel.clone(),
        None,
    ))
    .unwrap();

    // What a real `AgentLoop` would have recorded right before dispatching
    // `ToolRunner::run_batch` for a slow `bash` call -- see
    // `AgentTree::mark_tools_started`'s own doc.
    tree.mark_tools_started(
        agent,
        vec![conway_runtime::tree::InFlightCall::new(
            conway_core::ids::ToolName::new("bash"),
            &serde_json::json!({"command": "cargo test --workspace"}),
        )],
    );

    // Simulates being blocked in that tool call: never completes, ignores
    // cancellation entirely -- forcing this module past `grace` into its
    // own synthesis, exactly like `deadline_elapsed_while_blocked_resolves_
    // budget_exceeded` immediately above.
    let task: JoinHandle<AgentResult> = tokio::spawn(std::future::pending::<AgentResult>());
    let deadline = Utc::now() + chrono::Duration::milliseconds(30);

    supervisor::supervise(SuperviseArgs {
        tree: tree.clone(),
        bus: bus.clone(),
        agent,
        session,
        cancel,
        deadline: Some(deadline),
        grace: Duration::from_millis(50),
        task,
        hooks: no_hooks(),
        parent: None,
        store: fake_store(),
    });

    let result = tokio::time::timeout(Duration::from_secs(2), tree.await_result(agent))
        .await
        .expect("await_result did not resolve")
        .expect("await_result errored");
    assert!(matches!(result.status, ResultStatus::BudgetExceeded { .. }));
    assert!(
        result.summary.contains("bash"),
        "terminal summary must name the interrupted tool, got: {:?}",
        result.summary
    );
    assert!(
        result.summary.contains("cargo test"),
        "terminal summary must carry a short args summary, got: {:?}",
        result.summary
    );
}

#[tokio::test]
async fn hard_cancel_resolves_cancelled() {
    let bus = EventBus::new(64);
    let tree = Arc::new(AgentTree::new(bus.clone()));
    let agent = AgentId::new();
    let session = SessionId::new();
    let cancel = CancellationToken::new();

    tree.attach(mk_node(
        agent,
        None,
        session,
        Budget::default(),
        cancel.clone(),
        None,
    ))
    .unwrap();

    // Never completes on its own -- only an external cancel can end it.
    let task: JoinHandle<AgentResult> = tokio::spawn(std::future::pending::<AgentResult>());
    supervisor::supervise(SuperviseArgs {
        tree: tree.clone(),
        bus: bus.clone(),
        agent,
        session,
        cancel: cancel.clone(),
        deadline: None,
        grace: Duration::from_millis(50),
        task,
        hooks: no_hooks(),
        parent: None,
        store: fake_store(),
    });

    tokio::time::sleep(Duration::from_millis(10)).await;
    tree.cancel(agent, "hard stop requested".to_string())
        .unwrap();

    let result = tokio::time::timeout(Duration::from_secs(1), tree.await_result(agent))
        .await
        .expect("await_result did not resolve")
        .expect("await_result errored");
    assert!(matches!(result.status, ResultStatus::Cancelled { .. }));
}

#[tokio::test]
async fn cancelling_parent_cancels_entire_subtree_and_every_descendant_terminates() {
    let bus = EventBus::new(64);
    let tree = Arc::new(AgentTree::new(bus.clone()));

    let root = AgentId::new();
    let root_session = SessionId::new();
    let root_cancel = CancellationToken::new();
    tree.attach(mk_node(
        root,
        None,
        root_session,
        Budget::default(),
        root_cancel.clone(),
        None,
    ))
    .unwrap();

    let child = AgentId::new();
    let child_session = SessionId::new();
    let child_cancel = root_cancel.child_token();
    tree.attach(mk_node(
        child,
        Some(root),
        child_session,
        Budget::default(),
        child_cancel.clone(),
        Some(SubagentMode::Fork),
    ))
    .unwrap();

    let grandchild = AgentId::new();
    let grandchild_session = SessionId::new();
    let grandchild_cancel = child_cancel.child_token();
    tree.attach(mk_node(
        grandchild,
        Some(child),
        grandchild_session,
        Budget::default(),
        grandchild_cancel.clone(),
        Some(SubagentMode::Fork),
    ))
    .unwrap();

    let child_task: JoinHandle<AgentResult> = tokio::spawn(std::future::pending::<AgentResult>());
    let grandchild_task: JoinHandle<AgentResult> =
        tokio::spawn(std::future::pending::<AgentResult>());

    supervisor::supervise(SuperviseArgs {
        tree: tree.clone(),
        bus: bus.clone(),
        agent: child,
        session: child_session,
        cancel: child_cancel,
        deadline: None,
        grace: Duration::from_millis(50),
        task: child_task,
        hooks: no_hooks(),
        parent: None,
        store: fake_store(),
    });
    supervisor::supervise(SuperviseArgs {
        tree: tree.clone(),
        bus: bus.clone(),
        agent: grandchild,
        session: grandchild_session,
        cancel: grandchild_cancel,
        deadline: None,
        grace: Duration::from_millis(50),
        task: grandchild_task,
        hooks: no_hooks(),
        parent: None,
        store: fake_store(),
    });

    tree.cancel(root, "shutdown".to_string()).unwrap();

    let child_result = tokio::time::timeout(Duration::from_secs(1), tree.await_result(child))
        .await
        .expect("child did not resolve")
        .unwrap();
    let grandchild_result =
        tokio::time::timeout(Duration::from_secs(1), tree.await_result(grandchild))
            .await
            .expect("grandchild did not resolve")
            .unwrap();

    assert!(matches!(
        child_result.status,
        ResultStatus::Cancelled { .. }
    ));
    assert!(matches!(
        grandchild_result.status,
        ResultStatus::Cancelled { .. }
    ));
}

// ---------------------------------------------------------------------
// Event ordering invariant: AgentSpawned precedes everything, exactly one
// AgentFinished follows.
// ---------------------------------------------------------------------

/// Asserts the architecture §8 per-agent lifecycle invariant: the first
/// envelope bearing `agent` is `AgentSpawned`, and exactly one
/// `AgentFinished` appears among `events`. Written to be reusable verbatim
/// by later multi-agent test suites, which is why it takes a
/// plain envelope slice rather than anything specific to this file's
/// fixtures.
fn assert_agent_lifecycle_invariants(events: &[Envelope], agent: AgentId) {
    let mine: Vec<&Event> = events
        .iter()
        .filter(|e| e.agent == agent)
        .map(|e| &e.event)
        .collect();
    assert!(!mine.is_empty(), "no events observed for agent {agent}");
    assert!(
        matches!(mine[0], Event::AgentSpawned { .. }),
        "first event for agent {agent} must be AgentSpawned, got {:?}",
        mine[0]
    );
    let finished_count = mine
        .iter()
        .filter(|e| matches!(e, Event::AgentFinished { .. }))
        .count();
    assert_eq!(
        finished_count, 1,
        "expected exactly one AgentFinished for agent {agent}, got {finished_count}"
    );
}

#[tokio::test]
async fn agent_spawned_precedes_and_exactly_one_agent_finished_follows() {
    let bus = EventBus::new(64);
    let tree = Arc::new(AgentTree::new(bus.clone()));
    let mut stream = bus.subscribe();

    let parent = AgentId::new();
    let parent_session = SessionId::new();
    tree.attach(mk_node(
        parent,
        None,
        parent_session,
        Budget::default(),
        CancellationToken::new(),
        None,
    ))
    .unwrap();

    let agent = AgentId::new();
    let session = SessionId::new();
    let cancel = CancellationToken::new();
    // `kind: Some(..)` -- a simulated subagent, so `attach` emits
    // `AgentSpawned` (a root would not; see `tree.rs`'s module doc).
    tree.attach(mk_node(
        agent,
        Some(parent),
        session,
        Budget::default(),
        cancel.clone(),
        Some(SubagentMode::Fork),
    ))
    .unwrap();

    // Stands in for a real `AgentLoop::finish`, which always emits its own
    // `Event::AgentFinished` before returning -- the supervisor must not
    // double-fire it when the task resolves normally like this.
    let expected = AgentResult::new(agent, session, ResultStatus::Completed, "done");
    let bus_for_task = bus.clone();
    let expected_for_task = expected.clone();
    let task: JoinHandle<AgentResult> = tokio::spawn(async move {
        bus_for_task.emit(
            session,
            agent,
            Event::AgentFinished {
                result: expected_for_task.clone(),
                ephemeral: false,
            },
        );
        expected_for_task
    });

    supervisor::supervise(SuperviseArgs {
        tree: tree.clone(),
        bus: bus.clone(),
        agent,
        session,
        cancel,
        deadline: None,
        grace: Duration::from_millis(50),
        task,
        hooks: no_hooks(),
        parent: None,
        store: fake_store(),
    });

    let mut collected = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let envelope = stream.next().await.expect("stream ended early");
            let is_target = envelope.agent == agent;
            let is_finished = matches!(envelope.event, Event::AgentFinished { .. });
            collected.push(envelope);
            if is_target && is_finished {
                break;
            }
        }
    })
    .await
    .expect("agent never finished");

    assert_agent_lifecycle_invariants(&collected, agent);
}

// ---------------------------------------------------------------------
// The double-AgentFinished race: a task racing to
// publish its own result concurrently with the supervisor's own
// grace-timeout synthesis.
// ---------------------------------------------------------------------

/// An earlier review found: finding S1: before this fix, `supervise`'s
/// `Outcome::Synthesized` branch emitted `Event::AgentFinished`
/// unconditionally, without checking whether it had actually won
/// `AgentTree::publish_result`'s CAS. `task.abort()` (used on the
/// grace-timeout path) is only a cooperative *request*: a task doing
/// non-yielding work when `abort()` lands keeps running until its next real
/// `.await` point, so it can still reach its own terminal machinery and win
/// the CAS after the supervisor has already given up on joining it and
/// moved on to its own synthesis.
///
/// The EXACT interleaving that produces this race -- the task's blocking
/// window ending at the same instant the supervisor's synthesis calls
/// `publish_result` -- is not something this test can force
/// deterministically: there is no hook that lets a test observe or control
/// the precise moment `task.abort()`'s request lands relative to the
/// supervisor's own `publish_result` call, since both run on tokio's own
/// scheduler. Instead, this drives the realistic shape (a task that blocks
/// synchronously -- via `std::thread::sleep`, which `abort()` genuinely
/// cannot interrupt -- past `grace`, then races to publish its own result
/// exactly like `AgentLoop::finish` does) across a spread of blocking
/// durations straddling the grace boundary, so that across trials the race
/// is actually landed on both sides at least sometimes. What it asserts is
/// the invariant that must hold on EVERY trial regardless of which side
/// happens to win: at most one `Event::AgentFinished` is ever observable
/// for the agent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_task_completion_and_grace_synthesis_never_double_emit_agent_finished() {
    let grace = Duration::from_millis(20);

    for trial in 0..10u64 {
        let bus = EventBus::new(64);
        let tree = Arc::new(AgentTree::new(bus.clone()));
        let agent = AgentId::new();
        let session = SessionId::new();
        let cancel = CancellationToken::new();

        tree.attach(mk_node(
            agent,
            None,
            session,
            Budget::default(),
            cancel.clone(),
            None,
        ))
        .unwrap();

        let mut stream = bus.subscribe();

        // Spans 5..=41ms against a 20ms grace: the low trials should
        // usually have the task win (it finishes before the supervisor
        // even times out), the high trials should usually have the
        // supervisor win (it synthesizes well before the task is done),
        // and the middle trials straddle the actual race window.
        let block_ms = 5 + trial * 4;

        let tree_for_task = tree.clone();
        let bus_for_task = bus.clone();
        let task: JoinHandle<AgentResult> = tokio::spawn(async move {
            // Non-yielding (blocking) work: `task.abort()` cannot take
            // effect until this call returns and the task reaches its next
            // real `.await` point -- exactly why `supervise`'s `abort()` is
            // only ever a request, not a guarantee.
            std::thread::sleep(Duration::from_millis(block_ms));
            let result = AgentResult::new(agent, session, ResultStatus::Completed, "raced");
            // Mirrors `AgentLoop::finish`: publish first, emit only if this
            // call is the one that actually won.
            if tree_for_task
                .publish_result(agent, result.clone())
                .unwrap_or(true)
            {
                bus_for_task.emit(
                    session,
                    agent,
                    Event::AgentFinished {
                        result: result.clone(),
                        ephemeral: false,
                    },
                );
            }
            result
        });

        supervisor::supervise(SuperviseArgs {
            tree: tree.clone(),
            bus: bus.clone(),
            agent,
            session,
            cancel: cancel.clone(),
            deadline: None,
            grace,
            task,
            hooks: no_hooks(),
            parent: None,
            store: fake_store(),
        });
        // Trips the supervisor's cancel-arm almost immediately, so its
        // grace window starts well before most trials' blocking work ends.
        tokio::time::sleep(Duration::from_millis(1)).await;
        cancel.cancel();

        // `await_result` always resolves -- the supervisor's core
        // guarantee -- regardless of which side wins. A little extra slack
        // afterward lets the losing side's (harmless, no-op) publish
        // attempt actually run before this trial counts events.
        tokio::time::timeout(Duration::from_secs(2), tree.await_result(agent))
            .await
            .expect("await_result did not resolve")
            .expect("await_result errored");
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut finished_count = 0;
        while let Some(Some(envelope)) = stream.next().now_or_never() {
            if envelope.agent == agent && matches!(envelope.event, Event::AgentFinished { .. }) {
                finished_count += 1;
            }
        }
        assert_eq!(
            finished_count, 1,
            "trial {trial} (block_ms={block_ms}): expected exactly one AgentFinished, got {finished_count}"
        );
    }
}
