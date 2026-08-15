//! Acceptance tests for `Runtime::ask` (item c of the conway_ask epic):
//! the real `SubagentHost::ask` impl -- subscribe-before-launch,
//! agent-id-checked `TextDelta`/`AgentFinished` drain, full (untruncated)
//! text in `AskOutcome`, and `spec.ephemeral` plumbed into the child's
//! `SessionMeta` (and thus `AgentSpawned`/`AgentFinished`).
//!
//! These tests deliberately use a small local backend (`AskBackend`) rather
//! than `conway_testkit::ScriptedBackend` so each turn's response can
//! carry MULTIPLE `ContentBlock::Text` blocks -- which `run_stream`
//! (`attempt.rs`) emits as one `Event::TextDelta` per block -- AND a
//! per-turn pre-response delay, which lets the sibling-finish test inject a
//! synthetic `AgentFinished` onto the bus in a deterministic window DURING
//! the child's drain (proving the drain's agent-id check does not resolve on
//! a sibling's finish).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway_core::agent::{Budget, PermissionDecision, ResultStatus, SubagentMode, SubagentSpec};
use conway_core::capabilities::{HeadroomPolicy, ProbeReport};
use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::error::BackendError;
use conway_core::event::Event;
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias};
use conway_core::ports::{
    Backend, BoxStream, GenerateRequest, GenerateResponse, Router, SessionStore, StreamChunk,
    SubagentHost,
};
use conway_core::provenance::Provenance;
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{RootSpec, Runtime, RuntimeDeps};
use conway_testkit::{FakeGate, FakeHealth, FakeRouter};
use futures::{stream, StreamExt};
use tokio::time::sleep;

// ---------------------------------------------------------------------
// A small per-turn-delay backend
// ---------------------------------------------------------------------

/// One scripted turn for [`AskBackend`]: a response built from `content`
/// (one `Event::TextDelta` per `ContentBlock::Text`), emitted after `delay`
/// elapses. `usage` is what the agent loop stamps into `Event::TurnFinished`.
#[derive(Clone)]
struct AskTurn {
    content: Vec<ContentBlock>,
    delay: Duration,
    usage: Usage,
}

impl AskTurn {
    /// A turn that responds with a single text block after `delay`.
    fn text(text: impl Into<String>, delay: Duration) -> Self {
        Self {
            content: vec![ContentBlock::Text { text: text.into() }],
            delay,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            },
        }
    }

    /// A turn that responds with several text blocks (one `TextDelta` each)
    /// after `delay`. Used to prove the drain concatenates the FULL reply.
    fn deltas(blocks: &[&str], delay: Duration) -> Self {
        Self {
            content: blocks
                .iter()
                .map(|s| ContentBlock::Text {
                    text: s.to_string(),
                })
                .collect(),
            delay,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            },
        }
    }

    fn response(&self) -> GenerateResponse {
        GenerateResponse {
            content: self.content.clone(),
            tool_calls: vec![],
            stop: StopReason::EndTurn,
            usage: self.usage,
        }
    }
}

/// A backend that plays back a fixed script of [`AskTurn`]s in order. Each
/// turn sleeps for its configured `delay` before producing its response, so a
/// test can stage events from multiple agents in a deterministic order. The
/// stream path emits one `StreamChunk::TextDelta` per `ContentBlock::Text`
/// (then `StreamChunk::Done`), exactly mirroring `conway_testkit`'
/// `decompose_to_chunks` -- so `run_stream` (`attempt.rs`) emits one
/// `Event::TextDelta` per text block, the shape `Runtime::ask`'s drain
/// concatenates.
struct AskBackend {
    id: BackendId,
    script: Mutex<VecDeque<AskTurn>>,
}

impl AskBackend {
    fn new(id: BackendId, script: Vec<AskTurn>) -> Self {
        Self {
            id,
            script: Mutex::new(script.into()),
        }
    }
}

#[async_trait]
impl Backend for AskBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn capabilities(&self, _model: &ModelId) -> conway_core::capabilities::Capabilities {
        conway_core::capabilities::Capabilities {
            // `ToolCallSupport::None` so the attempt engine picks the
            // streaming path (`strategy_for`: no tools -> Stream), which is
            // what emits `Event::TextDelta` per `StreamChunk::TextDelta`.
            tool_calling: conway_core::capabilities::ToolCallSupport::None,
            cache: conway_core::capabilities::CacheMode::None,
            parallel_tool_calls: false,
            structured_output: conway_core::capabilities::StructuredOutput::None,
            max_context_tokens: 128_000,
            reasoning: false,
            reliability_tier: conway_core::capabilities::ReliabilityTier::Unknown,
        }
    }

    async fn generate(&self, req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
        let _ = req;
        let turn =
            self.script
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| BackendError::BadRequest {
                    detail: "ask backend script exhausted".into(),
                })?;
        sleep(turn.delay).await;
        Ok(turn.response())
    }

    async fn stream(
        &self,
        req: GenerateRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
        let response = self.generate(req).await?;
        // One TextDelta per text block, then Done -- mirrors
        // `conway_testkit::decompose_to_chunks` (private there).
        let mut chunks: Vec<Result<StreamChunk, BackendError>> = response
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(Ok(StreamChunk::TextDelta(text.clone()))),
                ContentBlock::Thinking { text, .. } => {
                    Some(Ok(StreamChunk::ThinkingDelta(text.clone())))
                }
                _ => None,
            })
            .collect();
        chunks.push(Ok(StreamChunk::Done(response)));
        Ok(Box::pin(stream::iter(chunks)))
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
// Fixtures
// ---------------------------------------------------------------------

fn build_runtime_with_backend(backend: Arc<dyn Backend>, bus: Arc<EventBus>) -> Arc<Runtime> {
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);
    let store: Arc<dyn SessionStore> = Arc::new(conway_testkit::FakeStore::new());

    Runtime::new(RuntimeDeps {
        store,
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        event_bus: bus,
        headroom: Arc::new(HeadroomPolicy::default()),
    })
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

/// The fork spec `conway_ask`'s AskTool will pass (item d sets `ephemeral:
/// true`; here we set it explicitly to exercise the `spec.ephemeral` ->
/// `SessionMeta.ephemeral` plumbing this item owns).
fn ask_fork_spec(prompt: &str) -> SubagentSpec {
    SubagentSpec {
        mode: SubagentMode::Fork,
        prompt: prompt.to_string(),
        agent_def: None,
        role: None,
        tools: None,
        budget: Budget::default(),
        result_contract: None,
        keep_alive: false,
        ephemeral: true,
        ask_origin: None,
        cwd: None,
        root: None,
        tag: None,
    }
}

async fn start_and_finish_root(runtime: &Runtime, prompt: &str) -> AgentId {
    let mut stream = runtime.subscribe();
    let root = runtime.start_root(root_spec(prompt)).await.unwrap();
    // Drain until the root's AgentFinished -- same idiom as
    // `subagent_fork_spawn.rs::wait_for_agent_finished`.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope = stream.next().await.expect("event stream ended early");
            if envelope.agent == root {
                if let Event::AgentFinished { .. } = envelope.event {
                    return;
                }
            }
        }
    })
    .await
    .expect("root never finished");
    root
}

// ---------------------------------------------------------------------
// Acceptance: ask returns FULL concatenated text, status, transcript_ref
// ---------------------------------------------------------------------

#[tokio::test]
async fn ask_returns_full_text_status_completed_and_transcript_ref() {
    // Root turn (immediate), then the child's turn: 3 TextDeltas
    // "Hello ", "world", "!" -- one per ContentBlock::Text -- emitted after a
    // tiny delay so the drain is observably live when they arrive.
    let backend = Arc::new(AskBackend::new(
        BackendId::new("b"),
        vec![
            AskTurn::text("root ok", Duration::ZERO),
            AskTurn::deltas(&["Hello ", "world", "!"], Duration::from_millis(20)),
        ],
    ));
    let bus = EventBus::with_default_capacity();
    let runtime = build_runtime_with_backend(backend.clone(), bus);

    let parent = start_and_finish_root(&runtime, "investigate").await;

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.ask(parent, parent, ask_fork_spec("say hi")),
    )
    .await
    .expect("ask did not resolve")
    .expect("ask errored");

    // FULL concatenated text -- NOT truncated, NOT just the last delta.
    assert_eq!(outcome.text, "Hello world!");
    assert_eq!(outcome.status, ResultStatus::Completed);
    // `usage` is the child's cumulative usage across its whole run, taken
    // from the terminal `AgentResult::usage` (NOT a single `TurnFinished`
    // slice). The child ran one turn with `AskTurn::deltas`'s usage
    // { input_tokens: 10, output_tokens: 5 }.
    assert_eq!(
        outcome.usage,
        Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        }
    );
    // transcript_ref is the child's SessionId, resolvable via the same
    // agent->session lookup `start` uses.
    let child_session = runtime
        .tree()
        .nodes
        .iter()
        .find(|n| n.parent == Some(parent))
        .expect("child node present in tree")
        .session;
    assert_eq!(outcome.transcript_ref, child_session);
}

// ---------------------------------------------------------------------
// enforcement: `ask` with a non-fork mode is a typed error, at the
// TRAIT boundary, in BOTH debug and release builds (the invariant this
// item replaces a `debug_assert!` for -- a `debug_assert!` alone compiles
// to nothing in release, which is every binary a user runs).
// ---------------------------------------------------------------------

#[tokio::test]
async fn ask_with_spawn_mode_returns_typed_error_not_panic() {
    // No backend turn is ever consumed for the (rejected) child -- an empty
    // script proves the rejection happens before any agent runs.
    let backend = Arc::new(AskBackend::new(
        BackendId::new("b"),
        vec![AskTurn::text("root ok", Duration::ZERO)],
    ));
    let bus = EventBus::with_default_capacity();
    let runtime = build_runtime_with_backend(backend, bus);

    let parent = start_and_finish_root(&runtime, "investigate").await;

    let mut spawn_spec = ask_fork_spec("say hi");
    spawn_spec.mode = SubagentMode::Spawn;

    let err = runtime
        .ask(parent, parent, spawn_spec)
        .await
        .expect_err("ask must reject a non-fork mode with a typed error");

    assert_eq!(
        err,
        conway_core::error::RuntimeError::AskRequiresFork {
            mode: SubagentMode::Spawn,
        }
    );

    // No child was ever attached to the tree -- the rejection happened
    // before `start` was called.
    assert!(
        runtime
            .tree()
            .nodes
            .iter()
            .all(|n| n.parent != Some(parent)),
        "no child should have been started for a rejected ask spec"
    );
}

// ---------------------------------------------------------------------
// Acceptance: the drain does NOT resolve on a sibling's AgentFinished
// ---------------------------------------------------------------------

#[tokio::test]
async fn ask_drain_ignores_sibling_agent_finished() {
    // Two children of the same parent. The sibling's turn completes quickly
    // (10ms); the child's turn is slower (200ms) so the sibling's
    // `AgentFinished` lands on the bus DURING the child's drain, BEFORE the
    // child's own `AgentFinished`. The drain must ignore the sibling's
    // finish (different `envelope.agent` / `result.agent_id`) and keep
    // accumulating the child's TextDeltas until the child's own finish.
    //
    // The sibling is started via `SubagentHost::start` (real, not synthetic):
    // its AgentFinished is a genuine supervisor emission, not a test
    // injection -- so this exercises the exact same `AgentFinished` shape
    // `ask`'s drain sees in production.
    let backend = Arc::new(AskBackend::new(
        BackendId::new("b"),
        vec![
            AskTurn::text("root ok", Duration::ZERO),
            // Sibling turn (consumed first, after `start(sibling)`).
            AskTurn::text("sibling ok", Duration::from_millis(10)),
            // Child turn (consumed second, inside `ask`).
            AskTurn::deltas(&["child ", "says ", "hi"], Duration::from_millis(200)),
        ],
    ));
    let bus = EventBus::with_default_capacity();
    let runtime = build_runtime_with_backend(backend.clone(), bus);

    let parent = start_and_finish_root(&runtime, "investigate").await;

    // Start the sibling (a real fork child of `parent`) but DO NOT await its
    // finish -- it runs concurrently with the child `ask` launches below.
    let mut sibling_stream = runtime.subscribe();
    let sibling = runtime
        .start(
            parent,
            parent,
            SubagentSpec::fork("sibling prompt", Budget::default()),
        )
        .await
        .expect("sibling start");

    // Drive `ask` for the child. `ask` subscribes BEFORE it launches the
    // child, so the sibling's `AgentFinished` (which arrives ~10ms after
    // `start(sibling)` returned, well within the child's 200ms turn) is
    // observed by the child's drain and MUST be ignored.
    let ask_fut = runtime.ask(parent, parent, ask_fork_spec("child prompt"));
    let outcome = tokio::time::timeout(Duration::from_secs(10), ask_fut)
        .await
        .expect("ask did not resolve")
        .expect("ask errored");

    // Confirm the sibling actually finished (its AgentFinished was emitted
    // on the bus) -- otherwise the test would not prove the drain ignored it.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope = sibling_stream.next().await.expect("stream ended early");
            if envelope.agent == sibling {
                if let Event::AgentFinished { .. } = envelope.event {
                    return;
                }
            }
        }
    })
    .await
    .expect("sibling never finished");

    // The sibling DID finish -- and yet `ask` returned the CHILD's text, not
    // an empty string and not the sibling's "sibling ok".
    assert_eq!(outcome.text, "child says hi");
    assert_eq!(outcome.status, ResultStatus::Completed);
}

// ---------------------------------------------------------------------
// Acceptance: subscribe BEFORE launch -> first TextDelta is not missed
// ---------------------------------------------------------------------

#[tokio::test]
async fn ask_subscribes_before_launch_so_first_text_delta_is_not_missed() {
    // HAPPY-PATH SMOKE TEST (not a true race guard). The child emits a SINGLE
    // early TextDelta; `ask` subscribes before `start`, so the delta is
    // caught and `outcome.text` carries it. Caveat: this test cannot on its
    // own PROVE the subscribe-before-launch ordering, because `Runtime::start`
    // returns only once `launch_agent` has *spawned* the child's tokio task
    // (not polled it) -- the child's first TextDelta is emitted from inside
    // that spawned task's async body, which the executor cannot poll until
    // `start` returns control to `ask`. So even a reversed order (subscribe
    // after `start`) would still see the subscriber in place before the
    // first delta, and this test would stay green. The real protection is
    // the code-level ordering -- `let mut stream = Runtime::subscribe(self);`
    // is literally the first statement of `Runtime::ask`, before
    // `self.start(parent, spec).await?` -- verified by code review (see the
    // item-c adversarial review). This test stays as a regression guard that
    // the single-delta happy path returns full text (it would catch a
    // broken drain that dropped the only delta), not as a race proof.
    let backend = Arc::new(AskBackend::new(
        BackendId::new("b"),
        vec![
            AskTurn::text("root ok", Duration::ZERO),
            AskTurn::text("first-and-only delta", Duration::ZERO),
        ],
    ));
    let bus = EventBus::with_default_capacity();
    let runtime = build_runtime_with_backend(backend, bus);

    let parent = start_and_finish_root(&runtime, "investigate").await;

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.ask(parent, parent, ask_fork_spec("say hi")),
    )
    .await
    .expect("ask did not resolve")
    .expect("ask errored");

    assert_eq!(outcome.text, "first-and-only delta");
    assert_eq!(outcome.status, ResultStatus::Completed);
}

// ---------------------------------------------------------------------
// Acceptance: spec.ephemeral -> child SessionMeta.ephemeral -> AgentFinished
// ---------------------------------------------------------------------

#[tokio::test]
async fn ask_child_emits_agent_finished_with_ephemeral_true() {
    // `ask_fork_spec` sets `ephemeral: true`; this item's change at
    // `subagent.rs` (the `SessionMeta { ephemeral: spec.ephemeral }` literal)
    // must stamp the child's session header `ephemeral: true`, which flows
    // through `AgentNode.ephemeral` to `Event::AgentFinished::ephemeral`.
    let backend = Arc::new(AskBackend::new(
        BackendId::new("b"),
        vec![
            AskTurn::text("root ok", Duration::ZERO),
            AskTurn::text("child ok", Duration::ZERO),
        ],
    ));
    let bus = EventBus::with_default_capacity();
    let runtime = build_runtime_with_backend(backend, bus);

    let parent = start_and_finish_root(&runtime, "investigate").await;

    let mut stream = runtime.subscribe();
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.ask(parent, parent, ask_fork_spec("say hi")),
    )
    .await
    .expect("ask did not resolve")
    .expect("ask errored");

    // Find the child's AgentFinished on the bus and assert `ephemeral: true`.
    let child_session = outcome.transcript_ref;
    let mut saw_ephemeral_finish = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope = stream.next().await.expect("event stream ended early");
            if let Event::AgentFinished { result, ephemeral } = envelope.event {
                if result.transcript_ref == child_session {
                    assert!(ephemeral, "child AgentFinished must be ephemeral");
                    saw_ephemeral_finish = true;
                    return;
                }
            }
        }
    })
    .await
    .expect("child AgentFinished never observed");
    assert!(saw_ephemeral_finish);
}

// ---------------------------------------------------------------------
// Acceptance: a cancelled child still resolves the drain (NO HANG)
// ---------------------------------------------------------------------

#[tokio::test]
async fn ask_drain_resolves_with_cancelled_status_when_parent_is_cancelled() {
    // The `SubagentHost::ask` contract (`conway-core/src/ports/subagent.rs`):
    // ask ALWAYS terminates -- a cancelled child emits `AgentFinished` with
    // `status: Cancelled` (its own `finish_cancelled`, or the supervisor's
    // grace-timeout synthesized finish if the loop itself cannot) and the
    // drain resolves on it, returning `AskOutcome { status: Cancelled, .. }`.
    //
    // The parent (root) is cancelled BEFORE `ask`: `start` derives the
    // child's cancel token from the parent's (`tree.child_cancel_token`), so
    // the child is born cancelled and never consumes its 60s-delayed backend
    // turn. That delay is the no-hang guard: if the drain waited on the
    // backend response (or on a Completed finish) instead of resolving on the
    // Cancelled `AgentFinished`, the 5s timeout below would trip first.
    let backend = Arc::new(AskBackend::new(
        BackendId::new("b"),
        vec![
            AskTurn::text("root ok", Duration::ZERO),
            AskTurn::text("unreachable", Duration::from_secs(60)),
        ],
    ));
    let bus = EventBus::with_default_capacity();
    let runtime = build_runtime_with_backend(backend, bus);

    let parent = start_and_finish_root(&runtime, "investigate").await;
    runtime
        .cancel(parent, "test: parent cancelled before ask".to_string())
        .expect("cancel root");

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.ask(parent, parent, ask_fork_spec("say hi")),
    )
    .await
    .expect("ask hung on a cancelled child")
    .expect("ask errored");

    assert!(
        matches!(outcome.status, ResultStatus::Cancelled { .. }),
        "expected Cancelled, got {:?}",
        outcome.status
    );
    // No turn ever ran: no TextDeltas accumulated, zero usage -- but
    // `transcript_ref` still names the (empty) child session ().
    assert_eq!(outcome.text, "");
    assert_eq!(outcome.usage, Usage::default());
    let child_session = runtime
        .tree()
        .nodes
        .iter()
        .find(|n| n.parent == Some(parent))
        .expect("child node present in tree")
        .session;
    assert_eq!(outcome.transcript_ref, child_session);
}

// ---------------------------------------------------------------------
// the `conway_ask` TOOL path (the
// `Runtime::ask` trait method `AskTool::invoke` -- `conway-tools`' --
// drives directly) must fill `agent_def` from the parent's own
// `SessionMeta` when the call site leaves it `None`, exactly like
// `conway`'s `SessionHandle::ask` already hardcodes for the facade path.
// ---------------------------------------------------------------------

fn restrictive_asker_def() -> conway_core::config::AgentDef {
    conway_core::config::AgentDef {
        name: "asker".to_string(),
        description: None,
        system_prompt: "You are a careful asker.".to_string(),
        role: None,
        model: None,
        tools: conway_core::agent::ToolSelector::Only(vec!["marker".to_string()]),
        skills: Vec::new(),
        max_steps: None,
        result_contract: None,
    }
}

fn build_runtime_with_backend_and_defs(
    backend: Arc<dyn Backend>,
    bus: Arc<EventBus>,
    agent_defs: HashMap<String, conway_core::config::AgentDef>,
) -> Arc<Runtime> {
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);
    let store: Arc<dyn SessionStore> = Arc::new(conway_testkit::FakeStore::new());

    Runtime::new(RuntimeDeps {
        store,
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs,
        event_bus: bus,
        headroom: Arc::new(HeadroomPolicy::default()),
    })
}

/// **Part 2 guard, shown to fail
/// before the fix.** The parent is started from a def (`"asker"`) with a
/// restrictive `tools: Only(["marker"])` selector; `ask_fork_spec` mirrors
/// EXACTLY what `AskTool::invoke` (`conway-tools`' `subagent/ask.rs`)
/// builds -- `agent_def: None`, since a `ToolCtx` has no `SessionMeta`
/// lookup of its own. Before this item's fix, `Runtime::ask` passed that
/// `None` straight through to `start`, so the child got no `agent_def` at
/// all: `AgentNode.agent_def` stayed `None` and the child's context opened
/// with no `Provenance::AgentDef` segment -- the parent's own def-declared
/// system prompt and tools selector never reached it.
#[tokio::test]
async fn ask_child_inherits_the_parents_agent_def_for_system_prompt_and_tools() {
    let backend = Arc::new(AskBackend::new(
        BackendId::new("b"),
        vec![
            AskTurn::text("root ok", Duration::ZERO),
            AskTurn::text("child ok", Duration::ZERO),
        ],
    ));
    let bus = EventBus::with_default_capacity();
    let mut defs = HashMap::new();
    defs.insert("asker".to_string(), restrictive_asker_def());
    let runtime = build_runtime_with_backend_and_defs(backend, bus, defs);

    let mut spec = root_spec("investigate");
    spec.agent_def = Some(conway_core::agent::AgentDefRef("asker".to_string()));
    let mut stream = runtime.subscribe();
    let parent = runtime.start_root(spec).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope = stream.next().await.expect("event stream ended early");
            if envelope.agent == parent {
                if let Event::AgentFinished { .. } = envelope.event {
                    return;
                }
            }
        }
    })
    .await
    .expect("root never finished");

    let ask_spec = ask_fork_spec("say hi");
    // `AskTool::invoke` always builds its `SubagentSpec` with `agent_def:
    // None` -- this test's whole point is that `Runtime::ask` (not the
    // call site) is what fills it in.
    assert!(ask_spec.agent_def.is_none());

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.ask(parent, parent, ask_spec),
    )
    .await
    .expect("ask did not resolve")
    .expect("ask errored");
    assert_eq!(outcome.status, ResultStatus::Completed);

    let child = runtime
        .tree()
        .nodes
        .iter()
        .find(|n| n.parent == Some(parent))
        .expect("ask child attached to the tree")
        .agent_id;
    let child_node = runtime
        .tree()
        .nodes
        .into_iter()
        .find(|n| n.agent_id == child)
        .expect("ask child node present");
    assert_eq!(
        child_node.agent_def,
        Some("asker".to_string()),
        "the ask child must inherit the PARENT's own agent_def, not start with none at all"
    );

    // The def's system prompt actually threads into the child's own
    // context (not just tree bookkeeping): its first segment carries
    // `Provenance::AgentDef { name: "asker" }` -- the same shape
    // `subagent_fork_spawn.rs`'s
    // `spawn_context_has_no_inherited_segment_and_uses_agent_def_system_prompt`
    // asserts for an ordinary spawn.
    let report = runtime.context_report(child).unwrap();
    assert!(
        report
            .segments
            .iter()
            .any(|e| matches!(&e.provenance, Provenance::AgentDef { name } if name == "asker")),
        "the ask child's context must carry the parent's agent_def system prompt, got: {:?}",
        report.segments
    );
}
