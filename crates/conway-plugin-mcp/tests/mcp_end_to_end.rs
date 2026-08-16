//! End-to-end proof of `conway-plugin-mcp` (board item
//! `01M03GPNF0KN59FHAEEAEY2JD3`) against a REAL MCP server (acceptance
//! criterion 2 -- NOT a mock of conway's protocol). Each test writes a
//! hand-written Python 3 stdio MCP server into a tempdir at run time (see
//! `common/mod.rs`), discovers it, and exercises the full path: the
//! `initialize`/`notifications/initialized`/`tools/list` handshake runs once
//! at `discover`; every tool the server declares appears as an ordinary
//! `conway::plugin::Tool` with the right name/schema; `invoke` calls
//! `tools/call` over the same persistent stdio; an MCP `isError: true` result
//! surfaces as `is_error: true`; a cancelled/timed-out call fails closed; a
//! server that dies mid-session surfaces a typed `SessionDied`, never a hang.

mod common;

use std::sync::Arc;
use std::time::Duration;

use conway::plugin::{
    ContentBlock, PermissionClass, Plugin as _, ToolCall, ToolCategory, ToolCtx, ToolError,
};
use conway::AgentId;
use conway_plugin_mcp::{McpPlugin, McpPluginError, McpPluginSpec};
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

/// The text content of a `ToolOutput`'s first `Text` block, or a panic if the
/// output has no text block. Keeps the success-path assertions one-liners.
fn first_text(out: &conway::plugin::ToolOutput) -> String {
    for b in &out.blocks {
        if let ContentBlock::Text { text } = b {
            return text.clone();
        }
    }
    panic!("expected at least one Text block, got {:?}", out.blocks);
}

// ---------------------------------------------------------------------
// Discovery -- initialize / tools/list
// ---------------------------------------------------------------------

#[tokio::test]
async fn discover_completes_the_handshake_and_registers_every_listed_tool() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for(dir.path(), "ref.py", common::REF_MCP_SERVER);

    let plugin = McpPlugin::discover(spec)
        .await
        .expect("discovery against the reference MCP server must succeed");

    // The manifest id is derived from the server's `serverInfo.name`
    // (`ref-mcp`), prefixed with `mcp.`.
    let manifest = plugin.manifest();
    assert_eq!(manifest.id, "mcp.ref-mcp");
    assert_eq!(manifest.version, "0.1");
    assert_eq!(
        manifest.tools,
        vec![conway::ToolName::new("add"), conway::ToolName::new("greet")]
    );
    // The MCP client needs NO conway host cap -- it has its own transport.
    assert!(manifest.required_host_caps.is_empty());

    let tools = plugin.tools();
    assert_eq!(tools.len(), 2);

    let add = tools
        .iter()
        .find(|t| t.spec().name == conway::ToolName::new("add"))
        .expect("add tool must be registered");
    let mut add_spec = add.spec();
    assert_eq!(add_spec.description, "Add two integers and return the sum.");
    // An MCP tool is opaque to conway -> the conservative default
    // (Execute / Dangerous), mirroring subprocess unknown-tag degradation.
    assert_eq!(add_spec.category, ToolCategory::Execute);
    assert_eq!(add_spec.permission, PermissionClass::Dangerous);
    // The MCP `inputSchema` was compiled into a RootSchema the runtime can
    // validate against -- `properties` carries the declared `a`/`b`.
    assert!(add_spec.schema.schema.object().properties.contains_key("a"));

    let greet = tools
        .iter()
        .find(|t| t.spec().name == conway::ToolName::new("greet"))
        .expect("greet tool must be registered");
    assert_eq!(greet.spec().description, "Greet the caller by name.");
}

#[tokio::test]
async fn discover_fails_closed_when_the_command_cannot_be_spawned() {
    let spec = McpPluginSpec::new(
        "no-such",
        vec!["/nonexistent/binary/that/does/not/exist".to_string()],
    );
    let err = McpPlugin::discover(spec)
        .await
        .expect_err("an unspawnable command must fail closed");
    assert!(
        matches!(err, McpPluginError::Spawn { ref config_id, .. } if config_id == "no-such"),
        "expected Spawn, got {err:?}"
    );
}

#[tokio::test]
async fn discover_refuses_a_server_that_does_not_offer_tools() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for(dir.path(), "no_tools.py", common::NO_TOOLS_CAP_SERVER);
    let err = McpPlugin::discover(spec)
        .await
        .expect_err("a server without the tools capability must be refused");
    assert!(
        matches!(err, McpPluginError::HandshakeFailed { .. }),
        "expected HandshakeFailed, got {err:?}"
    );
}

// ---------------------------------------------------------------------
// tools/call -- the persistent round-trip
// ---------------------------------------------------------------------

#[tokio::test]
async fn tools_call_round_trips_a_text_result_into_a_content_block() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for(dir.path(), "ref.py", common::REF_MCP_SERVER);
    let plugin = McpPlugin::discover(spec).await.expect("discover");
    let add = plugin
        .tools()
        .into_iter()
        .find(|t| t.spec().name == conway::ToolName::new("add"))
        .expect("add tool");

    let out = add
        .invoke(call("add", serde_json::json!({"a": 2, "b": 40})), ctx())
        .await
        .expect("add must succeed");
    assert!(!out.is_error, "a successful call is not an error");
    assert_eq!(first_text(&out), "42");
}

#[tokio::test]
async fn every_tool_shares_one_child_process() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for(dir.path(), "pid.py", common::PID_SERVER);
    let plugin = McpPlugin::discover(spec).await.expect("discover");
    let pid_tool = plugin
        .tools()
        .into_iter()
        .find(|t| t.spec().name == conway::ToolName::new("pid"))
        .expect("pid tool");

    let out1 = pid_tool
        .invoke(call("pid", serde_json::json!({})), ctx())
        .await
        .expect("first pid");
    let out2 = pid_tool
        .invoke(call("pid", serde_json::json!({})), ctx())
        .await
        .expect("second pid");
    // The load-bearing property: two sequential calls hit the SAME child
    // process, so the pid is identical (a fresh process per call would differ).
    assert_eq!(first_text(&out1), first_text(&out2));
}

#[tokio::test]
async fn an_mcp_iserror_result_surfaces_as_is_error_true() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for(dir.path(), "ref.py", common::REF_MCP_SERVER);
    let plugin = McpPlugin::discover(spec).await.expect("discover");
    let greet = plugin
        .tools()
        .into_iter()
        .find(|t| t.spec().name == conway::ToolName::new("greet"))
        .expect("greet tool");

    let out = greet
        .invoke(
            call("greet", serde_json::json!({"name": "__boom__"})),
            ctx(),
        )
        .await
        .expect("an isError result is still a successful ToolOutput, not an Err");
    // The load-bearing MCP distinction: `isError: true` is a tool-level
    // failure the caller reads (is_error: true), NOT a transport/protocol
    // failure (which would be `Err`).
    assert!(out.is_error, "isError:true must surface as is_error:true");
    assert_eq!(first_text(&out), "boom: greet refused");
}

#[tokio::test]
async fn an_unknown_content_block_type_is_dropped_and_surfaced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for(dir.path(), "mix.py", common::UNKNOWN_BLOCK_SERVER);
    let plugin = McpPlugin::discover(spec).await.expect("discover");
    let mix = plugin
        .tools()
        .into_iter()
        .find(|t| t.spec().name == conway::ToolName::new("mix"))
        .expect("mix tool");

    let out = mix
        .invoke(call("mix", serde_json::json!({})), ctx())
        .await
        .expect("the call still succeeds (drop+count+surface)");
    // The known block is preserved.
    assert!(
        first_text(&out) == "kept",
        "the known text block must be preserved"
    );
    // A drop-note naming the unknown `quantum` type is appended, and since
    // the server did NOT say isError, the note flips is_error to true.
    let has_note = out.blocks.iter().any(|b| {
        matches!(b, ContentBlock::Text { text } if text.contains("quantum") && text.contains("dropped"))
    });
    assert!(
        has_note,
        "a drop-note naming the unknown type must be appended: {:?}",
        out.blocks
    );
    assert!(
        out.is_error,
        "a dropped block flips is_error when the server did not"
    );
}

// ---------------------------------------------------------------------
// Failure handling -- fail closed, never a hang
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_timed_out_call_fails_closed_within_the_deadline() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A per-call deadline (2000ms) so a stuck server fails fast -- well under
    // the fixture's 10s sleep. The discover handshake uses the SAME deadline;
    // the SLEEPY server answers initialize/tools/list promptly, so discover
    // succeeds. 2000ms (not 500ms) is deliberate: under parallel test
    // execution every test spawns its own Python child at once, and Python
    // cold-start can exceed 500ms under that contention -- a 500ms discover
    // deadline is flaky. 2000ms survives parallel startup with margin while
    // still bounding the stuck call at ~2s, well under the 5s assertion below.
    let spec = common::spec_with_timeout(dir.path(), "sleepy.py", common::SLEEPY_SERVER, 2000);
    let plugin = McpPlugin::discover(spec).await.expect("discover");
    let sleep_tool = plugin
        .tools()
        .into_iter()
        .find(|t| t.spec().name == conway::ToolName::new("sleep"))
        .expect("sleep tool");

    let start = std::time::Instant::now();
    let err = sleep_tool
        .invoke(call("sleep", serde_json::json!({})), ctx())
        .await
        .expect_err("a stuck call must time out, not hang");
    // The per-call deadline (2000ms) bounds the wait -- well under the
    // fixture's 10s sleep. Allow generous slack for Python scheduling.
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "a timed-out call must fail within the deadline, not hang: {:?}",
        start.elapsed()
    );
    // A timeout is a transport failure -> ToolError::Io carrying the
    // McpPluginError::TimedOut Display.
    assert!(
        matches!(err, ToolError::Io { ref detail } if detail.contains("timed out")),
        "expected ToolError::Io mentioning timeout, got {err:?}"
    );
}

#[tokio::test]
async fn a_session_that_dies_mid_call_surfaces_a_typed_session_died() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for(dir.path(), "die.py", common::DIE_AFTER_ONE_SERVER);
    let plugin = McpPlugin::discover(spec).await.expect("discover");
    let die_tool = plugin
        .tools()
        .into_iter()
        .find(|t| t.spec().name == conway::ToolName::new("die"))
        .expect("die tool");

    // First call succeeds (the server answers once), then exits nonzero.
    let out = die_tool
        .invoke(call("die", serde_json::json!({})), ctx())
        .await
        .expect("the first call must succeed");
    assert_eq!(first_text(&out), "first");

    // The second call observes the death: a typed SessionDied, never a hang.
    let err = die_tool
        .invoke(call("die", serde_json::json!({})), ctx())
        .await
        .expect_err("the second call must fail closed after the server died");
    assert!(
        matches!(err, ToolError::Io { ref detail } if detail.contains("session died")),
        "expected ToolError::Io mentioning session died, got {err:?}"
    );
}

#[tokio::test]
async fn a_call_cancelled_in_flight_returns_cancelled_not_a_hang() {
    let dir = tempfile::tempdir().expect("tempdir");
    // SHORT_SLEEPY_SERVER sleeps a bounded 300ms per call -- long enough that a
    // 50ms cancel lands squarely in the read sleep (so the call resolves to
    // `Cancelled`, not a prompt Ok), short enough that the SECOND call below
    // (which waits for the first sleep to finish, then its own) completes well
    // inside the 5000ms per-call timeout. The default timeout (5000ms), not a
    // tight one: the cancel watcher polls every 10ms and cancels at 50ms.
    let spec = common::spec_for(dir.path(), "sleepy.py", common::SHORT_SLEEPY_SERVER);
    let plugin = McpPlugin::discover(spec).await.expect("discover");
    let sleep_tool = plugin
        .tools()
        .into_iter()
        .find(|t| t.spec().name == conway::ToolName::new("sleep"))
        .expect("sleep tool");

    // Build a ToolCtx whose cancel token we hold, so we can cancel mid-flight.
    let agent_id = AgentId::new();
    let mut tctx = ToolCtx::for_test(
        agent_id,
        std::env::temp_dir(),
        Arc::new(FakeSubagentHost::new(agent_id)),
        Arc::new(CollectingEventSink::new()),
    );
    let cancel = conway::plugin::CancellationToken::new();
    tctx.cancel = cancel.clone();

    let invoke_fut = sleep_tool.invoke(call("sleep", serde_json::json!({})), tctx);
    // Cancel after a short beat so the call is in flight (mid read-sleep). The
    // cancel watcher polls every 10ms, so this resolves to Cancelled well
    // before the 5000ms per-call timeout. Either Cancelled or TimedOut is a
    // fail-closed resolution (not a hang), but the watcher should observe the
    // cancel first, so assert Cancelled specifically.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
    });
    let start = std::time::Instant::now();
    let err = invoke_fut
        .await
        .expect_err("a cancelled call must fail, not hang");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "a cancelled call must resolve promptly, not hang: {:?}",
        start.elapsed()
    );
    assert!(
        matches!(err, ToolError::Cancelled)
            || matches!(err, ToolError::Io { ref detail } if detail.contains("timed out")),
        "expected Cancelled (or TimedOut as the fail-closed bound), got {err:?}"
    );

    // THE LOAD-BEARING SURVIVAL CHECK: a single cancellation must NOT take down
    // the shared session for every other tool on this plugin (the whole-plugin
    // outage a cancel-during-WRITE would cause if the write were raced against
    // cancel and dropped mid-`write_all`, leaving a partial newline-less
    // request line that corrupts the NDJSON framing). A second call -- with a
    // FRESH, un-cancelled token -- must still SUCCEED and return the `slept`
    // text, proving the session is alive and the framing is intact.
    let agent_id2 = AgentId::new();
    let mut tctx2 = ToolCtx::for_test(
        agent_id2,
        std::env::temp_dir(),
        Arc::new(FakeSubagentHost::new(agent_id2)),
        Arc::new(CollectingEventSink::new()),
    );
    tctx2.cancel = conway::plugin::CancellationToken::new();
    let out = sleep_tool
        .invoke(call("sleep", serde_json::json!({})), tctx2)
        .await
        .expect(
            "a second call after a cancelled call must succeed -- the \
                 shared session survived the cancellation and the framing is \
                 intact",
        );
    assert!(
        !out.is_error,
        "the second call must be a clean result, not an error: {out:?}"
    );
    let text = first_text(&out);
    assert!(
        text.contains("slept"),
        "the second call must round-trip the `slept` text, got {text:?}"
    );
}
