//! `/distill` (board item `01M1YVKQ6ABQDWYSA7CEF20WKG`): the explicit,
//! operator-typed verb PHILOSOPHY.md's own §3 already names as the shape
//! conway recommends for a conversation grown heavy -- "fork -> distill the
//! part that matters -> spawn a clean child with that briefing" -- built
//! entirely out of this crate's existing fork/await/spawn machinery, mirroring
//! `app/skill_propose.rs`'s own shape closely: an ephemeral fork child
//! (bounded by [`DISTILL_MAX_STEPS`]/[`DISTILL_DEADLINE_SECS`], the same
//! "tokens the operator did not ask for" precedent `conway_plugin_skills::
//! PROPOSAL_MAX_STEPS`/`PROPOSAL_DEADLINE_SECS` sets) is forked off the
//! FOCUSED agent (not always the root -- see [`crate::tui::commands::Effect::
//! RunDistill`]'s own doc for why this differs from `/ask`/`/conway.skills.
//! propose`), driven for one turn with a fixed directive, then purged the
//! instant its reply is captured -- a pure distillate, exactly like a skill
//! proposal's own ephemeral child (`app/skill_propose.rs`'s own "distillate
//! only" doc applies here verbatim).
//!
//! [`App::spawn_distill`] is the actual `tokio::spawn` call site
//! (`commands::execute`'s `SlashCommand::Distill` arm cannot spawn this
//! itself -- see `commands::Effect::RunDistill`'s own doc); [`App::
//! apply_distill_done`] opens the briefing modal once the child's reply
//! lands; [`App::spawn_from_distill`]/[`App::apply_distill_edit_action`] are
//! the modal's own `Enter`/`e` fates (`Esc` needs no method of its own --
//! `AppState::close_distill` is a plain, synchronous mode close, called
//! directly from `run.rs`, mirroring `SkillProposalFate::Discard`'s own
//! arm).
//!
//! # Why `Enter` is an `App` method returning `bool`, not a `commands::Effect`
//!
//! Spawning the fresh agent and delivering the briefing as its opening
//! prompt needs `self.conway`/`self.handle` directly (to build the new
//! session and then swap the app loop onto it) -- capabilities `commands::
//! Host` deliberately does not expose (that trait's own doc: "a thin
//! abstraction over exactly `SessionHandle`/`Conway`'s own methods" scoped to
//! what `execute` needs; swapping `self.handle` and the app loop's own
//! `events` subscription both live on `App`/`run.rs`, never in `execute`).
//! `Enter` is not a slash command in its own right either (it is a modal
//! fate, reached from `input::handle_distill_key`, exactly like
//! `SkillProposalFate::Write`), so there is no `SlashCommand`/`execute` arm
//! for it to live in at all. The `bool` return tells `run.rs`'s own
//! `Action::DistillFate` arm whether `self.handle` was actually swapped (a
//! successful spawn) so it knows to resubscribe `events` -- the same thing
//! `Effect::Resumed`'s own arm does, just reached through a direct return
//! value instead of through `Effect` (this fate never goes through
//! `commands::execute` at all).

use conway::{AgentId, AgentResult, Budget, Conway, ForkSpec, SessionHandle, SessionId};

use super::App;
use crate::tui::commands;
use crate::tui::state::{DistillModal, Entry, Mode};

/// **180 seconds.** Mirrors `conway_plugin_skills::PROPOSAL_DEADLINE_SECS`
/// exactly -- see this module's own doc, "a pure distillate... bounded by
/// the same 'tokens the operator did not ask for' precedent." A round,
/// plausible number for "read back over a conversation and write a
/// briefing," not a measured calibration -- the same "plausible, not
/// proven" disclosure that constant's own doc already makes.
const DISTILL_DEADLINE_SECS: u64 = 180;

/// **20 steps.** One step wider than `conway_plugin_skills::
/// PROPOSAL_MAX_STEPS` (15): a briefing covering "the task, decisions made,
/// open questions, and the files that matter" plausibly re-reads a FEW of
/// those files before writing, where a skill proposal only reflects on
/// conversation it already has in context -- still bounded, not unbounded.
const DISTILL_MAX_STEPS: u32 = 20;

/// The directive handed to the ephemeral `/distill` fork child -- asks for a
/// briefing covering the task itself, decisions already made and why, open
/// questions, and the files that matter, concrete enough that a fresh agent
/// with none of this conversation's history needs nothing else to start.
const DISTILL_DIRECTIVE: &str = "\
You are about to be discarded. A fresh agent, with none of this conversation's history, is \
about to take over this task. Write a briefing for that fresh agent, covering:\n\
\n\
- The task itself, stated plainly.\n\
- Decisions already made, and why.\n\
- Open questions that are still unresolved.\n\
- The files that matter, and why each one matters.\n\
\n\
Be concrete and complete enough that the fresh agent needs nothing else from this \
conversation to start. Respond with the briefing text only -- no preamble, no commentary \
before or after it.";

/// Builds the fork directive: [`DISTILL_DIRECTIVE`] alone, or -- when the
/// operator gave `/distill` its own `[<instructions>]` argument -- followed
/// by a paragraph naming them, so the child's briefing is steered the same
/// way a `/fork <text>` directive would be, without replacing the fixed
/// shape every `/distill` briefing must cover.
fn build_directive(instructions: Option<&str>) -> String {
    match instructions {
        Some(extra) if !extra.trim().is_empty() => {
            format!(
                "{DISTILL_DIRECTIVE}\n\nThe operator also asked you to focus the briefing on \
                 this: {}",
                extra.trim()
            )
        }
        _ => DISTILL_DIRECTIVE.to_string(),
    }
}

/// The bounded [`ForkSpec`] this feature forks with -- see this module's own
/// doc, "a pure distillate."
fn distill_fork_spec(directive: String) -> ForkSpec {
    ForkSpec::new(directive).ephemeral(true).budget(Budget {
        max_steps: DISTILL_MAX_STEPS,
        deadline: Some(
            chrono::Utc::now()
                + chrono::Duration::seconds(
                    i64::try_from(DISTILL_DEADLINE_SECS).unwrap_or(i64::MAX),
                ),
        ),
        max_tokens: None,
        max_tool_calls: None,
    })
}

/// One spawned `/distill` task's eventual reply. `old_context_tokens` is a
/// best-effort snapshot of the FOCUSED (soon-to-be-distilled) agent's own
/// context size, taken before the fork -- `None` when it could not be read
/// (never fatal to `/distill` itself: the cost notice just omits the "old"
/// half rather than failing the whole command). `generation` is the
/// [`crate::tui::state::AppState::distill_generation`] value stamped at
/// SPAWN time -- see that field's own doc for why `App::apply_distill_done`
/// compares it against the CURRENT value before doing anything else with
/// this reply.
pub(super) struct DistillDone {
    pub(super) result: conway::Result<AgentResult>,
    pub(super) old_session: SessionId,
    pub(super) old_context_tokens: Option<u64>,
    pub(super) generation: u64,
}

/// Forks `parent`, awaits the child's single reply, and purges the child
/// UNCONDITIONALLY (this module's own doc, "a pure distillate") before
/// sending the result back on `tx`. A free function (not an `App` method),
/// mirroring `skill_propose::run_skill_propose`'s own shape exactly: it runs
/// inside a `tokio::spawn`ed task that outlives any single `submit` call, so
/// it cannot borrow `self`.
pub(super) async fn run_distill(
    handle: SessionHandle,
    conway: Conway,
    parent: AgentId,
    instructions: Option<String>,
    generation: u64,
    tx: tokio::sync::mpsc::UnboundedSender<DistillDone>,
) {
    let old_context_tokens = handle
        .context_report_current(parent)
        .await
        .ok()
        .map(|r| u64::from(r.total_tokens_est));
    let old_session = handle
        .resolve_agent_session(parent)
        .await
        .unwrap_or_else(|_| handle.id());
    let directive = build_directive(instructions.as_deref());
    let result = match handle.fork(parent, distill_fork_spec(directive)).await {
        Ok(child) => {
            let awaited = handle.await_agent(child).await;
            // Distillate only -- purge regardless of outcome, best-effort:
            // a purge failure here only leaves residue for the next
            // startup's own crash sweep, mirroring every other best-effort
            // ephemeral-child cleanup in this crate.
            let _ = conway.purge(child).await;
            awaited
        }
        Err(e) => Err(e),
    };
    // The receiver only goes away when `App::run`'s loop has already
    // exited -- nothing left to notify, so a send failure here is silently
    // dropped, mirroring `run_skill_propose`'s own send site.
    let _ = tx.send(DistillDone {
        result,
        old_session,
        old_context_tokens,
        generation,
    });
}

/// How long [`wait_for_context_tokens`] polls before giving up -- a hang
/// detector, not a promptness assertion (see `app/ask.rs`'s own
/// `HANG_TIMEOUT` doc for the same reasoning): context assembly happens
/// before the fresh agent's first backend call, not after, so this
/// ordinarily resolves within a handful of polls.
const CONTEXT_POLL_BOUND: std::time::Duration = std::time::Duration::from_secs(5);
const CONTEXT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

/// Polls `handle.context_report(agent)` until it reports a genuinely
/// ASSEMBLED context or [`CONTEXT_POLL_BOUND`] elapses -- needed because a
/// BRAND NEW agent (no turn has ever been persisted for it) has no fallback
/// for `context_report_current` to fall back to (`SessionHandle::
/// context_report_current`'s own doc: it falls back to the most recently
/// PERSISTED report, and a fresh agent has none yet), so the plain,
/// live-only `context_report` is read instead, retried until the agent
/// loop has actually assembled its first turn's context -- which happens
/// before any backend call, not after. Returns `None` on a genuine timeout
/// (degrades the notice, never fails the spawn that already succeeded).
///
/// **A dogfood finding caught this returning `Ok` on its very first poll,
/// every time, reading back `0` for a context that in fact held the whole
/// briefing.** `Runtime::context_report` (`conway-runtime`'s own doc)
/// returns `Ok` immediately, with a PLACEHOLDER empty report (zero
/// segments, `total_tokens_est: 0`), for any agent that has been started
/// but has not yet had a turn's context assembled into its report slot --
/// which is exactly the state a just-`new_session`'d agent is in the
/// instant this function's very first poll runs, before the agent loop's
/// own background task has had a scheduling turn at all. Treating that
/// `Ok` as "assembled" is the bug: it is indistinguishable from a REAL
/// assembled report by its `Result` alone. `report.segments.is_empty()` is
/// the one field that tells the two apart -- a genuinely assembled first
/// turn always carries at least the preamble, where the placeholder never
/// carries anything -- so this now keeps polling past that placeholder
/// until a non-empty one lands, or the bound above gives up.
async fn wait_for_context_tokens(handle: &SessionHandle, agent: AgentId) -> Option<u64> {
    tokio::time::timeout(CONTEXT_POLL_BOUND, async {
        loop {
            if let Ok(report) = handle.context_report(agent).await {
                if !report.segments.is_empty() {
                    return u64::from(report.total_tokens_est);
                }
            }
            tokio::time::sleep(CONTEXT_POLL_INTERVAL).await;
        }
    })
    .await
    .ok()
}

/// Formats `/distill`'s own cost disclosure: the new agent's `/context` size
/// versus the old one's, so the operator sees what the move actually spent
/// before deciding whether it was worth it.
fn cost_notice(old_tokens: Option<u64>, new_tokens: Option<u64>) -> String {
    match (old_tokens, new_tokens) {
        (Some(old), Some(new)) => format!(
            "spawned a fresh agent from the briefing -- context: {old} tokens -> {new} tokens"
        ),
        (Some(old), None) => format!(
            "spawned a fresh agent from the briefing -- old context: {old} tokens (the new \
             agent's own context could not be read yet)"
        ),
        (None, Some(new)) => format!(
            "spawned a fresh agent from the briefing -- new context: {new} tokens (the old \
             agent's own context could not be read)"
        ),
        (None, None) => {
            "spawned a fresh agent from the briefing (context sizes could not be read)".to_string()
        }
    }
}

impl App {
    /// Spawns the `/distill` task off this loop's own `select!`, never on it
    /// -- see this module's own doc and `commands::Effect::RunDistill`'s for
    /// why this specific method (not `commands::execute` itself) is what
    /// does the actual `tokio::spawn`. `commands::execute`'s
    /// `SlashCommand::Distill` arm has already validated and set `state.
    /// distill_in_flight` before returning the effect that reaches this
    /// call.
    pub(super) fn spawn_distill(&self, parent: AgentId, instructions: Option<String>) {
        let handle = self.handle.clone();
        let conway = self.conway.clone();
        let tx = self.distill_tx.clone();
        // `commands::execute`'s `SlashCommand::Distill` arm already bumped
        // `state.distill_generation` for THIS fork before returning the
        // effect that reaches this call -- read at spawn time (not inside
        // the spawned task, which cannot borrow `self`) and threaded through
        // so `App::apply_distill_done` can tell this exact fork's eventual
        // reply apart from any other `/distill`'s.
        let generation = self.state.distill_generation;
        tokio::spawn(async move {
            run_distill(handle, conway, parent, instructions, generation, tx).await;
        });
    }

    /// Abandons an in-flight `/distill` from the keyboard (`Ctrl-C`,
    /// `App::handle_ctrl_c`) -- mirrors `App::abandon_ask`'s own shape
    /// exactly (that method's own doc): a no-op if no `/distill` is in
    /// flight, otherwise bumps `state.distill_generation` (so `App::
    /// apply_distill_done`'s own early check silently drops the eventual
    /// `DistillDone` instead of opening the briefing modal), clears `state.
    /// distill_in_flight`, and posts a "distill abandoned" notice.
    ///
    /// **Disclosed gap, mirroring `shutdown.rs::purge_open_ask_modal`'s own
    /// identical one for a still-in-flight skill proposal:** unlike `/ask`,
    /// `/distill` never surfaces the forked child's `AgentId` before its
    /// whole turn is done (`run_distill`'s own doc -- `DistillDone` is the
    /// ONE message the spawned task ever sends), so there is no `state.
    /// ask_child`-style handle this method could cancel; the fork is left to
    /// wind down on its own schedule, bounded by `DISTILL_MAX_STEPS`/
    /// `DISTILL_DEADLINE_SECS` and purged unconditionally by `run_distill`
    /// itself the instant it finishes -- bounded residue, never an
    /// unbounded hang, the same posture the disclosed gap above already
    /// accepts.
    ///
    /// **The race this used to disclose and accept is now closed.** Clearing
    /// `distill_in_flight` here (rather than leaving it set until the
    /// eventual `DistillDone`, the way `ask_in_flight` stays set across an
    /// `abandon_ask`) lets the operator start a SECOND `/distill`
    /// immediately. A former version of this method left `state.
    /// distill_generation`'s predecessor (a bare `distill_abandoned: bool`)
    /// untouched here, so if the abandoned fork's `DistillDone` arrived
    /// before the second one's own, `apply_distill_done`'s unconditional
    /// `distill_in_flight = false` cleared the SECOND one's flag too,
    /// briefly permitting a third concurrent `/distill`. Bumping the
    /// generation here too (not only at the NEXT `/distill`'s own start)
    /// closes it: the abandoned fork's reply now carries a generation that
    /// is stale the instant this method returns, whether or not the
    /// operator ever starts another `/distill` at all.
    pub(super) fn abandon_distill(&mut self) {
        if !self.state.distill_in_flight {
            return;
        }
        self.state.distill_generation = self.state.distill_generation.wrapping_add(1);
        self.state.distill_in_flight = false;
        self.state.transcript.push(Entry::Notice {
            text: "distill abandoned".to_string(),
        });
    }

    /// Applies one [`DistillDone`] reply: clears `state.distill_in_flight`
    /// and either posts a plain notice (the fork itself failed, or the
    /// child produced no usable briefing) or opens the `/distill` modal over
    /// a non-empty one. Called from `App::run`'s own `distill_rx.recv()`
    /// arm, unconditionally -- mirrors `App::apply_skill_propose_done`'s own
    /// doc for why this is never gated on `state.mode`.
    ///
    /// **Two early drops, a review finding (board item
    /// `01M1YVKQ6ABQDWYSA7CEF20WKG`), checked before any of the above:**
    ///
    /// 1. `done.generation != state.distill_generation` -- this reply is NOT
    ///    the one `/distill` whose briefing is currently wanted: either a
    ///    `Ctrl-C` already abandoned it (`App::abandon_distill`'s own doc)
    ///    and posted its own "distill abandoned" notice, or a NEWER
    ///    `/distill` has since started and is the one `distill_in_flight`
    ///    now describes. Either way this stale reply is dropped silently,
    ///    WITHOUT touching `distill_in_flight` -- the field it would clear
    ///    may belong to a fork that is still genuinely running. See
    ///    `AppState::distill_generation`'s own doc for the `distill_
    ///    abandoned: bool` this replaced and exactly which race that bare
    ///    flag could not close.
    /// 2. `done.old_session != self.handle.id()` -- `self.handle` was
    ///    swapped onto a GENUINELY different session since this `/distill`
    ///    was forked (belt and braces: `/new`'s own refusal above should
    ///    already prevent this while a `/distill` is in flight, but a
    ///    result racing in before that refusal existed -- or before this
    ///    one otherwise lands -- must never pop a briefing modal, or carry
    ///    a stale `old_session` into a cost notice, over a session the
    ///    operator has already left). `DistillDone::old_session` already
    ///    carries the forked-off agent's own session id (`run_distill`'s own
    ///    `handle.resolve_agent_session` call), so no new field is needed to
    ///    make this comparison.
    pub(super) fn apply_distill_done(&mut self, done: DistillDone) {
        if done.generation != self.state.distill_generation {
            return;
        }
        self.state.distill_in_flight = false;
        if done.old_session != self.handle.id() {
            self.state.transcript.push(Entry::Notice {
                text: format!(
                    "a /distill briefing from session {} arrived after the session changed -- \
                     discarded",
                    done.old_session
                ),
            });
            return;
        }
        let result = match done.result {
            Ok(result) => result,
            Err(e) => {
                self.state.transcript.push(Entry::Notice {
                    text: format!("/distill failed: {e}"),
                });
                return;
            }
        };
        if !result.is_terminal_success() || !result.has_output() {
            self.state.transcript.push(Entry::Notice {
                text: format!("/distill produced no usable briefing ({:?})", result.status),
            });
            return;
        }
        self.state.offer_distill(DistillModal {
            child: result.agent_id,
            briefing: result.summary,
            old_session: done.old_session,
            old_context_tokens: done.old_context_tokens,
            error: None,
        });
    }

    /// `Enter` on the `/distill` modal: starts a fresh root session (same
    /// "pure and light" tool profile and `keep_alive` shape `/new`'s own
    /// [`commands::new_session_spec`] uses, carrying the SAME role/model pin
    /// forward), delivers [`DistillModal::briefing`] as its opening prompt,
    /// and -- only on success -- swaps the app loop onto it, resets
    /// `AppState` to that fresh session (closing the modal as a side effect),
    /// and refreshes the session head. A failed spawn/delivery keeps the
    /// modal OPEN with the error shown (`AppState::fail_distill`), mirroring
    /// `commands::apply_ask_fate`'s own "a failed fate never silently
    /// vanishes" rule.
    ///
    /// **Dogfood finding (round 4): this used to swap `self.handle` alone,
    /// never resetting `AppState`.** The status bar kept showing the OLD
    /// session id and `ctx`, `/context` reported the OLD agent's context,
    /// and the old transcript stayed on screen with the new agent's output
    /// merely appended below it -- `/distill`'s own `Enter` was the ONE
    /// caller of `AppState::reset_for_new_session` that forgot to call it,
    /// unlike `/new` (`commands::execute`'s `SlashCommand::New` arm) and
    /// `/resume` (`commands::apply_resume`). Now goes through the exact same
    /// funnel those two already share -- see that method's own doc for the
    /// full CARRY/RESET classification (clean transcript, `focused_agent`
    /// pointed at the new root, every session-scoped permission mirror
    /// dropped) -- and carries `role_pin`/`model_pin` forward exactly like
    /// `/new`'s own arm does (the `SessionSpec` above already started this
    /// fresh agent from those same two pins).
    ///
    /// Returns `true` when `self.handle` was actually swapped -- the
    /// caller (`run.rs`'s own `Action::DistillFate` arm) uses this to know
    /// whether to resubscribe its own `events` local, the same follow-up
    /// `Effect::Resumed`'s arm performs (this fate never goes through
    /// `commands::execute`, so there is no `Effect` to carry that signal
    /// through -- see this module's own doc). A no-op (returns `false`) when
    /// no `/distill` modal is open.
    pub(super) async fn spawn_from_distill(&mut self) -> bool {
        let Mode::Distill(modal) = &self.state.mode else {
            return false;
        };
        let briefing = modal.briefing.clone();
        let old_session = modal.old_session;
        let old_context_tokens = modal.old_context_tokens;
        // One funnel, shared with `/new` -- see `commands::new_session_
        // spec`'s own doc for why a second, independently hand-rolled copy
        // of this exact shape is exactly the drift a dogfood round caught.
        let spec = commands::new_session_spec(&self.state);
        let new_handle = match self.conway.new_session(spec).await {
            Ok(handle) => handle,
            Err(e) => {
                self.state
                    .fail_distill(format!("could not start a fresh agent: {e}"));
                return false;
            }
        };
        let new_root = new_handle.root();
        if let Err(e) = new_handle.prompt(briefing).await {
            self.state
                .fail_distill(format!("could not deliver the briefing: {e}"));
            return false;
        }
        let new_context_tokens = wait_for_context_tokens(&new_handle, new_root).await;
        self.handle = new_handle;
        // See this method's own doc, "this used to swap `self.handle`
        // alone." `role_pin`/`model_pin` are captured before the reset
        // (which otherwise drops them -- `AppState::reset_for_new_session`'s
        // own doc on why that shared funnel cannot pick one answer for both
        // `/new` and `/resume`) and written back after, mirroring `/new`'s
        // own arm exactly.
        let role_pin = self.state.role_pin.clone();
        let model_pin = self.state.model_pin.clone();
        self.state.reset_for_new_session(new_root, &self.conway);
        self.state.role_pin = role_pin;
        self.state.model_pin = model_pin;
        self.refresh_session_head().await;
        self.state.transcript.push(Entry::Notice {
            text: format!(
                "{} -- the previous session {old_session} is still resumable (`/resume \
                 {old_session}` or `conway sessions list`)",
                cost_notice(old_context_tokens, new_context_tokens)
            ),
        });
        true
    }

    /// `e` on the `/distill` modal: suspends the terminal and opens
    /// `modal.briefing` in `$VISUAL`/`$EDITOR`/`vi`, exactly like `Ctrl-G`'s
    /// `Action::OpenExternalEditor` arm does for the main input line and
    /// `App::apply_skill_proposal_edit_action` does for that modal -- the
    /// ONE piece of this modal's own key handling that needs a live
    /// terminal. `editor_command` is caller-resolved, mirroring `app/
    /// skill_propose.rs`'s own identical doc for why. A no-op when no
    /// `/distill` modal is open.
    pub(super) fn apply_distill_edit_action<B: ratatui::backend::Backend>(
        &mut self,
        terminal: &mut ratatui::Terminal<B>,
        editor_command: &str,
    ) {
        let Mode::Distill(modal) = &self.state.mode else {
            return;
        };
        let current = modal.briefing.clone();
        let outcome = super::editor::edit_prompt_externally(terminal, &current, editor_command);
        match outcome {
            super::editor::EditorOutcome::Replace(text) => {
                self.state.apply_distill_edit(text);
            }
            super::editor::EditorOutcome::Unchanged => {}
            super::editor::EditorOutcome::Failed { notice } => {
                self.state.transcript.push(Entry::Notice { text: notice });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Board item `01M1YVKQ6ABQDWYSA7CEF20WKG`'s own primary tests: `/distill`
    //! driven against a REAL `Conway`/`ScriptedBackend` fixture (P-15: a
    //! fixture that would genuinely exercise the path under test, not a
    //! hand-built value that bypasses it), mirroring `skill_propose.rs`'s own
    //! test shape closely.

    use std::sync::Arc;
    use std::time::Duration;

    use conway::test_support::base_config_at;
    use conway::{AgentId, AgentResult, Conway, ResultStatus, SessionFilter, SessionId};
    use conway_core::content::ContentBlock;
    use conway_core::error::BackendError;
    use conway_core::ids::BackendId;
    use conway_testkit::{text_response, ScriptedBackend, ScriptedTurn};

    use super::super::fixtures::minimal_cli;
    use super::{App, DistillDone};
    use crate::tui::input::{self, Action};
    use crate::tui::state::{DistillFate, Entry, Mode};
    use crate::tui::test_support::key;

    /// See `ask.rs`'s own `HANG_TIMEOUT` doc for why every bound in this
    /// module is a hang detector, not a promptness assertion.
    const HANG_TIMEOUT: Duration = Duration::from_secs(10);

    const FIXED_BRIEFING: &str = "BRIEFING: the task is X, decided Y, open question Z, files: a.rs";

    /// A real, fully in-memory `Conway` whose backend scripts the ROOT's own
    /// reply to one planted message, then the ephemeral distill child's ONE
    /// turn as a fixed briefing reply -- `cwd` points at a fresh tempdir so
    /// nothing here touches a real `.conway` directory.
    fn conway_with_fixed_briefing(cwd: &std::path::Path) -> (Conway, Arc<ScriptedBackend>) {
        let backend = Arc::new(
            ScriptedBackend::new(vec![
                ScriptedTurn::Respond(text_response("ok")),
                ScriptedTurn::Respond(text_response(FIXED_BRIEFING)),
            ])
            .with_id(BackendId::new("fake")),
        );
        let conway = conway::test_support::test_builder(base_config_at(cwd))
            .with_backend(backend.clone())
            .build()
            .expect("build should succeed with every port injected");
        (conway, backend)
    }

    /// Drains `app.distill_rx` for the ONE `DistillDone` reply `/distill`
    /// produces.
    async fn recv_done(app: &mut App) -> super::DistillDone {
        tokio::time::timeout(
            HANG_TIMEOUT,
            app.distill_rx
                .as_mut()
                .expect("distill_rx is set by App::new")
                .recv(),
        )
        .await
        .expect("the spawned distill task must reply promptly")
        .expect("distill_tx's sender half is alive for the duration")
    }

    /// Waits until a REAL `generate` call reaches the backend whose own
    /// assembled request contains `needle` in a text block -- the only way
    /// to see the actual request text a session's own turn sent (unlike
    /// `ContextReport`, which carries token counts and provenance tags, never
    /// the text itself). Mirrors `skill_propose.rs`'s own `a_written_skill_
    /// appears_in_the_next_sessions_skill_index_end_to_end` test's identical
    /// `backend.calls()` inspection.
    async fn wait_for_request_containing(backend: &ScriptedBackend, needle: &str) -> Vec<String> {
        tokio::time::timeout(HANG_TIMEOUT, async {
            loop {
                for req in backend.calls() {
                    let texts: Vec<String> = req
                        .segments
                        .iter()
                        .flat_map(|s| s.content.iter())
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect();
                    if texts.iter().any(|t| t.contains(needle)) {
                        return texts;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no request ever reached the backend containing {needle:?}"))
    }

    /// **The primary end-to-end test.** `/distill` against a fixed-briefing
    /// fixture opens a modal with that exact briefing; `Enter`
    /// (`App::spawn_from_distill`) then starts a fresh agent whose own first
    /// assembled request contains the briefing and NOTHING of the old
    /// session's own transcript (acceptance criterion 2), swaps the app loop
    /// onto it, and leaves a notice reporting old-vs-new context size
    /// (acceptance criterion 3).
    ///
    /// **Dogfood round 4 widened this to the TUI STATE itself, not just the
    /// child's own request/session log.** The original version of this test
    /// only ever checked the latter -- which is exactly why a round of
    /// dogfooding was needed to catch `App::spawn_from_distill` swapping
    /// `self.handle` alone: the status bar, `/context`, and the transcript
    /// all kept showing the OLD session, and the cost notice's own new-side
    /// figure read 0, while this test's old assertions were already green.
    /// The assertions below pin all four: `state.focused_agent`/
    /// `self.handle.id()` genuinely follow the swap, the old session's
    /// planted message is gone from `state.transcript` (not merely absent
    /// from the new agent's own opening REQUEST, which the pre-existing
    /// assertion below already covers), `/context`'s own default target
    /// (`state.focused_agent`) resolves to the new agent, and the cost
    /// notice's new-side figure is genuinely non-zero.
    #[tokio::test]
    async fn distill_then_enter_spawns_a_fresh_agent_with_only_the_briefing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (conway, backend) = conway_with_fixed_briefing(dir.path());
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let old_root = app.handle.root();
        let old_session_id = app.handle.id();

        // Plant a distinguishing message in the OLD session before
        // distilling, so "nothing of the old transcript" is a genuine,
        // non-vacuous assertion (P-15) rather than trivially true of an
        // agent that never said anything.
        app.submit("SECRET_OLD_MESSAGE".to_string())
            .await
            .expect("submit should not error");
        tokio::time::timeout(HANG_TIMEOUT, async {
            while !app.handle.awaiting_prompt(old_root) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the planted message's own turn must settle");

        app.submit("/distill".to_string())
            .await
            .expect("submit should not error");
        assert!(app.state.distill_in_flight);

        let done = recv_done(&mut app).await;
        app.apply_distill_done(done);
        assert!(!app.state.distill_in_flight);
        match &app.state.mode {
            Mode::Distill(modal) => assert_eq!(modal.briefing, FIXED_BRIEFING),
            other => panic!("expected Mode::Distill, got {other:?}"),
        }

        let spawned = app.spawn_from_distill().await;
        assert!(spawned, "the spawn must succeed against a working fixture");
        assert_ne!(
            app.handle.root(),
            old_root,
            "the app loop must be pointed at a genuinely NEW root agent"
        );
        assert!(
            matches!(app.state.mode, Mode::Normal),
            "the modal must close after a successful spawn, got {:?}",
            app.state.mode
        );

        // Dogfood round 4 (SIGNIFICANT): `AppState` itself must follow the
        // swap, not just `self.handle`. `focused_agent` is what the status
        // bar, `/context`'s bare form, and every other focused-agent read
        // in this crate actually consult.
        assert_eq!(
            app.state.focused_agent,
            app.handle.root(),
            "state.focused_agent must be the NEW root -- the status bar must \
             never keep showing a focus left over from the old session"
        );
        assert_ne!(
            app.handle.id(),
            old_session_id,
            "self.handle must be pointed at a genuinely different SESSION, \
             not merely a different root agent id"
        );
        assert!(
            !app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::User(text) if text.contains("SECRET_OLD_MESSAGE")
            )),
            "the old session's planted message must not survive in \
             state.transcript after the swap: {:?}",
            app.state.transcript
        );

        // `/context`'s own default target (`state.focused_agent`, read by
        // `commands::execute`'s bare `SlashCommand::Context` arm) must
        // resolve to the NEW agent -- never a stale focus left over from
        // the old session.
        let context_report = app
            .handle
            .context_report(app.state.focused_agent)
            .await
            .expect("context_report for the newly focused agent must succeed");
        assert_eq!(
            context_report.agent_id,
            app.handle.root(),
            "/context's own default target must be the new agent"
        );

        // LOAD-BEARING (acceptance criterion 2): the new agent's own first
        // assembled request contains the briefing, and NOTHING of the old
        // transcript.
        let texts = wait_for_request_containing(&backend, FIXED_BRIEFING).await;
        assert!(
            !texts.iter().any(|t| t.contains("SECRET_OLD_MESSAGE")),
            "the new agent's opening context must carry nothing of the old transcript: {texts:?}"
        );

        // Acceptance criterion 3: the notice reports the cost of the move,
        // with a genuinely non-zero new-side figure (dogfood round 4: this
        // used to read 0 for a context that in fact held the whole
        // briefing -- see `wait_for_context_tokens`'s own doc).
        let cost_entry = app
            .state
            .transcript
            .iter()
            .find_map(|e| match e {
                Entry::Notice { text } if text.contains("spawned a fresh agent") => {
                    Some(text.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!(
                    "the notice must report old-vs-new context size: {:?}",
                    app.state.transcript
                )
            });
        assert!(
            cost_entry.contains("token"),
            "expected a token figure in the cost notice: {cost_entry}"
        );
        assert!(
            !cost_entry.contains("-> 0 tokens"),
            "the cost notice's new-context figure must never read 0 for a context that \
             holds the whole briefing: {cost_entry}"
        );
    }

    /// Dogfood round 4 finding (B4a ruling: "a session-scoped grant belongs
    /// to the session that earned it ... must not carry into a fresh
    /// session via /new, /resume or /distill"). THIS `Conway`'s own
    /// `PermissionBroker` is shared across every session it ever starts,
    /// for the lifetime of the process -- `App::spawn_from_distill` swapping
    /// `self.handle` onto the freshly spawned agent must not keep honoring
    /// whatever the OLD (soon-to-be-left) session earned interactively.
    /// Drives the grant through the REAL production entry point an
    /// operator's own "always" answer to a shell-prefix prompt uses
    /// (`Conway::grant_session_shell_prefix`, the same call `app/run.rs`'s
    /// own `[p]`-answer arm makes) and asserts on the BROKER's own review
    /// surface (`Conway::active_shell_prefix_grants`), not a TUI-side
    /// mirror -- the exact shape dogfood finding 3 reported (`01M42SYT`
    /// inheriting `01M42SK5`'s own `python3 -m pytest -q` grant).
    #[tokio::test]
    async fn distill_then_enter_revokes_a_session_scoped_shell_prefix_grant_the_old_session_earned()
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let (conway, _backend) = conway_with_fixed_briefing(dir.path());
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let old_root = app.handle.root();

        let installed = app.conway.grant_session_shell_prefix(
            "python3 -m pytest -q".to_string(),
            conway::PermissionScope::Session,
            old_root,
        );
        assert!(
            installed,
            "grant_session_shell_prefix must install the grant"
        );
        assert_eq!(
            app.conway.active_shell_prefix_grants(),
            vec![(
                "python3 -m pytest -q".to_string(),
                conway::GrantScope::Session
            )],
            "sanity: the grant must actually be live on the broker before /distill"
        );

        app.submit("/distill".to_string())
            .await
            .expect("submit should not error");
        let done = recv_done(&mut app).await;
        app.apply_distill_done(done);

        let spawned = app.spawn_from_distill().await;
        assert!(spawned, "the spawn must succeed against a working fixture");

        assert_eq!(
            app.conway.active_shell_prefix_grants(),
            Vec::new(),
            "a session-scoped grant the OLD session earned must not survive /distill's own \
             Enter on THIS Conway's shared PermissionBroker"
        );
    }

    /// `/distill <instructions>` folds the operator's own text into the
    /// fork child's directive -- acceptance criterion 2's own example,
    /// `/distill focus on the failing test`.
    #[tokio::test]
    async fn distill_with_instructions_folds_them_into_the_forked_childs_directive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (conway, backend) = conway_with_fixed_briefing(dir.path());
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        app.submit("/distill focus on the failing test".to_string())
            .await
            .expect("submit should not error");
        let _done = recv_done(&mut app).await;

        let texts = wait_for_request_containing(&backend, "focus on the failing test").await;
        assert!(
            texts
                .iter()
                .any(|t| t.contains("focus on the failing test")),
            "the operator's own instructions must reach the forked child's directive: {texts:?}"
        );
    }

    /// `Esc` discards -- the file is never written, the briefing is never
    /// delivered, and -- P-15 -- no new session is ever created at all.
    #[tokio::test]
    async fn esc_discards_and_spawns_no_agent_at_all() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (conway, _backend) = conway_with_fixed_briefing(dir.path());
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        app.submit("/distill".to_string())
            .await
            .expect("submit should not error");
        let done = recv_done(&mut app).await;
        app.apply_distill_done(done);
        assert!(matches!(app.state.mode, Mode::Distill(_)));

        let before = app
            .conway
            .sessions(SessionFilter {
                include_ephemeral: true,
                ..Default::default()
            })
            .await
            .expect("sessions() should succeed");

        let action =
            input::handle_key(&mut app.state, key(ratatui::crossterm::event::KeyCode::Esc));
        assert_eq!(action, Action::DistillFate(DistillFate::Discard));
        // Mirrors `run.rs`'s own `DistillFate::Discard` arm exactly.
        app.state.close_distill();

        assert!(matches!(app.state.mode, Mode::Normal));

        // LOAD-BEARING (P-15): no new session was ever created -- the
        // session tree's own ephemeral-inclusive listing is unchanged, not
        // merely "the modal closed."
        let after = app
            .conway
            .sessions(SessionFilter {
                include_ephemeral: true,
                ..Default::default()
            })
            .await
            .expect("sessions() should succeed");
        assert_eq!(
            before.len(),
            after.len(),
            "/distill's Esc must never spawn a new session: before {before:?}, after {after:?}"
        );
    }

    // -----------------------------------------------------------------
    // Review findings (board item `01M1YVKQ6ABQDWYSA7CEF20WKG`): the
    // failure branches of `App::apply_distill_done` had zero coverage
    // before this -- only the happy path and `Esc` were ever driven.
    // -----------------------------------------------------------------

    /// `handle.fork`'s own `ensure_agent_in_session` check (a REAL
    /// production validation, not a hand-built error) fails outright when
    /// `parent` is not an agent this session's tree actually knows --
    /// driven here by focusing an `AgentId` that was never forked/spawned
    /// at all, exactly the one case `run_distill`'s own `Err` arm covers.
    /// `apply_distill_done` must surface it as a plain `"/distill failed: \
    /// ..."` notice, never a panic or a silently-opened modal.
    #[tokio::test]
    async fn fork_failure_posts_a_distill_failed_notice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (conway, _backend) = conway_with_fixed_briefing(dir.path());
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        app.state.focused_agent = AgentId::new();

        app.submit("/distill".to_string())
            .await
            .expect("submit should not error");
        let done = recv_done(&mut app).await;
        app.apply_distill_done(done);

        assert!(!app.state.distill_in_flight);
        assert!(
            matches!(app.state.mode, Mode::Normal),
            "a failed fork must never open the briefing modal: {:?}",
            app.state.mode
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text.starts_with("/distill failed: ")
            )),
            "{:?}",
            app.state.transcript
        );
    }

    /// The forked child's own single turn can fail at the backend (here a
    /// `ScriptedTurn::Fail`, folded by the agent loop into a terminal
    /// `ResultStatus::Failed` -- see `conway/tests/intent.rs`'s identical
    /// `classify_propagates_a_failed_intent_turn...` comment for the same
    /// fold), which reaches `apply_distill_done` as `Ok(AgentResult { \
    /// status: Failed, .. })`, not an `Err` -- the OTHER way a `/distill`
    /// can produce no usable briefing, distinct from the fork-failure test
    /// above.
    #[tokio::test]
    async fn non_completed_status_posts_no_usable_briefing_notice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let backend = Arc::new(
            ScriptedBackend::new(vec![ScriptedTurn::Fail(BackendError::Transport {
                detail: "connection reset".to_string(),
            })])
            .with_id(BackendId::new("fake")),
        );
        let conway: Conway = conway::test_support::test_builder(base_config_at(dir.path()))
            .with_backend(backend)
            .build()
            .expect("build should succeed with every port injected");
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        app.submit("/distill".to_string())
            .await
            .expect("submit should not error");
        let done = recv_done(&mut app).await;
        app.apply_distill_done(done);

        assert!(!app.state.distill_in_flight);
        assert!(
            matches!(app.state.mode, Mode::Normal),
            "a non-Completed result must never open the briefing modal: {:?}",
            app.state.mode
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text }
                    if text.contains("/distill produced no usable briefing")
            )),
            "{:?}",
            app.state.transcript
        );
    }

    /// A `Completed` fork that replies with whitespace only is just as "no
    /// usable briefing" as an outright failure. The runtime replaces blank
    /// trailing text with its `(no output; ...)` placeholder, so this pins
    /// `AgentResult::has_output` rather than a bare `.trim().is_empty()`,
    /// which the placeholder slips past.
    #[tokio::test]
    async fn whitespace_only_summary_posts_no_usable_briefing_notice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let backend = Arc::new(
            ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("   \n  "))])
                .with_id(BackendId::new("fake")),
        );
        let conway: Conway = conway::test_support::test_builder(base_config_at(dir.path()))
            .with_backend(backend)
            .build()
            .expect("build should succeed with every port injected");
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        app.submit("/distill".to_string())
            .await
            .expect("submit should not error");
        let done = recv_done(&mut app).await;
        app.apply_distill_done(done);

        assert!(
            matches!(app.state.mode, Mode::Normal),
            "a whitespace-only summary must never open the briefing modal: {:?}",
            app.state.mode
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text }
                    if text.contains("/distill produced no usable briefing")
            )),
            "{:?}",
            app.state.transcript
        );
    }

    /// Review finding (board item `01M1YVKQ6ABQDWYSA7CEF20WKG`, SIGNIFICANT,
    /// belt and braces alongside `/new`'s own refusal): a `DistillDone`
    /// whose `old_session` no longer matches `self.handle.id()` (the
    /// session changed under it -- the exact shape `/new` swapping
    /// `self.handle` produces) must be discarded with a notice, never
    /// opened as a briefing modal over a session the operator has already
    /// left. Driven with a hand-built `DistillDone` (P-15 would normally
    /// object, but there is no production path left to reach this once
    /// `/new`'s own refusal above is in place -- this is the belt-and-
    /// braces half the finding asked for, proven directly at the unit the
    /// finding named).
    #[tokio::test]
    async fn a_result_from_a_session_no_longer_current_is_discarded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (conway, _backend) = conway_with_fixed_briefing(dir.path());
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let stale_session = SessionId::new();
        let done = DistillDone {
            result: Ok(AgentResult::new(
                AgentId::new(),
                stale_session,
                ResultStatus::Completed,
                FIXED_BRIEFING,
            )),
            old_session: stale_session,
            old_context_tokens: None,
            // The CURRENT generation -- this test proves the `old_session`
            // check, not the generation check, so the reply must not be
            // dropped by that earlier guard first.
            generation: app.state.distill_generation,
        };
        app.apply_distill_done(done);

        assert!(!app.state.distill_in_flight);
        assert!(
            matches!(app.state.mode, Mode::Normal),
            "a stale-session result must never open the briefing modal: {:?}",
            app.state.mode
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text }
                    if text.contains(&stale_session.to_string()) && text.contains("discarded")
            )),
            "{:?}",
            app.state.transcript
        );
    }

    /// Board item `01M1YVKQ6ABQDWYSA7CEF20WKG` (minor finding 5): `Ctrl-C`
    /// abandons an in-flight `/distill` exactly like `App::abandon_ask`
    /// abandons an in-flight `/ask` -- clears `distill_in_flight`, posts a
    /// "distill abandoned" notice -- and the fork's own eventual
    /// `DistillDone` (still running: this feature's own disclosed gap,
    /// `App::abandon_distill`'s own doc, has no child handle to cancel) is
    /// silently dropped when it finally arrives, never opening the modal
    /// and never posting a second notice.
    #[tokio::test]
    async fn ctrl_c_abandons_an_in_flight_distill_and_drops_its_late_reply() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (conway, _backend) = conway_with_fixed_briefing(dir.path());
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        app.submit("/distill".to_string())
            .await
            .expect("submit should not error");
        assert!(app.state.distill_in_flight);
        let generation_before_abandon = app.state.distill_generation;

        let mut last_ctrl_c = None;
        app.handle_ctrl_c(&mut last_ctrl_c)
            .await
            .expect("handle_ctrl_c should not error");

        assert!(
            !app.state.distill_in_flight,
            "Ctrl-C must abandon the in-flight /distill immediately"
        );
        assert_ne!(
            app.state.distill_generation, generation_before_abandon,
            "abandoning must bump the generation so the fork's own late reply is stale"
        );
        assert!(
            app.state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::Notice { text } if text == "distill abandoned")),
            "{:?}",
            app.state.transcript
        );

        let done = recv_done(&mut app).await;
        app.apply_distill_done(done);

        assert!(
            matches!(app.state.mode, Mode::Normal),
            "a late reply after abandonment must never open the briefing modal: {:?}",
            app.state.mode
        );
        assert!(
            !app.state.distill_in_flight,
            "a stale-generation reply must never flip distill_in_flight back on"
        );
        assert_eq!(
            app.state
                .transcript
                .iter()
                .filter(|e| matches!(e, Entry::Notice { text } if text.contains("distill")))
                .count(),
            1,
            "exactly the one abandon notice, nothing from the late reply: {:?}",
            app.state.transcript
        );
    }

    /// Board item `01M1YVKQ6ABQDWYSA7CEF20WKG` (review finding, round 2):
    /// the exact scenario `distill_generation` replaced `distill_abandoned`
    /// for -- `/distill`, `Ctrl-C`, `/distill` again, with the SECOND
    /// fork's reply delivered first. A bare `distill_abandoned: bool` could
    /// not express this: resetting it at the second `/distill`'s own start
    /// would have let the FIRST fork's still-outstanding reply later open as
    /// though it were current (it arrives with the flag already cleared),
    /// while leaving it set would have dropped the SECOND fork's own
    /// genuine reply too. Hand-built `DistillDone` values give this test
    /// control over arrival ORDER (the one thing this scenario is about)
    /// without depending on which of two real forks' tasks happens to
    /// schedule first -- `apply_distill_done`'s own generation comparison is
    /// exactly as exercised either way.
    #[tokio::test]
    async fn a_second_distill_after_an_abandon_wins_and_the_firsts_late_reply_is_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (conway, _backend) = conway_with_fixed_briefing(dir.path());
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        // First `/distill`: mirrors `commands::execute`'s own `SlashCommand::
        // Distill` arm (set `distill_in_flight`, bump the generation) without
        // actually forking -- this test is about `apply_distill_done`'s own
        // generation gate, not about `run_distill`'s fork mechanics (already
        // covered by the tests above).
        app.state.distill_in_flight = true;
        app.state.distill_generation = app.state.distill_generation.wrapping_add(1);
        let first_generation = app.state.distill_generation;

        // `Ctrl-C`: the real method, bumping the generation again and
        // clearing `distill_in_flight`.
        app.abandon_distill();
        assert!(!app.state.distill_in_flight);

        // Second `/distill`, immediately after -- same shape as the first.
        app.state.distill_in_flight = true;
        app.state.distill_generation = app.state.distill_generation.wrapping_add(1);
        let second_generation = app.state.distill_generation;
        assert_ne!(
            first_generation, second_generation,
            "the two forks must carry genuinely different generations"
        );

        const FIRST_BRIEFING: &str = "BRIEFING: from the FIRST, abandoned fork";
        const SECOND_BRIEFING: &str = "BRIEFING: from the SECOND, current fork";

        // The SECOND fork's reply arrives FIRST -- it is the one currently
        // wanted, so it must open the modal.
        app.apply_distill_done(DistillDone {
            result: Ok(AgentResult::new(
                AgentId::new(),
                app.handle.id(),
                ResultStatus::Completed,
                SECOND_BRIEFING,
            )),
            old_session: app.handle.id(),
            old_context_tokens: None,
            generation: second_generation,
        });
        assert!(
            !app.state.distill_in_flight,
            "the current generation's own reply must clear distill_in_flight"
        );
        match &app.state.mode {
            Mode::Distill(modal) => assert_eq!(modal.briefing, SECOND_BRIEFING),
            other => panic!("expected Mode::Distill with the second briefing, got {other:?}"),
        }

        // The FIRST (abandoned) fork's late reply arrives SECOND -- stale,
        // and must be dropped silently: no change to `distill_in_flight`
        // (already `false`, from the current generation's own reply just
        // above -- this must not be confused with "nothing to clear"), and
        // the modal must keep showing the SECOND briefing, not be clobbered
        // or closed by the first's.
        app.apply_distill_done(DistillDone {
            result: Ok(AgentResult::new(
                AgentId::new(),
                app.handle.id(),
                ResultStatus::Completed,
                FIRST_BRIEFING,
            )),
            old_session: app.handle.id(),
            old_context_tokens: None,
            generation: first_generation,
        });
        assert!(
            !app.state.distill_in_flight,
            "a stale reply must never touch distill_in_flight"
        );
        match &app.state.mode {
            Mode::Distill(modal) => assert_eq!(
                modal.briefing, SECOND_BRIEFING,
                "the stale first fork's reply must never replace the current modal"
            ),
            other => panic!("the modal must still show the second briefing, got {other:?}"),
        }
    }
}
