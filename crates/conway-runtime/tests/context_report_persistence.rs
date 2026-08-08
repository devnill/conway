//! Acceptance tests for WI-087 (`ContextReport` persistence and provenance
//! inspection API): every turn's `ContextReport` is durably persisted after
//! its assistant record, `Runtime::context_report_at` reads historical/
//! post-restart reports back, and `Runtime::context_report` continues to
//! serve the live/last-turn value it always has.
//!
//! Built entirely from `conway-core`'s fakes plus local scripted doubles
//! (mirrors `runtime_api.rs`'s and `subagent_fork_spawn.rs`'s own practice)
//! -- this file does not depend on `conway-backends` or `conway-tools`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use conway_core::agent::{AgentDefRef, Budget, PermissionDecision, SubagentSpec};
use conway_core::capabilities::HeadroomPolicy;
use conway_core::config::AgentDef;
use conway_core::content::{
    ContentBlock, PermissionClass, StopReason, ToolCall, ToolCategory, ToolSpec, TruncationPolicy,
    Usage,
};
use conway_core::error::{RuntimeError, ToolError};
use conway_core::event::Event;
use conway_core::fakes::{
    FakeGate, FakeHealth, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn,
};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, SeqRange, ToolName};
use conway_core::log::LogRecord;
use conway_core::ports::{
    Backend, GenerateResponse, Plugin, PluginManifest, Router, SessionStore, SubagentHost, Tool,
    ToolCtx, ToolOutput,
};
use conway_core::provenance::Provenance;
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{RootSpec, Runtime, RuntimeDeps};
use futures::StreamExt;

// ---------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------

fn text_response(text: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
    }
}

fn tool_call_response(call_id: &str, tool: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: ToolName::new(tool),
            arguments: serde_json::json!({}),
        }],
        stop: StopReason::ToolUse,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
    }
}

fn tool_spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: ToolName::new(name),
        description: "test tool".into(),
        schema: serde_json::from_value(serde_json::json!({"type": "object"})).unwrap(),
        category: ToolCategory::Read,
        permission: PermissionClass::Safe,
    }
}

/// A tool with a deliberate delay before returning -- gives a test a window
/// to send a steer while the child's tool call is in flight (mirrors
/// `subagent_fork_spawn.rs`'s `SlowTool`/`steering.rs`'s `DelayTool`).
struct SlowTool {
    delay: Duration,
}

#[async_trait]
impl Tool for SlowTool {
    fn spec(&self) -> ToolSpec {
        tool_spec("slow")
    }

    async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        tokio::time::sleep(self.delay).await;
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "done".into(),
            }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

/// A tool whose output is long enough to be truncated by its own declared
/// `TruncationPolicy::Head` -- gives the truncation-visibility test a
/// `LogRecord::ToolResultRecord.truncated` to inspect.
struct TruncatingTool;

#[async_trait]
impl Tool for TruncatingTool {
    fn spec(&self) -> ToolSpec {
        tool_spec("bigread")
    }

    async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "x".repeat(200),
            }],
            is_error: false,
            truncation: TruncationPolicy::Head { max_bytes: 10 },
            artifacts: vec![],
        })
    }
}

struct FakePlugin {
    tools: Vec<Arc<dyn Tool>>,
}

impl Plugin for FakePlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "test".to_string(),
            version: "0.0.0".to_string(),
            tools: self.tools.iter().map(|t| t.spec().name).collect(),
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}

/// Builds a `Runtime` whose backend plays back `script` in order across
/// every turn of every agent this test starts, and whose `agent_defs`/
/// `plugins` are as given. Returns the runtime plus the backing store, so
/// tests can assert on persisted records directly (mirrors
/// `runtime_api.rs`'s/`subagent_fork_spawn.rs`'s own `build_runtime`).
fn build_runtime(
    script: Vec<ScriptedTurn>,
    agent_defs: HashMap<String, AgentDef>,
    plugins: Vec<Arc<dyn Plugin>>,
) -> (Arc<Runtime>, Arc<dyn SessionStore>) {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(ScriptedBackend::new(script).with_id(BackendId::new("b")));
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);

    let runtime = Runtime::new(RuntimeDeps {
        store: store.clone(),
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins,
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs,
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),
    });
    (runtime, store)
}

fn root_spec(prompt: &str, agent_def: Option<AgentDefRef>) -> RootSpec {
    RootSpec {
        session: None,
        agent_def,
        role: Some(RoleAlias::new("planner")),
        tools: None,
        budget: Budget::default(),
        cwd: PathBuf::from("/tmp"),
        root: None,
        prompt: Some(prompt.to_string()),
        keep_alive: false,
        model: None,
    }
}

fn reviewer_def() -> AgentDef {
    AgentDef {
        name: "reviewer".to_string(),
        description: None,
        system_prompt: "You are a careful reviewer.".to_string(),
        role: Some(RoleAlias::new("reviewer")),
        model: None,
        tools: conway_core::agent::ToolSelector::All,
        skills: Vec::new(),
        max_steps: None,
        result_contract: None,
    }
}

async fn wait_for_agent_finished(
    stream: &mut conway_runtime::events::EventStream,
    agent: AgentId,
) -> conway_core::agent::AgentResult {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let envelope = stream.next().await.expect("event stream ended early");
            if envelope.agent == agent {
                if let Event::AgentFinished { result, .. } = envelope.event {
                    return result;
                }
            }
        }
    })
    .await
    .expect("agent never finished")
}

async fn wait_for_tool_call_started(
    stream: &mut conway_runtime::events::EventStream,
    agent: AgentId,
) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let envelope = stream.next().await.expect("event stream ended early");
            if envelope.agent == agent && matches!(envelope.event, Event::ToolCallStarted { .. }) {
                return;
            }
        }
    })
    .await
    .expect("tool call never started")
}

async fn start_and_finish_root(runtime: &Runtime, prompt: &str) -> AgentId {
    let mut stream = runtime.subscribe();
    let root = runtime.start_root(root_spec(prompt, None)).await.unwrap();
    wait_for_agent_finished(&mut stream, root).await;
    root
}

fn provenance_kind(p: &Provenance) -> &'static str {
    match p {
        Provenance::UserPrompt => "user_prompt",
        Provenance::AgentDef { .. } => "agent_def",
        Provenance::Skill { .. } => "skill",
        Provenance::ToolRegistry { .. } => "tool_registry",
        Provenance::Inherited { .. } => "inherited",
        Provenance::ForkDirective { .. } => "fork_directive",
        Provenance::ParentSteer { .. } => "parent_steer",
        Provenance::ToolResult { .. } => "tool_result",
        Provenance::SystemNote { .. } => "system_note",
        _ => "unknown",
    }
}

// ---------------------------------------------------------------------
// Write path: every turn persists its report after the assistant record
// ---------------------------------------------------------------------

#[tokio::test]
async fn every_turn_persists_a_context_report_after_its_assistant_record() {
    let (runtime, store) = build_runtime(
        vec![ScriptedTurn::Respond(text_response("hi"))],
        HashMap::new(),
        vec![],
    );
    let root = start_and_finish_root(&runtime, "hello").await;

    let session = runtime
        .tree()
        .nodes
        .iter()
        .find(|n| n.agent_id == root)
        .unwrap()
        .session;
    let records = store.read(&session, SeqRange::full()).await.unwrap();

    let assistant_index = records
        .iter()
        .position(|r| matches!(r, LogRecord::Assistant { .. }))
        .expect("an assistant record was persisted");
    let report_index = records
        .iter()
        .position(|r| matches!(r, LogRecord::ContextReportRecord { .. }))
        .expect("a context report record was persisted");
    assert!(
        report_index > assistant_index,
        "the context report record (index {report_index}) must follow the assistant record \
         (index {assistant_index}) it describes"
    );
}

// ---------------------------------------------------------------------
// context_report: live == persisted-equivalent for a finished agent;
// unknown agent errors; known-but-never-run returns an empty report.
// ---------------------------------------------------------------------

#[tokio::test]
async fn context_report_matches_persisted_value_for_a_finished_agent() {
    let (runtime, store) = build_runtime(
        vec![ScriptedTurn::Respond(text_response("hi"))],
        HashMap::new(),
        vec![],
    );
    let root = start_and_finish_root(&runtime, "hello").await;

    let live = runtime.context_report(root).unwrap();
    let session = runtime
        .tree()
        .nodes
        .iter()
        .find(|n| n.agent_id == root)
        .unwrap()
        .session;
    let persisted = conway_session::provenance::load_context_report(&*store, &session, live.turn)
        .await
        .unwrap()
        .expect("a report was persisted for this turn");

    assert_eq!(
        live, persisted,
        "the live slot and the durable store must carry the identical report for a finished turn"
    );
}

#[tokio::test]
async fn context_report_unknown_agent_errors_known_never_run_is_empty() {
    let (runtime, _store) = build_runtime(
        vec![ScriptedTurn::Respond(text_response("hi"))],
        HashMap::new(),
        vec![],
    );

    let err = runtime.context_report(AgentId::new()).unwrap_err();
    assert!(matches!(err, RuntimeError::AgentNotFound { .. }));

    // `start_root` returns before the first turn completes (WI-082), so
    // immediately after it the agent is known but has not yet built a
    // report.
    let root = runtime.start_root(root_spec("hello", None)).await.unwrap();
    let report = runtime.context_report(root).unwrap();
    assert!(report.segments.is_empty());
    assert_eq!(report.total_tokens_est, 0);
}

// ---------------------------------------------------------------------
// Restart: a fresh Runtime over the same store resolves a completed
// agent's report byte-for-byte, purely via context_report_at (no
// in-memory state survives the "restart").
// ---------------------------------------------------------------------

#[tokio::test]
async fn restart_over_the_same_store_returns_a_byte_equal_report() {
    let (runtime1, store) = build_runtime(
        vec![ScriptedTurn::Respond(text_response("hi"))],
        HashMap::new(),
        vec![],
    );
    let root = start_and_finish_root(&runtime1, "hello").await;
    let live_report = runtime1.context_report(root).unwrap();
    drop(runtime1);

    // A brand new `Runtime`, sharing only the store -- no `agents` map
    // entry for `root` exists in this instance.
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("unused"))])
            .with_id(BackendId::new("b2")),
    );
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);
    let runtime2 = Runtime::new(RuntimeDeps {
        store: store.clone(),
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),
    });

    let restored = runtime2
        .context_report_at(root, live_report.turn)
        .await
        .unwrap();
    assert_eq!(
        restored, live_report,
        "a fresh Runtime over the same store must resolve the identical report"
    );
}

// ---------------------------------------------------------------------
// context_report_at: historical turns, out-of-range typed error.
// ---------------------------------------------------------------------

#[tokio::test]
async fn context_report_at_out_of_range_names_the_valid_turn_range() {
    let (runtime, _store) = build_runtime(
        vec![ScriptedTurn::Respond(text_response("hi"))],
        HashMap::new(),
        vec![],
    );
    let root = start_and_finish_root(&runtime, "hello").await;

    let err = runtime.context_report_at(root, 99).await.unwrap_err();
    match &err {
        RuntimeError::Tool(ToolError::Internal { detail }) => {
            assert!(
                detail.contains("99"),
                "detail must name the requested turn: {detail}"
            );
            assert!(
                detail.contains('0'),
                "detail must name the valid range (turn 0 was persisted): {detail}"
            );
        }
        other => panic!("expected RuntimeError::Tool(ToolError::Internal{{..}}), got {other:?}"),
    }
}

#[tokio::test]
async fn context_report_at_unknown_agent_errors() {
    let (runtime, _store) = build_runtime(
        vec![ScriptedTurn::Respond(text_response("hi"))],
        HashMap::new(),
        vec![],
    );
    let err = runtime
        .context_report_at(AgentId::new(), 0)
        .await
        .unwrap_err();
    assert!(matches!(err, RuntimeError::AgentNotFound { .. }));
}

// ---------------------------------------------------------------------
// Completeness: fork-and-steer scenario covers every named provenance
// variant, `tokenizer`, and the per-entry/total token accounting.
// ---------------------------------------------------------------------

#[tokio::test]
async fn fork_and_steer_scenario_covers_every_named_provenance_variant() {
    let mut defs = HashMap::new();
    defs.insert("reviewer".to_string(), reviewer_def());

    let (runtime, _store) = build_runtime(
        vec![
            ScriptedTurn::Respond(text_response("ok")), // root's single turn
            ScriptedTurn::Respond(tool_call_response("c1", "slow")), // child turn 0
            ScriptedTurn::Respond(text_response("child done")), // child turn 1
        ],
        defs,
        vec![Arc::new(FakePlugin {
            tools: vec![Arc::new(SlowTool {
                delay: Duration::from_millis(150),
            })],
        })],
    );
    let mut stream = runtime.subscribe();

    // Root carries an `AgentDef` (`Provenance::AgentDef`).
    let root = runtime
        .start_root(root_spec(
            "investigate",
            Some(AgentDefRef("reviewer".to_string())),
        ))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    // Fork a child (`Provenance::Inherited` + `Provenance::ForkDirective`),
    // steer it mid-tool-call (`Provenance::ParentSteer`, landing alongside
    // the tool's own `Provenance::ToolResult` on the next turn).
    let child = SubagentHost::start(
        &*runtime,
        root,
        root,
        SubagentSpec::fork("go slow", Budget::default()),
    )
    .await
    .unwrap();
    wait_for_tool_call_started(&mut stream, child).await;
    SubagentHost::steer(
        &*runtime,
        root,
        child,
        "focus on the auth module".to_string(),
    )
    .await
    .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    let root_report = runtime.context_report_at(root, 0).await.unwrap();
    let turn0 = runtime.context_report_at(child, 0).await.unwrap();
    let turn1 = runtime.context_report_at(child, 1).await.unwrap();

    let mut seen: HashSet<&'static str> = HashSet::new();
    for report in [&root_report, &turn0, &turn1] {
        assert_eq!(report.tokenizer, "heuristic-chars4");
        let sum: u32 = report.segments.iter().map(|e| e.tokens_est).sum();
        assert_eq!(
            report.total_tokens_est, sum,
            "total_tokens_est must equal the sum of per-entry tokens_est"
        );
        // Entry count is the segment count by construction: `ContextBuilder::
        // build` derives `entries` from `segments` 1:1 (already golden-tested
        // at the builder level, WI-077's `context_golden.rs`); nothing outside
        // the builder can observe the two counts diverging, so this is
        // recorded here as a documented invariant rather than an independent
        // re-check.
        assert!(!report.segments.is_empty());
        for entry in &report.segments {
            seen.insert(provenance_kind(&entry.provenance));
        }
    }

    for required in [
        "agent_def",
        "tool_registry",
        "inherited",
        "fork_directive",
        "parent_steer",
        "tool_result",
    ] {
        assert!(
            seen.contains(required),
            "expected provenance kind {required:?} somewhere across the scenario, got {seen:?}"
        );
    }
}

// ---------------------------------------------------------------------
// Truncated tool result: visible via the ToolResult provenance's call_id
// cross-referenced against the persisted LogRecord::ToolResultRecord
// (ContextReportEntry itself carries no truncation field -- see
// `report.rs`'s module doc; the record it names already carries the full
// `TruncationRecord`).
// ---------------------------------------------------------------------

#[tokio::test]
async fn truncated_tool_result_is_visible_via_its_log_record() {
    let (runtime, store) = build_runtime(
        vec![
            ScriptedTurn::Respond(tool_call_response("tc_1", "bigread")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        HashMap::new(),
        vec![Arc::new(FakePlugin {
            tools: vec![Arc::new(TruncatingTool)],
        })],
    );

    let root = start_and_finish_root(&runtime, "read the big file").await;
    let session = runtime
        .tree()
        .nodes
        .iter()
        .find(|n| n.agent_id == root)
        .unwrap()
        .session;

    // The second turn's report contains the ToolResult provenance entry
    // for `tc_1` (the tool ran on turn 0, so its result appears on turn 1).
    let report = runtime.context_report_at(root, 1).await.unwrap();
    let matched = report.segments.iter().any(|e| {
        matches!(&e.provenance, Provenance::ToolResult { call_id, tool }
            if call_id == "tc_1" && tool.as_str() == "bigread")
    });
    assert!(matched, "expected a ToolResult entry for tc_1/bigread");

    let records = store.read(&session, SeqRange::full()).await.unwrap();
    let truncated = records.iter().find_map(|r| match r {
        LogRecord::ToolResultRecord { result, .. } if result.call_id == "tc_1" => {
            Some(result.truncated.clone())
        }
        _ => None,
    });
    assert!(
        matches!(truncated, Some(Some(_))),
        "the persisted ToolResultRecord for tc_1 must carry a visible TruncationRecord"
    );
}
