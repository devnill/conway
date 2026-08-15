//! Test-only builders shared across `app`'s split-out test modules --
//! `startup`, `plugin_cmd`, `focus`, and `app.rs`'s own `submit`-driving
//! tests all construct the same fully in-memory `Conway` (the fake port set
//! `conway`'s own `tests/session_handle.rs` builds), the same minimal `Cli`,
//! and the same buffered-envelope drain. Extracted verbatim out of `app.rs`'s
//! former single `mod tests` (this item, board) rather than duplicated per
//! file -- the sibling `state.rs` split's own `fixtures` module for the same
//! reason (see its own doc).

use std::collections::BTreeMap;
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::{Conway, ConwayBuilder, PermissionGate};
use conway_core::agent::PermissionDecision;
use conway_core::ids::{BackendId, ModelId};
use conway_testkit::{FakeBackend, FakeGate, FakeRouter, FakeStore};
use futures::Stream as _;

use crate::cli::{Cli, OutputFormat};
use crate::tui::state::AppState;

pub(super) fn base_config() -> ConwayConfig {
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
        default_role: conway::RoleAlias::new("default"),
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
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

/// An echoing, fully in-memory `Conway`: its backend replies with
/// exactly the last user-role segment's text, so a submitted prompt's
/// round trip is deterministic and needs no real network/model.
pub(super) fn build_conway_with_echo_backend() -> Conway {
    let backend: Arc<dyn conway::Backend> = Arc::new(FakeBackend::echo(BackendId::new("fake")));
    let gate: Arc<dyn PermissionGate> = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let router: Arc<dyn conway::Router> = Arc::new(FakeRouter::single(conway::ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }));
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend)
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(gate)
        .with_router(router)
        .build()
        .expect("build should succeed with every port injected")
}

pub(super) fn minimal_cli() -> Cli {
    Cli {
        print: None,
        output_format: OutputFormat::Text,
        allowed_tools: Vec::new(),
        deny_tools: Vec::new(),
        permission_mode: crate::cli::PermissionMode::Allowlist,
        role_override: None,
        model: None,
        agent: None,
        system_prompt: None,
        append_system_prompt: None,
        max_turns: None,
        max_tokens: None,
        max_seconds: None,
        output_schema: None,
        session: None,
        resume: None,
        fork_from: None,
        config: None,
        cwd: None,
        root: None,
        verbose: 0,
        command: None,
    }
}

/// Drains every envelope currently buffered on `events` (never blocks
/// past the first `Poll::Pending`) and applies each to `state` -- the
/// same `apply` call `App::run`'s own select-loop makes for every
/// envelope it polls, just without the terminal/crossterm half.
pub(super) fn drain_and_apply(events: &mut conway::EventStream, state: &mut AppState) {
    let waker = futures::task::noop_waker();
    let mut cx = std::task::Context::from_waker(&waker);
    while let std::task::Poll::Ready(Some(env)) =
        std::pin::Pin::new(&mut *events).poll_next(&mut cx)
    {
        state.apply(&env);
    }
}

/// Mirrors [`build_conway_with_echo_backend`], additionally handing
/// back the [`FakeStore`] so a test can read the persisted log directly
/// -- the same shape `conway-plugin-skeleton`'s own
/// `tests/skeleton_end_to_end.rs::build_conway` uses, reached here as
/// this crate's own in-file test helper since `App`'s private `submit`/
/// `apply_plugin_command_done`/`plugin_cmd_rx` are only reachable from
/// THIS crate's own test code, never from an external
/// `crates/conway-cli/tests/*.rs` integration file.
pub(super) fn build_conway_with_echo_backend_and_store() -> (Conway, Arc<FakeStore>) {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend_over(store.clone());
    (conway, store)
}

/// A fresh `Conway`/`Runtime` (own tree, own in-memory state) over an
/// ALREADY-EXISTING store -- the "simulated restart" shape
/// `resuming_a_session_refreshes_its_own_head_seq` needs: two
/// independent runtimes sharing one persisted log, mirroring
/// `crates/conway/tests/resume.rs`'s own `build_conway`/`resume` tests.
pub(super) fn build_conway_with_echo_backend_over(store: Arc<FakeStore>) -> Conway {
    let backend: Arc<dyn conway::Backend> = Arc::new(FakeBackend::echo(BackendId::new("fake")));
    let gate: Arc<dyn PermissionGate> = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let router: Arc<dyn conway::Router> = Arc::new(FakeRouter::single(conway::ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }));
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(router)
        .build()
        .expect("build should succeed with every port injected")
}
