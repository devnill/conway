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
//! server that dies mid-session is transparently respawned so the NEXT call
//! recovers, bounded by `MAX_AUTO_RESPAWNS` and guarded by a
//! tool-set-changed check on every respawn (board item's own "a dead MCP
//! plugin session never recovers" auto-respawn work) -- but the call that
//! discovered the death is NEVER resent against the fresh child (board item
//! `01M1ZR1DGZB8FB399SMG51RHP5`'s correction: a killed process may already
//! have completed the side effect, and nothing over stdio can tell).

mod common;

use std::sync::Arc;
use std::time::Duration;

use conway::plugin::{
    ContentBlock, PermissionClass, Plugin as _, ToolCall, ToolCategory, ToolCtx, ToolError,
};
use conway::AgentId;
use conway_plugin_mcp::{McpPlugin, McpPluginError, McpPluginSpec, MAX_AUTO_RESPAWNS};
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
    let spec = common::spec_for_warmed(dir.path(), "ref.py", common::REF_MCP_SERVER).await;

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
    let spec =
        common::spec_for_warmed(dir.path(), "no_tools.py", common::NO_TOOLS_CAP_SERVER).await;
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
    let spec = common::spec_for_warmed(dir.path(), "ref.py", common::REF_MCP_SERVER).await;
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
    let spec = common::spec_for_warmed(dir.path(), "pid.py", common::PID_SERVER).await;
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
    let spec = common::spec_for_warmed(dir.path(), "ref.py", common::REF_MCP_SERVER).await;
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
    let spec = common::spec_for_warmed(dir.path(), "mix.py", common::UNKNOWN_BLOCK_SERVER).await;
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
    //
    // This test used to fail intermittently under full-suite parallel runs
    // with `TimedOut` on DISCOVERY, not the sleep below -- root-caused
    // 2026-08-21 (board item `01M09MPZ9C188AHNBKWEJ3CEQA`) to a
    // first-execution OS cost paid by any freshly-written script's first
    // exec, not by CPU contention as such (see `common::warm`'s doc for the
    // measurement: up to 23.5s at 0% CPU on a brand-new file, 44ms/35ms on
    // the SAME file's later execs). `spec_with_timeout` now calls
    // `common::warm` on this fixture before this deadline governs anything,
    // so 2000ms only has to cover real Python cold-start under contention,
    // which is what the comment above was already trying (and, half the
    // time, failing) to buy with a bigger number. Do not raise this past
    // 2000ms to chase a future flake without first confirming `warm` ran --
    // if it did and this still flakes, that is new information about a
    // bigger tax, not a reason to guess again.
    // Retimed for board item `01M1YQ3MJQSCQTMVAZ3GCSTB8P` (patience before
    // the kill): `McpPluginSpec`'s default `first_call_timeout_ms` (20s) now
    // governs the FIRST ordinary round trip after discover, and the SLEEPY
    // fixture's 10s sleep fits comfortably inside that budget -- so the
    // FIRST `sleep` call below now SUCCEEDS where it used to time out. This
    // test now spends that first call deliberately (consuming the one-time
    // warm-up slot) and pins the timeout assertion on the SECOND call, which
    // is no longer eligible for it and hits the ordinary per-call ceiling
    // instead -- exactly the shape acceptance 3 of that item's spec
    // describes ("a fixture slow only on its first call passes; the same
    // slowness on its second call is killed at the ordinary deadline").
    let spec =
        common::spec_with_timeout(dir.path(), "sleepy.py", common::SLEEPY_SERVER, 2000).await;
    let plugin = McpPlugin::discover(spec).await.expect("discover");
    let sleep_tool = plugin
        .tools()
        .into_iter()
        .find(|t| t.spec().name == conway::ToolName::new("sleep"))
        .expect("sleep tool");

    let first = sleep_tool
        .invoke(call("sleep", serde_json::json!({})), ctx())
        .await
        .expect(
            "the FIRST call gets the first-call warm-up budget (20s default), comfortably \
             longer than the fixture's 10s sleep, so it must succeed",
        );
    assert!(
        first_text(&first).contains("slept"),
        "expected the fixture's own 'slept' text, got {first:?}"
    );

    // Board item `01M1ZR1DGZB8FB399SMG51RHP5`'s correction: a timeout kills
    // the process group, closing its stdout, which the reader's own EOF arm
    // records as `SessionDied` -- so this SECOND call's timeout ALSO leaves
    // the session dead, exactly the shape that used to trigger an
    // in-call respawn-and-RETRY. If this call were still retried against a
    // freshly-respawned session, that retry would get its OWN fresh
    // first-call warm-up budget (20s) and would actually SUCCEED against
    // the fixture's 10s sleep, well past this assertion's window --
    // `expect_err` would fail with `Ok(ToolOutput { text: "slept" })`. It
    // must not: the retry is gone, so this call fails closed on its OWN
    // timeout, deterministically, never denied "by luck" of a fresh budget
    // running out too.
    let start = std::time::Instant::now();
    let err = sleep_tool
        .invoke(call("sleep", serde_json::json!({})), ctx())
        .await
        .expect_err(
            "the SECOND call is no longer eligible for the warm-up budget and must time out at \
             the ordinary per-call ceiling, not hang, and must NEVER be silently retried \
             against a respawned session",
        );
    // The ordinary per-call deadline (2000ms) plus its own bounded grace
    // (`GRACE_CEILING_FACTOR`, currently 3x) puts the full ceiling at 6000ms
    // -- well under the fixture's 10s sleep. Allow generous slack for Python
    // scheduling and the kill-group reap that follows.
    assert!(
        start.elapsed() < Duration::from_secs(9),
        "a timed-out call must fail within the full (base + grace) ceiling, not hang: {:?}",
        start.elapsed()
    );
    // A timeout is a transport failure -> ToolError::Io carrying the
    // McpPluginError::TimedOut Display.
    assert!(
        matches!(err, ToolError::Io { ref detail } if detail.contains("timed out")),
        "expected ToolError::Io mentioning timeout, got {err:?}"
    );
}

/// **Acceptance criterion 1, corrected by board item
/// `01M1ZR1DGZB8FB399SMG51RHP5`'s ruling: respawn, but never retry the
/// in-flight request.** A plugin whose session dies mid-call fails THAT
/// call closed -- never silently retried -- but the respawn it triggers
/// leaves a fresh session in place, so the VERY NEXT tool call recovers, no
/// operator action, no conway restart, and succeeds against a DEMONSTRABLY
/// NEW child process. "Demonstrably new" is proven, not inferred:
/// `DIES_MID_FIRST_CALL_THEN_RECOVERS_SERVER` writes each generation's own
/// pid to `SPAWN_COUNTER_FILE` at spawn time (generation 1 never gets to
/// report its own pid over the wire -- it dies before answering), so this
/// test compares the logged generation-1 pid against BOTH the logged
/// generation-2 pid and the pid generation 2 actually reports over
/// `tools/call`, not merely that the second call happened not to error.
#[tokio::test]
async fn a_session_that_dies_mid_call_fails_closed_and_the_next_call_recovers_on_a_new_process() {
    let dir = tempfile::tempdir().expect("tempdir");
    let counter_path = dir.path().join("spawn-count.txt");
    let mut spec = common::spec_for_warmed(
        dir.path(),
        "die.py",
        common::DIES_MID_FIRST_CALL_THEN_RECOVERS_SERVER,
    )
    .await;
    spec.env.push((
        "SPAWN_COUNTER_FILE".to_string(),
        counter_path.display().to_string(),
    ));
    let plugin = McpPlugin::discover(spec).await.expect("discover");
    let die_tool = plugin
        .tools()
        .into_iter()
        .find(|t| t.spec().name == conway::ToolName::new("die"))
        .expect("die tool");

    let read_pids = || -> Vec<u32> {
        std::fs::read_to_string(&counter_path)
            .expect("read spawn counter")
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.parse().expect("counter line must be a pid"))
            .collect()
    };

    // `discover()` above already spawned generation 1.
    assert_eq!(
        read_pids().len(),
        1,
        "discover must spawn exactly one child"
    );

    // The FIRST call finds a LIVE session, but that session dies mid-flight
    // -- generation 1 exits WITHOUT ever answering. The ruling: this call
    // must fail closed, never silently retried against the fresh child the
    // death triggers a respawn of.
    let err = die_tool
        .invoke(call("die", serde_json::json!({})), ctx())
        .await
        .expect_err(
            "a call whose session dies mid-flight must fail closed, never silently succeed \
             via an in-call retry",
        );
    assert!(
        matches!(err, ToolError::Io { ref detail } if detail.contains("session died")),
        "expected ToolError::Io mentioning session died, got {err:?}"
    );
    // The respawn already ran as PART OF that failed call (never a second,
    // separate step this test has to trigger) -- generation 2 already
    // exists.
    let pids = read_pids();
    assert_eq!(
        pids.len(),
        2,
        "the failed call's own respawn must have spawned generation 2 already"
    );
    let pid_1 = pids[0];
    let pid_2_logged = pids[1];
    assert_ne!(
        pid_1, pid_2_logged,
        "generation 2 must be a genuinely different process from generation 1"
    );

    // The VERY NEXT call transparently recovers: it finds the ALREADY
    // fresh session in place and succeeds against generation 2, which
    // answers with its own pid -- matching what `SPAWN_COUNTER_FILE`
    // logged for generation 2, not merely differing from generation 1.
    let out = die_tool
        .invoke(call("die", serde_json::json!({})), ctx())
        .await
        .expect(
            "the very next call must transparently recover against the already-respawned session",
        );
    let pid_2_reported: u32 = first_text(&out)
        .parse()
        .expect("recovered call must report a pid");
    assert_eq!(
        pid_2_reported, pid_2_logged,
        "the recovered call must run against the SAME generation-2 process the respawn logged"
    );
    assert_ne!(
        pid_1, pid_2_reported,
        "the recovered call must run against a NEW child process, not the dead one"
    );
    // No THIRD generation was spawned by this recovering call.
    assert_eq!(
        read_pids().len(),
        2,
        "a call against an already-live session must not spawn anything further"
    );

    // The plugin's status contribution now names the respawn -- acceptance
    // criterion 6 ("make recovery visible, not silent"), read through the
    // SAME `Plugin::status_contributions` path every other plugin's
    // contributions surface through.
    let contributions = plugin.status_contributions();
    assert_eq!(contributions.len(), 1, "expected one status contribution");
    assert_eq!(
        contributions[0].status,
        conway::plugin::ResultStatus::Completed
    );
    assert!(
        contributions[0].value.contains("respawn"),
        "expected the contribution to name the respawn, got {:?}",
        contributions[0].value
    );
}

/// **The load-bearing test for board item `01M1ZR1DGZB8FB399SMG51RHP5`'s
/// correction.** A session death must never cause the SAME `tools/call` to
/// be sent more than once across ALL generations -- a doubled
/// `work_claim`/`work_complete`/`record_append` over this transport would
/// be durable and wrong, and nothing over stdio can tell a never-sent
/// request apart from one that ran to completion right before the process
/// died, so the only safe answer is to fail the call that hit the death
/// closed and let the caller decide to ask again (see
/// `McpPluginError::SessionDied`'s own doc, `docs/plugins/mcp.md`, and this
/// module's own doc for the argument in full).
///
/// `DIES_MID_FIRST_CALL_THEN_RECOVERS_SERVER`'s `REQUEST_LOG_FILE` records
/// every `tools/call` any generation ever receives, BEFORE that generation
/// decides whether to answer or die -- so a request silently resent to a
/// freshly-respawned generation is directly observable as a SECOND log
/// line, even though only one user-level `invoke` call happened. This test
/// asserts the log has exactly ONE line after that one failed call.
#[tokio::test]
async fn a_session_death_never_re_sends_the_in_flight_tools_call() {
    let dir = tempfile::tempdir().expect("tempdir");
    let counter_path = dir.path().join("spawn-count.txt");
    let request_log_path = dir.path().join("request-log.txt");
    let mut spec = common::spec_for_warmed(
        dir.path(),
        "die.py",
        common::DIES_MID_FIRST_CALL_THEN_RECOVERS_SERVER,
    )
    .await;
    spec.env.push((
        "SPAWN_COUNTER_FILE".to_string(),
        counter_path.display().to_string(),
    ));
    spec.env.push((
        "REQUEST_LOG_FILE".to_string(),
        request_log_path.display().to_string(),
    ));

    let plugin = McpPlugin::discover(spec)
        .await
        .expect("the handshake succeeds; only tools/call ever dies");
    let die_tool = plugin
        .tools()
        .into_iter()
        .find(|t| t.spec().name == conway::ToolName::new("die"))
        .expect("die tool");

    let read_request_count = || -> usize {
        std::fs::read_to_string(&request_log_path)
            .map(|s| s.lines().filter(|l| !l.is_empty()).count())
            .unwrap_or(0)
    };
    assert_eq!(
        read_request_count(),
        0,
        "no tools/call has been sent before this test's own call"
    );

    // ONE user-level call. Generation 1 receives it (logging it), dies
    // without answering, and the plugin respawns generation 2 -- but this
    // call must fail closed, not retry against generation 2.
    let err = die_tool
        .invoke(call("die", serde_json::json!({})), ctx())
        .await
        .expect_err("a call whose session dies mid-flight must fail closed");
    assert!(matches!(err, ToolError::Io { .. }), "got {err:?}");

    assert_eq!(
        read_request_count(),
        1,
        "exactly ONE tools/call must have reached a server process for this ONE user-level \
         invoke() call -- more than one means the dead session's request was silently \
         re-sent to the respawned child, the exact double-execution hazard this correction \
         forbids"
    );
}

// ---------------------------------------------------------------------
// `idempotentHint`-conditioned retry (a ruling from gRPC's client-retry
// rule, applied through MCP's own opt-in): a tool whose `tools/list` entry
// declares `annotations.idempotentHint: true` gets the ONE retry a dying
// mid-call session otherwise never gets; every other tool keeps the
// unconditional fail-closed default. The three tests below share ONE
// fixture script byte-for-byte (`DIES_MID_FIRST_CALL_ANNOTATED_SERVER`),
// differing ONLY in the `IDEMPOTENT_HINT` env var that fixture reads to
// decide what to put in its own `tools/list` answer -- so "retried" vs
// "not retried" is proven against a genuinely identical fixture, not two
// fixtures that merely look similar. See each test's own doc for exactly
// what a wrong implementation each one would catch, and why one alone
// could not tell a working gate from one that is always open.
// ---------------------------------------------------------------------

/// **The "gate opens" half of the required pair.** A tool that declared
/// `idempotentHint: true` at discover time, whose session dies mid-call
/// (generation 1 exits without answering, exactly like
/// `DIES_MID_FIRST_CALL_THEN_RECOVERS_SERVER`'s own proof), gets its
/// identical request RESENT against the fresh session the resulting
/// respawn puts in place, through the ONE `tools_call` call site
/// `crate::McpTool::invoke` has -- and the call SUCCEEDS, reporting
/// generation 2's own pid, never generation 1's.
///
/// **What this test alone could NOT catch, and what it's paired with
/// (below) to catch instead:** this test alone cannot distinguish "the
/// gate correctly opened because `idempotentHint` was `true`" from "there
/// is no gate at all -- every dying call is unconditionally retried now."
/// A regression that deleted the `retryable` check entirely (retrying
/// EVERY tool, annotated or not) would still pass this test. That is
/// exactly what `unannotated_tool_is_not_retried_after_a_mid_call_death_
/// against_the_identical_fixture` (immediately below) exists to catch: it
/// runs the byte-identical fixture with the annotation simply absent and
/// asserts the retry does NOT happen. Only the pair together proves a
/// working conditional gate rather than an always-open one.
#[tokio::test]
async fn idempotent_hint_true_retries_the_dying_call_against_the_fresh_session_and_it_succeeds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let counter_path = dir.path().join("spawn-count.txt");
    let request_log_path = dir.path().join("request-log.txt");
    let mut spec = common::spec_for_warmed(
        dir.path(),
        "die.py",
        common::DIES_MID_FIRST_CALL_ANNOTATED_SERVER,
    )
    .await;
    spec.env.push((
        "SPAWN_COUNTER_FILE".to_string(),
        counter_path.display().to_string(),
    ));
    spec.env.push((
        "REQUEST_LOG_FILE".to_string(),
        request_log_path.display().to_string(),
    ));
    spec.env
        .push(("IDEMPOTENT_HINT".to_string(), "true".to_string()));

    let plugin = McpPlugin::discover(spec)
        .await
        .expect("the handshake succeeds; only tools/call ever dies");
    let die_tool = plugin
        .tools()
        .into_iter()
        .find(|t| t.spec().name == conway::ToolName::new("die"))
        .expect("die tool");

    let read_pids = || -> Vec<u32> {
        std::fs::read_to_string(&counter_path)
            .expect("read spawn counter")
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.parse().expect("counter line must be a pid"))
            .collect()
    };
    let read_request_count = || -> usize {
        std::fs::read_to_string(&request_log_path)
            .map(|s| s.lines().filter(|l| !l.is_empty()).count())
            .unwrap_or(0)
    };

    assert_eq!(
        read_pids().len(),
        1,
        "discover must spawn exactly one child"
    );

    // ONE user-level call. Generation 1 receives it, dies mid-flight, the
    // plugin respawns generation 2 -- and because `die` declared
    // `idempotentHint: true`, the SAME request is resent against
    // generation 2 and succeeds, all within this ONE `invoke()`.
    let out = die_tool
        .invoke(call("die", serde_json::json!({})), ctx())
        .await
        .expect(
            "an idempotentHint:true tool's dying call must be retried against the fresh \
             session and succeed",
        );

    let pids = read_pids();
    assert_eq!(
        pids.len(),
        2,
        "exactly one respawn (generation 2) must have happened for this one call"
    );
    let pid_1 = pids[0];
    let pid_2 = pids[1];
    let reported_pid: u32 = first_text(&out)
        .parse()
        .expect("the retried call must report a pid");
    assert_eq!(
        reported_pid, pid_2,
        "the retried call must have run against generation 2, not generation 1"
    );
    assert_ne!(pid_1, reported_pid);

    assert_eq!(
        read_request_count(),
        2,
        "the fixture's request log must show TWO tools/call deliveries for this ONE \
         invoke() call -- one to generation 1 (which died before answering) and one \
         (the retry) to generation 2 -- proving the retry actually happened at the wire \
         level, not merely that the call happened to succeed"
    );

    // No THIRD generation was spawned by the retry succeeding.
    assert_eq!(read_pids().len(), 2);
}

/// **The "gate stays closed" half of the required pair, absent-annotation
/// case.** The BYTE-IDENTICAL fixture as the test above, with
/// `IDEMPOTENT_HINT` simply unset -- so the `die` tool's `tools/list` entry
/// carries no `annotations` object at all, MCP's own documented default
/// ("unannotated is unsafe to retry"). The dying call must fail closed,
/// never resent, exactly as it did before this item existed; the VERY NEXT
/// call (a separate `invoke()`) must still transparently recover against
/// the already-respawned session, unchanged.
///
/// **What this test alone could NOT catch, and what it's paired with
/// (above) to catch instead:** this test alone cannot distinguish "the gate
/// correctly stayed closed because `idempotentHint` was absent" from "the
/// retry exception was never actually wired up at all -- nothing is ever
/// retried, annotated or not." A regression that left `is_idempotent`
/// always returning `false` (e.g. the field never gets threaded from
/// `wire::ListedTool` into `SessionSlot`) would still pass this test. That
/// is exactly what `idempotent_hint_true_retries_the_dying_call_against_
/// the_fresh_session_and_it_succeeds` (above) exists to catch: it runs the
/// SAME fixture with the annotation set `true` and asserts the retry DOES
/// happen. Only the pair together proves a working conditional gate rather
/// than one that is always closed.
#[tokio::test]
async fn unannotated_tool_is_not_retried_after_a_mid_call_death_against_the_identical_fixture() {
    let dir = tempfile::tempdir().expect("tempdir");
    let counter_path = dir.path().join("spawn-count.txt");
    let request_log_path = dir.path().join("request-log.txt");
    let mut spec = common::spec_for_warmed(
        dir.path(),
        "die.py",
        common::DIES_MID_FIRST_CALL_ANNOTATED_SERVER,
    )
    .await;
    spec.env.push((
        "SPAWN_COUNTER_FILE".to_string(),
        counter_path.display().to_string(),
    ));
    spec.env.push((
        "REQUEST_LOG_FILE".to_string(),
        request_log_path.display().to_string(),
    ));
    // `IDEMPOTENT_HINT` deliberately NOT set -- the "absent" case.

    let plugin = McpPlugin::discover(spec)
        .await
        .expect("the handshake succeeds; only tools/call ever dies");
    let die_tool = plugin
        .tools()
        .into_iter()
        .find(|t| t.spec().name == conway::ToolName::new("die"))
        .expect("die tool");

    let read_spawn_count = || -> usize {
        std::fs::read_to_string(&counter_path)
            .map(|s| s.lines().filter(|l| !l.is_empty()).count())
            .unwrap_or(0)
    };
    let read_request_count = || -> usize {
        std::fs::read_to_string(&request_log_path)
            .map(|s| s.lines().filter(|l| !l.is_empty()).count())
            .unwrap_or(0)
    };

    let err = die_tool
        .invoke(call("die", serde_json::json!({})), ctx())
        .await
        .expect_err(
            "an unannotated tool's dying call must fail closed, never retried, exactly as \
             the pre-this-item default",
        );
    assert!(
        matches!(err, ToolError::Io { ref detail } if detail.contains("session died")),
        "expected ToolError::Io mentioning session died, got {err:?}"
    );
    assert_eq!(
        read_request_count(),
        1,
        "exactly ONE tools/call may have reached a server process -- an unannotated tool \
         must never be retried, so no second delivery to generation 2 may appear here"
    );
    assert_eq!(
        read_spawn_count(),
        2,
        "the respawn itself must still have happened (generation 2 exists) -- only the \
         RETRY is gated on the annotation, never the respawn"
    );

    // The very next call still transparently recovers against the
    // already-respawned session -- the no-retry rule for THIS call does not
    // regress the ordinary respawn-then-next-call-recovers behavior.
    let out = die_tool
        .invoke(call("die", serde_json::json!({})), ctx())
        .await
        .expect("the next call must recover against the already-respawned session");
    assert!(!first_text(&out).is_empty());
    assert_eq!(
        read_spawn_count(),
        2,
        "the recovering call must run against the already-respawned generation 2, not \
         spawn a third generation"
    );
}

/// **The explicit-`false` variant of the "gate stays closed" half.** The
/// BYTE-IDENTICAL fixture again, with `IDEMPOTENT_HINT=false` -- the
/// `die` tool's `tools/list` entry now carries `annotations: {"idempotentHint":
/// false}` explicitly, rather than omitting `annotations` altogether. The
/// spec (`crate::wire::ListedTool::idempotent_hint`'s own doc) requires
/// absent, `null`, and explicit `false` to ALL collapse to "do not retry" --
/// this test is the one that would catch a parser that only checked
/// "is `idempotentHint` present" (a bug the absent-case test above cannot
/// catch, since an absent field and a present-but-`false` field parse
/// identically ONLY if the parser reads the boolean itself, not merely
/// whether the key exists).
#[tokio::test]
async fn explicit_idempotent_hint_false_is_not_retried_after_a_mid_call_death() {
    let dir = tempfile::tempdir().expect("tempdir");
    let counter_path = dir.path().join("spawn-count.txt");
    let request_log_path = dir.path().join("request-log.txt");
    let mut spec = common::spec_for_warmed(
        dir.path(),
        "die.py",
        common::DIES_MID_FIRST_CALL_ANNOTATED_SERVER,
    )
    .await;
    spec.env.push((
        "SPAWN_COUNTER_FILE".to_string(),
        counter_path.display().to_string(),
    ));
    spec.env.push((
        "REQUEST_LOG_FILE".to_string(),
        request_log_path.display().to_string(),
    ));
    spec.env
        .push(("IDEMPOTENT_HINT".to_string(), "false".to_string()));

    let plugin = McpPlugin::discover(spec)
        .await
        .expect("the handshake succeeds; only tools/call ever dies");
    let die_tool = plugin
        .tools()
        .into_iter()
        .find(|t| t.spec().name == conway::ToolName::new("die"))
        .expect("die tool");

    let read_request_count = || -> usize {
        std::fs::read_to_string(&request_log_path)
            .map(|s| s.lines().filter(|l| !l.is_empty()).count())
            .unwrap_or(0)
    };

    let err = die_tool
        .invoke(call("die", serde_json::json!({})), ctx())
        .await
        .expect_err(
            "an explicit idempotentHint:false tool's dying call must fail closed, never \
             retried",
        );
    assert!(
        matches!(err, ToolError::Io { ref detail } if detail.contains("session died")),
        "expected ToolError::Io mentioning session died, got {err:?}"
    );
    assert_eq!(
        read_request_count(),
        1,
        "exactly ONE tools/call may have reached a server process for an explicit \
         idempotentHint:false tool"
    );
}

/// **Acceptance criterion 2.** A crash-looping server -- one that dies on
/// EVERY generation's first `tools/call`, never merely once -- stops being
/// auto-respawned once `MAX_AUTO_RESPAWNS` is spent, and every call after
/// that returns `SessionDied` without spawning anything further. Proven
/// against the OBSERVABLE outcome, not an intermediate signal: the fixture
/// itself records one line per spawn to a counter file this test reads, so
/// "stopped respawning" is checked by the counter no longer growing, not by
/// merely asserting the error variant.
#[tokio::test]
async fn a_crash_looping_server_stops_being_auto_respawned_past_the_bound() {
    let dir = tempfile::tempdir().expect("tempdir");
    let counter_path = dir.path().join("spawn-count.txt");
    let mut spec = common::spec_for_warmed(dir.path(), "loop.py", common::ALWAYS_DIES_SERVER).await;
    spec.env.push((
        "SPAWN_COUNTER_FILE".to_string(),
        counter_path.display().to_string(),
    ));

    let plugin = McpPlugin::discover(spec)
        .await
        .expect("the handshake succeeds; only tools/call ever dies");
    let boom_tool = plugin
        .tools()
        .into_iter()
        .find(|t| t.spec().name == conway::ToolName::new("boom"))
        .expect("boom tool");

    let read_spawn_count = || -> usize {
        std::fs::read_to_string(&counter_path)
            .map(|s| s.lines().filter(|l| !l.is_empty()).count())
            .unwrap_or(0)
    };

    // The initial `discover()` above already spawned generation 1.
    assert_eq!(
        read_spawn_count(),
        1,
        "discover must spawn exactly one child"
    );

    // Every call dies and gets auto-respawned until the bound is spent.
    // Each of these MAX_AUTO_RESPAWNS calls consumes exactly one respawn
    // (spawns one MORE child, retries once against it, and that retry dies
    // too -- ALWAYS_DIES_SERVER never answers).
    for attempt in 1..=MAX_AUTO_RESPAWNS {
        let err = boom_tool
            .invoke(call("boom", serde_json::json!({})), ctx())
            .await
            .expect_err("a crash-looping server must fail, never hang or silently succeed");
        assert!(
            matches!(err, ToolError::Io { ref detail } if detail.contains("session died")),
            "attempt {attempt}: expected ToolError::Io mentioning session died, got {err:?}"
        );
    }
    // 1 (discover) + MAX_AUTO_RESPAWNS (one fresh child per bounded respawn).
    let capped_at = read_spawn_count();
    assert_eq!(
        capped_at,
        1 + MAX_AUTO_RESPAWNS as usize,
        "spawn count must be capped at 1 + MAX_AUTO_RESPAWNS once the bound is spent"
    );

    // Further calls, past the bound, must fail WITHOUT spawning anything
    // further -- proving the bound is a permanent posture change, not a
    // one-time pause that resumes respawning later.
    for _ in 0..3 {
        let err = boom_tool
            .invoke(call("boom", serde_json::json!({})), ctx())
            .await
            .expect_err("past the bound, every call must still fail closed");
        assert!(
            matches!(err, ToolError::Io { ref detail } if detail.contains("session died")),
            "expected ToolError::Io mentioning session died, got {err:?}"
        );
    }
    assert_eq!(
        read_spawn_count(),
        capped_at,
        "no additional child must be spawned once the auto-respawn bound is exhausted"
    );
}

/// **Acceptance criterion 3.** A respawned server whose `tools/list` no
/// longer matches the tool set this plugin registered at `discover` time
/// produces a typed error naming the difference -- the mismatched tool is
/// never silently added or dropped from what the runtime believes this
/// plugin offers.
#[tokio::test]
async fn a_respawned_server_whose_tool_set_changed_fails_the_call_with_a_typed_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let counter_path = dir.path().join("spawn-count.txt");
    let mut spec = common::spec_for_warmed(
        dir.path(),
        "shifting.py",
        common::TOOL_SET_CHANGES_ON_RESPAWN_SERVER,
    )
    .await;
    spec.env.push((
        "SPAWN_COUNTER_FILE".to_string(),
        counter_path.display().to_string(),
    ));

    let plugin = McpPlugin::discover(spec)
        .await
        .expect("generation 1's handshake succeeds; only tools/call dies");
    // Generation 1 declares BOTH tools -- confirms the plugin's registered
    // set is the FULL original set before any respawn ever runs.
    let manifest = plugin.manifest();
    assert_eq!(manifest.tools.len(), 2);

    let stable_tool = plugin
        .tools()
        .into_iter()
        .find(|t| t.spec().name == conway::ToolName::new("stable"))
        .expect("stable tool");

    // Generation 1 dies mid-call without answering, forcing a respawn.
    // Generation 2's `tools/list` drops `only_on_first` -- the mismatch this
    // test exists to catch.
    let err = stable_tool
        .invoke(call("stable", serde_json::json!({})), ctx())
        .await
        .expect_err("a shrunk tool set on respawn must fail the call, never succeed silently");
    assert!(
        matches!(
            err,
            ToolError::Internal { ref detail }
                if detail.contains("tool set changed")
                    && detail.contains("only_on_first")
        ),
        "expected a typed error naming the missing tool, got {err:?}"
    );

    // The typed error is surfaced -- not a fallback to whatever the fresh
    // server offers -- so `manifest.tools` (read from the ORIGINAL
    // discovery, never mutated by a respawn) still names both tools.
    assert_eq!(plugin.manifest().tools.len(), 2);
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
    let spec = common::spec_for_warmed(dir.path(), "sleepy.py", common::SHORT_SLEEPY_SERVER).await;
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

/// **Operator-reported, 2026-08-30.** A server that is slow to become ready
/// must still open a session, even when the per-call deadline is far shorter
/// than its startup takes.
///
/// The concrete case: installing ideate under `[plugins].claude_compat` left
/// conway unable to start at all. Claude Code installs a plugin by cloning it
/// with no build step and bundles no runtime, so a plugin whose server is
/// compiled builds itself on first launch -- ideate's `bin/ideate-mcp` runs
/// `npm install && npm run build` before exec'ing Node. conway applied its
/// 5s PER-CALL deadline to the opening handshake, which that first launch
/// cannot fit inside.
///
/// The fixture sleeps 1.5s before answering `initialize` and the per-call
/// deadline here is 300ms, so this test FAILS on the old single-timeout code
/// and passes only once the handshake has its own budget. The two bounds are
/// deliberately far apart: an accidental fallback to `timeout_ms` cannot pass.
#[tokio::test]
async fn a_slow_starting_server_still_opens_under_the_startup_budget() {
    let dir = tempfile::tempdir().expect("tempdir");
    // 300ms per-call deadline -- five times SHORTER than the fixture's own
    // 1.5s startup sleep.
    let mut spec =
        common::spec_with_timeout(dir.path(), "slow_start.py", common::SLOW_START_SERVER, 300)
            .await;
    spec.startup_timeout_ms = 30_000;

    let plugin = McpPlugin::discover(spec).await.expect(
        "a server that takes longer than the PER-CALL deadline to become ready must \
                 still open: the handshake is bounded by startup_timeout_ms, not timeout_ms",
    );

    let tools = plugin.tools();
    assert_eq!(tools.len(), 1, "the slow starter's tool must be registered");
    assert_eq!(tools[0].spec().name, conway::ToolName::new("ping"));
}

/// The startup budget is not unbounded: a server that never answers
/// `initialize` still fails closed, it just fails on the startup deadline
/// rather than the per-call one. Without this, "give startup more room" would
/// be indistinguishable from "let a wedged server hang forever".
#[tokio::test]
async fn a_server_that_never_becomes_ready_still_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    // The same slow-start fixture, told to sleep far past the startup budget.
    // `SLEEPY_SERVER` would be the wrong fixture here: it sleeps on
    // `tools/call` and answers `initialize` promptly, so the session opens and
    // the assertion never exercises the startup deadline at all.
    let mut spec = common::spec_with_timeout(
        dir.path(),
        "never_ready.py",
        common::SLOW_START_SERVER,
        30_000,
    )
    .await;
    spec.env
        .push(("SLOW_START_SECONDS".to_string(), "30".to_string()));
    spec.startup_timeout_ms = 400;

    let err = McpPlugin::discover(spec)
        .await
        .expect_err("a server that never answers initialize must fail closed");
    assert!(
        matches!(err, McpPluginError::TimedOut { .. }),
        "expected TimedOut on the STARTUP deadline, got {err:?}"
    );
}
