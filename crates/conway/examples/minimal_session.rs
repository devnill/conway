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
    RoleEntry, RoutingSection, SessionConfig,
};
use conway::{ConwayBuilder, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::fakes::{FakeBackend, FakeGate, FakeRouter, FakeStore};
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias};

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
    }
}

#[tokio::main]
async fn main() -> conway::Result<()> {
    // Wire the facade with fake ports: an echo backend (its reply is the
    // prompt text back), an allow-once permission gate, an in-memory session
    // store, and a single-route router. Every one of these is an injected
    // trait object -- in production they come from
    // `ConwayBuilder::discover()` / `from_config()` instead.
    let conway = ConwayBuilder::from_parts(minimal_config())
        .with_backend(Arc::new(FakeBackend::echo(BackendId::new("fake"))))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_router(Arc::new(FakeRouter::single(ModelRef {
            backend: BackendId::new("fake"),
            model: ModelId::new("echo-model"),
        })))
        .build()?;

    // Open a live, multi-turn session.
    let session = conway.new_session(SessionSpec::default()).await?;

    // A normal turn: prompt in, streamed reply out.
    let turn = session.prompt("Hello, conway!").await?;
    println!("prompt -> {}", turn.text().await?);

    // `ask` runs an EPHEMERAL forked turn: it inherits the session's context
    // but its question and answer are discarded afterward, so a quick
    // side-question never pollutes the main transcript.
    let aside = session.ask("(ephemeral) just checking something").await?;
    println!("ask    -> {}", aside.text().await?);

    Ok(())
}
