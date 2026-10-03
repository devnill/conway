//! Board item `01M3XGPGT5W7GABVTC7F2NA0C9`: the live-path acceptance proof
//! for `SessionHandle::abort_turn` -- the non-terminal primitive behind the
//! TUI's first `Ctrl-C` and `tui.busy_input = "interrupt"`.
//!
//! Two scenarios, both against a real `keep_alive` session (the interactive
//! root's own shape -- `app/startup.rs::session_spec`):
//!
//! - [`ctrl_c_mid_generation_abort_lets_the_session_continue_with_a_new_prompt`]:
//!   the backend call itself hangs (`ScriptedTurn::Pending`).
//! - [`ctrl_c_mid_tool_execution_kills_the_process_group_and_the_session_continues`]:
//!   a REAL `bash` tool call (the `builtin-tools` feature's genuine
//!   `BashTool`, not a fixture) is held mid-execution.
//!
//! Both prove the full acceptance bar from the spec in one place: after one
//! abort, (1) the agent accepts and answers another prompt in the SAME
//! session, (2) a held tool's process group is actually dead, and (3) the
//! session log records the abort (`LogRecord::SystemNote { reason:
//! "turn_aborted_by_operator", .. }`).
#![cfg(feature = "builtin-tools")]

use std::sync::Arc;
use std::time::Duration;

use conway::test_support::{allow_once_gate, base_config, scripted_backend, test_builder};
use conway::{PluginSelection, SessionSpec};
use conway_core::content::{StopReason, ToolCall, Usage};
use conway_core::ids::ToolName;
use conway_core::log::LogRecord;
use conway_core::ports::{GenerateResponse, SessionStore};
use conway_testkit::{text_response, FakeStore, ScriptedTurn};

/// How long a test sleeps after `new_session` to give an idle keep_alive
/// session's agent loop a moment to actually reach its idle-await gate
/// before the test's own `prompt()` call races it -- mirrors `keep_alive.rs`'s
/// own identical `SETTLE` constant exactly (a prompt-less session never runs
/// a spontaneous turn, so this is purely a startup race, not a wait for work).
const SETTLE: Duration = Duration::from_millis(100);

/// One scripted `bash` call whose `command` argument is `command` verbatim --
/// mirrors `shell_prefix_grant_newline_seam.rs`'s own `bash_call_response`.
fn bash_call_response(command: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: "call_1".to_string(),
            name: ToolName::new("bash"),
            arguments: serde_json::json!({ "command": command }),
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

/// Polls `cond` every 10ms until it returns `true` or `bound` elapses,
/// panicking with `msg` on timeout -- a small shared wait helper so each
/// test's own polling loop stays a one-liner.
async fn wait_until(bound: Duration, msg: &str, mut cond: impl FnMut() -> bool) {
    tokio::time::timeout(bound, async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{msg}"));
}

/// **Mid-generation abort.** The backend's first call hangs forever
/// (`ScriptedTurn::Pending`, `conway-testkit`'s own fixture for exactly this
/// -- nothing custom needed). `SessionHandle::abort_turn` must reach it (the
/// turn-scoped abort token this item adds is raced INSIDE `attempt.rs`'s
/// `run_generate`/`run_stream`, in place of the agent's whole-lifetime
/// token -- see `agent_loop.rs`'s own `turn_cancel`), return the root to
/// idling, and leave the session able to run a genuine SECOND turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_c_mid_generation_abort_lets_the_session_continue_with_a_new_prompt() {
    let backend = scripted_backend(vec![
        ScriptedTurn::Pending,
        ScriptedTurn::Respond(text_response("second-response")),
    ]);
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = test_builder(base_config())
        .with_backend(backend)
        .with_session_store(store)
        .build()
        .expect("build should succeed");

    let handle = conway
        .new_session(SessionSpec {
            keep_alive: true,
            ..SessionSpec::default()
        })
        .await
        .expect("new_session should succeed");
    let root = handle.root();

    tokio::time::sleep(SETTLE).await;
    assert!(
        handle.awaiting_prompt(root),
        "sanity: a prompt-less keep_alive session idles at its resume gate"
    );

    // Do NOT await `.text()` on this turn -- the backend never resolves, so
    // it would hang forever. Submitting it is enough to drive the agent
    // past the resume gate and into the (permanently pending) backend call.
    let _turn1 = handle
        .prompt("first turn text")
        .await
        .expect("prompt should be accepted");

    wait_until(
        Duration::from_secs(5),
        "the root must leave its resume gate once a turn is submitted",
        || !handle.awaiting_prompt(root),
    )
    .await;

    let aborted = handle
        .abort_turn(root, "ctrl-c")
        .await
        .expect("abort_turn should not error");
    assert!(
        aborted,
        "a turn was genuinely in flight (mid-generation) -- abort_turn must report it aborted one"
    );

    wait_until(
        Duration::from_secs(5),
        "LOAD-BEARING: the root must return to its resume gate after a mid-generation abort -- \
         a timeout here means the agent is stuck, not merely slow",
        || handle.awaiting_prompt(root),
    )
    .await;

    // The session log records the abort -- `LogRecord::SystemNote` with the
    // operator-abort reason key, persisted BEFORE the live event (this
    // loop's own `abort_current_turn`'s doc).
    let records = handle
        .transcript(root)
        .await
        .expect("transcript should be readable");
    assert!(
        records.iter().any(|r| matches!(
            r,
            LogRecord::SystemNote { reason, .. } if reason == "turn_aborted_by_operator"
        )),
        "the session log must record the operator abort: {records:?}"
    );

    // The agent accepts and answers ANOTHER prompt in the SAME session.
    let turn2 = handle
        .prompt("second turn text")
        .await
        .expect("second prompt on the same live session should succeed");
    let text2 = tokio::time::timeout(Duration::from_secs(5), turn2.text())
        .await
        .expect(
            "second turn's text() must not hang -- this is the acceptance bar: the SAME session \
             accepts and answers a new prompt after a mid-turn abort",
        )
        .expect("second turn's text() should succeed");
    assert_eq!(text2, "second-response");
}

/// **Mid-tool-execution abort.** A real `bash` call is held inside its own
/// `sleep`, long enough to guarantee `abort_turn` lands while `ToolRunner::
/// run_batch` is still awaiting it. Proves the THIRD acceptance bar this
/// item's tool-dispatch site adds on top of the mid-generation case above:
/// the held tool's own process group is actually dead afterward, via the
/// SAME `ToolCtx::cancel`/`kill_group` path an ordinary whole-agent cancel
/// has always used -- this item only changes WHICH token feeds it, never the
/// kill mechanism itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ctrl_c_mid_tool_execution_kills_the_process_group_and_the_session_continues() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pidfile = tmp.path().join("pid");

    let backend = scripted_backend(vec![
        ScriptedTurn::Respond(bash_call_response(&format!(
            "echo $$ > {} && sleep 30",
            pidfile.display()
        ))),
        ScriptedTurn::Respond(text_response("second-response")),
    ]);
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = test_builder(base_config())
        .with_backend(backend)
        .with_permission_gate(allow_once_gate())
        .with_session_store(store)
        .with_builtin_plugins(PluginSelection::All)
        .build()
        .expect("build should succeed with the real builtin bash tool registered");

    let handle = conway
        .new_session(SessionSpec {
            keep_alive: true,
            ..SessionSpec::default()
        })
        .await
        .expect("new_session should succeed");
    let root = handle.root();

    tokio::time::sleep(SETTLE).await;

    let _turn1 = handle
        .prompt("run the held command")
        .await
        .expect("prompt should be accepted");

    wait_until(
        Duration::from_secs(10),
        "the held bash call must actually start running (its pidfile must appear)",
        || pidfile.exists(),
    )
    .await;
    let pid: u32 = tokio::fs::read_to_string(&pidfile)
        .await
        .expect("pidfile must be readable")
        .trim()
        .parse()
        .expect("pidfile must contain a bare pid");
    assert!(
        pid_is_alive(pid).await,
        "sanity: the held bash process must be alive right before the abort"
    );

    let aborted = handle
        .abort_turn(root, "ctrl-c")
        .await
        .expect("abort_turn should not error");
    assert!(
        aborted,
        "the bash call was genuinely in flight -- abort_turn must report it aborted one"
    );

    wait_until(
        Duration::from_secs(10),
        "LOAD-BEARING: the root must return to its resume gate after a mid-tool abort",
        || handle.awaiting_prompt(root),
    )
    .await;

    let dead = tokio::time::timeout(Duration::from_secs(5), async {
        while pid_is_alive(pid).await {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .is_ok();
    assert!(
        dead,
        "LOAD-BEARING: the held bash process's whole process group must be dead after the abort"
    );

    let records = handle
        .transcript(root)
        .await
        .expect("transcript should be readable");
    assert!(
        records.iter().any(|r| matches!(
            r,
            LogRecord::SystemNote { reason, .. } if reason == "turn_aborted_by_operator"
        )),
        "the session log must record the operator abort: {records:?}"
    );

    let turn2 = handle
        .prompt("second turn text")
        .await
        .expect("second prompt on the same live session should succeed");
    let text2 = tokio::time::timeout(Duration::from_secs(5), turn2.text())
        .await
        .expect("second turn's text() must not hang -- the session must still be alive")
        .expect("second turn's text() should succeed");
    assert_eq!(text2, "second-response");
}

/// `ps -p <pid>` succeeds iff `pid` names a live process -- portable across
/// the BSD `ps` on macOS dev boxes and procps-ng on Linux CI, mirroring
/// `conway-tools`' own `still_running_group_members`'s choice of `ps` over a
/// `/proc`-only syscall for the identical cross-platform reason.
async fn pid_is_alive(pid: u32) -> bool {
    tokio::process::Command::new("ps")
        .args(["-p", &pid.to_string()])
        .output()
        .await
        .map(|output| output.status.success())
        .unwrap_or(false)
}
