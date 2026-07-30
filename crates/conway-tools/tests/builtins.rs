//! Cross-plugin conformance coverage: [`conway_tools::builtin_plugins`] and
//! the crate-wide rules every built-in tool must follow (WI-067 criteria).
//!
//! Requires the `test-fakes` feature (for `conway_tools::testing::test_ctx`).
//! Declared with `required-features = ["test-fakes"]` in Cargo.toml, so a
//! plain `cargo test -p conway-tools` skips (not fails) this file.

#![cfg(feature = "test-fakes")]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use conway_core::agent::{AgentResult, ResultStatus};
use conway_core::content::{ContentBlock, ToolCall, TruncationPolicy};
use conway_core::error::ToolError;
use conway_core::ids::{AgentId, SessionId, ToolName};
use conway_core::ports::{SubagentHost, Tool, ToolCtx, ToolOutput};
use conway_tools::builtin_plugins;
use conway_tools::fs::{EditTool, GlobTool, GrepTool, ReadTool, WriteTool};
use conway_tools::report::ReportTool;
use conway_tools::shell::BashTool;
use conway_tools::subagent::{AskTool, AwaitTool, CancelTool, SteerTool, SubagentTool};
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
            Arc::new(AskTool::new()) as Arc<dyn Tool>,
            serde_json::json!({"prompt": "p"}),
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

// ------------------------------------------------------------ path_args ---

/// The declaration-matches-reality guard (S4). Every path name a tool
/// declares via `Tool::path_args` MUST be a real top-level property of that
/// tool's own JSON schema.
///
/// This is the test that makes the declaration trustworthy rather than
/// aspirational. A declaration naming a field the args struct does not have
/// is a silent hole: the later root-containment check would look up that key,
/// find nothing, and confine nothing -- while appearing to be configured.
/// That "a lookup that finds nothing becomes an authorization" shape is
/// exactly the class of bug fixed in 0.5.0, so it is pinned generically here,
/// across every built-in at once, rather than tool-by-tool where a newly
/// added tool could be forgotten.
#[test]
fn every_declared_path_arg_is_a_real_property_of_the_tools_schema() {
    for (tool, _) in all_tools_with_minimal_args() {
        let spec = tool.spec();
        let name = spec.name.as_str().to_string();

        let declared: &[&str] = match tool.path_args() {
            conway_core::ports::PathArgs::None => &[],
            conway_core::ports::PathArgs::Named(names) => names,
            conway_core::ports::PathArgs::Unconfinable { checkable } => checkable,
            // `PathArgs` is `#[non_exhaustive]`: a future variant must be
            // taught to this guard deliberately, not silently skipped.
            other => panic!("tool `{name}` declared an unhandled PathArgs variant: {other:?}"),
        };

        if declared.is_empty() {
            continue;
        }

        let schema = serde_json::to_value(&spec.schema)
            .unwrap_or_else(|e| panic!("tool `{name}`'s schema must serialize: {e}"));
        let properties = schema
            .get("properties")
            .and_then(|p| p.as_object())
            .unwrap_or_else(|| {
                panic!(
                    "tool `{name}` declares path args {declared:?} but its schema has no \
                     `properties` object to validate them against"
                )
            });

        for arg in declared {
            assert!(
                properties.contains_key(*arg),
                "tool `{name}` declares path arg `{arg}`, which is NOT a property of its own \
                 schema (properties: {:?}). A declared name that does not exist confines \
                 nothing while looking configured.",
                properties.keys().collect::<Vec<_>>()
            );
        }
    }
}

/// `bash` is the reason `Unconfinable` carries `checkable`: its `command` is
/// unconfinable while its `cwd` is a genuinely checkable path. Pinned
/// explicitly because an enforcing call site must be able to rely on both
/// facts arriving together, from one match, with no `if name == "bash"`.
#[test]
fn bash_declares_itself_unconfinable_yet_offers_cwd_as_checkable() {
    let tool = BashTool::new();
    match tool.path_args() {
        conway_core::ports::PathArgs::Unconfinable { checkable } => {
            assert_eq!(
                checkable, &["cwd"],
                "bash's cwd is resolved and handed to Command::current_dir, so it is checkable"
            );
        }
        other => panic!(
            "bash's free-form command string can reach any path, so it must never be \
             statically confinable; got {other:?}"
        ),
    }
}

/// The fail-closed default. A tool that does not override `path_args` must
/// NOT be treated as "no paths, therefore nothing to check" -- it must be
/// unconfinable, so adding this trait method silently auto-allows nothing.
#[test]
fn a_tool_that_does_not_override_path_args_is_unconfinable_not_pathless() {
    struct UndeclaredTool;

    #[async_trait]
    impl Tool for UndeclaredTool {
        fn spec(&self) -> conway_core::content::ToolSpec {
            conway_core::content::ToolSpec {
                name: ToolName::new("undeclared"),
                description: "a third-party tool that never heard of path_args".into(),
                schema: schemars::schema_for!(()),
                category: conway_core::content::ToolCategory::Read,
                permission: conway_core::content::PermissionClass::Safe,
            }
        }

        async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
            unreachable!("this tool exists only to observe the path_args default")
        }
    }

    assert_eq!(
        UndeclaredTool.path_args(),
        conway_core::ports::PathArgs::Unconfinable { checkable: &[] },
        "the default must fail closed: 'no declared paths' must never mean 'allow'"
    );
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
fn union_of_tools_is_exactly_the_documented_twelve() {
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
            "conway_ask",
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

    // `conway_ask` scripts an `AskOutcome` via `FakeSubagentHost`'s
    // `with_ask_outcome` builder so `SubagentHost::ask` resolves immediately
    // with a completed status — only `out.truncation` is under test here.
    let ask_parent = AgentId::new();
    let ask_outcome = conway_core::agent::AskOutcome {
        text: "curated brief".into(),
        usage: conway_core::content::Usage::default(),
        status: ResultStatus::Completed,
        transcript_ref: SessionId::new(),
    };
    let ask_host = Arc::new(FakeSubagentHost::new().with_ask_outcome(ask_parent, ask_outcome));
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());
    let ctx = ToolCtx {
        agent_id: ask_parent,
        subagents: ask_host as Arc<dyn SubagentHost>,
        ..ctx
    };
    let out = AskTool::new()
        .invoke(call("conway_ask", serde_json::json!({"prompt": "p"})), ctx)
        .await
        .unwrap();
    assert!(!out.is_error, "conway_ask: {}", text_of(&out));
    actual.insert("conway_ask", out.truncation);

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

    // Authoritative per docs/crates/conway-tools.md WI-067 truncation table,
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
        ("conway_ask", TruncationPolicy::Tail { max_bytes: 16_384 }),
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
