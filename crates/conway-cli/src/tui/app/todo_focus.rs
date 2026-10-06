//! Regression coverage for board item `01M48N3N1PRQXPGF6VQGK745VE`:
//! `conway.todo`'s status-line `todo: done/total` field used to report
//! whichever agent most recently called `todo_write`, not the one the
//! operator is actually focused on -- the same shape board item
//! `01M1YVVT9RYWZWAZC4YH21T3HN` already found and fixed for `conway.goal`.
//!
//! Drives the REAL production stack end to end: a real
//! `conway-plugin-todo` plugin, a real `todo_write` tool call through the
//! real agent loop, and the real `App::refresh_plugin_status_contributions`/
//! `view::status` render path -- never a hand-built
//! `PluginStatusContribution`. Mirrors `checkpoint_focus.rs`'s own
//! "real tool call, then check what a switched/focused agent sees" shape,
//! for a plugin-tracked value rather than an observer-persisted one.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use conway::test_support::{base_config, scripted_backend, test_builder};
    use conway_core::content::{StopReason, ToolCall, Usage};
    use conway_core::ids::ToolName;
    use conway_core::ports::GenerateResponse;
    use conway_plugin_todo::TodoPlugin;
    use conway_testkit::{text_response, ScriptedTurn};

    use super::super::fixtures::minimal_cli;
    use super::super::App;

    fn todo_write_call(call_id: &str, items: serde_json::Value) -> GenerateResponse {
        GenerateResponse {
            content: vec![],
            tool_calls: vec![ToolCall {
                call_id: call_id.to_string(),
                name: ToolName::new("todo_write"),
                arguments: serde_json::json!({ "items": items }),
            }],
            stop: StopReason::ToolUse,
            usage: Usage::default(),
        }
    }

    /// Drives `agent`'s next turn through BOTH scripted model rounds a real
    /// tool call needs -- the SAME two-round shape `checkpoint_focus.rs`'s
    /// own `drive_two_round_tool_turn` uses (see that function's own doc for
    /// why one `.text().await` is not enough: it would return the empty
    /// tool-proposing round's text, before the real `todo_write` tool --
    /// and therefore this plugin's own in-memory state -- has even run).
    /// Duplicated here rather than shared: the two modules' suites do not
    /// otherwise depend on each other, and the whole body is four lines.
    async fn drive_two_round_tool_turn(
        handle: &conway::SessionHandle,
        agent: conway::AgentId,
        prompt: &str,
    ) {
        let turn = handle
            .prompt_agent(agent, prompt)
            .await
            .expect("prompt_agent should not error");
        turn.text()
            .await
            .expect("the tool-proposing round must complete");
        turn.text()
            .await
            .expect("the follow-up round must complete");
    }

    /// A real `Conway` -- the real `todo_write`/`todo_read` tools (via a
    /// real `TodoPlugin`), a `ScriptedBackend` playing `script` back in
    /// order.
    fn build(script: Vec<ScriptedTurn>) -> conway::Conway {
        test_builder(base_config())
            .with_backend(scripted_backend(script))
            .with_plugin(Arc::new(TodoPlugin::new()))
            .build()
            .expect("build should succeed with the real todo_write tool wired")
    }

    /// `App::new` with `[tui.status_line.fields] = ["plugins"]` forced on,
    /// the same shape `app/plugin_status.rs`'s own rendered-status-line
    /// tests use -- without it, a narrow default-width render might drop
    /// the `plugins` field entirely before this test's own assertions ever
    /// get to it. `App::new` only borrows `Cli` and the tempdir's path for
    /// the duration of the call (`App` itself carries no lifetime), so
    /// nothing from this helper needs to outlive it -- the `TempDir` guard
    /// is dropped (and the directory cleaned up) when this function
    /// returns, same as every other fixture in this crate's test suites.
    async fn app_with_plugins_field(conway: &conway::Conway) -> App {
        let mut cli = minimal_cli();
        let tui_config_dir = tempfile::tempdir().expect("tempdir");
        let tui_config_path = tui_config_dir.path().join("settings.json");
        std::fs::write(
            &tui_config_path,
            serde_json::json!({"tui": {"status_line": {"fields": ["plugins"]}}}).to_string(),
        )
        .expect("write settings.json carrying [tui.status_line.fields]");
        cli.config = Some(tui_config_path);
        App::new(&cli, conway, &[])
            .await
            .expect("App::new should succeed")
    }

    #[tokio::test]
    async fn the_status_line_follows_focus_across_two_agents_with_different_lists() {
        let conway = build(vec![
            ScriptedTurn::Respond(todo_write_call(
                "call_1",
                serde_json::json!([
                    {"text": "root item 1", "status": "done"},
                    {"text": "root item 2", "status": "pending"},
                ]),
            )),
            ScriptedTurn::Respond(text_response("root done")),
            ScriptedTurn::Respond(todo_write_call(
                "call_2",
                serde_json::json!([
                    {"text": "child item 1", "status": "done"},
                    {"text": "child item 2", "status": "done"},
                    {"text": "child item 3", "status": "pending"},
                ]),
            )),
            ScriptedTurn::Respond(text_response("child done")),
        ]);

        let mut app = app_with_plugins_field(&conway).await;
        let root = app.handle.root();

        drive_two_round_tool_turn(&app.handle, root, "track root's own work").await;

        let child = app
            .handle
            .spawn(root, conway::SpawnSpec::new("child's own prompt"))
            .await
            .expect("spawn should succeed");
        drive_two_round_tool_turn(&app.handle, child, "track child's own work").await;

        // Still focused on root (spawning a child never moves focus) -- the
        // status line must show ROOT's own 1/2, not the child's 2/3, even
        // though the child wrote AFTER root did.
        assert_eq!(app.state.focused_agent, root);
        assert!(app.refresh_plugin_status_contributions());
        let root_text = crate::tui::test_support::render_text(&app.state, 120, 40);
        assert!(
            root_text.contains("todo: 1/2"),
            "focused on root, the status line must show root's own 1/2: {root_text}"
        );
        assert!(
            !root_text.contains("todo: 2/3"),
            "focused on root, the status line must NOT show the child's 2/3: {root_text}"
        );

        // Switching focus to the child must show the CHILD's own 2/3.
        app.try_focus_agent(child, None)
            .await
            .expect("focusing the child must succeed");
        assert!(app.refresh_plugin_status_contributions());
        let child_text = crate::tui::test_support::render_text(&app.state, 120, 40);
        assert!(
            child_text.contains("todo: 2/3"),
            "focused on the child, the status line must show the child's own 2/3: {child_text}"
        );
        assert!(
            !child_text.contains("todo: 1/2"),
            "focused on the child, the status line must NOT show root's own 1/2: {child_text}"
        );
    }

    #[tokio::test]
    async fn a_background_agents_todo_write_does_not_change_the_focused_agents_value() {
        let conway = build(vec![
            ScriptedTurn::Respond(todo_write_call(
                "call_1",
                serde_json::json!([
                    {"text": "focused item", "status": "pending"},
                ]),
            )),
            ScriptedTurn::Respond(text_response("focused done")),
            ScriptedTurn::Respond(todo_write_call(
                "call_2",
                serde_json::json!([
                    {"text": "bg item 1", "status": "done"},
                    {"text": "bg item 2", "status": "done"},
                ]),
            )),
            ScriptedTurn::Respond(text_response("background done")),
        ]);

        let mut app = app_with_plugins_field(&conway).await;
        let root = app.handle.root();

        drive_two_round_tool_turn(&app.handle, root, "track root's own work").await;
        assert!(app.refresh_plugin_status_contributions());
        let before = crate::tui::test_support::render_text(&app.state, 120, 40);
        assert!(
            before.contains("todo: 0/1"),
            "precondition: root's own 0/1 must already be on screen: {before}"
        );

        // A real `todo_write` call on a completely different, BACKGROUND
        // agent -- root stays focused throughout. This is the exact event
        // that used to blank or replace the focused agent's own value when
        // read through the single-value `Plugin::status_contributions`
        // alone.
        let background = app
            .handle
            .spawn(root, conway::SpawnSpec::new("background child's prompt"))
            .await
            .expect("spawn should succeed");
        drive_two_round_tool_turn(&app.handle, background, "track background work").await;

        assert_eq!(
            app.state.focused_agent, root,
            "precondition: the background agent's own turn must never move focus"
        );
        // The poll is scoped to the FOCUSED agent (root), so the
        // background agent's own write is not merely overridden -- it is
        // never even READ, and the refresh correctly reports no change at
        // all (proving this is not "poll everyone, then throw the
        // background one away downstream").
        assert!(
            !app.refresh_plugin_status_contributions(),
            "root's own contribution did not change, so a poll scoped to root must report no \
             change at all -- a background agent's own write must never even register as a \
             change to ROOT's own entry"
        );
        let after = crate::tui::test_support::render_text(&app.state, 120, 40);
        assert!(
            after.contains("todo: 0/1"),
            "a background agent's own todo_write must never change the FOCUSED agent's own \
             contribution: {after}"
        );
        assert!(
            !after.contains("todo: 2/2"),
            "the background agent's own list must never leak onto the focused agent's status \
             line: {after}"
        );
    }
}
