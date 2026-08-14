//! Conformance tests for the three dialects added later —
//! `VllmHermes`, `LmStudio`, `LlamaCppServer` — completing the five-dialect
//! matrix on top of the `OpenAi`/`Ollama` coverage.
//!
//! `ToolCallAccumulator`-level tests exercise the vllm#31871 inline-text
//! fallback (`push_content_delta`/`stop_override`) and the
//! codex#7517 repeated-id/name regression for `LmStudio`; wiremock tests
//! exercise per-dialect request-body quirks and end-to-end
//! `generate`/`stream` completion.

use std::collections::BTreeMap;

use conway_core::content::{
    ContentBlock, PermissionClass, SamplingParams, StopReason, ToolCategory, ToolSpec,
};
use conway_core::error::BackendError;
use conway_core::ids::{BackendId, ModelId, ToolName};
use conway_core::ports::{Backend, GenerateRequest, StreamChunk};
use conway_core::provenance::Provenance;
use conway_core::segment::PromptSegment;
use conway_plugin_backends::capabilities::{
    dialect_defaults, llama_cpp_server_defaults, lm_studio_defaults, vllm_hermes_defaults,
};
use conway_plugin_backends::config::{Dialect, OpenAiCompatConfig};
use conway_plugin_backends::openai_compat::OpenAiCompatBackend;
use conway_plugin_backends::tool_calls::{ToolCallAccumulator, ToolCallStyle};
use futures::StreamExt;
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

fn read_tool() -> ToolSpec {
    ToolSpec {
        name: ToolName::new("read"),
        description: "Read a file".into(),
        schema: serde_json::from_value(json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        }))
        .unwrap(),
        category: ToolCategory::Read,
        permission: PermissionClass::Safe,
    }
}

fn fixture_lines(name: &str) -> Vec<String> {
    let path = format!(
        "{}/tests/fixtures/streams/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("reading fixture {path}: {err}"))
        .lines()
        .map(str::to_string)
        .filter(|line| !line.trim().is_empty())
        .collect()
}

// --- DialectDefaults: distinct, matching the table ---------------

#[test]
fn vllm_hermes_defaults_is_distinct_and_matches_the_wi017_table() {
    let defaults = dialect_defaults(Dialect::VllmHermes);
    assert_eq!(defaults, vllm_hermes_defaults());
    assert_eq!(defaults.max_context_tokens, 32_768);
    assert!(defaults.parallel_tool_calls);
    assert_eq!(
        defaults.reliability_tier,
        conway_core::capabilities::ReliabilityTier::Community
    );
}

#[test]
fn lm_studio_defaults_is_distinct_and_matches_the_wi017_table() {
    let defaults = dialect_defaults(Dialect::LmStudio);
    assert_eq!(defaults, lm_studio_defaults());
    assert_eq!(defaults.cache, conway_core::capabilities::CacheMode::None);
    assert_eq!(
        defaults.structured_output,
        conway_core::capabilities::StructuredOutput::None
    );
    assert!(!defaults.parallel_tool_calls);
}

#[test]
fn llama_cpp_server_defaults_is_distinct_and_matches_the_wi017_table() {
    let defaults = dialect_defaults(Dialect::LlamaCppServer);
    assert_eq!(defaults, llama_cpp_server_defaults());
    assert_eq!(
        defaults.structured_output,
        conway_core::capabilities::StructuredOutput::Grammar
    );
    assert_eq!(
        defaults.reliability_tier,
        conway_core::capabilities::ReliabilityTier::Community
    );
}

#[test]
fn the_three_new_dialects_are_pairwise_distinct() {
    let vllm = dialect_defaults(Dialect::VllmHermes);
    let lm_studio = dialect_defaults(Dialect::LmStudio);
    let llama_cpp = dialect_defaults(Dialect::LlamaCppServer);
    assert_ne!(vllm, lm_studio);
    assert_ne!(vllm, llama_cpp);
    assert_ne!(lm_studio, llama_cpp);
}

// --- vllm#31871: inline `<tool_call>` text fallback ----------------------

#[test]
fn vllm_hermes_inline_text_tool_call_is_suppressed_and_produces_one_validated_call() {
    let specs = [read_tool()];
    let mut accumulator = ToolCallAccumulator::new(ToolCallStyle::HermesTextFallback, &specs);

    let mut emitted = String::new();
    for chunk in [
        "before ",
        r#"<tool_call>{"name":"read","arguments":{"path":"a.txt"}}</tool_call>"#,
        " after",
    ] {
        if let Some(text) = accumulator.push_content_delta(chunk).unwrap() {
            emitted.push_str(&text);
        }
    }

    // The tag and everything inside it must never surface as emittable
    // text — only the plain-text surroundings do.
    assert_eq!(emitted, "before  after");
    assert!(!emitted.contains("tool_call"));
    assert!(!emitted.contains("a.txt"));

    assert_eq!(accumulator.stop_override(), Some(StopReason::ToolUse));

    // Even though the caller's own `finish_reason` mapping said `EndTurn`,
    // the accumulator flags the override; a real caller would apply it.
    let calls = accumulator.finish(StopReason::EndTurn).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, ToolName::new("read"));
    assert_eq!(calls[0].arguments, json!({"path": "a.txt"}));
}

#[test]
fn vllm_hermes_with_structured_tool_calls_matches_openai_on_the_same_fixture() {
    let specs = [read_tool()];
    let lines = fixture_lines("openai_basic_two_chunks.txt");

    let mut openai_acc = ToolCallAccumulator::new(ToolCallStyle::Structured, &specs);
    for line in &lines {
        openai_acc.push_delta(line).unwrap();
    }
    let openai_calls = openai_acc.finish(StopReason::ToolUse).unwrap();

    let mut vllm_acc = ToolCallAccumulator::new(ToolCallStyle::HermesTextFallback, &specs);
    for line in &lines {
        vllm_acc.push_delta(line).unwrap();
    }
    // Structured deltas were fed: the Hermes fallback must not have fired.
    assert_eq!(vllm_acc.stop_override(), None);
    let vllm_calls = vllm_acc.finish(StopReason::ToolUse).unwrap();

    assert_eq!(openai_calls.len(), 1);
    assert_eq!(openai_calls, vllm_calls);
}

#[test]
fn vllm_hermes_unterminated_inline_tag_at_stream_end_is_tool_parse() {
    let specs = [read_tool()];
    let mut accumulator = ToolCallAccumulator::new(ToolCallStyle::HermesTextFallback, &specs);
    accumulator
        .push_content_delta(r#"<tool_call>{"name":"read","argum"#)
        .unwrap();
    let err = accumulator.finish(StopReason::EndTurn).unwrap_err();
    match err {
        BackendError::ToolParse { detail } => assert!(detail.contains("unterminated"), "{detail}"),
        other => panic!("expected ToolParse, got {other:?}"),
    }
}

// --- codex#7517: LmStudio repeated id/name/arguments ---------------------

#[test]
fn codex_7517_lm_studio_repeated_full_chunks_produce_one_call() {
    let specs = [read_tool()];
    let mut accumulator = ToolCallAccumulator::new(ToolCallStyle::Tolerant, &specs);
    let delta = r#"{"id":"call_1","function":{"name":"read","arguments":{"path":"a.txt"}}}"#;
    for _ in 0..3 {
        accumulator.push_delta(delta).unwrap();
    }
    let calls = accumulator.finish(StopReason::ToolUse).unwrap();
    assert_eq!(calls.len(), 1, "expected exactly one call, got {calls:?}");
    assert_eq!(calls[0].call_id, "call_1");
    assert_eq!(calls[0].arguments, json!({"path": "a.txt"}));
}

#[test]
fn codex_7517_lm_studio_shares_the_ollama_fixture_regression() {
    let specs = [read_tool()];
    let mut accumulator = ToolCallAccumulator::new(ToolCallStyle::Tolerant, &specs);
    for line in fixture_lines("codex_7517_repeated_id_and_name.txt") {
        accumulator.push_delta(&line).unwrap();
    }
    let calls = accumulator.finish(StopReason::ToolUse).unwrap();
    assert_eq!(calls.len(), 1, "expected exactly one call, got {calls:?}");
    assert_eq!(calls[0].call_id, "call_1");
    assert_eq!(calls[0].arguments, json!({"path": "a.txt"}));
}

// --- LlamaCppServer / LmStudio request-body quirks (wiremock) -----------

#[tokio::test]
async fn llama_cpp_server_generate_with_tools_emits_auto_choice_without_parallel_or_stream_options()
{
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut req = user_request("qwen3-coder-30b");
    req.tools = vec![read_tool()];
    let backend = OpenAiCompatBackend::new(config(&server.uri(), Dialect::LlamaCppServer)).unwrap();
    backend.generate(req).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["tool_choice"], "auto");
    assert!(body.get("parallel_tool_calls").is_none());
    assert!(body.get("stream_options").is_none());
    server.verify().await;
}

#[tokio::test]
async fn lm_studio_stream_request_body_omits_stream_options_and_parallel_tool_calls() {
    let server = MockServer::start().await;
    let body =
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
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

    let mut req = user_request("llama3.1-8b");
    req.tools = vec![read_tool()];
    let backend = OpenAiCompatBackend::new(config(&server.uri(), Dialect::LmStudio)).unwrap();
    let mut stream = backend.stream(req).await.unwrap();
    while stream.next().await.is_some() {}

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let sent_body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(sent_body["stream"], true);
    assert!(sent_body.get("stream_options").is_none());
    assert!(sent_body.get("parallel_tool_calls").is_none());
    server.verify().await;
}

// --- Text-only generate/stream completion for each dialect (6 tests) ----

async fn assert_text_only_generate_completes(dialect: Dialect) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "hi there"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 2}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let backend = OpenAiCompatBackend::new(config(&server.uri(), dialect)).unwrap();
    let response = backend.generate(user_request("test-model")).await.unwrap();

    assert_eq!(
        response.content,
        vec![ContentBlock::Text {
            text: "hi there".into()
        }]
    );
    assert!(response.tool_calls.is_empty());
    assert_eq!(response.stop, StopReason::EndTurn);
    server.verify().await;
}

async fn assert_text_only_stream_completes(dialect: Dialect) {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
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

    let backend = OpenAiCompatBackend::new(config(&server.uri(), dialect)).unwrap();
    let mut stream = backend.stream(user_request("test-model")).await.unwrap();

    let mut saw_delta = false;
    let mut done = None;
    while let Some(item) = stream.next().await {
        match item.unwrap() {
            StreamChunk::TextDelta(text) => {
                assert_eq!(text, "hi");
                saw_delta = true;
            }
            StreamChunk::Done(response) => done = Some(response),
            other => panic!("unexpected chunk: {other:?}"),
        }
    }
    assert!(saw_delta);
    let response = done.expect("stream must end with exactly one Done");
    assert_eq!(response.stop, StopReason::EndTurn);
    server.verify().await;
}

#[tokio::test]
async fn vllm_hermes_completes_a_text_only_generate() {
    assert_text_only_generate_completes(Dialect::VllmHermes).await;
}

#[tokio::test]
async fn lm_studio_completes_a_text_only_generate() {
    assert_text_only_generate_completes(Dialect::LmStudio).await;
}

#[tokio::test]
async fn llama_cpp_server_completes_a_text_only_generate() {
    assert_text_only_generate_completes(Dialect::LlamaCppServer).await;
}

#[tokio::test]
async fn vllm_hermes_completes_a_text_only_stream() {
    assert_text_only_stream_completes(Dialect::VllmHermes).await;
}

#[tokio::test]
async fn lm_studio_completes_a_text_only_stream() {
    assert_text_only_stream_completes(Dialect::LmStudio).await;
}

#[tokio::test]
async fn llama_cpp_server_completes_a_text_only_stream() {
    assert_text_only_stream_completes(Dialect::LlamaCppServer).await;
}

/// Rework regression (cycle 1): the Hermes inline-text fallback is
/// live in `Backend::stream()` — a real VllmHermes SSE stream carrying a
/// tool call as inline `<tool_call>` text yields a validated ToolCall and
/// ToolUse stop, with none of the tag text leaking as TextDelta.
#[tokio::test]
async fn vllm_hermes_live_stream_suppresses_inline_tag_and_yields_tool_call() {
    use conway_core::ports::StreamChunk;
    use futures::StreamExt;

    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"Sure, \"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"<tool_call>{\\\"name\\\":\\\"read\\\",\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"\\\"arguments\\\":{\\\"path\\\":\\\"a.txt\\\"}}</tool_call>\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(body, "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let backend = OpenAiCompatBackend::new(config(&server.uri(), Dialect::VllmHermes)).unwrap();
    let mut request = user_request("hermes-model");
    request.tools = vec![read_tool()];
    let mut stream = backend.stream(request).await.unwrap();

    let mut text = String::new();
    let mut done = None;
    while let Some(item) = stream.next().await {
        match item.unwrap() {
            StreamChunk::TextDelta(t) => text.push_str(&t),
            StreamChunk::Done(response) => done = Some(response),
            _ => {}
        }
    }
    assert!(
        !text.contains("<tool_call>") && !text.contains("arguments"),
        "inline tag text leaked as TextDelta: {text:?}"
    );
    let response = done.expect("one Done");
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].name.as_str(), "read");
    assert_eq!(
        response.tool_calls[0].arguments,
        serde_json::json!({"path": "a.txt"})
    );
    assert_eq!(response.stop, conway_core::content::StopReason::ToolUse);
}
