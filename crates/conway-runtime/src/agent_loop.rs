//! `AgentLoop`: the per-agent turn state machine (WI-081, architecture §7).
//!
//! Wires `ContextBuilder` -> `Router` -> `AttemptEngine` -> `ToolRunner` ->
//! `SessionStore` into one turn, with budgets and terminal-result
//! construction. `LoopDeps::subagents` is the real `Runtime` (WI-084,
//! `subagent.rs`), not a stub -- `ToolBatchCtx` gets a working host.
//!
//! ## WI-085: mailboxes and steering
//!
//! `drain_inbox` (previously a documented no-op hook) now really drains
//! this agent's inbox at every turn boundary and classifies what it finds
//! (`crate::mailbox::classify`) -- see that function's doc and
//! `drain_inbox`'s own doc for the turn-boundary and "no injection outside
//! drain_inbox" guarantees this buys. `AgentLoop` gained three fields:
//! `inbox` (this agent's own `MailboxReceiver`), `parent_mailbox` (used to
//! deliver this agent's terminal `Result` upward on `finish`), and
//! `pending_cancel` (turn-local bookkeeping a drained soft `Cancel`
//! resolves into). A drained `Result` is classified but drives no
//! drain-time action -- cycle-2 review (F-085 S2) removed the never-
//! populated `pending_subagent` map this used to resolve; see
//! `drain_inbox`'s own doc and `mailbox.rs`'s module doc for why
//! `AgentTree::await_result` (WI-083) is the real, and only, resolution
//! path. `LoopDeps` gained `tree`, used both to close the carried
//! F-083-1/F-084-1 double-`AgentFinished` race in `finish` (this file's
//! half of a two-sided fix -- see `supervisor.rs`'s module doc for the
//! other half) -- see `finish`'s own doc.
//!
//! ## WI-084: `inherited` context
//!
//! `AgentLoop` gained one field this item: `inherited: Option<InheritedPrefix>`.
//! For a root agent or a spawned child it is `None` and every turn's
//! `ContextInput::inherited` stays `None`, exactly as before. For a fork
//! child, `subagent.rs`'s `SubagentHost::start` resolves it exactly once
//! (via `conway_session::TranscriptResolver`, at fork time, before any of
//! the child's own records exist) and this loop simply clones it into
//! every turn's `ContextInput` unchanged -- see the field's own doc for why
//! no turn-boundary re-resolution is needed or correct.
//!
//! `AgentSpec::report_slot` (WI-082 cycle-1 review, F-082 C1) is this item's
//! one additive hook for a live caller: after each successful
//! `ContextBuilder::build`, and before that turn's backend call, the loop
//! pushes a clone of the just-built `ContextReport` into the slot if the
//! caller supplied one. This is the only channel through which a turn's
//! report reaches outside the loop — no event-bus reconstruction is
//! involved.
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
//!
//! ## WI-086: `AgentResult` construction and repeated-step detection
//!
//! `finish` no longer builds its `AgentResult` from a raw `summary` string
//! alone: it resolves a [`crate::result::ResultBuilder`] (report-tool
//! precedence over trailing text, non-empty-summary/status-naming
//! fallback) for `summary`/`facts`/`artifacts`/`structured` on every
//! terminal path. The tool-outcome loop also runs every dispatched call
//! through a [`crate::step_digest::StepDigest`], emitting `Event::RepeatedStep`
//! plus an injected `SystemNote` the instant a `(tool, canonical-args)`
//! digest is seen a 3rd time. Both are locals inside [`Self::run_inner`],
//! not new fields on `AgentLoop`/[`AgentSpec`] -- see `result.rs`'s module
//! doc for why (both structs are constructed via field literals in files
//! outside this item's original scope: `runtime.rs`, `subagent.rs`, and
//! existing tests).
//!
//! `AgentSpec` gained one field this item, `result_contract:
//! Option<schemars::schema::RootSchema>`, carried through from
//! `SubagentSpec::result_contract` by `subagent.rs`'s `SubagentHost::start`
//! (`None` for a root agent -- `runtime.rs`'s `start_root` has no
//! `SubagentSpec` to source one from). Adding it forced one-line, inert
//! `result_contract: None,` additions to `runtime.rs` and the two existing
//! test harnesses (`tests/agent_loop_e2e.rs`, `tests/steering.rs`) that
//! construct `AgentSpec` by field literal -- a file-scope extension the
//! coordinator explicitly authorized (this item's Self-Check) after the
//! initial implementation flagged the conflict rather than silently
//! expanding scope. The natural-completion branch of [`Self::run_inner`]
//! enforces the contract when present: `Ok` proceeds to `Completed`;
//! the first failure appends a `SystemNote { reason:
//! "result_contract_violation" }` and gives the agent one more turn
//! (`contract_retried` flips `true`, a local exactly like `result_builder`/
//! `step_digest`); a second failure is terminal,
//! `ResultStatus::Rejected { missing }`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use conway_core::agent::{AgentMessage, AgentResult, Budget, ResultStatus, ToolSelector};
use conway_core::capabilities::{CacheMode, RequiredCaps, ToolCallSupport};
use conway_core::content::{ContentBlock, ToolResult, Usage};
use conway_core::error::{ConwayError, RuntimeError, StoreError};
use conway_core::event::Event;
use conway_core::ids::{AgentId, ModelId, ModelRef, RoleAlias, SeqRange, SessionId};
use conway_core::log::LogRecord;
use conway_core::ports::{PluginConfig, Router, SessionStore, SubagentHost};
use conway_core::provenance::{ContextReport, Provenance};
use conway_core::routing::RouteRequest;
use conway_core::segment::CacheTtl;
use conway_routing::config::HeadroomPolicy;
use tokio_util::sync::CancellationToken;

use crate::attempt::{AttemptEngine, AttemptRequest};
use crate::context::{
    ContextBuilder, ContextInput, HeadSegment, InheritedPrefix, SkillFragment, SystemPromptSpec,
};
use crate::events::EventBus;
use crate::mailbox::{self, MailboxReceiver, MailboxSender};
use crate::result::{validate_result_contract, ContractOutcome, ResultBuilder};
use crate::step_digest::{StepDigest, DEFAULT_RING_CAPACITY};
use crate::tools::{PluginRegistry, ToolBatchCtx, ToolRunner};
use crate::tree::AgentTree;

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
    /// The live slot `Runtime::context_report` (WI-082) reads from. Pushed
    /// into by this loop after every successful `ContextBuilder::build`,
    /// before the turn's backend call — so a caller reading the slot always
    /// sees the most recently *assembled* context, independent of whether
    /// that turn's attempt has completed yet. `None` in contexts with no
    /// caller listening (e.g. some tests construct an `AgentLoop` directly).
    pub report_slot: Option<Arc<Mutex<Option<ContextReport>>>>,
    /// WI-086: the schema a `structured` result must satisfy, carried
    /// through from `SubagentSpec::result_contract` (`subagent.rs`'s
    /// `SubagentHost::start`) for a fork/spawn child; `None` for a root
    /// agent (`runtime.rs`'s `start_root` has no `SubagentSpec` to source
    /// one from) and for any `AgentSpec` a test constructs directly without
    /// opting in. Enforced once per natural-completion attempt in
    /// `Self::run_inner` -- see this file's module doc.
    pub result_contract: Option<schemars::schema::RootSchema>,
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
    /// The agent tree this agent belongs to. WI-085 carried follow-up
    /// (F-083-1/F-084-1): lets `finish` consult the tree's set-once
    /// publication before emitting `Event::AgentFinished`, closing the
    /// benign double-emit race against the supervisor's own grace-timeout
    /// synthesis -- see `finish`'s own doc and `supervisor.rs`'s module doc
    /// ("the narrow race this module does not close").
    pub tree: Arc<AgentTree>,
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
    /// `Some` for a fork child, resolved exactly once by `subagent.rs`'s
    /// `SubagentHost::start` (WI-084) at fork time via
    /// `conway_session::TranscriptResolver` and never recomputed afterward
    /// -- the parent's prefix at the fork point is immutable by
    /// construction (a later parent append only extends records the fork
    /// already excluded), so there is no turn-boundary event that could
    /// ever change this value. `None` for a root agent or a spawned child
    /// (spawn's context never inherits anything -- architecture §5.2).
    /// Cloned into every turn's `ContextInput::inherited` unchanged; see
    /// this crate's `context::InheritedPrefix` for why `records` stays a
    /// single shared `Arc` (sibling-fork memoization lives in
    /// `conway-session`, not here).
    pub inherited: Option<InheritedPrefix>,
    /// This agent's own inbox (WI-085). Drained exactly once per turn
    /// boundary by [`Self::drain_inbox`] -- never read anywhere else, which
    /// is what makes the turn-boundary landing guarantee hold by
    /// construction.
    pub inbox: MailboxReceiver,
    /// The parent's mailbox sender, used to deliver this agent's terminal
    /// `AgentMessage::Result` upward on `finish` (architecture §3.2: "child
    /// terminates -> AgentResult -> parent mailbox"). `None` for a root
    /// agent (nothing to deliver to).
    pub parent_mailbox: Option<MailboxSender>,
    /// Set by a drained `AgentMessage::Cancel { hard: false, .. }`;
    /// consumed (and cleared) by the top-of-turn cancel check in
    /// [`Self::run_inner`], which is what gives a soft cancel its
    /// turn-boundary semantics. A hard cancel never touches this field --
    /// it trips `cancel` directly at enqueue time instead (see
    /// `mailbox.rs`'s module doc). Every constructor should set this to
    /// `None`; `pub` only because this struct has no constructor function
    /// and is always built via a field literal (matching every other field
    /// here).
    pub pending_cancel: Option<String>,
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
    /// Drains every message queued on this agent's inbox and classifies
    /// each one (`mailbox::classify`, architecture §6.2). A `Steer` is
    /// persisted as `LogRecord::ParentSteer` *before* this call returns
    /// (persist-before-act) and before the next `SessionStore::read` this
    /// turn -- that ordering, plus this being the only site that ever
    /// calls `self.inbox.drain()`, is what makes "no code path injects into
    /// a context outside `drain_inbox`" hold structurally: a steer becomes
    /// visible by first becoming a stored record, read back exactly like
    /// any other own record (`split_head` below), never by this function
    /// handing a segment to anyone directly.
    ///
    /// A soft cancel only sets `self.pending_cancel`, consumed by the
    /// caller immediately after this returns. A hard cancel was already
    /// handled at enqueue time (`MailboxSender::send`) and is a no-op here.
    /// `Progress` is emitted as `Event::AgentProgress` and never persisted.
    /// `Result` is classified but drives no action here -- the real
    /// resolution path for a `conway_subagent` waiter is
    /// `AgentTree::await_result` (WI-083), not this mailbox; see
    /// `mailbox.rs`'s module doc (cycle-2 review F-085 S2).
    ///
    /// ## A mid-batch persist failure does not lose the rest of the batch
    ///
    /// `self.inbox.drain()` atomically empties the queue into one `Vec`
    /// before this loop starts, so every message it processes has already
    /// left the mailbox and cannot be recovered from there. Before cycle-2
    /// review finding M2, a `SessionStore::append` failure on message *k*
    /// early-returned via `?`, silently dropping every already-dequeued
    /// message after it (soft cancels, progress reports, everything) with
    /// no record and no signal. This function now keeps classifying and
    /// applying every remaining message's *non-persist* effect (a soft
    /// cancel still lands, a progress note is still emitted) even after a
    /// persist failure; it stops attempting further `append` calls against
    /// a store that has already failed once this drain (to avoid hammering
    /// it), and surfaces the first error at the end via a `tracing::error`
    /// naming exactly how many queued records could not be persisted,
    /// before returning it -- the agent is terminating either way (this
    /// error propagates through `run_inner`'s `try_rt!` into
    /// `finish_error`), so the caller's own error path is unaffected.
    async fn drain_inbox(&mut self) -> Result<(), RuntimeError> {
        let mut persist_err: Option<RuntimeError> = None;
        let mut lost_records = 0usize;

        for msg in self.inbox.drain() {
            match mailbox::classify(msg) {
                mailbox::DrainEffect::Persist(record) => {
                    if persist_err.is_some() {
                        lost_records += 1;
                        continue;
                    }
                    if let Err(err) = self.deps.store.append(&self.session, record).await {
                        persist_err = Some(err.into());
                        lost_records += 1;
                    }
                }
                mailbox::DrainEffect::SoftCancel { reason } => {
                    self.pending_cancel = Some(reason);
                }
                mailbox::DrainEffect::HardCancelAcknowledged => {}
                mailbox::DrainEffect::Progress { note } => {
                    self.deps
                        .bus
                        .emit(self.session, self.agent_id, Event::AgentProgress { note });
                }
                mailbox::DrainEffect::Result { from, .. } => {
                    tracing::trace!(
                        agent = %self.agent_id,
                        from = %from,
                        "drained AgentMessage::Result: AgentTree::await_result (WI-083) is \
                         the authoritative resolution path, no drain-time action taken"
                    );
                }
                mailbox::DrainEffect::Unknown => {}
            }
        }

        if let Some(err) = persist_err {
            tracing::error!(
                agent = %self.agent_id,
                error = %err,
                lost_records,
                "drain_inbox: SessionStore::append failed; {lost_records} already-dequeued \
                 record(s) could not be persisted -- the agent is terminating"
            );
            return Err(err);
        }
        Ok(())
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
        // WI-086: both are turn-loop-local, not `AgentLoop` fields -- see
        // `result.rs`'s module doc for why (both structs are constructed
        // via field literals in files outside this item's scope).
        let mut result_builder = ResultBuilder::new();
        let mut step_digest = StepDigest::new(DEFAULT_RING_CAPACITY);
        // WI-086 result-contract retry: `true` once this run has already
        // spent its one corrective turn (`self.spec.result_contract`'s
        // "retried exactly once" rule) -- a second failure after this is
        // `true` is terminal (`Rejected`), never another retry.
        let mut contract_retried = false;

        loop {
            try_rt!(state, self.drain_inbox().await);

            if let Some(reason) = self.pending_cancel.take() {
                return Ok(self
                    .finish(
                        ResultStatus::Cancelled { reason },
                        "",
                        state.usage,
                        state.turn,
                        &result_builder,
                    )
                    .await);
            }
            if let Some(result) = self.check_budget(state, &result_builder).await {
                return Ok(result);
            }
            if self.cancel.is_cancelled() {
                return Ok(self.finish_cancelled(state, &result_builder).await);
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
                inherited: self.inherited.clone(),
                head,
                own,
                cache_ttl: self.spec.cache_ttl,
            };
            let (segments, report) = try_rt!(state, self.deps.builder.build(&input));

            if let Some(slot) = &self.spec.report_slot {
                *slot.lock().expect("report slot poisoned") = Some(report.clone());
            }

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
                                &result_builder,
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
            // WI-087: persist the SAME report already pushed to
            // `report_slot` above -- one build, two surfaces (live slot,
            // durable store) -- and only after the assistant record it
            // describes is itself durable, so a report is never persisted
            // for a turn that did not happen.
            try_rt!(
                state,
                crate::context::report::persist(self.deps.store.as_ref(), &self.session, &report)
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

                if let Some(contract) = &self.spec.result_contract {
                    let parts = result_builder.resolve(&summary, &ResultStatus::Completed);
                    match validate_result_contract(
                        parts.structured.as_ref(),
                        contract,
                        contract_retried,
                    ) {
                        ContractOutcome::Ok => {}
                        ContractOutcome::Retry { errors } => {
                            let note_seq =
                                try_rt!(state, self.deps.store.head(&self.session).await);
                            let note_text = format!(
                                "the structured result failed its result_contract: {}",
                                errors.join("; ")
                            );
                            try_rt!(
                                state,
                                self.deps
                                    .store
                                    .append(
                                        &self.session,
                                        LogRecord::SystemNote {
                                            seq: note_seq,
                                            ts: Utc::now(),
                                            text: note_text,
                                            reason: "result_contract_violation".to_string(),
                                            prov: Provenance::SystemNote {
                                                reason: "result_contract_violation".to_string(),
                                            },
                                        },
                                    )
                                    .await
                            );
                            contract_retried = true;
                            state.turn += 1;
                            continue;
                        }
                        ContractOutcome::Rejected { missing } => {
                            return Ok(self
                                .finish(
                                    ResultStatus::Rejected { missing },
                                    summary,
                                    state.usage,
                                    state.turn + 1,
                                    &result_builder,
                                )
                                .await);
                        }
                    }
                }

                return Ok(self
                    .finish(
                        ResultStatus::Completed,
                        summary,
                        state.usage,
                        state.turn + 1,
                        &result_builder,
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
                return Ok(self.finish_cancelled(state, &result_builder).await);
            }

            let calls = outcome.response.tool_calls.clone();
            for (index, tool_outcome) in outcomes.into_iter().enumerate() {
                result_builder.observe_tool_outcome(&tool_outcome.tool, &tool_outcome);

                let seq = try_rt!(state, self.deps.store.head(&self.session).await);
                let result = ToolResult {
                    call_id: tool_outcome.call_id,
                    tool: tool_outcome.tool.clone(),
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

                // WI-086 MAST mitigation: repeated-step detection. `calls`
                // preserves input order (`ToolRunner::run_batch`'s own
                // contract), so `calls[index]` is this outcome's original
                // call and carries the arguments the digest is keyed on.
                if let Some(repeated) =
                    step_digest.observe(&tool_outcome.tool, &calls[index].arguments, seq)
                {
                    self.deps.bus.emit(
                        self.session,
                        self.agent_id,
                        Event::RepeatedStep {
                            tool: repeated.tool.clone(),
                            prior_seq: repeated.prior_seq,
                        },
                    );
                    let note_seq = try_rt!(state, self.deps.store.head(&self.session).await);
                    let note_text = format!(
                        "tool `{}` was called with identical arguments 3 times; see the result at seq {}",
                        repeated.tool, repeated.prior_seq
                    );
                    try_rt!(
                        state,
                        self.deps
                            .store
                            .append(
                                &self.session,
                                LogRecord::SystemNote {
                                    seq: note_seq,
                                    ts: Utc::now(),
                                    text: note_text,
                                    reason: "repeated_step".to_string(),
                                    prov: Provenance::SystemNote {
                                        reason: "repeated_step".to_string(),
                                    },
                                },
                            )
                            .await
                    );
                }
            }

            state.turn += 1;
        }
    }

    /// Checks every configured budget dimension at the top of a turn.
    /// Returns `Some(result)` the first exceeded dimension produces;
    /// `max_tool_calls` is not enforced by this item (no criterion requires
    /// it — WI-081's binding budget tests are `max_steps`, `deadline`, and
    /// `max_tokens`).
    async fn check_budget(&self, state: LoopState, builder: &ResultBuilder) -> Option<AgentResult> {
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
                    builder,
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
                        builder,
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
                        builder,
                    )
                    .await,
                );
            }
        }
        None
    }

    async fn finish_cancelled(&self, state: LoopState, builder: &ResultBuilder) -> AgentResult {
        self.finish(
            ResultStatus::Cancelled {
                reason: "cancelled".to_string(),
            },
            "",
            state.usage,
            state.turn,
            builder,
        )
        .await
    }

    /// Converts a bubbled-up `RuntimeError` into a terminal `AgentResult`.
    /// `RuntimeError::Cancelled` maps to `ResultStatus::Cancelled` (no fatal
    /// error event: this is a graceful stop, not a failure); everything
    /// else maps to `ResultStatus::Failed` with exactly one
    /// `Event::Error { fatal: true }`.
    ///
    /// Only called from [`Self::run`]'s catch of [`Self::run_inner`]'s `Err`
    /// path, after the turn loop's own `ResultBuilder` has already gone out
    /// of scope with it -- this constructs a fresh, empty one instead. That
    /// loses any artifacts/report accumulated in earlier turns of a run
    /// that then hit a late I/O error, which is an accepted trade-off: no
    /// criterion here requires facts/artifacts fidelity on a `Failed`
    /// result, only a non-empty summary (which `ResultBuilder::resolve`'s
    /// status-naming fallback still provides from a fresh builder) and
    /// correct `usage`/`steps_taken`/`transcript_ref`.
    async fn finish_error(&self, state: LoopState, err: RuntimeError) -> AgentResult {
        let builder = ResultBuilder::new();
        if let RuntimeError::Cancelled { reason, .. } = err {
            return self
                .finish(
                    ResultStatus::Cancelled { reason },
                    "",
                    state.usage,
                    state.turn,
                    &builder,
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
            &builder,
        )
        .await
    }

    /// Builds the terminal `AgentResult`, persists it (best-effort — a
    /// store failure here is logged, never propagated, since `finish` must
    /// always produce a value), publishes it to the tree, and -- only if
    /// that publication was the first one for this agent -- emits
    /// `AgentFinished` and delivers it to the parent's mailbox.
    ///
    /// ## Carried follow-up (F-083-1/F-084-1): the tree-publish gate
    ///
    /// `AgentTree::publish_result` is set-once (`tree.rs`, WI-083): its
    /// first caller for a given agent gets `Ok(true)`, every later caller
    /// gets `Ok(false)`. Calling it *here*, before emitting, means this is
    /// the one place a normal completion and the supervisor's own
    /// grace-timeout synthesis (`supervisor.rs`) race for real: whichever
    /// publishes first is the one that emits `Event::AgentFinished` and
    /// delivers the result upward; the loser's local `result` value is
    /// still returned (so `run()`'s caller — ultimately the supervisor's
    /// own `Outcome::from_join` — still sees a real, non-synthesized
    /// result), but produces no second event and no second parent
    /// delivery.
    ///
    /// This is only ONE side of the race's closure — not, as an earlier
    /// revision of this doc claimed, the whole of it. `supervisor.rs`'s
    /// `Outcome::Synthesized` branch (a caught panic, or a task still
    /// unresponsive after `grace` and `abort()`'d) must gate ITS emission
    /// on winning the very same `publish_result` CAS too: `task.abort()` is
    /// cooperative, so an aborted task can keep running past the abort
    /// request and reach this very `finish` method after the supervisor has
    /// already given up on joining it, legitimately winning the CAS in that
    /// gap. Before cycle-2 review finding S1, `supervisor.rs` emitted
    /// unconditionally on that path regardless of whether it had actually
    /// won, so the race was only half-closed even with this gate in place.
    /// See `supervisor.rs`'s own module doc for that side's fix; together
    /// the two gates make at most one `Event::AgentFinished` observable per
    /// agent, from whichever side wins.
    ///
    /// `publish_result`'s only error is `AgentNotFound` (this agent was
    /// never `attach`ed to the tree at all — true of some unit tests that
    /// construct an `AgentLoop` directly without a `Runtime`); that case
    /// defaults to "first" so those tests keep observing `AgentFinished`
    /// exactly as before this item.
    async fn finish(
        &self,
        status: ResultStatus,
        trailing_text: impl Into<String>,
        usage: Usage,
        steps_taken: u32,
        builder: &ResultBuilder,
    ) -> AgentResult {
        // WI-086: precedence between an explicit `report` tool call and
        // trailing assistant text -- and the non-empty-summary /
        // status-naming-fallback guarantee -- are both resolved here, in
        // one place, for every terminal path.
        let parts = builder.resolve(&trailing_text.into(), &status);
        let mut result = AgentResult::new(self.agent_id, self.session, status, parts.summary);
        result.facts = parts.facts;
        result.artifacts = parts.artifacts;
        result.structured = parts.structured;
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

        let is_first = self
            .deps
            .tree
            .publish_result(self.agent_id, result.clone())
            .unwrap_or(true);

        if is_first {
            self.deps.bus.emit(
                self.session,
                self.agent_id,
                Event::AgentFinished {
                    result: result.clone(),
                },
            );
            if let Some(parent_mailbox) = &self.parent_mailbox {
                parent_mailbox.send(AgentMessage::Result {
                    from: self.agent_id,
                    result: result.clone(),
                });
            }
        }
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
