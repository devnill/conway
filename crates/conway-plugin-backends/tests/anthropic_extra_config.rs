//! `AnthropicBackendFactory::build`'s `[backends.<id>].extra` handling: a
//! non-default `anthropic_version` reaching the real outgoing wire request,
//! a `headers` override reaching it too, an unrecognized key being a named,
//! typed rejection, and an empty `extra` behaving exactly as before this
//! item. Driven entirely through `wiremock` (`P-15`: no credentials, no
//! network beyond the loopback listener).
//!
//! Deliberately asserts on the ACTUAL captured wiremock request (header/
//! body), never on `AnthropicConfig`'s own fields -- `AnthropicConfig` has
//! carried `anthropic_version` since before this item; asserting that a
//! resolved config struct holds the right string would only prove
//! deserialization worked, not that the value the operator configured ever
//! reaches the wire. Anthropic's real API carries `anthropic-version` as an
//! HTTP header (never in the JSON body -- confirmed against
//! `src/anthropic/mod.rs`'s `request_builder`), so the header is this
//! field's true discriminating observable.

use std::collections::BTreeMap;

use conway_core::content::{ContentBlock, Role, SamplingParams};
use conway_core::error::ConwayError;
use conway_core::ids::{BackendId, ModelId};
use conway_core::ports::{BackendBuildContext, BackendFactory, GenerateRequest};
use conway_core::provenance::Provenance;
use conway_core::segment::PromptSegment;
use conway_plugin_backends::AnthropicBackendFactory;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ctx(id: &str, base_url: &str, extra: BTreeMap<String, Value>) -> BackendBuildContext {
    BackendBuildContext {
        id: BackendId::new(id),
        base_url: base_url.to_string(),
        api_key: Some("sk-ant-api03-test-key".to_string()),
        dialect: None,
        models: BTreeMap::new(),
        profile_file_paths: Vec::new(),
        extra,
    }
}

fn minimal_response() -> Value {
    json!({
        "content": [{"type": "text", "text": "ok"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    })
}

fn user_request() -> GenerateRequest {
    GenerateRequest {
        model: ModelId::new("claude-sonnet-4-6"),
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

/// The verification anchor: `extra.anthropic_version` set to a non-default
/// value produces a real outgoing request carrying that exact value in the
/// `anthropic-version` header -- asserted on wiremock's captured request,
/// not on the resolved config.
#[tokio::test]
async fn non_default_anthropic_version_reaches_the_anthropic_version_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(minimal_response()))
        .expect(1)
        .mount(&server)
        .await;

    let mut extra = BTreeMap::new();
    extra.insert("anthropic_version".to_string(), json!("2024-01-01"));
    let backend = AnthropicBackendFactory
        .build(ctx("anthropic", &server.uri(), extra))
        .expect("a non-default anthropic_version must be accepted");

    backend.generate(user_request()).await.unwrap();

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .headers
            .get("anthropic-version")
            .expect("anthropic-version header must be present")
            .to_str()
            .unwrap(),
        "2024-01-01",
        "the configured extra.anthropic_version must reach the real wire header, not just the \
         parsed config struct"
    );
}

/// `extra` with no `anthropic_version` key behaves exactly as before this
/// item: the crate's existing default (`"2023-06-01"`) is what reaches the
/// wire.
#[tokio::test]
async fn empty_extra_sends_the_existing_default_anthropic_version() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(minimal_response()))
        .expect(1)
        .mount(&server)
        .await;

    let backend = AnthropicBackendFactory
        .build(ctx("anthropic", &server.uri(), BTreeMap::new()))
        .expect("an empty extra must build exactly as before this item");

    backend.generate(user_request()).await.unwrap();

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .headers
            .get("anthropic-version")
            .unwrap()
            .to_str()
            .unwrap(),
        "2023-06-01"
    );
}

/// `extra.headers` is a genuinely consumed override, not merely a validated-
/// and-discarded key: a header named there reaches the real outgoing
/// request alongside (not instead of) the two hardcoded headers.
#[tokio::test]
async fn header_override_reaches_the_real_outgoing_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(minimal_response()))
        .expect(1)
        .mount(&server)
        .await;

    let mut headers = serde_json::Map::new();
    headers.insert("anthropic-beta".to_string(), json!("extended-context-2024"));
    let mut extra = BTreeMap::new();
    extra.insert("headers".to_string(), Value::Object(headers));
    let backend = AnthropicBackendFactory
        .build(ctx("anthropic", &server.uri(), extra))
        .expect("a headers override must be accepted");

    backend.generate(user_request()).await.unwrap();

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .headers
            .get("anthropic-beta")
            .expect("the overridden header must be present")
            .to_str()
            .unwrap(),
        "extended-context-2024"
    );
    // The two hardcoded headers are still present -- an override adds to,
    // never replaces, them.
    assert!(requests[0].headers.get("x-api-key").is_some());
    assert_eq!(
        requests[0]
            .headers
            .get("anthropic-version")
            .unwrap()
            .to_str()
            .unwrap(),
        "2023-06-01"
    );
}

/// An unrecognized `extra` key is a REJECTED build, never a silently
/// ignored one -- a typed `ConwayError::Config` naming the exact key.
#[tokio::test]
async fn unrecognized_extra_key_is_a_named_typed_error() {
    let mut extra = BTreeMap::new();
    extra.insert("totally_unsupported_key".to_string(), json!("value"));
    let err =
        match AnthropicBackendFactory.build(ctx("anthropic", "https://api.anthropic.com", extra)) {
            Err(err) => err,
            Ok(_) => panic!("an unrecognized extra key must be rejected"),
        };
    match err {
        ConwayError::Config { detail } => {
            assert!(
                detail.contains("totally_unsupported_key"),
                "error must name the unrecognized key: {detail}"
            );
        }
        other => panic!("expected ConwayError::Config, got {other:?}"),
    }
}

/// A malformed (non-string) `anthropic_version` value is rejected with a
/// named error rather than silently coerced or panicking.
#[tokio::test]
async fn non_string_anthropic_version_is_rejected() {
    let mut extra = BTreeMap::new();
    extra.insert("anthropic_version".to_string(), json!(20240101));
    let err =
        match AnthropicBackendFactory.build(ctx("anthropic", "https://api.anthropic.com", extra)) {
            Err(err) => err,
            Ok(_) => panic!("a non-string anthropic_version must be rejected"),
        };
    match err {
        ConwayError::Config { detail } => {
            assert!(detail.contains("anthropic_version"), "{detail}");
        }
        other => panic!("expected ConwayError::Config, got {other:?}"),
    }
}
