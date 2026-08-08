//! Acceptance tests for `Runtime` (WI-082, architecture §4, §7): the
//! facade's dependency injection, root-agent lifecycle, and public surface
//! (`start_root`, `prompt`, `cancel`, `subscribe`, `context_report`,
//! `tree`).
//!
//! Built entirely from `conway-core`'s fakes plus local scripted doubles
//! (mirroring `agent_loop_e2e.rs`'s own note: `ScriptedBackend` has no
//! built-in response delay, so a small local `DelayedBackend` stands in
//! wherever a test needs a window to observe "in progress" behavior) --
//! this file does not depend on `conway-backends` or `conway-tools`.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway_core::agent::{Budget, PermissionDecision, ResultStatus};
use conway_core::capabilities::{
    CacheMode, Capabilities, HeadroomPolicy, ProbeReport, ReliabilityTier, StructuredOutput,
    ToolCallSupport,
};
use conway_core::content::{
    ContentBlock, PermissionClass, StopReason, ToolCall, ToolCategory, ToolSpec, Usage,
};
use conway_core::error::{BackendError, RuntimeError, ToolError};
use conway_core::event::Event;
use conway_core::fakes::{
    FakeGate, FakeHealth, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn,
};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, SessionId, ToolName};
use conway_core::log::LogRecord;
use conway_core::ports::{
    Backend, BoxStream, GenerateRequest, GenerateResponse, Plugin, PluginManifest, Router,
    SessionStore, StreamChunk, Tool, ToolCtx, ToolOutput,
};
use conway_core::provenance::Provenance;
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{RootSpec, Runtime, RuntimeDeps};
use futures::stream;
use futures::StreamExt;

// ---------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------

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

fn text_response(text: &str) -> GenerateResponse {
    GenerateResponse {
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

fn tool_call_response(call_id: &str, tool: &str) -> GenerateResponse {
    GenerateResponse {
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

/// A one-shot backend that sleeps `delay` before returning its single
/// scripted response -- gives a test a deterministic window in which to
/// observe "the agent has not finished yet" (`ScriptedBackend` has no such
/// delay hook).
struct DelayedBackend {
    id: BackendId,
    delay: Duration,
    response: Mutex<VecDeque<GenerateResponse>>,
}

impl DelayedBackend {
    fn new(id: &str, delay: Duration, response: GenerateResponse) -> Self {
        Self {
            id: BackendId::new(id),
            delay,
            response: Mutex::new(VecDeque::from([response])),
        }
    }
}

#[async_trait]
impl Backend for DelayedBackend {
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
        let mut chunks: Vec<Result<StreamChunk, BackendError>> = response
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(Ok(StreamChunk::TextDelta(text.clone()))),
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

/// Sleeps for `delay` before returning -- gives a test a window to observe
/// `Event::ToolCallStarted` and trip cancellation mid-invoke.
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

// ---------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------

/// Builds a `Runtime` whose `RuntimeDeps` is constructed entirely from
/// `conway-core` fakes (`FakeStore`, `FakeGate`, `FakeHealth`) plus
/// `FakeRouter` and the given backend/plugins -- the WI-082 compile-check
/// criterion in living form. `RuntimeDeps` has no `subagents` field:
/// `Runtime::new` wires its own private `NoSubagentHost` stub in (see
/// `runtime.rs`'s module doc). Returns the runtime plus the backing store,
/// so tests can assert on persisted records directly.
fn build_runtime(
    backend: Arc<dyn Backend>,
    plugins: Vec<Arc<dyn Plugin>>,
) -> (Arc<Runtime>, Arc<dyn SessionStore>) {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
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
        plugins,
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
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

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

/// `RuntimeDeps` is constructible entirely from `conway-core` fakes plus a
/// fake router; this crate's `[dev-dependencies]` name neither
/// `conway-backends` nor `conway-tools` (see `Cargo.toml`).
#[tokio::test]
async fn runtime_deps_constructible_from_fakes_only() {
    let (runtime, _store) = build_runtime(
        Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Respond(
            text_response("hi"),
        )])),
        vec![],
    );
    assert!(runtime.tree().nodes.is_empty());
}

#[tokio::test]
async fn start_root_creates_session_header_and_completes() {
    let (runtime, store) = build_runtime(
        Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Respond(
            text_response("hi"),
        )])),
        vec![],
    );
    let mut stream = runtime.subscribe();

    let agent_id = runtime.start_root(root_spec("hello")).await.unwrap();
    let session = session_of(&runtime, agent_id);

    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    match records.first().expect("session has a head record") {
        LogRecord::UserTurn { text, prov, .. } => {
            assert_eq!(text, "hello");
            assert_eq!(*prov, Provenance::UserPrompt);
        }
        other => panic!("expected UserTurn head record, got {other:?}"),
    }
    store.meta(&session).await.expect("header was created");

    let result = wait_for_agent_finished(&mut stream, agent_id).await;
    assert_eq!(result.status, ResultStatus::Completed);
}

/// `start_root` returns before the first turn completes: a backend with a
/// deliberate delay proves the call did not block on it.
#[tokio::test]
async fn start_root_returns_before_first_turn_completes() {
    let delay = Duration::from_millis(200);
    let (runtime, _store) = build_runtime(
        Arc::new(DelayedBackend::new("b", delay, text_response("hi"))),
        vec![],
    );
    let mut stream = runtime.subscribe();

    let start = std::time::Instant::now();
    let agent_id = runtime.start_root(root_spec("hello")).await.unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed < delay,
        "start_root took {elapsed:?}, expected to return well before the backend's {delay:?} delay"
    );
    assert!(runtime.tree().nodes.iter().any(|n| n.agent_id == agent_id));

    let result = wait_for_agent_finished(&mut stream, agent_id).await;
    assert_eq!(result.status, ResultStatus::Completed);
}

#[tokio::test]
async fn prompt_appends_user_turn_before_returning_and_errors_for_unknown_agent() {
    let (runtime, store) = build_runtime(
        Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Respond(
            text_response("hi"),
        )])),
        vec![],
    );
    let agent_id = runtime.start_root(root_spec("hello")).await.unwrap();
    let session = session_of(&runtime, agent_id);

    runtime
        .prompt(agent_id, "more instructions".to_string())
        .await
        .unwrap();

    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    assert!(records.iter().any(|r| matches!(
        r,
        LogRecord::UserTurn { text, prov, .. }
            if text == "more instructions" && *prov == Provenance::UserPrompt
    )));

    let unknown = AgentId::new();
    let err = runtime.prompt(unknown, "x".to_string()).await.unwrap_err();
    assert!(matches!(err, RuntimeError::AgentNotFound { agent } if agent == unknown));
}

#[tokio::test]
async fn cancel_trips_token_and_agent_finishes_cancelled() {
    let tool: Arc<dyn Tool> = Arc::new(DelayTool {
        name: ToolName::new("slow"),
        delay: Duration::from_secs(5),
    });
    let backend = Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Respond(
        tool_call_response("tc_1", "slow"),
    )]));
    let (runtime, _store) =
        build_runtime(backend, vec![Arc::new(FakePlugin { tools: vec![tool] })]);
    let mut stream = runtime.subscribe();

    let agent_id = runtime.start_root(root_spec("hello")).await.unwrap();

    // Wait until the tool call has actually started before cancelling.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let envelope = stream.next().await.expect("stream open");
            if envelope.agent == agent_id && matches!(envelope.event, Event::ToolCallStarted { .. })
            {
                break;
            }
        }
    })
    .await
    .expect("ToolCallStarted was never observed");

    runtime
        .cancel(agent_id, "test requested cancellation".to_string())
        .unwrap();

    let result = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            let envelope = stream.next().await.expect("stream open");
            if envelope.agent == agent_id {
                if let Event::AgentFinished { result, .. } = envelope.event {
                    return result;
                }
            }
        }
    })
    .await
    .expect("agent did not resolve after cancellation");
    assert!(matches!(result.status, ResultStatus::Cancelled { .. }));

    let err = runtime.cancel(AgentId::new(), "x".to_string()).unwrap_err();
    assert!(matches!(err, RuntimeError::AgentNotFound { .. }));
}

#[tokio::test]
async fn subscribe_two_concurrent_subscribers_see_identical_seqs() {
    let (runtime, _store) = build_runtime(
        Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Respond(
            text_response("hi"),
        )])),
        vec![],
    );
    let s1 = runtime.subscribe();
    let s2 = runtime.subscribe();

    let agent_id = runtime.start_root(root_spec("hello")).await.unwrap();

    async fn collect(mut stream: conway_runtime::events::EventStream, agent: AgentId) -> Vec<u64> {
        let mut seqs = Vec::new();
        loop {
            let envelope = stream.next().await.expect("stream open");
            if envelope.agent == agent {
                let finished = matches!(envelope.event, Event::AgentFinished { .. });
                seqs.push(envelope.seq);
                if finished {
                    break;
                }
            }
        }
        seqs
    }

    let (seqs1, seqs2) = tokio::time::timeout(
        Duration::from_secs(2),
        futures::future::join(collect(s1, agent_id), collect(s2, agent_id)),
    )
    .await
    .expect("subscribers did not observe completion in time");

    assert_eq!(seqs1, seqs2);
    assert!(!seqs1.is_empty());
    assert!(seqs1.windows(2).all(|w| w[0] < w[1]));
}

#[tokio::test]
async fn context_report_returns_live_report_and_errors_for_unknown_agent() {
    let (runtime, _store) = build_runtime(
        Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Respond(
            text_response("hi"),
        )])),
        vec![],
    );
    let mut stream = runtime.subscribe();
    let agent_id = runtime.start_root(root_spec("hello")).await.unwrap();
    wait_for_agent_finished(&mut stream, agent_id).await;

    // `AgentLoop` pushes the turn's report into the shared slot synchronously,
    // before the turn's backend call and thus well before `AgentFinished` is
    // emitted -- no bus-fold catch-up window to wait out (FINDING C1).
    let report = runtime.context_report(agent_id).unwrap();

    assert!(!report.segments.is_empty());
    assert_eq!(report.tokenizer, "heuristic-chars4");
    assert_eq!(
        report.total_tokens_est,
        report.segments.iter().map(|e| e.tokens_est).sum::<u32>()
    );

    let err = runtime.context_report(AgentId::new()).unwrap_err();
    assert!(matches!(err, RuntimeError::AgentNotFound { .. }));
}

/// FINDING C1 (Critical): the live `context_report` must reflect the loop's
/// own report-slot pushes, growing turn over turn, rather than a
/// bus-reconstructed approximation. Captures the report mid-run (turn 0,
/// before any tool result exists) and again after the agent finishes (turn
/// 1, with the prior turn's assistant/tool-result records folded in), and
/// asserts the second strictly supersedes the first.
#[tokio::test]
async fn context_report_survives_and_updates_across_multiple_turns() {
    let tool: Arc<dyn Tool> = Arc::new(DelayTool {
        name: ToolName::new("quick"),
        delay: Duration::from_millis(0),
    });
    let backend = Arc::new(ScriptedBackend::new(vec![
        ScriptedTurn::Respond(tool_call_response("tc_1", "quick")),
        ScriptedTurn::Respond(text_response("done")),
    ]));
    let (runtime, _store) =
        build_runtime(backend, vec![Arc::new(FakePlugin { tools: vec![tool] })]);
    let mut stream = runtime.subscribe();

    let agent_id = runtime.start_root(root_spec("hello")).await.unwrap();

    // The first `ContextSegmentAdded` for this agent is emitted only after
    // that turn's report has already been pushed into the slot (the push
    // happens first in `AgentLoop::run_inner`), so this is turn 0's report.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let envelope = stream.next().await.expect("stream open");
            if envelope.agent == agent_id
                && matches!(envelope.event, Event::ContextSegmentAdded { .. })
            {
                break;
            }
        }
    })
    .await
    .expect("ContextSegmentAdded was never observed");
    let mid_run_report = runtime.context_report(agent_id).unwrap();

    let result = wait_for_agent_finished(&mut stream, agent_id).await;
    assert_eq!(result.status, ResultStatus::Completed);
    let final_report = runtime.context_report(agent_id).unwrap();

    assert_eq!(mid_run_report.turn, 0);
    assert!(
        final_report.turn > mid_run_report.turn,
        "expected the slot to have advanced past turn 0 by the time the agent finished"
    );
    assert!(
        final_report.segments.len() > mid_run_report.segments.len(),
        "turn 1's context must include turn 0's assistant/tool-result records, so it has strictly more segments"
    );
}

#[tokio::test]
async fn tree_contains_exactly_the_started_root_agents() {
    let (runtime, _store) = build_runtime(
        Arc::new(ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("one")),
            ScriptedTurn::Respond(text_response("two")),
        ])),
        vec![],
    );

    let agent1 = runtime.start_root(root_spec("one")).await.unwrap();
    let agent2 = runtime.start_root(root_spec("two")).await.unwrap();

    let snapshot = runtime.tree();
    let ids: Vec<AgentId> = snapshot.nodes.iter().map(|n| n.agent_id).collect();
    assert_eq!(snapshot.nodes.len(), 2);
    assert!(ids.contains(&agent1));
    assert!(ids.contains(&agent2));
}

#[tokio::test]
async fn two_roots_run_independently_with_interleaved_monotonic_seqs() {
    let (runtime, _store) = build_runtime(
        Arc::new(ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("one")),
            ScriptedTurn::Respond(text_response("two")),
        ])),
        vec![],
    );
    let mut stream = runtime.subscribe();

    let agent1 = runtime.start_root(root_spec("one")).await.unwrap();
    let agent2 = runtime.start_root(root_spec("two")).await.unwrap();
    let session1 = session_of(&runtime, agent1);
    let session2 = session_of(&runtime, agent2);

    let mut seqs1 = Vec::new();
    let mut seqs2 = Vec::new();
    let mut done1 = false;
    let mut done2 = false;

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let envelope = stream.next().await.expect("stream open");
            if envelope.session == session1 {
                let finished = matches!(envelope.event, Event::AgentFinished { .. });
                seqs1.push(envelope.seq);
                done1 |= finished;
            } else if envelope.session == session2 {
                let finished = matches!(envelope.event, Event::AgentFinished { .. });
                seqs2.push(envelope.seq);
                done2 |= finished;
            }
            if done1 && done2 {
                break;
            }
        }
    })
    .await
    .expect("both roots did not finish in time");

    assert!(!seqs1.is_empty() && !seqs2.is_empty());
    assert!(seqs1.windows(2).all(|w| w[0] < w[1]));
    assert!(seqs2.windows(2).all(|w| w[0] < w[1]));
}
