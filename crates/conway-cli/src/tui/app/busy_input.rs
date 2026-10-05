//! Board item `01M1YVHKTQVXJRDSRYT3TCRXFX` ("Typing while the agent
//! works"): the `App`-level half of `busy_input`'s delivery and
//! finished-agent guards. `crate::tui::state::busy_input` owns every pure
//! state mutation (the queue itself, the strip's summary, recall); this
//! file is the one place that actually reaches the facade once the state
//! layer has decided a message is ready to send, or that an agent needs to
//! be treated as unreachable -- mirroring `app/await_cmd.rs`/`app/
//! skill_propose.rs`'s own split between "the state this needs" and "the
//! one call site that needs `App` itself".
//!
//! ## Orchestrator ruling, after review round 2: no mode or key may end the session
//!
//! This file used to ALSO own `busy_input = interrupt`'s cancel-then-wait
//! ordering (`App::cancel_and_wait_idle`/`wait_for_agent_idle`) and the
//! `prompt.send_now` override's identical tail. Review round 2's own fix
//! below (`App::agent_is_busy`) surfaced, by testing it rather than
//! assuming it, that this runtime's cancellation (`CancelMode::Immediate`,
//! the same primitive `Ctrl-C` uses) is UNCONDITIONALLY TERMINAL for a
//! kept-alive agent in every state -- there was no "cancel this reply,
//! keep the session" outcome for `interrupt` to ever have delivered the
//! new message into. The orchestrator ruled conway must never ship a
//! `busy_input` mode or key whose only real effect is to end the
//! operator's session: `interrupt` and `prompt.send_now` are both removed
//! (code, `Action`, settings-cycle value, tests, and docs), and this
//! module keeps only `queue`/`steer`. See `crate::tui::config::
//! BusyInputMode`'s own doc for the config-load compatibility fallback and
//! `docs/interactive.md` for the one-sentence pointer to the actual,
//! separately-tracked gap (a genuine non-terminal turn cancel does not
//! exist in this runtime today).
//!
//! ## Review round 1, CRITICAL: delivery is polled, per agent, focus-
//! independent
//!
//! An earlier revision triggered delivery from `App::run`'s own event-
//! handling arm, gated on `Event::TurnFinished` for the FOCUSED agent
//! leaving `AppState::activity` at `Idle`. That is wrong for two
//! compounding reasons: (1) the TUI's own live subscription is scoped to
//! AT MOST ONE agent at a time (`conway::SessionHandle::turn_in_progress`'s
//! own doc: "it structurally cannot observe `TurnStarted`/`TurnFinished`
//! for an agent it is not currently subscribed to"), so it could never
//! have fired for a message queued on a DIFFERENT agent than whichever one
//! happened to be focused when that agent went idle; (2) even for the
//! focused agent, the trigger ran inside the SAME event-apply step that
//! also handles focus switches and `AgentFinished`, with no guard against
//! delivering into a session that had, in the meantime, finished.
//!
//! [`App::flush_ready_queues`] replaces that entirely with a poll, run
//! from `App::run`'s existing 16ms redraw tick (`REDRAW_TICK`), over
//! [`crate::tui::state::AppState::agents_with_held_prompts`]/
//! [`agents_with_pending_steers`](crate::tui::state::AppState::agents_with_pending_steers)
//! -- i.e. the AGENTS actually carrying a queue entry, never the currently
//! focused one. Both `SessionHandle::awaiting_prompt` (is this agent idle
//! at its resume gate RIGHT NOW) and [`App::agent_is_finished`] (has this
//! agent already published a terminal result) are direct, synchronous
//! runtime queries that do not depend on any live event subscription at
//! all, which is what makes checking an agent the TUI is not currently
//! watching possible, let alone correct. A short poll interval (the
//! existing 16ms redraw cadence) is indistinguishable from "immediate" to
//! an operator and needs no new ticker of its own.
//!
//! ## Review round 2, CRITICAL: `turn_in_progress` does not cover tool
//! execution
//!
//! An earlier revision polled `SessionHandle::turn_in_progress` (a model
//! round-trip in flight) as the whole "is this agent busy" signal. That
//! reads `false` for the ENTIRE duration of every tool call a turn makes --
//! `conway_runtime::tree::AgentTree::mark_turn_finished` fires right after
//! one model round-trip, strictly BEFORE the proposed tools run, and the
//! NEXT round's own `mark_turn_started` fires only once they finish. So on
//! every tool-calling turn (the common case), `queue` mode's own delivery
//! would fire mid-turn, collapsing `queue` to `steer` timing. A
//! `turn_in_flight || !in_flight_tools.is_empty()` combination would still
//! leave the gap between a batch's results being processed and the NEXT
//! round's own `mark_turn_started` (persisting records, building the next
//! context) uncovered.
//!
//! [`App::agent_is_busy`] now polls [`conway::SessionHandle::
//! awaiting_prompt`] instead -- the agent loop's OWN single honest answer
//! to "is there nothing left to do until the caller's next prompt," which
//! spans the model round-trip, tool dispatch, and every gap between them,
//! up to the genuine end of the turn (see that method's own doc for the
//! full mechanism: a `conway-runtime` field mirroring `ResumeGate::
//! awaiting_prompt` itself, not a combination of flags reconstructed from
//! outside the loop).

use std::time::Duration;

use conway::AgentId;
use futures::FutureExt;

use super::App;
use crate::tui::state::Entry;

/// The longest a single undeliverable message is quoted in full inside a
/// stranded-queue notice before being truncated with an ellipsis -- a
/// notice naming several multi-paragraph drafts verbatim would be its own
/// readability problem.
const NOTICE_PREVIEW_CHARS: usize = 60;

impl App {
    /// Drains every agent with a ready `held_prompts`/`pending_steers`
    /// entry and acts on it -- the turn-boundary delivery board item
    /// `01M1YVHKTQVXJRDSRYT3TCRXFX`'s acceptance criteria describe,
    /// generalized to every agent carrying a queue entry, not just the
    /// focused one (see this module's own doc). A no-op (no facade call at
    /// all, not even a `tree`/`turn_in_progress` query) when both queues
    /// are empty -- the common case on every ordinary redraw tick.
    ///
    /// For each agent with a pending steer VISIBILITY entry: once it is no
    /// longer mid-generation, [`crate::tui::state::AppState::
    /// clear_pending_steers_for`] clears it -- nothing is ever sent here
    /// (a steer already went out through the mailbox the instant it was
    /// marked pending).
    ///
    /// For each agent with a held (`queue`-mode) prompt: review round 1's
    /// second CRITICAL fix -- an agent that has already reached a terminal
    /// result is NEVER sent to (`Self::agent_is_finished`, checked BEFORE
    /// [`Self::agent_is_busy`], since a finished agent also reads "not
    /// busy" and would otherwise be misread as "ready"); its held prompts
    /// are instead handed to [`Self::surface_undeliverable_held_prompts`].
    /// An agent that is live and not busy gets every one of its held
    /// prompts sent, in order, via [`App::send_prompt_now`].
    pub(super) async fn flush_ready_queues(&mut self) -> bool {
        let mut changed = false;
        for agent in self.state.agents_with_pending_steers() {
            if !self.agent_is_busy(agent) {
                self.state.clear_pending_steers_for(agent);
                changed = true;
            }
        }
        for agent in self.state.agents_with_held_prompts() {
            if self.agent_is_finished(agent).await {
                self.surface_undeliverable_held_prompts(agent).await;
                changed = true;
            } else if !self.agent_is_busy(agent) {
                for text in self.state.take_held_prompts_for(agent) {
                    self.send_prompt_now(agent, text).await;
                }
                changed = true;
            }
        }
        changed
    }

    /// Review round 2, CRITICAL: the honest "is `agent` busy right now"
    /// signal -- `SessionHandle::awaiting_prompt`'s own doc has the full
    /// rationale for why this, and not `SessionHandle::turn_in_progress`
    /// alone (which reads `false` for the entire duration of every tool
    /// call), is what [`Self::flush_ready_queues`] must poll. Does NOT
    /// itself account for a finished agent (an agent that will never
    /// un-idle again still reads `awaiting_prompt == false` if it never
    /// reached that gate, e.g. a non-`keep_alive` agent) -- every caller
    /// here already checks [`Self::agent_is_finished`] FIRST, separately,
    /// for that.
    fn agent_is_busy(&self, agent: AgentId) -> bool {
        !self.handle.awaiting_prompt(agent)
    }

    /// Whether `agent` has already reached a terminal `AgentResult` -- a
    /// non-blocking PEEK at `SessionHandle::await_agent`'s own
    /// `watch`-channel-backed wait (`conway_runtime::tree::AgentTree::
    /// await_result`): that channel already holds the final result the
    /// instant the agent finishes and is checked BEFORE ever awaiting a
    /// change, so a future built from it resolves on its very FIRST poll
    /// once a result exists -- `now_or_never` (`futures::FutureExt`)
    /// therefore reads it without ever yielding, with no new facade
    /// primitive and no blocking wait. An unknown-agent `Err` counts as
    /// finished too (there is nothing left to ever deliver to).
    ///
    /// Deliberately NOT `conway_core::agent::AgentStatus`/`SessionHandle::
    /// tree()`: this crate may not depend on `conway-core` directly
    /// (`crates/conway-cli/tests/cli_surface.rs`'s `no_forbidden_deps`),
    /// and `AgentStatus` is not re-exported from the facade `conway` crate
    /// at all -- every existing tree-status read in this crate (`state::
    /// agent_tree`'s own `NodeStatus`) is instead built entirely from
    /// EVENTS, which is exactly the subscription-scoped mechanism this
    /// module's own doc explains cannot answer "is agent X finished" for
    /// an agent the TUI is not currently watching. `await_agent` is public
    /// facade surface with no such restriction.
    pub(super) async fn agent_is_finished(&self, agent: AgentId) -> bool {
        self.handle.await_agent(agent).now_or_never().is_some()
    }

    /// Review round 1, CRITICAL: `agent` has already reached a terminal
    /// result while messages were still held for it -- `flush_ready_queues`
    /// must never call `send_prompt_now` in this case (`Runtime::prompt`
    /// never refuses a finished agent; it would silently append a
    /// `UserTurn` no task will ever read and wedge `activity` on
    /// `Thinking` forever -- the exact bug `AppState::
    /// block_message_if_focused_agent_finished` exists to prevent at
    /// submit time, which `flush_ready_queues` bypassed before this fix
    /// since it never went through `App::submit` at all).
    ///
    /// Drains every held prompt for `agent` (so nothing is retried against
    /// it again), restores the NEWEST one directly into the input box --
    /// but ONLY when `agent` is the agent currently focused (putting text
    /// into the box while a DIFFERENT agent is focused would silently
    /// clobber whatever the operator is typing for THAT one) and the box
    /// is currently empty (never clobber an in-progress, unrelated draft
    /// either) -- and always leaves a `Notice` naming every message that
    /// was not delivered, including the restored one.
    async fn surface_undeliverable_held_prompts(&mut self, agent: AgentId) {
        let mut texts = self.state.take_held_prompts_for(agent);
        if texts.is_empty() {
            return;
        }
        let restore = agent == self.state.focused_agent && self.state.input.is_empty();
        let restored = if restore { texts.pop() } else { None };
        let mut parts: Vec<String> = texts.iter().map(|t| quote_for_notice(t)).collect();
        if let Some(text) = &restored {
            self.state.input = text.clone();
            self.state.cursor = self.state.input.chars().count();
            parts.push(format!(
                "{} (restored to the input box)",
                quote_for_notice(text)
            ));
        }
        self.state.transcript.push(Entry::Notice {
            text: format!(
                "{agent} finished before {} queued message(s) could be sent -- not delivered: {}",
                parts.len(),
                parts.join(", ")
            ),
        });
    }

    /// Review round 1, SIGNIFICANT ("quitting with a queue"): every quit
    /// path (`/quit`, `Ctrl-D`) calls this FIRST, mirroring `app/shutdown.
    /// rs`'s own double-`Ctrl-C` shape -- a non-empty queue (`held_prompts`
    /// OR `pending_steers`, across every agent, not just the focused one)
    /// refuses the first attempt with a `Notice` naming the count and arms
    /// a window; a SECOND attempt within [`QUIT_QUEUE_CONFIRM_WINDOW`]
    /// actually quits (discarding the queue -- quitting silently anyway is
    /// a worse outcome than a second keypress for a confirmed exit).
    /// Returns `true` immediately, with no mutation, when the queue is
    /// already empty -- the overwhelmingly common case, and every quit
    /// path's exact pre-item behavior.
    ///
    /// **Deliberately NOT applied to the double-`Ctrl-C` exit
    /// (`app/shutdown.rs::handle_ctrl_c`'s second press).** That path is
    /// already its own two-press confirmation for a DIFFERENT reason (an
    /// unresponsive/busy session); stacking a THIRD press requirement on
    /// top of an already-deliberate "I mean it, force-exit now" signal
    /// would contradict the universal terminal convention double-`Ctrl-C`
    /// already carries, not reinforce this feature's own safety goal. This
    /// item's own confirmation is for the two path, `/quit`/`Ctrl-D`,
    /// that were previously a single, unconfirmed keypress.
    pub(super) fn confirm_quit_with_nonempty_queue(&mut self) -> bool {
        let pending = self.state.held_prompts.len() + self.state.pending_steers.len();
        if pending == 0 {
            return true;
        }
        let now = std::time::Instant::now();
        if let Some(armed) = self.quit_queue_warned_at {
            if now.duration_since(armed) <= QUIT_QUEUE_CONFIRM_WINDOW {
                return true;
            }
        }
        self.quit_queue_warned_at = Some(now);
        self.state.transcript.push(Entry::Notice {
            text: format!("{pending} queued message(s) will be discarded -- quit again to confirm"),
        });
        false
    }
}

/// How long [`App::confirm_quit_with_nonempty_queue`]'s "quit again to
/// confirm" window stays armed -- the same 2-second figure `app/shutdown.
/// rs`'s own `DOUBLE_CTRL_C_WINDOW` uses, for the identical reason (long
/// enough for a deliberate second keypress, short enough that an unrelated
/// LATER quit attempt is not mistaken for a confirmation of a stale one).
const QUIT_QUEUE_CONFIRM_WINDOW: Duration = Duration::from_secs(2);

/// `"{text}"`, truncated with an ellipsis at [`NOTICE_PREVIEW_CHARS`] --
/// see that constant's own doc. Duplicated rather than shared with `view/
/// transcript.rs`'s own private truncation helper, matching this crate's
/// established "a handful of near-identical small per-module truncation
/// helpers" precedent (that module's own doc, re-affirmed by `view/
/// input_box.rs`'s identical `truncate_with_ellipsis`).
fn quote_for_notice(text: &str) -> String {
    let char_count = text.chars().count();
    let body = if char_count <= NOTICE_PREVIEW_CHARS {
        text.to_string()
    } else {
        let truncated: String = text
            .chars()
            .take(NOTICE_PREVIEW_CHARS.saturating_sub(1))
            .collect();
        format!("{truncated}…")
    };
    format!("\"{body}\"")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::fixtures::{echo_conway_and_store, minimal_cli};
    use super::super::App;
    use crate::tui::state::{Activity, Entry};

    /// Review round 2: a freshly-constructed keep-alive agent has never
    /// been given a prompt, so `SessionHandle::awaiting_prompt` -- this
    /// item's own CRITICAL fix -- correctly reads it as BUSY (not yet at
    /// the idle gate at all), not merely "not yet started a model round-
    /// trip" the way the superseded `turn_in_progress`-only check read it.
    /// Several of this module's OWN tests need a genuinely idle (not
    /// merely freshly-constructed) agent to test DELIVERY against, exactly
    /// the way a real operator's first message gets a real reply before
    /// anything is ever queued behind it -- this drives that one real
    /// warm-up turn and waits for the agent to reach genuine idle,
    /// bounded, so a regression here fails loudly rather than hanging.
    async fn warm_up_to_idle(app: &App, agent: conway::AgentId) {
        app.handle
            .prompt_agent(agent, "warm up".to_string())
            .await
            .expect("the warm-up prompt must be accepted");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !app.handle.awaiting_prompt(agent) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the warm-up turn must reach genuine idle within the hang-timeout bound");
    }

    /// Acceptance 1, the delivery half: once the queue is drained, the
    /// message actually reached the real session log as an ordinary
    /// `UserTurn` -- not merely cleared from `AppState`'s own bookkeeping.
    #[tokio::test]
    async fn flush_ready_queues_sends_every_held_prompt_to_the_real_session() {
        let conway = echo_conway_and_store().0;
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        warm_up_to_idle(&app, root).await;

        app.state.queue_prompt(root, "first".to_string());
        app.state.queue_prompt(root, "second".to_string());
        assert_eq!(app.state.held_prompts.len(), 2);

        assert!(app.flush_ready_queues().await);

        assert!(app.state.held_prompts.is_empty());
        let records = app
            .handle
            .transcript(root)
            .await
            .expect("transcript must be readable");
        let user_turns: Vec<&str> = records
            .iter()
            .filter_map(|r| match r {
                conway::LogRecord::UserTurn { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            user_turns,
            vec!["warm up", "first", "second"],
            "both queued messages must be delivered, in FIFO order, after the warm-up turn"
        );
        assert_eq!(
            app.state.activity,
            Activity::Thinking,
            "delivering a queued message must mark the indicator working, \
             exactly like an ordinary submit"
        );
    }

    /// `agent_is_finished` itself: `false` for a live keep-alive root,
    /// `true` once a non-keep-alive child's one turn genuinely completes --
    /// the authoritative, subscription-independent primitive `App::
    /// submit`'s own post-cancel re-check and `flush_ready_queues` both
    /// depend on.
    #[tokio::test]
    async fn agent_is_finished_distinguishes_a_live_agent_from_a_terminal_one() {
        let conway = echo_conway_and_store().0;
        let cli = minimal_cli();
        let app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        assert!(
            !app.agent_is_finished(root).await,
            "a live keep-alive root must never read as finished"
        );

        let child = app
            .handle
            .spawn(root, conway::SpawnSpec::new("hello"))
            .await
            .expect("spawn should succeed");
        app.handle
            .await_agent(child)
            .await
            .expect("the child's one turn must complete");
        assert!(
            app.agent_is_finished(child).await,
            "a completed non-keep-alive child must read as finished"
        );
    }

    /// A no-op when nothing is queued -- no panic, no spurious send, no
    /// `dirty` signal.
    #[tokio::test]
    async fn flush_ready_queues_on_empty_queues_is_a_no_op() {
        let conway = echo_conway_and_store().0;
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();

        assert!(!app.flush_ready_queues().await);

        let records = app
            .handle
            .transcript(root)
            .await
            .expect("transcript must be readable");
        assert!(
            !records
                .iter()
                .any(|r| matches!(r, conway::LogRecord::UserTurn { .. })),
            "nothing must be sent when nothing was queued"
        );
    }

    /// Review round 1, CRITICAL: a message queued for a background agent
    /// must be delivered to THAT agent's own session -- never the one
    /// currently focused -- regardless of focus having moved in between.
    #[tokio::test]
    async fn flush_ready_queues_delivers_to_the_tagged_agent_not_the_focused_one() {
        let conway = echo_conway_and_store().0;
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        let child = app
            .handle
            .spawn(root, conway::SpawnSpec::new("hello").keep_alive(true))
            .await
            .expect("keep-alive spawn should succeed");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !app.handle.awaiting_prompt(child) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the spawned child's own first turn must reach idle");

        // Queue a message for the CHILD while it is focused, then move
        // focus back to root before flushing -- the exact "focus moved
        // between queuing and delivery" scenario review round 1's CRITICAL
        // finding describes.
        app.state.focus_agent(child);
        app.state.queue_prompt(child, "for the child".to_string());
        app.state.focus_agent(root);

        assert!(app.flush_ready_queues().await);

        let child_records = app
            .handle
            .transcript(child)
            .await
            .expect("child transcript must be readable");
        assert!(
            child_records.iter().any(
                |r| matches!(r, conway::LogRecord::UserTurn { text, .. } if text == "for the child")
            ),
            "the message must reach the CHILD it was queued for: {child_records:?}"
        );
        let root_records = app
            .handle
            .transcript(root)
            .await
            .expect("root transcript must be readable");
        assert!(
            !root_records
                .iter()
                .any(|r| matches!(r, conway::LogRecord::UserTurn { .. })),
            "the currently-focused root must never receive a message queued for \
             a different agent: {root_records:?}"
        );
    }

    /// Review round 1, CRITICAL: an agent that has already finished must
    /// never be sent to -- its held prompts are surfaced as a notice
    /// instead, and (since it is the focused agent with an empty input
    /// box here) the newest is restored for editing.
    #[tokio::test]
    async fn flush_ready_queues_never_delivers_to_a_finished_agent_and_restores_the_newest() {
        let conway = echo_conway_and_store().0;
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        // A NON-keep-alive child finishes on its own once its one turn
        // completes -- the echo backend replies immediately, so awaiting
        // its result is enough to guarantee it is genuinely terminal
        // before this test queues anything for it.
        let child = app
            .handle
            .spawn(root, conway::SpawnSpec::new("hello"))
            .await
            .expect("spawn should succeed");
        app.handle
            .await_agent(child)
            .await
            .expect("the child's one turn must complete");

        app.state.focus_agent(child);
        app.state.queue_prompt(child, "stranded one".to_string());
        app.state.queue_prompt(child, "stranded two".to_string());

        assert!(app.flush_ready_queues().await);

        assert!(
            app.state.held_prompts.is_empty(),
            "a finished agent's held prompts must never be retried"
        );
        assert_eq!(
            app.state.input, "stranded two",
            "the NEWEST undelivered message must be restored to the input box"
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text }
                    if text.contains("stranded one") && text.contains("stranded two")
            )),
            "a notice must name every undelivered message: {:?}",
            app.state.transcript
        );
        let records = app
            .handle
            .transcript(child)
            .await
            .expect("transcript must be readable");
        assert!(
            !records.iter().any(
                |r| matches!(r, conway::LogRecord::UserTurn { text, .. } if text.starts_with("stranded"))
            ),
            "nothing must ever be sent to a finished agent: {records:?}"
        );
    }

    /// The restore-to-input-box half only fires while the AFFECTED agent
    /// is the one focused -- a stranded message for a BACKGROUND, non-
    /// focused agent must never clobber whatever the operator is looking
    /// at, only the notice fires.
    #[tokio::test]
    async fn flush_ready_queues_does_not_restore_input_for_a_non_focused_finished_agent() {
        let conway = echo_conway_and_store().0;
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        let child = app
            .handle
            .spawn(root, conway::SpawnSpec::new("hello"))
            .await
            .expect("spawn should succeed");
        app.handle
            .await_agent(child)
            .await
            .expect("the child's one turn must complete");

        app.state.focus_agent(child);
        app.state.queue_prompt(child, "stranded".to_string());
        app.state.focus_agent(root);
        app.state.input = "unrelated draft for root".to_string();

        assert!(app.flush_ready_queues().await);

        assert_eq!(
            app.state.input, "unrelated draft for root",
            "a background agent's stranded message must never clobber root's own draft"
        );
        assert!(app
            .state
            .transcript
            .iter()
            .any(|e| matches!(e, Entry::Notice { text } if text.contains("stranded"))));
    }

    /// SIGNIFICANT review round 1 finding 4: the FIRST quit attempt with a
    /// non-empty queue is refused, with a notice -- not a silent discard.
    #[tokio::test]
    async fn confirm_quit_with_nonempty_queue_refuses_the_first_attempt() {
        let conway = echo_conway_and_store().0;
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        app.state.queue_prompt(root, "important".to_string());

        assert!(!app.confirm_quit_with_nonempty_queue());
        assert!(
            app.state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::Notice { text } if text.contains("quit again"))),
            "the first refusal must leave a notice: {:?}",
            app.state.transcript
        );
        // The queue itself must survive the refused attempt.
        assert_eq!(app.state.held_prompts.len(), 1);
    }

    /// A SECOND attempt within the window actually confirms the quit.
    #[tokio::test]
    async fn confirm_quit_with_nonempty_queue_confirms_on_the_second_attempt() {
        let conway = echo_conway_and_store().0;
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();
        app.state.queue_prompt(root, "important".to_string());

        assert!(!app.confirm_quit_with_nonempty_queue());
        assert!(app.confirm_quit_with_nonempty_queue());
    }

    /// An empty queue never blocks a quit at all -- every quit path's
    /// exact pre-item behavior.
    #[tokio::test]
    async fn confirm_quit_with_an_empty_queue_never_blocks() {
        let conway = echo_conway_and_store().0;
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        assert!(app.confirm_quit_with_nonempty_queue());
        assert!(
            app.state.transcript.is_empty(),
            "an empty queue must produce no notice at all: {:?}",
            app.state.transcript
        );
    }
}

/// Review round 2, CRITICAL: regression coverage for "`turn_in_progress`
/// does not cover tool execution." Builds a REAL agent that proposes a
/// tool call and dispatches it to a tool this module fully controls (held
/// open until the test releases it, polling its own cancellation
/// cooperatively meanwhile -- the same shape `conway-runtime`'s own
/// `tests/tool_runner.rs::CancelObservingSleepTool` already establishes),
/// so `turn_in_progress` genuinely reads `false` while real tool work is
/// still in flight -- exactly the gap a combination of `turn_in_flight`/
/// `in_flight_tools` read from OUTSIDE the loop would still have, and
/// exactly why `SessionHandle::awaiting_prompt` is used instead.
#[cfg(test)]
mod tool_execution {
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use conway::test_support::test_builder;
    use conway::{Conway, Plugin, Tool};
    use conway_core::content::{
        ContentBlock, PermissionClass, StopReason, ToolCall, ToolCategory, ToolSpec,
        TruncationPolicy, Usage,
    };
    use conway_core::error::ToolError;
    use conway_core::ids::{BackendId, ToolName};
    use conway_core::ports::{GenerateResponse, PluginManifest, ToolCtx, ToolOutput};
    use conway_testkit::{text_response, FakeStore, ScriptedBackend, ScriptedTurn};

    use super::super::fixtures::{base_config, minimal_cli};
    use super::super::App;

    /// The legibility bound every `tokio::time::timeout` in this module
    /// uses to convert a genuine hang into a failed test rather than one
    /// that never returns -- mirrors `app/ask.rs`'s own `HANG_TIMEOUT` and
    /// its doc's reasoning exactly (orders of magnitude above the slowest
    /// plausible legitimate run, paid only on the rare occasion a bug
    /// actually makes it necessary).
    const HANG_TIMEOUT: Duration = Duration::from_secs(10);
    /// The poll interval `wait_until_mid_tool_call` uses inside that bound.
    const POLL: Duration = Duration::from_millis(5);

    /// A tool that blocks until [`HeldTool::gate`] is notified -- lets a
    /// test hold a REAL tool call open for as long as it needs to observe
    /// "busy" state, then release it on demand. (This module used to also
    /// exercise cancellation cooperatively observed from inside the tool;
    /// that machinery was removed along with `busy_input = interrupt` --
    /// see this file's own module doc.)
    struct HeldTool {
        gate: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl Tool for HeldTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: ToolName::new("held"),
                description: "test-only tool that blocks until released".into(),
                schema: serde_json::from_value(serde_json::json!({"type": "object"}))
                    .expect("valid schema"),
                category: ToolCategory::Read,
                permission: PermissionClass::Safe,
            }
        }

        async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
            self.gate.notified().await;
            Ok(ToolOutput {
                blocks: vec![ContentBlock::Text {
                    text: "released".into(),
                }],
                is_error: false,
                truncation: TruncationPolicy::None,
                artifacts: vec![],
            })
        }
    }

    struct HeldPlugin {
        gate: Arc<tokio::sync::Notify>,
    }

    impl Plugin for HeldPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                id: "test.held".to_string(),
                version: "0.0.0".to_string(),
                tools: vec![ToolName::new("held")],
                required_host_caps: vec![],
                optional_host_caps: vec![],
                requires: vec![],
                optional: vec![],
            }
        }

        fn tools(&self) -> Vec<Arc<dyn Tool>> {
            vec![Arc::new(HeldTool {
                gate: self.gate.clone(),
            })]
        }
    }

    fn tool_call_response(call_id: &str, tool: &str) -> GenerateResponse {
        GenerateResponse {
            content: vec![],
            tool_calls: vec![ToolCall {
                call_id: call_id.to_string(),
                name: ToolName::new(tool),
                arguments: serde_json::json!({}),
            }],
            stop: StopReason::ToolUse,
            usage: Usage::default(),
        }
    }

    /// A `Conway` whose root, once prompted, proposes exactly one call to
    /// the "held" tool, then (once released) answers with plain text --
    /// `allow_once_gate` so the call dispatches with no interactive
    /// permission prompt in the way (this module is not testing permission
    /// flow). Returns the gate the test uses to control the tool directly.
    fn conway_with_held_tool() -> (Conway, Arc<tokio::sync::Notify>) {
        let gate = Arc::new(tokio::sync::Notify::new());
        let backend = Arc::new(
            ScriptedBackend::new(vec![
                ScriptedTurn::Respond(tool_call_response("call_1", "held")),
                ScriptedTurn::Respond(text_response("final answer")),
            ])
            .with_id(BackendId::new("fake")),
        );
        let conway = test_builder(base_config())
            .with_backend(backend)
            .with_permission_gate(conway::test_support::allow_once_gate())
            .with_session_store(Arc::new(FakeStore::new()))
            .with_plugin(Arc::new(HeldPlugin { gate: gate.clone() }))
            .build()
            .expect("build should succeed with every port injected");
        (conway, gate)
    }

    /// Polls `turn_in_progress`/`awaiting_prompt` until the agent is
    /// genuinely mid-tool-call -- `turn_in_progress` already `false` (the
    /// model round-trip that proposed the call already finished) AND
    /// `awaiting_prompt` ALSO `false` (the fix this module's "review round
    /// 2" tests exist for: the turn has not genuinely ended). Bounded by
    /// [`HANG_TIMEOUT`] so a regression
    /// that never reaches this state fails loudly rather than hanging the
    /// suite.
    async fn wait_until_mid_tool_call(app: &App, agent: conway::AgentId) {
        tokio::time::timeout(HANG_TIMEOUT, async {
            loop {
                if !app.handle.turn_in_progress(agent) && !app.handle.awaiting_prompt(agent) {
                    return;
                }
                tokio::time::sleep(POLL).await;
            }
        })
        .await
        .expect("the agent must reach 'mid-tool-call' within the hang-timeout bound");
    }

    /// Review round 2, CRITICAL, acceptance test: a message queued
    /// (`busy_input = queue`; drives `AppState::queue_prompt` directly, so
    /// this is unaffected by which mode a session actually starts in --
    /// `steer`, board item `01M44PK089DF2M9TM3C4P5CKMZ`) WHILE a tool call
    /// is genuinely in flight is NOT delivered until the turn actually ends
    /// -- `flush_ready_queues` must not mistake "the model round-trip
    /// already finished" (`turn_in_progress == false`) for "the turn is
    /// over."
    #[tokio::test]
    async fn queued_message_is_not_delivered_until_the_tool_call_and_turn_genuinely_end() {
        let (conway, gate) = conway_with_held_tool();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let root = app.handle.root();

        app.submit("start".to_string())
            .await
            .expect("submit should not error");
        wait_until_mid_tool_call(&app, root).await;

        // The exact regression this finding names: `turn_in_progress` is
        // ALREADY false here (the model round-trip that proposed the tool
        // call is done), but the turn is genuinely still running.
        assert!(
            !app.handle.turn_in_progress(root),
            "sanity: round-trip done"
        );
        assert!(
            !app.handle.awaiting_prompt(root),
            "sanity: the turn must still be genuinely running, mid-tool-call"
        );

        app.state
            .queue_prompt(root, "queued while held".to_string());
        // Drive one poll -- `changed`'s own value does not matter here; the
        // assertion below is what actually proves nothing was delivered.
        let _ = app.flush_ready_queues().await;
        let records_mid_tool = app
            .handle
            .transcript(root)
            .await
            .expect("transcript must be readable");
        assert!(
            !records_mid_tool.iter().any(
                |r| matches!(r, conway::LogRecord::UserTurn { text, .. } if text == "queued while held")
            ),
            "a message queued mid-tool-call must NOT be delivered while the tool is still \
             running: {records_mid_tool:?}"
        );

        // Release the tool and let the turn genuinely conclude (the
        // scripted second round answers with plain text, ending the
        // keep-alive turn -- `awaiting_prompt` becomes `true`).
        gate.notify_one();
        tokio::time::timeout(HANG_TIMEOUT, async {
            while !app.handle.awaiting_prompt(root) {
                tokio::time::sleep(POLL).await;
            }
        })
        .await
        .expect("the turn must genuinely end within the hang-timeout bound");

        assert!(app.flush_ready_queues().await);
        let records_after = app
            .handle
            .transcript(root)
            .await
            .expect("transcript must be readable");
        assert!(
            records_after.iter().any(
                |r| matches!(r, conway::LogRecord::UserTurn { text, .. } if text == "queued while held")
            ),
            "the queued message must be delivered once the turn genuinely ends: {records_after:?}"
        );
    }
}
