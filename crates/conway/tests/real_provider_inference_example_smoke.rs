//! Smoke test for the `real_provider_inference` example's facade flow --
//! but unlike this crate's other `*_example_smoke.rs` files, this one
//! genuinely drives the REAL construction path the example uses
//! (`conway_plugin_backends::openai_compat::OpenAiCompatBackend`), against
//! a loopback `wiremock::MockServer`, never a live external endpoint --
//! the same discipline `crates/conway/src/builder.rs`'s own module doc
//! describes for `context_probe_overlay_seam.rs`
//! (`crates/conway-plugin-backends/tests/openai_compat_generate.rs` is the
//! precedent this file's mock response shape is taken from).
//!
//! This is what actually proves the example's central claim -- "this is
//! not a fake, it is a real `Backend` impl talking real HTTP" -- rather
//! than merely compiling. What this test does NOT cover: the example's own
//! `CONWAY_EXAMPLE_BASE_URL`-absent early return (trivial, and would need a
//! subprocess to test in isolation from this binary's own env — not worth
//! the ceremony for one `std::env::var().is_err()` branch) and the example's
//! own `ConwayBuilder::discover()` call (see `discover_getting_started_
//! example_smoke.rs`'s module doc for why an in-process test uses
//! `config::load` with an isolated `LoadOptions` instead).

mod support;

use std::sync::Arc;
use std::time::Duration;

use conway::backend::ModelId;
use conway::config::ConwayConfig;
use conway::{ConwayBuilder, ModelRef, PermissionDecision, SessionSpec};
use conway_core::ids::BackendId;
use conway_plugin_backends::config::{Dialect, OpenAiCompatConfig};
use conway_plugin_backends::openai_compat::OpenAiCompatBackend;
use conway_testkit::{FakeGate, FakeRouter, FakeStore};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const T: Duration = Duration::from_secs(5);

/// Renders `events` as an SSE body, one `data:` line per event, terminated
/// by `data: [DONE]` -- `OpenAiCompatBackend::stream` (what `conway-runtime`
/// actually calls, per its own `Backend::stream` contract; `generate` is a
/// separate, non-streaming method this facade's runtime never calls) reads
/// this shape, not a single JSON object. Precedent:
/// `crates/conway-plugin-backends/tests/openai_compat_stream.rs`'s own
/// `sse_body` helper, copied here rather than shared across crates for one
/// test.
fn sse_body(events: &[serde_json::Value]) -> String {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(&event.to_string());
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

#[tokio::test]
async fn real_provider_inference_example_flow_reaches_a_real_backend_over_loopback_http() {
    let server = MockServer::start().await;
    let body = sse_body(&[
        json!({"choices": [{"delta": {"content": "hi "}, "finish_reason": null}]}),
        json!({"choices": [{"delta": {"content": "there"}, "finish_reason": null}]}),
        json!({
            "choices": [{"delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 4}
        }),
    ]);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    // The exact construction the example itself performs, base URL/model
    // swapped for the mock server's own -- no `CONWAY_EXAMPLE_*` env vars
    // read here, `OpenAiCompatConfig` built directly instead.
    let backend_id = BackendId::new("real");
    let cfg = OpenAiCompatConfig {
        id: backend_id.clone(),
        base_url: server.uri().parse().expect("mock server URI must parse"),
        api_key: None,
        profile: Dialect::OpenAi.profile(),
        timeout: None,
        metadata_path: None,
        models: Default::default(),
    };
    let backend = Arc::new(OpenAiCompatBackend::new(cfg).expect("valid config must construct"));
    let route = ModelRef {
        backend: backend_id,
        model: ModelId::new("gpt-4.1"),
    };

    let cwd = support::unique_temp_dir("real-provider-inference");
    let outcome = conway::config::load(conway::config::LoadOptions {
        cwd,
        explicit_path: None,
        env: support::isolated_env(),
        cli_overrides: conway::config::CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .expect("load with no XDG/project layer must still succeed via built-in defaults");
    let config: ConwayConfig = outcome.config;

    let conway = ConwayBuilder::from_parts(config)
        .with_backend(backend)
        .with_router(Arc::new(FakeRouter::single(route)))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_session_store(Arc::new(FakeStore::new()))
        .build()
        .expect("build should succeed with a real Backend injected");

    let session = tokio::time::timeout(T, conway.new_session(SessionSpec::default()))
        .await
        .expect("new_session must not hang")
        .expect("new_session should succeed");
    let turn = tokio::time::timeout(T, session.prompt("Say hello in exactly three words."))
        .await
        .expect("prompt must not hang")
        .expect("prompt should succeed");
    let text = tokio::time::timeout(T, turn.text())
        .await
        .expect("text must not hang")
        .expect("text should succeed");

    assert_eq!(
        text, "hi there",
        "the reply must come from the real HTTP round trip to the mock server, not a fake"
    );
    let _ = tokio::time::timeout(T, turn.result())
        .await
        .expect("result must not hang")
        .expect("result should succeed");
}
