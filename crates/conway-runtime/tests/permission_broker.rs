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
    AuthorizedCall, PermissionBroker, PermissionCtx, PermissionOutcome,
};
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
    }
}

fn call(call_id: &str) -> AuthorizedCall {
    AuthorizedCall {
        call_id: call_id.into(),
        tool: ToolName::new("read"),
        category: ToolCategory::Read,
        arguments: serde_json::json!({"path": "a.txt"}),
        rendered: "read a.txt".into(),
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
