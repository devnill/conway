//! The supervisor: guarantees [`AgentTree::await_result`] always terminates
//! (architecture §7, the core objective -- the MAST "failure to
//! recognize termination" mitigation). Wraps an already-spawned agent task
//! so a panic, a blown deadline, or an external cancellation each resolve to
//! a terminal `AgentResult`, published through
//! [`AgentTree::publish_result`]'s set-once guarantee.
//!
//! ## Board item A5.3: the synthesized-result durability gap
//!
//! Before A5.3, the guarantee above was live-only: `Outcome::Synthesized`
//! published a panicked/grace-timed-out task's result to the in-memory
//! `AgentTree` (so a same-process `conway_await`/one-shot render loop saw
//! it immediately), but never wrote it to the session's own persisted log
//! at all -- only `AgentLoop::finish` (a method the synthesizing task, by
//! definition, never reached) ever appended a `LogRecord::
//! AgentResultRecord`. A session whose owning task panicked or was
//! `abort()`'d past `grace` would therefore end its ON-DISK transcript mid
//! turn, with no terminal record, even though the live process had already
//! moved on with a real, in-memory result -- a durable-storage version of
//! exactly the defect A5.3 exists to close. `SuperviseArgs::store` and the
//! `persist_agent_result` call on the `Synthesized` branch (below) close
//! it: this module is now, alongside `AgentLoop::finish`, one of the ONLY
//! TWO call sites that ever append that record (see `crate::result::
//! persist_agent_result`'s own doc), and both gate the append on the
//! identical `publish_result` CAS `won`/`is_first` already arbitrates --
//! see that persist call site's own comment for why an unconditional
//! append on both sides of the race would double-write the log.
//!
//! ## The double-`AgentFinished` race, closed on both sides
//!
//! `AgentLoop::finish`/`finish_cancelled` (`agent_loop.rs`)
//! already gates its own `Event::AgentFinished` emission on winning
//! `AgentTree::publish_result`'s set-once CAS (`tree.rs`) before
//! emitting -- see that method's own doc. Before An earlier review found: finding S1,
//! this module's `Outcome::Synthesized` branch (a caught panic, or a task
//! still unresponsive after `grace`) emitted `Event::AgentFinished`
//! unconditionally, without ever checking whether it had actually won that
//! same CAS -- so the race was only half-closed: `task.abort()` (used on
//! the grace-timeout path, below) is cooperative, and an aborted task can
//! keep running and reach its own `finish()` after this module has already
//! given up on joining it and moved on to synthesizing its own result,
//! legitimately winning `publish_result`'s CAS in that gap. This module now
//! calls `tree.publish_result` first on every path -- on the `Real` branch
//! it is a harmless idempotent no-op (the task's own `finish()` already
//! published) -- and emits `Event::AgentFinished` on the `Synthesized`
//! branch only if THIS call is the one that actually published. Because
//! both sides now gate their emission on the identical set-once CAS, at
//! most one `Event::AgentFinished` is ever observable for a given agent,
//! regardless of which side wins. See `tests/supervisor.rs`'s
//! `concurrent_task_completion_and_grace_synthesis_never_double_emit_agent_finished`
//! for regression coverage, and that test's own doc for why the exact
//! winning interleaving cannot be forced deterministically from outside
//! tokio's scheduler.
//!
//! ## `child_reported` rides the identical gate
//!
//! `AgentLoop::finish`'s own `is_first`-gated dispatch of `child_reported`
//! covers a normal completion, INCLUDING a client-observed cancellation
//! (`finish_cancelled` routes through the same `finish`). It does NOT cover
//! a task this module itself had to synthesize a result for -- a caught
//! panic, or a task still unresponsive past `grace` and `abort()`'d -- since
//! that task never reached its own `finish()` through this path. The
//! `Synthesized` branch below dispatches the same event under the identical
//! `won` gate `Event::AgentFinished` already uses, so a hook subscribed to
//! `child_reported` sees every terminal result that crosses back to a
//! parent, exactly once, regardless of which side of the race produced it.

use std::future::pending;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use conway_core::agent::{AgentResult, ResultStatus};
use conway_core::event::Event;
use conway_core::ids::{AgentId, SessionId};
use conway_core::ports::SessionStore;
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::events::EventBus;
use crate::hook_dispatch::{HookDispatcher, CHILD_REPORTED};
use crate::tree::AgentTree;

/// Grace window given to a task that is mid-shutdown (deadline elapsed or
/// externally cancelled) to publish its own real result -- which carries
/// real `usage`/`steps_taken` -- before this module synthesizes one.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(2);

/// Everything [`supervise`] needs to wrap one already-spawned agent task.
pub struct SuperviseArgs {
    pub tree: Arc<AgentTree>,
    pub bus: Arc<EventBus>,
    pub agent: AgentId,
    pub session: SessionId,
    pub cancel: CancellationToken,
    pub deadline: Option<DateTime<Utc>>,
    pub grace: Duration,
    pub task: JoinHandle<AgentResult>,
    /// Board item A5.3: the SAME `Arc<dyn SessionStore>` `AgentLoop`'s own
    /// `finish` persists through (`self.deps.store`) -- this module's
    /// `Outcome::Synthesized` branch needs it for the identical reason:
    /// before A5.3, a panicked or grace-timed-out task's synthesized result
    /// was published to the LIVE `AgentTree` (so a same-process
    /// `conway_await`/one-shot render loop saw it) but never durably
    /// written to the session's own log at all -- `AgentLoop::finish`'s own
    /// persist call is a method on the task's own `AgentLoop`, and a task
    /// this module had to synthesize a result FOR never reaches that
    /// method. Both `Runtime::launch_agent` and `Runtime::start_root` pass
    /// `self.store.clone()`, the same store `AgentLoopDeps.store` already
    /// names.
    pub store: Arc<dyn SessionStore>,
    /// The SAME `HookDispatcher`
    /// `Runtime`/`ToolRunner` share for every other observation event
    /// (`crate::runtime::Runtime`'s own `hooks` field doc) -- this module's
    /// only use of it is `child_reported`, dispatched on the `Synthesized`
    /// branch below. Both `Runtime::launch_agent` and `Runtime::start_root`
    /// pass `self.hooks.clone()`, so an embedder who never wires a runner
    /// leaves this a no-op, unchanged from every other dispatch site.
    pub hooks: Arc<HookDispatcher>,
    /// This agent's parent, if any -- `None` for a root
    /// (`crate::runtime::Runtime::start_root`'s own `SuperviseArgs`
    /// construction). `child_reported`'s own doc: "never fires for a root's
    /// own finish", so this module skips dispatching entirely when `None`,
    /// mirroring `AgentLoop::finish`'s identical `self.parent` check.
    pub parent: Option<AgentId>,
}

/// Whether the produced `AgentResult` came from the task itself (preferred:
/// it carries real usage/steps) or was synthesized by this module because
/// the task panicked or did not respond within `grace`.
enum Outcome {
    Real(AgentResult),
    Synthesized(AgentResult),
}

impl Outcome {
    fn from_join(
        agent: AgentId,
        session: SessionId,
        joined: Result<AgentResult, JoinError>,
    ) -> Self {
        match joined {
            Ok(result) => Outcome::Real(result),
            Err(err) if err.is_panic() => Outcome::Synthesized(panicked(agent, session, &err)),
            Err(_) => Outcome::Synthesized(cancelled(agent, session, "task aborted")),
        }
    }
}

/// Spawns the supervising wrapper around `args.task`. Always publishes a
/// result to `args.tree` for `args.agent` before this returned task ends --
/// there is no path through this function's spawned task that leaves an
/// awaiter of `AgentTree::await_result` hanging.
pub fn supervise(args: SuperviseArgs) -> JoinHandle<()> {
    let SuperviseArgs {
        tree,
        bus,
        agent,
        session,
        cancel,
        deadline,
        grace,
        mut task,
        hooks,
        parent,
        store,
    } = args;

    tokio::spawn(async move {
        let outcome = tokio::select! {
            biased;
            joined = &mut task => Outcome::from_join(agent, session, joined),
            () = deadline_sleep(deadline) => {
                cancel.cancel();
                match tokio::time::timeout(grace, &mut task).await {
                    Ok(joined) => Outcome::from_join(agent, session, joined),
                    Err(_elapsed) => {
                        // Abort the orphan: dropping the handle would leave
                        // the task running unsupervised, free to emit a
                        // second AgentFinished when it eventually completes
                        // (an earlier review finding).
                        task.abort();
                        // Board item A5.6: `abort()`'d WHILE `grace` elapsed
                        // with no response means the task never got to run
                        // its own `finish_cancelled` (which would have read
                        // this same tree slot itself) -- this is the one
                        // path that synthesizes a result with no
                        // `AgentLoop` left alive to consult at all, so the
                        // in-flight read happens here instead. Read AFTER
                        // `abort()`, not before: `mark_tools_finished` is
                        // never reached by an aborted task, so the window
                        // for staleness is "was this task genuinely still
                        // mid-batch", never "did we read too early".
                        let in_flight = tree.in_flight_tools(agent);
                        Outcome::Synthesized(budget_exceeded(agent, session, deadline, &in_flight))
                    }
                }
            }
            () = cancel.cancelled() => {
                match tokio::time::timeout(grace, &mut task).await {
                    Ok(joined) => Outcome::from_join(agent, session, joined),
                    Err(_elapsed) => {
                        // See the abort note on the deadline arm above.
                        task.abort();
                        // this branch
                        // only fires when the task fails to unwind within
                        // `grace` and this module synthesizes a result
                        // instead of the task's own `finish_cancelled`
                        // publishing one (the common case, which already
                        // reads `AgentTree::cancel_reason` itself) -- but a
                        // synthesized result still needs the SAME reason,
                        // not a generic placeholder, since it is otherwise
                        // indistinguishable from a real one to the caller.
                        // `tree.cancel_reason` is the identical lookup
                        // `AgentLoop::finish_cancelled` uses; `None` (an
                        // ancestor's cancel, not this agent's own) falls
                        // back to the same pre-existing literal.
                        let reason = tree
                            .cancel_reason(agent)
                            .unwrap_or_else(|| "cancelled".to_string());
                        Outcome::Synthesized(cancelled(agent, session, &reason))
                    }
                }
            }
        };

        match outcome {
            Outcome::Real(result) => {
                // `AgentLoop::finish` (or `finish_cancelled`/`finish_error`)
                // already published this result and gated its own emission
                // on winning that publish -- see its doc. This call is
                // idempotent bookkeeping: `Ok(false)` (already published) is
                // the expected outcome for a real `AgentLoop`; a bare mock
                // task in a test that never calls `publish_result` itself
                // (e.g. `tests/supervisor.rs`'s panic/deadline/cancel tests)
                // makes this the first -- and only -- publisher instead,
                // which is also correct.
                let _ = tree.publish_result(agent, result);
            }
            Outcome::Synthesized(result) => {
                // The task never reached its own terminal machinery through
                // THIS path (it panicked, or is still running past `grace`
                // and was `abort()`'d -- cooperative, so it may complete on
                // its own and legitimately win the race below). Emit
                // `Event::AgentFinished` only if this call is the one that
                // actually published -- see the module doc.
                let won = tree.publish_result(agent, result.clone()).unwrap_or(true);
                if won {
                    // Board item A5.3: persist a durable copy of this
                    // synthesized result, gated on the identical `won` CAS
                    // `AgentLoop::finish` itself now gates its own persist
                    // call on (see that method's own comment for why an
                    // unconditional persist on both sides of this exact
                    // race would double-write the session's log). Routes
                    // through the SAME `persist_agent_result` function
                    // `finish` calls -- see that function's own doc for why
                    // this is the "single implementation" this item's
                    // spec asks for, not a second hand-rolled copy.
                    // Best-effort: a lost durable copy of an
                    // already-published result is a logging/observability
                    // gap, never a reason to withhold the result from a
                    // live awaiter that already has it via `publish_result`
                    // above.
                    if let Err(err) =
                        crate::result::persist_agent_result(store.as_ref(), session, &result).await
                    {
                        tracing::error!(
                            agent = %agent,
                            error = %err,
                            "failed to persist a supervisor-synthesized terminal AgentResult"
                        );
                    }
                    let ephemeral = tree.ephemeral_of(agent);
                    // See `agent_loop.rs`'s `finish` (the other emission
                    // site) for why this read, and `emit_pruning`'s own
                    // doc, add no new contention.
                    let prune = tree.is_prunable_on_finish(agent);
                    bus.emit_pruning(
                        session,
                        agent,
                        Event::AgentFinished {
                            result: result.clone(),
                            ephemeral,
                        },
                        prune,
                    );
                    // `child_reported`
                    // -- see this module's own doc ("`child_reported` rides
                    // the identical gate") for why this is the ONE place a
                    // synthesized terminal result needs its own dispatch,
                    // separate from `AgentLoop::finish`'s.
                    if let Some(parent) = &parent {
                        if hooks.will_dispatch(CHILD_REPORTED) {
                            hooks
                                .dispatch(
                                    CHILD_REPORTED,
                                    serde_json::json!({
                                        "agent_id": agent,
                                        "parent": parent,
                                        "session": session,
                                        "result": result,
                                    }),
                                )
                                .await;
                        }
                    }
                }
            }
        }
    })
}

async fn deadline_sleep(deadline: Option<DateTime<Utc>>) {
    match deadline {
        Some(dl) => {
            let remaining = (dl - Utc::now()).to_std().unwrap_or(Duration::ZERO);
            tokio::time::sleep(remaining).await;
        }
        None => pending::<()>().await,
    }
}

fn panicked(agent: AgentId, session: SessionId, err: &JoinError) -> AgentResult {
    let detail = format!("agent task panicked: {err}");
    AgentResult::new(
        agent,
        session,
        ResultStatus::Failed {
            error: detail.clone(),
        },
        detail,
    )
}

/// **Deliberately does NOT append [`crate::result::interrupted_call_note`]**
/// -- unlike [`budget_exceeded`], below. Board item A5.6 scopes "the
/// synthesized result names the interrupted call" to a BUDGET-triggered
/// termination specifically (its own acceptance criteria; see
/// `AgentLoop::finish_cancelled`'s identical narrowing, in its own doc, for
/// the fuller reasoning and the regression that established it: an
/// ordinary hard cancel with a tool in flight has an existing, deliberate
/// test asserting its summary is UNCHANGED by this item --
/// `tests/steering.rs`'s `hard_cancel_reports_the_last_assistant_text_not_
/// a_bare_status_name`). This function's ONE caller that can reach a
/// budget-adjacent cancellation (the `cancel.cancelled()` arm below) is
/// itself scoped to an EXTERNAL cancel, never a deadline -- the deadline
/// arm calls `budget_exceeded`, not this fn -- so there is no case here
/// this item's own acceptance criteria actually cover.
fn cancelled(agent: AgentId, session: SessionId, reason: &str) -> AgentResult {
    AgentResult::new(
        agent,
        session,
        ResultStatus::Cancelled {
            reason: reason.to_string(),
        },
        format!("cancelled: {reason}"),
    )
}

/// `deadline`: the SAME `Option<DateTime<Utc>>` `SuperviseArgs::deadline`
/// carried in -- always `Some` at this fn's one call site (only reached
/// after `deadline_sleep(deadline)` itself resolved, which never resolves
/// for `None`, see that fn's own body), but threaded as the real `Option`
/// rather than re-asserted, so this function's own signature does not lie
/// about what it actually needs. Named with the real elapsed value (not
/// the bare literal `"deadline"` this fn used before A5.6) so a child's own
/// terminal result -- read by a parent, an operator, or a log -- says WHEN,
/// matching `ResultStatus::BudgetExceeded::limit`'s format everywhere else
/// it is produced (`AgentLoop::check_budget`'s own `format!("deadline={
/// deadline}")`). `in_flight`: see `cancelled`'s own doc immediately above.
fn budget_exceeded(
    agent: AgentId,
    session: SessionId,
    deadline: Option<DateTime<Utc>>,
    in_flight: &[crate::tree::InFlightCall],
) -> AgentResult {
    let limit = match deadline {
        Some(dl) => format!("deadline={dl}"),
        None => "deadline".to_string(),
    };
    let mut detail = "budget exceeded: deadline elapsed".to_string();
    if let Some(note) = crate::result::interrupted_call_note(in_flight) {
        detail = format!("{detail}\n\n{note}");
    }
    AgentResult::new(
        agent,
        session,
        ResultStatus::BudgetExceeded { limit },
        detail,
    )
}
