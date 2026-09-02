//! Plugin-declared permission modes reached through the REAL
//! `ConwayBuilder::build()` facade (board item `01M0X4YDNVP7TZ0PVSRJ0388SS`,
//! design `docs/vision/DESIGN-permission-modes.md` §2c/§3b/§3d/§6b).
//!
//! `conway_runtime::permission_mode`'s own test module already pins the
//! cycle algebra -- ordering, collisions, uninstall reconciliation -- over
//! hand-built `ModeCycle`s. This file pins the part that algebra cannot
//! see: that a mode a plugin actually declares survives `build()`, reaches
//! `Conway::mode_cycle`, and that `Conway::cycle_permission_mode` moves
//! the BROKER, not merely a display field.
//!
//! That distinction is the whole reason this file exists. A cycle that
//! walks correctly while the broker stays put would pass every test in
//! `permission_mode.rs` and still leave the operator with a status line
//! naming a mode that gates nothing.

use std::collections::BTreeMap;
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, BackendEntry, ConwayConfig, HealthSection, HooksConfig, LimitsConfig,
    ModelsConfig, PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig,
    ToolsConfig,
};
use conway::plugin::{PluginManifest, Tool};
use conway::{Conway, ConwayBuilder, ModeCycleEntry, PermissionMode, Plugin, PluginDeclaredMode};
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

/// A `Plugin` fake that declares one permission mode.
struct ModeDeclaringPlugin {
    id: &'static str,
    mode_name: &'static str,
    base: PermissionMode,
}

impl Plugin for ModeDeclaringPlugin {
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

    fn permission_modes(&self) -> Vec<PluginDeclaredMode> {
        vec![PluginDeclaredMode {
            name: self.mode_name.to_string(),
            base: self.base,
        }]
    }
}

fn build_with(plugins: Vec<Arc<dyn Plugin>>) -> Conway {
    let mut cfg = base_config();
    cfg.plugins.install = plugins.iter().map(|p| p.manifest().id).collect();

    ConwayBuilder::from_parts(cfg)
        .install_selected(plugins, vec![], vec![])
        .expect("every id resolves against the supplied bundle")
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .build()
        .expect("a mode-declaring plugin must build cleanly")
}

/// The baseline that must not regress: with nothing declaring a mode, the
/// cycle is exactly the three closed core modes, in their fixed order.
#[test]
fn a_build_with_no_declaring_plugin_cycles_exactly_the_three_core_modes() {
    let conway = build_with(vec![]);
    let cycle = conway.mode_cycle();

    assert_eq!(
        cycle.entries(),
        &[
            ModeCycleEntry::Core(PermissionMode::Prompt),
            ModeCycleEntry::Core(PermissionMode::Plan),
            ModeCycleEntry::Core(PermissionMode::AutoAllow),
        ],
        "an unchanged build must cycle exactly as it always has"
    );
    assert!(cycle.collisions().is_empty());
}

/// The claim this whole feature rests on: a declared mode does not merely
/// APPEAR in the cycle -- cycling onto it moves the broker, which is what
/// actually gates calls. A version that updated only the display identity
/// would pass a naive "is the name in the cycle" test and leave the
/// operator in a mode that enforces the wrong thing.
#[test]
fn cycling_onto_a_declared_mode_moves_the_broker_not_just_the_label() {
    let conway = build_with(vec![Arc::new(ModeDeclaringPlugin {
        id: "acme.permissions",
        mode_name: "auto-gated",
        base: PermissionMode::AutoAllow,
    }) as Arc<dyn Plugin>]);

    assert_eq!(conway.permission_mode(), PermissionMode::Prompt);
    assert_eq!(conway.active_declared_mode(), None);

    // Prompt -> Plan -> AutoAllow -> auto-gated: the declared entry sorts
    // after every core mode, so three steps land on it.
    let mut landed = None;
    for _ in 0..4 {
        let entry = conway.cycle_permission_mode();
        if let ModeCycleEntry::Declared { name, .. } = &entry {
            landed = Some(name.clone());
            break;
        }
    }
    assert_eq!(
        landed.as_deref(),
        Some("auto-gated"),
        "the declared mode must be reachable by cycling, not merely listed"
    );

    assert_eq!(
        conway.permission_mode(),
        PermissionMode::AutoAllow,
        "the BROKER is on the declared mode's base -- this is what gates calls"
    );
    let active = conway
        .active_declared_mode()
        .expect("the display identity is recorded too");
    assert_eq!(active.plugin_id, "acme.permissions");
    assert_eq!(active.name, "auto-gated");
}

/// A `Plan`-based declared mode may narrow; it may never widen. The base
/// is the ONLY field enforcement reads, so a plan-based declared mode
/// leaves the broker in `Plan` -- and plan mode's guarantee is untouched
/// by anything the declaring plugin can say.
#[test]
fn a_plan_based_declared_mode_leaves_the_broker_in_plan() {
    let conway = build_with(vec![Arc::new(ModeDeclaringPlugin {
        id: "acme.review",
        mode_name: "review-only",
        base: PermissionMode::Plan,
    }) as Arc<dyn Plugin>]);

    let mut landed = false;
    for _ in 0..4 {
        if matches!(
            conway.cycle_permission_mode(),
            ModeCycleEntry::Declared { .. }
        ) {
            landed = true;
            break;
        }
    }
    assert!(landed, "the declared mode must be reachable");
    assert_eq!(
        conway.permission_mode(),
        PermissionMode::Plan,
        "a declared mode enforces exactly its base, never something more permissive"
    );
}

/// Two plugins declaring the same name: the colliding name is excluded
/// from the cycle rather than silently resolved to one of them, and the
/// collision is reported naming both. Silently picking a winner would
/// give the operator a mode whose behaviour depends on install order.
#[test]
fn two_plugins_declaring_one_name_collide_without_a_silent_winner() {
    let conway = build_with(vec![
        Arc::new(ModeDeclaringPlugin {
            id: "acme.one",
            mode_name: "guarded",
            base: PermissionMode::AutoAllow,
        }) as Arc<dyn Plugin>,
        Arc::new(ModeDeclaringPlugin {
            id: "acme.two",
            mode_name: "guarded",
            base: PermissionMode::Plan,
        }) as Arc<dyn Plugin>,
    ]);

    let cycle = conway.mode_cycle();
    assert_eq!(
        cycle.entries().len(),
        3,
        "the colliding name is in NEITHER plugin's favour: {:?}",
        cycle.entries()
    );

    let collisions = cycle.collisions();
    assert_eq!(collisions.len(), 1);
    let described = collisions[0].describe();
    assert!(
        described.contains("acme.one") && described.contains("acme.two"),
        "the operator must be told which two plugins collided: {described}"
    );
}
