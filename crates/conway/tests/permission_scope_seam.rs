//! Seam tests for the grant-SCOPE wiring (grant-prompt Axis B, decision
//!: "WIRE IT"), and for the `render_kind`
//! plumbing the prompt's offer now depends on (Axis A).
//!
//! `conway-runtime`'s broker tests already prove `GrantScope::covers` in
//! isolation. What they cannot prove is that the PRODUCTION surfaces an
//! operator (or embedder) actually touches -- `Conway::grant_permission_pattern`
//! with a scope, the same facade call the TUI's `p` key now makes with the
//! scope its `s` key cycled to -- install a grant the real stack then
//! honors NARROWLY. This file drives the genuine stack (`Conway` with the
//! `builtin-tools` feature's real `read` tool, real `ToolRunner`, real
//! `PermissionBroker`) and asserts on the observable outcome: whether the
//! operator's gate is consulted at all.
//!
//! **Why `read`, not `bash` (AMENDED by board item
//! `01KZDDPC5MMD49F6JPV9CW4TVM`).** These scope tests exercise
//! `GrantScope::covers`, which is completely orthogonal to a tool's
//! `RenderKind` -- scope answers "who does this grant cover", `RenderKind`
//! answers "can a pattern grant cover this tool's render at all". They used
//! to run against `bash:git status`, back when a `ShellCommand` tool's
//! benign, unchained rendering could still be matched by a pattern grant.
//! That is no longer true for ANY `ShellCommand` tool, by design (see
//! `conway_core::permission_pattern`'s own module doc): a `bash` grant would
//! never cover even its OWN granting agent's identical later call, which
//! would make every "positive control" below fail before the scope logic it
//! exists to test is ever reached. `read` is a `RenderKind::Structured`
//! tool, entirely unaffected by that change, so `read:*` still isolates
//! exactly the scope-narrowing property this file exists to prove.
//!
//! The negative cases are the point: a per-agent grant must NOT
//! authorize a different agent's identical call, and a per-subtree grant
//! must NOT authorize an agent outside the subtree. Each negative case is
//! paired with a positive control (the covered agent IS authorized without
//! a prompt) so a grant that simply never matched anything would fail the
//! pair, not pass it -- and the per-agent negative was break-the-guard
//! verified (temporarily mapping `PermissionScope::Agent` to
//! `GrantScope::Session` in `grant_scope_for` makes it fail immediately;
//! see this item's completion report for the output).
//!
//! **The session-scope child-coverage case (board item
//! `01M32EBVQR3QKHK9FF3JHBFS2F`).** The file's negative cases pin what a
//! grant must NOT cover; its newest test pins the session-scope positive
//! the 2026-09-20 proxy run's architect re-prompts turned on: a
//! session-scope PATTERN grant must cover a SPAWNED child -- different
//! `agent_id`, `agent_path` under the parent -- for a DIFFERENT argument
//! value than the call that prompted the grant. That different-value half
//! is what separates a pattern grant (matched by rule, any args) from an
//! exact-args `AllowAlways` cache entry (`CacheKey`), so the test's gate
//! answers `AllowOnce` throughout and no cache entry ever exists: the
//! pattern grant is the only mechanism that CAN spare the child's prompt.
#![cfg(feature = "builtin-tools")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway::test_support::{base_config, build_conway_with_builtins, scripted_backend};
use conway::{Conway, PatternRule, SessionSpec, SpawnSpec};
use conway_core::agent::{PermissionDecision, PermissionRequest, PermissionScope};
use conway_core::content::{StopReason, ToolCall, ToolResult, Usage};
use conway_core::ids::{AgentId, ToolName};
use conway_core::log::LogRecord;
use conway_core::ports::{GenerateResponse, PermissionGate, RenderKind};
use conway_testkit::{text_response, ScriptedTurn};

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

/// The two turns one `read` prompt consumes -- the `RenderKind::Structured`
/// stand-in for the old `bash git status --short` script (see this file's
/// own module doc for why `read` is what the scope tests use now).
fn read_cargo_toml_script(call_id: &str) -> Vec<ScriptedTurn> {
    vec![
        ScriptedTurn::Respond(tool_call_response(
            call_id,
            "read",
            serde_json::json!({ "path": "Cargo.toml" }),
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
    let mut script = read_cargo_toml_script("a1");
    script.extend(read_cargo_toml_script("a2"));
    script.extend(read_cargo_toml_script("b1"));
    let conway = build_conway_with_builtins(
        base_config(),
        scripted_backend(script),
        gate.clone() as Arc<dyn PermissionGate>,
    );

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
        PatternRule::parse("read:*").expect("valid rule"),
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
    let mut script = read_cargo_toml_script("c1");
    script.extend(read_cargo_toml_script("c2"));
    let conway = build_conway_with_builtins(
        base_config(),
        scripted_backend(script),
        gate.clone() as Arc<dyn PermissionGate>,
    );

    // A subtree rooted at an agent that does not exist: no real
    // `agent_path` can contain it, so this grant covers NO ONE.
    conway.grant_permission_pattern(
        PatternRule::parse("read:*").expect("valid rule"),
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
        PatternRule::parse("read:*").expect("valid rule"),
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

/// **The session-scope positive case, proven through a SPAWNED child.**
/// A session-scope pattern grant installed in the shared broker must
/// authorize a spawned subagent -- a different `agent_id`, whose
/// `agent_path` sits under the parent's -- for a call with a DIFFERENT
/// argument value than the one that prompted the grant. The gate answers
/// `AllowOnce` throughout, so no `AllowAlways` cache entry ever exists and
/// the pattern grant is the only mechanism that can spare any prompt after
/// the first: a second green prompt proves the grant is not inert, and the
/// child's prompt-free, differently-argued read proves the shared broker
/// carries it across the spawn boundary (board item
/// `01M32EBVQR3QKHK9FF3JHBFS2F`).
#[tokio::test]
async fn a_session_scoped_pattern_grant_authorizes_a_spawned_childs_differently_argued_call() {
    let gate = RecordingGate::new();
    let mut script = read_cargo_toml_script("p1"); // the root's first call: reaches the gate
    script.extend(read_cargo_toml_script("p2")); // the root's second call: the grant's positive control
                                                 // The child's call: same tool, DIFFERENT argument value -- the shape no
                                                 // exact-args cache entry could ever cover (and none exists here at all).
                                                 // (`Cargo.toml` is the file the root's own scripted calls read, so
                                                 // `src/lib.rs` -- a real file under the test crate, the cwd `cargo test`
                                                 // runs in -- is the differently-argued call.)
    script.push(ScriptedTurn::Respond(tool_call_response(
        "p3",
        "read",
        serde_json::json!({ "path": "src/lib.rs" }),
    )));
    script.push(ScriptedTurn::Respond(text_response("done")));
    let conway = build_conway_with_builtins(
        base_config(),
        scripted_backend(script),
        gate.clone() as Arc<dyn PermissionGate>,
    );

    // The root's first `read` reaches the gate -- which is also how the
    // test learns the granting agent's real id (the spawned child's
    // `agent_path` will root at this agent, exactly as `SubagentHost::
    // start` builds it).
    let session = live_session(&conway).await;
    prompt_once(&session).await;
    let requests = gate.requests();
    assert_eq!(requests.len(), 1, "the first call must reach the gate");
    let parent = requests[0].agent_id;

    // The grant, installed at session scope via the same facade method the
    // TUI's `p`-at-session-scope installs it with.
    conway.grant_permission_pattern(
        PatternRule::parse("read:*").expect("valid rule"),
        PermissionScope::Session,
        parent,
    );

    // Positive control: the granting agent's own later call is covered.
    prompt_once(&session).await;
    assert_eq!(
        gate.requests().len(),
        1,
        "the granting agent's own matching call must NOT re-consult the gate \
         (or the grant is simply inert and the child case below proves nothing)"
    );

    // The case itself: a SPAWNED child -- different agent_id, its
    // `agent_path` under the parent's -- makes a call with a DIFFERENT
    // argument value than either of the root's. Only a rule-matched pattern
    // grant can authorize this; no cache entry exists to confuse the two
    // mechanisms.
    let child = session
        .spawn(session.root(), SpawnSpec::new("read the other source file"))
        .await
        .expect("spawn should succeed");
    assert_ne!(
        child, parent,
        "the child must genuinely be a DIFFERENT agent -- otherwise this test \
         proves nothing about cross-agent coverage"
    );
    let _ = tokio::time::timeout(Duration::from_secs(10), session.await_agent(child))
        .await
        .expect("child turn must not hang")
        .expect("await_agent should resolve Ok");
    assert_eq!(
        gate.requests().len(),
        1,
        "the spawned child's differently-argued call must be authorized by the \
         session-scope pattern grant in the shared broker, never reaching the gate"
    );

    // And the authorized read actually ran: the child's own tool result
    // must be a success, not a denial recorded after a gate the test never
    // saw.
    let records = session
        .transcript(child)
        .await
        .expect("transcript should resolve");
    let result: &ToolResult = records
        .iter()
        .rev()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. } => Some(result),
            _ => None,
        })
        .expect("the child's transcript must contain a ToolResultRecord");
    assert!(
        !result.is_error,
        "the covered read must have actually succeeded: {:?}",
        result.blocks
    );
}

/// **The `[p]` field editor's own grant shape, through a spawned child.**
/// The proxy run's operator grant was NOT the flat form above: the TUI's
/// `[p]` field editor submits a structured `When::ArgsMatch` rule via
/// `Conway::grant_permission_rule` (board item `01M32EBVQR3QKHK9FF3JHBFS2F`'s
/// verdict). With NO fields pinned -- the all-wildcard default -- that rule
/// is the `tool:*` equivalent, and it must cover a spawned child's
/// differently-argued call exactly like the flat form, or the editor's
/// "any call" promise is a lie for every agent but the granter.
#[tokio::test]
async fn the_p_field_editors_structured_any_call_grant_covers_a_spawned_child() {
    let gate = RecordingGate::new();
    let mut script = read_cargo_toml_script("q1");
    script.push(ScriptedTurn::Respond(tool_call_response(
        "q2",
        "read",
        serde_json::json!({ "path": "src/lib.rs" }),
    )));
    script.push(ScriptedTurn::Respond(text_response("done")));
    let conway = build_conway_with_builtins(
        base_config(),
        scripted_backend(script),
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let session = live_session(&conway).await;
    prompt_once(&session).await;
    let parent = gate.requests()[0].agent_id;

    let installed = conway.grant_permission_rule(
        conway::Rule::args_match_allow_rule("read", std::collections::BTreeMap::new()),
        PermissionScope::Session,
        parent,
    );
    assert!(installed, "the structured any-call rule must install");

    let child = session
        .spawn(session.root(), SpawnSpec::new("read the other source file"))
        .await
        .expect("spawn should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(10), session.await_agent(child))
        .await
        .expect("child turn must not hang")
        .expect("await_agent should resolve Ok");
    assert_eq!(
        gate.requests().len(),
        1,
        "the child's differently-argued read must be covered by the structured \
         any-call rule installed the way the TUI's `[p]` editor installs it"
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
    let conway = build_conway_with_builtins(
        base_config(),
        scripted_backend(script),
        gate.clone() as Arc<dyn PermissionGate>,
    );

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
