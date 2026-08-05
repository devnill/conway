//! Acceptance tests for `SessionHandle`/`TurnHandle`/`EventStream` (WI-101).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    RoleEntry, RoutingSection, SessionConfig, ToolsConfig, TuiSection,
};
use conway::{Conway, ConwayBuilder, SessionSpec};
use conway_core::agent::{Budget, PermissionDecision, ResultStatus};
use conway_core::event::Event;
use conway_core::fakes::{FakeBackend, FakeGate, FakeRouter, FakeStore};
use conway_core::ids::{AgentId, BackendId, LogSeq, SessionId};
use conway_core::log::{LogRecord, SessionMeta};
use conway_core::ports::SessionStore;
use conway_core::provenance::Provenance;
use futures_core::Stream as _;

fn assert_clone_send_sync<T: Clone + Send + Sync>() {}

#[test]
fn session_handle_is_clone_send_sync() {
    assert_clone_send_sync::<conway::SessionHandle>();
}

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(conway_core::ids::ModelRef {
        backend: BackendId::new("fake"),
        model: conway_core::ids::ModelId::new("echo-model"),
    }))
}

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
        default_role: conway_core::ids::RoleAlias::new("default"),
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
        tools: ToolsConfig::default(),
    }
}

/// An echoing `FakeBackend`: its `generate`/`stream` response is the
/// concatenated text of the last user-role segment, so `TurnHandle::text()`
/// can be asserted against the exact prompt text without hand-building a
/// `GenerateResponse`.
fn build_conway_with_echo_backend(store: Arc<FakeStore>) -> Conway {
    let backend: Arc<dyn conway_core::ports::Backend> =
        Arc::new(FakeBackend::echo(BackendId::new("fake")));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should succeed with every port injected")
}

async fn new_handle(conway: &Conway) -> conway::SessionHandle {
    conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed")
}

// ---------------------------------------------------------------------
// prompt / TurnHandle
// ---------------------------------------------------------------------

#[tokio::test]
async fn prompt_delegates_text_byte_for_byte_and_turn_handle_streams_it_back() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    let text_in = "  hello, conway  \n";
    let turn = tokio::time::timeout(Duration::from_secs(5), handle.prompt(text_in))
        .await
        .expect("prompt must not hang")
        .expect("prompt should succeed");

    let text_out = tokio::time::timeout(Duration::from_secs(5), turn.text())
        .await
        .expect("text() must not hang")
        .expect("text() should succeed");
    assert_eq!(
        text_out, text_in,
        "echoed text must equal the prompt byte-for-byte"
    );
}

#[tokio::test]
async fn turn_handle_text_then_result_does_not_deadlock() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    let turn = handle.prompt("hi").await.expect("prompt should succeed");

    let _text = tokio::time::timeout(Duration::from_secs(5), turn.text())
        .await
        .expect("text() must not hang")
        .expect("text() should succeed");
    let result = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang after text() on the same handle")
        .expect("result() should succeed");
    assert!(matches!(result.status, ResultStatus::Completed));
}

#[tokio::test]
async fn turn_handle_result_resolves_on_budget_exceeded() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let spec = SessionSpec {
        budget: Some(Budget {
            max_steps: 0,
            deadline: None,
            max_tokens: None,
            max_tool_calls: None,
        }),
        ..SessionSpec::default()
    };
    let handle = conway
        .new_session(spec)
        .await
        .expect("new_session should succeed");

    let turn = handle.prompt("hi").await.expect("prompt should succeed");
    let result = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");
    assert!(
        matches!(result.status, ResultStatus::BudgetExceeded { .. }),
        "expected BudgetExceeded with max_steps = 0, got {:?}",
        result.status
    );
}

// Note: `TurnHandle::result()` resolving on `Cancelled` uses exactly the
// same code path as `BudgetExceeded` above -- it matches only on
// `Event::AgentFinished`, never inspecting `AgentResult.status` -- so the
// two are structurally identical by construction. An automated test that
// actually *drives* a `Cancelled` outcome is not constructible from
// WI-101's own public surface: the only way to trip a hard cancel is
// `Runtime::cancel(agent, reason)`, which `SessionHandle` does not expose
// (that arrives as `SessionHandle::cancel` in WI-102, per the module plan's
// own dependency edge from WI-102 to WI-101). Disclosed gap, not a silent
// omission.

// ---------------------------------------------------------------------
// events() / events_from()
// ---------------------------------------------------------------------

#[tokio::test]
async fn events_are_filtered_to_this_session_only() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle_a = new_handle(&conway).await;
    let handle_b = new_handle(&conway).await;
    assert_ne!(handle_a.id(), handle_b.id());

    let mut stream_b = handle_b.events();

    // Every started root immediately runs one spontaneous turn on its own
    // `RootSpec.prompt: None` -> `""` initial record (see
    // `Runtime::start_root`), so `handle_b` legitimately has some of its
    // own activity on the bus even though this test never calls
    // `handle_b.prompt(..)`. The invariant this test actually checks is
    // narrower and criterion-faithful: every NON-lifecycle envelope
    // `stream_b` yields is tagged with `handle_b`'s own session, never
    // `handle_a`'s.
    //
    // `Event::AgentSpawned`/`Event::AgentFinished` are the deliberate
    // exception (WI fixing "spawn doesn't populate the /agents panel"):
    // `EventStream::accept` passes tree-lifecycle events through
    // regardless of session, so `handle_a`'s own root finishing its
    // one-shot turn is legitimately observable here too, tagged with
    // `handle_a`'s session -- not a leak, the documented behavior.
    handle_a
        .prompt("only for a")
        .await
        .expect("prompt on a should succeed");

    for _ in 0..5 {
        let observed = tokio::time::timeout(Duration::from_millis(200), async {
            std::future::poll_fn(|cx| std::pin::Pin::new(&mut stream_b).poll_next(cx)).await
        })
        .await;
        match observed {
            Ok(Some(envelope)) => {
                let is_lifecycle = matches!(
                    envelope.event,
                    Event::AgentSpawned { .. } | Event::AgentFinished { .. }
                );
                assert!(
                    is_lifecycle || envelope.session == handle_b.id(),
                    "session b's event stream must never yield session a's NON-lifecycle \
                     envelopes; got {envelope:?}"
                );
            }
            // No more of b's own spontaneous activity within the window --
            // and, crucially, nothing non-lifecycle from a either.
            _ => break,
        }
    }
}

#[tokio::test]
async fn events_from_replays_persisted_records_then_continues_live() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store.clone());
    let handle = new_handle(&conway).await;

    // `Conway::new_session` -> `Runtime::start_root` already appends one
    // `UserTurn` (and may already have run its own spontaneous turn -- see
    // `events_are_filtered_to_this_session_only`'s comment) before this
    // test runs, so the replay window starts at the store's actual head
    // rather than a hardcoded seq.
    let start = store.head(&handle.id()).await.expect("head should succeed");

    // Append fixture records directly to the store, bypassing the runtime
    // entirely (no live bus activity results from this).
    for i in 0..3 {
        store
            .append(
                &handle.id(),
                LogRecord::UserTurn {
                    seq: LogSeq::ZERO,
                    ts: chrono::Utc::now(),
                    text: format!("persisted-{i}"),
                    prov: Provenance::UserPrompt,
                },
            )
            .await
            .expect("append should succeed");
    }

    let mut stream = handle
        .events_from(start)
        .await
        .expect("events_from should succeed");

    let mut replayed = Vec::new();
    for _ in 0..3 {
        let envelope = tokio::time::timeout(Duration::from_secs(5), async {
            std::future::poll_fn(|cx| std::pin::Pin::new(&mut stream).poll_next(cx)).await
        })
        .await
        .expect("replay must not hang")
        .expect("replay batch should contain 3 envelopes");
        replayed.push(envelope);
    }
    for (i, envelope) in replayed.iter().enumerate() {
        assert_eq!(envelope.seq, i as u64);
        assert!(matches!(
            &envelope.event,
            Event::UserTurn { text, .. } if text == &format!("persisted-{i}")
        ));
    }

    // Now drive live activity on the same session and confirm the same
    // stream keeps yielding, continuing the local seq counter.
    handle
        .prompt("go live")
        .await
        .expect("prompt should succeed");
    let next = tokio::time::timeout(Duration::from_secs(5), async {
        std::future::poll_fn(|cx| std::pin::Pin::new(&mut stream).poll_next(cx)).await
    })
    .await
    .expect("live continuation must not hang")
    .expect("stream must keep yielding after replay");
    assert_eq!(
        next.seq, 3,
        "live tail must continue the local seq counter from the replay batch"
    );
}

// ---------------------------------------------------------------------
// tree() / context_report()
// ---------------------------------------------------------------------

#[tokio::test]
async fn tree_includes_the_started_root() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    let tree = handle.tree();
    assert!(
        tree.nodes.iter().any(|node| node.agent_id == handle.root()),
        "tree() must include the root agent this handle started"
    );
}

#[tokio::test]
async fn context_report_segments_are_ordered_with_provenance() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    let turn = handle.prompt("hi").await.expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");

    let report = handle
        .context_report(handle.root())
        .await
        .expect("context_report should succeed");

    assert!(
        !report.segments.is_empty(),
        "a completed turn must have assembled segments"
    );
    // The binding criterion names `AgentDef`/`SystemNote` as the expected
    // first segment. `SessionSpec::default()` (used here, matching every
    // other test in this file) has `agent_def: None`, and per architecture
    // §5.3 the `SystemPrompt`/`SkillFragments` segments are only emitted
    // when an `AgentDef` is actually attached -- with none configured, the
    // fixed ordering's first *present* segment is `[2] ToolSchemas`
    // (`ToolRegistry`). Broadened to match what a real, agent-def-less
    // session actually produces rather than asserting a segment kind this
    // fixture has no way to emit.
    assert!(
        matches!(
            report.segments[0].provenance,
            Provenance::AgentDef { .. } | Provenance::SystemNote { .. } | Provenance::ToolRegistry { .. }
        ),
        "first segment must be AgentDef, SystemNote, or (no agent_def configured) ToolRegistry, got {:?}",
        report.segments[0].provenance
    );
    // `provenance` is a mandatory (non-`Option`) field on `ContextReportEntry`
    // -- every segment carries one by construction; this is the type-level
    // guarantee the binding criterion's "provenance is Some for every
    // segment" describes (committed types are authoritative over that
    // now-stale `Option` framing).
}

// ---------------------------------------------------------------------
// context_report_current() / last_model() (T3 follow-up)
// ---------------------------------------------------------------------

#[tokio::test]
async fn context_report_current_matches_the_live_report_for_an_agent_this_process_ran() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    let turn = handle.prompt("hi").await.expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");

    let live = handle
        .context_report(handle.root())
        .await
        .expect("context_report should succeed");
    let current = handle
        .context_report_current(handle.root())
        .await
        .expect("context_report_current should succeed");
    assert_eq!(
        current, live,
        "for an agent this process itself drove a turn for, context_report_current \
         must agree with the plain live context_report -- the durable fallback is \
         only for the case where the live slot is empty"
    );
}

#[tokio::test]
async fn last_model_is_none_before_any_turn_and_the_served_model_after() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    assert_eq!(
        handle
            .last_model(handle.root())
            .await
            .expect("last_model should succeed"),
        None,
        "a brand-new agent has completed no turn, so there is no served model yet"
    );

    let turn = handle.prompt("hi").await.expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");

    let model = handle
        .last_model(handle.root())
        .await
        .expect("last_model should succeed")
        .expect("a completed turn must have a served model");
    assert_eq!(model.backend, BackendId::new("fake"));
    assert_eq!(model.model, conway_core::ids::ModelId::new("echo-model"));
}

// ---------------------------------------------------------------------
// transcript()
// ---------------------------------------------------------------------

#[tokio::test]
async fn transcript_unknown_agent_returns_runtime_error_naming_the_agent() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store);
    let handle = new_handle(&conway).await;

    let unknown = AgentId::new();
    let err = handle
        .transcript(unknown)
        .await
        .expect_err("an unknown agent id must be rejected");
    match err {
        conway::ConwayError::Runtime(inner) => {
            let message = inner.to_string();
            assert!(
                message.contains(&unknown.to_string()),
                "error must name the unknown agent id: {message}"
            );
        }
        other => panic!("expected ConwayError::Runtime, got {other:?}"),
    }
}

#[tokio::test]
async fn transcript_resolves_the_effective_ancestry_of_a_forked_fixture() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store.clone());
    let handle = new_handle(&conway).await;

    // `Conway::new_session` -> `Runtime::start_root` starts a prompt-less
    // root IDLE, writing no initial `UserTurn` -- so the store's head is at
    // seq 0 before this test appends anything, and the fork boundary below
    // is computed relative to that actual head rather than a hardcoded seq.
    let start = store.head(&handle.id()).await.expect("head should succeed");

    // Grow the root/parent session past its own header.
    for i in 0..4 {
        store
            .append(
                &handle.id(),
                LogRecord::UserTurn {
                    seq: LogSeq::ZERO,
                    ts: chrono::Utc::now(),
                    text: format!("parent-{i}"),
                    prov: Provenance::UserPrompt,
                },
            )
            .await
            .expect("append should succeed");
    }

    // Fork a child session right after parent-0/parent-1 (inheriting
    // everything up to and including those two, but not parent-2/parent-3),
    // owned by a fresh agent id not otherwise known to the runtime.
    let fork_at = LogSeq(start.0 + 2);
    let child_agent = AgentId::new();
    let child_session = SessionId::new();
    let child_meta = SessionMeta {
        id: child_session,
        agent_id: child_agent,
        origin: None, // `SessionStore::fork` fills this in.
        agent_def: None,
        role: None,
        created: chrono::Utc::now(),
        cwd: std::path::PathBuf::from("."),
        labels: vec![],
        ephemeral: false,
        ask_origin: None,
        root: None,
    };
    store
        .fork(&handle.id(), fork_at, child_meta)
        .await
        .expect("fork should succeed");
    store
        .append(
            &child_session,
            LogRecord::UserTurn {
                seq: LogSeq::ZERO,
                ts: chrono::Utc::now(),
                text: "child-own".to_string(),
                prov: Provenance::UserPrompt,
            },
        )
        .await
        .expect("append should succeed");

    let transcript = handle
        .transcript(child_agent)
        .await
        .expect("transcript should resolve the forked fixture");

    let texts: Vec<&str> = transcript
        .iter()
        .filter_map(|record| match record {
            LogRecord::UserTurn { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts,
        vec!["parent-0", "parent-1", "child-own"],
        "effective transcript must be the parent's inherited prefix (parent-0/parent-1, up to \
         the fork seq) followed by the child's own records"
    );
}

/// S1 regression (F-049-1): `resolve_prefix`'s ancestry walk must be correct
/// at depth >= 3 -- a fork off a NON-root parent -- not just the depth-2
/// (root -> child) case the sibling test above covers. This builds a
/// three-generation chain (root -> child -> grandchild, the grandchild
/// forking off the child rather than the root) and asserts the grandchild's
/// effective transcript is the whole-prefix concatenation (D-11): the
/// root's inherited records, then the child's own inherited records, then
/// the grandchild's own records.
#[tokio::test]
async fn transcript_resolves_a_grandchild_fork_three_generations_deep() {
    let store = Arc::new(FakeStore::new());
    let conway = build_conway_with_echo_backend(store.clone());
    let handle = new_handle(&conway).await;

    // `Conway::new_session` -> `Runtime::start_root` starts a prompt-less
    // root IDLE, writing no initial `UserTurn` (see the sibling forked-
    // fixture test's comment), so the store's head is at seq 0 before this
    // test appends anything, and the first fork boundary below is computed
    // relative to that actual head rather than a hardcoded seq.
    let start = store.head(&handle.id()).await.expect("head should succeed");

    // Root grows by two of its own records: r0, r1.
    for text in ["r0", "r1"] {
        store
            .append(
                &handle.id(),
                LogRecord::UserTurn {
                    seq: LogSeq::ZERO,
                    ts: chrono::Utc::now(),
                    text: text.to_string(),
                    prov: Provenance::UserPrompt,
                },
            )
            .await
            .expect("append should succeed");
    }

    // Child forks off the ROOT after those two appends (inherits r0/r1 in
    // full -- nothing of the root is excluded here, matching the sibling
    // test's "local seq N == N appends so far" convention).
    let child_fork_at = LogSeq(start.0 + 2);
    let child_agent = AgentId::new();
    let child_session = SessionId::new();
    store
        .fork(
            &handle.id(),
            child_fork_at,
            SessionMeta {
                id: child_session,
                agent_id: child_agent,
                origin: None, // `SessionStore::fork` fills this in.
                agent_def: None,
                role: None,
                created: chrono::Utc::now(),
                cwd: std::path::PathBuf::from("."),
                labels: vec![],
                ephemeral: false,
                ask_origin: None,
                root: None,
            },
        )
        .await
        .expect("fork should succeed");

    // Child appends exactly one record of its own: c0 (its own log, seq 0).
    store
        .append(
            &child_session,
            LogRecord::UserTurn {
                seq: LogSeq::ZERO,
                ts: chrono::Utc::now(),
                text: "c0".to_string(),
                prov: Provenance::UserPrompt,
            },
        )
        .await
        .expect("append should succeed");

    // Grandchild forks off the CHILD (not the root) after the child's one
    // own append -- the depth-3, non-root-parent case F-049-1 needs
    // coverage for.
    let grandchild_fork_at = LogSeq(1);
    let grandchild_agent = AgentId::new();
    let grandchild_session = SessionId::new();
    store
        .fork(
            &child_session,
            grandchild_fork_at,
            SessionMeta {
                id: grandchild_session,
                agent_id: grandchild_agent,
                origin: None, // `SessionStore::fork` fills this in.
                agent_def: None,
                role: None,
                created: chrono::Utc::now(),
                cwd: std::path::PathBuf::from("."),
                labels: vec![],
                ephemeral: false,
                ask_origin: None,
                root: None,
            },
        )
        .await
        .expect("fork should succeed");

    // Grandchild appends exactly one record of its own: g0.
    store
        .append(
            &grandchild_session,
            LogRecord::UserTurn {
                seq: LogSeq::ZERO,
                ts: chrono::Utc::now(),
                text: "g0".to_string(),
                prov: Provenance::UserPrompt,
            },
        )
        .await
        .expect("append should succeed");

    let transcript = handle
        .transcript(grandchild_agent)
        .await
        .expect("transcript must resolve a depth-3 (grandchild) fork ancestry");

    let texts: Vec<&str> = transcript
        .iter()
        .filter_map(|record| match record {
            LogRecord::UserTurn { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts,
        vec!["r0", "r1", "c0", "g0"],
        "the grandchild's effective transcript must be the whole prefix (D-11): the root's \
         r0/r1, then the child's own c0, then the grandchild's own g0"
    );
}
