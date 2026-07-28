//! Wiremock integration tests for `AnthropicBackend::generate` and
//! `capabilities` (WI-021). SSE streaming tests live in
//! `tests/anthropic_stream.rs`; cache-hint mapping tests live in
//! `tests/anthropic_cache_mapping.rs`. Segment→message golden-JSON and
//! consecutive-`ToolResult`-merge unit tests live in
//! `src/anthropic/wire.rs`, matching the WI-019 precedent
//! (`openai_compat/wire.rs`) for internals not reachable from an external
//! test crate.

use std::collections::BTreeMap;

use conway_backends::anthropic::AnthropicBackend;
use conway_backends::capabilities::{anthropic_defaults, build_capabilities, CapabilityInputs};
use conway_backends::config::{AnthropicConfig, SecretString};
use conway_backends::model_metadata::ModelMetadataStore;
use conway_core::capabilities::{CacheMode, ToolCallSupport};
use conway_core::content::{
    ContentBlock, PermissionClass, Role, SamplingParams, StopReason, ToolCategory, ToolSpec,
};
use conway_core::error::BackendError;
use conway_core::ids::{ModelId, ToolName};
use conway_core::ports::{Backend, GenerateRequest};
use conway_core::provenance::Provenance;
use conway_core::segment::PromptSegment;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config(base_url: &str) -> AnthropicConfig {
    AnthropicConfig {
        id: conway_core::ids::BackendId::new("anthropic"),
        api_key: SecretString::new("sk-ant-api03-test-key"),
        base_url: base_url.parse().unwrap(),
        anthropic_version: "2023-06-01".into(),
        timeout: None,
        models: BTreeMap::new(),
    }
}

fn user_request(model: &str) -> GenerateRequest {
    GenerateRequest {
        model: ModelId::new(model),
        segments: vec![PromptSegment::new(
            Role::User,
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

fn fixture_segments() -> Vec<PromptSegment> {
    vec![
        PromptSegment::new(
            Role::System,
            vec![ContentBlock::Text {
                text: "You are a helpful assistant.".into(),
            }],
            Provenance::AgentDef {
                name: "assistant".into(),
            },
        ),
        PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text {
                text: "What's the weather in Paris?".into(),
            }],
            Provenance::UserPrompt,
        ),
        PromptSegment::new(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                call_id: "call_1".into(),
                name: ToolName::new("get_weather"),
                arguments: json!({"city": "Paris"}),
            }],
            Provenance::SystemNote {
                reason: "turn".into(),
            },
        ),
        PromptSegment::new(
            Role::ToolResult,
            vec![ContentBlock::ToolResultBlock {
                call_id: "call_1".into(),
                blocks: vec![ContentBlock::Text {
                    text: "22C, sunny".into(),
                }],
                is_error: false,
            }],
            Provenance::ToolResult {
                call_id: "call_1".into(),
                tool: ToolName::new("get_weather"),
            },
        ),
    ]
}

#[tokio::test]
async fn golden_four_segment_fixture_produces_the_expected_request_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let backend = AnthropicBackend::new(config(&server.uri())).unwrap();
    let req = GenerateRequest {
        model: ModelId::new("claude-sonnet-4-6"),
        segments: fixture_segments(),
        tools: vec![],
        params: SamplingParams::default(),
        prefix_key: None,
    };
    backend.generate(req).await.unwrap();

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].headers.get("x-api-key").is_some());
    assert!(
        requests[0].headers.get("authorization").is_none(),
        "no Authorization: Bearer header is ever constructed (GP-09/C-02)"
    );

    let body: serde_json::Value = requests[0].body_json().unwrap();
    assert_eq!(
        body["system"],
        json!([{"type": "text", "text": "You are a helpful assistant."}])
    );
    assert_eq!(
        body["messages"],
        json!([
            {"role": "user", "content": [{"type": "text", "text": "What's the weather in Paris?"}]},
            {
                "role": "assistant",
                "content": [{"type": "tool_use", "id": "call_1", "name": "get_weather", "input": {"city": "Paris"}}]
            },
            {
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "call_1", "content": "22C, sunny", "is_error": false}]
            }
        ])
    );
}

#[tokio::test]
async fn generate_text_only_response_maps_stop_and_usage_from_all_four_wire_fields() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{"type": "text", "text": "hi there"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 4,
                "cache_read_input_tokens": 3,
                "cache_creation_input_tokens": 2
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let backend = AnthropicBackend::new(config(&server.uri())).unwrap();
    let response = backend
        .generate(user_request("claude-sonnet-4-6"))
        .await
        .unwrap();

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
    assert_eq!(response.usage.cache_read_tokens, 3);
    assert_eq!(response.usage.cache_write_tokens, 2);
    server.verify().await;
}

#[tokio::test]
async fn generate_tool_use_block_yields_one_validated_call_and_tool_use_stop() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Paris"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 20, "output_tokens": 8}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut req = user_request("claude-sonnet-4-6");
    req.tools = vec![weather_tool()];

    let backend = AnthropicBackend::new(config(&server.uri())).unwrap();
    let response = backend.generate(req).await.unwrap();

    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].call_id, "toolu_1");
    assert_eq!(response.tool_calls[0].name, ToolName::new("get_weather"));
    assert_eq!(response.tool_calls[0].arguments, json!({"city": "Paris"}));
    assert_eq!(response.stop, StopReason::ToolUse);
    server.verify().await;
}

#[tokio::test]
async fn rate_limit_429_with_retry_after_yields_rate_limit_after_budget_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
        .expect(3)
        .mount(&server)
        .await;

    let backend = AnthropicBackend::new(config(&server.uri())).unwrap();
    let err = backend
        .generate(user_request("claude-sonnet-4-6"))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        BackendError::RateLimit {
            retry_after_secs: Some(1)
        }
    );
    server.verify().await;
}

#[tokio::test]
async fn overloaded_529_yields_server_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(529).set_body_json(json!({
            "type": "error",
            "error": {"type": "overloaded_error", "message": "Overloaded"}
        })))
        .expect(3)
        .mount(&server)
        .await;

    let backend = AnthropicBackend::new(config(&server.uri())).unwrap();
    let err = backend
        .generate(user_request("claude-sonnet-4-6"))
        .await
        .unwrap_err();
    assert!(matches!(err, BackendError::ServerError { .. }), "{err:?}");
    server.verify().await;
}

#[tokio::test]
async fn prompt_too_long_400_yields_context_overflow() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "type": "error",
            "error": {"type": "invalid_request_error", "message": "prompt is too long: 250000 tokens > 200000 maximum"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let backend = AnthropicBackend::new(config(&server.uri())).unwrap();
    let err = backend
        .generate(user_request("claude-sonnet-4-6"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, BackendError::ContextOverflow { .. }),
        "{err:?}"
    );
    server.verify().await;
}

#[test]
fn capabilities_for_claude_sonnet_returns_explicit_breakpoints_and_validated_streaming() {
    let backend = AnthropicBackend::new(config("https://api.anthropic.com")).unwrap();
    let caps = backend.capabilities(&ModelId::new("claude-sonnet-4-6"));

    match caps.cache {
        CacheMode::ExplicitBreakpoints {
            max_breakpoints, ..
        } => {
            assert_eq!(max_breakpoints, 4);
        }
        other => panic!("expected ExplicitBreakpoints, got {other:?}"),
    }
    assert_eq!(
        caps.tool_calling,
        ToolCallSupport::Streaming { validated: true }
    );

    let expected = build_capabilities(CapabilityInputs {
        dialect_defaults: anthropic_defaults(),
        metadata: ModelMetadataStore::defaults().get(&ModelId::new("claude-sonnet-4-6")),
        overrides: None,
    });
    assert_eq!(caps, expected);
}

// Deliberately not `start_paused = true` anywhere in this file:
// `HttpClient::with_timeout` sets a real per-request timeout on the
// underlying `reqwest::Client`, which conflicts with tokio's auto-advancing
// virtual clock during real TCP handshakes to wiremock (see the identical
// rationale in `tests/openai_compat_generate.rs`). The rate-limit test above
// uses a 1-second `retry-after` for exactly this reason: fast enough to run
// for real, long enough to exercise the "honor retry-after" branch.
