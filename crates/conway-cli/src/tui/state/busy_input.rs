//! Board item `01M1YVHKTQVXJRDSRYT3TCRXFX` ("Typing while the agent
//! works"): makes the pre-existing queue-by-construction behaviour
//! visible and configurable.
//!
//! ## What already worked, and what this module adds
//!
//! A message submitted while the focused agent's turn was already running
//! always reached the model eventually (`SessionHandle::prompt_agent`
//! appends a durable `UserTurn` the agent's own next context build picks
//! up) -- there was just no on-screen signal that anything had been held
//! back, no way to pull a just-typed message back out, and no way to
//! choose a DIFFERENT delivery timing. This module is the session-side
//! half of closing that gap: [`AppState::busy_input`] (seeded from
//! `[tui.busy_input]`, `crate::tui::config::BusyInputMode`) selects which
//! of three behaviours `App::submit` (`tui/app.rs`) applies while the
//! focused agent is busy, and [`AppState::held_prompts`]/
//! [`AppState::pending_steers`] are the client-side bookkeeping that makes
//! the "queued" strip (`view/input_box.rs`) and the transcript's
//! [`Entry::QueuedUser`] rows possible.
//!
//! ## Review round 1, CRITICAL: every entry is tagged with its own agent
//!
//! An earlier revision stored bare `VecDeque<String>`s with no `AgentId` at
//! all, implicitly assuming "queued" meant "queued for whichever agent is
//! focused when it is delivered." That is wrong the instant focus moves
//! between queuing and delivery (`/agents`, `/spawn`, `/fork`, `/model`,
//! any focus-nav key): a message typed for agent A would be delivered to
//! whichever agent B happened to be focused when A's turn finished, or
//! never delivered at all if B never finishes. [`AppState::held_prompts`]/
//! [`AppState::pending_steers`] now store `(AgentId, String)` pairs, and every
//! read/write path is keyed by that `AgentId`, never by [`AppState::
//! focused_agent`] at delivery time -- see [`AppState::queue_prompt`]/
//! [`AppState::take_held_prompts_for`]'s own docs.
//!
//! **The queued strip is scoped to the focused agent; other agents' queues
//! are a bare count.** [`AppState::queued_strip_summary`] returns `(focused_
//! count, first_line, other_count)`: the FOCUSED agent's own count (with
//! its next message's first line, same as before) is shown in full; a
//! DIFFERENT, non-focused agent's queued messages are rolled into
//! `other_count` alone, never shown by name or content, since the
//! operator is not looking at that conversation. `Up`/[`AppState::
//! recall_last_queued`] only ever act on the FOCUSED agent's own entries
//! for the identical reason -- recalling a message you cannot see into an
//! editor you ARE looking at would silently cross agents. This is the
//! explicit choice the review asked this module to state plainly, over
//! the alternative (show every agent's queue by name): naming a
//! background agent's in-progress draft in the currently-focused
//! conversation reads as a different agent's private business leaking
//! into this one, where a bare count does not.
//!
//! **`Entry::QueuedUser` rows are a best-effort LIVE indicator, not
//! reconstructed on refocus.** They are pushed only at the moment a
//! message is queued (always for the then-focused agent) and removed by
//! whichever of recall/delivery/stranding touches that entry. Switching
//! focus away clears the whole transcript (`AppState::focus_agent`'s own,
//! unchanged, pre-item contract); switching back rebuilds it from the
//! PERSISTED log only, which never contained an undelivered entry in the
//! first place. The data survives (`held_prompts`/`pending_steers` are
//! keyed by agent, not cleared by a focus change), so `queued_strip_
//! summary` reports the correct count the instant that agent is refocused
//! -- only the in-transcript row is not retroactively reinserted,
//! mirroring this crate's own established precedent for other
//! replay-does-not-reconstruct, live-only indicators (`try_focus_agent`'s
//! own re-fetch of the model/context/usage figures, which replay also
//! cannot repopulate).
//!
//! ## `queue` needs no runtime change at all
//!
//! Board item `01M1YVHKTQVXJRDSRYT3TCRXFX`'s own spec asks whether
//! recalling a queued message needs the runtime to support withdrawing an
//! undelivered mailbox entry. It does not, for `queue` mode specifically:
//! `App::submit` never calls `SessionHandle::prompt_agent`/`steer` for a
//! message this mode withholds -- the text sits ONLY in [`AppState::
//! held_prompts`] (a plain client-side `VecDeque`) until [`AppState::
//! take_held_prompts_for`] drains it once that agent is actually ready.
//! `Up` ([`AppState::recall_last_queued`]) only ever removes an entry from
//! THIS deque -- never from anything already handed to the runtime -- so
//! "the message was already delivered, recall must not double-send" holds
//! structurally: there is nothing to recall once it has been drained by
//! `take_held_prompts_for`.
//!
//! **Review round 1, RULING: no `Esc`-discards-the-queue.** `Esc` is a
//! reflex key; destroying a message on it is unsafe. Discarding a queued
//! message now works the same way discarding anything else in the input
//! line does: `Up` recalls it into the editor, then the existing `Ctrl-U`
//! (`prompt.kill_to_start`) or ordinary backspacing clears it -- no
//! dedicated discard primitive, no `Esc` special case. `Esc` on the
//! `Prompt` context keeps its exact pre-existing meanings (`input.rs`'s own
//! `KeyCode::Esc` arm is untouched once more).
//!
//! `steer` mode is the opposite shape: it calls `SessionHandle::steer`
//! immediately (the existing mailbox primitive, already delivered no later
//! than the running turn's own next tool-loop step -- see
//! `conway_runtime::mailbox`'s own module doc), so there genuinely is
//! nothing left to withdraw by the time this module could try. A steer's
//! pending-strip entry ([`AppState::pending_steers`]) is therefore
//! DELIBERATELY never offered to `Up` at all -- it exists only for
//! VISIBILITY, cleared once `App::flush_ready_queues` (`app/busy_input.
//! rs`) observes that agent is no longer mid-generation, never for recall.

use std::collections::{HashSet, VecDeque};

pub use crate::tui::config::BusyInputMode;

use super::*;

impl AppState {
    /// The `/settings` menu's "display" group row for [`Self::busy_input`]
    /// (`Enter` on `LEAF_BUSY_INPUT`, `view/settings.rs`): cycles `queue ->
    /// steer -> queue`, mirroring the permission-mode row's own cycle
    /// shape. A pure `AppState` flip -- unlike `LEAF_PERMISSION_
    /// MODE`/`LEAF_DEFAULT_ROLE` (which need a broker write/config
    /// persistence the app loop owns), this setting is session-only (same
    /// posture as `show_reasoning`/`show_timestamps`), so `input.rs`'s
    /// `activate_settings_selection` calls this directly, with no `Action`
    /// round trip.
    ///
    /// **Orchestrator ruling (after review round 2):** a third value,
    /// `interrupt`, existed here and cycled `queue -> steer -> interrupt ->
    /// queue`. Removed outright -- review round 2 established that this
    /// runtime's cancellation is unconditionally terminal for a kept-alive
    /// agent in every state, so `interrupt` could only ever END the
    /// operator's session, never "cancel this reply, keep talking." The
    /// orchestrator ruled conway must never ship a `busy_input` mode or key
    /// whose effect is to end the session out from under the operator. See
    /// `crate::tui::config::BusyInputMode`'s own doc for the config-load
    /// compatibility fallback a `settings.json` still naming `interrupt`
    /// gets.
    pub fn cycle_busy_input(&mut self) {
        self.busy_input = match self.busy_input {
            BusyInputMode::Queue => BusyInputMode::Steer,
            BusyInputMode::Steer => BusyInputMode::Queue,
        };
    }

    /// `busy_input = queue`'s withhold: appends `(agent, text)` to the back
    /// of [`Self::held_prompts`] (FIFO per agent -- the oldest queued
    /// message for a given agent is the next one [`Self::
    /// take_held_prompts_for`] delivers for it) and pushes a matching
    /// [`Entry::QueuedUser`] onto the transcript -- `agent` is always
    /// [`AppState::focused_agent`] at the moment this is called (`App::
    /// submit` only ever queues the agent it is currently submitting to),
    /// so the push is always onto the conversation currently on screen.
    pub fn queue_prompt(&mut self, agent: AgentId, text: String) {
        self.held_prompts.push_back((agent, text.clone()));
        self.transcript.push(Entry::QueuedUser(text));
    }

    /// `Up` on an empty input line (`input.rs`'s fixed key-handling chain,
    /// checked BEFORE the bare-arrow scroll fallback): pulls the MOST
    /// RECENTLY queued message BELONGING TO THE FOCUSED AGENT (mirroring
    /// `Self::history_recall_prev`'s own "most recent first" shape) back
    /// into `input` for editing, removing it from the queue -- a recalled
    /// message is not ALSO delivered; it is as if it had never been typed,
    /// except that the operator gets the text back to edit or resubmit.
    /// Scoped to [`AppState::focused_agent`] deliberately (this module's
    /// own doc, "the queued strip is scoped to the focused agent"): `Up`
    /// never reaches into a DIFFERENT agent's queue, even if one exists and
    /// this one does not -- see this module's own doc for why. Returns
    /// `false` (no mutation at all) when the focused agent has nothing
    /// queued, letting the caller fall through to `Up`'s ordinary meaning
    /// (per board item `01M1YVHKTQVXJRDSRYT3TCRXFX`'s own spec: "ONLY if a
    /// queued message exists; otherwise Up keeps its current meaning").
    ///
    /// Removes the matching [`Entry::QueuedUser`] from the transcript too
    /// -- found by scanning backward for the LAST entry carrying this
    /// exact text, which (since entries are pushed in the same FIFO order
    /// as the queue itself) is always the one [`Self::queue_prompt`] pushed
    /// for THIS recall, even if an earlier, still-queued message happens
    /// to carry identical text.
    pub fn recall_last_queued(&mut self) -> bool {
        let Some(idx) = self
            .held_prompts
            .iter()
            .rposition(|(agent, _)| *agent == self.focused_agent)
        else {
            return false;
        };
        let (_, text) = self
            .held_prompts
            .remove(idx)
            .expect("idx was just found by rposition");
        self.remove_last_queued_user_entry(&text);
        self.input = text;
        self.cursor = self.input.chars().count();
        true
    }

    /// Drains every currently-held prompt belonging to `agent` (FIFO,
    /// oldest first), leaving every OTHER agent's own entries untouched in
    /// place -- the review round 1 CRITICAL fix: this is the ONLY read
    /// path `App::flush_ready_queues` uses to decide what to actually
    /// send, and it is keyed by `agent`, never by [`AppState::
    /// focused_agent`], so a focus change between queuing and delivery can
    /// never misdirect a message to the wrong agent or strand it
    /// unreachably. Removes each drained entry's matching [`Entry::
    /// QueuedUser`] transcript row too, but ONLY when `agent` is currently
    /// focused (that row only exists in the transcript at all while
    /// `agent` stays focused continuously -- see this module's own doc,
    /// "best-effort LIVE indicator, not reconstructed on refocus").
    pub fn take_held_prompts_for(&mut self, agent: AgentId) -> Vec<String> {
        let mut remaining = VecDeque::new();
        let mut taken = Vec::new();
        for (a, text) in std::mem::take(&mut self.held_prompts) {
            if a == agent {
                taken.push(text);
            } else {
                remaining.push_back((a, text));
            }
        }
        self.held_prompts = remaining;
        if agent == self.focused_agent {
            for text in &taken {
                self.remove_first_queued_user_entry(text);
            }
        }
        taken
    }

    /// `busy_input = steer`'s own pending-visibility bookkeeping: pushed
    /// immediately after `App::submit` calls `SessionHandle::steer`
    /// (already delivered through the mailbox -- there is nothing left to
    /// withhold or recall, see this module's own doc) so the strip/
    /// transcript show it the same way a queued message is shown, until
    /// `App::flush_ready_queues` observes `agent` is no longer mid-
    /// generation and calls [`Self::clear_pending_steers_for`].
    pub fn mark_steer_pending(&mut self, agent: AgentId, text: String) {
        self.pending_steers.push_back((agent, text.clone()));
        self.transcript.push(Entry::QueuedUser(text));
    }

    /// Clears every pending-steer VISIBILITY entry for `agent` -- called
    /// from `App::flush_ready_queues` once polling observes `agent` is no
    /// longer mid-generation (`SessionHandle::turn_in_progress`), the
    /// focus-independent replacement for the earlier `Event::TurnStarted`-
    /// driven clear (which could only ever fire for the currently focused
    /// agent's own live stream -- the same structural gap review round 1's
    /// CRITICAL finding named for delivery). Removes each entry's matching
    /// transcript row too, under the identical "only while `agent` is
    /// focused" condition [`Self::take_held_prompts_for`] uses.
    pub fn clear_pending_steers_for(&mut self, agent: AgentId) {
        let mut remaining = VecDeque::new();
        for (a, text) in std::mem::take(&mut self.pending_steers) {
            if a == agent {
                if agent == self.focused_agent {
                    self.remove_first_queued_user_entry(&text);
                }
            } else {
                remaining.push_back((a, text));
            }
        }
        self.pending_steers = remaining;
    }

    /// The distinct agents with at least one withheld prompt -- what
    /// `App::flush_ready_queues` polls over, first-occurrence order.
    pub fn agents_with_held_prompts(&self) -> Vec<AgentId> {
        let mut seen = HashSet::new();
        self.held_prompts
            .iter()
            .filter_map(|(agent, _)| seen.insert(*agent).then_some(*agent))
            .collect()
    }

    /// The distinct agents with at least one pending-steer visibility
    /// entry -- the `pending_steers` sibling of [`Self::
    /// agents_with_held_prompts`].
    pub fn agents_with_pending_steers(&self) -> Vec<AgentId> {
        let mut seen = HashSet::new();
        self.pending_steers
            .iter()
            .filter_map(|(agent, _)| seen.insert(*agent).then_some(*agent))
            .collect()
    }

    /// The queued strip's own summary (`view/input_box.rs`):
    /// `(focused_count, first_line, other_count)` -- see this module's own
    /// doc, "the queued strip is scoped to the focused agent," for why a
    /// non-focused agent's own queue is folded into a bare `other_count`
    /// rather than shown by name or content. `focused_count`/`first_line`
    /// cover BOTH withheld (`queue` mode) and already-sent-but-pending
    /// (`steer` mode) messages belonging to [`AppState::focused_agent`];
    /// `first_line` prefers the oldest WITHHELD message when one exists (it
    /// is the next thing that will actually be sent), else the oldest
    /// pending steer. `first_line` is `None` exactly when `focused_count`
    /// is `0`.
    pub fn queued_strip_summary(&self) -> (usize, Option<&str>, usize) {
        let focused = self.focused_agent;
        let focused_held = self
            .held_prompts
            .iter()
            .filter(|(agent, _)| *agent == focused)
            .count();
        let focused_steers = self
            .pending_steers
            .iter()
            .filter(|(agent, _)| *agent == focused)
            .count();
        let focused_count = focused_held + focused_steers;
        let total = self.held_prompts.len() + self.pending_steers.len();
        let other_count = total - focused_count;
        let first_line = self
            .held_prompts
            .iter()
            .find(|(agent, _)| *agent == focused)
            .or_else(|| {
                self.pending_steers
                    .iter()
                    .find(|(agent, _)| *agent == focused)
            })
            .map(|(_, text)| text.as_str());
        (focused_count, first_line, other_count)
    }

    /// Shared by [`Self::recall_last_queued`]: removes the LAST transcript
    /// entry carrying `Entry::QueuedUser(text)` -- see that method's own
    /// doc for why scanning backward for an exact-text match always finds
    /// the right one.
    fn remove_last_queued_user_entry(&mut self, text: &str) {
        if let Some(pos) = self
            .transcript
            .iter()
            .rposition(|e| matches!(e, Entry::QueuedUser(t) if t == text))
        {
            self.transcript.remove(pos);
        }
    }

    /// Shared by [`Self::take_held_prompts_for`]/[`Self::
    /// clear_pending_steers_for`]: removes the FIRST (oldest) transcript
    /// entry carrying `Entry::QueuedUser(text)` -- forward scan, pairing
    /// correctly with the FIFO order both callers drain their own queue
    /// in, even when duplicate text is queued twice.
    fn remove_first_queued_user_entry(&mut self, text: &str) {
        if let Some(pos) = self
            .transcript
            .iter()
            .position(|e| matches!(e, Entry::QueuedUser(t) if t == text))
        {
            self.transcript.remove(pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::AgentId;

    #[test]
    fn cycle_busy_input_wraps_queue_steer_queue() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(state.busy_input, BusyInputMode::Queue);

        state.cycle_busy_input();
        assert_eq!(state.busy_input, BusyInputMode::Steer);

        state.cycle_busy_input();
        assert_eq!(state.busy_input, BusyInputMode::Queue);
    }

    #[test]
    fn queue_prompt_adds_to_the_queue_and_the_transcript() {
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        state.queue_prompt(agent, "first".to_string());

        assert_eq!(state.held_prompts, vec![(agent, "first".to_string())]);
        assert!(matches!(
            state.transcript.last(),
            Some(Entry::QueuedUser(t)) if t == "first"
        ));
    }

    /// Acceptance 1: "two messages typed during a turn show '2 queued'".
    #[test]
    fn two_queued_messages_report_a_count_of_two() {
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        state.queue_prompt(agent, "first".to_string());
        state.queue_prompt(agent, "second".to_string());

        let (count, first, other) = state.queued_strip_summary();
        assert_eq!(count, 2);
        assert_eq!(other, 0);
        assert_eq!(
            first,
            Some("first"),
            "the strip shows the NEXT message to send"
        );
    }

    #[test]
    fn queued_strip_summary_is_zero_when_nothing_is_queued() {
        let state = AppState::new(AgentId::new());
        assert_eq!(state.queued_strip_summary(), (0, None, 0));
    }

    /// Review round 1, CRITICAL: a message queued for agent A must never
    /// count against, or be recallable from, a DIFFERENT focused agent B's
    /// own strip/`Up` -- it is folded into `other_count` alone.
    #[test]
    fn a_message_queued_for_another_agent_shows_only_as_a_bare_other_count() {
        let a = AgentId::new();
        let b = AgentId::new();
        let mut state = AppState::new(a);
        state.queue_prompt(a, "for a".to_string());
        state.focus_agent(b);

        let (focused_count, first_line, other_count) = state.queued_strip_summary();
        assert_eq!(focused_count, 0);
        assert_eq!(first_line, None, "must never leak another agent's text");
        assert_eq!(other_count, 1);
        assert!(
            !state.recall_last_queued(),
            "Up must not reach into another agent's queue"
        );
    }

    /// Acceptance 1: "Up recalls the second; the first is delivered at the
    /// boundary" -- recall takes the NEWEST (LIFO), leaving the oldest
    /// still queued for `take_held_prompts_for`.
    #[test]
    fn recall_last_queued_pulls_back_the_newest_leaving_the_rest_queued() {
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        state.queue_prompt(agent, "first".to_string());
        state.queue_prompt(agent, "second".to_string());

        assert!(state.recall_last_queued());

        assert_eq!(state.input, "second");
        assert_eq!(state.cursor, "second".chars().count());
        assert_eq!(state.held_prompts, vec![(agent, "first".to_string())]);
        assert!(
            !state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::QueuedUser(t) if t == "second")),
            "the recalled entry's own transcript row must be removed: {:?}",
            state.transcript
        );
        assert!(
            state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::QueuedUser(t) if t == "first")),
            "the still-queued entry must remain: {:?}",
            state.transcript
        );
    }

    #[test]
    fn recall_last_queued_on_an_empty_queue_does_not_fire() {
        let mut state = AppState::new(AgentId::new());
        state.input = "typing".to_string();

        assert!(!state.recall_last_queued());
        assert_eq!(state.input, "typing");
    }

    /// A recalled message is not ALSO delivered -- `take_held_prompts_for`
    /// (the ONLY path that hands a withheld message to the runtime) sees
    /// it gone.
    #[test]
    fn a_recalled_message_is_not_also_delivered() {
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        state.queue_prompt(agent, "only".to_string());
        assert!(state.recall_last_queued());

        let delivered = state.take_held_prompts_for(agent);
        assert!(
            delivered.is_empty(),
            "a recalled message must never be delivered: {delivered:?}"
        );
    }

    #[test]
    fn take_held_prompts_for_drains_in_fifo_order_and_clears_the_strip() {
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        state.queue_prompt(agent, "first".to_string());
        state.queue_prompt(agent, "second".to_string());

        let delivered = state.take_held_prompts_for(agent);
        assert_eq!(delivered, vec!["first".to_string(), "second".to_string()]);
        assert!(state.held_prompts.is_empty());
        assert_eq!(state.queued_strip_summary(), (0, None, 0));
        assert!(
            !state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::QueuedUser(_))),
            "every queued entry must be removed once delivered: {:?}",
            state.transcript
        );
    }

    /// Review round 1, CRITICAL: draining one agent's queue must never
    /// touch a DIFFERENT agent's own still-held entries.
    #[test]
    fn take_held_prompts_for_leaves_other_agents_queues_untouched() {
        let a = AgentId::new();
        let b = AgentId::new();
        let mut state = AppState::new(a);
        state.queue_prompt(a, "for a".to_string());
        state.focus_agent(b);
        state.queue_prompt(b, "for b".to_string());

        let delivered_a = state.take_held_prompts_for(a);
        assert_eq!(delivered_a, vec!["for a".to_string()]);
        assert_eq!(state.held_prompts, vec![(b, "for b".to_string())]);
    }

    #[test]
    fn mark_steer_pending_shows_in_the_strip_but_never_in_held_prompts() {
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        state.mark_steer_pending(agent, "steered".to_string());

        assert!(state.held_prompts.is_empty());
        let (count, first, other) = state.queued_strip_summary();
        assert_eq!(count, 1);
        assert_eq!(other, 0);
        assert_eq!(first, Some("steered"));
        // A pending steer is never recallable (it is already sent) --
        // `Up` must see nothing to act on.
        assert!(!state.recall_last_queued());
    }

    #[test]
    fn clear_pending_steers_for_removes_the_entry_and_its_transcript_row() {
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        state.mark_steer_pending(agent, "steered".to_string());

        state.clear_pending_steers_for(agent);

        assert!(state.pending_steers.is_empty());
        assert_eq!(state.queued_strip_summary(), (0, None, 0));
        assert!(!state
            .transcript
            .iter()
            .any(|e| matches!(e, Entry::QueuedUser(_))));
    }

    /// Clearing one agent's pending steers must never touch another
    /// agent's own.
    #[test]
    fn clear_pending_steers_for_leaves_other_agents_untouched() {
        let a = AgentId::new();
        let b = AgentId::new();
        let mut state = AppState::new(a);
        state.mark_steer_pending(a, "for a".to_string());
        state.focus_agent(b);
        state.mark_steer_pending(b, "for b".to_string());

        state.clear_pending_steers_for(a);

        assert_eq!(state.pending_steers, vec![(b, "for b".to_string())]);
    }

    #[test]
    fn agents_with_held_prompts_lists_each_distinct_agent_once() {
        let a = AgentId::new();
        let b = AgentId::new();
        let mut state = AppState::new(a);
        state.queue_prompt(a, "one".to_string());
        state.queue_prompt(a, "two".to_string());
        state.focus_agent(b);
        state.queue_prompt(b, "three".to_string());

        assert_eq!(state.agents_with_held_prompts(), vec![a, b]);
    }
}
