//! Acceptance tests for `PermissionBroker` (WI-078, architecture §4.3).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use conway_core::agent::{PermissionDecision, PermissionRequest, PermissionScope};
use conway_core::content::ToolCategory;
use conway_core::event::{Envelope, Event};
use conway_core::ids::{AgentId, SessionId, ToolName};
use conway_core::ports::PermissionGate;
use conway_runtime::events::EventBus;
use conway_runtime::permission::{
    AgentRoot, AuthorizedCall, PermissionBroker, PermissionCtx, PermissionOutcome,
};
use conway_core::permission_mode::PermissionMode;
use conway_core::permission_pattern::PatternRule;
use futures::StreamExt;

/// A gate that plays back a fixed script of decisions in order, recording
/// every request it receives and how many times it was called. Exhausting
/// the script denies — tests are written so this never happens on a path
/// meant to hit the cache.
struct ScriptedGate {
    calls: Mutex<u32>,
    responses: Mutex<VecDeque<PermissionDecision>>,
    requests: Mutex<Vec<PermissionRequest>>,
}

impl ScriptedGate {
    fn new(responses: Vec<PermissionDecision>) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(0),
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn call_count(&self) -> u32 {
        *self.calls.lock().unwrap()
    }

    fn requests(&self) -> Vec<PermissionRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl PermissionGate for ScriptedGate {
    async fn check(&self, req: PermissionRequest) -> PermissionDecision {
        *self.calls.lock().unwrap() += 1;
        self.requests.lock().unwrap().push(req);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(PermissionDecision::Deny {
                reason: "scripted gate exhausted".into(),
            })
    }
}

fn ctx(agent_id: AgentId, agent_path: Vec<AgentId>, session: SessionId) -> PermissionCtx {
    PermissionCtx {
        agent_id,
        agent_path,
        session,
        cwd: PathBuf::from("/tmp"),
        // S5: this file's tests are all about the cache/pattern/mode
        // machinery, not the root check -- `Unconfined` keeps every one of
        // them byte-for-byte unchanged from before this field existed.
        // `crates/conway/tests/root_containment_seam.rs` is the root check's
        // own dedicated (real end-to-end) test file.
        root: AgentRoot::Unconfined,
    }
}

fn call(call_id: &str) -> AuthorizedCall {
    AuthorizedCall {
        call_id: call_id.into(),
        tool: ToolName::new("read"),
        category: ToolCategory::Read,
        arguments: serde_json::json!({"path": "a.txt"}),
        rendered: "read a.txt".into(),
        path_args: conway_core::ports::PathArgs::Named(&["path"]),
        // `read` genuinely declares `Structured` in production (its render
        // is never a shell command); this fixture's `rendered` is a
        // hand-typed legible string anyway, not the real JSON dump, so
        // either `RenderKind` would pass every test in this file -- this
        // is the honest, production-matching declaration.
        render_kind: conway_core::ports::RenderKind::Structured,
    }
}

fn broker(gate: Arc<dyn PermissionGate>) -> (PermissionBroker, Arc<EventBus>) {
    let bus = EventBus::new(256);
    (PermissionBroker::new(gate, bus.clone()), bus)
}

async fn next_envelope(stream: &mut (impl futures::Stream<Item = Envelope> + Unpin)) -> Envelope {
    stream.next().await.expect("event stream ended early")
}

#[tokio::test]
async fn allow_always_session_caches_second_identical_call() {
    let gate = ScriptedGate::new(vec![PermissionDecision::AllowAlways {
        scope: PermissionScope::Session,
    }]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    let first = broker.decide(&c, &call("c1")).await;
    let second = broker.decide(&c, &call("c2")).await;

    assert_eq!(first, PermissionOutcome::Allow);
    assert_eq!(second, PermissionOutcome::Allow);
    assert_eq!(gate.call_count(), 1, "gate must be consulted exactly once");
}

#[tokio::test]
async fn allow_always_agent_scope_caches_for_granting_agent_only() {
    let gate = ScriptedGate::new(vec![
        PermissionDecision::AllowAlways {
            scope: PermissionScope::Agent,
        },
        PermissionDecision::AllowOnce,
    ]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let granter = AgentId::new();
    let sibling = AgentId::new();

    let granter_ctx = ctx(granter, vec![granter], session);
    let sibling_ctx = ctx(sibling, vec![sibling], session);

    let granted = broker.decide(&granter_ctx, &call("c1")).await;
    assert_eq!(granted, PermissionOutcome::Allow);
    assert_eq!(gate.call_count(), 1);

    // Same tool + args, but a different (sibling) agent: must re-consult.
    let sibling_result = broker.decide(&sibling_ctx, &call("c2")).await;
    assert_eq!(sibling_result, PermissionOutcome::Allow);
    assert_eq!(
        gate.call_count(),
        2,
        "a sibling agent's identical call must re-consult the gate"
    );

    // The granting agent itself still hits the cache on a third call.
    let cached_again = broker.decide(&granter_ctx, &call("c3")).await;
    assert_eq!(cached_again, PermissionOutcome::Allow);
    assert_eq!(gate.call_count(), 2);
}

#[tokio::test]
async fn allow_always_agent_subtree_honored_for_descendant_not_others() {
    let gate = ScriptedGate::new(vec![
        PermissionDecision::AllowAlways {
            scope: PermissionScope::AgentSubtree,
        },
        PermissionDecision::AllowOnce,
    ]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let root = AgentId::new();
    let child = AgentId::new();
    let unrelated = AgentId::new();

    let root_ctx = ctx(root, vec![root], session);
    let granted = broker.decide(&root_ctx, &call("c1")).await;
    assert_eq!(granted, PermissionOutcome::Allow);
    assert_eq!(gate.call_count(), 1);

    // Descendant: agent_path contains the granting agent -> cache hit.
    let child_ctx = ctx(child, vec![root, child], session);
    let child_result = broker.decide(&child_ctx, &call("c2")).await;
    assert_eq!(child_result, PermissionOutcome::Allow);
    assert_eq!(
        gate.call_count(),
        1,
        "a descendant must hit the subtree grant without consulting the gate"
    );

    // Not a descendant: agent_path does not contain the granting agent.
    let unrelated_ctx = ctx(unrelated, vec![unrelated], session);
    let unrelated_result = broker.decide(&unrelated_ctx, &call("c3")).await;
    assert_eq!(unrelated_result, PermissionOutcome::Allow);
    assert_eq!(
        gate.call_count(),
        2,
        "a non-descendant must re-consult the gate"
    );
}

#[tokio::test]
async fn allow_once_is_never_cached() {
    let gate = ScriptedGate::new(vec![
        PermissionDecision::AllowOnce,
        PermissionDecision::AllowOnce,
    ]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    let first = broker.decide(&c, &call("c1")).await;
    let second = broker.decide(&c, &call("c2")).await;

    assert_eq!(first, PermissionOutcome::Allow);
    assert_eq!(second, PermissionOutcome::Allow);
    assert_eq!(
        gate.call_count(),
        2,
        "AllowOnce must never be cached: two identical calls invoke the gate twice"
    );
}

#[tokio::test]
async fn deny_carries_reason_and_is_not_cached() {
    let gate = ScriptedGate::new(vec![
        PermissionDecision::Deny {
            reason: "not on the allowlist".into(),
        },
        PermissionDecision::Deny {
            reason: "still not on the allowlist".into(),
        },
    ]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    let first = broker.decide(&c, &call("c1")).await;
    match &first {
        PermissionOutcome::Deny { rendered_error } => {
            assert!(rendered_error.contains("not on the allowlist"));
        }
        PermissionOutcome::Allow => panic!("expected Deny"),
    }

    // Deny is never cached: a second identical call re-consults the gate.
    let second = broker.decide(&c, &call("c2")).await;
    assert!(matches!(second, PermissionOutcome::Deny { .. }));
    assert_eq!(gate.call_count(), 2);
}

#[tokio::test]
async fn deny_with_feedback_maps_message_to_rendered_error() {
    let gate = ScriptedGate::new(vec![PermissionDecision::DenyWithFeedback {
        message: "pass --force to overwrite".into(),
    }]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    let outcome = broker.decide(&c, &call("c1")).await;

    // `PermissionOutcome` has no separate "abort" variant, so any `Deny` —
    // including one derived from `DenyWithFeedback` — is, by construction,
    // something the caller can only turn into a model-visible tool error,
    // never an abort.
    assert_eq!(
        outcome,
        PermissionOutcome::Deny {
            rendered_error: "pass --force to overwrite".into()
        }
    );
}

#[tokio::test]
async fn each_decision_emits_exactly_one_requested_then_one_resolved_in_seq_order() {
    let gate = ScriptedGate::new(vec![
        PermissionDecision::AllowOnce,
        PermissionDecision::AllowAlways {
            scope: PermissionScope::Session,
        },
        PermissionDecision::Deny {
            reason: "no".into(),
        },
    ]);
    let (broker, bus) = broker(gate.clone());
    let mut stream = bus.subscribe();
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.decide(&c, &call("c1")).await;
    broker.decide(&c, &call("c2")).await;
    broker.decide(&c, &call("c3")).await;

    for expected_call_id in ["c1", "c2", "c3"] {
        let requested = next_envelope(&mut stream).await;
        match &requested.event {
            Event::PermissionRequested { call_id, .. } => {
                assert_eq!(call_id, expected_call_id)
            }
            other => panic!("expected PermissionRequested, got {other:?}"),
        }

        let resolved = next_envelope(&mut stream).await;
        match &resolved.event {
            Event::PermissionResolved { call_id, .. } => {
                assert_eq!(call_id, expected_call_id)
            }
            other => panic!("expected PermissionResolved, got {other:?}"),
        }

        assert!(
            requested.seq < resolved.seq,
            "PermissionRequested (seq {}) must precede PermissionResolved (seq {}) for {expected_call_id}",
            requested.seq,
            resolved.seq
        );
    }
}

#[tokio::test]
async fn cached_hit_still_emits_requested_then_resolved_pair() {
    let gate = ScriptedGate::new(vec![PermissionDecision::AllowAlways {
        scope: PermissionScope::Session,
    }]);
    let (broker, bus) = broker(gate.clone());
    let mut stream = bus.subscribe();
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.decide(&c, &call("c1")).await; // populates the cache
    broker.decide(&c, &call("c2")).await; // cache hit, no gate call

    // First pair (gate consulted).
    assert!(matches!(
        next_envelope(&mut stream).await.event,
        Event::PermissionRequested { .. }
    ));
    assert!(matches!(
        next_envelope(&mut stream).await.event,
        Event::PermissionResolved { .. }
    ));

    // Second pair (cache hit) — still exactly one Requested + one Resolved.
    assert!(matches!(
        next_envelope(&mut stream).await.event,
        Event::PermissionRequested { .. }
    ));
    let resolved = next_envelope(&mut stream).await;
    match resolved.event {
        Event::PermissionResolved { decision, .. } => {
            assert_eq!(decision, conway_core::agent::PermissionDecisionKind::Cached);
        }
        other => panic!("expected PermissionResolved, got {other:?}"),
    }

    assert_eq!(gate.call_count(), 1);
}

#[tokio::test]
async fn cancelled_gate_yields_deny_cancelled_and_still_emits_resolved() {
    // Per the `PermissionGate` port contract, gate cancellation (e.g. the
    // process shutting down) is surfaced by the gate itself as
    // `Deny { reason: "cancelled" }`, never as a hang or a dropped call.
    let gate = ScriptedGate::new(vec![PermissionDecision::Deny {
        reason: "cancelled".into(),
    }]);
    let (broker, bus) = broker(gate.clone());
    let mut stream = bus.subscribe();
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    let outcome = broker.decide(&c, &call("c1")).await;

    assert_eq!(
        outcome,
        PermissionOutcome::Deny {
            rendered_error: "cancelled".into()
        }
    );

    assert!(matches!(
        next_envelope(&mut stream).await.event,
        Event::PermissionRequested { .. }
    ));
    assert!(matches!(
        next_envelope(&mut stream).await.event,
        Event::PermissionResolved { .. }
    ));
}

#[tokio::test]
async fn broker_builds_permission_request_with_full_agent_path() {
    let gate = ScriptedGate::new(vec![PermissionDecision::AllowOnce]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let root = AgentId::new();
    let mid = AgentId::new();
    let leaf = AgentId::new();
    let c = ctx(leaf, vec![root, mid, leaf], session);

    broker.decide(&c, &call("c1")).await;

    let requests = gate.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].agent_id, leaf);
    assert_eq!(requests[0].agent_path, vec![root, mid, leaf]);
    assert_eq!(requests[0].call_id, "c1");
    assert_eq!(requests[0].rendered, "read a.txt");
    assert_eq!(requests[0].category, ToolCategory::Read);
}

// ---------------------------------------------------------------------
// V2: permission modes and pattern grants.
// ---------------------------------------------------------------------

/// A `bash` call carrying `rendered` — the shape pattern grants care about.
fn bash_call(call_id: &str, rendered: &str) -> AuthorizedCall {
    AuthorizedCall {
        call_id: call_id.into(),
        tool: ToolName::new("bash"),
        category: ToolCategory::Execute,
        arguments: serde_json::json!({ "command": rendered }),
        rendered: rendered.into(),
        path_args: conway_core::ports::PathArgs::Unconfinable {
            checkable: &["cwd"],
        },
        render_kind: conway_core::ports::RenderKind::ShellCommand,
    }
}

/// **The most important test in V2.**
///
/// A pattern grant for `git status` must not authorize a chained command
/// that merely begins with it. The gate lives inside `PatternRule::matches`,
/// but this proves it holds through the broker's real decision path — and,
/// critically, that the chained command actually REACHES the operator's
/// gate rather than being silently allowed or silently denied.
#[tokio::test]
async fn a_chained_command_still_reaches_the_operator_despite_a_matching_pattern() {
    // The gate is scripted to deny, so if the pattern wrongly authorized
    // the chained command we would see Allow with zero gate calls.
    let gate = ScriptedGate::new(vec![PermissionDecision::Deny {
        reason: "operator said no".into(),
    }]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.remember_pattern(
        PatternRule::parse("bash:git status").expect("valid rule"),
        PermissionScope::Session,
        agent,
    );

    // The plain granted command is authorized without troubling the gate.
    let plain = broker.decide(&c, &bash_call("c1", "git status")).await;
    assert_eq!(plain, PermissionOutcome::Allow);
    assert_eq!(
        gate.call_count(),
        0,
        "the granted command must not re-prompt"
    );

    // The chained one must fall through to the operator.
    let chained = broker
        .decide(&c, &bash_call("c2", "git status && rm -rf /"))
        .await;
    assert!(
        matches!(chained, PermissionOutcome::Deny { .. }),
        "a chained command must not be authorized by a prefix grant"
    );
    assert_eq!(
        gate.call_count(),
        1,
        "the chained command must actually REACH the operator's gate -- \
         being silently denied would be almost as wrong as being allowed, \
         because it would mean the pattern layer decided rather than deferred"
    );
}

/// Adversarial: a grant for one subcommand must not cover another.
#[tokio::test]
async fn a_pattern_grant_does_not_cover_a_different_subcommand() {
    let gate = ScriptedGate::new(vec![PermissionDecision::Deny {
        reason: "nope".into(),
    }]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.remember_pattern(
        PatternRule::parse("bash:git status").expect("valid rule"),
        PermissionScope::Session,
        agent,
    );

    let pushed = broker
        .decide(&c, &bash_call("c1", "git push --force"))
        .await;
    assert!(
        matches!(pushed, PermissionOutcome::Deny { .. }),
        "a `git status` grant must never authorize `git push --force`"
    );
    assert_eq!(gate.call_count(), 1, "it must reach the operator");
}

/// Subtree inheritance rides the existing `AgentSubtree` scope: a grant
/// made by a parent covers a descendant, and does not leak sideways to an
/// unrelated agent.
#[tokio::test]
async fn a_subtree_pattern_grant_covers_descendants_but_not_strangers() {
    let gate = ScriptedGate::new(vec![PermissionDecision::Deny {
        reason: "nope".into(),
    }]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let root = AgentId::new();
    let child = AgentId::new();
    let stranger = AgentId::new();

    broker.remember_pattern(
        PatternRule::parse("bash:git status").expect("valid rule"),
        PermissionScope::AgentSubtree,
        root,
    );

    let descendant = ctx(child, vec![root, child], session);
    assert_eq!(
        broker.decide(&descendant, &bash_call("c1", "git status")).await,
        PermissionOutcome::Allow,
        "a descendant inherits the subtree grant"
    );

    let unrelated = ctx(stranger, vec![stranger], session);
    let outcome = broker
        .decide(&unrelated, &bash_call("c2", "git status"))
        .await;
    assert!(
        matches!(outcome, PermissionOutcome::Deny { .. }),
        "an agent outside the granting subtree must not inherit the grant"
    );
}

/// Plan mode denies mutating categories without consulting the gate, and
/// its denial cannot be overridden by a pattern grant — plan mode is
/// selected for a guarantee, so it behaves like one.
#[tokio::test]
async fn plan_mode_denies_execute_even_with_a_matching_pattern_grant() {
    let gate = ScriptedGate::new(vec![]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.remember_pattern(
        PatternRule::parse("bash:git status").expect("valid rule"),
        PermissionScope::Session,
        agent,
    );
    broker.set_mode(PermissionMode::Plan);

    let outcome = broker.decide(&c, &bash_call("c1", "git status")).await;
    assert!(
        matches!(outcome, PermissionOutcome::Deny { .. }),
        "plan mode must deny an Execute tool even when a pattern matches"
    );
    assert_eq!(
        gate.call_count(),
        0,
        "plan mode decides without troubling the operator"
    );
}

/// Plan mode still permits the non-mutating categories.
#[tokio::test]
async fn plan_mode_allows_a_read_without_prompting() {
    let gate = ScriptedGate::new(vec![PermissionDecision::AllowOnce]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.set_mode(PermissionMode::Plan);

    // `call()` is a Read-category read; plan mode lets it through to the
    // gate as normal (it does not auto-allow, it simply does not block).
    let outcome = broker.decide(&c, &call("c1")).await;
    assert_eq!(outcome, PermissionOutcome::Allow);
}

/// Auto-allow does what it says, and — crucially — can be switched back
/// out of mid-session. That is the escape hatch.
#[tokio::test]
async fn auto_allow_allows_without_prompting_and_can_be_revoked_mid_session() {
    let gate = ScriptedGate::new(vec![PermissionDecision::Deny {
        reason: "operator said no".into(),
    }]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.set_mode(PermissionMode::AutoAllow);
    assert_eq!(
        broker.decide(&c, &bash_call("c1", "rm -rf /tmp/x")).await,
        PermissionOutcome::Allow,
        "auto-allow allows"
    );
    assert_eq!(gate.call_count(), 0, "without asking");

    // The escape hatch: back to prompting, no restart.
    broker.set_mode(PermissionMode::Prompt);
    let outcome = broker.decide(&c, &bash_call("c2", "rm -rf /tmp/x")).await;
    assert!(
        matches!(outcome, PermissionOutcome::Deny { .. }),
        "switching back to Prompt must restore operator control"
    );
    assert_eq!(gate.call_count(), 1);
}

/// A cached `AllowAlways` grant earned under `Prompt` must not survive a
/// later switch to `Plan` mode for the exact same call: plan mode's denial
/// is checked ahead of every allow path, including the cache, so plan mode
/// keeps its guarantee even against a grant left over from earlier.
#[tokio::test]
async fn plan_mode_denies_a_call_even_when_allow_always_was_cached_under_prompt() {
    let gate = ScriptedGate::new(vec![PermissionDecision::AllowAlways {
        scope: PermissionScope::Session,
    }]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    // Under Prompt, the gate grants AllowAlways and the broker caches it.
    let granted = broker.decide(&c, &bash_call("c1", "rm -rf /tmp/x")).await;
    assert_eq!(granted, PermissionOutcome::Allow);
    assert_eq!(gate.call_count(), 1);

    // Switching to Plan must deny the byte-identical call despite the cache.
    broker.set_mode(PermissionMode::Plan);
    let outcome = broker.decide(&c, &bash_call("c2", "rm -rf /tmp/x")).await;
    assert!(
        matches!(outcome, PermissionOutcome::Deny { .. }),
        "plan mode must not be talked out of its denial by a cached AllowAlways"
    );
    assert_eq!(
        gate.call_count(),
        1,
        "plan mode decides without troubling the gate"
    );
}

/// Revocation clears pattern grants AND the `AllowAlways` cache, so a
/// previously-granted call asks again.
#[tokio::test]
async fn revoke_all_grants_restores_prompting_for_previously_granted_calls() {
    let gate = ScriptedGate::new(vec![PermissionDecision::Deny {
        reason: "nope".into(),
    }]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.remember_pattern(
        PatternRule::parse("bash:git status").expect("valid rule"),
        PermissionScope::Session,
        agent,
    );
    assert_eq!(
        broker.decide(&c, &bash_call("c1", "git status")).await,
        PermissionOutcome::Allow
    );
    assert_eq!(broker.active_patterns().len(), 1, "reviewable before revoke");

    broker.revoke_all_grants();
    assert!(
        broker.active_patterns().is_empty(),
        "revocation clears the review list"
    );

    let outcome = broker.decide(&c, &bash_call("c2", "git status")).await;
    assert!(
        matches!(outcome, PermissionOutcome::Deny { .. }),
        "a revoked grant must ask again"
    );
}
