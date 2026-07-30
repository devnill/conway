//! Board item C3 (conway FR probe): does a PROMPTED agent whose tool set is
//! restricted to `report` alone -- the shape an embedder (Kepler) needs for
//! a pure-synthesis "proposer" (natural-language in, schema-validated
//! structured proposal out, no filesystem access) -- run cleanly end to end
//! against conway's existing `AgentLoop`/`ToolSelector`/`result_contract`
//! machinery?
//!
//! This file is Stage 1's "real integration probe": a session whose agent
//! has `tools: Some(ToolSelector::Only(["report"]))`, a system prompt (this
//! is NOT the old zero-tool, unprompted "inert host" shape --
//! `conway::intent`'s classifier -- the proposer IS prompted), and a
//! programmatic `result_contract`. Each test below answers one of the
//! item's (a)-(e) questions; its doc comment names which.
//!
//! Harness style follows `tests/result_contract.rs` (hand-assembled
//! `AgentLoop` + `ScriptedBackend`) rather than `tests/agent_loop_e2e.rs`'s
//! `TrackingBackend` -- `ScriptedBackend::calls()` already exposes the
//! `GenerateRequest.tools` this file's announcement assertions (b) need, with
//! no extra double.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use conway_core::agent::{Budget, PermissionDecision, ResultStatus, ToolSelector};
use conway_core::capabilities::{
    CacheMode, Capabilities, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{
    ContentBlock, PermissionClass, StopReason, ToolCall, ToolCategory, ToolSpec, Usage,
};
use conway_core::fakes::{
    FakeGate, FakeRouter, FakeStore, FakeSubagentHost, ScriptedBackend, ScriptedTurn,
};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, SessionId, ToolName};
use conway_core::log::{LogRecord, SessionMeta, SessionStatus};
use conway_core::ports::{
    Backend, GenerateResponse, HealthRegistry, PermissionGate, Plugin, PluginConfig,
    PluginManifest, Router, SessionStore, Tool, ToolCtx, ToolOutput,
};
use conway_core::provenance::Provenance;
use conway_runtime::agent_loop::{AgentLoop, AgentSpec, LoopDeps};
use conway_runtime::attempt::AttemptEngine;
use conway_runtime::context::{ContextBuilder, SystemPromptSpec};
use conway_runtime::events::EventBus;
use conway_runtime::permission::PermissionBroker;
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

fn report_call(call_id: &str, structured: serde_json::Value) -> GenerateResponse {
    tool_call_response(
        call_id,
        "report",
        serde_json::json!({"summary": "a proposal", "structured": structured}),
    )
}

fn schema_any_object() -> schemars::schema::RootSchema {
    serde_json::from_value(serde_json::json!({"type": "object"})).unwrap()
}

fn schema_requiring(prop: &str) -> schemars::schema::RootSchema {
    serde_json::from_value(serde_json::json!({
        "type": "object",
        "properties": { prop: { "type": "boolean" } },
        "required": [prop],
    }))
    .unwrap()
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

/// A local structural stand-in for `conway-tools`' `ReportTool` (WI-065),
/// mirroring `tests/result_contract.rs`'s own `FakeReportTool` -- emits the
/// same `{"conway_report": {...}}` envelope shape. Not imported from
/// `conway-tools`: `conway-runtime` must not depend on that crate.
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
// Harness
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
            role: Some(RoleAlias::new("proposer")),
            created: Utc::now(),
            cwd: PathBuf::from("/tmp"),
            labels: vec![],
            status: SessionStatus::Active,
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

/// Builds a `report`-only PROPOSER agent's loop: `tools` fixes the plugin
/// registry (what could ever be dispatched, regardless of announcement --
/// see `PluginRegistry::specs`'s own doc on the announcement/execution
/// split); `selector` fixes what `AgentSpec::tools` announces to the model
/// this turn. A real system prompt (`SystemPromptSpec`) is always set --
/// unlike `conway::intent`'s zero-tool, unprompted classifier, the
/// PROPOSER shape this item probes is prompted.
#[allow(clippy::too_many_arguments)]
fn build_loop(
    session: SessionId,
    agent: AgentId,
    store: Arc<dyn SessionStore>,
    backend: Arc<dyn Backend>,
    tools: Vec<Arc<dyn Tool>>,
    selector: Option<ToolSelector>,
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
        router: route(),
        attempt,
        registry: plugin_registry,
        tool_runner,
        subagents,
        plugin_config: Arc::new(PluginConfig::default()),
        bus: bus.clone(),
        builder: Arc::new(ContextBuilder::new()),
        headroom: Arc::new(conway_routing::config::HeadroomPolicy::default()),
        tree: tree.clone(),
        context_hook: std::sync::RwLock::new(None),
    });

    let spec = AgentSpec {
        system_prompt: Some(SystemPromptSpec {
            agent_def: "proposer".to_string(),
            text: "You are a PROPOSER agent. You have no filesystem tools -- your only \
                   available action is `report`. Read the user's request and call `report` \
                   exactly once with a structured proposal."
                .to_string(),
        }),
        skills: vec![],
        tools: selector,
        role: RoleAlias::new("proposer"),
        pin: None,
        budget: budget.clone(),
        cache_mode: CacheMode::None,
        cache_ttl: conway_core::segment::CacheTtl::FiveMinutes,
        headroom_override: None,
        max_parallel_tools: 4,
        report_slot: None,
        result_contract,
        keep_alive: false,
    };

    let cancel = CancellationToken::new();
    tree.attach(AgentNode {
        id: agent,
        parent: None,
        session,
        kind: None,
        agent_def: None,
        role: Some(RoleAlias::new("proposer")),
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

fn scripted(script: Vec<ScriptedTurn>) -> Arc<ScriptedBackend> {
    Arc::new(
        ScriptedBackend::new(script)
            .with_id(BackendId::new("scripted"))
            .with_capabilities(caps_ok()),
    )
}

fn announced_names(req: &conway_core::ports::GenerateRequest) -> Vec<String> {
    let mut names: Vec<String> = req
        .tools
        .iter()
        .map(|t| t.name.as_str().to_string())
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------
// (a) + (e): a PROMPTED agent whose ONLY tool is `report` runs and
// terminates correctly -- including through the result-contract corrective
// turn.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_e_report_only_agent_completes_and_corrective_retry_still_works() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "propose a migration plan").await;
    let backend = scripted(vec![
        // Turn 0: the agent's only available action is `report`; it calls
        // it once, but with a `structured` value missing the contract's
        // required `ok` key.
        ScriptedTurn::Respond(report_call("tc_1", serde_json::json!({}))),
        // Turn 1: natural completion (no further tool calls, since `report`
        // is the only tool and it was already called) -- first validation
        // attempt fails -> `Retry`, one more turn.
        ScriptedTurn::Respond(text_response("still working")),
        // Turn 2: the agent calls `report` again with a corrected value --
        // the corrective-turn path still works when `report` is the ONLY
        // announced tool.
        ScriptedTurn::Respond(report_call("tc_2", serde_json::json!({"ok": true}))),
        // Turn 3: natural completion -- now valid -> `Completed`.
        ScriptedTurn::Respond(text_response("done")),
    ]);
    let tool: Arc<dyn Tool> = Arc::new(FakeReportTool);

    let agent_loop = build_loop(
        session,
        agent,
        store.clone(),
        backend.clone(),
        vec![tool],
        Some(ToolSelector::Only(vec!["report".into()])),
        Budget::default(),
        Some(schema_requiring("ok")),
    );
    let result = agent_loop.run().await;

    assert_eq!(result.status, ResultStatus::Completed);
    assert_eq!(result.structured, Some(serde_json::json!({"ok": true})));

    // No infinite no-tool loop: the run terminated in exactly the 4
    // scripted turns, not hung or looping past them.
    assert_eq!(backend.calls().len(), 4);

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
        "exactly one corrective retry must have been spent, same as the full-tool-set case"
    );
}

// ---------------------------------------------------------------------
// (b): with `Only(["report"])`, only `report` is announced to the model.
// Reference variant: `Only([])` announces nothing at all.
// ---------------------------------------------------------------------

#[tokio::test]
async fn b_only_report_announces_exactly_report_even_when_more_tools_are_registered() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "propose a plan").await;
    // The plugin registry ALSO carries a filesystem-shaped tool -- proving
    // the announcement narrowing, not merely an empty registry.
    let backend = scripted(vec![
        ScriptedTurn::Respond(report_call("tc_1", serde_json::json!({"ok": true}))),
        // No `result_contract` is set here, so the `report` call alone does
        // not end the turn -- a second, no-tool-call turn is still needed
        // for natural completion (same two-turn shape as (a)/(e)).
        ScriptedTurn::Respond(text_response("done")),
    ]);
    let report_tool: Arc<dyn Tool> = Arc::new(FakeReportTool);
    struct StubReadTool;
    #[async_trait]
    impl Tool for StubReadTool {
        fn spec(&self) -> ToolSpec {
            tool_spec("read")
        }
        async fn invoke(
            &self,
            _call: ToolCall,
            _ctx: ToolCtx,
        ) -> Result<ToolOutput, conway_core::error::ToolError> {
            unreachable!("read must never be announced to a report-only agent")
        }
    }
    let read_tool: Arc<dyn Tool> = Arc::new(StubReadTool);

    let agent_loop = build_loop(
        session,
        agent,
        store,
        backend.clone(),
        vec![report_tool, read_tool],
        Some(ToolSelector::Only(vec!["report".into()])),
        Budget::default(),
        None,
    );
    let _ = agent_loop.run().await;

    let calls = backend.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(announced_names(&calls[0]), vec!["report".to_string()]);
    assert_eq!(announced_names(&calls[1]), vec!["report".to_string()]);
}

/// Reference shape: `Only([])` (the old zero-tool "inert host" selector,
/// e.g. `conway::intent`'s classifier) announces NOTHING, and the agent
/// still completes via trailing text -- a report-only agent's `Only(
/// ["report"])` is a strict superset of an already-supported shape, not a
/// novel one.
#[tokio::test]
async fn b_ref_only_empty_announces_nothing_and_agent_completes_via_trailing_text() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "classify this").await;
    let backend = scripted(vec![ScriptedTurn::Respond(text_response(
        "plain answer, no tools available",
    ))]);
    let report_tool: Arc<dyn Tool> = Arc::new(FakeReportTool);

    let agent_loop = build_loop(
        session,
        agent,
        store,
        backend.clone(),
        vec![report_tool],
        Some(ToolSelector::Only(vec![])),
        Budget::default(),
        None,
    );
    let result = agent_loop.run().await;

    assert_eq!(result.status, ResultStatus::Completed);
    assert_eq!(result.summary, "plain answer, no tools available");
    let calls = backend.calls();
    assert_eq!(calls.len(), 1);
    assert!(announced_names(&calls[0]).is_empty());
}

// ---------------------------------------------------------------------
// (c): a hallucinated call to a tool that does not exist in the report-only
// agent's own registry (e.g. `read`) gets clean model-visible feedback and
// the agent recovers on its next turn -- not a hang, a panic, or a real
// filesystem dispatch.
// ---------------------------------------------------------------------

#[tokio::test]
async fn c_hallucinated_nonexistent_tool_call_is_clean_feedback_then_recovery() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "propose a plan").await;
    let backend = scripted(vec![
        // Turn 0: the model hallucinates a call to `read` -- a tool that
        // does not exist anywhere in this agent's plugin registry (the
        // proposer's registry, matching its real deployment shape, has no
        // filesystem tool installed at all).
        ScriptedTurn::Respond(tool_call_response("tc_1", "read", serde_json::json!({}))),
        // Turn 1: having received clean error feedback (not a crash, not a
        // real filesystem dispatch), the agent recovers and calls the one
        // tool that actually exists.
        ScriptedTurn::Respond(report_call("tc_2", serde_json::json!({"ok": true}))),
        // Turn 2: natural completion.
        ScriptedTurn::Respond(text_response("done")),
    ]);
    let tool: Arc<dyn Tool> = Arc::new(FakeReportTool);

    let agent_loop = build_loop(
        session,
        agent,
        store.clone(),
        backend.clone(),
        vec![tool],
        Some(ToolSelector::Only(vec!["report".into()])),
        Budget::default(),
        None,
    );
    let result = agent_loop.run().await;

    assert_eq!(result.status, ResultStatus::Completed);
    assert_eq!(result.structured, Some(serde_json::json!({"ok": true})));
    assert_eq!(backend.calls().len(), 3, "the loop must recover, not hang");

    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    let read_result = records.iter().find_map(|r| match r {
        LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == "read" => {
            Some(result)
        }
        _ => None,
    });
    let read_result = read_result.expect("the hallucinated call must still be logged");
    assert!(
        read_result.is_error,
        "unknown tool must be a clean error, not silently ignored"
    );
    let text = match &read_result.blocks[0] {
        ContentBlock::Text { text } => text.clone(),
        other => panic!("expected a text block, got {other:?}"),
    };
    assert!(
        text.contains("read"),
        "error feedback must name the offending tool: {text}"
    );
}

// ---------------------------------------------------------------------
// (d): budgets/deadlines with a small `max_steps` interact sanely with the
// report-only shape -- comfortable headroom completes normally; too little
// headroom fails cleanly (`BudgetExceeded`), never hangs or panics.
// ---------------------------------------------------------------------

#[tokio::test]
async fn d_small_max_steps_with_comfortable_headroom_completes_normally() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "propose a plan").await;
    // report + natural completion is 2 turns -- well within max_steps: 4.
    let backend = scripted(vec![
        ScriptedTurn::Respond(report_call("tc_1", serde_json::json!({"ok": true}))),
        ScriptedTurn::Respond(text_response("done")),
    ]);
    let tool: Arc<dyn Tool> = Arc::new(FakeReportTool);

    let budget = Budget {
        max_steps: 4,
        ..Budget::default()
    };
    let agent_loop = build_loop(
        session,
        agent,
        store,
        backend.clone(),
        vec![tool],
        Some(ToolSelector::Only(vec!["report".into()])),
        budget,
        None,
    );
    let result = agent_loop.run().await;

    assert_eq!(result.status, ResultStatus::Completed);
    assert_eq!(backend.calls().len(), 2);
}

#[tokio::test]
async fn d_too_small_max_steps_fails_cleanly_not_a_hang() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "propose a plan").await;
    // Same 2-turn need as the test above, but max_steps: 1 -- one turn
    // short.
    let backend = scripted(vec![
        ScriptedTurn::Respond(report_call("tc_1", serde_json::json!({"ok": true}))),
        ScriptedTurn::Respond(text_response("done")),
    ]);
    let tool: Arc<dyn Tool> = Arc::new(FakeReportTool);

    let budget = Budget {
        max_steps: 1,
        ..Budget::default()
    };
    let agent_loop = build_loop(
        session,
        agent,
        store,
        backend.clone(),
        vec![tool],
        Some(ToolSelector::Only(vec!["report".into()])),
        budget,
        None,
    );
    let result = agent_loop.run().await;

    assert!(matches!(result.status, ResultStatus::BudgetExceeded { .. }));
    assert_eq!(
        backend.calls().len(),
        1,
        "the budget check must trip BEFORE a second backend call, not after"
    );
}

// ---------------------------------------------------------------------
// Cross-check via `SubagentHost::start`/`Runtime` (not the hand-assembled
// `AgentLoop`): the exact production shape Kepler would use -- a `Spawn`
// child whose `agent_def` supplies the PROPOSER's system prompt (proving
// this is prompted, unlike `conway::intent`'s zero-tool, unprompted
// classifier) and whose `SubagentSpec::tools` restricts it to `report`
// alone -- runs cleanly end to end through the real facade machinery
// (`AgentDef` resolution, `SubagentHost::start`, `await_result`), not just
// the lower-level hand-assembled harness above.
// ---------------------------------------------------------------------

fn proposer_def() -> conway_core::config::AgentDef {
    conway_core::config::AgentDef {
        name: "proposer".to_string(),
        description: None,
        system_prompt: "You are a PROPOSER agent. You have no filesystem tools -- your only \
                        available action is `report`."
            .to_string(),
        role: None,
        model: None,
        tools: ToolSelector::All,
        skills: Vec::new(),
        max_steps: None,
        result_contract: None,
    }
}

fn runtime_with_plugins(
    backend: Arc<dyn Backend>,
    plugins: Vec<Arc<dyn Plugin>>,
    agent_defs: std::collections::HashMap<String, conway_core::config::AgentDef>,
) -> (Arc<Runtime>, Arc<dyn SessionStore>) {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let mut backends: std::collections::HashMap<BackendId, Arc<dyn Backend>> =
        std::collections::HashMap::new();
    backends.insert(backend.id(), backend);
    let runtime = Runtime::new(RuntimeDeps {
        store: store.clone(),
        router: route(),
        health: Arc::new(conway_core::fakes::FakeHealth::new()),
        backends,
        plugins,
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs,
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(conway_routing::config::HeadroomPolicy::default()),
    });
    (runtime, store)
}

fn root_spec_no_tools(prompt: &str) -> RootSpec {
    RootSpec {
        session: None,
        agent_def: None,
        role: Some(RoleAlias::new("root")),
        tools: Some(ToolSelector::Only(vec![])),
        budget: Budget::default(),
        cwd: PathBuf::from("/tmp"),
        prompt: Some(prompt.to_string()),
        keep_alive: false,
        model: None,
    }
}

async fn wait_for_agent_finished(
    stream: &mut conway_runtime::events::EventStream,
    agent: AgentId,
) -> conway_core::agent::AgentResult {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let envelope = futures::StreamExt::next(stream)
                .await
                .expect("event stream ended early");
            if envelope.agent == agent {
                if let conway_core::event::Event::AgentFinished { result, .. } = envelope.event {
                    return result;
                }
            }
        }
    })
    .await
    .expect("agent never finished")
}

#[tokio::test]
async fn report_only_proposer_spawned_via_subagent_host_runs_end_to_end() {
    let backend = scripted(vec![
        // The root's own one-shot turn (no tools -- `Only([])`).
        ScriptedTurn::Respond(text_response("ok, spawning a proposer")),
        // The spawned proposer's turns: `report`, then natural completion.
        ScriptedTurn::Respond(report_call("tc_1", serde_json::json!({"ok": true}))),
        ScriptedTurn::Respond(text_response("done")),
    ]);
    let tool: Arc<dyn Tool> = Arc::new(FakeReportTool);
    let mut defs = std::collections::HashMap::new();
    defs.insert("proposer".to_string(), proposer_def());
    let (runtime, _store) = runtime_with_plugins(
        backend.clone(),
        vec![Arc::new(FakePlugin { tools: vec![tool] })],
        defs,
    );

    let mut stream = runtime.subscribe();
    let root = runtime
        .start_root(root_spec_no_tools("hello"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    let mut spec = conway_core::agent::SubagentSpec::spawn(
        "propose a migration plan",
        conway_core::agent::AgentDefRef("proposer".to_string()),
        Budget::default(),
    );
    spec.tools = Some(ToolSelector::Only(vec!["report".into()]));
    spec.result_contract = Some(schema_requiring("ok"));

    let child = conway_core::ports::SubagentHost::start(&*runtime, root, spec)
        .await
        .unwrap();
    let result = wait_for_agent_finished(&mut stream, child).await;

    assert_eq!(result.status, ResultStatus::Completed);
    assert_eq!(result.structured, Some(serde_json::json!({"ok": true})));

    // The child's turns (calls 1 and 2; call 0 is the root's) must announce
    // only `report`.
    let calls = backend.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(announced_names(&calls[1]), vec!["report".to_string()]);
    assert_eq!(announced_names(&calls[2]), vec!["report".to_string()]);
}
