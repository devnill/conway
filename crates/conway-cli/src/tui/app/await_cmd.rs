//! `/await`'s own async completion (INTENT.md §7a parity: the model's
//! `conway_await` tool already blocks a turn until a child finishes and
//! hands back its result -- the operator had no counterpart). Waits for a
//! target agent to reach a terminal state off this loop's own `select!`,
//! then posts a transcript notice -- never awaited inline on `submit`/
//! `execute` (see `commands::Effect::RunAwait`'s own doc for the
//! hang-safety reasoning: unlike an `/ask` child, an awaited agent can be
//! `keep_alive` and run indefinitely). [`App::spawn_await`] (this item,
//! board) is the actual `tokio::spawn` call site, mirroring `ask::App::
//! spawn_modal_ask`'s shape closely -- `commands::execute`'s `SlashCommand::
//! Await` arm cannot spawn this itself (it has no live `SessionHandle` to
//! clone and no `await_tx` -- see `commands::Effect::RunAwait`'s own doc),
//! so it validates, records `agent` in `state.awaiting_agents`, posts the
//! immediate "awaiting..." notice, and hands back that effect; `App::
//! submit`'s shared `Effect` match calls this method.
//!
//! ## Design question: which channel carries the completion notice?
//!
//! **A dedicated channel (`await_tx`/`await_rx`), NOT `modal_ask_tx`, and
//! NOT the `AgentFinished` event stream `App::run` already drains.** Both
//! alternatives were considered and rejected:
//!
//! - **Reusing `modal_ask_tx`** would mean widening `ask::AskUpdate` with a
//!   THIRD case, or racing a new message type through code that is
//!   genuinely about `/ask`'s own single-slot modal fate machinery
//!   (`Mode::AskModal`, `state.ask_in_flight`, three forced fates --
//!   `f`/`p`/`Esc`). An `/await` completion has none of that shape: it
//!   opens no modal, forces no fate, and must land in the transcript
//!   whether or not an ask modal (or a trust preview, or a permission
//!   prompt) happens to be open at the moment it arrives. Coupling it to
//!   `modal_ask_tx` would make its delivery depend on that channel's own
//!   consumer correctly disambiguating an unrelated message shape --
//!   exactly the "lost when the modal state changes" hazard this item's own
//!   design question warns against.
//! - **Watching `Event::AgentFinished` on `self.handle.events()`** (the
//!   stream `App::run`'s `maybe_env = events.next()` arm already drains for
//!   every agent in this session) would avoid a new channel entirely, but
//!   introduces a real loss window this item's acceptance cannot accept:
//!   that stream is RESUBSCRIBED on `/resume` and on a `ForkSession`/
//!   `Checkout`-outcome plugin command (`events = self.handle.events()`,
//!   `run.rs`'s own `Resubscribe`/`apply_plugin_command_done` arms) --
//!   whichever local `events` this loop is CURRENTLY polling is a live
//!   subscription, not a durable log tail, so an `AgentFinished` that fires
//!   in the gap between the old subscription ending and the new one
//!   starting would never reach either one. `SessionHandle::await_agent`
//!   has no such gap: it is a notification wait against the runtime's own
//!   result registry (`Runtime::await_result`), resolved whenever the
//!   result actually lands, independent of which event stream this loop
//!   happens to be subscribed to at that instant -- the same primitive the
//!   model-facing `conway_await` tool already relies on for the identical
//!   "however long it takes, however the operator's own view has since
//!   moved on" guarantee.
//!
//! `await_rx` is drained by `App::run` exactly like `plugin_cmd_rx`/
//! `provider_status_rx` already are: unconditionally, on every iteration,
//! with no `state.mode` check at all -- see [`App::apply_await_done`]'s own
//! doc.

use conway::{AgentId, AgentResult};

use super::App;
use crate::tui::state::Entry;

/// One spawned `/await` task's eventual reply (this module's own doc).
pub(super) struct AwaitDone {
    pub(super) agent: AgentId,
    pub(super) result: conway::Result<AgentResult>,
}

/// Waits for `agent` to reach a terminal state via `SessionHandle::
/// await_agent` -- the SAME facade primitive the model-facing `conway_await`
/// tool's own convenience wrapper reduces to internally (see
/// `commands::Host::await_agent`'s own doc) -- and sends the result back on
/// `tx`. A free function (not an `App` method), mirroring `ask::
/// run_modal_ask`'s own shape: it runs inside a `tokio::spawn`ed task that
/// outlives any single `submit` call, so it cannot borrow `self`.
pub(super) async fn run_await(
    handle: conway::SessionHandle,
    agent: AgentId,
    tx: tokio::sync::mpsc::UnboundedSender<AwaitDone>,
) {
    let result = handle.await_agent(agent).await;
    // The receiver only goes away when `App::run`'s loop has already
    // exited -- nothing left to notify, so a send failure here is silently
    // dropped, mirroring `run_modal_ask`'s own send site exactly.
    let _ = tx.send(AwaitDone { agent, result });
}

impl App {
    /// Spawns the `/await` task off this loop's own `select!`, never on it
    /// -- see this module's own doc and `commands::Effect::RunAwait`'s for
    /// why this specific method (not `commands::execute` itself) is what
    /// does the actual `tokio::spawn`. `commands::execute`'s `SlashCommand::
    /// Await` arm has already validated `agent`, recorded it in `state.
    /// awaiting_agents`, and posted the immediate notice before returning
    /// the effect that reaches this call.
    pub(super) fn spawn_await(&self, agent: AgentId) {
        let handle = self.handle.clone();
        let tx = self.await_tx.clone();
        tokio::spawn(async move {
            run_await(handle, agent, tx).await;
        });
    }

    /// Applies one [`AwaitDone`] reply: removes `agent` from `state.
    /// awaiting_agents` (so a fresh `/await` on it is accepted again) and
    /// posts a transcript notice naming the agent, its terminal status, its
    /// summary text, and how many facts/artifacts it produced -- or, on a
    /// facade error, a plain failure notice. Called from `App::run`'s own
    /// `await_rx.recv()` arm, unconditionally -- see this module's own doc
    /// for why this is never gated on `state.mode`: the notice must reach
    /// the transcript even if the operator has since focused a different
    /// agent or opened some other modal-bearing surface.
    pub(super) fn apply_await_done(&mut self, done: AwaitDone) {
        self.state.awaiting_agents.remove(&done.agent);
        let text = match done.result {
            Ok(result) => {
                let summary = if result.summary.is_empty() {
                    "(no summary)"
                } else {
                    result.summary.as_str()
                };
                format!(
                    "{} finished: {} -- {summary} ({} fact{}, {} artifact{})",
                    done.agent,
                    terminal_status_text(&result.status),
                    result.facts.len(),
                    if result.facts.len() == 1 { "" } else { "s" },
                    result.artifacts.len(),
                    if result.artifacts.len() == 1 { "" } else { "s" },
                )
            }
            Err(e) => format!("await {} failed: {e}", done.agent),
        };
        self.state.transcript.push(Entry::Notice { text });
    }
}

/// A short, human-readable rendering of a terminal `ResultStatus`, mirroring
/// `state::agent_tree`'s own private `terminal_reason` in shape (not reused
/// directly: that function is module-private to `state/agent_tree.rs`, and
/// duplicating this small a match is cheaper than widening its visibility
/// for one more caller in a different module tree). `ResultStatus` is
/// `#[non_exhaustive]`; the wildcard arm is forward compatibility, not a
/// modeled case.
fn terminal_status_text(status: &conway::ResultStatus) -> String {
    match status {
        conway::ResultStatus::Completed => "completed".to_string(),
        conway::ResultStatus::Failed { error } => format!("failed: {error}"),
        conway::ResultStatus::Cancelled { reason } => format!("cancelled: {reason}"),
        conway::ResultStatus::BudgetExceeded { limit } => format!("budget exceeded ({limit})"),
        conway::ResultStatus::Rejected { missing } => format!("rejected: {}", missing.join(", ")),
        _ => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use conway_core::agent::{Fact, ResultStatus};

    use super::super::fixtures::{echo_conway, minimal_cli};
    use super::App;
    use crate::tui::state::Entry;

    /// **End to end, driven the same way `App::run`'s own `await_rx.recv()`
    /// arm would.** `/await <child>` on a running (bare, keep-alive) child
    /// posts the immediate notice synchronously, then -- once the child is
    /// cancelled and its result lands -- the spawned task's reply carries
    /// enough to build a completion notice naming the agent, a terminal
    /// status, and fact/artifact counts.
    #[tokio::test]
    async fn await_end_to_end_posts_the_immediate_notice_then_the_completion_notice() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        // A bare `/spawn` creates a fresh, interactive keep-alive child --
        // exactly the "runs until told otherwise" shape `/await`'s own
        // immediate notice warns about.
        let outcome = app
            .submit("/spawn".to_string())
            .await
            .expect("submit should not error");
        let child = match outcome {
            super::super::SubmitOutcome::FocusNewSession { child, .. } => child,
            _ => panic!("expected a bare /spawn to produce Effect::FocusNewSession"),
        };

        let outcome = app
            .submit(format!("/await {child}"))
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, super::super::SubmitOutcome::Continue));
        assert!(
            app.state.awaiting_agents.contains(&child),
            "the agent must be recorded as awaited immediately"
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text.contains("awaiting") && text.contains(&child.to_string())
            )),
            "the immediate notice must be posted synchronously, at submit time: {:?}",
            app.state.transcript
        );

        // End the child so the spawned wait resolves.
        app.handle
            .cancel(child, "test cleanup")
            .await
            .expect("cancel should be accepted");

        let done = tokio::time::timeout(
            Duration::from_secs(5),
            app.await_rx
                .as_mut()
                .expect("await_rx is set by App::new")
                .recv(),
        )
        .await
        .expect("the spawned wait task must reply promptly")
        .expect("await_tx's sender half is alive for the duration");
        assert_eq!(done.agent, child);

        app.apply_await_done(done);

        assert!(
            !app.state.awaiting_agents.contains(&child),
            "a finished await must be cleared, so a fresh /await on the same agent is accepted \
             again"
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text }
                    if text.contains(&child.to_string()) && text.contains("finished")
            )),
            "the completion notice must reach the transcript: {:?}",
            app.state.transcript
        );
    }

    /// **The immediate notice is unconditional, and does not depend on the
    /// completion notice's own contents.** A direct proof that
    /// `apply_await_done` formats a non-empty summary, a plural fact/
    /// artifact count, and clears `awaiting_agents` -- exercised directly
    /// (no live agent needed) so the formatting itself is pinned
    /// independently of the end-to-end test above.
    #[tokio::test]
    async fn apply_await_done_formats_status_summary_and_counts() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let agent = app.state.focused_agent;
        app.state.awaiting_agents.insert(agent);

        let mut result = conway::AgentResult::new(
            agent,
            conway::SessionId::new(),
            ResultStatus::Completed,
            "did the thing",
        );
        result.facts.push(Fact {
            key: "k".to_string(),
            value: serde_json::json!("v"),
            source: None,
        });
        app.apply_await_done(super::AwaitDone {
            agent,
            result: Ok(result),
        });

        assert!(!app.state.awaiting_agents.contains(&agent));
        let text = app
            .state
            .transcript
            .iter()
            .rev()
            .find_map(|e| match e {
                Entry::Notice { text } => Some(text.clone()),
                _ => None,
            })
            .expect("a notice must have been pushed");
        assert!(text.contains(&agent.to_string()));
        assert!(text.contains("completed"));
        assert!(text.contains("did the thing"));
        assert!(text.contains("1 fact"));
        assert!(text.contains("0 artifacts"));
    }
}
