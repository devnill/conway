//! Acceptance tests for `SessionHandle`'s subagent surface (WI-102):
//! `fork`/`spawn`/`steer`/`await_agent`/`cancel`, plus `ForkSpec`/
//! `SpawnSpec`'s `From` conversions into `conway_core::agent::SubagentSpec`.
//!
//! Built the same way `tests/session_handle.rs` (WI-101) is: a real
//! `Arc<Runtime>` assembled from `conway_core::fakes` ports, not a literal
//! mock swapped in for `conway_core::ports::SubagentHost` -- `SessionHandle`
//! holds a concrete `Arc<Runtime>` (WI-101's committed struct shape, out of
//! this item's file scope to change), so "argument identity" is verified
//! through this crate's own observable effects (the resulting
//! `AgentTreeSnapshot` node, the child's persisted transcript, and terminal
//! `AgentResult`s) rather than by intercepting a trait-object double. This
//! mirrors WI-101's own "fake-runtime test" criteria, which used the exact
//! same real-Runtime-over-fakes construction for the same reason.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    PluginsConfig,
    RoleEntry, RoutingSection, SessionConfig, ToolsConfig, TuiSection,
};
use conway::{
    CancelMode, Conway, ConwayBuilder, ConwayError, ForkSpec, Plugin, SessionHandle, SessionId,
    SessionSpec, SpawnSpec, Tool,
};
use conway_core::agent::{Budget, PermissionDecision, ResultStatus, SubagentMode};
use conway_core::capabilities::{
    CacheMode, Capabilities, ProbeReport, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{ContentBlock, StopReason, ToolCall, Usage};
use conway_core::error::BackendError;
use conway_core::fakes::{
    FakeBackend, FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn,
};
use conway_core::ids::{AgentId, BackendId, LogSeq, ModelId, RoleAlias};
use conway_core::log::LogRecord;
use conway_core::ports::{
    Backend, BoxStream, GenerateRequest, GenerateResponse, SessionStore, StreamChunk,
};
use conway_core::provenance::Provenance;
use futures_core::Stream as _;

// ---------------------------------------------------------------------
// Fixtures (mirrors tests/session_handle.rs's own helpers)
// ---------------------------------------------------------------------

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(conway_core::ids::ModelRef {
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
        default_role: conway_core::ids::RoleAlias::new("default"),
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
        plugins: PluginsConfig::default(),
    }
}

fn build_conway(backend: Arc<dyn Backend>, store: Arc<FakeStore>) -> Conway {
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should succeed with every port injected")
}

fn build_conway_with_echo_backend(store: Arc<FakeStore>) -> Conway {
    build_conway(Arc::new(FakeBackend::echo(BackendId::new("fake"))), store)
}

async fn new_handle(conway: &Conway) -> SessionHandle {
    conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed")
}

fn caps() -> Capabilities {
    Capabilities {
        tool_calling: ToolCallSupport::None,
        cache: CacheMode::None,
        parallel_tool_calls: false,
        structured_output: StructuredOutput::None,
        max_context_tokens: 128_000,
        reasoning: false,
        reliability_tier: ReliabilityTier::Unknown,
    }
}

/// A trivial `Stream` that yields its queued items one poll at a time, then
/// ends -- stands in for `futures::stream::iter` (that crate is not a
/// dependency of this crate; `Cargo.toml` is out of this item's file
/// scope), used only by `DelayedEchoBackend::stream` below.
struct QueueStream(VecDeque<Result<StreamChunk, BackendError>>);

impl futures_core::Stream for QueueStream {
    type Item = Result<StreamChunk, BackendError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::task::Poll::Ready(self.0.pop_front())
    }
}

/// A one-shot backend that sleeps `delay` before returning a fixed text
/// response -- gives a test a deterministic window in which an agent is
/// known to be mid-turn (blocked inside `Backend::generate`), so `cancel`
/// can be observed racing it (mirrors `conway-runtime`'s own
/// `DelayedBackend` test double, `tests/runtime_api.rs`; duplicated locally
/// rather than shared, since that type is private to that crate's test
/// binary).
struct DelayedEchoBackend {
    id: BackendId,
    delay: Duration,
    response: Mutex<VecDeque<GenerateResponse>>,
}

impl DelayedEchoBackend {
    fn new(delay: Duration) -> Self {
        Self {
            id: BackendId::new("fake"),
            delay,
            response: Mutex::new(VecDeque::from([GenerateResponse {
                content: vec![ContentBlock::Text {
                    text: "done".to_string(),
                }],
                tool_calls: vec![],
                stop: StopReason::EndTurn,
                usage: Usage::default(),
            }])),
        }
    }
}

#[async_trait]
impl Backend for DelayedEchoBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn capabilities(&self, _model: &ModelId) -> Capabilities {
        caps()
    }

    async fn generate(&self, _req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
        tokio::time::sleep(self.delay).await;
        self.response
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(BackendError::BadRequest {
                detail: "delayed backend exhausted".to_string(),
            })
    }

    async fn stream(
        &self,
        req: GenerateRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
        let response = self.generate(req).await?;
        let mut chunks: VecDeque<Result<StreamChunk, BackendError>> = response
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(Ok(StreamChunk::TextDelta(text.clone()))),
                _ => None,
            })
            .collect();
        chunks.push_back(Ok(StreamChunk::Done(response)));
        Ok(Box::pin(QueueStream(chunks)))
    }

    async fn probe(&self) -> Result<ProbeReport, BackendError> {
        Ok(ProbeReport {
            ok: true,
            latency_ms: 1,
            models: vec![],
            detail: None,
            at: chrono::Utc::now(),
        })
    }
}

// ---------------------------------------------------------------------
// fork() / spawn(): argument mapping, observed through the tree snapshot
// and the child's persisted transcript (see the module doc for why this
// crate's tests verify delegation this way rather than via a literal fake
// `SubagentHost`).
// ---------------------------------------------------------------------

#[tokio::test]
async fn fork_without_a_role_inherits_the_parents_role_not_the_literal_default() {
    // WI-136 regression: with a normal config whose `default_role` is NOT the
    // literal "default", a fork that specifies no role of its own must inherit
    // the PARENT's role. Before the fix it fell back to a hardcoded
    // `RoleAlias::new("default")`, which is not a configured alias here, so
    // prompting the child failed routing with `unknown role alias: default`.
    let mut roles = BTreeMap::new();
    roles.insert(
        "coder".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    let config = ConwayConfig {
        default_role: RoleAlias::new("coder"),
        roles,
        ..base_config()
    };
    let store = Arc::new(FakeStore::new());
    let conway = ConwayBuilder::from_parts(config)
        .with_backend(Arc::new(FakeBackend::echo(BackendId::new("fake"))))
        .with_session_store(store.clone())
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(fake_router())
        .build()
        .expect("build should succeed");
    // The root's role defaults to config.default_role ("coder").
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");

    // Fork with NO role and NO agent def.
    let child = handle
        .fork(handle.root(), ForkSpec::new("keep going"))
        .await
        .expect("fork should succeed");

    let tree = handle.tree();
    let node = tree
        .nodes
        .iter()
        .find(|n| n.agent_id == child)
        .expect("child must be attached to the tree");
    assert_eq!(
        node.role,
        Some(RoleAlias::new("coder")),
        "a roleless fork must inherit the parent's role, not the literal \"default\""
    );
}

#[tokio::test]
async fn fork_produces_a_child_with_mapped_fields_and_an_inherited_prefix() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store.clone());
    let handle = new_handle(&conway).await;

    // A prompt-less root starts idle with no record of its own -- append
    // one directly to the store so this test can prove a forked child
    // inherits a REAL parent prefix, not an artifact of the old
    // spontaneous-turn behavior.
    store
        .append(
            &handle.id(),
            LogRecord::UserTurn {
                seq: LogSeq::ZERO,
                ts: chrono::Utc::now(),
                text: "root-own".to_string(),
                prov: Provenance::UserPrompt,
            },
        )
        .await
        .expect("append should succeed");

    let budget = Budget {
        max_steps: 5,
        deadline: None,
        max_tokens: Some(999),
        max_tool_calls: None,
    };
    let spec = ForkSpec::new("keep going")
        .role(RoleAlias::new("planner"))
        .budget(budget.clone());

    let child = handle
        .fork(handle.root(), spec)
        .await
        .expect("fork should succeed");

    let tree = handle.tree();
    let node = tree
        .nodes
        .iter()
        .find(|n| n.agent_id == child)
        .expect("child must be attached to the tree");
    assert_eq!(node.parent, Some(handle.root()));
    assert_eq!(node.mode, Some(SubagentMode::Fork));
    assert_eq!(node.role, Some(RoleAlias::new("planner")));
    assert_eq!(node.budget, budget);

    let transcript = handle
        .transcript(child)
        .await
        .expect("transcript should resolve");
    assert!(
        transcript
            .iter()
            .any(|r| matches!(r, LogRecord::UserTurn { text, .. } if text == "root-own")),
        "a forked child must inherit the forker's own prefix (here, the root's own prior record)"
    );
    assert!(
        transcript
            .iter()
            .any(|r| matches!(r, LogRecord::ForkDirective { text, .. } if text == "keep going")),
        "the fork directive must be recorded as the child's own head record, verbatim"
    );

    // Drain the child's one-shot turn so it does not linger past this test.
    let _ = tokio::time::timeout(Duration::from_secs(5), handle.await_agent(child)).await;
}

#[tokio::test]
async fn spawn_produces_a_child_with_mapped_fields_and_no_inherited_prefix() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store.clone());
    let handle = new_handle(&conway).await;

    // A prompt-less root starts idle with no record of its own -- append
    // one directly to the store so the "disclosed reconciliation" assertion
    // below proves `transcript()` still surfaces a REAL parent record for a
    // spawned child, not an artifact of the old spontaneous-turn behavior.
    store
        .append(
            &handle.id(),
            LogRecord::UserTurn {
                seq: LogSeq::ZERO,
                ts: chrono::Utc::now(),
                text: "root-own".to_string(),
                prov: Provenance::UserPrompt,
            },
        )
        .await
        .expect("append should succeed");

    let budget = Budget {
        max_steps: 3,
        deadline: None,
        max_tokens: None,
        max_tool_calls: None,
    };
    let spec = SpawnSpec::new("please review")
        .agent_def("unregistered-agent-def")
        .role(RoleAlias::new("reviewer"))
        .budget(budget.clone());

    let child = handle
        .spawn(handle.root(), spec)
        .await
        .expect("spawn should succeed");

    let tree = handle.tree();
    let node = tree
        .nodes
        .iter()
        .find(|n| n.agent_id == child)
        .expect("child must be attached to the tree");
    assert_eq!(node.parent, Some(handle.root()));
    assert_eq!(node.mode, Some(SubagentMode::Spawn));
    assert_eq!(node.role, Some(RoleAlias::new("reviewer")));
    assert_eq!(node.budget, budget);

    let transcript = handle
        .transcript(child)
        .await
        .expect("transcript should resolve");
    assert!(
        transcript
            .iter()
            .any(|r| matches!(r, LogRecord::UserTurn { text, .. } if text == "please review")),
        "the child's own head record must be its spawn prompt, verbatim"
    );

    // **Disclosed reconciliation:** the WI-102 binding notes' own framing
    // ("no inherited prefix") describes what a spawned child's *context
    // assembly* sees (`AgentLoop` always reads a session's own records
    // straight from the store, per `conway-runtime`'s `subagent.rs` --
    // architecture's "clean slate" guarantee, unaffected by this). It is
    // NOT what `SessionHandle::transcript` (WI-101, unmodified by this
    // item) returns: `SubagentHost::start` records `SessionMeta.origin` for
    // a spawned child too ("for tree reconstructability only"), and
    // `transcript`'s ancestry walk (`resolve_prefix`) follows any `Some
    // origin` unconditionally -- it does not (and, being out of this
    // item's file scope, cannot be made to) distinguish fork from spawn.
    // So a spawned child's *transcript* -- unlike its *context* -- does
    // show the parent's prefix. Asserted here as the actual, verified
    // behavior rather than the stronger claim the binding notes' prose
    // might suggest.
    assert!(
        transcript
            .iter()
            .any(|r| matches!(r, LogRecord::UserTurn { text, .. } if text == "root-own")),
        "transcript() is expected to still show the parent's own record for a spawned child \
         (see the comment above) -- if this ever stops holding, `SessionStore::fork`'s `origin` \
         recording for `SubagentMode::Spawn` has changed and this test's own premise needs \
         re-checking, not just this assertion"
    );

    let _ = tokio::time::timeout(Duration::from_secs(5), handle.await_agent(child)).await;
}

// ---------------------------------------------------------------------
// Session-ownership check
// ---------------------------------------------------------------------

#[tokio::test]
async fn fork_rejects_a_from_agent_that_belongs_to_a_different_session() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle_a = new_handle(&conway).await;
    let handle_b = new_handle(&conway).await;

    let err = handle_a
        .fork(handle_b.root(), ForkSpec::new("x"))
        .await
        .expect_err("a foreign session's root must be rejected");
    match err {
        ConwayError::Runtime(inner) => {
            // F-102-1, resolved (WI-119): `conway_core::error::RuntimeError`
            // now has a dedicated `AgentNotInSession { agent, session }`
            // variant (see `SessionHandle::ensure_agent_in_session`'s doc)
            // rendering exactly "agent does not belong to session" -- this
            // just asserts the message names the rejected agent id, without
            // pinning the variant name itself.
            assert!(
                inner.to_string().contains(&handle_b.root().to_string()),
                "error must name the rejected agent id: {inner}"
            );
        }
        other => panic!("expected ConwayError::Runtime, got {other:?}"),
    }
}

#[tokio::test]
async fn steer_await_and_cancel_reject_a_target_belonging_to_another_session() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway(
        Arc::new(DelayedEchoBackend::new(Duration::from_secs(2))),
        store,
    );
    let handle_a = new_handle(&conway).await;
    let handle_b = new_handle(&conway).await;

    // Cross-session: `handle_a` must not be able to steer, await, or
    // hard-cancel `handle_b`'s root, even though both live on the same
    // `Conway`/`Arc<Runtime>`.
    let steer_err = handle_a
        .steer(handle_b.root(), "hello from a foreign session")
        .await
        .expect_err("cross-session steer must be rejected");
    match steer_err {
        ConwayError::Runtime(inner) => {
            assert!(inner.to_string().contains(&handle_b.root().to_string()));
        }
        other => panic!("expected ConwayError::Runtime, got {other:?}"),
    }

    let await_err = handle_a
        .await_agent(handle_b.root())
        .await
        .expect_err("cross-session await_agent must be rejected");
    match await_err {
        ConwayError::Runtime(inner) => {
            assert!(inner.to_string().contains(&handle_b.root().to_string()));
        }
        other => panic!("expected ConwayError::Runtime, got {other:?}"),
    }

    let cancel_err = handle_a
        .cancel(handle_b.root(), "cross-session cancel attempt")
        .await
        .expect_err("cross-session cancel must be rejected");
    match cancel_err {
        ConwayError::Runtime(inner) => {
            assert!(inner.to_string().contains(&handle_b.root().to_string()));
        }
        other => panic!("expected ConwayError::Runtime, got {other:?}"),
    }

    // Same-session: `handle_b` acting on its own root must still succeed --
    // the check above must not over-reject a handle's own agents.
    let turn = handle_b
        .prompt("hi")
        .await
        .expect("same-session prompt should succeed");
    tokio::time::sleep(Duration::from_millis(150)).await;

    handle_b
        .steer(handle_b.root(), "please hurry")
        .await
        .expect("same-session steer must still succeed");

    handle_b
        .cancel(handle_b.root(), "test requested cancellation")
        .await
        .expect("same-session cancel must still succeed");

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        handle_b.await_agent(handle_b.root()),
    )
    .await
    .expect("same-session await_agent must not hang")
    .expect("same-session await_agent must still succeed");
    assert!(
        matches!(result.status, ResultStatus::Cancelled { .. }),
        "expected Cancelled, got {:?}",
        result.status
    );

    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result()).await;
}

#[tokio::test]
async fn spawn_rejects_an_unknown_from_agent() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    let unknown = AgentId::new();
    let err = handle
        .spawn(unknown, SpawnSpec::new("y").agent_def("x"))
        .await
        .expect_err("an unknown from agent must be rejected");
    assert!(matches!(err, ConwayError::Runtime(_)));
}

// ---------------------------------------------------------------------
// steer()
// ---------------------------------------------------------------------

#[tokio::test]
async fn steer_on_unknown_target_returns_runtime_error() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    let unknown = AgentId::new();
    let err = handle
        .steer(unknown, "hello")
        .await
        .expect_err("steering an unknown target must fail");
    match err {
        ConwayError::Runtime(inner) => {
            assert!(inner.to_string().contains(&unknown.to_string()));
        }
        other => panic!("expected ConwayError::Runtime, got {other:?}"),
    }
}

#[tokio::test]
async fn steer_on_a_live_agent_succeeds() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway(
        Arc::new(DelayedEchoBackend::new(Duration::from_secs(2))),
        store,
    );
    let handle = new_handle(&conway).await;

    let turn = handle.prompt("hi").await.expect("prompt should succeed");
    // Yield so the agent's task is actually scheduled and blocked inside
    // `Backend::generate` before steering it.
    tokio::time::sleep(Duration::from_millis(150)).await;

    handle
        .steer(handle.root(), "please hurry")
        .await
        .expect("steer should succeed while the agent is in flight");

    // Let it finish naturally so nothing dangles past this test.
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result()).await;
}

// ---------------------------------------------------------------------
// await_agent()
// ---------------------------------------------------------------------

#[tokio::test]
async fn await_agent_on_unknown_agent_returns_runtime_error_naming_the_id() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    let unknown = AgentId::new();
    let err = handle
        .await_agent(unknown)
        .await
        .expect_err("an unknown agent must be rejected");
    match err {
        ConwayError::Runtime(inner) => {
            assert!(inner.to_string().contains(&unknown.to_string()));
        }
        other => panic!("expected ConwayError::Runtime, got {other:?}"),
    }
}

#[tokio::test]
async fn await_agent_on_a_budget_exhausted_child_returns_ok_budget_exceeded() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    let spec = ForkSpec::new("go").budget(Budget {
        max_steps: 0,
        deadline: None,
        max_tokens: None,
        max_tool_calls: None,
    });
    let child = handle
        .fork(handle.root(), spec)
        .await
        .expect("fork should succeed");

    let result = tokio::time::timeout(Duration::from_secs(5), handle.await_agent(child))
        .await
        .expect("await_agent must not hang")
        .expect("await_agent should resolve Ok even on BudgetExceeded");
    assert!(
        matches!(result.status, ResultStatus::BudgetExceeded { .. }),
        "expected BudgetExceeded with max_steps = 0, got {:?}",
        result.status
    );
}

#[tokio::test]
async fn await_agent_on_a_hard_cancelled_child_returns_ok_cancelled() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway(
        Arc::new(DelayedEchoBackend::new(Duration::from_secs(2))),
        store,
    );
    let handle = new_handle(&conway).await;

    let child = handle
        .fork(handle.root(), ForkSpec::new("go"))
        .await
        .expect("fork should succeed");

    // Yield so the child's task is scheduled and blocked inside
    // `Backend::generate` (a genuinely in-flight turn) before cancelling.
    tokio::time::sleep(Duration::from_millis(150)).await;
    handle
        .cancel(child, "test requested cancellation")
        .await
        .expect("cancel should succeed");

    let result = tokio::time::timeout(Duration::from_secs(5), handle.await_agent(child))
        .await
        .expect("await_agent must not hang after a hard cancel")
        .expect("await_agent should resolve Ok even on Cancelled");
    assert!(
        matches!(result.status, ResultStatus::Cancelled { .. }),
        "expected Cancelled, got {:?}",
        result.status
    );
}

// ---------------------------------------------------------------------
// cancel() + TurnHandle::result() -- the F-101-1 deviation #4 gap WI-101
// disclosed as untestable until `SessionHandle::cancel` existed. This is
// that test, now that this item adds it: `Runtime::cancel`'s effect on the
// *root* agent's own `TurnHandle`, exercised through `SessionHandle::cancel`
// exactly as an embedder would reach it (WI-101's own suite could only
// cover `TurnHandle::result()` resolving on `Completed`/`BudgetExceeded`).
// ---------------------------------------------------------------------

#[tokio::test]
async fn cancel_resolves_the_root_turn_handles_result_as_cancelled() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway(
        Arc::new(DelayedEchoBackend::new(Duration::from_secs(2))),
        store,
    );
    let handle = new_handle(&conway).await;

    let turn = handle.prompt("hi").await.expect("prompt should succeed");
    tokio::time::sleep(Duration::from_millis(150)).await;

    handle
        .cancel(handle.root(), "test requested cancellation")
        .await
        .expect("cancel should succeed");

    let result = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang after cancel")
        .expect("result() should succeed");
    assert!(
        matches!(result.status, ResultStatus::Cancelled { .. }),
        "expected Cancelled, got {:?}",
        result.status
    );

    // A second cancel on an already-unknown agent still surfaces a typed
    // error rather than panicking.
    let err = handle
        .cancel(AgentId::new(), "x")
        .await
        .expect_err("cancelling an unknown agent must fail");
    assert!(matches!(err, ConwayError::Runtime(_)));
}

// ---------------------------------------------------------------------
// EventStream tree-lifecycle passthrough -- the actual bug this item
// fixes: a spawned child's `Event::AgentSpawned`/`Event::AgentFinished`
// are emitted on the CHILD's own session (`tree.rs::attach`,
// `supervisor.rs::finish`), so before the fix a subscriber scoped to the
// PARENT's session (e.g. the TUI's `handle.events()`, exercised here via
// `SessionHandle::events()` directly) never observed them, leaving the
// `/agents` panel and inline `Entry::Agent` activity empty.
// ---------------------------------------------------------------------

#[tokio::test]
async fn root_event_stream_observes_a_spawned_childs_agent_spawned_and_finished() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    let mut events = handle.events();

    let child = handle
        .spawn(
            handle.root(),
            SpawnSpec::new("please review").agent_def("reviewer"),
        )
        .await
        .expect("spawn should succeed");

    let spawned = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope = std::future::poll_fn(|cx| std::pin::Pin::new(&mut events).poll_next(cx))
                .await
                .expect("root event stream ended early");
            if matches!(
                envelope.event,
                conway_core::event::Event::AgentSpawned { .. }
            ) && envelope.agent == child
            {
                return envelope;
            }
        }
    })
    .await
    .expect("must observe the child's AgentSpawned on the root's own event stream, not hang");
    assert_eq!(
        spawned.agent, child,
        "the AgentSpawned envelope must identify the CHILD agent, not the parent"
    );
    assert_ne!(
        spawned.session,
        handle.id(),
        "sanity: the child's AgentSpawned is emitted on the CHILD's own session, not the \
         root's -- this test only passes because EventStream::accept now bypasses the session \
         filter for it"
    );

    let finished = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope = std::future::poll_fn(|cx| std::pin::Pin::new(&mut events).poll_next(cx))
                .await
                .expect("root event stream ended early");
            if let conway_core::event::Event::AgentFinished { result, .. } = &envelope.event {
                if result.agent_id == child {
                    return envelope;
                }
            }
        }
    })
    .await
    .expect("must observe the child's AgentFinished on the root's own event stream, not hang");
    assert_ne!(finished.session, handle.id());
}

// ---------------------------------------------------------------------
// agent_events() -- WI-140: a spawned child's own transcript + live events
// are observable via the new per-agent facade method, distinctly from the
// parent/root's own `events()`/`events_from()`.
// ---------------------------------------------------------------------

#[tokio::test]
async fn agent_events_replays_a_spawned_childs_own_transcript() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    // A prior turn on the ROOT, before the spawn -- if `agent_events`
    // regressed to building its replay from the ancestry-prefixed
    // `effective_transcript` (bug 4), this would appear as the first
    // envelope below instead of the child's own spawn prompt.
    let root_turn = handle
        .prompt("root turn before spawn")
        .await
        .expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), root_turn.result()).await;

    let child = handle
        .spawn(
            handle.root(),
            SpawnSpec::new("please review").agent_def("reviewer"),
        )
        .await
        .expect("spawn should succeed");

    // Let the child's one-shot turn actually run to completion before
    // observing its transcript, so the replay batch below has real content
    // beyond just the spawn prompt.
    let _ = tokio::time::timeout(Duration::from_secs(5), handle.await_agent(child)).await;

    let mut events = handle
        .agent_events(child)
        .await
        .expect("agent_events should resolve for a known child");

    // The replay batch must be built from the CHILD's own records ONLY
    // (its own head record is the spawn prompt) -- not the root's prior
    // conversation.
    let first = tokio::time::timeout(Duration::from_secs(5), async {
        std::future::poll_fn(|cx| std::pin::Pin::new(&mut events).poll_next(cx)).await
    })
    .await
    .expect("must not hang")
    .expect("stream ended before any replay envelope");
    assert!(
        matches!(&first.event, conway_core::event::Event::UserTurn { text, .. } if text.contains("please review")),
        "expected the replayed spawn prompt as a typed Event::UserTurn (this item), got {:?}",
        first.event
    );
}

#[tokio::test]
async fn agent_events_replay_excludes_the_inherited_prefix_transcript_still_includes_it() {
    // Bug 4 (decision 01KYAN6AHFG9JHQ6D2FAYCNFZJ): focusing a spawned
    // agent used to show the parent's prior conversation because
    // `agent_events` built its replay from `effective_transcript` (the
    // ancestry-prefixed view). This mirrors
    // `session_usage_sums_the_agents_own_assistant_turns_and_excludes_the_
    // inherited_prefix`'s own contrast, but for `agent_events`'s replay
    // batch versus `transcript`'s effective view.
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    // A distinctive turn on the root BEFORE the spawn -- this is the
    // "previous chat log" bug 4 reported leaking into the child's view.
    let root_turn = handle
        .prompt("root distinctive marker turn")
        .await
        .expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), root_turn.result()).await;

    let child = handle
        .spawn(
            handle.root(),
            SpawnSpec::new("child distinctive marker turn").agent_def("reviewer"),
        )
        .await
        .expect("spawn should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), handle.await_agent(child)).await;

    // Sanity: `transcript(child)` (the effective, ancestry-prefixed view)
    // really does carry the root's prior turn as an inherited prefix --
    // the leak this item's `agent_events` doc explicitly guards against
    // would only be possible via this same prefix mechanism.
    let child_transcript = handle
        .transcript(child)
        .await
        .expect("transcript should resolve");
    assert!(
        child_transcript.iter().any(|r| matches!(
            r,
            LogRecord::UserTurn { text, .. } if text.contains("root distinctive marker turn")
        )),
        "transcript(child) must still show the root's inherited prompt -- \
         effective_transcript's ancestry-prefixed semantics are unchanged"
    );

    // `agent_events(child)`'s replay must NOT surface the root's prior
    // conversation -- only the child's own records.
    let mut events = handle
        .agent_events(child)
        .await
        .expect("agent_events should resolve for a known child");

    let mut saw_root_marker = false;
    let mut saw_child_marker = false;
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match std::future::poll_fn(|cx| std::pin::Pin::new(&mut events).poll_next(cx)).await {
                Some(env) => {
                    let text = match &env.event {
                        conway_core::event::Event::AgentProgress { note } => note.clone(),
                        conway_core::event::Event::UserTurn { text, .. } => text.clone(),
                        conway_core::event::Event::TextDelta { text } => text.clone(),
                        _ => String::new(),
                    };
                    if text.contains("root distinctive marker turn") {
                        saw_root_marker = true;
                    }
                    if text.contains("child distinctive marker turn") {
                        saw_child_marker = true;
                    }
                    if matches!(env.event, conway_core::event::Event::AgentFinished { .. }) {
                        return;
                    }
                }
                None => return,
            }
        }
    })
    .await;

    assert!(
        saw_child_marker,
        "agent_events(child)'s replay must contain the child's own spawn prompt"
    );
    assert!(
        !saw_root_marker,
        "agent_events(child)'s replay must NOT contain the root's prior conversation \
         (bug 4: focusing a spawned agent showed the parent's chat log)"
    );
}

#[tokio::test]
async fn agent_events_replay_excludes_the_inherited_prefix_for_a_forked_child_too() {
    // Regression guard (coordinator review): the two tests above only
    // exercise a SPAWNED child. `agent_events` is mode-agnostic by
    // construction -- it reads `[0, head)` of `agent`'s own session and
    // never consults `SessionMeta.origin`/mode at all -- so a forked
    // child's focus view must exclude the inherited prefix exactly the
    // same way, even though `transcript()`'s ancestry-prefixed view (which
    // DOES consult `origin`) still shows it for both fork and spawn alike.
    // This mirrors `agent_events_replay_excludes_the_inherited_prefix_
    // transcript_still_includes_it` above, but builds the child via
    // `handle.fork(...)` (a `ForkSpec`), the same way
    // `fork_produces_a_child_with_mapped_fields_and_an_inherited_prefix`
    // does.
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    // A distinctive turn on the root BEFORE the fork -- this is the
    // inherited fork prefix `transcript(child)` must still show, but
    // `agent_events(child)` must not.
    let root_turn = handle
        .prompt("root distinctive marker turn")
        .await
        .expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), root_turn.result()).await;

    let child = handle
        .fork(
            handle.root(),
            ForkSpec::new("child distinctive marker turn"),
        )
        .await
        .expect("fork should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), handle.await_agent(child)).await;

    // Sanity: `transcript(child)` (the effective, ancestry-prefixed view)
    // really does carry the root's prior turn as an inherited prefix for a
    // FORKED child too -- the same mechanism
    // `fork_produces_a_child_with_mapped_fields_and_an_inherited_prefix`
    // already exercises.
    let child_transcript = handle
        .transcript(child)
        .await
        .expect("transcript should resolve");
    assert!(
        child_transcript.iter().any(|r| matches!(
            r,
            LogRecord::UserTurn { text, .. } if text.contains("root distinctive marker turn")
        )),
        "transcript(child) must still show the root's inherited prompt for a forked child"
    );

    // `agent_events(child)`'s replay must NOT surface the root's prior
    // conversation -- only the child's own records -- for a FORKED child,
    // exactly as it already does for a spawned one.
    let mut events = handle
        .agent_events(child)
        .await
        .expect("agent_events should resolve for a known child");

    let mut saw_root_marker = false;
    let mut saw_child_marker = false;
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match std::future::poll_fn(|cx| std::pin::Pin::new(&mut events).poll_next(cx)).await {
                Some(env) => {
                    let text = match &env.event {
                        conway_core::event::Event::AgentProgress { note } => note.clone(),
                        conway_core::event::Event::TextDelta { text } => text.clone(),
                        _ => String::new(),
                    };
                    if text.contains("root distinctive marker turn") {
                        saw_root_marker = true;
                    }
                    if text.contains("child distinctive marker turn") {
                        saw_child_marker = true;
                    }
                    if matches!(env.event, conway_core::event::Event::AgentFinished { .. }) {
                        return;
                    }
                }
                None => return,
            }
        }
    })
    .await;

    assert!(
        saw_child_marker,
        "agent_events(child)'s replay must contain the forked child's own fork directive"
    );
    assert!(
        !saw_root_marker,
        "agent_events(child)'s replay must NOT contain the root's prior conversation for a \
         forked child either -- agent_events reads [0, head) of the agent's own session and \
         never consults SessionMeta.origin/mode, so fork and spawn must behave identically here"
    );
}

#[tokio::test]
async fn agent_events_replay_surfaces_a_finished_childs_assistant_reply_text() {
    // End-to-end guard for the cycle-3 critical fix: a spawned child's own
    // ASSISTANT reply must be observable via `agent_events` replay (record_
    // to_event now maps `Assistant -> TextDelta{joined text}`), not silently
    // dropped -- so focusing a finished subagent shows its actual answer.
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    let child = handle
        .spawn(
            handle.root(),
            SpawnSpec::new("please review").agent_def("reviewer"),
        )
        .await
        .expect("spawn should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), handle.await_agent(child)).await;

    let mut events = handle
        .agent_events(child)
        .await
        .expect("agent_events should resolve for a known child");

    // Drain the replay batch and confirm the child's assistant turn appears
    // as a non-empty `TextDelta` (the echo backend produces a real reply).
    let saw_reply_text = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match std::future::poll_fn(|cx| std::pin::Pin::new(&mut events).poll_next(cx)).await {
                Some(env) => {
                    if let conway_core::event::Event::TextDelta { text } = &env.event {
                        if !text.is_empty() {
                            return true;
                        }
                    }
                    if matches!(env.event, conway_core::event::Event::AgentFinished { .. }) {
                        return false;
                    }
                }
                None => return false,
            }
        }
    })
    .await
    .expect("must not hang");

    assert!(
        saw_reply_text,
        "the finished child's assistant reply text must surface as a TextDelta on replay"
    );
}

#[tokio::test]
async fn agent_events_on_an_unknown_agent_returns_an_error() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    let unknown = AgentId::new();
    let result = handle.agent_events(unknown).await;
    match result {
        Err(ConwayError::Runtime(_)) => {}
        Ok(_) => panic!("an unknown agent must be rejected"),
        Err(other) => panic!("expected ConwayError::Runtime, got {other:?}"),
    }
}

#[tokio::test]
async fn agent_events_on_the_root_observes_live_progress_after_replay() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    let mut events = handle
        .agent_events(handle.root())
        .await
        .expect("agent_events should resolve for the root");

    let turn = handle.prompt("hello").await.expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result()).await;

    // The live half of the stream must observe the turn's own
    // `AgentFinished`, exactly as a fresh `events()`/`events_from()`
    // subscriber would.
    let saw_finished = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope = std::future::poll_fn(|cx| std::pin::Pin::new(&mut events).poll_next(cx))
                .await
                .expect("root agent_events stream ended early");
            if matches!(
                envelope.event,
                conway_core::event::Event::AgentFinished { .. }
            ) {
                return true;
            }
        }
    })
    .await
    .expect("must observe the root's own AgentFinished, not hang");
    assert!(saw_finished);
}

// ---------------------------------------------------------------------
// session_usage() -- board item 01KYAGP11FF9YC3G60TWHHKKST: sums an
// agent's OWN Assistant records, excluding any inherited fork prefix.
// ---------------------------------------------------------------------

/// A `LogRecord::Assistant` with a caller-chosen `usage`, otherwise a
/// minimal, valid record -- `seq` is ignored by `FakeStore::append` (it
/// assigns the real seq from the log's current length, mirroring every
/// other direct-`store.append` fixture in this file), so any placeholder
/// value here is fine.
fn assistant_record(usage: Usage) -> LogRecord {
    LogRecord::Assistant {
        seq: LogSeq::ZERO,
        ts: chrono::Utc::now(),
        content: vec![ContentBlock::Text {
            text: "ok".to_string(),
        }],
        model: conway_core::ids::ModelRef {
            backend: BackendId::new("fake"),
            model: ModelId::new("echo-model"),
        },
        route_reason: serde_json::Value::Null,
        usage,
        stop: StopReason::EndTurn,
    }
}

#[tokio::test]
async fn session_usage_is_zero_for_a_fresh_agent() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    // A freshly created session's root has no `Assistant` record at all
    // yet (no turn has ever run).
    let usage = handle
        .session_usage(handle.root())
        .await
        .expect("session_usage should resolve for a known agent");
    assert_eq!(usage, Usage::default());
}

#[tokio::test]
async fn session_usage_sums_the_agents_own_assistant_turns_and_excludes_the_inherited_prefix() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store.clone());
    let handle = new_handle(&conway).await;

    // Two `Assistant` records appended directly to the ROOT's own log --
    // this is the "inherited prefix" a forked child below will see via
    // `transcript()`, but must NOT see via `session_usage`.
    let root_usage_a = Usage {
        input_tokens: 10,
        output_tokens: 5,
        ..Usage::default()
    };
    let root_usage_b = Usage {
        input_tokens: 3,
        output_tokens: 2,
        cache_read_tokens: 1,
        ..Usage::default()
    };
    store
        .append(&handle.id(), assistant_record(root_usage_a))
        .await
        .expect("append should succeed");
    store
        .append(&handle.id(), assistant_record(root_usage_b))
        .await
        .expect("append should succeed");

    // `session_usage(root)` sums exactly the root's own two records.
    let root_total = handle
        .session_usage(handle.root())
        .await
        .expect("session_usage should resolve for the root");
    assert_eq!(root_total, root_usage_a + root_usage_b);

    // Fork a child -- it inherits the root's WHOLE prefix (the two records
    // above) for `transcript()`, but `session_usage` must count only the
    // child's own turns. Let its own one-shot (echo, zero-usage) turn
    // finish first so no direct-append below races the agent loop's own
    // append.
    let child = handle
        .fork(handle.root(), ForkSpec::new("keep going"))
        .await
        .expect("fork should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), handle.await_agent(child)).await;

    // Sanity: the child's effective `transcript()` really does carry the
    // root's own records as an inherited prefix -- the double-count this
    // item's `session_usage` doc explicitly guards against would only be
    // possible if this held.
    let child_transcript = handle
        .transcript(child)
        .await
        .expect("transcript should resolve");
    assert!(
        child_transcript
            .iter()
            .any(|r| matches!(r, LogRecord::Assistant { usage, .. } if *usage == root_usage_a)),
        "the child's effective transcript must show the root's inherited Assistant record"
    );

    // A large, distinctive usage appended directly to the CHILD's own log,
    // after its real turn has finished.
    let child_usage = Usage {
        input_tokens: 100,
        output_tokens: 50,
        reasoning_tokens: 7,
        ..Usage::default()
    };
    let child_session = store
        .list(conway_core::log::SessionFilter {
            include_ephemeral: true,
            ..Default::default()
        })
        .await
        .expect("list should succeed")
        .into_iter()
        .find(|meta| meta.agent_id == child)
        .map(|meta| meta.id)
        .expect("the forked child must have its own session");
    store
        .append(&child_session, assistant_record(child_usage))
        .await
        .expect("append should succeed");

    let child_total = handle
        .session_usage(child)
        .await
        .expect("session_usage should resolve for the child");
    // The echo backend's own real turn contributes `Usage::default()`
    // (zero), so the child's total is exactly the directly-appended
    // `child_usage` -- critically, NOT `child_usage + root_usage_a +
    // root_usage_b`, which is what summing the effective (ancestry-
    // prefixed) transcript instead would have produced.
    assert_eq!(child_total, child_usage);
    assert_ne!(
        child_total,
        child_usage + root_total,
        "the inherited root prefix must not be double-counted into the child's own total"
    );
}

// ---------------------------------------------------------------------
// No fork/spawn *logic* in session_handle.rs (this item's own criterion).
// ---------------------------------------------------------------------

#[test]
fn subagent_block_in_session_handle_has_no_fork_spawn_logic() {
    let source = include_str!("../src/session_handle.rs");
    const MARKER: &str = "WI-102: subagent surface";
    let idx = source
        .find(MARKER)
        .expect("session_handle.rs must contain the WI-102 subagent-surface marker");
    let new_code = &source[idx..];
    for forbidden in [
        "SessionStore",
        "TranscriptResolver",
        "ContextBuilder",
        "AgentTree",
    ] {
        assert!(
            !new_code.contains(forbidden),
            "the WI-102 subagent block in session_handle.rs must not reference `{forbidden}` -- \
             fork/spawn logic belongs in conway-runtime, not the facade"
        );
    }
}

#[tokio::test]
async fn spawn_without_an_agent_def_inherits_the_parents_role_not_the_literal_default() {
    // Mirrors `fork_without_a_role_inherits_the_parents_role_not_the_literal_default`
    // above: a spawn with no `agent_def` (and no `role`) must resolve its
    // role the same way a roleless fork does -- inherit the PARENT
    // session's role, not the hardcoded literal `"default"`. This is the
    // relaxed WI-099 "agent_def mandatory for spawn" rule's actual runtime
    // behavior: `agent_def: None` no longer fails `SubagentSpec::validate`,
    // and `conway_runtime`'s `SubagentHost::start` routes exactly like a
    // roleless fork (`spec.role -> agent_def.role (skipped, none) ->
    // parent_meta.role -> literal "default"`).
    let mut roles = BTreeMap::new();
    roles.insert(
        "coder".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    let config = ConwayConfig {
        default_role: RoleAlias::new("coder"),
        roles,
        ..base_config()
    };
    let store = Arc::new(FakeStore::new());
    let conway = ConwayBuilder::from_parts(config)
        .with_backend(Arc::new(FakeBackend::echo(BackendId::new("fake"))))
        .with_session_store(store.clone())
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(fake_router())
        .build()
        .expect("build should succeed");
    // The root's role defaults to config.default_role ("coder").
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");

    // Spawn with NO agent_def and NO role.
    let child = handle
        .spawn(handle.root(), SpawnSpec::new("please review"))
        .await
        .expect("spawn should succeed even with no agent_def");

    let tree = handle.tree();
    let node = tree
        .nodes
        .iter()
        .find(|n| n.agent_id == child)
        .expect("child must be attached to the tree");
    assert_eq!(
        node.role,
        Some(RoleAlias::new("coder")),
        "a spawn with no agent_def must inherit the parent's role, not the literal \"default\""
    );
    assert_eq!(node.mode, Some(SubagentMode::Spawn));

    let _ = tokio::time::timeout(Duration::from_secs(5), handle.await_agent(child)).await;
}

// ---------------------------------------------------------------------
// `keep_alive` interactive children + `prompt_agent` (WI "bare /spawn &
// /fork open an interactive session")
// ---------------------------------------------------------------------

/// Resolves `agent`'s own `SessionId` directly against `store` -- the same
/// lookup `session_usage`'s own test above already performs, factored out
/// here since every test below needs it.
async fn child_session(store: &FakeStore, agent: AgentId) -> SessionId {
    store
        .list(conway_core::log::SessionFilter {
            include_ephemeral: true,
            ..Default::default()
        })
        .await
        .expect("list should succeed")
        .into_iter()
        .find(|meta| meta.agent_id == agent)
        .map(|meta| meta.id)
        .expect("the child must have its own session")
}

#[tokio::test]
async fn keep_alive_spawn_starts_idle_with_no_own_records_then_runs_and_is_repromptable() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store.clone());
    let handle = new_handle(&conway).await;

    // Bare `/spawn`'s own shape: empty prompt, `keep_alive: true`.
    let child = handle
        .spawn(handle.root(), SpawnSpec::new("").keep_alive(true))
        .await
        .expect("keep-alive spawn should succeed");

    // Idle: no placeholder head record was written, so the child's own
    // session has zero records of its own -- proves the child never ran a
    // spontaneous turn against blank input (unlike the old hardcoded
    // `keep_alive: false` path, whose head record IS its own first turn).
    let session = child_session(&store, child).await;
    assert_eq!(
        store.head(&session).await.expect("head should resolve"),
        LogSeq::ZERO,
        "an idle keep-alive child must have no own records until its first prompt"
    );

    // First prompt: wakes the gated first iteration and runs a real turn.
    let turn1 = handle
        .prompt_agent(child, "first message")
        .await
        .expect("prompt_agent should drive the idle child's first turn");
    let text1 = tokio::time::timeout(Duration::from_secs(5), turn1.text())
        .await
        .expect("turn1.text() must not hang")
        .expect("turn1.text() should resolve");
    assert_eq!(text1, "first message", "echo backend echoes the prompt");

    // Re-promptable: the child's task must still be alive for a SECOND
    // turn, not have finished after the first (the whole point of
    // `keep_alive`).
    let turn2 = handle
        .prompt_agent(child, "second message")
        .await
        .expect("a keep-alive child must accept a second prompt");
    let text2 = tokio::time::timeout(Duration::from_secs(5), turn2.text())
        .await
        .expect("turn2.text() must not hang")
        .expect("turn2.text() should resolve");
    assert_eq!(text2, "second message");
}

#[tokio::test]
async fn keep_alive_fork_starts_idle_inherits_context_then_runs_and_is_repromptable() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store.clone());
    let handle = new_handle(&conway).await;

    // Give the root a real turn first, so the fork below has something to
    // inherit.
    let seed = handle.prompt("seed message").await.expect("prompt");
    tokio::time::timeout(Duration::from_secs(5), seed.text())
        .await
        .expect("seed turn must not hang")
        .expect("seed turn should resolve");

    // Bare `/fork`'s own shape: empty directive, `keep_alive: true`.
    let child = handle
        .fork(handle.root(), ForkSpec::new("").keep_alive(true))
        .await
        .expect("keep-alive fork should succeed");

    let session = child_session(&store, child).await;
    assert_eq!(
        store.head(&session).await.expect("head should resolve"),
        LogSeq::ZERO,
        "an idle keep-alive fork child must have no own records until its first prompt"
    );

    let turn1 = handle
        .prompt_agent(child, "fork first message")
        .await
        .expect("prompt_agent should drive the idle fork child's first turn");
    let text1 = tokio::time::timeout(Duration::from_secs(5), turn1.text())
        .await
        .expect("turn1.text() must not hang")
        .expect("turn1.text() should resolve");
    assert_eq!(text1, "fork first message");

    // Inherited context still reaches the child: its effective transcript
    // (ancestry-prefixed) includes the root's pre-fork "seed message" turn.
    let child_transcript = handle
        .transcript(child)
        .await
        .expect("transcript should resolve");
    assert!(
        child_transcript
            .iter()
            .any(|r| matches!(r, LogRecord::UserTurn { text, .. } if text == "seed message")),
        "a keep-alive fork child must still inherit the parent's pre-fork transcript"
    );

    // Re-promptable across a second turn, same as the spawn case above.
    let turn2 = handle
        .prompt_agent(child, "fork second message")
        .await
        .expect("a keep-alive fork child must accept a second prompt");
    let text2 = tokio::time::timeout(Duration::from_secs(5), turn2.text())
        .await
        .expect("turn2.text() must not hang")
        .expect("turn2.text() should resolve");
    assert_eq!(text2, "fork second message");
}

#[tokio::test]
async fn prompt_agent_drives_a_named_non_root_agents_turn_not_the_root() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    let child = handle
        .spawn(handle.root(), SpawnSpec::new("").keep_alive(true))
        .await
        .expect("keep-alive spawn should succeed");

    let turn = handle
        .prompt_agent(child, "hello child")
        .await
        .expect("prompt_agent should succeed");
    let text = tokio::time::timeout(Duration::from_secs(5), turn.text())
        .await
        .expect("text() must not hang")
        .expect("text() should resolve");
    assert_eq!(text, "hello child");

    // The root's own transcript must be untouched -- `prompt_agent`
    // targeted the child, not the root.
    let root_transcript = handle
        .transcript(handle.root())
        .await
        .expect("root transcript should resolve");
    assert!(
        !root_transcript
            .iter()
            .any(|r| matches!(r, LogRecord::UserTurn { text, .. } if text == "hello child")),
        "prompt_agent must not have prompted the root"
    );
}

#[tokio::test]
async fn autonomous_spawn_and_fork_default_keep_alive_false_and_finish_after_one_turn() {
    // The existing library behavior (a plain `SpawnSpec`/`ForkSpec`, no
    // `.keep_alive(true)`) must be entirely unchanged by this item: the
    // child still runs its one real-prompt turn immediately and finishes
    // (`await_agent` resolves), rather than idling for a second prompt that
    // will never come.
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    let spawned = handle
        .spawn(handle.root(), SpawnSpec::new("do it"))
        .await
        .expect("spawn should succeed");
    let spawn_result = tokio::time::timeout(Duration::from_secs(5), handle.await_agent(spawned))
        .await
        .expect("await_agent must not hang for a non-keep-alive spawn")
        .expect("await_agent should resolve");
    assert_eq!(spawn_result.status, ResultStatus::Completed);

    let forked = handle
        .fork(handle.root(), ForkSpec::new("do it too"))
        .await
        .expect("fork should succeed");
    let fork_result = tokio::time::timeout(Duration::from_secs(5), handle.await_agent(forked))
        .await
        .expect("await_agent must not hang for a non-keep-alive fork")
        .expect("await_agent should resolve");
    assert_eq!(fork_result.status, ResultStatus::Completed);
}

// ---------------------------------------------------------------------
// cancel_with(): the VERIFICATION ANCHOR for board item
// 01KZDC2222ARKMZKN8ZE4BYHD6 ("wire graceful cancellation") -- graceful
// cancellation, driven through a PUBLIC entry point (the facade), not the
// mailbox classifier `conway-runtime`'s own `tests/steering.rs:717`
// exercises (that test builds the loop harness directly; it proves nothing
// about whether any caller-reachable path can ever produce a soft
// `AgentMessage::Cancel` at all -- before this item, none did).
// ---------------------------------------------------------------------

/// Notifies `started` the instant `invoke` begins, then blocks on `release`
/// -- gives a test a deterministic window in which a child is known to be
/// mid-TOOL-EXECUTION (not merely mid-backend-call, like
/// `DelayedEchoBackend` above), so a cancel enqueued during that window
/// races a real, in-flight tool call rather than a sleep-based guess.
struct SlowTool {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Tool for SlowTool {
    fn spec(&self) -> conway_core::content::ToolSpec {
        conway_core::content::ToolSpec {
            name: conway_core::ids::ToolName::new("slow_tool"),
            description: "test-only slow tool".into(),
            schema: serde_json::from_value(serde_json::json!({"type": "object"}))
                .expect("a trivial object schema always parses"),
            category: conway_core::content::ToolCategory::Read,
            permission: conway_core::content::PermissionClass::Safe,
        }
    }

    async fn invoke(
        &self,
        _call: ToolCall,
        _ctx: conway_core::ports::ToolCtx,
    ) -> Result<conway_core::ports::ToolOutput, conway_core::error::ToolError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(conway_core::ports::ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "slow tool done".to_string(),
            }],
            is_error: false,
            truncation: conway_core::content::TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

/// Registers [`SlowTool`] as the sole tool of a test-only plugin -- mirrors
/// `keep_alive.rs`'s identical `FixtureToolsPlugin` pattern (duplicated
/// locally rather than shared: each integration test binary is its own
/// crate root).
struct SlowToolPlugin(Arc<SlowTool>);

impl Plugin for SlowToolPlugin {
    fn manifest(&self) -> conway_core::ports::PluginManifest {
        conway_core::ports::PluginManifest {
            id: "test.slow-tool".to_string(),
            version: "0.0.0".to_string(),
            tools: vec![conway_core::ids::ToolName::new("slow_tool")],
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![self.0.clone() as Arc<dyn Tool>]
    }
}

/// Mirrors `subagent_control_seam.rs`'s identical helper -- a
/// `GenerateResponse` carrying exactly one tool call, no text content.
fn tool_call_response(tool: &str, arguments: serde_json::Value) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: "call_1".to_string(),
            name: conway_core::ids::ToolName::new(tool),
            arguments,
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

/// Like [`build_conway`], but also registers `plugin` -- for the two tests
/// below, which need a real, in-flight tool call to cancel against.
fn build_conway_with_plugin(
    backend: Arc<dyn Backend>,
    store: Arc<FakeStore>,
    plugin: Arc<dyn Plugin>,
) -> Conway {
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .with_plugin(plugin)
        .build()
        .expect("build should succeed with every port injected")
}

#[tokio::test]
async fn graceful_cancel_through_the_facade_lets_the_in_flight_tool_finish_then_stops() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let tool = Arc::new(SlowTool {
        started: started.clone(),
        release: release.clone(),
    });
    // Scripted for exactly ONE turn: if a graceful cancel failed to stop
    // BEFORE the next backend round trip, the loop would ask this backend
    // for a second response, find the script exhausted, and fail the turn
    // outright -- a loud, unambiguous signal that the "backend called
    // exactly once" guarantee broke, not a silent pass.
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(tool_call_response(
            "slow_tool",
            serde_json::json!({}),
        ))])
        .with_id(BackendId::new("fake")),
    );
    let store = Arc::new(FakeStore::new());
    let conway =
        build_conway_with_plugin(backend.clone(), store, Arc::new(SlowToolPlugin(tool)));
    let handle = new_handle(&conway).await;

    let child = handle
        .fork(handle.root(), ForkSpec::new("go"))
        .await
        .expect("fork should succeed");

    // Wait for the child's tool call to actually be in flight before
    // cancelling -- a genuine race against a real suspension point, not a
    // sleep-based guess.
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("the slow tool must start");

    handle
        .cancel_with(child, "let it finish", CancelMode::Graceful)
        .await
        .expect("graceful cancel should succeed");

    // Let the tool actually finish -- a graceful cancel must not abort it.
    release.notify_one();

    let result = tokio::time::timeout(Duration::from_secs(5), handle.await_agent(child))
        .await
        .expect("await_agent must not hang after a graceful cancel")
        .expect("await_agent should resolve Ok even on Cancelled");
    assert_eq!(
        result.status,
        ResultStatus::Cancelled {
            reason: "let it finish".to_string()
        },
        "a graceful cancel must land with the supplied reason, at the next turn boundary"
    );

    let transcript = handle
        .transcript(child)
        .await
        .expect("transcript should resolve");
    assert!(
        transcript.iter().any(|r| matches!(
            r,
            LogRecord::ToolResultRecord { result, .. } if result.call_id == "call_1"
        )),
        "the in-flight tool's own result must have been persisted before the graceful cancel \
         landed -- that is exactly the 'finishes its current turn' contract"
    );
    assert_eq!(
        backend.calls().len(),
        1,
        "a graceful cancel must stop BEFORE the next backend round trip -- only the turn \
         already in flight when cancel was sent may complete"
    );
}

#[tokio::test]
async fn default_cancel_through_the_facade_stops_immediately_without_waiting_for_the_tool() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let tool = Arc::new(SlowTool {
        started: started.clone(),
        release: release.clone(),
    });
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(tool_call_response(
            "slow_tool",
            serde_json::json!({}),
        ))])
        .with_id(BackendId::new("fake")),
    );
    let store = Arc::new(FakeStore::new());
    let conway =
        build_conway_with_plugin(backend.clone(), store, Arc::new(SlowToolPlugin(tool)));
    let handle = new_handle(&conway).await;

    let child = handle
        .fork(handle.root(), ForkSpec::new("go"))
        .await
        .expect("fork should succeed");

    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("the slow tool must start");

    // The default form -- `cancel`, no mode -- must resolve `Cancelled`
    // WITHOUT `release` ever being notified: it must not wait for the
    // in-flight tool to finish.
    handle
        .cancel(child, "test requested cancellation")
        .await
        .expect("cancel should succeed");

    let result = tokio::time::timeout(Duration::from_secs(2), handle.await_agent(child))
        .await
        .expect("the default cancel path must resolve promptly, without waiting on the tool")
        .expect("await_agent should resolve Ok even on Cancelled");
    assert!(
        matches!(result.status, ResultStatus::Cancelled { .. }),
        "expected Cancelled, got {:?}",
        result.status
    );

    let transcript = handle
        .transcript(child)
        .await
        .expect("transcript should resolve");
    assert!(
        !transcript
            .iter()
            .any(|r| matches!(r, LogRecord::ToolResultRecord { .. })),
        "an immediate cancel must abort the in-flight tool call outright -- its result must \
         never be persisted, unlike the graceful path"
    );

    // Release the tool so its (already-dropped, by `ToolRunner::run_batch`'s
    // own `select!`) future does not matter either way -- a no-op given the
    // assertions above already passed, kept only so nothing here reads as
    // silently relying on the tool eventually finishing.
    release.notify_one();
}
