//! Acceptance tests for `AgentTree` and the supervisor (WI-083, architecture
//! §7): attachment/structural lookups, cancellation propagation, and the
//! guarantee that `await_result` always terminates -- panic containment,
//! budget-deadline synthesis, and hard cancellation.
//!
//! These tests exercise `AgentTree`/`supervisor::supervise` directly against
//! bare mock tasks (not a real `AgentLoop` -- `agent_loop.rs`/`subagent.rs`
//! wiring is WI-084's job) so the supervision guarantee itself is proven
//! independent of any particular agent implementation.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use conway_core::agent::{AgentResult, AgentStatus, Budget, ResultStatus, SubagentMode};
use conway_core::error::RuntimeError;
use conway_core::event::{Envelope, Event};
use conway_core::ids::{AgentId, RoleAlias, SessionId};
use conway_runtime::events::EventBus;
use conway_runtime::supervisor::{self, SuperviseArgs};
use conway_runtime::tree::{AgentNode, AgentTree};
use futures::future::FutureExt;
use futures::StreamExt;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

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
    });

    let result = tokio::time::timeout(Duration::from_secs(2), tree.await_result(agent))
        .await
        .expect("await_result did not resolve")
        .expect("await_result errored");
    assert!(matches!(result.status, ResultStatus::BudgetExceeded { .. }));
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
/// by later multi-agent test suites (WI-084/085), which is why it takes a
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
// The double-AgentFinished race (cycle-2 review F-085 S1): a task racing to
// publish its own result concurrently with the supervisor's own
// grace-timeout synthesis.
// ---------------------------------------------------------------------

/// Cycle-2 review finding S1: before this fix, `supervise`'s
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
