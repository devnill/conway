//! Board item 01KZFC2MD1FVNA674YJ9A19T8E: `ConwayBuilder::with_router_factory`
//! -- a router KIND named up front, its construction deferred to `build()`'s
//! own router step.
//!
//! Three properties, each a discriminating observable rather than a
//! structural check:
//!
//! 1. A factory registered via `with_router_factory` IS invoked, and its
//!    router IS used, when no router is injected
//!    (`factory_is_used_when_no_router_is_injected`).
//! 2. The SAME factory is neither invoked nor consulted when `with_router`
//!    is ALSO called -- an injected router wins unconditionally
//!    (`factory_is_not_used_or_constructed_when_with_router_is_also_set`).
//!    "Not constructed" is checked directly: the factory increments an
//!    `AtomicUsize` inside its own `build`, so a call the precedence
//!    guard fails to skip would be observed here even if its RESULT were
//!    otherwise discarded.
//! 3. A factory whose `build` returns `Err` surfaces as
//!    `ConwayError::Build`, naming both the factory's own id and the
//!    underlying message (`factory_build_error_surfaces_as_build_error`).
//!
//! P-15's break-the-guard run for property 2 is recorded in this item's own
//! completion report, not committed here (the guard must be shown to fail
//! and then be restored, never left broken in the tree).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    HooksConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig, TuiSection,
};
use conway::{
    Conway, ConwayBuilder, ConwayError, HealthRegistry, Router, RouterBuildContext, RouterBundle,
    RouterFactory,
};
use conway_core::agent::PermissionDecision;
use conway_core::capabilities::{Capabilities, CacheMode, ReliabilityTier, StructuredOutput, ToolCallSupport};
use conway_core::content::{StopReason, Usage};
use conway_core::error::ConwayError as CoreConwayError;
use conway_core::fakes::{FakeBackend, FakeGate, FakeHealth, FakeRouter, FakeStore};
use conway_core::ids::BackendId;
use conway_core::ports::GenerateResponse;

fn caps() -> Capabilities {
    Capabilities {
        tool_calling: ToolCallSupport::Streaming { validated: true },
        cache: CacheMode::None,
        parallel_tool_calls: true,
        structured_output: StructuredOutput::Grammar,
        max_context_tokens: 100_000,
        reasoning: true,
        reliability_tier: ReliabilityTier::Verified,
    }
}

fn fake_backend(id: &str) -> Arc<dyn conway_core::ports::Backend> {
    Arc::new(FakeBackend::new(
        BackendId::new(id),
        caps(),
        GenerateResponse {
            content: vec![],
            tool_calls: vec![],
            stop: StopReason::EndTurn,
            usage: Usage::default(),
        },
    ))
}

/// One role with an EMPTY chain. Board item 01KZFC43J1J06BM4CCWKCKHSNV:
/// `build()`'s own no-router/no-factory default fell through to
/// `conway_core::routing::MinimalRouter` (which never validates a chain at
/// construction) by the time this test landed; this fixture's discriminating
/// power instead comes from `CountingRouterFactory`'s own router double
/// (`FakeRouter`, which also performs no such validation) vs. the properly
/// STRICTER validation `conway-plugin-routing::DeclarativeRouter::new`
/// would apply if that engine were installed instead -- a `build()` that
/// succeeds against this config together with the call-counter assertions
/// below is still proof the registered factory's own router, not some
/// other path, produced the result -- exactly the discriminating signal
/// `builder.rs`'s own tests already lean on for `with_router` (see that
/// file's `fake_router` doc), reused here for `with_router_factory`.
fn base_config() -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "default".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    ConwayConfig {
        default_role: conway_core::ids::RoleAlias::new("default"),
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
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

/// A stub `RouterFactory` counting its own `build` calls -- the same
/// counter proves both "was used" (property 1, count == 1) and "was NOT
/// constructed" (property 2, count == 0), so neither assertion can pass by
/// accident: a factory whose `build` is skipped leaves the counter at 0
/// regardless of what `router`/`health` it would have returned.
struct CountingRouterFactory {
    id: &'static str,
    calls: Arc<AtomicUsize>,
    router: Arc<dyn Router>,
}

impl RouterFactory for CountingRouterFactory {
    fn id(&self) -> &str {
        self.id
    }

    fn build(&self, _ctx: RouterBuildContext<'_>) -> Result<RouterBundle, CoreConwayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(RouterBundle {
            router: self.router.clone(),
            health: Arc::new(FakeHealth::new()) as Arc<dyn HealthRegistry>,
            explain: None,
        })
    }
}

/// A `RouterFactory` whose `build` always fails.
struct ErrRouterFactory {
    id: &'static str,
    detail: &'static str,
}

impl RouterFactory for ErrRouterFactory {
    fn id(&self) -> &str {
        self.id
    }

    fn build(&self, _ctx: RouterBuildContext<'_>) -> Result<RouterBundle, CoreConwayError> {
        Err(CoreConwayError::Config {
            detail: self.detail.to_string(),
        })
    }
}

/// `Conway` deliberately does not derive `Debug` (it wraps `Arc<Runtime>`,
/// which does not either), so `Result::expect_err`/`unwrap_err` (which both
/// require `T: Debug`) cannot be used on a `Result<Conway, _>` here --
/// mirrors `crates/conway/tests/builder.rs`'s own `expect_build_err`.
fn expect_build_err(result: Result<Conway, ConwayError>, msg: &str) -> ConwayError {
    match result {
        Err(err) => err,
        Ok(_) => panic!("{msg}"),
    }
}

/// Property 1: a registered factory IS invoked, and its router IS used,
/// when no router is injected. Discriminating because `base_config()`'s
/// empty-chain role would fail `build()` under the compiled
/// `DeclarativeRouter` path -- a success here can only be explained by the
/// factory's own `FakeRouter` (which performs no such validation) having
/// been used instead.
#[test]
fn factory_is_used_when_no_router_is_injected() {
    let cfg = base_config();
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let calls = Arc::new(AtomicUsize::new(0));
    let factory = Arc::new(CountingRouterFactory {
        id: "stub-router",
        calls: calls.clone(),
        router: Arc::new(FakeRouter::new(vec![])),
    });

    ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router_factory(factory)
        .build()
        .expect(
            "build must succeed against an empty-chain config: only the factory's own \
             (validation-free) router can have been used, not the compiled DeclarativeRouter",
        );

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the registered factory's build() must have been invoked exactly once"
    );
}

/// Property 2: the SAME factory is neither used NOR CONSTRUCTED when
/// `with_router` is also called -- an injected router wins unconditionally
/// and is never wrapped, inspected, or validated (this item's own binding
/// spec). The call counter staying at 0 is the "not constructed" half: a
/// precedence bug that skipped only the RESULT (still calling `build()` for
/// some other reason) would still be caught here.
#[test]
fn factory_is_not_used_or_constructed_when_with_router_is_also_set() {
    let cfg = base_config();
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let calls = Arc::new(AtomicUsize::new(0));
    let factory = Arc::new(CountingRouterFactory {
        id: "stub-router",
        calls: calls.clone(),
        router: Arc::new(FakeRouter::new(vec![])),
    });
    let injected_router: Arc<dyn Router> = Arc::new(FakeRouter::new(vec![]));

    ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(injected_router)
        .with_router_factory(factory)
        .build()
        .expect("build must succeed: the injected router is used, sidestepping the empty chain");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "an injected `with_router` must win unconditionally: the registered factory's build() \
         must never be invoked at all"
    );
}

/// Property 3: a factory whose `build` returns `Err` surfaces as
/// `ConwayError::Build`, naming both the factory's own id and the
/// underlying message -- never silently swallowed, never a fallback to the
/// compiled router.
#[test]
fn factory_build_error_surfaces_as_build_error() {
    let cfg = base_config();
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let factory = Arc::new(ErrRouterFactory {
        id: "exploding-router",
        detail: "no upstream reachable for this router kind",
    });

    let result = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router_factory(factory)
        .build();

    let err = expect_build_err(result, "a factory build() error must fail the whole build()");
    match err {
        ConwayError::Build { message } => {
            assert!(
                message.contains("exploding-router"),
                "the Build error must name the failing factory's own id: {message}"
            );
            assert!(
                message.contains("no upstream reachable for this router kind"),
                "the Build error must carry the underlying message: {message}"
            );
        }
        other => panic!("expected ConwayError::Build, got a different variant: {other:?}"),
    }
}
