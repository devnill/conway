//! Integration matrix for `DeclarativeRouter::resolve` (WI-034, amended for
//! the headroom gate; further amended per decision 01KYXS3PTYVATWR58JR95AZJYN
//! / board item 01KYXNAHN64YMADZPQDQC0CPTJ to implement the T-1 port
//! contract literally): pin hit / pin capability-miss / pin health-open /
//! all-healthy chain / head-skipped chain / all-rejected / unknown role /
//! half-open retained, plus the headroom-specific scenarios the amendment
//! adds (headroom-only rejection, per-role override, global-default
//! inheritance, headroom flipping the outcome, pin rejected by headroom).
//!
//! **T-1 amendment (this file's own history):** the scenarios below whose
//! *every* candidate is rejected solely on headroom now assert
//! `RoutingError::ContextTooLarge`, not `RoutingError::NoCandidate` --
//! `headroom_skip_reports_capability_skip_with_shortfall` (renamed
//! `headroom_only_rejection_returns_context_too_large`) is the test this
//! item's spec named explicitly as pinning the old, superseded behavior.
//! Scenarios where at least one candidate fails for a *different* reason
//! (an unindexed model, a health skip, or a candidate that fails headroom
//! *and* something else) are deliberately left asserting `NoCandidate` --
//! see `mixed_headroom_and_capability_failure_stays_no_candidate` below for
//! that discrimination's own dedicated coverage.
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
    CacheMode, Capabilities, HeadroomPolicy, ReliabilityTier, RequiredCaps, StructuredOutput,
    ToolCallSupport,
};
use conway_core::error::RoutingError;
use conway_core::fakes::FakeHealth;
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias};
use conway_core::ports::{HealthRegistry, Router};
use conway_core::routing::{
    BreakerKind, BreakerState, HealthConfig, RouteRequest, RoutingConfig, RoutingReason,
};

use conway_plugin_routing::{CapabilityIndex, DeclarativeRouter};

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

/// The `Probe` breaker kind was retired (board item
/// `01KZ802GSF692EKYKQ2TTVCJB8`, "retire the health prober"); `Transport`
/// is now the only `BreakerKind` variant, so this only proves the one
/// surviving kind renders correctly.
#[test]
fn transport_open_reports_the_expected_breaker_kind() {
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
}

// ---------------------------------------------------------------------
// Headroom (amendment)
// ---------------------------------------------------------------------

#[test]
fn headroom_only_rejection_returns_context_too_large() {
    // Single candidate, rejected *solely* on headroom (nothing else about
    // it is wrong) -- T-1's port contract: `RoutingError::ContextTooLarge`,
    // naming est_tokens, resolved headroom, and this candidate's own window
    // (also the largest -- and only -- one considered).
    let m = model_ref("ollama-cloud", "glm-5.2");
    let config = routing_config(vec![("planner", vec![m.clone()], Some(16_000))], 4_096);
    let index = index_with(&[(m.clone(), caps(40_000))]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let err = router.resolve(&request("planner", 34_000)).unwrap_err();
    match err {
        RoutingError::ContextTooLarge {
            role,
            model,
            est_tokens,
            headroom_tokens,
            required_tokens,
            max_context_tokens,
            shortfall_tokens,
        } => {
            assert_eq!(role, RoleAlias::new("planner"));
            assert_eq!(model, m);
            assert_eq!(est_tokens, 34_000);
            assert_eq!(headroom_tokens, 16_000);
            assert_eq!(required_tokens, 50_000);
            assert_eq!(max_context_tokens, 40_000);
            assert_eq!(shortfall_tokens, 10_000);
        }
        other => panic!("expected ContextTooLarge, got {other:?}"),
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
        Err(RoutingError::ContextTooLarge { .. })
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
        Err(RoutingError::ContextTooLarge { .. })
    ));
    assert!(router.resolve(&request("fast", 34_000)).is_ok());
}

#[test]
fn all_rejected_by_headroom_reports_context_too_large_with_largest_window() {
    // Both candidates rejected solely on headroom -> ContextTooLarge, naming
    // the *largest* considered window (b's 45000, not a's 40000) as the
    // best case that still didn't fit.
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
        RoutingError::ContextTooLarge {
            model,
            max_context_tokens,
            required_tokens,
            shortfall_tokens,
            ..
        } => {
            assert_eq!(model, b, "the larger of the two rejected windows");
            assert_eq!(max_context_tokens, 45_000);
            assert_eq!(required_tokens, 50_000);
            assert_eq!(shortfall_tokens, 5_000);
        }
        other => panic!("expected ContextTooLarge, got {other:?}"),
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
        Err(RoutingError::ContextTooLarge { .. })
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
        Err(RoutingError::ContextTooLarge { .. })
    ));
}

// ---------------------------------------------------------------------
// Mixed failure discrimination (this item's own addition): a candidate
// rejected on headroom *and* something else is not attributable to context
// size alone, so it must not turn the request into `ContextTooLarge`.
// ---------------------------------------------------------------------

/// Weak capabilities: fails `tool_calling` outright, independent of context.
fn weak_caps(max_context_tokens: u32) -> Capabilities {
    Capabilities {
        tool_calling: ToolCallSupport::None,
        cache: CacheMode::None,
        parallel_tool_calls: true,
        structured_output: StructuredOutput::Grammar,
        max_context_tokens,
        reasoning: true,
        reliability_tier: ReliabilityTier::Verified,
    }
}

#[test]
fn single_candidate_failing_headroom_and_capability_stays_no_candidate() {
    // One candidate, but it fails BOTH the headroom gate and a required
    // capability -- a mixed failure for that single candidate. Per this
    // item's spec, a mixed failure is not a `ContextTooLarge`: the missing
    // list has more than one entry, so `check_candidate` must not report it
    // as headroom-only.
    let m = model_ref("ollama-cloud", "glm-5.2");
    let config = routing_config(vec![("planner", vec![m.clone()], Some(16_000))], 4_096);
    let index = index_with(&[(m.clone(), weak_caps(40_000))]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let mut req = request("planner", 34_000);
    req.required.tool_calling = Some(ToolCallSupport::NonStreamingOnly);

    let err = router.resolve(&req).unwrap_err();
    match err {
        RoutingError::NoCandidate { considered, .. } => {
            assert_eq!(considered.len(), 1);
            assert!(considered[0].1.contains("tool_calling"));
            assert!(considered[0].1.contains("context"));
        }
        other => panic!("expected NoCandidate (mixed failure), got {other:?}"),
    }
}

#[test]
fn one_headroom_only_candidate_plus_one_capability_only_candidate_stays_no_candidate() {
    // `a` fails solely on headroom; `b` fails solely on a missing
    // capability (its window is plenty large). Not every rejection in the
    // chain reduces to context size, so the aggregate must stay
    // `NoCandidate`, not `ContextTooLarge` -- even though `a` alone would
    // have qualified.
    let a = model_ref("anthropic", "claude-sonnet-4-6");
    let b = model_ref("local", "qwen3-coder-80b");
    let config = routing_config(
        vec![("planner", vec![a.clone(), b.clone()], Some(16_000))],
        4_096,
    );
    let index = index_with(&[(a.clone(), caps(40_000)), (b.clone(), weak_caps(200_000))]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let mut req = request("planner", 34_000);
    req.required.tool_calling = Some(ToolCallSupport::NonStreamingOnly);

    let err = router.resolve(&req).unwrap_err();
    match err {
        RoutingError::NoCandidate { considered, .. } => {
            assert_eq!(considered.len(), 2);
            assert!(considered[0].1.contains("context"));
            assert!(considered[1].1.contains("tool_calling"));
        }
        other => panic!("expected NoCandidate (mixed across chain), got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Role-configured capability floor (`RoleConfig::required`): the router
// must actually enforce a role's declared floor, not just carry it as an
// unread config field -- `routing_config`'s helper above deliberately
// never sets `required` (it always defaults to `RequiredCaps::default()`
// via `..Default::default()`), so these two tests build `RoutingConfig` by
// hand to set it explicitly.
// ---------------------------------------------------------------------

/// A role's configured `required.min_reliability` floor rejects a candidate
/// that fails to meet it, even though the request itself
/// (`request("coder", ..)`, unmodified) asks for nothing -- proving the
/// floor is read from `RoutingConfig`, not merely carried by it. Deliberate
/// choice of `min_reliability` as the probed field: it is untouched by
/// `conway-runtime`'s own `RouteRequest.required` construction (only
/// `tool_calling` gets a conditional runtime-side bump), so a pass here
/// cannot be explained by anything but this role's own configured floor.
#[test]
fn role_configured_capability_floor_rejects_a_candidate_that_does_not_meet_it() {
    let m = model_ref("local", "qwen3-coder-80b");
    let mut roles = BTreeMap::new();
    roles.insert(
        "coder".to_string(),
        conway_core::routing::RoleConfig {
            chain: vec![m.clone()],
            required: RequiredCaps {
                min_reliability: Some(ReliabilityTier::Verified),
                ..RequiredCaps::default()
            },
            ..Default::default()
        },
    );
    let config = RoutingConfig {
        roles,
        health: HealthConfig::default(),
        default_headroom_tokens: 4_096,
    };
    let community_tier = Capabilities {
        reliability_tier: ReliabilityTier::Community,
        ..caps(200_000)
    };
    let index = index_with(&[(m.clone(), community_tier)]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let err = router
        .resolve(&request("coder", 100))
        .expect_err("Community tier must not satisfy a configured Verified floor");
    match err {
        RoutingError::NoCandidate { considered, .. } => {
            assert_eq!(considered.len(), 1);
            assert!(
                considered[0].1.contains("reliability_tier"),
                "got: {}",
                considered[0].1
            );
        }
        other => panic!("expected NoCandidate, got {other:?}"),
    }
}

/// The complement (GP-14: "any check that cannot fail is not a check"):
/// identical fixture, the candidate's tier raised to meet the configured
/// floor -- resolution now succeeds.
#[test]
fn role_configured_capability_floor_admits_a_candidate_that_meets_it() {
    let m = model_ref("local", "qwen3-coder-80b");
    let mut roles = BTreeMap::new();
    roles.insert(
        "coder".to_string(),
        conway_core::routing::RoleConfig {
            chain: vec![m.clone()],
            required: RequiredCaps {
                min_reliability: Some(ReliabilityTier::Verified),
                ..RequiredCaps::default()
            },
            ..Default::default()
        },
    );
    let config = RoutingConfig {
        roles,
        health: HealthConfig::default(),
        default_headroom_tokens: 4_096,
    };
    let index = index_with(&[(m.clone(), caps(200_000))]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let routes = router
        .resolve(&request("coder", 100))
        .expect("Verified tier meets a configured Verified floor");
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].model, m.model);
}

/// Cross-role isolation (MINOR 5): every fixture above configures exactly
/// ONE role's floor, so none of them can distinguish "the floor applies to
/// the role that declared it" from "the floor leaked into every role's
/// admission check." This fixture configures TWO roles pointing at the
/// SAME candidate model -- `coder`'s floor (`min_reliability: Verified`)
/// and `planner`, left with no floor at all -- and resolves both against
/// the identical Community-tier candidate: `coder` must reject it,
/// `planner` must admit the very same candidate, proving the floor is
/// scoped per-role, not applied globally.
#[test]
fn role_configured_capability_floor_does_not_reach_an_unfloored_sibling_role() {
    let m = model_ref("local", "qwen3-coder-80b");
    let mut roles = BTreeMap::new();
    roles.insert(
        "coder".to_string(),
        conway_core::routing::RoleConfig {
            chain: vec![m.clone()],
            required: RequiredCaps {
                min_reliability: Some(ReliabilityTier::Verified),
                ..RequiredCaps::default()
            },
            ..Default::default()
        },
    );
    roles.insert(
        "planner".to_string(),
        conway_core::routing::RoleConfig {
            chain: vec![m.clone()],
            ..Default::default()
        },
    );
    let config = RoutingConfig {
        roles,
        health: HealthConfig::default(),
        default_headroom_tokens: 4_096,
    };
    let community_tier = Capabilities {
        reliability_tier: ReliabilityTier::Community,
        ..caps(200_000)
    };
    let index = index_with(&[(m.clone(), community_tier)]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    router
        .resolve(&request("coder", 100))
        .expect_err("coder's configured Verified floor must reject a Community-tier candidate");

    let routes = router
        .resolve(&request("planner", 100))
        .expect("planner has no configured floor, so the identical candidate must be admitted");
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].model, m.model);
}

// ---------------------------------------------------------------------
// `capability::strictest`'s pointwise merge (config floor vs. caller
// requirement) is only exercised above with one side always at
// `RequiredCaps::default()` -- every `request(..)` helper call hardcodes
// `required: RequiredCaps::default()`, and the pair above only ever sets
// the ROLE's floor, never the caller's own `RouteRequest.required`. A
// `strictest` that silently returned one side unconditionally would pass
// every test above it in this file. The three tests below set DIFFERENT
// fields on each side and assert the merge keeps both, observed as
// admit/reject outcomes (GP-14: assert the observable outcome, not the
// merged struct).
//
// Deliberately NOT `min_reliability`/`tool_calling` (the fields the
// finding's own illustrative scenario named): those two fields are each
// already independently exercised elsewhere in this file (config-only via
// `role_configured_capability_floor_*` above, request-only via the mixed
// headroom-and-capability tests below), so a mutation broad enough to drop
// a whole side would incidentally trip one of THOSE tests too, muddying
// the contrast this guard is meant to demonstrate. `structured_output`
// (config floor) and `parallel_tool_calls` (caller requirement) probe the
// identical merge machinery (`strictest_by_rank` / `strictest_bool`) but
// are untouched by any other test in this file, so these three tests are
// the only ones in the suite that can fail when the merge drops a side.
// ---------------------------------------------------------------------

/// Shared fixture for the three `strictest` merge tests: role "coder"'s
/// configured floor sets `structured_output: JsonSchema` (and nothing
/// else); callers separately set `required.parallel_tool_calls: true` on
/// their own `RouteRequest` (and nothing else) via `merge_test_request`.
fn merge_test_config(m: &ModelRef) -> RoutingConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "coder".to_string(),
        conway_core::routing::RoleConfig {
            chain: vec![m.clone()],
            required: RequiredCaps {
                structured_output: Some(StructuredOutput::JsonSchema),
                ..RequiredCaps::default()
            },
            ..Default::default()
        },
    );
    RoutingConfig {
        roles,
        health: HealthConfig::default(),
        default_headroom_tokens: 4_096,
    }
}

fn merge_test_request() -> RouteRequest {
    let mut req = request("coder", 100);
    req.required.parallel_tool_calls = Some(true);
    req
}

/// Meets the role's configured floor (`structured_output: JsonSchema`) but
/// not the caller's own requirement (`parallel_tool_calls: false`, caller
/// demands `true`). If `strictest` silently dropped the caller's side
/// (e.g. returned only `config_floor`), this candidate would be wrongly
/// admitted -- the rejection reason must name `parallel_tool_calls` and
/// must NOT name `structured_output` (the role floor was met).
#[test]
fn strictest_merge_still_enforces_the_callers_requirement_alongside_the_role_floor() {
    let m = model_ref("local", "qwen3-coder-80b");
    let config = merge_test_config(&m);
    let structured_but_no_parallel = Capabilities {
        structured_output: StructuredOutput::JsonSchema,
        parallel_tool_calls: false,
        ..caps(200_000)
    };
    let index = index_with(&[(m.clone(), structured_but_no_parallel)]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let err = router
        .resolve(&merge_test_request())
        .expect_err("meets the role's floor but not the caller's own requirement");
    match err {
        RoutingError::NoCandidate { considered, .. } => {
            assert_eq!(considered.len(), 1);
            assert!(
                considered[0].1.contains("parallel_tool_calls"),
                "rejection must be attributed to the caller's requirement, got: {}",
                considered[0].1
            );
            assert!(
                !considered[0].1.contains("structured_output"),
                "the role floor was met and must not appear as a failure reason, got: {}",
                considered[0].1
            );
        }
        other => panic!("expected NoCandidate, got {other:?}"),
    }
}

/// Symmetric to the test above: meets the caller's own requirement
/// (`parallel_tool_calls: true`) but not the role's configured floor
/// (`structured_output: JsonSchema`, candidate offers `None`). If
/// `strictest` silently dropped the config side (e.g. returned only
/// `request`), this candidate would be wrongly admitted -- the rejection
/// reason must name `structured_output` and must NOT name
/// `parallel_tool_calls` (the caller's requirement was met).
#[test]
fn strictest_merge_still_enforces_the_role_floor_alongside_the_callers_requirement() {
    let m = model_ref("local", "qwen3-coder-80b");
    let config = merge_test_config(&m);
    let parallel_but_no_structured = Capabilities {
        structured_output: StructuredOutput::None,
        parallel_tool_calls: true,
        ..caps(200_000)
    };
    let index = index_with(&[(m.clone(), parallel_but_no_structured)]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let err = router
        .resolve(&merge_test_request())
        .expect_err("meets the caller's requirement but not the role's own floor");
    match err {
        RoutingError::NoCandidate { considered, .. } => {
            assert_eq!(considered.len(), 1);
            assert!(
                considered[0].1.contains("structured_output"),
                "rejection must be attributed to the role's floor, got: {}",
                considered[0].1
            );
            assert!(
                !considered[0].1.contains("parallel_tool_calls"),
                "the caller's requirement was met and must not appear as a failure reason, got: {}",
                considered[0].1
            );
        }
        other => panic!("expected NoCandidate, got {other:?}"),
    }
}

/// The complement (GP-14: "any check that cannot fail is not a check"): a
/// candidate meeting BOTH the role's floor and the caller's requirement is
/// admitted -- proving the merge is not just "reject unless both fail" but
/// genuinely pointwise-strictest across both sides.
#[test]
fn strictest_merge_admits_a_candidate_meeting_both_the_role_floor_and_the_callers_requirement() {
    let m = model_ref("local", "qwen3-coder-80b");
    let config = merge_test_config(&m);
    let meets_both = Capabilities {
        structured_output: StructuredOutput::JsonSchema,
        parallel_tool_calls: true,
        ..caps(200_000)
    };
    let index = index_with(&[(m.clone(), meets_both)]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let routes = router
        .resolve(&merge_test_request())
        .expect("meets both the role's floor and the caller's own requirement");
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].model, m.model);
}
