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
use conway_testkit::{FakeBackend, FakeRouter, FakeStore};

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
    assert_eq!(outcome.config.default_role.as_str(), "coder");
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
