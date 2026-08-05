#![cfg(feature = "fakes")]
//! Conformance tests for the `feature = "fakes"` test doubles (WI-008).
//!
//! These are integration tests: they exercise the fakes exactly as a
//! downstream crate (`conway-runtime`) would — through the public port
//! traits, never through crate-internal APIs.

use std::path::PathBuf;
use std::sync::Arc;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use chrono::Utc;

use conway_core::fakes::{
    CollectingEventSink, FakeBackend, FakeGate, FakeHealth, FakeRouter, FakeStore,
    FakeSubagentHost, ScriptedBackend, ScriptedTurn,
};
use conway_core::prelude::*;

// ---------------------------------------------------------------------
// A tiny, dependency-free executor: none of these fakes ever actually
// `.await` on anything (their state is `std::sync::Mutex`), so every future
// they return resolves on its very first poll. This avoids pulling in
// tokio (or any other async runtime) as a dependency just for tests.
// ---------------------------------------------------------------------

fn noop_waker() -> Waker {
    fn no_op(_: *const ()) {}
    fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
}

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut fut = Box::pin(fut);
    loop {
        if let Poll::Ready(val) = fut.as_mut().poll(&mut cx) {
            return val;
        }
    }
}

fn drain_box_stream<T>(
    mut stream: std::pin::Pin<Box<dyn futures_core::Stream<Item = T> + Send>>,
) -> Vec<T> {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut items = Vec::new();
    loop {
        match stream.as_mut().poll_next(&mut cx) {
            Poll::Ready(Some(item)) => items.push(item),
            Poll::Ready(None) => break,
            Poll::Pending => continue,
        }
    }
    items
}

// ---------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------

fn sample_request() -> GenerateRequest {
    GenerateRequest {
        model: ModelId::new("test-model"),
        segments: vec![PromptSegment::new(
            Role::User,
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

fn sample_response(text: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![ContentBlock::Text { text: text.into() }],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage::default(),
    }
}

fn sample_session_meta(id: SessionId, origin: Option<ForkOrigin>) -> SessionMeta {
    SessionMeta {
        id,
        agent_id: AgentId::new(),
        origin,
        agent_def: None,
        role: None,
        created: Utc::now(),
        cwd: PathBuf::from("/tmp"),
        labels: vec![],
        ephemeral: false,
        ask_origin: None,
        root: None,
    }
}

fn sample_user_turn(i: u64) -> LogRecord {
    LogRecord::UserTurn {
        seq: LogSeq(i),
        ts: Utc::now(),
        text: format!("turn {i}"),
        prov: Provenance::UserPrompt,
    }
}

fn sample_permission_request(agent_path: Vec<AgentId>) -> PermissionRequest {
    PermissionRequest {
        agent_id: *agent_path.last().unwrap(),
        agent_path,
        tool: ToolName::new("read"),
        category: ToolCategory::Read,
        arguments: serde_json::json!({}),
        rendered: "read a file".into(),
        call_id: "tc_1".into(),
        // The real `read` tool declares Structured; mirror it honestly.
        render_kind: conway_core::ports::RenderKind::Structured,
    }
}

// ---------------------------------------------------------------------
// ScriptedBackend
// ---------------------------------------------------------------------

#[test]
fn scripted_backend_returns_in_order_then_exhausts_to_bad_request() {
    let resp1 = sample_response("first");
    let resp2 = sample_response("second");
    let backend = ScriptedBackend::new(vec![
        ScriptedTurn::Respond(resp1.clone()),
        ScriptedTurn::Respond(resp2.clone()),
    ]);

    let out1 = block_on(backend.generate(sample_request())).unwrap();
    assert_eq!(out1.content, resp1.content);

    let out2 = block_on(backend.generate(sample_request())).unwrap();
    assert_eq!(out2.content, resp2.content);

    let err = block_on(backend.generate(sample_request())).unwrap_err();
    match err {
        BackendError::BadRequest { detail } => {
            assert!(detail.contains("exhausted"), "detail was: {detail}");
        }
        other => panic!("expected BadRequest on exhaustion, got {other:?}"),
    }

    assert_eq!(backend.calls().len(), 3);
}

#[test]
fn scripted_backend_stream_decomposes_into_text_deltas_then_one_done() {
    let response = GenerateResponse {
        content: vec![
            ContentBlock::Text {
                text: "hello ".into(),
            },
            ContentBlock::Text {
                text: "world".into(),
            },
        ],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage::default(),
    };

    // One backend for `generate`, an identically-scripted one for `stream`,
    // so both draw from the same script content without racing each other
    // for the single queued turn.
    let generate_backend = ScriptedBackend::new(vec![ScriptedTurn::Respond(response.clone())]);
    let stream_backend = ScriptedBackend::new(vec![ScriptedTurn::Respond(response.clone())]);

    let generated = block_on(generate_backend.generate(sample_request())).unwrap();
    let generated_text: String = generated
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();

    let stream = block_on(stream_backend.stream(sample_request())).unwrap();
    let chunks = drain_box_stream(stream);

    assert!(!chunks.is_empty());
    let mut deltas = String::new();
    let mut done_seen = false;
    for (idx, chunk) in chunks.iter().enumerate() {
        let chunk = chunk.as_ref().expect("no BackendError expected");
        match chunk {
            StreamChunk::TextDelta(text) => {
                assert!(!done_seen, "TextDelta after Done");
                deltas.push_str(text);
            }
            StreamChunk::Done(final_response) => {
                assert_eq!(idx, chunks.len() - 1, "Done must be the last chunk");
                assert!(!done_seen, "more than one Done chunk");
                done_seen = true;
                assert_eq!(final_response.content, response.content);
            }
            other => panic!("unexpected chunk: {other:?}"),
        }
    }
    assert!(done_seen, "stream must end with exactly one Done");
    assert_eq!(
        deltas, generated_text,
        "stream deltas must equal generate() text"
    );
}

// ---------------------------------------------------------------------
// FakeStore
// ---------------------------------------------------------------------

#[test]
fn fake_store_fork_copies_zero_records_and_sets_origin() {
    let store = FakeStore::new();

    let parent_meta = sample_session_meta(SessionId::new(), None);
    let parent_id = block_on(store.create(parent_meta)).unwrap();
    for i in 0..100 {
        block_on(store.append(&parent_id, sample_user_turn(i))).unwrap();
    }
    assert_eq!(store.total_record_count(), 100);

    let child_meta = sample_session_meta(SessionId::new(), None);
    let at = LogSeq(50);
    let child_id = block_on(store.fork(&parent_id, at, child_meta)).unwrap();

    // Zero-copy: forking a 100-record parent does not increase the total
    // record count at all.
    assert_eq!(
        store.total_record_count(),
        100,
        "fork must copy zero records"
    );

    let child_meta_after = block_on(store.meta(&child_id)).unwrap();
    assert_eq!(
        child_meta_after.origin,
        Some(ForkOrigin {
            parent: parent_id,
            at_seq: at,
            mode: SubagentMode::Fork,
        })
    );

    let children = block_on(store.children(&parent_id)).unwrap();
    assert_eq!(children, vec![child_id]);
}

#[test]
fn fake_store_read_out_of_range_and_head() {
    let store = FakeStore::new();
    let meta = sample_session_meta(SessionId::new(), None);
    let id = block_on(store.create(meta)).unwrap();
    block_on(store.append(&id, sample_user_turn(0))).unwrap();
    block_on(store.append(&id, sample_user_turn(1))).unwrap();

    assert_eq!(block_on(store.head(&id)).unwrap(), LogSeq(2));

    let all = block_on(store.read(&id, SeqRange::full())).unwrap();
    assert_eq!(all.len(), 2);

    let err = block_on(store.read(&id, SeqRange::new(LogSeq(5), None))).unwrap_err();
    assert!(matches!(err, StoreError::SeqOutOfRange { .. }));
}

#[test]
fn fake_store_remove_enforces_the_ephemeral_and_children_guards() {
    let store = FakeStore::new();

    // Non-ephemeral: refused.
    let persistent = block_on(store.create(sample_session_meta(SessionId::new(), None))).unwrap();
    let err = block_on(store.remove(&persistent)).unwrap_err();
    assert!(
        matches!(err, StoreError::NotRemovable { .. }),
        "non-ephemeral removal must be refused, got: {err:?}"
    );

    // Ephemeral with an EPHEMERAL child: still refused (the
    // include_ephemeral trap — children() hides ephemeral children, so
    // only the guard's unfiltered child scan can see this one).
    let mut parent_meta = sample_session_meta(SessionId::new(), None);
    parent_meta.ephemeral = true;
    let parent = block_on(store.create(parent_meta)).unwrap();
    let mut child_meta = sample_session_meta(
        SessionId::new(),
        Some(ForkOrigin {
            parent,
            at_seq: LogSeq(0),
            mode: SubagentMode::Fork,
        }),
    );
    child_meta.ephemeral = true;
    let child = block_on(store.create(child_meta)).unwrap();
    assert!(
        block_on(store.children(&parent)).unwrap().is_empty(),
        "children() must hide the ephemeral child (this is why the guard cannot use it)"
    );
    let err = block_on(store.remove(&parent)).unwrap_err();
    assert!(
        matches!(err, StoreError::NotRemovable { .. }),
        "an ephemeral child must block removal, got: {err:?}"
    );

    // Remove the child, then the parent: both succeed, and both are gone.
    block_on(store.remove(&child)).unwrap();
    block_on(store.remove(&parent)).unwrap();
    let err = block_on(store.meta(&parent)).unwrap_err();
    assert!(matches!(err, StoreError::NotFound { .. }));

    // Missing session: NotFound.
    let err = block_on(store.remove(&SessionId::new())).unwrap_err();
    assert!(matches!(err, StoreError::NotFound { .. }));
}

#[test]
fn fake_store_set_ephemeral_enforces_the_one_way_promote_guard() {
    let store = FakeStore::new();

    // Demotion (false -> true request) is refused first, even for a
    // session that does not exist.
    let err = block_on(store.set_ephemeral(&SessionId::new(), true)).unwrap_err();
    assert!(
        matches!(err, StoreError::NotPromotable { .. }),
        "demotion must be refused, got: {err:?}"
    );

    // An ephemeral session flips, and the flip is visible through both
    // meta() and the default (exclude-ephemeral) listing.
    let mut meta = sample_session_meta(SessionId::new(), None);
    meta.ephemeral = true;
    let sid = block_on(store.create(meta)).unwrap();
    assert!(
        block_on(store.list(SessionFilter::default())).unwrap().is_empty(),
        "precondition: the ephemeral session is catalog-hidden"
    );
    block_on(store.set_ephemeral(&sid, false)).unwrap();
    assert!(
        !block_on(store.meta(&sid)).unwrap().ephemeral,
        "meta must show the flipped flag"
    );
    assert_eq!(
        block_on(store.list(SessionFilter::default())).unwrap().len(),
        1,
        "the promoted session must now appear in the default listing"
    );

    // A second flip (now a non-ephemeral no-op) is refused, and a false
    // request on a missing session is NotFound.
    let err = block_on(store.set_ephemeral(&sid, false)).unwrap_err();
    assert!(
        matches!(err, StoreError::NotPromotable { .. }),
        "a double promote must be refused, got: {err:?}"
    );
    let err = block_on(store.set_ephemeral(&SessionId::new(), false)).unwrap_err();
    assert!(matches!(err, StoreError::NotFound { .. }));
}

// ---------------------------------------------------------------------
// FakeGate
// ---------------------------------------------------------------------

#[test]
fn fake_gate_fixed_returns_configured_decision() {
    let gate = FakeGate::new(PermissionDecision::Deny {
        reason: "no".into(),
    });
    let req = sample_permission_request(vec![AgentId::new()]);
    let decision = block_on(gate.check(req));
    assert_eq!(
        decision,
        PermissionDecision::Deny {
            reason: "no".into()
        }
    );
}

#[test]
fn fake_gate_recording_preserves_agent_path() {
    let gate = FakeGate::recording();
    let path = vec![AgentId::new(), AgentId::new(), AgentId::new()];
    let req = sample_permission_request(path.clone());

    let decision = block_on(gate.check(req));
    assert_eq!(decision, PermissionDecision::AllowOnce);

    let recorded = gate.requests();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].agent_path, path);
}

// ---------------------------------------------------------------------
// CollectingEventSink
// ---------------------------------------------------------------------

#[test]
fn collecting_event_sink_preserves_emission_order() {
    let sink = CollectingEventSink::new();
    sink.emit(Event::TurnStarted { turn: 1 });
    sink.emit(Event::TurnStarted { turn: 2 });
    sink.emit(Event::TurnStarted { turn: 3 });

    assert_eq!(
        sink.events(),
        vec![
            Event::TurnStarted { turn: 1 },
            Event::TurnStarted { turn: 2 },
            Event::TurnStarted { turn: 3 },
        ]
    );

    sink.clear();
    assert!(sink.events().is_empty());
}

// ---------------------------------------------------------------------
// Cache-hint invariant (GP-06)
// ---------------------------------------------------------------------

#[test]
fn strip_cache_hints_leaves_content_and_provenance_byte_identical() {
    let hint = CacheHint {
        breakpoint: true,
        ttl: CacheTtl::FiveMinutes,
        prefix_key: PrefixKey::from_blake3(blake3::hash(b"conway-fakes")),
    };
    let mut segments = vec![
        PromptSegment::new(
            Role::System,
            vec![ContentBlock::Text { text: "sys".into() }],
            Provenance::AgentDef { name: "r".into() },
        )
        .with_cache_hint(hint.clone()),
        PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text { text: "hi".into() }],
            Provenance::UserPrompt,
        ),
    ];

    let before: Vec<(String, String)> = segments
        .iter()
        .map(|s| {
            (
                serde_json::to_string(&s.content).unwrap(),
                serde_json::to_string(&s.provenance).unwrap(),
            )
        })
        .collect();

    strip_cache_hints(&mut segments);

    assert!(segments.iter().all(|s| s.cache_hint.is_none()));

    let after: Vec<(String, String)> = segments
        .iter()
        .map(|s| {
            (
                serde_json::to_string(&s.content).unwrap(),
                serde_json::to_string(&s.provenance).unwrap(),
            )
        })
        .collect();

    assert_eq!(
        before, after,
        "stripping cache hints must not change content or provenance bytes"
    );
}

// ---------------------------------------------------------------------
// Headroom gate (WI-008 amendment)
// ---------------------------------------------------------------------

#[test]
fn fake_router_context_too_large_exercises_headroom_gate() {
    let role = RoleAlias::new("planner");
    let model = ModelRef {
        backend: BackendId::new("anthropic"),
        model: ModelId::new("claude-sonnet-4-6"),
    };
    let router = FakeRouter::context_too_large(role.clone(), model.clone(), 30_000, 8_192, 32_768);

    let req = RouteRequest {
        role,
        pin: None,
        required: RequiredCaps::default(),
        est_tokens: 30_000,
        agent_id: AgentId::new(),
    };

    let err = router.resolve(&req).unwrap_err();
    match &err {
        RoutingError::ContextTooLarge {
            shortfall_tokens, ..
        } => {
            assert_eq!(*shortfall_tokens, 5_424);
        }
        other => panic!("expected ContextTooLarge, got {other:?}"),
    }
    assert!(
        err.to_string().contains("reserved output"),
        "Display was: {err}"
    );
}

#[test]
fn fake_backend_with_capabilities_default_headroom_is_enforced() {
    let caps = Capabilities {
        tool_calling: ToolCallSupport::None,
        cache: CacheMode::None,
        parallel_tool_calls: false,
        structured_output: StructuredOutput::None,
        max_context_tokens: 32_768,
        reasoning: false,
        reliability_tier: ReliabilityTier::Community,
    };
    let backend = FakeBackend::with_capabilities(caps);
    let effective_caps = backend.capabilities(&ModelId::new("whatever"));

    // 30_000 est + the DEFAULT_HEADROOM_TOKENS (8_192) > 32_768: the default
    // headroom must actually be enforced by `satisfied_by`, not merely
    // stored on `RequiredCaps`.
    let result = RequiredCaps::default().satisfied_by(&effective_caps, 30_000);
    assert!(result.is_err(), "expected headroom-aware rejection, got Ok");
}

// ---------------------------------------------------------------------
// Object-safety / trait-object usability compile check
// ---------------------------------------------------------------------

#[allow(dead_code)]
fn _ports_are_usable_as_trait_objects() {
    let _backend: Arc<dyn Backend> = Arc::new(FakeBackend::echo(BackendId::new("fake")));
    let _store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let _gate: Arc<dyn PermissionGate> = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let _router: Arc<dyn Router> = Arc::new(FakeRouter::new(vec![]));
    let _health: Arc<dyn HealthRegistry> = Arc::new(FakeHealth::new());
    let _subagents: Arc<dyn SubagentHost> = Arc::new(FakeSubagentHost::new(AgentId::new()));
    let _events: Arc<dyn EventSink> = Arc::new(CollectingEventSink::new());
}
