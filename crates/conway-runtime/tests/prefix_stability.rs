//! Proof of conway's whole cache economy premise (architecture §5.3): each
//! `Backend::generate` request is the previous one PLUS new material at the
//! end -- never reordered, never rewritten mid-stream. `prompt_cache_e2e.rs`
//! already proves a single turn breakpoints correctly and that a cache hint
//! is never correctness-bearing; neither file asserts the property this one
//! does -- that request N's own segments survive, byte-for-byte and in
//! order, as a strict prefix of request N+1's. A real session held this
//! across 81 turns before this test existed to catch a regression in it.
//!
//! Two properties, two tests:
//!
//! 1. `three_consecutive_turns_form_a_growing_prefix_with_a_stable_prefix_key`:
//!    three ordinary turns against the SAME root session -- each its own
//!    "process" (a fresh `Runtime` over the SAME `SessionStore`, exactly
//!    `resume_root.rs`'s own established, race-free pattern: a live
//!    `keep_alive` agent's end-of-turn `ResumeGate` re-arm is a DIFFERENT,
//!    unrelated mechanism this file has no need to depend on), each turn
//!    itself a tool call -> tool result -> final text round trip, so both an
//!    intra-turn and a cross-turn boundary are exercised. Asserts every
//!    consecutive pair of the six resulting `Backend::generate` calls forms
//!    an exact leading prefix (`content_identity`, borrowed verbatim from
//!    `prompt_cache_e2e.rs` -- role/content/provenance/tokens_est, everything
//!    that is not agent-derived or cache-hint noise) and that
//!    `crate::context::prefix_key` (the runtime's own cache/dedup key) does
//!    not move.
//! 2. `a_fork_childs_first_request_extends_the_parents_last_request_as_a_wire_byte_prefix`:
//!    the same growing-prefix property, generalized across a FORK boundary.
//!    A fork child's `Provenance` for inherited content is legitimately
//!    different from the parent's own (`Inherited { from, seq_range }`
//!    rather than the parent's own record-kind-derived tag --
//!    `context/path.rs`'s own module doc: "Root / Spawn: own records only",
//!    vs. a fork child's ancestry-walked prefix) -- see
//!    `context/script_hook.rs`'s own `excluding_a_static_segment_does_
//!    change_the_prefix_key` for the precedent that this it is a
//!    LEGITIMATE difference, not a bug. So this test compares only what
//!    actually reaches the wire -- `(Role, Vec<ContentBlock>)`, never
//!    `Provenance` -- via `wire_identity`, a narrower projection than
//!    `content_identity`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use conway_core::agent::{Budget, PermissionDecision, SubagentSpec};
use conway_core::capabilities::HeadroomPolicy;
use conway_core::content::{
    ContentBlock, PermissionClass, Role, StopReason, ToolCall, ToolCategory, ToolSpec, Usage,
};
use conway_core::error::ToolError;
use conway_core::event::Event;
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, SessionId, ToolName};
use conway_core::ports::{
    Backend, GenerateResponse, Plugin, PluginManifest, Router, SessionStore, SubagentHost, Tool,
    ToolCtx, ToolOutput,
};
use conway_core::provenance::Provenance;
use conway_core::segment::PromptSegment;
use conway_runtime::context::prefix_key;
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{ResumeSpec, RootSpec, Runtime, RuntimeDeps};
use conway_testkit::{
    text_response_with_stub_usage as text_response, FakeGate, FakeHealth, FakeRouter, FakeStore,
    ScriptedBackend, ScriptedTurn,
};
use futures::StreamExt;

// ---------------------------------------------------------------------
// A trivial tool, so every scripted turn can genuinely be
// "tool call -> tool result -> text" -- not just text.
// ---------------------------------------------------------------------

const ECHO_TOOL: &str = "echo";

fn echo_tool_spec() -> ToolSpec {
    ToolSpec {
        name: ToolName::new(ECHO_TOOL),
        description: "returns a fixed string".into(),
        schema: serde_json::from_value(serde_json::json!({"type": "object"})).unwrap(),
        category: ToolCategory::Read,
        permission: PermissionClass::Safe,
    }
}

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> ToolSpec {
        echo_tool_spec()
    }

    async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "echoed".into(),
            }],
            is_error: false,
            truncation: conway_core::content::TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

struct EchoPlugin;

impl Plugin for EchoPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "echo".to_string(),
            version: "0.0.0".to_string(),
            tools: vec![echo_tool_spec().name],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(EchoTool)]
    }
}

/// A scripted assistant turn that calls `echo` and ends the turn there
/// (`StopReason::ToolUse`) -- the FIRST half of one "tool call -> result ->
/// text" turn; `text_response` (from `conway_testkit`) is the second half.
fn tool_call_response(call_id: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: ToolName::new(ECHO_TOOL),
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

// ---------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------

fn build_runtime(script: Vec<ScriptedTurn>) -> (Arc<Runtime>, Arc<ScriptedBackend>) {
    build_runtime_over(Arc::new(FakeStore::new()), script)
}

/// [`build_runtime`], over a caller-supplied store -- so a later "process"
/// (a fresh `Runtime`, `resume_root`ing a session the first `Runtime`
/// created) can share the same durable state, mirroring `resume_root.rs`'s
/// own `build_runtime_over`.
fn build_runtime_over(
    store: Arc<dyn SessionStore>,
    script: Vec<ScriptedTurn>,
) -> (Arc<Runtime>, Arc<ScriptedBackend>) {
    let backend = Arc::new(ScriptedBackend::new(script).with_id(BackendId::new("b")));
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend.clone());

    let runtime = Runtime::new(RuntimeDeps {
        store,
        path_store: Arc::new(conway_testkit::FakePathStore::new()),
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![Arc::new(EchoPlugin)],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        instructions: Vec::new(),
        skills: Default::default(),
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),

        session_discovery: Arc::new(conway_testkit::FakeSessionDiscoveryHost::new()),
        capabilities: Arc::new(conway_core::ports::CapabilityRegistry::default()),
    });
    (runtime, backend)
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
        model: None,
        tools: None,
        budget: Budget::default(),
        cwd: None,
        result_contract: None,
        keep_alive: false,
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
                if let Event::AgentFinished { result, .. } = envelope.event {
                    return result;
                }
            }
        }
    })
    .await
    .expect("agent never finished")
}

/// `(role, content, provenance, tokens_est)` for every segment -- everything
/// EXCEPT `id` (agent-derived) and `cache_hint` (economics, never
/// correctness-bearing). Verbatim copy of `prompt_cache_e2e.rs`'s own
/// helper of the same name -- each integration test binary is compiled
/// separately, so there is no shared-crate seam to hang a single copy off
/// of without inventing one.
fn content_identity(
    segments: &[PromptSegment],
) -> Vec<(Role, Vec<ContentBlock>, Provenance, Option<u32>)> {
    segments
        .iter()
        .map(|s| (s.role, s.content.clone(), s.provenance.clone(), s.tokens_est))
        .collect()
}

/// `(role, content)` only -- the projection that actually reaches the wire
/// (`conway-plugin-backends`'s own mapping never serializes `Provenance`).
/// Narrower than `content_identity` on purpose: see the module doc for why
/// a fork boundary legitimately changes `Provenance` for otherwise
/// byte-identical content.
fn wire_identity(segments: &[PromptSegment]) -> Vec<(Role, Vec<ContentBlock>)> {
    segments.iter().map(|s| (s.role, s.content.clone())).collect()
}

/// Asserts `before` is an exact leading prefix of `after` -- same length or
/// shorter, and every element it does have matches `after`'s element at the
/// same index.
fn assert_is_leading_prefix<T: PartialEq + std::fmt::Debug>(before: &[T], after: &[T], ctx: &str) {
    assert!(
        before.len() <= after.len(),
        "{ctx}: the earlier request has MORE segments ({}) than the later one ({}) -- \
         cannot be a prefix",
        before.len(),
        after.len()
    );
    assert_eq!(
        before,
        &after[..before.len()],
        "{ctx}: the earlier request's segments must survive, unchanged and in order, as \
         a leading prefix of the later one's"
    );
}

// ---------------------------------------------------------------------
// 1. Three ordinary turns on one root session.
// ---------------------------------------------------------------------

#[tokio::test]
async fn three_consecutive_turns_form_a_growing_prefix_with_a_stable_prefix_key() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());

    // "Process" 1: `start_root`, turn one.
    let (runtime1, backend1) = build_runtime_over(
        store.clone(),
        vec![
            ScriptedTurn::Respond(tool_call_response("c1")),
            ScriptedTurn::Respond(text_response("turn one done")),
        ],
    );
    let mut stream1 = runtime1.subscribe();
    let agent1 = runtime1.start_root(root_spec("turn one")).await.unwrap();
    wait_for_agent_finished(&mut stream1, agent1).await;
    let session = session_of(&runtime1, agent1);
    drop(runtime1);

    // "Process" 2: a fresh `Runtime` over the same store, `resume_root`,
    // then a follow-up prompt.
    let (runtime2, backend2) = build_runtime_over(
        store.clone(),
        vec![
            ScriptedTurn::Respond(tool_call_response("c2")),
            ScriptedTurn::Respond(text_response("turn two done")),
        ],
    );
    let mut stream2 = runtime2.subscribe();
    let agent2 = runtime2.resume_root(resume_spec(session)).await.unwrap();
    runtime2
        .prompt(agent2, "turn two".to_string())
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream2, agent2).await;
    drop(runtime2);

    // "Process" 3: same again, turn three.
    let (runtime3, backend3) = build_runtime_over(
        store.clone(),
        vec![
            ScriptedTurn::Respond(tool_call_response("c3")),
            ScriptedTurn::Respond(text_response("turn three done")),
        ],
    );
    let mut stream3 = runtime3.subscribe();
    let agent3 = runtime3.resume_root(resume_spec(session)).await.unwrap();
    runtime3
        .prompt(agent3, "turn three".to_string())
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream3, agent3).await;

    let mut calls = backend1.calls();
    calls.extend(backend2.calls());
    calls.extend(backend3.calls());
    assert_eq!(
        calls.len(),
        6,
        "three turns of (tool call, tool result, text) must reach the backend exactly \
         twice each, calls: {calls:#?}"
    );

    // Every consecutive pair -- both the intra-turn (call -> result) boundary
    // and the cross-turn boundary -- must be an exact leading prefix, AND
    // must actually grow (never regress to being merely equal, which would
    // mean the "new material at the end" half of the premise is untested).
    for i in 0..calls.len() - 1 {
        let before = content_identity(&calls[i].segments);
        let after = content_identity(&calls[i + 1].segments);
        assert_is_leading_prefix(&before, &after, &format!("call {i} -> call {}", i + 1));
        assert!(
            before.len() < after.len(),
            "call {i} -> call {}: expected strictly more segments, got {} -> {}",
            i + 1,
            before.len(),
            after.len()
        );
    }

    // `prefix_key` is the runtime's own cache/dedup key over the fixed
    // static+inherited boundary (`crate::context::prefix::boundary_index`).
    // A root session inherits nothing (`context/path.rs`'s own doc: "Root /
    // Spawn: own records only"), so every one of these six requests' entire
    // history -- every user turn, every tool call/result, every assistant
    // reply -- sits in the VOLATILE tier, and the boundary never moves past
    // the unconditional `ToolSchemas` segment. With the same model and the
    // same registered tool set throughout, the key must be the SAME value
    // on every one of the six calls -- not merely "unchanged between
    // consecutive pairs".
    let model = ModelId::new("m");
    let first_key = prefix_key(&model, &calls[0].segments);
    for (i, call) in calls.iter().enumerate().skip(1) {
        let key = prefix_key(&model, &call.segments);
        assert_eq!(
            key, first_key,
            "call {i}'s prefix_key must equal call 0's -- the static prefix (tool \
             registry only, for a root session) never changed across these six calls"
        );
    }
}

// ---------------------------------------------------------------------
// 2. A fork child's first request against the parent's last request.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_fork_childs_first_request_extends_the_parents_last_request_as_a_wire_byte_prefix() {
    let (runtime, backend) = build_runtime(vec![
        ScriptedTurn::Respond(tool_call_response("c1")),
        ScriptedTurn::Respond(text_response("root done")),
        ScriptedTurn::Respond(text_response("child done")),
    ]);

    let mut stream = runtime.subscribe();
    let root = runtime
        .start_root(root_spec("investigate"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, root).await;

    let calls_before_fork = backend.calls();
    assert_eq!(calls_before_fork.len(), 2, "root's own tool round trip");
    let parent_last = calls_before_fork.last().unwrap().clone();

    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(
        &*runtime,
        root,
        root,
        SubagentSpec::fork("look closer", Budget::default()),
    )
    .await
    .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    let calls = backend.calls();
    assert_eq!(
        calls.len(),
        3,
        "root's two calls, then the fork child's one call"
    );
    let child_first = &calls[2];

    let before = wire_identity(&parent_last.segments);
    let after = wire_identity(&child_first.segments);
    assert_is_leading_prefix(
        &before,
        &after,
        "the parent's last request -> the fork child's first request (wire bytes only)",
    );
    assert!(
        before.len() < after.len(),
        "the fork child must inherit strictly more than the parent's last request alone \
         (at minimum, the parent's own final reply, recorded only after that request was \
         sent) -- got {} -> {}",
        before.len(),
        after.len()
    );

    // Sanity: this genuinely exercised a fork, not merely a plain turn --
    // the child's context contains an `Inherited` segment at all.
    assert!(
        child_first
            .segments
            .iter()
            .any(|s| matches!(s.provenance, Provenance::Inherited { .. })),
        "a fork child's first request must carry at least one Inherited segment"
    );
}
