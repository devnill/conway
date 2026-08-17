//! Acceptance tests for mailboxes and steering (architecture §6):
//! bounded inbox, turn-boundary drain, overflow policy, both steering
//! directions, and the/ double-`AgentFinished` fix.
//!
//! Built entirely from `conway-testkit`'s fakes (mirroring `agent_loop_e2e.rs`'s
//! and `runtime_api.rs`'s own harness style) -- this file does not depend on
//! `conway-plugin-backends` or `conway-tools`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use conway_core::agent::{AgentMessage, AgentResult, Budget, PermissionDecision, ResultStatus};
use conway_core::capabilities::{
    CacheMode, Capabilities, HeadroomPolicy, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{
    ContentBlock, PermissionClass, Role, StopReason, ToolCall, ToolCategory, ToolSpec, Usage,
};
use conway_core::error::ToolError;
use conway_core::event::Event;
use conway_core::ids::{
    AgentId, BackendId, LogSeq, ModelId, ModelRef, RoleAlias, SessionId, ToolName,
};
use conway_core::log::{LogRecord, SessionMeta};
use conway_core::ports::{
    Backend, HealthRegistry, PermissionGate, Plugin, PluginConfig, PluginManifest, Router,
    SessionStore, SubagentHost, Tool, ToolCtx, ToolOutput,
};
use conway_core::provenance::Provenance;
use conway_runtime::agent_loop::{AgentLoop, AgentSpec, LoopDeps};
use conway_runtime::attempt::AttemptEngine;
use conway_runtime::context::ContextBuilder;
use conway_runtime::events::EventBus;
use conway_runtime::mailbox::{self, Mailbox, MailboxSender};
use conway_runtime::permission::PermissionBroker;
use conway_runtime::tools::{PluginRegistry, ToolRunner};
use conway_runtime::tree::{AgentNode, AgentTree};
use conway_testkit::{FakeGate, FakeHealth, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use futures::future::FutureExt;
use futures::stream::StreamExt;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------
// Fixtures (mirrors `agent_loop_e2e.rs`'s own fixtures)
// ---------------------------------------------------------------------

fn caps_ok() -> Capabilities {
    Capabilities {
        tool_calling: ToolCallSupport::Streaming { validated: true },
        cache: CacheMode::None,
        parallel_tool_calls: true,
        structured_output: StructuredOutput::None,
        max_context_tokens: 1_000_000,
        reasoning: false,
        reliability_tier: ReliabilityTier::Verified,
    }
}

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

fn tool_call_response(call_id: &str, tool: &str) -> conway_core::ports::GenerateResponse {
    conway_core::ports::GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: ToolName::new(tool),
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

fn schema_any_object() -> schemars::schema::RootSchema {
    serde_json::from_value(serde_json::json!({"type": "object"})).unwrap()
}

fn tool_spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: ToolName::new(name),
        description: "test tool".into(),
        schema: schema_any_object(),
        category: ToolCategory::Read,
        permission: PermissionClass::Safe,
    }
}

fn text_output(text: impl Into<String>) -> ToolOutput {
    ToolOutput {
        blocks: vec![ContentBlock::Text { text: text.into() }],
        is_error: false,
        truncation: conway_core::content::TruncationPolicy::None,
        artifacts: vec![],
    }
}

/// Sleeps for `delay` before returning fixed text -- gives a test a window
/// in which to send a mailbox message while the agent is mid-tool-call
/// (mirrors `agent_loop_e2e.rs`'s own `DelayTool`).
struct DelayTool {
    name: ToolName,
    delay: Duration,
}

#[async_trait]
impl Tool for DelayTool {
    fn spec(&self) -> ToolSpec {
        tool_spec(self.name.as_str())
    }

    async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        tokio::time::sleep(self.delay).await;
        Ok(text_output("done"))
    }
}

struct FakePlugin {
    tools: Vec<Arc<dyn Tool>>,
}

impl Plugin for FakePlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "test".to_string(),
            version: "0.0.0".to_string(),
            tools: self.tools.iter().map(|t| t.spec().name).collect(),
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}

fn registry(tools: Vec<Arc<dyn Tool>>) -> Arc<PluginRegistry> {
    Arc::new(PluginRegistry::from_plugins(vec![Arc::new(FakePlugin { tools })]).unwrap())
}

async fn seed_prompt(store: &dyn SessionStore, agent: AgentId, session: SessionId, prompt: &str) {
    store
        .create(SessionMeta {
            id: session,
            agent_id: agent,
            origin: None,
            agent_def: None,
            role: Some(RoleAlias::new("planner")),
            created: Utc::now(),
            cwd: PathBuf::from("/tmp"),
            labels: vec![],
            ephemeral: false,
            ask_origin: None,
            root: None,
            plugin_config: conway_core::ports::PluginConfig::default(),
        })
        .await
        .unwrap();
    let seq = store.head(&session).await.unwrap();
    store
        .append(
            &session,
            LogRecord::UserTurn {
                seq,
                ts: Utc::now(),
                text: prompt.to_string(),
                prov: Provenance::UserPrompt,
            },
        )
        .await
        .unwrap();
}

/// Drains every envelope already synchronously buffered on `stream` (valid
/// because every `bus.emit` call in this crate is synchronous and has
/// already run to completion by the time the awaited future returns).
fn drain(stream: &mut conway_runtime::events::EventStream) -> Vec<Event> {
    let mut out = Vec::new();
    while let Some(Some(envelope)) = stream.next().now_or_never() {
        out.push(envelope.event);
    }
    out
}

/// Like [`drain`], but for use sites that may need to collect more than
/// Tokio's cooperative-scheduling poll budget (128) worth of envelopes in
/// one go -- a tight `now_or_never` spin never yields, so a task can hit
/// that budget and see a spurious `Pending` well before the stream is
/// actually empty. A short real `timeout` per item forces an actual
/// scheduler turn (resetting the budget) between polls.
async fn drain_many(stream: &mut conway_runtime::events::EventStream) -> Vec<Event> {
    let mut out = Vec::new();
    while let Ok(Some(envelope)) =
        tokio::time::timeout(Duration::from_millis(20), stream.next()).await
    {
        out.push(envelope.event);
    }
    out
}

struct Harness {
    agent_loop: AgentLoop,
    bus: Arc<EventBus>,
    mailbox_tx: MailboxSender,
}

/// Builds one `AgentLoop` harness wired with a real mailbox pair --
/// `harness.mailbox_tx` is this agent's own inbox sender, exposed so a test
/// can simulate steering/cancelling/progress-reporting/resolving it exactly
/// as a real caller (an embedder, a sibling, a child, `SubagentHost::steer`)
/// would.
fn build_loop(
    session: SessionId,
    agent: AgentId,
    store: Arc<dyn SessionStore>,
    backend: Arc<ScriptedBackend>,
    tools: Vec<Arc<dyn Tool>>,
    parent_mailbox: Option<MailboxSender>,
) -> Harness {
    let bus = EventBus::new(1024);
    let health: Arc<dyn HealthRegistry> = Arc::new(FakeHealth::new());
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend as Arc<dyn Backend>);
    let attempt = Arc::new(AttemptEngine::new(backends, health, bus.clone()));
    let plugin_registry = registry(tools);
    let gate: Arc<dyn PermissionGate> = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let broker = Arc::new(PermissionBroker::new(gate, bus.clone()));
    let tool_runner = Arc::new(ToolRunner::new(
        plugin_registry.clone(),
        broker,
        bus.clone(),
    ));
    let subagents: Arc<dyn SubagentHost> = Arc::new(conway_testkit::FakeSubagentHost::new(agent));
    let tree = Arc::new(AgentTree::new(bus.clone()));

    let deps = Arc::new(LoopDeps {
        store: store.clone(),
        router,
        attempt,
        registry: plugin_registry,
        tool_runner,
        subagents,
        plugin_config: Arc::new(PluginConfig::default()),
        bus: bus.clone(),
        builder: Arc::new(ContextBuilder::new()),
        headroom: Arc::new(HeadroomPolicy::default()),
        tree: tree.clone(),
        context_hook: std::sync::RwLock::new(None),
        observers: Vec::new(),
        plugin_events: Arc::new(conway_runtime::hook_dispatch::HookDispatcher::new()),
    });

    let spec = AgentSpec {
        system_prompt: None,
        skills: vec![],
        tools: None,
        role: RoleAlias::new("planner"),
        pin: None,
        budget: Budget::default(),
        cache_mode: CacheMode::None,
        cache_ttl: conway_core::segment::CacheTtl::FiveMinutes,
        headroom_override: None,
        max_parallel_tools: 4,
        report_slot: None,
        // not exercised by this file -- `tests/result_contract.rs`
        // owns result-contract coverage.
        result_contract: None,
        keep_alive: false,
        tag: None,
    };

    let cancel = CancellationToken::new();
    tree.attach(AgentNode {
        id: agent,
        parent: None,
        session,
        kind: None,
        agent_def: None,
        role: Some(RoleAlias::new("planner")),
        budget: Budget::default(),
        cancel: cancel.clone(),
        inherited_upto: None,
        ephemeral: false,
    })
    .expect("fresh tree attach never fails");

    let (mailbox_tx, mailbox_rx) = Mailbox::new(mailbox::RUNTIME_CAPACITY);
    let mailbox_tx = mailbox_tx.with_events(bus.clone(), session, agent, cancel.clone());

    let agent_loop = AgentLoop {
        agent_id: agent,
        session,
        parent: None,
        agent_path: vec![agent],
        cwd: PathBuf::from("/tmp"),
        root: None,
        plugin_config: Arc::new(PluginConfig::default()),
        deps,
        spec,
        cancel,
        inherited: None,
        inbox: mailbox_rx,
        parent_mailbox,
        pending_cancel: None,
        resume_gate: Default::default(),
    };

    Harness {
        agent_loop,
        bus,
        mailbox_tx: mailbox_tx.clone(),
    }
}

async fn wait_for_tool_call_started(stream: &mut conway_runtime::events::EventStream) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let envelope = stream.next().await.expect("stream open");
            if matches!(envelope.event, Event::ToolCallStarted { .. }) {
                break;
            }
        }
    })
    .await
    .expect("ToolCallStarted was never observed");
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

/// Criterion: `Mailbox::new(capacity: usize)` exists, with runtime capacity
/// 64.
#[test]
fn mailbox_new_has_the_runtime_capacity_of_64() {
    assert_eq!(mailbox::RUNTIME_CAPACITY, 64);
    let (_tx, _rx) = Mailbox::new(mailbox::RUNTIME_CAPACITY);
}

/// Criterion: sending 70 `Steer` messages into a 64-slot inbox never blocks
/// the sender; the 6 oldest are dropped and exactly 6 `Event::SteerDropped`
/// envelopes are emitted.
#[tokio::test]
async fn overflow_drops_the_six_oldest_and_emits_exactly_six_steer_dropped_without_blocking() {
    let bus = EventBus::new(1024);
    let mut stream = bus.subscribe();
    let target = AgentId::new();
    let (tx, mut rx) = Mailbox::new(64);
    let tx = tx.with_events(bus, SessionId::new(), target, CancellationToken::new());

    let start = std::time::Instant::now();
    for i in 0..70 {
        tx.send(AgentMessage::Steer {
            from: AgentId::new(),
            text: format!("steer {i}"),
            at_parent_seq: LogSeq::ZERO,
        });
    }
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "70 sends into a bounded mailbox must never block the sender"
    );

    let drained = rx.drain();
    assert_eq!(drained.len(), 64, "the newest 64 messages must survive");
    // The 6 oldest ("steer 0".."steer 5") were evicted; the newest 64
    // ("steer 6".."steer 69") remain, oldest-first.
    match &drained[0] {
        AgentMessage::Steer { text, .. } => assert_eq!(text, "steer 6"),
        other => panic!("expected a Steer, got {other:?}"),
    }

    let dropped = drain_many(&mut stream)
        .await
        .into_iter()
        .filter(|e| matches!(e, Event::SteerDropped { .. }))
        .count();
    assert_eq!(dropped, 6);
}

/// Criterion: `Event::SteerDropped` is emitted
/// only when the EVICTED entry was itself a `Steer` -- evicting a queued
/// `Result` or soft `Cancel` must never be misreported as a dropped
/// steer. A 4-slot inbox is filled exactly to capacity with
/// `[Steer, Result, Steer, Cancel]`, then four more sends each evict
/// exactly one oldest entry, in order: the two `Steer`s ("s1", "s2") and
/// the `Result`/`Cancel` in between and after -- so only 2 of the 4
/// evictions should ever produce `Event::SteerDropped`.
#[tokio::test]
async fn overflow_emits_steer_dropped_only_for_evicted_steers_not_other_kinds() {
    let bus = EventBus::new(64);
    let mut stream = bus.subscribe();
    let target = AgentId::new();
    let (tx, mut rx) = Mailbox::new(4);
    let tx = tx.with_events(bus, SessionId::new(), target, CancellationToken::new());

    let steer = |text: &str| AgentMessage::Steer {
        from: AgentId::new(),
        text: text.to_string(),
        at_parent_seq: LogSeq::ZERO,
    };
    let result = |summary: &str| AgentMessage::Result {
        from: AgentId::new(),
        result: AgentResult::new(
            AgentId::new(),
            SessionId::new(),
            ResultStatus::Completed,
            summary,
        ),
    };
    let soft_cancel = |reason: &str| AgentMessage::Cancel {
        from: AgentId::new(),
        reason: reason.to_string(),
        hard: false,
    };

    // Fills the 4-slot inbox exactly (no eviction yet).
    tx.send(steer("s1"));
    tx.send(result("r1"));
    tx.send(steer("s2"));
    tx.send(soft_cancel("c1"));

    // Each of these evicts exactly one oldest entry, in order:
    // s1 (Steer), r1 (Result), s2 (Steer), c1 (Cancel).
    tx.send(steer("s3"));
    tx.send(result("r2"));
    tx.send(steer("s4"));
    tx.send(soft_cancel("c2"));

    let drained = rx.drain();
    assert_eq!(drained.len(), 4, "the inbox stays bounded at its capacity");

    let events = drain(&mut stream);
    let steer_dropped = events
        .iter()
        .filter(|e| matches!(e, Event::SteerDropped { .. }))
        .count();
    assert_eq!(
        steer_dropped, 2,
        "exactly the two evicted Steers (s1, s2) must produce SteerDropped -- \
         the evicted Result and Cancel must not"
    );
}

/// Minor (M1): concurrent senders never corrupt the mailbox's overflow
/// accounting. Multiple tasks race `MailboxSender::send` (cloned) against
/// the same bounded inbox at once; the review that requested this test
/// believes it cannot fail under `send`'s current single-`Mutex` critical
/// section, so this is coverage for the record rather than a fix for an
/// observed bug. Every message sent here is a `Steer`, so the accounting
/// check is exact: `sent - capacity` evictions, each one `SteerDropped`.
#[tokio::test]
async fn concurrent_senders_never_corrupt_overflow_accounting() {
    let bus = EventBus::new(4096);
    let mut stream = bus.subscribe();
    let target = AgentId::new();
    let (tx, mut rx) = Mailbox::new(16);
    let tx = tx.with_events(bus, SessionId::new(), target, CancellationToken::new());

    const SENDERS: usize = 8;
    const PER_SENDER: usize = 20;
    let mut handles = Vec::new();
    for s in 0..SENDERS {
        let tx = tx.clone();
        handles.push(tokio::spawn(async move {
            for i in 0..PER_SENDER {
                tx.send(AgentMessage::Steer {
                    from: AgentId::new(),
                    text: format!("s{s}-{i}"),
                    at_parent_seq: LogSeq::ZERO,
                });
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    let drained = rx.drain();
    assert_eq!(
        drained.len(),
        16,
        "surviving entries must equal the inbox capacity regardless of send concurrency"
    );

    let total_sent = SENDERS * PER_SENDER;
    let expected_dropped = total_sent - 16;
    let dropped = drain_many(&mut stream)
        .await
        .into_iter()
        .filter(|e| matches!(e, Event::SteerDropped { .. }))
        .count();
    assert_eq!(
        dropped, expected_dropped,
        "every eviction beyond capacity must be reported exactly once, even under concurrent senders"
    );
}

/// Criterion: `Event::SteerQueued { target }` is emitted at enqueue time,
/// carrying a queue timestamp.
#[tokio::test]
async fn steer_queued_is_emitted_at_enqueue_time_with_a_timestamp() {
    let bus = EventBus::new(64);
    let mut stream = bus.subscribe();
    let target = AgentId::new();
    let (tx, _rx) = Mailbox::new(64);
    let tx = tx.with_events(bus, SessionId::new(), target, CancellationToken::new());

    let before = Utc::now();
    tx.send(AgentMessage::Steer {
        from: AgentId::new(),
        text: "hi".to_string(),
        at_parent_seq: LogSeq::ZERO,
    });
    let after = Utc::now();

    let queued = drain(&mut stream)
        .into_iter()
        .find_map(|e| match e {
            Event::SteerQueued {
                target: t,
                queued_since,
            } => Some((t, queued_since)),
            _ => None,
        })
        .expect("SteerQueued must be emitted");
    assert_eq!(queued.0, target);
    assert!(queued.1 >= before && queued.1 <= after);
}

/// Criteria: a steer sent mid-tool-call is absent from the in-flight turn's
/// context and is present as the first user-role segment of the next turn;
/// the drained steer is persisted as `LogRecord::ParentSteer` with
/// `Provenance::ParentSteer { from, parent_seq }`.
#[tokio::test]
async fn steer_lands_only_at_the_next_turn_boundary_as_a_parent_steer_segment() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let session = SessionId::new();
    let agent = AgentId::new();
    seed_prompt(&*store, agent, session, "go").await;

    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response("tc_1", "read")),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_capabilities(caps_ok()),
    );
    let tool: Arc<dyn Tool> = Arc::new(DelayTool {
        name: ToolName::new("read"),
        delay: Duration::from_millis(150),
    });

    let harness = build_loop(
        session,
        agent,
        store.clone(),
        backend.clone(),
        vec![tool],
        None,
    );
    let mailbox_tx = harness.mailbox_tx.clone();
    let bus = harness.bus.clone();
    let mut stream = bus.subscribe();

    let handle = tokio::spawn(harness.agent_loop.run());
    wait_for_tool_call_started(&mut stream).await;

    let steer_from = AgentId::new();
    mailbox_tx.send(AgentMessage::Steer {
        from: steer_from,
        text: "focus on the auth module".to_string(),
        at_parent_seq: LogSeq::ZERO,
    });

    let result = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("agent loop did not finish")
        .expect("agent loop task panicked");
    assert_eq!(result.status, ResultStatus::Completed);

    let calls = backend.calls();
    assert_eq!(calls.len(), 2, "expected exactly two turns");

    // Absent from the in-flight (first) turn.
    assert!(
        !calls[0]
            .segments
            .iter()
            .any(|s| matches!(s.provenance, Provenance::ParentSteer { .. })),
        "the steer must not appear in the turn that was already in flight when it was sent"
    );

    // Present in the second turn, as the first NEW (own/volatile) user-role
    // segment, with the correct provenance.
    let new_segments = &calls[1].segments[calls[0].segments.len()..];
    let first_new_user_segment = new_segments
        .iter()
        .find(|s| s.role == Role::User)
        .expect("the next turn must contain a new user-role segment");
    match &first_new_user_segment.provenance {
        Provenance::ParentSteer { from, .. } => assert_eq!(*from, steer_from),
        other => panic!("expected Provenance::ParentSteer, got {other:?}"),
    }
    assert!(first_new_user_segment.content.iter().any(
        |b| matches!(b, ContentBlock::Text { text } if text.contains("focus on the auth module"))
    ));

    // Persisted as `LogRecord::ParentSteer` with matching provenance.
    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    assert!(records.iter().any(|r| matches!(
        r,
        LogRecord::ParentSteer { from, text, prov, .. }
            if *from == steer_from
                && text == "focus on the auth module"
                && matches!(prov, Provenance::ParentSteer { from: pf, .. } if *pf == steer_from)
    )));
}

/// Criterion (source-level/structural): no code path injects into a
/// context outside `drain_inbox` -- `ContextInput` is constructed exactly
/// once per turn, and its `path` is always derived from `all_records`
/// (sourced from a fresh `SessionStore::read` via `path_from_legacy`),
/// never from anything `drain_inbox` returns directly (`drain_inbox`
/// returns `()`, not records -- see `mailbox::DrainEffect::Persist`'s own
/// doc on why a steer becomes visible only by first becoming a stored
/// record).
#[test]
fn context_own_is_only_ever_populated_from_a_fresh_store_read() {
    let src = include_str!("../src/agent_loop.rs");
    assert_eq!(
        src.matches("ContextInput {").count(),
        1,
        "ContextInput must be constructed in exactly one place"
    );
    assert!(
        src.contains("path_from_legacy(self.inherited.as_ref(), &all_records, self.session)"),
        "path must be derived from path_from_legacy over all_records, sourced from a fresh store read"
    );
    assert!(
        src.contains("async fn drain_inbox(&mut self) -> Result<(), RuntimeError>"),
        "drain_inbox must not return records for direct injection into a context"
    );
}

/// Criterion: `Cancel { hard: true }` trips the `CancellationToken`
/// immediately, aborting in-flight tool futures, and still yields
/// `AgentResult { status: Cancelled }`.
#[tokio::test]
async fn hard_cancel_via_mailbox_resolves_cancelled_within_100ms() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let session = SessionId::new();
    let agent = AgentId::new();
    seed_prompt(&*store, agent, session, "go").await;

    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(tool_call_response(
            "tc_1", "slow",
        ))])
        .with_capabilities(caps_ok()),
    );
    let tool: Arc<dyn Tool> = Arc::new(DelayTool {
        name: ToolName::new("slow"),
        delay: Duration::from_secs(5),
    });

    let harness = build_loop(session, agent, store, backend, vec![tool], None);
    let mailbox_tx = harness.mailbox_tx.clone();
    let bus = harness.bus.clone();
    let mut stream = bus.subscribe();

    let handle = tokio::spawn(harness.agent_loop.run());
    wait_for_tool_call_started(&mut stream).await;

    mailbox_tx.send(AgentMessage::Cancel {
        from: agent,
        reason: "urgent stop".to_string(),
        hard: true,
    });

    let result = tokio::time::timeout(Duration::from_millis(100), handle)
        .await
        .expect("agent loop did not resolve within 100ms of the hard cancel")
        .expect("agent loop task panicked");
    assert!(
        matches!(result.status, ResultStatus::Cancelled { .. }),
        "expected Cancelled, got {:?}",
        result.status
    );
}

/// Criterion: `Cancel { hard: false }` takes effect at the next turn
/// boundary -- the in-flight tool call is allowed to complete (and its
/// result is persisted), but the loop stops before starting another turn.
#[tokio::test]
async fn soft_cancel_waits_for_the_inflight_tool_then_stops_at_the_next_boundary() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let session = SessionId::new();
    let agent = AgentId::new();
    seed_prompt(&*store, agent, session, "go").await;

    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response("tc_1", "slow")),
            ScriptedTurn::Respond(text_response("should never run")),
        ])
        .with_capabilities(caps_ok()),
    );
    let tool: Arc<dyn Tool> = Arc::new(DelayTool {
        name: ToolName::new("slow"),
        delay: Duration::from_millis(150),
    });

    let harness = build_loop(
        session,
        agent,
        store.clone(),
        backend.clone(),
        vec![tool],
        None,
    );
    let mailbox_tx = harness.mailbox_tx.clone();
    let bus = harness.bus.clone();
    let mut stream = bus.subscribe();

    let handle = tokio::spawn(harness.agent_loop.run());
    wait_for_tool_call_started(&mut stream).await;

    mailbox_tx.send(AgentMessage::Cancel {
        from: agent,
        reason: "please wrap up".to_string(),
        hard: false,
    });

    let result = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("agent loop did not resolve")
        .expect("agent loop task panicked");
    match result.status {
        ResultStatus::Cancelled { reason } => assert_eq!(reason, "please wrap up"),
        other => panic!("expected Cancelled, got {other:?}"),
    }

    // The in-flight tool call was allowed to finish: its result was
    // persisted...
    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    assert!(records
        .iter()
        .any(|r| matches!(r, LogRecord::ToolResultRecord { .. })));
    // ...but the second (text) turn never ran.
    assert_eq!(
        backend.calls().len(),
        1,
        "the soft cancel must stop the loop before the next turn's backend call"
    );
}

/// Criterion: the real resolution path for a
/// `conway_fork`/`conway_spawn` waiter that BLOCKED on this specific child by id is
/// still `AgentTree::await_result` alone, exercised end-to-end
/// (including genuinely BLOCKING until the child finishes, not just
/// observing an already-finished child) by `tests/subagent_fork_spawn.rs`'s
/// `await_result_blocks_until_the_child_actually_finishes_then_resolves_every_awaiter_once`
/// -- unmodified by this item.
///
/// What DID change: a drained `AgentMessage::Result` no longer vanishes.
/// This test proves the mailbox side stays well-behaved (delivering a
/// `Result` into an agent's own inbox must not error, panic, or otherwise
/// disturb that agent's own run) AND that it is durably persisted --
/// `a_parent_that_did_not_await_observes_its_childs_completion` below is
/// the test that proves the parent's own NEXT TURN actually sees it.
#[tokio::test]
async fn result_message_is_classified_and_persisted_without_disturbing_this_agents_own_run() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let session = SessionId::new();
    let agent = AgentId::new();
    seed_prompt(&*store, agent, session, "go").await;
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("hi"))])
            .with_capabilities(caps_ok()),
    );

    let harness = build_loop(session, agent, store.clone(), backend, vec![], None);
    let child = AgentId::new();
    let child_session = SessionId::new();
    let child_result =
        AgentResult::new(child, child_session, ResultStatus::Completed, "child done");
    harness.mailbox_tx.send(AgentMessage::Result {
        from: child,
        result: child_result,
    });

    let result = harness.agent_loop.run().await;
    assert_eq!(
        result.status,
        ResultStatus::Completed,
        "a drained Result must not disturb this agent's own run"
    );

    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    assert!(records.iter().any(|r| matches!(
        r,
        LogRecord::ChildResultRecord { result, prov, .. }
            if result.agent_id == child
                && result.summary == "child done"
                && matches!(prov, Provenance::ChildResult { from } if *from == child)
    )));
}

/// Criterion: a parent that starts
/// several children and never blocks on `AgentTree::await_result` for any
/// one of them by id can still learn that one finished -- by observing it
/// on its own very next turn, exactly the way it already observes a steer.
/// Asserted on what the SECOND turn's assembled context actually contains,
/// not merely on a record having been written (a record-only assertion
/// would prove the write, not the observation) -- see this test's own
/// segment-content assertion below.
#[tokio::test]
async fn a_parent_that_did_not_await_observes_its_childs_completion() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let session = SessionId::new();
    let agent = AgentId::new();
    seed_prompt(&*store, agent, session, "go").await;

    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response("tc_1", "read")),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_capabilities(caps_ok()),
    );
    let tool: Arc<dyn Tool> = Arc::new(DelayTool {
        name: ToolName::new("read"),
        delay: Duration::from_millis(150),
    });

    let harness = build_loop(
        session,
        agent,
        store.clone(),
        backend.clone(),
        vec![tool],
        None,
    );
    let mailbox_tx = harness.mailbox_tx.clone();
    let bus = harness.bus.clone();
    let mut stream = bus.subscribe();

    let handle = tokio::spawn(harness.agent_loop.run());
    wait_for_tool_call_started(&mut stream).await;

    // This agent (`agent`) never calls `AgentTree::await_result` for
    // `child` -- the ONLY thing that happens is exactly what a real
    // child's `AgentLoop::finish` does: deliver `AgentMessage::Result` to
    // the parent's mailbox (architecture §3.2).
    let child = AgentId::new();
    let child_session = SessionId::new();
    let child_result = AgentResult::new(
        child,
        child_session,
        ResultStatus::Completed,
        "found the bug",
    );
    mailbox_tx.send(AgentMessage::Result {
        from: child,
        result: child_result,
    });

    let result = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("agent loop did not finish")
        .expect("agent loop task panicked");
    assert_eq!(result.status, ResultStatus::Completed);

    let calls = backend.calls();
    assert_eq!(calls.len(), 2, "expected exactly two turns");

    // Absent from the in-flight (first) turn -- the same turn-boundary
    // landing guarantee steering already gets.
    assert!(
        !calls[0]
            .segments
            .iter()
            .any(|s| matches!(s.provenance, Provenance::ChildResult { .. })),
        "the child's result must not appear in the turn already in flight when it arrived"
    );

    // Present in the SECOND turn: THIS is the observation the acceptance
    // criterion asks for -- the parent's own next-turn context genuinely
    // carries the child's completion, with the correct provenance and
    // content, not a parent-authored stand-in for it.
    let new_segments = &calls[1].segments[calls[0].segments.len()..];
    let child_segment = new_segments
        .iter()
        .find(|s| matches!(s.provenance, Provenance::ChildResult { .. }))
        .expect("the next turn must contain a new segment carrying the child's result");
    match &child_segment.provenance {
        Provenance::ChildResult { from } => assert_eq!(*from, child),
        other => panic!("expected Provenance::ChildResult, got {other:?}"),
    }
    assert!(
        child_segment
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("found the bug"))),
        "the segment text must carry the child's summary"
    );

    // Durably persisted, at the tail, as `LogRecord::ChildResultRecord`
    // (the store-level byte-prefix invariant is exercised directly by
    // `conway-session`'s
    // `appending_a_child_result_record_leaves_the_prior_transcript_byte_identical`).
    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    assert!(records.iter().any(|r| matches!(
        r,
        LogRecord::ChildResultRecord { result, prov, .. }
            if result.agent_id == child
                && result.summary == "found the bug"
                && matches!(prov, Provenance::ChildResult { from } if *from == child)
    )));
}

/// Bidirectional messaging (Claude Code-style): the exact same mailbox
/// mechanism that lets a parent steer a child also lets a child steer its
/// own parent -- mailboxes are direction-agnostic. Whoever holds an
/// agent's `MailboxSender` (here: a "child" that only knows its own
/// `agent_id` and the parent's sender, exactly what `AgentLoop::finish`
/// itself holds via `parent_mailbox`) can steer that agent.
#[tokio::test]
async fn steering_is_bidirectional_a_child_can_steer_its_own_parent() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let parent_session = SessionId::new();
    let parent_agent = AgentId::new();
    seed_prompt(&*store, parent_agent, parent_session, "parent go").await;

    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("parent done"))])
            .with_capabilities(caps_ok()),
    );

    let harness = build_loop(
        parent_session,
        parent_agent,
        store.clone(),
        backend,
        vec![],
        None,
    );
    let parent_mailbox = harness.mailbox_tx.clone();

    let child_agent = AgentId::new();
    parent_mailbox.send(AgentMessage::Steer {
        from: child_agent,
        text: "child says hi".to_string(),
        at_parent_seq: LogSeq::ZERO,
    });

    let result = harness.agent_loop.run().await;
    assert_eq!(result.status, ResultStatus::Completed);

    let records = store
        .read(&parent_session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    assert!(records.iter().any(|r| matches!(
        r,
        LogRecord::ParentSteer { from, text, .. }
            if *from == child_agent && text == "child says hi"
    )));
}

/// Carried follow-up (/): `AgentLoop::finish` consults the
/// `AgentTree`'s set-once publication before emitting `Event::AgentFinished`
/// -- if the tree already has a result for this agent (simulating the
/// supervisor's own grace-timeout synthesis winning the race), `finish`
/// must not emit a second `AgentFinished`.
#[tokio::test]
async fn finish_does_not_double_emit_agent_finished_once_the_tree_already_has_a_result() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let session = SessionId::new();
    let agent = AgentId::new();
    seed_prompt(&*store, agent, session, "go").await;
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("hi"))])
            .with_capabilities(caps_ok()),
    );

    let harness = build_loop(session, agent, store, backend, vec![], None);
    let bus = harness.bus.clone();

    let synthesized = AgentResult::new(
        agent,
        session,
        ResultStatus::Cancelled {
            reason: "synthesized by supervisor".to_string(),
        },
        "synthesized",
    );
    let published = harness
        .agent_loop
        .deps
        .tree
        .publish_result(agent, synthesized)
        .unwrap();
    assert!(published, "the simulated synthesis must win the race");

    let mut stream = bus.subscribe();
    let _ = harness.agent_loop.run().await;

    let finished_count = drain(&mut stream)
        .into_iter()
        .filter(|e| matches!(e, Event::AgentFinished { .. }))
        .count();
    assert_eq!(
        finished_count, 0,
        "finish must not emit AgentFinished once the tree already has a published result"
    );
}
