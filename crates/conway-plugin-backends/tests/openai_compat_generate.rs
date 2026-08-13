//! Wiremock integration tests for `OpenAiCompatBackend::generate` and
//! `capabilities` (WI-019). Segment→message mapping golden-JSON and
//! cache-hint byte-identity unit tests live in
//! `src/openai_compat/wire.rs`; SSE streaming tests live in
//! `tests/openai_compat_stream.rs`.

use std::collections::BTreeMap;

use conway_core::content::{
    ContentBlock, PermissionClass, SamplingParams, StopReason, ToolCategory, ToolSpec,
};
use conway_core::error::BackendError;
use conway_core::ids::{BackendId, ModelId, ToolName};
use conway_core::ports::{Backend, GenerateRequest};
use conway_core::provenance::Provenance;
use conway_core::segment::PromptSegment;
use conway_plugin_backends::capabilities::{build_capabilities, CapabilityInputs};
use conway_plugin_backends::config::{Dialect, OpenAiCompatConfig};
use conway_plugin_backends::model_metadata::ModelMetadataStore;
use conway_plugin_backends::openai_compat::OpenAiCompatBackend;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config(base_url: &str, dialect: Dialect) -> OpenAiCompatConfig {
    OpenAiCompatConfig {
        id: BackendId::new("test"),
        base_url: base_url.parse().unwrap(),
        api_key: None,
        profile: dialect.profile(),
        timeout: None,
        metadata_path: None,
        models: BTreeMap::new(),
    }
}

fn user_request(model: &str) -> GenerateRequest {
    GenerateRequest {
        model: ModelId::new(model),
        segments: vec![PromptSegment::new(
            conway_core::content::Role::User,
            vec![ContentBlock::Text {
                text: "hello".into(),
            }],
            Provenance::UserPrompt,
        )],
        tools: vec![],
        params: SamplingParams::default(),
        prefix_key: None,
    }
}

fn weather_tool() -> ToolSpec {
    ToolSpec {
        name: ToolName::new("get_weather"),
        description: "Get the current weather for a city".into(),
        schema: serde_json::from_value(json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        }))
        .unwrap(),
        category: ToolCategory::Fetch,
        permission: PermissionClass::Safe,
    }
}

#[tokio::test]
async fn generate_text_only_completion_maps_content_stop_and_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "hi there"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 4}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let backend = OpenAiCompatBackend::new(config(&server.uri(), Dialect::OpenAi)).unwrap();
    let response = backend.generate(user_request("gpt-4.1")).await.unwrap();

    assert_eq!(
        response.content,
        vec![ContentBlock::Text {
            text: "hi there".into()
        }]
    );
    assert!(response.tool_calls.is_empty());
    assert_eq!(response.stop, StopReason::EndTurn);
    assert_eq!(response.usage.input_tokens, 10);
    assert_eq!(response.usage.output_tokens, 4);
    server.verify().await;
}

#[tokio::test]
async fn generate_tool_calls_finish_reason_yields_one_validated_call_and_tool_use_stop() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 20, "completion_tokens": 8}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut req = user_request("gpt-4.1");
    req.tools = vec![weather_tool()];

    let backend = OpenAiCompatBackend::new(config(&server.uri(), Dialect::OpenAi)).unwrap();
    let response = backend.generate(req).await.unwrap();

    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].call_id, "call_1");
    assert_eq!(response.tool_calls[0].name, ToolName::new("get_weather"));
    assert_eq!(response.tool_calls[0].arguments, json!({"city": "Paris"}));
    assert_eq!(response.stop, StopReason::ToolUse);
    server.verify().await;
}

// Deliberately not `start_paused = true`: `HttpClient::with_timeout` sets a
// real per-request timeout on the underlying `reqwest::Client`, and under
// paused/auto-advancing tokio time that timer can fire before the (real,
// OS-level) TCP handshake to wiremock completes. `http.rs`'s own
// `send_with_retry` tests sidestep this by building their client with
// `HttpClient::new(reqwest::Client::new(), ..)`, which never calls
// `.timeout()`; here the timeout is load-bearing production behavior, so
// these two tests just accept the real (sub-second) backoff delay instead.
#[tokio::test]
async fn generate_against_500_endpoint_yields_server_error_after_exactly_three_requests() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500))
        .expect(3)
        .mount(&server)
        .await;

    let backend = OpenAiCompatBackend::new(config(&server.uri(), Dialect::OpenAi)).unwrap();
    let err = backend.generate(user_request("gpt-4.1")).await.unwrap_err();
    assert!(matches!(err, BackendError::ServerError { .. }), "{err:?}");
    server.verify().await;
}

#[tokio::test]
async fn generate_against_401_endpoint_yields_auth_after_exactly_one_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;

    let backend = OpenAiCompatBackend::new(config(&server.uri(), Dialect::OpenAi)).unwrap();
    let err = backend.generate(user_request("gpt-4.1")).await.unwrap_err();
    assert!(matches!(err, BackendError::Auth { .. }), "{err:?}");
    server.verify().await;
}

#[test]
fn capabilities_returns_build_capabilities_output_for_present_and_absent_models() {
    let backend =
        OpenAiCompatBackend::new(config("http://localhost:11434/v1", Dialect::Ollama)).unwrap();

    // Present in the bundled DEFAULTS metadata (crate::model_metadata::DEFAULTS).
    let present_model = ModelId::new("qwen3-coder-30b");
    let present = backend.capabilities(&present_model);
    let expected_present = build_capabilities(CapabilityInputs {
        dialect_defaults: Dialect::Ollama.defaults(),
        metadata: ModelMetadataStore::defaults().get(&present_model),
        overrides: None,
    });
    assert_eq!(present, expected_present);

    // Absent from metadata: falls back to dialect defaults entirely.
    let absent_model = ModelId::new("totally-unknown-model");
    let absent = backend.capabilities(&absent_model);
    let expected_absent = build_capabilities(CapabilityInputs {
        dialect_defaults: Dialect::Ollama.defaults(),
        metadata: None,
        overrides: None,
    });
    assert_eq!(absent, expected_absent);
}
