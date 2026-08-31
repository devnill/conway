//! Wiremock integration tests for the context-window declaration honesty
//! item (num_ctx): proves against the REAL request body a live mock server
//! receives, not a mock that echoes whatever it was sent (this item's own
//! acceptance criterion 1).
//!
//! Two behaviors under test:
//! - A resolved context window (an `Override` in `ModelOverrides`) routes
//!   `generate`/`stream` through Ollama's NATIVE `/api/chat`, with
//!   `options.num_ctx` set to exactly that value.
//! - No resolved window (`Unverified`) takes the ORIGINAL, unchanged
//!   OpenAI-compatible `/chat/completions` path, with no `options` field at
//!   all — acceptance criterion 2, "no configuration can produce a silent
//!   truncation": conway never asks a server to arrange a window it never
//!   established, and never silently claims a window it did not confirm.

use std::collections::BTreeMap;

use conway_core::content::{ContentBlock, SamplingParams, StopReason};
use conway_core::ids::{BackendId, ModelId};
use conway_core::ports::{Backend, GenerateRequest};
use conway_core::provenance::Provenance;
use conway_core::routing::ModelOverrides;
use conway_core::segment::PromptSegment;
use conway_plugin_backends::config::{Dialect, OpenAiCompatConfig};
use conway_plugin_backends::openai_compat::OpenAiCompatBackend;
use futures::StreamExt;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config(base_url: &str, models: BTreeMap<String, ModelOverrides>) -> OpenAiCompatConfig {
    OpenAiCompatConfig {
        id: BackendId::new("test"),
        base_url: base_url.parse().unwrap(),
        api_key: None,
        profile: Dialect::Ollama.profile(),
        timeout: None,
        metadata_path: None,
        models,
    }
}

fn override_with_context_window(tokens: u32) -> ModelOverrides {
    ModelOverrides {
        stream_tools: None,
        max_context_tokens: Some(tokens),
        reliability_tier: None,
        parallel_tool_calls: None,
        min_headroom_tokens: None,
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

/// A resolved context window (here: a config override) routes `generate`
/// through the NATIVE `/api/chat` endpoint with `options.num_ctx` set to
/// exactly the resolved value -- asserted against the real captured request
/// body, not a response-echoing mock.
#[tokio::test]
async fn generate_with_a_resolved_context_window_requests_native_num_ctx() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "qwen3:8b",
            "message": {"role": "assistant", "content": "hi there"},
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": 10,
            "eval_count": 4
        })))
        .expect(1)
        .mount(&server)
        .await;
    // Never reached: proves the OpenAI-compatible path was NOT used.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let mut models = BTreeMap::new();
    models.insert("qwen3:8b".to_string(), override_with_context_window(131_072));
    let backend = OpenAiCompatBackend::new(config(&server.uri(), models)).unwrap();
    let response = backend.generate(user_request("qwen3:8b")).await.unwrap();

    assert_eq!(
        response.content,
        vec![ContentBlock::Text {
            text: "hi there".into()
        }]
    );
    assert_eq!(response.stop, StopReason::EndTurn);

    let requests = server.received_requests().await.unwrap();
    let native_request = requests
        .iter()
        .find(|r| r.url.path() == "/api/chat")
        .expect("the native endpoint must have been called");
    let body: serde_json::Value = native_request.body_json().unwrap();
    assert_eq!(
        body["options"]["num_ctx"], 131_072,
        "the actual request body must carry the resolved context window, \
         not merely a response the mock happened to echo"
    );
    server.verify().await;
}

/// The streaming counterpart of the above -- `stream()` must ALSO route
/// through the native endpoint and request `options.num_ctx` when a real
/// context window is resolved.
#[tokio::test]
async fn stream_with_a_resolved_context_window_requests_native_num_ctx() {
    let server = MockServer::start().await;
    let ndjson_body = "{\"model\":\"qwen3:8b\",\"message\":{\"role\":\"assistant\",\"content\":\"hi\"},\"done\":false}\n\
{\"model\":\"qwen3:8b\",\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"done_reason\":\"stop\",\"prompt_eval_count\":10,\"eval_count\":2}\n";
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(ndjson_body.to_string(), "application/x-ndjson"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut models = BTreeMap::new();
    models.insert("qwen3:8b".to_string(), override_with_context_window(65_536));
    let backend = OpenAiCompatBackend::new(config(&server.uri(), models)).unwrap();
    let mut stream = backend.stream(user_request("qwen3:8b")).await.unwrap();
    let mut saw_done = false;
    while let Some(item) = stream.next().await {
        if let conway_core::ports::StreamChunk::Done(_) = item.unwrap() {
            saw_done = true;
        }
    }
    assert!(saw_done, "the native NDJSON stream must reach a Done chunk");

    let requests = server.received_requests().await.unwrap();
    let native_request = requests
        .iter()
        .find(|r| r.url.path() == "/api/chat")
        .expect("the native endpoint must have been called");
    let body: serde_json::Value = native_request.body_json().unwrap();
    assert_eq!(body["options"]["num_ctx"], 65_536);
    assert_eq!(body["stream"], true);
}

/// No resolved context window (`Unverified` -- no override, no metadata):
/// the ORIGINAL, unchanged OpenAI-compatible endpoint is used, and no
/// `options` field is ever sent -- conway never invents a number to
/// request. This is acceptance criterion 2's own positive case: nothing
/// here can produce a silent truncation, because nothing is asked for that
/// was never established.
#[tokio::test]
async fn generate_with_no_resolved_context_window_never_touches_the_native_endpoint() {
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
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let backend = OpenAiCompatBackend::new(config(&server.uri(), BTreeMap::new())).unwrap();
    let response = backend
        .generate(user_request("totally-undescribed-model"))
        .await
        .unwrap();
    assert_eq!(
        response.content,
        vec![ContentBlock::Text {
            text: "hi there".into()
        }]
    );

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = requests[0].body_json().unwrap();
    assert!(
        body.get("options").is_none(),
        "an Unverified context window must never produce an options field"
    );
    server.verify().await;
}
