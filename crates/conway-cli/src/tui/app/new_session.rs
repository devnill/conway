//! `/new` (board item `01M1YVKQ6ABQDWYSA7CEF20WKG`): no production code of
//! its own -- the whole command is a single, synchronous `Host::new_session`
//! call plus an `AppState` reset, both already factored into `commands::
//! execute`'s own `SlashCommand::New` arm (mirroring `commands::
//! apply_resume`'s shape exactly: see that function's own doc). This module
//! exists purely to hold the `App`-level tests `commands.rs`'s own
//! `FakeHost` cannot reach -- `tests::FakeHost::new_session` has no public
//! `SessionHandle` constructor to return (mirrors `FakeHost::resume`'s own
//! disclosed limitation), so the SUCCESS half of `/new` -- a genuinely fresh
//! session, the old one left resumable -- is proven here instead, against a
//! real `Conway`.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use conway::SessionFilter;

    use super::super::fixtures::{echo_conway_and_store, echo_conway_over, minimal_cli};
    use super::super::App;
    use crate::tui::state::Entry;

    /// See `ask.rs`'s own `HANG_TIMEOUT` doc for why every bound in this
    /// module is a hang detector, not a promptness assertion.
    const HANG_TIMEOUT: Duration = Duration::from_secs(10);

    /// **The primary end-to-end test (acceptance criterion 1).** `/new`
    /// starts a genuinely fresh root session (a different `AgentId`/
    /// `SessionId`), clears the transcript, and leaves the OLD session
    /// resumable: it still appears in `conway sessions list`'s own facade
    /// call (`Conway::sessions`), and `Conway::resume` against it still
    /// succeeds and still carries the message that was sent before `/new`.
    #[tokio::test]
    async fn new_starts_a_fresh_session_and_leaves_the_old_one_resumable() {
        let (conway, store) = echo_conway_and_store();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let old_root = app.handle.root();
        let old_session = app.handle.id();

        app.submit("hello from the old session".to_string())
            .await
            .expect("submit should not error");
        tokio::time::timeout(HANG_TIMEOUT, async {
            while !app.handle.awaiting_prompt(old_root) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the old session's own turn must settle before /new");

        app.submit("/new".to_string())
            .await
            .expect("submit should not error");

        // LOAD-BEARING: a genuinely new root agent, not the same one
        // reused.
        assert_ne!(
            app.handle.root(),
            old_root,
            "/new must start a genuinely fresh root agent"
        );
        assert_ne!(app.handle.id(), old_session);
        assert!(
            app.state.transcript.iter().all(|e| !matches!(
                e,
                Entry::Notice { text } | Entry::Error { text, .. }
                    if text.contains("hello from the old session")
            )),
            "the fresh session's transcript must not carry the old one's messages: {:?}",
            app.state.transcript
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text }
                    if text.contains(&old_session.to_string()) && text.contains("resumable")
            )),
            "a notice must name the old session so it can be resumed: {:?}",
            app.state.transcript
        );

        // LOAD-BEARING (P-15): both sessions are listed by the SAME facade
        // call `conway sessions list` makes -- not merely "a notice claims
        // it," the actual listing.
        let sessions = app
            .conway
            .sessions(SessionFilter::default())
            .await
            .expect("sessions() should succeed");
        assert!(
            sessions.iter().any(|m| m.id == old_session),
            "the old session must still be listed: {sessions:?}"
        );
        assert!(
            sessions.iter().any(|m| m.id == app.handle.id()),
            "the new session must be listed too: {sessions:?}"
        );

        // The old session can still be resumed, with its own history
        // intact -- `/new` ends it cleanly, it does not delete it.
        //
        // **Resumed through a SECOND, wholly independent `Conway`/`Runtime`
        // over the SAME store** -- the "simulated restart" shape this
        // crate's own test suite already establishes elsewhere (`app/
        // startup.rs`'s `resuming_a_session_refreshes_its_own_head_seq`,
        // `fixtures::echo_conway_over`'s own doc) -- deliberately NOT
        // `app.conway.resume(old_session)` on the SAME live `Conway`:
        // `/new` never ends the old root's own task (it is `keep_alive`,
        // and nothing about `/new` cancels or detaches it -- "ends the
        // session cleanly" means the TUI stops driving it, not that the
        // agent stops running), so it is still attached to THAT `Runtime`'s
        // own tree, and `Runtime::attach` refuses to attach an `AgentId`
        // that already is (`RuntimeError::Tool(Internal{"already attached
        // to the tree"})`, a structural guard, not a bug in `/new` -- proven
        // by first running this test against `app.conway.resume` directly,
        // which reproduces exactly that error). A genuinely fresh process
        // (the real scenario `/new`'s "still resumable" promise describes)
        // has no such live attachment to conflict with, which is exactly
        // what a second `Conway` over the same store models.
        let restarted = echo_conway_over(store);
        let resumed = restarted
            .resume(old_session)
            .await
            .expect("the old session must still be resumable after a simulated restart");
        let records = resumed
            .transcript(resumed.root())
            .await
            .expect("transcript must be readable");
        assert!(
            records.iter().any(
                |r| matches!(r, conway::LogRecord::UserTurn { text, .. } if text == "hello from the old session")
            ),
            "the resumed old session must still carry its own history: {records:?}"
        );
    }

    /// Review finding (board item `01M1YVKQ6ABQDWYSA7CEF20WKG`, finding 6,
    /// pre-existing, also in `/resume`): before a carry-across funnel
    /// existed at all (today `AppState::reset_for_new_session`; originally a
    /// narrower, now-retired `commands::CarriedConfiguration`), `/new`'s own
    /// `*state = AppState::new(..)` reset silently dropped `AppState::
    /// grants_path` along with it --
    /// `app/run.rs::persist_permission_rule`'s own doc: "early-returns when
    /// `grants_path` is `None`" -- so a "remember this" permission grant the
    /// operator answers AFTER `/new` would silently stop being written to
    /// disk, with no error and no notice. This drives the REAL `/new` path
    /// (`App::submit`, not a hand-built `AppState`) and proves the fix at the
    /// field `persist_permission_rule` actually reads, plus its
    /// `permission_mode`/`permission_paths`/configured-deny-rule siblings,
    /// while proving the OTHER half of the same ruling holds too: a
    /// session-scoped allow grant (`permission_grants`) does NOT survive --
    /// it belongs to the OLD session, not the fresh one.
    #[tokio::test]
    async fn new_carries_grants_path_and_configured_permission_rules_forward() {
        let (conway, _store) = echo_conway_and_store();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let grants_path = std::path::PathBuf::from("/tmp/conway-test-grants.json");
        let permission_path = std::path::PathBuf::from("/tmp/conway-test-permissions.json");
        app.state.grants_path = Some(grants_path.clone());
        app.state.permission_mode = conway::PermissionMode::Plan;
        app.state.permission_paths = vec![permission_path.clone()];
        app.state.permission_denies = vec![(
            conway::PatternRule::parse("bash:rm -rf *").expect("valid rule"),
            conway::PatternOrigin::File(permission_path.clone()),
        )];
        // The OTHER half of the same ruling: a session-scoped allow grant
        // belongs to the OLD session and must NOT carry forward.
        app.state.permission_grants = vec![(
            conway::PatternRule::parse("bash:git status").expect("valid rule"),
            conway::PatternOrigin::Interactive,
        )];

        // Board item `01M1YVKQ6ABQDWYSA7CEF20WKG` (round 2, widened): the
        // SAME ruling applies to every other field `App::new` configures
        // once from config/CLI/environment and never recomputes --
        // `AppState::reset_for_new_session`'s own doc names the full set;
        // these five are the ones `/new`'s own review finding named by hand.
        let keybindings_dir = tempfile::tempdir().expect("tempdir");
        let keybindings_path = keybindings_dir.path().join("keybindings.json");
        std::fs::write(
            &keybindings_path,
            r#"{"prompt": {"open_editor": ["Ctrl-T"]}}"#,
        )
        .expect("write keybindings.json");
        app.state.keybindings = crate::tui::keybindings::Keymap::load(&keybindings_path)
            .expect("the hand-written keybindings.json must parse");
        app.state.busy_input = crate::tui::config::BusyInputMode::Steer;
        app.state.editor_mode = crate::tui::config::EditorMode::Vim;
        app.state.history =
            std::collections::VecDeque::from(vec!["an old-session history entry".to_string()]);
        app.state.status_line_config.fields = vec!["cwd".to_string()];
        app.state.project_config_ignored = true;
        app.state.transcript.push(crate::tui::state::Entry::Notice {
            text: "SECRET_OLD_TRANSCRIPT_ENTRY".to_string(),
        });

        app.submit("/new".to_string())
            .await
            .expect("submit should not error");

        assert_eq!(
            app.state.grants_path,
            Some(grants_path),
            "grants_path must survive /new -- otherwise persist_permission_rule silently \
             stops writing any grant for the rest of the process"
        );
        assert_eq!(app.state.permission_mode, conway::PermissionMode::Plan);
        assert_eq!(app.state.permission_paths, vec![permission_path.clone()]);
        assert_eq!(
            app.state.permission_denies,
            vec![(
                conway::PatternRule::parse("bash:rm -rf *").expect("valid rule"),
                conway::PatternOrigin::File(permission_path),
            )],
            "a configured (file-sourced) deny rule must survive /new"
        );
        assert!(
            app.state.permission_grants.is_empty(),
            "a session-scoped allow grant must NOT survive /new -- it belonged to the old \
             session: {:?}",
            app.state.permission_grants
        );
        assert_eq!(
            app.state
                .keybindings
                .keys_for(crate::tui::keybindings::Context::Prompt, "open_editor"),
            vec!["Ctrl-T".to_string()],
            "the loaded keybindings table must survive /new"
        );
        assert_eq!(
            app.state.busy_input,
            crate::tui::config::BusyInputMode::Steer,
            "the busy_input display preference must survive /new"
        );
        assert_eq!(
            app.state.editor_mode,
            crate::tui::config::EditorMode::Vim,
            "the editor_mode display preference must survive /new"
        );
        assert!(
            app.state
                .history
                .contains(&"an old-session history entry".to_string()),
            // `push_history` itself also records the "/new" submission that
            // triggered this reset, so the deque carries both -- this checks
            // the PRE-EXISTING entry survived, not that nothing else joined
            // it.
            "input history must survive /new: {:?}",
            app.state.history
        );
        assert_eq!(
            app.state.status_line_config.fields,
            vec!["cwd".to_string()],
            "the status-line config must survive /new"
        );
        assert!(
            app.state.project_config_ignored,
            "the project-config-ignored security marker must survive /new"
        );
        assert!(
            app.state.transcript.iter().all(|e| !matches!(
                e,
                crate::tui::state::Entry::Notice { text } if text == "SECRET_OLD_TRANSCRIPT_ENTRY"
            )),
            "the OLD session's own transcript must NOT survive /new: {:?}",
            app.state.transcript
        );
    }

    /// `/new` aborts a genuinely in-flight turn FIRST, rather than refusing
    /// or leaving it to run on orphaned -- driven against a REAL tool call
    /// in flight, mirroring `app/busy_input.rs`'s own `HeldTool` fixture.
    #[tokio::test]
    async fn new_aborts_a_genuinely_in_flight_turn_before_starting_fresh() {
        use std::sync::Arc;

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

            async fn invoke(
                &self,
                _call: ToolCall,
                _ctx: ToolCtx,
            ) -> Result<ToolOutput, ToolError> {
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

        let gate = Arc::new(tokio::sync::Notify::new());
        let backend = Arc::new(
            ScriptedBackend::new(vec![
                ScriptedTurn::Respond(tool_call_response("call_1", "held")),
                ScriptedTurn::Respond(text_response("final answer")),
            ])
            .with_id(BackendId::new("fake")),
        );
        let conway: Conway = test_builder(super::super::fixtures::base_config())
            .with_backend(backend)
            .with_permission_gate(conway::test_support::allow_once_gate())
            .with_session_store(Arc::new(FakeStore::new()))
            .with_plugin(Arc::new(HeldPlugin { gate: gate.clone() }))
            .build()
            .expect("build should succeed with every port injected");

        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let old_root = app.handle.root();

        app.submit("start".to_string())
            .await
            .expect("submit should not error");
        // Wait until genuinely mid-tool-call (round-trip done, turn not yet
        // over) -- mirrors `app/busy_input.rs`'s own `wait_until_mid_tool_call`.
        tokio::time::timeout(HANG_TIMEOUT, async {
            loop {
                if !app.handle.turn_in_progress(old_root) && !app.handle.awaiting_prompt(old_root) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the agent must reach mid-tool-call within the hang-timeout bound");

        // Captured BEFORE `/new` swaps `app.handle` -- one `Conway`/
        // `Runtime` serves every session in this process, so this handle
        // stays perfectly valid for querying the OLD agent after the swap.
        let old_handle = app.handle.clone();

        app.submit("/new".to_string())
            .await
            .expect("/new must not hang or error even with a turn genuinely in flight");

        assert_ne!(
            app.handle.root(),
            old_root,
            "/new must still start a fresh session even with the old turn aborted, not refused"
        );

        // The old agent was actually aborted (not left running): `abort_turn`
        // is NON-terminal (`app/shutdown.rs::handle_ctrl_c`'s own doc) --
        // the root stays alive, `keep_alive`, and returns to idling at its
        // own resume gate once the abort lands, rather than reaching a
        // terminal `AgentResult` at all (`await_agent` would hang forever on
        // a `keep_alive` agent that never finishes -- see `app/ask.rs`'s own
        // doc on exactly that hazard).
        //
        // LOAD-BEARING: the gate is deliberately NEVER released here -- if
        // `/new`'s `abort_turn` call did nothing (the same cooperative-
        // checkpoint limitation `app/ask.rs`'s own module doc describes for
        // a child "parked awaiting a permission decision"), `HeldTool::
        // invoke` would stay blocked forever and this bounded wait would
        // fail LOUDLY rather than hang indefinitely -- a genuine abort is
        // what lets the turn settle with no further input from this test at
        // all.
        tokio::time::timeout(HANG_TIMEOUT, async {
            while !old_handle.awaiting_prompt(old_root) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect(
            "the old agent's in-flight turn must have been genuinely aborted by /new -- it \
             must settle to idle on its own, with the held tool's gate never released",
        );
    }
}
