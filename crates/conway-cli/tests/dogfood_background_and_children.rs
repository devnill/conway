//! Dogfood gates 3-4 (board item `01M2MEZMH2KJ79XSADQWCBFVFH`), automating
//! the machine-assertable half of two gates that were operator-only only
//! because nobody had driven a real child process and a real OS signal
//! against the compiled `conway` binary end to end. Both gates' own
//! underlying features are already shipped -- `crates/conway-tools/tests/
//! shell_bash.rs` already pins the `BashTool` unit's own backgrounding
//! contract directly (`backgrounded_child_does_not_hold_the_call_to_its_
//! timeout`, `timeout_kills_the_process_group_and_reports_is_error`), and
//! `crates/conway-cli/src/signal.rs` + `docs/agents.md`'s "What a parent
//! sees when a child dies" section already document the SIGTERM/SIGHUP
//! handling this file drives end to end. What was missing, and what this
//! file adds, is coverage through the REAL agent/tool-call/permission
//! stack and a REAL external OS signal -- neither of which a crate-level
//! unit test calling `BashTool::execute` directly can exercise.
//!
//! ## Gate 3: backgrounding (board item `01M1ZJQEZGWFGND6M51EDX8N4F`)
//!
//! Uses a plain `sleep`, not a nested `conway` invocation -- gate 3's own
//! bullets describe generic backgrounding, and the "reproduce the
//! original shape... `conway -p ... &`" instruction is gate 4's own
//! closing bullet, not gate 3's (re-read against the board item text:
//! that sentence appears only under "GATE 4", immediately after "No
//! session log ends on a tool result with no terminal record").
//!
//! ## Gate 4: a killed child leaves a body (board item
//! `01M1ZJTBK4WM5MFMWXWJZXWF2K`)
//!
//! Reproduces the ORIGINAL shape specifically, per that gate's own closing
//! instruction: a real second `conway -p ... --allowed-tools bash`
//! process, launched backgrounded (`&`) from inside a `bash` tool call of
//! a FIRST, outer `conway` session -- the exact 2026-09-07 incident shape
//! `docs/agents.md`'s "layer 3" describes. The SIGKILL bullet is
//! deliberately NOT automated here; see
//! [`sigterm_mid_work_child_persists_a_cancelled_terminal_record`]'s own
//! doc for the SIGTERM half, and this module's closing doc comment for
//! why the SIGKILL half is reported rather than guessed at.

// No `#[allow(dead_code)]` here: `common/child_procs.rs` carries its own
// file-level `#![allow(dead_code)]`, which covers every consumer of the
// module rather than just this one. Declaring both is a `duplicated_attribute`
// error under `-D warnings`.
#[path = "common/child_procs.rs"]
mod child_procs;
#[allow(dead_code)]
mod common;

use std::time::{Duration, Instant};

use common::mock_backend::{Chunk, MockBackend, Script};
use conway::{LogRecord, ResultStatus, SessionFilter};

/// A one-entry `Script` whose only turn proposes a `bash` call, then ends
/// the turn on `tool_calls` -- every test in this file's own shape. A
/// request past the script's own length gets a graceful default empty
/// `Finish("stop")` (`common::mock_backend::Script`'s own doc), which is
/// exactly the harmless follow-up turn every test here relies on after
/// its one scripted tool call.
fn one_bash_call(command: serde_json::Value) -> Script {
    Script(vec![vec![
        Chunk::ToolCall {
            name: "bash",
            args: command,
        },
        Chunk::Finish("tool_calls"),
    ]])
}

/// The text of the first (and, in every test in this file, only)
/// `tool_result` record in `records`.
fn first_tool_result_text(records: &[LogRecord]) -> (String, bool) {
    records
        .iter()
        .find_map(|record| match record {
            LogRecord::ToolResultRecord { result, .. } => {
                let text = result
                    .blocks
                    .iter()
                    .find_map(|b| match b {
                        conway::plugin::ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                Some((text, result.is_error))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a tool_result record in the transcript: {records:?}"))
}

// ---------------------------------------------------------------------
// Gate 3: backgrounding
// ---------------------------------------------------------------------

/// "`sleep 30 &` through the `bash` tool returns in well under a second.
/// Assert the wall clock." + "the pid the result names is still alive
/// afterwards." + "output from the backgrounded job does not appear in
/// the result, and the result SAYS SO."
///
/// `(sleep 2 && echo LATE_MARKER) &` rather than a bare `sleep 30 &`: the
/// backgrounded subshell's own `echo` genuinely CANNOT have run by the
/// time this call returns (the whole round trip, proven below, completes
/// in well under the 2 real seconds `sleep 2` needs), so the "output does
/// not appear" assertion is a deterministic fact of timing, not a race
/// that merely usually wins.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backgrounded_job_returns_fast_leaves_output_uncaptured_and_leaves_the_pid_alive() {
    let mock = MockBackend::start(one_bash_call(serde_json::json!({
        "command": "(sleep 2 && echo LATE_MARKER) & echo $!"
    })))
    .await;
    let fixture = common::write_fixture(&mock, 5);

    let started = Instant::now();
    let out = common::run_conway(
        &["-p", "background a job", "--allowed-tools", "bash"],
        &fixture,
    );
    let elapsed = started.elapsed();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the whole `conway -p` round trip (one mock-backend turn, one bash call that only \
         backgrounds a job and echoes its pid) must return in a small, fixed amount of time, \
         nowhere near the 2 real seconds the backgrounded job itself needs to produce output: \
         took {elapsed:?}"
    );

    let conway = common::open_conway(&fixture).await;
    let sessions = conway
        .sessions(SessionFilter::default())
        .await
        .expect("list sessions");
    assert_eq!(sessions.len(), 1);
    let handle = conway.resume(sessions[0].id).await.expect("resume session");
    let root = handle.root();
    let records = handle.transcript(root).await.expect("read transcript");
    let (text, is_error) = first_tool_result_text(&records);
    assert!(
        !is_error,
        "a successfully backgrounded job must not be an error result: {text:?}"
    );

    let pid = child_procs::pid_from_bash_stdout(&text);
    let _guard = child_procs::KillOnDrop(pid);

    assert!(
        !text.contains("LATE_MARKER"),
        "output the backgrounded job produces only after this call returns must never appear \
         in the result: {text:?}"
    );
    assert!(
        text.contains(&format!("background process(es) still running: pid {pid}")),
        "the result must SAY that a background job is still running, naming its pid, rather \
         than silently having nothing to say about it: {text:?}"
    );
    assert!(
        child_procs::is_alive(pid),
        "the backgrounded job's own pid must still be alive immediately after the call \
         returns -- it was never waited on, let alone killed"
    );
}

/// "The result never reports BOTH an exit code AND a timeout." The
/// success-path half of that mutual exclusion is proven in
/// `conway-tools`'s own `shell_bash.rs`
/// (`backgrounded_child_does_not_hold_the_call_to_its_timeout` asserts
/// `exit code: 0` AND no `timed out` text) -- NOT by the compiled-binary
/// test above, which checks the timing, the pid, and the still-running
/// notice, but never the exit-code text. Said precisely because an earlier
/// version of this comment claimed the test above proved it, which sent a
/// coverage audit looking for an assertion that was never there. This
/// checks the SAME mutual-exclusion holds
/// on the timeout path too, by inspecting the timed-out result's own text
/// directly (`bash.rs::finish`'s own `unreachable!` guards this
/// structurally, but this is the black-box, compiled-binary proof of the
/// observable it protects).
///
/// "NEGATIVE CASE: a foreground `sleep 30` with a short tool timeout is
/// still killed and still reports ONLY `timed out`, with no exit code
/// beside it." The model-supplied `timeout_ms: 500` argument (`BashArgs`'s
/// own field) is what lets this stay fast and deterministic without a
/// fixed `sleep` anywhere in this test itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreground_sleep_past_a_short_timeout_is_killed_and_reports_only_timed_out() {
    let mock = MockBackend::start(one_bash_call(serde_json::json!({
        "command": "sleep 30",
        "timeout_ms": 500
    })))
    .await;
    let fixture = common::write_fixture(&mock, 5);

    let out = common::run_conway(
        &["-p", "run something slow", "--allowed-tools", "bash"],
        &fixture,
    );
    assert!(
        out.status.success(),
        "the RUN itself still completes (the tool call fails, the agent still finishes its \
         turn): stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let conway = common::open_conway(&fixture).await;
    let sessions = conway
        .sessions(SessionFilter::default())
        .await
        .expect("list sessions");
    let handle = conway.resume(sessions[0].id).await.expect("resume session");
    let root = handle.root();
    let records = handle.transcript(root).await.expect("read transcript");
    let (text, is_error) = first_tool_result_text(&records);

    assert!(
        is_error,
        "a killed-by-timeout foreground call must be an error result: {text:?}"
    );
    assert!(text.contains("timed out after 500ms"), "{text:?}");
    assert!(
        !text.contains("exit code:"),
        "a timed-out call must never ALSO report an exit code -- that contradiction (\"exit \
         code: 0\" beside \"timed out after ...ms\") was the original defect this gate pins \
         against a recurrence: {text:?}"
    );
}

// ---------------------------------------------------------------------
// Gate 4: a killed child leaves a body
// ---------------------------------------------------------------------

/// Reproduces the 2026-09-07 incident shape `docs/agents.md`'s "What a
/// parent sees when a child dies" section describes as "layer 3": a real,
/// second `conway -p ... --allowed-tools bash` OS process, launched
/// backgrounded from inside a FIRST (outer) session's own `bash` tool
/// call. The inner process is mid-work (executing its own scripted `bash`
/// `sleep 30` tool call) when this test sends it a real `SIGTERM` from
/// outside -- not through any conway-internal cancellation path.
///
/// Proves, by reading the inner process's OWN on-disk session log after
/// it has actually exited (`child_procs::wait_until_dead`, no fixed
/// sleep):
/// - the LAST record is `agent_result { status: Cancelled { reason:
///   "signal: SIGTERM" } }` -- gate 4's own "the last record in its
///   session log is `agent_result { status: cancelled, reason: signal }`,
///   not an edit result" bullet, and
/// - that same fact proves "no session log ends on a tool result with no
///   terminal record" too: the transcript's last record is structurally
///   an `agent_result`, never a dangling `tool_result`.
///
/// `crates/conway-cli/src/signal.rs`'s own module doc and `docs/
/// agents.md`'s "layer 2" section are what make `conway -p`'s SIGTERM
/// handling -- not this test -- responsible for that guarantee; this test
/// exists to prove the wiring actually reaches a process launched exactly
/// the way the original incident's processes were.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sigterm_mid_work_child_persists_a_cancelled_terminal_record() {
    // The inner (to-be-backgrounded) fixture: its own mock backend and its
    // own conway.json, entirely separate from the outer session's -- two
    // independent request streams that must never interleave through one
    // shared mock.
    let inner_mock = MockBackend::start(one_bash_call(serde_json::json!({
        "command": "sleep 30"
    })))
    .await;
    let inner_fixture = common::write_fixture(&inner_mock, 5);

    let conway_bin = assert_cmd::cargo::cargo_bin("conway");
    let inner_log = inner_fixture.dir.path().join("inner.log");
    // `CONWAY_CONFIG_DIR` AND the process `cwd` (via `BashArgs::cwd`, not
    // a shell `cd` -- a `cd X && CMD &` would background a SUBSHELL
    // running the whole list, so `$!` would name the subshell, not the
    // `conway` process itself) are BOTH pointed at the inner fixture's own
    // temp dir, matching exactly what `common::open_conway`/`session_dir`
    // assume when reading it back below (`common/mod.rs`'s own doc: the
    // project key is derived from `cwd`, and the central session root
    // from `CONWAY_CONFIG_DIR` -- both must match the SAME dir the inner
    // process actually used, or this test would silently read an empty,
    // unrelated store).
    let outer_command = format!(
        "CONWAY_CONFIG_DIR='{cfgdir}' CONWAY_LOCAL_PROBE_BASE_URL='http://127.0.0.1:1/v1' \
         '{bin}' --config '{cfg}' -p 'go slow' --allowed-tools bash > '{log}' 2>&1 & echo $!",
        cfgdir = inner_fixture.dir.path().display(),
        bin = conway_bin.display(),
        cfg = inner_fixture.config_path.display(),
        log = inner_log.display(),
    );

    let outer_mock = MockBackend::start(one_bash_call(serde_json::json!({
        "command": outer_command,
        "cwd": inner_fixture.dir.path().to_string_lossy().into_owned(),
    })))
    .await;
    let outer_fixture = common::write_fixture(&outer_mock, 5);

    let out = common::run_conway(
        &["-p", "launch a slow child", "--allowed-tools", "bash"],
        &outer_fixture,
    );
    assert!(
        out.status.success(),
        "the OUTER run (which only backgrounds the inner process and echoes its pid) must \
         succeed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let outer_conway = common::open_conway(&outer_fixture).await;
    let outer_sessions = outer_conway
        .sessions(SessionFilter::default())
        .await
        .expect("list outer sessions");
    let outer_handle = outer_conway
        .resume(outer_sessions[0].id)
        .await
        .expect("resume outer");
    let outer_root = outer_handle.root();
    let outer_records = outer_handle
        .transcript(outer_root)
        .await
        .expect("outer transcript");
    let (outer_tool_text, outer_is_error) = first_tool_result_text(&outer_records);
    assert!(
        !outer_is_error,
        "backgrounding the inner process must not itself be an error: {outer_tool_text:?}"
    );

    let inner_pid = child_procs::pid_from_bash_stdout(&outer_tool_text);
    let _guard = child_procs::KillOnDrop(inner_pid);

    // Wait for evidence the inner process has actually started its own
    // turn (reached its mock backend) before signalling it -- "mid-work",
    // not "killed before it even began". A pattern-with-timeout wait, not
    // a fixed sleep.
    child_procs::wait_until(
        || !inner_mock.requests().is_empty(),
        Duration::from_secs(20),
        "the inner `conway -p` process's first request to its own mock backend",
    );

    child_procs::send_signal(inner_pid, "TERM");
    // `crates/conway-cli/src/oneshot.rs`'s own grace window is 5s
    // (`grace_deadline = ... + Duration::from_secs(5)`); this margin
    // covers that plus real process-exit teardown time.
    child_procs::wait_until_dead(inner_pid, Duration::from_secs(10));

    let inner_conway = common::open_conway(&inner_fixture).await;
    let inner_sessions = inner_conway
        .sessions(SessionFilter::default())
        .await
        .expect("list inner sessions");
    assert_eq!(
        inner_sessions.len(),
        1,
        "expected exactly one session in the inner fixture's own store: {inner_sessions:?}"
    );
    let inner_handle = inner_conway
        .resume(inner_sessions[0].id)
        .await
        .expect("resume inner");
    let inner_root = inner_handle.root();
    let inner_records = inner_handle
        .transcript(inner_root)
        .await
        .expect("inner transcript");
    let last = inner_records
        .last()
        .unwrap_or_else(|| panic!("the inner session's transcript must not be empty"));

    match last {
        LogRecord::AgentResultRecord { result, .. } => {
            assert_eq!(
                result.status,
                ResultStatus::Cancelled {
                    reason: "signal: SIGTERM".to_string()
                },
                "the last record must be a Cancelled result naming the SIGTERM signal, not any \
                 other terminal status: {result:?}"
            );
        }
        other => panic!(
            "the last record in a SIGTERM'd child's session log must be an agent_result, never \
             a dangling tool_result or anything else: {other:?}"
        ),
    }
}

// ---------------------------------------------------------------------
// NOT automated, and why (gate 4's SIGKILL bullet)
// ---------------------------------------------------------------------
//
// "SIGKILL a child: the parent synthesizes `failed` naming the exit
// status, never a bare 'tool call failed'." This is deliberately NOT
// automated in this file. Reasoning, so this is a disclosed omission
// rather than a silently dropped assertion:
//
// - `SIGKILL` is uncatchable, by construction, for the process it kills --
//   `crates/conway-cli/src/signal.rs`'s own module doc states this
//   plainly, and `docs/agents.md`'s "layer 3" section is explicit that a
//   raw, shelled-out `conway -p ... &` child (exactly this file's own
//   shape, and gate 4's own named reproduction) gets NO durability
//   guarantee under `SIGKILL`: nothing writes a terminal record to ITS
//   OWN session log, because nothing in it ever runs again after the
//   signal lands. Confirmed by reading, not assumed.
// - The one place `docs/agents.md` names a "the parent synthesizes a
//   result" guarantee for a killed child is "layer 1": an IN-PROCESS
//   subagent (`conway_fork`/`conway_spawn`, awaited via `conway_await` or
//   `crates/conway-runtime/src/supervisor.rs`'s own panic/grace-timeout
//   synthesis) -- a fundamentally different shape from a raw OS process
//   shelled out via `bash`. Reproducing THAT would mean abandoning gate
//   4's own "reproduce the original shape specifically... `conway -p ...
//   &` from inside a `bash` tool call" instruction, not honoring it.
// - The board's own coordination note for this item states a sibling
//   agent is CURRENTLY fixing "exactly that class of bug" in
//   `crates/conway-tools` (child-process death/signal reporting) and asks
//   this wave not to create a second instance of it. Gate 4's SIGKILL
//   bullet sits squarely in that territory. Writing a test against an
//   ambiguous, possibly-nonexistent mechanism in code under concurrent,
//   unrelated repair risks exactly that duplication, for an assertion
//   this file could not otherwise pin down with the same confidence the
//   SIGTERM test above has.
//
// If this bullet describes something concrete and reachable that this
// reasoning missed, it needs to be named explicitly (which mechanism,
// which "parent", which vantage point observes the failure) before it can
// be automated safely.
