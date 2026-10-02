//! Regression coverage for "a plugin command issued after a `/model`/
//! `/role` switch must answer for the agent the operator is now talking
//! to, not the TUI's original session" -- the operator-visible failure:
//! switch model, let the new agent edit a file, type
//! `/conway.checkpoint.list`, and hear "no snapshots recorded yet" even
//! though the edit just happened, because the command's implicit session
//! was still the pre-switch one.
//!
//! Drives the REAL production stack end to end -- a real `write` tool, a
//! real `CheckpointObserver` hooked through `ToolRunner::execute_one`, and
//! the real `App::submit`/`commands::execute`/`Host::session_for`
//! dispatch path -- never a hand-built `ObservedCall`/`CommandCtx`, so this
//! proves the fix at the one layer a unit test of either plugin command or
//! dispatch alone cannot: that the two actually agree once wired together.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use conway::test_support::{allow_once_gate, base_config_at, scripted_backend, test_builder};
    use conway::PluginSelection;
    use conway_core::content::{StopReason, ToolCall, Usage};
    use conway_core::ids::ToolName;
    use conway_core::ports::GenerateResponse;
    use conway_plugin_checkpoint::CheckpointPlugin;
    use conway_testkit::{text_response, ScriptedTurn};

    use super::super::fixtures::minimal_cli;
    use super::super::{App, SubmitOutcome};

    fn write_call(call_id: &str, path: &str, content: &str) -> GenerateResponse {
        GenerateResponse {
            content: vec![],
            tool_calls: vec![ToolCall {
                call_id: call_id.to_string(),
                name: ToolName::new("write"),
                arguments: serde_json::json!({ "path": path, "content": content }),
            }],
            stop: StopReason::ToolUse,
            usage: Usage::default(),
        }
    }

    /// Drives `agent`'s next turn through BOTH scripted model rounds a real
    /// tool call needs: the proposing round (`write_call`'s own
    /// `stop: ToolUse`, no text) and the follow-up round the real tool
    /// RESULT feeds back in (this file's own `text_response` turns).
    ///
    /// **Why not one plain `.text().await`.** `TurnHandle::text` resolves
    /// on `Event::TurnFinished`, which `conway_runtime::agent_loop` emits
    /// once per MODEL ROUND, not once per multi-round tool exchange -- so a
    /// single `.text().await` here would return the EMPTY text of the
    /// tool-PROPOSING round, before the real tool call (and therefore the
    /// real `CheckpointObserver` hook this whole file exists to exercise)
    /// has even run. Calling `.text()` again on the SAME handle continues
    /// draining the identical stream from where the first call left off --
    /// past the server-side tool execution that already happened by the
    /// time the second model round was requested -- and returns THAT
    /// round's own text, which this helper asserts matches
    /// `expected_final_text` so a caller never has to re-derive "how many
    /// rounds did I script" by hand.
    async fn drive_two_round_tool_turn(
        handle: &conway::SessionHandle,
        agent: conway::AgentId,
        prompt: &str,
        expected_final_text: &str,
    ) {
        let turn = handle
            .prompt_agent(agent, prompt)
            .await
            .expect("prompt_agent should not error");
        turn.text()
            .await
            .expect("the tool-proposing round must complete");
        let final_text = turn
            .text()
            .await
            .expect("the follow-up round must complete");
        assert_eq!(
            final_text, expected_final_text,
            "the follow-up round's own text must be exactly what the script's second turn \
             supplies -- otherwise this drove the wrong number of rounds"
        );
    }

    /// Builds a real `Conway` -- real `write` tool, real `CheckpointObserver`
    /// (installed as `checkpoint`'s own `Plugin::observers()`), a
    /// `ScriptedBackend` playing `script` back in order -- rooted at a fresh
    /// tempdir so the write tool's relative paths and `CheckpointPlugin`'s
    /// own `<cwd>/.conway/checkpoints` store land somewhere real and
    /// disposable. Returns the tempdir too (kept alive by the caller) and
    /// the SAME `checkpoint` plugin handle `App::new`'s command registry
    /// must also be built from, so `/conway.checkpoint.*` is reachable at
    /// all.
    fn build(
        script: Vec<ScriptedTurn>,
    ) -> (
        tempfile::TempDir,
        conway::Conway,
        Arc<dyn conway::plugin::Plugin>,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = base_config_at(dir.path());
        let checkpoint: Arc<dyn conway::plugin::Plugin> =
            Arc::new(CheckpointPlugin::new(dir.path().to_path_buf()));
        let conway = test_builder(config)
            .with_backend(scripted_backend(script))
            .with_permission_gate(allow_once_gate())
            .with_builtin_plugins(PluginSelection::All)
            .with_plugin(checkpoint.clone())
            .build()
            .expect("build should succeed with the real write tool and checkpoint plugin wired");
        (dir, conway, checkpoint)
    }

    /// The acceptance scenario, almost verbatim: `/model` switches focus to
    /// a forked child, the child's own turn writes a file (a real `write`
    /// tool call through the real agent loop), and
    /// `/conway.checkpoint.list` -- submitted while STILL focused on that
    /// child -- must list the edit's own seq. Before the fix, `/conway.
    /// checkpoint.list`'s `CommandCtx::session_id` was the TUI's original
    /// (pre-switch) session, which the write never touched, so this would
    /// have printed "no snapshots recorded yet" instead.
    #[tokio::test]
    async fn checkpoint_list_after_a_model_switch_sees_the_switched_agents_own_edit() {
        let (_dir, conway, checkpoint) = build(vec![
            ScriptedTurn::Respond(write_call("call_1", "edited.txt", "hello from the switch")),
            ScriptedTurn::Respond(text_response("done")),
        ]);
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[checkpoint])
            .await
            .expect("App::new should succeed");

        let switch = app
            .submit("/model fake/echo-model".to_string())
            .await
            .expect("the switch should not error");
        let child = match switch {
            SubmitOutcome::FocusNewSession { child, .. } => child,
            _ => panic!("/model must yield FocusNewSession"),
        };
        app.try_focus_agent(child, None).await;
        assert_eq!(
            app.state.focused_agent, child,
            "precondition: focus must have actually moved to the switch's own child"
        );

        // Drives the child's own turn to completion -- the real write tool
        // call, the real `CheckpointObserver` hook, and the final "done"
        // reply the script's second turn supplies.
        drive_two_round_tool_turn(&app.handle, child, "edit the file", "done").await;

        // The one identity this whole item is about: the session `/conway.
        // checkpoint.list` is about to answer for must be the CHILD's own,
        // never the TUI's original session.
        let child_session = app
            .handle
            .resolve_agent_session(child)
            .await
            .expect("the child must have its own session");
        assert_ne!(
            child_session,
            app.handle.id(),
            "precondition: the switch's own child must be a DIFFERENT session from the \
             TUI's original one -- otherwise this test cannot distinguish the fix from the \
             bug it replaces"
        );

        let outcome = app
            .submit("/conway.checkpoint.list".to_string())
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, SubmitOutcome::Continue));
        let done = app
            .plugin_cmd_rx
            .as_mut()
            .expect("plugin_cmd_rx is set by App::new")
            .recv()
            .await
            .expect("the spawned command task must reply");
        assert_eq!(
            done.session_id, child_session,
            "the dispatched command's own session must be the FOCUSED (switched) agent's, \
             not the TUI's original session"
        );
        match done.outcome {
            conway::plugin::CommandOutcome::Output(lines) => {
                assert!(
                    lines.iter().any(|l| l.starts_with("seq ")
                        && l.contains("Write")
                        && l.contains("edited.txt")),
                    "the switched agent's own edit must be listed: {lines:?}"
                );
            }
            other => panic!("expected CommandOutcome::Output, got {other:?}"),
        }
    }

    /// The rollback half: after the identical switch-then-edit sequence,
    /// `/conway.checkpoint.rollback 1` -- submitted while still focused on
    /// the switch's own child -- must restore the file ON DISK to its
    /// pre-edit bytes. Proven by reading the real file back off the real
    /// tempdir, not by inspecting the command's own report text.
    #[tokio::test]
    async fn rollback_from_the_tui_after_a_switch_restores_the_file_on_disk() {
        let (dir, conway, checkpoint) = build(vec![
            ScriptedTurn::Respond(write_call(
                "call_1",
                "edited.txt",
                "new contents from the switch",
            )),
            ScriptedTurn::Respond(text_response("done")),
        ]);
        let path = dir.path().join("edited.txt");
        std::fs::write(&path, "original contents").expect("seed the pre-edit file");

        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[checkpoint])
            .await
            .expect("App::new should succeed");

        let switch = app
            .submit("/model fake/echo-model".to_string())
            .await
            .expect("the switch should not error");
        let child = match switch {
            SubmitOutcome::FocusNewSession { child, .. } => child,
            _ => panic!("/model must yield FocusNewSession"),
        };
        app.try_focus_agent(child, None).await;

        drive_two_round_tool_turn(&app.handle, child, "edit the file", "done").await;
        assert_eq!(
            std::fs::read_to_string(&path).expect("file must exist after the edit"),
            "new contents from the switch",
            "precondition: the switched agent's turn must have actually overwritten the file"
        );

        // `0`, not the edit's own seq: every checkpoint entry's seq is
        // >= 0, so "restore everything touched at or after 0" always
        // covers it without this test having to know (or guess) the exact
        // `LogSeq` the real agent loop assigned the write's own
        // `ToolResultRecord` -- which depends on how many OTHER records
        // (the user turn, the assistant's tool-use message, its context
        // report, the permission decision) precede it, none of which this
        // test is about.
        let outcome = app
            .submit("/conway.checkpoint.rollback 0".to_string())
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, SubmitOutcome::Continue));
        let done = app
            .plugin_cmd_rx
            .as_mut()
            .expect("plugin_cmd_rx is set by App::new")
            .recv()
            .await
            .expect("the spawned command task must reply");
        match done.outcome {
            conway::plugin::CommandOutcome::Output(_) => {}
            other => panic!("expected a successful rollback, got {other:?}"),
        }

        assert_eq!(
            std::fs::read_to_string(&path).expect("file must still exist after rollback"),
            "original contents",
            "rollback, issued from the TUI after a switch, must restore the file on disk"
        );
    }
}
