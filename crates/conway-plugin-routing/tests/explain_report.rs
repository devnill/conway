//! Integration coverage for `RoutingExplain`/`ExplainReport` (WI-036, amended
//! for headroom): serde round-trip, explain/resolve agreement across >=12
//! scenarios, entry-count invariants, breaker/capability projection, the
//! never-calls-record invariant, headroom_tokens equality, the exact header
//! line, and the golden-file render.
//!
//! Reuses the same fixture patterns as `router_resolution.rs` (WI-034):
//! `FakeHealth` rather than `BreakerRegistry` + `TestClock` (see that file's
//! module doc for why), `CapabilityIndex::builder()`, and
//! `HeadroomPolicy::from_routing_config`.

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

use conway_plugin_routing::{CapabilityIndex, DeclarativeRouter, EntryOutcome, RoutingExplain};

// ---------------------------------------------------------------------
// Shared fixture helpers (mirrors tests/router_resolution.rs).
// ---------------------------------------------------------------------

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

fn index_with(entries: &[(ModelRef, Capabilities)]) -> CapabilityIndex {
    let mut builder = CapabilityIndex::builder();
    for (r, c) in entries {
        builder = builder.insert(r.backend.clone(), r.model.clone(), c.clone());
    }
    builder.build()
}

fn router_from(
    config: RoutingConfig,
    health: Arc<dyn HealthRegistry>,
    index: CapabilityIndex,
) -> DeclarativeRouter {
    let policy = HeadroomPolicy::from_routing_config(&config);
    DeclarativeRouter::new(config, policy, health, index).expect("valid config")
}

// ---------------------------------------------------------------------
// serde round-trip
// ---------------------------------------------------------------------

#[test]
fn explain_report_serde_round_trips() {
    let a = model_ref("anthropic", "claude-sonnet-4-6");
    let config = routing_config(vec![("planner", vec![a.clone()], None)], 4_096);
    let index = index_with(&[(a.clone(), caps(100_000))]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let report = RoutingExplain::new(&router).explain(&request("planner", 1_000));

    let json = serde_json::to_string(&report).expect("serialize");
    let back: conway_plugin_routing::ExplainReport =
        serde_json::from_str(&json).expect("deserialize");
    assert_eq!(report, back);
}

// ---------------------------------------------------------------------
// entries.len() invariants
// ---------------------------------------------------------------------

#[test]
fn entries_len_equals_chain_len_for_non_pinned_request() {
    let a = model_ref("anthropic", "claude-sonnet-4-6");
    let b = model_ref("ollama-cloud", "glm-5.2");
    let c = model_ref("local", "qwen3-coder-80b");
    let config = routing_config(
        vec![("planner", vec![a.clone(), b.clone(), c.clone()], None)],
        4_096,
    );
    let index = index_with(&[(a.clone(), caps(100_000)), (c.clone(), caps(100_000))]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let report = RoutingExplain::new(&router).explain(&request("planner", 1_000));
    assert_eq!(report.entries.len(), 3);
}

#[test]
fn entries_len_is_one_for_pinned_request() {
    let a = model_ref("anthropic", "claude-sonnet-4-6");
    let b = model_ref("ollama-cloud", "glm-5.2");
    let config = routing_config(vec![("planner", vec![a.clone(), b.clone()], None)], 4_096);
    let index = index_with(&[(a.clone(), caps(100_000)), (b.clone(), caps(100_000))]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let mut req = request("planner", 1_000);
    req.pin = Some(a);

    let report = RoutingExplain::new(&router).explain(&req);
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.entries[0].chain_position, None);
}

// ---------------------------------------------------------------------
// explain never calls HealthRegistry::record
// ---------------------------------------------------------------------

#[test]
fn explain_never_calls_health_record() {
    let a = model_ref("local", "qwen3-coder-80b");
    let config = routing_config(vec![("fast", vec![a.clone()], None)], 4_096);
    let health = Arc::new(FakeHealth::new());
    let index = index_with(&[(a.clone(), caps(100_000))]);
    let router = router_from(config, Arc::clone(&health) as _, index);

    let explainer = RoutingExplain::new(&router);
    for _ in 0..100 {
        let _ = explainer.explain(&request("fast", 1_000));
    }
    assert!(health.observations().is_empty());
}

// ---------------------------------------------------------------------
// headroom_tokens equality
// ---------------------------------------------------------------------

#[test]
fn headroom_tokens_matches_policy_resolve_for_overridden_role() {
    let a = model_ref("anthropic", "claude-sonnet-4-6");
    let config = routing_config(vec![("planner", vec![a.clone()], Some(16_384))], 4_096);
    let index = index_with(&[(a.clone(), caps(100_000))]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let report = RoutingExplain::new(&router).explain(&request("planner", 1_000));
    assert_eq!(report.headroom_tokens, 16_384);
}

#[test]
fn headroom_tokens_matches_policy_resolve_for_default_role() {
    let a = model_ref("local", "qwen3-coder-80b");
    let config = routing_config(vec![("fast", vec![a.clone()], None)], 8_192);
    let index = index_with(&[(a.clone(), caps(100_000))]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let report = RoutingExplain::new(&router).explain(&request("fast", 1_000));
    assert_eq!(report.headroom_tokens, 8_192);
}

// ---------------------------------------------------------------------
// capability / breaker projection
// ---------------------------------------------------------------------

#[test]
fn capability_summary_present_when_indexed_absent_when_not() {
    let known = model_ref("anthropic", "claude-sonnet-4-6");
    let unknown = model_ref("ollama-cloud", "glm-5.2");
    let config = routing_config(
        vec![("planner", vec![known.clone(), unknown.clone()], None)],
        4_096,
    );
    let index = index_with(&[(known.clone(), caps(100_000))]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let report = RoutingExplain::new(&router).explain(&request("planner", 1_000));
    assert_eq!(report.entries.len(), 2);
    let known_entry = &report.entries[0];
    assert!(known_entry.capabilities.is_some());
    assert_eq!(
        known_entry
            .capabilities
            .as_ref()
            .unwrap()
            .max_context_tokens,
        100_000
    );
    let unknown_entry = &report.entries[1];
    assert!(unknown_entry.capabilities.is_none());
}

#[test]
fn breaker_snapshot_reflects_health_state_at_explain_time() {
    let a = model_ref("anthropic", "claude-sonnet-4-6");
    let config = routing_config(vec![("planner", vec![a.clone()], None)], 4_096);
    let index = index_with(&[(a.clone(), caps(100_000))]);
    let health = Arc::new(FakeHealth::new());
    let open = BreakerState::Open {
        until: "2026-07-21T00:00:00Z".parse().unwrap(),
        kind: BreakerKind::Transport,
    };
    health.set_state(
        conway_core::ids::EndpointId::new(a.backend.as_str()),
        open.clone(),
    );
    let router = router_from(config, health, index);

    let report = RoutingExplain::new(&router).explain(&request("planner", 1_000));
    assert_eq!(report.entries[0].breaker.state, open);
}

// ---------------------------------------------------------------------
// render_text header line (exact)
// ---------------------------------------------------------------------

#[test]
fn render_text_header_line_is_exact() {
    let a = model_ref("anthropic", "claude-sonnet-4-6");
    let config = routing_config(vec![("planner", vec![a.clone()], Some(16_384))], 4_096);
    let index = index_with(&[(a.clone(), caps(100_000))]);
    let router = router_from(config, Arc::new(FakeHealth::new()), index);

    let report = RoutingExplain::new(&router).explain(&request("planner", 34_000));
    let first_line = report.render_text().lines().next().unwrap().to_string();
    assert_eq!(
        first_line,
        "role: planner  (est_tokens=34000, headroom_tokens=16384)"
    );
}

// ---------------------------------------------------------------------
// golden file
// ---------------------------------------------------------------------

/// The golden fixture: role "planner", est_tokens=34000, effective headroom
/// 16000 (role override). Position 0 is capability-eligible but
/// health-skipped (transport breaker open); position 1 is the amendment's
/// exact headroom-rejected candidate (`ollama-cloud/glm-5.2`, max_context
/// 40000); position 2 is selected as a fallback.
fn golden_router_and_request() -> (DeclarativeRouter, RouteRequest) {
    let a = model_ref("anthropic", "claude-sonnet-4-6");
    let b = model_ref("ollama-cloud", "glm-5.2");
    let c = model_ref("local", "qwen3-coder-80b");
    let config = routing_config(
        vec![(
            "planner",
            vec![a.clone(), b.clone(), c.clone()],
            Some(16_000),
        )],
        4_096,
    );
    let index = index_with(&[
        (a.clone(), caps(100_000)),
        (b.clone(), caps(40_000)),
        (c.clone(), caps(100_000)),
    ]);
    let health = Arc::new(FakeHealth::new());
    health.set_state(
        conway_core::ids::EndpointId::new(a.backend.as_str()),
        BreakerState::Open {
            until: "2026-07-20T10:00:30Z".parse().unwrap(),
            kind: BreakerKind::Transport,
        },
    );
    let router = router_from(config, health, index);
    (router, request("planner", 34_000))
}

#[test]
fn render_text_matches_golden_file_byte_for_byte() {
    let (router, req) = golden_router_and_request();
    let report = RoutingExplain::new(&router).explain(&req);
    let rendered = report.render_text();

    let golden_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/golden/explain_planner.txt"
    );
    let golden = std::fs::read_to_string(golden_path).expect("read golden file");
    assert_eq!(
        rendered, golden,
        "render_text output drifted from the golden file"
    );

    // The golden fixture must include a headroom-rejected candidate, per
    // the WI-036 amendment's criterion.
    assert!(rendered.contains("headroom"));
}

// ---------------------------------------------------------------------
// explain/resolve agreement, >=12 hand-built scenarios
// ---------------------------------------------------------------------

/// Asserts `explain` and `resolve` agree by construction: the `Selected`
/// entries' `(backend, model)` pairs and `RoutingReason`s, in order, equal
/// `resolve`'s `Route`s; when `resolve` returns `Err(NoCandidate)`, `explain`
/// has zero `Selected` entries and the same candidate order as
/// `considered`; and when `resolve` returns `Err(ContextTooLarge)` (T-1,
/// board item 01KYXNAHN64YMADZPQDQC0CPTJ), `explain` likewise has zero
/// `Selected` entries and its named model appears among the (necessarily
/// all-`Skipped`) entries.
fn assert_explain_agrees_with_resolve(router: &DeclarativeRouter, req: &RouteRequest) {
    let resolved = router.resolve(req);
    let report = RoutingExplain::new(router).explain(req);

    let selected: Vec<(ModelRef, RoutingReason)> = report
        .entries
        .iter()
        .filter_map(|e| match &e.outcome {
            EntryOutcome::Selected { reason } => Some((e.model_ref.clone(), reason.clone())),
            EntryOutcome::Skipped { .. } => None,
        })
        .collect();

    match resolved {
        Ok(routes) => {
            let expected: Vec<(ModelRef, RoutingReason)> = routes
                .iter()
                .map(|r| {
                    (
                        ModelRef {
                            backend: r.backend.clone(),
                            model: r.model.clone(),
                        },
                        r.reason.clone(),
                    )
                })
                .collect();
            assert_eq!(
                selected, expected,
                "selected entries must equal resolve's routes"
            );
        }
        Err(RoutingError::NoCandidate { considered, .. }) => {
            assert!(
                selected.is_empty(),
                "NoCandidate must imply zero Selected entries"
            );
            let considered_refs: Vec<ModelRef> =
                considered.iter().map(|(m, _)| m.clone()).collect();
            let entry_refs: Vec<ModelRef> =
                report.entries.iter().map(|e| e.model_ref.clone()).collect();
            assert_eq!(
                considered_refs, entry_refs,
                "skipped entries must appear in the same order as NoCandidate.considered"
            );
        }
        Err(RoutingError::ContextTooLarge { model, .. }) => {
            assert!(
                selected.is_empty(),
                "ContextTooLarge must imply zero Selected entries"
            );
            assert!(
                report
                    .entries
                    .iter()
                    .any(|e| e.model_ref == model
                        && matches!(e.outcome, EntryOutcome::Skipped { .. })),
                "ContextTooLarge's named model must appear as a Skipped explain entry"
            );
        }
        Err(other) => panic!("unexpected resolve error in agreement scenario: {other:?}"),
    }
}

#[test]
fn explain_and_resolve_agree_across_scenarios() {
    // 1. Pin hit.
    {
        let m = model_ref("anthropic", "claude-sonnet-4-6");
        let config = routing_config(vec![("planner", vec![m.clone()], None)], 4_096);
        let index = index_with(&[(m.clone(), caps(100_000))]);
        let router = router_from(config, Arc::new(FakeHealth::new()), index);
        let mut req = request("planner", 1_000);
        req.pin = Some(m);
        assert_explain_agrees_with_resolve(&router, &req);
    }

    // 2. Pin capability miss.
    {
        let m = model_ref("anthropic", "claude-sonnet-4-6");
        let config = routing_config(vec![("planner", vec![m.clone()], None)], 4_096);
        let index = index_with(&[]);
        let router = router_from(config, Arc::new(FakeHealth::new()), index);
        let mut req = request("planner", 1_000);
        req.pin = Some(m);
        assert_explain_agrees_with_resolve(&router, &req);
    }

    // 3. Pin health open.
    {
        let m = model_ref("anthropic", "claude-sonnet-4-6");
        let config = routing_config(vec![("planner", vec![m.clone()], None)], 4_096);
        let index = index_with(&[(m.clone(), caps(100_000))]);
        let health = Arc::new(FakeHealth::new());
        health.set_state(
            conway_core::ids::EndpointId::new(m.backend.as_str()),
            BreakerState::Open {
                until: "2026-07-21T00:00:00Z".parse().unwrap(),
                kind: BreakerKind::Transport,
            },
        );
        let router = router_from(config, health, index);
        let mut req = request("planner", 1_000);
        req.pin = Some(m);
        assert_explain_agrees_with_resolve(&router, &req);
    }

    // 4. All healthy chain preserves order.
    {
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
        assert_explain_agrees_with_resolve(&router, &request("planner", 1_000));
    }

    // 5. Head-skipped chain.
    {
        let a = model_ref("anthropic", "claude-sonnet-4-6");
        let b = model_ref("ollama-cloud", "glm-5.2");
        let c = model_ref("local", "qwen3-coder-80b");
        let config = routing_config(
            vec![("planner", vec![a.clone(), b.clone(), c.clone()], None)],
            4_096,
        );
        let index = index_with(&[(c.clone(), caps(100_000))]);
        let router = router_from(config, Arc::new(FakeHealth::new()), index);
        assert_explain_agrees_with_resolve(&router, &request("planner", 1_000));
    }

    // 6. All rejected -> NoCandidate.
    {
        let a = model_ref("anthropic", "claude-sonnet-4-6");
        let b = model_ref("ollama-cloud", "glm-5.2");
        let config = routing_config(vec![("planner", vec![a.clone(), b.clone()], None)], 4_096);
        let index = index_with(&[]);
        let router = router_from(config, Arc::new(FakeHealth::new()), index);
        assert_explain_agrees_with_resolve(&router, &request("planner", 1_000));
    }

    // 7. HalfOpen and Closed candidates both retained.
    {
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
        assert_explain_agrees_with_resolve(&router, &request("planner", 1_000));
    }

    // 8. Headroom-only rejection.
    {
        let m = model_ref("ollama-cloud", "glm-5.2");
        let config = routing_config(vec![("planner", vec![m.clone()], Some(16_000))], 4_096);
        let index = index_with(&[(m.clone(), caps(40_000))]);
        let router = router_from(config, Arc::new(FakeHealth::new()), index);
        assert_explain_agrees_with_resolve(&router, &request("planner", 34_000));
    }

    // 9. Headroom flips selection across chain positions.
    {
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
        assert_explain_agrees_with_resolve(&router, &request("planner", 34_000));
    }

    // 10. Per-role headroom override differentiation (overridden role).
    {
        let m = model_ref("ollama-cloud", "glm-5.2");
        let config = routing_config(
            vec![
                ("planner", vec![m.clone()], Some(16_384)),
                ("fast", vec![m.clone()], None),
            ],
            4_096,
        );
        let index = index_with(&[(m.clone(), caps(40_000))]);
        let router = router_from(config, Arc::new(FakeHealth::new()), index);
        assert_explain_agrees_with_resolve(&router, &request("planner", 34_000));
    }

    // 11. Per-role headroom override differentiation (default-inheriting role).
    {
        let m = model_ref("ollama-cloud", "glm-5.2");
        let config = routing_config(
            vec![
                ("planner", vec![m.clone()], Some(16_384)),
                ("fast", vec![m.clone()], None),
            ],
            4_096,
        );
        let index = index_with(&[(m.clone(), caps(40_000))]);
        let router = router_from(config, Arc::new(FakeHealth::new()), index);
        assert_explain_agrees_with_resolve(&router, &request("fast", 34_000));
    }

    // 12. All-rejected-by-headroom -> ContextTooLarge naming the largest
    // considered window.
    {
        let a = model_ref("anthropic", "claude-sonnet-4-6");
        let b = model_ref("local", "qwen3-coder-80b");
        let config = routing_config(
            vec![("planner", vec![a.clone(), b.clone()], Some(16_000))],
            4_096,
        );
        let index = index_with(&[(a.clone(), caps(40_000)), (b.clone(), caps(45_000))]);
        let router = router_from(config, Arc::new(FakeHealth::new()), index);
        assert_explain_agrees_with_resolve(&router, &request("planner", 34_000));
    }

    // 13. Pin rejected by headroom.
    {
        let m = model_ref("ollama-cloud", "glm-5.2");
        let config = routing_config(vec![("planner", vec![m.clone()], Some(16_000))], 4_096);
        let index = index_with(&[(m.clone(), caps(40_000))]);
        let router = router_from(config, Arc::new(FakeHealth::new()), index);
        let mut req = request("planner", 34_000);
        req.pin = Some(m);
        assert_explain_agrees_with_resolve(&router, &req);
    }

    // 14. Global-default headroom inheritance.
    {
        let m = model_ref("local", "qwen3-coder-80b");
        let config = routing_config(vec![("fast", vec![m.clone()], None)], 8_000);
        let index = index_with(&[(m.clone(), caps(40_000))]);
        let router = router_from(config, Arc::new(FakeHealth::new()), index);
        assert_explain_agrees_with_resolve(&router, &request("fast", 34_000));
    }
}

// ---------------------------------------------------------------------
// UnknownRole: explain is infallible even where resolve errors differently.
// ---------------------------------------------------------------------

#[test]
fn explain_on_unknown_role_returns_empty_report_without_panicking() {
    let config = routing_config(vec![], 4_096);
    let router = router_from(config, Arc::new(FakeHealth::new()), index_with(&[]));

    assert!(matches!(
        router.resolve(&request("ghost-role", 1_000)),
        Err(RoutingError::UnknownRole { .. })
    ));

    let report = RoutingExplain::new(&router).explain(&request("ghost-role", 1_000));
    assert!(report.entries.is_empty());
}
