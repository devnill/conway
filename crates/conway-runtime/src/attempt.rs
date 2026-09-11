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
//! unchanged: per-candidate, computed once, never per-attempt. **One
//! deliberate exception (board item `01M23SDCE6T85Z48CRQ8NBY6PV` step 2):**
//! a `BackendError::ToolArgumentsInvalid` retry rebuilds `gen_req` around
//! `route_segments` plus a corrective segment pair
//! (`corrective_retry_segments`) naming what was wrong, so the model
//! actually sees the correction rather than an identical resend -- see
//! that function's own doc. The bare `ToolParse` retry (no valid
//! `(tool, arguments)` pair to correct) is unaffected and still resends
//! the cached `gen_req` unchanged.
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

use chrono::Utc;
use conway_core::capabilities::{Capabilities, ContextTokensSource, ToolCallSupport};
use conway_core::content::{ContentBlock, Role, StopReason, ToolCall, ToolSpec, Usage};
use conway_core::error::{BackendError, RoutingError, RuntimeError};
use conway_core::event::Event;
use conway_core::failure::{classify, observation_for, FailureClass};
use conway_core::ids::{
    AgentId, BackendId, EndpointId, ModelId, ModelRef, PrefixKey, RoleAlias, SessionId, ToolName,
};
use conway_core::ports::{Backend, GenerateRequest, GenerateResponse, HealthRegistry, StreamChunk};
use conway_core::provenance::Provenance;
use conway_core::retry::{max_jitter, MAX_RETRIES};
use conway_core::routing::{AttemptFailure, BreakerState, Observation, Route, RoutingReason};
use conway_core::segment::{CacheTtl, PromptSegment};
use futures::StreamExt;
use rand::RngExt;
use serde_json::Value;
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
    /// The winning route's own resolved context window ceiling
    /// (`caps.max_context_tokens`, the SAME `Capabilities` this fn already
    /// resolved via `backend.capabilities(&route.model)` for this
    /// candidate -- never a second, independent lookup). `crate::runway`
    /// reads this to compute the window-fill note.
    ///
    /// `None` only for the one sentinel value no real backend/dialect ever
    /// legitimately returns, `u32::MAX` -- see `window_of`'s own doc.
    pub max_context_tokens: Option<u32>,
    /// The winning route's [`ContextTokensSource`] -- `backend.
    /// context_window_source(&route.model)`, read directly from the SAME
    /// `Arc<dyn Backend>` this fn already holds (hosted OpenAI-compatible
    /// models item; closes the gap `crate::runway`'s own module doc used to
    /// disclose: "this crate cannot yet see `conway-plugin-backends`'
    /// `ContextTokensSource::Unverified` directly" -- it can, now, through
    /// this same `Backend` trait method every other capability question
    /// already goes through). `crate::runway` reads this to label the
    /// window-fill note's provenance when a floor governs.
    pub max_context_tokens_source: ContextTokensSource,
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

/// `caps.max_context_tokens`, unless it is the `u32::MAX` sentinel -- no
/// real backend/dialect ever legitimately returns that value (every dialect
/// floor and every declared window is far below `u32::MAX`), so a test
/// double that sets it is declaring "I have no number to offer" without
/// this crate needing an `Option<u32>` on `Capabilities` itself (a
/// `conway-core` type constructed by field literal at ~40 call sites
/// across the workspace -- widening it is out of this fn's scope). Distinct
/// from, and orthogonal to, `AttemptOutcome::max_context_tokens_source`:
/// this answers "is there a number at all", that answers "how much can the
/// number be trusted" -- a real backend's `Unverified`-sourced floor still
/// has a real, usable `u32` here.
fn window_of(caps: &Capabilities) -> Option<u32> {
    (caps.max_context_tokens != u32::MAX).then_some(caps.max_context_tokens)
}

/// Board item A1d ("say why a turn fell back"): appends `extra` (this
/// candidate's own predecessors' `Backend::admit` refusals, in the order
/// they were discovered) onto the winning `route`'s `RoutingReason::
/// Fallback::after`, WITHOUT disturbing whatever the router itself already
/// placed there (`conway_plugin_routing::DeclarativeRouter::evaluate`'s own
/// pre-filter skips, board item A1d's other half) -- the two lists never
/// name the same candidate: a router-level `CapabilitySkip`/`HealthSkip`
/// never reaches this engine's `req.routes` at all, and an admission
/// refusal can only happen to a candidate the router already selected. A
/// no-op for every other `RoutingReason` variant (`AliasPrimary`,
/// `PinnedByApi`/`PinnedByAgentDef`) -- `extra` is empty for the winning
/// route in every one of those cases anyway, since nothing before it could
/// have been skipped, but the match still names the reason explicitly
/// rather than reaching into an enum variant that cannot carry `after`.
fn with_admission_failures(mut route: Route, extra: &[AttemptFailure]) -> Route {
    if let RoutingReason::Fallback { after, .. } = &mut route.reason {
        after.extend(extra.iter().cloned());
    }
    route
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

/// Board item `01M23SDCE6T85Z48CRQ8NBY6PV` step 2: builds the "assistant
/// tried this malformed call, here is what was wrong" segment pair a
/// `BackendError::ToolArgumentsInvalid` retry appends before resending --
/// the model-facing correction coercion could not produce on its own.
/// Mirrors the SAME wire shape a real dispatched-and-failed tool call
/// already produces (`agent_loop.rs`'s own `ContentBlock::ToolUse`/
/// `ToolResultBlock` construction for a persisted turn; `context::builder`'s
/// `tool_result_block`), so every backend dialect renders it exactly like
/// an ordinary tool error -- the model has already learned to read that
/// shape from ordinary tool dispatch failures. The message names the exact
/// argument path and what was wrong (`detail`, `SchemaValidator::
/// validate`'s own rendering) -- "a bare 'invalid arguments' gives it
/// nothing to act on" is the constraint this satisfies.
///
/// **Ephemeral, not persisted.** These two segments live only inside the
/// ONE `GenerateRequest` this retry sends (`execute`'s caller rebuilds it
/// fresh around the return of this function, never reusing the cached
/// `gen_req`) -- nothing here reaches the session log. The turn's real,
/// eventually-successful assistant/tool-result pair is what `agent_loop.rs`
/// persists once `execute` returns `Ok`; a retry this function's own
/// caller gives up on (attempts exhausted) never touches the log either,
/// exactly like the pre-existing blind `ToolParse` retry it sits beside.
fn corrective_retry_segments(
    tool: &ToolName,
    call_id: &str,
    arguments: &Value,
    argument_path: &str,
    detail: &str,
) -> [PromptSegment; 2] {
    let assistant = PromptSegment::new(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            call_id: call_id.to_string(),
            name: tool.clone(),
            arguments: arguments.clone(),
        }],
        Provenance::Assistant,
    );
    let message = format!(
        "tool `{tool}`: arguments failed schema validation at `{argument_path}`: {detail}. \
         Retry this exact call with the value at `{argument_path}` corrected to match the \
         declared schema."
    );
    let result = PromptSegment::new(
        Role::ToolResult,
        vec![ContentBlock::ToolResultBlock {
            call_id: call_id.to_string(),
            blocks: vec![ContentBlock::Text { text: message }],
            is_error: true,
        }],
        Provenance::ToolResult {
            call_id: call_id.to_string(),
            tool: tool.clone(),
        },
    );
    [assistant, result]
}

/// `report`'s own tool name -- matched literally, never by depending on
/// `conway-tools`, mirroring `crate::result::REPORT_TOOL_NAME`'s own
/// precedent for this exact boundary (that module's own doc: `report_tool
/// .rs`'s architecture note says `conway-tools` must never become a
/// dependency of `conway-runtime`, so the ONLY way this crate can
/// recognize the tool is by the name every dialect actually calls it).
const REPORT_TOOL_NAME: &str = "report";

/// The one JSON Pointer instance path `SchemaValidator::validate`
/// (`conway-plugin-backends`) ever names for `report`'s `summary` field:
/// the value that fails is the string itself, directly under the
/// object root, so its own `instance_path()` is exactly `/summary` --
/// never nested further (unlike, say, `conway_spawn`'s `/budget`, itself
/// an object one level down).
const REPORT_SUMMARY_PATH: &str = "/summary";

/// Reads `report`'s own declared `maxLength` for `summary` directly out of
/// `tools`' compiled `ToolSpec.schema` -- the SAME schemars-generated
/// document `SchemaValidator` already rejected the call against -- rather
/// than restating `conway-tools::report::report_tool::MAX_SUMMARY_CHARS`
/// as a second literal this crate could drift from (P-14: one declaration
/// of the bound). `None` if `report` is not registered at all (a chain
/// with no report tool declared) or its schema carries no numeric
/// `maxLength` at that path -- either way there is nothing safe to
/// truncate against, so the caller falls through to the ordinary
/// `ToolCallRejected` failure.
fn report_summary_max_len(tools: &[ToolSpec]) -> Option<usize> {
    let spec = tools.iter().find(|t| t.name.as_str() == REPORT_TOOL_NAME)?;
    let schema = serde_json::to_value(&spec.schema).ok()?;
    let max = schema.pointer("/properties/summary/maxLength")?.as_u64()?;
    usize::try_from(max).ok()
}

/// Appends a marker to the truncated summary that is visible to BOTH the
/// operator (rendered wherever a summary is shown -- the TUI, one-shot
/// output, the library's own `AgentResult`) and the parent MODEL (this
/// exact text is what `child_result_text` -- `context::builder` -- later
/// replays into the parent's own context, board item
/// `01M23JSD6DRAR8FMDJAZYBXQMB`'s "visible to the parent model as well as
/// the operator" requirement). Never silent (P-9): the marker names both
/// the original length and the bound that was exceeded, so nobody -- human
/// or model -- mistakes the shortened text for the verbatim summary the
/// child actually wrote.
fn truncated_summary_marker(original_chars: usize, max_len: usize) -> String {
    format!(
        " [summary truncated: the original was {original_chars} characters, exceeding the \
         {max_len}-character limit, and a corrective retry did not produce a shorter one -- \
         this is a shortened version of what the agent reported]"
    )
}

/// Board item `01M23JSD6DRAR8FMDJAZYBXQMB` step 1's "one bounded retry,
/// then truncate to the bound and deliver it, marked as truncated"
/// fallback -- reached only once BOTH `SchemaValidator::validate`'s narrow
/// coercion (`conway-plugin-backends`) AND `execute`'s own corrective
/// retry above have already failed to produce a valid call.
///
/// Scoped to EXACTLY `report`'s own `summary` field, deliberately narrower
/// than "any oversized string argument on any tool": `report` is
/// `PermissionClass::Safe`/`ToolCategory::Think` with no side effects of
/// its own (`conway-tools/src/report/report_tool.rs`'s own doc), so
/// substituting a shortened value for what the model asked to declare can
/// never change what the run actually DID -- only what it SAYS about what
/// it did, which the marker above makes unmistakable. The identical move
/// for, say, a `bash` command or a `write` path would be unsafe: a
/// truncated shell command or file path is not obviously still the same
/// (or even a valid) operation, so this fallback must never generalize to
/// "any tool, any string field" (P-10: untrusted model input gets a typed
/// response, never a guess at what the model "really meant" to do).
///
/// Returns `None` (never truncates -- the caller then falls through to the
/// ordinary, always-safe `ToolCallRejected` failure) unless ALL of: the
/// rejected tool is `report`; the rejected instance path is exactly
/// `/summary`; `report`'s own schema declares a numeric `maxLength` for it;
/// the value actually AT that path in `arguments` is a string longer than
/// that bound; AND the marker `truncated_summary_marker` would append is
/// itself no longer than that bound. That last guard is load-bearing, not
/// defensive filler: `max_len` is read dynamically, per call, from whatever
/// `ToolSpec` in `tools` is named `report` (`report_summary_max_len`) --
/// the built-in tool hardcodes 2000 against a roughly-200-character marker,
/// but nothing stops a THIRD-PARTY plugin from registering its own tool
/// also named `report` with a much smaller `summary` bound (`conway`'s
/// `PluginRegistry::from_plugins` rejects only duplicate tool names, never
/// duplicate SCHEMAS). Without the guard, `keep = max_len.saturating_sub
/// (marker_chars)` saturates to zero and this function would return
/// `Some(marker alone)` -- a value `marker_chars` long, i.e. LONGER than
/// `max_len`, which is exactly the schema violation this whole fallback
/// exists to avoid delivering. Refusing here is correct, not merely safe:
/// if the bound cannot even hold the sentence saying the summary was
/// shortened, there is no useful truncated result to hand back, and the
/// ordinary refusal at least names the tool and argument honestly. A
/// `required`/missing-field/wrong-type failure at a DIFFERENT path (or on a
/// different tool entirely) is not this function's concern either, for the
/// same reason.
fn try_truncate_report_summary(
    tools: &[ToolSpec],
    tool: &ToolName,
    argument_path: &str,
    arguments: &Value,
) -> Option<Value> {
    if tool.as_str() != REPORT_TOOL_NAME || argument_path != REPORT_SUMMARY_PATH {
        return None;
    }
    let max_len = report_summary_max_len(tools)?;
    let summary = arguments.pointer(REPORT_SUMMARY_PATH)?.as_str()?;
    let original_chars = summary.chars().count();
    if original_chars <= max_len {
        // Some OTHER schema failure produced this exact path/tool
        // combination (unreachable today -- `maxLength` is the only
        // constraint `summary` carries -- but a future schema change
        // could add one): there is nothing this function knows how to
        // safely shorten, so it defers rather than guessing.
        return None;
    }
    let marker = truncated_summary_marker(original_chars, max_len);
    let marker_chars = marker.chars().count();
    if marker_chars >= max_len {
        // The bound is too small to hold even the "this was shortened"
        // sentence -- e.g. a non-built-in `report`-named tool with a tiny
        // `summary` bound (this function's own doc). Delivering the marker
        // alone would itself be longer than `max_len`, violating the very
        // schema this fallback exists to satisfy, so defer to the ordinary
        // `ToolCallRejected` refusal instead of guessing at a shorter
        // marker that might mislead as much as it informs.
        return None;
    }
    let keep = max_len - marker_chars;
    // Truncates on a CHAR boundary (`.chars()`, never a raw byte index --
    // the multi-byte panic this repo already hit once, `crates/
    // conway-cli/src/tui/view/transcript.rs`'s own `truncate_chars_with_
    // ellipsis` doc). `maxLength` here is enforced by this workspace's
    // vendored `jsonschema` 0.48.2 via `bytecount::num_chars`, which counts
    // Unicode SCALAR VALUES (`.chars().count()`) -- the exact unit this
    // truncates to. Given the `marker_chars >= max_len` guard above already
    // returned, `keep + marker_chars == max_len` exactly, so the patched
    // value is guaranteed to satisfy the same compiled schema
    // `SchemaValidator` rejected it against.
    let mut truncated: String = summary.chars().take(keep).collect();
    truncated.push_str(&marker);
    let mut patched = arguments.clone();
    *patched.pointer_mut(REPORT_SUMMARY_PATH)? = Value::String(truncated);
    Some(patched)
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
        // Board item `01M23SDCE6T85Z48CRQ8NBY6PV`: the most recent failure
        // pushed onto `considered` below (from EITHER site that pushes to
        // it -- always overwritten, never merely set-once), kept alongside
        // it so that IF the chain is ultimately exhausted AND that LAST
        // failure was a `BackendError::ToolParse` (every coercion attempt
        // and the bounded model-facing retry both already failed -- see
        // this fn's `is_tool_parse` handling below), the terminal error can
        // name the TOOL and ARGUMENT the model got wrong instead of
        // collapsing into `RoutingError::NoCandidate`'s "no candidate"
        // wording -- which is actively misleading here: a candidate DID
        // answer, with a malformed tool call, not silence. A chain that
        // fails ToolParse on one candidate and something else on a LATER
        // one is unaffected: only the LAST recorded failure is ever
        // consulted, so a genuinely mixed-cause exhaustion that does not
        // itself END on a ToolParse keeps today's `NoCandidate` aggregate
        // byte-for-byte.
        let mut last_terminal_err: Option<BackendError> = None;
        // The candidate route that produced `last_terminal_err`, kept in
        // lock-step with it (set at the SAME site, never independently) so
        // that IF the exhausted chain's own truncate-and-deliver fallback
        // (`try_truncate_report_summary`, board item
        // `01M23JSD6DRAR8FMDJAZYBXQMB`) fires below, the synthetic
        // `AttemptOutcome` it returns can still name a real, resolvable
        // route/backend -- `route` itself is a `for` loop variable and goes
        // out of scope once the chain is exhausted, so this is the only way
        // to recover it there.
        let mut last_terminal_route: Option<Route> = None;
        let mut skipped: Vec<(ModelRef, RoutingReason)> = Vec::new();
        // Board item A1d ("say why a turn fell back"): the SAME admission
        // refusals `skipped` above already records, reshaped as
        // `AttemptFailure` (`model`/`error`/`at`) so the eventual winning
        // route's `RoutingReason::Fallback::after` can name them WITH the
        // refusal's own numbers (`BackendError::ContextTooLarge`'s
        // `Display`) -- never a second, independently-worded reason.
        // Distinct from `skipped` itself: that field stays `(ModelRef,
        // RoutingReason)` for its existing consumers (`AttemptOutcome::
        // skipped`, unit-tested by name in `attempt_fallback.rs`); this one
        // exists solely to enrich the reason already carried on the route
        // that ultimately succeeds.
        let mut skip_failures: Vec<AttemptFailure> = Vec::new();
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
            // switch below. `mut`: a `ToolArgumentsInvalid` retry (board item
            // `01M23SDCE6T85Z48CRQ8NBY6PV` step 2) is the ONE exception --
            // it rebuilds `gen_req` around `route_segments` PLUS the
            // corrective segments `corrective_retry_segments` produces,
            // for that one retry only. The bare `ToolParse` retry (unknown
            // tool/unterminated JSON/conflicting name -- no valid
            // `(tool, arguments)` pair to correct) still resends this same,
            // never-rebuilt `gen_req` exactly as before.
            let mut gen_req = self.build_request(
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
                let error_text = err.to_string();
                skipped.push((
                    model_ref.clone(),
                    RoutingReason::CapabilitySkip {
                        skipped: model_ref.clone(),
                        missing: vec![error_text.clone()],
                    },
                ));
                skip_failures.push(AttemptFailure {
                    model: model_ref.clone(),
                    error: error_text,
                    at: Utc::now(),
                });
                admission_refusals.push((model_ref, err));
                continue;
            }
            any_admitted = true;

            let endpoint = endpoint_of(&route.backend);
            let mut strategy = strategy_for(&caps, has_tools);
            // Bounds BOTH the bare `ToolParse` blind retry AND the
            // `ToolArgumentsInvalid` corrective retry to exactly one
            // attempt each candidate ("one is likely enough; two at most",
            // board item `01M23SDCE6T85Z48CRQ8NBY6PV`'s own decision) --
            // shared, not per-class, since a candidate only ever hits ONE
            // of the two causes per turn in practice and the bound is the
            // same either way.
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
                            route: with_admission_failures(route.clone(), &skip_failures),
                            attempts: attempt,
                            latency,
                            skipped: skipped.clone(),
                            max_context_tokens: window_of(&caps),
                            max_context_tokens_source: backend.context_window_source(&route.model),
                        });
                    }
                    Err(err) => match classify(&err) {
                        FailureClass::Fatal => {
                            let is_tool_parse = matches!(err, BackendError::ToolParse { .. });
                            let is_tool_args_invalid =
                                matches!(err, BackendError::ToolArgumentsInvalid { .. });
                            if (is_tool_parse || is_tool_args_invalid)
                                && strategy == Strategy::Stream
                                && !toolparse_retried
                            {
                                // Exactly one non-streaming retry on the same
                                // route. `ToolParse` resends the identical
                                // `gen_req` (nothing to correct with); a
                                // `ToolArgumentsInvalid` rebuilds it with the
                                // corrective segments appended below (board
                                // item `01M23SDCE6T85Z48CRQ8NBY6PV` step 2)
                                // -- the model-facing hand-back the spec asks
                                // for, not a blind resend.
                                toolparse_retried = true;
                                strategy = Strategy::Generate;
                                if let BackendError::ToolArgumentsInvalid {
                                    tool,
                                    call_id,
                                    arguments,
                                    argument_path,
                                    detail,
                                } = &err
                                {
                                    let mut retry_segments = route_segments.clone();
                                    retry_segments.extend(corrective_retry_segments(
                                        tool,
                                        call_id,
                                        arguments,
                                        argument_path,
                                        detail,
                                    ));
                                    gen_req = self.build_request(
                                        &retry_segments,
                                        req.tools,
                                        req.prefix_key.clone(),
                                        req.max_tokens_override,
                                        req.headroom,
                                        &route,
                                        model_ref.model.clone(),
                                    );
                                }
                                continue;
                            }
                            if is_tool_parse || is_tool_args_invalid {
                                // A second failure of either kind (or one
                                // from a request that was already
                                // non-streaming): advance the chain, no
                                // health record (T-2).
                                considered.push((model_ref.clone(), err.to_string()));
                                last_terminal_err = Some(err);
                                last_terminal_route = Some(route.clone());
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
                            // Overwrites any earlier `ToolParse` this same
                            // chain may have recorded on a PRIOR candidate:
                            // `last_terminal_err` names the failure that
                            // caused exhaustion, and this class is never
                            // `ToolParse` (see this fn's `is_tool_parse`
                            // branch above), so a chain that failed
                            // ToolParse then something else must not report
                            // the stale ToolParse at the end.
                            last_terminal_err = Some(err);
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

        // Board item `01M23JSD6DRAR8FMDJAZYBXQMB` step 1: before collapsing
        // an exhausted `ToolArgumentsInvalid` chain into the terminal
        // `ToolCallRejected` failure below, give `report`'s own `summary`
        // field its one truncate-and-deliver fallback -- see
        // `try_truncate_report_summary`'s own doc for exactly how narrow
        // this is. `last_terminal_route` is always `Some` whenever
        // `last_terminal_err` is `Some(ToolArgumentsInvalid { .. })` (the
        // two are set together, at the one site above), so the `let-else`
        // pattern below never actually falls through on a live path -- it
        // exists only so a future refactor that broke that invariant fails
        // a match instead of panicking on an `.unwrap()`.
        if let Some(BackendError::ToolArgumentsInvalid {
            tool,
            call_id,
            arguments,
            argument_path,
            ..
        }) = &last_terminal_err
        {
            if let Some(patched) =
                try_truncate_report_summary(req.tools, tool, argument_path, arguments)
            {
                if let Some(route) = last_terminal_route.clone() {
                    tracing::warn!(
                        tool = %tool,
                        argument_path = %argument_path,
                        call_id = %call_id,
                        "report summary exceeded its bound after a corrective retry; \
                         truncating and delivering it rather than failing the turn"
                    );
                    return Ok(self.deliver_truncated_report(
                        route,
                        call_id.clone(),
                        tool.clone(),
                        patched,
                        attempt,
                        skipped.clone(),
                        skip_failures.clone(),
                    ));
                }
            }
        }

        // Board item `01M23SDCE6T85Z48CRQ8NBY6PV`: when the failure that
        // exhausted the chain was a malformed tool call (`ToolParse`/
        // `ToolArgumentsInvalid` -- coercion and the bounded model-facing
        // retry both already failed to produce a valid call), surface a
        // typed `RuntimeError::ToolCallRejected` naming the tool and
        // argument (that variant's own `Display`, sourced from `err`'s
        // rendering) rather than the generic `NoCandidate` aggregate --
        // `NoCandidate`'s "no candidate"/`RuntimeError::Routing`'s "routing
        // error:" wrapper are both actively wrong here: a candidate DID
        // answer, with a malformed call, not silence. `considered` is
        // carried through UNCHANGED (every candidate this chain tried,
        // including the one that ultimately failed on its tool call), so a
        // mixed-cause chain (an earlier candidate rate-limited, the last
        // one malformed) still names every candidate, not only the last.
        if let Some(
            err @ (BackendError::ToolParse { .. } | BackendError::ToolArgumentsInvalid { .. }),
        ) = last_terminal_err
        {
            return Err(RuntimeError::ToolCallRejected {
                role: req.role,
                detail: err.to_string(),
                considered,
            });
        }

        Err(RuntimeError::Routing(RoutingError::NoCandidate {
            role: req.role,
            considered,
        }))
    }

    /// Builds the synthetic success `try_truncate_report_summary`'s caller
    /// returns once it fires: a `GenerateResponse` carrying exactly one
    /// `ToolCall` for `report`, arguments already patched to the
    /// truncated, marked summary -- shaped byte-for-byte like a REAL
    /// tool-calling turn (`content: vec![]`, `tool_calls: vec![...]`,
    /// `stop: StopReason::ToolUse`; mirrors `AccumulatingBackend::
    /// next_response`'s own `ScriptedAttempt::ToolCalls` construction in
    /// `conway-runtime/tests/toolparse_recovery.rs`) so every downstream
    /// consumer -- `AgentLoop`'s dispatch, the persisted `Assistant`
    /// record, and every one of the TUI/one-shot/library facades built on
    /// top of it (C-03/GP-05/P-8) -- treats it exactly like an ordinary
    /// successful turn, with no new code path any of them needs to learn
    /// about. No backend was actually called for this "attempt" (`attempts`
    /// is passed through unchanged: however many REAL calls already
    /// happened), so no health `Observation` is recorded either (T-2: this
    /// is a request problem already resolved locally, never an
    /// endpoint-health signal) and `latency` is `Duration::ZERO`.
    #[allow(clippy::too_many_arguments)]
    fn deliver_truncated_report(
        &self,
        route: Route,
        call_id: String,
        tool: ToolName,
        arguments: Value,
        attempts: u8,
        skipped: Vec<(ModelRef, RoutingReason)>,
        skip_failures: Vec<AttemptFailure>,
    ) -> AttemptOutcome {
        let backend = self.backend_for(&route.backend);
        let caps = backend.capabilities(&route.model);
        let max_context_tokens = window_of(&caps);
        let max_context_tokens_source = backend.context_window_source(&route.model);
        let response = GenerateResponse {
            content: vec![],
            tool_calls: vec![ToolCall {
                call_id,
                name: tool,
                arguments,
            }],
            stop: StopReason::ToolUse,
            usage: Usage::default(),
        };
        AttemptOutcome {
            response,
            route: with_admission_failures(route, &skip_failures),
            attempts,
            latency: Duration::ZERO,
            skipped,
            max_context_tokens,
            max_context_tokens_source,
        }
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
                        // Board item `01M23SDCE6T85Z48CRQ8NBY6PV`: makes a
                        // coercion firing durable and visible in a default
                        // run (the operator's own ruling: "a coercion
                        // nobody can count is the one this decision would
                        // have rejected") -- mirrors `Event::
                        // StreamRestarted`/`Event::ModelDecision` exactly.
                        Some(Ok(StreamChunk::ToolArgumentCoerced {
                            tool,
                            call_id,
                            argument_path,
                        })) => {
                            failure.stream_opened = true;
                            self.bus.emit(
                                session,
                                agent,
                                Event::ToolArgumentCoerced {
                                    tool,
                                    call_id,
                                    argument_path,
                                },
                            );
                        }
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
