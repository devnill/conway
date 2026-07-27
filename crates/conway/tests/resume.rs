//! Acceptance tests for `Conway::resume`/`::sessions`/`::fork_from` (WI-103,
//! then WI-119).
//!
//! **WI-119 closes the WI-103 gap this file's doc used to disclose here:**
//! `Runtime::resume_root` (WI-118) now exists, and `Conway::resume`/
//! `::fork_from` both call it (see `crates/conway/src/conway.rs`'s doc for
//! the full mechanism). `resumed_handle_prompt_succeeds_and_continues_the_
//! transcript` below exercises the resumed-root criterion in full, and
//! `fork_from_returns_a_drivable_child_whose_prompt_succeeds` now exercises
//! the fork-child criterion in full too: `prompt` on a fork child succeeds
//! AND the child's context contains the inherited parent prefix
//! (`resume_root`, `conway-runtime`, now resolves it via
//! `conway_session::TranscriptResolver::resolve_prefix` -- see that
//! method's doc for the mechanism this file's own previous doc here
//! disclosed as missing).

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    RoleEntry, RoutingSection, SessionConfig,
};
use conway::{Conway, ConwayBuilder, ConwayError, ForkSpec, SessionSpec};
use conway_core::agent::{PermissionDecision, ResultStatus};
use conway_core::content::ContentBlock;
use conway_core::error::{RuntimeError, StoreError};
use conway_core::fakes::{
    FakeBackend, FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn,
};
use conway_core::ids::{AgentId, BackendId, LogSeq, ModelId, ModelRef, RoleAlias, SessionId};
use conway_core::log::{ForkOrigin, SessionFilter, SubagentMode};
use conway_core::ports::{Backend, GenerateResponse, SessionStore};

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }))
}

/// A fixed-text, zero-usage `GenerateResponse` -- for `ScriptedBackend`
/// scripts driving the drivability tests below, mirroring
/// `conway-runtime/tests/resume_root.rs`'s own `text_response` helper (this
/// crate has no equivalent already, and that one is private to its own
/// integration test binary).
fn text_response(text: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        tool_calls: vec![],
        stop: conway_core::content::StopReason::EndTurn,
        usage: conway_core::content::Usage::default(),
    }
}

/// The concatenated text of every `ContentBlock::Text` in `req`'s segments
/// -- for asserting what a `ScriptedBackend` call's assembled context
/// actually contained.
fn request_text(req: &conway_core::ports::GenerateRequest) -> String {
    req.segments
        .iter()
        .flat_map(|seg| seg.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn base_config() -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "default".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
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
    }
}

/// `SessionHandle` deliberately does not derive `Debug` (it wraps
/// `Arc<Runtime>`, which does not either -- same reasoning as `Conway`'s own
/// non-`Debug`, see `tests/builder.rs`'s `expect_build_err`), so
/// `Result::expect_err`/`unwrap_err` (which both require `T: Debug`) cannot
/// be used directly on a `Result<SessionHandle, _>` here.
fn expect_session_err(
    result: Result<conway::SessionHandle, ConwayError>,
    msg: &str,
) -> ConwayError {
    match result {
        Err(err) => err,
        Ok(_) => panic!("{msg}"),
    }
}

fn build_conway(store: Arc<dyn SessionStore>) -> Conway {
    let backend: Arc<dyn conway_core::ports::Backend> =
        Arc::new(FakeBackend::echo(BackendId::new("fake")));
    build_conway_with_backend(store, backend)
}

/// Like [`build_conway`], but with an injected backend -- for the
/// `ScriptedBackend`-driven drivability tests below, which need to script
/// and inspect the exact requests a resumed/forked agent sends. `fake_router`
/// pins every role to `BackendId::new("fake")`, so `backend.id()` must be
/// that same id (`ScriptedBackend::with_id(BackendId::new("fake"))`).
fn build_conway_with_backend(store: Arc<dyn SessionStore>, backend: Arc<dyn Backend>) -> Conway {
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should succeed with every port injected")
}

// ---------------------------------------------------------------------
// resume()
// ---------------------------------------------------------------------

#[tokio::test]
async fn resume_returns_handle_whose_id_and_root_match_the_session_header() {
    // Resuming happens over a SECOND `Conway`/`Runtime`, not the one that
    // created the session (WI-119): `Runtime::resume_root` re-attaches
    // `meta.agent_id` into `AgentTree`, and attaching an id already live in
    // the SAME tree errors (`"agent ... is already attached to the tree"`,
    // `conway-runtime`'s own `tree::already_attached` -- correct, since the
    // original root task is still running there). This mirrors every other
    // resume test in this file, all of which resume across a simulated
    // restart against the same persisted store.
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let (sid, root) = {
        let conway = build_conway(store.clone());
        let handle = conway
            .new_session(SessionSpec::default())
            .await
            .expect("new_session should succeed");
        (handle.id(), handle.root())
    };

    let conway2 = build_conway(store);
    let resumed = conway2
        .resume(sid)
        .await
        .expect("resume should succeed for an existing session");

    assert_eq!(resumed.id(), sid);
    assert_eq!(
        resumed.root(),
        root,
        "resumed root must equal the agent id recorded in the session header"
    );
}

#[tokio::test]
async fn resume_on_nonexistent_session_returns_store_error_naming_the_id() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = build_conway(store);
    let missing = SessionId::new();

    let err = expect_session_err(
        conway.resume(missing).await,
        "resuming an unknown session id must fail",
    );
    match err {
        ConwayError::Store(inner) => {
            let message = inner.to_string();
            assert!(
                message.contains(&missing.to_string()),
                "error must name the missing session id: {message}"
            );
        }
        other => panic!("expected ConwayError::Store, got {other:?}"),
    }
}

#[tokio::test]
async fn resumed_handle_transcript_matches_records_from_before_a_simulated_restart() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());

    let sid;
    let root;
    let original_transcript;
    {
        // Everything in this block stands in for "the process that created
        // the session" -- only `store` (not this `Conway`/its `Runtime`)
        // survives past the block, simulating a restart against the same
        // persisted store.
        let conway = build_conway(store.clone());
        let handle = conway
            .new_session(SessionSpec::default())
            .await
            .expect("new_session should succeed");

        let turn = handle
            .prompt("hello before restart")
            .await
            .expect("prompt should succeed");
        let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
            .await
            .expect("result() must not hang")
            .expect("result() should succeed");

        sid = handle.id();
        root = handle.root();
        original_transcript = handle
            .transcript(root)
            .await
            .expect("transcript should succeed before the simulated restart");
        assert!(
            !original_transcript.is_empty(),
            "the prompted turn must have left at least one record"
        );
    }

    let conway2 = build_conway(store);
    let resumed = conway2
        .resume(sid)
        .await
        .expect("resume should succeed after the simulated restart");
    assert_eq!(resumed.id(), sid);
    assert_eq!(resumed.root(), root);

    let resumed_transcript = resumed
        .transcript(root)
        .await
        .expect("transcript must still resolve purely through SessionStore after resume");
    assert_eq!(
        resumed_transcript, original_transcript,
        "resumed transcript must equal the pre-restart transcript record-for-record"
    );
}

/// The WI-119 headline criterion: a resumed handle is DRIVABLE. Verified
/// end-to-end over a real drop-and-rebuild of `Conway` against the same
/// store (mirroring `resumed_handle_transcript_matches_records_from_before_a_
/// simulated_restart` above), with a `ScriptedBackend` so the SECOND
/// `Runtime`'s single captured request can be inspected directly: it must
/// contain both the pre-restart turn's text and the new post-resume
/// prompt's text, proving the transcript was continued, not restarted from
/// scratch or dropped.
///
/// Runs on a real multi-thread runtime with an explicit delay between
/// `resume` and `prompt`, mirroring `conway-runtime/tests/resume_root.rs`'s
/// own `resume_root_makes_a_persisted_session_promptable` -- the same D-3
/// scheduling race that test's doc explains (a resumed agent's gated first
/// iteration must not run before the caller's own `prompt`) is exercised
/// here one layer up, through the facade.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_handle_prompt_succeeds_and_continues_the_transcript() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());

    let sid;
    {
        let backend1: Arc<dyn Backend> = Arc::new(
            ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("ack"))])
                .with_id(BackendId::new("fake")),
        );
        let conway = build_conway_with_backend(store.clone(), backend1);
        let handle = conway
            .new_session(SessionSpec::default())
            .await
            .expect("new_session should succeed");
        let turn = handle
            .prompt("first turn text")
            .await
            .expect("prompt should succeed");
        let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
            .await
            .expect("result() must not hang")
            .expect("result() should succeed");
        sid = handle.id();
        // `conway`/`handle` drop here -- only `store` survives, simulating a
        // process restart against the same persisted store.
    }

    let backend2 = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("continued"))])
            .with_id(BackendId::new("fake")),
    );
    let conway2 = build_conway_with_backend(store, backend2.clone());
    let resumed = conway2
        .resume(sid)
        .await
        .expect("resume should succeed after the simulated restart");

    // Give the resumed agent's spawned task every chance to run its gated
    // first iteration before `prompt` is ever called -- pre-WI-118/119, this
    // is exactly the window a would-be spurious turn would run in.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        backend2.calls().is_empty(),
        "resume must not run a turn before the caller's own prompt arrives, calls: {:?}",
        backend2.calls()
    );

    let turn = resumed
        .prompt("second turn text")
        .await
        .expect("prompt on a resumed handle must succeed, not AgentNotFound");
    let result = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");
    assert!(
        matches!(result.status, ResultStatus::Completed),
        "expected the resumed agent to complete after prompt, got: {:?}",
        result.status
    );
    assert_eq!(result.summary, "continued");

    let calls = backend2.calls();
    assert_eq!(
        calls.len(),
        1,
        "expected exactly one backend call once the resumed agent runs, calls: {calls:?}"
    );
    let text = request_text(&calls[0]);
    assert!(
        text.contains("first turn text"),
        "expected the resumed turn's context to contain the prior turn's text, got: {text}"
    );
    assert!(
        text.contains("second turn text"),
        "expected the resumed turn's context to contain the new prompt's text, got: {text}"
    );
}

/// A truncated trailing line (a crash mid-append) is repaired transparently
/// by `JsonlSessionStore` on first file access -- `resume` only calls
/// `SessionStore::meta`, which goes through that same repair path, so it
/// succeeds without any special-casing in `Conway::resume` itself. See that
/// method's doc for the disclosed gap in surfacing this as an
/// `Event::Error{fatal: false}` (the `SessionStore` port has no channel to
/// carry that signal back to the facade).
#[cfg(feature = "jsonl-store")]
#[tokio::test]
async fn resume_succeeds_on_a_session_with_a_truncated_trailing_line() {
    use conway_session::JsonlSessionStore;

    let root_dir = support::unique_temp_dir("resume-truncated");
    let sid = SessionId::new();

    {
        let store = JsonlSessionStore::open(root_dir.clone())
            .await
            .expect("open should succeed");
        store
            .create(conway_session::SessionMeta {
                id: sid,
                agent_id: AgentId::new(),
                origin: None,
                agent_def: None,
                role: None,
                created: chrono::Utc::now(),
                cwd: std::path::PathBuf::from("."),
                labels: vec![],
                status: conway_session::SessionStatus::Active,
                ephemeral: false,
                ask_origin: None,
            })
            .await
            .expect("create should succeed");
        for i in 0..3 {
            store
                .append(
                    &sid,
                    conway_core::log::LogRecord::UserTurn {
                        seq: LogSeq::ZERO,
                        ts: chrono::Utc::now(),
                        text: format!("ok-{i}"),
                        prov: conway_core::provenance::Provenance::UserPrompt,
                    },
                )
                .await
                .expect("append should succeed");
        }
        // `store` drops here, releasing its file handle before the raw
        // corruption write below.
    }

    let path = root_dir.join(format!("{sid}.jsonl"));
    {
        use std::io::Write;
        // A syntactically incomplete JSON object, no trailing `\n` --
        // exactly the "crash mid-write" shape `conway-session`'s own
        // recovery tests use.
        let torn = br#"{"kind":"user_turn","seq":3,"ts":"2026-07-20T00:00:00Z","text":"in"#;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open for raw append should succeed");
        f.write_all(torn).expect("raw write should succeed");
    }

    let store: Arc<dyn SessionStore> = Arc::new(
        JsonlSessionStore::open(root_dir)
            .await
            .expect("re-open should succeed"),
    );
    let conway = build_conway(store);

    let resumed = conway
        .resume(sid)
        .await
        .expect("resume must succeed (truncate-and-warn), not fail, on a damaged trailing line");
    assert_eq!(resumed.id(), sid);

    let transcript = resumed
        .transcript(resumed.root())
        .await
        .expect("transcript should resolve the 3 complete records");
    assert_eq!(
        transcript.len(),
        3,
        "the damaged 4th record must be dropped, the 3 complete ones kept"
    );
}

// ---------------------------------------------------------------------
// new_session(): SessionSpec::id (WI-119)
// ---------------------------------------------------------------------

#[tokio::test]
async fn new_session_with_a_fresh_caller_chosen_id_creates_exactly_that_id() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = build_conway(store.clone());
    let chosen = SessionId::new();

    let handle = conway
        .new_session(SessionSpec {
            id: Some(chosen),
            ..SessionSpec::default()
        })
        .await
        .expect("new_session with a fresh caller-chosen id should succeed");
    assert_eq!(handle.id(), chosen);

    let meta = store
        .meta(&chosen)
        .await
        .expect("the store must have a session under exactly the chosen id");
    assert_eq!(meta.id, chosen);
}

#[tokio::test]
async fn new_session_with_an_already_existing_id_returns_a_typed_error() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = build_conway(store);
    let existing = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");

    let err = expect_session_err(
        conway
            .new_session(SessionSpec {
                id: Some(existing.id()),
                ..SessionSpec::default()
            })
            .await,
        "new_session with an already-existing id must fail",
    );
    match err {
        ConwayError::Runtime(RuntimeError::Store(StoreError::AlreadyExists { session })) => {
            assert_eq!(
                session,
                existing.id(),
                "the typed error must name the colliding id"
            );
        }
        other => panic!(
            "expected ConwayError::Runtime(RuntimeError::Store(StoreError::AlreadyExists)), \
             got a distinct, non-generic failure: {other:?}"
        ),
    }
}

// ---------------------------------------------------------------------
// SessionHandle::context_report_at (carried from the capstone review)
// ---------------------------------------------------------------------

#[tokio::test]
async fn context_report_at_forwards_to_the_runtime_and_matches_the_live_report() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = build_conway(store);
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hello").await.expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");

    let live_report = handle
        .context_report(handle.root())
        .await
        .expect("context_report should resolve after a completed turn");

    let historical_report = handle
        .context_report_at(handle.root(), live_report.turn)
        .await
        .expect("context_report_at should resolve the same turn from durable storage");

    assert_eq!(
        historical_report, live_report,
        "context_report_at must return the same report context_report holds live"
    );
}

#[tokio::test]
async fn context_report_at_errors_typed_for_an_out_of_range_turn() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = build_conway(store);
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hello").await.expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");

    let err = handle
        .context_report_at(handle.root(), 99)
        .await
        .expect_err("turn 99 was never persisted");
    match err {
        ConwayError::Runtime(_) => {}
        other => panic!("expected ConwayError::Runtime, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// sessions()
// ---------------------------------------------------------------------

/// Builds a bare `SessionMeta` directly, bypassing `Conway::new_session`
/// entirely: `new_session`'s own committed implementation has a disclosed
/// gap (`RootSpec`, `conway-runtime`/WI-082, has no field for
/// `SessionSpec::labels`) that silently drops any labels a caller passes
/// it, which would make a `new_session`-based fixture unable to exercise
/// this test's label filter at all. `Conway::sessions` is a pure delegation
/// to `SessionStore::list`, so exercising it against directly-created
/// `SessionMeta` values -- the same technique WI-101's own test suite uses
/// for its forked-fixture tests -- tests exactly this item's own criterion
/// without depending on that unrelated, already-disclosed gap.
fn session_meta_with_labels(labels: Vec<String>) -> conway_core::log::SessionMeta {
    conway_core::log::SessionMeta {
        id: SessionId::new(),
        agent_id: AgentId::new(),
        origin: None,
        agent_def: None,
        role: None,
        created: chrono::Utc::now(),
        cwd: std::path::PathBuf::from("."),
        labels,
        status: conway_core::log::SessionStatus::Active,
        ephemeral: false,
        ask_origin: None,
    }
}

#[tokio::test]
async fn sessions_delegates_to_store_list_and_returns_the_filtered_subset() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = build_conway(store.clone());

    let a = session_meta_with_labels(vec!["keep".to_string()]);
    let b = session_meta_with_labels(vec![]);
    let c = session_meta_with_labels(vec!["keep".to_string()]);
    for meta in [&a, &b, &c] {
        store
            .create(meta.clone())
            .await
            .expect("create should succeed");
    }

    let all = conway
        .sessions(SessionFilter::default())
        .await
        .expect("sessions() should succeed");
    assert_eq!(all.len(), 3);

    let filtered = conway
        .sessions(SessionFilter {
            label: Some("keep".to_string()),
            ..SessionFilter::default()
        })
        .await
        .expect("sessions() with a label filter should succeed");
    let filtered_ids: std::collections::BTreeSet<SessionId> =
        filtered.iter().map(|meta| meta.id).collect();
    assert_eq!(
        filtered_ids,
        std::collections::BTreeSet::from([a.id, c.id]),
        "label filter must select exactly the two labeled sessions, excluding {}",
        b.id
    );
}

// ---------------------------------------------------------------------
// fork_from()
// ---------------------------------------------------------------------

#[tokio::test]
async fn fork_from_creates_child_with_expected_origin() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = build_conway(store.clone());
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let at = store.head(&handle.id()).await.expect("head should succeed");

    let child = conway
        .fork_from(handle.id(), at, ForkSpec::new("picking up from here"))
        .await
        .expect("fork_from should succeed");

    let child_meta = store
        .meta(&child.id())
        .await
        .expect("child session must have a header");
    assert_eq!(
        child_meta.origin,
        Some(ForkOrigin {
            parent: handle.id(),
            at_seq: at,
            mode: SubagentMode::Fork,
        })
    );
}

#[tokio::test]
async fn fork_from_rejects_at_beyond_parent_head_naming_both_values() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = build_conway(store.clone());
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let head = store.head(&handle.id()).await.expect("head should succeed");
    let beyond = LogSeq(head.0 + 5);

    let err = expect_session_err(
        conway
            .fork_from(handle.id(), beyond, ForkSpec::new("too far"))
            .await,
        "at beyond the parent's head must be rejected",
    );
    match err {
        ConwayError::Store(inner) => {
            let message = inner.to_string();
            assert!(
                message.contains(&beyond.0.to_string()),
                "error must name the requested seq: {message}"
            );
            assert!(
                message.contains(&head.0.to_string()),
                "error must name the parent's head: {message}"
            );
        }
        other => panic!("expected ConwayError::Store, got {other:?}"),
    }
}

#[tokio::test]
async fn fork_from_copies_zero_records() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = build_conway(store.clone());
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let at = store.head(&handle.id()).await.expect("head should succeed");

    let child = conway
        .fork_from(handle.id(), at, ForkSpec::new("zero-copy"))
        .await
        .expect("fork_from should succeed");

    let child_head = store
        .head(&child.id())
        .await
        .expect("child head should be readable");
    assert_eq!(
        child_head,
        LogSeq::ZERO,
        "a freshly forked child must have zero of its own records"
    );
}

#[tokio::test]
async fn fork_from_at_zero_is_valid_with_an_empty_inherited_prefix() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = build_conway(store.clone());
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");

    let child = conway
        .fork_from(
            handle.id(),
            LogSeq::ZERO,
            ForkSpec::new("from the very start"),
        )
        .await
        .expect("fork_from at seq 0 must be valid");

    let child_meta = store
        .meta(&child.id())
        .await
        .expect("child session must have a header");
    assert_eq!(child_meta.origin.map(|o| o.at_seq), Some(LogSeq::ZERO));

    // `SessionHandle::transcript`'s ancestry walk reads purely through
    // `SessionStore` (see `Conway::resume`'s doc for the same property
    // applied to a resumed session), so it resolves for a `fork_from`
    // child's agent id regardless of the child's live-registration state
    // (WI-119: `fork_from` now also registers the child live via
    // `resume_root`, exercised by the drivability test below -- this test
    // only checks the store-side prefix).
    let transcript = child
        .transcript(child.root())
        .await
        .expect("transcript should resolve for the forked child's agent id");
    assert!(
        transcript.is_empty(),
        "forking at seq 0 must produce an empty inherited prefix"
    );
}

/// The full WI-119 `fork_from` criterion, both halves: `prompt` on the
/// child succeeds and the child produces a real completion (DRIVABLE), AND
/// the child's assembled context contains the parent's inherited prefix
/// (GP-02: a fork inherits the forker's ENTIRE context up to the fork
/// point) -- not a clean-slate spawn in disguise. One `ScriptedBackend`
/// answers two turns -- the parent's, then the child's -- so the child's
/// captured request can be inspected directly.
///
/// Runs on a real multi-thread runtime with an explicit delay between
/// `fork_from` and the child's `prompt`, for the same D-3-shaped reason
/// `resumed_handle_prompt_succeeds_and_continues_the_transcript` above does:
/// the child's `resume_root`-gated first iteration must not run before this
/// handle's own first `prompt` call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_from_returns_a_drivable_child_whose_prompt_succeeds() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("child ack")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend.clone());

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle
        .prompt("parent turn text")
        .await
        .expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");
    let at = store.head(&handle.id()).await.expect("head should succeed");

    let child = conway
        .fork_from(handle.id(), at, ForkSpec::new("picking up from here"))
        .await
        .expect("fork_from should succeed");

    // Give the child's spawned task every chance to run its gated first
    // iteration before its own `prompt` is ever called.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        backend.calls().len(),
        1,
        "fork_from must not run a turn on the child before the caller's own prompt arrives, \
         calls: {:?}",
        backend.calls()
    );

    let child_turn = child
        .prompt("child turn text")
        .await
        .expect("prompt on a fork_from child must succeed, not AgentNotFound");
    let result = tokio::time::timeout(Duration::from_secs(5), child_turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");
    assert!(
        matches!(result.status, ResultStatus::Completed),
        "expected the child to complete after prompt, got: {:?}",
        result.status
    );
    assert_eq!(result.summary, "child ack");

    let calls = backend.calls();
    assert_eq!(
        calls.len(),
        2,
        "expected exactly one backend call for the parent, one for the child, calls: {calls:?}"
    );
    let child_request_text = request_text(&calls[1]);
    assert!(
        child_request_text.contains("child turn text"),
        "expected the child's context to contain its own new prompt, got: {child_request_text}"
    );
    assert!(
        child_request_text.contains("parent turn text"),
        "GP-02: a fork must inherit the forker's entire context up to the fork point -- expected \
         the child's context to contain the parent's pre-fork turn text, got: \
         {child_request_text}"
    );
}

/// GP-02 at fork depth >= 2: a grandchild forked from a fork child inherits
/// the WHOLE ancestor chain (root's turn, then the intermediate child's own
/// turn), not just its immediate parent's. Exercises `resume_root`'s
/// fork-child detection recursively -- resolving the grandchild's
/// `InheritedPrefix` requires walking up through the child's own `origin`
/// to the root, exactly as `conway_session::TranscriptResolver::
/// resolve_prefix`'s own ancestry walk does internally.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_from_at_depth_two_inherits_the_whole_ancestor_chain() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("root ack")),
            ScriptedTurn::Respond(text_response("child ack")),
            ScriptedTurn::Respond(text_response("grandchild ack")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend.clone());

    let root = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let root_turn = root
        .prompt("root turn text")
        .await
        .expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), root_turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");
    let root_head = store.head(&root.id()).await.expect("head should succeed");

    let child = conway
        .fork_from(root.id(), root_head, ForkSpec::new("fork to child"))
        .await
        .expect("fork_from (root -> child) should succeed");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let child_turn = child
        .prompt("child turn text")
        .await
        .expect("prompt on the child should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), child_turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");
    let child_head = store.head(&child.id()).await.expect("head should succeed");

    let grandchild = conway
        .fork_from(child.id(), child_head, ForkSpec::new("fork to grandchild"))
        .await
        .expect("fork_from (child -> grandchild) should succeed");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let grandchild_turn = grandchild
        .prompt("grandchild turn text")
        .await
        .expect("prompt on the grandchild should succeed");
    let result = tokio::time::timeout(Duration::from_secs(5), grandchild_turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");
    assert!(
        matches!(result.status, ResultStatus::Completed),
        "expected the grandchild to complete after prompt, got: {:?}",
        result.status
    );
    assert_eq!(result.summary, "grandchild ack");

    let calls = backend.calls();
    assert_eq!(
        calls.len(),
        3,
        "expected one backend call per generation (root, child, grandchild), calls: {calls:?}"
    );
    let grandchild_request_text = request_text(&calls[2]);
    assert!(
        grandchild_request_text.contains("root turn text"),
        "GP-02 at depth 2: the grandchild must inherit the root's turn text through the whole \
         chain, got: {grandchild_request_text}"
    );
    assert!(
        grandchild_request_text.contains("child turn text"),
        "GP-02 at depth 2: the grandchild must also inherit the intermediate child's own turn \
         text, got: {grandchild_request_text}"
    );
    assert!(
        grandchild_request_text.contains("grandchild turn text"),
        "expected the grandchild's context to contain its own new prompt, got: \
         {grandchild_request_text}"
    );
}

/// The correctness check `Runtime::resume_root`'s own doc calls out: a fork
/// child that has already run turns of its own (non-empty own records)
/// before being resumed (e.g. after a simulated process restart) must still
/// have its parent's prefix resolved into `inherited` -- and that
/// resolution must NOT also fold in the child's own already-persisted
/// records a second time (`AgentLoop` reads those separately, every turn).
/// Asserted here by counting occurrences of the child's pre-restart turn
/// text in the resumed turn's assembled request: exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_fork_child_with_its_own_history_inherits_the_parent_prefix_without_double_counting(
) {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());

    let child_sid;
    {
        let backend = Arc::new(
            ScriptedBackend::new(vec![
                ScriptedTurn::Respond(text_response("parent ack")),
                ScriptedTurn::Respond(text_response("child ack")),
            ])
            .with_id(BackendId::new("fake")),
        );
        let conway = build_conway_with_backend(store.clone(), backend);
        let parent = conway
            .new_session(SessionSpec::default())
            .await
            .expect("new_session should succeed");
        let parent_turn = parent
            .prompt("parent turn text")
            .await
            .expect("prompt should succeed");
        let _ = tokio::time::timeout(Duration::from_secs(5), parent_turn.result())
            .await
            .expect("result() must not hang")
            .expect("result() should succeed");
        let at = store.head(&parent.id()).await.expect("head should succeed");

        let child = conway
            .fork_from(parent.id(), at, ForkSpec::new("picking up from here"))
            .await
            .expect("fork_from should succeed");
        tokio::time::sleep(Duration::from_millis(50)).await;
        let child_turn = child
            .prompt("child turn text before restart")
            .await
            .expect("prompt on the child should succeed");
        let _ = tokio::time::timeout(Duration::from_secs(5), child_turn.result())
            .await
            .expect("result() must not hang")
            .expect("result() should succeed");
        child_sid = child.id();
        // `conway`/its handles drop here -- only `store` survives, simulating a
        // process restart against the same persisted store.
    }

    let backend2 = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("child ack 2"))])
            .with_id(BackendId::new("fake")),
    );
    let conway2 = build_conway_with_backend(store, backend2.clone());
    let resumed_child = conway2
        .resume(child_sid)
        .await
        .expect("resume should succeed after the simulated restart");

    tokio::time::sleep(Duration::from_millis(50)).await;
    let turn = resumed_child
        .prompt("child turn text after restart")
        .await
        .expect("prompt on a resumed fork child must succeed");
    let result = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");
    assert!(
        matches!(result.status, ResultStatus::Completed),
        "expected the resumed child to complete after prompt, got: {:?}",
        result.status
    );
    assert_eq!(result.summary, "child ack 2");

    let calls = backend2.calls();
    assert_eq!(
        calls.len(),
        1,
        "expected exactly one backend call for the resumed child, calls: {calls:?}"
    );
    let text = request_text(&calls[0]);
    assert!(
        text.contains("parent turn text"),
        "the resumed fork child's context must still contain the parent's inherited prefix, got: \
         {text}"
    );
    assert_eq!(
        text.matches("child turn text before restart").count(),
        1,
        "the child's own pre-restart turn must appear exactly once -- inherited-prefix \
         resolution must not double-count the child's own already-persisted records, got: {text}"
    );
    assert!(
        text.contains("child turn text after restart"),
        "expected the resumed child's context to contain the new post-restart prompt, got: {text}"
    );
}
