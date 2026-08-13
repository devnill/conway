//! Board item 01KZHF0RBKJZZC68F7GPFB347Q: `ConwayBuilder::with_backend_factory`
//! -- a provider-adapter KIND named up front, its construction deferred to
//! `build()`'s own backend step. Mirrors `tests/router_factory.rs`'s own
//! mechanism one layer over, restated for a SET rather than a singleton
//! (`with_backend_factory`'s own doc, `crates/conway/src/builder.rs`).
//!
//! **Updated for board item 01KZHF1E85MS1VF4YH8CDNCP9Z:** `[backends.<id>].
//! kind` is now an open name resolved against registered factories, so a
//! factory's `build` is invoked once per `[backends.<id>]` entry naming its
//! kind -- never unconditionally regardless of config, which is what this
//! file's fixtures exercised before that item landed (`with_backend_factory`'s
//! own doc, `crates/conway/src/builder.rs`, states this explicitly:
//! "registering a factory whose kind no entry names is still fine, not an
//! error" -- its `build` is simply never invoked). Every fixture below that
//! needs a factory invoked therefore names that factory's own `kind_id` in a
//! `[backends.<id>]` entry via `config_naming_kind`.
//!
//! Four properties, each a discriminating observable rather than a
//! structural check:
//!
//! 1. A factory registered via `with_backend_factory` and named by a
//!    `[backends.<id>].kind` entry IS invoked, and the backend it builds
//!    actually SERVES a turn -- not merely that `build()` succeeds
//!    (`factory_built_backend_serves_a_turn`).
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
//!    `build` runs, regardless of whether any `[backends.<id>]` entry names
//!    that kind (`duplicate_factory_kind_is_a_build_error_before_either_
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
use std::sync::{Arc, Mutex};

use conway::config::schema::{
    AgentsConfig, BackendEntry, ConwayConfig, HealthSection, HooksConfig, LimitsConfig,
    ModelsConfig, PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig,
    ToolsConfig, TuiSection,
};
use conway::{
    BackendBuildContext, BackendFactory, Conway, ConwayBuilder, ConwayError, CoreConwayError,
    SessionSpec,
};
use conway_core::agent::PermissionDecision;
use conway_core::capabilities::{
    CacheMode, Capabilities, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{ContentBlock, SamplingParams, StopReason, Usage};
use conway_core::fakes::{FakeBackend, FakeGate, FakeRouter, FakeStore};
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias};
use conway_core::ports::{Backend, GenerateResponse};
use conway_core::routing::{Route, RoutingReason};

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
        hooks: HooksConfig::default(),
    }
}

/// `base_config()` plus one `[backends.<entry_id>]` entry naming `kind` --
/// what selects a registered `BackendFactory` whose own `id()` is `kind`
/// (board item 01KZHF1E85MS1VF4YH8CDNCP9Z: `kind` is an open name resolved
/// against registered factories). `entry_id` is the JSON key only; it never
/// needs to match anything the factory itself returns from `Backend::id()`
/// (`BackendBuildContext::id` is advisory, not enforced -- that struct's own
/// doc).
fn config_naming_kind(entry_id: &str, kind: &str) -> ConwayConfig {
    let mut cfg = base_config();
    cfg.backends.insert(
        entry_id.to_string(),
        BackendEntry {
            kind: kind.to_string(),
            ..BackendEntry::default()
        },
    );
    cfg
}

/// `base_config()` plus TWO `[backends.<id>]` entries naming the SAME
/// `kind` -- what `two_entries_naming_one_kind_invoke_the_factory_twice_
/// and_produce_two_distinct_backends` below needs: two config entries must
/// not collapse into one factory invocation merely because they share a
/// kind.
fn config_naming_kind_twice(entry_a: &str, entry_b: &str, kind: &str) -> ConwayConfig {
    let mut cfg = base_config();
    cfg.backends.insert(
        entry_a.to_string(),
        BackendEntry {
            kind: kind.to_string(),
            ..BackendEntry::default()
        },
    );
    cfg.backends.insert(
        entry_b.to_string(),
        BackendEntry {
            kind: kind.to_string(),
            ..BackendEntry::default()
        },
    );
    cfg
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

/// Property 1: a factory registered via `with_backend_factory` and named by
/// a `[backends.<id>].kind` entry IS invoked, and the backend it builds
/// actually serves a turn. Discriminating because `config_naming_kind`'s one
/// `[backends.<id>]` entry is the ONLY backend source (no `with_backend`
/// call either) -- `build()`'s own "no backends configured" guard means a
/// success here, producing a turn whose text is the factory's OWN canned
/// response, can only be explained by the registered factory's `build()`
/// having actually run (selected by the config entry naming its kind) and
/// its backend having actually served the request.
#[tokio::test]
async fn factory_built_backend_serves_a_turn() {
    let cfg = config_naming_kind("stub", "stub-kind");
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
    let turn = session.prompt("hi").await.expect("prompt must succeed");
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
    let cfg = config_naming_kind("stub", "stub-kind");
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
    let turn = session.prompt("hi").await.expect("prompt must succeed");
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
    let cfg = config_naming_kind("exploding", "exploding-kind");
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

    let err = expect_build_err(
        result,
        "a factory build() error must fail the whole build()",
    );
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

/// A `BackendFactory` that builds a distinct backend PER CALL, using the
/// entry's own id (`ctx.id`) as `Backend::id()` -- so two `[backends.<id>]`
/// entries naming the SAME kind produce two independently addressable
/// backends, not one. The entry named `refuse_id` is built with a
/// `max_context_tokens` too small for ANY request to fit, so a routing
/// chain that lists it FIRST always skips it (`Backend::admit`'s default
/// `check_admission`) and falls through to the OTHER entry's own backend --
/// proving both concretely exist in the built runtime, not merely that
/// `build()` returned `Ok` twice.
struct DualEntryBackendFactory {
    kind_id: &'static str,
    refuse_id: &'static str,
    calls: Arc<Mutex<Vec<BackendId>>>,
}

impl BackendFactory for DualEntryBackendFactory {
    fn id(&self) -> &str {
        self.kind_id
    }

    fn build(&self, ctx: BackendBuildContext) -> Result<Arc<dyn Backend>, CoreConwayError> {
        self.calls.lock().unwrap().push(ctx.id.clone());
        let capabilities = if ctx.id.as_str() == self.refuse_id {
            Capabilities {
                max_context_tokens: 1,
                ..caps()
            }
        } else {
            caps()
        };
        Ok(Arc::new(FakeBackend::new(
            ctx.id.clone(),
            capabilities,
            text_response(&format!("served by {}", ctx.id.as_str())),
        )))
    }
}

/// Property 5 (this item's own binding notes, board item
/// 01KZMM9E5SMA9C1SB8D4RG6DDB): TWO `[backends.<id>]` entries naming the
/// SAME kind invoke that kind's registered factory TWICE, with two
/// DIFFERENT `BackendBuildContext`s -- the single material asymmetry
/// against `RouterFactory`, which builds at most once regardless of how
/// many config entries might reference it (`with_backend_factory`'s own
/// doc: "a router build has ONE outcome... a backend build has MANY").
///
/// The discriminating observable is that TWO DISTINCT backends resulted,
/// not merely that the call counter reached 2 (a dedupe-by-kind regression
/// that invoked the factory once but recorded the call twice, or invoked it
/// twice with the SAME context both times, would still pass a bare
/// call-count assertion). Proven by forcing the routing chain through BOTH
/// of them: the first-listed route's own backend (`entry-a`, built with an
/// impossibly small `max_context_tokens`) always refuses admission, so the
/// turn can only complete by falling through to the SECOND route's own
/// backend (`entry-b`) -- if the two entries had collapsed into one
/// factory invocation, or one had silently overwritten the other in the
/// built backend map, `entry-b`'s backend would be entirely absent from the
/// runtime, not merely differently configured, and this must fail loudly
/// rather than produce different error text.
#[tokio::test]
async fn two_entries_naming_one_kind_invoke_the_factory_twice_and_produce_two_distinct_backends() {
    let cfg = config_naming_kind_twice("entry-a", "entry-b", "shared-kind");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(DualEntryBackendFactory {
        kind_id: "shared-kind",
        refuse_id: "entry-a",
        calls: calls.clone(),
    });

    let route_a = Route {
        backend: BackendId::new("entry-a"),
        model: ModelId::new("stub-model"),
        params: SamplingParams::default(),
        reason: RoutingReason::AliasPrimary {
            alias: RoleAlias::new("primary"),
        },
    };
    let route_b = Route {
        backend: BackendId::new("entry-b"),
        model: ModelId::new("stub-model"),
        params: SamplingParams::default(),
        reason: RoutingReason::Fallback {
            position: 1,
            after: Vec::new(),
        },
    };
    let router: Arc<dyn conway_core::ports::Router> =
        Arc::new(FakeRouter::new(vec![route_a, route_b]));

    let conway = ConwayBuilder::from_parts(cfg)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(router)
        .with_backend_factory(factory)
        .build()
        .expect("build must succeed: two entries naming the same kind is not itself an error");

    let mut called: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|id| id.as_str().to_string())
        .collect();
    called.sort();
    assert_eq!(
        called,
        vec!["entry-a".to_string(), "entry-b".to_string()],
        "the registered factory must be invoked ONCE PER config entry naming its kind, not once \
         per kind -- got {called:?}"
    );

    let session = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session must succeed");
    let turn = session.prompt("hi").await.expect("prompt must succeed");
    let text = turn.text().await.expect("text must succeed");

    assert_eq!(
        text, "served by entry-b",
        "the first-listed route's own backend (entry-a) always refuses admission, so the turn \
         can only complete by falling through to entry-b's own, DISTINCT backend -- if the two \
         entries had collapsed into one factory call, or one had silently overwritten the \
         other, entry-b's backend would be missing from the built runtime entirely, not merely \
         differently configured -- got {text:?}"
    );
}
