//! The `observe/1` and `status.declare/1` / `status/1` wire points (board
//! item `01M03VKQ738DTGHHK2C4RWXC0E`): proof that two OBSERVER-class wire
//! points engage over the persistent NDJSON transport AFTER `initialize/1`
//! and `permission.policy/1`, both ONE-WAY once engaged, both DEGRADING on an
//! unsupported version (the observer rule, the OPPOSITE of
//! `permission.policy/1`'s participant refusal). Mirrors `tests/
//! permission_policy.rs`'s own mock-plugin-process pattern (fixtures in
//! `tests/common/mod.rs`): every fixture here is a plain Python 3 script this
//! suite writes into a fresh temp dir at run time, authored outside this
//! workspace's dependency graph.
//!
//! The four acceptance criteria, mapped to the tests below:
//!
//! - **Criterion 1** -- a persistent plugin subscribing to `observe/1`
//!   receives `Event`s (the host's one-way notifications reach the plugin's
//!   stdin); an unknown `Event` tag is IGNORED, not a session error
//!   (`a_subscribed_observe_plugin_receives_events_and_stays_alive`), and a
//!   `Tags` selector drops a non-matching `Event` at the host before it reaches
//!   the wire, keeping the session alive
//!   (`a_tags_selector_drops_non_matching_events_and_keeps_the_session_alive`).
//! - **Criterion 2** -- a plugin declaring `status.declare/1` has its pushed
//!   `status/1` notifications surface in the host's status path
//!   (`Plugin::status_contributions`); an unknown `ResultStatus` tag
//!   degrades to `Failed`, never `Completed`
//!   (`a_status_declaring_plugin_surfaces_contributions_with_unknown_degraded_to_failed`).
//! - **Criterion 3** -- an unsupported `observe/1` OR `status.declare/1`
//!   version DEGRADES (loads WITHOUT the point, warns), NOT refused -- the
//!   observer rule, contrasted with `permission.policy/1`'s participant
//!   refusal (`an_unsupported_observe_version_degrades_not_refuses` and
//!   `an_unsupported_status_declare_version_degrades_not_refuses`).
//! - **Criterion 4** -- the `docs/plugins/hooks.md` observe and status rows
//!   are updated from designed-not-built to implemented (verified by reading
//!   the doc; not a test in this file).
//! - **Presence-gating** -- a plugin that declares NEITHER point loads
//!   normally and contributes no observe sink / no status
//!   (`a_plugin_declaring_neither_point_loads_normally_with_no_observer_surfaces`).

mod common;

use std::sync::Arc;
use std::time::Duration;

use conway::plugin::{Plugin as _, ResultStatus, ToolCall, ToolCtx};
use conway::AgentId;
use conway_plugin_subprocess::{SubprocessPlugin, SubprocessTransport};
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

/// Finds the declared tool named `name` on `plugin`, panicking if absent.
fn tool_named(plugin: &SubprocessPlugin, name: &str) -> Arc<dyn conway::plugin::Tool> {
    plugin
        .tools()
        .into_iter()
        .find(|t| t.spec().name.as_str() == name)
        .unwrap_or_else(|| panic!("plugin declares a tool named {name}"))
}

// ---------------------------------------------------------------------
// Acceptance criterion 1 -- observe/1: a subscribed plugin receives Events;
// an unknown Event tag is ignored, not a session error.
// ---------------------------------------------------------------------

/// **Criterion 1.** A persistent plugin that declares `observe/1` at version
/// 1 and subscribes with `["*"]` must RECEIVE the `Event`s the host forwards
/// as one-way `observe/1` notifications on its stdin -- proven end-to-end by
/// the fixture recording every received notification to a file the test
/// reads. After receiving a notification, the session must STILL be alive (an
/// observer changes nothing by construction, so an unknown `Event` tag the
/// plugin receives is IGNORED, not a session error): the test invokes `greet`
/// over `tool/1` AFTER the notification and asserts it answers normally.
#[tokio::test]
async fn a_subscribed_observe_plugin_receives_events_and_stays_alive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let observe_log = dir.path().join("observe.log");
    // The child inherits the parent's env at spawn time, so set the log path
    // BEFORE `discover` spawns the persistent child. Only THIS test uses
    // `OBSERVE_LOG`, so a parallel test cannot clobber it.
    std::env::set_var("OBSERVE_LOG", &observe_log);
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "observe.py",
        common::PERSISTENT_OBSERVE_PLUGIN,
    )
    .await;
    assert_eq!(spec.transport, SubprocessTransport::Persistent);

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("an observe/1 plugin loads");

    let sink = plugin
        .observe_sink()
        .expect("a plugin that declared observe/1 at v1 engaged an observe sink");

    // Emit a known `Event` (TurnStarted) through the sink -- the host's
    // forwarding task (the facade's bus->sink bridge) calls this same
    // `emit`; here we drive it directly to prove the sink->writer->stdin
    // path. The host filters by the selector BEFORE forwarding; `["*"]`
    // matches every event.
    sink.emit(conway::plugin::Event::TurnStarted { turn: 1 });

    // The writer task drains the bounded channel and writes the notification
    // line asynchronously; give it a moment to flush.
    for _ in 0..50 {
        if observe_log.exists()
            && std::fs::read_to_string(&observe_log)
                .unwrap_or_default()
                .lines()
                .count()
                >= 1
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let recorded = std::fs::read_to_string(&observe_log).unwrap_or_default();
    let recorded_line = recorded.lines().next().unwrap_or("");
    let recorded_obj: serde_json::Value =
        serde_json::from_str(recorded_line).expect("the recorded notification is valid JSON");
    assert_eq!(
        recorded_obj.get("op").and_then(|v| v.as_str()),
        Some("observe/1"),
        "the plugin recorded an observe/1 notification: {recorded}"
    );
    assert_eq!(
        recorded_obj.get("event").and_then(|v| v.as_str()),
        Some("turn_started"),
        "the notification carries the Event's own tag: {recorded}"
    );
    assert_eq!(
        recorded_obj.get("turn").and_then(|v| v.as_u64()),
        Some(1),
        "the notification carries the Event's own field: {recorded}"
    );

    // The session is STILL ALIVE after receiving the notification -- an
    // observer must not error the session (an unknown Event tag is ignored,
    // not a session failure). Invoke `greet` over `tool/1` and assert it
    // answers normally.
    let greet = tool_named(&plugin, "greet");
    let output = greet
        .invoke(call("greet", serde_json::json!({"name": "world"})), ctx())
        .await
        .expect("tool/1 still answers after an observe notification");
    assert!(
        output
            .blocks
            .iter()
            .any(|b| matches!(b, conway::plugin::ContentBlock::Text { text, .. } if text.contains("hello, world"))),
        "the session is alive: {output:?}"
    );

    std::env::remove_var("OBSERVE_LOG");
}

/// **Criterion 1 (filter half).** The `["*"]` test above proves a subscribed
/// plugin RECEIVES a matching `Event` and stays alive. This test closes the
/// half the wire-parser unit tests leave end-to-end-untested: a plugin that
/// subscribes with a `Tags(["turn_started"])` selector must receive the
/// MATCHING `Event::TurnStarted` but NOT receive a non-matching
/// `Event::TextDelta` -- the host filters by the declared selector BEFORE
/// forwarding, so a non-matching event is dropped at the host and never reaches
/// the plugin's stdin. Asserting the plugin records EXACTLY one notification
/// (the matching one) -- after a grace window long enough for a
/// wrongly-forwarded `text_delta` to have arrived -- catches a regression that
/// inverted the selector filter (forwarding the non-matching and dropping the
/// matching, or skipping the filter entirely and forwarding both). The session
/// must STILL be alive after both emits: `greet` over `tool/1` answers
/// normally. The unit test `build_observe_notification_merges_op_and_filters_
/// by_selector` covers the filter's return value in isolation; this test covers
/// the session-level "a filtered event is dropped AND the session stays alive"
/// behavior the unit test cannot reach.
#[tokio::test]
async fn a_tags_selector_drops_non_matching_events_and_keeps_the_session_alive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let observe_log = dir.path().join("observe-tags.log");
    // A DISTINCT env var from the `["*"]` test's `OBSERVE_LOG` so the two
    // parallel tests cannot clobber each other's log path.
    std::env::set_var("OBSERVE_TAGS_LOG", &observe_log);
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "observe_tags.py",
        common::PERSISTENT_OBSERVE_TAGS_PLUGIN,
    )
    .await;
    assert_eq!(spec.transport, SubprocessTransport::Persistent);

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("a Tags-selector observe/1 plugin loads");

    let sink = plugin
        .observe_sink()
        .expect("a plugin that declared observe/1 at v1 engaged an observe sink");

    // Emit a MATCHING event (tag `turn_started`) -- the host's forwarding task
    // filters by the declared `Tags(["turn_started"])` selector BEFORE
    // writing; this one passes the filter and reaches the plugin's stdin.
    sink.emit(conway::plugin::Event::TurnStarted { turn: 1 });
    // Emit a NON-MATCHING event (tag `text_delta`) -- this one fails the
    // selector filter and is dropped at the host, never reaching the plugin.
    sink.emit(conway::plugin::Event::TextDelta {
        text: "this must be filtered".into(),
    });

    // Wait for the matching notification to arrive (the writer task drains the
    // bounded channel and writes the line asynchronously).
    for _ in 0..50 {
        if observe_log.exists()
            && std::fs::read_to_string(&observe_log)
                .unwrap_or_default()
                .lines()
                .count()
                >= 1
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Give a grace window long enough for a WRONGLY-forwarded `text_delta`
    // (had the filter been inverted or absent) to have arrived and been
    // recorded. The filtered event is dropped at the host, so it never will.
    tokio::time::sleep(Duration::from_millis(250)).await;

    let recorded = std::fs::read_to_string(&observe_log).unwrap_or_default();
    let lines: Vec<&str> = recorded.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "a Tags([\"turn_started\"]) selector forwards ONLY the matching event; \
         the non-matching text_delta is filtered at the host and never recorded: \
         {recorded}"
    );
    let recorded_obj: serde_json::Value =
        serde_json::from_str(lines[0]).expect("the recorded notification is valid JSON");
    assert_eq!(
        recorded_obj.get("op").and_then(|v| v.as_str()),
        Some("observe/1"),
        "the recorded line is an observe/1 notification: {recorded}"
    );
    assert_eq!(
        recorded_obj.get("event").and_then(|v| v.as_str()),
        Some("turn_started"),
        "the matching event was forwarded: {recorded}"
    );

    // The session is STILL ALIVE after both emits -- filtering a non-matching
    // event must not error the session. Invoke `greet` over `tool/1`.
    let greet = tool_named(&plugin, "greet");
    let output = greet
        .invoke(call("greet", serde_json::json!({"name": "world"})), ctx())
        .await
        .expect("tool/1 still answers after a filtered notification");
    assert!(
        output
            .blocks
            .iter()
            .any(|b| matches!(b, conway::plugin::ContentBlock::Text { text, .. } if text.contains("hello, world"))),
        "the session is alive: {output:?}"
    );

    std::env::remove_var("OBSERVE_TAGS_LOG");
}

// ---------------------------------------------------------------------
// Acceptance criterion 2 -- status.declare/1 / status/1: pushed
// contributions surface in the host status path; unknown ResultStatus ->
// Failed.
// ---------------------------------------------------------------------

/// **Criterion 2.** A persistent plugin that declares `status.declare/1` at
/// version 1 and PUSHES `status/1` notifications must have them surface in
/// the host's status path -- `Plugin::status_contributions`, the polled
/// snapshot the facade reads. A KNOWN `ResultStatus` tag (`"completed"`)
/// surfaces as `ResultStatus::Completed`; an UNKNOWN tag (`"quantum"`)
/// degrades to `ResultStatus::Failed` (the compatibility table's
/// `ResultStatus` row, never `Completed`), carrying the unknown tag in the
/// `error` string so the degradation is auditable. The fixture pushes both
/// right after answering the engagement; the test polls the snapshot until
/// both keys arrive.
#[tokio::test]
async fn a_status_declaring_plugin_surfaces_contributions_with_unknown_degraded_to_failed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "status.py",
        common::PERSISTENT_STATUS_PLUGIN,
    )
    .await;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("a status.declare/1 plugin loads");

    // The plugin pushes `status/1` lines immediately after the engagement;
    // the host's reader + handler drain them asynchronously. Poll the
    // snapshot until both keys arrive (bounded retry -- never hangs).
    let mut build = None;
    let mut lint = None;
    for _ in 0..100 {
        let contribs = plugin.status_contributions();
        build = contribs.iter().find(|c| c.key == "build").cloned();
        lint = contribs.iter().find(|c| c.key == "lint").cloned();
        if build.is_some() && lint.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let build = build.expect("the build contribution surfaced in the status snapshot");
    assert_eq!(
        build.status,
        ResultStatus::Completed,
        "a known `completed` tag surfaces as Completed"
    );
    assert_eq!(build.value, "green");

    let lint = lint.expect("the lint contribution surfaced in the status snapshot");
    match &lint.status {
        ResultStatus::Failed { error } => {
            assert!(
                error.contains("quantum"),
                "an unknown ResultStatus tag degrades to Failed, naming the tag: {error}"
            );
        }
        other => {
            panic!("unknown ResultStatus tag degrades to Failed (never Completed), got {other:?}")
        }
    }

    // The session is STILL ALIVE after pushing notifications -- an observer
    // must not error the session.
    let greet = tool_named(&plugin, "greet");
    greet
        .invoke(call("greet", serde_json::json!({})), ctx())
        .await
        .expect("tool/1 still answers after status notifications");
}

// ---------------------------------------------------------------------
// Acceptance criterion 3 -- an unsupported observe/1 OR status.declare/1
// version DEGRADES (loads without the point, warns), NOT refused -- the
// observer rule, the OPPOSITE of permission.policy/1's participant refusal.
// ---------------------------------------------------------------------

/// **Criterion 3 (observe).** A plugin that declares `observe/1` at version 2
/// (the host speaks version 1) must DEGRADE -- LOAD without the point and
/// warn -- NOT refuse. This is the observer rule, the OPPOSITE of
/// `permission.policy/1`'s participant refusal (which refuses an unsupported
/// version with `HandshakeRefused`, proven in `tests/permission_policy.rs`'s
/// `an_unsupported_permission_policy_version_refuses_to_load`). The plugin
/// still serves `tool/1`, and `observe_sink()` is `None` (no point engaged).
#[tokio::test]
async fn an_unsupported_observe_version_degrades_not_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "observe_ver.py",
        common::PERSISTENT_OBSERVE_VERSION_MISMATCH_PLUGIN,
    )
    .await;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("an unsupported observe/1 version DEGRADES (loads), not refuses");
    assert!(
        plugin.observe_sink().is_none(),
        "no observe sink is engaged for a degraded observe/1 point"
    );
    // The plugin still serves `tool/1` -- the degrade loads WITHOUT the
    // point, it does not refuse the whole plugin.
    let greet = tool_named(&plugin, "greet");
    greet
        .invoke(call("greet", serde_json::json!({})), ctx())
        .await
        .expect("tool/1 still answers after an observe/1 degrade");
}

/// **Criterion 3 (status).** A plugin that declares `status.declare/1` at
/// version 2 (the host speaks version 1) must DEGRADE -- LOAD without the
/// point and warn -- NOT refuse. Same observer rule as observe/1. The plugin
/// still serves `tool/1`, and `status_contributions()` is empty (no point
/// engaged, no `status/1` notifications routed).
#[tokio::test]
async fn an_unsupported_status_declare_version_degrades_not_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "status_ver.py",
        common::PERSISTENT_STATUS_VERSION_MISMATCH_PLUGIN,
    )
    .await;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("an unsupported status.declare/1 version DEGRADES (loads), not refuses");
    assert!(
        plugin.status_contributions().is_empty(),
        "no status contributions for a degraded status.declare/1 point"
    );
    let greet = tool_named(&plugin, "greet");
    greet
        .invoke(call("greet", serde_json::json!({})), ctx())
        .await
        .expect("tool/1 still answers after a status.declare/1 degrade");
}

// ---------------------------------------------------------------------
// Presence-gating -- a plugin that declares NEITHER point loads normally
// and contributes no observe sink / no status (advertising != requiring).
// ---------------------------------------------------------------------

/// A plugin that declares ONLY `tool/1` (not `observe/1`, not
/// `status.declare/1`) in its `initialize/1` answer must load NORMALLY -- the
/// host does NOT send an engagement request for either point,
/// `observe_sink()` is `None`, and `status_contributions()` is empty. This is
/// the "advertising a point means the host speaks it, not that the host
/// requires it" rule: the observer degrade is VERSION-gated (both speak the
/// point at incompatible versions), not presence-gated. Reuses the
/// handshake-ok fixture (which declares only `tool/1`).
#[tokio::test]
async fn a_plugin_declaring_neither_point_loads_normally_with_no_observer_surfaces() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "handshake_ok.py",
        common::PERSISTENT_HANDSHAKE_OK_PLUGIN,
    )
    .await;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("a plugin declaring neither observer point loads normally");
    assert!(
        plugin.observe_sink().is_none(),
        "a plugin that did not declare observe/1 contributes no observe sink"
    );
    assert!(
        plugin.status_contributions().is_empty(),
        "a plugin that did not declare status.declare/1 contributes no status"
    );
}
