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
//!
//! **Board item `01M3WJ7906NP3P2ZNVDK549T1Q` (round 2):
//! [`App::purge_open_ask_modal`] now takes a `reason` and, before anything
//! else, ABORTS (never cancels/terminates) the root's and every known
//! non-ephemeral subagent's current turn, bound-awaiting each back to idle
//! -- see [`App::abort_in_flight_turns`]'s own doc for why `abort_turn`
//! (the non-terminal, per-turn primitive `Ctrl-C`'s first press already
//! uses), not `cancel` (which would end the session with a terminal
//! result, breaking `--resume`/`--continue`'s "a `keep_alive` root stays
//! resumable" contract), is the right primitive here, and for why it
//! still reaches an in-flight model `bash` tool call's own `kill_group`
//! (previously nothing on any quit path reached it at all).

use std::time::{Duration, Instant};

use super::App;
use crate::exit::ExitCode;
use crate::tui::state::{Entry, Mode};

/// How long a lone `Ctrl-C` remains "armed" -- a second `Ctrl-C` within this
/// window exits 130; after it, a `Ctrl-C` is treated as a fresh first press
/// (module notes: "second within 2 s exits with 130").
const DOUBLE_CTRL_C_WINDOW: Duration = Duration::from_secs(2);

impl App {
    /// First `Ctrl-C`: **board item `01M3XGPGT5W7GABVTC7F2NA0C9`** -- aborts
    /// the ROOT's current turn (not the focused agent: this mirrors the
    /// pre-existing target of the cancel call this replaces, and
    /// `docs/interactive.md` describes `Ctrl-C` as stopping the session's
    /// own reply, not whichever agent happens to be in view) if one is
    /// genuinely running, via [`conway::SessionHandle::abort_turn`] -- the
    /// NON-terminal primitive: the root accepts and answers another prompt
    /// in the SAME session afterward, unlike the `SessionHandle::cancel`
    /// this method used to call, which always ended it (the bug board item
    /// `01M3XGPGT5W7GABVTC7F2NA0C9` fixes; see this module's own
    /// `first_ctrl_c_...` tests below for the before/after). Pressed with
    /// the root genuinely idle (sitting at its
    /// resume gate, awaiting the operator's next prompt): nothing
    /// destructive happens at all -- a notice tells the operator a second
    /// press is what quits. Also (board item `01M0RWFH6V709B7WTAFRZGFKG3`)
    /// abandons an in-flight `/ask` if one is running -- see
    /// [`App::abandon_ask`]'s own doc; the two are independent and both
    /// best-effort, so an ask with nothing else running still gets
    /// abandoned, and an ordinary turn with no ask in flight is unaffected.
    /// Arms the double-press window either way. Second `Ctrl-C` within
    /// [`DOUBLE_CTRL_C_WINDOW`]: exit 130, unchanged.
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
                self.purge_open_ask_modal("tui quit (double ctrl-c)").await;
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
        // Review finding (board item `01M1YVKQ6ABQDWYSA7CEF20WKG`, minor):
        // a no-op when no `/distill` is in flight (`App::abandon_distill`'s
        // own guard) -- mirrors `abandon_ask` immediately above, including
        // being independent of it (both best-effort, both checked before
        // the root-turn cancel below).
        self.abandon_distill();
        // Board item `01M3XGPGT5W7GABVTC7F2NA0C9`: `abort_turn`, not
        // `cancel` -- stops only the root's current turn (if any), leaving
        // the agent itself, and the session, alive. `Ok(true)` means a live
        // turn was actually aborted; `Ok(false)` means the root was already
        // idle at its resume gate, in which case this press must do nothing
        // destructive (the bug board item `01M3XGPGT5W7GABVTC7F2NA0C9` fixes)
        // -- a notice tells the operator what a SECOND press does instead,
        // mirroring the queued-message notice immediately above. Best-effort
        // either way: a
        // failure (e.g. an unknown root, which should never happen in
        // practice) is not fatal to the session -- surfaced as a notice, not
        // a crash.
        match self
            .handle
            .abort_turn(self.handle.root(), "user cancel")
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                self.state.transcript.push(Entry::Notice {
                    text: "nothing to interrupt -- press Ctrl-C again to quit".to_string(),
                });
            }
            Err(e) => {
                self.state.transcript.push(Entry::Notice {
                    text: format!("abort failed: {e}"),
                });
            }
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
    ///
    /// **Board item `01M3WJ7906NP3P2ZNVDK549T1Q` (round 2): the root's and
    /// every known subagent's CURRENT TURN is aborted and bound-awaited back
    /// to idle FIRST, now -- see [`App::abort_in_flight_turns`]'s own doc
    /// for the full mechanism, why `abort_turn` (non-terminal) and not
    /// `cancel` (terminal) is the one this method calls, and the gap this
    /// closes (an in-flight model `bash` tool call's process group was
    /// previously never killed on any quit path at all). `reason` is the
    /// `Event::TurnAbortedByUser`/`AgentResult`'s own abort-reason text, so
    /// `/quit` and the SIGTERM/SIGHUP path (`app/run.rs`'s
    /// `termination.notified()` arm) each leave a reason an operator can
    /// actually read back (`"tui quit"`, or `cause.reason()`'s `"signal:
    /// SIGTERM"`/`"signal: SIGHUP"`, matching `oneshot::run`'s own
    /// convention for the text, even though the MECHANISM here is the
    /// non-terminal abort, not that function's terminal cancel).
    pub(super) async fn purge_open_ask_modal(&mut self, reason: &str) {
        self.abort_in_flight_turns(reason).await;

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
        // Board item `01M1YVKQ6ABQDWYSA7CEF20WKG`: drain a parked `/distill`
        // briefing on exit too, for the identical reason the skill proposal
        // just above needs it -- there is no live child to purge (the
        // ephemeral fork that produced this briefing is ALREADY purged, see
        // `DistillModal`'s own doc), so quitting here IS the discard fate.
        // A briefing currently LIVE in `Mode::Distill` needs no special
        // handling either, mirroring `take_pending_skill_proposal`'s own
        // doc. The same disclosed gap applies too: a distill fork still IN
        // FLIGHT (`state.distill_in_flight`, no modal yet) has no
        // `state.ask_child`-style handle this method could cancel; its own
        // spawned task is bounded (`DISTILL_MAX_STEPS`/`DISTILL_DEADLINE_
        // SECS`) and holds its own `Conway` clone, so it purges its
        // ephemeral child and exits on its own schedule even after this
        // method returns.
        let _ = self.state.take_pending_distill();
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

    /// Board item `01M3WJ7906NP3P2ZNVDK549T1Q` (round 2): aborts the
    /// CURRENT turn of the root and of every non-ephemeral subagent this
    /// TUI knows about (`AppState::tree`, below), then bound-awaits each
    /// one back to idle -- called from [`Self::purge_open_ask_modal`],
    /// before anything else it does.
    ///
    /// **Why `abort_turn`, not `cancel`.** `SessionHandle::cancel`
    /// (`CancelMode::Immediate`) ends the target outright, publishing a
    /// TERMINAL `AgentResult` (`ResultStatus::Cancelled`) to its own
    /// persisted session log. For the root -- `keep_alive: true`, meant to
    /// be reattached by `--resume`/`/resume` across TUI restarts -- that is
    /// exactly wrong: a session whose log ends in a terminal record is
    /// precisely what `--resume` must NOT see for an ordinary quit (see
    /// `docs/sessions.md`'s own resume semantics; a terminal root may
    /// refuse to resume, or resume as if already dead). `SessionHandle::
    /// abort_turn` (`conway-runtime/src/tree.rs:608`'s own doc: "the
    /// non-terminal sibling of `cancel`") is the one `Ctrl-C`'s FIRST press
    /// already uses for exactly this reason (`Self::handle_ctrl_c`, this
    /// file) -- it trips only the agent's CURRENT TURN's own child token
    /// (`turn_abort`/`turn_cancel`), leaving the agent and its own
    /// `CancellationToken` subtree alive, and for a `keep_alive` agent
    /// returns it to idling at its resume gate with its conversation
    /// intact, never publishing anything terminal.
    ///
    /// **Verified this still reaches an in-flight `bash` call's own
    /// `kill_group` (file:line, not inferred from a name).**
    /// `crates/conway-runtime/src/agent_loop.rs:1752`:
    /// `let turn_cancel = self.cancel.child_token();`, then
    /// `agent_loop.rs:1754-1755`:
    /// `.set_turn_abort_token(self.agent_id, turn_cancel.clone())` --
    /// registering THIS EXACT `turn_cancel` value as the tree's own
    /// `AgentTree::abort_turn` target (`conway-runtime/src/tree.rs:608-629`:
    /// `abort_turn` looks up `entry.turn_abort`'s stored token and calls
    /// `token.cancel()` on it). The SAME `turn_cancel` local is cloned into
    /// `ToolBatchCtx.cancel` at `agent_loop.rs:2369`
    /// (`cancel: turn_cancel.clone()`), which `conway-runtime/src/tools/
    /// runner.rs:268` reads as `batch_cancel`, derives `call_cancel =
    /// batch_cancel.child_token()` (`runner.rs:608`), and bridges
    /// (`runner.rs:635-643`, a spawned task awaiting `watch.cancelled()`
    /// then calling `core_cancel.cancel()`) into the poll-based
    /// `conway_core::ports::plugin::CancellationToken` that becomes
    /// `ToolCtx.cancel` (`runner.rs:645,653`) -- the exact flag `crates/
    /// conway-tools/src/shell/bash.rs`'s run loop polls (`ctx.cancel.
    /// is_cancelled()`, line 350) to decide whether to `kill_group` its
    /// process group (lines 421/435/444). So `abort_turn(agent, reason)`
    /// reaches the SAME `kill_group` call `cancel(agent, reason)` would
    /// have, through the per-turn token rather than the whole-agent one --
    /// the only difference is what happens to the AGENT once that tool
    /// call resolves (idles again, vs. ends terminally), which is exactly
    /// the behavior this fix needs.
    ///
    /// **Subagents, not just the root.** `abort_turn` is scoped to ONE
    /// agent -- unlike `cancel`, it does NOT cascade to descendants
    /// (`AgentTree::abort_turn`'s own doc: "the non-terminal sibling of
    /// `cancel`... leaving the agent itself... untouched" says nothing
    /// about descendants, because there is no structural cascade to speak
    /// of -- each agent's `turn_abort` token is independent). A model-
    /// issued `bash` call running inside a FORKED/SPAWNED subagent's own
    /// turn (not the root's) would be missed entirely by aborting only the
    /// root. This method therefore walks `AppState::tree`
    /// (`AgentTreeView`, this crate's own event-sourced projection of the
    /// session's agent tree -- `tui/state/agent_tree.rs`'s own doc: built
    /// from `Event::AgentSpawned`/`Event::AgentFinished` alone, the
    /// established pattern this crate already uses instead of calling
    /// `SessionHandle::tree()` in production code, which returns the
    /// runtime-WIDE snapshot, every other session included) and aborts
    /// every node's turn, not only the root's.
    ///
    /// **Ephemeral forks (`/ask`, `/distill`, skill-propose) are
    /// deliberately EXCLUDED** (`TreeNode::ephemeral`) -- each already has
    /// its own dedicated, more careful cleanup elsewhere in `Self::
    /// purge_open_ask_modal` (cancel-then-purge, or a parked-card drain),
    /// built around the specific shape of ITS OWN residue; aborting them
    /// here too would be redundant at best and could race that dedicated
    /// handling at worst. A real, non-ephemeral subagent (`/fork`,
    /// `/spawn`, or the model's own `conway_fork`/`conway_spawn`) has no
    /// such separate handling and needs this one.
    ///
    /// **Bound-awaiting `awaiting_prompt(agent) || agent finished`, not
    /// `await_agent`.** `await_agent` waits for a TERMINAL result, which a
    /// `keep_alive` agent successfully returned to idle will now never
    /// produce -- waiting for one would simply burn the whole bound every
    /// time. `SessionHandle::awaiting_prompt` is the genuinely correct
    /// "back to idle" signal for a `keep_alive` agent (mirrors `App::
    /// interrupt_and_send`'s own identical `abort_turn` + bounded
    /// `awaiting_prompt` poll, `app.rs`, for `tui.busy_input = "interrupt"`/
    /// `prompt.send_now`) -- but it is permanently `false` for a
    /// non-`keep_alive` agent (an ordinary subagent), which never reaches a
    /// resume gate at all (`SessionHandle::awaiting_prompt`'s own doc).
    /// `Self::agent_is_finished` (`app/busy_input.rs`, a non-blocking peek
    /// at `await_agent`'s own result channel) covers that case: a
    /// non-`keep_alive` subagent's own `abort_turn` DOES end it terminally
    /// (`agent_loop.rs:2465-2470`: `turn_cancel.is_cancelled()` with
    /// `!self.spec.keep_alive` finishes the agent, unlike the root's own
    /// `keep_alive: true` branch just above it, which does not) -- exactly
    /// the same terminal `Cancelled` state ANY one-shot-shaped agent
    /// (including a SIGINT-cancelled `conway -p` run) already ends up in
    /// routinely, so this is not a new or fragile state for THAT class of
    /// agent -- the keep-alive root is the only agent kind this whole
    /// method exists to protect from exactly that.
    pub(super) async fn abort_in_flight_turns(&mut self, reason: &str) {
        let root = self.handle.root();
        let mut agents = vec![root];
        for node in &self.state.tree.nodes {
            if !node.ephemeral && node.agent_id != root {
                agents.push(node.agent_id);
            }
        }
        for &agent in &agents {
            let _ = self.handle.abort_turn(agent, reason).await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut all_settled = true;
                for &agent in &agents {
                    if !self.handle.awaiting_prompt(agent) && !self.agent_is_finished(agent).await
                    {
                        all_settled = false;
                        break;
                    }
                }
                if all_settled {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::fixtures::{echo_conway_and_store, echo_conway_over, minimal_cli};
    use super::super::App;
    use crate::tui::state::Entry;

    /// **Board item `01M3XGPGT5W7GABVTC7F2NA0C9`: this test used to be named
    /// `..._ends_the_session` and proved the opposite of what it proves
    /// now** -- it was "evidence for a separately-filed item" (this one),
    /// deliberately not a guard, because the fix needed a new primitive
    /// (`SessionHandle::abort_turn`) this file's own B3e slice did not have.
    /// Now that `handle_ctrl_c` calls `abort_turn` instead of `cancel`, a
    /// first press against a root sitting genuinely idle at its resume gate
    /// (the TUI's ordinary "nothing running, waiting for you to type" state)
    /// finds no live turn to abort (`AgentTree::abort_turn`'s own `Ok(false)`
    /// idle case) and does nothing destructive at all -- `docs/
    /// interactive.md`'s claim that `Ctrl-C` "pressed with nothing running,
    /// does nothing destructive" is accurate again.
    #[tokio::test]
    async fn first_ctrl_c_against_a_genuinely_idle_keep_alive_root_leaves_the_session_live() {
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

        // LOAD-BEARING: give any wrongly-triggered cancellation a real
        // window to land, then prove it never did -- the root must still be
        // alive, genuinely idle, and ready for the operator's next prompt.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !app.agent_is_finished(root).await,
            "a first Ctrl-C against an idle keep-alive root must leave the session live -- \
             finding it finished here would mean the regression board item \
             01M3XGPGT5W7GABVTC7F2NA0C9 fixes has come back"
        );
        assert!(
            app.handle.awaiting_prompt(root),
            "the root must still be sitting at its resume gate, ready for another prompt"
        );
        assert!(
            app.state.transcript.iter().any(
                |e| matches!(e, Entry::Notice { text } if text.contains("Ctrl-C again to quit"))
            ),
            "an idle first press must tell the operator what a second press does: {:?}",
            app.state.transcript
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

    /// **Board item `01M3WJ7906NP3P2ZNVDK549T1Q` (round 2), the regression
    /// guard a review finding on this fix's own first draft exists for.**
    /// That first draft called `SessionHandle::cancel` (terminal) from
    /// `purge_open_ask_modal`, which would have made THIS test fail: a
    /// `keep_alive` root quit that way ends up with a terminal
    /// `AgentResultRecord` as the last line of its own persisted log,
    /// which is exactly what `--resume`/`/resume` must never see for an
    /// ordinary quit. `Self::abort_in_flight_turns`'s own doc has the full
    /// reasoning for why `abort_turn` (non-terminal) is the fix instead.
    ///
    /// Proves the claim at the ONLY level that actually matters: not that
    /// `SessionHandle` merely reports the root as non-terminal in memory,
    /// but that the real, PERSISTED log -- read back through a SECOND,
    /// wholly independent `Conway`/`Runtime` over the SAME store (the
    /// "simulated restart" shape `app/new_session.rs`'s own
    /// `new_starts_a_fresh_session_and_leaves_the_old_one_resumable` test
    /// already establishes) -- carries no `AgentResultRecord` at all, and
    /// that `Conway::resume` against it still succeeds.
    #[tokio::test]
    async fn quitting_leaves_no_terminal_result_and_the_session_stays_resumable() {
        let (conway, store) = echo_conway_and_store();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        let session = app.handle.id();

        app.submit("hello before quitting".to_string())
            .await
            .expect("submit should not error");
        tokio::time::timeout(Duration::from_secs(10), async {
            while !app.handle.awaiting_prompt(root) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the turn must settle before quitting");

        // The exact funnel every quit path in `app/run.rs` calls.
        app.purge_open_ask_modal("test quit").await;

        assert!(
            !app.agent_is_finished(root).await,
            "quitting must not publish a terminal AgentResult for a keep_alive root"
        );
        assert!(
            app.handle.awaiting_prompt(root),
            "the root must be left idle at its resume gate, ready for a later --resume"
        );

        // Simulated restart: a SECOND, independent `Conway` over the SAME
        // store -- `Conway::resume` must succeed, and the resumed
        // session's own transcript must carry NO `AgentResultRecord` at
        // all. This is the real, on-disk proof; the in-memory assertions
        // above alone could not rule out a terminal record that merely
        // hadn't been observed yet.
        let restarted = echo_conway_over(store);
        let resumed = restarted
            .resume(session)
            .await
            .expect("a quit session must still be resumable after a simulated restart");
        let records = resumed
            .transcript(root)
            .await
            .expect("transcript must be readable");
        assert!(
            records
                .iter()
                .all(|r| !matches!(r, conway::LogRecord::AgentResultRecord { .. })),
            "quitting must never write a terminal AgentResultRecord into the root's own log: \
             {records:?}"
        );
        // The pre-quit history is still there too -- quitting discarded
        // nothing, it only ended the TUI's own attachment to a live turn.
        assert!(
            records.iter().any(
                |r| matches!(r, conway::LogRecord::UserTurn { text, .. } if text == "hello before quitting")
            ),
            "the session's own history must survive a quit: {records:?}"
        );
    }
}
