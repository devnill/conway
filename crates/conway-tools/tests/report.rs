//! Integration coverage for `ReportTool` and the `ReportPlugin` assembly
//! (WI-065 criteria).
//!
//! Requires the `test-fakes` feature (for `conway_tools::testing::test_ctx`).
//! Declared with `required-features = ["test-fakes"]` in Cargo.toml, so a
//! plain `cargo test -p conway-tools` skips (not fails) this file.

#![cfg(feature = "test-fakes")]

use std::path::PathBuf;

use conway_core::agent::Fact;
use conway_core::content::{Artifact, ContentBlock, ToolCall, ToolCategory, TruncationPolicy};
use conway_core::ids::ToolName;
use conway_core::ports::{Plugin, Tool, ToolOutput};
use conway_tools::report::{ReportPlugin, ReportTool};
use conway_tools::testing::test_ctx;

fn call(arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        call_id: "tc_1".into(),
        name: ToolName::new("report"),
        arguments,
    }
}

fn text_of(out: &ToolOutput) -> &str {
    match &out.blocks[0] {
        ContentBlock::Text { text } => text,
        other => panic!("expected a text block, got {other:?}"),
    }
}

/// The runtime does the `AgentResult` lift; this crate holds zero
/// session-logging or result-construction logic (architecture boundary,
/// WI-065 criteria). Read from outside `report_tool.rs` so this assertion's
/// own literal strings aren't part of the scanned content.
#[test]
fn report_tool_module_has_no_session_or_agent_result_construction() {
    let src = include_str!("../src/report/report_tool.rs");
    assert!(!src.contains("SessionStore"));
    assert!(!src.contains("AgentResult {"));
}

#[test]
fn plugin_has_one_tool_named_report() {
    let plugin = ReportPlugin::new();
    assert_eq!(plugin.manifest().id, "conway.report");

    let names: Vec<String> = plugin
        .tools()
        .iter()
        .map(|t| t.spec().name.as_str().to_string())
        .collect();
    assert_eq!(names, vec!["report"]);
    assert_eq!(plugin.tools()[0].spec().category, ToolCategory::Think);
}

#[tokio::test]
async fn valid_call_round_trips_facts_and_artifacts() {
    let (ctx, _h) = test_ctx(PathBuf::from("/tmp/x"));
    let out = ReportTool::new()
        .invoke(
            call(serde_json::json!({
                "summary": "did the thing",
                "facts": [
                    {"key": "files_changed", "value": "3", "source": "git diff"}
                ],
                "artifacts": [
                    {"kind": "diff", "path": "a.patch", "value": "the diff"}
                ],
                "structured": {"passed": true}
            })),
            ctx,
        )
        .await
        .unwrap();

    assert!(!out.is_error);
    assert_eq!(out.truncation, TruncationPolicy::None);

    let json: serde_json::Value = serde_json::from_str(text_of(&out)).unwrap();
    let report = &json["conway_report"];
    assert_eq!(report["version"], 1);
    assert_eq!(report["summary"], "did the thing");

    let facts: Vec<Fact> = serde_json::from_value(report["facts"].clone()).unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].key, "files_changed");

    let artifacts: Vec<Artifact> = serde_json::from_value(report["artifacts"].clone()).unwrap();
    assert_eq!(artifacts.len(), 1);

    // Parsed artifacts are also placed on ToolOutput.artifacts.
    assert_eq!(out.artifacts.len(), 1);
    assert_eq!(out.artifacts[0].id, artifacts[0].id);
}

#[tokio::test]
async fn omitted_fields_default_to_empty_arrays_and_null() {
    let (ctx, _h) = test_ctx(PathBuf::from("/tmp/x"));
    let out = ReportTool::new()
        .invoke(call(serde_json::json!({"summary": "s"})), ctx)
        .await
        .unwrap();

    assert!(!out.is_error);
    let json: serde_json::Value = serde_json::from_str(text_of(&out)).unwrap();
    let report = &json["conway_report"];
    assert_eq!(report["facts"], serde_json::json!([]));
    assert_eq!(report["artifacts"], serde_json::json!([]));
    assert_eq!(report["structured"], serde_json::Value::Null);
    assert!(out.artifacts.is_empty());
}

#[tokio::test]
async fn summary_over_2000_chars_is_error_and_emits_no_artifacts() {
    let (ctx, _h) = test_ctx(PathBuf::from("/tmp/x"));
    let out = ReportTool::new()
        .invoke(
            call(serde_json::json!({
                "summary": "a".repeat(2001),
                "artifacts": [{"kind": "file", "path": "a.txt"}]
            })),
            ctx,
        )
        .await
        .unwrap();

    assert!(out.is_error);
    assert!(text_of(&out).contains("summary exceeds 2000 characters"));
    assert!(out.artifacts.is_empty());
}
