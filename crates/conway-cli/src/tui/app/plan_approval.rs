//! Leaving `Plan` mode (board item `01M1YVPJW9W43HMM8WEF34N4RZ`, operator
//! ruling `01M48TK7QKBBSJ1FQ4XPX9DTF1`): the one funnel every path that
//! changes [`conway::PermissionMode`] away from `Plan` already shares --
//! `Action::CyclePermissionMode` (`app/run.rs`), reached identically from
//! `Shift-Tab` (`Mode::Normal`) and from `/settings`' own `permission_mode`
//! row (`input.rs`'s `LEAF_PERMISSION_MODE` arm) -- turns into a moment,
//! not a bare toggle, the instant the FOCUSED agent has said something
//! worth reviewing while `Plan` was gating it.
//!
//! [`App::maybe_offer_plan_approval`] is that one decision point, called
//! from `Action::CyclePermissionMode`'s own arm BEFORE it ever writes the
//! broker: when it returns `true`, the modal is now open and the caller
//! must not touch `permission_mode` itself -- the actual switch happens
//! later, from [`App::approve_plan`], only once the operator decides.
//! [`App::approve_plan`] is reached from every one of the modal's own
//! forward paths (`Enter`, with no custom text, targeting `Prompt`;
//! confirmed `a`, with no custom text, targeting `AutoAllow`; and `e`,
//! with the edited text, targeting `Prompt`) -- mirroring `app/distill.rs`'s
//! `App::spawn_from_distill`/`App::apply_distill_edit_action` split, except
//! this feature's `e` is a complete, one-shot action (edit THEN send, no
//! second `Enter` needed): the acceptance criterion itself ("send the
//! edited text as the user turn along with the switch") describes one
//! action, not two.
//!
//! # Two destinations, not one (review round)
//!
//! The ORIGINAL version of this feature let `Enter` approve into whatever
//! the mode cycle's own order happened to leave `Plan` by (`AutoAllow`,
//! always, in practice) with no confirmation step and no stated
//! destination. The operator ruling above replaces that with two
//! EXPLICITLY labelled forward paths -- see [`crate::tui::state::
//! PlanApprovalModal`]'s own doc for the full key map and
//! `input.rs::handle_plan_approval_key` for the two-keystroke confirm `a`
//! needs before `AutoAllow` actually commits. [`App::approve_plan`] itself
//! no longer decides which mode to switch to at all: it reads whatever
//! [`crate::tui::state::PlanApprovalModal::next_mode`] already carries,
//! written by the key handler at the moment the operator actually chose.
//!
//! # Turn in flight: defer, never switch silently (review round)
//!
//! [`App::maybe_offer_plan_approval`] still declines to open the modal
//! while the focused agent's own turn is genuinely in flight (a streaming
//! turn has no settled "last assistant message" yet). The ORIGINAL version
//! then fell through to the silent switch, as if nothing had ever been
//! said in `Plan` -- the review found this indistinguishable, from the
//! operator's seat, from the mode changing out from under a turn that was
//! still composing its own plan. [`App::defer_leaving_plan`] replaces that
//! fallthrough: `Plan` stays in force, a short notice says so, and
//! [`App::maybe_reveal_deferred_plan_approval`] (called from `app/run.rs`'s
//! own event-apply arm, on the SAME `TurnFinished`/`TurnAborted`-for-the-
//! focused-agent check that already sets `AppState::plan_turn_seen`) is
//! what actually decides once the turn settles.
//!
//! # Ordering: the mode write happens before anything else
//!
//! `Plan`'s own guarantee is that no tool call runs until the mode has
//! actually changed. [`App::approve_plan`] writes `conway::Conway::
//! set_permission_mode`/`AppState::permission_mode` as its very first
//! step, before the system note and before the approval turn are sent --
//! so by the time the model could possibly see either of those and start a
//! new tool-calling turn, the broker has already stopped gating on `Plan`.
//!
//! # Busy-input dedup
//!
//! [`App::approve_plan`] sends the approval/edited text through `App::
//! submit` -- the SAME, single funnel an ordinary typed prompt goes
//! through, already handling `busy_input`'s queue/steer/interrupt branches
//! without ever delivering a message twice. No new delivery mechanism is
//! introduced here.

use conway::PermissionMode;

use super::App;
use crate::tui::state::{Entry, Mode, PlanApprovalModal};

/// The fixed, short user turn [`App::approve_plan`] sends on a plain
/// `Enter` (no custom text) -- acceptance criterion 1's own exact wording,
/// so the model knows the mode actually changed.
const PLAN_APPROVED_TEXT: &str = "Plan approved; proceed.";

/// The `reason` tag [`App::approve_plan`]'s `SessionHandle::
/// append_system_note` call stamps onto the recorded note's own
/// `Provenance::SystemNote { reason }` -- a short, stable identifier,
/// mirroring `conway_plugin_goal::NOTE_REASON`'s own convention for the
/// identical field.
const PLAN_APPROVAL_NOTE_REASON: &str = "plan_approved";

/// The last [`crate::tui::state::Entry::Assistant`] text in `transcript`,
/// if any -- "that agent's last assistant message," read straight from the
/// SAME render model the transcript pane itself shows, never a second,
/// independent fetch. `transcript` is only ever the FOCUSED agent's own
/// entries (`AppState::apply`'s own doc: "`apply` is only ever fed the
/// currently subscribed agent's own stream"), so this is correct for
/// whichever agent is focused with no `AgentId` parameter needed.
pub(super) fn last_assistant_text(transcript: &[Entry]) -> Option<String> {
    transcript.iter().rev().find_map(|entry| match entry {
        Entry::Assistant { text, .. } => Some(text.clone()),
        _ => None,
    })
}

impl App {
    /// Called from `Action::CyclePermissionMode`'s own arm the instant the
    /// cycle is about to leave `Plan`, and from [`Self::
    /// maybe_reveal_deferred_plan_approval`] once a deferred leave's turn
    /// settles -- in both cases ONLY once the caller has already confirmed
    /// nothing of the focused agent's own is in flight right now.
    ///
    /// Opens the plan-approval modal, returning `true`, when `state.
    /// plan_turn_seen` names the focused agent -- it produced at least one
    /// assistant turn while `Plan` was gating it (`AppState::
    /// plan_turn_seen`'s own doc names the one event-apply site that
    /// records this, per agent). Returns `false`, touching nothing, when it
    /// does not -- the caller falls through to the bare, silent switch.
    ///
    /// The modal opens with [`PlanApprovalModal::next_mode`] seeded to
    /// [`PermissionMode::Prompt`] as a PLACEHOLDER only -- see that field's
    /// own doc: `input.rs::handle_plan_approval_key` overwrites it with
    /// whichever destination the operator actually chooses before
    /// [`Self::approve_plan`] ever reads it, so the seed value itself
    /// carries no meaning.
    pub(super) fn maybe_offer_plan_approval(&mut self) -> bool {
        if !self
            .state
            .plan_turn_seen
            .contains(&self.state.focused_agent)
        {
            return false;
        }
        // Defensive: both call sites already check this themselves before
        // calling in, but a turn is live state that can change out from
        // under either of them between that check and this one, and
        // showing a plan next to a turn that might still be composing its
        // own continuation is exactly what the review round's defer fix
        // exists to prevent.
        if self.handle.turn_in_progress(self.state.focused_agent) {
            return false;
        }
        let plan = last_assistant_text(&self.state.transcript).unwrap_or_default();
        self.state.offer_plan_approval(PlanApprovalModal {
            plan,
            next_mode: PermissionMode::Prompt,
            autoallow_confirm_armed: false,
        });
        true
    }

    /// Board item `01M1YVPJW9W43HMM8WEF34N4RZ` (review round, finding 2):
    /// `Action::CyclePermissionMode`'s own arm calls this instead of
    /// falling through to the silent switch when the focused agent's turn
    /// is genuinely in flight at the exact instant the operator tries to
    /// leave `Plan`. `Plan` stays in force -- NEITHER the broker nor the
    /// display mirror is touched -- and [`AppState::plan_approval_
    /// deferred`] records that the operator already asked, so [`Self::
    /// maybe_reveal_deferred_plan_approval`] knows to act once the turn
    /// settles.
    ///
    /// Idempotent: a second call while already deferred (the operator
    /// pressing `Shift-Tab`/the `/settings` row again before the turn
    /// finishes) does nothing further, including pushing a second notice
    /// -- the operator has already been told once.
    pub(super) fn defer_leaving_plan(&mut self) {
        if self.state.plan_approval_deferred {
            return;
        }
        self.state.plan_approval_deferred = true;
        self.state.transcript.push(Entry::Notice {
            text: "will show the plan when this turn finishes".to_string(),
        });
    }

    /// Board item `01M1YVPJW9W43HMM8WEF34N4RZ` (review round, finding 2):
    /// called from `app/run.rs`'s own event-apply arm on the SAME
    /// `TurnFinished`/`TurnAborted`-for-the-focused-agent condition that
    /// already records `AppState::plan_turn_seen` -- called UNCONDITIONALLY
    /// there; this is where the "only if actually deferred" check lives, so
    /// the ordinary (non-deferred) case stays a cheap no-op.
    ///
    /// A no-op unless [`AppState::plan_approval_deferred`] is set. When it
    /// is, this clears it and re-runs the ordinary decision (`Self::
    /// maybe_offer_plan_approval`) now that the turn has genuinely settled.
    /// The defensive fallback -- `maybe_offer_plan_approval` returning
    /// `false` even though a turn just finished in `Plan` (unreachable in
    /// practice: the SAME event that calls this one also just recorded the
    /// evidence it needs, in `app/run.rs`'s own arm, before this runs) --
    /// still completes the deferred request rather than leaving the
    /// operator stuck in `Plan` with no visible reason: it performs the
    /// identical silent switch to `AutoAllow` the ORIGINAL turn-in-flight
    /// fallthrough always used.
    pub(super) fn maybe_reveal_deferred_plan_approval(&mut self) {
        if !self.state.plan_approval_deferred {
            return;
        }
        self.state.plan_approval_deferred = false;
        if !self.maybe_offer_plan_approval() {
            self.conway.set_permission_mode(PermissionMode::AutoAllow);
            self.state.permission_mode = PermissionMode::AutoAllow;
            self.state.plan_turn_seen.clear();
        }
    }

    /// `Enter`/confirmed-`a` (`custom_text: None`) or a completed `e`-edit
    /// (`custom_text: Some(edited)`) on the plan-approval modal: switches
    /// the mode to whichever destination [`PlanApprovalModal::next_mode`]
    /// carries (written by `input.rs::handle_plan_approval_key` at the
    /// moment the operator chose it), records the approval as a
    /// `system_note` naming that SAME destination, then sends the turn --
    /// in that order (see this module's own doc, "Ordering"). A no-op if
    /// no plan-approval modal is open (a stale action after a race is not
    /// expected in practice, but this never panics on it, mirroring
    /// `App::spawn_from_distill`'s own guard).
    pub(super) async fn approve_plan(&mut self, custom_text: Option<String>) {
        let Mode::PlanApproval(modal) = &self.state.mode else {
            return;
        };
        let next_mode = modal.next_mode;
        // ORDERING (acceptance criterion): the mode write is the FIRST
        // thing this method does -- see this module's own doc.
        self.conway.set_permission_mode(next_mode);
        self.state.permission_mode = next_mode;
        // See `AppState::plan_turn_seen`'s own doc: the mode has now
        // genuinely left `Plan`, so every agent's recorded evidence is
        // spent -- there is no partial "still in Plan" state once the mode
        // has genuinely changed.
        self.state.plan_turn_seen.clear();
        let agent = self.state.focused_agent;
        self.state.close_plan_approval();
        let note = format!("plan approved by operator, mode → {}", next_mode.label());
        if let Err(e) = self
            .handle
            .append_system_note(agent, note, PLAN_APPROVAL_NOTE_REASON)
            .await
        {
            self.state.transcript.push(Entry::Notice {
                text: format!("plan approval note failed to record: {e}"),
            });
        }
        let text = custom_text.unwrap_or_else(|| PLAN_APPROVED_TEXT.to_string());
        // See this module's own doc, "Busy-input dedup" -- `App::submit` is
        // the one funnel, never duplicated here. It never returns `Err`
        // (every internal failure it can hit already becomes a transcript
        // `Notice` on its own), so the result is deliberately dropped,
        // mirroring `run_distill`'s own "nothing left to notify" send site.
        let _ = self.submit(text).await;
    }

    /// `e` on the plan-approval modal: suspends the terminal and opens
    /// `modal.plan` in `$VISUAL`/`$EDITOR`/`vi`, exactly like `Ctrl-G`'s
    /// `Action::OpenExternalEditor` arm does for the main input line and
    /// `App::apply_distill_edit_action` does for that modal's own briefing
    /// -- the ONE piece of this modal's own key handling that needs a live
    /// terminal. `editor_command` is caller-resolved, mirroring `app/
    /// distill.rs`'s identical doc for why.
    ///
    /// **Unlike `apply_distill_edit_action`, a `Replace` outcome does not
    /// merely update the modal -- it goes straight to `Self::
    /// approve_plan`, sending the edited text as the turn immediately.**
    /// See `Action::PlanApprovalEdit`'s own doc for why this modal's `e` is
    /// a complete, one-shot action rather than a second "now press Enter"
    /// step. `Unchanged`/`Failed` leave the modal open exactly as `/distill`
    /// 's own arm does -- nothing was sent, nothing switched.
    ///
    /// **Operator ruling `01M48TK7QKBBSJ1FQ4XPX9DTF1`: `e` always approves
    /// into `Prompt`, never `AutoAllow`**, regardless of whatever
    /// placeholder [`PlanApprovalModal::next_mode`] happened to carry
    /// beforehand (an edited plan is reviewed text, not the confirmed
    /// "never ask me again" act `a` is) -- set explicitly, right before
    /// `Self::approve_plan` reads it, rather than relying on the modal
    /// never having been armed toward `AutoAllow` first.
    pub(super) async fn apply_plan_approval_edit_action<B: ratatui::backend::Backend>(
        &mut self,
        terminal: &mut ratatui::Terminal<B>,
        editor_command: &str,
    ) {
        let Mode::PlanApproval(modal) = &self.state.mode else {
            return;
        };
        let current = modal.plan.clone();
        let outcome = super::editor::edit_prompt_externally(terminal, &current, editor_command);
        match outcome {
            super::editor::EditorOutcome::Replace(text) => {
                if let Mode::PlanApproval(modal) = &mut self.state.mode {
                    modal.next_mode = PermissionMode::Prompt;
                }
                self.approve_plan(Some(text)).await;
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
    //! Board item `01M1YVPJW9W43HMM8WEF34N4RZ`'s own primary tests, driven
    //! against REAL `Conway` fixtures (P-15: a fixture that genuinely
    //! exercises the path under test), mirroring `distill.rs`'s own test
    //! shape closely.

    use std::sync::Arc;
    use std::time::Duration;

    use conway::{AgentId, Conway, PermissionMode};
    use conway_core::ids::BackendId;
    use conway_testkit::{ScriptedBackend, ScriptedTurn};
    use futures::StreamExt;

    use super::super::fixtures::{echo_conway, minimal_cli};
    use super::{App, PLAN_APPROVAL_NOTE_REASON, PLAN_APPROVED_TEXT};
    use crate::tui::input::{self, Action};
    use crate::tui::state::{Entry, Mode, PlanApprovalFate, PlanApprovalModal};
    use crate::tui::test_support::key;

    /// See `ask.rs`'s own `HANG_TIMEOUT` doc for why every bound in this
    /// module is a hang detector, not a promptness assertion.
    const HANG_TIMEOUT: Duration = Duration::from_secs(10);

    /// Drains `events` until a `TurnFinished` for `agent` arrives (bounded),
    /// applying each envelope to `state` exactly like `App::run`'s own event
    /// arm, AND -- mirroring that SAME arm's own `plan_turn_seen` update
    /// (`AppState::plan_turn_seen`'s own doc) -- setting it when that
    /// `TurnFinished` lands while `state.permission_mode` is `Plan`. This
    /// crate's test modules cannot drive `App::run`'s real `select!` loop
    /// (no terminal), so a test that needs this one side effect reproduces
    /// it directly, the same established idiom `app/plugin_cmd.rs`'s own
    /// event-draining test helper already uses for the ordinary `apply`
    /// half.
    async fn drive_turn_to_finish(
        events: &mut conway::EventStream,
        state: &mut crate::tui::state::AppState,
        agent: AgentId,
    ) {
        tokio::time::timeout(HANG_TIMEOUT, async {
            loop {
                let env = events
                    .next()
                    .await
                    .expect("the event stream must not end mid-turn");
                state.apply(&env);
                if matches!(&env.event, conway::Event::TurnFinished { .. }) && env.agent == agent {
                    if state.permission_mode == PermissionMode::Plan {
                        state.plan_turn_seen.insert(agent);
                    }
                    return;
                }
            }
        })
        .await
        .expect("the turn must finish promptly")
    }

    /// A `Conway` whose one scripted turn never resolves -- the deterministic
    /// way to observe `SessionHandle::turn_in_progress` read `true` from a
    /// test, mirroring `conway_testkit::ScriptedTurn::Pending`'s own doc
    /// ("the calling agent stays mid-turn until the test runtime tears its
    /// task down").
    fn conway_with_a_turn_that_never_finishes() -> Conway {
        let backend = Arc::new(
            ScriptedBackend::new(vec![ScriptedTurn::Pending]).with_id(BackendId::new("fake")),
        );
        conway::test_support::test_builder(conway::test_support::base_config())
            .with_backend(backend)
            .build()
            .expect("build should succeed with every port injected")
    }

    /// The primary end-to-end proof: a REAL turn, run while `Plan` gates
    /// it, is what `AppState::plan_turn_seen` records -- and `App::
    /// maybe_offer_plan_approval` opens the modal with exactly that agent's
    /// own last reply as the plan, carrying the placeholder `next_mode`
    /// (`Prompt`) until the operator actually chooses a destination.
    #[tokio::test]
    async fn maybe_offer_plan_approval_opens_with_the_focused_agents_last_reply() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        app.conway.set_permission_mode(PermissionMode::Plan);
        app.state.permission_mode = PermissionMode::Plan;

        let mut events = app.handle.events();
        app.submit("hello".to_string())
            .await
            .expect("submit should not error");
        drive_turn_to_finish(&mut events, &mut app.state, root).await;

        assert!(
            app.state.plan_turn_seen.contains(&root),
            "a real turn finishing for the focused agent while Plan was active must be \
             recorded"
        );

        let opened = app.maybe_offer_plan_approval();
        assert!(
            opened,
            "an agent that said something in Plan must get the modal"
        );
        match &app.state.mode {
            Mode::PlanApproval(modal) => {
                assert_eq!(
                    modal.plan, "hello",
                    "the plan must be the agent's own last reply"
                );
                assert_eq!(
                    modal.next_mode,
                    PermissionMode::Prompt,
                    "the seeded placeholder -- overwritten once the operator chooses"
                );
                assert!(!modal.autoallow_confirm_armed);
            }
            other => panic!("expected Mode::PlanApproval, got {other:?}"),
        }
    }

    /// No assistant turn ever happened in `Plan` -- `maybe_offer_plan_
    /// approval` must return `false` and leave `mode` untouched, so the
    /// caller falls through to the silent switch exactly as it always did.
    #[tokio::test]
    async fn maybe_offer_plan_approval_returns_false_with_no_assistant_turn_seen() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        assert!(
            app.state.plan_turn_seen.is_empty(),
            "sanity: nothing has run yet"
        );

        let opened = app.maybe_offer_plan_approval();

        assert!(!opened);
        assert!(
            matches!(app.state.mode, Mode::Normal),
            "no modal must open when nothing was ever said in Plan, got: {:?}",
            app.state.mode
        );
    }

    /// Cross-agent leak must stay impossible (review round, finding 4):
    /// evidence recorded for a DIFFERENT agent must never open the modal
    /// while some other agent is focused.
    #[tokio::test]
    async fn maybe_offer_plan_approval_never_leaks_another_agents_evidence() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let other_agent = AgentId::new();
        app.state.plan_turn_seen.insert(other_agent);
        assert_ne!(app.state.focused_agent, other_agent);

        assert!(
            !app.maybe_offer_plan_approval(),
            "evidence recorded for a different agent must never open the modal for the \
             focused one"
        );
        assert!(matches!(app.state.mode, Mode::Normal));
    }

    /// Review round, finding 4: a focus round-trip (away, then back) must
    /// NOT lose the focused agent's own recorded evidence -- the exact
    /// defect the old bare-`bool` version of `plan_turn_seen` had.
    #[tokio::test]
    async fn maybe_offer_plan_approval_survives_a_focus_round_trip() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        app.state.plan_turn_seen.insert(root);

        let elsewhere = AgentId::new();
        app.state.focus_agent(elsewhere);
        assert!(
            !app.maybe_offer_plan_approval(),
            "a different agent, now focused, has no evidence of its own"
        );

        app.state.focus_agent(root);
        assert!(
            app.state.plan_turn_seen.contains(&root),
            "the root agent's own evidence must survive glancing away and back"
        );
        assert!(
            app.maybe_offer_plan_approval(),
            "and the modal must still open for it once refocused"
        );
    }

    /// The turn-in-flight race (`Action::CyclePermissionMode`'s own arm,
    /// `app/run.rs`): even with `plan_turn_seen` already recorded, a turn
    /// genuinely in flight for the focused agent RIGHT NOW must make this
    /// return `false` too -- see `Self::maybe_offer_plan_approval`'s own
    /// doc for why ("a turn still streaming has no settled 'last assistant
    /// message' yet").
    #[tokio::test]
    async fn maybe_offer_plan_approval_returns_false_while_a_turn_is_in_flight() {
        let conway = conway_with_a_turn_that_never_finishes();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        app.conway.set_permission_mode(PermissionMode::Plan);
        app.state.permission_mode = PermissionMode::Plan;
        // Evidence from an EARLIER turn, so the only thing this test
        // proves is the in-flight guard, not the evidence check above.
        app.state.plan_turn_seen.insert(root);

        app.submit("hello".to_string())
            .await
            .expect("submit should not error");
        tokio::time::timeout(HANG_TIMEOUT, async {
            while !app.handle.turn_in_progress(root) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the turn must genuinely start");

        let opened = app.maybe_offer_plan_approval();

        assert!(
            !opened,
            "a turn in flight right now must fall through to the silent switch"
        );
        assert!(matches!(app.state.mode, Mode::Normal));
        assert!(
            app.state.plan_turn_seen.contains(&root),
            "the evidence itself must survive this attempt, for the NEXT one"
        );
    }

    /// **Enter: ordering, the system note, and the approval turn.** The
    /// mode write happens before anything else (`App::approve_plan`'s own
    /// doc, "Ordering") -- proven here by checking both the broker and the
    /// display mirror read the NEW mode once `approve_plan` returns, with
    /// the fixed approval text and the recorded `system_note` both landed
    /// in the real session log. Operator ruling `01M48TK7QKBBSJ1FQ4XPX9DTF1`:
    /// a bare `Enter` approves into `Prompt`, never `AutoAllow`.
    #[tokio::test]
    async fn enter_switches_the_mode_records_a_system_note_and_sends_the_fixed_turn() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        app.conway.set_permission_mode(PermissionMode::Plan);
        app.state.permission_mode = PermissionMode::Plan;

        let mut events = app.handle.events();
        app.submit("here is my plan".to_string())
            .await
            .expect("submit should not error");
        drive_turn_to_finish(&mut events, &mut app.state, root).await;
        assert!(app.maybe_offer_plan_approval());
        // This test exercises the ordinary decision keys, not the
        // typeahead guard `offer_plan_approval` just armed (see
        // `input.rs`'s own `plan_approval_typeahead_guard_*` tests for
        // that) -- clear the stamp so the `Enter` below is not itself
        // swallowed by the window it opened.
        app.state.plan_approval_armed_at = None;

        let action = input::handle_key(
            &mut app.state,
            key(ratatui::crossterm::event::KeyCode::Enter),
        );
        assert_eq!(action, Action::PlanApprovalFate(PlanApprovalFate::Approve));

        app.approve_plan(None).await;

        assert_eq!(
            app.state.permission_mode,
            PermissionMode::Prompt,
            "the display mirror must read the NEW mode"
        );
        assert_eq!(
            app.conway.permission_mode(),
            PermissionMode::Prompt,
            "the broker -- the authority -- must read the NEW mode too"
        );
        assert!(
            matches!(app.state.mode, Mode::Normal),
            "the modal must close after a successful approval, got: {:?}",
            app.state.mode
        );
        assert!(
            app.state.plan_turn_seen.is_empty(),
            "the evidence is spent once the mode has genuinely left Plan"
        );

        let records = app
            .handle
            .transcript(root)
            .await
            .expect("transcript should read back");
        assert!(
            records.iter().any(
                |r| matches!(r, conway::LogRecord::UserTurn { text, .. } if text == PLAN_APPROVED_TEXT)
            ),
            "the fixed approval turn must reach the real session log: {records:?}"
        );
        assert!(
            records.iter().any(|r| matches!(
                r,
                conway::LogRecord::SystemNote { text, reason, .. }
                    if reason == PLAN_APPROVAL_NOTE_REASON
                        && text.contains("plan approved by operator")
                        && text.contains("prompt")
            )),
            "the approval must be recorded as a system_note naming the ACTUAL destination \
             (prompt, for a bare Enter), so /context and sessions show carry it: {records:?}"
        );
    }

    /// Operator ruling `01M48TK7QKBBSJ1FQ4XPX9DTF1`: a confirmed `a`
    /// approves into `AutoAllow` instead, and the system note names THAT
    /// destination.
    #[tokio::test]
    async fn confirmed_a_switches_the_mode_to_autoallow_and_the_note_names_it() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        app.conway.set_permission_mode(PermissionMode::Plan);
        app.state.permission_mode = PermissionMode::Plan;

        let mut events = app.handle.events();
        app.submit("here is my plan".to_string())
            .await
            .expect("submit should not error");
        drive_turn_to_finish(&mut events, &mut app.state, root).await;
        assert!(app.maybe_offer_plan_approval());
        app.state.plan_approval_armed_at = None;

        let arm = input::handle_key(
            &mut app.state,
            key(ratatui::crossterm::event::KeyCode::Char('a')),
        );
        assert_eq!(arm, Action::None, "the first `a` must only arm");
        let confirm = input::handle_key(
            &mut app.state,
            key(ratatui::crossterm::event::KeyCode::Char('a')),
        );
        assert_eq!(confirm, Action::PlanApprovalFate(PlanApprovalFate::Approve));

        app.approve_plan(None).await;

        assert_eq!(app.state.permission_mode, PermissionMode::AutoAllow);
        assert_eq!(app.conway.permission_mode(), PermissionMode::AutoAllow);

        let records = app
            .handle
            .transcript(root)
            .await
            .expect("transcript should read back");
        assert!(
            records.iter().any(|r| matches!(
                r,
                conway::LogRecord::SystemNote { text, reason, .. }
                    if reason == PLAN_APPROVAL_NOTE_REASON
                        && text.contains("AUTO-ALLOW")
            )),
            "a confirmed `a` must record its own real destination, AUTO-ALLOW: {records:?}"
        );
    }

    /// `e`: the edited text is sent as the turn instead of the fixed one,
    /// along with the same mode switch -- one action, not edit-then-a-
    /// second-Enter. A real child process (a shell script) is spawned
    /// against the modal's own plan text, mirroring `app/skill_propose.
    /// rs`'s own `e_opens_the_editor_and_the_edit_is_applied_through_the_
    /// real_run_rs_path` test shape exactly (a second, independent copy of
    /// its `write_test_script`/`test_terminal` helpers -- private to that
    /// module's own test tree).
    ///
    /// The modal is seeded with `next_mode: AutoAllow` here deliberately --
    /// operator ruling `01M48TK7QKBBSJ1FQ4XPX9DTF1` says `e` always
    /// approves into `Prompt` regardless of whatever placeholder was there
    /// beforehand, and this proves the override, not merely a value that
    /// was already correct.
    #[tokio::test]
    #[cfg(unix)]
    async fn e_sends_the_edited_text_as_the_turn_along_with_the_switch() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        app.conway.set_permission_mode(PermissionMode::Plan);
        app.state.permission_mode = PermissionMode::Plan;
        app.state.offer_plan_approval(PlanApprovalModal {
            plan: "original plan".to_string(),
            next_mode: PermissionMode::AutoAllow,
            autoallow_confirm_armed: false,
        });

        let script = write_test_script(
            "appends",
            "#!/bin/sh\necho ' -- edited' >> \"$1\"\nexit 0\n",
        );
        let mut terminal = test_terminal();

        apply_plan_approval_edit_action_retrying(
            &mut app,
            &mut terminal,
            script.to_str().expect("utf8 path"),
        )
        .await;
        let _ = std::fs::remove_file(&script);

        assert!(
            matches!(app.state.mode, Mode::Normal),
            "a completed edit is a complete action -- the modal must close, got: {:?}",
            app.state.mode
        );
        assert_eq!(
            app.state.permission_mode,
            PermissionMode::Prompt,
            "`e` always approves into Prompt, overriding whatever placeholder was seeded"
        );

        let records = app
            .handle
            .transcript(root)
            .await
            .expect("transcript should read back");
        assert!(
            records.iter().any(|r| matches!(
                r,
                conway::LogRecord::UserTurn { text, .. }
                    if text == "original plan -- edited"
            )),
            "the EDITED text must be sent as the turn, not the fixed approval message: \
             {records:?}"
        );
    }

    /// `Esc`: stays in `Plan`, nothing switches, nothing is sent.
    #[tokio::test]
    async fn esc_stays_in_plan_and_sends_nothing() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        app.conway.set_permission_mode(PermissionMode::Plan);
        app.state.permission_mode = PermissionMode::Plan;
        app.state.offer_plan_approval(PlanApprovalModal {
            plan: "a plan".to_string(),
            next_mode: PermissionMode::AutoAllow,
            autoallow_confirm_armed: false,
        });
        // This test exercises the ordinary `Esc` decision, not the
        // typeahead guard `offer_plan_approval` just armed.
        app.state.plan_approval_armed_at = None;

        let action =
            input::handle_key(&mut app.state, key(ratatui::crossterm::event::KeyCode::Esc));
        assert_eq!(action, Action::PlanApprovalFate(PlanApprovalFate::Discard));
        app.state.close_plan_approval();

        assert!(matches!(app.state.mode, Mode::Normal));
        assert_eq!(
            app.state.permission_mode,
            PermissionMode::Plan,
            "Esc must never switch the mode"
        );
        assert_eq!(app.conway.permission_mode(), PermissionMode::Plan);

        let records = app
            .handle
            .transcript(root)
            .await
            .expect("transcript should read back");
        assert!(
            !records
                .iter()
                .any(|r| matches!(r, conway::LogRecord::UserTurn { .. })),
            "Esc must send nothing at all: {records:?}"
        );
    }

    /// **Busy-input dedup.** `App::approve_plan` sends through `App::
    /// submit`, the SAME funnel an ordinary prompt uses -- a busy focused
    /// agent in `queue` mode withholds the approval turn exactly once,
    /// never delivers it twice, mirroring `app.rs`'s own `busy_queue_
    /// mode_withholds_the_message_instead_of_sending_it` test shape
    /// exactly (`activity` set directly to simulate "a turn is in flight",
    /// the same established idiom that test's own doc explains).
    #[tokio::test]
    async fn approve_plan_while_busy_queues_the_turn_through_the_ordinary_funnel_once() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        app.conway.set_permission_mode(PermissionMode::Plan);
        app.state.permission_mode = PermissionMode::Plan;
        app.state.busy_input = crate::tui::config::BusyInputMode::Queue;
        app.state.activity = crate::tui::state::Activity::Thinking;
        app.state.offer_plan_approval(PlanApprovalModal {
            plan: "the plan".to_string(),
            next_mode: PermissionMode::AutoAllow,
            autoallow_confirm_armed: false,
        });

        app.approve_plan(None).await;

        assert_eq!(
            app.state.permission_mode,
            PermissionMode::AutoAllow,
            "the mode switch must still happen even while the agent is busy"
        );
        assert_eq!(
            app.state.held_prompts,
            vec![(root, PLAN_APPROVED_TEXT.to_string())],
            "a busy focused agent must withhold the approval turn via the SAME queue path an \
             ordinary prompt uses, never send it directly"
        );
        assert_eq!(
            app.state
                .transcript
                .iter()
                .filter(|e| matches!(
                    e,
                    Entry::QueuedUser { text, .. } if text == PLAN_APPROVED_TEXT
                ))
                .count(),
            1,
            "the approval turn must be queued exactly once, never delivered twice: {:?}",
            app.state.transcript
        );
        let records = app
            .handle
            .transcript(root)
            .await
            .expect("transcript should read back");
        assert!(
            !records.iter().any(
                |r| matches!(r, conway::LogRecord::UserTurn { text, .. } if text == PLAN_APPROVED_TEXT)
            ),
            "a withheld approval turn must never reach the real session log while queued: \
             {records:?}"
        );
    }

    /// Board item `01M1YVPJW9W43HMM8WEF34N4RZ` (review round, finding 2):
    /// `App::defer_leaving_plan` is idempotent -- a second call while
    /// already deferred must push no second notice.
    #[tokio::test]
    async fn defer_leaving_plan_is_idempotent() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        app.defer_leaving_plan();
        app.defer_leaving_plan();

        assert!(app.state.plan_approval_deferred);
        assert_eq!(
            app.state
                .transcript
                .iter()
                .filter(|e| matches!(
                    e,
                    Entry::Notice { text } if text.contains("will show the plan")
                ))
                .count(),
            1,
            "a second deferral while already deferred must not push a second notice: {:?}",
            app.state.transcript
        );
    }

    /// End to end: a turn in flight defers (NOT a silent switch -- `Plan`
    /// stays in force), and once that turn settles,
    /// `App::maybe_reveal_deferred_plan_approval` opens the modal with the
    /// now-settled reply, exactly as if the operator had tried again after
    /// the turn finished.
    #[tokio::test]
    async fn a_deferred_leave_reveals_the_modal_once_the_turn_settles() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        app.conway.set_permission_mode(PermissionMode::Plan);
        app.state.permission_mode = PermissionMode::Plan;

        let mut events = app.handle.events();
        app.submit("here is my plan".to_string())
            .await
            .expect("submit should not error");

        // The operator tries to leave `Plan` while the turn above is still
        // in flight -- the review round's own defer, not a silent switch.
        app.defer_leaving_plan();
        assert!(app.state.plan_approval_deferred);
        assert_eq!(
            app.state.permission_mode,
            PermissionMode::Plan,
            "Plan must stay in force while deferred"
        );

        // A second ask while still deferred changes nothing further.
        app.defer_leaving_plan();
        assert_eq!(
            app.state
                .transcript
                .iter()
                .filter(
                    |e| matches!(e, Entry::Notice { text } if text.contains("will show the plan"))
                )
                .count(),
            1
        );

        drive_turn_to_finish(&mut events, &mut app.state, root).await;
        app.maybe_reveal_deferred_plan_approval();

        assert!(
            !app.state.plan_approval_deferred,
            "the deferred request must be consumed once revealed"
        );
        match &app.state.mode {
            Mode::PlanApproval(modal) => {
                assert_eq!(modal.plan, "here is my plan");
            }
            other => panic!("expected the deferred modal to open, got {other:?}"),
        }
        assert_eq!(
            app.state.permission_mode,
            PermissionMode::Plan,
            "revealing the modal must not itself switch the mode -- only approving does"
        );
    }

    // -----------------------------------------------------------------
    // `e`'s own test-only editor plumbing -- a second, independent copy of
    // `app/editor.rs`'s/`app/skill_propose.rs`'s identical helpers (both
    // private to their own test trees).
    // -----------------------------------------------------------------

    fn write_test_script(name: &str, body: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "conway-plan-approval-edit-test-{}-{}-{name}.sh",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, body).expect("write must succeed against a writable temp path");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)
                .expect("metadata must succeed")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).expect("chmod must succeed");
        }
        path
    }

    fn test_terminal() -> ratatui::Terminal<ratatui::backend::TestBackend> {
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 5))
            .expect("TestBackend construction cannot fail")
    }

    /// Retries on the ETXTBSY race `app/editor.rs`'s own module doc
    /// describes (a fresh script written and exec'd moments later, unsafe
    /// under `cargo test`'s parallelism) -- same bounded shape as that
    /// module's own retry wrapper (and `app/skill_propose.rs`'s identical
    /// copy), adapted to this method's own `async` signature.
    async fn apply_plan_approval_edit_action_retrying<B: ratatui::backend::Backend>(
        app: &mut App,
        terminal: &mut ratatui::Terminal<B>,
        editor_command: &str,
    ) {
        const ATTEMPTS: u32 = 10;
        for attempt in 0..ATTEMPTS {
            let notices_before = app.state.transcript.len();
            app.apply_plan_approval_edit_action(terminal, editor_command)
                .await;
            let busy = app.state.transcript[notices_before..]
                .iter()
                .any(|e| matches!(e, Entry::Notice { text } if text.contains("Text file busy")));
            if !busy {
                return;
            }
            if attempt + 1 < ATTEMPTS {
                tokio::time::sleep(Duration::from_millis(20 * u64::from(attempt + 1))).await;
            }
        }
        panic!(
            "apply_plan_approval_edit_action lost the ETXTBSY race {ATTEMPTS} times in a row \
             for {editor_command:?}; that is no longer a race, investigate rather than raising \
             the bound"
        );
    }
}
