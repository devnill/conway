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
use conway::test_support::build_conway_with_echo_backend;
use conway::Conway;
use conway_testkit::FakeStore;
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
pub(super) fn echo_conway() -> Conway {
    build_conway_with_echo_backend(base_config(), Arc::new(FakeStore::new()))
}

/// Mirrors [`echo_conway`], accepting a caller-supplied
/// `ConwayConfig` instead of always [`base_config`] -- for a test that
/// needs to vary a config field (e.g. `plugins.install`) while keeping
/// every other port the same fully in-memory shape.
pub(super) fn conway_over_config(config: ConwayConfig) -> Conway {
    build_conway_with_echo_backend(config, Arc::new(FakeStore::new()))
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

/// Mirrors [`echo_conway`], additionally handing
/// back the [`FakeStore`] so a test can read the persisted log directly
/// -- the same shape `conway-plugin-skeleton`'s own
/// `tests/skeleton_end_to_end.rs::build_conway` uses, reached here as
/// this crate's own in-file test helper since `App`'s private `submit`/
/// `apply_plugin_command_done`/`plugin_cmd_rx` are only reachable from
/// THIS crate's own test code, never from an external
/// `crates/conway-cli/tests/*.rs` integration file.
pub(super) fn echo_conway_and_store() -> (Conway, Arc<FakeStore>) {
    let store = Arc::new(FakeStore::new());
    let conway = echo_conway_over(store.clone());
    (conway, store)
}

/// A fresh `Conway`/`Runtime` (own tree, own in-memory state) over an
/// ALREADY-EXISTING store -- the "simulated restart" shape
/// `resuming_a_session_refreshes_its_own_head_seq` needs: two
/// independent runtimes sharing one persisted log, mirroring
/// `crates/conway/tests/resume.rs`'s own `build_conway`/`resume` tests.
pub(super) fn echo_conway_over(store: Arc<FakeStore>) -> Conway {
    build_conway_with_echo_backend(base_config(), store)
}

/// A plugin with no `status_contributions()` override contributes nothing
/// -- the trait's own zero-cost default, and every other fixture plugin in
/// this crate's test suites. Only a plugin that overrides it produces a
/// contribution, which is why this fixture exists as its own type -- the
/// SAME shape `app/startup.rs`'s own `ContributingPlugin` fixture uses (a
/// second, independent copy rather than a shared export: the two modules'
/// suites do not otherwise depend on each other, and six lines is cheaper
/// than the coupling).
pub(super) struct ContributingPlugin;

impl conway::plugin::Plugin for ContributingPlugin {
    fn manifest(&self) -> conway::plugin::PluginManifest {
        conway::plugin::PluginManifest {
            id: "test.guard".to_string(),
            version: "0.0.0".to_string(),
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn conway::plugin::Tool>> {
        vec![]
    }

    fn status_contributions(&self) -> Vec<conway::plugin::PluginStatusContribution> {
        vec![conway::plugin::PluginStatusContribution {
            key: "guard".to_string(),
            status: conway::ResultStatus::Completed,
            value: "qwen2.5-3b".to_string(),
        }]
    }
}

/// Mirrors [`echo_conway_and_store`], with [`ContributingPlugin`] installed
/// through the REAL `ConwayBuilder::with_plugin` -- board item
/// `01M0XDEDBR5YDF71Q7ZRXYMT85`'s own end-to-end proof that a plugin's
/// status-contribution snapshot survives `/resume` needs the shared-store
/// "simulated restart" shape [`echo_conway_over`] already establishes,
/// PLUS a plugin that actually contributes something for `App::new` to
/// snapshot in the first place.
pub(super) fn conway_with_contributing_plugin_and_store() -> (Conway, Arc<FakeStore>) {
    let store = Arc::new(FakeStore::new());
    let conway = conway::test_support::test_builder(base_config())
        .with_backend(Arc::new(conway_testkit::FakeBackend::echo(
            conway_core::ids::BackendId::new("fake"),
        )))
        .with_plugin(Arc::new(ContributingPlugin))
        .with_session_store(store.clone())
        .build()
        .expect("build should succeed with a status-contributing plugin installed");
    (conway, store)
}
