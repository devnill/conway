//! Cross-plugin conformance coverage: [`conway_tools::builtin_plugins`] and
//! the crate-wide rules every built-in tool must follow (WI-067 criteria).
//!
//! Requires the `test-fakes` feature (for `conway_tools::testing::test_ctx`).
//! Declared with `required-features = ["test-fakes"]` in Cargo.toml, so a
//! plain `cargo test -p conway-tools` skips (not fails) this file.

#![cfg(feature = "test-fakes")]

use std::collections::HashMap;
use std::sync::Arc;

use conway_core::agent::{AgentResult, ResultStatus};
use conway_core::content::{ContentBlock, ToolCall, TruncationPolicy};
use conway_core::error::ToolError;
use conway_core::ids::{AgentId, SessionId, ToolName};
use conway_core::ports::{SubagentHost, Tool, ToolCtx, ToolOutput};
use conway_tools::builtin_plugins;
use conway_tools::fs::{EditTool, GlobTool, GrepTool, ReadTool, WriteTool};
use conway_tools::report::ReportTool;
use conway_tools::shell::BashTool;
use conway_tools::subagent::{AwaitTool, CancelTool, SteerTool, SubagentTool};
use conway_tools::testing::{test_ctx, FakeSubagentHost};
use tempfile::TempDir;

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        call_id: "tc_1".into(),
        name: ToolName::new(name),
        arguments,
    }
}

fn text_of(out: &ToolOutput) -> &str {
    match &out.blocks[0] {
        ContentBlock::Text { text } => text,
        other => panic!("expected a text block, got {other:?}"),
    }
}

/// One entry per built-in tool: its call name and the minimal arguments that
/// pass schema validation for it. Shared by the cancellation-conformance and
/// schema/description sweeps below.
fn all_tools_with_minimal_args() -> Vec<(Arc<dyn Tool>, serde_json::Value)> {
    vec![
        (
            Arc::new(ReadTool::new()) as Arc<dyn Tool>,
            serde_json::json!({"path": "f.txt"}),
        ),
        (
            Arc::new(WriteTool::new()),
            serde_json::json!({"path": "f.txt", "content": "x"}),
        ),
        (
            Arc::new(EditTool::new()),
            serde_json::json!({"path": "f.txt", "old_string": "a", "new_string": "b"}),
        ),
        (
            Arc::new(GlobTool::new()),
            serde_json::json!({"pattern": "*.rs"}),
        ),
        (
            Arc::new(GrepTool::new()),
            serde_json::json!({"pattern": "fn"}),
        ),
        (
            Arc::new(BashTool::new()),
            serde_json::json!({"command": "true"}),
        ),
        (
            Arc::new(ReportTool::new()),
            serde_json::json!({"summary": "s"}),
        ),
        (
            Arc::new(SubagentTool::new()),
            serde_json::json!({"mode": "fork", "prompt": "p"}),
        ),
        (
            Arc::new(SteerTool::new()),
            serde_json::json!({"agent_id": AgentId::new().to_string(), "text": "x"}),
        ),
        (
            Arc::new(AwaitTool::new()),
            serde_json::json!({"agent_id": AgentId::new().to_string()}),
        ),
        (
            Arc::new(CancelTool::new()),
            serde_json::json!({"agent_id": AgentId::new().to_string()}),
        ),
    ]
}

// ------------------------------------------------------------- registry ---

#[test]
fn builtin_plugins_returns_exactly_four_with_expected_ids() {
    let mut ids: Vec<String> = builtin_plugins().iter().map(|p| p.manifest().id).collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "conway.fs",
            "conway.report",
            "conway.shell",
            "conway.subagent"
        ]
    );
}

#[test]
fn union_of_tools_is_exactly_the_documented_eleven() {
    let mut names: Vec<String> = builtin_plugins()
        .iter()
        .flat_map(|p| p.tools())
        .map(|t| t.spec().name.as_str().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "bash",
            "conway_await",
            "conway_cancel",
            "conway_steer",
            "conway_subagent",
            "edit",
            "glob",
            "grep",
            "read",
            "report",
            "write",
        ]
    );
}

#[test]
fn no_two_builtin_tools_share_a_name() {
    let names: Vec<String> = builtin_plugins()
        .iter()
        .flat_map(|p| p.tools())
        .map(|t| t.spec().name.as_str().to_string())
        .collect();
    let unique: std::collections::HashSet<&String> = names.iter().collect();
    assert_eq!(unique.len(), names.len());
}

#[test]
fn every_schema_is_a_valid_json_schema_object() {
    for plugin in builtin_plugins() {
        for tool in plugin.tools() {
            let spec = tool.spec();
            let json = serde_json::to_value(&spec.schema).unwrap();
            assert_eq!(
                json["type"],
                serde_json::json!("object"),
                "{}: schema type is not \"object\"",
                spec.name.as_str()
            );
            assert!(
                json["properties"].is_object(),
                "{}: schema has no properties object",
                spec.name.as_str()
            );
        }
    }
}

#[test]
fn every_description_is_non_empty_and_bounded() {
    for plugin in builtin_plugins() {
        for tool in plugin.tools() {
            let spec = tool.spec();
            assert!(
                !spec.description.is_empty(),
                "{}: description is empty",
                spec.name.as_str()
            );
            assert!(
                spec.description.chars().count() <= 1024,
                "{}: description exceeds 1024 characters",
                spec.name.as_str()
            );
        }
    }
}

// --------------------------------------------------------- cancellation ---

#[tokio::test]
async fn every_builtin_tool_honors_pre_cancellation() {
    for (tool, args) in all_tools_with_minimal_args() {
        let name = tool.spec().name.as_str().to_string();
        let dir = TempDir::new().unwrap();
        let (ctx, handles) = test_ctx(dir.path().to_path_buf());
        handles.cancel.cancel();
        let err = tool
            .invoke(call(&name, args), ctx)
            .await
            .expect_err(&format!("{name}: expected Err on a pre-cancelled ctx"));
        assert!(
            matches!(err, ToolError::Cancelled),
            "{name}: expected ToolError::Cancelled, got {err:?}"
        );
    }
}

// ----------------------------------------------------------- truncation ---

#[tokio::test]
async fn truncation_matches_the_documented_table_per_tool() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("f.txt"), "hello world").unwrap();
    std::fs::write(dir.path().join("g.rs"), "fn foo() {}").unwrap();

    let mut actual: HashMap<&'static str, TruncationPolicy> = HashMap::new();

    let (ctx, _h) = test_ctx(dir.path().to_path_buf());
    let out = ReadTool::new()
        .invoke(call("read", serde_json::json!({"path": "f.txt"})), ctx)
        .await
        .unwrap();
    assert!(!out.is_error, "read: {}", text_of(&out));
    actual.insert("read", out.truncation);

    let (ctx, _h) = test_ctx(dir.path().to_path_buf());
    let out = WriteTool::new()
        .invoke(
            call(
                "write",
                serde_json::json!({"path": "w.txt", "content": "x"}),
            ),
            ctx,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "write: {}", text_of(&out));
    actual.insert("write", out.truncation);

    let (ctx, _h) = test_ctx(dir.path().to_path_buf());
    let out = EditTool::new()
        .invoke(
            call(
                "edit",
                serde_json::json!({"path": "f.txt", "old_string": "hello", "new_string": "HELLO"}),
            ),
            ctx,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "edit: {}", text_of(&out));
    actual.insert("edit", out.truncation);

    let (ctx, _h) = test_ctx(dir.path().to_path_buf());
    let out = GlobTool::new()
        .invoke(call("glob", serde_json::json!({"pattern": "*.rs"})), ctx)
        .await
        .unwrap();
    assert!(!out.is_error, "glob: {}", text_of(&out));
    actual.insert("glob", out.truncation);

    let (ctx, _h) = test_ctx(dir.path().to_path_buf());
    let out = GrepTool::new()
        .invoke(call("grep", serde_json::json!({"pattern": "fn"})), ctx)
        .await
        .unwrap();
    assert!(!out.is_error, "grep: {}", text_of(&out));
    actual.insert("grep", out.truncation);

    let (ctx, _h) = test_ctx(dir.path().to_path_buf());
    let out = BashTool::new()
        .invoke(call("bash", serde_json::json!({"command": "echo hi"})), ctx)
        .await
        .unwrap();
    assert!(!out.is_error, "bash: {}", text_of(&out));
    actual.insert("bash", out.truncation);

    let (ctx, _h) = test_ctx(dir.path().to_path_buf());
    let out = ReportTool::new()
        .invoke(call("report", serde_json::json!({"summary": "s"})), ctx)
        .await
        .unwrap();
    assert!(!out.is_error, "report: {}", text_of(&out));
    actual.insert("report", out.truncation);

    // `await: false` sidesteps needing a scripted `AgentResult` for this
    // success-path invocation — only `out.truncation` is under test here.
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());
    let out = SubagentTool::new()
        .invoke(
            call(
                "conway_subagent",
                serde_json::json!({"mode": "fork", "prompt": "p", "await": false}),
            ),
            ctx,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "conway_subagent: {}", text_of(&out));
    actual.insert("conway_subagent", out.truncation);

    let (ctx, handles) = test_ctx(dir.path().to_path_buf());
    let steer_target = handles.subagents.next_agent_id();
    let out = SteerTool::new()
        .invoke(
            call(
                "conway_steer",
                serde_json::json!({"agent_id": steer_target.to_string(), "text": "hi"}),
            ),
            ctx,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "conway_steer: {}", text_of(&out));
    actual.insert("conway_steer", out.truncation);

    let await_target = AgentId::new();
    let scripted = AgentResult::new(
        await_target,
        SessionId::new(),
        ResultStatus::Completed,
        "done",
    );
    let host = Arc::new(FakeSubagentHost::new().with_result(await_target, scripted));
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());
    let ctx = ToolCtx {
        subagents: host as Arc<dyn SubagentHost>,
        ..ctx
    };
    let out = AwaitTool::new()
        .invoke(
            call(
                "conway_await",
                serde_json::json!({"agent_id": await_target.to_string()}),
            ),
            ctx,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "conway_await: {}", text_of(&out));
    actual.insert("conway_await", out.truncation);

    let (ctx, handles) = test_ctx(dir.path().to_path_buf());
    let cancel_target = handles.subagents.next_agent_id();
    let out = CancelTool::new()
        .invoke(
            call(
                "conway_cancel",
                serde_json::json!({"agent_id": cancel_target.to_string()}),
            ),
            ctx,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "conway_cancel: {}", text_of(&out));
    actual.insert("conway_cancel", out.truncation);

    // Authoritative per docs/plan/wi-conway-tools.md WI-067 truncation table,
    // adjusted for conway-core's actual `HeadTail { head_bytes, tail_bytes }`
    // shape (WI-064 deviation: the plan sketched `{ max_bytes }`, which
    // conway-core does not have; `bash.rs` splits its 30_000-byte budget
    // evenly across the two real fields).
    let expected: Vec<(&str, TruncationPolicy)> = vec![
        ("read", TruncationPolicy::Head { max_bytes: 65_536 }),
        ("write", TruncationPolicy::None),
        ("edit", TruncationPolicy::None),
        ("glob", TruncationPolicy::Head { max_bytes: 32_768 }),
        ("grep", TruncationPolicy::Head { max_bytes: 32_768 }),
        (
            "bash",
            TruncationPolicy::HeadTail {
                head_bytes: 15_000,
                tail_bytes: 15_000,
            },
        ),
        ("report", TruncationPolicy::None),
        (
            "conway_subagent",
            TruncationPolicy::Tail { max_bytes: 16_384 },
        ),
        ("conway_steer", TruncationPolicy::Tail { max_bytes: 16_384 }),
        ("conway_await", TruncationPolicy::Tail { max_bytes: 16_384 }),
        (
            "conway_cancel",
            TruncationPolicy::Tail { max_bytes: 16_384 },
        ),
    ];
    assert_eq!(actual.len(), expected.len());
    for (name, policy) in expected {
        assert_eq!(actual[name], policy, "truncation mismatch for {name}");
    }
}
