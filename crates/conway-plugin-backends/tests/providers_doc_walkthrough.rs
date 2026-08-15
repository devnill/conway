//! S4c's verification anchor, executed: `docs/providers.md`'s "Adding a
//! provider variant" section, followed VERBATIM for both shipped families
//! -- the `[[profile]]`/`backends.<id>` snippets below are the exact text
//! that section shows (kept in sync by convention, the same "the doc text
//! is proven, not merely plausible" contract `docs/providers.md`'s own "A
//! complete worked example" section states for its third-party-backend
//! walkthrough).
//!
//! **This is the load-bearing half the item exists for**: a procedure that
//! only actually works for the family its author was thinking about is the
//! defect a single "Adding a provider" section (openai-compat-only, before
//! this item) could hide indefinitely. Both worked examples below build a
//! real backend through the real `BackendFactory::build` used in
//! production and drive a real request through it against `wiremock`,
//! asserting on the wire body/headers the profile's fields are documented
//! to control -- never on a resolved config struct, which would only prove
//! deserialization worked.

use std::collections::BTreeMap;

use conway_core::content::{
    ContentBlock, PermissionClass, Role, SamplingParams, ToolCategory, ToolSpec,
};
use conway_core::ids::{BackendId, ModelId, ToolName};
use conway_core::ports::{BackendBuildContext, BackendFactory, GenerateRequest};
use conway_core::provenance::Provenance;
use conway_core::segment::PromptSegment;
use conway_plugin_backends::{AnthropicBackendFactory, OpenAiCompatBackendFactory};
use futures::StreamExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Directory-per-call helper -- mirrors `tests/profile_facility.rs`'s own
/// `write_profile_file`, including its reasoning: concurrent tests in this
/// file landing in the same nanosecond is not actually rare, so a counter
/// alongside the pid keeps every call's directory unique.
fn write_profile_file(contents: &str) -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "conway-plugin-backends-providers-doc-walkthrough-{}-{n}",
        std::process::id(),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("profiles.toml");
    std::fs::write(&path, contents).unwrap();
    path
}

// ---------------------------------------------------------------------
// docs/providers.md, "Adding a provider variant" -- worked example 1:
// an `openai-compat` variant.
// ---------------------------------------------------------------------

/// Byte-for-byte the `.conway/profiles.toml` block docs/providers.md
/// shows for the `openai-compat` worked example.
const DOC_OPENAI_COMPAT_PROFILE_TOML: &str = r#"
[[profile]]
id = "my-local-server"
supports_stream_options = true
tool_call_style = "tolerant"
max_context_tokens = 65536
reliability_tier = "community"

[profile.cache]
kind = "implicit_prefix"
min_prefix_tokens = 0
"#;

/// The `backends.local` entry docs/providers.md shows alongside it --
/// `base_url`/`dialect` reproduced verbatim; `id`/`kind` become this
/// test's `BackendBuildContext` fields directly rather than a JSON string,
/// since this test drives `BackendFactory::build` (the JSON shown in the
/// doc is `conway`'s facade-level config, which resolves to exactly this
/// context).
fn doc_openai_compat_ctx(base_url: &str, profile_path: std::path::PathBuf) -> BackendBuildContext {
    BackendBuildContext {
        id: BackendId::new("local"),
        base_url: base_url.to_string(),
        api_key: None,
        dialect: Some("my-local-server".to_string()),
        models: BTreeMap::new(),
        profile_file_paths: vec![profile_path],
        extra: BTreeMap::new(),
    }
}

fn weather_tool() -> ToolSpec {
    ToolSpec {
        name: ToolName::new("get_weather"),
        description: "Get the current weather for a city".into(),
        schema: serde_json::from_value(serde_json::json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        }))
        .unwrap(),
        category: ToolCategory::Fetch,
        permission: PermissionClass::Safe,
    }
}

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

/// The doc's own claim for `my-local-server`: `supports_stream_options =
/// true` reaches the wire request body as `"stream_options"`, and
/// `tool_call_style = "tolerant"` accepts a tool-call delta whose
/// `function.arguments` arrives as a complete JSON OBJECT rather than a
/// string fragment (the Ollama-quirk shape `tool_calls/ollama.rs`
/// documents) -- both properties this profile's fields, and no others,
/// control.
#[tokio::test]
async fn doc_openai_compat_worked_example_drives_both_documented_profile_effects() {
    let profile_path = write_profile_file(DOC_OPENAI_COMPAT_PROFILE_TOML);
    let server = MockServer::start().await;
    let body = sse_body(&[
        serde_json::json!({
            "choices": [{
                "delta": {"tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    // A complete-object `arguments` value, never a string
                    // fragment -- only `tool_call_style = "tolerant"`
                    // accepts this shape.
                    "function": {"name": "get_weather", "arguments": {"city": "Paris"}}
                }]},
                "finish_reason": null
            }]
        }),
        serde_json::json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
    ]);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(&server)
        .await;

    let backend = OpenAiCompatBackendFactory
        .build(doc_openai_compat_ctx(&server.uri(), profile_path.clone()))
        .expect("the doc's my-local-server profile must resolve and build");

    let req = GenerateRequest {
        model: ModelId::new("local-model"),
        segments: vec![PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text { text: "hi".into() }],
            Provenance::UserPrompt,
        )],
        tools: vec![weather_tool()],
        params: SamplingParams::default(),
        prefix_key: None,
    };
    let mut stream = backend.stream(req).await.expect("stream must open");
    let mut response = None;
    while let Some(item) = stream.next().await {
        if let conway_core::ports::StreamChunk::Done(done) = item.unwrap() {
            response = Some(done);
        }
    }
    let response = response.expect("stream must end with a Done chunk");

    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(
        response.tool_calls[0].arguments,
        serde_json::json!({"city": "Paris"}),
        "tool_call_style = \"tolerant\" must accept the complete-object arguments shape"
    );

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 1);
    let sent_body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        sent_body["stream_options"]["include_usage"], true,
        "supports_stream_options = true must reach the wire body's stream_options key: {sent_body}"
    );

    std::fs::remove_dir_all(profile_path.parent().unwrap()).ok();
}

// ---------------------------------------------------------------------
// docs/providers.md, "Adding a provider variant" -- worked example 2:
// an `anthropic` variant.
// ---------------------------------------------------------------------

/// Byte-for-byte the `.conway/profiles.toml` block docs/providers.md
/// shows for the `anthropic` worked example -- a DIFFERENT file than the
/// openai-compat one above, per the one-file-per-kind constraint the same
/// doc section states (and `tests/profile_facility.rs` proves for a
/// different pair of ids).
const DOC_ANTHROPIC_PROFILE_TOML: &str = r#"
[[profile]]
id = "my-anthropic-gateway"
anthropic_version = "2024-10-22"

[profile.headers]
"anthropic-beta" = "my-feature-flag"
"#;

fn doc_anthropic_ctx(base_url: &str, profile_path: std::path::PathBuf) -> BackendBuildContext {
    BackendBuildContext {
        id: BackendId::new("gateway"),
        base_url: base_url.to_string(),
        api_key: Some("sk-ant-api03-test-key".to_string()),
        dialect: Some("my-anthropic-gateway".to_string()),
        models: BTreeMap::new(),
        profile_file_paths: vec![profile_path],
        extra: BTreeMap::new(),
    }
}

fn minimal_anthropic_response() -> serde_json::Value {
    serde_json::json!({
        "content": [{"type": "text", "text": "ok"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    })
}

fn user_request() -> GenerateRequest {
    GenerateRequest {
        model: ModelId::new("test-model"),
        segments: vec![PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text { text: "hi".into() }],
            Provenance::UserPrompt,
        )],
        tools: vec![],
        params: SamplingParams::default(),
        prefix_key: None,
    }
}

/// The doc's own claim for `my-anthropic-gateway`: `anthropic_version =
/// "2024-10-22"` and `headers."anthropic-beta" = "my-feature-flag"` both
/// reach the real outgoing request's HTTP HEADERS (never the JSON body --
/// Anthropic's wire format carries both as headers), alongside (never in
/// place of) the `x-api-key` header `AnthropicBackend` always sets.
#[tokio::test]
async fn doc_anthropic_worked_example_drives_both_documented_header_overrides() {
    let profile_path = write_profile_file(DOC_ANTHROPIC_PROFILE_TOML);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(minimal_anthropic_response()))
        .expect(1)
        .mount(&server)
        .await;

    let backend = AnthropicBackendFactory
        .build(doc_anthropic_ctx(&server.uri(), profile_path.clone()))
        .expect("the doc's my-anthropic-gateway profile must resolve and build");
    backend.generate(user_request()).await.unwrap();

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        request
            .headers
            .get("anthropic-version")
            .expect("anthropic-version header must be present")
            .to_str()
            .unwrap(),
        "2024-10-22",
        "anthropic_version from the doc's profile must reach the real wire header"
    );
    assert_eq!(
        request
            .headers
            .get("anthropic-beta")
            .expect("the doc's headers.anthropic-beta must reach the real wire request")
            .to_str()
            .unwrap(),
        "my-feature-flag"
    );
    assert_eq!(
        request
            .headers
            .get("x-api-key")
            .expect("x-api-key must still be present alongside the override")
            .to_str()
            .unwrap(),
        "sk-ant-api03-test-key"
    );

    std::fs::remove_dir_all(profile_path.parent().unwrap()).ok();
}
