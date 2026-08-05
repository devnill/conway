//! Smoke test for the `minimal_session` example's facade flow (WI-135,
//! review finding M2). The example itself is only compile-checked by
//! `cargo build --examples`; this test exercises the same public-facade path
//! at runtime so a regression in `prompt`/`ask`/persistence cannot slip
//! through, and asserts the ephemerality property the example demonstrates
//! (finding G3): an `ask` does not extend the main session's log.
//!
//! Every `.await` is wrapped in a short `tokio::time::timeout` so a hang
//! (e.g. reintroducing a second prompt on a non-keep-alive session) fails the
//! test quickly instead of blocking forever.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    RoleEntry, RoutingSection, SessionConfig, ToolsConfig, TuiSection,
};
use conway::{Conway, ConwayBuilder, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::fakes::{FakeBackend, FakeGate, FakeRouter, FakeStore};
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias};
use conway_core::ports::SessionStore;

const T: Duration = Duration::from_secs(5);

/// The same minimal config the example builds.
fn minimal_config() -> ConwayConfig {
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
        tui: TuiSection::default(),
        tools: ToolsConfig::default(),
    }
}

fn build(store: Arc<FakeStore>) -> Conway {
    ConwayBuilder::from_parts(minimal_config())
        .with_backend(Arc::new(FakeBackend::echo(BackendId::new("fake"))))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_session_store(store)
        .with_router(Arc::new(FakeRouter::single(ModelRef {
            backend: BackendId::new("fake"),
            model: ModelId::new("echo-model"),
        })))
        .build()
        .expect("build with every port injected should succeed")
}

#[tokio::test]
async fn minimal_session_example_flow_runs_and_ask_leaves_no_trace() {
    let store = Arc::new(FakeStore::new());
    let conway = build(store.clone());

    let session = tokio::time::timeout(T, conway.new_session(SessionSpec::default()))
        .await
        .expect("new_session must not hang")
        .expect("new_session should succeed");

    // One real turn, drained fully so the log has settled.
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

    let head_before = store
        .head(&session.id())
        .await
        .expect("head read should succeed");

    // The ephemeral ask forks a hidden child and runs a turn there.
    let aside = tokio::time::timeout(T, session.ask("(ephemeral) checking"))
        .await
        .expect("ask must not hang")
        .expect("ask should succeed");
    let _ = tokio::time::timeout(T, aside.text())
        .await
        .expect("ask text must not hang")
        .expect("ask text should succeed");
    let _ = tokio::time::timeout(T, aside.result())
        .await
        .expect("ask result must not hang")
        .expect("ask result should succeed");

    let head_after = store
        .head(&session.id())
        .await
        .expect("head read should succeed");

    assert_eq!(
        head_before, head_after,
        "the ephemeral ask must not append to the main session's log"
    );
}
