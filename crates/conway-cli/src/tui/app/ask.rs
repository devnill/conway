//! The `/ask` (B5) modal's own async completion: forking an ephemeral
//! child and draining its single turn, off the app loop's own task so a
//! slow/failed answer never blocks input handling. Extracted out of
//! `app.rs` verbatim (original split item, board). [`super::run`]'s own
//! `modal_ask_rx.recv()` arm is the production consumer of [`AskUpdate`].
//!
//! [`App::spawn_modal_ask`] (board item `01KZVZ5XV162XCQR96AQKCCCF7`) is the
//! actual `tokio::spawn` call site: `commands::execute`'s `SlashCommand::
//! Ask` arm cannot spawn this itself (it has no live `SessionHandle` to
//! clone and no `modal_ask_tx` -- see `commands::Effect::RunModalAsk`'s own
//! doc), so it validates and hands back that effect, and `App::submit`'s
//! shared `Effect` match calls this method, mirroring `plugin_cmd::App::
//! spawn_plugin_command`'s own shape exactly.
//!
//! **Board item `01M0RWFH6V709B7WTAFRZGFKG3` widened this module's own
//! shape.** Before this item, the spawned task sent exactly one message
//! (the finished [`ModalAskOutcome`]) once the child's whole turn was
//! done -- so nothing in `AppState` ever learned the child's `AgentId`
//! until the ask had ALREADY finished, which is too late to cancel it.
//! [`AskUpdate::Started`] is the fix: sent the moment `SessionHandle::ask`
//! itself returns (the fork succeeded), before `text()` is even awaited,
//! so [`App::abandon_ask`] has a target to cancel while the ask is still
//! genuinely in flight. [`AskUpdate::Done`] is the pre-existing message,
//! renamed into this enum's second variant, unchanged in shape.

use conway::SessionHandle;

use super::App;

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

/// One message from a spawned `/ask` task (this module's own doc explains
/// why there are now two, board item `01M0RWFH6V709B7WTAFRZGFKG3`).
pub(super) enum AskUpdate {
    /// The fork succeeded and `child` is now a real, running agent --
    /// sent immediately, before the child's turn is drained. This is the
    /// ONLY way `AppState::ask_child` is ever populated; without it there
    /// is no target for a keyboard abandon to cancel until the ask has
    /// already finished on its own.
    Started { child: conway::AgentId },
    /// The child's single turn is done (answered, errored, or -- once
    /// abandoned -- wound down by cancellation): the pre-existing outcome,
    /// unchanged in shape.
    Done(ModalAskOutcome),
}

/// Drives one `/ask` (B5) to completion: `SessionHandle::ask` forks an
/// ephemeral child (attaching it as a proper fork child of the asker --
/// post-B2, so its `AgentSpawned` reaches the `/agents` tree marked
/// `(ephemeral)`) and returns a `TurnHandle` over it (exactly like
/// `SessionHandle::prompt`, but scoped to that throwaway child). Sends
/// [`AskUpdate::Started`] the moment the fork succeeds and
/// [`AskUpdate::Done`] once the child's WHOLE run is over (however it
/// ends) -- see this module's own doc.
///
/// **`turn.result()`, deliberately, not `turn.text()`** (board item
/// `01M0RWFH6V709B7WTAFRZGFKG3`, a correction made after this item's own
/// verification sweep first shipped with `text()` and a flaky test).
/// `TurnHandle::text`'s own doc: it stops draining "as soon as it sees
/// `Event::AgentFinished`... **or, if the agent finishes within the same
/// generation**" -- and `agent_loop.rs`'s own module doc states
/// `Event::TurnFinished` "is emitted immediately after the assistant
/// record is appended, **before any tool call is dispatched**. A 'turn' is
/// one model generation; tool execution feeds the *next* turn's context,
/// not the current one's completion event." So whenever the ask child's
/// FIRST model generation proposes a tool call, `text()` returns the
/// moment that first (often textless) generation is recorded --
/// deterministically, not intermittently -- long before the tool's
/// permission decision is even sent to the gate, let alone answered.
/// Measured directly: instrumented tracing showed `text()` resolving to
/// `Ok("")` microseconds after the fork, with `TuiGate::check`'s own
/// "sending request" print not yet reached. `result()` instead resolves
/// only on `Event::AgentFinished` for the WHOLE agent (every generation,
/// however many tool rounds it took) -- exactly the primitive
/// `SubagentHost::ask` (`conway-runtime/src/subagent.rs`, what the
/// `conway_ask` TOOL's own child-await goes through) already uses for this
/// identical "await one full ask, however long it runs" need; this mirrors
/// that established, working precedent rather than inventing a second one.
/// `AgentResult.summary` is the reply text -- an ask child is never
/// `keep_alive` (`SessionHandle::ask`'s own spec construction), so
/// `result()`'s one documented hazard ("hangs for the lifetime of the
/// session" on a keep-alive turn that completes normally) does not apply
/// here.
///
/// This was the ACTUAL cause of `abandon_ask_leaves_no_running_agent_and_
/// no_dangling_session`'s flakiness, not scheduling contention: `text()`'s
/// premature `Done` and the tool-call permission request reaching
/// `gate_rx` were two independently-ready events racing inside `tokio::
/// select!` in that test's own wait loop, so no `HANG_TIMEOUT` bound --
/// however large -- could have fixed it (there was never a second `Done`
/// coming once the first was silently discarded by a loop iteration that
/// wasn't listening for it yet). Widening the timeout before finding this
/// only made the failure slower, not rarer -- disclosed here because that
/// is precisely the "read past the warning" pattern this project has
/// flagged before. A free function (not an `App` method) since it owns
/// none of `App`'s state -- it runs inside a `tokio::spawn`ed task that
/// outlives any single `submit` call, so it cannot borrow `self`.
pub(super) async fn run_modal_ask(
    handle: SessionHandle,
    question: String,
    tx: tokio::sync::mpsc::UnboundedSender<AskUpdate>,
) {
    match handle.ask(question.clone()).await {
        Ok(turn) => {
            let child = turn.agent();
            // The receiver only goes away when `App::run`'s loop has
            // already exited -- nothing left to notify, so a send failure
            // here is silently dropped, same as the final send below.
            let _ = tx.send(AskUpdate::Started { child });
            let reply = turn.result().await.map(|r| r.summary);
            let _ = tx.send(AskUpdate::Done(ModalAskOutcome {
                question,
                child: Some(child),
                reply,
            }));
        }
        Err(e) => {
            let _ = tx.send(AskUpdate::Done(ModalAskOutcome {
                question,
                child: None,
                reply: Err(e),
            }));
        }
    }
}

impl App {
    /// Spawns the `/ask` (B5) task off this loop's own `select!`, never on
    /// it -- see this module's own doc and `commands::Effect::
    /// RunModalAsk`'s for why this specific method (not `commands::
    /// execute` itself) is what does the actual `tokio::spawn`.
    /// `commands::execute`'s `SlashCommand::Ask` arm has already validated
    /// `question` and set `state.ask_in_flight`/`state.ask_started_at`
    /// before returning the effect that reaches this call.
    pub(super) fn spawn_modal_ask(&self, question: String) {
        let handle = self.handle.clone();
        let tx = self.modal_ask_tx.clone();
        tokio::spawn(async move {
            run_modal_ask(handle, question, tx).await;
        });
    }

    /// Abandons the in-flight `/ask` from the keyboard (board item
    /// `01M0RWFH6V709B7WTAFRZGFKG3`, the fourth way out an in-flight ask
    /// previously had none of): a no-op if no ask is in flight. Two steps,
    /// in this order, both best-effort and both needed --
    /// `SessionHandle::cancel` ALONE cannot free a child parked awaiting a
    /// permission decision (see `AppState::discard_prompts_for_agent`'s own
    /// doc for the mechanism, confirmed by this item's own reproduction
    /// test), so the pending prompt is discarded FIRST:
    ///
    /// 1. [`crate::tui::state::AppState::discard_prompts_for_agent`] on the
    ///    ask child, if one is known yet (`state.ask_child`) -- frees a
    ///    child stuck on the gate.
    /// 2. `SessionHandle::cancel` (immediate mode) on the same child --
    ///    trips its `CancellationToken`, which the agent loop DOES check at
    ///    every other cooperative point (a backend call in flight, a
    ///    dispatched tool batch) -- covers the ordinary "just thinking, no
    ///    tool involved" case step 1 does nothing for.
    ///
    /// Marks `state.ask_abandoned` so `App::run`'s own `AskUpdate::Done` arm
    /// purges the child once it actually reaches a terminal state (never
    /// synchronously here -- an immediate `purge` on a child that has not
    /// yet finished would reproduce the exact `RuntimeError::Store(
    /// StoreError::NotRemovable)` error this item was filed over) instead
    /// of opening the answer modal over a question nobody is waiting on any
    /// more. When `state.ask_child` is still `None` (the operator abandoned
    /// before `AskUpdate::Started` even arrived -- the fork is still in
    /// flight), only the flag is set here; `App::run`'s `Started` arm
    /// finishes the job the moment the child id is known.
    pub(super) async fn abandon_ask(&mut self) {
        if !self.state.ask_in_flight {
            return;
        }
        self.state.ask_abandoned = true;
        if let Some(child) = self.state.ask_child {
            self.cancel_ask_child(child).await;
        }
        self.state
            .transcript
            .push(crate::tui::state::Entry::Notice {
                text: "ask abandoned -- cleaning up".to_string(),
            });
    }

    /// The two-step free-a-gate-stuck-child sequence [`Self::abandon_ask`]'s
    /// own doc describes, factored out so `Self::run`'s `AskUpdate::Started`
    /// arm can apply it immediately for an abandon that arrived before the
    /// child id was even known (`abandon_ask` itself cannot reach this
    /// case -- `state.ask_child` is `None` at that point).
    pub(super) async fn cancel_ask_child(&mut self, child: conway::AgentId) {
        self.state.discard_prompts_for_agent(child);
        if let Err(e) = self.handle.cancel(child, "ask abandoned").await {
            self.state
                .transcript
                .push(crate::tui::state::Entry::Notice {
                    text: format!("could not cancel the /ask child: {e}"),
                });
        }
    }
}

#[cfg(test)]
mod tests {
    //! Board item `01M0RWFH6V709B7WTAFRZGFKG3`: reproductions of the
    //! reported "asking what time it is hangs the menu, with no way to
    //! resolve or cancel" symptom, driven at the level `App::run`'s own
    //! `select!` operates at -- `modal_ask_rx` (the spawned `/ask` task's
    //! reply) raced against a REAL `TuiGate`'s `gate_rx` (the permission
    //! channel a tool call inside the ask child's turn blocks on), exactly
    //! the two channel arms `run.rs` polls concurrently. `App::run` itself
    //! is not driven (it owns a real terminal -- see `app.rs`'s own module
    //! doc); these tests reproduce the channel arms directly, which is the
    //! whole of the mechanism in question.
    //!
    //! **Written and run against unmodified code before any fix landed**
    //! (this item's own instruction). Two findings, in tension, both
    //! load-bearing for the fix below:
    //!
    //! 1. [`ask_child_tool_call_is_answerable_exactly_as_an_interactive_
    //!    operator_would`] proves the permission request DOES reach the
    //!    gate (mode is `Normal` during the ask's flight -- no modal has
    //!    opened yet -- so `offer_prompt` promotes it to
    //!    `AwaitingPermission` rather than queuing behind one), and that
    //!    answering it the way an operator would (a `y` keypress through
    //!    the real `input::handle_key` + `AppState::resolve_current_prompt`)
    //!    lets the ask reach a resolution. **There is no mode-stacking
    //!    deadlock** -- the hypothesis this item's own spec flagged as
    //!    "verify before fixing" does not hold.
    //! 2. [`cancelling_an_in_flight_ask_does_not_unblock_a_child_stuck_on_
    //!    the_gate_today`] is the REAL mechanism: `SessionHandle::cancel`
    //!    (even `CancelMode::Immediate`) only trips a `CancellationToken`
    //!    that the agent loop checks cooperatively at specific points
    //!    (`conway-runtime/src/agent_loop.rs`) -- and the call site that
    //!    blocks on a permission decision
    //!    (`conway-runtime/src/tools/runner.rs`'s `broker.decide(..).await`,
    //!    BEFORE the `tokio::select!` that races the tool's own `invoke`
    //!    against cancellation) is never one of them. A child parked
    //!    awaiting the gate's reply is untouched by cancellation and stays
    //!    running -- which is exactly why `purge` (which refuses a running
    //!    agent) produces the reported error. The only thing that unblocks
    //!    it today is answering the prompt, or dropping its reply sender
    //!    (`TuiGate::check`'s own fail-closed `Deny { reason: "cancelled"
    //!    }` fallback) -- which nothing in the abandon/quit path did before
    //!    this item.
    //!
    //! Together: an in-flight ask is not doomed by mode-stacking, but
    //! abandoning one that is waiting on a tool permission needs more than
    //! `cancel()` -- it needs the pending prompt discarded too. That is
    //! what `App::abandon_ask`/`App`'s quit path (`shutdown.rs`) now do.

    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use conway::test_support::test_builder;
    use conway::{Conway, PermissionGate, Plugin, Tool};
    use conway_core::content::{
        ContentBlock, PermissionClass, StopReason, ToolCall, ToolCategory, ToolSpec,
        TruncationPolicy, Usage,
    };
    use conway_core::error::ToolError;
    use conway_core::ids::{BackendId, ToolName};
    use conway_core::ports::{GenerateResponse, PluginManifest, ToolCtx, ToolOutput};
    use conway_testkit::{text_response, ScriptedBackend, ScriptedTurn};

    use super::super::fixtures::{base_config, minimal_cli};
    use super::{App, AskUpdate};
    use crate::tui::gate::{GateReceiver, TuiGate};
    use crate::tui::input::{self, Action};
    use crate::tui::state::Mode;
    use crate::tui::test_support::key;

    /// Board item `01M0RWFH6V709B7WTAFRZGFKG3` (follow-up correction, same
    /// item): every `tokio::time::timeout` bound in this module -- unless
    /// individually marked otherwise -- exists ONLY to convert a genuine
    /// hang into a legible test failure, never to assert that the work
    /// under test completes promptly. That distinction matters because
    /// this bound's own first version was tuned near the UNCONTENDED
    /// duration it was measuring (~200ms observed, bound set to 5s, later
    /// 10s), and was WRONGLY diagnosed as merely flaking under ordinary
    /// desktop CPU contention -- widening it to 10s did not fix
    /// `abandon_ask_leaves_no_running_agent_and_no_dangling_session`'s own
    /// flake (still ~1 run in 3-9), because widening was treating the
    /// wrong cause: that test's ACTUAL defect was a genuine, always-present
    /// race between two already-ready `tokio::select!` branches (see
    /// `run_modal_ask`'s own doc for the root cause and its fix -- `text()`
    /// vs `result()`), not scheduling variance. No `timeout` bound of any
    /// size fixes a race between two events that are both always ready;
    /// disclosed here rather than silently corrected, because shipping a
    /// bigger number and calling it fixed is exactly the "read past the
    /// warning" pattern this project has flagged before. This constant is
    /// still the right shape for the OTHER bounds in this file, which are
    /// genuine hang-vs-slow legibility bounds: the failure mode they guard
    /// against -- a child permanently parked on the gate with nothing to
    /// free it -- is UNBOUNDED (it never resolves on its own), not slow, so
    /// the right comparison for a legibility bound is "orders of magnitude
    /// above the slowest plausible legitimate run," not "close to the
    /// typical run." A green run pays this cost only on the rare occasion
    /// it is actually needed; a genuinely hung run still fails, just not
    /// instantly. The waits themselves are already event-driven, not
    /// polled (`mpsc::UnboundedReceiver::recv` resolves the instant a
    /// message arrives; `SessionHandle::await_agent` delegates to
    /// `Runtime::await_result`, itself a notification wait, not a poll
    /// loop -- see that method's own doc), so there is no tighter,
    /// deterministic replacement available for this bound to fall back
    /// from; widening its magnitude IS the fix.
    const HANG_TIMEOUT: Duration = Duration::from_secs(60);

    /// A trivial always-succeeds tool -- only its INVOCABILITY (which
    /// requires a permission decision, since `base_config`'s
    /// `PermissionsConfig::default()` is `Prompt` mode) matters here, not
    /// its output. Mirrors `conway/tests/ask.rs`'s own `MarkerTool` (a
    /// separate crate's private test fixture, not reusable from here).
    struct MarkerTool;

    #[async_trait]
    impl Tool for MarkerTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: ToolName::new("marker"),
                description: "test-only marker tool".into(),
                schema: serde_json::from_value(serde_json::json!({"type": "object"})).unwrap(),
                category: ToolCategory::Read,
                permission: PermissionClass::Safe,
            }
        }

        async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput {
                blocks: vec![ContentBlock::Text {
                    text: "marked".into(),
                }],
                is_error: false,
                truncation: TruncationPolicy::None,
                artifacts: vec![],
            })
        }
    }

    struct MarkerPlugin;

    impl Plugin for MarkerPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                id: "test.marker".to_string(),
                version: "0.0.0".to_string(),
                tools: vec![ToolName::new("marker")],
                required_host_caps: vec![],
            }
        }

        fn tools(&self) -> Vec<Arc<dyn Tool>> {
            vec![Arc::new(MarkerTool)]
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

    /// A real `TuiGate` (not `FakeGate` -- production wiring, `main.rs`'s
    /// own shape) wired into a fresh `Conway` whose backend scripts the
    /// ask child's turn as ONE tool call (`marker`, which needs a
    /// permission decision under the default `Prompt` mode) followed by a
    /// final text reply -- so the child's turn cannot finish until
    /// something answers the gate. Returns the matching `GateReceiver`
    /// (the app loop's own half) so the test can play the operator.
    fn conway_with_real_gate() -> (Conway, GateReceiver, Arc<ScriptedBackend>) {
        let (gate, gate_rx) = TuiGate::channel();
        let gate: Arc<dyn PermissionGate> = Arc::new(gate);
        let backend = Arc::new(
            ScriptedBackend::new(vec![
                ScriptedTurn::Respond(tool_call_response("call_1", "marker")),
                ScriptedTurn::Respond(text_response("ask done")),
            ])
            .with_id(BackendId::new("fake")),
        );
        let conway = test_builder(base_config())
            .with_backend(backend.clone())
            .with_permission_gate(gate)
            .with_plugin(Arc::new(MarkerPlugin))
            .build()
            .expect("build should succeed with every port injected");
        (conway, gate_rx, backend)
    }

    /// **Reproduction 1 (the negative control): mode-stacking is NOT the
    /// deadlock.** Submits `/ask` against a child whose only turn proposes
    /// a tool call, then races `modal_ask_rx` (the spawned task's eventual
    /// reply) against `gate_rx` (the SAME channel `App::run`'s own
    /// `select!` polls) inside one bounded `tokio::time::timeout` -- macOS
    /// has no `timeout(1)`, so the bound is this `tokio::time::timeout`
    /// call, not a shell wrapper. A `PendingPrompt` that arrives is routed
    /// through `AppState::offer_prompt` and then answered with a plain `y`
    /// keypress through the REAL `input::handle_key` +
    /// `AppState::resolve_current_prompt` -- the exact two calls an
    /// interactive operator's keystroke would drive.
    ///
    /// Passes today, unmodified (and after this item's fix): the request
    /// DOES reach `gate_rx` (mode is `Normal` during the ask's flight, so
    /// `offer_prompt` promotes it to `AwaitingPermission` rather than
    /// queuing behind a modal that has not opened yet), and answering it
    /// the way an operator would lets the ask reach a resolution
    /// (`outcome.child.is_some()`). This item's OTHER acceptance criteria
    /// close what remains: nothing surfaces this prompt to an operator who
    /// does not already know that sequence (no spinner names it as
    /// belonging to the ask), and there was previously no way to abandon
    /// it instead of answering it -- see the next test.
    #[tokio::test]
    async fn ask_child_tool_call_is_answerable_exactly_as_an_interactive_operator_would() {
        let (conway, mut gate_rx, _backend) = conway_with_real_gate();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        app.submit("/ask please use the marker tool".to_string())
            .await
            .expect("submit should not error");
        assert!(app.state.ask_in_flight);

        let outcome = tokio::time::timeout(HANG_TIMEOUT, async {
            loop {
                tokio::select! {
                    maybe_ask = app
                        .modal_ask_rx
                        .as_mut()
                        .expect("modal_ask_rx is set by App::new")
                        .recv() => {
                        match maybe_ask {
                            // `Started` just names the child -- not the
                            // resolution this loop is waiting for; keep
                            // looping.
                            Some(AskUpdate::Started { .. }) => continue,
                            Some(AskUpdate::Done(outcome)) => return Some(outcome),
                            None => return None,
                        }
                    }
                    maybe_prompt = gate_rx.recv() => {
                        let prompt = maybe_prompt.expect("TuiGate's sender half is alive");
                        app.state.offer_prompt(prompt);
                        assert!(
                            matches!(app.state.mode, Mode::AwaitingPermission(_)),
                            "offer_prompt must promote to AwaitingPermission while mode is \
                             Normal (ask in flight, no modal open yet), got: {:?}",
                            app.state.mode
                        );
                        let action = input::handle_key(
                            &mut app.state,
                            key(ratatui::crossterm::event::KeyCode::Char('y')),
                        );
                        match action {
                            Action::PermissionDecision(decision) => {
                                app.state.resolve_current_prompt(decision);
                            }
                            other => panic!("expected a PermissionDecision action, got {other:?}"),
                        }
                    }
                }
            }
        })
        .await
        .expect(
            "LOAD-BEARING: the ask must reach a resolution once the gate is answered the way an \
             operator would answer it -- a timeout here would mean the deadlock is inside the \
             gate/mode-stacking mechanism itself, not merely in what the TUI shows",
        );

        let outcome = outcome.expect("modal_ask_tx's sender half is alive for the duration");
        assert!(
            outcome.child.is_some(),
            "the ask must reach a resolution once the tool call it needed was permitted: {:?}",
            outcome.reply
        );
    }

    /// **Reproduction 2 (the real defect): `SessionHandle::cancel` alone
    /// does not unblock a child parked on the gate.** Submits `/ask`,
    /// captures the `PendingPrompt` that reaches `gate_rx` WITHOUT
    /// resolving it (its reply sender is kept alive in `_prompt` for the
    /// whole test -- dropping it early would take the OTHER, already-known
    /// escape hatch: `TuiGate::check`'s fail-closed `Deny {reason:
    /// "cancelled"}` on a dropped reply channel), then calls
    /// `SessionHandle::cancel` (immediate mode, the pre-existing primitive
    /// this item's own recon pointed at) on the child and waits, bounded,
    /// for it to actually finish.
    ///
    /// **Written and run against unmodified code first, per this item's
    /// own instruction, and it genuinely times out**: `cancel` only trips
    /// a `CancellationToken` the agent loop checks cooperatively at
    /// specific points (`conway-runtime/src/agent_loop.rs`); the call site
    /// blocked on a permission decision
    /// (`conway-runtime/src/tools/runner.rs`'s `broker.decide(..).await`,
    /// BEFORE the `tokio::select!` that later races the tool's own
    /// `invoke` against cancellation) is not one of them. A child parked
    /// there stays running no matter how many times it is cancelled --
    /// which is exactly why `purge` (which refuses a running agent)
    /// produces the reported "agent is still running" error. The second
    /// half proves what DOES unblock it: dropping the pending prompt's
    /// reply sender (`TuiGate`'s own fail-closed fallback) -- the
    /// ingredient `cancel` alone was missing.
    #[tokio::test]
    async fn cancelling_an_in_flight_ask_does_not_unblock_a_child_stuck_on_the_gate_today() {
        let (conway, mut gate_rx, _backend) = conway_with_real_gate();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        app.submit("/ask please use the marker tool".to_string())
            .await
            .expect("submit should not error");

        let prompt = tokio::time::timeout(HANG_TIMEOUT, gate_rx.recv())
            .await
            .expect("the tool call must reach the gate promptly")
            .expect("TuiGate's sender half is alive");
        let child = prompt.request.agent_id;

        // The primitive this item's own recon pointed at -- immediate
        // cancellation, applied to exactly the stuck child.
        app.handle
            .cancel(child, "ask abandoned")
            .await
            .expect("cancel should be accepted");

        // NOT a `HANG_TIMEOUT` site (see this module's own doc on that
        // constant): this bound is not converting a hang into a message,
        // it is a probe for the ABSENCE of completion within a window --
        // structurally, `await_agent` cannot resolve here no matter how
        // much CPU time this process is given, since `child` is blocked on
        // a permission decision this test has not yet answered or dropped
        // (the very next statement below). A slow/starved scheduler can
        // only make `timeout` fire LATER after more real time has already
        // elapsed while still correctly observing "not yet done" --
        // `Elapsed(())` is what this assertion wants, on any schedule --
        // so, unlike every `HANG_TIMEOUT` site, widening this one buys
        // nothing and shortening it risks nothing this test depends on.
        let cancel_alone_result =
            tokio::time::timeout(Duration::from_millis(500), app.handle.await_agent(child)).await;
        assert!(
            cancel_alone_result.is_err(),
            "LOAD-BEARING (this is the reproduction): `cancel` alone must NOT unblock a child \
             parked on the gate today -- a child that actually finished here means either the \
             gate/agent-loop wiring changed underneath this test, or something else besides \
             `cancel` resolved it; got {cancel_alone_result:?}"
        );

        // What DOES unblock it, today: discarding the pending prompt.
        // Dropping `prompt` drops its `oneshot::Sender`, which is exactly
        // `TuiGate::check`'s own documented fail-closed fallback --
        // `reply_rx.await.unwrap_or(Deny { reason: "cancelled" })`.
        drop(prompt);

        let after_drop = tokio::time::timeout(HANG_TIMEOUT, app.handle.await_agent(child))
            .await
            .expect(
                "once the pending prompt is discarded, the already-tripped cancellation token \
                 must let the child actually finish -- a timeout here would mean even dropping \
                 the prompt cannot free it, which is a stronger and different defect",
            )
            .expect("await_agent should resolve to a terminal AgentResult, not error");
        assert!(
            matches!(
                after_drop.status,
                conway_core::agent::ResultStatus::Cancelled { .. }
            ),
            "the child's terminal status should reflect the cancellation once it can finally \
             land, got: {:?}",
            after_drop.status
        );
    }

    /// **The fix, end to end.** Drives `/ask` through `App::submit` exactly
    /// like the two reproductions above, routes the tool-call permission
    /// request through `AppState::offer_prompt` exactly as `App::run`'s own
    /// `gate_rx` arm would (this is the ONLY thing that puts it somewhere
    /// `App::abandon_ask` can find it), then calls `App::abandon_ask` --
    /// the same call `Action::CtrlC` -> `App::handle_ctrl_c` makes -- while
    /// the ask is still stuck on that very prompt. Acceptance criterion 2:
    /// abandoning leaves no running agent and no dangling ephemeral
    /// session.
    #[tokio::test]
    async fn abandon_ask_leaves_no_running_agent_and_no_dangling_session() {
        let (conway, mut gate_rx, _backend) = conway_with_real_gate();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        app.submit("/ask please use the marker tool".to_string())
            .await
            .expect("submit should not error");

        // Drive both channels exactly like `App::run`'s own `select!` does,
        // until the child id is known AND its tool-call permission request
        // has been routed into `AppState` (mirroring `run.rs`'s `gate_rx`
        // arm) -- both must have happened before `abandon_ask` has anything
        // to act on.
        let mut child = None;
        let mut prompt_in_state = false;
        tokio::time::timeout(HANG_TIMEOUT, async {
            while child.is_none() || !prompt_in_state {
                tokio::select! {
                    maybe_ask = app
                        .modal_ask_rx
                        .as_mut()
                        .expect("modal_ask_rx is set by App::new")
                        .recv() => {
                        if let Some(AskUpdate::Started { child: c }) = maybe_ask {
                            child = Some(c);
                            app.state.ask_child = Some(c);
                        }
                    }
                    maybe_prompt = gate_rx.recv() => {
                        let prompt = maybe_prompt.expect("TuiGate's sender half is alive");
                        app.state.offer_prompt(prompt);
                        prompt_in_state = true;
                    }
                }
            }
        })
        .await
        .expect("both the child id and its pending prompt must arrive promptly");
        let child = child.expect("set by the loop above");
        assert!(
            matches!(app.state.mode, Mode::AwaitingPermission(_)),
            "the tool-call prompt must be showing before abandon -- this is the scenario an \
             operator who does not want to deal with the prompt is actually in, got: {:?}",
            app.state.mode
        );

        app.abandon_ask().await;

        assert!(
            !matches!(app.state.mode, Mode::AwaitingPermission(_)),
            "abandon_ask must discard the pending prompt (AppState::discard_prompts_for_agent), \
             not leave it showing: {:?}",
            app.state.mode
        );

        // The spawned task's `text()` call is still running at this point
        // (cancellation + the discarded prompt only just let the child's
        // OWN turn wind down) -- drain `modal_ask_rx` until `Done` arrives,
        // exactly like `App::run`'s own arm. `HANG_TIMEOUT` -- see this
        // module's own doc on it: this bound previously flaked under
        // ordinary desktop CPU contention when tuned near its uncontended
        // duration (~200ms observed, bound at 5s then 10s); it is a hang
        // detector, not a promptness assertion.
        let done = tokio::time::timeout(HANG_TIMEOUT, async {
            loop {
                if let Some(AskUpdate::Done(outcome)) = app
                    .modal_ask_rx
                    .as_mut()
                    .expect("modal_ask_rx is set by App::new")
                    .recv()
                    .await
                {
                    return outcome;
                }
            }
        })
        .await
        .expect(
            "the abandoned ask must still reach AskUpdate::Done -- abandon_ask cancels and \
             discards the prompt, it does not make the spawned task vanish",
        );
        // Mirrors `App::run`'s own `AskUpdate::Done` arm's abandoned branch:
        // `ask_abandoned` is still set (only that arm clears it) and the
        // outcome's child matches the one `abandon_ask` cancelled.
        assert!(
            app.state.ask_abandoned,
            "set by abandon_ask; only App::run's own Done arm clears it"
        );
        assert_eq!(done.child, Some(child));
        // `AskUpdate::Done` arriving does NOT by itself prove the agent
        // tree considers `child` terminal yet (measured directly: a bare
        // `purge` here, immediately after `Done`, intermittently reproduces
        // the exact `RuntimeError::Store(StoreError::NotRemovable)`/"agent
        // is still running" error this item was filed over --
        // `TurnHandle::text`'s own drain-to-event heuristic can resolve
        // before the tree's status flips). `await_agent` is what actually
        // confirms the terminal state; `App::run`'s own arm does the same
        // wait before ever attempting `purge` -- see `run.rs`'s own
        // `AskUpdate::Done` arm.
        tokio::time::timeout(HANG_TIMEOUT, app.handle.await_agent(child))
            .await
            .expect("the abandoned child must reach a terminal state promptly")
            .expect("await_agent should resolve to a terminal AgentResult, not error");
        app.conway.purge(child).await.expect(
            "LOAD-BEARING: purge must succeed once await_agent confirms the terminal state",
        );

        let sessions = app
            .conway
            .sessions(conway::SessionFilter {
                include_ephemeral: true,
                ..Default::default()
            })
            .await
            .expect("sessions() should succeed");
        assert!(
            sessions.iter().all(|m| m.agent_id != child),
            "no dangling ephemeral session must remain after abandon: {sessions:?}"
        );
    }
}
