//! Proves `Runtime::set_context_hook`'s own claim (board item
//! `01M00RGARPESWXYAVY960KDE7S`, round 2, `INTENT.md` §8.6): a bare,
//! un-self-checking `ContextHook` -- exactly what every real implementation
//! looks like, since coherence-checking was never part of the trait's
//! contract -- is wrapped at the ONE place it enters the runtime, so it
//! cannot ship a request that orphans a tool call/result pair, without
//! either `AgentLoop` call site (`before_request`/`route_and_attempt`'s
//! `on_overflow`) doing anything to remember the check.
//!
//! Mirrors `resume_root.rs`'s harness exactly: a second `Runtime` sharing
//! the first's backing store, so a session can carry a persisted,
//! already-answered tool call/result pair before the hook (registered only
//! on the second `Runtime`) ever gets a chance to see it -- proving the
//! wrap is a property of `set_context_hook` itself, not of anything
//! `crates/conway-runtime/tests/agent_loop_e2e.rs`'s own harness happens to
//! do.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use conway_core::agent::{Budget, PermissionDecision, ResultStatus};
use conway_core::capabilities::HeadroomPolicy;
use conway_core::content::{ContentBlock, StopReason, ToolResult, Usage};
use conway_core::event::Event;
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias, SessionId, ToolName};
use conway_core::log::LogRecord;
use conway_core::ports::{
    Backend, ContextHook, ContextHookCtx, ContextPayload, OverflowInfo, Router, SessionStore,
};
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{ResumeSpec, RootSpec, Runtime, RuntimeDeps};
use conway_testkit::{FakeGate, FakeHealth, FakeRouter, FakeStore, ScriptedBackend};
use futures::StreamExt;

/// An ordinary, un-self-checking `ContextHook` -- see this file's own doc.
/// Strips every `ToolResultBlock` segment it is handed, unconditionally.
struct DropsEveryResult;

fn strip_results(payload: ContextPayload) -> ContextPayload {
    ContextPayload {
        segments: payload
            .segments
            .into_iter()
            .filter(|s| {
                !s.content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolResultBlock { .. }))
            })
            .collect(),
        tools: payload.tools,
    }
}

#[async_trait]
impl ContextHook for DropsEveryResult {
    async fn before_request(
        &self,
        _ctx: &ContextHookCtx,
        payload: ContextPayload,
    ) -> ContextPayload {
        strip_results(payload)
    }

    async fn on_overflow(
        &self,
        _ctx: &ContextHookCtx,
        payload: ContextPayload,
        _overflow: OverflowInfo,
    ) -> Option<ContextPayload> {
        Some(strip_results(payload))
    }
}

fn build_runtime_over(store: Arc<dyn SessionStore>, backend: Arc<dyn Backend>) -> Arc<Runtime> {
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);

    Runtime::new(RuntimeDeps {
        store,
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        skills: Default::default(),
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),
    })
}

fn root_spec(prompt: &str) -> RootSpec {
    RootSpec {
        session: None,
        agent_def: None,
        role: Some(RoleAlias::new("planner")),
        tools: None,
        budget: Budget::default(),
        cwd: PathBuf::from("/tmp"),
        root: None,
        prompt: Some(prompt.to_string()),
        keep_alive: false,
        model: None,
        system_prompt_override: None,
        result_contract: None,
        labels: Vec::new(),
    }
}

fn resume_spec(session: SessionId) -> ResumeSpec {
    ResumeSpec {
        session,
        agent_def: None,
        role: None,
        tools: None,
        budget: Budget::default(),
        cwd: None,
        result_contract: None,
        keep_alive: false,
    }
}

fn session_of(runtime: &Runtime, agent: conway_core::ids::AgentId) -> SessionId {
    runtime
        .tree()
        .nodes
        .iter()
        .find(|n| n.agent_id == agent)
        .expect("agent present in tree")
        .session
}

async fn wait_for_agent_finished(
    stream: &mut conway_runtime::events::EventStream,
    agent: conway_core::ids::AgentId,
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

/// The end-to-end proof: `set_context_hook` is called with a completely
/// bare `Arc<dyn ContextHook>` (`DropsEveryResult`, defined above, does
/// nothing to check its own output), yet the turn it later breaks is still
/// refused, not silently sent -- because `set_context_hook` -- not either
/// `AgentLoop` call site -- is what wraps it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bare_hook_registered_via_set_context_hook_still_cannot_ship_incoherence() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());

    // First "process": an ordinary completed turn, establishing the
    // session. Its own scripted response is irrelevant text -- the point
    // of this half is just a real session with a real head record to
    // append after.
    let backend1 = Arc::new(ScriptedBackend::new(vec![
        conway_testkit::ScriptedTurn::Respond(conway_core::ports::GenerateResponse {
            content: vec![ContentBlock::Text {
                text: "hi".to_string(),
            }],
            tool_calls: vec![],
            stop: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            },
        }),
    ]));
    let runtime1 = build_runtime_over(store.clone(), backend1);
    let mut stream1 = runtime1.subscribe();
    let agent = runtime1.start_root(root_spec("hello")).await.unwrap();
    let session = session_of(&runtime1, agent);
    wait_for_agent_finished(&mut stream1, agent).await;
    drop(runtime1);

    // Append an already-ANSWERED tool call/result pair directly -- coherent
    // by construction, exactly the shape a real prior turn would have left.
    // Whatever incoherence a test later observes is provably the hook's
    // doing, not a pairing this harness introduced.
    let seq = store.head(&session).await.unwrap();
    store
        .append(
            &session,
            LogRecord::Assistant {
                seq,
                ts: Utc::now(),
                content: vec![ContentBlock::ToolUse {
                    call_id: "call_missing_result".into(),
                    name: ToolName::new("read"),
                    arguments: serde_json::json!({"path": "a.txt"}),
                }],
                model: ModelRef {
                    backend: BackendId::new("b"),
                    model: ModelId::new("m"),
                },
                route_reason: serde_json::json!({}),
                usage: Usage::default(),
                stop: StopReason::ToolUse,
            },
        )
        .await
        .unwrap();
    let seq = store.head(&session).await.unwrap();
    store
        .append(
            &session,
            LogRecord::ToolResultRecord {
                seq,
                ts: Utc::now(),
                result: ToolResult {
                    call_id: "call_missing_result".into(),
                    tool: ToolName::new("read"),
                    blocks: vec![ContentBlock::Text {
                        text: "file contents".into(),
                    }],
                    is_error: false,
                    truncated: None,
                },
            },
        )
        .await
        .unwrap();

    // Second "process": a fresh `Runtime`, same store. Zero scripted
    // responses -- if the backend is EVER called, `ScriptedBackend` has
    // nothing to hand back and the turn would fail for the wrong reason,
    // so an empty script is itself a second, independent check that this
    // never reaches routing.
    let backend2 = Arc::new(ScriptedBackend::new(vec![]));
    let runtime2 = build_runtime_over(store.clone(), backend2.clone());
    let mut stream2 = runtime2.subscribe();

    // The seam under test: a completely ordinary hook, registered through
    // the one production entry point. Nothing here constructs a
    // `GuardedContextHook` -- that is `set_context_hook`'s job alone.
    runtime2.set_context_hook(Some(Arc::new(DropsEveryResult) as Arc<dyn ContextHook>));

    let resumed_agent = runtime2.resume_root(resume_spec(session)).await.unwrap();
    runtime2
        .prompt(resumed_agent, "next".to_string())
        .await
        .expect("resumed agent must be promptable");

    let result = wait_for_agent_finished(&mut stream2, resumed_agent).await;

    match &result.status {
        ResultStatus::Failed { error } => {
            assert!(
                error.contains("call_missing_result"),
                "the orphaned call_id must be named in the failure: {error}"
            );
            assert!(
                error.contains("before_request"),
                "the responsible hook method must be named in the failure: {error}"
            );
        }
        other => panic!("expected ResultStatus::Failed naming the orphan, got {other:?}"),
    }
    assert!(
        backend2.calls().is_empty(),
        "an incoherent request must never reach routing/the backend at all"
    );

    // Belt and suspenders: the persisted log gained no NEW tool-related
    // records from this refused turn (only whatever `resume_root`/`prompt`
    // themselves append, e.g. the new `UserTurn`) -- refusing a turn must
    // not fabricate a repaired transcript nobody asked for.
    let records = store
        .read(&session, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    let tool_result_records = records
        .iter()
        .filter(|r| matches!(r, LogRecord::ToolResultRecord { .. }))
        .count();
    assert_eq!(
        tool_result_records, 1,
        "exactly the one pre-seeded tool result -- refusal appends nothing"
    );
}
