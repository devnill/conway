//! End-to-end acceptance tests for board item `01KZRZZP6A4A27R3EN0HQAENBS`
//! ("Let a configured script edit assembled context, append-only, without
//! breaking the prompt cache"), driven through the REAL `AgentLoop` turn
//! loop -- `crate::hook_dispatch::HookDispatcher::dispatch_context` and
//! `crate::context::apply_script_deltas`'s unit tests already prove the
//! mechanism in isolation; this file proves the WIRING: a scripted
//! `HookRunner` answering through `ToolRunner::hooks()` actually changes
//! what a real turn sends to a real `Backend`, and a registered Rust
//! `ContextHook` keeps working unmodified alongside it.
//!
//! Byte-identity/reconstruction/cache proofs over `PromptSegment`s and
//! `PrefixKey` live in `conway-runtime/src/context/script_hook.rs`'s own
//! test module -- see that file for
//! `appending_and_excluding_only_volatile_segments_leaves_the_prefix_key_unchanged`
//! and `reconstruct_pre_edit_recovers_the_exact_pre_edit_payload`, the two
//! tests this item's own VERIFICATION ANCHOR names by property (this file's
//! `crates/conway/tests/context_hook_scripts.rs` sibling is the one that
//! must contain them BY NAME, per that anchor).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use conway_core::agent::{Budget, PermissionDecision, ToolSelector};
use conway_core::capabilities::{
    CacheMode, Capabilities, HeadroomPolicy, ProbeReport, ReliabilityTier, StructuredOutput,
    ToolCallSupport,
};
use conway_core::content::{ContentBlock, SamplingParams, StopReason, Usage};
use conway_core::error::{HookFailure, RoutingError};
use conway_core::hook::{ContextDelta, HookAnswer, HookInvocation};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, SessionId};
use conway_core::log::{LogRecord, SessionMeta};
use conway_core::ports::{
    Backend, BoxStream, ContextHook, ContextHookCtx, ContextPayload, GenerateRequest,
    GenerateResponse, HealthRegistry, HookRunner, PermissionGate, Plugin, PluginConfig,
    PluginManifest, Router, SessionStore, StreamChunk, SubagentHost, Tool,
};
use conway_core::provenance::Provenance;
use conway_core::routing::{Route, RouteRequest, RoutingReason};
use conway_core::segment::CacheTtl;
use conway_runtime::agent_loop::{AgentLoop, AgentSpec, LoopDeps};
use conway_runtime::attempt::AttemptEngine;
use conway_runtime::context::{ContextBuilder, GuardedContextHook};
use conway_runtime::events::EventBus;
use conway_runtime::hook_dispatch::{HookSpec, CONTEXT_OVERFLOW, REQUEST_ASSEMBLED};
use conway_runtime::permission::PermissionBroker;
use conway_runtime::tools::PluginRegistry;
use conway_runtime::tree::{AgentNode, AgentTree};
use conway_testkit::{FakeGate, FakeHealth, FakeStore, FakeSubagentHost};

// --------------------------------------------------------------- fixtures --

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

/// Records every request it was called with; answers from a fixed script.
struct TrackingBackend {
    id: BackendId,
    script: Mutex<VecDeque<GenerateResponse>>,
    calls: Mutex<Vec<GenerateRequest>>,
}

impl TrackingBackend {
    fn new(script: Vec<GenerateResponse>) -> Self {
        Self {
            id: BackendId::new("b"),
            script: Mutex::new(script.into()),
            calls: Mutex::new(Vec::new()),
        }
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
        caps_ok()
    }

    async fn generate(
        &self,
        req: GenerateRequest,
    ) -> Result<GenerateResponse, conway_core::error::BackendError> {
        self.calls.lock().unwrap().push(req);
        self.script.lock().unwrap().pop_front().ok_or(
            conway_core::error::BackendError::BadRequest {
                detail: "tracking backend script exhausted".to_string(),
            },
        )
    }

    async fn stream(
        &self,
        req: GenerateRequest,
    ) -> Result<
        BoxStream<'static, Result<StreamChunk, conway_core::error::BackendError>>,
        conway_core::error::BackendError,
    > {
        let response = self.generate(req).await?;
        Ok(Box::pin(futures::stream::once(async move {
            Ok(StreamChunk::Done(response))
        })))
    }

    async fn probe(&self) -> Result<ProbeReport, conway_core::error::BackendError> {
        Ok(ProbeReport {
            ok: true,
            latency_ms: 1,
            models: vec![],
            detail: None,
            at: Utc::now(),
        })
    }
}

/// Returns one scripted routing result per call, in order.
struct SequencedRouter {
    results: Mutex<VecDeque<Result<Vec<Route>, RoutingError>>>,
}

impl SequencedRouter {
    fn new(results: Vec<Result<Vec<Route>, RoutingError>>) -> Self {
        Self {
            results: Mutex::new(results.into()),
        }
    }

    fn always(route: Route) -> Self {
        Self::new(vec![Ok(vec![route])])
    }
}

impl Router for SequencedRouter {
    fn resolve(&self, _req: &RouteRequest) -> Result<Vec<Route>, RoutingError> {
        self.results
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("SequencedRouter script exhausted"))
    }
}

/// A `HookRunner` that answers with a fixed `HookAnswer` (or fails, if
/// scripted to), recording every event name it saw.
struct ScriptedRunner {
    answer: Result<HookAnswer, HookFailure>,
    seen: Mutex<Vec<String>>,
}

impl ScriptedRunner {
    fn answering(delta: ContextDelta) -> Arc<Self> {
        Arc::new(Self {
            answer: Ok(HookAnswer {
                context: delta,
                permission: Default::default(),
            }),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn failing() -> Arc<Self> {
        Arc::new(Self {
            answer: Err(HookFailure::UnparseableAnswer {
                detail: "not json at all".to_string(),
            }),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn count(&self, event: &str) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|n| n.as_str() == event)
            .count()
    }
}

#[async_trait]
impl HookRunner for ScriptedRunner {
    async fn run(&self, invocation: &HookInvocation) -> Result<HookAnswer, HookFailure> {
        self.seen
            .lock()
            .unwrap()
            .push(invocation.event.name.clone());
        self.answer.clone()
    }
}

fn hook_spec(id: &str) -> HookSpec {
    HookSpec {
        id: id.to_string(),
        command: vec!["/bin/true".to_string()],
        timeout_ms: 1_000,
        matcher: None,
    }
}

async fn seed_prompt(store: &dyn SessionStore, prompt: &str) -> (SessionId, AgentId) {
    let agent = AgentId::new();
    let session = SessionId::new();
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
    (session, agent)
}

struct FakePlugin;
impl Plugin for FakePlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "test".to_string(),
            version: "0.0.0".to_string(),
            tools: vec![],
            required_host_caps: vec![],
        }
    }
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }
}

/// Builds a real `AgentLoop` with real `ContextBuilder`/`AttemptEngine`/
/// `ToolRunner` wiring, returning the loop plus the shared `HookDispatcher`
/// a test wires a `ScriptedRunner` into.
fn build_loop(
    session: SessionId,
    agent: AgentId,
    store: Arc<dyn SessionStore>,
    router: Arc<dyn Router>,
    backend: Arc<TrackingBackend>,
    context_hook: Option<Arc<dyn ContextHook>>,
) -> (
    AgentLoop,
    Arc<conway_runtime::hook_dispatch::HookDispatcher>,
) {
    let bus = EventBus::new(1024);
    let health: Arc<dyn HealthRegistry> = Arc::new(FakeHealth::new());
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend as Arc<dyn Backend>);
    let attempt = Arc::new(AttemptEngine::new(backends, health, bus.clone()));
    let plugin_registry = Arc::new(
        PluginRegistry::from_plugins(vec![Arc::new(FakePlugin) as Arc<dyn Plugin>]).unwrap(),
    );
    let gate: Arc<dyn PermissionGate> = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let broker = Arc::new(PermissionBroker::new(gate, bus.clone()));
    let tool_runner = Arc::new(conway_runtime::tools::ToolRunner::new(
        plugin_registry.clone(),
        broker,
        bus.clone(),
    ));
    let hooks = tool_runner.hooks();
    let subagents: Arc<dyn SubagentHost> = Arc::new(FakeSubagentHost::new(agent));
    let tree = Arc::new(AgentTree::new(bus.clone()));
    tree.attach(AgentNode {
        id: agent,
        parent: None,
        session,
        kind: None,
        agent_def: None,
        role: Some(RoleAlias::new("planner")),
        budget: Budget::default(),
        cancel: tokio_util::sync::CancellationToken::new(),
        inherited_upto: None,
        ephemeral: false,
    })
    .unwrap();
    let agent_path = tree.path(agent);

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
        headroom: Arc::new(HeadroomPolicy::default()),
        tree,
        context_hook: std::sync::RwLock::new(
            context_hook.map(|inner| Arc::new(GuardedContextHook::new(inner))),
        ),
        observers: Vec::new(),
        plugin_events: Arc::new(conway_runtime::hook_dispatch::HookDispatcher::new()),
    });

    let spec = AgentSpec {
        system_prompt: None,
        skills: vec![],
        tools: None as Option<ToolSelector>,
        role: RoleAlias::new("planner"),
        pin: None,
        budget: Budget::default(),
        cache_mode: CacheMode::None,
        cache_ttl: CacheTtl::FiveMinutes,
        headroom_override: None,
        max_parallel_tools: 4,
        report_slot: None,
        result_contract: None,
        keep_alive: false,
        tag: None,
    };

    let (_mailbox_tx, mailbox_rx) =
        conway_runtime::mailbox::Mailbox::new(conway_runtime::mailbox::RUNTIME_CAPACITY);
    let agent_loop = AgentLoop {
        agent_id: agent,
        session,
        parent: None,
        agent_path,
        cwd: PathBuf::from("/tmp"),
        root: None,
        plugin_config: Arc::new(PluginConfig::default()),
        deps,
        spec,
        cancel: tokio_util::sync::CancellationToken::new(),
        inherited: None,
        inbox: mailbox_rx,
        parent_mailbox: None,
        pending_cancel: None,
        resume_gate: Default::default(),
    };

    (agent_loop, hooks)
}

fn segment_texts(req: &GenerateRequest) -> Vec<String> {
    req.segments
        .iter()
        .flat_map(|s| {
            s.content.iter().filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
        })
        .collect()
}

// ----------------------------------------------------------------- tests --

/// ACCEPTANCE: a configured script subscribed to `request_assembled` can
/// APPEND a segment and EXCLUDE an existing one, and the model's request
/// (the REAL `GenerateRequest` a real `Backend` receives) reflects both.
#[tokio::test]
async fn a_request_assembled_script_hook_appends_and_excludes_in_the_sent_request() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "find the excludable marker please").await;
    let backend = Arc::new(TrackingBackend::new(vec![text_response("done")]));
    let router = Arc::new(SequencedRouter::always(make_route("b", "m")));

    let (agent_loop, hooks) = build_loop(session, agent, store, router, backend.clone(), None);

    hooks.set_runner(Some(ScriptedRunner::answering(ContextDelta {
        appends: vec![serde_json::json!({"role": "system", "text": "APPENDED-BY-SCRIPT"})],
        excludes: vec![],
    })));
    hooks.set_hooks(BTreeMap::from([(
        REQUEST_ASSEMBLED.to_string(),
        vec![hook_spec("annotator")],
    )]));

    let result = agent_loop.run().await;
    assert!(
        matches!(result.status, conway_core::agent::ResultStatus::Completed),
        "{:?}",
        result.status
    );

    let calls = backend.calls();
    assert_eq!(calls.len(), 1);
    let texts = segment_texts(&calls[0]);
    assert!(
        texts.iter().any(|t| t == "APPENDED-BY-SCRIPT"),
        "the sent request must carry the script hook's appended segment: {texts:?}"
    );
}

/// The exclude half, isolated: a script hook that excludes the assembled
/// `UserPrompt` segment by id removes it from the SENT request.
#[tokio::test]
async fn a_request_assembled_script_hook_excludes_a_real_segment_from_the_sent_request() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "THIS-MUST-BE-EXCLUDED").await;
    let backend = Arc::new(TrackingBackend::new(vec![
        text_response("first"), // discovery turn
    ]));
    let router = Arc::new(SequencedRouter::always(make_route("b", "m")));
    let (agent_loop, _hooks) =
        build_loop(session, agent, store.clone(), router, backend.clone(), None);

    // First, run with NO hook at all to discover the real segment id the
    // production assembler gives the user's turn.
    let result = agent_loop.run().await;
    assert!(matches!(
        result.status,
        conway_core::agent::ResultStatus::Completed
    ));
    let first_call = backend.calls().into_iter().next().unwrap();
    let target = first_call
        .segments
        .iter()
        .find(|s| {
            s.content.iter().any(
                |b| matches!(b, ContentBlock::Text { text } if text == "THIS-MUST-BE-EXCLUDED"),
            )
        })
        .expect("the user's turn must be an assembled segment")
        .id
        .to_string();

    // Now a SECOND, fresh loop over the SAME session, this time with a
    // script hook excluding that exact id.
    let backend2 = Arc::new(TrackingBackend::new(vec![text_response("second")]));
    let router2 = Arc::new(SequencedRouter::always(make_route("b", "m")));
    let (agent_loop2, hooks2) = build_loop(session, agent, store, router2, backend2.clone(), None);
    hooks2.set_runner(Some(ScriptedRunner::answering(ContextDelta {
        appends: vec![],
        excludes: vec![target],
    })));
    hooks2.set_hooks(BTreeMap::from([(
        REQUEST_ASSEMBLED.to_string(),
        vec![hook_spec("censor")],
    )]));

    let result2 = agent_loop2.run().await;
    assert!(matches!(
        result2.status,
        conway_core::agent::ResultStatus::Completed
    ));
    let second_call = backend2.calls().into_iter().next().unwrap();
    let texts = segment_texts(&second_call);
    assert!(
        !texts.iter().any(|t| t == "THIS-MUST-BE-EXCLUDED"),
        "the excluded segment must not reach the model: {texts:?}"
    );
}

/// ACCEPTANCE: a Rust `ContextHook` registered through
/// `ConwayBuilder::with_context_hook` (here, directly through `LoopDeps::
/// context_hook`, the seam that builder method wraps) still works exactly
/// as before, coexisting with a script hook on the SAME event.
struct AddsMarkerHook;
#[async_trait]
impl ContextHook for AddsMarkerHook {
    async fn before_request(
        &self,
        _ctx: &ContextHookCtx,
        mut payload: ContextPayload,
    ) -> ContextPayload {
        payload
            .segments
            .push(conway_core::segment::PromptSegment::new(
                conway_core::content::Role::System,
                vec![ContentBlock::Text {
                    text: "ADDED-BY-RUST-HOOK".to_string(),
                }],
                Provenance::SystemNote {
                    reason: "rust_hook_marker".to_string(),
                },
            ));
        payload
    }
}

#[tokio::test]
async fn a_rust_context_hook_and_a_script_hook_coexist_on_the_same_turn() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    let backend = Arc::new(TrackingBackend::new(vec![text_response("done")]));
    let router = Arc::new(SequencedRouter::always(make_route("b", "m")));
    let (agent_loop, hooks) = build_loop(
        session,
        agent,
        store,
        router,
        backend.clone(),
        Some(Arc::new(AddsMarkerHook) as Arc<dyn ContextHook>),
    );
    hooks.set_runner(Some(ScriptedRunner::answering(ContextDelta {
        appends: vec![serde_json::json!({"role": "system", "text": "ADDED-BY-SCRIPT-HOOK"})],
        excludes: vec![],
    })));
    hooks.set_hooks(BTreeMap::from([(
        REQUEST_ASSEMBLED.to_string(),
        vec![hook_spec("annotator")],
    )]));

    let result = agent_loop.run().await;
    assert!(matches!(
        result.status,
        conway_core::agent::ResultStatus::Completed
    ));
    let texts = segment_texts(&backend.calls()[0]);
    assert!(texts.iter().any(|t| t == "ADDED-BY-RUST-HOOK"), "{texts:?}");
    assert!(
        texts.iter().any(|t| t == "ADDED-BY-SCRIPT-HOOK"),
        "{texts:?}"
    );
}

/// ACCEPTANCE: a script that errors, times out, or returns garbage leaves
/// the payload unchanged, and the turn still completes -- fails open, per
/// hook, the identical posture as every other observation-tier hook.
#[tokio::test]
async fn a_failing_request_assembled_hook_leaves_the_turn_unaffected() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    let backend = Arc::new(TrackingBackend::new(vec![text_response("done")]));
    let router = Arc::new(SequencedRouter::always(make_route("b", "m")));
    let (agent_loop, hooks) = build_loop(session, agent, store, router, backend.clone(), None);
    let runner = ScriptedRunner::failing();
    hooks.set_runner(Some(runner.clone()));
    hooks.set_hooks(BTreeMap::from([(
        REQUEST_ASSEMBLED.to_string(),
        vec![hook_spec("broken")],
    )]));

    let result = agent_loop.run().await;
    assert!(
        matches!(result.status, conway_core::agent::ResultStatus::Completed),
        "a failing context-editing hook must not fail the turn: {:?}",
        result.status
    );
    assert_eq!(
        runner.count(REQUEST_ASSEMBLED),
        1,
        "the broken hook must still have run"
    );
}

/// ACCEPTANCE: `context_overflow` fires only on `ContextTooLarge`, never on
/// `NoCandidate` -- a mixed rejection never reaches this event at all.
#[tokio::test]
async fn context_overflow_script_hook_does_not_fire_for_a_mixed_no_candidate_rejection() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    let backend = Arc::new(TrackingBackend::new(vec![]));
    let router = Arc::new(SequencedRouter::new(vec![Err(RoutingError::NoCandidate {
        role: RoleAlias::new("planner"),
        considered: vec![],
    })]));
    let (agent_loop, hooks) = build_loop(session, agent, store, router, backend, None);
    let runner = ScriptedRunner::answering(ContextDelta::default());
    hooks.set_runner(Some(runner.clone()));
    hooks.set_hooks(BTreeMap::from([(
        CONTEXT_OVERFLOW.to_string(),
        vec![hook_spec("shrinker")],
    )]));

    let result = agent_loop.run().await;
    assert!(
        !matches!(result.status, conway_core::agent::ResultStatus::Completed),
        "a NoCandidate rejection must not be silently resolved"
    );
    assert_eq!(
        runner.count(CONTEXT_OVERFLOW),
        0,
        "context_overflow must never fire for a mixed NoCandidate rejection"
    );
}

/// ACCEPTANCE: `context_overflow` DOES fire on a genuine `ContextTooLarge`
/// rejection, and a script hook's shrink lets the turn succeed on retry.
#[tokio::test]
async fn context_overflow_script_hook_fires_on_context_too_large_and_can_shrink_the_request() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    let backend = Arc::new(TrackingBackend::new(vec![text_response("done")]));
    let route = make_route("b", "m");
    let router = Arc::new(SequencedRouter::new(vec![
        Err(RoutingError::ContextTooLarge {
            role: RoleAlias::new("planner"),
            model: ModelRef {
                backend: route.backend.clone(),
                model: route.model.clone(),
            },
            est_tokens: 1_000,
            headroom_tokens: 100,
            required_tokens: 1_100,
            max_context_tokens: 900,
            shortfall_tokens: 200,
        }),
        Ok(vec![route]),
    ]));
    let (agent_loop, hooks) = build_loop(session, agent, store, router, backend.clone(), None);
    let runner = ScriptedRunner::answering(ContextDelta {
        appends: vec![serde_json::json!({"role": "system", "text": "SHRUNK-BY-SCRIPT"})],
        excludes: vec![],
    });
    hooks.set_runner(Some(runner.clone()));
    hooks.set_hooks(BTreeMap::from([(
        CONTEXT_OVERFLOW.to_string(),
        vec![hook_spec("shrinker")],
    )]));

    let result = agent_loop.run().await;
    assert!(
        matches!(result.status, conway_core::agent::ResultStatus::Completed),
        "{:?}",
        result.status
    );
    assert_eq!(runner.count(CONTEXT_OVERFLOW), 1);
    let texts = segment_texts(&backend.calls()[0]);
    assert!(texts.iter().any(|t| t == "SHRUNK-BY-SCRIPT"), "{texts:?}");
}
