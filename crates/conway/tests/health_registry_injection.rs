//! Board item `01M1WVQM440XZDSP0KC664HJ7S` (architecture review finding
//! F11): `ConwayBuilder::with_health_registry` is new -- before this item
//! `HealthRegistry` had no injection point at all, unlike `SubagentHost`
//! (which carries an explicit, cited, single-authority ruling for why it
//! has none). This test proves the new method's override is genuinely
//! consulted by a real backend attempt, not merely accepted by the builder.
//! Mirrors `example_smoke.rs`'s own minimal-session shape.

use std::sync::Arc;
use std::time::Duration;

use conway::test_support::base_config;
use conway::{ConwayBuilder, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::ids::{BackendId, ModelId, ModelRef};
use conway_testkit::{FakeBackend, FakeGate, FakeHealth, FakeRouter, FakeStore};

const T: Duration = Duration::from_secs(5);

fn route() -> ModelRef {
    ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }
}

#[tokio::test]
async fn with_health_registry_is_genuinely_consulted_on_a_backend_attempt() {
    // `FakeHealth` is a facade-external, non-default `HealthRegistry`
    // implementation -- the default `build()` would otherwise construct
    // (absent an installed router factory) is the honestly degenerate
    // `AlwaysClosedHealthRegistry`, whose own `record` is a no-op.
    let health = Arc::new(FakeHealth::new());

    let conway = ConwayBuilder::from_parts(base_config())
        .with_backend(Arc::new(FakeBackend::echo(BackendId::new("fake"))))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_router(Arc::new(FakeRouter::single(route())))
        .with_health_registry(health.clone())
        .build()
        .expect("build with a custom, non-default HealthRegistry should succeed");

    assert!(
        health.observations().is_empty(),
        "sanity: no observation recorded before any turn has run"
    );

    let session = tokio::time::timeout(T, conway.new_session(SessionSpec::default()))
        .await
        .expect("new_session must not hang")
        .expect("new_session should succeed");
    let turn = tokio::time::timeout(T, session.prompt("Hello, custom health registry!"))
        .await
        .expect("prompt must not hang")
        .expect("prompt should succeed");
    let _ = tokio::time::timeout(T, turn.result())
        .await
        .expect("result must not hang")
        .expect("result should succeed");

    // The proof: `AttemptEngine::execute` must have actually called
    // `record` on the INJECTED registry for this real backend attempt, not
    // merely accepted it at build time and silently fallen back to the
    // degenerate default.
    let observations = health.observations();
    assert_eq!(
        observations.len(),
        1,
        "conway must record exactly one Observation, through the injected \
         HealthRegistry, for the one successful attempt this turn made"
    );
    assert_eq!(observations[0].0, conway::EndpointId::new("fake"));
}
