//! The supervisor: guarantees [`AgentTree::await_result`] always terminates
//! (architecture §7, WI-083's core objective -- the MAST "failure to
//! recognize termination" mitigation). Wraps an already-spawned agent task
//! so a panic, a blown deadline, or an external cancellation each resolve to
//! a terminal `AgentResult`, published through
//! [`AgentTree::publish_result`]'s set-once guarantee.
//!
//! ## The narrow race this module does not close
//!
//! `AgentLoop::finish`/`finish_cancelled` (`agent_loop.rs`, WI-081,
//! committed and out of this item's file scope) already emits exactly one
//! `Event::AgentFinished` on every path the loop reaches under its own
//! power -- including its own graceful cancellation handling, which
//! ordinarily wins the race against this module's synthesis (see
//! `grace_wait` below). This module therefore emits `Event::AgentFinished`
//! itself *only* on its own synthesis paths (a caught panic, or a task still
//! unresponsive after `grace`) -- never for a real result it received
//! directly -- so the common case never double-fires the event.
//!
//! This does not make double-firing impossible: if a task is still running
//! past `grace` (so this module synthesizes and emits), and that task later
//! completes on its own and runs its own terminal machinery, a second
//! `Event::AgentFinished` for the same agent can still reach the bus. The
//! `AgentTree`'s set-once `publish_result` guarantees only one `AgentResult`
//! is ever *observable*; it does not retroactively suppress a bus event
//! already sent. Closing this fully would require `AgentLoop::finish` to
//! consult the tree's resolved flag before emitting, which is a reasonable
//! follow-up once WI-084/085 give real children a way to reach this path,
//! not something addressable from this file alone.

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

        let result = match outcome {
            Outcome::Real(result) => result,
            Outcome::Synthesized(result) => {
                // The task never reached its own terminal machinery (it
                // panicked, or is still running past `grace`): this is the
                // only `Event::AgentFinished` this agent gets from this
                // path. See the module doc for the narrow race this does
                // not close.
                bus.emit(
                    session,
                    agent,
                    Event::AgentFinished {
                        result: result.clone(),
                    },
                );
                result
            }
        };
        let _ = tree.publish_result(agent, result);
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
