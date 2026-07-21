//! `AgentLoop`: the per-agent turn state machine (WI-081, architecture §7).
//!
//! Wires `ContextBuilder` -> `Router` -> `AttemptEngine` -> `ToolRunner` ->
//! `SessionStore` into one turn, with budgets and terminal-result
//! construction. No subagent code exists in this item — `drain_inbox` is a
//! no-op hook (WI-085 implements it), inherited context is always `None`
//! (WI-084 supplies it via an injected transcript source), and
//! `LoopDeps::subagents` exists only because `ToolBatchCtx` requires one.
//!
//! ## Reconciliations against the WI-081 amendment's illustrative types
//!
//! The amendment's prose assumes a runtime-local `HeadroomPolicy` (in a
//! `headroom.rs` this item would create) and a `RouteRequest.required.
//! min_context` field carrying `est_tokens + headroom`. Neither exists in
//! the committed workspace:
//! - `HeadroomPolicy` is `conway_routing::config::HeadroomPolicy` (WI-034,
//!   already committed) — reused directly rather than duplicated.
//! - `conway_core::routing::RequiredCaps` has no `min_context: u32` total;
//!   it has `min_context: Option<u32>` (an independent absolute floor,
//!   unrelated to headroom) and `headroom_tokens: u32` (the headroom
//!   value itself). This loop sets `required.headroom_tokens` to the
//!   turn's resolved headroom instead. `DeclarativeRouter` (WI-034)
//!   documents that it never actually reads this field back (it resolves
//!   headroom itself from its own compiled config) — this loop sets it
//!   anyway so a `RouteRequest` is a complete, honest description of what
//!   the turn asked for, and so alternate `Router` implementations that do
//!   honor it see the same value the attempt engine's gate uses.
//! - Intra-loop consistency: `est_tokens` and `headroom` are each resolved
//!   exactly once per turn, into locals, and both `RouteRequest` and
//!   `AttemptRequest` are built from those same two locals. This does NOT
//!   extend to the real `DeclarativeRouter`'s own filter when
//!   `AgentSpec.headroom_override` diverges from the policy value: the
//!   router resolves headroom from its own compiled config and ignores the
//!   request field, so an override is honored only by the attempt engine's
//!   backstop gate (cycle-1 review S1). The divergence fails safe (a
//!   spurious rejection at one gate, never corrupted output); plumbing
//!   per-agent overrides into `DeclarativeRouter` is queued as a follow-up,
//!   and callers must not rely on `headroom_override` affecting routing
//!   decisions until it lands.
//!
//! Event ordering also reconciles the amendment's step-9 prose ("run tools,
//! emit `TurnFinished`") against this item's own binding criterion
//! (`TurnStarted < ModelDecision < TextDelta* < TurnFinished <
//! ToolCallProposed*`): `TurnFinished` is emitted immediately after the
//! assistant record is appended, before any tool call is dispatched. A
//! "turn" is one model generation; tool execution feeds the *next* turn's
//! context, not the current one's completion event.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use conway_core::agent::{AgentResult, Budget, ResultStatus, ToolSelector};
use conway_core::capabilities::{CacheMode, RequiredCaps, ToolCallSupport};
use conway_core::content::{ContentBlock, ToolResult, Usage};
use conway_core::error::{ConwayError, RuntimeError, StoreError};
use conway_core::event::Event;
use conway_core::ids::{AgentId, ModelId, ModelRef, RoleAlias, SeqRange, SessionId};
use conway_core::log::LogRecord;
use conway_core::ports::{PluginConfig, Router, SessionStore, SubagentHost};
use conway_core::routing::RouteRequest;
use conway_core::segment::CacheTtl;
use conway_routing::config::HeadroomPolicy;
use tokio_util::sync::CancellationToken;

use crate::attempt::{AttemptEngine, AttemptRequest};
use crate::context::{ContextBuilder, ContextInput, HeadSegment, SkillFragment, SystemPromptSpec};
use crate::events::EventBus;
use crate::tools::{PluginRegistry, ToolBatchCtx, ToolRunner};

/// The per-agent turn loop's static configuration: everything about *this*
/// agent that does not change turn to turn.
#[derive(Clone, Debug)]
pub struct AgentSpec {
    pub system_prompt: Option<SystemPromptSpec>,
    pub skills: Vec<SkillFragment>,
    /// `None` behaves as [`ToolSelector::All`] (see
    /// [`PluginRegistry::specs`]).
    pub tools: Option<ToolSelector>,
    pub role: RoleAlias,
    pub pin: Option<ModelRef>,
    pub budget: Budget,
    pub cache_mode: CacheMode,
    pub cache_ttl: CacheTtl,
    /// Overrides the resolved headroom for every turn of this agent's run.
    /// Resolution order: `headroom_override` -> `HeadroomPolicy::resolve`.
    pub headroom_override: Option<u32>,
    pub max_parallel_tools: usize,
}

/// Everything an [`AgentLoop`] needs beyond its own identity and spec:
/// store, router, attempt engine, tool dispatch, and the event bus. Shared
/// across every agent task in a runtime (cheap to clone: every field is an
/// `Arc`).
pub struct LoopDeps {
    pub store: Arc<dyn SessionStore>,
    pub router: Arc<dyn Router>,
    pub attempt: Arc<AttemptEngine>,
    pub registry: Arc<PluginRegistry>,
    pub tool_runner: Arc<ToolRunner>,
    /// Handed to every dispatched tool's `ToolCtx` (WI-079). No subagent
    /// implementation exists yet (WI-084); a fake or a not-yet-wired real
    /// host is injected by the caller.
    pub subagents: Arc<dyn SubagentHost>,
    pub plugin_config: Arc<PluginConfig>,
    pub bus: Arc<EventBus>,
    pub builder: Arc<ContextBuilder>,
    pub headroom: Arc<HeadroomPolicy>,
}

/// One agent's turn state machine (architecture §7). `run` drives turns
/// until a terminal `AgentResult` is produced; it never returns early with
/// an error — every failure path is folded into a non-`Completed`
/// `AgentResult`.
pub struct AgentLoop {
    pub agent_id: AgentId,
    pub session: SessionId,
    pub parent: Option<AgentId>,
    /// The root->this-agent chain, including this agent's own id. A root
    /// agent's path is `vec![agent_id]`.
    pub agent_path: Vec<AgentId>,
    pub cwd: PathBuf,
    pub deps: Arc<LoopDeps>,
    pub spec: AgentSpec,
    pub cancel: CancellationToken,
}

/// Per-turn accumulator: turns executed and usage accrued so far. `Copy` so
/// it can be captured into an error tuple without disturbing the loop's own
/// copy (see [`AgentLoop::run_inner`]'s early-return sites).
#[derive(Clone, Copy, Debug, Default)]
struct LoopState {
    turn: u32,
    usage: Usage,
}

/// Early-returns `Err((err.into(), $state))` from the enclosing
/// `Result<AgentResult, (RuntimeError, LoopState)>`-returning fn on a
/// fallible expression's `Err` arm, so every store/router/attempt failure
/// carries the turn state needed to construct a `Failed`/`Cancelled`
/// `AgentResult` without threading it through every call site by hand.
macro_rules! try_rt {
    ($state:expr, $result:expr) => {
        match $result {
            Ok(value) => value,
            Err(err) => return Err((err.into(), $state)),
        }
    };
}

impl AgentLoop {
    /// Drains queued parent messages at a turn boundary. A no-op in this
    /// item (WI-085 implements mailboxes); the call site exists now so
    /// steering lands at a turn boundary by construction once it does.
    fn drain_inbox(&mut self) -> Vec<LogRecord> {
        Vec::new()
    }

    /// Runs turns until a terminal result is produced. Infallible in return
    /// type: every internal failure (store I/O, routing, backend, budget,
    /// cancellation) is folded into a non-`Completed` [`AgentResult`] by
    /// [`Self::finish`]/[`Self::finish_error`] rather than propagated.
    pub async fn run(mut self) -> AgentResult {
        match self.run_inner().await {
            Ok(result) => result,
            Err((err, state)) => self.finish_error(state, err).await,
        }
    }

    async fn run_inner(&mut self) -> Result<AgentResult, (RuntimeError, LoopState)> {
        let mut state = LoopState::default();
        let mut seen_segments = HashSet::new();

        loop {
            let _ = self.drain_inbox();

            if let Some(result) = self.check_budget(state).await {
                return Ok(result);
            }
            if self.cancel.is_cancelled() {
                return Ok(self.finish_cancelled(state).await);
            }

            self.deps.bus.emit(
                self.session,
                self.agent_id,
                Event::TurnStarted { turn: state.turn },
            );

            let all_records = try_rt!(
                state,
                self.deps.store.read(&self.session, SeqRange::full()).await
            );
            let (head, own) = try_rt!(state, split_head(&all_records, self.session));

            let tool_specs = self.deps.registry.specs(self.spec.tools.as_ref());
            let has_tools = !tool_specs.is_empty();
            let model_hint = self
                .spec
                .pin
                .as_ref()
                .map(|pin| pin.model.clone())
                .unwrap_or_else(|| ModelId::new("unrouted"));

            let input = ContextInput {
                agent_id: self.agent_id,
                turn: state.turn,
                model: model_hint,
                cache_mode: self.spec.cache_mode.clone(),
                system_prompt: self.spec.system_prompt.clone(),
                skills: self.spec.skills.clone(),
                tools: tool_specs.clone(),
                inherited: None,
                head,
                own,
                cache_ttl: self.spec.cache_ttl,
            };
            let (segments, report) = try_rt!(state, self.deps.builder.build(&input));

            for entry in &report.segments {
                if seen_segments.insert(entry.segment) {
                    self.deps.bus.emit(
                        self.session,
                        self.agent_id,
                        Event::ContextSegmentAdded {
                            segment: entry.segment,
                            provenance: entry.provenance.clone(),
                            tokens_est: entry.tokens_est,
                        },
                    );
                }
            }

            let est_tokens = report.total_tokens_est;
            let headroom = resolve_headroom(&self.spec, &self.deps.headroom);

            let mut required = RequiredCaps {
                headroom_tokens: headroom,
                ..RequiredCaps::default()
            };
            if has_tools {
                required.tool_calling = Some(ToolCallSupport::NonStreamingOnly);
            }
            let route_req = RouteRequest {
                role: self.spec.role.clone(),
                pin: self.spec.pin.clone(),
                required,
                est_tokens,
                agent_id: self.agent_id,
            };
            let routes = try_rt!(state, self.deps.router.resolve(&route_req));
            let prefix_key = routes
                .first()
                .map(|route| crate::context::prefix_key(&route.model, &segments));

            let attempt_req = AttemptRequest {
                agent_id: self.agent_id,
                session: self.session,
                role: self.spec.role.clone(),
                routes,
                segments: &segments,
                tools: &tool_specs,
                prefix_key,
                est_tokens,
                headroom,
                max_tokens_override: None,
                cancel: self.cancel.clone(),
            };

            let attempt_fut = self.deps.attempt.execute(attempt_req);
            let attempt_result = match self.spec.budget.deadline {
                Some(deadline) => {
                    let remaining = (deadline - Utc::now()).to_std().unwrap_or(Duration::ZERO);
                    tokio::select! {
                        biased;
                        () = tokio::time::sleep(remaining) => {
                            return Ok(self.finish(
                                ResultStatus::BudgetExceeded { limit: format!("deadline={deadline}") },
                                "",
                                state.usage,
                                state.turn,
                            ).await);
                        }
                        res = attempt_fut => res,
                    }
                }
                None => attempt_fut.await,
            };
            let outcome = try_rt!(state, attempt_result);

            let usage = outcome.response.usage;
            let seq = try_rt!(state, self.deps.store.head(&self.session).await);
            let assistant_record = LogRecord::Assistant {
                seq,
                ts: Utc::now(),
                content: outcome.response.content.clone(),
                model: ModelRef {
                    backend: outcome.route.backend.clone(),
                    model: outcome.route.model.clone(),
                },
                route_reason: serde_json::to_value(&outcome.route.reason)
                    .expect("RoutingReason always serializes"),
                usage,
                stop: outcome.response.stop,
            };
            try_rt!(
                state,
                self.deps
                    .store
                    .append(&self.session, assistant_record)
                    .await
            );
            state.usage += usage;

            self.deps.bus.emit(
                self.session,
                self.agent_id,
                Event::TurnFinished {
                    usage,
                    stop: outcome.response.stop,
                },
            );

            if outcome.response.tool_calls.is_empty() {
                let summary = full_text(&outcome.response.content);
                return Ok(self
                    .finish(
                        ResultStatus::Completed,
                        summary,
                        state.usage,
                        state.turn + 1,
                    )
                    .await);
            }

            let batch_ctx = ToolBatchCtx {
                agent_id: self.agent_id,
                agent_path: self.agent_path.clone(),
                session_id: self.session,
                cwd: self.cwd.clone(),
                cancel: self.cancel.clone(),
                subagents: self.deps.subagents.clone(),
                plugin_config: self.deps.plugin_config.clone(),
                max_parallel_tools: self.spec.max_parallel_tools.max(1),
            };
            let outcomes = self
                .deps
                .tool_runner
                .run_batch(&batch_ctx, outcome.response.tool_calls.clone())
                .await;

            if self.cancel.is_cancelled() {
                // The batch's outcomes are dropped here, including any calls
                // that completed real side effects before the cancel fired —
                // their results never reach the session log (cycle-1 review
                // M1; follow-up if audit/replay completeness requires
                // partial-batch persistence).
                return Ok(self.finish_cancelled(state).await);
            }

            for tool_outcome in outcomes {
                let seq = try_rt!(state, self.deps.store.head(&self.session).await);
                let result = ToolResult {
                    call_id: tool_outcome.call_id,
                    tool: tool_outcome.tool,
                    blocks: tool_outcome.blocks,
                    is_error: tool_outcome.is_error,
                    truncated: tool_outcome.truncation,
                };
                try_rt!(
                    state,
                    self.deps
                        .store
                        .append(
                            &self.session,
                            LogRecord::ToolResultRecord {
                                seq,
                                ts: Utc::now(),
                                result,
                            },
                        )
                        .await
                );
            }

            state.turn += 1;
        }
    }

    /// Checks every configured budget dimension at the top of a turn.
    /// Returns `Some(result)` the first exceeded dimension produces;
    /// `max_tool_calls` is not enforced by this item (no criterion requires
    /// it — WI-081's binding budget tests are `max_steps`, `deadline`, and
    /// `max_tokens`).
    async fn check_budget(&self, state: LoopState) -> Option<AgentResult> {
        let budget = &self.spec.budget;
        if state.turn >= budget.max_steps {
            return Some(
                self.finish(
                    ResultStatus::BudgetExceeded {
                        limit: format!("max_steps={}", budget.max_steps),
                    },
                    "",
                    state.usage,
                    state.turn,
                )
                .await,
            );
        }
        if let Some(deadline) = budget.deadline {
            if Utc::now() >= deadline {
                return Some(
                    self.finish(
                        ResultStatus::BudgetExceeded {
                            limit: format!("deadline={deadline}"),
                        },
                        "",
                        state.usage,
                        state.turn,
                    )
                    .await,
                );
            }
        }
        if let Some(max_tokens) = budget.max_tokens {
            let spent = state.usage.input_tokens as u64 + state.usage.output_tokens as u64;
            if spent >= max_tokens as u64 {
                return Some(
                    self.finish(
                        ResultStatus::BudgetExceeded {
                            limit: format!("max_tokens={max_tokens}"),
                        },
                        "",
                        state.usage,
                        state.turn,
                    )
                    .await,
                );
            }
        }
        None
    }

    async fn finish_cancelled(&self, state: LoopState) -> AgentResult {
        self.finish(
            ResultStatus::Cancelled {
                reason: "cancelled".to_string(),
            },
            "",
            state.usage,
            state.turn,
        )
        .await
    }

    /// Converts a bubbled-up `RuntimeError` into a terminal `AgentResult`.
    /// `RuntimeError::Cancelled` maps to `ResultStatus::Cancelled` (no fatal
    /// error event: this is a graceful stop, not a failure); everything
    /// else maps to `ResultStatus::Failed` with exactly one
    /// `Event::Error { fatal: true }`.
    async fn finish_error(&self, state: LoopState, err: RuntimeError) -> AgentResult {
        if let RuntimeError::Cancelled { reason, .. } = err {
            return self
                .finish(
                    ResultStatus::Cancelled { reason },
                    "",
                    state.usage,
                    state.turn,
                )
                .await;
        }
        self.deps.bus.emit(
            self.session,
            self.agent_id,
            Event::Error {
                error: ConwayError::from(err.clone()),
                fatal: true,
            },
        );
        self.finish(
            ResultStatus::Failed {
                error: err.to_string(),
            },
            "",
            state.usage,
            state.turn,
        )
        .await
    }

    /// Builds the terminal `AgentResult`, persists it (best-effort — a
    /// store failure here is logged, never propagated, since `finish` must
    /// always produce a value), and emits `AgentFinished`.
    async fn finish(
        &self,
        status: ResultStatus,
        summary: impl Into<String>,
        usage: Usage,
        steps_taken: u32,
    ) -> AgentResult {
        let mut result = AgentResult::new(self.agent_id, self.session, status, summary);
        result.usage = usage;
        result.steps_taken = steps_taken;

        match self.deps.store.head(&self.session).await {
            Ok(seq) => {
                let record = LogRecord::AgentResultRecord {
                    seq,
                    ts: Utc::now(),
                    result: result.clone(),
                };
                if let Err(err) = self.deps.store.append(&self.session, record).await {
                    tracing::error!(
                        agent = %self.agent_id,
                        error = %err,
                        "failed to persist terminal AgentResult"
                    );
                }
            }
            Err(err) => {
                tracing::error!(
                    agent = %self.agent_id,
                    error = %err,
                    "failed to read session head while persisting terminal AgentResult"
                );
            }
        }

        self.deps.bus.emit(
            self.session,
            self.agent_id,
            Event::AgentFinished {
                result: result.clone(),
            },
        );
        result
    }
}

/// `spec.headroom_override` if set, else the policy's resolution for the
/// agent's role. Resolved exactly once per turn by the caller, into a
/// local reused for both the `RouteRequest` and the `AttemptRequest` — see
/// the module doc's reconciliation note.
fn resolve_headroom(spec: &AgentSpec, policy: &HeadroomPolicy) -> u32 {
    spec.headroom_override
        .unwrap_or_else(|| policy.resolve(&spec.role))
}

/// Splits a session's full record list into the fixed head segment (the
/// session's own record 0: a fork directive or the initial prompt) and the
/// volatile `own` records that follow it (architecture §5.3). A session
/// with no records, or whose first record is neither, is a caller
/// precondition violation — `Runtime::start_root`/`prompt` (WI-082) always
/// append the head record before an `AgentLoop` task is spawned.
fn split_head(
    records: &[LogRecord],
    session: SessionId,
) -> Result<(HeadSegment, std::sync::Arc<[LogRecord]>), RuntimeError> {
    match records.first() {
        Some(LogRecord::UserTurn { text, .. }) => Ok((
            HeadSegment::Prompt { text: text.clone() },
            std::sync::Arc::from(&records[1..]),
        )),
        Some(LogRecord::ForkDirective { text, by, .. }) => Ok((
            HeadSegment::ForkDirective {
                text: text.clone(),
                by: *by,
            },
            std::sync::Arc::from(&records[1..]),
        )),
        Some(other) => Err(RuntimeError::Store(StoreError::Corrupt {
            session,
            line: 0,
            detail: format!(
                "expected the session's first record to be a user_turn or fork_directive, found {}",
                other.kind_str()
            ),
        })),
        None => Err(RuntimeError::Store(StoreError::Corrupt {
            session,
            line: 0,
            detail: "session has no records to build context from".to_string(),
        })),
    }
}

/// Concatenates every `ContentBlock::Text` in `blocks`, in order — the
/// `Completed` terminal summary source (trailing assistant text).
fn full_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}
