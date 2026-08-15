//! Wiremock integration tests for `AnthropicBackend`'s
//! `CacheMode::ExplicitBreakpoints` cache-hint mapping: the
//! breakpoint cap, the byte-identity invariant, and the `CacheTtl` → wire
//! shape table. These assert on the actual outgoing request body captured
//! by wiremock (black-box, through the public `Backend::generate` API) —
//! `src/anthropic/{wire,cache}.rs` are private submodules, matching the
//! precedent of keeping adapter internals out of the crate's public
//! surface.

use std::collections::BTreeMap;

use conway_core::content::{ContentBlock, Role, SamplingParams};
use conway_core::ids::{ModelId, PrefixKey};
use conway_core::ports::{Backend, GenerateRequest};
use conway_core::provenance::Provenance;
use conway_core::segment::{strip_cache_hints, CacheHint, CacheTtl, PromptSegment};
use conway_plugin_backends::anthropic::AnthropicBackend;
use conway_plugin_backends::config::{AnthropicConfig, SecretString};
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

fn minimal_response() -> Value {
    json!({
        "content": [{"type": "text", "text": "ok"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    })
}

fn req_with(segments: Vec<PromptSegment>) -> GenerateRequest {
    GenerateRequest {
        model: ModelId::new("claude-sonnet-4-6"),
        segments,
        tools: vec![],
        params: SamplingParams::default(),
        prefix_key: None,
    }
}

fn system_segment(text: &str) -> PromptSegment {
    PromptSegment::new(
        Role::System,
        vec![ContentBlock::Text { text: text.into() }],
        Provenance::AgentDef { name: "r".into() },
    )
}

fn breakpoint_hint(ttl: CacheTtl, key: &str) -> CacheHint {
    CacheHint {
        breakpoint: true,
        ttl,
        prefix_key: key.parse::<PrefixKey>().unwrap(),
    }
}

/// Recursively deletes every `"cache_control"` key from `value`, in place —
/// the byte-identity invariant's "removing every cache_control key"
/// operationalization.
fn strip_cache_control_keys(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("cache_control");
            for v in map.values_mut() {
                strip_cache_control_keys(v);
            }
        }
        Value::Array(items) => {
            for v in items.iter_mut() {
                strip_cache_control_keys(v);
            }
        }
        _ => {}
    }
}

fn sample_tool(name: &str) -> conway_core::content::ToolSpec {
    conway_core::content::ToolSpec {
        name: conway_core::ids::ToolName::new(name),
        description: format!("{name} tool"),
        schema: schemars::schema_for!(std::collections::BTreeMap<String, String>),
        category: conway_core::content::ToolCategory::Read,
        permission: conway_core::content::PermissionClass::Safe,
    }
}

/// This acceptance test drives cache-hint mapping end to end through the real
/// `AnthropicBackend::generate`: a `Provenance::ToolRegistry` segment
/// carrying a breakpoint hint (exactly what `conway-runtime`'s
/// `ContextBuilder` now produces -- empty `content`) must NOT put a second
/// copy of the schema in `body["system"]`, and its `cache_control` must
/// land on the LAST entry of the real native `body["tools"]` array instead.
#[tokio::test]
async fn tool_registry_breakpoint_lands_on_the_last_native_tool_not_a_system_entry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(minimal_response()))
        .expect(1)
        .mount(&server)
        .await;

    let segments = vec![
        system_segment("You are a helpful assistant."),
        PromptSegment::new(
            Role::System,
            Vec::new(),
            Provenance::ToolRegistry {
                hash: "deadbeef".into(),
            },
        )
        .with_cache_hint(breakpoint_hint(CacheTtl::FiveMinutes, "k1")),
    ];
    let req = GenerateRequest {
        model: ModelId::new("claude-sonnet-4-6"),
        segments,
        tools: vec![sample_tool("search"), sample_tool("fetch")],
        params: SamplingParams::default(),
        prefix_key: None,
    };

    let backend = AnthropicBackend::new(config(&server.uri())).unwrap();
    backend.generate(req).await.unwrap();

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 1);
    let body: Value = requests[0].body_json().unwrap();

    let system = body["system"].as_array().expect("system array");
    assert_eq!(
        system.len(),
        1,
        "only the AgentDef segment produces a system entry: {system:?}"
    );
    assert!(
        system[0]["text"]
            .as_str()
            .unwrap()
            .contains("helpful assistant"),
        "no second, tool-schema-duplicating system entry: {system:?}"
    );

    let tools = body["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 2);
    assert!(
        tools[0].get("cache_control").is_none(),
        "only the LAST tool carries the marker: {tools:?}"
    );
    assert_eq!(tools[1]["cache_control"], json!({"type": "ephemeral"}));
}

#[tokio::test]
async fn six_breakpointed_segments_produce_exactly_four_cache_control_markers_on_the_last_four() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(minimal_response()))
        .expect(1)
        .mount(&server)
        .await;

    let segments: Vec<PromptSegment> = (0..6)
        .map(|i| {
            system_segment(&format!("segment {i}"))
                .with_cache_hint(breakpoint_hint(CacheTtl::FiveMinutes, &format!("key{i}")))
        })
        .collect();

    let backend = AnthropicBackend::new(config(&server.uri())).unwrap();
    backend.generate(req_with(segments)).await.unwrap();

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 1);
    let body: Value = requests[0].body_json().unwrap();
    let system = body["system"].as_array().expect("system array");
    assert_eq!(system.len(), 6);

    let has_cache_control: Vec<bool> = system
        .iter()
        .map(|entry| entry.get("cache_control").is_some())
        .collect();
    assert_eq!(
        has_cache_control,
        vec![false, false, true, true, true, true],
        "only the last 4 of 6 breakpointed segments (in segment order) must retain a cache_control marker: {system:?}"
    );
}

#[tokio::test]
async fn body_with_hints_stripped_equals_body_with_hints_minus_every_cache_control_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(minimal_response()))
        .expect(2)
        .mount(&server)
        .await;

    let hinted_segments = vec![
        system_segment("sys").with_cache_hint(breakpoint_hint(CacheTtl::OneHour, "k1")),
        PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text { text: "hi".into() }],
            Provenance::UserPrompt,
        ),
    ];
    let mut stripped_segments = hinted_segments.clone();
    strip_cache_hints(&mut stripped_segments);
    assert!(hinted_segments[0].cache_hint.is_some());
    assert!(stripped_segments[0].cache_hint.is_none());

    let backend = AnthropicBackend::new(config(&server.uri())).unwrap();
    backend.generate(req_with(hinted_segments)).await.unwrap();
    backend.generate(req_with(stripped_segments)).await.unwrap();

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 2);
    let mut hinted_body: Value = requests[0].body_json().unwrap();
    let stripped_body: Value = requests[1].body_json().unwrap();

    // Sanity: the hinted body actually carries a cache_control marker
    // before we strip it, otherwise this test would pass vacuously.
    assert_eq!(
        hinted_body["system"][0]["cache_control"]["type"],
        "ephemeral"
    );

    strip_cache_control_keys(&mut hinted_body);
    assert_eq!(hinted_body, stripped_body);
}

#[tokio::test]
async fn one_hour_ttl_emits_ttl_key_five_minutes_omits_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(minimal_response()))
        .expect(2)
        .mount(&server)
        .await;

    let backend = AnthropicBackend::new(config(&server.uri())).unwrap();

    let one_hour_segment =
        system_segment("sys").with_cache_hint(breakpoint_hint(CacheTtl::OneHour, "k1"));
    backend
        .generate(req_with(vec![one_hour_segment]))
        .await
        .unwrap();

    let five_minute_segment =
        system_segment("sys").with_cache_hint(breakpoint_hint(CacheTtl::FiveMinutes, "k2"));
    backend
        .generate(req_with(vec![five_minute_segment]))
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 2);
    let one_hour_body: Value = requests[0].body_json().unwrap();
    let five_minute_body: Value = requests[1].body_json().unwrap();

    assert_eq!(
        one_hour_body["system"][0]["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );

    let five_minute_cache_control = &five_minute_body["system"][0]["cache_control"];
    assert_eq!(five_minute_cache_control, &json!({"type": "ephemeral"}));
    assert!(
        five_minute_cache_control.get("ttl").is_none(),
        "FiveMinutes must not emit a ttl key: {five_minute_cache_control:?}"
    );
}
