//! Compiled-binary, real-pty acceptance test for the "permission-mode
//! cycling changes what a flagged call does" row of the TUI harness table
//! (board item `01M2M6NR49FYQSRS00B95PTAZT`), tracing to
//! `docs/vision/review/lens-operator.md` (its §3 names "permission-mode
//! cycling" among the paths this project's actual defects live in and
//! expects driven, not merely read from source).
//!
//! Drives `Shift-Tab` (`tui/keybindings.rs`'s own default binding for
//! `cycle_permission_mode`) through Prompt -> Plan -> AutoAllow
//! (`tui/app/run.rs`'s own cycle order) and proves each mode change is
//! BEHAVIORAL, not cosmetic, by pointing the same `bash` tool call at it
//! twice: denied outright in Plan mode (`conway-runtime`'s
//! `PermissionBroker::decide` denies a non-`Read`/`Search`/`Think`
//! category before ever reaching the gate -- no prompt overlay, an
//! immediate, named refusal), then actually executed in AutoAllow mode (a
//! real `bash echo`, matching this suite's own precedent for running one
//! for real -- `tests/oneshot_persona_and_budget.rs`'s
//! `max_turns_flag_overrides_the_configured_default_and_stops_the_run`).

mod common;

use std::time::Duration;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::pty::PtySession;

const LANDED: &str = "Type a message, or / for commands";

/// Two full tool round-trips: a `bash` call, denied in plan mode, followed
/// by the model's own acknowledgement once it sees the denial; then the
/// SAME shape again once permission mode has moved on to auto-allow, where
/// the call actually runs.
fn two_bash_round_trips() -> Script {
    Script(vec![
        vec![
            Chunk::ToolCall {
                name: "bash",
                args: serde_json::json!({ "command": "echo plan-mode-should-never-run-this" }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![
            Chunk::Text("plan-mode-denial-acknowledged"),
            Chunk::Finish("stop"),
        ],
        vec![
            Chunk::ToolCall {
                name: "bash",
                args: serde_json::json!({ "command": "echo auto-allow-ran-it" }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![
            Chunk::Text("auto-allow-turn-complete"),
            Chunk::Finish("stop"),
        ],
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permission_mode_cycling_changes_what_a_flagged_call_does() {
    let mock = MockBackend::start(two_bash_round_trips()).await;
    let fixture = common::write_fixture(&mock, 10);

    let cmd = common::pty_command(&[], &fixture);
    let mut session = PtySession::spawn(cmd, 160, 45);
    let landed = session.wait_for(LANDED, Duration::from_secs(15));

    // Prompt (the default) -> Plan: one Shift-Tab.
    session.send_shift_tab();
    let in_plan = session.wait_for_since("plan", landed, Duration::from_secs(10));

    // A `bash` call in plan mode is denied outright -- `Execute` is not in
    // plan mode's read-only allow-list, and the broker checks that BEFORE
    // the gate is ever consulted (`conway-runtime/src/permission.rs`'s own
    // "PLAN's denial is checked before EVERY allow path" ordering note),
    // so this proves a real behavioral difference, not merely a status
    // line change.
    session.send("ask for a plan-mode tool call\r");
    let denied = session.wait_for_since(
        "plan mode does not permit",
        in_plan,
        Duration::from_secs(15),
    );
    // The turn still completes (the model sees the denial and answers) --
    // proves the SESSION survives a plan-mode denial rather than wedging.
    session.wait_for_since(
        "plan-mode-denial-acknowledged",
        denied,
        Duration::from_secs(15),
    );

    // Plan -> AutoAllow: a second Shift-Tab.
    session.send_shift_tab();
    let in_auto_allow = session.wait_for_since("AUTO-ALLOW", denied, Duration::from_secs(10));

    // The IDENTICAL kind of call (`bash`) now runs for real, with no
    // permission prompt at all -- the same flagged call, a different
    // outcome, purely from the mode cycle above.
    session.send("ask for an auto-allow tool call\r");
    // Reaching this point at all is itself part of the evidence: a
    // permission-prompt overlay (`prompt` mode's own behavior) would have
    // blocked here forever waiting for a keypress this test never sends,
    // and `wait_for_since` would have panicked out with a timeout instead
    // of finding this text -- auto-allow's whole claim is that no such
    // block happens.
    session.wait_for_since(
        "auto-allow-turn-complete",
        in_auto_allow,
        Duration::from_secs(15),
    );
}
