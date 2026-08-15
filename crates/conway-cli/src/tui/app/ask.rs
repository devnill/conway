//! The `/ask` (B5) modal's own async completion: forking an ephemeral
//! child and draining its single turn, off the app loop's own task so a
//! slow/failed answer never blocks input handling. Extracted out of
//! `app.rs` verbatim (this item, board); the four pre-parser command
//! interceptions -- including `/ask`'s own -- stay in `app.rs` itself (T9's
//! own guard greps that exact file's source text for them; see `app.rs`'s
//! module doc). [`super::run`]'s own `modal_ask_rx.recv()` arm is the
//! production consumer of [`ModalAskOutcome`].

use conway::SessionHandle;

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
