//! Acceptance tests for `Conway::resume`/`::sessions`/`::fork_from` (WI-103).
//!
//! **Disclosed gap (see `crates/conway/src/conway.rs`'s `Conway::resume`
//! doc for the full reasoning):** two of this work item's binding criteria
//! -- a resumed handle's `tree()` reconstructing all agents including
//! children created before the restart, and a resumed handle's `prompt()`
//! appending at `head + 1` -- require `conway-runtime` to expose a
//! resume/registration capability it does not have today (only
//! `Runtime::start_root` exists, and it cannot be repurposed: it
//! unconditionally calls `SessionStore::create`, which rejects an id that
//! already has a persisted session). This item's own file scope is
//! `crates/conway/src/conway.rs` only, so adding that capability to
//! `conway-runtime` is out of scope here. Flagged to the architect (per the
//! binding notes' own instruction for exactly this situation) rather than
//! worked around with facade-local state that would misrepresent what the
//! runtime actually knows. No test below exercises `tree()`/`prompt()` on a
//! resumed handle; every other criterion is covered.

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    RoleEntry, RoutingSection, SessionConfig,
};
use conway::{Conway, ConwayBuilder, ConwayError, ForkSpec, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::fakes::{FakeBackend, FakeGate, FakeRouter, FakeStore};
use conway_core::ids::{AgentId, BackendId, LogSeq, ModelId, ModelRef, RoleAlias, SessionId};
use conway_core::log::{ForkOrigin, SessionFilter, SubagentMode};
use conway_core::ports::SessionStore;

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }))
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
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = build_conway(store);
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");

    let resumed = conway
        .resume(handle.id())
        .await
        .expect("resume should succeed for an existing session");

    assert_eq!(resumed.id(), handle.id());
    assert_eq!(
        resumed.root(),
        handle.root(),
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
    // child's agent id even though that agent was never attached to the
    // runtime.
    let transcript = child
        .transcript(child.root())
        .await
        .expect("transcript should resolve for the forked child's agent id");
    assert!(
        transcript.is_empty(),
        "forking at seq 0 must produce an empty inherited prefix"
    );
}
