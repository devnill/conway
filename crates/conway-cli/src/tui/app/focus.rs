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
                // Board `01M0VWMMEG4CER8Y8VH77KZ0CV`: `focus_agent` just
                // reset `turn_started_at` to `None` -- correct for the
                // common case (a freshly focused agent with no turn in
                // flight), wrong for the one this item exists to fix: `agent`
                // was already streaming a reply when this switch happened,
                // and `Event::TurnStarted` is bus-only (never replayed), so
                // the fresh subscription above can never observe the
                // bracket that already started. `SessionHandle::
                // turn_in_progress` is the authoritative, facade-level
                // answer to "is a turn in flight for `agent` right now" --
                // see that method's own doc for why it cannot fire for a
                // pull-in's synthetic twin or a replayed assistant reply
                // (both leave it `false`, since neither one is a real
                // `AgentTree::mark_turn_started`/`mark_turn_finished`
                // bracket). Seeded with the SAME two fields `Event::
                // TurnStarted`'s own `apply` arm sets (`state.rs`), so a
                // refocus shows the working indicator immediately rather
                // than waiting for the turn's next live delta.
                if self.handle.turn_in_progress(agent) {
                    self.state.activity = Activity::Thinking;
                    self.state.turn_started_at = Some(std::time::Instant::now());
                }
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
    use super::super::fixtures::{drain_and_apply, echo_conway, minimal_cli};
    use super::App;
    use crate::tui::state::{Activity, Entry, SPINNER_FRAMES};

    /// Board `01M0VWMMEG4CER8Y8VH77KZ0CV`'s own repro backend: a `Backend`
    /// whose `stream()` yields ONE real `TextDelta`, then genuinely
    /// suspends (a `tokio::sync::Notify` await, not a busy-loop) until the
    /// test releases it, then yields a second `TextDelta` and `Done`.
    ///
    /// **Why not `conway_testkit::ScriptedTurn::Pending`:** that variant
    /// suspends `generate()`/`stream()` itself BEFORE any chunk is ever
    /// produced (`std::future::pending()`), which is exactly right for a
    /// "this agent never responds" repro (`pull_in.rs`'s still-running-child
    /// guard) but wrong for this one -- this item's own bug is specifically
    /// about REAL, already-streaming `TextDelta` chunks arriving before AND
    /// after a focus switch, so the repro needs a turn that has genuinely
    /// started producing text, not one stuck before its first byte.
    struct GatedStreamBackend {
        id: conway_core::ids::BackendId,
        gate: std::sync::Arc<tokio::sync::Notify>,
    }

    impl GatedStreamBackend {
        fn new(gate: std::sync::Arc<tokio::sync::Notify>) -> Self {
            Self {
                id: conway_core::ids::BackendId::new("fake"),
                gate,
            }
        }
    }

    #[async_trait::async_trait]
    impl conway_core::ports::Backend for GatedStreamBackend {
        fn id(&self) -> conway_core::ids::BackendId {
            self.id.clone()
        }

        fn capabilities(
            &self,
            _model: &conway_core::ids::ModelId,
        ) -> conway_core::capabilities::Capabilities {
            // `Streaming { validated: true }`, not `None`: the real `App`
            // this test drives registers built-in tools unconditionally
            // (`bash` "ships off by default and cannot be declined" --
            // `conway::config::schema::PluginsConfig`'s own doc), so
            // `attempt.rs::strategy_for`'s `has_tools` is `true` here even
            // though this test never calls a tool -- without this,
            // `strategy_for` falls back to `Strategy::Generate` and this
            // backend's deliberately-`unimplemented!` `generate` panics.
            conway_core::capabilities::Capabilities {
                tool_calling: conway_core::capabilities::ToolCallSupport::Streaming {
                    validated: true,
                },
                cache: conway_core::capabilities::CacheMode::None,
                parallel_tool_calls: false,
                structured_output: conway_core::capabilities::StructuredOutput::None,
                max_context_tokens: 128_000,
                reasoning: false,
                reliability_tier: conway_core::capabilities::ReliabilityTier::Unknown,
            }
        }

        async fn generate(
            &self,
            _req: conway_core::ports::GenerateRequest,
        ) -> Result<conway_core::ports::GenerateResponse, conway_core::error::BackendError>
        {
            unimplemented!(
                "this backend's own `capabilities()` declares \
                 `Streaming {{ validated: true }}`, so `attempt.rs::strategy_for` \
                 always chooses `Strategy::Stream` -- `generate` is never called"
            )
        }

        async fn stream(
            &self,
            _req: conway_core::ports::GenerateRequest,
        ) -> Result<
            conway_core::ports::BoxStream<
                'static,
                Result<conway_core::ports::StreamChunk, conway_core::error::BackendError>,
            >,
            conway_core::error::BackendError,
        > {
            let gate = self.gate.clone();
            let done = conway_core::ports::GenerateResponse {
                content: vec![conway_core::content::ContentBlock::Text {
                    text: "first chunk more".to_string(),
                }],
                tool_calls: vec![],
                stop: conway_core::content::StopReason::EndTurn,
                usage: conway_core::content::Usage::default(),
            };
            let stream = futures::stream::unfold(0u8, move |step| {
                let gate = gate.clone();
                let done = done.clone();
                async move {
                    match step {
                        0 => Some((
                            Ok(conway_core::ports::StreamChunk::TextDelta(
                                "first chunk ".to_string(),
                            )),
                            1,
                        )),
                        // The genuine suspend point: the test drives a
                        // focus-away-and-back sequence while this future is
                        // parked here, un-notified.
                        1 => {
                            gate.notified().await;
                            Some((
                                Ok(conway_core::ports::StreamChunk::TextDelta(
                                    "more".to_string(),
                                )),
                                2,
                            ))
                        }
                        // Deliberately never resolves (mirrors
                        // `conway_testkit::ScriptedTurn::Pending`'s own
                        // `std::future::pending()`), so `Event::TurnFinished`
                        // never fires and cannot race the test's own
                        // post-"more" assertion -- a real `Done` here would
                        // let `run_inner`'s per-round finish (`clear_turn_
                        // state`'s own `TurnFinished` arm) reset `activity`
                        // back to `Idle` in the SAME `drain_and_apply` batch
                        // as the "more" delta, which is correct production
                        // behavior (a finished round legitimately goes
                        // idle) but would make this test unable to observe
                        // the moment this item's fix is actually
                        // responsible for.
                        2 => {
                            let _ = done;
                            std::future::pending::<()>().await;
                            unreachable!("never notified a second time")
                        }
                        _ => None,
                    }
                }
            });
            Ok(Box::pin(stream))
        }

        async fn probe(
            &self,
        ) -> Result<conway_core::capabilities::ProbeReport, conway_core::error::BackendError>
        {
            unimplemented!("not exercised by this test")
        }
    }

    /// [`echo_conway`]'s shape, over [`GatedStreamBackend`] instead of the
    /// echo backend, for the one test that needs a turn it can genuinely
    /// pause mid-stream.
    fn conway_with_gated_backend(gate: std::sync::Arc<tokio::sync::Notify>) -> conway::Conway {
        conway::test_support::build_conway(
            super::super::fixtures::base_config(),
            std::sync::Arc::new(GatedStreamBackend::new(gate)),
            std::sync::Arc::new(conway_testkit::FakeStore::new()),
        )
    }

    /// Acceptance test: "focus-switching to an agent with history shows its
    /// prompts as user turns." Spawns a real child with a real prompt,
    /// lets its one-shot turn finish (so `try_focus_agent`'s replay batch
    /// has real, persisted history to reconstruct, not a live tail), then
    /// focuses it and asserts the replayed prompt renders as `Entry::User`
    /// -- not `Entry::Notice`, and not string-matched.
    #[tokio::test]
    async fn focus_switch_replays_a_spawned_childs_prompt_as_a_user_turn() {
        let conway = echo_conway();
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

    /// This item's own replay-sibling acceptance test (board: "pulling in
    /// an /ask answer wedges the status bar in a working state forever" --
    /// the item's own text names this as "a likely sibling ... NOT verified
    /// empirically"). `record_to_event` (`conway/src/session_handle.rs`)
    /// maps a replayed `LogRecord::Assistant` to the SAME bare
    /// `Event::TextDelta` shape `Runtime::pull_in`'s merge twin uses, with
    /// no synthesized `Event::TurnStarted` either -- so a focus switch onto
    /// an agent whose persisted history ends in an assistant reply replays
    /// that same wedge shape, unless the `TextDelta` arm's
    /// `turn_started_at.is_some()` gate (this item) also covers it.
    ///
    /// Non-vacuous: `try_focus_agent`'s own `state.focus_agent` call resets
    /// `activity` to `Idle` BEFORE the replay batch below is ever applied
    /// (asserted explicitly), so the closing assertion is not "stayed at
    /// its untouched default" -- it is "the replay batch, once actually
    /// drained through the real `apply`, did not move it away". Confirmed to
    /// fail pre-fix (this item's own report quotes the output): with the
    /// `turn_started_at` gate reverted, this same replay batch left
    /// `activity` at `Responding` after the drain below.
    ///
    /// **Deliberately a KEEP-ALIVE child that is re-promptable, not a
    /// one-shot spawn.** A one-shot child's log ends with an
    /// `AgentResultRecord` after its `Assistant` reply, which
    /// `record_to_event` maps to `Event::AgentFinished` -- and `apply`'s own
    /// `AgentFinished` arm resets `activity` back to `Idle` regardless of
    /// this item's gate, masking the exact bug this test exists to catch.
    /// A keep-alive child's log genuinely ends on the bare `Assistant`
    /// record (`keep_alive_spawn_starts_idle_with_no_own_records_then_runs_
    /// and_is_repromptable`, `conway/tests/session_handle_subagent.rs`, is
    /// the source for this shape) -- no finish record ever follows, which is
    /// exactly why the item's own text calls this sibling out as
    /// independently dangerous.
    #[tokio::test]
    async fn focus_switch_replaying_an_assistant_reply_does_not_wedge_activity_responding() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        // Bare `/spawn`'s own shape (`commands.rs::bare_spawn`): empty
        // prompt, `keep_alive: true` -- the child idles with NO records of
        // its own until prompted.
        let child = app
            .handle
            .spawn(
                app.handle.root(),
                conway::SpawnSpec::new("").keep_alive(true),
            )
            .await
            .expect("keep-alive spawn should succeed");

        // The echo backend replies with the prompt text verbatim, so this
        // real turn completes with a persisted `LogRecord::Assistant` reply
        // -- the shape `record_to_event`'s `Assistant` arm replays as
        // `TextDelta` -- and, being keep-alive, the child's log ends
        // THERE: no `AgentResultRecord`/`Event::AgentFinished` follows to
        // mask the gate under test.
        let turn = app
            .handle
            .prompt_agent(child, "what time is it")
            .await
            .expect("prompt_agent must drive the idle keep-alive child's first turn");
        tokio::time::timeout(std::time::Duration::from_secs(5), turn.text())
            .await
            .expect("turn.text() must not hang")
            .expect("turn.text() should resolve");

        let mut events = app
            .try_focus_agent(child, None)
            .await
            .expect("focusing a known child must succeed");
        assert_eq!(
            app.state.activity,
            crate::tui::state::Activity::Idle,
            "sanity: AppState::focus_agent resets activity to Idle BEFORE the \
             replay batch below is applied -- this is the reset the drain \
             must not move away from"
        );

        drain_and_apply(&mut events, &mut app.state);

        // The replay really did reach the assistant reply -- otherwise the
        // assertion below would not be exercising anything.
        assert!(
            app.state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::Assistant { .. })),
            "the child's replayed assistant reply must have rendered, got {:?}",
            app.state.transcript
        );
        assert_eq!(
            app.state.activity,
            crate::tui::state::Activity::Idle,
            "a replayed assistant reply carries no bracketing TurnStarted \
             either -- it must not wedge activity at Responding, same as a \
             pull-in merge's twin must not"
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
        let conway = echo_conway();
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

    /// THIS ITEM'S OWN ACCEPTANCE TEST (board `01M0VWMMEG4CER8Y8VH77KZ0CV`):
    /// focusing away from a streaming agent and back before its turn ends
    /// must not lose the working indicator.
    ///
    /// Built at the `AppState`/`App` level with the real `drain_and_apply`
    /// harness, per the item's own "determine before building" instruction
    /// -- no compiled binary, real focus switches, real events.
    /// [`GatedStreamBackend`] is what makes "still mid-turn" a fact the test
    /// controls rather than a race: the agent's `TurnStarted` has fired and
    /// its `Event::TurnFinished` provably has not, because the backend's
    /// `stream()` is parked on a `Notify` the test itself releases.
    ///
    /// Pre-fix, this test fails exactly where the item's own report quotes
    /// it: the assertion right after the refocus, `left: Idle, right:
    /// Thinking` (reverting `App::try_focus_agent`'s `turn_in_progress`
    /// seed reproduces it -- see this item's completion report for the
    /// verbatim output).
    #[tokio::test]
    async fn focus_away_and_back_mid_turn_keeps_the_working_indicator() {
        let gate = std::sync::Arc::new(tokio::sync::Notify::new());
        let conway = conway_with_gated_backend(gate.clone());
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        // Keep-alive, like the replay-sibling test above: its own loop task
        // is already running, idling at `resume_gate`, ready to consume a
        // prompt the instant one lands.
        let child = app
            .handle
            .spawn(
                app.handle.root(),
                conway::SpawnSpec::new("").keep_alive(true),
            )
            .await
            .expect("keep-alive spawn should succeed");

        // Focus the child BEFORE it has ever run a turn -- mirrors the real
        // sequence (the operator is already watching this agent when its
        // reply starts streaming). `turn_in_progress` must read `false`
        // here: nothing has happened yet.
        let mut events = app
            .try_focus_agent(child, None)
            .await
            .expect("focusing a known child must succeed");
        assert_eq!(
            app.state.activity,
            Activity::Idle,
            "sanity: a freshly focused, never-prompted agent starts idle"
        );

        let _turn = app
            .handle
            .prompt_agent(child, "what time is it")
            .await
            .expect("prompt_agent must drive the keep-alive child's first turn");

        // Same synchronization idiom `conway/tests/pull_in.rs` uses for its
        // own `ScriptedTurn::Pending` repro: a real sleep, not a busy poll,
        // gives the child's own background loop task a genuine scheduling
        // turn to run everything synchronous up to the gate (`TurnStarted`
        // emitted, `mark_turn_started` recorded, first `TextDelta` emitted,
        // then parked on `gate.notified()`).
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        drain_and_apply(&mut events, &mut app.state);

        // Non-vacuous, P-15: establish the ABSENT-then-present shape before
        // claiming the fix restores it. This is the state BEFORE any focus
        // switch -- genuinely streaming, not the default.
        assert_eq!(
            app.state.activity,
            Activity::Responding,
            "sanity: the child must already be genuinely streaming before any \
             focus switch, or this test cannot tell a real fix from a no-op"
        );
        assert!(
            app.state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::Assistant { .. })),
            "sanity: the first chunk must have rendered as a real assistant \
             entry, got {:?}",
            app.state.transcript
        );

        // Focus AWAY -- to root, an unrelated agent -- exactly the
        // "switched away" half of the item's own repro shape. This drops
        // the child-scoped subscription (`agent_events`'s stream) entirely;
        // nothing about the child's still-running turn is observed from
        // here until refocused.
        let root = app.handle.root();
        let _root_events = app
            .try_focus_agent(root, None)
            .await
            .expect("focusing root must succeed");
        assert_eq!(
            app.state.activity,
            Activity::Idle,
            "focus_agent resets activity for the newly focused agent (root, \
             which has never run a turn) -- expected, not the bug"
        );

        // Focus BACK onto the child WHILE its turn is still genuinely in
        // flight: the backend is still parked on `gate`, so `Event::
        // TurnFinished` has provably not fired. This is the exact
        // resubscribe this item's report traces: `agent_events` cannot
        // replay a bus-only `Event::TurnStarted` that already fired before
        // this subscription existed.
        let mut events = app
            .try_focus_agent(child, None)
            .await
            .expect("refocusing the child must succeed");

        // ACCEPTANCE 1: the working indicator survives the switch --
        // asserted immediately, with NOTHING drained from the fresh
        // subscription yet, so this is `try_focus_agent`'s own seed, not a
        // side effect of the replay batch.
        assert_eq!(
            app.state.activity,
            Activity::Thinking,
            "the working indicator must survive focusing away and back onto \
             an agent whose turn has not yet finished"
        );
        assert!(
            app.state.turn_started_at.is_some(),
            "turn_started_at must be seeded on refocus so the elapsed clock \
             and the TextDelta gate both read a turn as in flight"
        );

        // Prove the seed is not merely cosmetic: the turn's REAL remaining
        // `TextDelta` must still reach `Entry::Assistant` and flip
        // `activity` back to `Responding` through the ordinary, unchanged
        // gate (`turn_started_at.is_some()`) -- not a special case for the
        // seeded value.
        gate.notify_one();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        drain_and_apply(&mut events, &mut app.state);

        assert_eq!(
            app.state.activity,
            Activity::Responding,
            "the turn's remaining live TextDelta must flip activity back to \
             Responding through the ordinary gate, using the seeded \
             turn_started_at"
        );

        // ACCEPTANCE 3: the rendered line, not only the enum -- both the
        // status line's spinner/\"responding…\" label and the transcript's
        // own streaming cursor read `activity`, and a fix that satisfies
        // one but not the other is half a fix.
        // 150 columns, not the crate's usual 80x24 default: at 80 columns
        // the status line's own width-aware field ladder (`view/status.rs`)
        // legitimately drops the LOWER-priority `activity` field entirely
        // before `mode`/`hint` give up anything (verified directly: at 300
        // columns the very same state renders `"... | ⠋ responding… 0s · +0
        // tok | ..."`) -- unrelated to this item, and not something a wider
        // render is "cheating" around, just a real terminal size where the
        // configured Lean field set actually fits.
        let text = crate::tui::test_support::render_text(&app.state, 150, 24);
        assert!(
            SPINNER_FRAMES.iter().any(|glyph| text.contains(glyph)),
            "the rendered status line must show the working spinner: {text}"
        );
        assert!(
            text.contains("responding…"),
            "the rendered status line must show the responding… label: {text}"
        );
        assert!(
            text.contains('▌'),
            "the rendered transcript must show the streaming cursor on the \
             live assistant line: {text}"
        );
    }
}
