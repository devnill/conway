//! Smoke test for the `discover_getting_started` example's facade flow,
//! mirroring `example_smoke.rs`'s own precedent for `minimal_session`: the
//! example itself is only compile-checked by `cargo build --examples`; this
//! test exercises the same public-facade path at runtime.
//!
//! **This is the isolated stand-in for the example's own
//! `ConwayBuilder::discover()` call.** `crates/conway/tests/
//! config_isolation_guard.rs` forbids calling `discover()` (or
//! `LoadOptions::default()`) from an in-process test -- both read THIS
//! process's real `$XDG_CONFIG_HOME`/`~/.conway/settings.json` and real
//! `std::env::vars()`, which would make this test's outcome depend on
//! whatever happens to be on the machine running it. `config::load` with a
//! hermetic `LoadOptions` (`support::isolated_env`, an empty cwd with no
//! `.conway/settings.json` reachable by walking up from it) is the
//! isolated equivalent: it exercises the SAME five-source precedence chain
//! `discover()` runs, deterministically landing on the same built-in
//! defaults (`config::merge::default_document`) a real host gets when
//! neither an XDG nor a project config file exists.
//!
//! Every `.await` is wrapped in a short `tokio::time::timeout`, matching
//! `example_smoke.rs`'s own discipline, so a hang fails the test quickly
//! rather than blocking forever.

mod support;

use std::sync::Arc;
use std::time::Duration;

use conway::backend::{BackendId, ModelId};
use conway::config::{load, CliOverrides, LoadOptions};
use conway::{ConwayBuilder, ModelRef, PluginSelection, SessionSpec};
use conway_core::agent::ResultStatus;
use conway_testkit::{FakeBackend, FakeRouter, FakeStore, ScriptedBackend};

const T: Duration = Duration::from_secs(5);

#[tokio::test]
async fn discover_getting_started_example_flow_reaches_an_answer() {
    let cwd = support::unique_temp_dir("discover-getting-started");

    // The isolated equivalent of the example's own `ConwayBuilder::
    // discover()?` -- see this file's module doc for why `discover()`
    // itself cannot be called here.
    let outcome = load(LoadOptions {
        cwd,
        explicit_path: None,
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .expect("load with no XDG/project layer must still succeed via built-in defaults");

    // Confirms the premise the example's own doc states in prose: with
    // nothing on disk, the discovery chain lands on the documented built-in
    // default document, not an error.
    assert_eq!(outcome.config.default_role.as_str(), "default");
    assert!(outcome.warnings.is_empty());

    let backend = Arc::new(FakeBackend::echo(BackendId::new("fake")));
    let route = ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    };

    // The exact same builder chain the example itself uses after
    // `discover()?` -- see that example's own comments for why each call is
    // here.
    let conway = ConwayBuilder::from_parts(outcome.config)
        .with_cli_overrides(CliOverrides {
            permission_mode: Some("deny".to_string()),
            ..CliOverrides::default()
        })
        .with_builtin_plugins(PluginSelection::None)
        .with_backend(backend)
        .with_router(Arc::new(FakeRouter::single(route)))
        .with_session_store(Arc::new(FakeStore::new()))
        .build()
        .expect("build should succeed with only the two ports discovery cannot supply injected");

    let session = tokio::time::timeout(T, conway.new_session(SessionSpec::default()))
        .await
        .expect("new_session must not hang")
        .expect("new_session should succeed");
    let turn = tokio::time::timeout(T, session.prompt("Hello, conway!"))
        .await
        .expect("prompt must not hang")
        .expect("prompt should succeed");
    let text = tokio::time::timeout(T, turn.text())
        .await
        .expect("text must not hang")
        .expect("text should succeed");
    assert_eq!(
        text, "Hello, conway!",
        "echo backend returns the prompt text"
    );
    let _ = tokio::time::timeout(T, turn.result())
        .await
        .expect("result must not hang")
        .expect("result should succeed");
}

/// The pairing the finding (board item 01M02CWXP4846SX97KNW35501S) asks
/// for: the test above proves the shipped default's NAME
/// (`default_role.as_str() == "default"`); this one proves the property
/// that must survive whatever that name is -- an unmodified default
/// routes NOWHERE, loudly, rather than accidentally being given a working
/// chain by the same rename. Unlike the test above, `.with_router(..)` is
/// NOT called here: the point is to exercise the REAL router
/// `ConwayBuilder::build` compiles from `roles.default.chain = []`
/// (`conway_core::routing::MinimalRouter`, the no-plugin default), not a
/// double that could paper over a regression.
#[tokio::test]
async fn unmodified_default_role_still_fails_to_route_with_a_named_no_candidate_error() {
    let cwd = support::unique_temp_dir("discover-getting-started-no-candidate");

    let outcome = load(LoadOptions {
        cwd,
        explicit_path: None,
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .expect("load with no XDG/project layer must still succeed via built-in defaults");
    assert_eq!(outcome.config.default_role.as_str(), "default");

    // `build()` requires at least one backend registered even though an
    // empty chain means it is never called (`RoutingError::NoCandidate`
    // fires inside `Router::resolve`, before any `AttemptEngine`/backend
    // involvement) -- see `crates/conway/tests/builder.rs`'s
    // `build_fails_with_no_backends_configured` for the check this
    // satisfies. `ScriptedBackend` (not `FakeBackend`), specifically so
    // `.calls()` below can prove the backend really was never reached, not
    // just that the turn ended in `Failed`.
    let backend = Arc::new(ScriptedBackend::new(vec![]).with_id(BackendId::new("fake")));

    // No `.with_router(..)` override -- `build()` falls through to
    // `conway_core::routing::MinimalRouter`, compiled from `outcome.config`
    // exactly as a real embedder who called `discover()` unmodified would
    // get.
    let conway = ConwayBuilder::from_parts(outcome.config)
        .with_cli_overrides(CliOverrides {
            permission_mode: Some("deny".to_string()),
            ..CliOverrides::default()
        })
        .with_builtin_plugins(PluginSelection::None)
        .with_backend(backend.clone())
        .with_session_store(Arc::new(FakeStore::new()))
        .build()
        .expect("build should succeed: an empty chain is a valid, if useless, config");

    let session = tokio::time::timeout(T, conway.new_session(SessionSpec::default()))
        .await
        .expect("new_session must not hang")
        .expect("new_session should succeed");
    let turn = tokio::time::timeout(T, session.prompt("hello"))
        .await
        .expect("prompt must not hang")
        .expect("prompt should succeed");
    let result = tokio::time::timeout(T, turn.result())
        .await
        .expect("result must not hang")
        .expect("result() itself must not error -- the turn ends Failed, not the stream");

    // The backend was never reached: `NoCandidate` fires inside
    // `Router::resolve`, before any attempt is made -- a rename that
    // accidentally gave the default role a working chain would show up
    // here as a real call.
    assert!(
        backend.calls().is_empty(),
        "an empty-chain default role must never reach the backend; calls: {:?}",
        backend.calls()
    );

    match &result.status {
        ResultStatus::Failed { error } => {
            assert!(
                error.contains("no candidate for role default"),
                "must name the role, not route silently: {error}"
            );
            assert!(
                error.contains("(0 considered)"),
                "an empty chain has nothing to consider: {error}"
            );
        }
        other => panic!("expected ResultStatus::Failed, got {other:?}"),
    }
}
