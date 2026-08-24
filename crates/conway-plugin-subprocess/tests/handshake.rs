//! The `initialize/1` version-negotiation handshake (board item
//! `01M03VK7MRPSAVWMW7YNYPRPGT`): proof that a persistent-transport plugin is
//! greeted with one `initialize/1` request/response at session open BEFORE any
//! `tool/1` call, that `docs/plugins/compatibility.md`'s version-negotiation
//! table is enforced (refuse on major mismatch or unsatisfied `minor_min`;
//! accept otherwise; unknown fields ignored-and-counted), and that a plugin
//! that closes without answering fails closed within `timeout_ms`, never
//! hangs. Mirrors `tests/persistent.rs`'s own mock-plugin-process pattern
//! (fixtures in `tests/common/mod.rs`): every fixture here is a plain Python 3
//! script this suite writes into a fresh temp dir at run time, authored
//! outside this workspace's dependency graph.

mod common;

use std::sync::Arc;
use std::time::Duration;

use conway::plugin::{Plugin as _, ToolCall, ToolCtx};
use conway::AgentId;
use conway_plugin_subprocess::{
    SubprocessPlugin, SubprocessPluginError, SubprocessPluginSpec, SubprocessTransport,
};
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
// Acceptance criterion 1 -- a matching handshake opens the session and
// tool/1 proceeds
// ---------------------------------------------------------------------

/// **Criterion 1.** A persistent-transport plugin whose `initialize/1` answer
/// carries matching `major=1` and `minor_min=1` (<= host minor) must OPEN and
/// proceed to serve `tool/1` calls. The fixture also carries an unknown extra
/// field (`future_field`) so criterion 4's "accepted, not rejected" half is
/// covered by the SAME load (the "counted/surfaced" half is pinned by the
/// `wire::tests::initialize_answer_with_unknown_field_is_accepted_and_counted`
/// unit test, which asserts `unknown_field_count == 1`).
#[tokio::test]
async fn a_matching_handshake_opens_the_session_and_serves_tool_calls() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "handshake_ok.py",
        common::PERSISTENT_HANDSHAKE_OK_PLUGIN,
    )
    .await;
    assert_eq!(
        spec.transport,
        SubprocessTransport::Persistent,
        "the fixture must be configured for persistent transport"
    );

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("a matching initialize handshake must open the session");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let output = tool
        .invoke(call("greet", serde_json::json!({"name": "world"})), ctx())
        .await
        .expect("a tool/1 call after a successful handshake must succeed");
    assert!(!output.is_error);
    assert_eq!(text_of(&output), "hello, world");

    // The per-point version record the handshake produced is reachable
    // through the plugin WITHOUT re-negotiating -- the shape later wire-point
    // items consult.
    assert_eq!(
        plugin.point_version("tool/1"),
        Some(1),
        "the plugin's declared tool/1 version is recorded and readable"
    );
    assert_eq!(
        plugin.point_version("permission.policy/1"),
        None,
        "a point the plugin did not declare is None (no future-item point yet)"
    );
}

// ---------------------------------------------------------------------
// Acceptance criterion 2 -- a major mismatch refuses to load
// ---------------------------------------------------------------------

/// **Criterion 2.** A plugin whose `initialize/1` answer declares `major=2`
/// (the host's `HOST_WIRE_MAJOR` is `1`) must be REFUSED at `discover` with a
/// typed `HandshakeRefused` error naming BOTH majors and the "major mismatch"
/// condition. One-shot discovery (`tool.spec/1`) succeeds first, so the
/// refusal is specifically the handshake's.
#[tokio::test]
async fn a_major_mismatch_refuses_to_load_naming_both_majors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "handshake_major.py",
        common::PERSISTENT_HANDSHAKE_MAJOR_MISMATCH_PLUGIN,
    )
    .await;

    let err = SubprocessPlugin::discover(spec)
        .await
        .expect_err("a major mismatch must refuse the plugin at discover time");

    match err {
        SubprocessPluginError::HandshakeRefused {
            condition, detail, ..
        } => {
            assert!(
                condition.contains("major mismatch"),
                "the condition names 'major mismatch', got: {condition}"
            );
            // Names BOTH majors -- the host's (1) and the plugin's (2).
            assert!(
                detail.contains("1") && detail.contains("2"),
                "the detail names both majors (host 1, plugin 2), got: {detail}"
            );
        }
        other => panic!("expected HandshakeRefused, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Acceptance criterion 3 -- an unsatisfied minor_min refuses to load
// ---------------------------------------------------------------------

/// **Criterion 3.** A plugin whose `initialize/1` answer declares `minor_min=2`
/// (the host's `HOST_WIRE_MINOR` is `1`) must be REFUSED at `discover` with a
/// typed `HandshakeRefused` error naming the required minor and the host's
/// minor.
#[tokio::test]
async fn an_unsatisfied_minor_min_refuses_to_load_naming_the_required_minor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "handshake_minor.py",
        common::PERSISTENT_HANDSHAKE_MINOR_MIN_TOO_HIGH_PLUGIN,
    )
    .await;

    let err = SubprocessPlugin::discover(spec)
        .await
        .expect_err("an unsatisfied minor_min must refuse the plugin at discover time");

    match err {
        SubprocessPluginError::HandshakeRefused {
            condition, detail, ..
        } => {
            assert!(
                condition.contains("minor_min"),
                "the condition names 'minor_min', got: {condition}"
            );
            // Names the required minor (2) and the host's minor (1).
            assert!(
                detail.contains(">= 2") && detail.contains("1"),
                "the detail names the required minor_min (2) and the host minor (1), got: {detail}"
            );
        }
        other => panic!("expected HandshakeRefused, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Acceptance criterion 4 -- an unknown extra field is accepted, not rejected
// ---------------------------------------------------------------------

/// **Criterion 4.** An `initialize/1` answer carrying an unknown extra field
/// (`future_field`) must be ACCEPTED -- the session opens and a `tool/1` call
/// proceeds -- NOT rejected. This is the compatibility table's accept branch /
/// forward-compat rule: a newer plugin's extra field does not break an older
/// host. The "counted/surfaced in a debug/log path" half is pinned by the
/// `wire::tests::initialize_answer_with_unknown_field_is_accepted_and_counted`
/// unit test (asserts `unknown_field_count == 1` and that the count is
/// surfaced via `tracing::debug!` at the session level); this integration test
/// pins the load-bearing half -- the plugin LOADS, not rejected.
#[tokio::test]
async fn an_initialize_answer_with_an_unknown_field_is_accepted_not_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "handshake_ok.py",
        common::PERSISTENT_HANDSHAKE_OK_PLUGIN,
    )
    .await;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("an answer with an unknown extra field must be ACCEPTED, not rejected");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let output = tool
        .invoke(
            call("greet", serde_json::json!({"name": "accepted"})),
            ctx(),
        )
        .await
        .expect("a tool/1 call after an accepted handshake must succeed");
    assert_eq!(text_of(&output), "hello, accepted");
}

// ---------------------------------------------------------------------
// Acceptance criterion 5 -- a plugin that closes without answering fails
// closed within timeout_ms, never hangs
// ---------------------------------------------------------------------

/// **Criterion 5.** A plugin that reads the `initialize/1` request then closes
/// stdout (exits) WITHOUT answering must fail closed with a typed error within
/// `timeout_ms`, never hang. The reader task's EOF path `kill_all`s the session
/// as `SessionDied`; the initialize sender is dropped; `framed_round_trip`
/// surfaces the typed death reason. One-shot discovery still answers first, so
/// the failure is specifically the handshake's.
#[tokio::test]
async fn a_plugin_that_closes_without_answering_initialize_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = common::write_script(
        dir.path(),
        "handshake_no_answer.py",
        common::PERSISTENT_HANDSHAKE_NO_ANSWER_PLUGIN,
    );
    // Warm the fixture BEFORE building the timed spec: this test's own
    // budget was raised from 1000ms to 5000ms to survive a flake that was
    // never the handshake itself but a freshly-written, freshly-chmod'd
    // script's first exec costing seconds at ~0% CPU (board item
    // `01M09MPZ9C188AHNBKWEJ3CEQA`; see `common::warm`'s doc for the full
    // measurement). `discover` execs this SAME file twice in a row for a
    // persistent-transport plugin -- once for the one-shot `tool.spec/1`
    // manifest call, once to spawn the persistent session -- so paying the
    // tax here, once, discarded, covers both.
    common::warm(&path).await;
    let mut spec = SubprocessPluginSpec::new("test-fixture", vec![path.display().to_string()]);
    spec.transport = SubprocessTransport::Persistent;
    // Brought back DOWN from 5000ms now that `warm` above has already paid
    // the first-exec tax that made 5000ms necessary: a warm `python3`
    // interpreter's own cold start measures in the tens of milliseconds on
    // this machine (see `common::warm`'s doc), and `discover`'s two
    // sequential warm spawns (one-shot manifest, then the persistent
    // session) plus the near-instant close-without-answer path this test
    // exercises comfortably clear 1500ms -- the same per-spawn budget
    // `tests/mechanism.rs`'s own post-`warm` fix (`invoke_fails_closed_on_
    // timeout`) settled on for an identical "one warm interpreter startup
    // under parallel test load" bound. 5000ms with the tax gone would be a
    // materially weaker assertion (it would tolerate a near-5-second
    // regression in this path before ever failing); do not raise this back
    // up without first checking `warm` is still being called.
    spec.timeout_ms = 1500;

    let start = std::time::Instant::now();
    let err = SubprocessPlugin::discover(spec)
        .await
        .expect_err("a plugin that closes without answering must fail closed");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "the failure must surface within timeout_ms, not hang: took {:?}",
        start.elapsed()
    );

    // The close-without-answer case surfaces as `SessionDied` (the reader's
    // EOF `kill_all`) -- a typed error, never a hang. A never-answer-while-
    // alive case would surface as `TimedOut` via the same `framed_round_trip`
    // path (already pinned for tool/1 in `tests/persistent.rs`); this fixture
    // specifically covers the "closes without answering" shape the spec names.
    assert!(
        matches!(err, SubprocessPluginError::SessionDied { .. }),
        "expected SessionDied (the reader's EOF kill_all), got {err:?}"
    );
}

// ---------------------------------------------------------------------
// Hazard -- host.version is informational only, never branched on
// ---------------------------------------------------------------------

/// **The hazard the spec names: do NOT branch on conway's own `host.version`
/// for any compatibility decision.** The host puts `host.version` (the conway
/// crate version) on the wire for the plugin to read, but the negotiation
/// compares ONLY `major` and `minor_min` -- a host version bump does NOT
/// change the negotiation outcome. This test proves (a) `host.version` reaches
/// the plugin (the fixture reflects it back as the `tool/1` text result, and
/// the test asserts it equals `env!("CARGO_PKG_VERSION")`), AND (b) the
/// negotiation SUCCEEDED (the session opened) regardless of what version
/// string was sent. The structural guarantee -- that `initialize` never
/// references `host.version` post-serialization -- is in the code:
/// `PersistentSession::initialize` compares `HOST_WIRE_MAJOR`/`HOST_WIRE_MINOR`
/// only; `host.version` is serialized by `PersistentInitializeRequest::new`
/// and never read back or compared.
#[tokio::test]
async fn host_version_is_informational_only_and_not_branched_on() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "handshake_reflect.py",
        common::PERSISTENT_HANDSHAKE_REFLECTS_HOST_VERSION_PLUGIN,
    )
    .await;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("the negotiation succeeds regardless of the host version string");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let output = tool
        .invoke(call("echo", serde_json::json!({})), ctx())
        .await
        .expect("the tool/1 call must succeed");
    // The host version reached the plugin (it reflected it back), proving
    // `host.version` IS on the wire; the load succeeded, proving the
    // negotiation did NOT branch on it.
    assert_eq!(
        text_of(&output),
        env!("CARGO_PKG_VERSION"),
        "the host.version reached the plugin (reflected back); the negotiation \
         succeeded regardless, proving host.version is informational only"
    );
}
