//! Smoke test for the `event_stream_consumer` example's facade flow,
//! mirroring `example_smoke.rs`'s own precedent. See `discover_getting_
//! started_example_smoke.rs`'s own module doc for why this test builds via
//! `ConwayBuilder::from_parts` (an isolated `config::load` outcome) rather
//! than calling the example's own `ConwayBuilder::discover()` directly.

mod support;

use std::sync::Arc;
use std::time::Duration;

use conway::backend::{BackendId, ModelId};
use conway::config::ConwayConfig;
use conway::{ConwayBuilder, Event, ModelRef, PermissionDecision, SessionSpec};
use conway_testkit::{FakeBackend, FakeGate, FakeRouter, FakeStore};
use futures::StreamExt;

const T: Duration = Duration::from_secs(5);

#[tokio::test]
async fn event_stream_consumer_example_flow_assembles_the_reply_from_the_stream_alone() {
    let cwd = support::unique_temp_dir("event-stream-consumer");
    let outcome = conway::config::load(conway::config::LoadOptions {
        cwd,
        explicit_path: None,
        env: support::isolated_env(),
        cli_overrides: conway::config::CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .expect("load with no XDG/project layer must still succeed via built-in defaults");
    let config: ConwayConfig = outcome.config;

    let backend = Arc::new(FakeBackend::echo(BackendId::new("fake")));
    let route = ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    };

    let conway = ConwayBuilder::from_parts(config)
        .with_backend(backend)
        .with_router(Arc::new(FakeRouter::single(route)))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_session_store(Arc::new(FakeStore::new()))
        .build()
        .expect("build should succeed");

    let session = tokio::time::timeout(T, conway.new_session(SessionSpec::default()))
        .await
        .expect("new_session must not hang")
        .expect("new_session should succeed");

    let mut events = session.events();
    let _turn = tokio::time::timeout(T, session.prompt("Hello, conway!"))
        .await
        .expect("prompt must not hang")
        .expect("prompt should succeed");

    let mut reply = String::new();
    let saw_finished;
    let mut delta_count = 0;
    loop {
        let envelope = tokio::time::timeout(T, events.next())
            .await
            .expect("event stream must not hang")
            .expect("event stream must not end before the turn finishes");
        match envelope.event {
            Event::TextDelta { text } => {
                delta_count += 1;
                reply.push_str(&text);
            }
            Event::AgentFinished { result, .. } if result.agent_id == session.root() => {
                saw_finished = true;
                break;
            }
            _ => {}
        }
    }

    assert!(
        saw_finished,
        "the stream must yield this agent's own AgentFinished before ending"
    );
    assert_eq!(
        reply, "Hello, conway!",
        "the reply assembled purely from TextDelta events must match the echo backend's text"
    );
    assert!(
        delta_count >= 1,
        "the echo backend's stream() implementation must chunk its response into at least \
         one TextDelta"
    );
}
