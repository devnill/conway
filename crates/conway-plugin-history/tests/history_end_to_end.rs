//! This crate's own facade-level acceptance test, written the way a
//! library embedder would write it -- `ConwayBuilder` plus the fakes family
//! (no live provider, no network), mirroring
//! `conway-plugin-skeleton/tests/skeleton_end_to_end.rs`'s own shape.
//!
//! **Why this file does NOT (and cannot) drive `/conway.history.rewind`
//! through a real turn the way the skeleton's own end-to-end test drives
//! `skeleton_ping`.** A `Tool` is called BY THE MODEL, through
//! `Conway`/`SessionHandle` -- the facade owns that whole path, so a fake
//! backend scripted to call it is enough to prove real dispatch. A
//! `Command` is called BY THE OPERATOR, through the TUI's own
//! `commands::parse`/`execute`/`CommandRegistry` -- none of which live in
//! this crate's one dependency, `conway` (`docs/plugins/hooks.md` point
//! 15's own doc: "`Plugin`/`Command` live in `conway-core`, which
//! structurally cannot depend on `conway`... without a cycle"; dispatch
//! lives one layer up again, in `conway-cli`). So the REAL end-to-end
//! proof -- installed vs. not, unknown-command vs. dispatched, and the
//! parent session's log staying byte-for-byte unchanged after a fork --
//! is `crates/conway-cli/tests/rewind_history_plugin.rs`, against the
//! actual compiled dispatch path this crate is linked into
//! (`crates/conway-cli/src/first_party_plugins.rs`). What THIS file proves
//! instead is the boundary this crate itself owns: the plugin builds
//! cleanly with a real `Conway`, and its one command is reachable and
//! correct through the exact `Arc<dyn Command>` type erasure a host
//! actually stores it behind (`CommandRegistry`'s own field), not merely
//! through the concrete `RewindCommand` type this crate's own `src/lib.rs`
//! unit tests already exercise directly.

use std::collections::BTreeMap;
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::plugin::{Command, CommandCtx, CommandOutcome, Plugin as _};
use conway::LogSeq;
use conway_core::ids::{BackendId, RoleAlias};
use conway_testkit::FakeBackend;

use conway::test_support::test_builder;
use conway_plugin_history::{
    HistoryPlugin, COMMAND_NAME_CHECKOUT, COMMAND_NAME_MASK, COMMAND_NAME_REWIND, PLUGIN_ID,
};

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
        // Deliberately empty, same as `conway-plugin-skeleton`'s own test:
        // `[plugins].install` is read by whatever BINARY links this crate
        // (`conway-cli`'s `first_party_plugins.rs`); a library embedder
        // instead attaches directly via `with_plugin`, below.
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

/// A real `Conway` build succeeds with this plugin installed -- the same
/// "no stub, no special case" property `PluginManifest` validation would
/// otherwise catch (a malformed manifest, a tool-name collision) fails
/// `build()` outright rather than silently.
#[test]
fn conway_builds_with_history_plugin_installed() {
    let _conway = test_builder(base_config())
        .with_backend(
            Arc::new(FakeBackend::echo(BackendId::new("fake"))) as Arc<dyn conway::Backend>
        )
        .with_plugin(Arc::new(HistoryPlugin))
        .build()
        .expect("build should succeed with HistoryPlugin installed and every port injected");
}

/// The plugin's manifest id matches the published constant a config author
/// (or `conway-cli`'s own bundle) resolves `[plugins].install` entries
/// against.
#[test]
fn manifest_id_matches_the_published_constant() {
    assert_eq!(HistoryPlugin.manifest().id, PLUGIN_ID);
}

/// Reaches this plugin's one command only through the `Arc<dyn Command>`
/// type erasure `Plugin::commands()` returns -- the exact shape
/// `conway_cli::tui::commands::CommandRegistry` stores it behind -- rather
/// than this crate's own concrete type, proving the ONE public entry point
/// a host actually uses is wired correctly end to end, not merely that the
/// concrete struct's own inherent method works.
#[tokio::test]
async fn the_erased_command_still_forks_at_the_typed_sequence() {
    let plugin = HistoryPlugin;
    let commands: Vec<Arc<dyn Command>> = plugin.commands();
    assert_eq!(commands.len(), 3);
    assert_eq!(commands[0].spec().name, COMMAND_NAME_REWIND);

    let ctx = CommandCtx {
        focused_agent: conway::AgentId::new(),
        root_agent: conway::AgentId::new(),
        session_id: conway::SessionId::new(),
        args: "5".to_string(),
    };
    let outcome = commands[0].invoke(ctx).await;
    assert_eq!(
        outcome,
        CommandOutcome::ForkSession {
            at_seq: LogSeq(5),
            directive: String::new(),
        }
    );
}

/// This plugin's SECOND command, reached only through the same `Arc<dyn
/// Command>` type erasure -- `/conway.history.mask <seq>` returns a
/// `CommandOutcome::MaskRecord` requesting the CALLING session's own
/// `target_seq` be excluded.
#[tokio::test]
async fn the_erased_mask_command_returns_a_mask_record_outcome() {
    let plugin = HistoryPlugin;
    let commands: Vec<Arc<dyn Command>> = plugin.commands();
    assert_eq!(commands[1].spec().name, COMMAND_NAME_MASK);

    let ctx = CommandCtx {
        focused_agent: conway::AgentId::new(),
        root_agent: conway::AgentId::new(),
        session_id: conway::SessionId::new(),
        args: "3".to_string(),
    };
    let outcome = commands[1].invoke(ctx).await;
    assert_eq!(
        outcome,
        CommandOutcome::MaskRecord {
            target_seq: LogSeq(3),
            excluded: true,
        }
    );
}

/// This plugin's THIRD command, reached only through the same `Arc<dyn
/// Command>` type erasure -- `/conway.history.checkout <session-id>`
/// returns a `CommandOutcome::Checkout` naming the typed session.
#[tokio::test]
async fn the_erased_checkout_command_returns_a_checkout_outcome() {
    let plugin = HistoryPlugin;
    let commands: Vec<Arc<dyn Command>> = plugin.commands();
    assert_eq!(commands[2].spec().name, COMMAND_NAME_CHECKOUT);

    let target = conway::SessionId::new();
    let ctx = CommandCtx {
        focused_agent: conway::AgentId::new(),
        root_agent: conway::AgentId::new(),
        session_id: conway::SessionId::new(),
        args: target.to_string(),
    };
    let outcome = commands[2].invoke(ctx).await;
    assert_eq!(outcome, CommandOutcome::Checkout { target });
}
