//! `ConwayBuilder::install_selected`:
//! plugin assembly as a facade capability over CALLER-SUPPLIED bundles,
//! collapsing `crates/conway-cli/src/first_party_plugins.rs`'s ~70-line
//! hand-rolled resolution onto one method any embedder can call. This file
//! is the library-level, fakes-driven coverage of that resolution logic
//! itself; `crates/conway-cli/tests/first_party_plugins.rs` (and its sibling
//! `decline_backend_kind.rs`) is the real-compiled-binary liveness proof
//! that `first_party_plugins::install` wires this binary's own three linked
//! bundles into it correctly -- the two are deliberately non-overlapping,
//! not duplicated coverage of the same property.
//!
//! **The facade depends on no plugin crate here either.** Every "bundle" in
//! this file is a fake defined in this test, never a real first-party
//! plugin crate -- the discriminating proof that `install_selected` truly
//! resolves against whatever `Vec`s a caller hands it, not against
//! anything this crate itself knows how to construct.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, BackendEntry, ConwayConfig, HealthSection, HooksConfig, LimitsConfig,
    ModelsConfig, PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig,
    ToolsConfig,
};
use conway::plugin::{PluginManifest, Tool};
use conway::{
    BackendBuildContext, BackendFactory, Conway, ConwayBuilder, CoreConwayError, FacadeError,
    HealthRegistry, Plugin, Router, RouterBuildContext, RouterBundle, RouterFactory,
};
use conway_core::agent::PermissionDecision;
use conway_core::capabilities::{
    CacheMode, Capabilities, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{StopReason, Usage};
use conway_core::ids::{BackendId, RoleAlias};
use conway_core::ports::GenerateResponse;
use conway_testkit::{FakeBackend, FakeGate, FakeHealth, FakeRouter, FakeStore};

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

/// DIVERGENT, deliberately: `FakeRouter::new(vec![])` is an EMPTY chain,
/// not the one-route `FakeRouter::single(conway::test_support::
/// echo_model())` every other suite injects. These tests only care that a
/// `Router` is present so `build()` proceeds; they never resolve a route.
/// Named `empty_router` rather than `fake_router` so that one name does
/// not mean two routers (`tests/builder.rs` has the same double, under the
/// same name and for the same reason).
fn empty_router() -> Arc<dyn Router> {
    Arc::new(FakeRouter::new(vec![]))
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

/// One role with an empty chain (routing is not what this file exercises),
/// no backends, `[plugins]` left at whatever `install`/`default_backends`
/// each test sets.
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
        default_role: RoleAlias::new("default"),
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
        tools: ToolsConfig::default(),
        // `default_backends` is deliberately empty here (the schema default
        // is `["anthropic", "openai-compat"]`): every test in this file
        // attaches its own fake backend via `.with_backend()` -- see
        // `capability_channel.rs`'s identical note.
        plugins: PluginsConfig {
            default_backends: vec![],
            ..PluginsConfig::default()
        },
        hooks: HooksConfig::default(),
    }
}

/// `Conway` deliberately does not derive `Debug`, mirroring
/// `crates/conway/tests/builder.rs`'s own `expect_build_err`.
fn expect_build_err(result: Result<Conway, FacadeError>, msg: &str) -> FacadeError {
    match result {
        Err(err) => err,
        Ok(_) => panic!("{msg}"),
    }
}

// ---------------------------------------------------------------------------
// A minimal no-op `Plugin` fake -- this file's own bundle member, never a
// real crate.
// ---------------------------------------------------------------------------

struct FakePlugin(&'static str);

impl Plugin for FakePlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.0.to_string(),
            version: "0.0.0".to_string(),
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }
}

/// A `Plugin` fake carrying configurable `PluginManifest::requires`/
/// `::optional` -- `FakePlugin` above stays a plain zero-dependency
/// fixture; this is the dedicated one for the dependency-resolution tests
/// below.
struct DependentPlugin {
    id: &'static str,
    requires: Vec<String>,
    optional: Vec<String>,
}

impl Plugin for DependentPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.id.to_string(),
            version: "0.0.0".to_string(),
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: self.requires.clone(),
            optional: self.optional.clone(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }
}

/// A `RouterFactory` fake counting its own `build` calls -- the same
/// discriminating pattern `crates/conway/tests/router_factory.rs`'s
/// `CountingRouterFactory` uses: a factory whose `build` is skipped leaves
/// the counter at 0 regardless of what it would otherwise have returned.
struct CountingRouterFactory {
    id: &'static str,
    calls: Arc<AtomicUsize>,
}

impl RouterFactory for CountingRouterFactory {
    fn id(&self) -> &str {
        self.id
    }

    fn build(&self, _ctx: RouterBuildContext<'_>) -> Result<RouterBundle, CoreConwayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(RouterBundle {
            router: empty_router(),
            health: Arc::new(FakeHealth::new()) as Arc<dyn HealthRegistry>,
            explain: None,
        })
    }
}

/// A `BackendFactory` fake counting its own `build` calls, returning a
/// `FakeBackend` under its own kind id.
struct CountingBackendFactory {
    id: &'static str,
    calls: Arc<AtomicUsize>,
}

impl BackendFactory for CountingBackendFactory {
    fn id(&self) -> &str {
        self.id
    }

    fn build(
        &self,
        ctx: BackendBuildContext,
    ) -> Result<Arc<dyn conway_core::ports::Backend>, CoreConwayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(FakeBackend::new(
            ctx.id,
            caps(),
            GenerateResponse {
                content: vec![],
                tool_calls: vec![],
                stop: StopReason::EndTurn,
                usage: Usage::default(),
            },
        )))
    }
}

/// Shape 1: a `[plugins].install` id matching a bundle `Plugin`'s own
/// manifest id installs it -- proven indirectly (this crate exposes no
/// "list installed plugins" accessor) by then injecting a SECOND plugin
/// under the identical id via `with_plugin` and observing `build()`'s
/// pre-existing duplicate-manifest-id rejection: the duplicate check can
/// only fire if `install_selected` genuinely attached the first one.
#[test]
fn plugin_id_resolves_and_attaches_via_with_plugin() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["test.echo".to_string()];

    let result = ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![Arc::new(FakePlugin("test.echo")) as Arc<dyn Plugin>],
            vec![],
            vec![],
        )
        .expect("a known plugin id must resolve")
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .with_plugin(Arc::new(FakePlugin("test.echo")))
        .build();

    let err = expect_build_err(
        result,
        "install_selected's plugin must already be attached, so a second one under the same id \
         must collide",
    );
    match err {
        FacadeError::Build { message } => {
            assert!(message.contains("duplicate plugin id"), "{message}");
            assert!(message.contains("test.echo"), "{message}");
        }
        other => panic!("expected FacadeError::Build, got {other:?}"),
    }
}

/// Shape 2: a `[plugins].install` id matching a bundle `RouterFactory`'s own
/// `id()` installs it via `with_router_factory` -- observed directly, the
/// same counting pattern `router_factory.rs` uses: the factory's `build` is
/// actually invoked, and no `with_router`/other router is set, so its
/// router is what `build()` used.
#[test]
fn router_factory_id_resolves_and_is_used() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["test.router".to_string()];
    let calls = Arc::new(AtomicUsize::new(0));

    ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![],
            vec![Arc::new(CountingRouterFactory {
                id: "test.router",
                calls: calls.clone(),
            })],
            vec![],
        )
        .expect("a known router-factory id must resolve")
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_backend(fake_backend("fake"))
        .build()
        .expect("build must succeed with the resolved router factory");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the resolved router factory's own build() must have been invoked exactly once"
    );
}

/// Two ids in `[plugins].install` each matching a DIFFERENT router factory
/// is a hard error naming both, never a silent "last one wins" -- a build
/// has exactly one router.
#[test]
fn two_router_factory_ids_is_a_hard_error() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["router.a".to_string(), "router.b".to_string()];

    let result = ConwayBuilder::from_parts(cfg).install_selected(
        vec![],
        vec![
            Arc::new(CountingRouterFactory {
                id: "router.a",
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(CountingRouterFactory {
                id: "router.b",
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        ],
        vec![],
    );

    let err = result
        .err()
        .expect("naming two router factories must fail before build() is ever reached")
        .to_string();
    assert!(err.contains("router.a"), "{err}");
    assert!(err.contains("router.b"), "{err}");
    assert!(err.contains("exactly one router"), "{err}");
}

/// Shape 3: a `[plugins].default_backends` id (WITH NO `[plugins].install`
/// entry naming it at all) matching a bundle `BackendFactory`'s own `id()`
/// installs it via `with_backend_factory` -- the asymmetry `PluginsConfig::
/// default_backends`'s own doc states: a fresh install reaches a model with
/// zero `[plugins]` configuration by naming this list's own default.
#[test]
fn backend_factory_id_resolves_from_default_backends_with_no_install_entry() {
    let mut cfg = base_config();
    cfg.plugins.install = vec![]; // deliberately empty
    cfg.plugins.default_backends = vec!["test.backend".to_string()];
    cfg.backends.insert(
        "b".to_string(),
        BackendEntry {
            kind: "test.backend".to_string(),
            ..BackendEntry::default()
        },
    );
    let calls = Arc::new(AtomicUsize::new(0));

    ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![],
            vec![],
            vec![Arc::new(CountingBackendFactory {
                id: "test.backend",
                calls: calls.clone(),
            })],
        )
        .expect("a default_backends id must resolve with no [plugins].install entry")
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .build()
        .expect("build must succeed: the backend factory resolved and constructed a backend");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the resolved backend factory's own build() must have been invoked exactly once"
    );
}

/// The stated outcome for "a configured id matches nothing in the supplied
/// bundles": a hard, named `FacadeError::Config`, never a silent no-op --
/// matching `first_party_plugins::install`'s own pre-existing behavior,
/// extended to name whichever bundles THIS caller supplied. The message
/// names the offending id and lists every id each of the three supplied
/// bundles actually carries.
#[test]
fn unknown_id_is_a_hard_error_naming_the_id_and_every_supplied_bundle() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["totally.unknown".to_string()];

    let result = ConwayBuilder::from_parts(cfg).install_selected(
        vec![Arc::new(FakePlugin("known.plugin")) as Arc<dyn Plugin>],
        vec![Arc::new(CountingRouterFactory {
            id: "known.router",
            calls: Arc::new(AtomicUsize::new(0)),
        })],
        vec![Arc::new(CountingBackendFactory {
            id: "known.backend",
            calls: Arc::new(AtomicUsize::new(0)),
        })],
    );

    let err = result
        .err()
        .expect("an id naming nothing in any supplied bundle must fail, not silently no-op")
        .to_string();
    assert!(
        err.contains("totally.unknown"),
        "the error must name the offending id: {err}"
    );
    assert!(err.contains("known.plugin"), "{err}");
    assert!(err.contains("known.router"), "{err}");
    assert!(err.contains("known.backend"), "{err}");
}

/// An empty resolved id set (`[plugins].install` empty, `[plugins].
/// default_backends` empty -- `base_config`'s own default) is NOT itself an
/// error: `install_selected` returns `Ok`, and none of the three supplied
/// bundles are consulted (a bundle entry that would otherwise be an
/// unknown-id error, were its id named, is simply never looked at).
#[test]
fn empty_resolved_id_set_is_not_an_error_and_consults_no_bundle() {
    let cfg = base_config();
    assert!(cfg.plugins.install.is_empty());
    assert!(cfg.plugins.default_backends.is_empty());

    let builder = ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![Arc::new(FakePlugin("unused.plugin")) as Arc<dyn Plugin>],
            vec![],
            vec![],
        )
        .expect("an empty resolved id set must not itself be an error");

    // The unused plugin was never attached: injecting a plugin under the
    // SAME id via `with_plugin` must not collide (a collision would prove
    // `install_selected` attached it despite naming no id for it).
    let result = builder
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .with_plugin(Arc::new(FakePlugin("unused.plugin")))
        .build();
    assert!(
        result.is_ok(),
        "the supplied plugin bundle must not have been consulted at all when the resolved id \
         set is empty"
    );
}

/// `install_selected` also calls `with_declined_backend_kinds` unconditionally
/// ('s mechanism), naming every
/// supplied backend-factory id the resolved id set does not select --
/// mirroring `crates/conway/tests/builder.rs`'s own
/// `declined_backend_kind_error_is_distinct_from_unknown_backend_kind_error`,
/// through `install_selected` instead of a direct `with_declined_backend_
/// kinds` call.
#[test]
fn a_supplied_backend_factory_not_selected_is_diagnosed_as_declined_not_unknown() {
    let mut cfg = base_config();
    // Neither `install` nor `default_backends` names "test.backend" -- it is
    // supplied but not selected.
    cfg.backends.insert(
        "b".to_string(),
        BackendEntry {
            kind: "test.backend".to_string(),
            ..BackendEntry::default()
        },
    );

    let result = ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![],
            vec![],
            vec![Arc::new(CountingBackendFactory {
                id: "test.backend",
                calls: Arc::new(AtomicUsize::new(0)),
            })],
        )
        .expect("an empty resolved id set is not itself an error")
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .build();

    let err = expect_build_err(
        result,
        "a [backends.<id>] entry naming a supplied-but-unselected kind must fail build()",
    );
    match err {
        FacadeError::Config { message, .. } => {
            assert!(message.contains("declined"), "{message}");
            assert!(
                !message.to_lowercase().contains("unknown kind"),
                "{message}"
            );
        }
        other => panic!("expected FacadeError::Config, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// PluginManifest::requires / ::optional (board item
// `01M0WWJMYK0KDC2X7B7MR46FRR`): topological dependency resolution.
// ---------------------------------------------------------------------------

/// `install_selected` performs a topological cycle check over what IT can
/// see -- every plugin its own `plugins` bundle resolves via `wanted` --
/// and refuses a cycle there before `build()` is ever reached: neither
/// side of `a requires b requires a` can ever be satisfied "first",
/// regardless of what `build()` later adds.
#[test]
fn install_selected_refuses_a_requires_cycle_among_what_it_can_see() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["cycle.a".to_string(), "cycle.b".to_string()];

    let result = ConwayBuilder::from_parts(cfg).install_selected(
        vec![
            Arc::new(DependentPlugin {
                id: "cycle.a",
                requires: vec!["cycle.b".to_string()],
                optional: vec![],
            }) as Arc<dyn Plugin>,
            Arc::new(DependentPlugin {
                id: "cycle.b",
                requires: vec!["cycle.a".to_string()],
                optional: vec![],
            }) as Arc<dyn Plugin>,
        ],
        vec![],
        vec![],
    );

    let err = result
        .err()
        .expect("a requires cycle among the resolved bundle must be refused before build()")
        .to_string();
    assert!(err.contains("cycle"), "{err}");
    assert!(err.contains("cycle.a"), "{err}");
    assert!(err.contains("cycle.b"), "{err}");
}

/// The mirror of the cycle case: `install_selected` does NOT refuse a
/// MISSING required dependency on its own, because it cannot see whether
/// the missing id will turn out to be a built-in `build()` adds later (this
/// file's own bundles never include built-ins -- only `build()`'s full
/// `resolved_plugins` set does). The check is deferred, and it IS
/// authoritative there: a required dependency absent from the FINAL
/// installed set is a hard `FacadeError::Build` naming both the dependent
/// and the missing dependency.
#[test]
fn install_selected_defers_missing_required_check_to_build() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["needs.ui".to_string()];

    let builder = ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![Arc::new(DependentPlugin {
                id: "needs.ui",
                requires: vec!["conway.ui".to_string()],
                optional: vec![],
            }) as Arc<dyn Plugin>],
            vec![],
            vec![],
        )
        .expect(
            "install_selected must not refuse a missing required dependency itself -- it cannot \
             see whether build() will supply it as a built-in",
        );

    let result = builder
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .build();

    let err = expect_build_err(
        result,
        "a required dependency absent from the FINAL installed set must fail build()",
    );
    match err {
        FacadeError::Build { message } => {
            assert!(
                message.contains("needs.ui"),
                "the dependent must be named: {message}"
            );
            assert!(
                message.contains("conway.ui"),
                "the missing dependency must be named: {message}"
            );
        }
        other => panic!("expected FacadeError::Build, got {other:?}"),
    }
}

/// `PluginManifest::optional`'s absence NEVER fails `build()`: the
/// dependent loads anyway, degraded, and the degradation is recorded on
/// `Conway::warnings()` (`WarningCode::OptionalPluginDependencyMissing`)
/// naming both the dependent and the missing dependency.
#[test]
fn missing_optional_dependency_loads_degraded_and_is_recorded_on_warnings() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["wants.ui".to_string()];

    let conway = ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![Arc::new(DependentPlugin {
                id: "wants.ui",
                requires: vec![],
                optional: vec!["conway.ui".to_string()],
            }) as Arc<dyn Plugin>],
            vec![],
            vec![],
        )
        .expect("install_selected must succeed: no REQUIRED dependency is missing")
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .build()
        .expect("a missing OPTIONAL dependency must never fail build()");

    let warnings = conway.warnings();
    let degraded: Vec<_> = warnings
        .iter()
        .filter(|w| w.code == conway::config::WarningCode::OptionalPluginDependencyMissing)
        .collect();
    assert_eq!(
        degraded.len(),
        1,
        "exactly one degradation warning must be recorded: {warnings:?}"
    );
    assert!(
        degraded[0].message.contains("wants.ui"),
        "{}",
        degraded[0].message
    );
    assert!(
        degraded[0].message.contains("conway.ui"),
        "{}",
        degraded[0].message
    );
}

/// The positive control beside the two refusal tests above: a `requires`
/// edge that DOES resolve (both plugins present) builds cleanly, and no
/// degradation warning is recorded for it (only an `optional` miss
/// produces one).
#[test]
fn a_satisfied_requires_edge_builds_cleanly_with_no_warning() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["needs.base".to_string(), "base".to_string()];

    let conway = ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![
                Arc::new(DependentPlugin {
                    id: "needs.base",
                    requires: vec!["base".to_string()],
                    optional: vec![],
                }) as Arc<dyn Plugin>,
                Arc::new(DependentPlugin {
                    id: "base",
                    requires: vec![],
                    optional: vec![],
                }) as Arc<dyn Plugin>,
            ],
            vec![],
            vec![],
        )
        .expect("both ids resolve against the supplied bundle")
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .build()
        .expect("a satisfied requires edge must build cleanly");

    assert!(
        conway
            .warnings()
            .iter()
            .all(|w| w.code != conway::config::WarningCode::OptionalPluginDependencyMissing),
        "a satisfied requires edge must not produce a degradation warning: {:?}",
        conway.warnings()
    );
}
