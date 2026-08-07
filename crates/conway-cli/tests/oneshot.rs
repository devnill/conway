//! WI-113: one-shot (`-p`) integration tests against the real, compiled
//! `conway` binary -- exit codes, stdout purity, streaming shape, and
//! SIGINT behavior. See `docs/scripting.md` (WI-113) for the exit-code and
//! output-format contract this suite locks in as executable acceptance
//! evidence.
//!
//! ## Exit-code liveness
//!
//! Every exit code `docs/scripting.md` declares has a test in this file
//! that drives the real binary and asserts the observed process exit
//! status -- a unit test of `exit.rs`'s mapping functions is NOT evidence
//! a code is reachable (GP-14): the exit-4 classifier's unit tests passed
//! for its entire unreachable lifetime, because they constructed a
//! `ConwayError::Routing` by hand and never drove the path
//! (`AgentLoop::finish_error` folding the routing failure into
//! `ResultStatus::Failed`) that a live turn actually takes. The exit-4
//! tests below (`exit_4_no_backend`, `exit_4_unregistered_model`,
//! `exit_4_unknown_role_override`, `exit_4_context_too_large`) each drive a
//! distinct live routing rejection through the real one-shot path.
//!
//! There is deliberately no exit-3 test: code 3 (`PermissionDenied`) was
//! removed from the contract rather than wired -- a permission denial is a
//! tool result fed back into the agent's own turn, not a terminal
//! condition (see `exit.rs`'s module doc, entry 1, and
//! `docs/scripting.md`). `denied_calls_stay_in_turn_until_budget` below
//! pins the behavior that decision describes: under `--permission-mode
//! deny`, a script that only ever proposes tool calls keeps taking turns
//! (every denial re-prompts the model) until `budget.max_steps` is
//! exhausted, terminating as `BudgetExceeded` (exit 5), with the denial
//! visible as a `PermissionResolved` envelope in the `jsonl` stream.

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

/// `seq` is only guaranteed strictly increasing WITHIN one session (see
/// `docs/scripting.md`'s jsonl contract) -- across sessions it can go
/// backward, because a subagent's lifecycle lines carry the *child's own*
/// counter, not the root's. This single-agent script spawns nothing, so
/// every line here shares one session and the per-session grouping below
/// degenerates to the whole stream -- `jsonl_seq_is_per_session_not_global`
/// is the test that actually exercises more than one session.
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
    let mut last_seq_by_session: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();
    for value in &lines {
        let obj = value.as_object().expect("each line is a JSON object");
        assert!(obj.contains_key("seq"));
        assert!(obj.contains_key("session"));
        assert!(obj.contains_key("agent"));
        assert!(obj.contains_key("event"));
        let session = obj["session"]
            .as_str()
            .expect("session is a string")
            .to_string();
        let seq = obj["seq"].as_i64().expect("seq is a number");
        if let Some(prev) = last_seq_by_session.get(&session) {
            assert!(
                seq > *prev,
                "seq must be strictly increasing within session {session}: {prev} then {seq}"
            );
        }
        last_seq_by_session.insert(session, seq);
    }
}

/// Pins the four-part jsonl `seq` contract `docs/scripting.md` documents,
/// against a real multi-agent run (root turn -> `conway_spawn` ->
/// child text -> root final text), driven through the real compiled
/// binary. Asserts:
/// (i) seq is strictly increasing WITHIN each session, grouped/keyed on
///     session before ordering;
/// (ii) the root session's own seqs are gap-free, `0..=n` contiguous;
/// (iii) the child session appears only as a sparse lifecycle slice --
///      exactly `[agent_spawned, agent_finished]`, non-contiguous;
/// (iv) exactly one `agent_finished` carries the root agent id, and it is
///      the LAST line in the stream;
/// (v) a non-root `agent_finished` (the child's) appears earlier.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jsonl_seq_is_per_session_not_global() {
    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall {
                name: "conway_spawn",
                args: serde_json::json!({ "prompt": "child task" }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("child done"), Chunk::Finish("stop")],
        vec![Chunk::Text("root final"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(
        &[
            "-p",
            "spawn a subagent to do a task, then answer",
            "--allowed-tools",
            "conway_spawn",
            "--output-format",
            "jsonl",
        ],
        &fixture,
    );

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines = jsonl_lines(&out.stdout);
    assert!(!lines.is_empty());

    let mut by_session: std::collections::BTreeMap<String, Vec<&Value>> =
        std::collections::BTreeMap::new();
    for line in &lines {
        let session = line["session"]
            .as_str()
            .expect("session is a string")
            .to_string();
        by_session.entry(session).or_default().push(line);
    }
    assert_eq!(
        by_session.len(),
        2,
        "expected exactly the root session plus one spawned child session; got: {by_session:#?}"
    );

    // (i) seq is strictly increasing WITHIN each session.
    for (session, group) in &by_session {
        let mut last: Option<i64> = None;
        for line in group {
            let seq = line["seq"].as_i64().expect("seq is a number");
            if let Some(prev) = last {
                assert!(
                    seq > prev,
                    "session {session}: seq must be strictly increasing within a session: \
                     {prev} then {seq}"
                );
            }
            last = Some(seq);
        }
    }

    // The root session is the one that carries every non-lifecycle line
    // (turn/model/tool/text events); the child appears only as the sparse
    // two-line lifecycle slice asserted below.
    let (root_session, root_group) = by_session
        .iter()
        .max_by_key(|(_, group)| group.len())
        .expect("at least one session group");
    let (child_session, child_group) = by_session
        .iter()
        .find(|(session, _)| *session != root_session)
        .expect("a second, child session group");

    // (ii) the root session's own seqs are gap-free, 0..=n contiguous.
    let root_seqs: Vec<i64> = root_group
        .iter()
        .map(|l| l["seq"].as_i64().unwrap())
        .collect();
    let expected: Vec<i64> = (0..root_seqs.len() as i64).collect();
    assert_eq!(
        root_seqs, expected,
        "root session {root_session}'s seq must be gap-free from 0"
    );

    // Every root-group line shares the same `agent` (the root agent id) --
    // the passthrough that would otherwise interleave a foreign agent id is
    // scoped to the child's OWN session, which is why grouping by session
    // alone is enough to isolate the root's own agent id here.
    let root_agent_id = root_group[0]["agent"]
        .as_str()
        .expect("root agent id is a string");
    for line in root_group {
        assert_eq!(
            line["agent"].as_str().unwrap(),
            root_agent_id,
            "every line in the root session's group must be stamped with the root's own agent id"
        );
    }

    // (iii) the child session is exactly [agent_spawned, agent_finished],
    // non-contiguous.
    let child_events: Vec<&str> = child_group
        .iter()
        .map(|l| l["event"].as_str().expect("event is a string"))
        .collect();
    assert_eq!(
        child_events,
        vec!["agent_spawned", "agent_finished"],
        "child session {child_session} must appear only as the sparse lifecycle slice"
    );
    let child_seqs: Vec<i64> = child_group
        .iter()
        .map(|l| l["seq"].as_i64().unwrap())
        .collect();
    assert_eq!(
        child_seqs[0], 0,
        "the child's own per-session counter starts at 0, at its agent_spawned"
    );
    assert!(
        child_seqs[1] > child_seqs[0] + 1,
        "the child's agent_finished seq must NOT be contiguous with its agent_spawned -- the \
         child's own turn content (seqs in between) never crosses the session filter: {child_seqs:?}"
    );

    // (iv) exactly one agent_finished carries the root agent id, and it is
    // the LAST line in the whole stream.
    let finished_lines: Vec<(usize, &Value)> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l["event"] == "agent_finished")
        .collect();
    assert_eq!(
        finished_lines.len(),
        2,
        "root + the one spawned child, each contributing exactly one agent_finished: {finished_lines:#?}"
    );
    let root_finished: Vec<&(usize, &Value)> = finished_lines
        .iter()
        .filter(|(_, l)| l["result"]["agent_id"].as_str() == Some(root_agent_id))
        .collect();
    assert_eq!(
        root_finished.len(),
        1,
        "exactly one agent_finished must carry the root agent id: {finished_lines:#?}"
    );
    let (root_finished_index, _) = root_finished[0];
    assert_eq!(
        *root_finished_index,
        lines.len() - 1,
        "the agent_finished naming the root agent id must be the LAST line in the stream"
    );

    // (v) a non-root agent_finished (the child's) appears earlier.
    let non_root_finished: Vec<&(usize, &Value)> = finished_lines
        .iter()
        .filter(|(_, l)| l["result"]["agent_id"].as_str() != Some(root_agent_id))
        .collect();
    assert_eq!(
        non_root_finished.len(),
        1,
        "expected exactly one non-root agent_finished (the child's): {finished_lines:#?}"
    );
    let (non_root_index, _) = non_root_finished[0];
    assert!(
        *non_root_index < *root_finished_index,
        "the child's agent_finished must appear strictly earlier than the root's terminal \
         agent_finished -- breaking on the first agent_finished would truncate the run and lose \
         the root's own final answer"
    );
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
    // A 401 from the backend is `BackendError::Auth`, which T-2 classifies
    // as `FailureClass::Fatal` -- `AttemptEngine` aborts the whole chain
    // with `RuntimeError::Backend` (an auth failure is NOT a routing
    // rejection: retrying another candidate against the same bad key is
    // pointless, so it never becomes `NoCandidate`). `finish_error` folds
    // it into `ResultStatus::Failed`, whose text carries no routing
    // rejection wording, so the exit classifier lands on `AgentFailed` (1)
    // -- distinct from every `exit_4_*` driver, where the backend either
    // could not be reached or could not serve the request at all.
    let mock = MockBackend::start(Script(vec![vec![Chunk::HttpError {
        status: 401,
        body: r#"{"error":{"message":"invalid api key"}}"#,
    }]]))
    .await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["-p", "hi"], &fixture);

    // Contact + wording assertions, not just the code: exit 1 is the
    // classifier's fallback, so ANY non-routing failure (including one that
    // never reaches the backend) also exits 1. Without these the test passes
    // vacuously while the 401 -> Auth -> Fatal path goes unexercised.
    assert!(
        !mock.requests().is_empty(),
        "the run never contacted the backend -- the 401 path was not exercised"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("authentication failed"),
        "expected the auth failure in stderr, got: {stderr}"
    );
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
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
/// no entry for it, `check_candidate` skips it), a routing rejection that
/// exits 4 (`exit_4_unregistered_model` below locks that in). Passing
/// `--model mock/<real model>` must override that broken chain and route
/// to the mock successfully instead (exit 0) -- proving the pin, not the
/// chain, decided the outcome.
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

/// Exit 4 (`NoHealthyBackend`), driver 1 of 4: every candidate in the
/// chain fails live (the mock is dropped, so every connection is refused).
/// `AttemptEngine` exhausts the chain and surfaces
/// `RoutingError::NoCandidate`, which `AgentLoop::finish_error` folds into
/// `ResultStatus::Failed` -- and `ExitCode::from_result`'s `Failed` arm
/// classifies that as the routing rejection it is (see `exit.rs`'s module
/// doc, entry 2, for why the wiring lives there and not in `from_error`).
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
        Some(4),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Exit 4, driver 2 of 4: the role's chain names a `backend/model` pair
/// `models.json` does not register, so the router rejects the only
/// candidate on capabilities (`CapabilitySkip`) before ever dialing a
/// backend -- `NoCandidate` with zero backend contact. (Same broken-chain
/// setup `model_flag_pins_and_overrides_role_chain` rescues with `--model`;
/// here nothing rescues it.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_4_unregistered_model() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    let broken = std::fs::read_to_string(&fixture.config_path)
        .unwrap()
        .replace(&format!("mock/{}", mock.model), "mock/unregistered-model");
    std::fs::write(&fixture.config_path, broken).unwrap();

    let out = run_conway(&["-p", "hi"], &fixture);

    assert_eq!(
        out.status.code(),
        Some(4),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        mock.requests().is_empty(),
        "an unindexed candidate must be rejected before any backend request"
    );
}

/// Exit 4, driver 3 of 4: `--role-override` names a role the config does
/// not define -- `RoutingError::UnknownRole`, a routing rejection like
/// `NoCandidate` (routing could not supply any model for the turn).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_4_unknown_role_override() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["-p", "hi", "--role-override", "doesnotexist"], &fixture);

    assert_eq!(
        out.status.code(),
        Some(4),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown role alias"),
        "the routing rejection should be named on stderr: {stderr}"
    );
}

/// Exit 4, driver 4 of 4: every candidate's context window is too small for
/// the assembled prompt plus reserved headroom, and headroom is the ONLY
/// rejection reason -- amended P-9 makes the router return
/// `RoutingError::ContextTooLarge` (not `NoCandidate`) for exactly this
/// case, and the exit classifier treats it coherently with every other
/// routing rejection. With no `ContextHook` registered in one-shot mode,
/// there is no truncation or escalation: the turn is terminally rejected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_4_context_too_large() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    // Shrink the mock model's declared window to one token: the sole
    // candidate then fails ONLY the headroom gate.
    let models_path = fixture.dir.path().join(".conway/models.json");
    let shrunk = std::fs::read_to_string(&models_path)
        .unwrap()
        .replace("128000", "1");
    std::fs::write(&models_path, shrunk).unwrap();

    let out = run_conway(&["-p", "hi"], &fixture);

    assert_eq!(
        out.status.code(),
        Some(4),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("context rejected:"),
        "the T-1 rejection should be named on stderr: {stderr}"
    );
    assert!(
        mock.requests().is_empty(),
        "a headroom-rejected candidate must never be dialed"
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

/// There is no permission-denied exit code (see this file's module doc and
/// `exit.rs`'s, entry 1): a denied tool call is fed back into the agent's
/// own turn, never a terminal condition, so a deny-mode script that only
/// ever proposes tool calls keeps taking turns until `budget.max_steps` is
/// exhausted -- exit 5 (`BudgetExceeded`). What this test also locks in,
/// because it is live: a `PermissionResolved` denial envelope is visible
/// in the `jsonl` stream for every denied tool call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn denied_calls_stay_in_turn_until_budget() {
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
