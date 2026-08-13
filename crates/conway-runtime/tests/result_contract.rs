//! Acceptance tests for WI-086's `AgentResult` contract: `ResultBuilder`
//! precedence (`report` tool over trailing text), the non-empty/status-
//! naming summary guarantee, every terminal path populating
//! `transcript_ref`/`usage`/`steps_taken`, the result-contract validation
//! decision procedure (`validate_result_contract`) both standalone and
//! wired live into `AgentLoop::run` (including through
//! `subagent.rs`'s `SubagentHost::start` plumbing from
//! `SubagentSpec::result_contract`), and the "no raw transcript ever
//! crosses the trust boundary" property of `AgentResult` itself.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use conway_core::agent::{
    AgentResult, Budget, PermissionDecision, ResultStatus, SubagentSpec, ToolSelector,
};
use conway_core::capabilities::{
    CacheMode, Capabilities, HeadroomPolicy, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{
    ContentBlock, PermissionClass, StopReason, ToolCall, ToolCategory, ToolSpec, Usage,
};
use conway_core::error::RoutingError;
use conway_core::fakes::{
    FakeGate, FakeRouter, FakeStore, FakeSubagentHost, ScriptedBackend, ScriptedTurn,
};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, SessionId, ToolName};
use conway_core::log::{LogRecord, SessionMeta};
use conway_core::ports::{
    Backend, GenerateResponse, HealthRegistry, PermissionGate, Plugin, PluginConfig,
    PluginManifest, Router, SessionStore, SubagentHost, Tool, ToolCtx, ToolOutput,
};
use conway_core::provenance::Provenance;
use conway_core::routing::{Route, RouteRequest, RoutingReason};
use conway_runtime::agent_loop::{AgentLoop, AgentSpec, LoopDeps};
use conway_runtime::attempt::AttemptEngine;
use conway_runtime::context::ContextBuilder;
use conway_runtime::events::EventBus;
use conway_runtime::permission::PermissionBroker;
use conway_runtime::result::{validate_result_contract, ContractOutcome};
use conway_runtime::runtime::{RootSpec, Runtime, RuntimeDeps};
use conway_runtime::tools::PluginRegistry;
use conway_runtime::tree::{AgentNode, AgentTree};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------

fn caps_ok() -> Capabilities {
    Capabilities {
        tool_calling: ToolCallSupport::Streaming { validated: true },
        cache: CacheMode::None,
        parallel_tool_calls: true,
        structured_output: StructuredOutput::None,
        max_context_tokens: 1_000_000,
        reasoning: false,
        reliability_tier: ReliabilityTier::Verified,
    }
}

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

fn empty_response() -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage::default(),
    }
}

fn tool_call_response(call_id: &str, tool: &str, args: serde_json::Value) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: ToolName::new(tool),
            arguments: args,
        }],
        stop: StopReason::ToolUse,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
    }
}

fn schema_any_object() -> schemars::schema::RootSchema {
    serde_json::from_value(serde_json::json!({"type": "object"})).unwrap()
}

fn tool_spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: ToolName::new(name),
        description: "test tool".into(),
        schema: schema_any_object(),
        category: ToolCategory::Think,
        permission: PermissionClass::Safe,
    }
}

/// A local structural stand-in for `conway-tools`' `ReportTool` (WI-065):
/// emits the same `{"conway_report": {...}}` envelope shape. Not imported
/// from `conway-tools` -- `conway-runtime` must not depend on that crate
/// (architecture boundary; see `result.rs`'s module doc).
struct FakeReportTool;

#[async_trait]
impl Tool for FakeReportTool {
    fn spec(&self) -> ToolSpec {
        tool_spec("report")
    }

    async fn invoke(
        &self,
        call: ToolCall,
        _ctx: ToolCtx,
    ) -> Result<ToolOutput, conway_core::error::ToolError> {
        let envelope = serde_json::json!({
            "conway_report": {
                "version": 1,
                "summary": call.arguments["summary"],
                "facts": call.arguments.get("facts").cloned().unwrap_or(serde_json::json!([])),
                "artifacts": call.arguments.get("artifacts").cloned().unwrap_or(serde_json::json!([])),
                "structured": call.arguments.get("structured").cloned().unwrap_or(serde_json::Value::Null),
            }
        });
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: envelope.to_string(),
            }],
            is_error: false,
            truncation: conway_core::content::TruncationPolicy::None,
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

// ---------------------------------------------------------------------
// Harness (trimmed from `tests/agent_loop_e2e.rs`'s own)
// ---------------------------------------------------------------------

async fn seed_prompt(store: &dyn SessionStore, prompt: &str) -> (SessionId, AgentId) {
    let session = SessionId::new();
    let agent = AgentId::new();
    store
        .create(SessionMeta {
            id: session,
            agent_id: agent,
            origin: None,
            agent_def: None,
            role: Some(RoleAlias::new("planner")),
            created: Utc::now(),
            cwd: PathBuf::from("/tmp"),
            labels: vec![],
            ephemeral: false,
            ask_origin: None,
            root: None,
        })
        .await
        .unwrap();
    let seq = store.head(&session).await.unwrap();
    store
        .append(
            &session,
            LogRecord::UserTurn {
                seq,
                ts: Utc::now(),
                text: prompt.to_string(),
                prov: Provenance::UserPrompt,
            },
        )
        .await
        .unwrap();
    (session, agent)
}

fn build_loop(
    session: SessionId,
    agent: AgentId,
    store: Arc<dyn SessionStore>,
    backend: Arc<dyn Backend>,
    tools: Vec<Arc<dyn Tool>>,
    router: Arc<dyn Router>,
    budget: Budget,
) -> AgentLoop {
    build_loop_with_contract(session, agent, store, backend, tools, router, budget, None)
}

#[allow(clippy::too_many_arguments)]
fn build_loop_with_contract(
    session: SessionId,
    agent: AgentId,
    store: Arc<dyn SessionStore>,
    backend: Arc<dyn Backend>,
    tools: Vec<Arc<dyn Tool>>,
    router: Arc<dyn Router>,
    budget: Budget,
    result_contract: Option<schemars::schema::RootSchema>,
) -> AgentLoop {
    let bus = EventBus::new(1024);
    let health: Arc<dyn HealthRegistry> = Arc::new(conway_core::fakes::FakeHealth::new());
    let mut backends: std::collections::HashMap<BackendId, Arc<dyn Backend>> =
        std::collections::HashMap::new();
    backends.insert(backend.id(), backend);
    let attempt = Arc::new(AttemptEngine::new(backends, health, bus.clone()));
    let plugin_registry =
        Arc::new(PluginRegistry::from_plugins(vec![Arc::new(FakePlugin { tools })]).unwrap());
    let gate: Arc<dyn PermissionGate> = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let broker = Arc::new(PermissionBroker::new(gate, bus.clone()));
    let tool_runner = Arc::new(conway_runtime::tools::ToolRunner::new(
        plugin_registry.clone(),
        broker,
        bus.clone(),
    ));
    let subagents: Arc<dyn conway_core::ports::SubagentHost> =
        Arc::new(FakeSubagentHost::new(agent));
    let tree = Arc::new(AgentTree::new(bus.clone()));

    let deps = Arc::new(LoopDeps {
        store,
        router,
        attempt,
        registry: plugin_registry,
        tool_runner,
        subagents,
        plugin_config: Arc::new(PluginConfig::default()),
        bus: bus.clone(),
        builder: Arc::new(ContextBuilder::new()),
        headroom: Arc::new(HeadroomPolicy::default()),
        tree: tree.clone(),
        context_hook: std::sync::RwLock::new(None),
    });

    let spec = AgentSpec {
        system_prompt: None,
        skills: vec![],
        tools: None as Option<ToolSelector>,
        role: RoleAlias::new("planner"),
        pin: None,
        budget: budget.clone(),
        cache_mode: CacheMode::None,
        cache_ttl: conway_core::segment::CacheTtl::FiveMinutes,
        headroom_override: None,
        max_parallel_tools: 4,
        report_slot: None,
        result_contract,
        keep_alive: false,
        tag: None,
    };

    let cancel = CancellationToken::new();
    tree.attach(AgentNode {
        id: agent,
        parent: None,
        session,
        kind: None,
        agent_def: None,
        role: Some(RoleAlias::new("planner")),
        budget,
        cancel: cancel.clone(),
        inherited_upto: None,
        ephemeral: false,
    })
    .expect("fresh tree attach never fails");
    let (_mailbox_tx, mailbox_rx) =
        conway_runtime::mailbox::Mailbox::new(conway_runtime::mailbox::RUNTIME_CAPACITY);
    AgentLoop {
        agent_id: agent,
        session,
        parent: None,
        agent_path: vec![agent],
        cwd: PathBuf::from("/tmp"),
        root: None,
        deps,
        spec,
        cancel,
        inherited: None,
        inbox: mailbox_rx,
        parent_mailbox: None,
        pending_cancel: None,
        resume_gate: Default::default(),
    }
}

fn route() -> Arc<dyn Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("scripted"),
        model: ModelId::new("m"),
    }))
}

fn scripted(script: Vec<ScriptedTurn>) -> Arc<dyn Backend> {
    Arc::new(
        ScriptedBackend::new(script)
            .with_id(BackendId::new("scripted"))
            .with_capabilities(caps_ok()),
    )
}

// ---------------------------------------------------------------------
// Precedence: `report` tool call wins over trailing text
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_report_call_in_the_final_turn_takes_precedence_over_trailing_text() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    let backend = scripted(vec![
        ScriptedTurn::Respond(tool_call_response(
            "tc_1",
            "report",
            serde_json::json!({
                "summary": "the report's own summary",
                "facts": [{"key": "k", "value": "v", "source": null}],
                "structured": {"answer": 42}
            }),
        )),
        // Trailing text on the NEXT (final) turn -- must lose to the
        // `report` call the agent already made.
        ScriptedTurn::Respond(text_response("trailing text nobody should see")),
    ]);
    let tool: Arc<dyn Tool> = Arc::new(FakeReportTool);

    let agent_loop = build_loop(
        session,
        agent,
        store,
        backend,
        vec![tool],
        route(),
        Budget::default(),
    );
    let result = agent_loop.run().await;

    assert_eq!(result.status, ResultStatus::Completed);
    assert_eq!(result.summary, "the report's own summary");
    assert_eq!(result.facts.len(), 1);
    assert_eq!(result.structured, Some(serde_json::json!({"answer": 42})));
}

#[tokio::test]
async fn no_report_call_falls_back_to_trailing_text() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    let backend = scripted(vec![ScriptedTurn::Respond(text_response(
        "plain trailing text",
    ))]);

    let agent_loop = build_loop(
        session,
        agent,
        store,
        backend,
        vec![],
        route(),
        Budget::default(),
    );
    let result = agent_loop.run().await;

    assert_eq!(result.status, ResultStatus::Completed);
    assert_eq!(result.summary, "plain trailing text");
    assert!(result.facts.is_empty());
    assert!(result.structured.is_none());
}

// ---------------------------------------------------------------------
// Non-empty summary / status-naming fallback / truncation
// ---------------------------------------------------------------------

#[tokio::test]
async fn an_agent_producing_no_text_gets_a_status_naming_summary_not_an_empty_string() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    let backend = scripted(vec![ScriptedTurn::Respond(empty_response())]);

    let agent_loop = build_loop(
        session,
        agent,
        store,
        backend,
        vec![],
        route(),
        Budget::default(),
    );
    let result = agent_loop.run().await;

    assert_eq!(result.status, ResultStatus::Completed);
    assert!(!result.summary.is_empty(), "summary must never be empty");
    assert!(result.summary.contains("completed"));
}

#[tokio::test]
async fn budget_exceeded_also_gets_a_non_empty_status_naming_summary() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    // Always proposes a tool call, so the loop never completes on its own
    // and instead hits `max_steps`.
    let backend = scripted(vec![
        ScriptedTurn::Respond(tool_call_response("tc_1", "noop", serde_json::json!({}))),
        ScriptedTurn::Respond(tool_call_response("tc_2", "noop", serde_json::json!({}))),
    ]);
    struct NoopTool;
    #[async_trait]
    impl Tool for NoopTool {
        fn spec(&self) -> ToolSpec {
            tool_spec("noop")
        }
        async fn invoke(
            &self,
            _call: ToolCall,
            _ctx: ToolCtx,
        ) -> Result<ToolOutput, conway_core::error::ToolError> {
            Ok(ToolOutput {
                blocks: vec![],
                is_error: false,
                truncation: conway_core::content::TruncationPolicy::None,
                artifacts: vec![],
            })
        }
    }
    let tool: Arc<dyn Tool> = Arc::new(NoopTool);

    let budget = Budget {
        max_steps: 2,
        ..Budget::default()
    };
    let agent_loop = build_loop(session, agent, store, backend, vec![tool], route(), budget);
    let result = agent_loop.run().await;

    assert!(matches!(result.status, ResultStatus::BudgetExceeded { .. }));
    assert!(!result.summary.is_empty());
    assert!(result.summary.contains("budget_exceeded"));
}

// ---------------------------------------------------------------------
// Every terminal path populates transcript_ref/usage/steps_taken
// ---------------------------------------------------------------------

#[tokio::test]
async fn completed_and_failed_paths_populate_transcript_ref_usage_and_steps_taken() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    let backend = scripted(vec![ScriptedTurn::Respond(text_response("hi"))]);

    let agent_loop = build_loop(
        session,
        agent,
        store,
        backend,
        vec![],
        route(),
        Budget::default(),
    );
    let result = agent_loop.run().await;
    assert_eq!(result.status, ResultStatus::Completed);
    assert_eq!(result.transcript_ref, session);
    assert_eq!(result.usage.input_tokens, 10);
    assert_eq!(result.steps_taken, 1);

    // Failed: the router has no candidate at all.
    let store2: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session2, agent2) = seed_prompt(&*store2, "hello").await;
    let backend2 = scripted(vec![]);
    let failing_router: Arc<dyn Router> =
        Arc::new(FakeRouter::erroring(RoutingError::NoCandidate {
            role: RoleAlias::new("planner"),
            considered: vec![],
        }));
    let agent_loop2 = build_loop(
        session2,
        agent2,
        store2,
        backend2,
        vec![],
        failing_router,
        Budget::default(),
    );
    let result2 = agent_loop2.run().await;
    assert!(matches!(result2.status, ResultStatus::Failed { .. }));
    assert_eq!(result2.transcript_ref, session2);
    assert_eq!(result2.steps_taken, 0);
}

/// `Rejected` is exercised live elsewhere in this file
/// (`result_contract_retry_then_rejected_when_still_invalid` and
/// `a_spawned_childs_result_contract_is_enforced_through_subagent_host`);
/// `Cancelled` is already covered end to end by
/// `tests/agent_loop_e2e.rs::cancellation_mid_tool_batch_resolves_cancelled_within_100ms`.
/// This test instead asserts the shared construction path
/// (`AgentResult::new`) that every `AgentLoop::finish` call goes through
/// populates the three fields identically for both, directly.
#[test]
fn rejected_and_cancelled_populate_the_same_three_fields_via_agent_result_new() {
    let session = SessionId::new();
    let agent = AgentId::new();
    let mut result = AgentResult::new(
        agent,
        session,
        ResultStatus::Rejected {
            missing: vec!["/summary: is a required property".to_string()],
        },
        "attempted",
    );
    result.usage = Usage {
        input_tokens: 1,
        ..Default::default()
    };
    result.steps_taken = 3;
    assert_eq!(result.transcript_ref, session);
    assert_eq!(result.steps_taken, 3);
    assert_eq!(result.usage.input_tokens, 1);

    let mut result = AgentResult::new(
        agent,
        session,
        ResultStatus::Cancelled {
            reason: "cancelled".into(),
        },
        "",
    );
    result.steps_taken = 2;
    assert_eq!(result.transcript_ref, session);
    assert_eq!(result.steps_taken, 2);
}

// ---------------------------------------------------------------------
// Summary truncation at the finish boundary
// ---------------------------------------------------------------------

#[tokio::test]
async fn an_overlong_trailing_summary_is_truncated_to_2000_chars_at_finish() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    let long_text = "a".repeat(5000);
    let backend = scripted(vec![ScriptedTurn::Respond(text_response(&long_text))]);

    let agent_loop = build_loop(
        session,
        agent,
        store,
        backend,
        vec![],
        route(),
        Budget::default(),
    );
    let result = agent_loop.run().await;

    assert_eq!(result.status, ResultStatus::Completed);
    assert_eq!(result.summary.chars().count(), 2000);
}

// ---------------------------------------------------------------------
// Result-contract validation decision procedure
// ---------------------------------------------------------------------

fn schema_requiring_summary() -> schemars::schema::RootSchema {
    serde_json::from_value(serde_json::json!({
        "type": "object",
        "properties": { "summary": { "type": "string" } },
        "required": ["summary"],
    }))
    .unwrap()
}

#[tokio::test]
async fn valid_structured_output_on_the_first_attempt_causes_zero_retries() {
    let contract = schema_requiring_summary();
    let structured = serde_json::json!({"summary": "ok"});
    let outcome = validate_result_contract(Some(&structured), &contract, false);
    assert_eq!(outcome, ContractOutcome::Ok);
}

#[tokio::test]
async fn failing_structured_output_is_retried_exactly_once_then_rejected() {
    let contract = schema_requiring_summary();
    let missing_property = serde_json::json!({});

    // First failure: not yet retried -> `Retry`, never `Rejected`.
    match validate_result_contract(Some(&missing_property), &contract, false) {
        ContractOutcome::Retry { errors } => {
            assert!(
                !errors.is_empty(),
                "errors must enumerate the failing schema path"
            )
        }
        other => panic!("expected Retry on the first failure, got {other:?}"),
    }

    // Second failure, after a retry has already been spent -> terminal
    // `Rejected { missing }` enumerating the failing schema paths.
    match validate_result_contract(Some(&missing_property), &contract, true) {
        ContractOutcome::Rejected { missing } => {
            assert!(!missing.is_empty());
            assert!(
                missing.iter().any(|m| m.contains("summary")),
                "missing must name the failing path: {missing:?}"
            );
        }
        other => panic!("expected Rejected on the second failure, got {other:?}"),
    }
}

#[tokio::test]
async fn a_missing_structured_value_entirely_is_treated_as_null_and_fails_an_object_contract() {
    let contract = schema_requiring_summary();
    match validate_result_contract(None, &contract, false) {
        ContractOutcome::Retry { errors } => assert!(!errors.is_empty()),
        other => panic!("expected Retry, got {other:?}"),
    }
}

fn schema_requiring(prop: &str) -> schemars::schema::RootSchema {
    serde_json::from_value(serde_json::json!({
        "type": "object",
        "properties": { prop: { "type": "boolean" } },
        "required": [prop],
    }))
    .unwrap()
}

fn report_call(call_id: &str, structured: serde_json::Value) -> GenerateResponse {
    tool_call_response(
        call_id,
        "report",
        serde_json::json!({"summary": "a report", "structured": structured}),
    )
}

// ---------------------------------------------------------------------
// Result-contract enforcement wired live into `AgentLoop::run`
// ---------------------------------------------------------------------

#[tokio::test]
async fn result_contract_retry_then_completed_once_the_agent_corrects_structured() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    let backend = scripted(vec![
        // Turn 0: calls `report` with a `structured` value missing the
        // contract's required `ok` key.
        ScriptedTurn::Respond(report_call("tc_1", serde_json::json!({}))),
        // Turn 1: natural completion with no further tool calls -- the
        // FIRST validation attempt fails -> `Retry`: one more turn, no
        // terminal result yet.
        ScriptedTurn::Respond(text_response("still working")),
        // Turn 2: the agent calls `report` again, this time with a
        // corrected `structured` value.
        ScriptedTurn::Respond(report_call("tc_2", serde_json::json!({"ok": true}))),
        // Turn 3: natural completion -- now valid -> `Completed`, zero
        // further retries.
        ScriptedTurn::Respond(text_response("done")),
    ]);
    let tool: Arc<dyn Tool> = Arc::new(FakeReportTool);

    let agent_loop = build_loop_with_contract(
        session,
        agent,
        store.clone(),
        backend,
        vec![tool],
        route(),
        Budget::default(),
        Some(schema_requiring("ok")),
    );
    let result = agent_loop.run().await;

    assert_eq!(result.status, ResultStatus::Completed);
    assert_eq!(result.structured, Some(serde_json::json!({"ok": true})));

    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    let violation_notes = records
        .iter()
        .filter(|r| matches!(r, LogRecord::SystemNote { reason, .. } if reason == "result_contract_violation"))
        .count();
    assert_eq!(
        violation_notes, 1,
        "exactly one corrective retry must have been spent"
    );
}

#[tokio::test]
async fn result_contract_retry_then_rejected_when_still_invalid() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    let backend = scripted(vec![
        // Turn 0: `report` called once with an invalid `structured` value
        // -- never corrected afterward.
        ScriptedTurn::Respond(report_call("tc_1", serde_json::json!({}))),
        // Turn 1: natural completion -- 1st failure -> `Retry`.
        ScriptedTurn::Respond(text_response("still working")),
        // Turn 2: natural completion again, `structured` unchanged (no new
        // `report` call) -- 2nd failure, a retry has already been spent ->
        // terminal `Rejected { missing }`.
        ScriptedTurn::Respond(text_response("still failing")),
    ]);
    let tool: Arc<dyn Tool> = Arc::new(FakeReportTool);

    let agent_loop = build_loop_with_contract(
        session,
        agent,
        store.clone(),
        backend,
        vec![tool],
        route(),
        Budget::default(),
        Some(schema_requiring("ok")),
    );
    let result = agent_loop.run().await;

    match &result.status {
        ResultStatus::Rejected { missing } => {
            assert!(!missing.is_empty());
            assert!(missing.iter().any(|m| m.contains("ok")));
        }
        other => panic!("expected Rejected, got {other:?}"),
    }

    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    let violation_notes = records
        .iter()
        .filter(|r| matches!(r, LogRecord::SystemNote { reason, .. } if reason == "result_contract_violation"))
        .count();
    assert_eq!(
        violation_notes, 1,
        "exactly one retry -- never a second corrective note before Rejected"
    );
}

#[tokio::test]
async fn valid_structured_output_through_the_live_loop_causes_zero_retries() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    let backend = scripted(vec![
        ScriptedTurn::Respond(report_call("tc_1", serde_json::json!({"ok": true}))),
        ScriptedTurn::Respond(text_response("done")),
    ]);
    let tool: Arc<dyn Tool> = Arc::new(FakeReportTool);

    let agent_loop = build_loop_with_contract(
        session,
        agent,
        store.clone(),
        backend,
        vec![tool],
        route(),
        Budget::default(),
        Some(schema_requiring("ok")),
    );
    let result = agent_loop.run().await;

    assert_eq!(result.status, ResultStatus::Completed);
    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    assert!(
        !records
            .iter()
            .any(|r| matches!(r, LogRecord::SystemNote { reason, .. } if reason == "result_contract_violation")),
        "valid structured output on the first attempt must cause zero retries"
    );
}

/// A `Router` that resolves a distinct route per role, so a parent and a
/// child agent sharing one `Runtime` never contend over the same
/// `ScriptedBackend`'s script queue.
struct RoleRouter {
    parent: Route,
    child: Route,
}

impl Router for RoleRouter {
    fn resolve(&self, req: &RouteRequest) -> Result<Vec<Route>, RoutingError> {
        if req.role.as_str() == "child" {
            Ok(vec![self.child.clone()])
        } else {
            Ok(vec![self.parent.clone()])
        }
    }
}

/// Per the coordinator's ruling on this item's Self-Check: `result_contract`
/// enforcement must be reachable for a real subagent, not just a directly
/// -constructed `AgentLoop` -- `subagent.rs`'s `SubagentHost::start` carries
/// `SubagentSpec::result_contract` through to `AgentSpec::result_contract`
/// verbatim, and this test exercises exactly that path end to end.
#[tokio::test]
async fn a_spawned_childs_result_contract_is_enforced_through_subagent_host() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let health: Arc<dyn HealthRegistry> = Arc::new(conway_core::fakes::FakeHealth::new());

    let parent_backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("parent turn"))])
            .with_id(BackendId::new("parent-backend"))
            .with_capabilities(caps_ok()),
    );
    let child_backend = Arc::new(
        ScriptedBackend::new(vec![
            // Never calls `report` at all -- `structured` stays `None`,
            // which fails an object-shaped contract just like an empty
            // object does.
            ScriptedTurn::Respond(text_response("child turn 1")),
            ScriptedTurn::Respond(text_response("child turn 2")),
        ])
        .with_id(BackendId::new("child-backend"))
        .with_capabilities(caps_ok()),
    );
    let mut backends: std::collections::HashMap<BackendId, Arc<dyn Backend>> =
        std::collections::HashMap::new();
    backends.insert(parent_backend.id(), parent_backend);
    backends.insert(child_backend.id(), child_backend);

    let router = Arc::new(RoleRouter {
        parent: Route {
            backend: BackendId::new("parent-backend"),
            model: ModelId::new("m"),
            params: Default::default(),
            reason: RoutingReason::AliasPrimary {
                alias: RoleAlias::new("parent"),
            },
        },
        child: Route {
            backend: BackendId::new("child-backend"),
            model: ModelId::new("m"),
            params: Default::default(),
            reason: RoutingReason::AliasPrimary {
                alias: RoleAlias::new("child"),
            },
        },
    });

    let runtime = Runtime::new(RuntimeDeps {
        store,
        router,
        health,
        backends,
        plugins: vec![],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: std::collections::HashMap::new(),
        event_bus: EventBus::new(1024),
        headroom: Arc::new(HeadroomPolicy::default()),
    });

    let parent = runtime
        .start_root(RootSpec {
            session: None,
            agent_def: None,
            role: Some(RoleAlias::new("parent")),
            tools: None,
            budget: Budget::default(),
            cwd: PathBuf::from("/tmp"),
            root: None,
            prompt: Some("go".to_string()),
            keep_alive: false,
            model: None,
        })
        .await
        .unwrap();

    let mut spec = SubagentSpec::fork("do the child's work", Budget::default());
    spec.role = Some(RoleAlias::new("child"));
    spec.result_contract = Some(schema_requiring("ok"));

    let child = SubagentHost::start(&*runtime, parent, parent, spec)
        .await
        .unwrap();
    let result = SubagentHost::await_result(&*runtime, parent, child)
        .await
        .unwrap();

    match &result.status {
        ResultStatus::Rejected { missing } => assert!(!missing.is_empty()),
        other => panic!(
            "expected the spawned child's undeclared `structured` output to be Rejected by its \
             SubagentSpec::result_contract, got {other:?}"
        ),
    }
}

// ---------------------------------------------------------------------
// `AgentResult` never carries raw transcript content across the trust
// boundary -- only an id (`transcript_ref: SessionId`).
// ---------------------------------------------------------------------

#[test]
fn agent_result_serializes_only_the_bounded_field_set_no_raw_transcript() {
    let session = SessionId::new();
    let agent = AgentId::new();
    let mut result = AgentResult::new(agent, session, ResultStatus::Completed, "done");
    result.structured = Some(serde_json::json!({"k": "v"}));

    let json = serde_json::to_value(&result).unwrap();
    let mut keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(|s| s.as_str())
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "agent_id",
            "artifacts",
            "facts",
            "status",
            "steps_taken",
            "structured",
            "summary",
            "transcript_ref",
            "usage",
        ]
    );
    // `transcript_ref` is exactly the session id string -- an opaque
    // pointer, never the transcript's own content.
    assert_eq!(json["transcript_ref"], serde_json::json!(session));
    assert!(json["transcript_ref"].is_string());
}

// ---------------------------------------------------------------------
// keep_alive + result_contract: the two halves, pinned separately
// ---------------------------------------------------------------------

/// Builds the same loop as [`build_loop_with_contract`] but KEPT ALIVE.
///
/// Deliberately bypasses `SubagentSpec::validate`, which now rejects this
/// combination outright (board item 01KZS38F5TN3DEYHWG3VC0FZ9R). These two
/// tests pin the RUNTIME behaviour the rejection exists to prevent, so that
/// if anyone later removes the rejection believing it unnecessary, the
/// behaviour it was guarding is still described here in executable form.
#[allow(clippy::too_many_arguments)]
fn build_kept_alive_loop_with_contract(
    session: SessionId,
    agent: AgentId,
    store: Arc<dyn SessionStore>,
    backend: Arc<dyn Backend>,
    tools: Vec<Arc<dyn Tool>>,
    router: Arc<dyn Router>,
    budget: Budget,
    result_contract: Option<schemars::schema::RootSchema>,
) -> AgentLoop {
    let mut agent_loop = build_loop_with_contract(
        session,
        agent,
        store,
        backend,
        tools,
        router,
        budget,
        result_contract,
    );
    agent_loop.spec.keep_alive = true;
    agent_loop
}

/// HALF ONE — validation RUNS under `keep_alive`.
///
/// The spec this item was filed with claimed the contract "is never
/// evaluated" for a kept-alive agent. That was false, and asserting it would
/// have failed. Positive evidence is required here rather than the absence of
/// a violation note: absence is equally consistent with "never evaluated",
/// which is exactly the conflation this item was filed with. So the response
/// VIOLATES the contract, and the corrective `SystemNote` it produces is
/// proof the evaluation ran.
#[tokio::test]
async fn keep_alive_still_evaluates_its_result_contract() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    let backend = scripted(vec![
        ScriptedTurn::Respond(report_call("tc_1", serde_json::json!({}))),
        ScriptedTurn::Respond(text_response("still working")),
    ]);
    let tool: Arc<dyn Tool> = Arc::new(FakeReportTool);

    let agent_loop = build_kept_alive_loop_with_contract(
        session,
        agent,
        store.clone(),
        backend,
        vec![tool],
        route(),
        Budget::default(),
        Some(schema_requiring("ok")),
    );

    // The loop never returns (that is half two), so it is driven under a
    // timeout and the assertion is on what it PERSISTED, not on its return.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), agent_loop.run()).await;

    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    let violations = records
        .iter()
        .filter(|r| matches!(r, LogRecord::SystemNote { reason, .. } if reason == "result_contract_violation"))
        .count();
    assert!(
        violations >= 1,
        "a kept-alive agent must still evaluate its result_contract -- no \
         corrective note means the contract was skipped, which is the claim \
         this item was originally filed with and which is false"
    );
}

/// HALF TWO — delivery does NOT happen under `keep_alive`.
///
/// The contract PASSES here, so there is a validated result. A non-kept-alive
/// agent would return it and `await_result` would resolve. This one does not
/// return at all: `finish` is never reached, so no `AgentMessage::Result` is
/// ever sent and no caller can ever receive the value.
///
/// The assertion is that `run()` does NOT complete. That is the hang, stated
/// as an observable: a test that merely checked for the absence of a
/// violation note would pass just as happily against a build that never
/// evaluated the contract at all.
#[tokio::test]
async fn keep_alive_validates_its_contract_but_never_resolves_await_result() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    let backend = scripted(vec![
        ScriptedTurn::Respond(report_call("tc_1", serde_json::json!({"ok": true}))),
        ScriptedTurn::Respond(text_response("done")),
    ]);
    let tool: Arc<dyn Tool> = Arc::new(FakeReportTool);

    let agent_loop = build_kept_alive_loop_with_contract(
        session,
        agent,
        store.clone(),
        backend,
        vec![tool],
        route(),
        Budget::default(),
        Some(schema_requiring("ok")),
    );

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(3), agent_loop.run()).await;

    assert!(
        outcome.is_err(),
        "a kept-alive agent with a PASSING contract must not return -- if it \
         did, the delivery gap this item exists for is closed and the \
         rejection in SubagentSpec::validate should be revisited"
    );

    // And the contract really did pass: no corrective note was written, so
    // the non-return above is the delivery gap rather than a rejection loop.
    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    assert!(
        !records.iter().any(
            |r| matches!(r, LogRecord::SystemNote { reason, .. } if reason == "result_contract_violation")
        ),
        "this half pins the PASSING path -- a violation note means the script \
         no longer satisfies the contract and the test is measuring the wrong \
         thing"
    );
}

/// The FIX, at the type boundary: the combination is refused outright.
///
/// `SubagentSpec::validate` is the single chokepoint every subagent path
/// already passes through -- `SubagentHost::start` calls it before any cwd
/// resolution, store I/O or tree attach -- so one rejection covers the
/// model-invoked tools, the facade's `ForkSpec`/`SpawnSpec`, and any direct
/// library caller alike. Enforcing at one tool callsite instead would leave
/// every other caller able to construct the hang.
#[test]
fn keep_alive_with_a_result_contract_is_rejected_by_validate() {
    let mut spec = conway_core::agent::SubagentSpec::fork("go", Budget::default());
    spec.keep_alive = true;
    spec.result_contract = Some(schema_requiring("ok"));

    let err = spec
        .validate()
        .expect_err("keep_alive + result_contract must not validate");

    let rendered = err.to_string();
    // The message must name BOTH flags: a caller who set two things and got
    // one word back has to guess which one to change.
    assert!(
        rendered.contains("keep_alive") && rendered.contains("result_contract"),
        "the error must name both flags, got: {rendered}"
    );
}

/// Each flag ALONE still validates -- the rejection is about the combination,
/// not about either feature, and a guard that rejected too much would be a
/// regression wearing a fix's clothes.
#[test]
fn either_flag_alone_still_validates() {
    let mut kept_alive = conway_core::agent::SubagentSpec::fork("go", Budget::default());
    kept_alive.keep_alive = true;
    kept_alive
        .validate()
        .expect("keep_alive alone is a supported shape");

    let mut contracted = conway_core::agent::SubagentSpec::fork("go", Budget::default());
    contracted.result_contract = Some(schema_requiring("ok"));
    contracted
        .validate()
        .expect("result_contract alone is a supported shape");

    conway_core::agent::SubagentSpec::fork("go", Budget::default())
        .validate()
        .expect("neither flag is obviously fine");
}

/// THE OBSERVABLE OUTCOME: a real caller gets a typed error instead of a hang.
///
/// The `validate` test above proves the TYPE refuses the combination. This
/// proves the refusal actually reaches someone -- `SubagentHost::start`
/// surfaces it as `RuntimeError::InvalidSpec`, which the tool boundary in
/// turn maps to a model-correctable `ToolError::InvalidArguments`. Asserting
/// only at the type would leave "the runtime swallows it" untested, and this
/// item exists precisely because a failure nobody surfaced looked like
/// success.
///
/// Reuses the same two-role router harness as the enforcement test above so
/// the parent and child never contend over one scripted queue.
#[tokio::test]
async fn keep_alive_with_a_result_contract_is_refused_by_subagent_host() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let health: Arc<dyn HealthRegistry> = Arc::new(conway_core::fakes::FakeHealth::new());
    let parent_backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("ok"))])
            .with_id(BackendId::new("parent"))
            .with_capabilities(caps_ok()),
    );
    let mut backends: std::collections::HashMap<BackendId, Arc<dyn Backend>> =
        std::collections::HashMap::new();
    backends.insert(parent_backend.id(), parent_backend);
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("parent"),
        model: ModelId::new("m"),
    }));

    let runtime = Runtime::new(RuntimeDeps {
        store,
        router,
        health,
        backends,
        plugins: vec![],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: std::collections::HashMap::new(),
        event_bus: EventBus::new(1024),
        headroom: Arc::new(HeadroomPolicy::default()),
    });

    let parent = runtime
        .start_root(RootSpec {
            session: None,
            agent_def: None,
            role: Some(RoleAlias::new("parent")),
            tools: None,
            budget: Budget::default(),
            cwd: PathBuf::from("/tmp"),
            root: None,
            prompt: Some("go".to_string()),
            keep_alive: false,
            model: None,
        })
        .await
        .expect("root starts");

    let mut spec = SubagentSpec::fork("hold open and validate", Budget::default());
    spec.keep_alive = true;
    spec.result_contract = Some(schema_requiring("ok"));

    let err = SubagentHost::start(&*runtime, parent, parent, spec)
        .await
        .expect_err("the combination must be refused, not started and then hung");

    let rendered = err.to_string();
    assert!(
        rendered.contains("keep_alive") && rendered.contains("result_contract"),
        "the surfaced error must name both flags so a caller knows which to \
         change, got: {rendered}"
    );
}
