//! Edge B's capability CALL channel (board item `01M0WWNHQQYN1EVTH8WPZ33EBF`,
//! `docs/vision/DESIGN-plugin-dependencies.md` §2), exercised through the
//! REAL `ConwayBuilder::build()` facade -- distinct from
//! `crates/conway-core/src/ports/capability.rs`'s own test module, which
//! exercises the channel primitives (`CapabilityCallHandle`,
//! `CapabilityRegistry`, `CapabilityProvider`) directly, and from
//! `crates/conway/src/builder.rs`'s own `plugin_dependency_resolution_tests`
//! module, which exercises the free resolution functions
//! (`missing_required_dependency`/`missing_optional_dependencies`/
//! `provided_capability_names`) at the graph-algorithm level. This file is
//! the facade-level proof that acceptance 2/3 hold through `build()` itself,
//! not merely through the private functions that implement them --
//! mirroring `install_selected.rs`'s own split for the plugin-id case
//! (`PluginManifest::requires`/`::optional`).

use std::collections::BTreeMap;
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, BackendEntry, ConwayConfig, HealthSection, HooksConfig, LimitsConfig,
    ModelsConfig, PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig,
    ToolsConfig,
};
use conway::plugin::{
    CapabilityError, CapabilityProvider, CapabilityRegistration, HostCapability, PluginManifest,
    Tool,
};
use conway::{Conway, ConwayBuilder, FacadeError, Plugin};
use conway_core::agent::PermissionDecision;
use conway_core::capabilities::{
    CacheMode, Capabilities, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{StopReason, Usage};
use conway_core::ids::{BackendId, RoleAlias};
use conway_core::ports::{GenerateResponse, Router};
use conway_testkit::{FakeBackend, FakeGate, FakeRouter, FakeStore};

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

/// Mirrors `install_selected.rs`'s own `empty_router` exactly (same name,
/// same reason): these tests only need a `Router` present so `build()`
/// proceeds, never a resolved route.
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
        backends: BTreeMap::<String, BackendEntry>::new(),
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig::default(),
        tools: ToolsConfig::default(),
        plugins: PluginsConfig {
            install: vec![],
            default_backends: vec![],
            subprocess: vec![],
            mcp: vec![],
            claude_compat: vec![],
        },
        hooks: HooksConfig::default(),
    }
}

/// `Conway` deliberately does not derive `Debug`, mirroring
/// `install_selected.rs`'s own `expect_build_err`.
fn expect_build_err(result: Result<Conway, FacadeError>, msg: &str) -> FacadeError {
    match result {
        Err(err) => err,
        Ok(_) => panic!("{msg}"),
    }
}

/// A fixture provider that answers every call with its input, unchanged.
struct EchoProvider;

#[async_trait::async_trait]
impl CapabilityProvider for EchoProvider {
    async fn call(&self, payload: serde_json::Value) -> Result<serde_json::Value, CapabilityError> {
        Ok(payload)
    }
}

/// A `Plugin` fake that registers one live capability provider under
/// `capability` (its own `PluginManifest::provides`-equivalent runtime
/// registration -- see `Plugin::capabilities`'s own doc for why this is a
/// trait method, not a manifest field).
struct ProvidingPlugin {
    id: &'static str,
    capability: &'static str,
}

impl Plugin for ProvidingPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.id.to_string(),
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

    fn capabilities(&self) -> Vec<CapabilityRegistration> {
        vec![CapabilityRegistration {
            capability: HostCapability::named(self.capability).unwrap(),
            provider: Arc::new(EchoProvider) as Arc<dyn CapabilityProvider>,
        }]
    }
}

/// A `Plugin` fake carrying configurable `PluginManifest::requires`/
/// `::optional` -- mirrors `install_selected.rs`'s own `DependentPlugin`.
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

/// Acceptance 2 (positive control): a `requires` entry satisfied by a
/// PROVIDED capability -- not a plugin id -- builds cleanly, no different
/// from a satisfied plugin-id `requires` edge.
#[test]
fn a_requires_edge_satisfied_by_a_provided_capability_builds_cleanly() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["acme.ui".to_string(), "acme.consumer".to_string()];

    ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![
                Arc::new(ProvidingPlugin {
                    id: "acme.ui",
                    capability: "acme.ui.checkbox",
                }) as Arc<dyn Plugin>,
                Arc::new(DependentPlugin {
                    id: "acme.consumer",
                    requires: vec!["acme.ui.checkbox".to_string()],
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
        .expect("a requires entry satisfied by a PROVIDED capability must build cleanly");
}

/// Acceptance 2: a `requires` entry naming a capability NOTHING installed
/// provides fails at `build()`, naming the consumer and the unprovided
/// capability -- the SAME `FacadeError::Build` shape a missing plugin id
/// already produces.
#[test]
fn a_requires_edge_naming_an_unprovided_capability_fails_build_naming_both_sides() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["acme.consumer".to_string()];

    let result = ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![Arc::new(DependentPlugin {
                id: "acme.consumer",
                requires: vec!["acme.ui.checkbox".to_string()],
                optional: vec![],
            }) as Arc<dyn Plugin>],
            vec![],
            vec![],
        )
        .expect("install_selected must not itself refuse a missing required dependency")
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .build();

    let err = expect_build_err(
        result,
        "a requires entry naming a capability nothing installed provides must fail build()",
    );
    match err {
        FacadeError::Build { message } => {
            assert!(message.contains("acme.consumer"), "{message}");
            assert!(message.contains("acme.ui.checkbox"), "{message}");
        }
        other => panic!("expected FacadeError::Build, got {other:?}"),
    }
}

/// Acceptance 3: an `optional` capability nothing provides degrades (never
/// fails `build()`) and is announced via `Conway::warnings()`
/// (`WarningCode::OptionalPluginDependencyMissing`) -- the SAME two-channel
/// announcement a missing optional plugin-id dependency already gets.
#[test]
fn an_optional_capability_nothing_provides_degrades_and_is_announced() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["acme.consumer".to_string()];

    let conway = ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![Arc::new(DependentPlugin {
                id: "acme.consumer",
                requires: vec![],
                optional: vec!["acme.ui.checkbox".to_string()],
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
        .expect("a missing OPTIONAL capability must never fail build()");

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
    assert!(degraded[0].message.contains("acme.consumer"));
    assert!(degraded[0].message.contains("acme.ui.checkbox"));
}

/// Positive control beside the optional-degrade test above: the SAME
/// `optional` capability, but a provider IS installed this time -- builds
/// cleanly with no degradation warning at all.
#[test]
fn an_optional_capability_something_provides_produces_no_warning() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["acme.ui".to_string(), "acme.consumer".to_string()];

    let conway = ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![
                Arc::new(ProvidingPlugin {
                    id: "acme.ui",
                    capability: "acme.ui.checkbox",
                }) as Arc<dyn Plugin>,
                Arc::new(DependentPlugin {
                    id: "acme.consumer",
                    requires: vec![],
                    optional: vec!["acme.ui.checkbox".to_string()],
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
        .expect("an optional capability satisfied by a real provider must build cleanly");

    assert!(
        conway
            .warnings()
            .iter()
            .all(|w| w.code != conway::config::WarningCode::OptionalPluginDependencyMissing),
        "a satisfied optional capability must not produce a degradation warning: {:?}",
        conway.warnings()
    );
}
