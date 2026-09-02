//! Pins the exact cache cost `docs/plugins/memory.md`'s new "Cache cost"
//! section names: a `remember` call, once its `ToolResult` is recorded,
//! shifts every later request's static prefix by inserting one
//! `Provenance::Memory` segment immediately before the first
//! `Provenance::ToolRegistry` segment (`MemoryInjectHook::insertion_index`,
//! `crates/conway-plugin-memory/src/lib.rs`) -- INSIDE the static preamble,
//! not appended to the volatile tail. `conway-runtime`'s own
//! `prefix_stability.rs` proves the general "each request extends the
//! previous one" property holds for ordinary turns; this file proves the
//! one first-party plugin the item names as a known exception to it, and
//! PINS the exact divergence point rather than merely asserting "something
//! changed".
//!
//! Facade-driven (`conway::Conway`, `conway::test_support::test_builder`),
//! matching this crate's own `memory_end_to_end.rs` precedent: a real
//! `remember` tool call, scripted through a `ScriptedBackend`, not a
//! hand-rolled `MemoryStore::put` -- the cost this item documents is what a
//! MODEL-initiated `remember` costs the very next request, and only a real
//! tool-call round trip through `AgentLoop`/`ToolRunner` proves the write
//! actually lands before that request is built (not merely that
//! `MemoryStore::put` and `ContextBuilder::build` each work in isolation).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::backend::{BackendId, GenerateResponse, StopReason, Usage};
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::plugin::{
    ContentBlock, MemoryStore, PromptSegment, Provenance, Role, ToolCall, ToolName,
};
use conway::test_support::test_builder;
use conway::{RoleAlias, SessionHandle, SessionSpec};
use conway_plugin_memory::{InMemoryMemoryStore, MemoryConfig, MemoryPlugin, REMEMBER_TOOL_NAME};
use conway_testkit::{text_response, FakeStore, ScriptedBackend, ScriptedTurn};

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
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

/// A scripted assistant turn that calls `remember` with `text` and ends the
/// turn there (`StopReason::ToolUse`).
fn remember_call_response(call_id: &str, text: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: ToolName::new(REMEMBER_TOOL_NAME),
            arguments: serde_json::json!({ "text": text }),
        }],
        stop: StopReason::ToolUse,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
    }
}

async fn run_prompt(session: &SessionHandle, text: &str) {
    let turn = session.prompt(text).await.expect("prompt");
    tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");
}

/// `(role, content)` -- the projection that actually reaches the wire (never
/// `Provenance`, which is local-only bookkeeping never serialized to a
/// backend).
fn wire_identity(segments: &[PromptSegment]) -> Vec<(Role, Vec<ContentBlock>)> {
    segments.iter().map(|s| (s.role, s.content.clone())).collect()
}

/// Mirrors `MemoryInjectHook::insertion_index` (private to
/// `conway-plugin-memory`'s own crate, unreachable from this integration
/// test binary): the position of the first `Provenance::ToolRegistry`
/// segment. Computed independently here, from the same PUBLIC
/// `Provenance` tag the hook itself matches on -- not by importing the
/// plugin's private function -- so this test proves the two land on the
/// SAME index by observation, not by construction.
fn first_tool_registry_index(segments: &[PromptSegment]) -> usize {
    segments
        .iter()
        .position(|s| matches!(s.provenance, Provenance::ToolRegistry { .. }))
        .expect("a ToolRegistry segment is unconditional")
}

#[tokio::test]
async fn a_mid_session_remember_shifts_the_prefix_at_exactly_the_insertion_index() {
    let store = Arc::new(FakeStore::new());
    let memory_store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());

    let backend = Arc::new(
        ScriptedBackend::new(vec![
            // Turn one: plain text, no memory exists yet.
            ScriptedTurn::Respond(text_response("hi there")),
            // Turn two: the model calls `remember`, then answers.
            ScriptedTurn::Respond(remember_call_response(
                "c1",
                "the deploy secret lives in vault path secret/data/prod-deploy",
            )),
            ScriptedTurn::Respond(text_response("noted")),
        ])
        .with_id(BackendId::new("fake")),
    );

    let conway = test_builder(base_config())
        .with_backend(backend.clone())
        .with_session_store(store)
        .with_plugin(Arc::new(MemoryPlugin::new(
            memory_store.clone(),
            MemoryConfig::default(),
        )))
        .build()
        .expect("build should succeed with every port injected");

    // A non-trivial (non-zero) insertion index: an `AgentDef` segment ahead
    // of the `ToolRegistry` one, from a plain system-prompt override -- so
    // the pinned divergence index below is not the vacuously-true "0".
    let session = conway
        .new_session(SessionSpec {
            // Multi-turn: this session's root must survive past its first
            // completed turn so `run_prompt` can drive a second one.
            keep_alive: true,
            system_prompt_override: Some("You are a helpful assistant.".to_string()),
            ..Default::default()
        })
        .await
        .expect("new_session should succeed");

    run_prompt(&session, "hello").await;
    run_prompt(&session, "remember something for me").await;

    let calls = backend.calls();
    assert_eq!(
        calls.len(),
        3,
        "turn one (1 call) + turn two's remember round trip (2 calls); calls: {calls:#?}"
    );

    // The request BEFORE the memory was written -- turn one's only request.
    let before_remember = &calls[0];
    // The very NEXT request after `remember`'s `ToolResult` was recorded --
    // turn two's second call, built once `AgentLoop` re-assembles context
    // for the follow-up generate() after the tool round trip. By this
    // point `MemoryStore::put` (inside `RememberTool::invoke`) has already
    // completed, so `MemoryInjectHook::before_request` sees the new memory.
    let after_remember = &calls[2];

    let insertion_index = first_tool_registry_index(&before_remember.segments);
    assert_eq!(
        insertion_index, 1,
        "sanity: this test's fixed session config (one AgentDef segment ahead of \
         ToolRegistry, no skills/instructions) always yields index 1 -- if this fails, the \
         fixture changed, not the property under test"
    );

    let before = wire_identity(&before_remember.segments);
    let after = wire_identity(&after_remember.segments);

    // THE pin: the first index at which the two requests diverge is exactly
    // the memory hook's own insertion index -- not zero (a naive
    // "something differs"), not the volatile tail (which every ordinary
    // turn already extends, per `prefix_stability.rs`), but precisely one
    // segment INSIDE the static preamble. `Some(_)`, not `None`, is itself
    // the "NOT a prefix-extension" half of the acceptance criterion: were
    // `after` merely `before` plus new material at the end (the ordinary,
    // undisturbed case), `.position` below would find no differing pair at
    // all within `before`'s own length and return `None`.
    let divergence = before
        .iter()
        .zip(after.iter())
        .position(|(b, a)| b != a);
    assert_eq!(
        divergence,
        Some(insertion_index),
        "a mid-session `remember` must shift the very next request's prefix starting \
         exactly at the memory hook's insertion index -- got divergence {divergence:?} vs. \
         insertion_index {insertion_index}; before: {before:#?}; after: {after:#?}"
    );

    // Sanity: the segment actually AT the divergence index in `after` is
    // the freshly injected memory, carrying the honestly-attributed
    // `Provenance::Memory` tag and the remembered text -- not some other,
    // coincidental change.
    let injected = &after_remember.segments[insertion_index];
    assert!(
        matches!(injected.provenance, Provenance::Memory { .. }),
        "the segment at the divergence index must be the injected memory itself, got \
         provenance {:?}",
        injected.provenance
    );
    let injected_text: String = injected
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        injected_text.contains("secret/data/prod-deploy"),
        "the injected segment must carry the just-remembered text, got: {injected_text:?}"
    );
}
