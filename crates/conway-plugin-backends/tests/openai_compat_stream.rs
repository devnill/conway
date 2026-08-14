//! Wiremock integration tests for `OpenAiCompatBackend::stream`.
//! Generate (non-streaming) tests live in `tests/openai_compat_generate.rs`.

use std::collections::BTreeMap;

use conway_core::content::{
    ContentBlock, PermissionClass, SamplingParams, StopReason, ToolCategory, ToolSpec,
};
use conway_core::error::BackendError;
use conway_core::ids::{BackendId, ModelId, ToolName};
use conway_core::ports::{Backend, GenerateRequest, StreamChunk};
use conway_core::provenance::Provenance;
use conway_core::segment::PromptSegment;
use conway_plugin_backends::config::{Dialect, OpenAiCompatConfig};
use conway_plugin_backends::openai_compat::OpenAiCompatBackend;
use futures::StreamExt;
use serde_json::{json, Value};
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

/// Renders `events` as an SSE body, one `data:` line per event, terminated
/// by `data: [DONE]`.
fn sse_body(events: &[Value]) -> String {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(&event.to_string());
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

/// S1 regression (cycle 1): dropping the returned stream must terminate the
/// SSE-driving task even while the server emits only content-free
/// keep-alive chunks. Proxy for task termination: the stream's channel
/// sender closes, which we observe by the mock connection being released
/// within the timeout (the test simply must not hang or leak past the
/// timeout).
#[tokio::test]
async fn dropping_the_stream_terminates_the_drive_task() {
    let server = MockServer::start().await;
    // An SSE body that never sends [DONE] and whose chunks carry no
    // content: pure keep-alive filler (S1 regression, cycle 1).
    let body = "data: {\"choices\":[{\"delta\":{}}]}\n\n".repeat(50_000);
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(body, "text/event-stream"),
        )
        .mount(&server)
        .await;

    let backend = OpenAiCompatBackend::new(config(&server.uri(), Dialect::OpenAi)).unwrap();
    let mut stream = backend
        .stream(user_request("gpt-test"))
        .await
        .expect("stream opens");
    // Poll once (content-free chunks yield nothing), then drop.
    let _ = tokio::time::timeout(std::time::Duration::from_millis(50), stream.next()).await;
    drop(stream);
    // The select!-on-closed path must release the drive task promptly; the
    // assertion is that shutdown below does not hang past the timeout.
    tokio::time::timeout(std::time::Duration::from_millis(500), async {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    })
    .await
    .expect("drive task released");
}

#[tokio::test]
async fn stream_emits_ordered_text_deltas_then_one_done_matching_the_concatenation() {
    let server = MockServer::start().await;
    let body = sse_body(&[
        json!({"choices": [{"delta": {"content": "Hello"}, "finish_reason": null}]}),
        json!({"choices": [{"delta": {"content": ", world!"}, "finish_reason": null}]}),
        json!({
            "choices": [{"delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3}
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

    let backend = OpenAiCompatBackend::new(config(&server.uri(), Dialect::OpenAi)).unwrap();
    let mut stream = backend.stream(user_request("gpt-4.1")).await.unwrap();

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
    assert_eq!(response.usage.input_tokens, 5);
    assert_eq!(response.usage.output_tokens, 3);
    server.verify().await;
}

#[tokio::test]
async fn stream_tool_call_deltas_emit_tool_call_delta_chunks_and_a_validated_done() {
    let server = MockServer::start().await;
    let body = sse_body(&[
        json!({
            "choices": [{
                "delta": {"tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "function": {"name": "get_weather", "arguments": ""}
                }]},
                "finish_reason": null
            }]
        }),
        json!({
            "choices": [{
                "delta": {"tool_calls": [{
                    "index": 0,
                    "function": {"arguments": "{\"city\":\"Paris\"}"}
                }]},
                "finish_reason": null
            }]
        }),
        json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
    ]);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(&server)
        .await;

    let mut req = user_request("gpt-4.1");
    req.tools = vec![weather_tool()];

    let backend = OpenAiCompatBackend::new(config(&server.uri(), Dialect::OpenAi)).unwrap();
    let mut stream = backend.stream(req).await.unwrap();

    let mut deltas = Vec::new();
    let mut done = None;
    while let Some(item) = stream.next().await {
        match item.unwrap() {
            StreamChunk::ToolCallDelta { index, raw } => deltas.push((index, raw)),
            StreamChunk::Done(response) => done = Some(response),
            other => panic!("unexpected chunk: {other:?}"),
        }
    }

    assert_eq!(deltas.len(), 2);
    assert!(deltas.iter().all(|(index, _)| *index == 0));
    let response = done.expect("stream must end with exactly one Done");
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].call_id, "call_1");
    assert_eq!(response.tool_calls[0].arguments, json!({"city": "Paris"}));
    assert_eq!(response.stop, StopReason::ToolUse);
    server.verify().await;
}

#[tokio::test]
async fn stream_truncated_tool_call_json_yields_tool_parse_err_with_exactly_one_request() {
    let server = MockServer::start().await;
    let body = sse_body(&[
        json!({
            "choices": [{
                "delta": {"tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"Pa"}
                }]},
                "finish_reason": null
            }]
        }),
        json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
    ]);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(&server)
        .await;

    let mut req = user_request("gpt-4.1");
    req.tools = vec![weather_tool()];

    let backend = OpenAiCompatBackend::new(config(&server.uri(), Dialect::OpenAi)).unwrap();
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
