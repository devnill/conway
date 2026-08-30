//! The TUI's in-process [`conway_plugin_ui::FormSurface`] implementation --
//! board item `01M19NH39AE2D5AMJK0RZRQY86`, the live surface
//! `conway-plugin-ui`'s own module doc names as this item's whole point.
//!
//! Mirrors [`crate::tui::gate::TuiGate`] deliberately, not by coincidence:
//! `TuiFormSurface::ask_select` never decides anything itself -- it forwards
//! every [`conway_plugin_ui::AskSelectRequest`] over an `mpsc` channel as a
//! [`PendingFormAsk`] and awaits the app loop's `oneshot` reply, exactly the
//! shape `TuiGate::check` already establishes for "the tool-call thread
//! blocks while the app loop renders and waits for a keypress." Both
//! `PermissionGate`/`FormSurface` implementations must be handed to
//! `ConwayBuilder` *before* `build()` returns (`TuiGate`'s own doc explains
//! why: `conway_runtime::runtime::Runtime` bakes them in at construction,
//! with no later swap point) -- `main.rs` builds the matching
//! [`TuiFormSurface::channel`] pair alongside `TuiGate::channel` for the
//! identical reason, threading the [`FormReceiver`] half through
//! `dispatch`/`tui::run`/`App::run` the same way `GateReceiver` already
//! travels.
//!
//! **Fail-closed, exactly like `TuiGate`.** A dropped sender (the app loop
//! exited) or a dropped reply channel (the app loop exited mid-question)
//! both resolve as [`conway_plugin_ui::FormSurfaceError`] naming
//! `"cancelled"`, never as a hang -- the same posture `TuiGate::check`'s own
//! doc states for a gate whose receiver or reply channel is gone.

use conway_plugin_ui::{AskSelectAnswer, AskSelectRequest, FormSurface, FormSurfaceError};
use tokio::sync::{mpsc, oneshot};

/// One question awaiting the app loop's answer.
pub struct PendingFormAsk {
    pub request: AskSelectRequest,
    reply: oneshot::Sender<Result<AskSelectAnswer, FormSurfaceError>>,
}

impl PendingFormAsk {
    /// Sends `answer` back to the blocked `TuiFormSurface::ask_select` call.
    /// A closed receiver (the tool call already gave up -- e.g. the agent
    /// was cancelled) is not an error here; there is nothing left to notify,
    /// mirroring [`crate::tui::gate::PendingPrompt::resolve`] exactly.
    pub fn resolve(self, answer: Result<AskSelectAnswer, FormSurfaceError>) {
        let _ = self.reply.send(answer);
    }

    #[cfg(test)]
    pub(crate) fn reply_sender(
        self,
    ) -> oneshot::Sender<Result<AskSelectAnswer, FormSurfaceError>> {
        self.reply
    }

    /// Test-only constructor: `reply` is private outside this module by
    /// design -- only `TuiFormSurface::ask_select` ever builds a real one,
    /// tied to the live surface channel. State/render tests elsewhere under
    /// `tui/` that need a `Mode::UiForm` `AppState` with no live surface at
    /// all go through this instead of reaching into a private field --
    /// mirrors [`crate::tui::gate::PendingPrompt::new_for_test`] exactly.
    pub(crate) fn new_for_test(
        request: AskSelectRequest,
    ) -> (PendingFormAsk, oneshot::Receiver<Result<AskSelectAnswer, FormSurfaceError>>) {
        let (reply, rx) = oneshot::channel();
        (PendingFormAsk { request, reply }, rx)
    }
}

/// The app loop's half of a [`TuiFormSurface`] channel -- selected on
/// alongside the gate/event/input streams, exactly like
/// [`crate::tui::gate::GateReceiver`].
pub type FormReceiver = mpsc::UnboundedReceiver<PendingFormAsk>;

/// Implements [`FormSurface`] by relaying every request to the app loop and
/// blocking on its reply. Cheap to `Clone` (an `Arc`-backed channel sender
/// under the hood) -- not that cloning matters in practice: exactly one
/// instance is ever constructed, in `main.rs`, and handed to
/// `ConwayUiPlugin::new` wrapped in one `Arc`.
#[derive(Clone)]
pub struct TuiFormSurface {
    tx: mpsc::UnboundedSender<PendingFormAsk>,
}

impl TuiFormSurface {
    /// Builds a linked `(surface, receiver)` pair. The surface half is
    /// `Arc`-boxed and handed to `ConwayUiPlugin::new`; the receiver half is
    /// driven by the app loop (`App::run`'s own `select!`).
    pub fn channel() -> (TuiFormSurface, FormReceiver) {
        let (tx, rx) = mpsc::unbounded_channel();
        (TuiFormSurface { tx }, rx)
    }
}

#[async_trait::async_trait]
impl FormSurface for TuiFormSurface {
    async fn ask_select(
        &self,
        request: AskSelectRequest,
    ) -> Result<AskSelectAnswer, FormSurfaceError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let ask = PendingFormAsk {
            request,
            reply: reply_tx,
        };
        // The receiver is gone (app loop already exited) -- fail closed
        // rather than hang the tool call forever.
        if self.tx.send(ask).is_err() {
            return Err(FormSurfaceError::new("cancelled"));
        }
        // A dropped `reply_tx` (app loop exited mid-question, or panicked)
        // must also fail closed, never hang -- mirrors `TuiGate::check`'s
        // own doc: "Gate cancellation ... surfaces as `Deny { reason:
        // 'cancelled' }`, never as a hang".
        reply_rx
            .await
            .unwrap_or_else(|_| Err(FormSurfaceError::new("cancelled")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `Plugin::tools`/`Tool::invoke`/`Tool::spec` -- needed by
    // `a_real_ask_question_call_renders_is_answered_and_reaches_the_tool_result`
    // below, not by this module's own production code.
    use conway::plugin::{Plugin, Tool};

    fn sample_request() -> AskSelectRequest {
        AskSelectRequest {
            prompt: "proceed?".to_string(),
            options: vec!["yes".to_string(), "no".to_string()],
        }
    }

    #[tokio::test]
    async fn an_answer_round_trips() {
        let (surface, mut rx) = TuiFormSurface::channel();
        let ask = tokio::spawn(async move { surface.ask_select(sample_request()).await });

        let pending = rx.recv().await.expect("pending ask");
        assert_eq!(pending.request.prompt, "proceed?");
        pending.resolve(Ok(AskSelectAnswer {
            selected: "yes".to_string(),
        }));

        let answer = ask
            .await
            .expect("ask_select task")
            .expect("resolved Ok");
        assert_eq!(answer.selected, "yes");
    }

    #[tokio::test]
    async fn a_surface_error_round_trips() {
        let (surface, mut rx) = TuiFormSurface::channel();
        let ask = tokio::spawn(async move { surface.ask_select(sample_request()).await });

        let pending = rx.recv().await.expect("pending ask");
        pending.resolve(Err(FormSurfaceError::new("operator cancelled")));

        let err = ask
            .await
            .expect("ask_select task")
            .expect_err("resolved Err");
        assert_eq!(err.message, "operator cancelled");
    }

    #[tokio::test]
    async fn dropped_reply_channel_fails_closed_as_cancelled() {
        let (surface, mut rx) = TuiFormSurface::channel();
        let ask = tokio::spawn(async move { surface.ask_select(sample_request()).await });

        let pending = rx.recv().await.expect("pending ask");
        // Drop the reply sender without resolving -- simulates the app loop
        // exiting mid-question.
        drop(pending.reply_sender());

        let err = ask
            .await
            .expect("ask_select task")
            .expect_err("a dropped reply must fail closed");
        assert_eq!(err.message, "cancelled");
    }

    #[tokio::test]
    async fn dropped_receiver_fails_closed_as_cancelled() {
        let (surface, rx) = TuiFormSurface::channel();
        drop(rx);

        let err = surface
            .ask_select(sample_request())
            .await
            .expect_err("a dropped receiver must fail closed");
        assert_eq!(err.message, "cancelled");
    }

    /// **VERIFICATION ANCHOR, acceptance 2, board item
    /// `01M19NH39AE2D5AMJK0RZRQY86`.** Drives every real production
    /// component this acceptance criterion names, end to end, with no mock
    /// standing in for any of them: a real `conway_plugin_ui::ConwayUiPlugin`
    /// tool call blocks inside `TuiFormSurface::ask_select`; the resulting
    /// `PendingFormAsk` is routed through `AppState::offer_ui_form` the
    /// SAME way `App::run`'s own `form_rx.recv()` arm does; the modal
    /// RENDERS (`Mode::UiForm`, asserted directly, and pixel-checked
    /// separately in `view::mod::tests::draw_ui_form_shows_the_prompt_and_
    /// the_highlighted_option`); the operator answers through the REAL
    /// `input::handle_key` router, exactly the keystrokes an interactive
    /// session would deliver; and the chosen answer reaches back through to
    /// the blocked tool call's own `ToolOutput` -- the exact text the model
    /// would see.
    ///
    /// **What this is NOT**: a PTY-driven run of the compiled `conway`
    /// binary. No such harness exists anywhere in this crate's test suite
    /// (checked before writing this test) -- every existing TUI-behavior
    /// test in this crate (`tui/app/ask.rs`'s own module doc: "`App::run`
    /// itself is not driven ... it owns a real terminal") drives the SAME
    /// real components this test does, minus the terminal itself, for the
    /// identical reason. Disclosed rather than silently treated as
    /// equivalent to a full-binary run.
    #[tokio::test]
    async fn a_real_ask_question_call_renders_is_answered_and_reaches_the_tool_result() {
        let (surface, mut form_rx) = TuiFormSurface::channel();
        let plugin =
            conway_plugin_ui::ConwayUiPlugin::new(Some(std::sync::Arc::new(surface)));
        let tool = plugin
            .tools()
            .into_iter()
            .find(|t| t.spec().name.as_str() == conway_plugin_ui::ASK_QUESTION_TOOL_NAME)
            .expect("conway.ui declares ask_question");

        let call = conway::plugin::ToolCall {
            call_id: "call-1".to_string(),
            name: conway::ToolName::new(conway_plugin_ui::ASK_QUESTION_TOOL_NAME),
            arguments: serde_json::json!({
                "prompt": "which way?",
                "options": ["left", "right"],
            }),
        };
        let agent_id = conway::AgentId::new();
        let ctx = conway::plugin::ToolCtx::for_test(
            agent_id,
            std::env::temp_dir(),
            std::sync::Arc::new(conway_testkit::FakeSubagentHost::new(agent_id)),
            std::sync::Arc::new(conway_testkit::CollectingEventSink::new()),
        );
        // The model's own call -- blocked inside `ask_select` until the
        // operator answers, exactly like a live turn's tool-call thread.
        let invoke = tokio::spawn(async move { tool.invoke(call, ctx).await });

        // The app loop's own `form_rx.recv()` arm, reproduced directly --
        // mirrors `App::run`'s real select-loop arm byte for byte (see
        // `tui/app/run.rs`).
        let ask = form_rx.recv().await.expect("the tool call reaches the surface promptly");
        let mut state = crate::tui::state::AppState::new(agent_id);
        state.offer_ui_form(ask);

        // RENDERS: the question is now the live modal, carrying the real
        // request the model sent.
        match &state.mode {
            crate::tui::state::Mode::UiForm(form) => {
                assert_eq!(form.ask.request.prompt, "which way?");
                assert_eq!(form.ask.request.options, vec!["left", "right"]);
                assert_eq!(form.selected, 0, "the modal opens with the first option lit");
            }
            other => panic!("expected Mode::UiForm, got {other:?}"),
        }

        // The operator answers exactly the way an interactive session
        // would: `Down` to move off the first option, then `Enter`.
        let move_action = crate::tui::input::handle_key(
            &mut state,
            crate::tui::test_support::key(ratatui::crossterm::event::KeyCode::Down),
        );
        assert_eq!(move_action, crate::tui::input::Action::None);
        assert!(matches!(
            &state.mode,
            crate::tui::state::Mode::UiForm(form) if form.selected == 1
        ));
        let answer_action = crate::tui::input::handle_key(
            &mut state,
            crate::tui::test_support::key(ratatui::crossterm::event::KeyCode::Enter),
        );
        assert_eq!(
            answer_action,
            crate::tui::input::Action::UiFormDecision(
                crate::tui::state::UiFormDecision::Answer
            )
        );
        state.resolve_ui_form(crate::tui::state::UiFormDecision::Answer);
        assert!(
            matches!(state.mode, crate::tui::state::Mode::Normal),
            "answering must close the modal"
        );

        // ANSWER REACHES THE MODEL: the blocked tool call's own
        // `ToolOutput` names the option the operator actually picked.
        let output = tokio::time::timeout(std::time::Duration::from_secs(5), invoke)
            .await
            .expect("the tool call must unblock promptly once answered")
            .expect("invoke task")
            .expect("ask_question never returns a ToolError for a well-formed call");
        assert!(!output.is_error);
        let text = output
            .blocks
            .iter()
            .find_map(|b| match b {
                conway::plugin::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .expect("ask_question replies with a text block");
        assert_eq!(text, "operator selected: right");
    }
}
