//! Board item 01KZHF0RBKJZZC68F7GPFB347Q: `ConwayBuilder::with_backend_factory`
//! -- a provider-adapter KIND named up front, its construction deferred to
//! `build()`'s own backend step. Mirrors `tests/router_factory.rs`'s own
//! mechanism one layer over, restated for a SET rather than a singleton
//! (`with_backend_factory`'s own doc, `crates/conway/src/builder.rs`).
//!
//! Four properties, each a discriminating observable rather than a
//! structural check:
//!
//! 1. A factory registered via `with_backend_factory` IS invoked, and the
//!    backend it builds actually SERVES a turn -- not merely that `build()`
//!    succeeds (`factory_built_backend_serves_a_turn`).
//! 2. An injected `with_backend` backend wins over a factory-built backend
//!    sharing the same `Backend::id()`: the turn is served by the
//!    INJECTED backend's own content, and the factory's `build()` was
//!    still invoked (its result is discarded, not skipped -- unlike
//!    `RouterFactory`, where an injected router skips the factory
//!    entirely; see `with_backend_factory`'s own doc for why backends, a
//!    SET, differ from routing here)
//!    (`injected_backend_wins_over_factory_built_backend_sharing_its_id`).
//! 3. Two factories reporting the same `BackendFactory::id()` (a duplicate
//!    KIND) is a hard `build()` error naming it, and NEITHER factory's
//!    `build` runs (`duplicate_factory_kind_is_a_build_error_before_either_
//!    build_runs`).
//! 4. A factory whose `build` returns `Err` surfaces as `ConwayError::Build`,
//!    naming both the factory's own kind id and the underlying message
//!    (`factory_build_error_surfaces_as_build_error`).
//!
//! P-15's break-the-guard run for property 2 is recorded in this item's own
//! completion report, not committed here (the guard must be shown to fail
//! and then be restored, never left broken in the tree).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig, TuiSection,
};
use conway::{
    BackendBuildContext, BackendFactory, Conway, ConwayBuilder, ConwayError, CoreConwayError,
    SessionSpec,
};
use conway_core::agent::PermissionDecision;
use conway_core::capabilities::{
    CacheMode, Capabilities, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::fakes::{FakeBackend, FakeGate, FakeRouter, FakeStore};
use conway_core::ids::{BackendId, ModelId, ModelRef};
use conway_core::ports::{Backend, GenerateResponse};

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

/// One role with an EMPTY chain -- `merge::validate`'s own chain/backend
/// existence check has nothing to reject, and `build()`'s no-router/
/// no-factory default (`conway_core::routing::MinimalRouter`) never
/// validates a chain either, matching `tests/router_factory.rs`'s own
/// `base_config` precedent.
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
    }
}

fn fake_router_single(id: &str) -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new(id),
        model: ModelId::new("stub-model"),
    }))
}

/// A `BackendFactory` counting its own `build` calls (the same
/// `CountingRouterFactory` precedent in `tests/router_factory.rs`) whose
/// `build` returns a `FakeBackend` with `id`/`text` fixed at construction --
/// distinct content is what lets `injected_backend_wins_over_factory_built_
/// backend_sharing_its_id` prove WHICH backend served a turn, not merely
/// that `build()` succeeded.
struct CountingBackendFactory {
    kind_id: &'static str,
    backend_id: &'static str,
    text: &'static str,
    calls: Arc<AtomicUsize>,
}

impl BackendFactory for CountingBackendFactory {
    fn id(&self) -> &str {
        self.kind_id
    }

    fn build(&self, _ctx: BackendBuildContext) -> Result<Arc<dyn Backend>, CoreConwayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(FakeBackend::new(
            BackendId::new(self.backend_id),
            caps(),
            text_response(self.text),
        )))
    }
}

/// A `BackendFactory` whose `build` always fails.
struct ErrBackendFactory {
    kind_id: &'static str,
    detail: &'static str,
}

impl BackendFactory for ErrBackendFactory {
    fn id(&self) -> &str {
        self.kind_id
    }

    fn build(&self, _ctx: BackendBuildContext) -> Result<Arc<dyn Backend>, CoreConwayError> {
        Err(CoreConwayError::Config {
            detail: self.detail.to_string(),
        })
    }
}

/// `Conway` deliberately does not derive `Debug`, so `expect_err`/
/// `unwrap_err` cannot be used on a `Result<Conway, _>` -- mirrors
/// `tests/builder.rs`/`tests/router_factory.rs`'s own `expect_build_err`.
fn expect_build_err(result: Result<Conway, ConwayError>, msg: &str) -> ConwayError {
    match result {
        Err(err) => err,
        Ok(_) => panic!("{msg}"),
    }
}

/// Property 1: a factory registered via `with_backend_factory` IS invoked,
/// and the backend it builds actually serves a turn. Discriminating
/// because `base_config()` configures NO `[backends.<id>]` entries at all
/// (`config.backends` is empty) and no `with_backend` is called either --
/// `build()`'s own "no backends configured" guard means a success here,
/// producing a turn whose text is the factory's OWN canned response, can
/// only be explained by the registered factory's `build()` having actually
/// run and its backend having actually served the request.
#[tokio::test]
async fn factory_built_backend_serves_a_turn() {
    let cfg = base_config();
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let calls = Arc::new(AtomicUsize::new(0));
    let factory = Arc::new(CountingBackendFactory {
        kind_id: "stub-kind",
        backend_id: "stub-instance",
        text: "hello from the factory-built backend",
        calls: calls.clone(),
    });

    let conway = ConwayBuilder::from_parts(cfg)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router_single("stub-instance"))
        .with_backend_factory(factory)
        .build()
        .expect("build must succeed: the registered factory is the build's only backend source");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the registered factory's build() must have been invoked exactly once"
    );

    let session = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session must succeed");
    let turn = session
        .prompt("hi")
        .await
        .expect("prompt must succeed");
    let text = turn.text().await.expect("text must succeed");

    assert_eq!(
        text, "hello from the factory-built backend",
        "the turn's own text must be the factory-built backend's canned response -- an \
         observable outcome, not merely that build() and prompt() returned Ok"
    );
}

/// Property 2: an injected `with_backend` backend wins over a factory-built
/// backend sharing the same `Backend::id()` -- the turn's text is the
/// INJECTED backend's own content, never the factory's, and the factory's
/// `build()` still ran (its result was discarded at the id-merge step, not
/// skipped the way an injected `with_router` skips `RouterFactory::build`
/// entirely -- `with_backend_factory`'s own doc states this asymmetry).
#[tokio::test]
async fn injected_backend_wins_over_factory_built_backend_sharing_its_id() {
    let cfg = base_config();
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let calls = Arc::new(AtomicUsize::new(0));
    let factory = Arc::new(CountingBackendFactory {
        kind_id: "stub-kind",
        backend_id: "shared-id",
        text: "factory-built (must lose)",
        calls: calls.clone(),
    });
    let injected: Arc<dyn Backend> = Arc::new(FakeBackend::new(
        BackendId::new("shared-id"),
        caps(),
        text_response("injected (must win)"),
    ));

    let conway = ConwayBuilder::from_parts(cfg)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router_single("shared-id"))
        .with_backend_factory(factory)
        .with_backend(injected)
        .build()
        .expect("build must succeed: exactly one backend id is ever visible to routing");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the factory's build() still runs even though its result is discarded -- backends are a \
         SET merged by id, not a router's unconditional single winner"
    );

    let session = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session must succeed");
    let turn = session
        .prompt("hi")
        .await
        .expect("prompt must succeed");
    let text = turn.text().await.expect("text must succeed");

    assert_eq!(
        text, "injected (must win)",
        "with_backend must take precedence over a factory-built backend sharing its id -- the \
         factory-built backend's own content must never be the one that served the turn"
    );
}

/// Property 3: two factories reporting the same `BackendFactory::id()` --
/// a duplicate KIND, not a duplicate instance -- is a hard `build()` error
/// naming it, checked BEFORE either factory's `build` runs (both call
/// counters stay at 0, proving neither ran, not merely that the error
/// surfaced).
#[test]
fn duplicate_factory_kind_is_a_build_error_before_either_build_runs() {
    let cfg = base_config();
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let calls_a = Arc::new(AtomicUsize::new(0));
    let calls_b = Arc::new(AtomicUsize::new(0));
    let factory_a = Arc::new(CountingBackendFactory {
        kind_id: "duplicate-kind",
        backend_id: "a",
        text: "a",
        calls: calls_a.clone(),
    });
    let factory_b = Arc::new(CountingBackendFactory {
        kind_id: "duplicate-kind",
        backend_id: "b",
        text: "b",
        calls: calls_b.clone(),
    });

    let result = ConwayBuilder::from_parts(cfg)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router_single("a"))
        .with_backend_factory(factory_a)
        .with_backend_factory(factory_b)
        .build();

    let err = expect_build_err(
        result,
        "two factories reporting the same BackendFactory::id() must fail build()",
    );
    match err {
        ConwayError::Build { message } => {
            assert!(
                message.contains("duplicate-kind"),
                "the Build error must name the duplicated kind id: {message}"
            );
        }
        other => panic!("expected ConwayError::Build, got a different variant: {other:?}"),
    }
    assert_eq!(
        calls_a.load(Ordering::SeqCst),
        0,
        "the duplicate-kind check must run before either factory's build() -- side-effect-free \
         on this error"
    );
    assert_eq!(calls_b.load(Ordering::SeqCst), 0);
}

/// Property 4: a factory whose `build` returns `Err` fails the whole
/// `build()` call as `ConwayError::Build`, naming both the factory's own
/// kind id and the underlying message -- never silently swallowed, never a
/// fallback that drops the kind and proceeds.
#[test]
fn factory_build_error_surfaces_as_build_error() {
    let cfg = base_config();
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let factory = Arc::new(ErrBackendFactory {
        kind_id: "exploding-kind",
        detail: "no upstream reachable for this kind",
    });

    let result = ConwayBuilder::from_parts(cfg)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router_single("irrelevant"))
        .with_backend_factory(factory)
        .build();

    let err = expect_build_err(result, "a factory build() error must fail the whole build()");
    match err {
        ConwayError::Build { message } => {
            assert!(
                message.contains("exploding-kind"),
                "the Build error must name the failing factory's own kind id: {message}"
            );
            assert!(
                message.contains("no upstream reachable for this kind"),
                "the Build error must carry the underlying message: {message}"
            );
        }
        other => panic!("expected ConwayError::Build, got a different variant: {other:?}"),
    }
}
