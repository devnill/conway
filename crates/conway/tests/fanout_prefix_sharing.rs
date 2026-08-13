//! Board item 01KZDDPZPAXFHDF1YTZG7F95EG: `PHILOSOPHY.md`'s "Useful
//! patterns" describes map/gather and panel as forking once so several
//! children share a common inherited prefix, then giving each a different
//! directive. "Working with the cache" prices that arrangement: "siblings
//! forked at the same point open with the same bytes... which is the
//! economic argument for map and gather." That claim is only true if N
//! sibling `conway_fork` calls issued from ONE parent turn actually produce
//! N requests sharing a byte-identical leading run, WITH the cache
//! breakpoint landing inside that shared run -- and only true ON THE WIRE,
//! not merely as an in-process `Arc::ptr_eq` (`conway-runtime::subagent`'s
//! own module doc already asserts the latter via the `peek_prefix` test
//! seam; that is necessary but not sufficient -- see this item's own
//! completion report).
//!
//! This file drives the arrangement through the REAL, model-reachable
//! surface: a real `conway_fork` tool call (`conway-tools`, `builtin-tools`
//! feature, not a hand-rolled fake), dispatched three times in a single
//! assistant turn (three `tool_use` blocks -- exactly what a model
//! requesting a three-way map/gather fan-out would emit), against a REAL
//! `AnthropicBackend` pointed at a loopback `wiremock` server (P-15:
//! credential-free -- the same fake API key `anthropic_cache_mapping.rs`
//! uses, never a live network call). Every wire body this test asserts on is
//! the adapter's own `wire::build_request_body` + `cache::apply_cache_hints`
//! output, captured off the actual HTTP request `wiremock` received --
//! never internal state.
//!
//! ## Why the assertion targets the SEMANTIC prefix, not raw HTTP bytes
//!
//! `serde_json`'s default `Map` (no `preserve_order` feature anywhere in
//! this workspace -- verified against `Cargo.lock`) serializes object keys
//! in `BTreeMap` (alphabetical) order, not insertion order. That reorders
//! `{model, max_tokens, system, messages, tools, ...}` alphabetically on the
//! wire, which would put `"messages"` (containing the one PER-SIBLING
//! block) ahead of `"system"`/`"tools"` (wholly shared) in the raw HTTP
//! body text -- so a literal "first K bytes of the HTTP body" comparison
//! would materially understate what is actually shared, for a reason that
//! has nothing to do with Anthropic's real caching behavior (Anthropic
//! parses `system`/`tools`/`messages` as independent JSON fields; its cache
//! matching is defined over that parsed, ordered prompt structure -- system,
//! then tools, then message history, exactly the order `conway-runtime`'s
//! `ContextBuilder` assembles segments in -- not over the literal byte
//! offset of a JSON object key in the transmitted text). This file therefore
//! asserts byte-identity of each of those three WIRE FIELDS independently
//! (`system`, `tools`, and every `messages` entry up to and including the
//! shared boundary), which is the semantically correct operationalization
//! of "byte-identical leading run" for this wire format, and additionally
//! confirms the `cache_control` breakpoint lands on the LAST block of that
//! shared boundary -- exactly where a real Anthropic server's prefix match
//! would need it to be for the second and later sibling to hit cache.
#![cfg(feature = "builtin-tools")]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
    TuiSection,
};
use conway::{Conway, ConwayBuilder, SessionSpec};
use conway_core::agent::{PermissionDecision, ResultStatus};
use conway_core::fakes::{FakeGate, FakeRouter, FakeStore};
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias};
use conway_core::ports::Backend;
use conway_plugin_backends::anthropic::AnthropicBackend;
use conway_plugin_backends::config::{AnthropicConfig, SecretString};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The backend id [`fake_router`] always resolves to, and the id the real
/// [`AnthropicBackend`] built below is constructed under -- the two must
/// agree for `AttemptEngine` to find a candidate `Backend` for the router's
/// chosen route.
fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("claude-sonnet-4-6"),
    }))
}

/// Distinct directive text for each sibling's `conway_fork` call -- an
/// exact-match sentinel (never a mere substring of anything else in a
/// request body), so filtering `wiremock`'s captured requests for "is this
/// one of the three sibling requests" cannot accidentally match the
/// parent's own turns.
const DIRECTIVES: [&str; 3] = [
    "FANOUT_MARKER_REVIEW_CORRECTNESS",
    "FANOUT_MARKER_REVIEW_STYLE",
    "FANOUT_MARKER_REVIEW_SECURITY",
];

fn base_config() -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "default".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    ConwayConfig {
        default_role: RoleAlias::new("default"),
        cwd: std::path::PathBuf::from("."),
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        permissions: PermissionsConfig::default(),
        backends: BTreeMap::new(),
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig::default(),
        tui: TuiSection::default(),
        // `ToolsConfig::default()` includes `"conway.subagent"` (the plugin
        // registering `conway_fork`/`conway_spawn`) -- unchanged, so this
        // test exercises the exact tool surface a model gets out of the box.
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

fn anthropic_config(base_url: &str) -> AnthropicConfig {
    AnthropicConfig {
        id: BackendId::new("fake"),
        // P-15: a syntactically-shaped but fake credential, matching
        // `anthropic_cache_mapping.rs`'s own fixture -- never a live key,
        // never a live network call (loopback `wiremock` only).
        api_key: SecretString::new("sk-ant-api03-test-key"),
        base_url: base_url.parse().unwrap(),
        anthropic_version: "2023-06-01".into(),
        timeout: None,
        models: BTreeMap::new(),
    }
}

/// `anthropic_defaults()` (`conway-plugin-backends::capabilities`) declares
/// `tool_calling: ToolCallSupport::Streaming { validated: true }` with no
/// per-model override field able to change it (`ModelOverrides` has no
/// `tool_calling` key -- only `metadata` can, and `AnthropicBackend::new`
/// hardcodes `ModelMetadataStore::defaults()`, not caller-suppliable), so
/// `attempt.rs`'s `Strategy::Stream` selection is unconditional here --
/// every mocked response in this file MUST be real Anthropic SSE, not a
/// single JSON document, or `AnthropicBackend::stream` silently parses zero
/// events and the turn "completes" having sent nothing.
fn sse_event(json: Value) -> String {
    format!("data: {json}\n\n")
}

/// The parent's FIRST turn response: one assistant message carrying THREE
/// `tool_use` blocks, all named `conway_fork`, each with its own directive
/// -- exactly what a model requesting a three-way map/gather fan-out would
/// emit in a single turn. `wire.rs`'s `segments_to_body_parts` appends all
/// three `ToolUse` blocks onto ONE assistant record (WI-122), so `run_batch`
/// dispatches all three `conway_fork` calls against the SAME parent session
/// head -- see `conway-runtime::subagent`'s own module doc, "`InheritedPrefix`
/// and sibling sharing", for why that timing is what makes the three
/// children siblings at the same fork point rather than three independent,
/// serially-drifted forks.
fn fanout_sse() -> String {
    let mut body = String::new();
    body.push_str(&sse_event(json!({"type": "message_start", "message": {"usage": {"input_tokens": 10, "output_tokens": 0}}})));
    for (i, directive) in DIRECTIVES.iter().enumerate() {
        let index = i as u32;
        let id = format!("call_{i}");
        body.push_str(&sse_event(json!({
            "type": "content_block_start",
            "index": index,
            "content_block": {"type": "tool_use", "id": id, "name": "conway_fork", "input": {}}
        })));
        let partial_json = json!({ "prompt": directive }).to_string();
        body.push_str(&sse_event(json!({
            "type": "content_block_delta",
            "index": index,
            "delta": {"type": "input_json_delta", "partial_json": partial_json}
        })));
        body.push_str(&sse_event(
            json!({"type": "content_block_stop", "index": index}),
        ));
    }
    body.push_str(&sse_event(json!({
        "type": "message_delta",
        "delta": {"stop_reason": "tool_use"},
        "usage": {"output_tokens": 10}
    })));
    body.push_str(&sse_event(json!({"type": "message_stop"})));
    body
}

/// Every turn after the first (each child's own first turn, and the
/// parent's follow-up turn once the three fork results return) ends
/// immediately with plain text -- nothing under test depends on what any of
/// them says.
fn plain_ok_sse() -> String {
    let mut body = String::new();
    body.push_str(&sse_event(json!({"type": "message_start", "message": {"usage": {"input_tokens": 1, "output_tokens": 0}}})));
    body.push_str(&sse_event(json!({
        "type": "content_block_start",
        "index": 0,
        "content_block": {"type": "text", "text": ""}
    })));
    body.push_str(&sse_event(json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": {"type": "text_delta", "text": "ok"}
    })));
    body.push_str(&sse_event(
        json!({"type": "content_block_stop", "index": 0}),
    ));
    body.push_str(&sse_event(json!({
        "type": "message_delta",
        "delta": {"stop_reason": "end_turn"},
        "usage": {"output_tokens": 1}
    })));
    body.push_str(&sse_event(json!({"type": "message_stop"})));
    body
}

/// The `messages` array index up to which (exclusive) `body_a` and `body_b`
/// must agree, per this file's module doc ("the semantic prefix, not raw
/// HTTP bytes"): every entry strictly before the final, sibling-own
/// `ForkDirective` message. Panics (via the assertion) if the two bodies'
/// `messages` arrays are not exactly one entry apart in content, since that
/// is the only shape a shared-prefix-plus-one-directive fork produces.
fn shared_message_count(body_a: &Value, body_b: &Value) -> usize {
    let messages_a = body_a["messages"].as_array().expect("messages array");
    let messages_b = body_b["messages"].as_array().expect("messages array");
    assert_eq!(
        messages_a.len(),
        messages_b.len(),
        "two siblings forked from the same point must produce the same NUMBER of \
         messages (shared inherited history + exactly one own ForkDirective each): \
         a={messages_a:?} b={messages_b:?}"
    );
    // Every message except the last must be byte-for-byte (deep-equal, which
    // for a canonical `serde_json::Value` comparison is the same thing)
    // identical -- the last is each sibling's own directive and is expected
    // to differ.
    let shared = messages_a.len() - 1;
    for i in 0..shared {
        assert_eq!(
            messages_a[i], messages_b[i],
            "message {i} must be byte-identical across siblings forked from the same \
             point (the shared inherited prefix) -- a={:?} b={:?}",
            messages_a[i], messages_b[i]
        );
    }
    shared
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn n_siblings_forked_from_one_point_share_a_byte_identical_leading_run_on_the_wire() {
    let server = MockServer::start().await;

    // Mock A: the parent's very first request only (`up_to_n_times(1)`,
    // `with_priority(1)` -- checked before the unlimited catch-all below,
    // per wiremock's own "lower number = higher priority" rule) -- the
    // three-tool_use fan-out turn.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(fanout_sse(), "text/event-stream"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    // Mock B: everything after (three children's own first turns, plus the
    // parent's follow-up turn once all three fork results return).
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(plain_ok_sse(), "text/event-stream"))
        .mount(&server)
        .await;

    let backend = Arc::new(
        AnthropicBackend::new(anthropic_config(&server.uri())).expect("valid backend config"),
    );
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let conway: Conway = ConwayBuilder::from_parts(base_config())
        .with_backend(backend as Arc<dyn Backend>)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should succeed with the real builtin conway_fork tool registered");

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle
        .prompt("Review this change from three angles, in parallel.")
        .await
        .expect("prompt should succeed");
    let result = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("the whole turn (parent + 3 forked children + parent follow-up) must not hang")
        .expect("result() must not error");
    assert_eq!(
        result.status,
        ResultStatus::Completed,
        "got: {:?}",
        result.status
    );

    // Identify the three sibling requests: each one's LAST message's LAST
    // content block is an exact-match one of DIRECTIVES (the ForkDirective
    // `ContextBuilder` appends as this child's own head segment -- see
    // `context/builder.rs`'s `[4] ForkDirective | Prompt` step). No other
    // request in this run (the parent's own two turns) can produce that
    // shape, since the parent's own messages end in ToolResult/tool_use
    // content, never a bare directive text block.
    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(
        requests.len(),
        5,
        "expected exactly 5 requests: the parent's fan-out turn, the 3 children's own \
         first turns, and the parent's follow-up turn after the 3 fork results return"
    );
    let mut siblings: Vec<Value> = Vec::new();
    for req in &requests {
        let Ok(body) = req.body_json::<Value>() else {
            continue;
        };
        let Some(messages) = body["messages"].as_array() else {
            continue;
        };
        let Some(last_message) = messages.last() else {
            continue;
        };
        let Some(last_block) = last_message["content"].as_array().and_then(|c| c.last()) else {
            continue;
        };
        if let Some(text) = last_block["text"].as_str() {
            if DIRECTIVES.contains(&text) {
                siblings.push(body);
            }
        }
    }

    assert_eq!(
        siblings.len(),
        3,
        "expected exactly 3 sibling fork requests (one per DIRECTIVES entry), got {}: {:?}",
        siblings.len(),
        siblings.iter().map(|b| &b["messages"]).collect::<Vec<_>>()
    );

    // --- The requirement itself: N differently-prompted siblings forked
    // from ONE point present a byte-identical leading run, with the cache
    // breakpoint positioned inside it. ---

    // `system`: wholly shared (there is no per-sibling system content at
    // all -- `HeadSegment::ForkDirective` is a `Role::User` message, never a
    // `system` entry).
    for pair in siblings.windows(2) {
        assert_eq!(
            pair[0]["system"], pair[1]["system"],
            "system array must be byte-identical across siblings"
        );
    }

    // `tools`: wholly shared, INCLUDING the `cache_control` breakpoint A
    // marker on the last entry (`BreakpointTarget::Tools` -- `cache.rs`).
    for pair in siblings.windows(2) {
        assert_eq!(
            pair[0]["tools"], pair[1]["tools"],
            "tools array (native schema + breakpoint A cache_control) must be \
             byte-identical across siblings"
        );
    }
    let tools = siblings[0]["tools"]
        .as_array()
        .expect("builtin conway.subagent/conway.fs/conway.report tools registered");
    assert!(
        !tools.is_empty(),
        "the builtin tool set must be non-empty for this to be a meaningful assertion"
    );
    assert_eq!(
        tools.last().unwrap()["cache_control"],
        json!({"type": "ephemeral"}),
        "breakpoint A must land on the last tool: {tools:?}"
    );

    // `messages`: every entry before the sibling-own trailing directive must
    // be byte-identical, for every pair of siblings.
    let shared_a_b = shared_message_count(&siblings[0], &siblings[1]);
    let shared_a_c = shared_message_count(&siblings[0], &siblings[2]);
    assert_eq!(
        shared_a_b, shared_a_c,
        "the shared-history boundary must be the SAME position for every sibling"
    );
    assert!(
        shared_a_b > 0,
        "the parent's own pre-fork turn (its prompt + the assistant turn requesting the \
         three forks) must have produced at least one real inherited message -- otherwise \
         this test cannot distinguish 'siblings share history' from 'siblings share nothing \
         because there was nothing to share'"
    );

    // Breakpoint B: `cache_control` on the LAST content block of the LAST
    // SHARED message -- exactly the boundary a real Anthropic server needs
    // it at for the second and later sibling to hit cache on everything
    // through that point.
    for sibling in &siblings {
        let messages = sibling["messages"].as_array().unwrap();
        let last_shared = &messages[shared_a_b - 1];
        let last_block = last_shared["content"]
            .as_array()
            .and_then(|c| c.last())
            .expect("last shared message must carry at least one content block");
        assert_eq!(
            last_block["cache_control"],
            json!({"type": "ephemeral"}),
            "breakpoint B must land on the last shared inherited block: {last_block:?}"
        );
    }

    // Negative half of the claim, so this test cannot pass vacuously: each
    // sibling's OWN trailing message really does differ from the others'
    // (otherwise "shared_message_count == messages.len() - 1" would hold
    // even if the whole array were identical, e.g. if the tool never
    // actually threaded each call's own `prompt` argument through).
    let own_texts: Vec<&str> = siblings
        .iter()
        .map(|body| {
            let messages = body["messages"].as_array().unwrap();
            messages
                .last()
                .unwrap()
                .pointer("/content/0/text")
                .and_then(Value::as_str)
                .expect("own trailing message must carry text")
        })
        .collect();
    assert_eq!(
        own_texts.len(),
        std::collections::HashSet::<&str>::from_iter(own_texts.iter().copied()).len(),
        "each sibling's own trailing directive must be distinct: {own_texts:?}"
    );
}
