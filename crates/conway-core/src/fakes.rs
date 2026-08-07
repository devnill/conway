//! In-crate test doubles for every port trait, gated behind `feature =
//! "fakes"`.
//!
//! These are the only trait implementations `conway-core` is permitted to
//! contain (every other implementation lives in a dedicated crate). They
//! exist so `conway-runtime` (and any other consumer) can be developed and
//! tested end-to-end with zero network and zero filesystem access (GP-04).
//!
//! All state is `std::sync::{Mutex, RwLock}` — no tokio, no async runtime
//! primitives. `#[async_trait]` methods do their work synchronously inside
//! the async fn body and never actually await anything, so they resolve on
//! first poll.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, RwLock};

use async_trait::async_trait;
use chrono::Utc;

use crate::agent::{
    AgentResult, AgentTreeSnapshot, AskOutcome, CancelMode, PermissionDecision, PermissionRequest,
    ResultStatus, SubagentSpec,
};
use crate::capabilities::{
    CacheMode, Capabilities, ProbeReport, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use crate::content::{ContentBlock, Role, SamplingParams, StopReason, Usage};
use crate::error::{BackendError, RoutingError, RuntimeError, StoreError};
use crate::event::Event;
use crate::ids::{
    AgentId, BackendId, EndpointId, LogSeq, ModelId, ModelRef, RoleAlias, SeqRange, SessionId,
};
use crate::log::{ForkOrigin, LogRecord, SessionFilter, SessionMeta, SubagentMode};
use crate::ports::{
    Backend, BoxStream, EventSink, GenerateRequest, GenerateResponse, HealthRegistry, LiveOwner,
    PermissionGate, Router, SessionStore, StreamChunk, SubagentHost,
};
use crate::routing::{BreakerState, Observation, Route, RouteRequest, RoutingReason};

// ---------------------------------------------------------------------
// Backend fakes
// ---------------------------------------------------------------------

fn default_capabilities() -> Capabilities {
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

fn default_generate_response() -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage::default(),
    }
}

/// Decomposes a `GenerateResponse`'s text/thinking content into stream
/// chunks, followed by exactly one `Done(response)`.
fn decompose_to_chunks(response: GenerateResponse) -> Vec<StreamChunk> {
    let mut chunks: Vec<StreamChunk> = response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(StreamChunk::TextDelta(text.clone())),
            ContentBlock::Thinking { text, .. } => Some(StreamChunk::ThinkingDelta(text.clone())),
            _ => None,
        })
        .collect();
    chunks.push(StreamChunk::Done(response));
    chunks
}

fn concat_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// A `futures_core::Stream` over a fixed, already-computed sequence of
/// items. Every item is immediately `Poll::Ready`; the fakes never actually
/// wait for anything.
struct VecStream<T> {
    items: VecDeque<T>,
}

impl<T> VecStream<T> {
    fn new(items: Vec<T>) -> Self {
        Self {
            items: items.into(),
        }
    }
}

impl<T: Unpin> futures_core::Stream for VecStream<T> {
    type Item = T;

    fn poll_next(
        self: core::pin::Pin<&mut Self>,
        _cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Option<T>> {
        core::task::Poll::Ready(self.get_mut().items.pop_front())
    }
}

/// A backend that returns a fixed response, echoes the last `User`-role
/// segment, or always fails — the same outcome for every call. See
/// [`ScriptedBackend`] for turn-by-turn scripting.
#[derive(Debug)]
pub struct FakeBackend {
    id: BackendId,
    caps: Capabilities,
    response: GenerateResponse,
    echo: bool,
    fail: Option<BackendError>,
}

impl FakeBackend {
    /// A backend with a fixed id, capabilities, and response.
    pub fn new(id: BackendId, caps: Capabilities, response: GenerateResponse) -> Self {
        Self {
            id,
            caps,
            response,
            echo: false,
            fail: None,
        }
    }

    /// Echoes the concatenated text of the last `User`-role segment back as
    /// a single `ContentBlock::Text`, with `stop: EndTurn` and zeroed usage.
    pub fn echo(id: BackendId) -> Self {
        Self {
            id,
            caps: default_capabilities(),
            response: default_generate_response(),
            echo: true,
            fail: None,
        }
    }

    /// A backend whose `capabilities()` returns exactly `caps`, for
    /// exercising capability/headroom gating in isolation from generation.
    pub fn with_capabilities(caps: Capabilities) -> Self {
        Self {
            id: BackendId::new("fake"),
            caps,
            response: default_generate_response(),
            echo: false,
            fail: None,
        }
    }

    /// A backend that returns `err` for every call — needed to test the
    /// runtime's fallback loop and health recording.
    pub fn failing(err: BackendError) -> Self {
        Self {
            id: BackendId::new("fake"),
            caps: default_capabilities(),
            response: default_generate_response(),
            echo: false,
            fail: Some(err),
        }
    }
}

#[async_trait]
impl Backend for FakeBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn capabilities(&self, _model: &ModelId) -> Capabilities {
        self.caps.clone()
    }

    async fn generate(&self, req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
        if let Some(err) = &self.fail {
            return Err(err.clone());
        }
        if self.echo {
            let text = req
                .segments
                .iter()
                .rev()
                .find(|s| s.role == Role::User)
                .map(|s| concat_text(&s.content))
                .unwrap_or_default();
            return Ok(GenerateResponse {
                content: vec![ContentBlock::Text { text }],
                tool_calls: vec![],
                stop: StopReason::EndTurn,
                usage: Usage::default(),
            });
        }
        Ok(self.response.clone())
    }

    async fn stream(
        &self,
        req: GenerateRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
        let response = self.generate(req).await?;
        Ok(Box::pin(VecStream::new(
            decompose_to_chunks(response).into_iter().map(Ok).collect(),
        )))
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

/// One scripted turn for [`ScriptedBackend`].
#[derive(Clone, Debug)]
pub enum ScriptedTurn {
    Respond(GenerateResponse),
    Fail(BackendError),
    /// Never resolves (`std::future::pending`) — the calling agent stays
    /// mid-turn until the test runtime tears its task down. Deterministic
    /// alternative to a sleep for tests that need an agent held in a
    /// non-terminal state (e.g. pull_in's still-running-child guard);
    /// std-only, so conway-core keeps its no-async-runtime boundary.
    Pending,
}

/// A backend that plays back a fixed script of responses/failures in order,
/// recording every request it receives. Exhausting the script yields
/// `BackendError::BadRequest { detail: "scripted backend exhausted" }`.
#[derive(Debug)]
pub struct ScriptedBackend {
    id: BackendId,
    caps: Capabilities,
    script: Mutex<VecDeque<ScriptedTurn>>,
    calls: Mutex<Vec<GenerateRequest>>,
}

impl ScriptedBackend {
    pub fn new(script: Vec<ScriptedTurn>) -> Self {
        Self {
            id: BackendId::new("scripted"),
            caps: default_capabilities(),
            script: Mutex::new(script.into()),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn with_id(mut self, id: BackendId) -> Self {
        self.id = id;
        self
    }

    pub fn with_capabilities(mut self, caps: Capabilities) -> Self {
        self.caps = caps;
        self
    }

    /// Every request received so far, in call order — so tests can assert
    /// segment ordering (architecture §5.3) and cache-hint placement.
    pub fn calls(&self) -> Vec<GenerateRequest> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl Backend for ScriptedBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn capabilities(&self, _model: &ModelId) -> Capabilities {
        self.caps.clone()
    }

    async fn generate(&self, req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
        self.calls.lock().unwrap().push(req);
        let next = self.script.lock().unwrap().pop_front();
        match next {
            Some(ScriptedTurn::Respond(response)) => Ok(response),
            Some(ScriptedTurn::Fail(err)) => Err(err),
            Some(ScriptedTurn::Pending) => std::future::pending().await,
            None => Err(BackendError::BadRequest {
                detail: "scripted backend exhausted".into(),
            }),
        }
    }

    async fn stream(
        &self,
        req: GenerateRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
        let response = self.generate(req).await?;
        Ok(Box::pin(VecStream::new(
            decompose_to_chunks(response).into_iter().map(Ok).collect(),
        )))
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
// SessionStore fake
// ---------------------------------------------------------------------

#[derive(Debug)]
struct FakeSession {
    meta: SessionMeta,
    records: Vec<LogRecord>,
}

/// A full in-memory [`SessionStore`]. `fork` is O(1) in parent size: it
/// copies zero records, matching the real store's zero-copy fork contract
/// (architecture §5.1, §8).
#[derive(Debug, Default)]
pub struct FakeStore {
    sessions: RwLock<BTreeMap<SessionId, FakeSession>>,
    /// Cross-process liveness marker — the test knob for
    /// `sweep_stale_modal_asks`'s S1 follow-up. `touch_live_owner` sets
    /// `Some(now)`; `live_owner` returns it; `clear_live_owner` sets `None`.
    /// A test injects a STALE owner via [`Self::set_live_owner`] to drive the
    /// sweep's "stale marker → reap" branch without waiting on a clock.
    live_owner: Mutex<Option<LiveOwner>>,
}

impl FakeStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Total record count across every session — for asserting the
    /// zero-copy fork invariant mechanically.
    pub fn total_record_count(&self) -> usize {
        self.sessions
            .read()
            .unwrap()
            .values()
            .map(|s| s.records.len())
            .sum()
    }

    /// Test knob: inject an arbitrary liveness marker (e.g. a STALE
    /// `heartbeat` to drive the sweep's reap branch, or `None` to clear).
    /// Production paths use `touch_live_owner` / `clear_live_owner` via the
    /// `SessionStore` trait, which stamp `heartbeat = now`; this setter
    /// exists because the sweep's staleness decision is time-based and tests
    /// must not sleep.
    pub fn set_live_owner(&self, owner: Option<LiveOwner>) {
        *self.live_owner.lock().unwrap() = owner;
    }
}

/// Parity with `JsonlSessionStore`'s `assign_seq`: the store — not the
/// caller — owns seq assignment on append, so a record carrying a stale or
/// foreign seq (e.g. B4's `Conway::pull_in` appending a child session's
/// records verbatim into the parent's log) is re-sequenced to the session's
/// head, never stored with a gap or a duplicate. Implemented as a serde
/// round-trip for the same reason `assign_seq` is: `LogRecord` has no seq
/// setter, and matching every variant here would duplicate the enum's shape
/// (and silently drift when it grows).
fn reassign_seq(rec: LogRecord, seq: LogSeq) -> LogRecord {
    let mut value = serde_json::to_value(&rec).expect("LogRecord always serializes");
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "seq".to_string(),
            serde_json::to_value(seq).expect("LogSeq always serializes"),
        );
    }
    serde_json::from_value(value)
        .expect("a LogRecord with only its `seq` key replaced always deserializes")
}

#[async_trait]
impl SessionStore for FakeStore {
    async fn create(&self, meta: SessionMeta) -> Result<SessionId, StoreError> {
        let id = meta.id;
        let mut sessions = self.sessions.write().unwrap();
        if sessions.contains_key(&id) {
            return Err(StoreError::AlreadyExists { session: id });
        }
        sessions.insert(
            id,
            FakeSession {
                meta,
                records: Vec::new(),
            },
        );
        Ok(id)
    }

    async fn append(&self, sid: &SessionId, rec: LogRecord) -> Result<LogSeq, StoreError> {
        let mut sessions = self.sessions.write().unwrap();
        let session = sessions
            .get_mut(sid)
            .ok_or(StoreError::NotFound { session: *sid })?;
        let seq = LogSeq(session.records.len() as u64);
        session.records.push(reassign_seq(rec, seq));
        Ok(seq)
    }

    async fn read(&self, sid: &SessionId, range: SeqRange) -> Result<Vec<LogRecord>, StoreError> {
        let sessions = self.sessions.read().unwrap();
        let session = sessions
            .get(sid)
            .ok_or(StoreError::NotFound { session: *sid })?;
        let head = LogSeq(session.records.len() as u64);
        if range.start > head {
            return Err(StoreError::SeqOutOfRange {
                requested: range.start,
                head,
            });
        }
        let start = range.start.0 as usize;
        let end = range
            .end
            .map(|e| e.0 as usize)
            .unwrap_or(session.records.len())
            .min(session.records.len());
        if start >= end {
            return Ok(Vec::new());
        }
        Ok(session.records[start..end].to_vec())
    }

    async fn head(&self, sid: &SessionId) -> Result<LogSeq, StoreError> {
        let sessions = self.sessions.read().unwrap();
        let session = sessions
            .get(sid)
            .ok_or(StoreError::NotFound { session: *sid })?;
        Ok(LogSeq(session.records.len() as u64))
    }

    async fn fork(
        &self,
        parent: &SessionId,
        at: LogSeq,
        mut meta: SessionMeta,
    ) -> Result<SessionId, StoreError> {
        let mode = meta
            .origin
            .as_ref()
            .map(|o| o.mode)
            .unwrap_or(SubagentMode::Fork);
        let mut sessions = self.sessions.write().unwrap();
        if !sessions.contains_key(parent) {
            return Err(StoreError::NotFound { session: *parent });
        }
        let child_id = meta.id;
        if sessions.contains_key(&child_id) {
            return Err(StoreError::AlreadyExists { session: child_id });
        }
        meta.origin = Some(ForkOrigin {
            parent: *parent,
            at_seq: at,
            mode,
        });
        // Zero-copy: the child starts with an empty record vec, never a
        // clone of the parent's records.
        sessions.insert(
            child_id,
            FakeSession {
                meta,
                records: Vec::new(),
            },
        );
        Ok(child_id)
    }

    async fn meta(&self, sid: &SessionId) -> Result<SessionMeta, StoreError> {
        let sessions = self.sessions.read().unwrap();
        sessions
            .get(sid)
            .map(|s| s.meta.clone())
            .ok_or(StoreError::NotFound { session: *sid })
    }

    /// Hides ephemeral children unconditionally -- this method has no
    /// `SessionFilter` parameter to carry an `include_ephemeral` opt-in
    /// (matching `JsonlSessionStore::children`/`SessionIndex::children`; see
    /// that method's doc for why). A caller that needs a parent's ephemeral
    /// children too uses `list(SessionFilter{parent: Some(sid),
    /// include_ephemeral: true, ..})` instead.
    async fn children(&self, sid: &SessionId) -> Result<Vec<SessionId>, StoreError> {
        let sessions = self.sessions.read().unwrap();
        Ok(sessions
            .values()
            .filter(|s| s.meta.origin.as_ref().map(|o| o.parent) == Some(*sid))
            .filter(|s| !s.meta.ephemeral)
            .map(|s| s.meta.id)
            .collect())
    }

    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionMeta>, StoreError> {
        let sessions = self.sessions.read().unwrap();
        let mut result: Vec<SessionMeta> = sessions
            .values()
            .filter(|s| {
                filter
                    .agent_def
                    .as_ref()
                    .is_none_or(|d| s.meta.agent_def.as_deref() == Some(d.as_str()))
            })
            .filter(|s| {
                filter
                    .label
                    .as_ref()
                    .is_none_or(|l| s.meta.labels.contains(l))
            })
            .filter(|s| {
                filter
                    .parent
                    .is_none_or(|p| s.meta.origin.as_ref().map(|o| o.parent) == Some(p))
            })
            .filter(|s| filter.include_ephemeral || !s.meta.ephemeral)
            .map(|s| s.meta.clone())
            .collect();
        if let Some(limit) = filter.limit {
            result.truncate(limit);
        }
        Ok(result)
    }

    /// Enforces the same guard matrix as `JsonlSessionStore::remove` (see
    /// the trait-level doc): ephemeral-only, and ANY children — ephemeral
    /// ones included — block removal.
    async fn remove(&self, sid: &SessionId) -> Result<(), StoreError> {
        let mut sessions = self.sessions.write().unwrap();
        let session = sessions
            .get(sid)
            .ok_or(StoreError::NotFound { session: *sid })?;
        if !session.meta.ephemeral {
            return Err(StoreError::NotRemovable {
                session: *sid,
                reason: "session is not ephemeral (purge is only for ephemeral sessions)".into(),
            });
        }
        let child_count = sessions
            .values()
            .filter(|s| s.meta.origin.as_ref().map(|o| o.parent) == Some(*sid))
            .count();
        if child_count > 0 {
            return Err(StoreError::NotRemovable {
                session: *sid,
                reason: format!("session has {child_count} child session(s)"),
            });
        }
        sessions.remove(sid);
        Ok(())
    }

    /// Enforces the same one-way guard as `JsonlSessionStore::set_ephemeral`
    /// (see the trait-level doc): only a true→false flip on a currently
    /// ephemeral session succeeds.
    async fn set_ephemeral(&self, sid: &SessionId, ephemeral: bool) -> Result<(), StoreError> {
        if ephemeral {
            return Err(StoreError::NotPromotable {
                session: *sid,
                reason: "demotion (ephemeral false -> true) is not supported; promotion is one-way"
                    .into(),
            });
        }
        let mut sessions = self.sessions.write().unwrap();
        let session = sessions
            .get_mut(sid)
            .ok_or(StoreError::NotFound { session: *sid })?;
        if !session.meta.ephemeral {
            return Err(StoreError::NotPromotable {
                session: *sid,
                reason: "session is not ephemeral".into(),
            });
        }
        session.meta.ephemeral = false;
        Ok(())
    }

    // The liveness marker is a plain in-memory cell here — `live_owner`
    // returns whatever a test (or `touch_live_owner`) last set, with no
    // freshness filtering; the sweep owns the threshold. A `FakeStore` never
    // touches the filesystem, so there is no sidecar to corrupt or lose.
    async fn live_owner(&self) -> Result<Option<LiveOwner>, StoreError> {
        Ok(self.live_owner.lock().unwrap().clone())
    }

    async fn touch_live_owner(&self, pid: u32) -> Result<(), StoreError> {
        *self.live_owner.lock().unwrap() = Some(LiveOwner {
            pid,
            heartbeat: Utc::now(),
        });
        Ok(())
    }

    async fn clear_live_owner(&self) -> Result<(), StoreError> {
        *self.live_owner.lock().unwrap() = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------
// PermissionGate fake
// ---------------------------------------------------------------------

/// A `PermissionGate` that returns a fixed decision, optionally recording
/// every request it receives.
#[derive(Debug)]
pub struct FakeGate {
    decision: PermissionDecision,
    requests: Option<Mutex<Vec<PermissionRequest>>>,
}

impl FakeGate {
    /// Always returns `decision`.
    pub fn new(decision: PermissionDecision) -> Self {
        Self {
            decision,
            requests: None,
        }
    }

    /// Records every `PermissionRequest` it receives (retrievable via
    /// [`Self::requests`]) and always returns `AllowOnce`.
    pub fn recording() -> Self {
        Self {
            decision: PermissionDecision::AllowOnce,
            requests: Some(Mutex::new(Vec::new())),
        }
    }

    /// Always denies, with `reason`.
    pub fn deny_all(reason: impl Into<String>) -> Self {
        Self::new(PermissionDecision::Deny {
            reason: reason.into(),
        })
    }

    /// Every request recorded so far. Empty unless constructed via
    /// [`Self::recording`].
    pub fn requests(&self) -> Vec<PermissionRequest> {
        self.requests
            .as_ref()
            .map(|r| r.lock().unwrap().clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl PermissionGate for FakeGate {
    async fn check(&self, req: PermissionRequest) -> PermissionDecision {
        if let Some(requests) = &self.requests {
            requests.lock().unwrap().push(req);
        }
        self.decision.clone()
    }
}

// ---------------------------------------------------------------------
// Router / HealthRegistry fakes
// ---------------------------------------------------------------------

/// A `Router` that returns a fixed candidate list or a fixed error.
#[derive(Debug)]
pub struct FakeRouter {
    routes: Vec<Route>,
    err: Option<RoutingError>,
}

impl FakeRouter {
    pub fn new(routes: Vec<Route>) -> Self {
        Self { routes, err: None }
    }

    /// Always fails with `err`.
    pub fn erroring(err: RoutingError) -> Self {
        Self {
            routes: Vec::new(),
            err: Some(err),
        }
    }

    /// A one-element chain resolving to `model` via
    /// `RoutingReason::AliasPrimary`.
    pub fn single(model: ModelRef) -> Self {
        Self::new(vec![Route {
            backend: model.backend,
            model: model.model,
            params: SamplingParams::default(),
            reason: RoutingReason::AliasPrimary {
                alias: RoleAlias::new("primary"),
            },
        }])
    }

    /// Produces the headroom-aware T-1 rejection: `est_tokens` prompt +
    /// `headroom_tokens` reserved output measured against
    /// `max_context_tokens`, so the runtime's context-rejection path is
    /// testable without a real router. `required_tokens` and
    /// `shortfall_tokens` are computed (saturating), matching
    /// `RequiredCaps::satisfied_by`'s own arithmetic.
    pub fn context_too_large(
        role: RoleAlias,
        model: ModelRef,
        est_tokens: u32,
        headroom_tokens: u32,
        max_context_tokens: u32,
    ) -> Self {
        let required_tokens = est_tokens.saturating_add(headroom_tokens);
        let shortfall_tokens = required_tokens.saturating_sub(max_context_tokens);
        Self::erroring(RoutingError::ContextTooLarge {
            role,
            model,
            est_tokens,
            headroom_tokens,
            required_tokens,
            max_context_tokens,
            shortfall_tokens,
        })
    }
}

impl Router for FakeRouter {
    fn resolve(&self, _req: &RouteRequest) -> Result<Vec<Route>, RoutingError> {
        match &self.err {
            Some(err) => Err(err.clone()),
            None => Ok(self.routes.clone()),
        }
    }
}

/// A `HealthRegistry` that tracks breaker state and observations in memory.
/// Unknown endpoints report `Closed`.
#[derive(Debug, Default)]
pub struct FakeHealth {
    states: RwLock<BTreeMap<EndpointId, BreakerState>>,
    observations: Mutex<Vec<(EndpointId, Observation)>>,
}

impl FakeHealth {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets `ep`'s breaker state directly (test setup helper).
    pub fn set_state(&self, ep: EndpointId, state: BreakerState) {
        self.states.write().unwrap().insert(ep, state);
    }

    /// Every observation recorded so far, in order — so tests can assert
    /// that `BadRequest`/`Auth`/`ContextOverflow` produce **no** observation
    /// (§8).
    pub fn observations(&self) -> Vec<(EndpointId, Observation)> {
        self.observations.lock().unwrap().clone()
    }
}

impl HealthRegistry for FakeHealth {
    fn state(&self, ep: &EndpointId) -> BreakerState {
        self.states
            .read()
            .unwrap()
            .get(ep)
            .cloned()
            .unwrap_or(BreakerState::Closed)
    }

    fn record(&self, ep: &EndpointId, obs: Observation) {
        self.observations.lock().unwrap().push((ep.clone(), obs));
    }
}

// ---------------------------------------------------------------------
// SubagentHost fake
// ---------------------------------------------------------------------

/// An in-memory [`SubagentHost`]: `start` records the spec and returns a
/// fresh id; `await_result` always terminates immediately, returning a
/// preconfigured result or a synthesized `Completed` fallback — never
/// blocking, honoring the §6.4 always-terminates invariant.
#[derive(Debug)]
pub struct FakeSubagentHost {
    started: Mutex<Vec<(AgentId, SubagentSpec)>>,
    results: Mutex<BTreeMap<AgentId, AgentResult>>,
    asks: Mutex<Vec<(AgentId, SubagentSpec)>>,
    ask_outcomes: Mutex<BTreeMap<AgentId, AskOutcome>>,
    tree: RwLock<AgentTreeSnapshot>,
}

impl FakeSubagentHost {
    pub fn new(root: AgentId) -> Self {
        Self {
            started: Mutex::new(Vec::new()),
            results: Mutex::new(BTreeMap::new()),
            asks: Mutex::new(Vec::new()),
            ask_outcomes: Mutex::new(BTreeMap::new()),
            tree: RwLock::new(AgentTreeSnapshot {
                root,
                nodes: Vec::new(),
                at: Utc::now(),
            }),
        }
    }

    /// Preconfigures the result `await_result` returns for `agent`.
    pub fn with_result(self, agent: AgentId, result: AgentResult) -> Self {
        self.results.lock().unwrap().insert(agent, result);
        self
    }

    /// Preconfigures the [`AskOutcome`] `ask` returns when called with
    /// `parent`. Unconfigured parents get the default `AskOutcome { text:
    /// "fake ask reply", usage: Usage::default(), status:
    /// ResultStatus::Completed, transcript_ref: SessionId::new() }`.
    pub fn with_ask_outcome(self, parent: AgentId, outcome: AskOutcome) -> Self {
        self.ask_outcomes.lock().unwrap().insert(parent, outcome);
        self
    }

    /// Every `(agent_id, spec)` pair recorded by `start`, in call order.
    pub fn started(&self) -> Vec<(AgentId, SubagentSpec)> {
        self.started.lock().unwrap().clone()
    }

    /// Every `(agent_id, spec)` pair recorded by `ask`, in call order.
    pub fn asks(&self) -> Vec<(AgentId, SubagentSpec)> {
        self.asks.lock().unwrap().clone()
    }
}

#[async_trait]
impl SubagentHost for FakeSubagentHost {
    // Board item 01KYTP0PGKJ4VCJP5TD39A1WHF added a `caller` parameter to
    // `start`/`ask` (mirroring the trio below); this fake stays a pure
    // recorder/no-op and does not enforce descendancy itself (see this
    // crate's own module doc, item 1) -- `_caller` is accepted (so it
    // type-checks against the real trait) and otherwise ignored.
    async fn start(
        &self,
        _caller: AgentId,
        _parent: AgentId,
        spec: SubagentSpec,
    ) -> Result<AgentId, RuntimeError> {
        let child = AgentId::new();
        self.started.lock().unwrap().push((child, spec));
        Ok(child)
    }

    // Board item 01KYT8TS0EBKJHYNJRF6S88NRH added a `caller` parameter to
    // this trio on the trait; this fake stays a pure recorder/no-op and
    // does not enforce descendancy itself (see this crate's own module
    // doc, item 1: it is explicitly a lightweight "no fork/spawn logic"
    // double) -- `caller` is accepted (so it type-checks against the real
    // trait) and otherwise ignored, exactly as `_target`/`_text` already
    // were.
    async fn steer(
        &self,
        _caller: AgentId,
        _target: AgentId,
        _text: String,
    ) -> Result<(), RuntimeError> {
        Ok(())
    }

    async fn await_result(
        &self,
        _caller: AgentId,
        target: AgentId,
    ) -> Result<AgentResult, RuntimeError> {
        let preconfigured = self.results.lock().unwrap().get(&target).cloned();
        Ok(preconfigured.unwrap_or_else(|| {
            AgentResult::new(target, SessionId::new(), ResultStatus::Completed, "fake")
        }))
    }

    // Board item 01KZDC2222ARKMZKN8ZE4BYHD6 added `mode` to `cancel`; this
    // fake stays a pure recorder/no-op (module doc, item 1) and does not
    // itself distinguish the two modes -- `mode` is accepted (so it
    // type-checks against the real trait) and otherwise ignored, exactly as
    // `_caller`/`_target`/`_reason` already were.
    async fn cancel(
        &self,
        _caller: AgentId,
        _target: AgentId,
        _reason: String,
        _mode: CancelMode,
    ) -> Result<(), RuntimeError> {
        Ok(())
    }

    // `caller` accepted and ignored, same rationale as `start` above --
    // this fake's preconfigured `tree` snapshot is returned unscoped
    // regardless of caller (it is a pure recorder, not a descendancy
    // enforcer).
    fn tree(&self, _caller: AgentId) -> AgentTreeSnapshot {
        self.tree.read().unwrap().clone()
    }

    async fn ask(
        &self,
        _caller: AgentId,
        parent: AgentId,
        spec: SubagentSpec,
    ) -> Result<AskOutcome, RuntimeError> {
        self.asks.lock().unwrap().push((parent, spec));
        let outcome = self.ask_outcomes.lock().unwrap().get(&parent).cloned();
        Ok(outcome.unwrap_or_else(|| AskOutcome {
            text: "fake ask reply".into(),
            usage: Usage::default(),
            status: ResultStatus::Completed,
            transcript_ref: SessionId::new(),
        }))
    }
}

// ---------------------------------------------------------------------
// EventSink fake
// ---------------------------------------------------------------------

/// Collects every emitted `Event` in order, for assertion.
#[derive(Debug, Default)]
pub struct CollectingEventSink {
    events: Mutex<Vec<Event>>,
}

impl CollectingEventSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every event emitted so far, in emission order.
    pub fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
    }

    pub fn find<F: Fn(&Event) -> bool>(&self, f: F) -> Option<Event> {
        self.events.lock().unwrap().iter().find(|e| f(e)).cloned()
    }
}

impl EventSink for CollectingEventSink {
    fn emit(&self, event: Event) {
        self.events.lock().unwrap().push(event);
    }
}
