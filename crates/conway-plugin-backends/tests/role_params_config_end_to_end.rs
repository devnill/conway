//! **The load-bearing test for A1c**: `roles.<alias>.params` (a facade
//! `settings.json` shape) reaches the REAL Anthropic/openai-compat wire
//! request body, driven through the full stack -- `ConwayConfig::routing()`
//! -> `conway_core::routing::RoleConfig::params` -> a real `Backend` built
//! by a registered `BackendFactory` -> a real HTTP request against a
//! loopback `wiremock` server. Mirrors `tests/builder_end_to_end.rs`'s own
//! "no injected `Backend`/`Router`, only a registered factory" shape, so
//! this is proven the way a library embedder's own config would actually
//! exercise it -- never by constructing a `GenerateRequest`/`SamplingParams`
//! by hand and asserting the wire layer reads it (that only proves the
//! wire adapter's OWN existing behavior, not that `settings.json` ever
//! reaches it; see `crates/conway/src/config/schema.rs`'s own
//! `routing_threads_role_params_extra_reasoning_budget_tokens` for the
//! config-to-`RoleConfig` half this test does not re-prove, and
//! `src/anthropic/wire.rs`/`src/openai_compat/wire.rs`'s own
//! `reasoning_budget_tokens_serializes_into_thinking_param_when_set`/
//! `reasoning_effort_is_emitted_only_for_openai_dialect_when_set` unit
//! tests for the wire-adapter half).

use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{BackendEntry, ConwayConfig, RoleEntry, RoleParams};
use conway::test_support::{base_config, test_builder_without_router};
use conway::SessionSpec;
use conway_core::ids::RoleAlias;
use conway_plugin_backends::{AnthropicBackendFactory, OpenAiCompatBackendFactory};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Renders an Anthropic SSE stream: `AnthropicBackend`'s capabilities
/// always resolve `tool_calling` to `Streaming { validated: true }` (see
/// `tests/anthropic_generate.rs`'s
/// `capabilities_for_claude_sonnet_returns_explicit_breakpoints_and_
/// validated_streaming`), so a real turn through the runtime always
/// selects the streaming path -- a single-JSON `generate`-style response
/// would never be requested.
fn anthropic_sse_body() -> String {
    let events = [
        json!({"type": "message_start", "message": {"id": "msg_1", "usage": {"input_tokens": 5, "output_tokens": 0}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "hello from thinking"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
        json!({"type": "message_stop"}),
    ];
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(&event.to_string());
        body.push_str("\n\n");
    }
    body
}

/// A `thinking` role naming `params.extra.reasoning_budget_tokens = 8000`
/// on a real `anthropic`-kind backend -- byte for byte the shape an
/// operator's `settings.json` would carry.
fn thinking_role_config(base_url: String) -> ConwayConfig {
    let mut config = base_config();
    config.default_role = RoleAlias::new("thinking");
    let mut extra = serde_json::Map::new();
    extra.insert("reasoning_budget_tokens".into(), json!(8000));
    config.roles.insert(
        "thinking".to_string(),
        RoleEntry {
            chain: vec!["anthropic/claude-sonnet-4-6".to_string()],
            params: RoleParams {
                extra,
                ..RoleParams::default()
            },
            ..Default::default()
        },
    );
    config.backends.insert(
        "anthropic".to_string(),
        BackendEntry {
            kind: "anthropic".to_string(),
            api_key: "sk-ant-api03-test-key".to_string(),
            base_url,
            ..BackendEntry::default()
        },
    );
    config
}

/// ACCEPTANCE 1: `roles.thinking.params.extra.reasoning_budget_tokens =
/// 8000` in a `settings.json`-shaped `ConwayConfig` produces a real
/// Anthropic request body carrying `thinking.budget_tokens: 8000`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thinking_role_reasoning_budget_reaches_the_anthropic_request_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(anthropic_sse_body())
                .insert_header("content-type", "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let conway = test_builder_without_router(thinking_role_config(server.uri()))
        .with_backend_factory(Arc::new(AnthropicBackendFactory))
        .build()
        .expect("build should succeed: a real config-derived anthropic backend");

    let handle = conway
        .new_session(SessionSpec {
            role: Some(RoleAlias::new("thinking")),
            ..Default::default()
        })
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hi").await.expect("prompt should succeed");
    let text = tokio::time::timeout(Duration::from_secs(10), turn.text())
        .await
        .expect("turn must not hang")
        .expect("turn should succeed");
    assert_eq!(text, "hello from thinking");

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        body["thinking"],
        json!({"type": "enabled", "budget_tokens": 8000}),
        "roles.thinking.params.extra.reasoning_budget_tokens must reach the real Anthropic \
         request body's \"thinking\" field: {body}"
    );
    server.verify().await;
}

/// A `fast` role naming `params.temperature = 0` on a real
/// `openai-compat`-kind backend.
fn fast_role_config(base_url: String) -> ConwayConfig {
    let mut config = base_config();
    config.default_role = RoleAlias::new("fast");
    config.roles.insert(
        "fast".to_string(),
        RoleEntry {
            chain: vec!["mock/echo-model".to_string()],
            params: RoleParams {
                temperature: Some(0.0),
                ..RoleParams::default()
            },
            ..Default::default()
        },
    );
    config.backends.insert(
        "mock".to_string(),
        BackendEntry {
            kind: "openai-compat".to_string(),
            base_url,
            dialect: Some("openai".to_string()),
            ..BackendEntry::default()
        },
    );
    config
}

/// Renders `events` as an SSE body -- mirrors
/// `tests/builder_end_to_end.rs`'s own `sse_body` helper for the same
/// reason (the `"openai"` profile's `tool_calling` default always selects
/// the streaming path).
fn openai_sse_body(events: &[serde_json::Value]) -> String {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(&event.to_string());
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

/// ACCEPTANCE 2: `roles.fast.params.temperature = 0` in a
/// `settings.json`-shaped `ConwayConfig` produces a real OpenAI-compatible
/// request body carrying `temperature: 0`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fast_role_temperature_reaches_the_openai_compat_request_body() {
    let server = MockServer::start().await;
    let body = openai_sse_body(&[
        json!({"choices": [{"delta": {"content": "hello from fast"}, "finish_reason": null}]}),
        json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
    ]);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;

    let conway = test_builder_without_router(fast_role_config(server.uri()))
        .with_backend_factory(Arc::new(OpenAiCompatBackendFactory))
        .build()
        .expect("build should succeed: a real config-derived openai-compat backend");

    let handle = conway
        .new_session(SessionSpec {
            role: Some(RoleAlias::new("fast")),
            ..Default::default()
        })
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hi").await.expect("prompt should succeed");
    let text = tokio::time::timeout(Duration::from_secs(10), turn.text())
        .await
        .expect("turn must not hang")
        .expect("turn should succeed");
    assert_eq!(text, "hello from fast");

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        body["temperature"], 0.0,
        "roles.fast.params.temperature must reach the real openai-compat request body: {body}"
    );
    server.verify().await;
}
