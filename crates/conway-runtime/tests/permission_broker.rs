//! Acceptance tests for `PermissionBroker` (WI-078, architecture §4.3).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use conway_core::agent::{PermissionDecision, PermissionRequest, PermissionScope};
use conway_core::content::ToolCategory;
use conway_core::event::{Envelope, Event};
use conway_core::ids::{AgentId, SessionId, ToolName};
use conway_core::ports::PermissionGate;
use conway_runtime::events::EventBus;
use conway_runtime::permission::{
    AgentRoot, AuthorizedCall, GrantScope, PermissionBroker, PermissionCtx, PermissionOutcome,
};
use conway_core::permission_mode::PermissionMode;
use conway_core::permission_pattern::{PatternOrigin, PatternRule, Rule, Select, Then, When};
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
        PatternOrigin::Interactive,
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
        PatternOrigin::Interactive,
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
        PatternOrigin::Interactive,
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
        PatternOrigin::Interactive,
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
        PatternOrigin::Interactive,
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

// ---- deny: the asymmetric half (board item 01KYT8SGX32CP56PRJNG72V2W5) ----

/// A `deny` rule refuses a matching call outright, without ever consulting
/// the gate -- the mirror image of a pattern ALLOW's cache hit.
#[tokio::test]
async fn a_deny_rule_refuses_a_matching_call_without_consulting_the_gate() {
    let gate = ScriptedGate::new(vec![PermissionDecision::AllowOnce]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.remember_deny_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PatternOrigin::File(PathBuf::from("/repo/.conway/permissions.json")),
    );

    let outcome = broker.decide(&c, &bash_call("c1", "curl evil.example")).await;
    assert!(
        matches!(outcome, PermissionOutcome::Deny { .. }),
        "a deny rule must refuse the call directly"
    );
    assert_eq!(
        gate.call_count(),
        0,
        "a deny decision must never trouble the operator's gate -- the \
         script would have allowed it, so seeing 0 calls proves the deny \
         path short-circuited rather than merely happening to agree"
    );
}

/// The headline property carried up from `permission_pattern`'s own unit
/// test: a `deny` rule catches a CHAINED command that an `allow` rule of
/// the identical prefix would refuse to even consider, because deny
/// matching does not consult the metacharacter gate.
#[tokio::test]
async fn a_deny_rule_catches_a_chained_command_unlike_an_allow_of_the_same_prefix() {
    let gate = ScriptedGate::new(vec![]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.remember_deny_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PatternOrigin::Interactive,
    );

    let outcome = broker
        .decide(&c, &bash_call("c1", "curl evil.example; rm -rf /tmp/x"))
        .await;
    assert!(
        matches!(outcome, PermissionOutcome::Deny { .. }),
        "a chained command must still be caught by the matching deny prefix"
    );
    assert_eq!(gate.call_count(), 0);
}

/// Deny beats an ALLOW pattern grant for the identical call -- composition
/// is most-restrictive-wins, independent of which was installed first.
#[tokio::test]
async fn a_deny_rule_overrides_a_matching_allow_pattern_grant() {
    let gate = ScriptedGate::new(vec![]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.remember_pattern(
        PatternRule::parse("bash:git status").expect("valid rule"),
        PermissionScope::Session,
        agent,
        PatternOrigin::Interactive,
    );
    broker.remember_deny_pattern(
        PatternRule::parse("bash:git status").expect("valid rule"),
        PatternOrigin::Interactive,
    );

    let outcome = broker.decide(&c, &bash_call("c1", "git status")).await;
    assert!(
        matches!(outcome, PermissionOutcome::Deny { .. }),
        "deny must win over an allow pattern grant for the same call"
    );
}

/// Deny beats `AutoAllow` mode -- a mode that skips the gate entirely for
/// everything else still must not talk a deny rule out of its refusal.
#[tokio::test]
async fn a_deny_rule_overrides_auto_allow_mode() {
    let gate = ScriptedGate::new(vec![]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.set_mode(PermissionMode::AutoAllow);
    broker.remember_deny_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PatternOrigin::Interactive,
    );

    let outcome = broker.decide(&c, &bash_call("c1", "curl evil.example")).await;
    assert!(
        matches!(outcome, PermissionOutcome::Deny { .. }),
        "AutoAllow must not override a deny rule"
    );
    assert_eq!(gate.call_count(), 0);
}

/// `revoke_all_grants` clears ALLOW grants but deliberately leaves `deny`
/// rules in force -- see `PermissionBroker::revoke_all_grants`'s own doc
/// for why.
#[tokio::test]
async fn revoke_all_grants_does_not_clear_deny_rules() {
    let gate = ScriptedGate::new(vec![]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.remember_deny_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PatternOrigin::Interactive,
    );
    broker.revoke_all_grants();

    assert_eq!(
        broker.active_deny_patterns().len(),
        1,
        "revoke_all_grants must not touch deny rules"
    );
    let outcome = broker.decide(&c, &bash_call("c1", "curl evil.example")).await;
    assert!(matches!(outcome, PermissionOutcome::Deny { .. }));
}

// ---- sanitizer laundering (board item 01KYTMA306JH81R083Y8K9PWCR) ----

/// In `Prompt` mode, a deny rule that was defeated by laundering used to
/// silently DEGRADE TO A PROMPT: `deny_matches` missed it, so the call fell
/// through to the operator's gate exactly as if no deny rule existed at
/// all. Post-fix, the deny rule must catch it BEFORE the gate is ever
/// consulted -- a `deny` violation must never merely become "ask the
/// operator" instead of "refuse".
#[tokio::test]
async fn a_laundered_deny_match_refuses_under_prompt_mode_without_degrading_to_a_prompt() {
    // Scripted to ALLOW, so if the bug is present the outcome flips to
    // `Allow` and the assertion below catches it -- Prompt mode is the
    // broker's default, so this is not set explicitly.
    let gate = ScriptedGate::new(vec![PermissionDecision::AllowOnce]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.remember_deny_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PatternOrigin::Interactive,
    );

    // The sanitized shape of a leading tab, as the real `render_call` seam
    // actually produces it (`conway_runtime::tools::runner::sanitize_rendered`
    // -> `conway_core::text::sanitize_control_chars`). The fixture hand-writes
    // the post-sanitization shape (`SANITIZED_CONTROL_PLACEHOLDER` = `\u{FFFD}`);
    // the render-seam test exercises the genuine sanitizer end to end, so this
    // shape cannot drift from the real one.
    let outcome = broker
        .decide(&c, &bash_call("c1", "\u{FFFD}curl http://evil"))
        .await;

    assert!(
        matches!(outcome, PermissionOutcome::Deny { .. }),
        "a laundered command matching a deny rule must be refused, not \
         silently downgraded to a prompt the gate happens to allow"
    );
    assert_eq!(
        gate.call_count(),
        0,
        "the deny rule must short-circuit before the gate is ever \
         consulted -- reaching the gate at all is the bug"
    );
}

/// In `AutoAllow` mode, a deny rule is the LAST guardrail -- there is no
/// operator on the other end of a miss to catch it. A laundered command
/// that defeats the deny match must not be allowed.
#[tokio::test]
async fn a_laundered_deny_match_is_not_allowed_under_auto_allow() {
    // An empty script: the gate must never be consulted at all in
    // AutoAllow, so any call to it is itself a failure regardless of what
    // it would have answered.
    let gate = ScriptedGate::new(vec![]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.set_mode(PermissionMode::AutoAllow);
    broker.remember_deny_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PatternOrigin::Interactive,
    );

    let outcome = broker
        .decide(&c, &bash_call("c1", "\u{FFFD}curl http://evil"))
        .await;

    assert!(
        matches!(outcome, PermissionOutcome::Deny { .. }),
        "AutoAllow must not allow a command a deny rule was meant to \
         refuse just because laundering broke the naive prefix match"
    );
    assert_eq!(gate.call_count(), 0);
}

/// `active_patterns()` reports each grant's origin, so a project-loaded
/// rule is distinguishable from an interactively-approved one.
#[tokio::test]
async fn active_patterns_reports_origin() {
    let gate = ScriptedGate::new(vec![]);
    let (broker, _bus) = broker(gate.clone());
    let agent = AgentId::new();

    let file_origin = PatternOrigin::File(PathBuf::from("/repo/.conway/permissions.json"));
    broker.remember_pattern(
        PatternRule::parse("bash:cargo test").expect("valid rule"),
        PermissionScope::Session,
        agent,
        file_origin.clone(),
    );

    let patterns = broker.active_patterns();
    assert_eq!(patterns.len(), 1);
    assert_eq!(patterns[0].1, file_origin);
}

// ---- per-rule revocation (board item 01KYND4WGHSZXW5YQ6ZWHCDDNN) ----

/// The headline property: revoking ONE grant leaves a second, unrelated
/// grant fully intact -- the revoked pattern must ask again, the surviving
/// one must keep suppressing its prompt.
#[tokio::test]
async fn revoke_pattern_removes_one_grant_and_leaves_the_other_intact() {
    let gate = ScriptedGate::new(vec![PermissionDecision::Deny {
        reason: "nope".into(),
    }]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    let revoked_rule = PatternRule::parse("bash:git status").expect("valid rule");
    let kept_rule = PatternRule::parse("bash:cargo test").expect("valid rule");
    broker.remember_pattern(
        revoked_rule.clone(),
        PermissionScope::Session,
        agent,
        PatternOrigin::Interactive,
    );
    broker.remember_pattern(
        kept_rule.clone(),
        PermissionScope::Session,
        agent,
        PatternOrigin::Interactive,
    );
    assert_eq!(broker.active_patterns().len(), 2);

    let removed = broker.revoke_pattern(&revoked_rule, &PatternOrigin::Interactive);
    assert!(removed, "a matching grant must be reported as removed");

    let patterns = broker.active_patterns();
    assert_eq!(patterns.len(), 1, "exactly one grant survives");
    assert_eq!(patterns[0].0, kept_rule);

    let revoked_outcome = broker.decide(&c, &bash_call("c1", "git status")).await;
    assert!(
        matches!(revoked_outcome, PermissionOutcome::Deny { .. }),
        "the revoked pattern must ask again"
    );

    let kept_outcome = broker.decide(&c, &bash_call("c2", "cargo test")).await;
    assert!(
        matches!(kept_outcome, PermissionOutcome::Allow),
        "the surviving pattern must still suppress its prompt"
    );
}

/// `(rule, origin)` is the identity: two grants for the identical rule text
/// but different origins are addressed independently.
#[tokio::test]
async fn revoke_pattern_is_addressed_by_origin_too_not_just_the_rule_text() {
    let gate = ScriptedGate::new(vec![]);
    let (broker, _bus) = broker(gate.clone());
    let agent = AgentId::new();

    let rule = PatternRule::parse("bash:git status").expect("valid rule");
    let file_origin = PatternOrigin::File(PathBuf::from("/repo/.conway/permissions.json"));
    broker.remember_pattern(
        rule.clone(),
        PermissionScope::Session,
        agent,
        PatternOrigin::Interactive,
    );
    broker.remember_pattern(
        rule.clone(),
        PermissionScope::Session,
        agent,
        file_origin.clone(),
    );
    assert_eq!(broker.active_patterns().len(), 2);

    let removed = broker.revoke_pattern(&rule, &PatternOrigin::Interactive);
    assert!(removed);

    let patterns = broker.active_patterns();
    assert_eq!(patterns.len(), 1, "only the matching origin's grant is removed");
    assert_eq!(patterns[0].1, file_origin);
}

/// Revoking something not currently installed is reported, not panicked.
#[tokio::test]
async fn revoke_pattern_reports_false_when_nothing_matches() {
    let gate = ScriptedGate::new(vec![]);
    let (broker, _bus) = broker(gate.clone());

    let removed = broker.revoke_pattern(
        &PatternRule::parse("bash:git status").expect("valid rule"),
        &PatternOrigin::Interactive,
    );
    assert!(!removed);
}

/// `revoke_pattern` never touches `deny_patterns` -- there is no deny
/// counterpart at this layer (see `Conway::revoke_permission_pattern`'s own
/// doc for why the surface above this one never offers a deny row at all).
#[tokio::test]
async fn revoke_pattern_does_not_touch_deny_rules() {
    let gate = ScriptedGate::new(vec![]);
    let (broker, _bus) = broker(gate.clone());
    let agent = AgentId::new();

    let deny_rule = PatternRule::parse("bash:curl").expect("valid rule");
    broker.remember_deny_pattern(deny_rule.clone(), PatternOrigin::Interactive);
    broker.remember_pattern(
        PatternRule::parse("bash:git status").expect("valid rule"),
        PermissionScope::Session,
        agent,
        PatternOrigin::Interactive,
    );

    broker.revoke_pattern(
        &PatternRule::parse("bash:git status").expect("valid rule"),
        &PatternOrigin::Interactive,
    );

    assert_eq!(
        broker.active_deny_patterns().len(),
        1,
        "revoke_pattern must not clear deny rules"
    );
}

// ---- prompt: the second narrowing effect (board item 01KYTP1D3XWEZPW4AKPH54FNB3) ----
//
// Before this item, `must_reach_gate` was set EXCLUSIVELY by `check_root`,
// so a plugin-contributed `prompt` rule had nothing evaluating it anywhere
// in this broker -- it could never force `gate.check`, in any mode. Each
// test below installs ONLY a `prompt` rule (no `deny`, no root) against a
// scenario that would otherwise resolve WITHOUT ever consulting the gate,
// and proves the gate is reached anyway. A `ScriptedGate` with an EMPTY (or
// deliberately wrong) script is the proof technique used throughout this
// file for "must not be consulted"; here it is inverted to "must be
// consulted", so every test below scripts a decision and asserts the gate
// was actually called that many times -- zero calls would mean the rule
// stayed exactly as inert as it was before this item.

/// **Failure A from the item's own board record.** A `prompt` rule must
/// force the gate under `AutoAllow` -- the one mode with no operator
/// already in the loop to catch what the rule would have caught, and
/// therefore the mode a guardrail plugin matters most in.
#[tokio::test]
async fn a_prompt_rule_forces_the_gate_under_auto_allow() {
    let gate = ScriptedGate::new(vec![PermissionDecision::Deny {
        reason: "operator refused".into(),
    }]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.set_mode(PermissionMode::AutoAllow);
    broker.remember_prompt_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PatternOrigin::Interactive,
    );

    let outcome = broker.decide(&c, &bash_call("c1", "curl evil.example")).await;

    assert_eq!(
        gate.call_count(),
        1,
        "a matching prompt rule must force gate.check even under AutoAllow -- \
         before this item, AutoAllow's own branch would have returned Allow \
         with zero gate calls, exactly the failure this item exists to fix"
    );
    assert!(
        matches!(outcome, PermissionOutcome::Deny { .. }),
        "the outcome must be whatever the REAL gate decided, not an \
         AutoAllow auto-approval"
    );
}

/// **Failure B from the item's own board record.** A `prompt` rule must
/// force the gate even over a MATCHING pattern ALLOW grant -- without this,
/// `pattern_allows` resolves the call before a prompt rule is ever
/// consulted.
#[tokio::test]
async fn a_prompt_rule_forces_the_gate_over_a_matching_allow_pattern_grant() {
    let gate = ScriptedGate::new(vec![PermissionDecision::AllowOnce]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.remember_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PermissionScope::Session,
        agent,
        PatternOrigin::Interactive,
    );
    broker.remember_prompt_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PatternOrigin::Interactive,
    );

    // Prompt is the broker's default mode -- not set explicitly.
    let outcome = broker.decide(&c, &bash_call("c1", "curl evil.example")).await;

    assert_eq!(
        gate.call_count(),
        1,
        "the matching pattern grant would normally resolve this with zero \
         gate calls -- a prompt rule matching the identical call must force \
         gate.check anyway"
    );
    assert_eq!(outcome, PermissionOutcome::Allow);
}

/// A `prompt` rule installed AFTER an `AllowAlways` was cached still forces
/// the gate for a later, byte-identical call -- a plugin's `prompt` rule can
/// invalidate the operator's own earlier `AllowAlways`. Narrowing an
/// existing grant is always permitted; this is that principle applied to
/// the cache specifically, the sharpest form of the design question this
/// item's own record raises explicitly.
#[tokio::test]
async fn a_prompt_rule_installed_after_the_fact_forces_the_gate_over_a_cached_allow_always() {
    let gate = ScriptedGate::new(vec![
        PermissionDecision::AllowAlways {
            scope: PermissionScope::Session,
        },
        PermissionDecision::Deny {
            reason: "operator refused the second time".into(),
        },
    ]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    let first = broker.decide(&c, &bash_call("c1", "curl evil.example")).await;
    assert_eq!(first, PermissionOutcome::Allow);
    assert_eq!(gate.call_count(), 1, "the first call grants and caches AllowAlways");

    // Without a prompt rule, the second identical call would hit the cache
    // (see `allow_always_session_caches_second_identical_call`) with zero
    // further gate calls. Installing a prompt rule NOW, after the grant was
    // already cached, must still force the second call to the gate.
    broker.remember_prompt_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PatternOrigin::Interactive,
    );

    let second = broker.decide(&c, &bash_call("c2", "curl evil.example")).await;
    assert_eq!(
        gate.call_count(),
        2,
        "a prompt rule must force the gate even for a call an earlier \
         AllowAlways already cached -- the cache must never outrank a \
         narrowing rule installed after the fact"
    );
    assert!(matches!(second, PermissionOutcome::Deny { .. }));
}

/// A `prompt` rule must not be able to force a call PAST plan mode's own
/// denial -- plan mode's guarantee (checked before the prompt step) is
/// unaffected either way, but this pins that a prompt rule cannot somehow
/// widen what plan mode already refused.
#[tokio::test]
async fn a_prompt_rule_does_not_override_plan_modes_denial() {
    let gate = ScriptedGate::new(vec![]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.set_mode(PermissionMode::Plan);
    broker.remember_prompt_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PatternOrigin::Interactive,
    );

    let outcome = broker.decide(&c, &bash_call("c1", "curl evil.example")).await;
    assert!(
        matches!(outcome, PermissionOutcome::Deny { .. }),
        "plan mode must still deny an Execute-category tool even with a \
         matching prompt rule installed"
    );
    assert_eq!(gate.call_count(), 0, "plan mode decides without troubling the gate");
}

/// A `prompt` rule matching a Read-category call in PLAN mode still forces
/// the gate -- proving the step applies in plan mode too (for the
/// categories plan mode itself allows through), not only in Prompt/
/// AutoAllow.
#[tokio::test]
async fn a_prompt_rule_forces_the_gate_in_plan_mode_for_an_allowed_category() {
    let gate = ScriptedGate::new(vec![PermissionDecision::AllowOnce]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.remember_pattern(
        PatternRule::parse("read:*").expect("valid rule"),
        PermissionScope::Session,
        agent,
        PatternOrigin::Interactive,
    );
    broker.remember_prompt_pattern(
        PatternRule::parse("read:*").expect("valid rule"),
        PatternOrigin::Interactive,
    );
    broker.set_mode(PermissionMode::Plan);

    // `call()` is a Read-category call, which plan mode permits through to
    // the ordinary allow paths -- normally the `read:*` pattern grant above
    // would resolve it with zero gate calls.
    let outcome = broker.decide(&c, &call("c1")).await;

    assert_eq!(
        gate.call_count(),
        1,
        "a matching prompt rule must force the gate in plan mode too, for a \
         category plan mode itself would otherwise let through unprompted"
    );
    assert_eq!(outcome, PermissionOutcome::Allow);
}

/// Deny still beats prompt: when a call matches BOTH a `deny` rule and a
/// `prompt` rule, the outcome is a flat refusal, never merely an escalated
/// ask -- `deny_matches` returns before `prompt_matches` is ever consulted.
#[tokio::test]
async fn a_deny_rule_still_overrides_a_matching_prompt_rule() {
    let gate = ScriptedGate::new(vec![]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.set_mode(PermissionMode::AutoAllow);
    broker.remember_prompt_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PatternOrigin::Interactive,
    );
    broker.remember_deny_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PatternOrigin::Interactive,
    );

    let outcome = broker.decide(&c, &bash_call("c1", "curl evil.example")).await;
    assert!(
        matches!(outcome, PermissionOutcome::Deny { .. }),
        "a call matching both a deny rule and a prompt rule must be refused \
         outright, not merely escalated to an ask"
    );
    assert_eq!(
        gate.call_count(),
        0,
        "deny must short-circuit before the gate (and before the prompt \
         step) is ever reached"
    );
}

/// Registration order between a `prompt` rule and a matching pattern ALLOW
/// grant must not change the outcome -- prompt-then-allow and
/// allow-then-prompt both force the gate identically.
#[tokio::test]
async fn prompt_rule_registration_order_does_not_change_the_outcome() {
    for prompt_registered_first in [true, false] {
        let gate = ScriptedGate::new(vec![PermissionDecision::AllowOnce]);
        let (broker, _bus) = broker(gate.clone());
        let session = SessionId::new();
        let agent = AgentId::new();
        let c = ctx(agent, vec![agent], session);

        if prompt_registered_first {
            broker.remember_prompt_pattern(
                PatternRule::parse("bash:curl").expect("valid rule"),
                PatternOrigin::Interactive,
            );
            broker.remember_pattern(
                PatternRule::parse("bash:curl").expect("valid rule"),
                PermissionScope::Session,
                agent,
                PatternOrigin::Interactive,
            );
        } else {
            broker.remember_pattern(
                PatternRule::parse("bash:curl").expect("valid rule"),
                PermissionScope::Session,
                agent,
                PatternOrigin::Interactive,
            );
            broker.remember_prompt_pattern(
                PatternRule::parse("bash:curl").expect("valid rule"),
                PatternOrigin::Interactive,
            );
        }

        let outcome = broker.decide(&c, &bash_call("c1", "curl evil.example")).await;
        assert_eq!(outcome, PermissionOutcome::Allow);
        assert_eq!(
            gate.call_count(),
            1,
            "registration order (prompt_registered_first={prompt_registered_first}) \
             must not change whether the gate is reached"
        );
    }
}

/// `revoke_all_grants` clears ALLOW grants but deliberately leaves `prompt`
/// rules in force, mirroring `revoke_all_grants_does_not_clear_deny_rules`.
#[tokio::test]
async fn revoke_all_grants_does_not_clear_prompt_rules() {
    let gate = ScriptedGate::new(vec![PermissionDecision::AllowOnce]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    broker.remember_prompt_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PatternOrigin::Interactive,
    );
    broker.revoke_all_grants();

    assert_eq!(
        broker.active_prompt_patterns().len(),
        1,
        "revoke_all_grants must not touch prompt rules"
    );
    let outcome = broker.decide(&c, &bash_call("c1", "curl evil.example")).await;
    assert_eq!(
        gate.call_count(),
        1,
        "the surviving prompt rule must still force the gate after revocation"
    );
    assert_eq!(outcome, PermissionOutcome::Allow);
}

/// `active_prompt_patterns()` reports each rule's origin, mirroring
/// `active_patterns_reports_origin`.
#[tokio::test]
async fn active_prompt_patterns_reports_origin() {
    let gate = ScriptedGate::new(vec![]);
    let (broker, _bus) = broker(gate.clone());

    let file_origin = PatternOrigin::File(PathBuf::from("/repo/.conway/permissions.json"));
    broker.remember_prompt_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        file_origin.clone(),
    );

    let patterns = broker.active_prompt_patterns();
    assert_eq!(patterns.len(), 1);
    assert_eq!(patterns[0].1, file_origin);
}

// ---- structured-allow revocation by Rule identity (board item A2) ----
//
// `revoke_pattern` is keyed on the flat `to_pattern_rule()` projection,
// which collapses every structured allow rule (`paths_under`,
// `categories`, `category_in`, multi-tool) to `None` -- so before A2 a
// structured allow rule could never be revoked individually.
// `revoke_pattern_rule` keys on full `Rule` equality plus origin instead.

/// A structured allow rule (`Tools(["read","write"]) + Always`, a rule the
/// flat form cannot express) installed alongside a flat grant: revoking the
/// structured rule by `(Rule, origin)` removes exactly it -- the flat
/// sibling survives and still suppresses its prompt, and the structured
/// rule no longer matches.
#[tokio::test]
async fn revoke_pattern_rule_removes_only_the_structured_rule_addressed() {
    let gate = ScriptedGate::new(vec![
        // After the revoke, a call the structured rule used to cover must
        // reach the gate again.
        PermissionDecision::Deny {
            reason: "operator asked again".into(),
        },
    ]);
    let (broker, _bus) = broker(gate.clone());
    let session = SessionId::new();
    let agent = AgentId::new();
    let c = ctx(agent, vec![agent], session);

    let structured = Rule {
        select: Select::Tools(vec!["read".to_string(), "write".to_string()]),
        when: When::Always,
        then: Then::Allow,
    };
    let flat = PatternRule::parse("bash:git status").expect("valid rule");
    assert!(
        broker.remember_pattern_rule(
            structured.clone(),
            PermissionScope::Session,
            agent,
            PatternOrigin::Interactive,
            // B2: no `paths_under` on this rule, so the base is never
            // consulted -- a placeholder, not a resolution choice.
            Path::new("/"),
        ),
        "a structured allow rule with no paths_under installs"
    );
    broker.remember_pattern(
        flat.clone(),
        PermissionScope::Session,
        agent,
        PatternOrigin::Interactive,
    );
    assert_eq!(broker.active_structured_allow_rules().len(), 1);
    assert_eq!(broker.active_patterns().len(), 1);

    // Before the revoke: the structured rule authorizes a `read` call, and
    // the flat revoke cannot name the structured rule at all (its key is
    // the flat projection, which is None for this rule) -- the A2 gap.
    let before = broker.decide(&c, &call("c1")).await;
    assert_eq!(before, PermissionOutcome::Allow);
    assert!(
        !broker.revoke_pattern(&flat, &PatternOrigin::File(PathBuf::from("/elsewhere"))),
        "sanity: flat revoke against a non-matching origin removes nothing"
    );
    assert_eq!(
        broker.active_structured_allow_rules().len(),
        1,
        "the flat revoke's key cannot address a structured rule"
    );

    let removed =
        broker.revoke_pattern_rule(&structured, &PatternOrigin::Interactive, &GrantScope::Session);
    assert!(removed, "a matching structured rule must be reported removed");

    assert!(
        broker.active_structured_allow_rules().is_empty(),
        "the structured rule is gone"
    );
    assert_eq!(
        broker.active_patterns().len(),
        1,
        "the flat sibling grant survives"
    );

    // The observable outcome: a call the structured rule used to authorize
    // now reaches the gate; the flat sibling still suppresses its own.
    let after = broker.decide(&c, &call("c2")).await;
    assert!(
        matches!(after, PermissionOutcome::Deny { .. }),
        "the revoked structured rule must ask again"
    );
    let flat_outcome = broker.decide(&c, &bash_call("c3", "git status")).await;
    assert_eq!(
        flat_outcome,
        PermissionOutcome::Allow,
        "the surviving flat grant must still suppress its prompt"
    );
    assert_eq!(gate.call_count(), 1, "only the revoked rule's call re-asked");
}

/// `(Rule, origin)` is the identity: the identical rule text installed from
/// two origins is addressed independently, exactly as the flat path's
/// origin test pins.
#[tokio::test]
async fn revoke_pattern_rule_is_addressed_by_origin_too_not_just_the_rule() {
    let gate = ScriptedGate::new(vec![]);
    let (broker, _bus) = broker(gate.clone());
    let agent = AgentId::new();

    let rule = Rule {
        select: Select::Tools(vec!["read".to_string(), "write".to_string()]),
        when: When::Always,
        then: Then::Allow,
    };
    let file_origin = PatternOrigin::File(PathBuf::from("/repo/.conway/permissions.json"));
    broker.remember_pattern_rule(
        rule.clone(),
        PermissionScope::Session,
        agent,
        PatternOrigin::Interactive,
        Path::new("/"),
    );
    broker.remember_pattern_rule(
        rule.clone(),
        PermissionScope::Session,
        agent,
        file_origin.clone(),
        Path::new("/"),
    );
    assert_eq!(broker.active_structured_allow_rules().len(), 2);

    let removed =
        broker.revoke_pattern_rule(&rule, &PatternOrigin::Interactive, &GrantScope::Session);
    assert!(removed);

    let remaining = broker.active_structured_allow_rules();
    assert_eq!(
        remaining.len(),
        1,
        "only the matching origin's rule is removed"
    );
    assert_eq!(remaining[0].1, file_origin);
}

/// Revoking a structured rule that is not installed reports false (the
/// facade folds this into `RevokeOutcome::NotFound`), and a structured rule
/// that IS installed is not matched by a DIFFERENT rule's identity.
#[tokio::test]
async fn revoke_pattern_rule_reports_false_when_nothing_matches() {
    let gate = ScriptedGate::new(vec![]);
    let (broker, _bus) = broker(gate.clone());
    let agent = AgentId::new();

    let installed = Rule {
        select: Select::Tools(vec!["read".to_string(), "write".to_string()]),
        when: When::Always,
        then: Then::Allow,
    };
    broker.remember_pattern_rule(
        installed,
        PermissionScope::Session,
        agent,
        PatternOrigin::Interactive,
        Path::new("/"),
    );

    let never_installed = Rule {
        select: Select::Tools(vec!["bash".to_string()]),
        when: When::CategoryIn(vec![conway_core::content::ToolCategory::Execute]),
        then: Then::Allow,
    };
    assert!(
        !broker.revoke_pattern_rule(
            &never_installed,
            &PatternOrigin::Interactive,
            &GrantScope::Session
        ),
        "a rule that was never installed must not match"
    );
    assert_eq!(broker.active_structured_allow_rules().len(), 1);
}

/// The scope IS part of the revoke key (A2 review fix): two entries equal in
/// `(rule, origin)` but remembered at different scopes are addressed
/// independently -- revoking the agent-scoped row removes THAT instance and
/// leaves the session-scoped one installed, exactly the row the operator
/// pointed at. A scope-blind first-match could remove the session-scoped
/// instance instead.
#[tokio::test]
async fn revoke_pattern_rule_is_addressed_by_scope_too() {
    let gate = ScriptedGate::new(vec![]);
    let (broker, _bus) = broker(gate.clone());
    let agent = AgentId::new();

    let rule = Rule {
        select: Select::Tools(vec!["read".to_string(), "write".to_string()]),
        when: When::Always,
        then: Then::Allow,
    };
    broker.remember_pattern_rule(
        rule.clone(),
        PermissionScope::Session,
        agent,
        PatternOrigin::Interactive,
        Path::new("/"),
    );
    broker.remember_pattern_rule(
        rule.clone(),
        PermissionScope::Agent,
        agent,
        PatternOrigin::Interactive,
        Path::new("/"),
    );
    assert_eq!(broker.active_structured_allow_rules().len(), 2);

    // A scope-mismatched key matches nothing, even with rule and origin exact.
    assert!(
        !broker.revoke_pattern_rule(
            &rule,
            &PatternOrigin::Interactive,
            &GrantScope::Agent(AgentId::new()),
        ),
        "a different agent's scope must not match"
    );

    // Revoke the agent-scoped instance: exactly it goes, the session one stays.
    let removed = broker.revoke_pattern_rule(
        &rule,
        &PatternOrigin::Interactive,
        &GrantScope::Agent(agent),
    );
    assert!(removed);

    let remaining = broker.active_structured_allow_rules();
    assert_eq!(remaining.len(), 1, "only the agent-scoped instance is removed");
    assert_eq!(
        remaining[0].2,
        GrantScope::Session,
        "the session-scoped instance survives"
    );
}

/// `active_structured_allow_rules` surfaces each rule's grant scope (A2):
/// an allow rule is the one scoped rule kind, and a review surface that hid
/// the scope would misrepresent how much of the agent tree a grant covers.
#[tokio::test]
async fn active_structured_allow_rules_reports_origin_and_scope() {
    let gate = ScriptedGate::new(vec![]);
    let (broker, _bus) = broker(gate.clone());
    let agent = AgentId::new();

    let rule = Rule {
        select: Select::Tools(vec!["read".to_string(), "write".to_string()]),
        when: When::Always,
        then: Then::Allow,
    };
    let file_origin = PatternOrigin::File(PathBuf::from("/repo/.conway/permissions.json"));
    broker.remember_pattern_rule(
        rule.clone(),
        PermissionScope::Agent,
        agent,
        file_origin.clone(),
        Path::new("/"),
    );

    let rules = broker.active_structured_allow_rules();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].0, rule);
    assert_eq!(rules[0].1, file_origin);
    assert_eq!(
        rules[0].2,
        conway_runtime::permission::GrantScope::Agent(agent),
        "the scope the rule was remembered at rides along"
    );
}
