//! Compiled-binary, real-pty acceptance tests for board item
//! `01M3WJ7906NP3P2ZNVDK549T1Q`: closing the terminal (`SIGHUP`), or a
//! process manager stopping conway (`SIGTERM`), while the TUI has a long
//! `!` command running must not leave that command's child running
//! unsupervised, must restore the terminal, and must exit with the SAME
//! documented signal-specific code one-shot mode already uses (see
//! `docs/scripting.md`'s exit-code table and this item's own note in
//! `docs/interactive.md`'s "Closing the terminal, or a process manager
//! stopping conway").
//!
//! # Why a BACKGROUNDED grandchild, not a plain foreground command
//!
//! `!sleep 999999 & echo $! > <file> && wait` is the EXACT shape
//! `tui/app/shell_cmd.rs`'s own
//! `quitting_while_a_bang_command_runs_kills_its_whole_process_group` test
//! uses, for the identical reason that test's own doc gives: a background
//! job never gets its own process group under non-interactive `bash -c`, so
//! `sleep`'s pid is genuinely different from the `!` command's own leader
//! (the shell running `... && wait`). Only a real `kill(-pgid, ..)` against
//! the WHOLE group reaches it -- a leader-only kill (`kill_on_drop(true)`
//! alone, with no group signal at all) would coincidentally still catch a
//! plain foreground command via bash's own exec optimization, proving
//! nothing about whether this item's group-kill path actually ran. Sending
//! the real OS signal to the TUI's own process (not calling
//! `purge_open_ask_modal` directly, the way that unit test does) is what
//! proves the NEW wiring this item adds -- `tui::run`'s own
//! `signal::install_termination` call and `App::run`'s own
//! `termination.notified()` arm -- rather than re-proving machinery that
//! already existed and was already covered.
//!
//! # Isolation
//!
//! Every fixture here goes through [`common::write_fixture`]/
//! [`common::pty_command`], which already point `CONWAY_CONFIG_DIR` and the
//! subprocess's own cwd at a fresh `tempfile::tempdir()` -- never this
//! developer's real `~/.conway` -- exactly like every other `tui_*.rs` suite
//! in this directory.

#[allow(dead_code)]
mod common;

use std::time::{Duration, Instant};

use common::mock_backend::{Chunk, MockBackend, Script};
use common::pty::PtySession;
use common::Fixture;

const LANDED: &str = "Type a message, or / for commands";

/// crossterm's own `LeaveAlternateScreen` escape, raw -- `tui::mod.rs`'s
/// `restore_terminal` emits this (among other steps) on every exit path,
/// including the one this item adds. Searched for in the RAW, un-stripped
/// pty output (`PtySession::raw_contains`), never the de-ANSI'd `screen()`,
/// which exists specifically to discard sequences exactly like this one.
const LEAVE_ALT_SCREEN: &[u8] = b"\x1b[?1049l";

/// No scripted turns: this suite never sends a prompt to the model, only a
/// `!` shell command, which never reaches the backend at all
/// (`tui/app/shell_cmd.rs`'s own module doc: a `!` command runs through the
/// operator's own shell, never through the model or its tools).
fn no_turns_script() -> Script {
    Script(vec![])
}

/// Waits (bounded) for `path` to contain a parseable pid, polling -- the
/// same shape `tui/app/shell_cmd.rs`'s own in-process tests use for the
/// identical "the backgrounded child wrote its own pid" synchronization,
/// just over a real file a SEPARATE (pty-attached) process writes instead
/// of one this test process spawned directly.
fn wait_for_pid_file(path: &std::path::Path, timeout: Duration) -> i32 {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        if Instant::now() > deadline {
            panic!(
                "the backgrounded child never wrote its pid to {} within {timeout:?}",
                path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Polls `kill(pid, 0)` (a signal-less liveness probe, never one that can
/// itself kill anything) until the OS reports the process gone (`ESRCH`),
/// bounded by `timeout` -- never a single check, which could race the
/// signal's own asynchronous delivery and cleanup. Panics, naming `pid`, if
/// `timeout` elapses with the process still alive.
fn wait_for_process_death(pid: i32, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
            return;
        }
        if Instant::now() > deadline {
            panic!(
                "pid {pid} (the backgrounded grandchild of the TUI's own `!` command) is still \
                 alive after {timeout:?} -- it was orphaned rather than killed"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn start_bang_and_capture_grandchild_pid(
    session: &mut PtySession,
    fixture_dir: &std::path::Path,
) -> i32 {
    let pid_file = fixture_dir.join("bang-grandchild.pid");
    session.send(&format!(
        "!sleep 999999 & echo $! > {} && wait\r",
        pid_file.display()
    ));
    wait_for_pid_file(&pid_file, Duration::from_secs(5))
}

/// ACCEPTANCE: a real `SIGTERM` (the signal a process manager/`kill`/the
/// `bash` tool's own `kill_group` sends, per `signal.rs`'s own module doc)
/// sent to a TUI with a `!` command running kills that command's whole
/// process group -- including a backgrounded grandchild the leader alone
/// cannot reach -- restores the terminal, and exits with the documented
/// `143` (`128 + SIGTERM(15)`), the SAME code one-shot mode's own
/// `sigterm_leaves_a_terminal_agent_result_as_the_logs_last_record`
/// (`tests/oneshot.rs`) already proves for `-p` mode.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "harness busy-spins under portable-pty; behaviour verified manually in tmux 2026-10-06 -- board item 01M488Q85AE58P0S666NED2R4P"]
async fn sigterm_kills_the_bang_commands_whole_process_group_and_exits_143() {
    let mock = MockBackend::start(no_turns_script()).await;
    let fixture = common::write_fixture(&mock, 10);

    let cmd = common::pty_command(&[], &fixture);
    let mut session = PtySession::spawn(cmd, 160, 45);
    session.wait_for(LANDED, Duration::from_secs(15));

    let grandchild_pid = start_bang_and_capture_grandchild_pid(&mut session, fixture.dir.path());

    let tui_pid = session.pid();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(tui_pid as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .expect("send SIGTERM to the TUI process");

    // The sharpest possible assertion: the OS-level process is actually
    // gone, not merely that `execute` labeled it killed (there is no such
    // label to read here at all -- this is a SEPARATE process observed only
    // through `kill(pid, 0)`, exactly the shape `shell_cmd.rs`'s own
    // `ctrl_c_kills_the_child_process_group` test uses for the identical
    // reason).
    wait_for_process_death(grandchild_pid, Duration::from_secs(10));

    let status = session.wait_for_exit(Duration::from_secs(10));
    assert_eq!(
        status.exit_code(),
        143,
        "documented SIGTERM exit code (128 + 15), matching one-shot mode's own \
         TerminatedBySigterm; screen:\n{}",
        session.screen()
    );

    assert!(
        session.raw_contains(LEAVE_ALT_SCREEN),
        "the terminal must be restored (LeaveAlternateScreen emitted) on the SIGTERM exit path, \
         not left in the alternate screen"
    );
}

/// The `SIGHUP` sibling of the test above -- same shape, the other
/// documented signal-specific exit code (`129` = `128 + SIGHUP(1)`), the
/// signal a closed controlling terminal sends.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "harness busy-spins under portable-pty; behaviour verified manually in tmux 2026-10-06 -- board item 01M488Q85AE58P0S666NED2R4P"]
async fn sighup_kills_the_bang_commands_whole_process_group_and_exits_129() {
    let mock = MockBackend::start(no_turns_script()).await;
    let fixture = common::write_fixture(&mock, 10);

    let cmd = common::pty_command(&[], &fixture);
    let mut session = PtySession::spawn(cmd, 160, 45);
    session.wait_for(LANDED, Duration::from_secs(15));

    let grandchild_pid = start_bang_and_capture_grandchild_pid(&mut session, fixture.dir.path());

    let tui_pid = session.pid();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(tui_pid as i32),
        nix::sys::signal::Signal::SIGHUP,
    )
    .expect("send SIGHUP to the TUI process");

    wait_for_process_death(grandchild_pid, Duration::from_secs(10));

    let status = session.wait_for_exit(Duration::from_secs(10));
    assert_eq!(
        status.exit_code(),
        129,
        "documented SIGHUP exit code (128 + 1), matching one-shot mode's own \
         TerminatedBySighup; screen:\n{}",
        session.screen()
    );

    assert!(
        session.raw_contains(LEAVE_ALT_SCREEN),
        "the terminal must be restored (LeaveAlternateScreen emitted) on the SIGHUP exit path, \
         not left in the alternate screen"
    );
}

// ---------------------------------------------------------------------
// Board item `01M3WJ7906NP3P2ZNVDK549T1Q`, round 2: a MODEL-issued `bash`
// tool call's own child, not an operator-typed `!` command.
// ---------------------------------------------------------------------

/// As [`common::write_fixture`], except `tools.builtin_plugins` also names
/// `"conway.shell"` -- the one opt-in a scripted `bash` tool call needs to
/// reach the runtime at all (the TUI's real, shipped default never
/// registers it; see `tui_permission_mode.rs`'s own `write_fixture_with_bash`,
/// which this is a byte-for-byte sibling copy of -- each `tests/*.rs` file
/// compiles independently, so a second copy here is this suite's own
/// established convention, not an oversight).
fn write_fixture_with_bash(mock: &common::mock_backend::MockHandle, max_steps: u32) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = serde_json::json!({
        "default_role": "default",
        "limits": { "max_steps": max_steps },
        "backends": {
            "mock": { "kind": "openai-compat", "base_url": mock.base_url, "dialect": "openai" }
        },
        "roles": {
            "default": { "chain": [format!("mock/{}", mock.model)] },
            "coder": { "chain": [format!("mock/{}", mock.model)] }
        },
        "tools": {
            "builtin_plugins": [
                "conway.fs",
                "conway.subagent",
                "conway.report",
                "conway.shell"
            ]
        }
    });
    let config_path = dir.path().join("conway.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&config).expect("serialize conway.json"),
    )
    .expect("write conway.json");

    let models_dir = dir.path().join(".conway");
    std::fs::create_dir_all(&models_dir).expect("create .conway dir");
    let models_json = serde_json::json!({
        "models": {
            format!("mock/{}", mock.model): {
                "max_context_tokens": 128_000,
                "tool_calling": "streaming_validated",
                "reasoning": false,
                "reliability_tier": "verified",
            }
        }
    });
    std::fs::write(
        models_dir.join("models.json"),
        serde_json::to_vec(&models_json).expect("serialize models.json"),
    )
    .expect("write models.json");

    Fixture { dir, config_path }
}

/// ACCEPTANCE (round 2): a real `SIGTERM` sent to a TUI with a MODEL-ISSUED
/// `bash` tool call in flight -- not an operator-typed `!` command -- kills
/// that call's whole process group too, including a backgrounded
/// grandchild the call's own leader cannot reach, via the SAME
/// `App::purge_open_ask_modal` funnel the `!`-command tests above exercise.
/// This is the scenario `crates/conway-tools/src/shell/bash.rs`'s own run
/// loop never protected on ANY quit path before `shutdown.rs::
/// purge_open_ask_modal`'s round-2 fix (that method's own doc): a model's
/// `bash` child is never `kill_on_drop`'d and has no `Drop` impl of its
/// own, unlike `conway::plugin::ChildSession` (MCP/subprocess-plugin
/// children) -- it is only ever killed by an explicit, awaited
/// `kill_group` call reached through a `CancellationToken` trip, which
/// nothing on any quit path performed until this fix threaded
/// `SessionHandle::abort_turn`, bound-awaited via `awaiting_prompt`/
/// `agent_is_finished` (not `cancel`+`await_agent`, which would end the
/// session with a terminal result rather than leave it resumable -- see
/// `shutdown.rs::abort_in_flight_turns`'s own doc), into the shared
/// funnel.
///
/// **Why Shift-Tab into `AutoAllow` rather than answering a live permission
/// prompt.** Mirrors `tui_permission_mode.rs`'s own established approach
/// exactly (that file's own doc explains the opt-in `conway.shell` fixture
/// requirement) -- a prompt overlay would add an extra interactive step
/// this test does not need to prove its own point, and `tui_permission_
/// mode.rs` already separately proves prompt-mode's own behavior.
///
/// **Why `sleep 999999 & echo $! > <file> ; wait`, not a plain foreground
/// command.** Identical reasoning to `start_bang_and_capture_grandchild_
/// pid` above: a background job never gets its own process group under
/// non-interactive `bash -c`, so the grandchild's pid is genuinely
/// different from the tool call's own leader -- only a real whole-group
/// kill reaches it.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "harness busy-spins under portable-pty; behaviour verified manually in tmux 2026-10-06 -- board item 01M488Q85AE58P0S666NED2R4P"]
async fn sigterm_kills_a_model_issued_bash_calls_whole_process_group_too() {
    let pidfile_dir = tempfile::tempdir().expect("tempdir for the pidfile coordination point");
    let pid_file = pidfile_dir.path().join("model-bash-grandchild.pid");

    // One scripted turn: the model calls `bash` once, with a command that
    // backgrounds a long-lived grandchild, records its pid, then blocks on
    // `wait` -- so the tool call itself never completes on its own, exactly
    // the "in-flight" shape this test needs. No second script entry: the
    // call is killed (never allowed to return a result), so no follow-up
    // request is ever sent.
    let script = Script(vec![vec![
        Chunk::ToolCall {
            name: "bash",
            args: serde_json::json!({
                "command": format!(
                    "sleep 999999 & echo $! > {} ; wait",
                    pid_file.display()
                )
            }),
        },
        Chunk::Finish("tool_calls"),
    ]]);
    let mock = MockBackend::start(script).await;
    let fixture = write_fixture_with_bash(&mock, 10);

    let cmd = common::pty_command(&[], &fixture);
    let mut session = PtySession::spawn(cmd, 160, 45);
    let landed = session.wait_for(LANDED, Duration::from_secs(15));

    // Prompt (the default) -> Plan -> AutoAllow: two Shift-Tabs, mirroring
    // `tui_permission_mode.rs`'s own cycle exactly, including waiting for
    // each intermediate mode to actually land before sending the next
    // keystroke (never two sent back-to-back with no wait between).
    session.send_shift_tab();
    let in_plan = session.wait_for_since("plan", landed, Duration::from_secs(10));
    session.send_shift_tab();
    let in_auto_allow = session.wait_for_since("AUTO-ALLOW", in_plan, Duration::from_secs(10));

    session.send("run the scripted tool call\r");

    let grandchild_pid = wait_for_pid_file(&pid_file, Duration::from_secs(10));
    // The call is genuinely in flight, not already finished -- the status
    // line's own `Activity::RunningTool` indicator (`"running bash…"`,
    // `tui/view/status.rs`) is up.
    session.wait_for_since("running bash", in_auto_allow, Duration::from_secs(10));

    let tui_pid = session.pid();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(tui_pid as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .expect("send SIGTERM to the TUI process");

    // The sharpest possible assertion, exactly like the `!`-command tests
    // above: the OS-level process is actually gone.
    wait_for_process_death(grandchild_pid, Duration::from_secs(10));

    let status = session.wait_for_exit(Duration::from_secs(10));
    assert_eq!(
        status.exit_code(),
        143,
        "documented SIGTERM exit code; screen:\n{}",
        session.screen()
    );
}
