//! End-to-end acceptance tests for `AgentLoop` (WI-081, architecture §7):
//! ContextBuilder -> Router -> AttemptEngine -> ToolRunner -> SessionStore
//! wiring, budgets, and terminal-result construction.
//!
//! Uses local scripted doubles throughout (`conway_core::fakes::FakeBackend`
//! has no per-id scripting support, and `ScriptedBackend::with_id` does not
//! exist) rather than the shared `conway-core` fakes wherever per-call
//! recording or ordering instrumentation is needed.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use conway_core::agent::{Budget, PermissionDecision, ResultStatus, ToolSelector};
use conway_core::capabilities::{
    CacheMode, Capabilities, ProbeReport, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{
    ContentBlock, PermissionClass, SamplingParams, StopReason, ToolCall, ToolCategory, ToolSpec,
    Usage,
};
use conway_core::error::{BackendError, RoutingError, StoreError};
use conway_core::event::Event;
use conway_core::fakes::{FakeGate, FakeHealth, FakeStore, FakeSubagentHost};
use conway_core::ids::{
    AgentId, BackendId, LogSeq, ModelId, ModelRef, RoleAlias, SessionId, ToolName,
};
use conway_core::log::{LogRecord, SessionFilter, SessionMeta, SessionStatus};
use conway_core::ports::{
    Backend, BoxStream, GenerateRequest, GenerateResponse, HealthRegistry, PermissionGate, Plugin,
    PluginConfig, PluginManifest, Router, SessionStore, StreamChunk, SubagentHost, Tool, ToolCtx,
    ToolOutput,
};
use conway_core::provenance::Provenance;
use conway_core::routing::{Route, RouteRequest, RoutingReason};
use conway_core::segment::CacheTtl;
use conway_routing::config::HeadroomPolicy;
use conway_runtime::agent_loop::{AgentLoop, AgentSpec, LoopDeps};
use conway_runtime::attempt::AttemptEngine;
use conway_runtime::context::ContextBuilder;
use conway_runtime::events::EventBus;
use conway_runtime::permission::PermissionBroker;
use conway_runtime::tools::PluginRegistry;
use conway_runtime::tree::{AgentNode, AgentTree};
use futures::future::FutureExt;
use futures::stream::{self, StreamExt};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------
// Fixtures: capabilities, routes, responses
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

fn make_route(backend: &str, model: &str) -> Route {
    Route {
        backend: BackendId::new(backend),
        model: ModelId::new(model),
        params: SamplingParams::default(),
        reason: RoutingReason::AliasPrimary {
            alias: RoleAlias::new("test"),
        },
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

fn tool_call_response(call_id: &str, tool: &str, args: serde_json::Value) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: ToolName::new(tool),
            arguments: args,
        }],
        stop: StopReason::ToolUse,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
    }
}

// ---------------------------------------------------------------------
// A local scripted `Backend` double that records every request and,
// optionally, appends a marker to a shared ordering log on every call --
// `conway_core::fakes::ScriptedBackend` records requests but has no id
// customization (`with_id` does not exist) and no ordering hook.
// ---------------------------------------------------------------------

struct TrackingBackend {
    id: BackendId,
    caps: Capabilities,
    script: Mutex<VecDeque<GenerateResponse>>,
    calls: Mutex<Vec<GenerateRequest>>,
    order: Option<Arc<Mutex<Vec<String>>>>,
}

impl TrackingBackend {
    fn new(id: &str, script: Vec<GenerateResponse>) -> Self {
        Self {
            id: BackendId::new(id),
            caps: caps_ok(),
            script: Mutex::new(script.into()),
            calls: Mutex::new(Vec::new()),
            order: None,
        }
    }

    fn with_order(mut self, order: Arc<Mutex<Vec<String>>>) -> Self {
        self.order = Some(order);
        self
    }

    fn calls(&self) -> Vec<GenerateRequest> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl Backend for TrackingBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn capabilities(&self, _model: &ModelId) -> Capabilities {
        self.caps.clone()
    }

    async fn generate(&self, req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
        self.calls.lock().unwrap().push(req);
        if let Some(order) = &self.order {
            order.lock().unwrap().push("backend_call".to_string());
        }
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(BackendError::BadRequest {
                detail: "tracking backend script exhausted".to_string(),
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
            at: Utc::now(),
        })
    }
}

// ---------------------------------------------------------------------
// A `Router` double that records every `RouteRequest` it receives, so
// tests can assert the headroom/est_tokens values the loop resolved.
// ---------------------------------------------------------------------

struct CapturingRouter {
    requests: Mutex<Vec<RouteRequest>>,
    routes: Vec<Route>,
    err: Option<RoutingError>,
}

impl CapturingRouter {
    fn ok(routes: Vec<Route>) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            routes,
            err: None,
        }
    }

    fn erroring(err: RoutingError) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            routes: Vec::new(),
            err: Some(err),
        }
    }

    fn requests(&self) -> Vec<RouteRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Router for CapturingRouter {
    fn resolve(&self, req: &RouteRequest) -> Result<Vec<Route>, RoutingError> {
        self.requests.lock().unwrap().push(req.clone());
        match &self.err {
            Some(err) => Err(err.clone()),
            None => Ok(self.routes.clone()),
        }
    }
}

// ---------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------

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

/// Returns fixed text, optionally recording its invocation onto a shared
/// ordering log before doing anything else.
struct RecordingTool {
    name: ToolName,
    output: String,
    order: Option<Arc<Mutex<Vec<String>>>>,
}

#[async_trait]
impl Tool for RecordingTool {
    fn spec(&self) -> ToolSpec {
        tool_spec(self.name.as_str())
    }

    async fn invoke(
        &self,
        _call: ToolCall,
        _ctx: ToolCtx,
    ) -> Result<ToolOutput, conway_core::error::ToolError> {
        if let Some(order) = &self.order {
            order.lock().unwrap().push("tool_invoke".to_string());
        }
        Ok(text_output(self.output.clone()))
    }
}

/// Sleeps for `delay` before returning fixed text -- used to give a test a
/// window to observe `Event::ToolCallStarted` and trip cancellation
/// mid-invoke.
struct DelayTool {
    name: ToolName,
    delay: Duration,
}

#[async_trait]
impl Tool for DelayTool {
    fn spec(&self) -> ToolSpec {
        tool_spec(self.name.as_str())
    }

    async fn invoke(
        &self,
        _call: ToolCall,
        _ctx: ToolCtx,
    ) -> Result<ToolOutput, conway_core::error::ToolError> {
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

// ---------------------------------------------------------------------
// A `SessionStore` wrapper that records an `append:<kind>` marker onto a
// shared ordering log after every successful append -- used by the
// persist-before-act test to interleave with a tool's own ordering marker.
// ---------------------------------------------------------------------

struct OrderingStore {
    inner: FakeStore,
    order: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl SessionStore for OrderingStore {
    async fn create(&self, meta: SessionMeta) -> Result<SessionId, StoreError> {
        self.inner.create(meta).await
    }

    async fn append(&self, sid: &SessionId, rec: LogRecord) -> Result<LogSeq, StoreError> {
        let tag = rec.kind_str().to_string();
        let seq = self.inner.append(sid, rec).await?;
        self.order.lock().unwrap().push(format!("append:{tag}"));
        Ok(seq)
    }

    async fn read(
        &self,
        sid: &SessionId,
        range: conway_core::ids::SeqRange,
    ) -> Result<Vec<LogRecord>, StoreError> {
        self.inner.read(sid, range).await
    }

    async fn head(&self, sid: &SessionId) -> Result<LogSeq, StoreError> {
        self.inner.head(sid).await
    }

    async fn fork(
        &self,
        parent: &SessionId,
        at: LogSeq,
        meta: SessionMeta,
    ) -> Result<SessionId, StoreError> {
        self.inner.fork(parent, at, meta).await
    }

    async fn meta(&self, sid: &SessionId) -> Result<SessionMeta, StoreError> {
        self.inner.meta(sid).await
    }

    async fn children(&self, sid: &SessionId) -> Result<Vec<SessionId>, StoreError> {
        self.inner.children(sid).await
    }

    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionMeta>, StoreError> {
        self.inner.list(filter).await
    }
}

// ---------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------

async fn seed_prompt(store: &dyn SessionStore, role: &str, prompt: &str) -> (SessionId, AgentId) {
    let session = SessionId::new();
    let agent = AgentId::new();
    store
        .create(SessionMeta {
            id: session,
            agent_id: agent,
            origin: None,
            agent_def: None,
            role: Some(RoleAlias::new(role)),
            created: Utc::now(),
            cwd: PathBuf::from("/tmp"),
            labels: vec![],
            status: SessionStatus::Active,
            ephemeral: false,
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
    (session, agent)
}

struct Harness {
    agent_loop: AgentLoop,
    bus: Arc<EventBus>,
    cancel: CancellationToken,
}

#[allow(clippy::too_many_arguments)]
fn build_loop(
    session: SessionId,
    agent: AgentId,
    store: Arc<dyn SessionStore>,
    router: Arc<dyn Router>,
    backend: Arc<dyn Backend>,
    tools: Vec<Arc<dyn Tool>>,
    gate: Arc<dyn PermissionGate>,
    budget: Budget,
    headroom: HeadroomPolicy,
    headroom_override: Option<u32>,
    role: &str,
) -> Harness {
    build_loop_inner(
        session,
        agent,
        store,
        router,
        backend,
        tools,
        gate,
        budget,
        headroom,
        headroom_override,
        role,
        None,
    )
}

/// Like [`build_loop`], but wires `report_slot` into the `AgentSpec` so a
/// test can observe the live slot `Runtime::context_report` (WI-082) reads
/// from without going through `Runtime` itself.
#[allow(clippy::too_many_arguments)]
fn build_loop_with_report_slot(
    session: SessionId,
    agent: AgentId,
    store: Arc<dyn SessionStore>,
    router: Arc<dyn Router>,
    backend: Arc<dyn Backend>,
    tools: Vec<Arc<dyn Tool>>,
    gate: Arc<dyn PermissionGate>,
    budget: Budget,
    headroom: HeadroomPolicy,
    role: &str,
    report_slot: Arc<Mutex<Option<conway_core::provenance::ContextReport>>>,
) -> Harness {
    build_loop_inner(
        session,
        agent,
        store,
        router,
        backend,
        tools,
        gate,
        budget,
        headroom,
        None,
        role,
        Some(report_slot),
    )
}

#[allow(clippy::too_many_arguments)]
fn build_loop_inner(
    session: SessionId,
    agent: AgentId,
    store: Arc<dyn SessionStore>,
    router: Arc<dyn Router>,
    backend: Arc<dyn Backend>,
    tools: Vec<Arc<dyn Tool>>,
    gate: Arc<dyn PermissionGate>,
    budget: Budget,
    headroom: HeadroomPolicy,
    headroom_override: Option<u32>,
    role: &str,
    report_slot: Option<Arc<Mutex<Option<conway_core::provenance::ContextReport>>>>,
) -> Harness {
    let bus = EventBus::new(1024);
    let health: Arc<dyn HealthRegistry> = Arc::new(FakeHealth::new());
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);
    let attempt = Arc::new(AttemptEngine::new(backends, health, bus.clone()));
    let plugin_registry = registry(tools);
    let broker = Arc::new(PermissionBroker::new(gate, bus.clone()));
    let tool_runner = Arc::new(conway_runtime::tools::ToolRunner::new(
        plugin_registry.clone(),
        broker,
        bus.clone(),
    ));
    let subagents: Arc<dyn SubagentHost> = Arc::new(FakeSubagentHost::new(agent));
    let tree = Arc::new(AgentTree::new(bus.clone()));

    let deps = Arc::new(LoopDeps {
        store,
        router,
        attempt,
        registry: plugin_registry,
        tool_runner,
        subagents,
        plugin_config: Arc::new(PluginConfig::default()),
        bus: bus.clone(),
        builder: Arc::new(ContextBuilder::new()),
        headroom: Arc::new(headroom),
        tree: tree.clone(),
    });

    let spec = AgentSpec {
        system_prompt: None,
        skills: vec![],
        tools: None as Option<ToolSelector>,
        role: RoleAlias::new(role),
        pin: None,
        budget: budget.clone(),
        cache_mode: CacheMode::None,
        cache_ttl: CacheTtl::FiveMinutes,
        headroom_override,
        max_parallel_tools: 4,
        report_slot,
        // WI-086: not exercised by this file -- `tests/result_contract.rs`
        // owns result-contract coverage.
        result_contract: None,
        // Keep-alive is exercised at the facade level
        // (`crates/conway/tests/session_handle.rs`), not through this
        // file's own hand-built harness.
        keep_alive: false,
    };

    let cancel = CancellationToken::new();
    tree.attach(AgentNode {
        id: agent,
        parent: None,
        session,
        kind: None,
        agent_def: None,
        role: Some(RoleAlias::new(role)),
        budget,
        cancel: cancel.clone(),
        inherited_upto: None,
    })
    .expect("fresh tree attach never fails");
    let (_mailbox_tx, mailbox_rx) =
        conway_runtime::mailbox::Mailbox::new(conway_runtime::mailbox::RUNTIME_CAPACITY);
    let agent_loop = AgentLoop {
        agent_id: agent,
        session,
        parent: None,
        agent_path: vec![agent],
        cwd: PathBuf::from("/tmp"),
        deps,
        spec,
        cancel: cancel.clone(),
        // WI-084: no test in this file exercises fork inheritance --
        // that's `tests/subagent_fork_spawn.rs`'s job.
        inherited: None,
        // WI-085: no test in this file exercises mailboxes/steering --
        // that's `tests/steering.rs`'s job.
        inbox: mailbox_rx,
        parent_mailbox: None,
        pending_cancel: None,
        resume_gate: Default::default(),
    };

    Harness {
        agent_loop,
        bus,
        cancel,
    }
}

/// Drains every envelope already synchronously buffered on `stream` --
/// valid because every `bus.emit` call in this crate is synchronous and has
/// already run to completion by the time the awaited future returns.
fn drain(stream: &mut conway_runtime::events::EventStream) -> Vec<Event> {
    let mut out = Vec::new();
    while let Some(Some(envelope)) = stream.next().now_or_never() {
        out.push(envelope.event);
    }
    out
}

fn kind(event: &Event) -> &'static str {
    match event {
        Event::TurnStarted { .. } => "turn_started",
        Event::ModelDecision { .. } => "model_decision",
        Event::TextDelta { .. } => "text_delta",
        Event::TurnFinished { .. } => "turn_finished",
        Event::ToolCallProposed { .. } => "tool_call_proposed",
        Event::ToolCallStarted { .. } => "tool_call_started",
        Event::AgentFinished { .. } => "agent_finished",
        Event::Error { .. } => "error",
        _ => "other",
    }
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[tokio::test]
async fn text_only_response_completes_in_one_turn_with_expected_events() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "planner", "hello").await;
    let backend = Arc::new(TrackingBackend::new("b", vec![text_response("hi there")]));
    let router = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let harness = build_loop(
        session,
        agent,
        store,
        router,
        backend,
        vec![],
        gate,
        Budget::default(),
        HeadroomPolicy::default(),
        None,
        "planner",
    );
    let mut stream = harness.bus.subscribe();

    let result = harness.agent_loop.run().await;
    assert_eq!(result.status, ResultStatus::Completed);

    let kinds: Vec<&'static str> = drain(&mut stream).iter().map(kind).collect();
    assert!(kinds.contains(&"turn_started"));
    assert!(kinds.contains(&"text_delta"));
    assert!(kinds.contains(&"turn_finished"));
    assert!(kinds.contains(&"agent_finished"));
}

#[tokio::test]
async fn tool_call_then_text_runs_two_turns_and_second_context_sees_the_result() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "planner", "hello").await;
    let backend = Arc::new(TrackingBackend::new(
        "b",
        vec![
            tool_call_response("tc_1", "read", serde_json::json!({"path": "a.txt"})),
            text_response("done"),
        ],
    ));
    let router = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let tool: Arc<dyn Tool> = Arc::new(RecordingTool {
        name: ToolName::new("read"),
        output: "file contents".to_string(),
        order: None,
    });

    let harness = build_loop(
        session,
        agent,
        store.clone(),
        router,
        backend.clone(),
        vec![tool],
        gate,
        Budget::default(),
        HeadroomPolicy::default(),
        None,
        "planner",
    );

    let result = harness.agent_loop.run().await;
    assert_eq!(result.status, ResultStatus::Completed);

    let calls = backend.calls();
    assert_eq!(
        calls.len(),
        2,
        "expected exactly two backend calls (two turns)"
    );

    let second_turn_segments = &calls[1].segments;
    assert!(
        second_turn_segments.iter().any(|s| matches!(
            &s.provenance,
            Provenance::ToolResult { tool, .. } if tool.as_str() == "read"
        )),
        "second turn's context must include the first turn's tool result"
    );
    assert!(
        second_turn_segments
            .iter()
            .any(|s| s.content.iter().any(|b| matches!(
                b,
                ContentBlock::Text { text } if text.contains("file contents")
            ))),
        "second turn's context must contain the tool result's text"
    );

    // The store holds the appended tool_result record with correct provenance.
    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    assert!(records.iter().any(|r| matches!(
        r,
        LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == "read" && !result.is_error
    )));
}

#[tokio::test]
async fn assistant_record_persists_before_any_tool_invoke_begins() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let inner_store = FakeStore::new();
    let store: Arc<dyn SessionStore> = Arc::new(OrderingStore {
        inner: inner_store,
        order: order.clone(),
    });
    let (session, agent) = seed_prompt(&*store, "planner", "hello").await;
    order.lock().unwrap().clear(); // drop the seed's own append marker

    let backend = Arc::new(
        TrackingBackend::new(
            "b",
            vec![
                tool_call_response("tc_1", "read", serde_json::json!({})),
                text_response("done"),
            ],
        )
        .with_order(order.clone()),
    );
    let router = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let tool: Arc<dyn Tool> = Arc::new(RecordingTool {
        name: ToolName::new("read"),
        output: "ok".to_string(),
        order: Some(order.clone()),
    });

    let harness = build_loop(
        session,
        agent,
        store,
        router,
        backend,
        vec![tool],
        gate,
        Budget::default(),
        HeadroomPolicy::default(),
        None,
        "planner",
    );

    let result = harness.agent_loop.run().await;
    assert_eq!(result.status, ResultStatus::Completed);

    let log = order.lock().unwrap().clone();
    let assistant_idx = log
        .iter()
        .position(|m| m == "append:assistant")
        .expect("assistant record must be appended");
    let invoke_idx = log
        .iter()
        .position(|m| m == "tool_invoke")
        .expect("tool must have been invoked");
    assert!(
        assistant_idx < invoke_idx,
        "assistant append must complete before tool invoke begins: {log:?}"
    );

    let backend_call_idx = log.iter().position(|m| m == "backend_call").unwrap();
    assert!(
        backend_call_idx < invoke_idx,
        "first backend call must precede the tool invoke: {log:?}"
    );
}

#[tokio::test]
async fn budget_max_steps_exceeded_after_exactly_two_turns() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "planner", "hello").await;
    // Always returns a tool call, so the loop never completes on its own.
    let backend = Arc::new(TrackingBackend::new(
        "b",
        vec![
            tool_call_response("tc_1", "read", serde_json::json!({})),
            tool_call_response("tc_2", "read", serde_json::json!({})),
        ],
    ));
    let router = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let tool: Arc<dyn Tool> = Arc::new(RecordingTool {
        name: ToolName::new("read"),
        output: "ok".to_string(),
        order: None,
    });

    let budget = Budget {
        max_steps: 2,
        ..Budget::default()
    };
    let harness = build_loop(
        session,
        agent,
        store,
        router,
        backend.clone(),
        vec![tool],
        gate,
        budget,
        HeadroomPolicy::default(),
        None,
        "planner",
    );

    let result = harness.agent_loop.run().await;
    assert!(matches!(result.status, ResultStatus::BudgetExceeded { .. }));
    assert_eq!(backend.calls().len(), 2, "exactly two turns must have run");
}

#[tokio::test]
async fn budget_deadline_already_elapsed_exceeds_before_any_backend_call() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "planner", "hello").await;
    let backend = Arc::new(TrackingBackend::new("b", vec![text_response("hi")]));
    let router = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let budget = Budget {
        deadline: Some(Utc::now() - chrono::Duration::seconds(5)),
        ..Budget::default()
    };
    let harness = build_loop(
        session,
        agent,
        store,
        router,
        backend.clone(),
        vec![],
        gate,
        budget,
        HeadroomPolicy::default(),
        None,
        "planner",
    );

    let result = harness.agent_loop.run().await;
    assert!(matches!(result.status, ResultStatus::BudgetExceeded { .. }));
    assert_eq!(backend.calls().len(), 0);
}

#[tokio::test]
async fn budget_max_tokens_exceeded_stops_the_loop() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "planner", "hello").await;
    let backend = Arc::new(TrackingBackend::new(
        "b",
        vec![
            tool_call_response("tc_1", "read", serde_json::json!({})),
            tool_call_response("tc_2", "read", serde_json::json!({})),
            tool_call_response("tc_3", "read", serde_json::json!({})),
        ],
    ));
    let router = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let tool: Arc<dyn Tool> = Arc::new(RecordingTool {
        name: ToolName::new("read"),
        output: "ok".to_string(),
        order: None,
    });

    // Each turn's response reports usage of 15 tokens (10 in + 5 out); a
    // budget of 20 must stop after the second turn.
    let budget = Budget {
        max_tokens: Some(20),
        ..Budget::default()
    };
    let harness = build_loop(
        session,
        agent,
        store,
        router,
        backend.clone(),
        vec![tool],
        gate,
        budget,
        HeadroomPolicy::default(),
        None,
        "planner",
    );

    let result = harness.agent_loop.run().await;
    assert!(matches!(result.status, ResultStatus::BudgetExceeded { .. }));
    assert_eq!(backend.calls().len(), 2);
}

#[tokio::test]
async fn denied_tool_call_is_recorded_as_an_error_result_and_the_loop_continues() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "planner", "hello").await;
    let backend = Arc::new(TrackingBackend::new(
        "b",
        vec![
            tool_call_response("tc_1", "read", serde_json::json!({})),
            text_response("done"),
        ],
    ));
    let router = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate = Arc::new(FakeGate::deny_all("not allowed"));
    let tool: Arc<dyn Tool> = Arc::new(RecordingTool {
        name: ToolName::new("read"),
        output: "should never run".to_string(),
        order: None,
    });

    let harness = build_loop(
        session,
        agent,
        store.clone(),
        router,
        backend,
        vec![tool],
        gate,
        Budget::default(),
        HeadroomPolicy::default(),
        None,
        "planner",
    );

    let result = harness.agent_loop.run().await;
    assert_eq!(result.status, ResultStatus::Completed);

    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    let tool_result = records
        .iter()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. } => Some(result),
            _ => None,
        })
        .expect("a tool_result record must be appended for the denied call");
    assert!(
        tool_result.is_error,
        "a denial must be a model-visible error result"
    );
}

#[tokio::test]
async fn no_candidate_from_the_router_yields_failed_and_one_fatal_error_event() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "planner", "hello").await;
    let backend = Arc::new(TrackingBackend::new("b", vec![]));
    let router = Arc::new(CapturingRouter::erroring(RoutingError::NoCandidate {
        role: RoleAlias::new("planner"),
        considered: vec![],
    }));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let harness = build_loop(
        session,
        agent,
        store,
        router,
        backend,
        vec![],
        gate,
        Budget::default(),
        HeadroomPolicy::default(),
        None,
        "planner",
    );
    let mut stream = harness.bus.subscribe();

    let result = harness.agent_loop.run().await;
    assert!(matches!(result.status, ResultStatus::Failed { .. }));

    let fatal_errors: Vec<_> = drain(&mut stream)
        .into_iter()
        .filter(|e| matches!(e, Event::Error { fatal: true, .. }))
        .collect();
    assert_eq!(fatal_errors.len(), 1);
}

#[tokio::test]
async fn context_too_large_from_the_router_yields_failed_naming_the_shortfall() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "planner", "hello").await;
    let backend = Arc::new(TrackingBackend::new("b", vec![]));
    let model = ModelRef {
        backend: BackendId::new("b"),
        model: ModelId::new("m"),
    };
    let router = Arc::new(CapturingRouter::erroring(RoutingError::ContextTooLarge {
        role: RoleAlias::new("planner"),
        model,
        est_tokens: 30_000,
        headroom_tokens: 4_000,
        required_tokens: 34_000,
        max_context_tokens: 32_768,
        shortfall_tokens: 1_232,
    }));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let harness = build_loop(
        session,
        agent,
        store,
        router,
        backend,
        vec![],
        gate,
        Budget::default(),
        HeadroomPolicy::default(),
        None,
        "planner",
    );
    let mut stream = harness.bus.subscribe();

    let result = harness.agent_loop.run().await;
    match &result.status {
        ResultStatus::Failed { error } => {
            for needle in ["30000", "4000", "34000", "32768", "1232"] {
                assert!(error.contains(needle), "missing {needle} in {error:?}");
            }
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    let fatal_errors: Vec<_> = drain(&mut stream)
        .into_iter()
        .filter(|e| matches!(e, Event::Error { fatal: true, .. }))
        .collect();
    assert_eq!(fatal_errors.len(), 1);
}

#[tokio::test]
async fn event_ordering_within_a_turn_is_turnstarted_before_modeldecision_before_turnfinished_before_toolcallproposed(
) {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "planner", "hello").await;
    let backend = Arc::new(TrackingBackend::new(
        "b",
        vec![
            tool_call_response("tc_1", "read", serde_json::json!({})),
            text_response("done"),
        ],
    ));
    let router = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let tool: Arc<dyn Tool> = Arc::new(RecordingTool {
        name: ToolName::new("read"),
        output: "ok".to_string(),
        order: None,
    });

    let harness = build_loop(
        session,
        agent,
        store,
        router,
        backend,
        vec![tool],
        gate,
        Budget::default(),
        HeadroomPolicy::default(),
        None,
        "planner",
    );
    let mut stream = harness.bus.subscribe();

    let result = harness.agent_loop.run().await;
    assert_eq!(result.status, ResultStatus::Completed);

    let kinds: Vec<&'static str> = drain(&mut stream).iter().map(kind).collect();
    let idx = |k: &str| kinds.iter().position(|x| *x == k).expect(k);
    let turn_started = idx("turn_started");
    let model_decision = idx("model_decision");
    let turn_finished = idx("turn_finished");
    let tool_call_proposed = idx("tool_call_proposed");
    assert!(turn_started < model_decision, "{kinds:?}");
    assert!(model_decision < turn_finished, "{kinds:?}");
    assert!(turn_finished < tool_call_proposed, "{kinds:?}");
}

#[tokio::test]
async fn cancellation_mid_tool_batch_resolves_cancelled_within_100ms() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "planner", "hello").await;
    let backend = Arc::new(TrackingBackend::new(
        "b",
        vec![tool_call_response("tc_1", "slow", serde_json::json!({}))],
    ));
    let router = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let tool: Arc<dyn Tool> = Arc::new(DelayTool {
        name: ToolName::new("slow"),
        delay: Duration::from_secs(5),
    });

    let harness = build_loop(
        session,
        agent,
        store,
        router,
        backend,
        vec![tool],
        gate,
        Budget::default(),
        HeadroomPolicy::default(),
        None,
        "planner",
    );
    let bus = harness.bus.clone();
    let cancel = harness.cancel.clone();
    let mut stream = bus.subscribe();

    let handle = tokio::spawn(harness.agent_loop.run());

    // Wait until the tool call has actually started before cancelling.
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

    cancel.cancel();

    let result = tokio::time::timeout(Duration::from_millis(100), handle)
        .await
        .expect("agent loop did not resolve within 100ms of cancellation")
        .expect("agent loop task panicked");
    assert!(
        matches!(result.status, ResultStatus::Cancelled { .. }),
        "expected Cancelled, got {:?}",
        result.status
    );
}

#[tokio::test]
async fn headroom_is_resolved_once_and_flows_consistently_to_route_and_backend_request() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "planner", "hello").await;
    let backend = Arc::new(TrackingBackend::new("b", vec![text_response("hi")]));
    let router = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let policy = HeadroomPolicy {
        default_headroom_tokens: 12_345,
        per_role: Default::default(),
    };
    let harness = build_loop(
        session,
        agent,
        store,
        router.clone(),
        backend.clone(),
        vec![],
        gate,
        Budget::default(),
        policy,
        None,
        "planner",
    );

    let result = harness.agent_loop.run().await;
    assert_eq!(result.status, ResultStatus::Completed);

    let captured = router.requests();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].required.headroom_tokens, 12_345);
    assert!(captured[0].est_tokens > 0);

    let calls = backend.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].params.max_tokens,
        Some(12_345),
        "AttemptRequest.headroom must equal the same value the RouteRequest carried"
    );
}

#[tokio::test]
async fn headroom_override_wins_over_the_policy_default() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "planner", "hello").await;
    let backend = Arc::new(TrackingBackend::new("b", vec![text_response("hi")]));
    let router = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let policy = HeadroomPolicy {
        default_headroom_tokens: 12_345,
        per_role: Default::default(),
    };
    let harness = build_loop(
        session,
        agent,
        store,
        router.clone(),
        backend.clone(),
        vec![],
        gate,
        Budget::default(),
        policy,
        Some(777),
        "planner",
    );

    let result = harness.agent_loop.run().await;
    assert_eq!(result.status, ResultStatus::Completed);

    assert_eq!(router.requests()[0].required.headroom_tokens, 777);
    assert_eq!(backend.calls()[0].params.max_tokens, Some(777));
}

/// FINDING C1 (WI-082 cycle-1 review): the loop pushes the turn's just-built
/// `ContextReport` into `AgentSpec.report_slot` every turn, before the
/// backend call -- proving a caller reading the slot sees a live report that
/// both exists mid-run and grows across turns, independent of the event bus.
#[tokio::test]
async fn report_slot_is_populated_and_updates_across_turns() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "planner", "hello").await;
    let backend = Arc::new(TrackingBackend::new(
        "b",
        vec![
            tool_call_response("tc_1", "read", serde_json::json!({"path": "a.txt"})),
            text_response("done"),
        ],
    ));
    let router = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let tool: Arc<dyn Tool> = Arc::new(RecordingTool {
        name: ToolName::new("read"),
        output: "file contents".to_string(),
        order: None,
    });
    let report_slot = Arc::new(Mutex::new(None));

    let harness = build_loop_with_report_slot(
        session,
        agent,
        store,
        router,
        backend,
        vec![tool],
        gate,
        Budget::default(),
        HeadroomPolicy::default(),
        "planner",
        report_slot.clone(),
    );

    // Before the loop runs, the slot is empty -- it is populated only once a
    // turn's context has actually been assembled.
    assert!(report_slot.lock().unwrap().is_none());

    let result = harness.agent_loop.run().await;
    assert_eq!(result.status, ResultStatus::Completed);

    let final_report = report_slot
        .lock()
        .unwrap()
        .clone()
        .expect("report slot populated by the time the loop finishes");
    assert_eq!(final_report.agent_id, agent);
    // Turn 1's context includes the tool's result (persisted after turn 0),
    // so it has strictly more segments than turn 0's did -- proof the slot
    // was overwritten with a *new* report, not left stuck on the first one.
    assert!(final_report.turn >= 1);
    assert!(!final_report.segments.is_empty());
}
