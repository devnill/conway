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
use conway_core::permission_pattern::{PatternOrigin, PatternRule};
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
    // actually produces it (`conway_runtime::tools::runner::
    // sanitize_rendered`) -- a hand-copy for the identical layering reason
    // `permission_pattern`'s own tests document.
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
