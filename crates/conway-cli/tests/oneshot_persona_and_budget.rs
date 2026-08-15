//! Integration tests for the one-shot mode flags added by board item
//! 01M00QG7GHHVDKRC0J87NH0FNR: `--agent`, `--system-prompt`/
//! `--append-system-prompt`, and the budget flags (`--max-turns`/
//! `--max-tokens`/`--max-seconds`) -- driven against the real, compiled
//! `conway` binary, exactly as `tests/continuity.rs` drives `--session`/
//! `--resume`/`--fork-from`.
//!
//! Every positive assertion below reads the REAL wire request the mock
//! backend received (`mock.requests()`), or the REAL process exit code with
//! ZERO requests reached (the budget tests) -- never a parsed-flag
//! assertion alone, per this item's own standard: "a flag that parses and
//! then does nothing is worse than a missing flag."

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{run_conway, write_fixture, Fixture};

/// Writes `.conway/agents/<name>.md` inside `fixture`'s own temp dir (the
/// process cwd `common::command` sets for the spawned binary), with
/// `body` as the agent's system-prompt text.
fn write_agent_def(fixture: &Fixture, name: &str, body: &str) {
    let dir = fixture.dir.path().join(".conway").join("agents");
    std::fs::create_dir_all(&dir).expect("create .conway/agents");
    let content = format!("---\nname: {name}\n---\n{body}\n");
    std::fs::write(dir.join(format!("{name}.md")), content).expect("write agent def");
}

fn ok_script() -> Script {
    Script(vec![vec![Chunk::Text("ok"), Chunk::Finish("stop")]])
}

/// The `content` string of every `role: "system"` message in `request`'s
/// `messages` array, concatenated -- the observable this whole file checks
/// its system-prompt assertions against, read straight off the real wire
/// request rather than any intermediate signal.
fn system_message_text(request: &serde_json::Value) -> String {
    request["messages"]
        .as_array()
        .expect("request must carry a messages array")
        .iter()
        .filter(|m| m["role"] == "system")
        .map(|m| m["content"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------
// --agent
// ---------------------------------------------------------------------

/// `--agent <name>` selects a named `.conway/agents/<name>.md` def: its own
/// `system_prompt` text reaches the real backend request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_flag_selects_named_def_and_its_system_prompt_reaches_the_request() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);
    write_agent_def(
        &fixture,
        "reviewer",
        "You are REVIEWER-PERSONA-MARKER, a careful code reviewer.",
    );

    let out = run_conway(&["-p", "hi", "--agent", "reviewer"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let requests = mock.requests();
    let system_text = system_message_text(requests.last().expect("one request"));
    assert!(
        system_text.contains("REVIEWER-PERSONA-MARKER"),
        "the named agent def's own system_prompt must reach the request, got system \
         message(s): {system_text:?}"
    );
}

/// An unknown `--agent` name is a usage error naming both the requested
/// name and the directory searched -- never a silent no-op that quietly
/// runs with no persona at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_flag_unknown_name_is_a_usage_error_not_a_silent_no_op() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["-p", "hi", "--agent", "does-not-exist"], &fixture);

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("does-not-exist"),
        "stderr must name the requested agent: {stderr}"
    );
    assert!(mock.requests().is_empty(), "no request must have been sent");
}

/// `--agent` combined with `--resume` is a usage error: a resumed session's
/// agent definition is fixed by the session it continues, and `Conway::
/// resume` has no parameter to carry an override through at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_flag_with_resume_is_a_usage_error() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);
    write_agent_def(&fixture, "reviewer", "You are a reviewer.");

    let first = run_conway(&["-p", "hi"], &fixture);
    assert!(first.status.success());

    // Any session id (even a fresh, never-created one) is enough: the
    // usage-error guard fires before `--resume` is ever looked up.
    let out = run_conway(
        &[
            "-p",
            "hi",
            "--agent",
            "reviewer",
            "--resume",
            &conway::SessionId::new().to_string(),
        ],
        &fixture,
    );
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--agent") && stderr.contains("--resume"),
        "stderr should name both flags: {stderr}"
    );
}

// ---------------------------------------------------------------------
// --system-prompt / --append-system-prompt
// ---------------------------------------------------------------------

/// `--system-prompt` alone, with no `--agent`: the run gets EXACTLY that
/// text as its system prompt -- the mechanism that stops a one-shot run
/// from being the built-in coding agent at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_prompt_flag_replaces_the_default_entirely() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(
        &[
            "-p",
            "5 + 7",
            "--system-prompt",
            "You are HAIKU-BOT-MARKER, a calculator that only speaks in haiku.",
        ],
        &fixture,
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let requests = mock.requests();
    let system_text = system_message_text(requests.last().expect("one request"));
    assert_eq!(
        system_text, "You are HAIKU-BOT-MARKER, a calculator that only speaks in haiku.",
        "the system prompt must be EXACTLY the flag's text, with no coding-agent framing added"
    );
}

/// `--system-prompt` wins over a named `--agent`'s own `system_prompt` --
/// the def's `role`/`tools`/`model` still apply (not directly observable
/// through this mock's wire request), only the prompt TEXT is replaced.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_prompt_flag_overrides_the_named_agents_own_prompt() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);
    write_agent_def(&fixture, "reviewer", "REVIEWER-DEFAULT-MARKER text.");

    let out = run_conway(
        &[
            "-p",
            "hi",
            "--agent",
            "reviewer",
            "--system-prompt",
            "OVERRIDE-MARKER text.",
        ],
        &fixture,
    );
    assert!(out.status.success());

    let requests = mock.requests();
    let system_text = system_message_text(requests.last().expect("one request"));
    assert!(system_text.contains("OVERRIDE-MARKER"));
    assert!(
        !system_text.contains("REVIEWER-DEFAULT-MARKER"),
        "the agent def's own prompt text must be fully replaced, not merged: {system_text:?}"
    );
}

/// `--append-system-prompt` adds to the named `--agent`'s own prompt,
/// rather than replacing it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn append_system_prompt_flag_appends_to_the_named_agents_own_prompt() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);
    write_agent_def(&fixture, "reviewer", "REVIEWER-BASE-MARKER text.");

    let out = run_conway(
        &[
            "-p",
            "hi",
            "--agent",
            "reviewer",
            "--append-system-prompt",
            "EXTRA-RULE-MARKER: always cite line numbers.",
        ],
        &fixture,
    );
    assert!(out.status.success());

    let requests = mock.requests();
    let system_text = system_message_text(requests.last().expect("one request"));
    assert!(
        system_text.contains("REVIEWER-BASE-MARKER"),
        "the agent def's own prompt text must still be present: {system_text:?}"
    );
    assert!(
        system_text.contains("EXTRA-RULE-MARKER"),
        "the appended text must also be present: {system_text:?}"
    );
}

/// `--append-system-prompt` alone (no `--agent`, no `--system-prompt`)
/// becomes the entire system prompt by itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn append_system_prompt_alone_becomes_the_entire_prompt() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(
        &[
            "-p",
            "hi",
            "--append-system-prompt",
            "SOLO-APPEND-MARKER text.",
        ],
        &fixture,
    );
    assert!(out.status.success());

    let requests = mock.requests();
    let system_text = system_message_text(requests.last().expect("one request"));
    assert_eq!(system_text, "SOLO-APPEND-MARKER text.");
}

/// `--system-prompt`/`--append-system-prompt` combined with `--resume` or
/// `--fork-from` is a usage error, not a silent drop: neither facade path
/// (`Conway::resume`, `ForkSpec`) has a literal-text field to carry it
/// through.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_prompt_flag_is_a_usage_error_with_resume_and_fork_from() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);

    let first = run_conway(&["-p", "hi"], &fixture);
    assert!(first.status.success());
    let sid = conway::SessionId::new(); // never created; the guard fires first either way

    let resume_out = run_conway(
        &[
            "-p",
            "hi",
            "--system-prompt",
            "x",
            "--resume",
            &sid.to_string(),
        ],
        &fixture,
    );
    assert_eq!(resume_out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&resume_out.stderr).contains("--resume"));

    let fork_out = run_conway(
        &[
            "-p",
            "hi",
            "--append-system-prompt",
            "x",
            "--fork-from",
            &sid.to_string(),
        ],
        &fixture,
    );
    assert_eq!(fork_out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&fork_out.stderr).contains("--fork-from"));
}

// ---------------------------------------------------------------------
// Budget flags: --max-turns / --max-tokens / --max-seconds
// ---------------------------------------------------------------------

/// `--max-turns 1`, with the fixture's OWN `[limits].max_steps` set much
/// higher (10): the run still stops after exactly one turn with exit 5
/// (`BudgetExceeded`) -- proving the FLAG, not the config default, capped
/// it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_turns_flag_overrides_the_configured_default_and_stops_the_run() {
    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall {
                name: "bash",
                args: serde_json::json!({ "command": "echo hi" }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![
            Chunk::ToolCall {
                name: "bash",
                args: serde_json::json!({ "command": "echo again" }),
            },
            Chunk::Finish("tool_calls"),
        ],
    ]))
    .await;
    let fixture = write_fixture(&mock, 10); // [limits].max_steps = 10

    let out = run_conway(
        &["-p", "hi", "--max-turns", "1", "--allowed-tools", "bash"],
        &fixture,
    );

    assert_eq!(
        out.status.code(),
        Some(5),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        mock.requests().len(),
        1,
        "--max-turns 1 must stop the run after exactly one request, even though the \
         configured [limits].max_steps (10) would have allowed many more"
    );
}

/// `--max-tokens 0`: the budget check runs before the very first request is
/// ever dispatched (`agent_loop::AgentLoop::check_budget`, at the top of
/// the loop) -- with zero tokens spent so far, `0 >= 0` trips immediately,
/// so the run exits 5 with NO request ever reaching the backend. Proves the
/// flag reaches a live `Budget.max_tokens`, independent of any usage this
/// mock does not simulate reporting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_tokens_flag_reaches_the_budget_and_stops_the_run_before_any_request() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 40);

    let out = run_conway(&["-p", "hi", "--max-tokens", "0"], &fixture);

    assert_eq!(
        out.status.code(),
        Some(5),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        mock.requests().is_empty(),
        "the budget check runs before any request is dispatched; --max-tokens 0 must trip \
         it immediately"
    );
}

/// `--max-seconds 0`: the deadline is `now`, so by the time the loop's own
/// `check_budget` reads `Utc::now()` a moment later, the deadline has
/// already passed -- the run exits 5 with no request ever reaching the
/// backend. Same mechanism as the `--max-tokens 0` test above, for the
/// wall-clock dimension.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_seconds_flag_reaches_the_budget_and_stops_the_run_before_any_request() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 40);

    let out = run_conway(&["-p", "hi", "--max-seconds", "0"], &fixture);

    assert_eq!(
        out.status.code(),
        Some(5),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        mock.requests().is_empty(),
        "the budget check runs before any request is dispatched; --max-seconds 0 must trip \
         it immediately"
    );
}

/// Budget flags combined with `--resume`/`--fork-from` are a usage error,
/// not a silent drop -- neither facade path accepts a caller-supplied
/// budget override today.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn budget_flags_are_a_usage_error_with_resume_and_fork_from() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);

    let first = run_conway(&["-p", "hi"], &fixture);
    assert!(first.status.success());
    let sid = conway::SessionId::new();

    let out = run_conway(
        &["-p", "hi", "--max-turns", "1", "--resume", &sid.to_string()],
        &fixture,
    );
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--max-turns") && stderr.contains("--resume"),
        "{stderr}"
    );
}

/// Sanity: `--agent` genuinely composes with `--fork-from` (unlike
/// `--system-prompt`/budget) -- `ForkSpec::agent_def` already exists and is
/// wired for real, so a forked child can select a different named persona
/// from its parent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_flag_composes_with_fork_from() {
    let mock = MockBackend::start(Script(vec![
        vec![Chunk::Text("root reply"), Chunk::Finish("stop")],
        vec![Chunk::Text("child reply"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);
    write_agent_def(&fixture, "reviewer", "REVIEWER-FORK-MARKER text.");

    let first = run_conway(&["-p", "hi"], &fixture);
    assert!(first.status.success());

    let conway_lib = open_conway(&fixture).await;
    let sessions = conway_lib
        .sessions(conway::SessionFilter::default())
        .await
        .expect("list sessions");
    let parent = sessions[0].id;

    let second = run_conway(
        &[
            "-p",
            "branch",
            "--fork-from",
            &parent.to_string(),
            "--agent",
            "reviewer",
        ],
        &fixture,
    );
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let system_text = system_message_text(&requests[1]);
    assert!(
        system_text.contains("REVIEWER-FORK-MARKER"),
        "the forked child must carry the --agent-selected def's own system prompt: \
         {system_text:?}"
    );
}

/// Mirrors `tests/continuity.rs`'s own `open_conway` helper -- a fresh,
/// read-only `Conway` against `fixture`'s on-disk session store, for
/// reading back facts the compiled binary's subprocess just wrote.
async fn open_conway(fixture: &Fixture) -> conway::Conway {
    use std::sync::Arc;

    use conway::config::CliOverrides;
    use conway::gates::AllowListGate;
    use conway::{ConwayBuilder, PermissionGate};

    let gate: Arc<dyn PermissionGate> = Arc::new(AllowListGate::new(Vec::new(), Vec::new()));
    ConwayBuilder::from_config_only(&fixture.config_path)
        .expect("load fixture config")
        .with_cli_overrides(CliOverrides {
            cwd: Some(fixture.dir.path().to_path_buf()),
            ..Default::default()
        })
        .with_permission_gate(gate)
        .with_backend_factory(Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory))
        .build()
        .expect("build conway against the fixture's own store")
}
