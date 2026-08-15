//! The three mutually-exclusive modal-bearing surfaces
//! ([`Mode::AwaitingPermission`], [`Mode::AskModal`],
//! [`Mode::IntentConfirm`]) and their shared park/promote priority queue
//! ([`AppState::promote_next_surface`]): the `/ask` single-turn modal (B5:
//! [`AskModal`], [`AskFate`]), the permission-prompt surface itself
//! ([`AppState::offer_prompt`], [`AppState::resolve_current_prompt`]), and
//! the two informational overlays that share its mutual-exclusion rules
//! without being a [`Mode`] variant ([`AppState::open_help`],
//! [`AppState::open_settings`] -- see [`Mode`]'s own doc for why).
//!
//! The NL intent confirmation card's own state (`IntentConfirm`,
//! `IntentChoice`) and its `offer`/`close`/`begin_edit` lifecycle live on
//! [`super::AppState`] itself, not here -- `crates/conway-cli/tests/
//! intent_confirm.rs` pins their definitions to `state.rs`'s own text as a
//! source-level surface check, so this seam only owns
//! [`AppState::take_pending_intent_confirm`] (the drain the quit path
//! uses) and the `Mode::IntentConfirm` variant/park-queue plumbing.

use super::*;

/// The `/ask` modal's state (B5): one answered ephemeral fork-ask waiting
/// for the user to choose its fate. The modal opens only once the child's
/// single turn has COMPLETED (`app.rs` drives `SessionHandle::ask` +
/// `TurnHandle::text` to the finished reply, then `offer_ask_modal`), so
/// `answer` is always the final reply text -- never a pending placeholder.
/// `child` is the ephemeral fork child's [`AgentId`] (from
/// `TurnHandle::agent`), the value all three fates' facade ops take.
/// `error` is `Some` only after a fate attempt FAILED -- the modal stays
/// open with the error shown (the user still must choose; a failed fate
/// never silently falls through to another one).
#[derive(Debug, Clone, PartialEq)]
pub struct AskModal {
    pub question: String,
    pub child: AgentId,
    pub answer: String,
    pub error: Option<String>,
}

/// One of the three forced fates closing the `/ask` modal (B5) -- exactly one
/// of these runs; there is no fourth way out (quitting with the modal open
/// purges, wired in `app.rs`'s quit path). Each maps to exactly one facade op
/// (`commands::apply_ask_fate`): `Fork` -> `Conway::promote` (B3: keep -- the
/// node loses its `(ephemeral)` marking and becomes a session in its own
/// right), `PullIn` -> `Conway::pull_in` (B4: the question+answer merge into
/// the parent's own log, child purged), `Discard` -> `Conway::purge` (the
/// explicit exception to provenance being permanent and visible: the answer is
/// thrown away).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskFate {
    Fork,
    PullIn,
    Discard,
}

/// `Normal` (the input line submits a prompt or a `/command`) or
/// `AwaitingPermission` (the input line is inert; `y`/`a`/`n`/`Esc` resolve
/// the pending prompt -- see `input.rs`). Only one prompt is shown at a
/// time; concurrent requests queue in `pending_prompts` (module notes:
/// "concurrent requests queue in arrival order").
pub enum Mode {
    Normal,
    AwaitingPermission(PendingPrompt),
    /// The `/ask` single-turn modal (B5). While this is the mode, the input
    /// line is inert and `/agents` is neither visible nor available --
    /// `input.rs::handle_ask_modal_key` swallows every key except the three
    /// fate keys (`f`/`p`/`Esc`) and the quit keys (`Ctrl-C`/`Ctrl-D`,
    /// which purge before exiting -- see `app.rs`). A permission prompt
    /// arriving while the modal is open queues in `queued_prompts` exactly
    /// as it does behind another prompt, and an ask answer arriving while a
    /// permission prompt is showing parks in `pending_ask_modal` until the
    /// prompt resolves -- the two modals never stack.
    AskModal(AskModal),
    /// The NL intent confirmation card (C2). While this is the mode, the
    /// input line is inert and `/agents` is neither visible nor available
    /// -- `input.rs::handle_intent_confirm_key` swallows every key except
    /// `Enter` (confirm), `e` (edit -- drops the classified prompt into the
    /// input line and closes the card) and `Esc` (manual fallback), plus
    /// the quit keys (`Ctrl-C`/`Ctrl-D`, which pass through -- unlike the
    /// `/ask` modal there is no live child to purge, since the card opens
    /// BEFORE any agent is created). A permission prompt arriving while the
    /// card is open queues in `queued_prompts` exactly as it does behind
    /// another prompt, and an intent card arriving while a permission prompt
    /// OR an `/ask` modal is showing parks in `pending_intent_confirm` until
    /// the surface clears -- the three modal-bearing surfaces
    /// (`AwaitingPermission`, `AskModal`, `IntentConfirm`) never stack.
    IntentConfirm(IntentConfirm),
}

impl std::fmt::Debug for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mode::Normal => write!(f, "Normal"),
            Mode::AwaitingPermission(p) => {
                write!(f, "AwaitingPermission({})", p.request.call_id)
            }
            Mode::AskModal(m) => write!(f, "AskModal(child={})", m.child),
            Mode::IntentConfirm(ic) => {
                write!(f, "IntentConfirm(recipe={:?})", ic.intent.recipe)
            }
        }
    }
}

impl AppState {
    /// Opens the `/ask` modal for one answered ask (B5), parking it in
    /// `pending_ask_modal` instead whenever another modal surface (a
    /// permission prompt, an intent confirmation card, or another ask
    /// modal) currently owns `mode` -- mirroring [`Self::offer_prompt`]'s
    /// own queue-if-busy behavior, so the modal-bearing surfaces never
    /// stack. `Self::promote_next_surface` opens the parked modal once
    /// the surface ahead of it clears.
    pub fn offer_ask_modal(&mut self, modal: AskModal) {
        if matches!(self.mode, Mode::Normal) {
            self.mode = Mode::AskModal(modal);
            // V1: a freshly opened modal starts scrolled to its own top --
            // see `Self::modal_scroll`'s own doc on why one field can serve
            // every modal-bearing surface.
            self.modal_scroll = 0;
        } else {
            self.pending_ask_modal = Some(modal);
        }
    }

    /// Drains a modal parked in `pending_ask_modal` (B5's M1 fix). Used by
    /// `app.rs::purge_open_ask_modal` so the quit path also discards a
    /// modal that was queued behind a permission prompt when the ask
    /// completed -- without this the parked modal's child leaks as residue
    /// (reaped by the next startup sweep, but still a fourth way out of an
    /// undecided ask). Returns the parked modal if one was waiting, else
    /// `None`; either way `pending_ask_modal` is cleared.
    pub fn take_pending_ask_modal(&mut self) -> Option<AskModal> {
        self.pending_ask_modal.take()
    }

    /// Closes the `/ask` modal after a fate SUCCEEDED (B5 --
    /// `commands::apply_ask_fate`'s success path), promoting the next
    /// parked/queued surface via `Self::promote_next_surface` (a queued
    /// permission prompt, a parked intent card, or a parked ask -- in that
    /// priority order). A no-op when no ask modal is open.
    pub fn close_ask_modal(&mut self) {
        if !matches!(self.mode, Mode::AskModal(_)) {
            return;
        }
        self.mode = Mode::Normal;
        self.promote_next_surface();
    }

    /// Records a fate attempt's FAILURE on the open modal (B5 --
    /// `commands::apply_ask_fate`'s error path): the modal STAYS OPEN with
    /// the error shown, so the user still must choose a fate -- a failed
    /// fate never silently falls through to another one. A no-op when no
    /// modal is open.
    pub fn fail_ask_modal(&mut self, error: String) {
        if let Mode::AskModal(modal) = &mut self.mode {
            modal.error = Some(error);
        }
    }

    /// Drains a card parked in `pending_intent_confirm`. Used by
    /// `app.rs`'s quit path so a card parked behind a permission prompt
    /// when the user quits does not leave a dangling classified intent --
    /// unlike the `/ask` modal there is no live child to purge (the card
    /// opens BEFORE any agent is created), so "draining" here just means
    /// dropping it on the floor. Returns the parked card if one was
    /// waiting, else `None`; either way `pending_intent_confirm` is
    /// cleared.
    pub fn take_pending_intent_confirm(&mut self) -> Option<IntentConfirm> {
        self.pending_intent_confirm.take()
    }

    /// The shared "what surfaces gets promoted next after a modal/prompt
    /// closes" logic (C2 generalizes B5's two-surface version to three).
    /// Called with `mode` already reset to `Mode::Normal` by the caller
    /// ([`Self::close_ask_modal`], [`Self::close_intent_confirm`],
    /// [`Self::resolve_current_prompt`]). Priority order:
    /// 1. A queued permission prompt ([`Self::queued_prompts`]) -- the
    ///    gate's pending prompts are always the highest-priority surface
    ///    (a tool call is waiting on a decision).
    /// 2. A parked `/ask` modal ([`Self::pending_ask_modal`]) -- an ask
    ///    that completed while a prompt was showing.
    /// 3. A parked intent card ([`Self::pending_intent_confirm`]) -- a
    ///    classify that completed while a prompt or an ask was showing.
    /// 4. Nothing -- `mode` stays `Normal`.
    ///
    /// Exactly one surface (at most) is promoted per call; the next call
    /// happens when THAT surface closes.
    pub(super) fn promote_next_surface(&mut self) {
        // V1: every branch below resets `modal_scroll` -- the newly
        // promoted surface starts scrolled to its own top, never carrying
        // over wherever a PREVIOUS, unrelated surface's content happened to
        // be scrolled (see `Self::modal_scroll`'s own doc on why one field
        // safely serves all of them).
        if let Some(next) = self.queued_prompts.pop_front() {
            self.mode = Mode::AwaitingPermission(next);
            self.modal_scroll = 0;
            // A scope chosen for the PREVIOUS prompt must not leak into
            // this one -- see `Self::permission_grant_scope`'s own doc.
            self.permission_grant_scope = conway::PermissionScope::Session;
            return;
        }
        if let Some(modal) = self.pending_ask_modal.take() {
            self.mode = Mode::AskModal(modal);
            self.modal_scroll = 0;
            return;
        }
        if let Some(card) = self.pending_intent_confirm.take() {
            self.mode = Mode::IntentConfirm(card);
            self.modal_scroll = 0;
        }
    }

    /// Enqueues a freshly arrived prompt from the gate channel, promoting it
    /// to `mode` immediately if nothing is currently showing.
    pub fn offer_prompt(&mut self, prompt: PendingPrompt) {
        if matches!(self.mode, Mode::Normal) {
            self.mode = Mode::AwaitingPermission(prompt);
            // A freshly promoted prompt starts scrolled to the top of its
            // own command -- never carries over wherever a PREVIOUS,
            // unrelated prompt's overlay happened to be scrolled.
            self.modal_scroll = 0;
            // ...and at the DEFAULT grant scope -- a narrower scope chosen
            // for an earlier, unrelated prompt must not silently apply to
            // this one (see `Self::permission_grant_scope`'s own doc).
            self.permission_grant_scope = conway::PermissionScope::Session;
        } else {
            self.queued_prompts.push_back(prompt);
        }
    }

    /// Cycles the scope the prompt's remembered-grant keys (`a`/`p`) grant
    /// at: `Session` -> `Agent` -> `AgentSubtree` -> `Session`. Bound to
    /// the prompt's `s` key in `input.rs`; the overlay states the current
    /// scope in words next to the grant keys so the operator never grants
    /// narrower or broader than they can see.
    pub fn cycle_permission_grant_scope(&mut self) {
        self.permission_grant_scope = match self.permission_grant_scope {
            conway::PermissionScope::Session => conway::PermissionScope::Agent,
            conway::PermissionScope::Agent => conway::PermissionScope::AgentSubtree,
            // `AgentSubtree`, and any future variant (`PermissionScope` is
            // `#[non_exhaustive]`): back to the default. A future scope
            // sorts itself into the cycle only by a deliberate edit here,
            // never by accident.
            _ => conway::PermissionScope::Session,
        };
    }

    /// The agent whose call the current permission prompt is asking about,
    /// if one is pending. This -- NOT `focused_agent` -- is the agent a
    /// per-agent or per-subtree grant must be recorded against: the broker
    /// narrows such a grant to the GRANTING agent's identity, and the call
    /// being decided belongs to the requester in the prompt, which need
    /// not be the agent whose transcript the operator is looking at.
    pub fn pending_permission_agent(&self) -> Option<conway::AgentId> {
        let Mode::AwaitingPermission(pending) = &self.mode else {
            return None;
        };
        Some(pending.request.agent_id)
    }

    /// Resolves the currently-shown prompt (if any) and promotes the next
    /// queued one, if there is one.
    pub fn resolve_current_prompt(&mut self, decision: conway::PermissionDecision) {
        let Mode::AwaitingPermission(_) = &self.mode else {
            return;
        };
        let Mode::AwaitingPermission(prompt) = std::mem::replace(&mut self.mode, Mode::Normal)
        else {
            unreachable!()
        };
        prompt.resolve(decision);
        // C2: the prompt queue drains first (highest priority -- a tool
        // call is waiting on a decision); then a parked `/ask` modal (B5);
        // then a parked intent card (C2). [`Self::promote_next_surface`]
        // encodes that fixed priority order so the three modal-bearing
        // surfaces never stack and never drift out of sync across the
        // close/resolve call sites.
        self.promote_next_surface();
    }

    /// Opens the `/help` keybinding overlay (T7). See [`Self::help_open`]'s
    /// own doc for why this is a plain flag flip rather than a `mode`
    /// transition/park -- `commands.rs`'s `SlashCommand::Help` arm can only
    /// ever reach this while `mode` is already `Normal` (the input line is
    /// inert otherwise), so there is nothing to park against. V1: also
    /// resets `modal_scroll` -- see that field's own doc on why one shared
    /// field serves every modal-bearing surface, `/help` included.
    pub fn open_help(&mut self) {
        self.help_open = true;
        self.modal_scroll = 0;
        // V4: mutually exclusive with the settings menu -- see
        // `Self::settings_open`'s own doc for why both flags clear each
        // other on open rather than relying on check-order to keep at most
        // one of them showing.
        self.settings_open = false;
    }

    /// Closes the `/help` keybinding overlay (T7's `Esc` binding, wired in
    /// `input.rs`). A no-op when it is already closed.
    pub fn close_help(&mut self) {
        self.help_open = false;
    }

    /// Opens the `/settings` menu (V4). Mirrors [`Self::open_help`] exactly
    /// -- see [`Self::settings_open`]'s own doc for the full "informational,
    /// gated ahead of `Mode::Normal`, mutually exclusive with `/help`"
    /// reasoning. Deliberately does NOT reset [`Self::settings_selected`] or
    /// [`Self::settings_collapsed_groups`] -- re-opening the menu within the
    /// same session restores wherever the cursor/collapse state was left,
    /// the same way re-opening the `/agents` panel does not reset
    /// `agent_selected`.
    /// V2b: the pattern grant Conway would offer for the pending
    /// permission prompt, if any.
    ///
    /// Shaped by the pending call's own `render_kind` (carried on the
    /// request from the broker -- the same declaration the evaluation side
    /// matched against): a `ShellCommand` tool gets the narrow two-token
    /// prefix, or no offer at all when the command carries shell
    /// metacharacters; a `Structured` tool gets the registerable wildcard
    /// (`tool:*`), the only rule shape F12's registration check admits
    /// against a JSON-dump rendering. See
    /// `permission_pattern::suggested_rule` for the full reasoning.
    pub fn offered_permission_rule(&self) -> Option<conway::PatternRule> {
        let Mode::AwaitingPermission(pending) = &self.mode else {
            return None;
        };
        conway::permission_pattern::suggested_rule(
            pending.request.tool.as_str(),
            &pending.request.rendered,
            pending.request.render_kind,
        )
    }

    pub fn open_settings(&mut self) {
        self.settings_open = true;
        self.help_open = false;
    }

    /// Closes the `/settings` menu (V4's `Esc` binding, wired in
    /// `input.rs`). A no-op when it is already closed. Cursor/collapse
    /// state is left untouched (see [`Self::open_settings`]'s own doc).
    pub fn close_settings(&mut self) {
        self.settings_open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::ToolName;

    fn ask_modal(question: &str) -> AskModal {
        AskModal {
            question: question.to_string(),
            child: AgentId::new(),
            answer: "the answer".to_string(),
            error: None,
        }
    }

    fn permission_prompt(rendered: &str) -> crate::tui::gate::PendingPrompt {
        let (prompt, _rx) =
            crate::tui::gate::PendingPrompt::new_for_test(conway::PermissionRequest {
                agent_id: AgentId::new(),
                agent_path: Vec::new(),
                tool: ToolName::new("bash"),
                category: conway::ToolCategory::Execute,
                arguments: serde_json::json!({}),
                rendered: rendered.to_string(),
                call_id: "tc_1".to_string(),
                render_kind: conway::RenderKind::ShellCommand,
            });
        prompt
    }

    #[test]
    fn offer_ask_modal_opens_immediately_in_normal_mode() {
        let mut state = AppState::new(AgentId::new());
        assert!(matches!(state.mode, Mode::Normal));

        state.offer_ask_modal(ask_modal("q"));

        assert!(
            matches!(&state.mode, Mode::AskModal(m) if m.question == "q" && m.error.is_none()),
            "the modal must open immediately, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn offer_ask_modal_parks_behind_a_permission_prompt_and_opens_once_it_resolves() {
        let mut state = AppState::new(AgentId::new());
        state.offer_prompt(permission_prompt("bash: ls"));
        assert!(matches!(state.mode, Mode::AwaitingPermission(_)));

        state.offer_ask_modal(ask_modal("q"));

        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "the permission prompt must keep the floor; the modal parks, got: {:?}",
            state.mode
        );

        state.resolve_current_prompt(conway::PermissionDecision::AllowOnce);

        assert!(
            matches!(&state.mode, Mode::AskModal(m) if m.question == "q"),
            "the parked modal must open once the prompt queue drains, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn take_pending_ask_modal_drains_a_parked_modal_and_clears_the_slot() {
        // M1's quit-path fix: `purge_open_ask_modal` must be able to reach a
        // modal that was parked behind a permission prompt when the ask
        // completed, or its child leaks as residue. `take_pending_ask_modal`
        // is the accessor that lets app.rs drain it without `pending_ask_modal`
        // being pub.
        let mut state = AppState::new(AgentId::new());
        state.offer_prompt(permission_prompt("bash: ls"));
        state.offer_ask_modal(ask_modal("parked"));

        let drained = state.take_pending_ask_modal();
        assert!(
            matches!(&drained, Some(m) if m.question == "parked"),
            "the parked modal must be returned, got: {drained:?}"
        );
        assert!(
            state.take_pending_ask_modal().is_none(),
            "the slot must be cleared after the take"
        );
        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "take must NOT clobber the surface currently owning `mode`"
        );
    }

    #[test]
    fn take_pending_ask_modal_is_none_when_nothing_is_parked() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.take_pending_ask_modal().is_none());
        // An open (live) modal is in `mode`, not in the parking slot.
        state.offer_ask_modal(ask_modal("live"));
        assert!(
            state.take_pending_ask_modal().is_none(),
            "a live modal is not parked -- take must return None"
        );
    }

    #[test]
    fn close_ask_modal_returns_to_normal() {
        let mut state = AppState::new(AgentId::new());
        state.offer_ask_modal(ask_modal("q"));

        state.close_ask_modal();

        assert!(matches!(state.mode, Mode::Normal));
    }

    #[test]
    fn close_ask_modal_promotes_a_prompt_queued_while_the_modal_was_open() {
        let mut state = AppState::new(AgentId::new());
        state.offer_ask_modal(ask_modal("q"));
        // A permission request arrives WHILE the modal owns the floor --
        // `offer_prompt` queues it, exactly as behind another prompt.
        state.offer_prompt(permission_prompt("bash: ls"));
        assert!(matches!(state.mode, Mode::AskModal(_)));

        state.close_ask_modal();

        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "closing the modal must promote the queued prompt, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn fail_ask_modal_keeps_the_modal_open_with_the_error_shown() {
        let mut state = AppState::new(AgentId::new());
        state.offer_ask_modal(ask_modal("q"));

        state.fail_ask_modal("pull_in refused".to_string());

        match &state.mode {
            Mode::AskModal(m) => {
                assert_eq!(m.error.as_deref(), Some("pull_in refused"));
                assert_eq!(m.question, "q", "the modal's content is untouched");
            }
            other => panic!("a failed fate must KEEP the modal open, got: {other:?}"),
        }
    }

    fn intent_card(prompt: &str) -> IntentConfirm {
        IntentConfirm {
            intent: AgentIntent {
                recipe: SubagentMode::Spawn,
                agent_def: None,
                prompt: prompt.to_string(),
            },
            default_recipe: SubagentMode::Spawn,
            raw_text: prompt.to_string(),
            parent: AgentId::new(),
        }
    }

    #[test]
    fn offer_intent_confirm_opens_immediately_in_normal_mode() {
        let mut state = AppState::new(AgentId::new());
        assert!(matches!(state.mode, Mode::Normal));

        state.offer_intent_confirm(intent_card("refactor the parser"));

        assert!(
            matches!(&state.mode, Mode::IntentConfirm(ic) if ic.intent.prompt == "refactor the parser"),
            "the card must open immediately, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn offer_intent_confirm_parks_behind_a_permission_prompt_and_opens_once_it_resolves() {
        let mut state = AppState::new(AgentId::new());
        state.offer_prompt(permission_prompt("bash: ls"));
        assert!(matches!(state.mode, Mode::AwaitingPermission(_)));

        state.offer_intent_confirm(intent_card("parked"));

        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "the permission prompt must keep the floor; the card parks, got: {:?}",
            state.mode
        );

        state.resolve_current_prompt(conway::PermissionDecision::AllowOnce);

        assert!(
            matches!(&state.mode, Mode::IntentConfirm(ic) if ic.intent.prompt == "parked"),
            "the parked card must open once the prompt queue drains, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn offer_intent_confirm_parks_behind_an_ask_modal_and_opens_once_it_closes() {
        // The three modal-bearing surfaces never stack: an intent card
        // arriving while an /ask modal owns the floor parks in
        // `pending_intent_confirm`, and `close_ask_modal` promotes it via
        // `promote_next_surface`.
        let mut state = AppState::new(AgentId::new());
        state.offer_ask_modal(ask_modal("q"));
        assert!(matches!(state.mode, Mode::AskModal(_)));

        state.offer_intent_confirm(intent_card("parked-behind-ask"));

        assert!(
            matches!(state.mode, Mode::AskModal(_)),
            "the ask modal must keep the floor; the card parks, got: {:?}",
            state.mode
        );

        state.close_ask_modal();

        assert!(
            matches!(&state.mode, Mode::IntentConfirm(ic) if ic.intent.prompt == "parked-behind-ask"),
            "the parked card must open once the ask modal closes, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn take_pending_intent_confirm_drains_a_parked_card_and_clears_the_slot() {
        let mut state = AppState::new(AgentId::new());
        state.offer_prompt(permission_prompt("bash: ls"));
        state.offer_intent_confirm(intent_card("parked"));

        let drained = state.take_pending_intent_confirm();
        assert!(
            matches!(&drained, Some(ic) if ic.intent.prompt == "parked"),
            "the parked card must be returned, got: {drained:?}"
        );
        assert!(
            state.take_pending_intent_confirm().is_none(),
            "the slot must be cleared after the take"
        );
        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "take must NOT clobber the surface currently owning `mode`"
        );
    }

    #[test]
    fn close_intent_confirm_returns_to_normal() {
        let mut state = AppState::new(AgentId::new());
        state.offer_intent_confirm(intent_card("q"));

        state.close_intent_confirm();

        assert!(matches!(state.mode, Mode::Normal));
    }

    #[test]
    fn close_intent_confirm_promotes_a_prompt_queued_while_the_card_was_open() {
        let mut state = AppState::new(AgentId::new());
        state.offer_intent_confirm(intent_card("q"));
        state.offer_prompt(permission_prompt("bash: ls"));
        assert!(matches!(state.mode, Mode::IntentConfirm(_)));

        state.close_intent_confirm();

        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "closing the card must promote the queued prompt, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn close_intent_confirm_promotes_a_parked_ask_modal() {
        // Priority order: queued prompts, then parked ask modal, then
        // parked intent card. With no queued prompt, closing the card
        // promotes a parked ask modal.
        let mut state = AppState::new(AgentId::new());
        state.offer_intent_confirm(intent_card("q"));
        // Park an ask behind the card (offer_ask_modal parks when mode !=
        // Normal).
        state.offer_ask_modal(ask_modal("parked-ask"));
        assert!(matches!(state.mode, Mode::IntentConfirm(_)));

        state.close_intent_confirm();

        assert!(
            matches!(&state.mode, Mode::AskModal(m) if m.question == "parked-ask"),
            "closing the card must promote the parked ask modal, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn begin_intent_confirm_edit_drops_the_classified_prompt_into_the_input_line() {
        let mut state = AppState::new(AgentId::new());
        state.input = "stale text".to_string();
        state.cursor = state.input.chars().count();
        state.offer_intent_confirm(IntentConfirm {
            intent: AgentIntent {
                recipe: SubagentMode::Spawn,
                agent_def: Some("reviewer".to_string()),
                prompt: "review the diff carefully".to_string(),
            },
            default_recipe: SubagentMode::Spawn,
            raw_text: "review the diff".to_string(),
            parent: AgentId::new(),
        });

        state.begin_intent_confirm_edit();

        assert_eq!(
            state.input, "review the diff carefully",
            "the classified prompt (not the raw text) must land in the input line"
        );
        assert_eq!(
            state.cursor,
            state.input.chars().count(),
            "the cursor must be at the end of the dropped prompt"
        );
        assert!(
            matches!(state.mode, Mode::Normal),
            "the card must close after edit, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn begin_intent_confirm_edit_is_a_noop_when_no_card_is_open() {
        let mut state = AppState::new(AgentId::new());
        state.input = "keep me".to_string();

        state.begin_intent_confirm_edit();

        assert_eq!(
            state.input, "keep me",
            "a no-card edit must not touch the input line"
        );
        assert!(matches!(state.mode, Mode::Normal));
    }

    #[test]
    fn open_settings_and_help_are_mutually_exclusive() {
        let mut state = AppState::new(AgentId::new());
        state.open_help();
        assert!(state.help_open);

        state.open_settings();
        assert!(state.settings_open, "/settings must open");
        assert!(!state.help_open, "opening settings must close help");

        state.open_help();
        assert!(state.help_open, "/help must open");
        assert!(!state.settings_open, "opening help must close settings");
    }

    #[test]
    fn close_settings_is_a_noop_when_already_closed() {
        let mut state = AppState::new(AgentId::new());
        assert!(!state.settings_open);
        state.close_settings();
        assert!(!state.settings_open);
    }
}
