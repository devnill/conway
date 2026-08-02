//! Seam tests for the grant-SCOPE wiring (grant-prompt Axis B, decision
//! 01KZ1NAXE0KZRSRFBDDJFCPMK8: "WIRE IT"), and for the `render_kind`
//! plumbing the prompt's offer now depends on (Axis A).
//!
//! `conway-runtime`'s broker tests already prove `GrantScope::covers` in
//! isolation. What they cannot prove is that the PRODUCTION surfaces an
//! operator (or embedder) actually touches -- `Conway::grant_permission_pattern`
//! with a scope, the same facade call the TUI's `p` key now makes with the
//! scope its `s` key cycled to -- install a grant the real stack then
//! honors NARROWLY. This file drives the genuine stack (`Conway` with the
//! `builtin-tools` feature's real `bash` tool, real `ToolRunner`, real
//! `PermissionBroker`) and asserts on the observable outcome: whether the
//! operator's gate is consulted at all.
//!
//! The negative cases are the point (GP-14): a per-agent grant must NOT
//! authorize a different agent's identical call, and a per-subtree grant
//! must NOT authorize an agent outside the subtree. Each negative case is
//! paired with a positive control (the covered agent IS authorized without
//! a prompt) so a grant that simply never matched anything would fail the
//! pair, not pass it -- and the per-agent negative was break-the-guard
//! verified (temporarily mapping `PermissionScope::Agent` to
//! `GrantScope::Session` in `grant_scope_for` makes it fail immediately;
//! see this item's completion report for the output).
#![cfg(feature = "builtin-tools")]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    RoleEntry, RoutingSection, SessionConfig, TuiSection,
};
use conway::{Conway, ConwayBuilder, PatternRule, SessionSpec};
use conway_core::agent::{PermissionDecision, PermissionRequest, PermissionScope};
use conway_core::content::{ContentBlock, StopReason, ToolCall, Usage};
use conway_core::fakes::{FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, ToolName};
use conway_core::ports::{Backend, GenerateResponse, PermissionGate, RenderKind};

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }))
}

fn text_response(text: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage::default(),
    }
}

/// One scripted tool call (`bash` or `read`), followed by a final text
/// response once the tool step completes. Each `call_id` must be unique
/// within a session's script (the broker keys grants and cache entries by
/// call, and duplicate ids across TURNS of one session would be a fixture
/// bug, not production behavior).
fn tool_call_response(call_id: &str, tool: &str, arguments: serde_json::Value) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: ToolName::new(tool),
            arguments,
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

fn base_config() -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "default".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
        },
    );
    ConwayConfig {
        default_role: RoleAlias::new("default"),
        cwd: std::path::PathBuf::from("."),
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        permissions: PermissionsConfig::default(),
        backends: BTreeMap::new(),
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig::default(),
        tui: TuiSection::default(),
    }
}

/// Records every `PermissionRequest` it receives and always answers
/// `AllowOnce`: a call the test EXPECTS to reach the gate (the negative
/// cases) then runs for real (`git status --short` in the fixture cwd is
/// harmless), and a call the grant covers must never reach it at all --
/// making gate-consultation the observable both directions assert on.
struct RecordingGate {
    requests: Mutex<Vec<PermissionRequest>>,
}

impl RecordingGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<PermissionRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl PermissionGate for RecordingGate {
    async fn check(&self, req: PermissionRequest) -> PermissionDecision {
        self.requests.lock().unwrap().push(req);
        PermissionDecision::AllowOnce
    }
}

/// Builds a `Conway` whose scripted backend serves `script` in order --
/// every prompt a test makes consumes exactly the turns that prompt needs,
/// so the script's length IS the test's expected turn count.
fn build_conway(script: Vec<ScriptedTurn>, gate: Arc<dyn PermissionGate>) -> Conway {
    let backend = Arc::new(ScriptedBackend::new(script).with_id(BackendId::new("fake")));
    let store = Arc::new(FakeStore::new());
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend as Arc<dyn Backend>)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should succeed with the real builtin tools registered")
}

/// The two turns one `bash git status --short` prompt consumes.
fn bash_git_status_script(call_id: &str) -> Vec<ScriptedTurn> {
    vec![
        ScriptedTurn::Respond(tool_call_response(
            call_id,
            "bash",
            serde_json::json!({ "command": "git status --short" }),
        )),
        ScriptedTurn::Respond(text_response("done")),
    ]
}

/// A multi-turn session. `keep_alive: true` is REQUIRED, not a nicety:
/// these tests prompt the same session twice (once to learn the real
/// requesting agent's id from the gate, once as the grant's positive
/// control), and a second prompt on a non-keep-alive session never runs a
/// turn at all (see `keep_alive.rs`'s own module doc).
async fn live_session(conway: &Conway) -> conway::SessionHandle {
    conway
        .new_session(SessionSpec {
            keep_alive: true,
            ..SessionSpec::default()
        })
        .await
        .expect("new_session")
}

/// Drives one prompt to its `TurnFinished`. Consumed via
/// `TurnHandle::text()` rather than `result()` precisely because the
/// session is keep-alive: a keep-alive turn's completion emits
/// `TurnFinished`, never `AgentFinished` (which is what `result()` waits
/// on) -- see `session_handle.rs`'s own doc.
async fn prompt_once(handle: &conway::SessionHandle) {
    let turn = handle.prompt("do the thing").await.expect("prompt");
    // One prompt here is two loop steps: a ToolUse step (whose
    // `TurnFinished` carries EMPTY text) and then the final text step.
    // `text()` resolves at the FIRST `TurnFinished` it sees -- the tool
    // step's, before the tool has even run -- so a single `text()` call
    // would return mid-prompt and let the next prompt's script entries
    // interleave with this one's. Drain until the final, non-empty text
    // step lands.
    let drain = async {
        loop {
            let text = turn.text().await.expect("text should succeed");
            if !text.is_empty() {
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), drain)
        .await
        .expect("turn must not hang");
}

/// **The per-agent negative case, end to end.** A pattern grant installed
/// at `PermissionScope::Agent` -- exactly what the TUI's `p` key produces
/// after one `s` press, via the same facade method this test calls --
/// must authorize the granting agent's own later calls and NO ONE ELSE's.
#[tokio::test]
async fn an_agent_scoped_pattern_grant_does_not_authorize_a_different_agent() {
    let gate = RecordingGate::new();
    // Two prompts for session A (first reaches the gate, second is covered
    // by the grant), one for session B (must reach the gate again).
    let mut script = bash_git_status_script("a1");
    script.extend(bash_git_status_script("a2"));
    script.extend(bash_git_status_script("b1"));
    let conway = build_conway(script, gate.clone() as Arc<dyn PermissionGate>);

    // Session A's first call: no grant yet, so it reaches the gate --
    // which is also how the test learns the REAL requesting agent's id
    // (never a hand-picked fixture id that could accidentally agree).
    let session_a = live_session(&conway).await;
    prompt_once(&session_a).await;
    let requests = gate.requests();
    assert_eq!(requests.len(), 1, "the first call must reach the gate");
    let agent_a = requests[0].agent_id;

    // The grant, installed the way the TUI's `p`-at-agent-scope installs
    // it: same facade method, same scope, the prompting agent as granter.
    conway.grant_permission_pattern(
        PatternRule::parse("bash:git status").expect("valid rule"),
        PermissionScope::Agent,
        agent_a,
    );

    // Positive control: the SAME agent's identical later call is covered.
    prompt_once(&session_a).await;
    assert_eq!(
        gate.requests().len(),
        1,
        "the granting agent's own matching call must NOT re-consult the gate \
         (or the grant is simply inert and the negative case below proves nothing)"
    );

    // The negative case: a DIFFERENT agent (a fresh session's root) running
    // the byte-identical command must be asked for itself.
    let session_b = live_session(&conway).await;
    prompt_once(&session_b).await;
    let requests = gate.requests();
    assert_eq!(
        requests.len(),
        2,
        "a per-agent grant must never authorize a different agent's identical call"
    );
    assert_ne!(
        requests[1].agent_id, agent_a,
        "the second request must genuinely come from a DIFFERENT agent -- \
         otherwise this test's negative case is vacuous"
    );
}

/// **The per-subtree negative case, end to end.** A grant scoped to a
/// subtree the requester is not in must not authorize it; the same grant
/// scoped to the requester's OWN subtree must. (The descendant-coverage
/// half of `Subtree` is broker-tested in
/// `conway-runtime/tests/permission_broker.rs`; the facade-level proof
/// here is that a real grant installed through the public surface narrows
/// at all.)
#[tokio::test]
async fn a_subtree_scoped_pattern_grant_does_not_authorize_an_agent_outside_the_subtree() {
    let gate = RecordingGate::new();
    let mut script = bash_git_status_script("c1");
    script.extend(bash_git_status_script("c2"));
    let conway = build_conway(script, gate.clone() as Arc<dyn PermissionGate>);

    // A subtree rooted at an agent that does not exist: no real
    // `agent_path` can contain it, so this grant covers NO ONE.
    conway.grant_permission_pattern(
        PatternRule::parse("bash:git status").expect("valid rule"),
        PermissionScope::AgentSubtree,
        AgentId::new(),
    );

    let session = live_session(&conway).await;
    prompt_once(&session).await;
    let requests = gate.requests();
    assert_eq!(
        requests.len(),
        1,
        "a subtree grant the requester is outside of must not authorize it"
    );
    let agent = requests[0].agent_id;

    // Positive control: a subtree grant rooted at the requester itself
    // covers that requester (its own `agent_path` contains the root).
    conway.grant_permission_pattern(
        PatternRule::parse("bash:git status").expect("valid rule"),
        PermissionScope::AgentSubtree,
        agent,
    );
    prompt_once(&session).await;
    assert_eq!(
        gate.requests().len(),
        1,
        "a subtree grant rooted at the requesting agent must cover it \
         (or the negative case above proves nothing)"
    );
}

/// **Axis A's plumbing, proven where it is consumed.** The prompt's offer
/// logic (`suggested_rule`) now takes the requesting tool's `render_kind`;
/// this asserts the `PermissionRequest` the gate actually receives carries
/// the REAL tool's REAL declaration through the production render seam --
/// `ShellCommand` for `bash` (its rendering IS the shell command),
/// `Structured` for `read` (a JSON dump no shell ever sees) -- rather than
/// some default or a second lookup that could disagree with the value the
/// broker's own evaluation just used.
#[tokio::test]
async fn the_gate_request_carries_the_proposing_tools_own_render_kind() {
    let gate = RecordingGate::new();
    let script = vec![
        ScriptedTurn::Respond(tool_call_response(
            "k1",
            "bash",
            serde_json::json!({ "command": "git status" }),
        )),
        ScriptedTurn::Respond(text_response("done")),
        ScriptedTurn::Respond(tool_call_response(
            "k2",
            "read",
            serde_json::json!({ "path": "Cargo.toml" }),
        )),
        ScriptedTurn::Respond(text_response("done")),
    ];
    let conway = build_conway(script, gate.clone() as Arc<dyn PermissionGate>);

    let session = live_session(&conway).await;
    prompt_once(&session).await;
    prompt_once(&session).await;

    let requests = gate.requests();
    assert_eq!(requests.len(), 2, "both calls must reach the gate");
    assert_eq!(
        requests[0].render_kind,
        RenderKind::ShellCommand,
        "bash's prompt must carry its real declaration"
    );
    assert_eq!(
        requests[1].render_kind,
        RenderKind::Structured,
        "read's prompt must carry its real declaration -- a default here \
         would silently re-hide every Structured tool's pattern offer"
    );
}

