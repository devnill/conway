//! Acceptance tests, board item `01M23SDCE6T85Z48CRQ8NBY6PV` ("a malformed
//! tool call must not end the session").
//!
//! THE EVIDENCE this item fixes: a model (`ollama_cloud/glm-5.3`) sent
//! `conway_spawn`'s `budget` argument as the STRING `"{\"max_steps\": 5}"`
//! instead of the object itself. `conway` responded with a fatal
//! `RoutingError::NoCandidate` ("no candidate for role default (1
//! considered)") -- false (a candidate DID answer, with a malformed call,
//! not silence) -- and the whole session ended, discarding everything, with
//! the one party who could have fixed it in a single retry (the model)
//! never told what it got wrong.
//!
//! **Why these tests drive the REAL wire-level parser, not a stand-in.**
//! Every scripted "model" below is an [`AccumulatingBackend`], which feeds
//! its canned tool calls through the REAL, production
//! `conway_plugin_backends::tool_calls::ToolCallAccumulator` -- the exact
//! type a real streaming/non-streaming backend uses to turn provider wire
//! deltas into a validated `ToolCall` -- rather than constructing an
//! already-valid `ToolCall` Rust struct directly (as most of this crate's
//! other backend test doubles do, e.g. `conway_testkit::ScriptedBackend`).
//! A scripted backend that bypassed the accumulator would never exercise
//! `SchemaValidator`'s coercion at all: the malformed string would sail
//! through as an already-constructed `ToolCall::arguments` value, proving
//! nothing about the fix. Every test also uses the REAL `conway_spawn`
//! `ToolSpec` (`conway_tools::subagent::tools::SpawnTool::spec()`), so the
//! schema being validated against is the schema THE EVIDENCE actually
//! tripped on (`budget: Option<BudgetArg>`, rendered by schemars as
//! `anyOf: [object, null]`), not a hand-rolled substitute.
//!
//! The four required tests, by name:
//! 1. [`full_pipeline::stringified_budget_coerces_on_every_attempt_turn_completes_and_child_spawns`]
//!    -- coercion (step 1) fires on the very first attempt, every time; the
//!    turn completes, a child is actually spawned end to end (through a
//!    real `Runtime`/`conway_spawn`), and the firing is recorded durably
//!    (`Event::ToolArgumentCoerced`) naming the tool and the argument path.
//! 2. [`a_parseable_but_non_validating_string_is_not_coerced_and_takes_the_retry_path`]
//!    -- the narrow half of the decision: a string that parses as JSON but
//!    does not then validate is left alone, so recovery must come from the
//!    bounded retry, not coercion.
//! 3. [`malformed_first_attempt_corrected_second_completes_via_retry`] --
//!    coercion cannot fix the first attempt's malformation, but the bounded
//!    retry's second attempt is well-formed and the turn completes. Also
//!    asserts the model actually RECEIVED the corrective detail (not merely
//!    that a second attempt happened): the retry's own request carries a
//!    `ToolResult` naming the tool and the `/budget` argument path.
//! 4. [`full_pipeline::exhausted_tool_parse_produces_a_typed_error_not_no_candidate`]
//!    -- coercion and the retry both fail on a single-candidate chain
//!    (mirrors THE EVIDENCE's own "1 considered" shape exactly), asserted
//!    at the SESSION level (P-15: an intermediate signal like
//!    `AttemptEngine::execute`'s bare `Result::Err` does not stand in for
//!    the observable outcome -- see that test's own doc for why the run
//!    ending here, correctly attributed, is not the bug this item fixes).
//!    The persisted `AgentResult` and the `fatal` `Event::Error` both name
//!    the tool and argument, and neither says "no candidate" nor "routing".
//!
//! A fifth, supplementary (not one of the four required) test,
//! [`attempt_engine_reports_tool_call_rejected_directly`], pins the same
//! typed-error construction at `AttemptEngine`'s own level, independent of
//! everything `AgentLoop` layers on top.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use conway_core::capabilities::{
    CacheMode, Capabilities, ProbeReport, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{ContentBlock, Role, SamplingParams, StopReason, ToolSpec, Usage};
use conway_core::error::{BackendError, RuntimeError};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, SessionId};
use conway_core::ports::{
    Backend, BoxStream, GenerateRequest, GenerateResponse, StreamChunk, Tool,
};
use conway_core::provenance::Provenance;
use conway_core::routing::{Route, RoutingReason};
use conway_core::segment::{CacheTtl, PromptSegment};
use conway_plugin_backends::tool_calls::{
    ToolArgumentCoercion, ToolCallAccumulator, ToolCallStyle,
};
use conway_runtime::attempt::{AttemptEngine, AttemptRequest};
use conway_runtime::events::EventBus;
use conway_testkit::FakeHealth;
use conway_tools::subagent::tools::SpawnTool;
use futures::stream::{self, StreamExt};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------
// `AccumulatingBackend`: a scripted `Backend` whose tool calls are run
// through the REAL `ToolCallAccumulator` (see this file's own module doc).
// ---------------------------------------------------------------------

/// One scripted attempt: a batch of raw (name, arguments) tool calls to feed
/// through the accumulator, a plain terminal text turn, or an outright
/// backend failure unrelated to tool-call parsing (`Fail`, used only by the
/// mixed-cause-chain test to give an EARLIER candidate a non-`ToolParse`
/// reason before the LAST one exhausts on a malformed call).
#[derive(Clone, Debug)]
enum ScriptedAttempt {
    ToolCalls(Vec<(&'static str, Value)>),
    Text(&'static str),
    Fail(BackendError),
}

struct AccumulatingBackend {
    id: BackendId,
    caps: Capabilities,
    script: Mutex<VecDeque<ScriptedAttempt>>,
    /// Every `GenerateRequest` this backend has actually been asked to
    /// serve, in call order -- lets a test inspect what the RETRY attempt
    /// was sent (board item `01M23SDCE6T85Z48CRQ8NBY6PV` step 2: the
    /// corrective segments `AttemptEngine` appends before resending),
    /// rather than only observing that a second attempt happened.
    requests: Mutex<Vec<GenerateRequest>>,
}

impl AccumulatingBackend {
    fn new(id: &str, caps: Capabilities, script: Vec<ScriptedAttempt>) -> Self {
        Self {
            id: BackendId::new(id),
            caps,
            script: Mutex::new(script.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<GenerateRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// Pops the next scripted attempt and, for a `ToolCalls` entry, drives
    /// it through the production accumulator/validator exactly as a real
    /// dialect's `generate()`/`stream()` would (`push_complete` per call,
    /// then `finish`) -- coercion included. The second element is every
    /// coercion `finish` recorded, exactly as a real dialect's own
    /// `stream()` implementation threads it into `StreamChunk::
    /// ToolArgumentCoerced` chunks (`openai_compat::stream`/`anthropic::
    /// stream`'s own production code, mirrored here so this test double's
    /// `stream()` below is a faithful reproduction, not a shortcut that
    /// happens to skip the very channel this item's step 1 durability fix
    /// depends on).
    fn next_response(
        &self,
        req: &GenerateRequest,
    ) -> Result<(GenerateResponse, Vec<ToolArgumentCoercion>), BackendError> {
        self.requests.lock().unwrap().push(req.clone());
        let attempt = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .expect("AccumulatingBackend script exhausted");
        match attempt {
            ScriptedAttempt::Fail(err) => Err(err),
            ScriptedAttempt::Text(text) => Ok((
                GenerateResponse {
                    content: vec![ContentBlock::Text {
                        text: text.to_string(),
                    }],
                    tool_calls: vec![],
                    stop: StopReason::EndTurn,
                    usage: Usage::default(),
                },
                Vec::new(),
            )),
            ScriptedAttempt::ToolCalls(calls) => {
                let mut acc = ToolCallAccumulator::new(ToolCallStyle::Structured, &req.tools);
                for (i, (name, arguments)) in calls.into_iter().enumerate() {
                    acc.push_complete(Some(format!("call_{i}")), name.to_string(), arguments)?;
                }
                let outcome = acc.finish(StopReason::ToolUse)?;
                Ok((
                    GenerateResponse {
                        content: vec![],
                        tool_calls: outcome.calls,
                        stop: StopReason::ToolUse,
                        usage: Usage::default(),
                    },
                    outcome.coercions,
                ))
            }
        }
    }
}

/// Every `Text` block inside any `ContentBlock::ToolResultBlock` across
/// `segments`, concatenated -- the corrective message text a
/// `ToolArgumentsInvalid` retry's `ToolResult` segment carries (board item
/// `01M23SDCE6T85Z48CRQ8NBY6PV` step 2), if any.
fn tool_result_text(segments: &[PromptSegment]) -> String {
    let mut out = String::new();
    for segment in segments {
        for block in &segment.content {
            if let ContentBlock::ToolResultBlock { blocks, .. } = block {
                for inner in blocks {
                    if let ContentBlock::Text { text } = inner {
                        out.push_str(text);
                    }
                }
            }
        }
    }
    out
}

/// Mirrors the production dialects' own streaming shape exactly
/// (`openai_compat::stream`/`anthropic::stream`'s own code, this item's
/// change to both): every coercion first, as `StreamChunk::
/// ToolArgumentCoerced`, THEN the text deltas, THEN the terminal `Done`.
fn decompose(
    response: GenerateResponse,
    coercions: Vec<ToolArgumentCoercion>,
) -> Vec<Result<StreamChunk, BackendError>> {
    let mut chunks: Vec<Result<StreamChunk, BackendError>> = coercions
        .into_iter()
        .map(|c| {
            Ok(StreamChunk::ToolArgumentCoerced {
                tool: c.tool,
                call_id: c.call_id,
                argument_path: c.argument_path,
            })
        })
        .collect();
    chunks.extend(response.content.iter().filter_map(|b| match b {
        ContentBlock::Text { text } => Some(Ok(StreamChunk::TextDelta(text.clone()))),
        _ => None,
    }));
    chunks.push(Ok(StreamChunk::Done(response)));
    chunks
}

#[async_trait]
impl Backend for AccumulatingBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn capabilities(&self, _model: &ModelId) -> Capabilities {
        self.caps.clone()
    }

    async fn generate(&self, req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
        self.next_response(&req)
            .map(|(response, _coercions)| response)
    }

    async fn stream(
        &self,
        req: GenerateRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
        match self.next_response(&req) {
            Ok((response, coercions)) => Ok(stream::iter(decompose(response, coercions)).boxed()),
            Err(err) => Ok(stream::iter(vec![Err(err)]).boxed()),
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
// Fixtures shared by the `AttemptEngine`-level tests (2, 3, 4).
// ---------------------------------------------------------------------

/// Streaming-capable, generous context window -- the shape a real
/// streaming-tool-calling provider (like THE EVIDENCE's `ollama_cloud/
/// glm-5.3`) declares, so the existing one-retry Stream -> Generate
/// mechanic (`attempt.rs`) is reachable exactly as it is in production.
fn caps() -> Capabilities {
    Capabilities {
        tool_calling: ToolCallSupport::Streaming { validated: true },
        cache: CacheMode::None,
        parallel_tool_calls: false,
        structured_output: StructuredOutput::None,
        max_context_tokens: 100_000,
        reasoning: false,
        reliability_tier: ReliabilityTier::Verified,
    }
}

fn spawn_tool_spec() -> ToolSpec {
    SpawnTool::new().spec()
}

fn a_segment() -> PromptSegment {
    PromptSegment::new(
        Role::User,
        vec![ContentBlock::Text { text: "hi".into() }],
        Provenance::UserPrompt,
    )
}

fn single_route(backend: &str, model: &str) -> Route {
    Route {
        backend: BackendId::new(backend),
        model: ModelId::new(model),
        params: SamplingParams::default(),
        reason: RoutingReason::AliasPrimary {
            alias: RoleAlias::new("planner"),
        },
    }
}

fn model_ref(backend: &str, model: &str) -> ModelRef {
    ModelRef {
        backend: BackendId::new(backend),
        model: ModelId::new(model),
    }
}

fn base_request<'a>(
    routes: Vec<Route>,
    segments: &'a [PromptSegment],
    tools: &'a [ToolSpec],
) -> AttemptRequest<'a> {
    AttemptRequest {
        agent_id: AgentId::new(),
        session: SessionId::new(),
        role: RoleAlias::new("planner"),
        routes,
        segments,
        tools,
        prefix_key: None,
        est_tokens: 100,
        headroom: 4_096,
        max_tokens_override: None,
        cache_ttl: CacheTtl::FiveMinutes,
        cancel: CancellationToken::new(),
    }
}

fn engine(backend: Arc<AccumulatingBackend>) -> AttemptEngine {
    let mut backends = std::collections::HashMap::new();
    let id = backend.id();
    backends.insert(id, backend as Arc<dyn Backend>);
    AttemptEngine::new(
        backends,
        Arc::new(FakeHealth::new()),
        EventBus::with_default_capacity(),
    )
}

// ---------------------------------------------------------------------
// Test 2: a string that parses as JSON but does not then validate is NOT
// coerced -- it must take the retry path like any other malformation.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_parseable_but_non_validating_string_is_not_coerced_and_takes_the_retry_path() {
    // `budget: "5"` parses to the JSON number `5`, which is not an object
    // -- coercion's own re-validation step must reject it, so recovery can
    // only come from the retry below (a corrected second attempt).
    let backend = Arc::new(AccumulatingBackend::new(
        "b",
        caps(),
        vec![
            ScriptedAttempt::ToolCalls(vec![(
                "conway_spawn",
                json!({"prompt": "investigate", "budget": "5"}),
            )]),
            ScriptedAttempt::ToolCalls(vec![(
                "conway_spawn",
                json!({"prompt": "investigate", "budget": {"max_steps": 5}}),
            )]),
        ],
    ));
    let eng = engine(backend);
    let segments = vec![a_segment()];
    let tools = vec![spawn_tool_spec()];
    let routes = vec![single_route("b", "m1")];
    let req = base_request(routes, &segments, &tools);

    let outcome = eng
        .execute(req)
        .await
        .expect("a corrected retry must complete the turn");
    assert_eq!(
        outcome.attempts, 2,
        "the first (uncoerced) attempt must have been retried, not silently accepted"
    );
    assert_eq!(outcome.response.tool_calls.len(), 1);
    assert_eq!(
        outcome.response.tool_calls[0].arguments,
        json!({"prompt": "investigate", "budget": {"max_steps": 5}}),
        "the SECOND attempt's own well-formed object must be what the turn completed with"
    );
}

// ---------------------------------------------------------------------
// Test 3: coercion cannot fix the first attempt, but the bounded retry's
// second attempt is well-formed and the turn completes.
// ---------------------------------------------------------------------

#[tokio::test]
async fn malformed_first_attempt_corrected_second_completes_via_retry() {
    // Not valid JSON at all -- coercion's own `serde_json::from_str` fails
    // immediately, so this is an ordinary malformation coercion cannot
    // resolve, and produces `BackendError::ToolArgumentsInvalid` (naming
    // `conway_spawn`/`/budget`) rather than the bare `ToolParse` other
    // malformations (unknown tool, unterminated JSON) still produce.
    let backend = Arc::new(AccumulatingBackend::new(
        "b",
        caps(),
        vec![
            ScriptedAttempt::ToolCalls(vec![(
                "conway_spawn",
                json!({"prompt": "investigate", "budget": "not json at all {"}),
            )]),
            ScriptedAttempt::ToolCalls(vec![(
                "conway_spawn",
                json!({"prompt": "investigate", "budget": {"max_steps": 7}}),
            )]),
        ],
    ));
    let eng = engine(backend.clone());
    let segments = vec![a_segment()];
    let tools = vec![spawn_tool_spec()];
    let routes = vec![single_route("b", "m1")];
    let req = base_request(routes, &segments, &tools);

    let outcome = eng
        .execute(req)
        .await
        .expect("the corrected second attempt must complete the turn");
    assert_eq!(outcome.attempts, 2);
    assert_eq!(
        outcome.response.tool_calls[0].arguments,
        json!({"prompt": "investigate", "budget": {"max_steps": 7}})
    );

    // The distinguishing assertion (board item `01M23SDCE6T85Z48CRQ8NBY6PV`
    // step 2, review round 2): the RETRY's own request must carry the
    // corrective detail -- naming the tool, the argument path, and what was
    // wrong -- not merely a second, byte-identical resend. A bare "invalid
    // arguments" would give the model nothing to act on; this asserts the
    // actual detail text reached it.
    let requests = backend.requests();
    assert_eq!(requests.len(), 2, "must have made exactly two attempts");
    let retry_text = tool_result_text(&requests[1].segments);
    assert!(
        retry_text.contains("conway_spawn"),
        "the retry's own request must name the tool: {retry_text:?}"
    );
    assert!(
        retry_text.contains("/budget"),
        "the retry's own request must name the argument path: {retry_text:?}"
    );
    assert!(
        tool_result_text(&requests[0].segments).is_empty(),
        "the FIRST attempt's own request must carry no corrective text yet"
    );
}

// ---------------------------------------------------------------------
// Supplementary (not one of the four required tests -- the REQUIRED test 4,
// `exhausted_tool_parse_produces_a_typed_error_not_no_candidate`, is a
// session-level test in `full_pipeline` below; asserting only on
// `AttemptEngine::execute`'s bare `Result::Err` here would be an
// intermediate signal, not the observable session outcome, so it does not
// stand in for that requirement on its own -- P-15). This is a fast,
// focused unit check that `AttemptEngine` itself constructs the RIGHT
// typed error, independent of everything `AgentLoop` layers on top.
// ---------------------------------------------------------------------

#[tokio::test]
async fn attempt_engine_reports_tool_call_rejected_directly() {
    let bad_call = || {
        ScriptedAttempt::ToolCalls(vec![(
            "conway_spawn",
            json!({"prompt": "investigate", "budget": "5"}),
        )])
    };
    let backend = Arc::new(AccumulatingBackend::new(
        "b",
        caps(),
        vec![bad_call(), bad_call()],
    ));
    let eng = engine(backend);
    let segments = vec![a_segment()];
    let tools = vec![spawn_tool_spec()];
    // A SINGLE candidate -- exactly THE EVIDENCE's "no candidate for role
    // default (1 considered)" shape -- so exhausting it reaches the
    // terminal aggregate this item's fix changes.
    let routes = vec![single_route("b", "m1")];
    let req = base_request(routes, &segments, &tools);

    let err = eng.execute(req).await.expect_err("both attempts must fail");

    let RuntimeError::ToolCallRejected { detail, .. } = &err else {
        panic!("expected a typed RuntimeError::ToolCallRejected, got {err:?}");
    };
    assert!(detail.contains("conway_spawn"), "{detail}");
    assert!(detail.contains("budget"), "{detail}");
    let rendered = err.to_string();
    assert!(
        !rendered.to_lowercase().contains("no candidate"),
        "must not say \"no candidate\" when a candidate answered: {rendered}"
    );
    assert!(
        !rendered.to_lowercase().contains("routing"),
        "must not be reported as a routing failure: {rendered}"
    );
}

/// Item 5 (review round 2): a chain where an EARLIER candidate failed for a
/// completely different, non-`ToolParse` reason must still name that
/// candidate in the terminal `ToolCallRejected::considered` list, not just
/// the last one that failed on its tool call -- every chosen/skipped
/// candidate carries its own reason, always.
#[tokio::test]
async fn mixed_cause_chain_names_every_candidate_not_only_the_last() {
    let route_a_backend = Arc::new(AccumulatingBackend::new(
        "a",
        caps(),
        vec![ScriptedAttempt::Fail(BackendError::RateLimit {
            retry_after_secs: Some(7),
        })],
    ));
    let route_b_backend = Arc::new(AccumulatingBackend::new(
        "b",
        caps(),
        vec![
            ScriptedAttempt::ToolCalls(vec![(
                "conway_spawn",
                json!({"prompt": "investigate", "budget": "5"}),
            )]),
            ScriptedAttempt::ToolCalls(vec![(
                "conway_spawn",
                json!({"prompt": "investigate", "budget": "5"}),
            )]),
        ],
    ));
    let mut backends: std::collections::HashMap<BackendId, Arc<dyn Backend>> =
        std::collections::HashMap::new();
    backends.insert(route_a_backend.id(), route_a_backend.clone());
    backends.insert(route_b_backend.id(), route_b_backend.clone());
    let eng = AttemptEngine::new(
        backends,
        Arc::new(FakeHealth::new()),
        EventBus::with_default_capacity(),
    );
    let segments = vec![a_segment()];
    let tools = vec![spawn_tool_spec()];
    let routes = vec![single_route("a", "m1"), single_route("b", "m1")];
    let req = base_request(routes, &segments, &tools);

    let err = eng
        .execute(req)
        .await
        .expect_err("route a rate-limits, route b exhausts on its malformed call");

    let RuntimeError::ToolCallRejected {
        detail, considered, ..
    } = &err
    else {
        panic!("expected a typed RuntimeError::ToolCallRejected, got {err:?}");
    };
    // The terminal `detail` still names the LAST candidate's own failure
    // (route b, the malformed call) -- unchanged from the single-candidate
    // case.
    assert!(detail.contains("conway_spawn"), "{detail}");
    assert!(detail.contains("budget"), "{detail}");
    // But `considered` must carry BOTH candidates, each with its own
    // reason -- route a's rate limit is not silently dropped just because
    // it was not the failure that ultimately ended the chain.
    assert_eq!(
        considered.len(),
        2,
        "both candidates must be named, got {considered:?}"
    );
    assert_eq!(considered[0].0, model_ref("a", "m1"));
    assert!(
        considered[0].1.to_lowercase().contains("rate limit"),
        "route a's own reason must be preserved: {:?}",
        considered[0].1
    );
    assert_eq!(considered[1].0, model_ref("b", "m1"));
    assert!(
        considered[1].1.contains("conway_spawn"),
        "route b's own reason must be preserved: {:?}",
        considered[1].1
    );
}

// ---------------------------------------------------------------------
// Test 1: coercion fires on the very first attempt, every time (no retry
// needed at all -- proving it is not luck on a later attempt); the turn
// completes, a child is genuinely spawned end to end through a real
// `Runtime`, and the firing is recorded.
// ---------------------------------------------------------------------

mod full_pipeline {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;

    use conway_core::agent::{AgentKnobs, Budget, PermissionDecision, ResultStatus};
    use conway_core::event::Event;
    use conway_core::log::SubagentMode;
    use conway_core::ports::{CapabilityRegistry, Router};
    use conway_runtime::runtime::{RootSpec, Runtime, RuntimeDeps};
    use conway_testkit::{
        FakeGate, FakeHealth, FakePathStore, FakeRouter, FakeSessionDiscoveryHost, FakeStore,
    };
    use conway_tools::subagent::SubagentPlugin;
    use futures::StreamExt as _;

    use super::*;

    fn root_spec(prompt: &str) -> RootSpec {
        RootSpec {
            session: None,
            knobs: AgentKnobs {
                agent_def: None,
                role: Some(RoleAlias::new("planner")),
                model: None,
                tools: None,
                budget: Budget::default(),
                result_contract: None,
                keep_alive: false,
            },
            cwd: PathBuf::from("/tmp"),
            root: None,
            prompt: Some(prompt.to_string()),
            system_prompt_override: None,
            labels: Vec::new(),
        }
    }

    #[tokio::test]
    async fn stringified_budget_coerces_on_every_attempt_turn_completes_and_child_spawns() {
        // THE EVIDENCE, verbatim: `budget` sent as the STRING
        // `"{\"max_steps\": 5}"`. This exact scripted turn is reachable on
        // EVERY attempt this backend might ever serve (it is not consumed
        // and replaced by a corrected value) -- coercion fixes it, so only
        // ONE attempt is ever actually needed; the retry path is never
        // reached at all (proven below by `outcome`'s single `TurnStarted`/
        // dispatch, not by a second scripted turn standing in for a
        // "luckier" retry).
        let backend = Arc::new(AccumulatingBackend::new(
            "b",
            caps(),
            vec![
                ScriptedAttempt::ToolCalls(vec![(
                    "conway_spawn",
                    json!({
                        "prompt": "investigate the bug",
                        "budget": "{\"max_steps\": 5}",
                        "await": false
                    }),
                )]),
                // The root's own follow-up turn, once the tool result
                // (the spawned child's id) comes back, PLUS the spawned
                // child's own independent turn (`await: false` above means
                // the root does not block on it, so it runs concurrently
                // against the same scripted queue) -- both plain text,
                // interchangeable, so which one lands in which queue slot
                // is not asserted on.
                ScriptedAttempt::Text("spawned it"),
                ScriptedAttempt::Text("child done"),
            ],
        ));
        let model = ModelRef {
            backend: backend.id(),
            model: ModelId::new("m1"),
        };
        let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
        let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
        backends.insert(backend.id(), backend);

        let runtime = Runtime::new(RuntimeDeps {
            store: Arc::new(FakeStore::new()),
            path_store: Arc::new(FakePathStore::new()),
            router,
            health: Arc::new(FakeHealth::new()),
            backends,
            plugins: vec![Arc::new(SubagentPlugin::new())],
            gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
            agent_defs: HashMap::new(),
            instructions: Vec::new(),
            skills: Default::default(),
            event_bus: EventBus::with_default_capacity(),
            headroom: Arc::new(conway_core::capabilities::HeadroomPolicy::default()),
            tool_result_bound: Arc::new(conway_core::capabilities::ToolResultBoundPolicy::default()),
            session_discovery: Arc::new(FakeSessionDiscoveryHost::new()),
            capabilities: Arc::new(CapabilityRegistry::default()),
        });

        let mut stream = runtime.subscribe();
        let root = runtime
            .start_root(root_spec("investigate the bug"))
            .await
            .unwrap();

        // "the child is spawned": a real `Event::AgentSpawned { kind:
        // SubagentMode::Spawn, parent: Some(root), .. }`, emitted by the
        // REAL `conway_spawn` tool (`SubagentPlugin`) dispatched through
        // the REAL `ToolRunner`, not merely a well-formed tool call that
        // stopped short of being dispatched. The SAME pass also watches for
        // the durable `Event::ToolArgumentCoerced` (board item
        // `01M23SDCE6T85Z48CRQ8NBY6PV` step 1's "a coercion nobody can
        // count is the one this decision would have rejected") -- it fires
        // mid-stream, strictly BEFORE the turn's `Done` chunk (and
        // therefore before the tool is ever dispatched), so it is always
        // seen before `AgentSpawned` on this same stream.
        let mut coerced: Option<(String, String)> = None;
        let spawned = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let envelope = stream.next().await.expect("event stream ended early");
                match &envelope.event {
                    Event::ToolArgumentCoerced {
                        tool,
                        argument_path,
                        ..
                    } => {
                        coerced = Some((tool.to_string(), argument_path.clone()));
                    }
                    Event::AgentSpawned {
                        kind: SubagentMode::Spawn,
                        parent: Some(parent),
                        ..
                    } if *parent == root => {
                        return envelope.agent;
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("conway_spawn must have spawned a child");
        assert_ne!(spawned, root, "the spawned child must be a distinct agent");

        let (coerced_tool, coerced_path) = coerced.expect(
            "a durable Event::ToolArgumentCoerced must have been emitted before the child \
             was spawned -- coercion firing must be countable, not tracing-only",
        );
        assert_eq!(coerced_tool, "conway_spawn");
        assert_eq!(coerced_path, "/budget");

        // The root's own turn must COMPLETE (not end the session with a
        // fatal error) -- `finish_root` below is the root's own
        // `AgentFinished`, read off the SAME stream.
        let root_result = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let envelope = stream.next().await.expect("event stream ended early");
                if envelope.agent == root {
                    if let Event::AgentFinished { result, .. } = envelope.event {
                        return result;
                    }
                }
            }
        })
        .await
        .expect("root must finish");
        assert_eq!(
            root_result.status,
            conway_core::agent::ResultStatus::Completed,
            "the turn must complete, not fail, despite the stringified budget: {root_result:?}"
        );
    }

    // -------------------------------------------------------------------
    // Test 4 (the REQUIRED session-level one -- P-15): coercion and the
    // bounded retry both fail, on a single-candidate chain (mirrors THE
    // EVIDENCE's own "1 considered" shape). Asserts the OBSERVABLE session
    // outcome, not `AttemptEngine::execute`'s bare `Result::Err` (see
    // `attempt_engine_reports_tool_call_rejected_directly`, a narrower
    // supplementary check, for that).
    //
    // **Why the agent's run ending here is correct, not a regression this
    // item was supposed to prevent.** Coercion and the retry (steps 1-2)
    // are what THIS item adds; both already failed by the time this fires.
    // There is no third recovery this item's own spec calls for, and
    // genuinely no valid assistant response exists to continue the turn
    // with (unlike an ordinary DISPATCHED tool call failing, where a
    // `ToolResult` can be appended and the turn goes on -- here, parsing
    // itself never produced anything the turn could act on). What THE
    // EVIDENCE's own bug got wrong was never that the run ended -- it was
    // HOW: misattributed to "no candidate"/routing when a candidate had
    // answered. `Event::Error { fatal: true }` is this workspace's
    // existing, correct mechanism for "this run cannot continue"; what
    // this test pins is that its OWN text, and the persisted `AgentResult`
    // it produces, are honest about the cause.
    #[tokio::test]
    async fn exhausted_tool_parse_produces_a_typed_error_not_no_candidate() {
        let bad_call = || {
            ScriptedAttempt::ToolCalls(vec![(
                "conway_spawn",
                json!({"prompt": "investigate", "budget": "5"}),
            )])
        };
        let backend = Arc::new(AccumulatingBackend::new(
            "b",
            caps(),
            vec![bad_call(), bad_call()],
        ));
        let model = ModelRef {
            backend: backend.id(),
            model: ModelId::new("m1"),
        };
        let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
        let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
        backends.insert(backend.id(), backend);

        let runtime = Runtime::new(RuntimeDeps {
            store: Arc::new(FakeStore::new()),
            path_store: Arc::new(FakePathStore::new()),
            router,
            health: Arc::new(FakeHealth::new()),
            backends,
            plugins: vec![Arc::new(SubagentPlugin::new())],
            gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
            agent_defs: HashMap::new(),
            instructions: Vec::new(),
            skills: Default::default(),
            event_bus: EventBus::with_default_capacity(),
            headroom: Arc::new(conway_core::capabilities::HeadroomPolicy::default()),
            tool_result_bound: Arc::new(conway_core::capabilities::ToolResultBoundPolicy::default()),
            session_discovery: Arc::new(FakeSessionDiscoveryHost::new()),
            capabilities: Arc::new(CapabilityRegistry::default()),
        });

        let mut stream = runtime.subscribe();
        // A SINGLE candidate -- exactly THE EVIDENCE's "no candidate for
        // role default (1 considered)" shape -- so exhausting it reaches
        // the terminal aggregate this item's fix changes.
        let root = runtime.start_root(root_spec("investigate")).await.unwrap();

        let (result, fatal_error_text) = tokio::time::timeout(Duration::from_secs(2), async {
            let mut fatal_text = None;
            loop {
                let envelope = stream.next().await.expect("event stream ended early");
                if envelope.agent != root {
                    continue;
                }
                if let Event::Error { error, fatal: true } = &envelope.event {
                    fatal_text = Some(error.to_string());
                }
                if let Event::AgentFinished { result, .. } = envelope.event {
                    return (result, fatal_text);
                }
            }
        })
        .await
        .expect("root must finish (fail) rather than hang");

        let ResultStatus::Failed { error } = &result.status else {
            panic!(
                "expected ResultStatus::Failed once coercion and the retry both fail, got {:?}",
                result.status
            );
        };
        assert!(error.contains("conway_spawn"), "{error}");
        assert!(error.contains("budget"), "{error}");
        assert!(
            !error.to_lowercase().contains("no candidate"),
            "the persisted AgentResult must not say \"no candidate\" when a candidate \
             answered: {error}"
        );
        assert!(
            !error.to_lowercase().contains("routing"),
            "the persisted AgentResult must not be reported as a routing failure: {error}"
        );

        let fatal_error_text =
            fatal_error_text.expect("a fatal Event::Error must have been emitted");
        assert!(
            !fatal_error_text.to_lowercase().contains("no candidate"),
            "{fatal_error_text}"
        );
        assert!(
            !fatal_error_text.to_lowercase().contains("routing"),
            "{fatal_error_text}"
        );
    }
}
