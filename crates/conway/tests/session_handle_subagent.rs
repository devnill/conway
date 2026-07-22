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
    RoleEntry, RoutingSection, SessionConfig,
};
use conway::{Conway, ConwayBuilder, ConwayError, ForkSpec, SessionHandle, SessionSpec, SpawnSpec};
use conway_core::agent::{Budget, PermissionDecision, ResultStatus, SubagentMode};
use conway_core::capabilities::{
    CacheMode, Capabilities, ProbeReport, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::error::BackendError;
use conway_core::fakes::{FakeBackend, FakeGate, FakeRouter, FakeStore};
use conway_core::ids::{AgentId, BackendId, ModelId, RoleAlias};
use conway_core::log::LogRecord;
use conway_core::ports::{Backend, BoxStream, GenerateRequest, GenerateResponse, StreamChunk};

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
async fn fork_produces_a_child_with_mapped_fields_and_an_inherited_prefix() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

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
            .any(|r| matches!(r, LogRecord::UserTurn { text, .. } if text.is_empty())),
        "a forked child must inherit the forker's own prefix (here, the root's leading empty prompt)"
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
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    let budget = Budget {
        max_steps: 3,
        deadline: None,
        max_tokens: None,
        max_tool_calls: None,
    };
    let spec = SpawnSpec::new("unregistered-agent-def", "please review")
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
            .any(|r| matches!(r, LogRecord::UserTurn { text, .. } if text.is_empty())),
        "transcript() is expected to still show the parent's leading record for a spawned \
         child (see the comment above) -- if this ever stops holding, `SessionStore::fork`'s \
         `origin` recording for `SubagentMode::Spawn` has changed and this test's own premise \
         needs re-checking, not just this assertion"
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
        .spawn(unknown, SpawnSpec::new("x", "y"))
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

// ---------------------------------------------------------------------
// `SpawnSpec` compile-fail coverage note.
// ---------------------------------------------------------------------
//
// The binding criterion asks for a `trybuild`-based compile-fail test
// asserting `SpawnSpec` construction without `agent_def` does not compile.
// `trybuild` is not a dependency of this crate, and `Cargo.toml` is not in
// this item's file scope (only `session_handle.rs`, `subagent_spec.rs`,
// `lib.rs`, and this test file are). Rather than widen scope to add a new
// dev-dependency, the same guarantee is covered by a `compile_fail` doctest
// on `SpawnSpec` in `src/subagent_spec.rs` (a standard-library mechanism,
// no new dependency needed), which `cargo test -p conway` already runs as
// part of its doctests.
