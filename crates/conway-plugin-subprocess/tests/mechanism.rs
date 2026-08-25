//! Direct proof of `SubprocessPlugin`'s wire mechanism and its failure
//! modes -- spawn failure, timeout, nonzero exit, garbage output, and a
//! subprocess-declared error -- each asserted to fail CLOSED with a typed
//! error, never a hang and never a silent success. `tests/end_to_end.rs` is
//! the companion proof that the same mechanism reaches a real agent turn,
//! not merely a direct call.

mod common;

use std::sync::Arc;
use std::time::Duration;

use conway::plugin::{Plugin as _, ToolCall, ToolCtx, ToolError};
use conway::AgentId;
use conway_plugin_subprocess::{SubprocessPlugin, SubprocessPluginError, SubprocessPluginSpec};
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

// ---------------------------------------------------------------------
// Discovery -- `tool.spec/1`
// ---------------------------------------------------------------------

#[tokio::test]
async fn discover_builds_a_plugin_from_a_real_subprocess_manifest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for_warmed(dir.path(), "greet.py", common::GREET_PLUGIN).await;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery against a well-behaved fixture must succeed");

    let manifest = plugin.manifest();
    assert_eq!(manifest.id, "acme.greet");
    assert_eq!(manifest.version, "0.1.0");
    assert_eq!(manifest.tools, vec![conway::ToolName::new("greet")]);

    let tools = plugin.tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].spec().name, conway::ToolName::new("greet"));
}

#[tokio::test]
async fn discover_fails_closed_when_the_command_cannot_be_spawned() {
    let spec = SubprocessPluginSpec::new(
        "unspawnable",
        vec!["/nonexistent/path/does-not-exist-conway-test".to_string()],
    );

    let err = SubprocessPlugin::discover(spec)
        .await
        .expect_err("a nonexistent command must fail closed, never silently register zero tools");
    assert!(
        matches!(err, SubprocessPluginError::Spawn { .. }),
        "expected Spawn, got {err:?}"
    );
}

#[tokio::test]
async fn discover_fails_closed_on_timeout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut spec = common::spec_for(dir.path(), "sleepy.py", common::SLEEPY_PLUGIN);
    spec.timeout_ms = 500;

    let start = std::time::Instant::now();
    let err = SubprocessPlugin::discover(spec)
        .await
        .expect_err("a subprocess that never answers must time out, never hang the caller");
    assert!(
        matches!(err, SubprocessPluginError::TimedOut { after_ms: 500, .. }),
        "expected TimedOut{{after_ms: 500}}, got {err:?}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "discover must return promptly once the configured timeout elapses, not hang until the \
         fixture's own 10s sleep finishes: took {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn discover_fails_closed_on_nonzero_exit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for_warmed(dir.path(), "failing.py", common::FAILING_PLUGIN).await;

    let err = SubprocessPlugin::discover(spec)
        .await
        .expect_err("a nonzero exit must fail closed");
    assert!(
        matches!(
            err,
            SubprocessPluginError::NonzeroExit { code: Some(3), .. }
        ),
        "expected NonzeroExit{{code: Some(3)}}, got {err:?}"
    );
}

#[tokio::test]
async fn discover_fails_closed_on_garbage_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for_warmed(dir.path(), "garbage.py", common::GARBAGE_PLUGIN).await;

    let err = SubprocessPlugin::discover(spec)
        .await
        .expect_err("output that is not valid JSON must fail closed, never be read as zero tools");
    assert!(
        matches!(err, SubprocessPluginError::UnparseableAnswer { .. }),
        "expected UnparseableAnswer, got {err:?}"
    );
}

#[tokio::test]
async fn discover_rejects_a_manifest_declaring_zero_tools() {
    let dir = tempfile::tempdir().expect("tempdir");
    let empty_manifest = r#"#!/usr/bin/env python3
import sys, json
sys.stdin.read()
print(json.dumps({"id": "acme.empty", "version": "0.1.0", "tools": []}))
"#;
    let spec = common::spec_for_warmed(dir.path(), "empty.py", empty_manifest).await;

    let err = SubprocessPlugin::discover(spec)
        .await
        .expect_err("a manifest with zero tools must be refused, not silently accepted");
    assert!(
        matches!(err, SubprocessPluginError::InvalidManifest { .. }),
        "expected InvalidManifest, got {err:?}"
    );
}

#[tokio::test]
async fn discover_rejects_a_manifest_with_a_duplicate_tool_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dup_manifest = r#"#!/usr/bin/env python3
import sys, json
sys.stdin.read()
tool = {
    "name": "dup",
    "description": "d",
    "schema": {"type": "object"},
    "category": "read",
    "permission": "safe",
}
print(json.dumps({"id": "acme.dup", "version": "0.1.0", "tools": [tool, tool]}))
"#;
    let spec = common::spec_for_warmed(dir.path(), "dup.py", dup_manifest).await;

    let err = SubprocessPlugin::discover(spec)
        .await
        .expect_err("a duplicate declared tool name must be refused");
    assert!(
        matches!(err, SubprocessPluginError::InvalidManifest { .. }),
        "expected InvalidManifest, got {err:?}"
    );
}

// ---------------------------------------------------------------------
// Discovery -- `required_host_caps` on the wire (board item
// `01M03VJXARFHSDAGHFXGCWKJTY`)
// ---------------------------------------------------------------------

/// A `tool.spec/1` manifest declaring a KNOWN `required_host_caps` value
/// (`["subagent"]`) loads, and the declared cap is mapped verbatim into
/// `PluginManifest::required_host_caps`. (Whether the host then OFFERS the
/// cap is the `conway` builder's gate, proven in `crates/conway/tests/
/// builder.rs`; this proves only that the wire carries the field and
/// `discover` maps it.)
#[tokio::test]
async fn discover_maps_a_declared_known_host_cap_into_the_manifest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for_warmed(dir.path(), "cap.py", common::CAP_REQUIRED_PLUGIN).await;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("a manifest declaring a known required_host_caps value must load");
    let manifest = plugin.manifest();
    assert_eq!(manifest.id, "acme.needs-subagent");
    assert_eq!(
        manifest.required_host_caps,
        vec![conway::plugin::HostCapability::Subagent],
        "the declared cap maps verbatim into PluginManifest::required_host_caps"
    );
}

/// A `tool.spec/1` manifest declaring a previously-UNKNOWN
/// `required_host_caps` tag (`"quantum-cap"`) is still REFUSED -- but at the
/// host-capability gate, not at parse.
///
/// **This test used to assert the opposite location, and the relocation is
/// deliberate** (board item `01M0WWKA8K1E7JPK87J6RRQMZF`, which opened
/// `HostCapability` from a closed two-variant enum to a namespaced
/// vocabulary). Under the closed enum, `discover` could reject an unknown
/// tag because the CORE did not know the word. Under an open vocabulary
/// that rejection is impossible by construction: if parsing refused every
/// name the core has not blessed, no third party could ever declare a
/// capability, which is the entire point of opening it.
///
/// **The fail-closed guarantee is not weakened, it moved and got sharper.**
/// `discover` now resolves the tag to `HostCapability::Named`, and
/// `conway::HostCaps::check_manifest` refuses it with
/// `PluginError::MissingHostCapability` naming BOTH the plugin and the cap
/// -- a better error than the old `UnparseableAnswer`, and it answers the
/// semantically right question ("does this host offer that?") rather than
/// the accidental one ("has the core heard of that word?").
///
/// A MALFORMED tag -- one failing `validate_event_name`'s shape check --
/// still fails closed at parse, unchanged. Well-formed-but-unknown is the
/// only case that moved.
#[tokio::test]
async fn an_unknown_required_host_cap_is_refused_by_the_gate_not_by_the_parser() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec =
        common::spec_for_warmed(dir.path(), "unknown_cap.py", common::UNKNOWN_CAP_PLUGIN).await;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("a well-formed but unknown cap tag now parses -- see this test's own doc");
    let manifest = plugin.manifest();
    assert_eq!(
        manifest.required_host_caps,
        vec![conway::plugin::HostCapability::named("quantum-cap").expect("well-formed")],
        "the unknown tag resolves to a Named capability rather than failing to parse"
    );

    // The refusal itself, at its new home: a host offering BOTH built-in
    // caps still does not offer `quantum-cap`, so the plugin is refused --
    // naming both sides. Built explicitly rather than from config so the
    // assertion is about the cap being unoffered, not about which caps a
    // particular config happened to enable.
    let host = conway::HostCaps::with_capabilities([
        conway::plugin::HostCapability::Subagent,
        conway::plugin::HostCapability::PersistentTransport,
    ]);
    let err = host
        .check_manifest(&manifest)
        .expect_err("an unoffered required cap must still refuse the plugin");
    let rendered = err.to_string();
    assert!(
        rendered.contains("acme.needs-quantum") && rendered.contains("quantum-cap"),
        "the refusal must name both the plugin and the cap, got: {rendered}"
    );
}

/// A `tool.spec/1` manifest that OMITS `required_host_caps` entirely still
/// loads (`#[serde(default)]` parses the omitted field as empty -- "needs
/// nothing the host might lack"), so existing plugins that predate the field
/// are unaffected. The built-in `GREET_PLUGIN` fixture omits the field.
#[tokio::test]
async fn discover_loads_a_manifest_omitting_required_host_caps_as_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for_warmed(dir.path(), "greet.py", common::GREET_PLUGIN).await;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("a manifest omitting required_host_caps must load (#[serde(default)])");
    assert!(
        plugin.manifest().required_host_caps.is_empty(),
        "an omitted required_host_caps field deserializes to empty"
    );
}

// ---------------------------------------------------------------------
// Discovery -- `requires`/`optional` on the wire (board item
// `01M0XCD3P8S3VR0T1H0KNG5TMD`)
// ---------------------------------------------------------------------

/// A `tool.spec/1` manifest declaring both `requires` (`["conway.ui"]`) and
/// `optional` (`["conway.notifications"]`) loads, and both are mapped
/// verbatim into `PluginManifest::requires`/`optional` -- the SAME fields an
/// in-process `Plugin`'s manifest populates, resolved and checked by the
/// SAME `ConwayBuilder::build` dependency-resolution code (proven in
/// `crates/conway/tests/builder.rs`; this proves only that the wire carries
/// the fields and `discover` maps them, not a parallel resolution path).
#[tokio::test]
async fn discover_maps_declared_requires_and_optional_into_the_manifest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for_warmed(dir.path(), "deps.py", common::DEPENDENCY_PLUGIN).await;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("a manifest declaring known requires/optional plugin ids must load");
    let manifest = plugin.manifest();
    assert_eq!(manifest.id, "acme.needs-ui");
    assert_eq!(
        manifest.requires,
        vec!["conway.ui".to_string()],
        "the declared requires id maps verbatim into PluginManifest::requires"
    );
    assert_eq!(
        manifest.optional,
        vec!["conway.notifications".to_string()],
        "the declared optional id maps verbatim into PluginManifest::optional"
    );
}

/// A `tool.spec/1` manifest that OMITS `requires`/`optional` entirely still
/// loads (`#[serde(default)]` parses both as empty), so an older plugin
/// built before this item shipped is unaffected -- `docs/plugins/
/// compatibility.md`'s versioning table calls a new optional field a
/// `minor`-compatible addition for exactly this reason. The built-in
/// `GREET_PLUGIN` fixture predates both fields and omits them.
#[tokio::test]
async fn discover_loads_a_manifest_omitting_requires_and_optional_as_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for_warmed(dir.path(), "greet.py", common::GREET_PLUGIN).await;

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("a manifest omitting requires/optional must load unchanged (#[serde(default)])");
    let manifest = plugin.manifest();
    assert!(
        manifest.requires.is_empty(),
        "an omitted requires field deserializes to empty"
    );
    assert!(
        manifest.optional.is_empty(),
        "an omitted optional field deserializes to empty"
    );
}

// ---------------------------------------------------------------------
// Invocation -- `tool/1`
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_successful_call_reaches_the_real_subprocess_and_returns_its_reply() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for_warmed(dir.path(), "greet.py", common::GREET_PLUGIN).await;
    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery must succeed");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let output = tool
        .invoke(call("greet", serde_json::json!({"name": "world"})), ctx())
        .await
        .expect("a well-formed call to a well-behaved fixture must succeed");

    assert!(!output.is_error);
    let text: String = output
        .blocks
        .iter()
        .filter_map(|b| match b {
            conway::plugin::ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text, "hello, world",
        "the real subprocess's own tool/1 answer must reach the caller verbatim"
    );
}

#[tokio::test]
async fn a_subprocess_declared_error_maps_to_the_matching_typed_tool_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for_warmed(dir.path(), "greet.py", common::GREET_PLUGIN).await;
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
        },
        "the wire error's kind/detail must map onto the matching ToolError variant, not a \
         generic catch-all"
    );
}

#[tokio::test]
async fn invoke_fails_closed_when_the_subprocess_dies_mid_call() {
    // "A plugin that exits mid-call yields a typed error and the agent loop
    // continues" -- this item's own ACCEPTANCE, proved by a fixture that
    // exits nonzero for every call (a call that "dies" -- crashes -- looks
    // identical on the wire to one that exits nonzero deliberately: both
    // are "the process ended without producing a valid tool/1 answer").
    let dir = tempfile::tempdir().expect("tempdir");
    let crashing = r#"#!/usr/bin/env python3
import sys, json
req = json.loads(sys.stdin.read())
if req.get("op") == "tool.spec/1":
    print(json.dumps({
        "id": "acme.crasher",
        "version": "0.1.0",
        "tools": [{
            "name": "crash",
            "description": "always crashes on tool/1",
            "schema": {"type": "object"},
            "category": "read",
            "permission": "safe",
        }],
    }))
else:
    sys.exit(1)
"#;
    let spec = common::spec_for_warmed(dir.path(), "crasher.py", crashing).await;
    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery must succeed even though invocation will crash");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let err = tool
        .invoke(call("crash", serde_json::json!({})), ctx())
        .await
        .expect_err("a process that dies mid-call must yield a typed error, never a hang");
    assert!(
        matches!(err, ToolError::Io { .. }),
        "expected ToolError::Io, got {err:?}"
    );
}

#[tokio::test]
async fn invoke_fails_closed_on_timeout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let hangs_on_invoke = r#"#!/usr/bin/env python3
import sys, json, time
req = json.loads(sys.stdin.read())
if req.get("op") == "tool.spec/1":
    print(json.dumps({
        "id": "acme.hangs",
        "version": "0.1.0",
        "tools": [{
            "name": "hang",
            "description": "hangs forever on tool/1",
            "schema": {"type": "object"},
            "category": "read",
            "permission": "safe",
        }],
    }))
else:
    time.sleep(10)
    print("{}")
"#;
    // `tool.spec/1` on this fixture answers immediately (no sleep); only
    // `tool/1` hangs. 1500ms comfortably covers a fresh `python3`
    // interpreter's own startup under concurrent test load while still
    // proving the invoke call (which hits a 10s sleep) times out well
    // before that sleep would ever finish.
    //
    // This test used to fail intermittently under full-suite parallel runs
    // with `TimedOut` on DISCOVERY, not the hang below -- root-caused
    // 2026-08-21 (board item `01M09MPZ9C188AHNBKWEJ3CEQA`) to a
    // first-execution OS cost paid by any freshly-written, freshly-chmod'd
    // script's first exec (measured up to 23.5s at 0% CPU on one run; see
    // `common::warm`'s doc for the full measurement). `discover` execs
    // THIS file for the handshake under the SAME 1500ms budget, so the tax
    // landed inside the timed assertion. `common::warm` below pays that
    // tax here, discarded, before the clock starts, so 1500ms only has to
    // cover a warm `python3` interpreter's own startup under contention --
    // which it comfortably does. Do not raise this number to paper over a
    // failure without first checking `warm` is still being called; if it
    // is and this still flakes, the tax is bigger than warming amortizes
    // and that is new information, not a reason to guess a bigger budget.
    let path = common::write_script(dir.path(), "hangs.py", hangs_on_invoke);
    common::warm(&path).await;
    let mut spec = conway_plugin_subprocess::SubprocessPluginSpec::new(
        "hangs",
        vec![path.display().to_string()],
    );
    spec.timeout_ms = 1_500;
    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery must succeed promptly; only the tool/1 call hangs");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let start = std::time::Instant::now();
    let err = tool
        .invoke(call("hang", serde_json::json!({})), ctx())
        .await
        .expect_err("a call that never answers within timeout_ms must fail closed");
    assert!(
        matches!(err, ToolError::Io { .. }),
        "expected ToolError::Io (timeout is reported through the Io variant, carrying the \
         underlying SubprocessPluginError::TimedOut in its detail), got {err:?}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "invoke must return promptly once timeout_ms elapses: took {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn invoke_fails_closed_on_garbage_tool_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let garbage_on_invoke = r#"#!/usr/bin/env python3
import sys, json
req = json.loads(sys.stdin.read())
if req.get("op") == "tool.spec/1":
    print(json.dumps({
        "id": "acme.garbler",
        "version": "0.1.0",
        "tools": [{
            "name": "garble",
            "description": "returns garbage on tool/1",
            "schema": {"type": "object"},
            "category": "read",
            "permission": "safe",
        }],
    }))
else:
    print("this is not json")
"#;
    let spec = common::spec_for_warmed(dir.path(), "garbler.py", garbage_on_invoke).await;
    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery must succeed");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let err = tool
        .invoke(call("garble", serde_json::json!({})), ctx())
        .await
        .expect_err("garbage stdout must fail closed, never be read as an empty success");
    assert!(
        matches!(err, ToolError::Internal { .. }),
        "expected ToolError::Internal, got {err:?}"
    );
}

#[tokio::test]
async fn invoke_never_spawns_a_process_when_already_cancelled() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A fixture that leaves a marker file behind IF it is ever invoked --
    // absence of the marker after `invoke` is this test's proof that
    // cancellation was honored BEFORE any process was spawned, not merely
    // that the call returned `Cancelled` for some other reason.
    let marker = dir.path().join("was-invoked");
    let marks_and_answers = format!(
        r#"#!/usr/bin/env python3
import sys, json
open({marker:?}, "w").close()
req = json.loads(sys.stdin.read())
if req.get("op") == "tool.spec/1":
    print(json.dumps({{
        "id": "acme.marker",
        "version": "0.1.0",
        "tools": [{{
            "name": "mark",
            "description": "marks that it ran",
            "schema": {{"type": "object"}},
            "category": "read",
            "permission": "safe",
        }}],
    }}))
else:
    print(json.dumps({{"ok": True, "blocks": [], "is_error": False}}))
"#,
        marker = marker.display().to_string()
    );
    let spec = common::spec_for_warmed(dir.path(), "marker.py", &marks_and_answers).await;
    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery legitimately runs the fixture once, so the marker exists now");
    assert!(
        marker.exists(),
        "discovery itself must have run the fixture once"
    );
    std::fs::remove_file(&marker).expect("reset marker before the cancellation assertion");

    let tool = plugin.tools().into_iter().next().expect("one tool");
    let cancelled_ctx = ctx();
    cancelled_ctx.cancel.cancel();

    let err = tool
        .invoke(call("mark", serde_json::json!({})), cancelled_ctx)
        .await
        .expect_err("an already-cancelled call must be refused");
    assert_eq!(err, ToolError::Cancelled);
    assert!(
        !marker.exists(),
        "a call cancelled before it started must never spawn the subprocess at all"
    );
}
