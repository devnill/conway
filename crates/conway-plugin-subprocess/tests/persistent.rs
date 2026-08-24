//! The persistent NDJSON transport (board item `01M03VJHG1WFECFJB4ZH3CKWDX`):
//! direct proof that a `SubprocessPluginSpec` configured for persistent
//! transport spawns its command ONCE and answers many `tool/1` calls over
//! the same child, plus every failure mode the spec's acceptance criteria
//! name -- death mid-session, per-call timeout, and a malformed frame --
//! each asserted to fail CLOSED with a typed error, never a hang and never
//! a silent retry. Mirrors `tests/mechanism.rs`'s own mock-plugin-process
//! pattern (fixtures in `tests/common/mod.rs`): every fixture here is a
//! plain Python 3 script this suite writes into a fresh temp dir at run
//! time, authored outside this workspace's dependency graph.

mod common;

use std::sync::Arc;
use std::time::Duration;

use conway::plugin::{Plugin as _, ToolCall, ToolCtx, ToolError};
use conway::AgentId;
use conway_plugin_subprocess::{SubprocessPlugin, SubprocessPluginSpec, SubprocessTransport};
use conway_testkit::{CollectingEventSink, FakeSubagentHost};

fn ctx() -> ToolCtx {
    let agent_id = AgentId::new();
    ToolCtx::for_test(
        agent_id,
        std::env::temp_dir(),
        Arc::new(FakeSubagentHost::new(agent_id)),
        Arc::new(CollectingEventSink::new()),
    )
}

fn call(tool: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        call_id: "call-1".to_string(),
        name: conway::ToolName::new(tool),
        arguments,
    }
}

/// Extracts the single text block from a `ToolOutput`, panicking if the
/// shape is not exactly one text block.
fn text_of(output: &conway::plugin::ToolOutput) -> String {
    output
        .blocks
        .iter()
        .filter_map(|b| match b {
            conway::plugin::ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------
// Acceptance criterion 1 -- persistent transport reuses the SAME child
// ---------------------------------------------------------------------

/// **The load-bearing test for criterion 1.** A persistent-transport plugin
/// that reports its own `os.getpid()` over `tool/1` must return the SAME
/// pid across two sequential calls -- the child was spawned ONCE and reused,
/// not re-spawned fresh per call. The assertion is made NON-tautological by
/// the control in [`one_shot_transport_spawns_a_fresh_process_per_call`]:
/// the SAME fixture under the one-shot transport returns DIFFERENT pids, so
/// "identical pids" here is the persistent path's own property, not a
/// fixture artifact (a fixture that always returned a constant would pass
/// this test and fail the control).
#[tokio::test]
async fn persistent_transport_reuses_the_same_child_across_calls() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec =
        common::persistent_spec_for_warmed(dir.path(), "pid.py", common::PERSISTENT_PID_PLUGIN)
            .await;
    assert_eq!(
        spec.transport,
        SubprocessTransport::Persistent,
        "the fixture must be configured for persistent transport"
    );

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery (one-shot) against the fixture must succeed");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let first = tool
        .invoke(call("pid", serde_json::json!({})), ctx())
        .await
        .expect("first call must succeed");
    let second = tool
        .invoke(call("pid", serde_json::json!({})), ctx())
        .await
        .expect("second call must succeed");

    let first_pid: u32 = text_of(&first)
        .parse()
        .expect("the fixture reports its pid as the tool's text result");
    let second_pid: u32 = text_of(&second).parse().expect("second pid");
    assert_eq!(
        first_pid, second_pid,
        "the persistent transport must reuse the SAME child process across two sequential \
         tool/1 calls (got {first_pid} then {second_pid})"
    );
}

/// **The control that makes criterion 1's assertion non-tautological.** The
/// SAME pid-reporting fixture under the DEFAULT (one-shot) transport must
/// return DIFFERENT pids across two calls -- a fresh process per call, the
/// behavior this item preserves as the default. If this control ever
/// returned identical pids, the persistent test above would be proving
/// nothing about the transport.
#[tokio::test]
async fn one_shot_transport_spawns_a_fresh_process_per_call() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Default transport (one-shot) -- use spec_for, NOT persistent_spec_for.
    let spec = common::spec_for_warmed(dir.path(), "pid.py", common::PERSISTENT_PID_PLUGIN).await;
    assert_eq!(
        spec.transport,
        SubprocessTransport::OneShot,
        "the default transport must stay one-shot"
    );

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery must succeed");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let first = tool
        .invoke(call("pid", serde_json::json!({})), ctx())
        .await
        .expect("first call must succeed");
    let second = tool
        .invoke(call("pid", serde_json::json!({})), ctx())
        .await
        .expect("second call must succeed");

    let first_pid: u32 = text_of(&first).parse().expect("first pid");
    let second_pid: u32 = text_of(&second).parse().expect("second pid");
    assert_ne!(
        first_pid, second_pid,
        "the one-shot transport must spawn a FRESH process per call (got {first_pid} then \
         {second_pid}) -- this control proves the persistent test above is not a tautology"
    );
}

// ---------------------------------------------------------------------
// Acceptance criterion 2 -- a session that dies mid-session fails closed
// ---------------------------------------------------------------------

/// **Criterion 2.** A plugin that answers the first `tool/1` call then exits
/// nonzero must surface a typed `SessionDied` error for the SECOND call --
/// never a hang, never a silent retry, and within the per-call timeout. The
/// first call succeeds (the session was alive); the death is surfaced on the
/// next call, fail-closed.
#[tokio::test]
async fn a_session_that_dies_mid_session_fails_closed_on_the_next_call() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "die.py",
        common::PERSISTENT_DIE_AFTER_ONE_PLUGIN,
    )
    .await;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery must succeed");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let first = tool
        .invoke(call("die", serde_json::json!({})), ctx())
        .await
        .expect("the first call must succeed while the session is alive");
    assert_eq!(text_of(&first), "first");

    let start = std::time::Instant::now();
    let err = tool
        .invoke(call("die", serde_json::json!({})), ctx())
        .await
        .expect_err("the second call must fail closed once the session has died");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "the second call must fail promptly once the death is detected, not hang: took {:?}",
        start.elapsed()
    );
    assert!(
        matches!(err, ToolError::Io { .. }),
        "expected ToolError::Io (SessionDied is surfaced through the Io variant), got {err:?}"
    );
    let detail = match err {
        ToolError::Io { detail } => detail,
        _ => unreachable!(),
    };
    assert!(
        detail.contains("session died"),
        "the error must name the failure mode (session died), got: {detail}"
    );
}

// ---------------------------------------------------------------------
// Acceptance criterion 3 -- per-call timeout bounds each RPC
// ---------------------------------------------------------------------

/// **Criterion 3.** A plugin that reads a `tool/1` request and sleeps (never
/// answers) must be killed and reported `TimedOut` within `timeout_ms` --
/// the per-call deadline on the framed read, NOT a session-wide idle kill.
#[tokio::test]
async fn a_call_that_never_answers_times_out_within_timeout_ms() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Warm the fixture BEFORE building the timed spec. Historically this
    // budget relied on the crate's DEFAULT timeout_ms (5000ms) precisely
    // because it bounds BOTH the one-shot discovery spawn AND the
    // persistent session spawn `discover` does for a persistent-transport
    // plugin -- a tighter budget (an earlier 500ms draft) flaked under
    // this workspace's parallel test load, timing out one of those
    // DISCOVERY-time spawns rather than the tool/1 call this test means to
    // pin (board item `01M09MPZ9C188AHNBKWEJ3CEQA`: a freshly-written,
    // freshly-chmod'd script's first exec can cost seconds at ~0% CPU; see
    // `common::warm`'s doc). `warm` pays that tax here, once, discarded,
    // so `discover`'s spawns no longer need 5000ms of runway.
    let path = common::write_script(dir.path(), "sleepy.py", common::PERSISTENT_SLEEPY_PLUGIN);
    common::warm(&path).await;
    let mut spec = conway_plugin_subprocess::SubprocessPluginSpec::new(
        "test-fixture",
        vec![path.display().to_string()],
    );
    spec.transport = SubprocessTransport::Persistent;
    // Brought DOWN from the 5000ms default now that `warm` above has
    // already paid the first-exec tax that made 5000ms necessary here: a
    // warm `python3` startup measures in the tens of milliseconds on this
    // machine, so 1500ms leaves ample margin for `discover`'s two
    // sequential warm spawns (matching `tests/handshake.rs`'s identical,
    // separately-justified choice for the same "warm interpreter startup
    // under parallel load" bound) while still being a meaningfully tighter
    // assertion than 5000ms, which -- tax gone -- would tolerate a
    // near-1.5-second regression in this per-call deadline before ever
    // failing. Do not raise this back up without first checking `warm` is
    // still being called.
    spec.timeout_ms = 1500;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery answers promptly; only the tool/1 call sleeps");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let start = std::time::Instant::now();
    let err = tool
        .invoke(call("sleep", serde_json::json!({})), ctx())
        .await
        .expect_err("a call that never answers within timeout_ms must fail closed");
    assert!(
        start.elapsed() >= Duration::from_millis(1_200),
        "the call must actually wait for the per-call deadline (not fail instantly for a wrong \
         reason): took {:?}",
        start.elapsed()
    );
    assert!(
        start.elapsed() < Duration::from_secs(8),
        "the call must return once timeout_ms elapses, not wait for the fixture's 10s sleep: \
         took {:?}",
        start.elapsed()
    );
    assert!(
        matches!(err, ToolError::Io { .. }),
        "expected ToolError::Io (TimedOut is surfaced through the Io variant), got {err:?}"
    );
    let detail = match err {
        ToolError::Io { detail } => detail,
        _ => unreachable!(),
    };
    assert!(
        detail.contains("timed out"),
        "the error must name the timeout failure mode, got: {detail}"
    );
}

// ---------------------------------------------------------------------
// Acceptance criterion 4 -- a malformed frame is a typed parse error
// ---------------------------------------------------------------------

/// **Criterion 4 (partial line then EOF -- unterminated frame).** A plugin
/// that writes a half-line (no trailing newline) then exits must produce a
/// typed parse error (`ToolError::Internal`, carrying `MalformedFrame`),
/// not a deadlock.
#[tokio::test]
async fn a_partial_frame_then_eof_is_a_typed_parse_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "half.py",
        common::PERSISTENT_HALF_LINE_PLUGIN,
    )
    .await;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery must succeed");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let start = std::time::Instant::now();
    let err = tool
        .invoke(call("half", serde_json::json!({})), ctx())
        .await
        .expect_err("an unterminated frame must fail closed, not deadlock");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "a malformed frame must be detected promptly, not hang: took {:?}",
        start.elapsed()
    );
    assert!(
        matches!(err, ToolError::Internal { .. }),
        "expected ToolError::Internal (MalformedFrame is a parse error, surfaced through \
         Internal), got {err:?}"
    );
    let detail = match err {
        ToolError::Internal { detail } => detail,
        _ => unreachable!(),
    };
    assert!(
        detail.contains("malformed frame"),
        "the error must name the malformed-frame failure mode, got: {detail}"
    );
}

/// **Criterion 4 (invalid JSON -- a full line that is not JSON).** A plugin
/// that writes a complete line of garbage JSON then exits must produce the
/// same typed parse error, not a deadlock.
#[tokio::test]
async fn an_invalid_json_line_is_a_typed_parse_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "badjson.py",
        common::PERSISTENT_BAD_JSON_PLUGIN,
    )
    .await;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery must succeed");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let err = tool
        .invoke(call("badjson", serde_json::json!({})), ctx())
        .await
        .expect_err("an invalid JSON frame must fail closed, not deadlock");
    assert!(
        matches!(err, ToolError::Internal { .. }),
        "expected ToolError::Internal (MalformedFrame), got {err:?}"
    );
    let detail = match err {
        ToolError::Internal { detail } => detail,
        _ => unreachable!(),
    };
    assert!(
        detail.contains("malformed frame"),
        "the error must name the malformed-frame failure mode, got: {detail}"
    );
}

// ---------------------------------------------------------------------
// Regression -- a wedged child that stops draining stdin times out on the
// WRITE (not a hang)
// ---------------------------------------------------------------------

/// **Regression for the unbounded-write hang an adversarial review surfaced
/// (MEDIUM).** The module doc promises every failure mode is bounded, never a
/// hang -- true for the framed read (it sits under a per-call `timeout`) but,
/// before the fix, FALSE for the write: a `write_all`/`flush` left bare would
/// block forever if a child stopped draining stdin while staying alive (the
/// OS pipe buffer fills and `write_all` waits for space that never comes).
/// The one-shot path bounds its whole `drive` under one `timeout_at`; the
/// persistent path dropped that bounding when it moved the write out from
/// under a timeout. This test restores and pins it.
///
/// A persistent plugin that reads only a small prefix of a `tool/1` request
/// then WEDGES (stops draining stdin, stays alive, keeps stdout open) must
/// fail `TimedOut` within `timeout_ms` when sent a payload far exceeding the
/// OS pipe buffer (64 KiB on Linux, 16 KiB on macOS) -- the write deadline
/// bounds the block, never a hang. Without the write timeout, `write_all`
/// blocks on the full pipe forever and this test hangs to the harness's own
/// ceiling.
#[tokio::test]
async fn a_wedged_child_that_stops_draining_stdin_times_out_on_the_write() {
    let dir = tempfile::tempdir().expect("tempdir");
    // The DEFAULT timeout_ms (5000ms) bounds BOTH the one-shot discovery and
    // the wedged write. A tighter budget (an earlier 500ms draft) is the same
    // budget the criterion-3 sleepy test flaked on -- the DISCOVERY spawn
    // timed out under the workspace's parallel test load rather than the
    // write -- so this test reuses the default 5000ms every other persistent
    // test relies on, and proves the per-call WRITE deadline still bounds a
    // wedged child (the write blocks 5s, not a hang) within the same window.
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "wedge.py",
        common::PERSISTENT_WEDGE_ON_WRITE_PLUGIN,
    )
    .await;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("one-shot discovery must succeed (the fixture answers tool.spec/1)");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    // A payload far exceeding any OS pipe buffer: the wedged child drains
    // only ~100 bytes then sleeps, so the pipe fills and `write_all` blocks.
    // This is the exact hang the per-call write deadline bounds.
    let huge = "x".repeat(256 * 1024);
    let start = std::time::Instant::now();
    let err = tool
        .invoke(call("wedge", serde_json::json!({ "x": huge })), ctx())
        .await
        .expect_err("a wedged child that stops draining stdin must time out, not hang");
    assert!(
        start.elapsed() >= Duration::from_secs(4),
        "the call must actually wait for the write deadline (not fail instantly for a wrong \
         reason): took {:?}",
        start.elapsed()
    );
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "the wedged write must be bounded by the per-call deadline, not hang: took {:?}",
        start.elapsed()
    );
    assert!(
        matches!(err, ToolError::Io { .. }),
        "expected ToolError::Io (TimedOut is surfaced through the Io variant), got {err:?}"
    );
    let detail = match err {
        ToolError::Io { detail } => detail,
        _ => unreachable!(),
    };
    assert!(
        detail.contains("timed out"),
        "the error must name the timeout failure mode (the write deadline, not a hang), got: \
         {detail}"
    );
}

// ---------------------------------------------------------------------
// Acceptance criterion 5 -- default stays one-shot (positive coverage)
// ---------------------------------------------------------------------

/// **Criterion 5 (positive half).** The default transport constructed by
/// [`SubprocessPluginSpec::new`] is one-shot, so existing behavior is
/// unchanged. The negative half -- every existing test still passes -- is
/// verified by running `tests/mechanism.rs` and `tests/end_to_end.rs`
/// unchanged alongside this file.
#[tokio::test]
async fn the_default_transport_is_one_shot() {
    let spec = SubprocessPluginSpec::new("test-fixture", vec!["/bin/true".to_string()]);
    assert_eq!(
        spec.transport,
        SubprocessTransport::OneShot,
        "the default transport must stay one-shot so existing behavior is unchanged"
    );
}

// ---------------------------------------------------------------------
// Extra positive coverage -- success and declared-error over persistent
// ---------------------------------------------------------------------

/// A successful `tool/1` call over the persistent channel returns the real
/// subprocess's reply verbatim -- the same property
/// `mechanism.rs::a_successful_call_reaches_the_real_subprocess_and_returns_its_reply`
/// proves for the one-shot path, here for the persistent NDJSON transport.
#[tokio::test]
async fn a_successful_call_over_persistent_returns_the_real_reply() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec =
        common::persistent_spec_for_warmed(dir.path(), "greet.py", common::PERSISTENT_GREET_PLUGIN)
            .await;
    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery must succeed");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let output = tool
        .invoke(call("greet", serde_json::json!({"name": "world"})), ctx())
        .await
        .expect("a well-formed persistent call must succeed");
    assert!(!output.is_error);
    assert_eq!(text_of(&output), "hello, world");
}

/// A subprocess-declared error over the persistent channel maps to the
/// matching typed `ToolError`, the same property
/// `mechanism.rs::a_subprocess_declared_error_maps_to_the_matching_typed_tool_error`
/// proves for one-shot.
#[tokio::test]
async fn a_subprocess_declared_error_over_persistent_maps_to_the_typed_tool_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec =
        common::persistent_spec_for_warmed(dir.path(), "greet.py", common::PERSISTENT_GREET_PLUGIN)
            .await;
    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery must succeed");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let err = tool
        .invoke(
            call("greet", serde_json::json!({"name": "__boom__"})),
            ctx(),
        )
        .await
        .expect_err("the fixture deliberately declares failure for this argument");
    assert_eq!(
        err,
        ToolError::Internal {
            detail: "boom".to_string()
        }
    );
}
