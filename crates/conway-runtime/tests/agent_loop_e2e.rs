//! End-to-end acceptance tests for `AgentLoop` (architecture §7):
//! ContextBuilder -> Router -> AttemptEngine -> ToolRunner -> SessionStore
//! wiring, budgets, and terminal-result construction.
//!
//! Uses local scripted doubles throughout (`conway_testkit::FakeBackend`
//! has no per-id scripting support, and `ScriptedBackend::with_id` does not
//! exist) rather than the shared `conway-testkit` fakes wherever per-call
//! recording or ordering instrumentation is needed.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use conway_core::agent::{Budget, PermissionDecision, ResultStatus, ToolSelector};
use conway_core::capabilities::{
    CacheMode, Capabilities, HeadroomPolicy, ProbeReport, ReliabilityTier, StructuredOutput,
    ToolCallSupport,
};
use conway_core::content::{
    ContentBlock, PermissionClass, SamplingParams, StopReason, ToolCall, ToolCategory, ToolSpec,
    Usage,
};
use conway_core::error::{BackendError, RoutingError, StoreError};
use conway_core::event::Event;
use conway_core::ids::{
    AgentId, BackendId, LogSeq, ModelId, ModelRef, RoleAlias, SessionId, ToolName,
};
use conway_core::log::{LogRecord, SessionFilter, SessionMeta};
use conway_core::ports::{
    Backend, BoxStream, ContextHook, ContextHookCtx, ContextPayload, GenerateRequest,
    GenerateResponse, HealthRegistry, LiveOwner, OverflowInfo, PermissionGate, Plugin,
    PluginConfig, PluginManifest, Router, SessionStore, StreamChunk, SubagentHost, Tool, ToolCtx,
    ToolOutput,
};
use conway_core::provenance::Provenance;
use conway_core::routing::{Route, RouteRequest, RoutingReason};
use conway_core::segment::{CacheTtl, PromptSegment};
use conway_runtime::agent_loop::{AgentLoop, AgentSpec, LoopDeps};
use conway_runtime::attempt::AttemptEngine;
use conway_runtime::context::{ContextBuilder, GuardedContextHook};
use conway_runtime::events::EventBus;
use conway_runtime::permission::PermissionBroker;
use conway_runtime::tools::PluginRegistry;
use conway_runtime::tree::{AgentNode, AgentTree};
use conway_testkit::{FakeGate, FakeHealth, FakeStore, FakeSubagentHost};
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
// `conway_testkit::ScriptedBackend` records requests but has no id
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

    async fn remove(&self, sid: &SessionId) -> Result<(), StoreError> {
        self.inner.remove(sid).await
    }

    async fn set_ephemeral(&self, sid: &SessionId, ephemeral: bool) -> Result<(), StoreError> {
        self.inner.set_ephemeral(sid, ephemeral).await
    }

    async fn live_owner(&self) -> Result<Option<LiveOwner>, StoreError> {
        self.inner.live_owner().await
    }

    async fn touch_live_owner(&self, pid: u32) -> Result<(), StoreError> {
        self.inner.touch_live_owner(pid).await
    }

    async fn clear_live_owner(&self) -> Result<(), StoreError> {
        self.inner.clear_live_owner().await
    }
}

async fn seed_prompt(store: &dyn SessionStore, role: &str, prompt: &str) -> (SessionId, AgentId) {
    let agent = AgentId::new();
    seed_prompt_for(store, agent, role, prompt).await
}

/// Like [`seed_prompt`], but for a caller-chosen `agent` id -- needed by any
/// test that must know the leaf agent's id up front (e.g. to attach it under
/// a specific ancestor chain in the `AgentTree` before the loop runs).
async fn seed_prompt_for(
    store: &dyn SessionStore,
    agent: AgentId,
    role: &str,
    prompt: &str,
) -> (SessionId, AgentId) {
    let session = SessionId::new();
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
        vec![],
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
        None,
        Vec::new(),
    )
}

/// Like [`build_loop`], but wires `report_slot` into the `AgentSpec` so a
/// test can observe the live slot `Runtime::context_report` reads
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
        vec![],
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
        None,
        Vec::new(),
    )
}

/// Like [`build_loop`], but attaches `agent` under `ancestors` (root-first,
/// NOT including `agent` itself) in the `AgentTree` before the loop runs,
/// derives `AgentLoop::agent_path` from a real `AgentTree::path` walk over
/// that chain (mirroring `subagent.rs`'s own construction, not a hand-typed
/// `vec![agent]`), and installs `context_hook` so a test can observe what a
/// registered `ContextHook` actually receives.
#[allow(clippy::too_many_arguments)]
fn build_loop_with_ancestry_and_hook(
    session: SessionId,
    agent: AgentId,
    ancestors: Vec<AgentId>,
    store: Arc<dyn SessionStore>,
    router: Arc<dyn Router>,
    backend: Arc<dyn Backend>,
    tools: Vec<Arc<dyn Tool>>,
    gate: Arc<dyn PermissionGate>,
    budget: Budget,
    headroom: HeadroomPolicy,
    role: &str,
    context_hook: Arc<dyn ContextHook>,
) -> Harness {
    build_loop_inner(
        session,
        agent,
        ancestors,
        store,
        router,
        backend,
        tools,
        gate,
        budget,
        headroom,
        None,
        role,
        None,
        Some(context_hook),
        Vec::new(),
    )
}

#[allow(clippy::too_many_arguments)]
fn build_loop_inner(
    session: SessionId,
    agent: AgentId,
    ancestors: Vec<AgentId>,
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
    context_hook: Option<Arc<dyn ContextHook>>,
    observers: Vec<conway_core::ports::RegisteredObserver>,
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
        // Mirrors `Runtime::set_context_hook`'s own wrap exactly -- the
        // fixture is the "a hook enters the runtime" seam for every test in
        // this file, same as that method is for production, so a raw
        // `Arc<dyn ContextHook>` never reaches `LoopDeps::context_hook`
        // unguarded here either (see that field's own doc,
        // `01M00RGARPESWXYAVY960KDE7S`).
        context_hook: std::sync::RwLock::new(
            context_hook.map(|inner| Arc::new(GuardedContextHook::new(inner))),
        ),
        observers,
        plugin_events: Arc::new(conway_runtime::hook_dispatch::HookDispatcher::new()),
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
        // not exercised by this file -- `tests/result_contract.rs`
        // owns result-contract coverage.
        result_contract: None,
        // Keep-alive is exercised at the facade level
        // (`crates/conway/tests/session_handle.rs`), not through this
        // file's own hand-built harness.
        keep_alive: false,
        tag: None,
    };

    let cancel = CancellationToken::new();
    let mut parent: Option<AgentId> = None;
    for ancestor in &ancestors {
        tree.attach(AgentNode {
            id: *ancestor,
            parent,
            session,
            kind: None,
            agent_def: None,
            role: Some(RoleAlias::new(role)),
            budget: budget.clone(),
            cancel: CancellationToken::new(),
            inherited_upto: None,
            ephemeral: false,
        })
        .expect("fresh tree attach never fails");
        parent = Some(*ancestor);
    }
    tree.attach(AgentNode {
        id: agent,
        parent,
        session,
        kind: None,
        agent_def: None,
        role: Some(RoleAlias::new(role)),
        budget,
        cancel: cancel.clone(),
        inherited_upto: None,
        ephemeral: false,
    })
    .expect("fresh tree attach never fails");
    // Same construction `subagent.rs`'s `SubagentHost::start` uses for a
    // real child's `agent_path` (§4.3): a real `AgentTree::path` walk, not a
    // hand-typed literal -- so this harness's `agent_path` has the exact
    // shape production code produces.
    let agent_path = tree.path(agent);
    let (_mailbox_tx, mailbox_rx) =
        conway_runtime::mailbox::Mailbox::new(conway_runtime::mailbox::RUNTIME_CAPACITY);
    let agent_loop = AgentLoop {
        agent_id: agent,
        session,
        parent,
        agent_path,
        cwd: PathBuf::from("/tmp"),
        root: None,
        plugin_config: Arc::new(PluginConfig::default()),
        deps,
        spec,
        cancel: cancel.clone(),
        // no test in this file exercises fork inheritance --
        // that's `tests/subagent_fork_spawn.rs`'s job.
        inherited: None,
        // no test in this file exercises mailboxes/steering --
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
    // the tool result rides in a `ToolResultBlock` (carrying its
    // call_id) so the wire adapters can serialize it as a `tool` message --
    // its text lives inside that block, not as a bare top-level `Text`.
    assert!(
        second_turn_segments
            .iter()
            .any(|s| s.content.iter().any(|b| matches!(
                b,
                ContentBlock::ToolResultBlock { blocks, .. }
                    if blocks.iter().any(|inner| matches!(
                        inner,
                        ContentBlock::Text { text } if text.contains("file contents")
                    ))
            ))),
        "second turn's context must contain the tool result's text inside a ToolResultBlock"
    );
    // regression: the second turn's context must ALSO carry the first
    // turn's tool CALL as an assistant `ToolUse` block. Without it the
    // assistant message has no `tool_calls`, the model never sees that it
    // called a tool, and it re-calls the tool indefinitely (the orphaned
    // tool result loops forever).
    assert!(
        second_turn_segments
            .iter()
            .any(|s| s.content.iter().any(|b| matches!(
                b,
                ContentBlock::ToolUse { call_id, name, .. }
                    if call_id == "tc_1" && name.as_str() == "read"
            ))),
        "second turn's context must carry the first turn's tool call as an assistant ToolUse block"
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
    // regression: the persisted assistant record is the WHOLE turn --
    // its tool calls folded in as trailing `ToolUse` blocks, not just text.
    assert!(
        records.iter().any(|r| matches!(
            r,
            LogRecord::Assistant { content, .. }
                if content.iter().any(|b| matches!(
                    b,
                    ContentBlock::ToolUse { call_id, name, .. }
                        if call_id == "tc_1" && name.as_str() == "read"
                ))
        )),
        "the persisted assistant record must carry the tool call as a ToolUse block"
    );
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

/// The fourth budget dimension. `max_tool_calls` was a public, settable,
/// serialized field that nothing read: an embedder who set a tool-call
/// ceiling got no ceiling and no warning. This pins that it binds, and that
/// the terminal result names WHICH dimension tripped -- an operator who set
/// several needs to know which one ended the run.
#[tokio::test]
async fn budget_max_tool_calls_exceeded_stops_the_loop() {
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

    // Each turn's response dispatches exactly one tool call; a ceiling of 2
    // must stop at the top of the third turn, before a third backend call.
    let budget = Budget {
        max_tool_calls: Some(2),
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
    match &result.status {
        ResultStatus::BudgetExceeded { limit } => {
            assert_eq!(limit, "max_tool_calls=2", "the tripped dimension is named");
        }
        other => panic!("expected BudgetExceeded, got {other:?}"),
    }
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

/// FINDING C1 (An earlier review found: ): the loop pushes the turn's just-built
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

// ---------------------------------------------------------------------
// `ContextHookCtx::agent_path`
// ---------------------------------------------------------------------

/// Records every `ContextHookCtx` a registered `ContextHook` sees, so a test
/// can assert on what the hook actually received rather than on an
/// intermediate value.
struct RecordingContextHook {
    captured: Mutex<Vec<ContextHookCtx>>,
}

impl RecordingContextHook {
    fn new() -> Self {
        Self {
            captured: Mutex::new(Vec::new()),
        }
    }

    fn agent_paths(&self) -> Vec<Vec<AgentId>> {
        self.captured
            .lock()
            .unwrap()
            .iter()
            .map(|ctx| ctx.agent_path.clone())
            .collect()
    }
}

#[async_trait]
impl ContextHook for RecordingContextHook {
    async fn before_request(
        &self,
        ctx: &ContextHookCtx,
        payload: ContextPayload,
    ) -> ContextPayload {
        self.captured.lock().unwrap().push(ctx.clone());
        payload
    }
}

/// The criterion that matters most: `ContextHookCtx::agent_path` and
/// `PermissionRequest::agent_path`, walked from the SAME leaf agent in the
/// SAME turn, must be byte-for-byte equal -- not merely equal by reading
/// both struct definitions. Builds a real four-agent chain
/// (root -> mid1 -> mid2 -> leaf) in the `AgentTree`, drives one turn that
/// both fires the registered `ContextHook` and sends one tool call through
/// the permission gate, and compares what each side actually observed.
#[tokio::test]
async fn context_hook_ctx_agent_path_equals_permission_request_agent_path_at_depth_four() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let root = AgentId::new();
    let mid1 = AgentId::new();
    let mid2 = AgentId::new();
    let leaf = AgentId::new();
    let (session, _) = seed_prompt_for(&*store, leaf, "planner", "hello").await;

    let backend = Arc::new(TrackingBackend::new(
        "b",
        vec![
            tool_call_response("tc_1", "read", serde_json::json!({"path": "a.txt"})),
            text_response("done"),
        ],
    ));
    let router = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate = Arc::new(FakeGate::recording());
    let tool: Arc<dyn Tool> = Arc::new(RecordingTool {
        name: ToolName::new("read"),
        output: "file contents".to_string(),
        order: None,
    });
    let hook = Arc::new(RecordingContextHook::new());

    let harness = build_loop_with_ancestry_and_hook(
        session,
        leaf,
        vec![root, mid1, mid2],
        store,
        router,
        backend,
        vec![tool],
        gate.clone(),
        Budget::default(),
        HeadroomPolicy::default(),
        "planner",
        hook.clone(),
    );

    let result = harness.agent_loop.run().await;
    assert_eq!(result.status, ResultStatus::Completed);

    let expected_path = vec![root, mid1, mid2, leaf];

    let hook_paths = hook.agent_paths();
    assert!(
        !hook_paths.is_empty(),
        "the registered ContextHook must have been invoked at least once"
    );
    assert_eq!(
        hook_paths[0], expected_path,
        "ContextHookCtx::agent_path must be the root-first, self-inclusive chain"
    );

    let requests = gate.requests();
    assert_eq!(
        requests.len(),
        1,
        "the tool call must have gone through the permission gate exactly once"
    );
    assert_eq!(requests[0].agent_id, leaf);
    assert_eq!(requests[0].agent_path, expected_path);

    // Not by reading both definitions and concluding they agree: compare the
    // two OBSERVED vectors directly.
    assert_eq!(
        hook_paths[0], requests[0].agent_path,
        "ContextHookCtx and PermissionRequest must agree, walked from the same agent"
    );

    // A one-level tree can't distinguish a working copy from an empty
    // vector -- assert this fixture is genuinely depth-four, not
    // vacuously equal.
    assert_eq!(hook_paths[0].len(), 4);
    assert_ne!(hook_paths[0], vec![leaf]);
}

/// A hook can tell a depth-1 agent apart from a depth-4 one, using nothing
/// but `ContextHookCtx::agent_path` -- proven by actually running both
/// shapes and asserting on what the SAME hook implementation observed in
/// each, not by inspecting a single fixture value.
#[tokio::test]
async fn context_hook_ctx_agent_path_distinguishes_depth_one_from_depth_four() {
    // Depth 1: a root agent, no ancestors.
    let store1: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session1, agent1) = seed_prompt(&*store1, "planner", "hello").await;
    let backend1 = Arc::new(TrackingBackend::new("b", vec![text_response("hi")]));
    let router1 = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate1 = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let hook1 = Arc::new(RecordingContextHook::new());
    let harness1 = build_loop_with_ancestry_and_hook(
        session1,
        agent1,
        vec![],
        store1,
        router1,
        backend1,
        vec![],
        gate1,
        Budget::default(),
        HeadroomPolicy::default(),
        "planner",
        hook1.clone(),
    );
    let result1 = harness1.agent_loop.run().await;
    assert_eq!(result1.status, ResultStatus::Completed);

    // Depth 4: root -> mid1 -> mid2 -> leaf.
    let store4: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let root = AgentId::new();
    let mid1 = AgentId::new();
    let mid2 = AgentId::new();
    let leaf = AgentId::new();
    let (session4, _) = seed_prompt_for(&*store4, leaf, "planner", "hello").await;
    let backend4 = Arc::new(TrackingBackend::new("b", vec![text_response("hi")]));
    let router4 = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate4 = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let hook4 = Arc::new(RecordingContextHook::new());
    let harness4 = build_loop_with_ancestry_and_hook(
        session4,
        leaf,
        vec![root, mid1, mid2],
        store4,
        router4,
        backend4,
        vec![],
        gate4,
        Budget::default(),
        HeadroomPolicy::default(),
        "planner",
        hook4.clone(),
    );
    let result4 = harness4.agent_loop.run().await;
    assert_eq!(result4.status, ResultStatus::Completed);

    let path1 = hook1.agent_paths()[0].clone();
    let path4 = hook4.agent_paths()[0].clone();

    assert_eq!(path1, vec![agent1]);
    assert_eq!(path4, vec![root, mid1, mid2, leaf]);
    assert_ne!(
        path1.len(),
        path4.len(),
        "a hook keyed on ctx.agent_path.len() must be able to tell a depth-1 agent \
         apart from a depth-4 one"
    );
}

// ---------------------------------------------------------------------
// `ContextHookCtx` at the
// `ContextHook::on_overflow` construction site (`agent_loop.rs`'s
// `route_and_attempt`) was REACHED but never OBSERVED by any test --
// unlike the `before_request` site above,
//). Measured, not assumed: stubbing `tag` at
// that second construction site to `None` left the full `--all-features`
// workspace suite green, 0 failed. `agent_path` and `artifacts` are built
// from the identical field-literal pattern as `tag` at both sites, so one
// test capturing the WHOLE `ContextHookCtx` an `on_overflow` call actually
// received closes all three at once, rather than filing three items.
// ---------------------------------------------------------------------

/// A `Router` double that returns one SCRIPTED result per `resolve` call,
/// in order -- unlike `CapturingRouter` above, which returns the same
/// result every time. Needed to drive `AgentLoop::route_and_attempt`'s
/// `ContextHook::on_overflow` retry loop: the first `resolve` must fail
/// with `ContextTooLarge` (entering the overflow branch at all), and the
/// second -- reached only after `on_overflow` hands back a payload to
/// retry with -- must succeed, or the turn can never reach
/// `ResultStatus::Completed` and nothing below would be exercising a real
/// `on_overflow` call.
struct SequencedRouter {
    results: Mutex<VecDeque<Result<Vec<Route>, RoutingError>>>,
}

impl SequencedRouter {
    fn new(results: Vec<Result<Vec<Route>, RoutingError>>) -> Self {
        Self {
            results: Mutex::new(results.into()),
        }
    }
}

impl Router for SequencedRouter {
    fn resolve(&self, _req: &RouteRequest) -> Result<Vec<Route>, RoutingError> {
        self.results.lock().unwrap().pop_front().expect(
            "SequencedRouter script exhausted -- fixture called resolve() more times \
             than scripted",
        )
    }
}

/// Records every `ContextHookCtx` a registered `ContextHook` sees at its
/// two call sites SEPARATELY, so a test can assert on what `on_overflow`
/// actually received without conflating it with the (already-guarded)
/// `before_request` call earlier in the same turn. `on_overflow` passes
/// `payload` through unchanged and returns `Some` -- observing, not
/// transforming; returning `Some` (rather than the trait's default `None`)
/// is what lets `route_and_attempt`'s retry loop reach a second
/// `Router::resolve` call at all.
struct RecordingOverflowHook {
    before_request_calls: Mutex<Vec<ContextHookCtx>>,
    overflow_calls: Mutex<Vec<ContextHookCtx>>,
}

impl RecordingOverflowHook {
    fn new() -> Self {
        Self {
            before_request_calls: Mutex::new(Vec::new()),
            overflow_calls: Mutex::new(Vec::new()),
        }
    }

    fn overflow_ctxs(&self) -> Vec<ContextHookCtx> {
        self.overflow_calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl ContextHook for RecordingOverflowHook {
    async fn before_request(
        &self,
        ctx: &ContextHookCtx,
        payload: ContextPayload,
    ) -> ContextPayload {
        self.before_request_calls.lock().unwrap().push(ctx.clone());
        payload
    }

    async fn on_overflow(
        &self,
        ctx: &ContextHookCtx,
        payload: ContextPayload,
        _overflow: OverflowInfo,
    ) -> Option<ContextPayload> {
        self.overflow_calls.lock().unwrap().push(ctx.clone());
        Some(payload)
    }
}

/// `ContextHookCtx` on the `on_overflow` call must carry the same
/// `agent_path` and `tag` the `before_request` call sees, and a
/// genuinely-working, agent-scoped `artifacts` handle -- proven by
/// actually driving one turn into the overflow retry over a four-agent
/// chain with a tagged spec, not by reading the two field-literal blocks
/// in `agent_loop.rs` side by side and concluding they agree.
#[tokio::test]
async fn context_hook_ctx_at_on_overflow_carries_agent_path_tag_and_a_working_artifacts_handle() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let root = AgentId::new();
    let mid1 = AgentId::new();
    let mid2 = AgentId::new();
    let leaf = AgentId::new();
    let (session, _) = seed_prompt_for(&*store, leaf, "planner", "hello").await;

    let backend = Arc::new(TrackingBackend::new("b", vec![text_response("done")]));

    let model = ModelRef {
        backend: BackendId::new("b"),
        model: ModelId::new("m"),
    };
    // First `resolve`: rejected as too large, entering the overflow
    // branch. Second `resolve` (after `on_overflow` hands back a payload
    // to retry with): succeeds, so the turn actually completes.
    let router = Arc::new(SequencedRouter::new(vec![
        Err(RoutingError::ContextTooLarge {
            role: RoleAlias::new("planner"),
            model: model.clone(),
            est_tokens: 30_000,
            headroom_tokens: 4_000,
            required_tokens: 34_000,
            max_context_tokens: 32_768,
            shortfall_tokens: 1_232,
        }),
        Ok(vec![make_route("b", "m")]),
    ]));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let hook = Arc::new(RecordingOverflowHook::new());

    let mut harness = build_loop_with_ancestry_and_hook(
        session,
        leaf,
        vec![root, mid1, mid2],
        store,
        router,
        backend,
        vec![],
        gate,
        Budget::default(),
        HeadroomPolicy::default(),
        "planner",
        hook.clone(),
    );
    // `build_loop_with_ancestry_and_hook` has no `tag` parameter (no
    // existing caller needs one) -- set it directly on the built spec,
    // exactly as `SubagentSpec::tag` is set on
    // a real fork/spawn child before that child's first turn ever runs.
    harness.agent_loop.spec.tag = Some("ticket-42".to_string());
    // A real, writable confinement-free cwd for the `artifacts` assertion
    // below -- `/tmp` literal (what `build_loop_inner` defaults to) would
    // work too, but a fresh tempdir avoids any cross-test collision on the
    // written file's name.
    let tmp = tempfile::tempdir().unwrap();
    harness.agent_loop.cwd = tmp.path().to_path_buf();

    let result = harness.agent_loop.run().await;
    assert_eq!(
        result.status,
        ResultStatus::Completed,
        "the overflow retry must actually succeed on the second Router::resolve call, or \
         nothing below is testing a real on_overflow ctx at all"
    );

    let calls = hook.overflow_ctxs();
    assert_eq!(
        calls.len(),
        1,
        "on_overflow must have fired exactly once for this fixture's single overflow"
    );
    let ctx = &calls[0];

    let expected_path = vec![root, mid1, mid2, leaf];
    assert_eq!(
        ctx.agent_path, expected_path,
        "ContextHookCtx::agent_path on the on_overflow call must be the same root-first, \
         self-inclusive chain the before_request call sees -- not the earlier default of \
         just [agent_id]"
    );
    // A one-level tree can't distinguish a working copy from an empty
    // vector -- assert this fixture is genuinely depth-four.
    assert_eq!(ctx.agent_path.len(), 4);

    assert_eq!(
        ctx.tag,
        Some("ticket-42".to_string()),
        "ContextHookCtx::tag on the on_overflow call must be the consumer's tag, not None -- \
         this is the field was filed over"
    );

    assert_eq!(ctx.agent_id, leaf);
    assert_eq!(ctx.session_id, session);
    assert_eq!(
        ctx.model,
        Some(model),
        "on_overflow's ctx.model must be the specific route that was found to overflow"
    );

    // `artifacts`: `ArtifactWriteHandle` has neither `PartialEq` nor an
    // `agent_id` getter (by design -- see that type's own doc), so the
    // only way to prove this is a REAL, working, agent-scoped handle
    // (rather than e.g. a stray `ArtifactWriteHandle::noop` a future edit
    // could substitute unnoticed) is to actually write through it and see
    // the bytes land on disk.
    let written = ctx
        .artifacts
        .write("overflow-hook-proof.txt", b"proof".to_vec())
        .await
        .expect("a working ArtifactWriteHandle must accept this write");
    let on_disk = tokio::fs::read(&written).await.expect(
        "the write must have actually landed on disk at the path ArtifactWriteHandle \
         returned -- a stray ArtifactWriteHandle::noop reports success without writing \
         anything, which this read would catch",
    );
    assert_eq!(
        on_disk, b"proof",
        "the bytes read back from disk must be exactly what was written"
    );
}

// ---------------------------------------------------------------------
// `ToolObserver`: the seam that lets loop-intervention policy live outside
// the core. The pair below is the parity check for moving repeated-step
// detection into `conway-plugin-stepguard` -- the same three identical calls
// must produce a note with an observer installed, and nothing at all
// without one. The second half is the half that matters: `PHILOSOPHY.md` §6
// says writing no policy is a real option, and it only is if a default
// build genuinely does nothing.
// ---------------------------------------------------------------------

/// Counts calls it sees and asks for a note on the third identical one --
/// the smallest thing shaped like the real plugin, so this test exercises
/// the RUNTIME's seam rather than the plugin's policy (which has its own
/// tests, in its own crate, where it belongs).
struct NoteOnThird {
    seen: Mutex<u32>,
}

#[async_trait]
impl conway_core::ports::ToolObserver for NoteOnThird {
    async fn after_tool_call(
        &self,
        _ctx: &conway_core::ports::ObserverCtx,
        call: &conway_core::ports::ObservedCall,
    ) -> conway_core::ports::ObserverAnswer {
        let mut seen = self.seen.lock().unwrap();
        *seen += 1;
        if *seen == 3 {
            conway_core::ports::ObserverAnswer {
                notes: vec![conway_core::ports::ObserverNote {
                    text: format!("saw `{}` three times", call.tool),
                    reason: "repeated_step".to_string(),
                }],
            }
        } else {
            conway_core::ports::ObserverAnswer::default()
        }
    }
}

/// Observation must never fail the thing it observed -- the call already ran
/// its side effects by the time an observer sees it.
struct PanickingObserver;

#[async_trait]
impl conway_core::ports::ToolObserver for PanickingObserver {
    async fn after_tool_call(
        &self,
        _ctx: &conway_core::ports::ObserverCtx,
        _call: &conway_core::ports::ObservedCall,
    ) -> conway_core::ports::ObserverAnswer {
        panic!("observer blew up");
    }
}

async fn run_three_identical_calls(
    observers: Vec<conway_core::ports::RegisteredObserver>,
) -> Vec<LogRecord> {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "planner", "hello").await;
    let backend = Arc::new(TrackingBackend::new(
        "b",
        vec![
            tool_call_response("tc_1", "read", serde_json::json!({"path": "a.txt"})),
            tool_call_response("tc_2", "read", serde_json::json!({"path": "a.txt"})),
            tool_call_response("tc_3", "read", serde_json::json!({"path": "a.txt"})),
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

    let harness = build_loop_inner(
        session,
        agent,
        vec![],
        store.clone(),
        router,
        backend,
        vec![tool],
        gate,
        Budget::default(),
        HeadroomPolicy::default(),
        None,
        "planner",
        None,
        None,
        observers,
    );
    let result = harness.agent_loop.run().await;
    assert_eq!(result.status, ResultStatus::Completed);

    store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .unwrap()
}

fn repeated_step_notes(records: &[LogRecord]) -> Vec<String> {
    records
        .iter()
        .filter_map(|r| match r {
            LogRecord::SystemNote { text, reason, .. } if reason == "repeated_step" => {
                Some(text.clone())
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_installed_observer_can_add_a_note_the_model_will_read() {
    let observer = Arc::new(NoteOnThird {
        seen: Mutex::new(0),
    });
    let records = run_three_identical_calls(vec![conway_core::ports::RegisteredObserver {
        plugin_id: "test.stepguard".to_string(),
        observer,
    }])
    .await;

    let notes = repeated_step_notes(&records);
    assert_eq!(
        notes.len(),
        1,
        "the observer's note must be appended to the durable log exactly once"
    );
    assert!(
        notes[0].contains("three times"),
        "note text: {:?}",
        notes[0]
    );
}

/// The half that proves the move actually removed something. Before this
/// change the runtime produced this note on its own; now, with no observing
/// plugin installed, the log must contain nothing of the kind.
#[tokio::test]
async fn with_no_observer_installed_the_runtime_writes_no_notes_of_its_own() {
    let records = run_three_identical_calls(Vec::new()).await;
    assert!(
        repeated_step_notes(&records).is_empty(),
        "a default build holds no repeated-step policy: {:?}",
        repeated_step_notes(&records)
    );
}

#[tokio::test]
async fn a_panicking_observer_does_not_fail_the_call_it_observed() {
    let records = run_three_identical_calls(vec![conway_core::ports::RegisteredObserver {
        plugin_id: "test.panics".to_string(),
        observer: Arc::new(PanickingObserver),
    }])
    .await;

    // `run_three_identical_calls` already asserts the run reached
    // `Completed`; this pins that the tool results still landed, so the
    // panic was contained rather than merely not propagated to the status.
    let results = records
        .iter()
        .filter(|r| matches!(r, LogRecord::ToolResultRecord { .. }))
        .count();
    assert_eq!(results, 3, "every tool result must still be recorded");
}

/// A tool call the assembler had to drop reaches the caller's report, and
/// **survives a registered `ContextHook`**.
///
/// The second half is the one worth a real turn rather than a unit test.
/// `retotal` rebuilds the report from scratch after a hook edits the payload,
/// and by then the removed blocks are gone from `segments` -- so if it does
/// not carry the drop list forward explicitly, the field silently empties on
/// every turn a hook is registered. That would replace the silent drop this
/// field exists to expose with a second one, in the exact configuration an
/// operator installed a hook to gain visibility.
#[tokio::test]
async fn a_dropped_tool_call_reaches_the_report_slot_even_through_a_context_hook() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "planner", "hello").await;

    // An assistant turn with two calls, and a result for only one of them.
    let seq = store.head(&session).await.unwrap();
    store
        .append(
            &session,
            LogRecord::Assistant {
                seq,
                ts: Utc::now(),
                content: vec![
                    ContentBlock::ToolUse {
                        call_id: "answered".into(),
                        name: ToolName::new("read"),
                        arguments: serde_json::json!({"path": "a.txt"}),
                    },
                    ContentBlock::ToolUse {
                        call_id: "orphaned".into(),
                        name: ToolName::new("read"),
                        arguments: serde_json::json!({"path": "b.txt"}),
                    },
                ],
                model: ModelRef {
                    backend: BackendId::new("b"),
                    model: ModelId::new("m"),
                },
                route_reason: serde_json::json!({}),
                usage: conway_core::content::Usage::default(),
                stop: conway_core::content::StopReason::ToolUse,
            },
        )
        .await
        .unwrap();
    let seq = store.head(&session).await.unwrap();
    store
        .append(
            &session,
            LogRecord::ToolResultRecord {
                seq,
                ts: Utc::now(),
                result: conway_core::content::ToolResult {
                    call_id: "answered".into(),
                    tool: ToolName::new("read"),
                    blocks: vec![ContentBlock::Text {
                        text: "file contents".into(),
                    }],
                    is_error: false,
                    truncated: None,
                },
            },
        )
        .await
        .unwrap();

    let backend = Arc::new(TrackingBackend::new("b", vec![text_response("done")]));
    let router = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let report_slot = Arc::new(Mutex::new(None));
    // A pass-through hook: it changes nothing, so any difference in the
    // report is `retotal`'s doing, not the hook's.
    let hook: Arc<dyn ContextHook> = Arc::new(RecordingContextHook::new());

    let harness = build_loop_inner(
        session,
        agent,
        vec![],
        store,
        router,
        backend,
        vec![],
        gate,
        Budget::default(),
        HeadroomPolicy::default(),
        None,
        "planner",
        Some(report_slot.clone()),
        Some(hook),
        Vec::new(),
    );

    let result = harness.agent_loop.run().await;
    assert_eq!(result.status, ResultStatus::Completed);

    let report = report_slot
        .lock()
        .unwrap()
        .clone()
        .expect("report slot populated by the time the loop finishes");
    assert_eq!(
        report.dropped,
        vec!["orphaned".to_string()],
        "the unanswered call must be named in the report the caller reads, \
         after the hook pass as well as before it"
    );
}

// ---------------------------------------------------------------------
// A `ContextHook` that orphans a tool call/result pair (board item
// `01M00RGARPESWXYAVY960KDE7S`): `ContextBuilder::build` guarantees no
// rendered context carries a tool call without its result, but that
// guarantee only covers ITS OWN output -- a hook edits an already-coherent
// list and can undo the guarantee just as easily as a mid-batch prefix cut
// created the problem `drop_unanswered_tool_calls` exists for in the first
// place. These are the regression tests: the seam refuses (never repairs)
// either direction, at the real `AgentLoop::run_inner` `before_request`
// call site, not merely in `context::hook_guard`'s own unit tests.
// ---------------------------------------------------------------------

/// Which half of a tool call/result pair [`OrphaningHook`] strips out of
/// whatever `ContextBuilder::build` (or a prior hook pass) already
/// assembled -- the two directions [`conway_runtime::context::hook_guard`]
/// (unit-tested directly in that module) must both refuse.
#[derive(Clone, Copy)]
enum OrphanDirection {
    /// Drop every segment carrying a `ToolResultBlock`, stranding its
    /// `ToolUse` -- the direction `drop_unanswered_tool_calls` ALSO
    /// handles, but never sees again once a hook runs after it.
    DropResult,
    /// Drop every segment carrying a `ToolUse`, stranding its
    /// `ToolResultBlock` -- the direction `drop_unanswered_tool_calls`
    /// never handled even before hooks existed (ordering made it
    /// impossible on assembly's own output).
    DropCall,
}

/// A `ContextHook` that always orphans one direction of a tool call/result
/// pair, so the request it hands back is exactly what an operator's
/// mis-curating hook (a masking rule that matches too broadly, a stale
/// `ContextMask`) could produce for real.
struct OrphaningHook {
    direction: OrphanDirection,
}

fn segment_carries(
    segment: &PromptSegment,
    is_the_orphaned_kind: impl Fn(&ContentBlock) -> bool,
) -> bool {
    segment.content.iter().any(is_the_orphaned_kind)
}

#[async_trait]
impl ContextHook for OrphaningHook {
    async fn before_request(
        &self,
        _ctx: &ContextHookCtx,
        payload: ContextPayload,
    ) -> ContextPayload {
        let direction = self.direction;
        let segments = payload
            .segments
            .into_iter()
            .filter(|segment| match direction {
                OrphanDirection::DropResult => !segment_carries(segment, |block| {
                    matches!(block, ContentBlock::ToolResultBlock { .. })
                }),
                OrphanDirection::DropCall => !segment_carries(segment, |block| {
                    matches!(block, ContentBlock::ToolUse { .. })
                }),
            })
            .collect();
        ContextPayload {
            segments,
            tools: payload.tools,
        }
    }
}

/// Seeds a session whose own log already carries ONE fully-answered tool
/// call/result pair (`call_id`) -- coherent by the time `ContextBuilder::
/// build` runs, so any incoherence a test observes afterward is provably
/// the registered hook's doing, not `drop_unanswered_tool_calls`'s.
async fn seed_answered_tool_call(store: &dyn SessionStore, call_id: &str) -> (SessionId, AgentId) {
    let (session, agent) = seed_prompt(store, "planner", "hello").await;

    let seq = store.head(&session).await.unwrap();
    store
        .append(
            &session,
            LogRecord::Assistant {
                seq,
                ts: Utc::now(),
                content: vec![ContentBlock::ToolUse {
                    call_id: call_id.to_string(),
                    name: ToolName::new("read"),
                    arguments: serde_json::json!({"path": "a.txt"}),
                }],
                model: ModelRef {
                    backend: BackendId::new("b"),
                    model: ModelId::new("m"),
                },
                route_reason: serde_json::json!({}),
                usage: Usage::default(),
                stop: StopReason::ToolUse,
            },
        )
        .await
        .unwrap();
    let seq = store.head(&session).await.unwrap();
    store
        .append(
            &session,
            LogRecord::ToolResultRecord {
                seq,
                ts: Utc::now(),
                result: conway_core::content::ToolResult {
                    call_id: call_id.to_string(),
                    tool: ToolName::new("read"),
                    blocks: vec![ContentBlock::Text {
                        text: "file contents".into(),
                    }],
                    is_error: false,
                    truncated: None,
                },
            },
        )
        .await
        .unwrap();

    (session, agent)
}

/// A `before_request` hook that drops the `ToolResultBlock` segment out of
/// an otherwise-answered pair is refused, not silently sent -- the request
/// it would have produced is exactly the shape every provider rejects
/// outright (`kimi/k3`: "an assistant message with 'tool_calls' must be
/// followed by tool messages responding to each 'tool_call_id'"). The
/// failure names the orphaned `call_id` and the hook method that produced
/// it, rather than surfacing as an opaque backend error.
#[tokio::test]
async fn a_hook_dropping_a_tool_result_segment_is_refused_not_silently_sent() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_answered_tool_call(&*store, "call_missing_result").await;

    let backend = Arc::new(TrackingBackend::new("b", vec![text_response("done")]));
    let router = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let hook: Arc<dyn ContextHook> = Arc::new(OrphaningHook {
        direction: OrphanDirection::DropResult,
    });

    let harness = build_loop_inner(
        session,
        agent,
        vec![],
        store,
        router,
        backend.clone(),
        vec![],
        gate,
        Budget::default(),
        HeadroomPolicy::default(),
        None,
        "planner",
        None,
        Some(hook),
        Vec::new(),
    );

    let result = harness.agent_loop.run().await;

    match &result.status {
        ResultStatus::Failed { error } => {
            assert!(
                error.contains("call_missing_result"),
                "the orphaned call_id must be named in the failure: {error}"
            );
            assert!(
                error.contains("before_request"),
                "the responsible hook method must be named in the failure: {error}"
            );
        }
        other => panic!("expected ResultStatus::Failed naming the orphan, got {other:?}"),
    }
    assert!(
        backend.calls().is_empty(),
        "an incoherent request must never reach the backend at all -- refused, not sent"
    );
}

/// The other direction: a `before_request` hook drops the `ToolUse`
/// segment, stranding its `ToolResultBlock`. `drop_unanswered_tool_calls`
/// never handled this direction even before hooks existed -- see that
/// function's own (corrected) doc -- so this is the case that makes its old
/// "could not be made to fail" claim false.
#[tokio::test]
async fn a_hook_dropping_a_tool_use_segment_is_refused_not_silently_sent() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_answered_tool_call(&*store, "call_missing_use").await;

    let backend = Arc::new(TrackingBackend::new("b", vec![text_response("done")]));
    let router = Arc::new(CapturingRouter::ok(vec![make_route("b", "m")]));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let hook: Arc<dyn ContextHook> = Arc::new(OrphaningHook {
        direction: OrphanDirection::DropCall,
    });

    let harness = build_loop_inner(
        session,
        agent,
        vec![],
        store,
        router,
        backend.clone(),
        vec![],
        gate,
        Budget::default(),
        HeadroomPolicy::default(),
        None,
        "planner",
        None,
        Some(hook),
        Vec::new(),
    );

    let result = harness.agent_loop.run().await;

    match &result.status {
        ResultStatus::Failed { error } => {
            assert!(
                error.contains("call_missing_use"),
                "the orphaned call_id must be named in the failure: {error}"
            );
            assert!(
                error.contains("before_request"),
                "the responsible hook method must be named in the failure: {error}"
            );
        }
        other => panic!("expected ResultStatus::Failed naming the orphan, got {other:?}"),
    }
    assert!(
        backend.calls().is_empty(),
        "an incoherent request must never reach the backend at all -- refused, not sent"
    );
}
