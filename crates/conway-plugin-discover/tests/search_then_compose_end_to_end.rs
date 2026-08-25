//! End-to-end acceptance for `conway.discover` (board item
//! `01M0PS8J3AK7Z7253Z3E3RD3GY`): a real, fully-faked `Conway` (no network,
//! no live provider) drives `search_sessions` through a REAL model turn
//! (`ScriptedBackend`), never `Tool::invoke` called directly, finds a
//! record in a session it neither started nor spawned, hands the found
//! `(session, seq)` to `compose_context_path` (`conway-plugin-path`), and
//! proves the composed content survives a LATER, independently-scripted
//! turn -- the acceptance criterion this item names explicitly: "prove the
//! pair works together, not each half separately."
//!
//! Harness mirrors `conway-plugin-path`'s own
//! `tests/compose_context_path_end_to_end.rs` closely: session B is minted
//! by a THROWAWAY `Conway` sharing the SAME `conway_testkit::FakeStore`,
//! then a SECOND `Conway` (session A, the one under test) is built against
//! that same store -- session A never forks or spawns session B, so
//! anything A finds about B can only have come through discovery.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::backend::{BackendId, GenerateRequest, GenerateResponse, ModelId, StopReason, Usage};
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::plugin::{ContentBlock, Event, ToolCall, ToolName};
use conway::{
    Conway, ConwayBuilder, EventStream, ModelRef, PermissionDecision, RoleAlias, SessionSpec,
};
use conway_testkit::{
    text_response, FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn,
};

use conway_plugin_discover::{DiscoverPlugin, SEARCH_TOOL_NAME};
use conway_plugin_path::{PathPlugin, COMPOSE_TOOL_NAME};

fn base_config(cwd: std::path::PathBuf) -> ConwayConfig {
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
        cwd,
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

fn tool_call_response(call_id: &str, tool: &str, args: serde_json::Value) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: ToolName::new(tool),
            arguments: args,
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

/// A real, fully-faked `Conway` with BOTH first-party plugins attached the
/// way a library embedder would (`ConwayBuilder::with_plugin`) -- the same
/// call `crates/conway-cli/src/first_party_plugins.rs` makes internally
/// once both are named in `[plugins].install`.
fn build_conway(
    cwd: std::path::PathBuf,
    backend: Arc<ScriptedBackend>,
    store: Arc<FakeStore>,
) -> Conway {
    let gate: Arc<dyn conway::PermissionGate> =
        Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let router: Arc<dyn conway::Router> = Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }));
    ConwayBuilder::from_parts(base_config(cwd))
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(router)
        .with_plugin(Arc::new(DiscoverPlugin))
        .with_plugin(Arc::new(PathPlugin))
        .build()
        .expect("build should succeed with every port injected")
}

fn multi_turn_spec() -> SessionSpec {
    SessionSpec {
        keep_alive: true,
        ..SessionSpec::default()
    }
}

/// See `conway-plugin-path`'s own identical helper for why `TurnFinished`
/// counting (not `TurnHandle::text()`/`::result()`) is what a tool-calling,
/// keep-alive turn needs.
async fn drain_n_turn_finished(stream: &mut EventStream, steps: usize) {
    use futures_core::Stream as _;
    let mut seen = 0usize;
    loop {
        let envelope = tokio::time::timeout(
            Duration::from_secs(5),
            std::future::poll_fn(|cx| std::pin::Pin::new(&mut *stream).poll_next(cx)),
        )
        .await
        .expect("event stream must not hang")
        .expect("event stream must not end mid-session");
        if let Event::TurnFinished { .. } = envelope.event {
            seen += 1;
            if seen == steps {
                return;
            }
        }
    }
}

async fn run_turn(
    session: &conway::SessionHandle,
    events: &mut EventStream,
    prompt: &str,
    steps: usize,
) {
    session.prompt(prompt).await.expect("prompt");
    drain_n_turn_finished(events, steps).await;
}

/// Unlike `conway-plugin-path`'s own identical-looking helper, this ALSO
/// recurses into `ContentBlock::ToolResultBlock` -- `search_sessions`'s own
/// reply is a tool result, which the wire represents as a `ToolResultBlock`
/// wrapping its `Text` block(s), not a bare top-level `Text` block. The
/// path plugin's own test never needed this (it only ever asserted on
/// plain `UserTurn` text), so this is a genuine, disclosed difference, not
/// a copy-paste oversight.
fn all_text(req: &GenerateRequest) -> String {
    fn walk(blocks: &[ContentBlock], out: &mut String) {
        for block in blocks {
            match block {
                ContentBlock::Text { text } => {
                    out.push_str(text);
                    out.push('\n');
                }
                ContentBlock::ToolResultBlock { blocks, .. } => walk(blocks, out),
                _ => {}
            }
        }
    }
    let mut out = String::new();
    for segment in &req.segments {
        walk(&segment.content, &mut out);
    }
    out
}

#[test]
fn manifest_id_matches_the_published_constant() {
    use conway::plugin::Plugin as _;
    assert_eq!(
        DiscoverPlugin.manifest().id,
        conway_plugin_discover::PLUGIN_ID
    );
}

/// THE end-to-end proof (acceptance criterion 1): session A finds session
/// B's record through `search_sessions` -- a session A neither named up
/// front nor spawned -- and the found `(session, seq)` composes onto A's
/// path through `compose_context_path`, surviving a LATER,
/// independently-scripted proof turn. The search step's OWN reply (what
/// was found, what was searched, what it cost) is asserted on the wire
/// too -- acceptance criterion 2, "what was searched, and what it cost, is
/// visible."
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_finds_a_record_and_composing_it_survives_the_next_turn() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(FakeStore::new());

    // Session B: minted by a THROWAWAY Conway sharing the same store.
    // Session A (below) never forks or spawns this -- it is a genuinely
    // foreign session, exactly the board item's own motivating example
    // ("yesterday's session, same project").
    let mint_backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("b replies"))])
            .with_id(BackendId::new("fake")),
    );
    let mint_conway = build_conway(tmp.path().to_path_buf(), mint_backend, store.clone());
    let session_b = mint_conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session b");
    let mut b_events = session_b.events();
    run_turn(
        &session_b,
        &mut b_events,
        "unique-retry-logic-marker-from-session-b",
        1,
    )
    .await;
    let b_id = session_b.id();

    // Session A: the one under test. Never told `b_id` in its prompts --
    // only the SCRIPT (standing in for a model that already read the
    // search tool's own reply) names it, in the compose step, exactly as
    // `compose_context_path`'s own end-to-end test scripts a resolved ref.
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("a's own first reply")),
            ScriptedTurn::Respond(tool_call_response(
                "tc_search",
                SEARCH_TOOL_NAME,
                serde_json::json!({"text": "retry-logic-marker"}),
            )),
            ScriptedTurn::Respond(text_response("found it")),
            ScriptedTurn::Respond(tool_call_response(
                "tc_compose",
                COMPOSE_TOOL_NAME,
                serde_json::json!({
                    "include": [{"session": b_id.to_string(), "seq": 0}],
                }),
            )),
            ScriptedTurn::Respond(text_response("composed")),
            ScriptedTurn::Respond(text_response("proof turn reply")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(tmp.path().to_path_buf(), backend.clone(), store.clone());

    let session_a = conway
        .new_session(multi_turn_spec())
        .await
        .expect("new_session a");
    let mut a_events = session_a.events();
    run_turn(&session_a, &mut a_events, "a's own first prompt", 1).await;
    run_turn(
        &session_a,
        &mut a_events,
        "find what we said about retry logic yesterday",
        2, // tool-call generation, then the follow-up "found it"
    )
    .await;

    // Acceptance criterion 2, asserted where the model itself would read
    // it: the generation immediately after the search tool ran (index 2,
    // 0-based: [0]=a's first turn, [1]=search tool-call gen, [2]=the
    // follow-up gen, whose REQUEST already carries the search tool's own
    // result).
    let calls = backend.calls();
    let after_search = calls.get(2).expect("at least 3 calls recorded by now");
    let after_search_text = all_text(after_search);
    assert!(
        after_search_text.contains(&b_id.to_string()),
        "the search result must name the foreign session it found: {after_search_text}"
    );
    assert!(
        after_search_text.contains("unique-retry-logic-marker-from-session-b"),
        "the search result must show a snippet of the matching record: {after_search_text}"
    );
    assert!(
        after_search_text.contains("session(s) considered")
            && after_search_text.contains("record(s) read"),
        "the search result must state what was searched and what it cost: {after_search_text}"
    );

    run_turn(
        &session_a,
        &mut a_events,
        "bring that in",
        2, // tool-call generation, then the follow-up "composed"
    )
    .await;
    run_turn(&session_a, &mut a_events, "proof turn", 1).await;

    let calls = backend.calls();
    let proof_request = calls.last().expect("at least one call recorded");
    let text = all_text(proof_request);
    assert!(
        text.contains("unique-retry-logic-marker-from-session-b"),
        "the composed foreign record must be in the proof turn's context, proving the \
         search -> compose pair survives a later turn, not just the turn it landed on: {text}"
    );
    assert!(
        text.contains("a's own first prompt"),
        "the own tail must survive by default (no drop_own_tail): {text}"
    );
}
