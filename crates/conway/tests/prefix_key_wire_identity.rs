//! Pins the property the whole prompt-cache economics story rests on
//! (board item `01M00QF4WSAD3RYB8PZN7ZKPFB`, `docs/vision/PLAN.md` D1-2b):
//! `conway_runtime::context::prefix_key`'s equality is meaningful only if
//! **equal `PrefixKey` implies a byte-identical wire prefix, and a
//! different `PrefixKey` implies the wire prefix is NOT byte-identical**.
//!
//! Nothing in this workspace asserted that before this file.
//! `conway-runtime/tests/context_golden.rs` pins context *assembly* (the
//! `PromptSegment` list `ContextBuilder` produces); `conway-plugin-backends/
//! tests/anthropic_cache_mapping.rs` pins that stripping every
//! `cache_hint` leaves the wire body unchanged; `conway/tests/
//! fanout_prefix_sharing.rs` proves that segments known-by-construction to
//! be sibling forks (the same in-process `Arc`) really do share wire bytes.
//! None of the three ever computes `prefix_key` itself and checks it
//! against the bytes a backend actually sent. A change to `prefix.rs`'s
//! hashed projection (e.g. dropping a field from `canonical_segment_bytes`,
//! or a wire adapter starting to read a field `prefix_key` ignores) could
//! silently decouple "two requests hash equal" from "two requests share a
//! wire prefix" — exactly `PHILOSOPHY.md` §4's failure mode: caching stops
//! working and "reads as a steady zero and otherwise looks exactly like an
//! expensive workload." A build that regresses this has no other symptom.
//!
//! **Home**: here, not `conway-plugin-backends/tests/` (that crate has no
//! dependency edge onto `conway-runtime`, which owns `prefix_key` — adding
//! one only for this file would be a bigger footprint than using a crate
//! that already has both edges) and not `conway-runtime/tests/` (which has
//! no dependency onto either wire dialect at all). Not
//! `backend_parity.rs` either: that file's whole point is proving a
//! *third-party* embedder's surface (`conway::backend`) is complete, so it
//! deliberately never imports `conway_core`/`conway_runtime` directly and
//! carries its own hand-rolled `StubBackend` with no wire format of its
//! own — the opposite of what this file needs. `conway/tests/` already
//! carries the real dependency edges this needs for free: `conway-runtime`
//! is an ordinary `[dependencies]` entry (so `prefix_key` is reachable),
//! and `conway-plugin-backends` is a `[dev-dependencies]` entry already
//! used by `fanout_prefix_sharing.rs` for the identical "drive a real
//! adapter over loopback wiremock" mechanism this file reuses for BOTH
//! wire families.
//!
//! Each fixture below is the same three-segment static+inherited shape
//! `boundary_index` (`prefix.rs`) computes a key over: `[0] AgentDef
//! (static) -> [1] ToolRegistry (static, empty content) -> [2] Inherited
//! (inherited tier)`, followed by `[3]` a `ForkDirective` (volatile tier —
//! excluded from the key, the same segment kind two real fork siblings
//! diverge on). Two request variants built from the same `[0..=2]` content
//! but fresh (random) `SegmentId`s — exactly what two sibling forks
//! produce, per `prefix.rs`'s own doc on why `id` is excluded — must hash
//! equal AND must serialize an identical wire prefix. A third variant that
//! changes `[0]`'s text (still within the hashed boundary) must hash
//! different AND must serialize a different wire prefix.

use std::collections::BTreeMap;

use conway_core::content::{ContentBlock, Role, SamplingParams};
use conway_core::ids::{AgentId, BackendId, ModelId, SeqRange, SessionId};
use conway_core::ports::{Backend, GenerateRequest};
use conway_core::provenance::Provenance;
use conway_core::segment::PromptSegment;
use conway_plugin_backends::anthropic::AnthropicBackend;
use conway_plugin_backends::config::{AnthropicConfig, Dialect, OpenAiCompatConfig, SecretString};
use conway_plugin_backends::openai_compat::OpenAiCompatBackend;
use conway_runtime::context::prefix_key;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The `[0..=2]` static+inherited prefix `prefix_key`'s boundary covers —
/// fresh `SegmentId`s every call (via `PromptSegment::new`), the same as
/// two independent sibling forks reading the identical parent state.
fn boundary_segments(agent_def_text: &str, from: SessionId) -> Vec<PromptSegment> {
    vec![
        PromptSegment::new(
            Role::System,
            vec![ContentBlock::Text {
                text: agent_def_text.into(),
            }],
            Provenance::AgentDef {
                name: "reviewer".into(),
            },
        ),
        PromptSegment::new(
            Role::System,
            Vec::new(),
            Provenance::ToolRegistry {
                hash: "deadbeef".into(),
            },
        ),
        PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text {
                text: "Investigate the failing test.".into(),
            }],
            Provenance::Inherited {
                from,
                seq_range: SeqRange::full(),
            },
        ),
    ]
}

/// Appends `[3]`, a volatile `ForkDirective` — outside `prefix_key`'s
/// boundary, so a distinct `by` and distinct text here must never change
/// the key even though it changes the request.
fn with_fork_directive(
    mut segments: Vec<PromptSegment>,
    directive_text: &str,
) -> Vec<PromptSegment> {
    segments.push(PromptSegment::new(
        Role::User,
        vec![ContentBlock::Text {
            text: directive_text.into(),
        }],
        Provenance::ForkDirective { by: AgentId::new() },
    ));
    segments
}

fn generate_request(model: &ModelId, segments: Vec<PromptSegment>) -> GenerateRequest {
    GenerateRequest {
        model: model.clone(),
        segments,
        tools: vec![],
        params: SamplingParams::default(),
        prefix_key: None,
    }
}

// ---------------------------------------------------------------------
// Anthropic-native
// ---------------------------------------------------------------------

fn anthropic_config(base_url: &str) -> AnthropicConfig {
    AnthropicConfig {
        id: BackendId::new("fake"),
        // A syntactically-shaped but fake credential, matching
        // `anthropic_cache_mapping.rs`'s own fixture — never a live key,
        // never a live network call (loopback `wiremock` only).
        api_key: SecretString::new("sk-ant-api03-test-key"),
        base_url: base_url.parse().unwrap(),
        anthropic_version: "2023-06-01".into(),
        timeout: None,
        models: BTreeMap::new(),
    }
}

fn anthropic_ok_response() -> Value {
    json!({
        "content": [{"type": "text", "text": "ok"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    })
}

#[tokio::test]
async fn anthropic_prefix_key_equality_matches_wire_prefix_byte_identity() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(anthropic_ok_response()))
        .expect(3)
        .mount(&server)
        .await;

    let model = ModelId::new("claude-sonnet-4-6");
    let from = SessionId::new();

    // Variant A and B: same boundary content, fresh SegmentIds, different
    // (volatile) fork directive text — exactly two sibling forks.
    let segments_a = with_fork_directive(
        boundary_segments("You are a careful reviewer.", from),
        "FORK_A review correctness",
    );
    let segments_b = with_fork_directive(
        boundary_segments("You are a careful reviewer.", from),
        "FORK_B review style",
    );
    // Variant C: the boundary itself differs (AgentDef text changed) —
    // must hash to a DIFFERENT key.
    let segments_c = with_fork_directive(
        boundary_segments("You are a strict security reviewer.", from),
        "FORK_C review security",
    );

    let key_a = prefix_key(&model, &segments_a);
    let key_b = prefix_key(&model, &segments_b);
    let key_c = prefix_key(&model, &segments_c);
    assert_eq!(
        key_a, key_b,
        "sibling forks sharing boundary content but distinct SegmentIds/directives must hash equal"
    );
    assert_ne!(
        key_a, key_c,
        "a boundary-content change must change the key — otherwise this test could pass vacuously"
    );

    let backend = AnthropicBackend::new(anthropic_config(&server.uri())).unwrap();
    backend
        .generate(generate_request(&model, segments_a))
        .await
        .unwrap();
    backend
        .generate(generate_request(&model, segments_b))
        .await
        .unwrap();
    backend
        .generate(generate_request(&model, segments_c))
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 3);
    let body_a: Value = requests[0].body_json().unwrap();
    let body_b: Value = requests[1].body_json().unwrap();
    let body_c: Value = requests[2].body_json().unwrap();

    // `[0]` AgentDef -> body["system"][0]; `[2]` Inherited (User role) ->
    // body["messages"][0]; `[1]` ToolRegistry contributes to neither (it
    // has no `tools` array here to target). This is the semantic wire
    // prefix `prefix_key`'s `[0..=2]` boundary corresponds to.
    assert_eq!(
        body_a["system"], body_b["system"],
        "equal PrefixKey (A, B) must serialize an identical system block"
    );
    assert_eq!(
        body_a["messages"][0], body_b["messages"][0],
        "equal PrefixKey (A, B) must serialize an identical leading message"
    );
    // Sanity: the two requests are not trivially identical overall — the
    // volatile tail genuinely differs, so the equality above is not
    // vacuous.
    assert_ne!(
        body_a["messages"][1], body_b["messages"][1],
        "each sibling's own fork directive must differ, or this test proves nothing"
    );

    // Negative half: variant C's DIFFERENT PrefixKey must correspond to a
    // DIFFERENT wire prefix — the property does not hold in only one
    // direction. This fixture only changes segment `[0]` (AgentDef) between
    // A and C, so `system` (which `[0]` maps to) is where the wire
    // difference must land; `messages[0]` (segment `[2]`, unchanged text)
    // legitimately stays equal — a `PrefixKey` mismatch means the WHOLE
    // hashed boundary is not proven identical, not that every individual
    // segment within it must differ.
    assert_ne!(
        body_a["system"], body_c["system"],
        "different PrefixKey (A, C) must NOT serialize an identical system block"
    );
}

// ---------------------------------------------------------------------
// OpenAI-compatible
// ---------------------------------------------------------------------

fn openai_compat_config(base_url: &str) -> OpenAiCompatConfig {
    OpenAiCompatConfig {
        id: BackendId::new("fake"),
        base_url: base_url.parse().unwrap(),
        api_key: None,
        profile: Dialect::OpenAi.profile(),
        timeout: None,
        metadata_path: None,
        models: BTreeMap::new(),
    }
}

fn openai_compat_ok_response() -> Value {
    json!({
        "choices": [{
            "message": {"role": "assistant", "content": "ok"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1}
    })
}

#[tokio::test]
async fn openai_compat_prefix_key_equality_matches_wire_prefix_byte_identity() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_compat_ok_response()))
        .expect(3)
        .mount(&server)
        .await;

    let model = ModelId::new("gpt-4.1");
    let from = SessionId::new();

    let segments_a = with_fork_directive(
        boundary_segments("You are a careful reviewer.", from),
        "FORK_A review correctness",
    );
    let segments_b = with_fork_directive(
        boundary_segments("You are a careful reviewer.", from),
        "FORK_B review style",
    );
    let segments_c = with_fork_directive(
        boundary_segments("You are a strict security reviewer.", from),
        "FORK_C review security",
    );

    let key_a = prefix_key(&model, &segments_a);
    let key_b = prefix_key(&model, &segments_b);
    let key_c = prefix_key(&model, &segments_c);
    assert_eq!(
        key_a, key_b,
        "sibling forks sharing boundary content but distinct SegmentIds/directives must hash equal"
    );
    assert_ne!(
        key_a, key_c,
        "a boundary-content change must change the key — otherwise this test could pass vacuously"
    );

    let backend = OpenAiCompatBackend::new(openai_compat_config(&server.uri())).unwrap();
    backend
        .generate(generate_request(&model, segments_a))
        .await
        .unwrap();
    backend
        .generate(generate_request(&model, segments_b))
        .await
        .unwrap();
    backend
        .generate(generate_request(&model, segments_c))
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 3);
    let body_a: Value = requests[0].body_json().unwrap();
    let body_b: Value = requests[1].body_json().unwrap();
    let body_c: Value = requests[2].body_json().unwrap();

    // No native cache-control side channel for this dialect (unlike
    // Anthropic's `tools`/`system` split), so the OpenAI-compatible wire
    // prefix is `messages[0..=1]` in full: `[0]` AgentDef's `system`
    // message, `[1]` the Inherited segment's `user` message. `[1]`
    // ToolRegistry contributes no message at all (`segment_to_messages`
    // skips it), so `messages` has exactly 3 entries: [0]=AgentDef,
    // [1]=Inherited, [2]=ForkDirective.
    assert_eq!(
        body_a["messages"][0], body_b["messages"][0],
        "equal PrefixKey (A, B) must serialize an identical system message"
    );
    assert_eq!(
        body_a["messages"][1], body_b["messages"][1],
        "equal PrefixKey (A, B) must serialize an identical inherited message"
    );
    assert_ne!(
        body_a["messages"][2], body_b["messages"][2],
        "each sibling's own fork directive must differ, or this test proves nothing"
    );

    assert_ne!(
        body_a["messages"][0], body_c["messages"][0],
        "different PrefixKey (A, C) must NOT serialize an identical system message"
    );
}
