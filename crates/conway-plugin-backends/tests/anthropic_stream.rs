//! Wiremock integration tests for `AnthropicBackend::stream`.
//! Non-streaming `generate` tests live in `tests/anthropic_generate.rs`.

use std::collections::BTreeMap;

use conway_core::content::{
    ContentBlock, PermissionClass, Role, SamplingParams, StopReason, ToolCategory, ToolSpec,
};
use conway_core::error::BackendError;
use conway_core::ids::{ModelId, ToolName};
use conway_core::ports::{Backend, GenerateRequest, StreamChunk};
use conway_core::provenance::Provenance;
use conway_core::segment::PromptSegment;
use conway_plugin_backends::anthropic::AnthropicBackend;
use conway_plugin_backends::config::{AnthropicConfig, SecretString};
use futures::StreamExt;
use serde_json::{json, Value};
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

/// Renders `events` as an SSE body, one `data:` line per event. Anthropic
/// streams terminate on `message_stop`, not a `[DONE]` marker.
fn sse_body(events: &[Value]) -> String {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(&event.to_string());
        body.push_str("\n\n");
    }
    body
}

#[tokio::test]
async fn canonical_sse_sequence_emits_ordered_text_deltas_then_one_done() {
    let server = MockServer::start().await;
    let body = sse_body(&[
        json!({"type": "message_start", "message": {"id": "msg_1", "usage": {"input_tokens": 10, "output_tokens": 0}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hello"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": ", world!"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 5}}),
        json!({"type": "message_stop"}),
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let backend = AnthropicBackend::new(config(&server.uri())).unwrap();
    let mut stream = backend
        .stream(user_request("claude-sonnet-4-6"))
        .await
        .unwrap();

    let mut deltas = Vec::new();
    let mut done = None;
    while let Some(item) = stream.next().await {
        match item.unwrap() {
            StreamChunk::TextDelta(text) => deltas.push(text),
            StreamChunk::Done(response) => done = Some(response),
            other => panic!("unexpected chunk: {other:?}"),
        }
    }

    assert_eq!(deltas, vec!["Hello".to_string(), ", world!".to_string()]);
    let response = done.expect("stream must end with exactly one Done");
    assert_eq!(
        response.content,
        vec![ContentBlock::Text {
            text: "Hello, world!".into()
        }]
    );
    assert_eq!(response.stop, StopReason::EndTurn);
    assert_eq!(response.usage.input_tokens, 10);
    assert_eq!(response.usage.output_tokens, 5);
    server.verify().await;
}

#[tokio::test]
async fn streamed_tool_use_with_valid_json_deltas_yields_one_validated_call() {
    let server = MockServer::start().await;
    let body = sse_body(&[
        json!({"type": "message_start", "message": {"id": "msg_1", "usage": {"input_tokens": 10, "output_tokens": 0}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {}}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"city\":"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "\"Paris\"}"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 8}}),
        json!({"type": "message_stop"}),
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(&server)
        .await;

    let mut req = user_request("claude-sonnet-4-6");
    req.tools = vec![weather_tool()];

    let backend = AnthropicBackend::new(config(&server.uri())).unwrap();
    let mut stream = backend.stream(req).await.unwrap();

    let mut tool_call_deltas = Vec::new();
    let mut done = None;
    while let Some(item) = stream.next().await {
        match item.unwrap() {
            StreamChunk::ToolCallDelta { index, raw } => tool_call_deltas.push((index, raw)),
            StreamChunk::Done(response) => done = Some(response),
            other => panic!("unexpected chunk: {other:?}"),
        }
    }

    assert_eq!(tool_call_deltas.len(), 2);
    assert!(tool_call_deltas.iter().all(|(index, _)| *index == 0));
    let response = done.expect("stream must end with exactly one Done");
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].call_id, "toolu_1");
    assert_eq!(response.tool_calls[0].arguments, json!({"city": "Paris"}));
    assert_eq!(response.stop, StopReason::ToolUse);
    server.verify().await;
}

#[tokio::test]
async fn invalid_input_json_delta_fragments_yield_final_tool_parse_err_with_one_request() {
    let server = MockServer::start().await;
    let body = sse_body(&[
        json!({"type": "message_start", "message": {"id": "msg_1", "usage": {"input_tokens": 10, "output_tokens": 0}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {}}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"city\":\"Pa"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 8}}),
        json!({"type": "message_stop"}),
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(&server)
        .await;

    let mut req = user_request("claude-sonnet-4-6");
    req.tools = vec![weather_tool()];

    let backend = AnthropicBackend::new(config(&server.uri())).unwrap();
    let mut stream = backend.stream(req).await.unwrap();

    let mut saw_tool_parse_err = false;
    let mut saw_done = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(StreamChunk::Done(_)) => saw_done = true,
            Ok(_) => {}
            Err(BackendError::ToolParse { .. }) => saw_tool_parse_err = true,
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }

    assert!(saw_tool_parse_err, "expected a ToolParse stream item");
    assert!(!saw_done, "a truncated tool call must not also yield Done");
    server.verify().await;
}

#[tokio::test]
async fn thinking_delta_events_emit_thinking_deltas_and_a_thinking_content_block_in_done() {
    let server = MockServer::start().await;
    let body = sse_body(&[
        json!({"type": "message_start", "message": {"id": "msg_1", "usage": {"input_tokens": 10, "output_tokens": 0}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "Let me think"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": " about it."}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig-abc"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "123"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "The answer is 4"}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 5}}),
        json!({"type": "message_stop"}),
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(&server)
        .await;

    let backend = AnthropicBackend::new(config(&server.uri())).unwrap();
    let mut stream = backend
        .stream(user_request("claude-sonnet-4-6"))
        .await
        .unwrap();

    let mut thinking_deltas = Vec::new();
    let mut done = None;
    while let Some(item) = stream.next().await {
        match item.unwrap() {
            StreamChunk::ThinkingDelta(text) => thinking_deltas.push(text),
            StreamChunk::TextDelta(_) => {}
            StreamChunk::Done(response) => done = Some(response),
            other => panic!("unexpected chunk: {other:?}"),
        }
    }

    assert_eq!(
        thinking_deltas,
        vec!["Let me think".to_string(), " about it.".to_string()]
    );
    let response = done.expect("stream must end with exactly one Done");
    assert!(
        response.content.iter().any(|block| matches!(
            block,
            ContentBlock::Thinking { text, signature }
                if text == "Let me think about it."
                    && signature.as_deref() == Some("sig-abc123")
        )),
        "expected a signed Thinking block in Done.content: {:?}",
        response.content
    );
    assert!(
        response
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { text } if text == "The answer is 4")),
        "expected a Text content block in Done.content: {:?}",
        response.content
    );
    server.verify().await;
}
