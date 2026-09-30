//! Slice 2's automatic trigger (board item `01M3DTT078W25MD2S4527R0WAV`), in
//! its ONE reachable dispatch target -- a real one-shot `conway -p` run (see
//! `oneshot.rs::run`'s own doc for why a `keep_alive` TUI root cannot use
//! this path at all). Written the same way `confine_cli.rs`/`first_party_
//! plugins.rs` drive a first-party plugin end to end: real compiled binary,
//! real mock OpenAI-compatible server, `--allowed-tools` naming the one call
//! so it never needs a live permission prompt.
//!
//! **Stdin is always `Stdio::null()`** (`common::command`'s own fixed
//! wiring) -- deterministically NOT a terminal, so every test here exercises
//! the "no unattended writes" gate (`oneshot.rs::maybe_propose_skill`'s own
//! doc, gate 3) rather than needing a pty to script a `y`/`N` decision. The
//! interactive write-on-`y` path is covered end to end against a real,
//! scripted `Conway` in `crates/conway-cli/src/tui/app/skill_propose.rs`'s
//! own test module (the primary, on-demand surface); this file's job is
//! narrower and different: proving the AUTOMATIC trigger genuinely reaches
//! a real one-shot run at all, and that it never writes unattended.

mod common;

use std::time::Duration;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::pty::PtySession;
use common::{pty_command, run_conway, write_fixture, Fixture};

/// Installs `conway.skills` and sets a low `tool_call_threshold` (so a
/// two-tool-call fixture reliably crosses it, without needing eight calls
/// just to exercise the mechanism) -- mirrors `confine_cli.rs`'s own
/// `add_plugins_install`, widened to also set `[plugins.config]`.
fn install_skills_with_threshold(fixture: &Fixture, threshold: u32) {
    let raw = std::fs::read_to_string(&fixture.config_path).expect("read rendered conway.json");
    let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse conway.json");
    value["plugins"] = serde_json::json!({
        "install": ["conway.skills"],
        "config": {
            "conway.skills": { "tool_call_threshold": threshold },
        },
    });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize conway.json"),
    )
    .expect("rewrite conway.json with [plugins].install/config");
}

/// A scripted root turn making `calls` successive `bash` tool calls (each
/// its own request/response round, `finish_reason: "tool_calls"`), then a
/// final plain text reply (`finish_reason: "stop"`) that ends the turn --
/// `conway.shell`'s `bash` is registered unconditionally for one-shot
/// dispatch (`confine_cli.rs`'s own doc), so no extra plugin wiring is
/// needed to make it callable.
fn script_n_tool_calls(calls: u32) -> Script {
    let mut rounds = Vec::new();
    for _ in 0..calls {
        rounds.push(vec![
            Chunk::ToolCall {
                name: "bash",
                args: serde_json::json!({ "command": "echo hi" }),
            },
            Chunk::Finish("tool_calls"),
        ]);
    }
    rounds.push(vec![Chunk::Text("done"), Chunk::Finish("stop")]);
    Script(rounds)
}

/// The `.conway/skills` directory this run's fixture would write into, were
/// anything ever written.
fn skills_dir(fixture: &Fixture) -> std::path::PathBuf {
    fixture.dir.path().join(".conway").join("skills")
}

/// **The primary test.** A tool-call-heavy one-shot run crosses
/// `tool_call_threshold`, the automatic trigger fires, and -- because stdin
/// is not a terminal (this harness's own fixed wiring) -- it refuses to
/// propose anything unattended: a named notice on stderr, and NO file ever
/// written, not even the directory created.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_heavy_one_shot_run_triggers_but_refuses_to_propose_without_a_terminal() {
    let mock = MockBackend::start(script_n_tool_calls(2)).await;
    let fixture = write_fixture(&mock, 10);
    install_skills_with_threshold(&fixture, 2);

    let out = run_conway(
        &[
            "-p",
            "do a small task",
            "--allowed-tools",
            "bash",
            "--output-format",
            "jsonl",
        ],
        &fixture,
    );

    assert!(
        out.status.success(),
        "the run itself must complete normally regardless of the trigger; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conway.skills:") && stderr.contains("re-run interactively"),
        "the automatic trigger must fire and refuse without a terminal, naming why: {stderr}"
    );
    assert!(
        !skills_dir(&fixture).exists(),
        "no unattended write may ever happen -- not even the directory: {}",
        skills_dir(&fixture).display()
    );
}

/// Below the threshold, and no other mechanical rule fires: the trigger
/// evaluates (slice 1's own machinery runs regardless) but publishes no
/// evidence worth proposing, so NOTHING reaches stderr about it at all --
/// distinguishing "evaluated, decided against" from "never evaluated" the
/// positive test above already covers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_small_task_below_threshold_never_mentions_a_proposal() {
    let mock = MockBackend::start(script_n_tool_calls(1)).await;
    let fixture = write_fixture(&mock, 10);
    install_skills_with_threshold(&fixture, 8);

    let out = run_conway(
        &[
            "-p",
            "do a tiny task",
            "--allowed-tools",
            "bash",
            "--output-format",
            "jsonl",
        ],
        &fixture,
    );

    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("conway.skills:"),
        "a task that never crosses any mechanical rule must never mention a proposal: {stderr}"
    );
    assert!(!skills_dir(&fixture).exists());
}

/// `write_approval = never` suppresses the trigger BEFORE the terminal
/// check even runs -- distinct code paths, both must independently refuse.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_approval_never_suppresses_the_automatic_trigger_too() {
    let mock = MockBackend::start(script_n_tool_calls(2)).await;
    let fixture = write_fixture(&mock, 10);
    let raw = std::fs::read_to_string(&fixture.config_path).expect("read rendered conway.json");
    let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse conway.json");
    value["plugins"] = serde_json::json!({
        "install": ["conway.skills"],
        "config": {
            "conway.skills": { "tool_call_threshold": 2, "write_approval": "never" },
        },
    });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize conway.json"),
    )
    .expect("rewrite conway.json");

    let out = run_conway(
        &[
            "-p",
            "do a small task",
            "--allowed-tools",
            "bash",
            "--output-format",
            "jsonl",
        ],
        &fixture,
    );

    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("conway.skills:"),
        "write_approval=never must suppress the automatic trigger with no trace on stderr: \
         {stderr}"
    );
    assert!(!skills_dir(&fixture).exists());
}

// ---------------------------------------------------------------------
// SIGINT during the reflection phase (review round 1, board item
// `01M3DTT078W25MD2S4527R0WAV`)
// ---------------------------------------------------------------------
//
// Every test above uses `common::command`'s fixed `Stdio::null()` stdin --
// deterministically NOT a terminal, so `maybe_propose_skill`'s own
// `is_terminal()` gate refuses before ever forking. Reaching the phase this
// section tests (the forked child's own turn, and the interactive `y`/`N`
// read) needs a REAL terminal on stdin, which only `common::pty::PtySession`
// gives a one-shot process -- see that module's own top doc.

#[cfg(unix)]
fn send_sigint(pid: u32) {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), Signal::SIGINT).expect("send SIGINT");
}

/// **The primary signal-handling test.** The root turn crosses the
/// threshold normally; the ephemeral proposal child's OWN request is
/// scripted to `Chunk::Hang` (never answers), so `handle.await_agent(child)`
/// inside `maybe_propose_skill` is reliably still in flight when SIGINT
/// arrives. Before the fix (round 1 of review), this would have run for the
/// full 180s deadline with the installed signal handler swallowing the
/// first Ctrl-C; asserts the process exits promptly instead, with the
/// documented `Interrupted` (130) code (the root's own result was already a
/// SUCCESS -- this is NOT `from_result_with_sigint`'s pre-existing
/// `Cancelled`-status override, see `oneshot.rs::run`'s own doc on exactly
/// why this needed a second, direct path), and that no file is ever written
/// and no ephemeral child is left registered.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigint_during_the_reflection_phase_exits_promptly_and_writes_nothing() {
    let mut rounds = script_n_tool_calls(2).0;
    // The propose child's own request: never answers.
    rounds.push(vec![Chunk::Hang]);
    let mock = MockBackend::start(Script(rounds)).await;
    let fixture = write_fixture(&mock, 10);
    install_skills_with_threshold(&fixture, 2);

    let cmd = pty_command(
        &[
            "-p",
            "do a small task",
            "--allowed-tools",
            "bash",
            "--output-format",
            "jsonl",
        ],
        &fixture,
    );
    let mut session = PtySession::spawn(cmd, 80, 24);

    // Printed right before the ephemeral child is forked -- by the time
    // this is on screen, `handle.await_agent(child)` is either about to
    // start or already in flight against the `Chunk::Hang`'d request.
    session.wait_for(
        "conway.skills: reflecting on this task for a possible skill proposal",
        Duration::from_secs(15),
    );

    send_sigint(session.pid());

    // Generous next to product reality (`CLEANUP_BOUND` inside
    // `cleanup_interrupted_child` is 5s), but still an order of magnitude
    // below the 180s deadline this fix eliminates waiting for -- the
    // comparison this bound is meant to prove, not "as fast as possible".
    let status = session.wait_for_exit(Duration::from_secs(20));
    assert_eq!(
        status.exit_code(),
        130,
        "expected Interrupted (130); screen so far:\n{}",
        session.screen()
    );

    assert!(
        !skills_dir(&fixture).exists(),
        "no file may ever be written when interrupted mid-reflection"
    );

    // LOAD-BEARING (P-15): no ephemeral child left registered -- not merely
    // "no file appeared". A fresh `Conway`, opened AFTER the subprocess has
    // exited, over the SAME on-disk session store that run used: the root
    // session itself is expected (one non-ephemeral entry), but no
    // EPHEMERAL session -- the proposal child -- may still be sitting there
    // un-purged.
    let conway = common::open_conway(&fixture).await;
    let sessions = conway
        .sessions(conway::SessionFilter {
            include_ephemeral: true,
            ..Default::default()
        })
        .await
        .expect("sessions() should succeed");
    let ephemeral_left: Vec<_> = sessions.iter().filter(|m| m.ephemeral).collect();
    assert!(
        ephemeral_left.is_empty(),
        "no ephemeral proposal child may be left registered after a signal-interrupted \
         reflection: {ephemeral_left:?} (all sessions: {sessions:?})"
    );
}
