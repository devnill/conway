//! `AttemptEngine`: turns an ordered candidate list plus assembled segments
//! into one `GenerateResponse` (WI-080).
//!
//! Responsibilities: choose streaming vs non-streaming per the declared
//! tool-calling capability, sequence the fallback chain, enforce the
//! headroom-aware T-1 context gate as a backstop covering the pin path, and
//! record health observations with the T-2 classification.
//!
//! T-1 error-shape reconciliation (decision 01KYXS3PTYVATWR58JR95AZJYN,
//! closing board item 01KYXNAHN64YMADZPQDQC0CPTJ): the router (conway-routing
//! `DeclarativeRouter`, WI-034) now also constructs
//! `RoutingError::ContextTooLarge` -- but only when every candidate it
//! rejected failed *solely* on headroom (see that crate's `router.rs` module
//! doc); a mixed rejection, or one this engine reaches at all, still surfaces
//! as `NoCandidate` from the router side. This engine's own T-1 check below
//! is a second, independent construction site for `ContextTooLarge` -- its
//! per-route backstop gate, which fires only when every route *this engine*
//! was handed fails the `est_tokens + headroom <= max_context_tokens` check,
//! independent of whatever filtering the caller already did upstream (this
//! matters for the pin path, which can bypass the router's chain filtering
//! entirely, and as a backstop against a router capability index that has
//! drifted from what the backend actually reports).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use conway_core::capabilities::{Capabilities, ToolCallSupport};
use conway_core::content::{ContentBlock, ToolSpec};
use conway_core::error::{BackendError, RoutingError, RuntimeError};
use conway_core::event::Event;
use conway_core::ids::{
    AgentId, BackendId, EndpointId, ModelId, ModelRef, PrefixKey, RoleAlias, SessionId,
};
use conway_core::ports::{Backend, GenerateRequest, GenerateResponse, HealthRegistry, StreamChunk};
use conway_core::routing::{BreakerState, Observation, Route, RoutingReason};
use conway_core::segment::{CacheTtl, PromptSegment};
use conway_routing::failure::{classify, observation_for, FailureClass};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::context::builder::{attach_cache_hints, breakpoint_indices};
use crate::context::prefix_key;
use crate::events::EventBus;

/// One call to [`AttemptEngine::execute`]: an ordered fallback chain plus
/// the assembled request the caller wants served.
pub struct AttemptRequest<'a> {
    pub agent_id: AgentId,
    pub session: SessionId,
    pub role: RoleAlias,
    pub routes: Vec<Route>,
    pub segments: &'a [PromptSegment],
    pub tools: &'a [ToolSpec],
    pub prefix_key: Option<PrefixKey>,
    pub est_tokens: u32,
    /// Reserved output/reasoning budget, resolved by the caller (WI-081).
    /// The engine never reads config; it only performs the arithmetic.
    pub headroom: u32,
    pub max_tokens_override: Option<u32>,
    /// TTL applied to any cache breakpoint this engine attaches (see
    /// `execute`'s cache-hint post-pass). Threaded straight from
    /// `AgentSpec::cache_ttl` — every production caller sets
    /// `CacheTtl::FiveMinutes` today (`runtime.rs`, `subagent.rs`), so this
    /// is a plain value handoff, not a new policy decision.
    pub cache_ttl: CacheTtl,
    pub cancel: CancellationToken,
}

/// The result of a successful [`AttemptEngine::execute`] call.
#[derive(Debug, Clone)]
pub struct AttemptOutcome {
    pub response: GenerateResponse,
    pub route: Route,
    /// Total backend calls made across the whole chain, including the
    /// non-streaming `ToolParse` retry.
    pub attempts: u8,
    pub latency: Duration,
    /// Candidates the headroom gate excluded before any backend call, each
    /// with the `CapabilitySkip` reason it was skipped for.
    pub skipped: Vec<(ModelRef, RoutingReason)>,
}

/// Which shape of backend call one attempt uses, resolved from the
/// candidate's declared tool-calling capability and whether the request
/// carries any tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strategy {
    Stream,
    Generate,
}

/// §runtime strategy table: `Streaming{validated:true}` + tools -> stream;
/// any other tool-calling level + tools -> generate; no tools -> stream
/// regardless of capability.
fn strategy_for(caps: &Capabilities, has_tools: bool) -> Strategy {
    if !has_tools {
        return Strategy::Stream;
    }
    match caps.tool_calling {
        ToolCallSupport::Streaming { validated: true } => Strategy::Stream,
        _ => Strategy::Generate,
    }
}

/// Concatenates every `ContentBlock::Text` in `blocks`, in order — used to
/// synthesize a single full-text `TextDelta` for the `generate()` path so
/// its caller-facing stream contract matches `stream()`.
fn full_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The endpoint identity for a backend. Mirrors
/// `conway_routing::router::endpoint_of` (crate-private there): endpoint
/// identity is 1:1 with backend identity for MVP.
fn endpoint_of(backend: &BackendId) -> EndpointId {
    EndpointId::new(backend.as_str())
}

/// Attaches cache breakpoint hints to `segments` in place, keyed on `caps`
/// — the capability actually resolved for `model` (`Backend::capabilities`,
/// called by `execute` right before this), never a pre-routing placeholder.
/// This is `conway-backends`' own "additive post-pass" framing
/// (`anthropic::wire`'s module doc) applied one layer up: by the time this
/// runs, `segments` is the FINAL list a `ContextHook` may already have
/// added to, dropped from, or reordered (WI-126), so the A/B breakpoint
/// indices are re-derived from provenance here (`breakpoint_indices`)
/// rather than threaded from `ContextBuilder::build` time, where they could
/// have gone stale.
///
/// A no-op whenever `caps.cache` is `ImplicitPrefix`/`None`/any other mode
/// `context::builder::attach_cache_hints` does not recognize (that
/// function's own match decides this) — which is what keeps every
/// OpenAI-compatible backend's request byte-identical to before this
/// existed: `PromptSegment::cache_hint` is read by exactly one module in
/// the whole workspace, `conway_backends::anthropic::cache`
/// (`openai_compat::wire`'s own module doc; `GenerateRequest::cache_hint`
/// does not exist — the field lives per-segment, not per-request).
///
/// Also a no-op if `segments` carries no `Provenance::ToolRegistry` segment
/// at all (a hook dropped the normally-unconditional `ToolSchemas`
/// segment) — there is no A to breakpoint on in that case, so nothing is
/// marked rather than guessing.
fn attach_route_cache_hints(
    segments: &mut [PromptSegment],
    model: &ModelId,
    caps: &Capabilities,
    ttl: CacheTtl,
) {
    let (a_index, b_index) = breakpoint_indices(segments);
    let Some(a_index) = a_index else {
        return;
    };
    let key = prefix_key(model, segments);
    attach_cache_hints(segments, &caps.cache, ttl, a_index, b_index, &key);
}

/// Turns an ordered candidate list plus assembled segments into one
/// `GenerateResponse`. Backends are injected; the engine never constructs
/// one.
pub struct AttemptEngine {
    backends: HashMap<BackendId, Arc<dyn Backend>>,
    health: Arc<dyn HealthRegistry>,
    bus: Arc<EventBus>,
}

impl AttemptEngine {
    pub fn new(
        backends: HashMap<BackendId, Arc<dyn Backend>>,
        health: Arc<dyn HealthRegistry>,
        bus: Arc<EventBus>,
    ) -> Self {
        Self {
            backends,
            health,
            bus,
        }
    }

    /// Looks up the backend for `id`. A `Route` naming a backend absent from
    /// the injected map is a caller precondition violation (the router/pin
    /// resolver must only ever name backends the runtime was configured
    /// with), so this panics rather than inventing an `RuntimeError` variant
    /// for a state that should be unreachable. The blast radius is one
    /// agent task, not the process: WI-083's supervisor catches panics via
    /// `JoinError::is_panic()` and synthesizes a `Failed` terminal result.
    fn backend_for(&self, id: &BackendId) -> Arc<dyn Backend> {
        self.backends
            .get(id)
            .unwrap_or_else(|| panic!("AttemptEngine: no backend injected for {id}"))
            .clone()
    }

    pub async fn execute(&self, req: AttemptRequest<'_>) -> Result<AttemptOutcome, RuntimeError> {
        // T-1 pre-flight (backstop gate): partition candidates by whether
        // their declared window covers the prompt plus reserved headroom.
        // The admissibility predicate is the SHARED one --
        // `context_shortfall(...).is_none()` (Min-1 / P-14, board item
        // 01KZ00VV3F3EBZ9WQSB292TBJZ) -- not a local restatement of the
        // arithmetic, so a future edit to the gate (a `>=` vs `>`, a safety
        // margin) changes this backstop in lockstep with the router's gate.
        // Inclusive bound: `est + headroom == max_context_tokens` is
        // admissible (context_shortfall returns `None` there).
        let mut admissible: Vec<Route> = Vec::new();
        let mut skipped: Vec<(ModelRef, RoutingReason)> = Vec::new();
        for route in req.routes {
            let caps = self.backend_for(&route.backend).capabilities(&route.model);
            if conway_routing::context_shortfall(&caps, req.est_tokens, req.headroom).is_none() {
                admissible.push(route);
            } else {
                let model_ref = ModelRef {
                    backend: route.backend.clone(),
                    model: route.model.clone(),
                };
                skipped.push((
                    model_ref.clone(),
                    RoutingReason::CapabilitySkip {
                        skipped: model_ref,
                        missing: vec!["min_context".to_string()],
                    },
                ));
            }
        }

        if admissible.is_empty() {
            let (model, caps) = skipped
                .iter()
                .map(|(model_ref, _)| {
                    (
                        model_ref.clone(),
                        self.backend_for(&model_ref.backend).capabilities(&model_ref.model),
                    )
                })
                .max_by_key(|(_, caps)| caps.max_context_tokens)
                .expect("T-1 gate: req.routes is non-empty whenever admissible is empty");
            // The error payload's arithmetic is sourced from the SAME shared
            // gate (Min-1 / P-14): `shortfall` is exactly what
            // `context_shortfall` computes, and `required = window +
            // shortfall` (== est + headroom) is derived from it rather than
            // restated. `expect` is an internal invariant, not user input:
            // `admissible` is empty only because every route shortfell.
            let shortfall = conway_routing::context_shortfall(&caps, req.est_tokens, req.headroom)
                .expect("T-1 gate: every skipped route shortfalls against its window");
            return Err(RuntimeError::Routing(RoutingError::ContextTooLarge {
                role: req.role,
                model,
                est_tokens: req.est_tokens,
                headroom_tokens: req.headroom,
                required_tokens: caps.max_context_tokens.saturating_add(shortfall),
                max_context_tokens: caps.max_context_tokens,
                shortfall_tokens: shortfall,
            }));
        }

        let has_tools = !req.tools.is_empty();
        let mut attempt: u8 = 0;
        let mut considered: Vec<(ModelRef, String)> = Vec::new();

        for route in &admissible {
            let backend = self.backend_for(&route.backend);
            let caps = backend.capabilities(&route.model);
            let model_ref = ModelRef {
                backend: route.backend.clone(),
                model: route.model.clone(),
            };
            let endpoint = endpoint_of(&route.backend);

            // Cache-hint post-pass (WI: prompt caching), keyed on THIS
            // route's resolved `caps.cache` — not `ContextInput.cache_mode`,
            // which every production caller sets to a pre-routing
            // placeholder (`CacheMode::None`) since `ContextBuilder::build`
            // runs before a model is known. Computed once per candidate
            // route (not once per whole `execute` call) so a fallback chain
            // that crosses dialects (e.g. Anthropic -> a local
            // `ImplicitPrefix` model) gets each candidate's OWN correct
            // treatment rather than the first route's. See
            // `attach_route_cache_hints`'s own doc.
            let mut route_segments = req.segments.to_vec();
            attach_route_cache_hints(&mut route_segments, &route.model, &caps, req.cache_ttl);

            let mut strategy = strategy_for(&caps, has_tools);
            let mut toolparse_retried = false;

            loop {
                self.bus.emit(
                    req.session,
                    req.agent_id,
                    Event::ModelDecision {
                        role: req.role.clone(),
                        chosen: model_ref.clone(),
                        reason: route.reason.clone(),
                        attempt,
                    },
                );
                attempt += 1;

                let gen_req = self.build_request(
                    &route_segments,
                    req.tools,
                    req.prefix_key.clone(),
                    req.max_tokens_override,
                    req.headroom,
                    route,
                    model_ref.model.clone(),
                );
                let start = Instant::now();
                let result = match strategy {
                    Strategy::Stream => {
                        self.run_stream(req.session, req.agent_id, &*backend, gen_req, &req.cancel)
                            .await
                    }
                    Strategy::Generate => {
                        self.run_generate(
                            req.session,
                            req.agent_id,
                            &*backend,
                            gen_req,
                            &req.cancel,
                        )
                        .await
                    }
                };

                match result {
                    Ok(response) => {
                        let latency = start.elapsed();
                        self.health.record(
                            &endpoint,
                            Observation::Ok {
                                latency_ms: latency.as_millis().min(u32::MAX as u128) as u32,
                            },
                        );
                        return Ok(AttemptOutcome {
                            response,
                            route: route.clone(),
                            attempts: attempt,
                            latency,
                            skipped: skipped.clone(),
                        });
                    }
                    Err(err) => match classify(&err) {
                        FailureClass::Fatal => {
                            let is_tool_parse = matches!(err, BackendError::ToolParse { .. });
                            if is_tool_parse && strategy == Strategy::Stream && !toolparse_retried {
                                // Exactly one non-streaming retry against the
                                // identical request on the same route.
                                toolparse_retried = true;
                                strategy = Strategy::Generate;
                                continue;
                            }
                            if is_tool_parse {
                                // A second ToolParse (or a ToolParse from a
                                // request that was already non-streaming):
                                // advance the chain, no health record (T-2).
                                considered.push((model_ref.clone(), err.to_string()));
                                break;
                            }
                            // Auth, Cancelled, and any future unrecognized
                            // Fatal variant: not worth retrying anywhere in
                            // this turn (T-2) -- abort the whole chain.
                            return Err(match err {
                                BackendError::Cancelled => RuntimeError::Cancelled {
                                    agent: req.agent_id,
                                    reason: "attempt cancelled".to_string(),
                                },
                                other => RuntimeError::Backend(other),
                            });
                        }
                        FailureClass::FailoverRetryable | FailureClass::RequestIncompatible => {
                            if let Some(obs) = observation_for(&err) {
                                let before = self.health.state(&endpoint);
                                self.health.record(&endpoint, obs);
                                if let (BreakerState::Closed, BreakerState::Open { until, kind }) =
                                    (before, self.health.state(&endpoint))
                                {
                                    self.bus.emit(
                                        req.session,
                                        req.agent_id,
                                        Event::BackendDegraded {
                                            endpoint: endpoint.clone(),
                                            breaker: kind,
                                            until,
                                        },
                                    );
                                }
                            }
                            considered.push((model_ref.clone(), err.to_string()));
                            break;
                        }
                    },
                }
            }
        }

        Err(RuntimeError::Routing(RoutingError::NoCandidate {
            role: req.role,
            considered,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn build_request(
        &self,
        segments: &[PromptSegment],
        tools: &[ToolSpec],
        prefix_key: Option<PrefixKey>,
        max_tokens_override: Option<u32>,
        headroom: u32,
        route: &Route,
        model: conway_core::ids::ModelId,
    ) -> GenerateRequest {
        let mut params = route.params.clone();
        params.max_tokens = Some(max_tokens_override.unwrap_or(headroom));
        GenerateRequest {
            model,
            segments: segments.to_vec(),
            tools: tools.to_vec(),
            params,
            prefix_key,
        }
    }

    /// Drives a non-streamed backend call, then emits one `TextDelta`
    /// carrying the full response text so the caller-facing stream contract
    /// is identical to [`Self::run_stream`]'s.
    async fn run_generate(
        &self,
        session: SessionId,
        agent: AgentId,
        backend: &dyn Backend,
        req: GenerateRequest,
        cancel: &CancellationToken,
    ) -> Result<GenerateResponse, BackendError> {
        let response = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(BackendError::Cancelled),
            res = backend.generate(req) => res?,
        };
        self.bus.emit(
            session,
            agent,
            Event::TextDelta {
                text: full_text(&response.content),
            },
        );
        Ok(response)
    }

    /// Drives a streamed backend call, mapping `TextDelta`/`ThinkingDelta`
    /// chunks to bus events immediately as they arrive, and accumulating
    /// into the final `Done(GenerateResponse)`.
    async fn run_stream(
        &self,
        session: SessionId,
        agent: AgentId,
        backend: &dyn Backend,
        req: GenerateRequest,
        cancel: &CancellationToken,
    ) -> Result<GenerateResponse, BackendError> {
        let mut stream = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(BackendError::Cancelled),
            res = backend.stream(req) => res?,
        };

        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(BackendError::Cancelled),
                next = stream.next() => {
                    match next {
                        Some(Ok(StreamChunk::TextDelta(text))) => {
                            self.bus.emit(session, agent, Event::TextDelta { text });
                        }
                        Some(Ok(StreamChunk::ThinkingDelta(text))) => {
                            self.bus.emit(session, agent, Event::ThinkingDelta { text });
                        }
                        Some(Ok(StreamChunk::Done(response))) => return Ok(response),
                        // `ToolCallDelta` and any future non-exhaustive
                        // variant carry nothing this engine's stream
                        // contract needs to forward.
                        Some(Ok(_)) => {}
                        Some(Err(err)) => return Err(err),
                        None => {
                            return Err(BackendError::Transport {
                                detail: "stream ended without a Done chunk".to_string(),
                            });
                        }
                    }
                }
            }
        }
    }
}
