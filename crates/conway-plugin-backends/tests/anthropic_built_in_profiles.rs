//! S4c: `AnthropicBackendFactory`'s own built-in Anthropic-compatible
//! profile set (`crate::factory::ANTHROPIC_BUILT_IN_PROFILES`, `kimi-code`)
//! proven resolving through a real `BackendFactory::build` -- with NO
//! `.conway/profiles.toml` on disk at all -- the property that constant's
//! own doc claims for the one built-in it ships ("`dialect = "kimi-code"`
//! resolves out of the box... where before this item it failed every time
//! with a typed `UnknownProfile`").
//!
//! Credential-free throughout (`wiremock`, never a real Kimi/Anthropic
//! endpoint) -- exactly the constraint that shaped WHICH built-in this
//! crate ships at all: `factory.rs`'s own doc on `ANTHROPIC_BUILT_IN_PROFILES`
//! records that a header/version override this crate cannot exercise this
//! same way was deliberately left unshipped rather than guessed.

use std::collections::BTreeMap;

use conway_core::content::{ContentBlock, Role, SamplingParams};
use conway_core::error::ConwayError;
use conway_core::ids::{BackendId, ModelId};
use conway_core::ports::{BackendBuildContext, BackendFactory, GenerateRequest};
use conway_core::provenance::Provenance;
use conway_core::segment::PromptSegment;
use conway_plugin_backends::AnthropicBackendFactory;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ctx(base_url: &str, dialect: Option<&str>) -> BackendBuildContext {
    BackendBuildContext {
        id: BackendId::new("kimi"),
        base_url: base_url.to_string(),
        api_key: Some("kimi-coding-plan-key".to_string()),
        dialect: dialect.map(str::to_string),
        models: BTreeMap::new(),
        // Deliberately empty: this file's whole point is that the built-in
        // resolves with no profile FILE discovered at all.
        profile_file_paths: Vec::new(),
        extra: BTreeMap::new(),
    }
}

fn minimal_response() -> serde_json::Value {
    serde_json::json!({
        "content": [{"type": "text", "text": "ok"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    })
}

fn user_request() -> GenerateRequest {
    GenerateRequest {
        model: ModelId::new("k3-256k"),
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

/// The verification anchor for shipping `kimi-code` at all: it resolves
/// with zero `profile_file_paths`, and the resulting wire request carries
/// this kind's ordinary default `anthropic-version` header, with no
/// unexpected extra header -- proving BOTH that the built-in selects
/// successfully and that it adds no field override it does not claim to
/// (`kimi-code` ships zero fields; see `factory.rs`'s own doc for why).
#[tokio::test]
async fn kimi_code_built_in_profile_resolves_with_no_profile_file_on_disk() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(minimal_response()))
        .expect(1)
        .mount(&server)
        .await;

    let backend = AnthropicBackendFactory
        .build(ctx(&server.uri(), Some("kimi-code")))
        .expect("the built-in 'kimi-code' profile must resolve with no profile file on disk");
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
        "2023-06-01",
        "kimi-code ships zero field overrides -- the ordinary crate default must reach the wire"
    );
    assert_eq!(
        request
            .headers
            .get("x-api-key")
            .expect("x-api-key header must be present")
            .to_str()
            .unwrap(),
        "kimi-coding-plan-key"
    );
}

/// Before this item, EVERY `dialect` value for `"anthropic"` failed with
/// `UnknownProfile` (the kind shipped no built-ins at all). This is the
/// regression net for the negative case: shipping `kimi-code` did not
/// widen that into "anything now resolves" -- an unrelated name is still a
/// named, typed rejection.
#[tokio::test]
async fn an_unrelated_dialect_is_still_an_unknown_profile_error() {
    let err = match AnthropicBackendFactory
        .build(ctx("https://api.kimi.com", Some("not-a-real-profile")))
    {
        Err(err) => err,
        Ok(_) => panic!("an unknown profile name must be rejected"),
    };
    match err {
        ConwayError::Config { detail } => {
            assert!(detail.contains("not-a-real-profile"), "{detail}");
        }
        other => panic!("expected ConwayError::Config, got {other:?}"),
    }
}

/// No `dialect` at all is still a completely valid, unrelated configuration
/// -- shipping a built-in profile set didn't make `dialect` required for
/// this kind (unlike `"openai-compat"`; see `docs/providers.md`'s "Wire
/// version and header overrides" section).
#[tokio::test]
async fn no_dialect_at_all_is_unaffected_by_the_new_built_in() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(minimal_response()))
        .expect(1)
        .mount(&server)
        .await;

    let backend = AnthropicBackendFactory
        .build(ctx(&server.uri(), None))
        .expect("no dialect must still build");
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
