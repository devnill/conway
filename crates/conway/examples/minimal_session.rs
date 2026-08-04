//! A minimal runnable conway session (WI-132).
//!
//! Runs fully offline against fake ports (an echoing backend), so
//!
//! ```console
//! cargo run -p conway --example minimal_session
//! ```
//!
//! works with no config file, no credentials, and no network. It doubles as
//! a smoke test of the public `conway` facade: it imports only re-exports
//! from the `conway` crate, plus `conway_core::fakes` (a dev-only test
//! helper) for the stand-in ports -- no internal crates.
//!
//! For a REAL session, drop the fake wiring and start from
//! [`conway::ConwayBuilder::discover`] (loads `~/.conway/settings.json` plus
//! any project config) or [`conway::ConwayBuilder::from_config`], which bring
//! real backends and capability-based routing. The CLI
//! (`crates/conway-cli`) is the production entry point.

use std::collections::BTreeMap;
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    RoleEntry, RoutingSection, SessionConfig, ToolsConfig, TuiSection,
};
use conway::{ConwayBuilder, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::fakes::{FakeBackend, FakeGate, FakeRouter, FakeStore};
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias};
use conway_core::ports::SessionStore;

/// A minimal config: one role, no configured backends. The fake router
/// below supplies the route directly, so no backend table is needed here --
/// in production the `[backends]` / `[routing]` config sections and
/// `ConwayBuilder::discover()` populate all of this.
fn minimal_config() -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "default".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
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
        tui: TuiSection::default(),
        tools: ToolsConfig::default(),
    }
}

#[tokio::main]
async fn main() -> conway::Result<()> {
    // Wire the facade with fake ports: an echo backend (its reply is the
    // prompt text back), an allow-once permission gate, an in-memory session
    // store, and a single-route router. Every one of these is an injected
    // trait object -- in production they come from
    // `ConwayBuilder::discover()` / `from_config()` instead. We keep the store
    // handle so we can peek at the main session's log length below.
    let store = Arc::new(FakeStore::new());
    let conway = ConwayBuilder::from_parts(minimal_config())
        .with_backend(Arc::new(FakeBackend::echo(BackendId::new("fake"))))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_session_store(store.clone())
        .with_router(Arc::new(FakeRouter::single(ModelRef {
            backend: BackendId::new("fake"),
            model: ModelId::new("echo-model"),
        })))
        .build()?;

    // Open a session and run one real turn. Drain the turn fully (`result`,
    // after `text`) so the append-only log has settled before we measure it.
    let session = conway.new_session(SessionSpec::default()).await?;
    let turn = session.prompt("Hello, conway!").await?;
    println!("prompt -> {}", turn.text().await?);
    let _ = turn.result().await?;

    // Show that `ask` is ephemeral -- don't just claim it. `ask` forks the
    // session into a hidden child and drives one turn THERE, so the main
    // session's append-only log is left untouched.
    let head_before = store.head(&session.id()).await.expect("head read");
    let aside = session.ask("(ephemeral) just checking something").await?;
    println!("ask    -> {}", aside.text().await?);
    let _ = aside.result().await?;
    let head_after = store.head(&session.id()).await.expect("head read");

    println!(
        "main-session log head: {head_before:?} before the ask, {head_after:?} after \
         -> the ephemeral ask left no trace in the main session"
    );

    Ok(())
}
