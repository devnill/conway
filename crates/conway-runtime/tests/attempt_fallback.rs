//! Acceptance tests: strategy resolution, fallback chain,
//! headroom-aware T-1 context gate, and health recording for
//! `conway_runtime::attempt::AttemptEngine`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use conway_core::capabilities::{
    CacheMode, Capabilities, ProbeReport, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{
    ContentBlock, PermissionClass, Role, SamplingParams, StopReason, ToolCategory, ToolSpec, Usage,
};
use conway_core::error::{BackendError, RoutingError, RuntimeError};
use conway_core::event::Event;
use conway_core::fakes::FakeHealth;
use conway_core::ids::{
    AgentId, BackendId, EndpointId, ModelId, ModelRef, RoleAlias, SessionId, ToolName,
};
use conway_core::ports::{
    check_admission, Admission, Backend, BoxStream, GenerateRequest, GenerateResponse,
    HealthRegistry, StreamChunk,
};
use conway_core::provenance::Provenance;
use conway_core::routing::{BreakerKind, BreakerState, Route, RoutingReason};
use conway_core::segment::{CacheTtl, PromptSegment};
use conway_runtime::attempt::{AttemptEngine, AttemptRequest};
use conway_runtime::events::EventBus;
use futures::future::FutureExt;
use futures::stream::{self, StreamExt};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------
// A local scripted `Backend` double that -- unlike `conway_core::fakes`'
// `ScriptedBackend` -- distinguishes `generate()` calls from `stream()`
// calls, so strategy-selection and the ToolParse-retry mechanic (which
// switches strategy mid-chain) are directly observable. `ScriptedBackend`'s
// `stream()` is implemented in terms of its own `generate()`, which makes
// the two indistinguishable from a test's point of view.
// ---------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Turn {
    Respond(GenerateResponse),
    Fail(BackendError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    Generate,
    Stream,
}

#[derive(Debug)]
struct RecordingBackend {
    id: BackendId,
    caps: Capabilities,
    /// Feeds this backend's own `admit` override (see the `Backend` impl
    /// below) --: `AttemptEngine`
    /// now gates admission through `Backend::admit` over the actually-built
    /// `GenerateRequest`, not `AttemptRequest.est_tokens`, so a test
    /// exercising the T-1 gate needs a way to control the ESTIMATE a
    /// candidate reports, independent of this file's tiny real fixture
    /// segments (`a_segment()`'s "hi" would otherwise estimate a token
    /// count of a handful, never large enough to exercise rejection).
    est_tokens: u32,
    script: Mutex<VecDeque<Turn>>,
    calls: Mutex<Vec<(Method, GenerateRequest)>>,
}

impl RecordingBackend {
    fn new(id: &str, caps: Capabilities, est_tokens: u32, script: Vec<Turn>) -> Self {
        Self {
            id: BackendId::new(id),
            caps,
            est_tokens,
            script: Mutex::new(script.into()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn generate_count(&self) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| *m == Method::Generate)
            .count()
    }

    fn stream_count(&self) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| *m == Method::Stream)
            .count()
    }

    fn requests(&self) -> Vec<GenerateRequest> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(_, r)| r.clone())
            .collect()
    }

    fn next_turn(&self) -> Turn {
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .expect("RecordingBackend script exhausted")
    }
}

fn decompose(response: GenerateResponse) -> Vec<Result<StreamChunk, BackendError>> {
    let mut chunks: Vec<Result<StreamChunk, BackendError>> = response
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(Ok(StreamChunk::TextDelta(text.clone()))),
            ContentBlock::Thinking { text, .. } => {
                Some(Ok(StreamChunk::ThinkingDelta(text.clone())))
            }
            _ => None,
        })
        .collect();
    chunks.push(Ok(StreamChunk::Done(response)));
    chunks
}

#[async_trait]
impl Backend for RecordingBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn capabilities(&self, _model: &ModelId) -> Capabilities {
        self.caps.clone()
    }

    /// Overrides the trait default so tests control the estimate directly
    /// (see the `est_tokens` field's own doc) rather than the default
    /// dialect-neutral estimator's real (tiny) count of this file's fixture
    /// segments. Still calls the ONE shared arithmetic, `check_admission`
    /// () -- only the estimate is test-controlled, not the comparison.
    fn admit(
        &self,
        req: &GenerateRequest,
        headroom_tokens: u32,
    ) -> Result<Admission, BackendError> {
        check_admission(
            req.model.clone(),
            self.est_tokens,
            headroom_tokens,
            self.caps.max_context_tokens,
        )
    }

    async fn generate(&self, req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
        self.calls.lock().unwrap().push((Method::Generate, req));
        match self.next_turn() {
            Turn::Respond(r) => Ok(r),
            Turn::Fail(e) => Err(e),
        }
    }

    async fn stream(
        &self,
        req: GenerateRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
        self.calls.lock().unwrap().push((Method::Stream, req));
        match self.next_turn() {
            Turn::Respond(r) => Ok(stream::iter(decompose(r)).boxed()),
            Turn::Fail(e) => Ok(stream::iter(vec![Err(e)]).boxed()),
        }
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

fn caps(tool_calling: ToolCallSupport, max_context_tokens: u32) -> Capabilities {
    Capabilities {
        tool_calling,
        cache: CacheMode::None,
        parallel_tool_calls: false,
        structured_output: StructuredOutput::None,
        max_context_tokens,
        reasoning: false,
        reliability_tier: ReliabilityTier::Verified,
    }
}

fn model_ref(backend: &str, model: &str) -> ModelRef {
    ModelRef {
        backend: BackendId::new(backend),
        model: ModelId::new(model),
    }
}

fn route(backend: &str, model: &str, reason: RoutingReason) -> Route {
    Route {
        backend: BackendId::new(backend),
        model: ModelId::new(model),
        params: SamplingParams::default(),
        reason,
    }
}

fn primary(alias: &str) -> RoutingReason {
    RoutingReason::AliasPrimary {
        alias: RoleAlias::new(alias),
    }
}

fn fallback(position: u8) -> RoutingReason {
    RoutingReason::Fallback {
        position,
        after: Vec::new(),
    }
}

fn text_response(text: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage::default(),
    }
}

fn a_tool() -> ToolSpec {
    ToolSpec {
        name: ToolName::new("read"),
        description: "Read a file".into(),
        schema: schemars::schema_for!(std::collections::BTreeMap<String, String>),
        category: ToolCategory::Read,
        permission: PermissionClass::Safe,
    }
}

fn a_segment() -> PromptSegment {
    PromptSegment::new(
        Role::User,
        vec![ContentBlock::Text { text: "hi".into() }],
        Provenance::UserPrompt,
    )
}

fn backends_map(pairs: Vec<(&str, Arc<dyn Backend>)>) -> HashMap<BackendId, Arc<dyn Backend>> {
    pairs
        .into_iter()
        .map(|(id, b)| (BackendId::new(id), b))
        .collect()
}

struct Fixture {
    engine: AttemptEngine,
    health: Arc<FakeHealth>,
    bus: Arc<EventBus>,
}

fn fixture(backends: HashMap<BackendId, Arc<dyn Backend>>) -> Fixture {
    let health = Arc::new(FakeHealth::new());
    let bus = EventBus::new(1024);
    let engine = AttemptEngine::new(
        backends,
        health.clone() as Arc<dyn HealthRegistry>,
        bus.clone(),
    );
    Fixture {
        engine,
        health,
        bus,
    }
}

fn base_request<'a>(
    routes: Vec<Route>,
    segments: &'a [PromptSegment],
    tools: &'a [ToolSpec],
    est_tokens: u32,
    headroom: u32,
) -> AttemptRequest<'a> {
    AttemptRequest {
        agent_id: AgentId::new(),
        session: SessionId::new(),
        role: RoleAlias::new("planner"),
        routes,
        segments,
        tools,
        prefix_key: None,
        est_tokens,
        headroom,
        max_tokens_override: None,
        cache_ttl: CacheTtl::FiveMinutes,
        cancel: CancellationToken::new(),
    }
}

/// Drains every envelope already buffered on `stream` without waiting --
/// safe because the engine's `bus.emit` calls are synchronous and all run
/// to completion (on this single-threaded test) before this is called.
fn drain(stream: &mut conway_runtime::events::EventStream) -> Vec<Event> {
    let mut out = Vec::new();
    while let Some(Some(envelope)) = stream.next().now_or_never() {
        out.push(envelope.event);
    }
    out
}

// ---------------------------------------------------------------------
// Strategy table
// ---------------------------------------------------------------------

#[tokio::test]
async fn strategy_table_selects_stream_or_generate() {
    struct Row {
        tool_calling: ToolCallSupport,
        has_tools: bool,
        expect_stream: bool,
    }
    let rows = vec![
        Row {
            tool_calling: ToolCallSupport::Streaming { validated: true },
            has_tools: true,
            expect_stream: true,
        },
        Row {
            tool_calling: ToolCallSupport::Streaming { validated: false },
            has_tools: true,
            expect_stream: false,
        },
        Row {
            tool_calling: ToolCallSupport::NonStreamingOnly,
            has_tools: true,
            expect_stream: false,
        },
        Row {
            tool_calling: ToolCallSupport::NonStreamingOnly,
            has_tools: false,
            expect_stream: true,
        },
    ];

    for row in rows {
        let backend = Arc::new(RecordingBackend::new(
            "b1",
            caps(row.tool_calling, 100_000),
            100,
            vec![Turn::Respond(text_response("hi"))],
        ));
        let fx = fixture(backends_map(vec![(
            "b1",
            backend.clone() as Arc<dyn Backend>,
        )]));
        let segments = vec![a_segment()];
        let tools = if row.has_tools {
            vec![a_tool()]
        } else {
            vec![]
        };
        let routes = vec![route("b1", "m1", primary("planner"))];
        let req = base_request(routes, &segments, &tools, 100, 4_096);

        let outcome = fx.engine.execute(req).await.expect("should succeed");
        assert_eq!(outcome.attempts, 1);
        if row.expect_stream {
            assert_eq!(backend.stream_count(), 1, "expected stream() call");
            assert_eq!(backend.generate_count(), 0);
        } else {
            assert_eq!(backend.generate_count(), 1, "expected generate() call");
            assert_eq!(backend.stream_count(), 0);
        }
    }
}

// ---------------------------------------------------------------------
// generate() path still emits a full-text TextDelta
// ---------------------------------------------------------------------

#[tokio::test]
async fn generate_path_emits_full_text_delta() {
    let backend = Arc::new(RecordingBackend::new(
        "b1",
        caps(ToolCallSupport::NonStreamingOnly, 100_000),
        100,
        vec![Turn::Respond(text_response("hello world"))],
    ));
    let fx = fixture(backends_map(vec![("b1", backend as Arc<dyn Backend>)]));
    let mut sub = fx.bus.subscribe();
    let segments = vec![a_segment()];
    let tools = vec![a_tool()];
    let routes = vec![route("b1", "m1", primary("planner"))];
    let req = base_request(routes, &segments, &tools, 100, 4_096);

    fx.engine.execute(req).await.expect("should succeed");

    let events = drain(&mut sub);
    let found = events
        .iter()
        .any(|e| matches!(e, Event::TextDelta { text } if text == "hello world"));
    assert!(found, "expected a full-text TextDelta, got {events:?}");
}

// ---------------------------------------------------------------------
// ToolParse retry mechanic
// ---------------------------------------------------------------------

#[tokio::test]
async fn toolparse_triggers_one_retry_then_advances_chain() {
    let route_a_backend = Arc::new(RecordingBackend::new(
        "a",
        caps(ToolCallSupport::Streaming { validated: true }, 100_000),
        100,
        vec![
            Turn::Fail(BackendError::ToolParse {
                detail: "bad json".into(),
            }),
            Turn::Fail(BackendError::ToolParse {
                detail: "bad json again".into(),
            }),
        ],
    ));
    let route_b_backend = Arc::new(RecordingBackend::new(
        "b",
        caps(ToolCallSupport::Streaming { validated: true }, 100_000),
        100,
        vec![Turn::Respond(text_response("ok"))],
    ));
    let fx = fixture(backends_map(vec![
        ("a", route_a_backend.clone() as Arc<dyn Backend>),
        ("b", route_b_backend.clone() as Arc<dyn Backend>),
    ]));
    let segments = vec![a_segment()];
    let tools = vec![a_tool()];
    let routes = vec![
        route("a", "m1", primary("planner")),
        route("b", "m1", fallback(1)),
    ];
    let req = base_request(routes, &segments, &tools, 100, 4_096);

    let outcome = fx
        .engine
        .execute(req)
        .await
        .expect("should succeed via route b");
    assert_eq!(outcome.route.backend, BackendId::new("b"));
    assert_eq!(outcome.attempts, 3); // stream(a), generate(a) retry, stream(b)

    assert_eq!(route_a_backend.stream_count(), 1);
    assert_eq!(route_a_backend.generate_count(), 1);
    assert_eq!(route_b_backend.stream_count(), 1);

    // The retry sends the identical request (same model/segments/tools/params).
    let a_reqs = route_a_backend.requests();
    assert_eq!(a_reqs.len(), 2);
    assert_eq!(
        serde_json::to_value(&a_reqs[0]).unwrap(),
        serde_json::to_value(&a_reqs[1]).unwrap(),
        "retry must resend an identical GenerateRequest"
    );

    // ToolParse never feeds a health observation -- the only observation
    // recorded is the `Ok` from route b's eventual success.
    let obs = fx.health.observations();
    assert_eq!(obs.len(), 1);
    assert_eq!(obs[0].0, EndpointId::new("b"));
    assert!(matches!(
        obs[0].1,
        conway_core::routing::Observation::Ok { .. }
    ));
}

// ---------------------------------------------------------------------
// ModelDecision ordering and monotonic attempt counter
// ---------------------------------------------------------------------

#[tokio::test]
async fn model_decision_before_every_call_with_monotonic_attempt() {
    let a = Arc::new(RecordingBackend::new(
        "a",
        caps(ToolCallSupport::Streaming { validated: true }, 100_000),
        100,
        vec![Turn::Fail(BackendError::Transport { detail: "x".into() })],
    ));
    let b = Arc::new(RecordingBackend::new(
        "b",
        caps(ToolCallSupport::Streaming { validated: true }, 100_000),
        100,
        vec![Turn::Respond(text_response("ok"))],
    ));
    let fx = fixture(backends_map(vec![
        ("a", a as Arc<dyn Backend>),
        ("b", b as Arc<dyn Backend>),
    ]));
    let mut sub = fx.bus.subscribe();
    let segments = vec![a_segment()];
    let tools: Vec<ToolSpec> = vec![];
    let routes = vec![
        route("a", "m1", primary("planner")),
        route("b", "m1", fallback(1)),
    ];
    let req = base_request(routes, &segments, &tools, 100, 4_096);

    fx.engine
        .execute(req)
        .await
        .expect("should succeed via route b");

    let events = drain(&mut sub);
    let decisions: Vec<u8> = events
        .iter()
        .filter_map(|e| match e {
            Event::ModelDecision { attempt, .. } => Some(*attempt),
            _ => None,
        })
        .collect();
    assert_eq!(decisions, vec![0, 1]);
}

// ---------------------------------------------------------------------
// Health recording per T-2 class
// ---------------------------------------------------------------------

#[tokio::test]
async fn health_records_transport_error_and_advances() {
    let a = Arc::new(RecordingBackend::new(
        "a",
        caps(ToolCallSupport::Streaming { validated: true }, 100_000),
        100,
        vec![Turn::Fail(BackendError::Transport { detail: "x".into() })],
    ));
    let b = Arc::new(RecordingBackend::new(
        "b",
        caps(ToolCallSupport::Streaming { validated: true }, 100_000),
        100,
        vec![Turn::Respond(text_response("ok"))],
    ));
    let fx = fixture(backends_map(vec![
        ("a", a as Arc<dyn Backend>),
        ("b", b as Arc<dyn Backend>),
    ]));
    let segments = vec![a_segment()];
    let tools: Vec<ToolSpec> = vec![];
    let routes = vec![
        route("a", "m1", primary("planner")),
        route("b", "m1", fallback(1)),
    ];
    let req = base_request(routes, &segments, &tools, 100, 4_096);

    fx.engine
        .execute(req)
        .await
        .expect("should succeed via route b");

    let obs = fx.health.observations();
    assert_eq!(obs.len(), 2); // transport error on a, Ok on b
    assert!(matches!(
        obs[0],
        (ref ep, conway_core::routing::Observation::TransportError) if *ep == EndpointId::new("a")
    ));
    assert!(matches!(
        obs[1],
        (ref ep, conway_core::routing::Observation::Ok { .. }) if *ep == EndpointId::new("b")
    ));
}

#[tokio::test]
async fn health_records_rate_limited() {
    let a = Arc::new(RecordingBackend::new(
        "a",
        caps(ToolCallSupport::Streaming { validated: true }, 100_000),
        100,
        vec![Turn::Fail(BackendError::RateLimit {
            retry_after_secs: Some(7),
        })],
    ));
    let b = Arc::new(RecordingBackend::new(
        "b",
        caps(ToolCallSupport::Streaming { validated: true }, 100_000),
        100,
        vec![Turn::Respond(text_response("ok"))],
    ));
    let fx = fixture(backends_map(vec![
        ("a", a as Arc<dyn Backend>),
        ("b", b as Arc<dyn Backend>),
    ]));
    let segments = vec![a_segment()];
    let tools: Vec<ToolSpec> = vec![];
    let routes = vec![
        route("a", "m1", primary("planner")),
        route("b", "m1", fallback(1)),
    ];
    let req = base_request(routes, &segments, &tools, 100, 4_096);

    fx.engine
        .execute(req)
        .await
        .expect("should succeed via route b");

    let obs = fx.health.observations();
    assert_eq!(
        obs[0].1,
        conway_core::routing::Observation::RateLimited {
            retry_after_secs: Some(7)
        }
    );
}

// ---------------------------------------------------------------------
// T-2: zero-record classes
// ---------------------------------------------------------------------

#[tokio::test]
async fn t2_context_overflow_and_bad_request_advance_with_zero_health_records() {
    for err in [
        BackendError::ContextOverflow {
            required_tokens: 1,
            max_context_tokens: 1,
        },
        BackendError::BadRequest { detail: "x".into() },
    ] {
        let a = Arc::new(RecordingBackend::new(
            "a",
            caps(ToolCallSupport::Streaming { validated: true }, 100_000),
            100,
            vec![Turn::Fail(err)],
        ));
        let b = Arc::new(RecordingBackend::new(
            "b",
            caps(ToolCallSupport::Streaming { validated: true }, 100_000),
            100,
            vec![Turn::Respond(text_response("ok"))],
        ));
        let fx = fixture(backends_map(vec![
            ("a", a as Arc<dyn Backend>),
            ("b", b as Arc<dyn Backend>),
        ]));
        let segments = vec![a_segment()];
        let tools: Vec<ToolSpec> = vec![];
        let routes = vec![
            route("a", "m1", primary("planner")),
            route("b", "m1", fallback(1)),
        ];
        let req = base_request(routes, &segments, &tools, 100, 4_096);

        fx.engine
            .execute(req)
            .await
            .expect("should succeed via route b");
        let obs = fx.health.observations();
        // Only the success on route b feeds a record.
        assert_eq!(obs.len(), 1);
        assert!(matches!(
            obs[0].1,
            conway_core::routing::Observation::Ok { .. }
        ));
    }
}

#[tokio::test]
async fn t2_auth_produces_zero_health_records_and_aborts_chain() {
    let a = Arc::new(RecordingBackend::new(
        "a",
        caps(ToolCallSupport::Streaming { validated: true }, 100_000),
        100,
        vec![Turn::Fail(BackendError::Auth {
            detail: "no key".into(),
        })],
    ));
    // Route b would succeed if reached -- it must not be.
    let b = Arc::new(RecordingBackend::new(
        "b",
        caps(ToolCallSupport::Streaming { validated: true }, 100_000),
        100,
        vec![Turn::Respond(text_response("ok"))],
    ));
    let fx = fixture(backends_map(vec![
        ("a", a.clone() as Arc<dyn Backend>),
        ("b", b.clone() as Arc<dyn Backend>),
    ]));
    let segments = vec![a_segment()];
    let tools: Vec<ToolSpec> = vec![];
    let routes = vec![
        route("a", "m1", primary("planner")),
        route("b", "m1", fallback(1)),
    ];
    let req = base_request(routes, &segments, &tools, 100, 4_096);

    let err = fx
        .engine
        .execute(req)
        .await
        .expect_err("Auth must abort the chain");
    assert!(matches!(
        err,
        RuntimeError::Backend(BackendError::Auth { .. })
    ));
    assert!(fx.health.observations().is_empty());
    assert_eq!(a.stream_count(), 1);
    assert_eq!(b.stream_count(), 0, "chain must not advance past Auth");
}

// ---------------------------------------------------------------------
// Breaker Closed -> Open transition emits exactly one BackendDegraded
// ---------------------------------------------------------------------

#[tokio::test]
async fn breaker_closed_to_open_emits_exactly_one_backend_degraded() {
    let a = Arc::new(RecordingBackend::new(
        "a",
        caps(ToolCallSupport::Streaming { validated: true }, 100_000),
        100,
        vec![
            Turn::Fail(BackendError::Transport { detail: "1".into() }),
            Turn::Fail(BackendError::Transport { detail: "2".into() }),
        ],
    ));
    let health = Arc::new(FakeHealth::new());
    let bus = EventBus::new(1024);
    let engine = AttemptEngine::new(
        backends_map(vec![("a", a.clone() as Arc<dyn Backend>)]),
        health.clone() as Arc<dyn HealthRegistry>,
        bus.clone(),
    );
    let mut sub = bus.subscribe();
    let segments = vec![a_segment()];
    let tools: Vec<ToolSpec> = vec![];

    // First failure: still Closed -> Closed (FakeHealth doesn't
    // auto-transition), so we manually flip state to Open right after
    // recording the first observation to simulate the breaker tripping,
    // then assert the SECOND failure (already Open) emits nothing further.
    let routes = vec![route("a", "m1", primary("planner"))];
    let req = base_request(routes, &segments, &tools, 100, 4_096);
    let err = engine.execute(req).await.expect_err("no candidates left");
    assert!(matches!(
        err,
        RuntimeError::Routing(RoutingError::NoCandidate { .. })
    ));

    // Simulate the breaker having tripped after that first observation.
    health.set_state(
        EndpointId::new("a"),
        BreakerState::Open {
            until: "2026-07-21T00:00:00Z".parse().unwrap(),
            kind: BreakerKind::Transport,
        },
    );

    let routes = vec![route("a", "m1", primary("planner"))];
    let req = base_request(routes, &segments, &tools, 100, 4_096);
    let _ = engine.execute(req).await;

    let events = drain(&mut sub);
    let degraded_count = events
        .iter()
        .filter(|e| matches!(e, Event::BackendDegraded { .. }))
        .count();
    // Neither call here observed a Closed -> Open edge (FakeHealth's state
    // is set directly, not derived from `record`), so both should emit
    // zero. This asserts the "no edge, no event" half of the contract; the
    // edge-triggered half is asserted by the transitioning fixture below.
    assert_eq!(degraded_count, 0);
}

/// A `HealthRegistry` whose `record` call flips Closed -> Open after N
/// observations for the same endpoint, so the edge-detection path in
/// `AttemptEngine` is exercised end-to-end (unlike `FakeHealth`, whose
/// state is only ever set directly by test setup).
#[derive(Debug, Default)]
struct TrippingHealth {
    counts: Mutex<HashMap<EndpointId, u32>>,
    observations: Mutex<Vec<(EndpointId, conway_core::routing::Observation)>>,
}

impl HealthRegistry for TrippingHealth {
    fn state(&self, ep: &EndpointId) -> BreakerState {
        let counts = self.counts.lock().unwrap();
        match counts.get(ep) {
            Some(n) if *n >= 1 => BreakerState::Open {
                until: "2026-07-21T00:00:00Z".parse().unwrap(),
                kind: BreakerKind::Transport,
            },
            _ => BreakerState::Closed,
        }
    }

    fn record(&self, ep: &EndpointId, obs: conway_core::routing::Observation) {
        self.observations.lock().unwrap().push((ep.clone(), obs));
        *self.counts.lock().unwrap().entry(ep.clone()).or_insert(0) += 1;
    }
}

#[tokio::test]
async fn breaker_edge_triggers_exactly_one_backend_degraded() {
    let a = Arc::new(RecordingBackend::new(
        "a",
        caps(ToolCallSupport::Streaming { validated: true }, 100_000),
        100,
        vec![
            Turn::Fail(BackendError::Transport { detail: "1".into() }),
            Turn::Fail(BackendError::Transport { detail: "2".into() }),
        ],
    ));
    let health = Arc::new(TrippingHealth::default());
    let bus = EventBus::new(1024);
    let engine = AttemptEngine::new(
        backends_map(vec![("a", a as Arc<dyn Backend>)]),
        health as Arc<dyn HealthRegistry>,
        bus.clone(),
    );
    let mut sub = bus.subscribe();
    let segments = vec![a_segment()];
    let tools: Vec<ToolSpec> = vec![];

    // First call: Closed -> Open (after record). Second call: already Open
    // going in, so `state()` before == Open, no edge, no second event.
    for _ in 0..2 {
        let routes = vec![route("a", "m1", primary("planner"))];
        let req = base_request(routes, &segments, &tools, 100, 4_096);
        let _ = engine.execute(req).await;
    }

    let events = drain(&mut sub);
    let degraded_count = events
        .iter()
        .filter(|e| matches!(e, Event::BackendDegraded { .. }))
        .count();
    assert_eq!(
        degraded_count, 1,
        "exactly one BackendDegraded on the Closed->Open edge"
    );
}

// ---------------------------------------------------------------------
// T-1 headroom gate
// ---------------------------------------------------------------------

#[tokio::test]
async fn t1_headroom_gate_rejects_when_all_candidates_too_small() {
    let a = Arc::new(RecordingBackend::new(
        "a",
        caps(ToolCallSupport::Streaming { validated: true }, 32_768),
        30_000,
        vec![],
    ));
    let b = Arc::new(RecordingBackend::new(
        "b",
        caps(ToolCallSupport::Streaming { validated: true }, 32_000),
        30_000,
        vec![],
    ));
    let fx = fixture(backends_map(vec![
        ("a", a.clone() as Arc<dyn Backend>),
        ("b", b.clone() as Arc<dyn Backend>),
    ]));
    let segments = vec![a_segment()];
    let tools: Vec<ToolSpec> = vec![];
    let routes = vec![
        route("a", "m1", primary("planner")),
        route("b", "m1", fallback(1)),
    ];
    let req = base_request(routes, &segments, &tools, 30_000, 4_000);

    let err = fx
        .engine
        .execute(req)
        .await
        .expect_err("both candidates too small");
    match err {
        RuntimeError::Routing(RoutingError::ContextTooLarge {
            est_tokens,
            headroom_tokens,
            required_tokens,
            max_context_tokens,
            shortfall_tokens,
            model,
            ..
        }) => {
            assert_eq!(est_tokens, 30_000);
            assert_eq!(headroom_tokens, 4_000);
            assert_eq!(required_tokens, 34_000);
            assert_eq!(max_context_tokens, 32_768);
            assert_eq!(shortfall_tokens, 1_232);
            assert_eq!(model, model_ref("a", "m1"));
        }
        other => panic!("expected ContextTooLarge, got {other:?}"),
    }
    assert_eq!(a.stream_count(), 0, "no backend call on T-1 rejection");
    assert_eq!(b.stream_count(), 0, "no backend call on T-1 rejection");
    assert!(
        fx.health.observations().is_empty(),
        "a too-large prompt must never feed a health Observation (T-2)"
    );
}

/// The largest-window-among-refusals rule (acceptance criterion 7, board
/// item) picked by VALUE, not by chain position:
/// the SECOND route (`b`, `fallback(1)`) has the larger window here, so a
/// bug that reported the FIRST refusal instead of the LARGEST one would
/// still pass a fixture where position 0 happens to have the larger window
/// (as `t1_headroom_gate_rejects_when_all_candidates_too_small` above does)
/// -- this fixture is shaped so only the correct rule passes.
#[tokio::test]
async fn t1_all_refused_reports_the_largest_window_even_when_it_is_not_first() {
    let a = Arc::new(RecordingBackend::new(
        "a",
        caps(ToolCallSupport::Streaming { validated: true }, 20_000),
        30_000,
        vec![],
    ));
    let b = Arc::new(RecordingBackend::new(
        "b",
        caps(ToolCallSupport::Streaming { validated: true }, 32_768),
        30_000,
        vec![],
    ));
    let fx = fixture(backends_map(vec![
        ("a", a.clone() as Arc<dyn Backend>),
        ("b", b.clone() as Arc<dyn Backend>),
    ]));
    let segments = vec![a_segment()];
    let tools: Vec<ToolSpec> = vec![];
    let routes = vec![
        route("a", "m1", primary("planner")),
        route("b", "m1", fallback(1)),
    ];
    let req = base_request(routes, &segments, &tools, 30_000, 4_000);

    let err = fx
        .engine
        .execute(req)
        .await
        .expect_err("both candidates too small");
    match err {
        RuntimeError::Routing(RoutingError::ContextTooLarge {
            max_context_tokens,
            model,
            ..
        }) => {
            assert_eq!(
                max_context_tokens, 32_768,
                "must report b's larger window, not a's smaller one"
            );
            assert_eq!(model, model_ref("b", "m1"));
        }
        other => panic!("expected ContextTooLarge, got {other:?}"),
    }
    assert!(fx.health.observations().is_empty());
}

#[tokio::test]
async fn t1_boundary_inclusive_and_exclusive() {
    // Exact fit: admissible.
    let a = Arc::new(RecordingBackend::new(
        "a",
        caps(ToolCallSupport::Streaming { validated: true }, 32_768),
        28_768,
        vec![Turn::Respond(text_response("ok"))],
    ));
    let fx = fixture(backends_map(vec![("a", a as Arc<dyn Backend>)]));
    let segments = vec![a_segment()];
    let tools: Vec<ToolSpec> = vec![];
    let routes = vec![route("a", "m1", primary("planner"))];
    let req = base_request(routes, &segments, &tools, 28_768, 4_000);
    fx.engine
        .execute(req)
        .await
        .expect("exact fit must be admissible");

    // One token over: rejected.
    let a2 = Arc::new(RecordingBackend::new(
        "a",
        caps(ToolCallSupport::Streaming { validated: true }, 32_768),
        28_769,
        vec![],
    ));
    let fx2 = fixture(backends_map(vec![("a", a2 as Arc<dyn Backend>)]));
    let routes = vec![route("a", "m1", primary("planner"))];
    let req = base_request(routes, &segments, &tools, 28_769, 4_000);
    let err = fx2
        .engine
        .execute(req)
        .await
        .expect_err("one over must be rejected");
    assert!(matches!(
        err,
        RuntimeError::Routing(RoutingError::ContextTooLarge { .. })
    ));
}

#[tokio::test]
async fn t1_mixed_candidates_skips_small_attempts_large() {
    let small = Arc::new(RecordingBackend::new(
        "small",
        caps(ToolCallSupport::Streaming { validated: true }, 32_768),
        30_000,
        vec![],
    ));
    let large = Arc::new(RecordingBackend::new(
        "large",
        caps(ToolCallSupport::Streaming { validated: true }, 200_000),
        30_000,
        vec![Turn::Respond(text_response("ok"))],
    ));
    let fx = fixture(backends_map(vec![
        ("small", small.clone() as Arc<dyn Backend>),
        ("large", large.clone() as Arc<dyn Backend>),
    ]));
    let mut sub = fx.bus.subscribe();
    let segments = vec![a_segment()];
    let tools: Vec<ToolSpec> = vec![];
    let routes = vec![
        route("small", "m1", primary("planner")),
        route("large", "m1", fallback(1)),
    ];
    let req = base_request(routes, &segments, &tools, 30_000, 4_000);

    let outcome = fx
        .engine
        .execute(req)
        .await
        .expect("large candidate must be attempted");
    assert_eq!(outcome.route.backend, BackendId::new("large"));
    assert_eq!(
        small.stream_count(),
        0,
        "small candidate must never be called"
    );
    assert_eq!(large.stream_count(), 1);

    assert_eq!(outcome.skipped.len(), 1);
    let (skipped_model, reason) = &outcome.skipped[0];
    assert_eq!(*skipped_model, model_ref("small", "m1"));
    match reason {
        RoutingReason::CapabilitySkip { missing, .. } => {
            // The `Backend::admit` refusal's own `Display`
            // (previously a hardcoded
            // "min_context" placeholder from the pre-flight partition this
            // item retired).
            assert_eq!(missing.len(), 1);
            assert!(
                missing[0].starts_with("context too large:"),
                "got: {missing:?}"
            );
            for needle in ["30000", "4000", "34000", "32768", "1232"] {
                assert!(
                    missing[0].contains(needle),
                    "missing {needle} in {missing:?}"
                );
            }
        }
        other => panic!("expected CapabilitySkip, got {other:?}"),
    }

    // ModelDecision is emitted only for the attempted (large) candidate.
    let events = drain(&mut sub);
    let decisions: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::ModelDecision { chosen, .. } => Some(chosen.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(decisions, vec![model_ref("large", "m1")]);

    // No health record for the skipped candidate.
    assert_eq!(fx.health.observations().len(), 1);
    assert_eq!(fx.health.observations()[0].0, EndpointId::new("large"));
}

#[tokio::test]
async fn t1_error_display_names_all_five_values() {
    let a = Arc::new(RecordingBackend::new(
        "a",
        caps(ToolCallSupport::Streaming { validated: true }, 32_768),
        30_000,
        vec![],
    ));
    let fx = fixture(backends_map(vec![("a", a as Arc<dyn Backend>)]));
    let segments = vec![a_segment()];
    let tools: Vec<ToolSpec> = vec![];
    let routes = vec![route("a", "m1", primary("planner"))];
    let req = base_request(routes, &segments, &tools, 30_000, 4_000);

    let err = fx.engine.execute(req).await.expect_err("must be rejected");
    let rendered = err.to_string();
    for needle in ["30000", "4000", "34000", "32768", "1232"] {
        assert!(
            rendered.contains(needle),
            "missing {needle} in {rendered:?}"
        );
    }
}

// ---------------------------------------------------------------------
// max_tokens default / override
// ---------------------------------------------------------------------

#[tokio::test]
async fn max_tokens_defaults_to_headroom_and_override_passes_through_unclamped() {
    // No override: max_tokens == headroom.
    let a = Arc::new(RecordingBackend::new(
        "a",
        caps(ToolCallSupport::Streaming { validated: true }, 100_000),
        100,
        vec![Turn::Respond(text_response("ok"))],
    ));
    let fx = fixture(backends_map(vec![("a", a.clone() as Arc<dyn Backend>)]));
    let segments = vec![a_segment()];
    let tools: Vec<ToolSpec> = vec![];
    let routes = vec![route("a", "m1", primary("planner"))];
    let req = base_request(routes, &segments, &tools, 100, 4_096);
    fx.engine.execute(req).await.expect("should succeed");
    assert_eq!(a.requests()[0].params.max_tokens, Some(4_096));

    // Override: passed through even though it exceeds headroom.
    let a2 = Arc::new(RecordingBackend::new(
        "a",
        caps(ToolCallSupport::Streaming { validated: true }, 100_000),
        100,
        vec![Turn::Respond(text_response("ok"))],
    ));
    let fx2 = fixture(backends_map(vec![("a", a2.clone() as Arc<dyn Backend>)]));
    let mut req2 = base_request(
        vec![route("a", "m1", primary("planner"))],
        &segments,
        &tools,
        100,
        4_096,
    );
    req2.max_tokens_override = Some(50_000);
    fx2.engine.execute(req2).await.expect("should succeed");
    assert_eq!(a2.requests()[0].params.max_tokens, Some(50_000));
}

// ---------------------------------------------------------------------
// Exhausting all routes -> NoCandidate
// ---------------------------------------------------------------------

#[tokio::test]
async fn exhausting_all_routes_returns_no_candidate_enumerating_failures() {
    let a = Arc::new(RecordingBackend::new(
        "a",
        caps(ToolCallSupport::Streaming { validated: true }, 100_000),
        100,
        vec![Turn::Fail(BackendError::Transport {
            detail: "a down".into(),
        })],
    ));
    let b = Arc::new(RecordingBackend::new(
        "b",
        caps(ToolCallSupport::Streaming { validated: true }, 100_000),
        100,
        vec![Turn::Fail(BackendError::ServerError {
            status: 503,
            detail: "b down".into(),
        })],
    ));
    let fx = fixture(backends_map(vec![
        ("a", a as Arc<dyn Backend>),
        ("b", b as Arc<dyn Backend>),
    ]));
    let segments = vec![a_segment()];
    let tools: Vec<ToolSpec> = vec![];
    let routes = vec![
        route("a", "m1", primary("planner")),
        route("b", "m1", fallback(1)),
    ];
    let req = base_request(routes, &segments, &tools, 100, 4_096);

    let err = fx.engine.execute(req).await.expect_err("both routes fail");
    match err {
        RuntimeError::Routing(RoutingError::NoCandidate { considered, .. }) => {
            assert_eq!(considered.len(), 2);
            assert_eq!(considered[0].0, model_ref("a", "m1"));
            assert!(considered[0].1.contains("a down"));
            assert_eq!(considered[1].0, model_ref("b", "m1"));
            assert!(considered[1].1.contains("b down"));
        }
        other => panic!("expected NoCandidate, got {other:?}"),
    }
}
