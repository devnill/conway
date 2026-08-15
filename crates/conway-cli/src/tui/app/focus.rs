//! Focus-switching: resubscribing an agent's own event stream and making
//! `AppState` follow it, plus the small facade calls a switch needs to show
//! real numbers immediately rather than the zeroed reset `AppState::
//! focus_agent` leaves behind. Extracted out of `app.rs` verbatim (this
//! item, board); [`super::run`]'s own `Action::FocusAgent`/
//! `SubmitOutcome::FocusNewSession` arms are the production callers of
//! [`App::try_focus_agent`].

use super::App;
use crate::tui::commands::{self, Host};
use crate::tui::state::{Activity, Entry};

impl App {
    /// Best-effort authoritative refresh of `AppState::session_head_seq`
    /// -- the same "re-fetch rather
    /// than reconstruct" shape `Self::run`'s own `refresh_focused_usage`
    /// local already establishes for `AppState::focused_agent_usage`, one
    /// field over. Scoped to `self.handle`'s OWN session id (never the
    /// focused agent's), and called from that spot plus `Self::new` and
    /// `Effect::Resumed`'s own call site -- see the field's own doc for why
    /// `apply_plugin_command_done`'s `ForkSession` arm does NOT call this
    /// (it sets the field directly, with no round trip needed). A failed
    /// fetch just leaves whatever figure was already showing.
    pub(super) async fn refresh_session_head(&mut self) {
        if let Ok(head) = self.conway.session_head(self.handle.id()).await {
            self.state.session_head_seq = Some(head);
        }
    }

    /// Shared by `Action::FocusAgent` and `SubmitOutcome::FocusNewSession`
    /// (WI "bare /spawn & /fork open an interactive session"): resubscribes
    /// `agent`'s own event stream and switches `state`'s focus to it,
    /// returning the new stream for the run loop's `events` local to adopt.
    ///
    /// **Fallible-but-matched, not `?`-propagated (carried from the
    /// review fix this factors out of `Action::FocusAgent`'s old inline
    /// body):** `agent_events` can fail (unknown/foreign agent, store I/O,
    /// ancestry depth) -- surfaced as a `Notice`, returning `None`, rather
    /// than killing the whole interactive session over one bad focus
    /// switch.
    ///
    /// **Ordering matters:** `agent_events` is called (and must succeed)
    /// BEFORE `state.focus_agent` runs, so a failure leaves both the
    /// transcript and the caller's live subscription exactly as they were.
    ///
    /// `focus_agent` already reset
    /// `focused_agent_usage` to zero -- the `session_usage` call below is
    /// the authoritative fetch that fills in the newly focused agent's REAL
    /// cumulative total (replay carries no `Usage`, so the zero reset would
    /// otherwise stick). Best-effort: a failed fetch just leaves it at zero
    /// rather than failing the whole focus switch.
    ///
    /// T3 follow-up (this item): `focus_agent` also zeroes
    /// `focused_ctx_tokens`/`focused_model`/`focused_model_max_context` --
    /// same problem, same fix shape. `host.context_report`/`host.last_model`
    /// below are the authoritative re-fetches for those, run alongside the
    /// `session_usage` one. Each is independently best-effort: a failed
    /// fetch just leaves that one field at its post-`focus_agent` reset
    /// rather than failing the whole switch, matching `session_usage`'s own
    /// convention.
    ///
    /// `focused_seen_segments` is seeded from the fetched report's own
    /// segment ids (not left empty) -- without this, the very next LIVE
    /// `Event::ContextSegmentAdded` for a segment this fetch already
    /// counted (e.g. a non-keep-alive child's fresh `AgentLoop` re-emitting
    /// its whole existing context on the first turn of a new run --
    /// `focused_seen_segments`'s own doc) would double-count it on top of
    /// the total this fetch just established.
    ///
    /// `on_fail_extra` (Fix 3, minor review finding): appended verbatim to
    /// the failure `Notice` when `agent_events` errors -- lets
    /// `SubmitOutcome::FocusNewSession`'s call site disclose that a pending
    /// first message was ALSO dropped, rather than silently losing it with
    /// no trace in the transcript. `None` for the plain `Action::FocusAgent`
    /// call site, which has no message riding along.
    pub(super) async fn try_focus_agent(
        &mut self,
        agent: conway::AgentId,
        on_fail_extra: Option<&str>,
    ) -> Option<conway::EventStream> {
        match self.handle.agent_events(agent).await {
            Ok(stream) => {
                self.state.focus_agent(agent);
                let host = commands::LiveHost {
                    handle: &self.handle,
                    conway: &self.conway,
                    commands: &self.command_registry,
                };
                if let Ok(usage) = host.session_usage(agent).await {
                    self.state.focused_agent_usage = usage;
                }
                if let Ok(report) = host.context_report(agent).await {
                    self.state.focused_ctx_tokens = u64::from(report.total_tokens_est);
                    self.state.focused_seen_segments =
                        report.segments.iter().map(|entry| entry.segment).collect();
                }
                if let Ok(Some(model)) = host.last_model(agent).await {
                    let name = model.to_string();
                    let max = self
                        .state
                        .model_max_context
                        .get(&name)
                        .copied()
                        .or_else(|| {
                            self.state
                                .model_max_context
                                .get(model.model.as_str())
                                .copied()
                        });
                    self.state.focused_model = Some(name);
                    self.state.focused_model_max_context = max;
                }
                Some(stream)
            }
            Err(e) => {
                let mut text = format!("could not focus agent: {e}");
                if let Some(extra) = on_fail_extra {
                    text.push_str(extra);
                }
                self.state.transcript.push(Entry::Notice { text });
                None
            }
        }
    }

    /// `SubmitOutcome::FocusNewSession`'s own first-message delivery (WI
    /// "bare /spawn & /fork open an interactive session"): sends `text` as
    /// `child`'s first turn via `prompt_agent` -- best-effort, a failure
    /// becomes a `Notice` rather than propagating (the new session was
    /// already successfully created and focused by this point; losing the
    /// whole focus switch over a failed first message would be worse than
    /// just reporting it).
    ///
    /// This item: no local `Entry::User` push here either (mirroring
    /// `Self::submit`'s own tail, see its comment) -- the caller
    /// (`App::run`'s `FocusNewSession` arm) already resubscribed `events` to
    /// `child`'s own stream via `try_focus_agent` BEFORE calling this
    /// method, so the `Event::UserTurn` `prompt_agent` emits below is
    /// observed on that same, already-live subscription and rendered by
    /// `state.rs`'s `apply` exactly once.
    pub(super) async fn deliver_first_message(&mut self, child: conway::AgentId, text: String) {
        match self.handle.prompt_agent(child, text).await {
            Ok(_) => self.state.activity = Activity::Thinking,
            Err(e) => self.state.transcript.push(Entry::Notice {
                text: format!("could not deliver the first message: {e}"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{build_conway_with_echo_backend, drain_and_apply, minimal_cli};
    use super::App;
    use crate::tui::state::Entry;

    /// Acceptance test: "focus-switching to an agent with history shows its
    /// prompts as user turns." Spawns a real child with a real prompt,
    /// lets its one-shot turn finish (so `try_focus_agent`'s replay batch
    /// has real, persisted history to reconstruct, not a live tail), then
    /// focuses it and asserts the replayed prompt renders as `Entry::User`
    /// -- not `Entry::Notice`, and not string-matched.
    #[tokio::test]
    async fn focus_switch_replays_a_spawned_childs_prompt_as_a_user_turn() {
        let conway = build_conway_with_echo_backend();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let child = app
            .handle
            .spawn(
                app.handle.root(),
                conway::SpawnSpec::new("child's own prompt"),
            )
            .await
            .expect("spawn should succeed");
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            app.handle.await_agent(child),
        )
        .await;

        let mut events = app
            .try_focus_agent(child, None)
            .await
            .expect("focusing a known child must succeed");
        assert_eq!(
            app.state.focused_agent, child,
            "try_focus_agent must switch focus to the child"
        );

        drain_and_apply(&mut events, &mut app.state);

        assert!(
            app.state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::User(text) if text == "child's own prompt")),
            "the child's replayed prompt must render as Entry::User, got {:?}",
            app.state.transcript
        );
        assert!(
            !app.state.transcript.iter().any(
                |e| matches!(e, Entry::Notice { text } if text.contains("child's own prompt"))
            ),
            "the child's replayed prompt must NOT fall back to a Notice, got {:?}",
            app.state.transcript
        );
    }

    /// T3 follow-up acceptance test: focusing an agent that has already run
    /// a turn shows its real serving model and a non-zero `ctx` total
    /// IMMEDIATELY -- straight out of `try_focus_agent`, with nothing
    /// drained from the freshly-subscribed stream yet (`AppState::apply`
    /// never sees this stream at all in this test) and no live turn
    /// required. Before this item, `focus_agent` zeroed both and nothing
    /// repopulated them until the focused agent's own next LIVE
    /// `ModelDecision`/`ContextSegmentAdded` -- this asserts the fix, not
    /// just its absence of a crash.
    #[tokio::test]
    async fn focus_switch_shows_real_model_and_ctx_total_with_no_live_turn_required() {
        let conway = build_conway_with_echo_backend();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let child = app
            .handle
            .spawn(
                app.handle.root(),
                conway::SpawnSpec::new("hi from the child"),
            )
            .await
            .expect("spawn should succeed");
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            app.handle.await_agent(child),
        )
        .await;

        let _events = app
            .try_focus_agent(child, None)
            .await
            .expect("focusing a known child must succeed");

        assert_eq!(
            app.state.focused_model.as_deref(),
            Some("fake/echo-model"),
            "the serving model must be re-fetched from the child's own log, \
             not left at try_focus_agent's `None` reset"
        );
        assert!(
            app.state.focused_ctx_tokens > 0,
            "the cumulative context total must be re-fetched non-zero for an \
             agent that already ran a turn, not left at try_focus_agent's `0` reset"
        );
        assert!(
            !app.state.focused_seen_segments.is_empty(),
            "the re-fetch must also seed the dedup set from the report's own \
             segments, or the child's next live turn would double-count them"
        );
    }
}
