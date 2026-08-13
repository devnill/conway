//! Board item 01KZFC1KNGQ51TZ0BG7P7RAY9H: `conway routes explain` must stay
//! honest when the caller supplied its own `Router`
//! (`conway::ConwayBuilder::with_router`) rather than letting `Conway`
//! compile its own `DeclarativeRouter`. Before this item, that path made
//! `Conway::explain_routing` fall back to a fabricated-empty report, which
//! `commands::routes::run` then misread as "unknown role" for a
//! correctly-configured one -- a silent behavioral inversion, not a compile
//! error.
//!
//! This can only be driven in-process: `tests/subcommands.rs`'s
//! `run_conway` spawns the real compiled `conway` binary (see
//! `tests/common/mod.rs`), which has no flag to inject a `Router` --
//! `ConwayBuilder::with_router` is a library-only call. This file therefore
//! calls `conway_cli::commands::routes::run` directly against a `Conway`
//! built with `ConwayBuilder::from_parts(..).with_router(..)`, reusing the
//! same fake-port pattern `conway_cli::tui::app`'s own in-crate test module
//! already uses (`conway_core::fakes::{FakeBackend, FakeGate, FakeRouter,
//! FakeStore}`).

use std::collections::BTreeMap;
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, BackendEntry, ConwayConfig, HealthSection, HooksConfig, LimitsConfig,
    ModelsConfig, PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig,
    ToolsConfig, TuiSection,
};
use conway::{Conway, ConwayBuilder, PermissionGate};
use conway_cli::commands::routes::{run, RoutesAction, RoutesArgs};
use conway_cli::exit::ExitCode;
use conway_core::agent::PermissionDecision;
use conway_core::fakes::{FakeBackend, FakeGate, FakeRouter, FakeStore};
use conway_core::ids::{BackendId, ModelId};

/// A `ConwayConfig` declaring `role` (with a real chain entry) plus
/// `"empty-chain"` (present in `[roles]` but with an EMPTY chain), otherwise
/// as bare as `ConwayConfig` allows -- mirrors `conway_cli::tui::app`'s own
/// `#[cfg(test)]` `base_config()` helper (not reusable here: that one is
/// private to the lib crate's own test module, not `pub`).
///
/// `"empty-chain"` exists to distinguish the two ways `MinimalRouter::explain`
/// can return zero entries: an unconfigured role (genuinely `UnknownRole`)
/// versus a configured role whose chain happens to be empty. Ordinarily
/// `crate::config::validate` (`conway-routing`) rejects an empty chain on
/// ANY role at `DeclarativeRouter::new` time -- but that validation runs
/// only in `ConwayBuilder::build`'s "compile our own router" branch (step
/// 7), never when a `Router` is injected via `with_router`, so an
/// empty-chain role is reachable here precisely because this suite exists
/// to test that configuration. `report.entries.is_empty()` cannot tell
/// these two apart; `conway.config().roles.contains_key(..)` can.
fn config_with_role(role: &str) -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        role.to_string(),
        RoleEntry {
            chain: vec!["fake/echo-model".to_string()],
            ..Default::default()
        },
    );
    roles.insert("empty-chain".to_string(), RoleEntry::default());
    // `config::merge::validate` (called from `ConwayBuilder::build`'s step
    // 1) rejects a chain entry naming a backend id absent from
    // `[backends]`, regardless of whether a real `Backend` was later
    // injected via `with_backend` -- so `"fake"` needs a config-side entry
    // too, even though `build_conway` below overwrites the backend it
    // constructs from this entry with the injected `FakeBackend` (same id,
    // last insert wins -- `ConwayBuilder::build`'s step 3+4). `api_key` is
    // a throwaway placeholder: `conway_plugin_backends::AnthropicBackend::new`
    // rejects an empty one, but this backend is never actually dialed --
    // it is immediately shadowed by the injected `FakeBackend` sharing its
    // id.
    let mut backends = BTreeMap::new();
    backends.insert(
        "fake".to_string(),
        BackendEntry {
            api_key: "unused-placeholder-key".to_string(),
            ..BackendEntry::default()
        },
    );
    ConwayConfig {
        default_role: conway::RoleAlias::new(role),
        cwd: std::path::PathBuf::from("."),
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        permissions: PermissionsConfig::default(),
        backends,
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

/// A fully in-memory `Conway` built with every port injected, `router`
/// included -- `ConwayBuilder::build` therefore leaves `router_explain:
/// None` (`builder.rs`, step 7), which is exactly the case under test:
/// `Conway::explain_routing` has no concrete `DeclarativeRouter` to project
/// through and must fall back to `conway_core::routing::MinimalRouter`.
fn build_conway(role: &str) -> Conway {
    let backend: Arc<dyn conway::Backend> = Arc::new(FakeBackend::echo(BackendId::new("fake")));
    let gate: Arc<dyn PermissionGate> = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let router: Arc<dyn conway::Router> = Arc::new(FakeRouter::single(conway::ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }));

    ConwayBuilder::from_parts(config_with_role(role))
        .with_backend(backend)
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(gate)
        .with_router(router)
        // Board item 01KZHF270T3W8GZ7NM6DSNQ4MM: `conway` no longer
        // compiles the `"fake"` entry's default `kind = "anthropic"` in
        // (overwritten by the injected `FakeBackend` above, but still
        // resolved by `build()` before that overwrite happens).
        .with_backend_factory(Arc::new(conway_plugin_backends::AnthropicBackendFactory))
        .build()
        .expect("build should succeed with every port injected")
}

fn explain_args(role: &str) -> RoutesArgs {
    RoutesArgs {
        action: RoutesAction::Explain {
            role: role.to_string(),
            json: false,
        },
    }
}

/// Redirects the real process `stderr` fd to an anonymous temp file for the
/// duration of `fut`, then restores it and returns whatever text landed
/// there -- `diag::error` (`commands::routes::run`'s only stderr writer)
/// goes straight to `std::io::stderr()`, not through any injectable sink,
/// so this is the only way to observe it from inside the same process. Unix
/// only (`nix::unistd::dup`/`dup2`/`close`, already a dev-dependency of this
/// crate for `tests/oneshot.rs`'s SIGINT suite) -- safe here because this
/// file's tests run sequentially in one `#[tokio::test]` function below, so
/// there is no concurrent writer to race against the redirected fd. `async`
/// (not a plain closure) because `fut` must be polled on THIS task rather
/// than through a nested `block_on`, which would panic ("cannot start a
/// runtime from within a runtime") given `#[tokio::test]` already has one
/// running.
#[cfg(unix)]
async fn capture_stderr<F: std::future::Future<Output = R>, R>(fut: F) -> (R, String) {
    use std::io::{Read, Seek, SeekFrom, Write as _};
    use std::os::fd::AsRawFd;

    let mut tmp = tempfile::tempfile().expect("create anonymous temp file for stderr capture");
    let _ = std::io::stderr().flush();
    let saved = nix::unistd::dup(2).expect("dup saved stderr fd");
    nix::unistd::dup2(tmp.as_raw_fd(), 2).expect("redirect stderr to temp file");

    let result = fut.await;

    let _ = std::io::stderr().flush();
    nix::unistd::dup2(saved, 2).expect("restore stderr fd");
    let _ = nix::unistd::close(saved);

    tmp.seek(SeekFrom::Start(0)).expect("seek captured stderr");
    let mut captured = String::new();
    tmp.read_to_string(&mut captured)
        .expect("read captured stderr");
    (result, captured)
}

#[cfg(unix)]
#[tokio::test]
async fn injected_router_explain_stays_honest_for_configured_and_unknown_roles() {
    let conway = build_conway("primary");

    // A configured role: `Conway::explain_routing` must fall back to
    // `MinimalRouter`'s honest degenerate report, not the old
    // fabricated-empty one -- `commands::routes::run` must not print
    // "unknown role" or exit `Usage` for it.
    let (result, stderr) = capture_stderr(run(&explain_args("primary"), &conway)).await;
    let code = result.expect("routes explain must not error for a configured role");
    assert_ne!(
        code,
        ExitCode::Usage,
        "a configured role must not report unknown role"
    );
    assert!(
        !stderr.contains("unknown role"),
        "a configured role's stderr must not mention 'unknown role', got: {stderr:?}"
    );

    // An unconfigured role: still reported as `unknown role`, listing the
    // configured roles -- this is what `commands::routes::run` now reads
    // from `conway.config().roles` directly rather than inferring from
    // `report.entries.is_empty()`.
    let (result, stderr) = capture_stderr(run(&explain_args("no-such-role"), &conway)).await;
    let code = result.expect("routes explain must not error for an unknown role either");
    assert_eq!(
        code,
        ExitCode::Usage,
        "an unconfigured role must report unknown role"
    );
    assert!(
        stderr.contains("unknown role"),
        "an unconfigured role's stderr must mention 'unknown role', got: {stderr:?}"
    );
    assert!(
        stderr.contains("primary"),
        "the unknown-role message must list the configured roles, got: {stderr:?}"
    );

    // A configured role whose chain is empty: `MinimalRouter::explain`
    // honestly reports zero entries for it too (there is nothing to
    // iterate), but it is NOT unknown -- `conway.config().roles` says so
    // directly. `report.entries.is_empty()` cannot distinguish this from
    // the unconfigured case above; this is the scenario P-15's
    // break-the-guard check restores and re-fails against (see this item's
    // own report).
    let (result, stderr) = capture_stderr(run(&explain_args("empty-chain"), &conway)).await;
    let code = result.expect("routes explain must not error for a configured-but-empty role");
    assert_ne!(
        code,
        ExitCode::Usage,
        "a configured role with an empty chain must not report unknown role"
    );
    assert!(
        !stderr.contains("unknown role"),
        "a configured-but-empty role's stderr must not mention 'unknown role', got: {stderr:?}"
    );
}
