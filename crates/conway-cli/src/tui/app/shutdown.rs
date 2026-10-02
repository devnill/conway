//! `Ctrl-C`/quit handling: the double-press-to-exit window and B5's "no
//! fourth way out" of the `/ask` modal (every quit path purges its live or
//! parked child before the process actually exits). Extracted out of
//! `app.rs` verbatim; [`super::run`]'s own
//! `Action::CtrlC`/`Action::Quit` arms are the production callers.
//!
//! **Board item `01M0RWFH6V709B7WTAFRZGFKG3` widened both paths to cover
//! an ask that is IN FLIGHT (no modal open yet -- the question was asked
//! but no answer has arrived), which the pre-existing "no fourth way out"
//! machinery above never reached at all** (it only ever looked at
//! `Mode::AskModal`/`pending_ask_modal`, both of which are empty during
//! flight -- `AppState::mode`'s own doc). [`App::handle_ctrl_c`]'s first
//! press now also abandons an in-flight ask (`App::abandon_ask`);
//! [`App::purge_open_ask_modal`] now also cancels one and discards any
//! pending prompt on quit -- but deliberately does NOT attempt to `purge`
//! it (see that method's own doc for why attempting to would reproduce the
//! exact `RuntimeError::Store(StoreError::NotRemovable)`/"agent is still
//! running" error).

use std::time::{Duration, Instant};

use super::App;
use crate::exit::ExitCode;
use crate::tui::state::{Entry, Mode};

/// How long a lone `Ctrl-C` remains "armed" -- a second `Ctrl-C` within this
/// window exits 130; after it, a `Ctrl-C` is treated as a fresh first press
/// (module notes: "second within 2 s exits with 130").
const DOUBLE_CTRL_C_WINDOW: Duration = Duration::from_secs(2);

impl App {
    /// First `Ctrl-C`: cancel the running turn (and, board item
    /// `01M0RWFH6V709B7WTAFRZGFKG3`, abandon an in-flight `/ask` if one is
    /// running -- see [`App::abandon_ask`]'s own doc; the two are
    /// independent and both best-effort, so an ask with nothing else
    /// running still gets abandoned, and an ordinary turn with no ask in
    /// flight is unaffected), arm the double-press window. Second `Ctrl-C`
    /// within [`DOUBLE_CTRL_C_WINDOW`]: exit 130.
    pub(super) async fn handle_ctrl_c(
        &mut self,
        last_ctrl_c: &mut Option<Instant>,
    ) -> conway::Result<Option<ExitCode>> {
        // Board item `01M1YVFRPH0DCE8N0DR5BS5BRT` ("Run a shell command
        // yourself"): a `!` command in flight is this press's ONLY target
        // -- it is not an agent turn, so neither the double-press-to-exit
        // window below nor an in-flight `/ask`'s own abandon logic applies
        // to it. `take()`, never a bare read: the sender is one-shot, and
        // taking it here is what makes a SECOND `Ctrl-C` (while
        // `shell_in_flight` is still `true` because the kill is not yet
        // confirmed) fall through to the ordinary agent-cancel path below
        // instead of sending on an already-consumed channel. The actual
        // process-group kill happens inside the spawned task
        // (`tui::app::shell_cmd::execute`'s own `cancel_rx` arm); this
        // send only asks for it -- `App::apply_shell_done` (reached once
        // that task replies) is what actually clears `shell_in_flight` and
        // renders the outcome, exactly like a `!` command finishing on its
        // own.
        if let Some(tx) = self.shell_cancel_tx.take() {
            let _ = tx.send(());
            return Ok(None);
        }
        let now = Instant::now();
        if let Some(prev) = *last_ctrl_c {
            if now.duration_since(prev) <= DOUBLE_CTRL_C_WINDOW {
                // B5: exiting with the /ask modal open purges its child
                // first, exactly like `Action::Quit` (see that arm).
                self.purge_open_ask_modal().await;
                return Ok(Some(ExitCode::Interrupted));
            }
        }
        *last_ctrl_c = Some(now);
        // Review round 2 (board item `01M1YVHKTQVXJRDSRYT3TCRXFX`,
        // SIGNIFICANT finding 2): the double-`Ctrl-C` exit path above
        // deliberately has no extra confirmation gate for a non-empty
        // queue -- round 1's choice to leave it alone was re-confirmed
        // "fine" on re-review, since it mirrors every other
        // double-`Ctrl-C` semantic in this method. But that makes THIS,
        // the single most common escape gesture, the one place a queued
        // message could be lost with no visibility at all. This is pure
        // visibility, not another gate: it fires once, on the first
        // press, and never blocks the second press from exiting. Counts
        // across every agent, the same sum `Self::
        // confirm_quit_with_nonempty_queue` uses for the analogous quit
        // path (not scoped to the focused agent -- a press that would
        // discard ANY agent's queue deserves the warning).
        let pending = self.state.held_prompts.len() + self.state.pending_steers.len();
        if pending > 0 {
            self.state.transcript.push(Entry::Notice {
                text: format!(
                    "{pending} queued message(s) will be discarded if you press Ctrl-C again"
                ),
            });
        }
        // Board item `01M0RWFH6V709B7WTAFRZGFKG3`: a no-op when no ask is
        // in flight (`abandon_ask`'s own guard) -- checked before the
        // root-turn cancel below so an ask abandoned this press still gets
        // its own "ask abandoned -- cleaning up" notice ahead of whatever
        // the root cancel below reports.
        self.abandon_ask().await;
        // Best-effort: a cancel failure (e.g. nothing running) is not fatal
        // to the session -- surfaced as a notice, not a crash.
        if let Err(e) = self.handle.cancel(self.handle.root(), "user cancel").await {
            self.state.transcript.push(Entry::Notice {
                text: format!("cancel failed: {e}"),
            });
        }
        Ok(None)
    }

    /// B5's "no fourth way out": every quit path (`/quit`, `Ctrl-D`
    /// (`Action::Quit`), the double-`Ctrl-C` exit, and every other
    /// `Effect::Quit` arm in `app/run.rs` -- review round 1 widened this
    /// from the original two to ALL of them, see finding 1's own note just
    /// below) funnels through here before leaving the app loop. If the
    /// `/ask` modal is open -- OR parked behind a permission
    /// prompt in `pending_ask_modal` (the two compete for the one modal
    /// slot, so at most one is present) -- its child is purged via
    /// `Conway::purge`. Quitting IS the discard fate (purge
    /// only ever happens by an explicit user action, and quitting with the
    /// modal open is one). Best-effort: the process is exiting anyway, so
    /// a purge failure only leaves residue the NEXT startup's crash sweep
    /// (`Conway::sweep_stale_modal_asks`, wired in `tui::mod.rs`) reaps --
    /// it never blocks the exit.
    ///
    /// **Board item `01M0RWFH6V709B7WTAFRZGFKG3`: a FOURTH case, an ask
    /// still genuinely in flight** (no modal open, none parked -- the
    /// question was asked but no answer has arrived yet). This is
    /// deliberately handled differently from the other three: `purge`
    /// requires a TERMINAL agent (`RuntimeError::Store(StoreError::
    /// NotRemovable)`, "agent is still running", otherwise), and a running
    /// turn does not become
    /// terminal the instant this method cancels it (a reproduction test
    /// measured the gap). Attempting `purge` here
    /// synchronously would reproduce that same error on quit, so this does
    /// NOT attempt it. What it does instead, deliberately: best-effort
    /// cancel the child and discard any pending permission prompt for it
    /// (`Self::cancel_ask_child`, same sequence a keyboard abandon runs),
    /// clear the in-flight bookkeeping, and record a notice naming what
    /// happens to the residue -- the next startup's own crash sweep
    /// (`Conway::sweep_stale_modal_asks`), the SAME mechanism every other
    /// branch of this method already leans on for a purge failure. The
    /// process is exiting either way; there is nothing left in THIS run to
    /// wait for the cancellation to land.
    pub(super) async fn purge_open_ask_modal(&mut self) {
        // The modal is either live (`Mode::AskModal`) or parked in
        // `pending_ask_modal` while a permission prompt is showing; take
        // the child from whichever holds it. Without the parked arm,
        // quitting while a prompt covered the modal would leave the child
        // for the next startup's sweep instead of discarding it now (M1).
        let live_child = if matches!(self.state.mode, Mode::AskModal(_)) {
            let modal = match std::mem::replace(&mut self.state.mode, Mode::Normal) {
                Mode::AskModal(m) => m,
                _ => unreachable!("guarded by the matches! check above"),
            };
            Some(modal.child)
        } else {
            None
        };
        let parked_child = self.state.take_pending_ask_modal().map(|m| m.child);
        for child in live_child.into_iter().chain(parked_child) {
            if let Err(e) = self.conway.purge(child).await {
                self.state.transcript.push(Entry::Notice {
                    text: format!("could not discard the /ask child on exit: {e}"),
                });
            }
        }
        // The fourth case -- see this method's own doc above.
        if self.state.ask_in_flight {
            if let Some(child) = self.state.ask_child {
                self.cancel_ask_child(child).await;
            }
            self.state.ask_in_flight = false;
            self.state.ask_child = None;
            self.state.ask_started_at = None;
            self.state.ask_abandoned = false;
            self.state.transcript.push(Entry::Notice {
                text: "ask abandoned on exit -- its child will be cleaned up automatically \
                       on the next startup"
                    .to_string(),
            });
        }
        // C2: drain a parked intent confirmation card on exit too. Unlike
        // the /ask modal there is no live child to purge (the card opens
        // BEFORE any agent is created -- quitting with the card open IS
        // the manual fallback), so this is just a drop-on-the-floor for
        // symmetry with `take_pending_ask_modal` above: it keeps the
        // parking slot empty rather than leaving a classified intent
        // dangling in `pending_intent_confirm` at process exit.
        let _ = self.state.take_pending_intent_confirm();
        // Board item (split from `01KZHVFCN6ZEAXV7K5JHRQN1YB`): drain a
        // parked trust-preview card on exit too, for the identical reason
        // the intent-confirm card just above needs it -- no live child to
        // purge (nothing has been created OR written yet, since the actual
        // trust call only happens on an explicit confirm), so quitting
        // with the card open IS the cancel outcome. A card currently LIVE
        // in `Mode::TrustPreview` (rather than parked) needs no special
        // handling either: the process is exiting, and nothing was ever
        // written for it, so there is nothing left to undo.
        let _ = self.state.take_pending_trust_preview();
        // Board item `01M19NH39AE2D5AMJK0RZRQY86`: drain a parked
        // `ask_question` question on exit too, for the identical reason the
        // trust-preview card just above needs it -- dropping the returned
        // `PendingFormAsk` drops its reply channel, which is exactly
        // `TuiFormSurface::ask_select`'s own fail-closed fallback (the
        // blocked tool call resolves as a named `FormSurfaceError` rather
        // than hanging forever). A card currently LIVE in `Mode::UiForm`
        // (rather than parked) needs no special handling either, mirroring
        // `take_pending_trust_preview`'s own doc: the process is exiting,
        // and `self.state` (and the reply sender it owns) is dropped along
        // with it either way.
        let _ = self.state.take_pending_ui_form();
        // Slice 2 (board item `01M3DTT078W25MD2S4527R0WAV`): drain a parked
        // skill proposal on exit too, for the identical reason the
        // `ask_question` question just above needs it -- there is no live
        // child to purge (the ephemeral fork that produced this proposal is
        // ALREADY purged by the time a `SkillProposalModal` exists at all,
        // see that type's own doc), so quitting with the proposal open or
        // parked IS the discard fate: nothing was ever written. A proposal
        // currently LIVE in `Mode::SkillProposal` needs no special handling
        // either, mirroring `take_pending_ui_form`'s own doc.
        //
        // **Disclosed, accepted gap:** a proposal fork still IN FLIGHT
        // (`state.skill_propose_in_flight`, no modal yet) has no
        // `state.ask_child`-style handle this method could cancel -- unlike
        // `/ask`, this feature never surfaces the forked child's id before
        // its whole turn is done (see `tui/app/skill_propose.rs`'s own
        // doc). Quitting here leaves that spawned task to wind down on its
        // own; it is bounded by `conway_plugin_skills::PROPOSAL_MAX_STEPS`/
        // `PROPOSAL_DEADLINE_SECS` and holds its own `Conway` clone, so it
        // purges its ephemeral child and exits on its own schedule even
        // after this method returns, the same "bounded residue, never an
        // unbounded hang" posture the mechanical trigger's own lossy-
        // delivery decision already accepts elsewhere in this feature.
        let _ = self.state.take_pending_skill_proposal();
        // Review round 1 (SIGNIFICANT finding 1, "orphaned child on
        // quit"): an in-flight `!` command's own process group, killed and
        // bound-awaited -- see `Self::kill_shell_command_for_quit`'s own
        // doc (`app/shell_cmd.rs`) for why a bare cancel signal alone
        // (this method's EVERY other step above is a bare best-effort
        // signal/drop, never awaited to completion) is not enough here:
        // unlike an ask/trust/intent/form/skill-proposal residue (each
        // bounded by its own eventual timeout or reaped by the next
        // startup's crash sweep), an unawaited `!` command is a live OS
        // process that would otherwise be orphaned the instant this
        // process exits, with nothing left to ever clean it up.
        self.kill_shell_command_for_quit().await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::fixtures::{echo_conway_and_store, minimal_cli};
    use super::super::App;
    use crate::tui::state::Entry;

    /// **Evidence for a separately-filed item, not a guard for this one --
    /// do not change `handle_ctrl_c`'s behavior to make this pass
    /// differently.** The TUI's own keybinding table (`docs/interactive.md`)
    /// has long claimed `Ctrl-C` "pressed with nothing running, does
    /// nothing destructive on its own." This test shows that claim is
    /// false for the ordinary, unconfigured case: `handle_ctrl_c` ->
    /// `self.handle.cancel(root, "user cancel")` -> `SessionHandle::
    /// cancel_with(..., CancelMode::Immediate)` (`session_handle.rs`) trips
    /// the SAME `CancellationToken` `AgentLoop::run_inner`'s resume-gate
    /// `tokio::select!` races, `biased`, ahead of the gate's own
    /// `notify.notified()` arm (`agent_loop.rs`, the `ResumeGate::
    /// awaiting_prompt` branch) -- so a FIRST, single `Ctrl-C` press
    /// against a root sitting genuinely idle at that gate (the TUI's
    /// ordinary "nothing running, waiting for you to type" state, since
    /// the interactive root is always spawned `keep_alive: true` --
    /// `app/startup.rs::session_spec`'s own doc) routes straight to
    /// `finish_cancelled`: a TERMINAL result, not a no-op. The session
    /// ends before the operator ever typed a second message.
    #[tokio::test]
    async fn first_ctrl_c_against_a_genuinely_idle_keep_alive_root_ends_the_session() {
        let conway = echo_conway_and_store().0;
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();

        // Prove "genuinely idle," not merely "freshly constructed": wait
        // for the root's own agent-loop task to actually reach the resume
        // gate (`awaiting_prompt == true`) before pressing anything --
        // `App::new` never submits a prompt (`Runtime::start_root`'s own
        // doc: "a prompt-less root ... idles until the user types"), so
        // this is the TUI's ordinary post-launch, pre-first-message state.
        tokio::time::timeout(Duration::from_secs(5), async {
            while !app.handle.awaiting_prompt(root) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the freshly-started root must reach genuine idle within the bound");
        assert!(
            !app.agent_is_finished(root).await,
            "sanity: the root must still be alive right before the press"
        );

        let mut last_ctrl_c = None;
        let outcome = app
            .handle_ctrl_c(&mut last_ctrl_c)
            .await
            .expect("handle_ctrl_c should not error");
        assert!(
            outcome.is_none(),
            "a first press must never exit the process on its own: {outcome:?}"
        );

        // The root is now terminal -- a single first press, no second
        // press, no turn ever having run.
        let became_finished = tokio::time::timeout(Duration::from_secs(5), async {
            while !app.agent_is_finished(root).await {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .is_ok();
        assert!(
            became_finished,
            "LOAD-BEARING: a first Ctrl-C against an idle keep-alive root must end its \
             session -- a timeout here would mean this finding no longer holds and the \
             docs/interactive.md claim this test disproves may be accurate again"
        );
    }

    /// Review round 2, SIGNIFICANT finding 2: the FIRST `Ctrl-C` with a
    /// non-empty queue must leave a visible notice -- break-the-guard
    /// precedent lives right next to this test (temporarily gate the
    /// `pending > 0` check in `App::handle_ctrl_c` to `false` and confirm
    /// this goes red before trusting it green).
    #[tokio::test]
    async fn first_ctrl_c_with_a_nonempty_queue_leaves_a_discard_warning() {
        let conway = echo_conway_and_store().0;
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        app.state.queue_prompt(root, "important".to_string());

        let mut last_ctrl_c = None;
        let outcome = app
            .handle_ctrl_c(&mut last_ctrl_c)
            .await
            .expect("handle_ctrl_c should not error");

        assert!(
            outcome.is_none(),
            "a first press must never exit on its own: {outcome:?}"
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text }
                    if text.contains("1 queued message")
                        && text.contains("discarded")
                        && text.contains("Ctrl-C again")
            )),
            "the first press must warn about the queue it would discard: {:?}",
            app.state.transcript
        );
        // The queue itself must survive a single press -- this is pure
        // visibility, never a discard on its own.
        assert_eq!(app.state.held_prompts.len(), 1);
    }

    /// An empty queue must stay exactly as quiet as it was before this
    /// finding -- no new notice noise on the overwhelmingly common path.
    #[tokio::test]
    async fn first_ctrl_c_with_an_empty_queue_leaves_no_queue_notice() {
        let conway = echo_conway_and_store().0;
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let mut last_ctrl_c = None;
        app.handle_ctrl_c(&mut last_ctrl_c)
            .await
            .expect("handle_ctrl_c should not error");

        assert!(
            !app.state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::Notice { text } if text.contains("queued message"))),
            "an empty queue must produce no queue-discard notice at all: {:?}",
            app.state.transcript
        );
    }
}
