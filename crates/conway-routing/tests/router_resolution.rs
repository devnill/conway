//! Integration matrix for `DeclarativeRouter::resolve` (WI-034, amended for
//! the headroom gate): pin hit / pin capability-miss / pin health-open /
//! all-healthy chain / head-skipped chain / all-rejected / unknown role /
//! half-open retained, plus the headroom-specific scenarios the amendment
//! adds (headroom-only rejection, per-role override, global-default
//! inheritance, headroom flipping the outcome, pin rejected by headroom).
//!
//! Uses `conway_core::fakes::FakeHealth` rather than `BreakerRegistry` +
//! `TestClock`: the amendment's notes suggest the latter, but `TestClock` is
//! only public behind the `test-clock` feature (`#[cfg(any(test, feature =
//! "test-clock"))]` in `breaker.rs`), which this crate does not enable for
//! plain `cargo test -p conway-routing`. Breaker *transition* semantics are
//! already exhaustively covered by `breaker.rs`'s own unit tests (WI-033);
//! this file only needs to drive `HealthRegistry::state` to fixed
//! `Closed`/`Open`/`HalfOpen` values, which `FakeHealth::set_state` does
//! directly and deterministically. Flagged as a deviation from the
//! amendment's literal suggestion, not a gap in coverage.

use std::collections::BTreeMap;
use std::sync::Arc;

use conway_core::capabilities::{
    CacheMode, Capabilities, ReliabilityTier, RequiredCaps, StructuredOutput, ToolCallSupport,
};
use conway_core::error::RoutingError;
use conway_core::fakes::FakeHealth;
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias};
use conway_core::ports::{HealthRegistry, Router};
use conway_core::routing::{
    BreakerKind, BreakerState, HealthConfig, RouteRequest, RoutingConfig, RoutingReason,
};

use conway_routing::config::HeadroomPolicy;
use conway_routing::{CapabilityIndex, DeclarativeRouter};

fn model_ref(backend: &str, model: &str) -> ModelRef {
    ModelRef {
        backend: BackendId::new(backend),
        model: ModelId::new(model),
    }
}

fn caps(max_context_tokens: u32) -> Capabilities {
    Capabilities {
        tool_calling: ToolCallSupport::Streaming { validated: true },
        cache: CacheMode::None,
        parallel_tool_calls: true,
        structured_output: StructuredOutput::Grammar,
        max_context_tokens,
        reasoning: true,
        reliability_tier: ReliabilityTier::Verified,
    }
}

fn routing_config(
    roles: Vec<(&str, Vec<ModelRef>, Option<u32>)>,
    default_headroom_tokens: u32,
) -> RoutingConfig {
    let mut map = BTreeMap::new();
    for (name, chain, headroom_tokens) in roles {
        map.insert(
            name.to_string(),
            conway_core::routing::RoleConfig {
                chain,
                headroom_tokens,
                ..Default::default()
            },
        );
    }
    RoutingConfig {
        roles: map,
        health: HealthConfig::default(),
        default_headroom_tokens,
    }
}

fn request(role: &str, est_tokens: u32) -> RouteRequest {
    RouteRequest {
        role: RoleAlias::new(role),
        pin: None,
        required: RequiredCaps::default(),
        est_tokens,
        agent_id: AgentId::new(),
    }
}

fn open_state() -> BreakerState {
    BreakerState::Open {
        until: "2026-07-21T00:00:00Z".parse().unwrap(),
        kind: BreakerKind::Transport,
    }
}

fn build_router(
    config: RoutingConfig,
    policy: HeadroomPolicy,
    health: Arc<dyn HealthRegistry>,
    index: CapabilityIndex,
) -> DeclarativeRouter {
    DeclarativeRouter::new(config, policy, health, index).expect("valid config")
}

/// Derives the `HeadroomPolicy` from `config`'s own `default_headroom_tokens`
/// and each role's `RoleConfig::headroom_tokens`, so a fixture that sets a
/// per-role override on the `RoutingConfig` actually takes effect (passing
/// `HeadroomPolicy::default()` alongside such a fixture would silently
/// ignore the override -- `HeadroomPolicy` and `RoutingConfig` are separate
/// inputs to `DeclarativeRouter::new`).
fn router_from(
    config: RoutingConfig,
    health: Arc<dyn HealthRegistry>,
    index: CapabilityIndex,
) -> DeclarativeRouter {
    let policy = HeadroomPolicy::from_routing_config(&config);
    build_router(config, policy, health, index)
}

fn index_with(entries: &[(ModelRef, Capabilities)]) -> CapabilityIndex {
    let mut builder = CapabilityIndex::builder();
    for (r, c) in entries {
        builder = builder.insert(r.backend.clone(), r.model.clone(), c.clone());
    }
    builder.build()
}

// ---------------------------------------------------------------------
// Pin path
// ---------------------------------------------------------------------

#[test]
fn pin_hit_returns_single_route_with_pinned_by_api() {
    let m = model_ref("anthropic", "claude-sonnet-4-6");
    let config = routing_config(vec![("planner", vec![m.clone()], None)], 4_096);
    let index = index_with(&[(m.clone(), caps(100_000))]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let mut req = request("planner", 1_000);
    req.pin = Some(m.clone());

    let routes = router.resolve(&req).expect("pin should resolve");
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].backend, m.backend);
    assert_eq!(routes[0].model, m.model);
    assert_eq!(routes[0].reason, RoutingReason::PinnedByApi);
}

#[test]
fn pin_capability_miss_returns_no_candidate_with_single_entry() {
    let m = model_ref("anthropic", "claude-sonnet-4-6");
    let config = routing_config(vec![("planner", vec![m.clone()], None)], 4_096);
    // No capability entry at all for `m`.
    let index = index_with(&[]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let mut req = request("planner", 1_000);
    req.pin = Some(m.clone());

    let err = router.resolve(&req).unwrap_err();
    match err {
        RoutingError::NoCandidate { considered, .. } => {
            assert_eq!(considered.len(), 1);
            assert_eq!(considered[0].0, m);
            assert!(considered[0].1.starts_with("capability:"));
        }
        other => panic!("expected NoCandidate, got {other:?}"),
    }
}

#[test]
fn pin_health_open_returns_no_candidate_with_single_entry() {
    let m = model_ref("anthropic", "claude-sonnet-4-6");
    let config = routing_config(vec![("planner", vec![m.clone()], None)], 4_096);
    let index = index_with(&[(m.clone(), caps(100_000))]);
    let health = Arc::new(FakeHealth::new());
    health.set_state(
        conway_core::ids::EndpointId::new(m.backend.as_str()),
        open_state(),
    );
    let router = router_from(config, health, index);

    let mut req = request("planner", 1_000);
    req.pin = Some(m.clone());

    let err = router.resolve(&req).unwrap_err();
    match err {
        RoutingError::NoCandidate { considered, .. } => {
            assert_eq!(considered.len(), 1);
            assert!(considered[0].1.starts_with("health:"));
        }
        other => panic!("expected NoCandidate, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Chain order and health filtering
// ---------------------------------------------------------------------

#[test]
fn all_healthy_chain_preserves_order_with_primary_and_fallback_reasons() {
    let a = model_ref("anthropic", "claude-sonnet-4-6");
    let b = model_ref("ollama-cloud", "glm-5.2");
    let c = model_ref("local", "qwen3-coder-80b");
    let config = routing_config(
        vec![("planner", vec![a.clone(), b.clone(), c.clone()], None)],
        4_096,
    );
    let index = index_with(&[
        (a.clone(), caps(100_000)),
        (b.clone(), caps(100_000)),
        (c.clone(), caps(100_000)),
    ]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let routes = router.resolve(&request("planner", 1_000)).unwrap();
    assert_eq!(routes.len(), 3);
    assert_eq!(
        routes[0].reason,
        RoutingReason::AliasPrimary {
            alias: RoleAlias::new("planner")
        }
    );
    assert_eq!(
        routes[1].reason,
        RoutingReason::Fallback {
            position: 1,
            after: vec![]
        }
    );
    assert_eq!(
        routes[2].reason,
        RoutingReason::Fallback {
            position: 2,
            after: vec![]
        }
    );
    assert_eq!(routes[0].model, a.model);
    assert_eq!(routes[1].model, b.model);
    assert_eq!(routes[2].model, c.model);
}

#[test]
fn head_skipped_chain_survivor_keeps_its_chain_index_not_survivor_index() {
    let a = model_ref("anthropic", "claude-sonnet-4-6");
    let b = model_ref("ollama-cloud", "glm-5.2");
    let c = model_ref("local", "qwen3-coder-80b");
    let config = routing_config(
        vec![("planner", vec![a.clone(), b.clone(), c.clone()], None)],
        4_096,
    );
    let index = index_with(&[(c.clone(), caps(100_000))]); // a, b unknown to the index
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let routes = router.resolve(&request("planner", 1_000)).unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].model, c.model);
    assert_eq!(
        routes[0].reason,
        RoutingReason::Fallback {
            position: 2,
            after: vec![]
        },
        "survivor at chain position 2 must not be AliasPrimary"
    );
}

#[test]
fn all_rejected_no_candidate_enumerates_every_chain_entry_in_order() {
    let a = model_ref("anthropic", "claude-sonnet-4-6");
    let b = model_ref("ollama-cloud", "glm-5.2");
    let config = routing_config(vec![("planner", vec![a.clone(), b.clone()], None)], 4_096);
    let index = index_with(&[]); // neither known
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let err = router.resolve(&request("planner", 1_000)).unwrap_err();
    match err {
        RoutingError::NoCandidate { role, considered } => {
            assert_eq!(role, RoleAlias::new("planner"));
            assert_eq!(considered.len(), 2);
            assert_eq!(considered[0].0, a);
            assert_eq!(considered[1].0, b);
        }
        other => panic!("expected NoCandidate, got {other:?}"),
    }
}

#[test]
fn unknown_role_returns_unknown_role_error() {
    let config = routing_config(vec![], 4_096);
    let router = router_from(config, Arc::new(FakeHealth::new()), index_with(&[]));

    let err = router.resolve(&request("ghost-role", 1_000)).unwrap_err();
    assert!(matches!(
        err,
        RoutingError::UnknownRole { role } if role == RoleAlias::new("ghost-role")
    ));
}

#[test]
fn half_open_and_closed_candidates_are_retained() {
    let a = model_ref("anthropic", "claude-sonnet-4-6");
    let b = model_ref("local", "qwen3-coder-80b");
    let config = routing_config(vec![("planner", vec![a.clone(), b.clone()], None)], 4_096);
    let index = index_with(&[(a.clone(), caps(100_000)), (b.clone(), caps(100_000))]);
    let health = Arc::new(FakeHealth::new());
    health.set_state(
        conway_core::ids::EndpointId::new(a.backend.as_str()),
        BreakerState::HalfOpen,
    );
    let router = router_from(config, health, index);

    let routes = router.resolve(&request("planner", 1_000)).unwrap();
    assert_eq!(routes.len(), 2, "HalfOpen and Closed are both retained");
    assert_eq!(routes[0].model, a.model);
    assert_eq!(routes[1].model, b.model);
}

#[test]
fn transport_open_and_probe_open_report_the_expected_breaker_kind() {
    let a = model_ref("anthropic", "claude-sonnet-4-6");

    let transport_open = Arc::new(FakeHealth::new());
    transport_open.set_state(
        conway_core::ids::EndpointId::new(a.backend.as_str()),
        BreakerState::Open {
            until: "2026-07-21T00:00:00Z".parse().unwrap(),
            kind: BreakerKind::Transport,
        },
    );
    let router = router_from(
        routing_config(vec![("planner", vec![a.clone()], None)], 4_096),
        transport_open,
        index_with(&[(a.clone(), caps(100_000))]),
    );
    let err = router.resolve(&request("planner", 1_000)).unwrap_err();
    match err {
        RoutingError::NoCandidate { considered, .. } => {
            assert_eq!(considered[0].1, "health: Transport breaker open");
        }
        other => panic!("expected NoCandidate, got {other:?}"),
    }

    let probe_open = Arc::new(FakeHealth::new());
    probe_open.set_state(
        conway_core::ids::EndpointId::new(a.backend.as_str()),
        BreakerState::Open {
            until: "2026-07-21T00:00:00Z".parse().unwrap(),
            kind: BreakerKind::Probe,
        },
    );
    let router2 = router_from(
        routing_config(vec![("planner", vec![a.clone()], None)], 4_096),
        probe_open,
        index_with(&[(a.clone(), caps(100_000))]),
    );
    let err2 = router2.resolve(&request("planner", 1_000)).unwrap_err();
    match err2 {
        RoutingError::NoCandidate { considered, .. } => {
            assert_eq!(considered[0].1, "health: Probe breaker open");
        }
        other => panic!("expected NoCandidate, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Headroom (amendment)
// ---------------------------------------------------------------------

#[test]
fn headroom_skip_reports_capability_skip_with_shortfall() {
    let m = model_ref("ollama-cloud", "glm-5.2");
    let config = routing_config(vec![("planner", vec![m.clone()], Some(16_000))], 4_096);
    let index = index_with(&[(m.clone(), caps(40_000))]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let err = router.resolve(&request("planner", 34_000)).unwrap_err();
    match err {
        RoutingError::NoCandidate { considered, .. } => {
            assert_eq!(considered.len(), 1);
            assert_eq!(
                considered[0].1,
                "capability: context: needs 34000 input + 16000 headroom = 50000, \
                 model max_context_tokens is 40000"
            );
        }
        other => panic!("expected NoCandidate, got {other:?}"),
    }
}

#[test]
fn headroom_changes_the_outcome_not_just_the_message() {
    let m = model_ref("ollama-cloud", "glm-5.2");
    let index_of = || index_with(&[(m.clone(), caps(40_000))]);

    let zero_headroom_config = routing_config(vec![("planner", vec![m.clone()], Some(0))], 4_096);
    let router_zero = router_from(
        zero_headroom_config,
        Arc::new(FakeHealth::new()),
        index_of(),
    );
    assert!(router_zero.resolve(&request("planner", 34_000)).is_ok());

    let sixteen_k_config = routing_config(vec![("planner", vec![m.clone()], Some(16_000))], 4_096);
    let router_16k = router_from(sixteen_k_config, Arc::new(FakeHealth::new()), index_of());
    assert!(matches!(
        router_16k.resolve(&request("planner", 34_000)),
        Err(RoutingError::NoCandidate { .. })
    ));
}

#[test]
fn headroom_flips_selection_across_chain_positions() {
    // Position 0 has a small window (rejected by headroom); position 1 has
    // a large window (selected as Fallback{position: 1}).
    let small = model_ref("anthropic", "claude-haiku-4-5");
    let large = model_ref("local", "qwen3-coder-80b");
    let config = routing_config(
        vec![("planner", vec![small.clone(), large.clone()], Some(16_000))],
        4_096,
    );
    let index = index_with(&[
        (small.clone(), caps(40_000)),
        (large.clone(), caps(100_000)),
    ]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let routes = router.resolve(&request("planner", 34_000)).unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].model, large.model);
    assert_eq!(
        routes[0].reason,
        RoutingReason::Fallback {
            position: 1,
            after: vec![]
        }
    );
}

#[test]
fn per_role_headroom_override_is_honored_end_to_end() {
    let m = model_ref("ollama-cloud", "glm-5.2");
    let config = routing_config(
        vec![
            ("planner", vec![m.clone()], Some(16_384)),
            ("fast", vec![m.clone()], None), // inherits the 4096 default
        ],
        4_096,
    );
    let index = index_with(&[(m.clone(), caps(40_000))]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    // 40000 - 34000 = 6000 headroom of slack: planner's 16384 blows it,
    // fast's inherited 4096 default fits.
    assert!(matches!(
        router.resolve(&request("planner", 34_000)),
        Err(RoutingError::NoCandidate { .. })
    ));
    assert!(router.resolve(&request("fast", 34_000)).is_ok());
}

#[test]
fn all_rejected_by_headroom_no_candidate_lists_every_entry() {
    let a = model_ref("anthropic", "claude-sonnet-4-6");
    let b = model_ref("local", "qwen3-coder-80b");
    let config = routing_config(
        vec![("planner", vec![a.clone(), b.clone()], Some(16_000))],
        4_096,
    );
    let index = index_with(&[(a.clone(), caps(40_000)), (b.clone(), caps(45_000))]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let err = router.resolve(&request("planner", 34_000)).unwrap_err();
    match err {
        RoutingError::NoCandidate { considered, .. } => {
            assert_eq!(considered.len(), 2);
            for (_, reason) in &considered {
                assert!(reason.contains("headroom"));
            }
        }
        other => panic!("expected NoCandidate, got {other:?}"),
    }
}

#[test]
fn pin_uses_the_headroom_of_req_role_not_the_policy_default() {
    let m = model_ref("ollama-cloud", "glm-5.2");
    let config = routing_config(vec![("planner", vec![m.clone()], Some(16_000))], 4_096);
    let index = index_with(&[(m.clone(), caps(40_000))]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let mut req = request("planner", 34_000);
    req.pin = Some(m);

    // planner's overridden headroom (16000) rejects; the global policy
    // default (4096) would have accepted, proving the pin used the role's
    // headroom rather than the policy's bare default.
    assert!(matches!(
        router.resolve(&req),
        Err(RoutingError::NoCandidate { .. })
    ));
}

#[test]
fn global_default_headroom_applies_when_role_has_no_override() {
    let m = model_ref("local", "qwen3-coder-80b");
    let config = routing_config(vec![("fast", vec![m.clone()], None)], 8_000);
    let index = index_with(&[(m.clone(), caps(40_000))]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    // 34000 + 8000 = 42000 > 40000: rejected under the global default.
    assert!(matches!(
        router.resolve(&request("fast", 34_000)),
        Err(RoutingError::NoCandidate { .. })
    ));
}
