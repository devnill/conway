//! The `/ask` (B5) modal's own async completion: forking an ephemeral
//! child and draining its single turn, off the app loop's own task so a
//! slow/failed answer never blocks input handling. Extracted out of
//! `app.rs` verbatim (original split item, board). [`super::run`]'s own
//! `modal_ask_rx.recv()` arm is the production consumer of
//! [`ModalAskOutcome`].
//!
//! [`App::spawn_modal_ask`] (board item `01KZVZ5XV162XCQR96AQKCCCF7`) is the
//! actual `tokio::spawn` call site: `commands::execute`'s `SlashCommand::
//! Ask` arm cannot spawn this itself (it has no live `SessionHandle` to
//! clone and no `modal_ask_tx` -- see `commands::Effect::RunModalAsk`'s own
//! doc), so it validates and hands back that effect, and `App::submit`'s
//! shared `Effect` match calls this method, mirroring `plugin_cmd::App::
//! spawn_plugin_command`'s own shape exactly.

use conway::SessionHandle;

use super::App;

/// The result of one spawned `/ask` task (B5 -- see [`super::App::submit`]'s
/// `/ask` branch and [`run_modal_ask`]). `child` is the ephemeral fork
/// child's `AgentId` (from `TurnHandle::agent`), the value the modal's
/// three fates need; it is `None` only when `SessionHandle::ask` itself
/// failed (no child was ever attached -- nothing to open a modal over, so
/// the failure becomes a plain transcript `Notice` instead).
pub(super) struct ModalAskOutcome {
    pub(super) question: String,
    pub(super) child: Option<conway::AgentId>,
    pub(super) reply: conway::Result<String>,
}

/// Drives one `/ask` (B5) to completion: `SessionHandle::ask` forks an
/// ephemeral child (attaching it as a proper fork child of the asker --
/// post-B2, so its `AgentSpawned` reaches the `/agents` tree marked
/// `(ephemeral)`) and returns a `TurnHandle` over it (exactly like
/// `SessionHandle::prompt`, but scoped to that throwaway child); `text()`
/// drains it to the finished reply. The child's `AgentId`
/// (`TurnHandle::agent`) rides along in the outcome -- the modal's fates
/// all need it. A free function (not an `App` method) since it owns none
/// of `App`'s state -- it runs inside a `tokio::spawn`ed task that
/// outlives any single `submit` call, so it cannot borrow `self`.
pub(super) async fn run_modal_ask(handle: SessionHandle, question: String) -> ModalAskOutcome {
    match handle.ask(question.clone()).await {
        Ok(turn) => {
            let child = turn.agent();
            let reply = turn.text().await;
            ModalAskOutcome {
                question,
                child: Some(child),
                reply,
            }
        }
        Err(e) => ModalAskOutcome {
            question,
            child: None,
            reply: Err(e),
        },
    }
}

impl App {
    /// Spawns the `/ask` (B5) task off this loop's own `select!`, never on
    /// it -- see this module's own doc and `commands::Effect::
    /// RunModalAsk`'s for why this specific method (not `commands::
    /// execute` itself) is what does the actual `tokio::spawn`.
    /// `commands::execute`'s `SlashCommand::Ask` arm has already validated
    /// `question` and set `state.ask_in_flight` before returning the effect
    /// that reaches this call.
    pub(super) fn spawn_modal_ask(&self, question: String) {
        let handle = self.handle.clone();
        let tx = self.modal_ask_tx.clone();
        tokio::spawn(async move {
            let outcome = run_modal_ask(handle, question).await;
            // The receiver only goes away when `App::run`'s loop has
            // already exited -- nothing left to notify, so a send failure
            // here is silently dropped, mirroring `spawn_plugin_command`'s
            // own send site exactly.
            let _ = tx.send(outcome);
        });
    }
}
