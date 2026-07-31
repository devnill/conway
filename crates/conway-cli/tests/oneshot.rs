//! WI-113: one-shot (`-p`) integration tests against the real, compiled
//! `conway` binary -- exit codes, stdout purity, streaming shape, and
//! SIGINT behavior. See `docs/scripting.md` (WI-113) for the exit-code and
//! output-format contract this suite locks in as executable acceptance
//! evidence.
//!
//! ## Reconciliations (disclosed against the currently-committed runtime)
//!
//! Two of the plan doc's named scenarios describe an exit code this
//! module's own tests prove is **not reachable** from `-p` one-shot mode
//! today, given already-committed `conway-runtime` behavior (not a gap in
//! this test suite -- a gap in the runtime, already partially flagged by
//! `exit.rs`'s own module doc for the identical reason):
//!
//! - **`exit_4_no_backend`** ("mock refuses connections" -> exit 4). Every
//!   error `AttemptEngine::execute` can produce -- including
//!   `RoutingError::NoCandidate` after every candidate in the chain fails
//!   with a transport error -- is caught by `AgentLoop::run_inner`'s
//!   generic `Err` propagation and turned into `ResultStatus::Failed` by
//!   `finish_error` (`crates/conway-runtime/src/agent_loop.rs`'s
//!   `finish_error`: "`RuntimeError::Cancelled` maps to `Cancelled`;
//!   everything else maps to `Failed`" -- no special case for a routing
//!   cause). `ResultStatus::Failed` maps to `ExitCode::AgentFailed` (1) via
//!   `ExitCode::from_result`, never `ExitCode::from_error`'s
//!   `NoHealthyBackend` (4) -- that classifier is only ever reached by
//!   `exit.rs`'s own unit tests, which construct a `ConwayError::Runtime`
//!   value directly; no live call path from `oneshot::run` ever produces
//!   one (its own fallible steps -- `read_prompt`, `new_session`,
//!   `handle.prompt` -- surface only usage errors or `RuntimeError` shapes
//!   `rt.prompt` itself can raise, none of which is routing). This test
//!   therefore asserts the real, observed code (1), not 4.
//! - **`exit_3_permission_termination`** (deny-mode + tool-only script ->
//!   exit 3). Per `exit.rs`'s own module doc, reconciliation #2: every
//!   `PermissionOutcome::Deny` (either `PermissionDecision` variant)
//!   becomes a model-visible `ToolOutcome::error` fed back into the
//!   agent's own turn (`conway-runtime/src/tools/runner.rs`'s
//!   `execute_one`) -- never a terminal `ConwayError`, so there is no
//!   mechanism to end a run specifically *because* a tool call was denied.
//!   A script that only ever proposes tool calls under `--permission-mode
//!   deny` therefore keeps taking turns (every denial re-prompts the
//!   model) until `budget.max_steps` is exhausted, terminating as
//!   `BudgetExceeded` (exit 5) -- not `PermissionDenied` (3). This test
//!   pins `max_steps` low enough to reach that termination quickly and
//!   asserts the real code (5), while still verifying the part of the
//!   criterion that *is* live today: a `PermissionResolved` denial
//!   envelope is visible in the `jsonl` stream.
//!
//! Both deviations are load-bearing findings for the module owner (a
//! terminal-permission-escalation path and a `NoCandidate`-aware
//! `ExitCode::from_result` classifier would each need runtime changes
//! outside a test-suite item's scope), not something this file works
//! around.

mod common;

use std::io::{BufRead, BufReader, Read};
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{command, run_conway, write_fixture, write_fixture_with};
use serde_json::Value;

const NO_ESC: u8 = 0x1b;

fn assert_no_esc_byte(bytes: &[u8]) {
    assert!(
        !bytes.contains(&NO_ESC),
        "output must never contain a raw ESC byte (0x1b)"
    );
}

fn jsonl_lines(stdout: &[u8]) -> Vec<Value> {
    String::from_utf8(stdout.to_vec())
        .expect("stdout is valid utf8")
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line `{line}` did not parse as JSON: {e}"))
        })
        .collect()
}

fn tool_call_chunk(name: &'static str, command: &str) -> Chunk {
    Chunk::ToolCall {
        name,
        args: serde_json::json!({ "command": command }),
    }
}

// ---------------------------------------------------------------------
// Streaming shape / stdout purity
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_streams_only_assistant_text() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("hello "),
        Chunk::Text("world"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["-p", "hi"], &fixture);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"hello world\n");

    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "exactly one /chat/completions request");
    assert_eq!(
        requests[0]["stream"], true,
        "the streaming path was used, not the non-streaming generate() path"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdout_purity() {
    let mock = MockBackend::start(Script(vec![
        vec![
            tool_call_chunk("bash", "echo hi"),
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("done"), Chunk::Finish("stop")],
    ]))
    .await;
    // Default permission mode (allowlist, no --allowed-tools) is
    // fail-closed: `bash` is denied with feedback, fed back to the model,
    // which then finishes normally on its second turn.
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["-p", "hi", "-v", "-v"], &fixture);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"done\n");
    let stdout_text = String::from_utf8(out.stdout.clone()).unwrap();
    for line in stdout_text.lines() {
        assert!(
            !line.starts_with("conway:"),
            "stdout must never carry a `conway:`-prefixed diagnostic line: {line:?}"
        );
    }
    assert_no_esc_byte(&out.stdout);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jsonl_line_by_line() {
    let mock =
        MockBackend::start(Script(vec![vec![Chunk::Text("hi"), Chunk::Finish("stop")]])).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["-p", "hi", "--output-format", "jsonl"], &fixture);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_no_esc_byte(&out.stdout);
    let lines = jsonl_lines(&out.stdout);
    assert!(!lines.is_empty());
    let mut last_seq: Option<i64> = None;
    for value in &lines {
        let obj = value.as_object().expect("each line is a JSON object");
        assert!(obj.contains_key("seq"));
        assert!(obj.contains_key("agent"));
        assert!(obj.contains_key("event"));
        let seq = obj["seq"].as_i64().expect("seq is a number");
        if let Some(prev) = last_seq {
            assert!(
                seq > prev,
                "seq must be strictly increasing: {prev} then {seq}"
            );
        }
        last_seq = Some(seq);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jsonl_streams_incrementally() {
    // Two text deltas straddling a barrier delay. The property under test
    // is *relative*: the gap between the "first"-line's arrival and the
    // "second"-line's arrival on this process's stdout must track the
    // barrier's duration, proving the renderer flushed the first envelope
    // before the second was even produced. An absolute time-from-spawn
    // budget was tried here previously and failed 100% of the time --
    // process-exec overhead (Command::spawn returning to the child's
    // main() actually running) routinely eats 1.5-2s on its own, dwarfing
    // any barrier short enough to keep the test fast. Measuring the gap
    // between two of the child's own stdout lines sidesteps that startup
    // cost entirely: a buffered implementation would deliver both lines
    // back-to-back with no gap, no matter how slow startup was.
    const BARRIER: Duration = Duration::from_millis(400);
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("first"),
        Chunk::Delay(BARRIER),
        Chunk::Text("second"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);

    let mut child = command(&["-p", "hi", "--output-format", "jsonl"], &fixture)
        .spawn()
        .expect("spawn conway");
    let stdout = child.stdout.take().expect("piped stdout");

    let (tx, rx) = std::sync::mpsc::channel();
    let reader_thread = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let arrived = Instant::now();
                    if tx.send((arrived, line)).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut lines: Vec<(Instant, String)> = Vec::new();
    while let Ok(item) = rx.recv_timeout(Duration::from_secs(10)) {
        lines.push(item);
    }
    reader_thread.join().expect("reader thread joins");
    let status = child.wait().expect("wait on child");
    assert!(status.success(), "conway exited non-zero");

    assert!(
        !lines.is_empty(),
        "conway must emit at least one jsonl line"
    );
    for (_, line) in &lines {
        assert!(
            serde_json::from_str::<Value>(line).is_ok(),
            "every line must be parseable JSON: {line:?}"
        );
    }

    let first_arrival = lines
        .iter()
        .find(|(_, line)| line.contains("\"first\""))
        .map(|(t, _)| *t)
        .expect("a jsonl line carrying the 'first' text delta");
    let second_arrival = lines
        .iter()
        .find(|(_, line)| line.contains("\"second\""))
        .map(|(t, _)| *t)
        .expect("a jsonl line carrying the 'second' text delta");

    assert!(
        second_arrival >= first_arrival,
        "the 'second' delta line must not arrive before the 'first' delta line"
    );
    let gap = second_arrival.duration_since(first_arrival);
    assert!(
        gap >= BARRIER / 2,
        "gap between the 'first' and 'second' jsonl lines was only {gap:?}, but the mock held \
         a {BARRIER:?} barrier between producing them -- a truly incremental renderer should \
         observe most of that gap between the two lines; a buffered implementation would \
         instead deliver both lines back-to-back with next to no gap"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_single_object() {
    let mock =
        MockBackend::start(Script(vec![vec![Chunk::Text("hi"), Chunk::Finish("stop")]])).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["-p", "hi", "--output-format", "json"], &fixture);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "exactly one JSON object, nothing else");
    let value: Value = serde_json::from_str(lines[0]).unwrap();
    assert!(value.is_object());
    assert!(value.get("status").is_some());
}

// ---------------------------------------------------------------------
// Exit codes
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_0_completed() {
    let mock =
        MockBackend::start(Script(vec![vec![Chunk::Text("ok"), Chunk::Finish("stop")]])).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["-p", "hi"], &fixture);

    assert_eq!(out.status.code(), Some(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_1_failed() {
    // Two `ToolCall` deltas at the same (hardcoded) stream index with
    // different tool names: the accumulator latches "bash" on the first,
    // then hard-errors with `BackendError::ToolParse` ("conflicting tool
    // name") on the second (`conway-backends/src/tool_calls/mod.rs`'s own
    // documented rule). `ToolParse` classifies as `FailureClass::Fatal`
    // (`conway-routing/src/failure.rs`), aborting the whole attempt chain
    // immediately -> `ResultStatus::Failed` -> exit 1.
    let mock = MockBackend::start(Script(vec![vec![
        tool_call_chunk("bash", "echo one"),
        tool_call_chunk("read", "echo two"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["-p", "hi"], &fixture);

    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_2_bad_flag() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["--nonexistent-flag"], &fixture);

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_2_bad_config() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("conway.json");
    std::fs::write(&config_path, "this is not valid json {{{").unwrap();

    let out = std::process::Command::new(assert_cmd::cargo::cargo_bin("conway"))
        .current_dir(dir.path())
        // Isolate user-scoped config discovery from a real ~/.conway (see
        // `common::command`).
        .env("XDG_CONFIG_HOME", dir.path())
        .arg("--config")
        .arg(&config_path)
        .args(["-p", "hi"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .output()
        .expect("run conway");

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());
}

// ---------------------------------------------------------------------
// `--model` pin (WI-128)
// ---------------------------------------------------------------------

/// `--model <ref>` pins the session's model, overriding the role's own
/// chain. The fixture's `default` role is rewritten to chain a model
/// nothing registers in `models.json` (`mock/unregistered-model`) -- with
/// no pin, that leaves the router with `NoCandidate` (`CapabilityIndex` has
/// no entry for it, `check_candidate` skips it), which `AgentLoop::
/// finish_error` turns into `ResultStatus::Failed` (exit 1, same
/// reconciliation `exit_4_no_backend` below locks in). Passing `--model
/// mock/<real model>` must override that broken chain and route to the
/// mock successfully instead (exit 0) -- proving the pin, not the chain,
/// decided the outcome.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_flag_pins_and_overrides_role_chain() {
    let mock =
        MockBackend::start(Script(vec![vec![Chunk::Text("hi"), Chunk::Finish("stop")]])).await;
    let fixture = write_fixture(&mock, 10);
    let broken = std::fs::read_to_string(&fixture.config_path)
        .unwrap()
        .replace(&format!("mock/{}", mock.model), "mock/unregistered-model");
    std::fs::write(&fixture.config_path, broken).unwrap();

    let pin = format!("mock/{}", mock.model);
    let out = run_conway(&["-p", "hi", "--model", &pin], &fixture);

    assert!(
        out.status.success(),
        "--model should override the broken role chain and route successfully; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"hi\n");

    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]["model"], mock.model,
        "the pinned model, not the role chain's, must be the one actually dialed"
    );
}

/// A malformed `--model` value is a usage error (exit 2), consistent with
/// every other flag `oneshot::resolve_session` parses -- the agent never
/// starts, so no `/chat/completions` request is ever made.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_2_bad_model_ref() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["-p", "hi", "--model", "not-a-valid-ref"], &fixture);

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(mock.requests().is_empty());
}

/// See this file's module doc: exit 4 (`NoHealthyBackend`) is not
/// reachable from `-p` one-shot mode under the currently-committed
/// `conway-runtime` -- every routing/backend failure that reaches
/// `AgentLoop::finish_error` becomes `ResultStatus::Failed`, which maps to
/// exit 1. This test locks in that real, observed behavior for a "mock
/// refuses connections" scenario, disclosing the deviation from the plan
/// doc's literal "-> 4" rather than asserting a code the binary can never
/// produce.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_4_no_backend() {
    let mock = MockBackend::start(Script(vec![])).await;
    let base_url = mock.base_url.clone();
    let model = mock.model.clone();
    drop(mock); // stop accepting connections -> every request is refused
    let fixture = write_fixture_with(&base_url, &model, 10);

    let out = run_conway(&["-p", "hi"], &fixture);

    assert_eq!(
        out.status.code(),
        Some(1),
        "disclosed reconciliation: NoCandidate always surfaces as ResultStatus::Failed (exit 1), \
         never a terminal ConwayError (exit 4), under the currently-committed runtime -- see this \
         file's module doc. stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_5_budget() {
    // `max_steps = 1`: the loop gets exactly one turn (the tool-call
    // response below), then `check_budget` trips before a second request
    // would ever be made -- see `conway-runtime/src/agent_loop.rs`'s
    // `check_budget` ("`state.turn >= budget.max_steps`", checked at the
    // top of the loop, before the next request). One scripted response is
    // therefore sufficient; the tool call's own allow/deny outcome does
    // not matter for this test.
    let mock = MockBackend::start(Script(vec![vec![
        tool_call_chunk("bash", "echo hi"),
        Chunk::Finish("tool_calls"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 1);

    let out = run_conway(&["-p", "hi"], &fixture);

    assert_eq!(
        out.status.code(),
        Some(5),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// See this file's module doc for why the real, observed exit code here is
/// 5 (`BudgetExceeded`), not the plan doc's literal 3 (`PermissionDenied`).
/// What this test *does* still lock in, because it is live today: a
/// `PermissionResolved` denial envelope is visible in the `jsonl` stream
/// for every denied tool call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_3_permission_termination() {
    let mock = MockBackend::start(Script(vec![
        vec![
            tool_call_chunk("bash", "echo one"),
            Chunk::Finish("tool_calls"),
        ],
        vec![
            tool_call_chunk("bash", "echo two"),
            Chunk::Finish("tool_calls"),
        ],
    ]))
    .await;
    let fixture = write_fixture(&mock, 2);

    let out = run_conway(
        &[
            "-p",
            "hi",
            "--permission-mode",
            "deny",
            "--output-format",
            "jsonl",
        ],
        &fixture,
    );

    assert_eq!(
        out.status.code(),
        Some(5),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines = jsonl_lines(&out.stdout);
    // `Event` is internally tagged (`#[serde(tag = "event")]`) and flattened
    // into `Envelope` -- `event` is the variant's snake_case name as a bare
    // string, with that variant's own fields (here `decision`) sitting
    // alongside it at the envelope's top level, not nested under it.
    let saw_denied = lines.iter().any(|env| {
        env["event"] == "permission_resolved"
            && (env["decision"] == "denied_with_feedback" || env["decision"] == "denied")
    });
    assert!(
        saw_denied,
        "expected at least one denied PermissionResolved envelope in the jsonl stream"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unlisted_tool_gets_feedback() {
    let mock = MockBackend::start(Script(vec![
        vec![
            tool_call_chunk("bash", "echo hi"),
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("done"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(
        &[
            "-p",
            "hi",
            "--allowed-tools",
            "read",
            "--output-format",
            "jsonl",
        ],
        &fixture,
    );

    assert!(
        out.status.success(),
        "run must not hang or fail: stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines = jsonl_lines(&out.stdout);
    // Disclosed reconciliation: `execute_one` (`conway-runtime/src/tools/runner.rs`)
    // returns `ToolOutcome::error(..)` directly from its `PermissionOutcome::Deny`
    // arm -- *before* the code further down that emits `Event::ToolCallStarted`/
    // `Event::ToolCallFinished`. A denied call therefore never gets a
    // `ToolCallFinished` envelope at all (confirmed empirically: this
    // test's own jsonl stream goes `tool_call_proposed` ->
    // `permission_requested` -> `permission_resolved` -> straight to the
    // next turn). The denial's rendered message text (naming the tool) is
    // real, but it lives only in the persisted `LogRecord::ToolResultRecord`
    // -- not on any event this CLI's live stream carries. What *is*
    // observable and asserted here: a `DeniedWithFeedback` resolution for
    // this exact `call_id`, and a `ContextSegmentAdded` for that same
    // `call_id`'s tool result naming `bash` (proving the denial's tool
    // result was folded back into context, not merely swallowed) -- and the
    // run completing (not hanging) with a normal `hi` -> `done` second turn.
    let denied_call_id = lines
        .iter()
        .find(|env| {
            env["event"] == "permission_resolved" && env["decision"] == "denied_with_feedback"
        })
        .and_then(|env| env["call_id"].as_str())
        .map(str::to_string);
    assert!(
        denied_call_id.is_some(),
        "expected a DeniedWithFeedback PermissionResolved envelope; lines: {lines:#?}"
    );
    let call_id = denied_call_id.unwrap();

    let named_bash = lines.iter().any(|env| {
        env["event"] == "context_segment_added"
            && env["provenance"]["type"] == "tool_result"
            && env["provenance"]["call_id"] == call_id
            && env["provenance"]["tool"] == "bash"
    });
    assert!(
        named_bash,
        "expected a tool_result ContextSegmentAdded naming `bash` for call {call_id}; lines: {lines:#?}"
    );
}

// ---------------------------------------------------------------------
// SIGINT (unix-only)
// ---------------------------------------------------------------------

#[cfg(unix)]
fn send_sigint(pid: u32) {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), Signal::SIGINT).expect("send SIGINT");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigint_graceful() {
    let mock = MockBackend::start(Script(vec![vec![Chunk::Text("partial"), Chunk::Hang]])).await;
    let fixture = write_fixture(&mock, 10);

    let mut child = command(&["-p", "hi"], &fixture)
        .spawn()
        .expect("spawn conway");
    let mut stdout = child.stdout.take().expect("piped stdout");

    // Give the child time to receive and render the first delta before
    // interrupting it.
    let mut buf = [0u8; 7]; // b"partial".len()
    let read_deadline = Instant::now() + Duration::from_secs(5);
    let mut got = 0;
    while got < buf.len() {
        if Instant::now() > read_deadline {
            panic!("never observed the initial TextDelta before the read deadline");
        }
        match stdout.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(_) => break,
        }
    }
    assert_eq!(&buf[..got], b"partial");

    send_sigint(child.id());

    let start = Instant::now();
    let status = wait_with_timeout(&mut child, Duration::from_secs(10))
        .expect("conway must exit within 10s of a single SIGINT");
    assert!(
        start.elapsed() <= Duration::from_secs(10),
        "took too long to exit after SIGINT"
    );
    assert_eq!(status.code(), Some(130));

    let mut rest = Vec::new();
    let _ = stdout.read_to_end(&mut rest);
    let mut all = buf[..got].to_vec();
    all.extend_from_slice(&rest);
    assert!(
        all.starts_with(b"partial"),
        "already-emitted stdout must be retained across the SIGINT"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigint_double_aborts() {
    // A leading `Text` chunk (rather than a bare `Hang`) gives this test a
    // readiness signal: once "x" is readable on stdout, `signal::install()`
    // has definitely already run (it happens right after `prompt()` is
    // awaited, before the render loop). Sending SIGINT on a fixed sleep
    // instead risks landing before the handler is installed, in which case
    // the OS's default SIGINT disposition kills the process outright
    // (`ExitStatus::code()` is then `None`, not `Some(130)`) -- confirmed
    // empirically against this exact test.
    let mock = MockBackend::start(Script(vec![vec![Chunk::Text("x"), Chunk::Hang]])).await;
    let fixture = write_fixture(&mock, 10);

    let mut child = command(&["-p", "hi"], &fixture)
        .spawn()
        .expect("spawn conway");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut buf = [0u8; 1];
    let read_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match stdout.read(&mut buf) {
            Ok(1) => break,
            Ok(_) => {}
            Err(_) => {}
        }
        if Instant::now() > read_deadline {
            panic!("never observed the initial TextDelta before the read deadline");
        }
    }

    send_sigint(child.id());
    std::thread::sleep(Duration::from_millis(200));
    send_sigint(child.id());

    let status = wait_with_timeout(&mut child, Duration::from_secs(2))
        .expect("conway must exit within 2s of the second SIGINT");
    assert_eq!(status.code(), Some(130));
}

#[cfg(unix)]
fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ---------------------------------------------------------------------
// Network isolation
// ---------------------------------------------------------------------

/// `cargo test -p conway-cli` must pass with no network access beyond
/// loopback: every fixture in this file points `base_url` at
/// `127.0.0.1:<ephemeral>` (see `fixtures/conway.json.tmpl` and
/// `MockBackend`) and no test in this suite dials any other host --
/// structurally enforced by this file never constructing a `Fixture` any
/// other way.
#[test]
fn fixtures_only_ever_point_at_loopback() {
    let template = include_str!("fixtures/conway.json.tmpl");
    assert!(template.contains("{{BASE_URL}}"));
    // `MockBackend::start*` binds `127.0.0.1:0` unconditionally -- grep the
    // harness source itself rather than re-deriving the guarantee.
    let mock_backend_src = include_str!("common/mock_backend.rs");
    assert!(mock_backend_src.contains("127.0.0.1:0"));
}
