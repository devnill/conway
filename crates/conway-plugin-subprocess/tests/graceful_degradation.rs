//! Board item `01M03VJPRT8629CYR8JK4A8JPF`: graceful unknown-tag degradation
//! for the one-shot wire answers. A NEWER plugin using an enum tag this host
//! does not know degrades to the MOST RESTRICTIVE value instead of refusing
//! the whole plugin -- the convergence rule in
//! `docs/plugins/compatibility.md`'s wire-protocol table. These tests pin
//! each degradation and the fail-closed line for structural malformation
//! (which stays a hard error, NOT degraded).

mod common;

use std::sync::Arc;

use conway::plugin::{ContentBlock, PermissionClass, ToolCategory};
use conway::plugin::{Plugin as _, ToolCall, ToolCtx, ToolError};
use conway::AgentId;
use conway_plugin_subprocess::{SubprocessPlugin, SubprocessPluginError};
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
// tool.spec/1 -- unknown ToolCategory / PermissionClass degrade to the
// most restrictive value, NOT a hard parse error.
// ---------------------------------------------------------------------

#[tokio::test]
async fn unknown_category_tag_degrades_to_execute() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for(dir.path(), "unknown_tags.py", common::UNKNOWN_TAG_PLUGIN);

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("an unknown category tag must degrade, not refuse the whole plugin");

    let tools = plugin.tools();
    assert_eq!(tools.len(), 1);
    let spec = tools[0].spec();
    assert_eq!(
        spec.category,
        ToolCategory::Execute,
        "unknown category 'deploy' degrades to Execute (the category plan mode already \
         denies -- the most restrictive value)"
    );
}

#[tokio::test]
async fn unknown_permission_tag_degrades_to_dangerous() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for(dir.path(), "unknown_tags.py", common::UNKNOWN_TAG_PLUGIN);

    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("an unknown permission tag must degrade, not refuse the whole plugin");

    let tools = plugin.tools();
    assert_eq!(tools.len(), 1);
    let spec = tools[0].spec();
    assert_eq!(
        spec.permission,
        PermissionClass::Dangerous,
        "unknown permission 'yolo' degrades to Dangerous (the most restrictive value)"
    );
}

#[tokio::test]
async fn unknown_tag_tool_still_invokes_normally() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for(dir.path(), "unknown_tags.py", common::UNKNOWN_TAG_PLUGIN);
    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("degradation loads the tool");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let output = tool
        .invoke(call("frob", serde_json::json!({})), ctx())
        .await
        .expect("a degraded-tag tool still answers tool/1 normally");
    assert!(!output.is_error);
    assert_eq!(
        output.blocks,
        vec![ContentBlock::Text {
            text: "frobbed".to_string()
        }]
    );
}

// ---------------------------------------------------------------------
// tool/1 -- an unknown ContentBlock type is dropped, counted, and surfaced.
// The call SUCCEEDS with the known blocks; the drop is reported via an
// appended summary ContentBlock::Text and the is_error flag.
// ---------------------------------------------------------------------

#[tokio::test]
async fn unknown_content_block_is_dropped_counted_and_surfaced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for(dir.path(), "unknown_block.py", common::UNKNOWN_BLOCK_PLUGIN);
    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery has only known tags");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let output = tool
        .invoke(call("mix", serde_json::json!({})), ctx())
        .await
        .expect("the call succeeds with the known block despite the unknown one");

    // The known block is preserved verbatim.
    assert!(
        output
            .blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text == "kept")),
        "the known text block must reach the caller; got {:?}",
        output.blocks
    );
    // The unknown block type is NOT present as a typed block -- it was
    // dropped, not silently misparsed.
    assert!(
        !output.blocks.iter().any(|b| matches!(
            b,
            ContentBlock::ToolUse { .. }
                | ContentBlock::Image { .. }
                | ContentBlock::Thinking { .. }
                | ContentBlock::ToolResultBlock { .. }
        )),
        "the unknown 'quantum' block must not appear as a typed ContentBlock; got {:?}",
        output.blocks
    );
    // The drop is surfaced: is_error is set and a summary block names the
    // dropped count and the unknown type tag.
    assert!(
        output.is_error,
        "a dropped unknown block must set is_error so the host knows the output is incomplete"
    );
    let summary = output
        .blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .find(|t| t.contains("dropped") && t.contains("quantum"));
    assert!(
        summary.is_some(),
        "a summary block naming the dropped count and the unknown type tag 'quantum' must be \
         appended; got {:?}",
        output.blocks
    );
}

// ---------------------------------------------------------------------
// tool/1 over the PERSISTENT NDJSON transport -- the ContentBlock
// drop+count+surface degradation lives in the shared
// `RawToolResult::classify`, so it applies on both transports. This pins
// the persistent channel: an unknown block type over NDJSON is dropped,
// counted, and surfaced exactly as it is over the one-shot path.
// ---------------------------------------------------------------------

#[tokio::test]
async fn unknown_content_block_over_persistent_transport_is_dropped_and_surfaced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for(
        dir.path(),
        "persistent_unknown_block.py",
        common::PERSISTENT_UNKNOWN_BLOCK_PLUGIN,
    );
    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery has only known tags");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let output = tool
        .invoke(call("mix", serde_json::json!({})), ctx())
        .await
        .expect("the call succeeds with the known block despite the unknown one");

    // The known block is preserved verbatim over the persistent channel.
    assert!(
        output
            .blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text == "kept")),
        "the known text block must reach the caller over the persistent channel; got {:?}",
        output.blocks
    );
    // The unknown block type is dropped, not silently misparsed.
    assert!(
        !output.blocks.iter().any(|b| matches!(
            b,
            ContentBlock::ToolUse { .. }
                | ContentBlock::Image { .. }
                | ContentBlock::Thinking { .. }
                | ContentBlock::ToolResultBlock { .. }
        )),
        "the unknown 'quantum' block must not appear as a typed ContentBlock; got {:?}",
        output.blocks
    );
    // The drop is surfaced: is_error set and a summary block names 'quantum'.
    assert!(
        output.is_error,
        "a dropped unknown block over the persistent channel must set is_error"
    );
    let summary = output
        .blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .find(|t| t.contains("dropped") && t.contains("quantum"));
    assert!(
        summary.is_some(),
        "a summary block naming the dropped count and the unknown type tag 'quantum' must be \
         appended over the persistent channel; got {:?}",
        output.blocks
    );
}

// ---------------------------------------------------------------------
// Structural malformation STILL fails closed (regression guard for the
// line: unknown enum TAG degrades; missing/structurally-invalid FIELD
// fails closed).
// ---------------------------------------------------------------------

#[tokio::test]
async fn non_string_category_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = r#"#!/usr/bin/env python3
import sys, json
sys.stdin.read()
print(json.dumps({
    "id": "acme.nonstring",
    "version": "0.1.0",
    "tools": [{
        "name": "x",
        "description": "d",
        "schema": {"type": "object"},
        "category": 42,
        "permission": "safe",
    }],
}))
"#;
    let spec = common::spec_for(dir.path(), "nonstring.py", manifest);

    let err = SubprocessPlugin::discover(spec)
        .await
        .expect_err("a non-string category is structural malformation, not an unknown tag");
    assert!(
        matches!(err, SubprocessPluginError::UnparseableAnswer { .. }),
        "expected UnparseableAnswer for a non-string category, got {err:?}"
    );
}

#[tokio::test]
async fn non_string_permission_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = r#"#!/usr/bin/env python3
import sys, json
sys.stdin.read()
print(json.dumps({
    "id": "acme.nonstringperm",
    "version": "0.1.0",
    "tools": [{
        "name": "x",
        "description": "d",
        "schema": {"type": "object"},
        "category": "read",
        "permission": 42,
    }],
}))
"#;
    let spec = common::spec_for(dir.path(), "nonstringperm.py", manifest);

    let err = SubprocessPlugin::discover(spec)
        .await
        .expect_err("a non-string permission is structural malformation, not an unknown tag");
    assert!(
        matches!(err, SubprocessPluginError::UnparseableAnswer { .. }),
        "expected UnparseableAnswer for a non-string permission, got {err:?}"
    );
}

#[tokio::test]
async fn missing_ok_field_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A tool/1 answer with NO `ok` field -- structural malformation, not an
    // unknown-tag case.
    let plugin_src = r#"#!/usr/bin/env python3
import sys, json
req = json.loads(sys.stdin.read())
if req.get("op") == "tool.spec/1":
    print(json.dumps({
        "id": "acme.nook",
        "version": "0.1.0",
        "tools": [{
            "name": "nook",
            "description": "answers without an ok field",
            "schema": {"type": "object"},
            "category": "read",
            "permission": "safe",
        }],
    }))
else:
    print(json.dumps({"blocks": [{"type": "text", "text": "no ok here"}]}))
"#;
    let spec = common::spec_for(dir.path(), "nook.py", plugin_src);
    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery is well-formed");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let err = tool
        .invoke(call("nook", serde_json::json!({})), ctx())
        .await
        .expect_err("a tool/1 answer with no `ok` field must fail closed");
    assert!(
        matches!(err, ToolError::Internal { .. }),
        "expected ToolError::Internal for a missing ok field, got {err:?}"
    );
}

#[tokio::test]
async fn ok_false_with_no_error_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plugin_src = r#"#!/usr/bin/env python3
import sys, json
req = json.loads(sys.stdin.read())
if req.get("op") == "tool.spec/1":
    print(json.dumps({
        "id": "acme.nerr",
        "version": "0.1.0",
        "tools": [{
            "name": "nerr",
            "description": "ok false with no error",
            "schema": {"type": "object"},
            "category": "read",
            "permission": "safe",
        }],
    }))
else:
    print(json.dumps({"ok": False}))
"#;
    let spec = common::spec_for(dir.path(), "nerr.py", plugin_src);
    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery is well-formed");
    let tool = plugin.tools().into_iter().next().expect("one tool");

    let err = tool
        .invoke(call("nerr", serde_json::json!({})), ctx())
        .await
        .expect_err("ok:false with no error object must fail closed");
    assert!(
        matches!(err, ToolError::Internal { .. }),
        "expected ToolError::Internal for ok:false with no error, got {err:?}"
    );
}

#[tokio::test]
async fn empty_manifest_id_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = r#"#!/usr/bin/env python3
import sys, json
sys.stdin.read()
print(json.dumps({
    "id": "",
    "version": "0.1.0",
    "tools": [{
        "name": "x",
        "description": "d",
        "schema": {"type": "object"},
        "category": "read",
        "permission": "safe",
    }],
}))
"#;
    let spec = common::spec_for(dir.path(), "emptyid.py", manifest);

    let err = SubprocessPlugin::discover(spec)
        .await
        .expect_err("an empty manifest id is structural malformation");
    assert!(
        matches!(err, SubprocessPluginError::InvalidManifest { .. }),
        "expected InvalidManifest for an empty id, got {err:?}"
    );
}
