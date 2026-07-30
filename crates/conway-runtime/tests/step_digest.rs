//! Integration tests for `StepDigest`'s wiring into `AgentLoop`'s turn loop
//! (WI-086): repeated identical tool calls emit `Event::RepeatedStep` and an
//! injected `SystemNote` citing the first occurrence's `seq`. The pure
//! digest algorithm itself (blake3 keying, argument canonicalization,
//! notice-once-per-digest, the bounded 64-entry LRU ring over 10 000 calls)
//! is unit-tested directly in `src/step_digest.rs`; this file only proves
//! the loop actually drives that type at the right point in the turn
//! machine and persists/emits what the spec requires.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use conway_core::agent::{Budget, ResultStatus, ToolSelector};
use conway_core::capabilities::{
    CacheMode, Capabilities, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{
    ContentBlock, PermissionClass, StopReason, ToolCall, ToolCategory, ToolSpec, Usage,
};
use conway_core::event::Event;
use conway_core::fakes::{FakeGate, FakeStore, FakeSubagentHost, ScriptedBackend, ScriptedTurn};
use conway_core::ids::{AgentId, BackendId, LogSeq, ModelId, RoleAlias, SessionId, ToolName};
use conway_core::log::{LogRecord, SessionMeta, SessionStatus};
use conway_core::ports::{
    Backend, HealthRegistry, PermissionGate, Plugin, PluginConfig, PluginManifest, Router,
    SessionStore, Tool, ToolCtx, ToolOutput,
};
use conway_core::provenance::Provenance;
use conway_core::routing::{Route, RouteRequest, RoutingReason};
use conway_core::segment::CacheTtl;
use conway_routing::config::HeadroomPolicy;
use conway_runtime::agent_loop::{AgentLoop, AgentSpec, LoopDeps};
use conway_runtime::attempt::AttemptEngine;
use conway_runtime::context::ContextBuilder;
use conway_runtime::events::EventBus;
use conway_runtime::permission::PermissionBroker;
use conway_runtime::tools::PluginRegistry;
use conway_runtime::tree::{AgentNode, AgentTree};
use futures::future::FutureExt;
use futures::stream::StreamExt;
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

fn text_response(text: &str) -> conway_core::ports::GenerateResponse {
    conway_core::ports::GenerateResponse {
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

fn tool_call_response(
    call_id: &str,
    tool: &str,
    args: serde_json::Value,
) -> conway_core::ports::GenerateResponse {
    conway_core::ports::GenerateResponse {
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
        category: ToolCategory::Read,
        permission: PermissionClass::Safe,
    }
}

/// Always returns the same fixed text, ignoring its arguments -- so calling
/// it repeatedly with identical arguments is exactly the loop the digest is
/// meant to notice.
struct FixedTool {
    name: ToolName,
}

#[async_trait]
impl Tool for FixedTool {
    fn spec(&self) -> ToolSpec {
        tool_spec(self.name.as_str())
    }

    async fn invoke(
        &self,
        _call: ToolCall,
        _ctx: ToolCtx,
    ) -> Result<ToolOutput, conway_core::error::ToolError> {
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "ok".to_string(),
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
// Harness (trimmed from `tests/agent_loop_e2e.rs`'s own -- no headroom
// override, no report slot: neither is exercised by this file)
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

struct Harness {
    agent_loop: AgentLoop,
    bus: Arc<EventBus>,
}

fn build_loop(
    session: SessionId,
    agent: AgentId,
    store: Arc<dyn SessionStore>,
    backend: Arc<dyn Backend>,
    tools: Vec<Arc<dyn Tool>>,
) -> Harness {
    let bus = EventBus::new(1024);
    let health: Arc<dyn HealthRegistry> = Arc::new(conway_core::fakes::FakeHealth::new());
    let mut backends: std::collections::HashMap<BackendId, Arc<dyn Backend>> =
        std::collections::HashMap::new();
    backends.insert(backend.id(), backend);
    let attempt = Arc::new(AttemptEngine::new(backends, health, bus.clone()));
    let plugin_registry =
        Arc::new(PluginRegistry::from_plugins(vec![Arc::new(FakePlugin { tools })]).unwrap());
    let gate: Arc<dyn PermissionGate> = Arc::new(FakeGate::new(
        conway_core::agent::PermissionDecision::AllowOnce,
    ));
    let broker = Arc::new(PermissionBroker::new(gate, bus.clone()));
    let tool_runner = Arc::new(conway_runtime::tools::ToolRunner::new(
        plugin_registry.clone(),
        broker,
        bus.clone(),
    ));
    let subagents: Arc<dyn conway_core::ports::SubagentHost> =
        Arc::new(FakeSubagentHost::new(agent));
    let tree = Arc::new(AgentTree::new(bus.clone()));

    struct SingleRoute(Route);
    impl Router for SingleRoute {
        fn resolve(
            &self,
            _req: &RouteRequest,
        ) -> Result<Vec<Route>, conway_core::error::RoutingError> {
            Ok(vec![self.0.clone()])
        }
    }
    let router: Arc<dyn Router> = Arc::new(SingleRoute(Route {
        backend: BackendId::new("scripted"),
        model: ModelId::new("m"),
        params: Default::default(),
        reason: RoutingReason::AliasPrimary {
            alias: RoleAlias::new("planner"),
        },
    }));

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
        budget: Budget::default(),
        cache_mode: CacheMode::None,
        cache_ttl: CacheTtl::FiveMinutes,
        headroom_override: None,
        max_parallel_tools: 4,
        report_slot: None,
        result_contract: None,
        keep_alive: false,
    };

    let cancel = CancellationToken::new();
    tree.attach(AgentNode {
        id: agent,
        parent: None,
        session,
        kind: None,
        agent_def: None,
        role: Some(RoleAlias::new("planner")),
        budget: Budget::default(),
        cancel: cancel.clone(),
        inherited_upto: None,
        ephemeral: false,
    })
    .expect("fresh tree attach never fails");
    let (_mailbox_tx, mailbox_rx) =
        conway_runtime::mailbox::Mailbox::new(conway_runtime::mailbox::RUNTIME_CAPACITY);
    let agent_loop = AgentLoop {
        agent_id: agent,
        session,
        parent: None,
        agent_path: vec![agent],
        cwd: PathBuf::from("/tmp"),
        deps,
        spec,
        cancel: cancel.clone(),
        inherited: None,
        inbox: mailbox_rx,
        parent_mailbox: None,
        pending_cancel: None,
        resume_gate: Default::default(),
    };

    Harness { agent_loop, bus }
}

fn drain(stream: &mut conway_runtime::events::EventStream) -> Vec<Event> {
    let mut out = Vec::new();
    while let Some(Some(envelope)) = stream.next().now_or_never() {
        out.push(envelope.event);
    }
    out
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[tokio::test]
async fn third_identical_call_emits_repeated_step_and_a_citing_system_note() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    let args = serde_json::json!({"path": "a.txt"});
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response("tc_1", "read", args.clone())),
            ScriptedTurn::Respond(tool_call_response("tc_2", "read", args.clone())),
            ScriptedTurn::Respond(tool_call_response("tc_3", "read", args.clone())),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("scripted"))
        .with_capabilities(caps_ok()),
    );
    let tool: Arc<dyn Tool> = Arc::new(FixedTool {
        name: ToolName::new("read"),
    });

    let harness = build_loop(session, agent, store.clone(), backend, vec![tool]);
    let mut stream = harness.bus.subscribe();

    let result = harness.agent_loop.run().await;
    assert_eq!(result.status, ResultStatus::Completed);

    let events = drain(&mut stream);
    let repeated: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::RepeatedStep { tool, prior_seq } => Some((tool.clone(), *prior_seq)),
            _ => None,
        })
        .collect();
    assert_eq!(
        repeated.len(),
        1,
        "exactly one RepeatedStep must fire, on the 3rd identical call: {events:?}"
    );
    assert_eq!(repeated[0].0, ToolName::new("read"));

    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    let tool_result_seqs: Vec<LogSeq> = records
        .iter()
        .filter_map(|r| match r {
            LogRecord::ToolResultRecord { seq, result, .. } if result.tool.as_str() == "read" => {
                Some(*seq)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        tool_result_seqs.len(),
        3,
        "three `read` calls must have run"
    );
    assert_eq!(
        repeated[0].1, tool_result_seqs[0],
        "the notice must cite the FIRST occurrence's seq, not the 3rd's"
    );

    let note = records
        .iter()
        .find_map(|r| match r {
            LogRecord::SystemNote {
                reason, text, prov, ..
            } if reason == "repeated_step" => Some((text.clone(), prov.clone())),
            _ => None,
        })
        .expect("a repeated_step SystemNote must be appended");
    assert!(note.0.contains(&tool_result_seqs[0].to_string()));
    assert_eq!(
        note.1,
        Provenance::SystemNote {
            reason: "repeated_step".to_string()
        }
    );
}

#[tokio::test]
async fn fourth_and_fifth_identical_calls_do_not_emit_a_second_notice() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    let args = serde_json::json!({"path": "a.txt"});
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response("tc_1", "read", args.clone())),
            ScriptedTurn::Respond(tool_call_response("tc_2", "read", args.clone())),
            ScriptedTurn::Respond(tool_call_response("tc_3", "read", args.clone())),
            ScriptedTurn::Respond(tool_call_response("tc_4", "read", args.clone())),
            ScriptedTurn::Respond(tool_call_response("tc_5", "read", args.clone())),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("scripted"))
        .with_capabilities(caps_ok()),
    );
    let tool: Arc<dyn Tool> = Arc::new(FixedTool {
        name: ToolName::new("read"),
    });

    let harness = build_loop(session, agent, store, backend, vec![tool]);
    let mut stream = harness.bus.subscribe();

    let result = harness.agent_loop.run().await;
    assert_eq!(result.status, ResultStatus::Completed);

    let repeated_count = drain(&mut stream)
        .iter()
        .filter(|e| matches!(e, Event::RepeatedStep { .. }))
        .count();
    assert_eq!(
        repeated_count, 1,
        "the 4th and 5th identical calls must not notice again"
    );
}

#[tokio::test]
async fn distinct_arguments_are_not_conflated_into_the_same_digest() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (session, agent) = seed_prompt(&*store, "hello").await;
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response(
                "tc_1",
                "read",
                serde_json::json!({"path": "a.txt"}),
            )),
            ScriptedTurn::Respond(tool_call_response(
                "tc_2",
                "read",
                serde_json::json!({"path": "b.txt"}),
            )),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("scripted"))
        .with_capabilities(caps_ok()),
    );
    let tool: Arc<dyn Tool> = Arc::new(FixedTool {
        name: ToolName::new("read"),
    });

    let harness = build_loop(session, agent, store, backend, vec![tool]);
    let mut stream = harness.bus.subscribe();

    let result = harness.agent_loop.run().await;
    assert_eq!(result.status, ResultStatus::Completed);

    let repeated_count = drain(&mut stream)
        .iter()
        .filter(|e| matches!(e, Event::RepeatedStep { .. }))
        .count();
    assert_eq!(
        repeated_count, 0,
        "two calls with DIFFERENT arguments must never notice, regardless of tool name"
    );
}
