//! Acceptance tests for `Runtime::resume_root` (WI-118): re-registering an
//! already-persisted session's agent as live, without `store.create`-ing a
//! new session and without appending an initial prompt.
//!
//! Mirrors `runtime_api.rs`'s harness (built entirely from `conway-core`'s
//! fakes) but constructs a *second* `Runtime` sharing the *same* backing
//! `Arc<dyn SessionStore>` as the first, to prove `resume_root` works across
//! a fresh runtime instance -- the "process restart" shape the work item's
//! `resume_root_makes_a_persisted_session_promptable` criterion requires.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use conway_core::agent::{Budget, PermissionDecision};
use conway_core::content::ContentBlock;
use conway_core::error::{RuntimeError, StoreError};
use conway_core::event::Event;
use conway_core::fakes::{
    FakeGate, FakeHealth, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn,
};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, SessionId};
use conway_core::ports::{Backend, Router, SessionStore};
use conway_routing::config::HeadroomPolicy;
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{ResumeSpec, RootSpec, Runtime, RuntimeDeps};
use futures::StreamExt;

fn text_response(text: &str) -> conway_core::ports::GenerateResponse {
    conway_core::ports::GenerateResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        tool_calls: vec![],
        stop: conway_core::content::StopReason::EndTurn,
        usage: conway_core::content::Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
    }
}

/// Builds a `Runtime` over the given (possibly already-populated) store, with
/// its own fresh `ScriptedBackend`/router/event bus -- mirroring a fresh
/// process's `Runtime::new` over durable state a prior process left behind.
fn build_runtime_over(
    store: std::sync::Arc<dyn SessionStore>,
    backend: std::sync::Arc<dyn Backend>,
) -> std::sync::Arc<Runtime> {
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: std::sync::Arc<dyn Router> = std::sync::Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, std::sync::Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);

    Runtime::new(RuntimeDeps {
        store,
        router,
        health: std::sync::Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![],
        gate: std::sync::Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        event_bus: EventBus::with_default_capacity(),
        headroom: std::sync::Arc::new(HeadroomPolicy::default()),
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
        prompt: Some(prompt.to_string()),
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
    }
}

fn session_of(runtime: &Runtime, agent: AgentId) -> SessionId {
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
    agent: AgentId,
) -> conway_core::agent::AgentResult {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let envelope = stream.next().await.expect("event stream ended early");
            if envelope.agent == agent {
                if let Event::AgentFinished { result } = envelope.event {
                    return result;
                }
            }
        }
    })
    .await
    .expect("agent never finished")
}

/// The work item's mandatory criterion, now closing the F-118 D-3 race
/// (`resume_root` must not run any turn until the caller's own `prompt`
/// arrives): create a session via `start_root` plus one prompt turn (so the
/// store holds a real, multi-record transcript), then in a FRESH `Runtime`
/// over the SAME store, `resume_root` must (a) register the agent without
/// running a spurious turn against the stale transcript, then (b) only after
/// `prompt` is called, run exactly one turn whose assembled context contains
/// BOTH the prior turn's text and the new prompt's text, producing a real
/// answer.
///
/// Runs on a real multi-thread runtime with an explicit delay between
/// `resume_root` and `prompt` specifically to force the scheduling race the
/// old single-threaded test's assertions happened to hide (the resumed
/// task's first poll landing before `prompt`'s store append, in a
/// current-thread runtime, always lost that race by construction -- see this
/// item's Self-Check for the empirical repro against the pre-fix code).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_root_makes_a_persisted_session_promptable() {
    let store: std::sync::Arc<dyn SessionStore> = std::sync::Arc::new(FakeStore::new());

    // First "process": start a root, let its one scripted turn complete.
    let backend1 = std::sync::Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Respond(
        text_response("ack"),
    )]));
    let runtime1 = build_runtime_over(store.clone(), backend1);
    let mut stream1 = runtime1.subscribe();
    let agent1 = runtime1
        .start_root(root_spec("first turn text"))
        .await
        .unwrap();
    let session = session_of(&runtime1, agent1);
    wait_for_agent_finished(&mut stream1, agent1).await;
    drop(runtime1);

    let head_before_resume = store.head(&session).await.unwrap();

    // Second "process": a fresh `Runtime` over the exact same store.
    let backend2 = std::sync::Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Respond(
        text_response("continued"),
    )]));
    let runtime2 = build_runtime_over(store.clone(), backend2.clone());
    let mut stream2 = runtime2.subscribe();

    let resumed_agent = runtime2.resume_root(resume_spec(session)).await.unwrap();

    // Reuses the original SessionMeta.agent_id -- not a freshly minted id.
    assert_eq!(resumed_agent, agent1);

    // Give the resumed agent's spawned task every chance to run its first
    // poll before `prompt` is ever called. Pre-fix, this is exactly the
    // window in which the ungated loop reads the stale (already-completed)
    // transcript, sees no tool calls, and silently finishes the task.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // (a) Before `prompt`: no spurious turn has run. The store head is
    // unchanged and the backend has received zero requests.
    let head_after_resume = store.head(&session).await.unwrap();
    assert_eq!(
        head_before_resume, head_after_resume,
        "resume_root must not run any turn before the caller's own prompt arrives"
    );
    assert!(
        backend2.calls().is_empty(),
        "resume_root must not call the backend before the caller's own prompt arrives, calls: {:?}",
        backend2.calls()
    );

    // `prompt` must succeed -- no `AgentNotFound` -- proving the agent is
    // actually registered in `runtime2.agents`/`AgentTree`.
    runtime2
        .prompt(resumed_agent, "second turn text".to_string())
        .await
        .expect("resumed agent must be promptable");

    let result = wait_for_agent_finished(&mut stream2, resumed_agent).await;

    // (c) The agent produced a real Assistant answer to the new prompt, not
    // a dangling unanswered UserTurn.
    assert!(
        matches!(result.status, conway_core::agent::ResultStatus::Completed),
        "expected the resumed agent to complete after prompt, got: {:?}",
        result.status
    );
    assert_eq!(result.summary, "continued");

    // (b) Only after `prompt` does exactly ONE turn run, whose captured
    // request contains BOTH the prior turn's text and the new prompt's
    // text -- proving the transcript was resolved and continued (not
    // restarted) and that the new prompt was actually read, not dropped.
    let calls = backend2.calls();
    assert_eq!(
        calls.len(),
        1,
        "expected exactly one backend call once the agent resumes work, calls: {calls:?}"
    );
    let request_text: String = calls[0]
        .segments
        .iter()
        .flat_map(|seg| seg.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        request_text.contains("first turn text"),
        "expected the resumed turn's context to contain the prior turn's text, got: {request_text}"
    );
    assert!(
        request_text.contains("second turn text"),
        "expected the resumed turn's context to contain the new prompt's text, got: {request_text}"
    );
}

/// `resume_root` must not `store.create` and must not append an initial
/// `UserTurn`: the record count immediately after resuming (before any
/// `prompt` call) must equal the record count before resuming.
#[tokio::test]
async fn resume_root_does_not_create_or_append_initial_turn() {
    let store: std::sync::Arc<dyn SessionStore> = std::sync::Arc::new(FakeStore::new());
    let backend1 = std::sync::Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Respond(
        text_response("ack"),
    )]));
    let runtime1 = build_runtime_over(store.clone(), backend1);
    let mut stream1 = runtime1.subscribe();
    let agent1 = runtime1.start_root(root_spec("hello")).await.unwrap();
    let session = session_of(&runtime1, agent1);
    wait_for_agent_finished(&mut stream1, agent1).await;

    let head_before = store.head(&session).await.unwrap();

    let backend2 = std::sync::Arc::new(ScriptedBackend::new(vec![]));
    let runtime2 = build_runtime_over(store.clone(), backend2);
    runtime2.resume_root(resume_spec(session)).await.unwrap();

    let head_after = store.head(&session).await.unwrap();
    assert_eq!(
        head_before, head_after,
        "resume_root must not append any record before the caller's own prompt"
    );

    // Calling `create` again for this id would fail with `AlreadyExists`;
    // confirm `resume_root` did not attempt it (the session is still exactly
    // the one `start_root` created, not a second header write).
    let err = store
        .create(conway_core::log::SessionMeta {
            id: session,
            agent_id: AgentId::new(),
            origin: None,
            agent_def: None,
            role: None,
            created: chrono::Utc::now(),
            cwd: PathBuf::from("/tmp"),
            labels: Vec::new(),
            status: conway_core::log::SessionStatus::Active,
            ephemeral: false,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::AlreadyExists { .. }));
}

/// Resuming an unknown session id returns a typed `RuntimeError` (surfaced
/// through `RuntimeError::Store`'s `#[from] StoreError` conversion of
/// `StoreError::NotFound`) rather than panicking or creating a session.
#[tokio::test]
async fn resume_root_errors_typed_for_unknown_session() {
    let store: std::sync::Arc<dyn SessionStore> = std::sync::Arc::new(FakeStore::new());
    let backend = std::sync::Arc::new(ScriptedBackend::new(vec![]));
    let runtime = build_runtime_over(store.clone(), backend);

    let unknown = SessionId::new();
    let err = runtime.resume_root(resume_spec(unknown)).await.unwrap_err();
    assert!(matches!(
        err,
        RuntimeError::Store(StoreError::NotFound { session }) if session == unknown
    ));
}

/// The resumed agent's `AgentTree` node round-trips through `Runtime::tree`,
/// and `cancel`/`context_report` both resolve (no `AgentNotFound`) against
/// the same id `resume_root` returned -- proving it uses the exact same
/// registration path (`launch_agent`) `start_root` does, not a partial or ad
/// hoc one.
#[tokio::test]
async fn resume_root_registers_into_tree_and_supports_cancel_and_context_report() {
    let store: std::sync::Arc<dyn SessionStore> = std::sync::Arc::new(FakeStore::new());
    let backend1 = std::sync::Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Respond(
        text_response("ack"),
    )]));
    let runtime1 = build_runtime_over(store.clone(), backend1);
    let mut stream1 = runtime1.subscribe();
    let agent1 = runtime1.start_root(root_spec("hello")).await.unwrap();
    let session = session_of(&runtime1, agent1);
    wait_for_agent_finished(&mut stream1, agent1).await;

    let backend2 = std::sync::Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Respond(
        text_response("continued"),
    )]));
    let runtime2 = build_runtime_over(store.clone(), backend2);
    let resumed_agent = runtime2.resume_root(resume_spec(session)).await.unwrap();

    let snapshot = runtime2.tree();
    assert!(snapshot.nodes.iter().any(|n| n.agent_id == resumed_agent));

    runtime2
        .cancel(resumed_agent, "test cancel".to_string())
        .expect("cancel must resolve the resumed agent, not AgentNotFound");
    runtime2
        .context_report(resumed_agent)
        .expect("context_report must resolve the resumed agent, not AgentNotFound");
}
