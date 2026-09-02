//! `AttemptEngine`: turns an ordered candidate list plus assembled segments
//! into one `GenerateResponse`.
//!
//! Responsibilities: choose streaming vs non-streaming per the declared
//! tool-calling capability, sequence the fallback chain, enforce the
//! per-candidate `Backend::admit` context gate, and record health
//! observations with the T-2 classification.
//!
//! **T-1, AUTHORITATIVE:** each
//! route's `GenerateRequest` is built first -- segments already carrying
//! that specific candidate's cache hints, tools, prefix key, and resolved
//! sampling params -- then handed to `backend.admit(&gen_req, req.headroom)`.
//! `admit` is the backend's OWN dialect-aware estimate of its OWN wire body
//! (Anthropic's Messages envelope vs. an OpenAI-compatible chat-completions
//! body genuinely serialize to different byte counts for identical content),
//! never a pre-flight restatement of the arithmetic. A candidate `admit`
//! refuses (`Err(BackendError::ContextTooLarge)`) is skipped before any
//! network call and never feeds a health `Observation` (T-2: a too-large
//! prompt is a request problem, not an endpoint-health signal). When EVERY
//! candidate refuses this way, the refusals are aggregated into
//! `RuntimeError::Routing(RoutingError::ContextTooLarge)`, naming the
//! largest window among them and sourcing every number from the refusing
//! `BackendError`s directly -- never recomputed locally. This replaces the
//! former pre-flight partition by `conway-routing`'s own restatement of the
//! arithmetic over the router's declared window (now retired) -- see
//! `docs/routing.md`'s "Advisory vs. authoritative" section for the split
//! this item drew.
//!
//! **Build-order note:** `gen_req` is built once per candidate, immediately
//! after that candidate's own cache-hint pass (`attach_route_cache_hints`)
//! -- the SAME relative order as before this item (cache hints were always
//! resolved before `build_request`; only WHEN `build_request` ran relative
//! to the retry loop changed). `gen_req`'s fields (segments/tools/
//! prefix_key/params/model) do not depend on `Strategy`, so a single build
//! reused (cloned) across the ToolParse-retry loop's Stream -> Generate
//! switch is behaviourally identical to the old per-attempt rebuild --
//! `toolparse_triggers_one_retry_then_advances_chain` (`attempt_fallback.rs`)
//! pins byte-identical retry requests. Cache-hint semantics are therefore
//! unchanged: per-candidate, computed once, never per-attempt.
//!
//! T-1 error-shape reconciliation (a decision
//! closing an earlier gap): the router (conway-routing
//! `DeclarativeRouter`) still constructs `RoutingError::
//! ContextTooLarge` from its own ADVISORY declared-window check
//! (`conway_core::capabilities::RequiredCaps`-based `satisfies`, evaluated
//! against the router's own `heuristic-chars4` estimate) when every
//! candidate it considered was rejected solely on that check (see that
//! crate's `router.rs` module doc); this engine's `admit`-based gate above
//! is the AUTHORITATIVE second construction site, reachable whenever the
//! router admitted a candidate whose real backend still refuses (a stale or
//! incorrect capability entry, or simply a different, more accurate
//! estimate) -- this matters especially for the pin path, which can bypass
//! the router's chain filtering entirely. The two are deliberately NOT
//! required to agree: the router's
//! estimate over a declared window and `admit`'s measure of the actual
//! serialized wire body are different questions asked at different times.
//!
//! **Same-candidate stream retry (board item `01M1FSJ4E2S5M9KBSBJAAPJQ48`).**
//! `conway_plugin_backends::http::HttpClient::send_with_retry` only ever
//! retries the INITIAL response of a request -- a mid-stream drop used to
//! either advance the fallback chain immediately (silently changing models
//! mid-task) or, on the last candidate, fail the whole turn. `run_stream`
//! now distinguishes a failure raised before the stream ever opened (kept
//! exactly as before: classify, record health if eligible, advance the
//! chain) from one raised AFTER it opened -- `BackendError::Transport`/
//! `ServerError` mid-stream, or a stream that ends with no `Done` chunk.
//! For that second case, `execute`'s per-candidate loop retries the SAME
//! candidate up to twice more (three attempts total, `conway_core::retry`'s
//! shared `MAX_RETRIES`/`max_jitter` -- the identical policy
//! `send_with_retry` uses, so the two can never drift), emitting
//! `Event::StreamRestarted` before each retry so a renderer can discard the
//! partial deltas already on the bus (the assistant record itself was never
//! at risk: it is only persisted after a `Done`). Each failed attempt --
//! retried or not -- records a health `Observation` exactly as before
//! (`record_failure_observation`, shared by both this retry and the
//! eventual chain-advancing failure); the chain advances only once the
//! same-candidate budget is exhausted. `RateLimit`, `RequestIncompatible`,
//! `Fatal`, and any pre-stream failure are untouched by this -- they keep
//! today's immediate chain-advance (or abort, for `Fatal`) behavior.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use conway_core::capabilities::{Capabilities, ToolCallSupport};
use conway_core::content::{ContentBlock, ToolSpec};
use conway_core::error::{BackendError, RoutingError, RuntimeError};
use conway_core::event::Event;
use conway_core::failure::{classify, observation_for, FailureClass};
use conway_core::ids::{
    AgentId, BackendId, EndpointId, ModelId, ModelRef, PrefixKey, RoleAlias, SessionId,
};
use conway_core::ports::{Backend, GenerateRequest, GenerateResponse, HealthRegistry, StreamChunk};
use conway_core::retry::{max_jitter, MAX_RETRIES};
use conway_core::routing::{BreakerState, Observation, Route, RoutingReason};
use conway_core::segment::{CacheTtl, PromptSegment};
use futures::StreamExt;
use rand::RngExt;
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
    /// The caller's own (advisory) estimate -- carried for callers that
    /// still want it (e.g. `agent_loop.rs`'s `ContextHookCtx`), but no
    /// longer read by `execute` itself:
    /// each candidate's own `Backend::admit` produces its own authoritative
    /// estimate from the actually-built `GenerateRequest`, not this field.
    pub est_tokens: u32,
    /// Reserved output/reasoning budget, resolved by the caller.
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
    /// Candidates `Backend::admit` refused before any network call, each
    /// with a `CapabilitySkip` reason carrying that refusal's own message.
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
/// `conway_plugin_routing::router::endpoint_of` (crate-private there): endpoint
/// identity is 1:1 with backend identity for MVP.
fn endpoint_of(backend: &BackendId) -> EndpointId {
    EndpointId::new(backend.as_str())
}

/// Attaches cache breakpoint hints to `segments` in place, keyed on `caps`
/// — the capability actually resolved for `model` (`Backend::capabilities`,
/// called by `execute` right before this), never a pre-routing placeholder.
/// This is `conway-plugin-backends`' own "additive post-pass" framing
/// (`anthropic::wire`'s module doc) applied one layer up: by the time this
/// runs, `segments` is the FINAL list a `ContextHook` may already have
/// added to, dropped from, or reordered, so the A/B breakpoint
/// indices are re-derived from provenance here (`breakpoint_indices`)
/// rather than threaded from `ContextBuilder::build` time, where they could
/// have gone stale.
///
/// A no-op whenever `caps.cache` is `ImplicitPrefix`/`None`/any other mode
/// `context::builder::attach_cache_hints` does not recognize (that
/// function's own match decides this) — which is what keeps every
/// OpenAI-compatible backend's request byte-identical to before this
/// existed: `PromptSegment::cache_hint` is read by exactly one module in
/// the whole workspace, `conway_plugin_backends::anthropic::cache`
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
    /// agent task, not the process: the supervisor catches panics via
    /// `JoinError::is_panic()` and synthesizes a `Failed` terminal result.
    fn backend_for(&self, id: &BackendId) -> Arc<dyn Backend> {
        self.backends
            .get(id)
            .unwrap_or_else(|| panic!("AttemptEngine: no backend injected for {id}"))
            .clone()
    }

    pub async fn execute(&self, req: AttemptRequest<'_>) -> Result<AttemptOutcome, RuntimeError> {
        let has_tools = !req.tools.is_empty();
        let mut attempt: u8 = 0;
        let mut considered: Vec<(ModelRef, String)> = Vec::new();
        let mut skipped: Vec<(ModelRef, RoutingReason)> = Vec::new();
        // The raw refusals `Backend::admit` produced, kept alongside
        // `skipped` (whose `missing` is already a rendered `String`) so the
        // all-refused aggregate below can source its numbers directly from
        // the `BackendError`s rather than recomputing anything.
        let mut admission_refusals: Vec<(ModelRef, BackendError)> = Vec::new();
        let mut any_admitted = false;

        for route in req.routes {
            let backend = self.backend_for(&route.backend);
            let caps = backend.capabilities(&route.model);
            let model_ref = ModelRef {
                backend: route.backend.clone(),
                model: route.model.clone(),
            };

            // Cache-hint post-pass (WI: prompt caching), keyed on THIS
            // route's resolved `caps.cache` — not `ContextInput.cache_mode`,
            // which every production caller sets to a pre-routing
            // placeholder (`CacheMode::None`) since `ContextBuilder::build`
            // runs before a model is known. Computed once per candidate
            // route (not once per whole `execute` call) so a fallback chain
            // that crosses dialects (e.g. Anthropic -> a local
            // `ImplicitPrefix` model) gets each candidate's OWN correct
            // treatment rather than the first route's. See
            // `attach_route_cache_hints`'s own doc. UNCHANGED relative
            // order vs. before this item: cache hints are still resolved
            // before the request is built, and still once per candidate.
            let mut route_segments = req.segments.to_vec();
            attach_route_cache_hints(&mut route_segments, &route.model, &caps, req.cache_ttl);

            // Built once per candidate (see this fn's module-doc "build-order
            // note"): every field is independent of `Strategy`, so the same
            // `gen_req` -- cloned, never rebuilt -- serves every attempt this
            // route makes, including the ToolParse-retry's Stream -> Generate
            // switch below.
            let gen_req = self.build_request(
                &route_segments,
                req.tools,
                req.prefix_key.clone(),
                req.max_tokens_override,
                req.headroom,
                &route,
                model_ref.model.clone(),
            );

            // T-1, AUTHORITATIVE (see module doc): the backend's own
            // `admit`, over the request actually built for it. A refusal
            // skips this ONE candidate -- never a backend call, never a
            // health `Observation` -- and the chain advances.
            if let Err(err) = backend.admit(&gen_req, req.headroom) {
                skipped.push((
                    model_ref.clone(),
                    RoutingReason::CapabilitySkip {
                        skipped: model_ref.clone(),
                        missing: vec![err.to_string()],
                    },
                ));
                admission_refusals.push((model_ref, err));
                continue;
            }
            any_admitted = true;

            let endpoint = endpoint_of(&route.backend);
            let mut strategy = strategy_for(&caps, has_tools);
            let mut toolparse_retried = false;
            // Same-candidate stream retry (see this fn's module doc): how
            // many of the (up to `MAX_RETRIES`) mid-stream retries THIS
            // candidate has already used. Reset per candidate -- a fresh
            // route gets its own full budget, mirroring `toolparse_retried`
            // just above.
            let mut stream_retry_count: u32 = 0;

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

                let start = Instant::now();
                let mut stream_failure = StreamFailure::default();
                let result = match strategy {
                    Strategy::Stream => {
                        self.run_stream(
                            req.session,
                            req.agent_id,
                            &*backend,
                            gen_req.clone(),
                            &req.cancel,
                            &mut stream_failure,
                        )
                        .await
                    }
                    Strategy::Generate => {
                        self.run_generate(
                            req.session,
                            req.agent_id,
                            &*backend,
                            gen_req.clone(),
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
                            //
                            // `reason` here is a generic placeholder, not the
                            // caller's own `conway_cancel` string: this
                            // engine has no `AgentTree` handle to look that
                            // up with (it is deliberately backend/routing
                            // machinery only), and `run_generate`/
                            // `run_stream`'s `select!` -- the only place a
                            // `BackendError::Cancelled` is produced -- has
                            // already collapsed whichever reason the caller
                            // gave down to a bare token trip by this point.
                            // rather
                            // than plumb a tree handle in here, the caller's
                            // reason is recovered one level up --
                            // `agent_loop.rs`'s `finish_error` OVERWRITES
                            // this placeholder with `tree.cancel_reason`
                            // whenever this agent was itself the direct
                            // target of the cancel (see that fn's own doc),
                            // so it never actually reaches a persisted
                            // `AgentResult`.
                            return Err(match err {
                                BackendError::Cancelled => RuntimeError::Cancelled {
                                    agent: req.agent_id,
                                    reason: "attempt cancelled".to_string(),
                                },
                                other => RuntimeError::Backend(other),
                            });
                        }
                        FailureClass::FailoverRetryable | FailureClass::RequestIncompatible => {
                            // Same-candidate stream retry (module doc):
                            // eligible only for a `Transport`/`ServerError`
                            // raised AFTER at least one real chunk was read
                            // off the stream (`stream_failure.stream_opened`,
                            // set in `run_stream` -- a pre-stream failure,
                            // an immediate error/end with zero content read,
                            // or the `Strategy::Generate` path, which never
                            // sets it, all keep today's immediate-advance
                            // behavior), and only while THIS candidate's
                            // budget remains. `RateLimit` and
                            // `RequestIncompatible` (`ContextOverflow`/
                            // `ContextTooLarge`/`BadRequest`) never match
                            // the `Transport | ServerError` guard below, so
                            // they always fall through to the unconditional
                            // record-and-advance path exactly as before.
                            let same_candidate_retry_eligible = stream_failure.stream_opened
                                && stream_retry_count < MAX_RETRIES
                                && matches!(
                                    err,
                                    BackendError::Transport { .. }
                                        | BackendError::ServerError { .. }
                                );

                            if same_candidate_retry_eligible {
                                // This attempt failed but does NOT advance
                                // the chain -- record its health
                                // observation now (T-2: "each failed
                                // attempt records exactly as today"); the
                                // eventual chain-advancing failure (below,
                                // once the budget is exhausted) records its
                                // own separately.
                                self.record_failure_observation(
                                    req.session,
                                    req.agent_id,
                                    &endpoint,
                                    &err,
                                );

                                stream_retry_count += 1;
                                // 1-based ordinal of the UPCOMING retry:
                                // `attempt` (the `u8` "total calls made"
                                // counter above) already equals the ordinal
                                // of the call that just failed (it was
                                // incremented past it at this loop
                                // iteration's top, before the call ran), so
                                // the NEXT call's ordinal is one more.
                                self.bus.emit(
                                    req.session,
                                    req.agent_id,
                                    Event::StreamRestarted {
                                        agent_id: req.agent_id,
                                        attempt: u32::from(attempt) + 1,
                                        discarded_text_chars: stream_failure.discarded_text_chars,
                                        discarded_thinking_chars: stream_failure
                                            .discarded_thinking_chars,
                                    },
                                );

                                let sleep_for = jittered_backoff(stream_retry_count - 1);
                                tokio::select! {
                                    biased;
                                    () = req.cancel.cancelled() => {
                                        return Err(RuntimeError::Cancelled {
                                            agent: req.agent_id,
                                            reason: "attempt cancelled".to_string(),
                                        });
                                    }
                                    () = tokio::time::sleep(sleep_for) => {}
                                }
                                continue;
                            }

                            self.record_failure_observation(
                                req.session,
                                req.agent_id,
                                &endpoint,
                                &err,
                            );
                            considered.push((model_ref.clone(), err.to_string()));
                            break;
                        }
                    },
                }
            }
        }

        if !any_admitted {
            // Every candidate refused on size (see module doc): aggregate
            // into the WHOLE request's `ContextTooLarge`, naming the
            // largest window among the refusals -- the best case that
            // still didn't fit, mirroring the router's own T-1 aggregate
            // (`router.rs`'s `resolve`). Every number is sourced from the
            // refusing `BackendError`s directly, never recomputed. Never
            // records a health `Observation` (T-2: a too-large prompt is a
            // request problem, not an endpoint-health signal) -- this
            // branch makes no call to `self.health.record` anywhere above.
            let worst = admission_refusals
                .into_iter()
                .max_by_key(|(_, err)| match err {
                    BackendError::ContextTooLarge {
                        max_context_tokens, ..
                    } => *max_context_tokens,
                    _ => 0,
                });
            let Some((model, err)) = worst else {
                // `req.routes` was itself empty -- unreachable in production
                // (`Router::resolve` never returns an empty `Ok(routes)`;
                // `config::validate` rejects an empty chain), but a direct
                // `AttemptRequest` construction (e.g. a test, or a future
                // caller) could still hit it. `NoCandidate` with nothing
                // considered describes that precisely.
                return Err(RuntimeError::Routing(RoutingError::NoCandidate {
                    role: req.role,
                    considered: Vec::new(),
                }));
            };
            let BackendError::ContextTooLarge {
                est_tokens,
                headroom_tokens,
                required_tokens,
                max_context_tokens,
                shortfall_tokens,
                ..
            } = err
            else {
                // `Backend::admit`'s documented contract is `Ok` or
                // `Err(BackendError::ContextTooLarge)` only -- a
                // non-conformant implementation returning anything else is
                // surfaced as the backend error it actually produced rather
                // than fabricating T-1 numbers it never gave us.
                return Err(RuntimeError::Backend(err));
            };
            return Err(RuntimeError::Routing(RoutingError::ContextTooLarge {
                role: req.role,
                model,
                est_tokens,
                headroom_tokens,
                required_tokens,
                max_context_tokens,
                shortfall_tokens,
            }));
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
    ///
    /// `failure` is an out-parameter, written only on an `Err` return (left
    /// at its `Default` -- `stream_opened: false`, zero counts -- on `Ok`,
    /// where nothing reads it): `stream_opened` flips to `true` on the
    /// first chunk this attempt actually reads off the stream, success or
    /// error -- NOT merely on `backend.stream()` itself returning `Ok`. A
    /// connection that opens at the HTTP layer and then fails before a
    /// single `StreamChunk` is read (a proxy that accepts the socket while
    /// the real upstream is down, an immediate reset) is indistinguishable
    /// from a pre-open failure and must fail over immediately, exactly like
    /// one -- the caller's "did this failure happen after the stream
    /// opened" question (module doc) means "did real content start
    /// arriving," not "did the initial handshake succeed."
    /// `discarded_text_chars`/`discarded_thinking_chars` accumulate this
    /// ONE attempt's own deltas -- already forwarded to the bus below as
    /// they arrive -- so a caller that decides to retry can tell a
    /// renderer exactly how much to discard via `Event::StreamRestarted`.
    async fn run_stream(
        &self,
        session: SessionId,
        agent: AgentId,
        backend: &dyn Backend,
        req: GenerateRequest,
        cancel: &CancellationToken,
        failure: &mut StreamFailure,
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
                            failure.stream_opened = true;
                            failure.discarded_text_chars += text.chars().count();
                            self.bus.emit(session, agent, Event::TextDelta { text });
                        }
                        Some(Ok(StreamChunk::ThinkingDelta(text))) => {
                            failure.stream_opened = true;
                            failure.discarded_thinking_chars += text.chars().count();
                            self.bus.emit(session, agent, Event::ThinkingDelta { text });
                        }
                        Some(Ok(StreamChunk::Done(response))) => return Ok(response),
                        // `ToolCallDelta` and any future non-exhaustive
                        // variant carry nothing this engine's stream
                        // contract needs to forward, but reading one
                        // successfully is still real content arriving.
                        Some(Ok(_)) => {
                            failure.stream_opened = true;
                        }
                        // No chunk was ever successfully read -- an error
                        // (or immediate end) right after `stream()` opened
                        // the connection is not distinguishable from a
                        // pre-open failure, so `stream_opened` stays
                        // `false` and the caller fails over immediately.
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

    /// This module's ONE place a failed attempt's health `Observation` is
    /// recorded (T-2): shared by the same-candidate stream retry (a failure
    /// that does NOT yet advance the chain) and the eventual chain-
    /// advancing failure, so both apply the identical Closed->Open edge
    /// detection and `Event::BackendDegraded` emission. `observation_for`
    /// returning `None` (a `RequestIncompatible`/`Fatal`-class error) is a
    /// documented no-op -- both call sites only ever pass an err whose
    /// class is `FailoverRetryable` or `RequestIncompatible`, so this
    /// silently does nothing for the latter, exactly as before this was
    /// factored out.
    fn record_failure_observation(
        &self,
        session: SessionId,
        agent: AgentId,
        endpoint: &EndpointId,
        err: &BackendError,
    ) {
        if let Some(obs) = observation_for(err) {
            let before = self.health.state(endpoint);
            self.health.record(endpoint, obs);
            if let (BreakerState::Closed, BreakerState::Open { until, kind }) =
                (before, self.health.state(endpoint))
            {
                self.bus.emit(
                    session,
                    agent,
                    Event::BackendDegraded {
                        endpoint: endpoint.clone(),
                        breaker: kind,
                        until,
                    },
                );
            }
        }
    }
}

/// The information [`AttemptEngine::run_stream`] hands back to its caller on
/// an `Err` return, via an out-parameter (see that method's own doc for
/// why): whether the failure happened after the stream opened, and how much
/// of THIS attempt's own content already reached the bus.
#[derive(Debug, Default, Clone, Copy)]
struct StreamFailure {
    stream_opened: bool,
    discarded_text_chars: usize,
    discarded_thinking_chars: usize,
}

/// Draws this same-candidate stream retry's backoff sleep from
/// `conway_core::retry`'s shared full-jitter window -- the identical policy
/// `conway_plugin_backends::http::HttpClient::send_with_retry` draws from,
/// so the two can never quietly disagree. `retry_index` is zero-based (`0`
/// for the first retry -> `250ms` window, `1` for the second -> `500ms`).
fn jittered_backoff(retry_index: u32) -> Duration {
    let max_jitter_ms = max_jitter(retry_index).as_millis() as u64;
    let millis = rand::rng().random_range(0..=max_jitter_ms);
    Duration::from_millis(millis)
}
