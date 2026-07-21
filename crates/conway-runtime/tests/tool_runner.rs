//! Acceptance tests for `PluginRegistry` and `ToolRunner` (WI-079,
//! architecture §4.2, §8).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use conway_core::agent::{PermissionDecision, ToolSelector};
use conway_core::content::{ContentBlock, PermissionClass, ToolCall, ToolCategory};
use conway_core::error::ToolError;
use conway_core::event::Event;
use conway_core::fakes::{FakeGate, FakeSubagentHost};
use conway_core::ids::{AgentId, SessionId, ToolName};
use conway_core::ports::{
    Plugin, PluginConfig, PluginManifest, SubagentHost, Tool, ToolCtx, ToolOutput,
};
use conway_runtime::events::EventBus;
use conway_runtime::permission::PermissionBroker;
use conway_runtime::tools::{PluginRegistry, ToolBatchCtx, ToolRunner};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------
// Test fixtures: fake plugins/tools
// ---------------------------------------------------------------------

fn schema(value: serde_json::Value) -> schemars::schema::RootSchema {
    serde_json::from_value(value).expect("valid RootSchema JSON")
}

fn any_object_schema() -> schemars::schema::RootSchema {
    schema(serde_json::json!({"type": "object"}))
}

fn simple_spec(name: ToolName) -> conway_core::content::ToolSpec {
    conway_core::content::ToolSpec {
        name,
        description: "test tool".into(),
        schema: any_object_schema(),
        category: ToolCategory::Read,
        permission: PermissionClass::Safe,
    }
}

fn text_output(text: impl Into<String>) -> ToolOutput {
    ToolOutput {
        blocks: vec![ContentBlock::Text { text: text.into() }],
        is_error: false,
        truncation: conway_core::content::TruncationPolicy::None,
        artifacts: vec![],
    }
}

/// Echoes its arguments back as text.
struct EchoTool(ToolName);

#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> conway_core::content::ToolSpec {
        simple_spec(self.0.clone())
    }

    async fn invoke(&self, call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(text_output(call.arguments.to_string()))
    }
}

/// A tool with a schema requiring `{"path": <string>}`.
struct TypedTool(ToolName);

#[async_trait]
impl Tool for TypedTool {
    fn spec(&self) -> conway_core::content::ToolSpec {
        conway_core::content::ToolSpec {
            name: self.0.clone(),
            description: "typed tool".into(),
            schema: schema(serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            })),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        panic!("TypedTool::invoke must never be called on invalid arguments");
    }
}

/// Sleeps for a fixed delay, then returns fixed text.
struct DelayTool {
    name: ToolName,
    delay: Duration,
}

#[async_trait]
impl Tool for DelayTool {
    fn spec(&self) -> conway_core::content::ToolSpec {
        simple_spec(self.name.clone())
    }

    async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        tokio::time::sleep(self.delay).await;
        Ok(text_output("done"))
    }
}

/// Tracks concurrent in-flight `invoke` calls via `current`/`peak`.
struct ConcurrencyTool {
    name: ToolName,
    current: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    delay: Duration,
}

#[async_trait]
impl Tool for ConcurrencyTool {
    fn spec(&self) -> conway_core::content::ToolSpec {
        simple_spec(self.name.clone())
    }

    async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        self.current.fetch_sub(1, Ordering::SeqCst);
        Ok(text_output("done"))
    }
}

/// Always panics.
struct PanicTool(ToolName);

#[async_trait]
impl Tool for PanicTool {
    fn spec(&self) -> conway_core::content::ToolSpec {
        simple_spec(self.0.clone())
    }

    async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        panic!("kaboom");
    }
}

/// Returns a long text output with a `HeadTail` truncation policy.
struct BigOutputTool {
    name: ToolName,
    text: String,
}

#[async_trait]
impl Tool for BigOutputTool {
    fn spec(&self) -> conway_core::content::ToolSpec {
        simple_spec(self.name.clone())
    }

    async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: self.text.clone(),
            }],
            is_error: false,
            truncation: conway_core::content::TruncationPolicy::HeadTail {
                head_bytes: 20,
                tail_bytes: 20,
            },
            artifacts: vec![],
        })
    }
}

struct FakePlugin {
    id: String,
    tools: Vec<Arc<dyn Tool>>,
}

impl Plugin for FakePlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.id.clone(),
            version: "0.0.0".into(),
            tools: self.tools.iter().map(|t| t.spec().name).collect(),
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}

fn plugin(id: &str, tools: Vec<Arc<dyn Tool>>) -> Arc<dyn Plugin> {
    Arc::new(FakePlugin {
        id: id.into(),
        tools,
    })
}

fn registry(tools: Vec<Arc<dyn Tool>>) -> Arc<PluginRegistry> {
    Arc::new(PluginRegistry::from_plugins(vec![plugin("fake", tools)]).unwrap())
}

fn call(id: &str, tool: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        call_id: id.into(),
        name: ToolName::new(tool),
        arguments: args,
    }
}

fn runner_with_gate(
    registry: Arc<PluginRegistry>,
    decision: PermissionDecision,
) -> (ToolRunner, Arc<EventBus>) {
    let bus = EventBus::new(1024);
    let gate: Arc<dyn conway_core::ports::PermissionGate> = Arc::new(FakeGate::new(decision));
    let broker = Arc::new(PermissionBroker::new(gate, bus.clone()));
    (ToolRunner::new(registry, broker, bus.clone()), bus)
}

fn batch_ctx(max_parallel_tools: usize) -> ToolBatchCtx {
    ToolBatchCtx {
        agent_id: AgentId::new(),
        agent_path: vec![],
        session_id: SessionId::new(),
        cwd: PathBuf::from("/tmp"),
        cancel: CancellationToken::new(),
        subagents: Arc::new(FakeSubagentHost::new(AgentId::new())) as Arc<dyn SubagentHost>,
        plugin_config: Arc::new(PluginConfig::default()),
        max_parallel_tools,
    }
}

fn text_of(outcome: &conway_runtime::tools::ToolOutcome) -> String {
    outcome
        .blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------
// PluginRegistry
// ---------------------------------------------------------------------

#[test]
fn duplicate_tool_name_errors_naming_both_plugins() {
    let a = plugin("plugin-a", vec![Arc::new(EchoTool(ToolName::new("dup")))]);
    let b = plugin("plugin-b", vec![Arc::new(EchoTool(ToolName::new("dup")))]);

    let err = match PluginRegistry::from_plugins(vec![a, b]) {
        Ok(_) => panic!("expected a duplicate-tool error"),
        Err(err) => err,
    };
    let msg = err.to_string();
    assert!(msg.contains("plugin-a"), "{msg}");
    assert!(msg.contains("plugin-b"), "{msg}");
    assert!(msg.contains("dup"), "{msg}");
}

#[test]
fn specs_are_lexicographically_ordered() {
    let reg = PluginRegistry::from_plugins(vec![plugin(
        "fake",
        vec![
            Arc::new(EchoTool(ToolName::new("zebra"))),
            Arc::new(EchoTool(ToolName::new("apple"))),
            Arc::new(EchoTool(ToolName::new("mango"))),
        ],
    )])
    .unwrap();

    let names: Vec<String> = reg
        .specs(None)
        .into_iter()
        .map(|s| s.name.as_str().to_string())
        .collect();
    assert_eq!(names, vec!["apple", "mango", "zebra"]);
}

#[test]
fn specs_respects_selector() {
    let reg = PluginRegistry::from_plugins(vec![plugin(
        "fake",
        vec![
            Arc::new(EchoTool(ToolName::new("read"))),
            Arc::new(EchoTool(ToolName::new("write"))),
        ],
    )])
    .unwrap();

    let selector = ToolSelector::Only(vec!["read".into()]);
    let names: Vec<String> = reg
        .specs(Some(&selector))
        .into_iter()
        .map(|s| s.name.as_str().to_string())
        .collect();
    assert_eq!(names, vec!["read"]);
}

// ---------------------------------------------------------------------
// ToolRunner: ordering, validation, permissions
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outcomes_returned_in_input_call_id_order_regardless_of_completion_order() {
    let reg = registry(vec![
        Arc::new(DelayTool {
            name: ToolName::new("slow"),
            delay: Duration::from_millis(80),
        }),
        Arc::new(DelayTool {
            name: ToolName::new("fast"),
            delay: Duration::from_millis(1),
        }),
    ]);
    let (runner, _bus) = runner_with_gate(reg, PermissionDecision::AllowOnce);
    let ctx = batch_ctx(4);

    let outcomes = runner
        .run_batch(
            &ctx,
            vec![
                call("c1", "slow", serde_json::json!({})),
                call("c2", "fast", serde_json::json!({})),
            ],
        )
        .await;

    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0].call_id, "c1");
    assert_eq!(outcomes[1].call_id, "c2");
    assert!(!outcomes[0].is_error && !outcomes[1].is_error);
}

#[tokio::test]
async fn schema_invalid_arguments_are_error_without_invoking() {
    let reg = registry(vec![Arc::new(TypedTool(ToolName::new("typed")))]);
    let (runner, _bus) = runner_with_gate(reg, PermissionDecision::AllowOnce);
    let ctx = batch_ctx(4);

    // `path` is present but the wrong type, so `instance_path` names it.
    let outcomes = runner
        .run_batch(
            &ctx,
            vec![call("c1", "typed", serde_json::json!({"path": 5}))],
        )
        .await;

    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].is_error);
    let text = text_of(&outcomes[0]);
    assert!(text.contains("/path"), "{text}");
}

#[tokio::test]
async fn unknown_tool_is_error() {
    let reg = registry(vec![]);
    let (runner, _bus) = runner_with_gate(reg, PermissionDecision::AllowOnce);
    let ctx = batch_ctx(4);

    let outcomes = runner
        .run_batch(&ctx, vec![call("c1", "nope", serde_json::json!({}))])
        .await;

    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].is_error);
    assert!(text_of(&outcomes[0]).contains("nope"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn denied_call_emits_no_started_and_carries_denial_text() {
    let reg = registry(vec![Arc::new(EchoTool(ToolName::new("read")))]);
    let (runner, bus) = runner_with_gate(
        reg,
        PermissionDecision::Deny {
            reason: "no way".into(),
        },
    );
    let mut stream = bus.subscribe();
    let ctx = batch_ctx(4);

    let outcomes = runner
        .run_batch(&ctx, vec![call("c1", "read", serde_json::json!({}))])
        .await;

    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].is_error);
    assert!(text_of(&outcomes[0]).contains("no way"));

    // Drain every envelope emitted for this call: must never include
    // `ToolCallStarted`.
    let mut tags = Vec::new();
    while let Ok(Some(envelope)) = tokio::time::timeout(
        Duration::from_millis(200),
        futures::StreamExt::next(&mut stream),
    )
    .await
    {
        tags.push(event_tag(&envelope.event));
    }
    assert!(!tags.contains(&"tool_call_started"), "{tags:?}");
    assert!(tags.contains(&"tool_call_proposed"), "{tags:?}");
    assert!(tags.contains(&"permission_requested"), "{tags:?}");
    assert!(tags.contains(&"permission_resolved"), "{tags:?}");
}

fn event_tag(event: &Event) -> &'static str {
    match event {
        Event::ToolCallProposed { .. } => "tool_call_proposed",
        Event::PermissionRequested { .. } => "permission_requested",
        Event::PermissionResolved { .. } => "permission_resolved",
        Event::ToolCallStarted { .. } => "tool_call_started",
        Event::ToolProgress { .. } => "tool_progress",
        Event::ToolCallFinished { .. } => "tool_call_finished",
        _ => "other",
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_call_event_order_is_proposed_permission_started_finished() {
    let reg = registry(vec![Arc::new(EchoTool(ToolName::new("read")))]);
    let (runner, bus) = runner_with_gate(reg, PermissionDecision::AllowOnce);
    let mut stream = bus.subscribe();
    let ctx = batch_ctx(4);

    let outcomes = runner
        .run_batch(&ctx, vec![call("c1", "read", serde_json::json!({}))])
        .await;
    assert!(!outcomes[0].is_error);

    let mut tags = Vec::new();
    while let Ok(Some(envelope)) = tokio::time::timeout(
        Duration::from_millis(200),
        futures::StreamExt::next(&mut stream),
    )
    .await
    {
        tags.push(event_tag(&envelope.event));
    }
    assert_eq!(
        tags,
        vec![
            "tool_call_proposed",
            "permission_requested",
            "permission_resolved",
            "tool_call_started",
            "tool_call_finished",
        ]
    );
}

// ---------------------------------------------------------------------
// ToolRunner: concurrency, cancellation, panics
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn max_parallel_tools_bounds_peak_concurrency() {
    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let reg = registry(vec![Arc::new(ConcurrencyTool {
        name: ToolName::new("work"),
        current: current.clone(),
        peak: peak.clone(),
        delay: Duration::from_millis(60),
    })]);
    let (runner, _bus) = runner_with_gate(reg, PermissionDecision::AllowOnce);
    let ctx = batch_ctx(2);

    let calls = (0..5)
        .map(|i| call(&format!("c{i}"), "work", serde_json::json!({})))
        .collect();
    let outcomes = runner.run_batch(&ctx, calls).await;

    assert_eq!(outcomes.len(), 5);
    assert!(outcomes.iter().all(|o| !o.is_error));
    assert_eq!(peak.load(Ordering::SeqCst), 2);
    assert_eq!(current.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn batch_cancellation_returns_within_100ms_with_cancelled_outcomes() {
    let reg = registry(vec![Arc::new(DelayTool {
        name: ToolName::new("slow"),
        delay: Duration::from_secs(5),
    })]);
    let (runner, _bus) = runner_with_gate(reg, PermissionDecision::AllowOnce);
    let mut ctx = batch_ctx(2);
    let cancel = CancellationToken::new();
    ctx.cancel = cancel.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
    });

    let calls = vec![
        call("c1", "slow", serde_json::json!({})),
        call("c2", "slow", serde_json::json!({})),
        call("c3", "slow", serde_json::json!({})),
    ];

    let start = Instant::now();
    let outcomes = runner.run_batch(&ctx, calls).await;
    let elapsed = start.elapsed();

    assert!(elapsed < Duration::from_millis(100), "{elapsed:?}");
    assert_eq!(outcomes.len(), 3);
    assert!(outcomes.iter().all(|o| o.is_error));
    for outcome in &outcomes {
        assert!(text_of(outcome).contains("cancelled"), "{outcome:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn panicking_tool_yields_error_outcome_naming_tool() {
    let reg = registry(vec![Arc::new(PanicTool(ToolName::new("boom")))]);
    let (runner, _bus) = runner_with_gate(reg, PermissionDecision::AllowOnce);
    let ctx = batch_ctx(4);

    let outcomes = runner
        .run_batch(&ctx, vec![call("c1", "boom", serde_json::json!({}))])
        .await;

    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].is_error);
    assert!(text_of(&outcomes[0]).contains("boom"), "{outcomes:?}");
}

// ---------------------------------------------------------------------
// ToolRunner: truncation
// ---------------------------------------------------------------------

#[tokio::test]
async fn head_tail_truncation_is_applied_and_recorded() {
    let big_text: String = "a".repeat(1000);
    let reg = registry(vec![Arc::new(BigOutputTool {
        name: ToolName::new("big"),
        text: big_text.clone(),
    })]);
    let (runner, _bus) = runner_with_gate(reg, PermissionDecision::AllowOnce);
    let ctx = batch_ctx(4);

    let outcomes = runner
        .run_batch(&ctx, vec![call("c1", "big", serde_json::json!({}))])
        .await;

    assert_eq!(outcomes.len(), 1);
    assert!(!outcomes[0].is_error);
    let record = outcomes[0]
        .truncation
        .as_ref()
        .expect("output should have been truncated");
    assert_eq!(record.original_bytes, 1000);
    // Exactly head_bytes + tail_bytes of ORIGINAL content is retained — the
    // elision marker's own bytes must not inflate the audit record
    // (cycle-1 review S1).
    assert_eq!(record.kept_bytes, 40);
    match record.policy {
        conway_core::content::TruncationPolicy::HeadTail {
            head_bytes,
            tail_bytes,
        } => {
            assert_eq!(head_bytes, 20);
            assert_eq!(tail_bytes, 20);
        }
        other => panic!("expected HeadTail, got {other:?}"),
    }
    let text = text_of(&outcomes[0]);
    assert!(text.contains("bytes omitted"), "{text}");
    assert!(text.len() < big_text.len());
}

#[tokio::test]
async fn small_output_under_limit_is_not_truncated() {
    let reg = registry(vec![Arc::new(BigOutputTool {
        name: ToolName::new("small"),
        text: "hi".into(),
    })]);
    let (runner, _bus) = runner_with_gate(reg, PermissionDecision::AllowOnce);
    let ctx = batch_ctx(4);

    let outcomes = runner
        .run_batch(&ctx, vec![call("c1", "small", serde_json::json!({}))])
        .await;

    assert!(outcomes[0].truncation.is_none());
    assert_eq!(text_of(&outcomes[0]), "hi");
}
