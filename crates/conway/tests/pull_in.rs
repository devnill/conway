//! Acceptance tests for `Conway::pull_in` (B4): an ephemeral `/ask` child's
//! turns merged into the parent's log VERBATIM — the child's `ForkDirective`
//! head record materialized as a `UserTurn` re-stamped
//! `Provenance::MergedAsk { from: child_session }`, its `Assistant` records
//! passed through untouched — followed by the child's purge via
//! `SessionStore::remove` (B1). Also covers the guard matrix: parent not
//! live (`AgentNotLive`), child has children (`NotRemovable`), non-ephemeral
//! child (`NotRemovable`), unknown child (`AgentNotFound`) — each refusing
//! BEFORE the parent's log is mutated.
//!
//! And, since board item `01M0TNBACHQSAMMJ3TY14S47MX`, the failures that
//! happen AFTER the mutation starts — which every guard test above is by
//! construction blind to. The merge is N separate appends with no rollback
//! available, so `FakeStore`'s injected-append-failure seam drives a merge
//! into failure part-way and pins down the state it leaves behind: an
//! annotated (not dangling) parent log, an un-purged child, and a
//! `RuntimeError::PullInIncomplete` saying how far it got.

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::{Conway, ConwayBuilder, FacadeError, Provenance, SessionHandle, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::content::ContentBlock;
use conway_core::error::{RuntimeError, StoreError};
use conway_core::event::Event;
use conway_core::ids::{
    AgentId, BackendId, LogSeq, ModelId, ModelRef, RoleAlias, SeqRange, SessionId,
};
use conway_core::log::{LogRecord, SessionFilter, SessionMeta};
use conway_core::ports::{Backend, GenerateResponse, SessionStore};
use conway_testkit::{FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use futures_core::Stream as _;

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }))
}

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

/// Drives a KEEP-ALIVE session (so the parent stays live — non-terminal
/// `AgentStatus` — for `pull_in`'s guard) through one parent turn plus one
/// completed `/ask`, returning the handle and the ephemeral child's
/// `(AgentId, SessionId)`.
async fn live_session_with_completed_ask(conway: &Conway) -> (SessionHandle, AgentId, SessionId) {
    let handle = conway
        .new_session(SessionSpec {
            keep_alive: true,
            ..SessionSpec::default()
        })
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("parent turn").await.expect("prompt");
    // keep_alive: `result()` would hang (no AgentFinished until the session
    // ends); `text()` resolves on the turn's own TurnFinished.
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.text())
        .await
        .expect("parent turn must not hang")
        .expect("parent turn should succeed");

    let ask_turn = handle.ask("an ephemeral aside").await.expect("ask");
    let _ = tokio::time::timeout(Duration::from_secs(5), ask_turn.result())
        .await
        .expect("ask result must not hang")
        .expect("ask result should succeed");

    let child_meta = conway
        .sessions(SessionFilter {
            include_ephemeral: true,
            ..SessionFilter::default()
        })
        .await
        .expect("sessions() should succeed")
        .into_iter()
        .find(|m| m.id != handle.id())
        .expect("the ephemeral ask child must be present");
    (handle, child_meta.agent_id, child_meta.id)
}

async fn read_all(store: &Arc<dyn SessionStore>, sid: SessionId) -> Vec<LogRecord> {
    let head = store.head(&sid).await.expect("head should succeed");
    store
        .read(&sid, SeqRange::new(LogSeq::ZERO, Some(head)))
        .await
        .expect("read should succeed")
}

// ---------------------------------------------------------------------
// Happy path: verbatim merge, contiguous re-sequencing, child purged.
// ---------------------------------------------------------------------

#[tokio::test]
async fn pull_in_merges_the_ask_child_verbatim_then_purges_it() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("the ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    let (handle, child_agent, child_session) = live_session_with_completed_ask(&conway).await;

    // Capture the child's own records pre-merge as the verbatim baseline.
    let child_records = read_all(&store, child_session).await;
    let child_assistants: Vec<&LogRecord> = child_records
        .iter()
        .filter(|r| matches!(r, LogRecord::Assistant { .. }))
        .collect();
    assert_eq!(
        child_assistants.len(),
        1,
        "precondition: the scripted ask child produced exactly one Assistant record, got: {child_records:?}"
    );
    assert!(
        matches!(child_records.first(), Some(LogRecord::ForkDirective { .. })),
        "precondition (the B2 amendment): the child is ForkDirective-headed, got: {child_records:?}"
    );

    let parent_head_before = store.head(&handle.id()).await.expect("head");

    conway
        .pull_in(child_agent)
        .await
        .expect("pull_in of a completed ephemeral ask child must succeed");

    // The parent log gained exactly the merge set: the question (as a
    // UserTurn) plus the one Assistant record.
    let parent_records = read_all(&store, handle.id()).await;
    let before = parent_head_before.0 as usize;
    assert_eq!(
        parent_records.len(),
        before + 2,
        "expected the question + the answer appended, got: {parent_records:?}"
    );

    // The store re-sequenced the appended records: seqs are contiguous
    // across the whole parent log (no child-local seqs leaked through).
    for (i, record) in parent_records.iter().enumerate() {
        assert_eq!(
            record.seq(),
            Some(LogSeq(i as u64)),
            "parent log seqs must be contiguous after the merge"
        );
    }

    // The question landed as a UserTurn re-stamped MergedAsk, naming the
    // (now purged) child session.
    match &parent_records[before] {
        LogRecord::UserTurn { text, prov, .. } => {
            assert_eq!(text, "an ephemeral aside");
            assert_eq!(
                *prov,
                Provenance::MergedAsk {
                    from: child_session
                },
                "the merged question must carry MergedAsk provenance ()"
            );
        }
        other => panic!("expected the merged question as a UserTurn, got: {other:?}"),
    }

    // The answer landed VERBATIM: every untrusted model-produced field
    // — content, model, route_reason, usage, stop — plus ts is
    // field-for-field the child's original record; only the seq moved.
    let (merged_content, merged_model, merged_route, merged_usage, merged_stop, merged_ts) =
        match &parent_records[before + 1] {
            LogRecord::Assistant {
                content,
                model,
                route_reason,
                usage,
                stop,
                ts,
                ..
            } => (content, model, route_reason, usage, stop, ts),
            other => panic!("expected the merged answer as an Assistant record, got: {other:?}"),
        };
    match child_assistants[0] {
        LogRecord::Assistant {
            content,
            model,
            route_reason,
            usage,
            stop,
            ts,
            ..
        } => {
            assert_eq!(merged_content, content);
            assert_eq!(merged_model, model);
            assert_eq!(merged_route, route_reason);
            assert_eq!(merged_usage, usage);
            assert_eq!(merged_stop, stop);
            assert_eq!(merged_ts, ts);
        }
        other => unreachable!("filtered to Assistant above, got: {other:?}"),
    }

    // The child session is gone — from the store AND from every listing,
    // ephemeral-inclusive ones included.
    let err = store
        .meta(&child_session)
        .await
        .expect_err("the child's meta must be gone");
    assert!(matches!(err, StoreError::NotFound { session } if session == child_session));
    let remaining = conway
        .sessions(SessionFilter {
            include_ephemeral: true,
            ..SessionFilter::default()
        })
        .await
        .expect("sessions() should succeed");
    assert_eq!(
        remaining.len(),
        1,
        "only the parent's session may remain, got: {remaining:?}"
    );

    // A second remove of the child is NotFound (the purge was real, not a
    // flag flip).
    let err = store
        .remove(&child_session)
        .await
        .expect_err("remove of the purged child must be NotFound");
    assert!(matches!(err, StoreError::NotFound { session } if session == child_session));

    // The child's tree node stays attached (AgentTree never detaches — the
    // provenance that the ask happened survives the purge).
    assert!(
        handle
            .tree()
            .nodes
            .iter()
            .any(|n| n.agent_id == child_agent),
        "the child's tree node must survive the purge"
    );
}

// ---------------------------------------------------------------------
// The live surface (board item `01M0RWT9V7GNYRR53MTTQ2Y07K`): the merge
// above was always durable — this proves it is now ALSO visible, live, to
// a subscriber on the parent's own event stream, without a resume.
// ---------------------------------------------------------------------

/// **The load-bearing new test for this item.** The happy-path test above
/// (`pull_in_merges_the_ask_child_verbatim_then_purges_it`) already proves
/// the STORAGE side; it asserts nothing about the event stream, which is
/// exactly why the bug this item fixes shipped past it — a test that
/// `pull_in` was *called* (or even that it durably merged) cannot see that
/// its result was invisible. This test instead subscribes to
/// `handle.events()` — the SAME primitive a library embedder or any other
/// non-TUI `EventStream` consumer would use, not a TUI-internal type —
/// BEFORE calling `Conway::pull_in`, then asserts the merged question and
/// answer actually arrive on that live stream, in order, naming the
/// parent. Driving the production `Conway::pull_in` call (not a unit test
/// of a rendering function) is deliberate: only the live path can show
/// this bug is fixed.
#[tokio::test]
async fn pull_in_emits_the_merged_question_and_answer_on_the_parents_live_stream() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("the ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    let (handle, child_agent, child_session) = live_session_with_completed_ask(&conway).await;

    // Subscribe BEFORE pull_in, on the PARENT-scoped stream: unlike
    // `AgentPromoted` (stamped under the CHILD's own session — see
    // `promote.rs`'s own test), the merge's events are emitted under the
    // PARENT's own session/agent, since this is the parent's own
    // transcript growing, not the (about-to-be-purged) child's.
    let mut events = handle.events();

    conway
        .pull_in(child_agent)
        .await
        .expect("pull_in of a completed ephemeral ask child must succeed");

    // Drain the parent's live stream until the marker, the question AND
    // the answer have all been seen — a BOUNDED collection, so a bug that
    // emits nothing (this item's own reported symptom) fails this test
    // instead of hanging it.
    let (marker, question, answer) = tokio::time::timeout(Duration::from_secs(5), async {
        let mut marker = None;
        let mut question = None;
        let mut answer = None;
        while question.is_none() || answer.is_none() {
            let envelope = std::future::poll_fn(|cx| std::pin::Pin::new(&mut events).poll_next(cx))
                .await
                .expect("event stream open");
            match envelope.event {
                Event::AgentProgress { note } if marker.is_none() => {
                    marker = Some((envelope.agent, envelope.session, note));
                }
                Event::UserTurn { text, prov } if question.is_none() => {
                    question = Some((envelope.agent, envelope.session, text, prov));
                }
                Event::TextDelta { text } if answer.is_none() => {
                    answer = Some((envelope.agent, envelope.session, text));
                }
                _ => {}
            }
        }
        (marker, question, answer)
    })
    .await
    .expect(
        "timed out waiting for the merged question and answer to reach the parent's live \
         stream — pull_in must EMIT them, not merely persist them",
    );

    // The marker (this item's answer to "what exactly should appear" —
    // `Provenance::MergedAsk`'s own doc says the merge origin stays
    // "explicit and inspectable", so the live surface must say so too, not
    // render the merged turns indistinguishable from an ordinary
    // prompt/reply) precedes the content, names the parent, and names the
    // child session the merge came from.
    let (marker_agent, marker_session, marker_note) =
        marker.expect("an Event::AgentProgress marker must precede the merged content");
    assert_eq!(marker_agent, handle.root());
    assert_eq!(marker_session, handle.id());
    assert!(
        marker_note.contains(&child_session.to_string()),
        "the marker must name the child session the merge came from, got: {marker_note}"
    );

    // The question: same text, and the SAME `MergedAsk` provenance the
    // persisted record carries (proven separately by the happy-path test
    // above) — the live event is not a different, weaker signal.
    let (q_agent, q_session, q_text, q_prov) = question.expect("set inside the loop above");
    assert_eq!(q_agent, handle.root(), "the merge is the PARENT's own turn");
    assert_eq!(q_session, handle.id());
    assert_eq!(q_text, "an ephemeral aside");
    assert_eq!(
        q_prov,
        Provenance::MergedAsk {
            from: child_session
        },
        "the live event must carry the same MergedAsk provenance the persisted record does"
    );

    // The answer: the child's real reply text, also on the parent's own
    // stream.
    let (a_agent, a_session, a_text) = answer.expect("set inside the loop above");
    assert_eq!(a_agent, handle.root());
    assert_eq!(a_session, handle.id());
    assert_eq!(a_text, "the ask answer");
}

// ---------------------------------------------------------------------
// Partial merges (board item `01M0TNBACHQSAMMJ3TY14S47MX`): the merge is
// N separate appends and `SessionStore` has no multi-record append and no
// rollback, so a failure part-way through leaves durable records behind.
// The guard-matrix tests below all refuse BEFORE any mutation, so none of
// them can see this; these three drive the mutation itself into failure,
// using `FakeStore`'s injected-append-failure seam.
// ---------------------------------------------------------------------

/// Builds the same `Conway` the helper above does, but keeps the CONCRETE
/// `FakeStore` handle so a test can reach its injected-failure knobs (the
/// `Arc<dyn SessionStore>` the builder takes cannot).
fn build_conway_with_fake_store(
    store: Arc<FakeStore>,
    backend: Arc<dyn Backend>,
) -> (Conway, Arc<dyn SessionStore>) {
    let dyn_store: Arc<dyn SessionStore> = store.clone();
    let conway = build_conway_with_backend(dyn_store.clone(), backend);
    (conway, dyn_store)
}

fn two_turn_backend() -> Arc<dyn Backend> {
    Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("the ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    )
}

/// **The load-bearing test for this item.** A pull-in whose SECOND append
/// fails: the question has already landed in the parent's durable log and
/// cannot be withdrawn, so the answer never arriving would leave the
/// parent's transcript — and, on the next turn, the MODEL's assembled
/// context — holding a question that nothing answers. The fix is a
/// forward-written `SystemNote` (the only repair an append-only log
/// admits) plus an error that says how far the merge got.
///
/// Every assertion below except the "nothing was purged" pair fails
/// against the pre-fix code, which `?`-propagated the append error: it
/// returned `FacadeError::Store(Io)` (not `PullInIncomplete`) and left the
/// parent's log ending on the dangling question with no note after it.
#[tokio::test]
async fn pull_in_whose_second_append_fails_annotates_the_truncation_and_reports_how_far_it_got() {
    let fake = Arc::new(FakeStore::new());
    let (conway, store) = build_conway_with_fake_store(fake.clone(), two_turn_backend());

    let (handle, child_agent, child_session) = live_session_with_completed_ask(&conway).await;

    // Precondition: the merge set really is exactly two records (the
    // ForkDirective-headed question + one Assistant answer), so "the
    // second append" is unambiguously the answer's.
    let child_records = read_all(&store, child_session).await;
    let merge_set = child_records
        .iter()
        .filter(|r| {
            matches!(
                r,
                LogRecord::ForkDirective { .. }
                    | LogRecord::Assistant { .. }
                    | LogRecord::UserTurn { .. }
            )
        })
        .count();
    assert_eq!(
        merge_set, 2,
        "precondition: question + answer, got: {child_records:?}"
    );
    let child_head_before = store.head(&child_session).await.expect("head");
    let parent_head_before = store.head(&handle.id()).await.expect("head");

    // Subscribe before the failure so the live surface can be checked too:
    // the operator has ALREADY been shown the question by the time the
    // answer's append fails, so the truncation has to reach the transcript
    // as well as the log, or the two disagree.
    let mut events = handle.events();

    // Arm the seam: the next append (the question) succeeds, the one after
    // it (the answer) fails. The knob is one-shot, so the note's own
    // append — the third — succeeds.
    fake.fail_nth_append(
        2,
        StoreError::Io {
            detail: "injected: the answer's append failed".into(),
        },
    );

    let err = conway
        .pull_in(child_agent)
        .await
        .expect_err("a merge whose second append fails must not report success");

    // (1) The caller is told the merge is INCOMPLETE, and how far it got —
    // not handed a bare store error indistinguishable from the guard
    // refusals, which mutate nothing. FAILS pre-fix: `Store(Io)`.
    match &err {
        FacadeError::Runtime(RuntimeError::PullInIncomplete {
            parent,
            child,
            merged,
            of,
            note_appended,
            cause,
        }) => {
            assert_eq!(*parent, handle.id());
            assert_eq!(*child, child_session);
            assert_eq!(
                (*merged, *of),
                (1, 2),
                "one of the two merge-set records landed"
            );
            assert!(
                *note_appended,
                "the store only failed one append, so the truncation note must have landed"
            );
            assert!(
                matches!(&**cause, StoreError::Io { detail } if detail.contains("injected")),
                "the underlying store error must be carried through, got: {cause:?}"
            );
        }
        other => panic!("expected PullInIncomplete, got: {other:?}"),
    }

    // (2) The parent's log is COHERENT: the question that landed is
    // followed by a SystemNote saying the merge was truncated, so the next
    // turn's context assembly cannot show the model a question with no
    // answer. FAILS pre-fix: the log gained ONE record (the bare question)
    // and ends there.
    let parent_records = read_all(&store, handle.id()).await;
    let before = parent_head_before.0 as usize;
    assert_eq!(
        parent_records.len(),
        before + 2,
        "expected the question + the truncation note, got: {parent_records:?}"
    );
    match &parent_records[before] {
        LogRecord::UserTurn { text, prov, .. } => {
            assert_eq!(text, "an ephemeral aside");
            assert_eq!(
                *prov,
                Provenance::MergedAsk {
                    from: child_session
                }
            );
        }
        other => panic!("expected the merged question, got: {other:?}"),
    }
    match &parent_records[before + 1] {
        LogRecord::SystemNote {
            text, reason, prov, ..
        } => {
            assert_eq!(
                reason, "pull_in_truncated",
                "the note carries a machine-readable reason, not only prose"
            );
            assert_eq!(
                *prov,
                Provenance::SystemNote {
                    reason: "pull_in_truncated".to_string()
                }
            );
            assert!(
                text.contains(&child_session.to_string()),
                "the note must name the child that still holds the ask, got: {text}"
            );
        }
        other => panic!("expected a truncation SystemNote after the question, got: {other:?}"),
    }

    // (3) The child was NOT purged and its records are untouched — the
    // only surviving copy of the answer that did not merge. (Also true
    // pre-fix, by accident: the `?` skipped the purge. Asserted because
    // it is now a DELIBERATE guarantee, not a side effect.)
    store
        .meta(&child_session)
        .await
        .expect("the child must survive a failed merge — it holds the unmerged answer");
    assert_eq!(
        store.head(&child_session).await.expect("head"),
        child_head_before,
        "the child's own log must be untouched"
    );

    // (4) The transcript says what the log says: the marker and the
    // question were already emitted, and the truncation note follows them
    // live. FAILS pre-fix: no third event ever arrives, so this times out.
    let notes = tokio::time::timeout(Duration::from_secs(5), async {
        let mut notes: Vec<String> = Vec::new();
        loop {
            let envelope = std::future::poll_fn(|cx| std::pin::Pin::new(&mut events).poll_next(cx))
                .await
                .expect("event stream open");
            // Scoped to the PARENT's own session/agent, like the merge
            // events themselves — anything emitted elsewhere is not this
            // operation's live surface, and a note that landed under the
            // wrong session times this out rather than passing.
            if envelope.session != handle.id() || envelope.agent != handle.root() {
                continue;
            }
            if let Event::AgentProgress { note } = envelope.event {
                let done = note.contains("did not complete");
                notes.push(note);
                if done {
                    return notes;
                }
            }
        }
    })
    .await
    .expect(
        "timed out waiting for the truncation to reach the parent's live stream — the operator \
         was already shown the question, so the log and the transcript must not disagree",
    );
    assert!(
        notes.len() >= 2 && notes[0].contains("pulled in from /ask"),
        "the merge marker must still precede the truncation note, got: {notes:?}"
    );
    assert!(
        notes
            .last()
            .expect("non-empty")
            .contains(&child_session.to_string()),
        "the live truncation note must name the child too, got: {notes:?}"
    );
}

/// A merge whose FIRST append fails mutated nothing, so it stays an
/// ordinary `FacadeError::Store` — the same shape every guard refusal
/// returns. This is the other half of "the caller can tell nothing-happened
/// from something-happened": without it, `PullInIncomplete` would be
/// evidence of nothing.
#[tokio::test]
async fn pull_in_whose_first_append_fails_is_a_clean_no_op() {
    let fake = Arc::new(FakeStore::new());
    let (conway, store) = build_conway_with_fake_store(fake.clone(), two_turn_backend());

    let (handle, child_agent, child_session) = live_session_with_completed_ask(&conway).await;
    let parent_head_before = store.head(&handle.id()).await.expect("head");

    fake.fail_nth_append(
        1,
        StoreError::Io {
            detail: "injected: the question's append failed".into(),
        },
    );

    let err = conway
        .pull_in(child_agent)
        .await
        .expect_err("a merge whose first append fails must be reported");
    assert!(
        matches!(&err, FacadeError::Store(StoreError::Io { detail }) if detail.contains("injected")),
        "nothing landed, so this must stay an ordinary store error, got: {err:?}"
    );
    assert!(
        !matches!(
            err,
            FacadeError::Runtime(RuntimeError::PullInIncomplete { .. })
        ),
        "PullInIncomplete must mean something landed, or it means nothing at all"
    );

    assert_eq!(
        store.head(&handle.id()).await.expect("head"),
        parent_head_before,
        "no record — not even a truncation note — may be written when nothing merged"
    );
    store
        .meta(&child_session)
        .await
        .expect("the child must survive");
}

/// When the store has gone away entirely, the best-effort truncation note
/// cannot be written either — and that is reported (`note_appended:
/// false`) rather than silently swallowed, because it is the difference
/// between a log left annotated and a log left with a dangling question.
#[tokio::test]
async fn pull_in_reports_when_even_the_truncation_note_cannot_be_written() {
    let fake = Arc::new(FakeStore::new());
    let (conway, store) = build_conway_with_fake_store(fake.clone(), two_turn_backend());

    let (handle, child_agent, child_session) = live_session_with_completed_ask(&conway).await;
    let parent_head_before = store.head(&handle.id()).await.expect("head");

    // Sticky: the answer's append fails AND so does the note's.
    fake.fail_appends_from(
        2,
        StoreError::Io {
            detail: "injected: the store is gone".into(),
        },
    );

    let err = conway
        .pull_in(child_agent)
        .await
        .expect_err("the merge must be reported as incomplete");
    assert!(
        matches!(
            err,
            FacadeError::Runtime(RuntimeError::PullInIncomplete {
                merged: 1,
                of: 2,
                note_appended: false,
                ..
            })
        ),
        "expected an unannotated truncation, got: {err:?}"
    );

    // The honest end state: exactly the one record that landed, and no
    // note claiming otherwise.
    let parent_records = read_all(&store, handle.id()).await;
    assert_eq!(parent_records.len(), parent_head_before.0 as usize + 1);
    assert!(
        matches!(parent_records.last(), Some(LogRecord::UserTurn { .. })),
        "got: {parent_records:?}"
    );
    store
        .meta(&child_session)
        .await
        .expect("the child must survive");
}

/// The second half of the same disclosure problem: a merge that landed in
/// FULL and then failed to purge the child. Pre-fix this returned a bare
/// `NotRemovable` — byte-identical in shape to the pre-check guard that
/// refuses before writing anything — so a caller could read "nothing
/// happened", retry, and merge the ask twice. It now reports
/// `PullInIncomplete` with `merged == of`: nothing was truncated (so there
/// is no note, and the log is coherent), only the purge is outstanding.
#[tokio::test]
async fn pull_in_whose_purge_fails_reports_a_complete_but_unpurged_merge() {
    let fake = Arc::new(FakeStore::new());
    let (conway, store) = build_conway_with_fake_store(fake.clone(), two_turn_backend());

    let (handle, child_agent, child_session) = live_session_with_completed_ask(&conway).await;
    let parent_head_before = store.head(&handle.id()).await.expect("head");

    fake.fail_nth_remove(
        1,
        StoreError::Io {
            detail: "injected: the purge failed".into(),
        },
    );

    let err = conway
        .pull_in(child_agent)
        .await
        .expect_err("a failed purge after a complete merge must be reported");
    assert!(
        matches!(
            &err,
            FacadeError::Runtime(RuntimeError::PullInIncomplete {
                child,
                merged: 2,
                of: 2,
                note_appended: false,
                ..
            }) if *child == child_session
        ),
        "expected a complete-but-unpurged report, got: {err:?}"
    );

    // The merge itself is whole and unannotated — there is nothing
    // incoherent about it, so nothing to annotate.
    let parent_records = read_all(&store, handle.id()).await;
    assert_eq!(
        parent_records.len(),
        parent_head_before.0 as usize + 2,
        "the whole merge set landed, got: {parent_records:?}"
    );
    assert!(
        !parent_records
            .iter()
            .any(|r| matches!(r, LogRecord::SystemNote { .. })),
        "a COMPLETE merge must not be annotated as truncated, got: {parent_records:?}"
    );
    store
        .meta(&child_session)
        .await
        .expect("the un-purged child is exactly what this error reports");
}

// ---------------------------------------------------------------------
// Guard matrix — each refusal happens BEFORE the parent's log is mutated.
// ---------------------------------------------------------------------

/// Parent not live: a non-keep-alive parent is `Finished` once its first
/// turn completes, and merging into a log nothing will ever read again is
/// refused with `AgentNotLive` — and neither the merge nor the purge
/// happens.
#[tokio::test]
async fn pull_in_refuses_when_the_parent_is_not_live() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    // NOT keep_alive: the root agent task terminates on its first
    // Completed turn, leaving the tree node `Finished`.
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("parent turn").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("parent result must not hang")
        .expect("parent result should succeed");

    let ask_turn = handle.ask("an ephemeral aside").await.expect("ask");
    let _ = tokio::time::timeout(Duration::from_secs(5), ask_turn.result())
        .await
        .expect("ask result must not hang")
        .expect("ask result should succeed");

    let child_meta = conway
        .sessions(SessionFilter {
            include_ephemeral: true,
            ..SessionFilter::default()
        })
        .await
        .expect("sessions()")
        .into_iter()
        .find(|m| m.id != handle.id())
        .expect("the ask child must be present");

    let parent_head_before = store.head(&handle.id()).await.expect("head");

    let err = conway
        .pull_in(child_meta.agent_id)
        .await
        .expect_err("pull_in with a finished parent must be refused");
    assert!(
        matches!(err, FacadeError::Runtime(RuntimeError::AgentNotLive { agent }) if agent == handle.root()),
        "expected AgentNotLive naming the parent, got: {err:?}"
    );

    // Failure ordering: the parent's log is untouched and the child was NOT
    // purged.
    assert_eq!(
        store.head(&handle.id()).await.expect("head"),
        parent_head_before,
        "a refused pull_in must not mutate the parent's log"
    );
    store
        .meta(&child_meta.id)
        .await
        .expect("the child session must survive a refused pull_in");
}

/// B1's guards apply: a child that has children of its own (even ephemeral
/// ones) is refused with `NotRemovable` — the merge would orphan the
/// grandchild's provenance.
#[tokio::test]
async fn pull_in_refuses_when_the_child_has_children() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    let (handle, child_agent, child_session) = live_session_with_completed_ask(&conway).await;

    // Give the child a child of its own, directly at the store layer (B1's
    // guard reads `list(parent: child, include_ephemeral: true)`, so a
    // store-level grandchild exercises exactly what `remove` will check).
    let child_head = store.head(&child_session).await.expect("head");
    let grandchild_meta = SessionMeta {
        id: SessionId::new(),
        agent_id: AgentId::new(),
        origin: None, // overridden by `fork`
        agent_def: None,
        role: None,
        created: chrono::Utc::now(),
        cwd: std::path::PathBuf::from("."),
        labels: vec![],
        ephemeral: false,
        ask_origin: None,
        root: None,
        plugin_config: conway_core::ports::PluginConfig::default(),
    };
    store
        .fork(&child_session, child_head, grandchild_meta)
        .await
        .expect("forking the ask child should succeed");

    let parent_head_before = store.head(&handle.id()).await.expect("head");

    let err = conway
        .pull_in(child_agent)
        .await
        .expect_err("pull_in of a child that has children must be refused");
    assert!(
        matches!(err, FacadeError::Store(StoreError::NotRemovable { session, .. }) if session == child_session),
        "expected NotRemovable naming the child session, got: {err:?}"
    );

    assert_eq!(
        store.head(&handle.id()).await.expect("head"),
        parent_head_before,
        "a refused pull_in must not mutate the parent's log"
    );
    store
        .meta(&child_session)
        .await
        .expect("the child session must survive a refused pull_in");
}

/// B1's guards apply: a non-ephemeral child (here: an ask child that was
/// first promoted, B3) is refused with `NotRemovable` — pull_in merges only
/// ephemeral scratchpads.
#[tokio::test]
async fn pull_in_refuses_a_non_ephemeral_child() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    let (handle, child_agent, child_session) = live_session_with_completed_ask(&conway).await;

    // Promote flips the child to persistent — after which pull_in must
    // refuse it (the two fates are mutually exclusive).
    conway
        .promote(child_agent)
        .await
        .expect("promote of an ephemeral ask child must succeed");

    let parent_head_before = store.head(&handle.id()).await.expect("head");

    let err = conway
        .pull_in(child_agent)
        .await
        .expect_err("pull_in of a non-ephemeral child must be refused");
    assert!(
        matches!(err, FacadeError::Store(StoreError::NotRemovable { session, .. }) if session == child_session),
        "expected NotRemovable naming the child session, got: {err:?}"
    );

    assert_eq!(
        store.head(&handle.id()).await.expect("head"),
        parent_head_before,
        "a refused pull_in must not mutate the parent's log"
    );
    store
        .meta(&child_session)
        .await
        .expect("the (promoted) child session must survive a refused pull_in");
}

/// The facade-layer live check: an agent id absent from `Runtime::tree()`
/// is refused with `AgentNotFound` before any store access.
#[tokio::test]
async fn pull_in_an_unknown_child_is_refused() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("parent ack"))])
            .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("parent turn").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("parent result must not hang")
        .expect("parent result should succeed");

    let unknown = AgentId::new();
    let err = conway
        .pull_in(unknown)
        .await
        .expect_err("an unknown child must fail");
    assert!(
        matches!(err, FacadeError::Runtime(RuntimeError::AgentNotFound { agent }) if agent == unknown),
        "unknown child must be AgentNotFound, got: {err:?}"
    );
}

/// The child must be TERMINAL: a still-running child is refused BEFORE any
/// merge (cycle-5 B4 review, significant finding 1). Merging mid-turn would
/// read a partial child log and then purge the session under a live agent
/// loop (its next append would fail `NotFound`). Terminal status is
/// absorbing, so the snapshot check can never go stale.
#[tokio::test]
async fn pull_in_refuses_a_still_running_child() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    // The child's only scripted turn never resolves, so the ask child is
    // deterministically mid-turn (non-terminal) when pull_in is called.
    let backend =
        Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Pending]).with_id(BackendId::new("fake")));
    let conway = build_conway_with_backend(store.clone(), backend);

    // keep_alive root, NO parent prompt: the parent is live (idle), and the
    // child turn is the only backend call.
    let handle = conway
        .new_session(SessionSpec {
            keep_alive: true,
            ..SessionSpec::default()
        })
        .await
        .expect("new_session should succeed");
    let ask_turn = handle.ask("an ephemeral aside").await.expect("ask");

    // Poll the tree until the ephemeral child node appears in a non-terminal
    // state (attach precedes the turn; the Pending turn keeps it there).
    let mut child_node = None;
    for _ in 0..100 {
        if let Some(n) = handle
            .tree()
            .nodes
            .iter()
            .find(|n| n.parent == Some(handle.root()) && n.ephemeral)
        {
            child_node = Some(n.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let child_node = child_node.expect("the ask child must attach within 5s");
    assert!(
        !matches!(
            child_node.status,
            conway_core::agent::AgentStatus::Finished
                | conway_core::agent::AgentStatus::Failed
                | conway_core::agent::AgentStatus::Cancelled
        ),
        "precondition: the child must be non-terminal, got {:?}",
        child_node.status
    );

    let parent_head_before = store.head(&handle.id()).await.expect("head");

    let err = conway
        .pull_in(child_node.agent_id)
        .await
        .expect_err("pull_in of a still-running child must be refused");
    assert!(
        matches!(err, FacadeError::Store(StoreError::NotRemovable { .. })),
        "expected NotRemovable for a still-running child, got: {err:?}"
    );

    // Failure ordering: the parent's log is untouched and the child was NOT
    // purged.
    assert_eq!(
        store.head(&handle.id()).await.expect("head"),
        parent_head_before,
        "a refused pull_in must not mutate the parent's log"
    );
    store
        .meta(&child_node.session)
        .await
        .expect("the child session must survive a refused pull_in");

    // Do NOT await ask_turn (its scripted answer is 60s out); dropping it
    // leaves the runtime task to be torn down with the test runtime.
    drop(ask_turn);
}
