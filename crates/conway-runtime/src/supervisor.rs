//! The supervisor: guarantees [`AgentTree::await_result`] always terminates
//! (architecture §7, WI-083's core objective -- the MAST "failure to
//! recognize termination" mitigation). Wraps an already-spawned agent task
//! so a panic, a blown deadline, or an external cancellation each resolve to
//! a terminal `AgentResult`, published through
//! [`AgentTree::publish_result`]'s set-once guarantee.
//!
//! ## The double-`AgentFinished` race, closed on both sides
//!
//! `AgentLoop::finish`/`finish_cancelled` (`agent_loop.rs`, WI-081/085)
//! already gates its own `Event::AgentFinished` emission on winning
//! `AgentTree::publish_result`'s set-once CAS (`tree.rs`, WI-083) before
//! emitting -- see that method's own doc. Before cycle-2 review finding S1,
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

use std::future::pending;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use conway_core::agent::{AgentResult, ResultStatus};
use conway_core::event::Event;
use conway_core::ids::{AgentId, SessionId};
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::events::EventBus;
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
                        // (cycle-1 review S1).
                        task.abort();
                        Outcome::Synthesized(budget_exceeded(agent, session))
                    }
                }
            }
            () = cancel.cancelled() => {
                match tokio::time::timeout(grace, &mut task).await {
                    Ok(joined) => Outcome::from_join(agent, session, joined),
                    Err(_elapsed) => {
                        // See the abort note on the deadline arm above.
                        task.abort();
                        Outcome::Synthesized(cancelled(agent, session, "cancelled"))
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
                    bus.emit(session, agent, Event::AgentFinished { result });
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

fn budget_exceeded(agent: AgentId, session: SessionId) -> AgentResult {
    AgentResult::new(
        agent,
        session,
        ResultStatus::BudgetExceeded {
            limit: "deadline".to_string(),
        },
        "budget exceeded: deadline elapsed".to_string(),
    )
}
