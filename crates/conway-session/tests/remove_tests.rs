//! Integration tests for `SessionStore::remove` (B1): the guarded purge
//! primitive — the single user-explicit exception to mandatory provenance
//! retention (P-2/GP-10). Covers the guard matrix (ephemeral-only;
//! children — ephemeral ones included — block removal), file deletion,
//! `SessionIndex` eviction (`by_id` + `children` map), and the no-WARN-
//! rebuild-on-reopen invariant.
//!
//! WARN capture approach: `tracing-test` is not a dependency of this crate
//! (see `recovery_tests.rs`), so these tests reuse the same minimal
//! `tracing::Subscriber` capture harness as `index_tests.rs`.

use std::path::PathBuf;

use chrono::{DateTime, Utc};

use conway_core::error::StoreError;
use conway_core::ids::{AgentId, LogSeq, SessionId};
use conway_core::log::{ForkOrigin, SessionStatus, SubagentMode};
use conway_core::ports::SessionStore;
use conway_session::{JsonlSessionStore, SessionFilter, SessionMeta};

fn ts() -> DateTime<Utc> {
    "2026-07-20T00:00:00Z".parse().unwrap()
}

fn meta(id: SessionId, origin: Option<ForkOrigin>, ephemeral: bool) -> SessionMeta {
    SessionMeta {
        id,
        agent_id: AgentId::new(),
        origin,
        agent_def: None,
        role: None,
        created: ts(),
        cwd: PathBuf::from("/tmp/project"),
        labels: vec![],
        status: SessionStatus::Active,
        ephemeral,
        ask_origin: None,
        root: None,
    }
}

fn ephemeral_meta(id: SessionId) -> SessionMeta {
    meta(id, None, true)
}

fn persistent_meta(id: SessionId) -> SessionMeta {
    meta(id, None, false)
}

fn child_origin(parent: SessionId) -> ForkOrigin {
    ForkOrigin {
        parent,
        at_seq: LogSeq(0),
        mode: SubagentMode::Fork,
    }
}

// ---------------------------------------------------------------------
// Minimal tracing WARN capture (no tracing-subscriber dependency) — the
// identical pattern and rationale as index_tests.rs / recovery_tests.rs.
// ---------------------------------------------------------------------

#[derive(Clone, Default)]
struct CaptureLog {
    entries: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl CaptureLog {
    fn contains(&self, needle: &str) -> bool {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.contains(needle))
    }
}

struct CaptureSubscriber {
    log: CaptureLog,
}

struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl tracing::Subscriber for CaptureSubscriber {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);
        self.log.entries.lock().unwrap().push(visitor.0);
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

fn install_capture() -> (CaptureLog, tracing::subscriber::DefaultGuard) {
    let log = CaptureLog::default();
    let guard = tracing::subscriber::set_default(CaptureSubscriber { log: log.clone() });
    (log, guard)
}

// ---------------------------------------------------------------------
// Happy path: remove deletes records; reopen shows the session gone with
// no WARN-rebuild (index eviction + persist_full worked).
// ---------------------------------------------------------------------

#[tokio::test]
async fn remove_deletes_records_and_reopen_shows_session_gone_without_rebuild_warn() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();

    {
        let store = JsonlSessionStore::open(root.clone()).await.unwrap();
        store.create(ephemeral_meta(sid)).await.unwrap();
        let head = store.head(&sid).await.unwrap();
        store
            .append(
                &sid,
                conway_core::log::LogRecord::UserTurn {
                    seq: head,
                    ts: ts(),
                    text: "scratchpad turn".into(),
                    prov: conway_core::provenance::Provenance::UserPrompt,
                },
            )
            .await
            .unwrap();
        assert!(root.join(format!("{sid}.jsonl")).exists());

        store.remove(&sid).await.unwrap();

        // The session file is gone, and the in-memory index no longer
        // surfaces the session through any read path.
        assert!(
            !root.join(format!("{sid}.jsonl")).exists(),
            "remove must delete the session file"
        );
        assert!(matches!(
            store.meta(&sid).await.unwrap_err(),
            StoreError::NotFound { .. }
        ));
        assert!(matches!(
            store.head(&sid).await.unwrap_err(),
            StoreError::NotFound { .. }
        ));
        let listed = store
            .list(SessionFilter {
                include_ephemeral: true,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            listed.iter().all(|m| m.id != sid),
            "removed session must be evicted from the index by_id map"
        );
        assert!(
            store.children(&sid).await.unwrap().is_empty(),
            "removed session must be evicted from the index children map"
        );
    }

    // Reopen over the same root: index.jsonl must be consistent with disk
    // (remove rewrote it), so try_load succeeds and no "index rebuild"
    // WARN fires.
    let (log, _guard) = install_capture();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    assert!(
        !log.contains("index rebuild"),
        "expected no rebuild after remove + reopen, got: {:?}",
        log.entries.lock().unwrap()
    );
    assert!(matches!(
        store.meta(&sid).await.unwrap_err(),
        StoreError::NotFound { .. }
    ));
    let listed = store
        .list(SessionFilter {
            include_ephemeral: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        listed.is_empty(),
        "a reopened store must not list a removed session"
    );
}

// ---------------------------------------------------------------------
// Index children-map eviction: removing an ephemeral child must remove
// its entry in the parent's children list, which is what then unblocks
// removal of the parent itself.
// ---------------------------------------------------------------------

#[tokio::test]
async fn removing_last_child_unblocks_removal_of_the_parent() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::open(dir.path().to_path_buf())
        .await
        .unwrap();

    let parent = SessionId::new();
    store.create(ephemeral_meta(parent)).await.unwrap();
    let child = SessionId::new();
    store
        .create(meta(child, Some(child_origin(parent)), true))
        .await
        .unwrap();

    // While the child exists the parent is guarded (see the trap test
    // below); after removing the child, the parent's children-map entry
    // must be gone from the index — proven mechanically by the parent's
    // own remove now succeeding.
    store.remove(&child).await.unwrap();
    let kids = store
        .list(SessionFilter {
            parent: Some(parent),
            include_ephemeral: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        kids.is_empty(),
        "removed child must be evicted from the parent's children list"
    );

    store
        .remove(&parent)
        .await
        .unwrap_or_else(|e| panic!("parent removal must succeed once its last child is gone: {e}"));
}

// ---------------------------------------------------------------------
// Guard 1: non-ephemeral sessions are never removable.
// ---------------------------------------------------------------------

#[tokio::test]
async fn remove_refuses_a_non_ephemeral_session() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    let sid = SessionId::new();
    store.create(persistent_meta(sid)).await.unwrap();

    let err = store.remove(&sid).await.unwrap_err();
    assert!(
        matches!(err, StoreError::NotRemovable { .. }),
        "expected NotRemovable, got: {err:?}"
    );

    // A refused remove must leave the session fully intact.
    assert!(root.join(format!("{sid}.jsonl")).exists());
    assert_eq!(store.meta(&sid).await.unwrap().id, sid);
}

// ---------------------------------------------------------------------
// Guard 2: any children block removal.
// ---------------------------------------------------------------------

#[tokio::test]
async fn remove_refuses_an_ephemeral_session_with_a_non_ephemeral_child() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();

    let parent = SessionId::new();
    store.create(ephemeral_meta(parent)).await.unwrap();
    let child = SessionId::new();
    store
        .create(meta(child, Some(child_origin(parent)), false))
        .await
        .unwrap();

    let err = store.remove(&parent).await.unwrap_err();
    assert!(
        matches!(err, StoreError::NotRemovable { .. }),
        "expected NotRemovable, got: {err:?}"
    );
    assert!(root.join(format!("{parent}.jsonl")).exists());
    assert_eq!(store.meta(&parent).await.unwrap().id, parent);
}

/// The `include_ephemeral` trap (B1 guard matrix #2): `children()` hides
/// ephemeral children, so a guard implemented on `children()` would see
/// an empty list here and wrongly allow the removal, orphaning the
/// ephemeral child's file. The guard must use
/// `list(SessionFilter { parent, include_ephemeral: true, .. })`.
#[tokio::test]
async fn remove_refuses_an_ephemeral_session_with_an_ephemeral_child() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();

    let parent = SessionId::new();
    store.create(ephemeral_meta(parent)).await.unwrap();
    let child = SessionId::new();
    store
        .create(meta(child, Some(child_origin(parent)), true))
        .await
        .unwrap();

    // The trap precondition: children() really does hide the ephemeral
    // child, so only the include_ephemeral list query can see it.
    assert!(
        store.children(&parent).await.unwrap().is_empty(),
        "children() must hide the ephemeral child (this is why the guard cannot use it)"
    );

    let err = store.remove(&parent).await.unwrap_err();
    assert!(
        matches!(err, StoreError::NotRemovable { .. }),
        "an ephemeral child must still block removal, got: {err:?}"
    );
    assert!(root.join(format!("{parent}.jsonl")).exists());
    assert_eq!(store.meta(&parent).await.unwrap().id, parent);
}

// ---------------------------------------------------------------------
// Missing session: NotFound, not a guard error.
// ---------------------------------------------------------------------

#[tokio::test]
async fn remove_of_a_missing_session_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::open(dir.path().to_path_buf())
        .await
        .unwrap();
    let err = store.remove(&SessionId::new()).await.unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound { .. }),
        "expected NotFound, got: {err:?}"
    );
}

// ---------------------------------------------------------------------
// Concurrency regressions (review F-1/F-2): remove is serialized against
// fork/create/append via the store's `lifecycle` mutex plus a removal
// tombstone in the handles map (lock order documented on
// `JsonlSessionStore`). No sleeps anywhere: ordering is forced either by
// sequential calls (deterministic) or by a `tokio::sync::Barrier`
// releasing the racers simultaneously (the same spawn-based concurrency
// style as store_tests.rs).
// ---------------------------------------------------------------------

fn user_turn(text: &str) -> conway_core::log::LogRecord {
    conway_core::log::LogRecord::UserTurn {
        seq: LogSeq(0), // the store is the seq authority; this is overwritten
        ts: ts(),
        text: text.into(),
        prov: conway_core::provenance::Provenance::UserPrompt,
    }
}

/// Serialization order A (deterministic): a fork that completed before
/// remove runs MUST be seen by remove's children guard — the removal is
/// refused and both sessions survive. Before the fix this exact
/// interleaving was possible mid-remove (guard checked first, child
/// created second): the barrier race test below exercises that window.
#[tokio::test]
async fn fork_completed_before_remove_is_seen_by_the_guard() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();

    let parent = SessionId::new();
    store.create(ephemeral_meta(parent)).await.unwrap();
    store.append(&parent, user_turn("p")).await.unwrap();

    let child = SessionId::new();
    store
        .fork(&parent, LogSeq(0), meta(child, None, false))
        .await
        .unwrap();

    let err = store.remove(&parent).await.unwrap_err();
    assert!(
        matches!(err, StoreError::NotRemovable { .. }),
        "remove must see the forked child and refuse, got: {err:?}"
    );
    assert!(root.join(format!("{parent}.jsonl")).exists());
    assert!(root.join(format!("{child}.jsonl")).exists());
    let kids = store
        .list(SessionFilter {
            parent: Some(parent),
            include_ephemeral: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        kids.iter().any(|m| m.id == child),
        "the forked child must remain indexed under its parent"
    );
}

/// Serialization order B (deterministic): a fork of an already-removed
/// parent fails NotFound — the removal tombstone, not a stale warm
/// handle, answers the head check — and no child file is left behind.
#[tokio::test]
async fn fork_of_an_already_removed_parent_fails_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();

    let parent = SessionId::new();
    store.create(ephemeral_meta(parent)).await.unwrap();
    store.append(&parent, user_turn("p")).await.unwrap();
    store.remove(&parent).await.unwrap();

    let child = SessionId::new();
    let err = store
        .fork(&parent, LogSeq(0), meta(child, None, false))
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound { .. }),
        "fork of a removed parent must fail NotFound, got: {err:?}"
    );
    assert!(
        !root.join(format!("{child}.jsonl")).exists(),
        "a failed fork must not leave a child file behind"
    );
}

/// The removal tombstone (deterministic, F-1): after remove, the
/// handles-map entry is a tombstone — not a plain eviction — so every
/// later handle acquisition (append/read/head/meta, warm or cold-open)
/// fails NotFound, and a cold-open that raced the delete can never
/// resurrect a warm handle for the purged session.
///
/// Why this fails before the fix: pre-fix remove plain-evicted the map
/// entry (`handles.write().await.remove(sid)`), leaving no tombstone
/// behind — `is_removal_tombstoned` (and the no-resurrection property it
/// witnesses) did not hold.
#[tokio::test]
async fn remove_leaves_a_tombstone_that_fails_all_later_access() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::open(dir.path().to_path_buf())
        .await
        .unwrap();

    let sid = SessionId::new();
    store.create(ephemeral_meta(sid)).await.unwrap();
    store.append(&sid, user_turn("warm")).await.unwrap();
    assert!(!store.is_removal_tombstoned(&sid).await);

    store.remove(&sid).await.unwrap();

    assert!(
        store.is_removal_tombstoned(&sid).await,
        "remove must leave a tombstone, not a plain eviction"
    );
    assert!(matches!(
        store.append(&sid, user_turn("x")).await.unwrap_err(),
        StoreError::NotFound { .. }
    ));
    assert!(matches!(
        store.head(&sid).await.unwrap_err(),
        StoreError::NotFound { .. }
    ));
    assert!(matches!(
        store.meta(&sid).await.unwrap_err(),
        StoreError::NotFound { .. }
    ));

    // A later create of the same id (practically impossible — ULIDs — but
    // the tombstone must not poison it) overwrites the tombstone.
    store.create(ephemeral_meta(sid)).await.unwrap();
    assert!(!store.is_removal_tombstoned(&sid).await);
    assert_eq!(store.head(&sid).await.unwrap(), LogSeq(0));
}

/// The F-1 race: fork(parent) and remove(parent) released simultaneously
/// must always end in one of the two serialized outcomes — never in the
/// pre-fix orphan state (remove Ok, fork Ok, parent file gone, child file
/// present with a dangling `origin.parent`). Barrier-released, no sleeps.
///
/// Why this fails before the fix: pre-fix there was no `lifecycle`
/// mutex, so remove's Guard-2 list and `remove_file` could straddle
/// fork's head-check and `create` (which wrote the child file and
/// recorded the index BEFORE touching the handles lock). With both tasks
/// released on one barrier over 30 rounds — each side performing several
/// blocking-pool file ops, so the windows overlap — the orphan outcome
/// `(Ok, Ok)` occurs with overwhelming probability pre-fix and is
/// impossible post-fix (mutual exclusion admits only the two orders
/// asserted below).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fork_racing_remove_never_produces_an_orphaned_child() {
    use std::sync::Arc;
    use tokio::sync::Barrier;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = Arc::new(JsonlSessionStore::open(root.clone()).await.unwrap());

    for round in 0..30 {
        let parent = SessionId::new();
        store.create(ephemeral_meta(parent)).await.unwrap();
        store.append(&parent, user_turn("p")).await.unwrap();
        let child = SessionId::new();

        let barrier = Arc::new(Barrier::new(3));

        let fork_task = {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                store
                    .fork(&parent, LogSeq(0), meta(child, None, false))
                    .await
            })
        };
        let remove_task = {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                store.remove(&parent).await
            })
        };
        barrier.wait().await;

        let fork_res = fork_task.await.unwrap();
        let remove_res = remove_task.await.unwrap();

        let parent_file = root.join(format!("{parent}.jsonl"));
        let child_file = root.join(format!("{child}.jsonl"));
        match (fork_res, remove_res) {
            // Fork serialized first: remove must have seen the child.
            (Ok(forked), Err(StoreError::NotRemovable { .. })) => {
                assert_eq!(forked, child);
                assert!(parent_file.exists(), "round {round}: parent must survive");
                assert!(child_file.exists(), "round {round}: child must survive");
            }
            // Remove serialized first: fork must have failed NotFound and
            // left nothing behind.
            (Err(StoreError::NotFound { .. }), Ok(())) => {
                assert!(
                    !parent_file.exists(),
                    "round {round}: parent must be gone"
                );
                assert!(
                    !child_file.exists(),
                    "round {round}: failed fork must leave no child file"
                );
            }
            // The pre-fix orphan outcome, or any other inconsistency.
            (fork_res, remove_res) => panic!(
                "round {round}: inconsistent fork/remove outcome — fork: {fork_res:?}, \
                 remove: {remove_res:?}, parent file exists: {}, child file exists: {}",
                parent_file.exists(),
                child_file.exists()
            ),
        }
    }
}

/// The second F-1 manifestation: an `append` holding the warm handle Arc
/// across the removal must never report success AFTER `remove` has
/// returned — pre-fix it wrote to the unlinked inode and returned Ok, a
/// silently lost record. Post-fix the removal tombstone (no new handle
/// acquisition) plus the `removed` flag checked under the per-session
/// mutex (stale Arc holders) make every late append fail NotFound.
///
/// No sleeps: all tasks are released on one barrier. Measurement note:
/// each appender reads `remover_done` in the statement immediately after
/// its `append` returns, and between a legitimate Ok-return (which
/// post-fix is linearized before remove's mark under the session mutex)
/// and the remover's flag store lies remove's remaining `remove_file`
/// plus `persist_full` tmp-write/fsync/rename — milliseconds of real
/// file I/O — so thread scheduling alone cannot plausibly delay that
/// adjacent load long enough to produce a false `(Ok, after=true)`.
///
/// Why this fails before the fix: pre-fix remove only evicted the map
/// entry and unlinked the file; the 50 barrier-released appenders had
/// already cloned the warm handle Arc, and those still queued on the
/// per-session mutex when `remove_file` landed wrote to the dead inode
/// and returned Ok — with `remover_done` already set, producing the
/// `(Ok, after=true)` pairs asserted against below.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn append_in_flight_across_remove_never_reports_success_after_removal() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::Barrier;

    const APPENDERS: usize = 50;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(JsonlSessionStore::open(dir.path().to_path_buf()).await.unwrap());
    let sid = SessionId::new();
    store.create(ephemeral_meta(sid)).await.unwrap();
    // Warm the handle so every appender clones the same Arc.
    store.append(&sid, user_turn("warm")).await.unwrap();

    let remover_done = Arc::new(AtomicBool::new(false));
    let results: Arc<Mutex<Vec<(bool, bool)>>> = Arc::new(Mutex::new(Vec::new()));
    let barrier = Arc::new(Barrier::new(APPENDERS + 2));

    let mut tasks = Vec::new();
    for _ in 0..APPENDERS {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let remover_done = Arc::clone(&remover_done);
        let results = Arc::clone(&results);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            let ok = store.append(&sid, user_turn("x")).await.is_ok();
            let after = remover_done.load(Ordering::SeqCst);
            results.lock().unwrap().push((ok, after));
        }));
    }
    let remover = {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let remover_done = Arc::clone(&remover_done);
        tokio::spawn(async move {
            barrier.wait().await;
            store.remove(&sid).await.unwrap();
            remover_done.store(true, Ordering::SeqCst);
        })
    };
    barrier.wait().await;
    remover.await.unwrap();
    for t in tasks {
        t.await.unwrap();
    }

    let results = results.lock().unwrap();
    assert_eq!(results.len(), APPENDERS);
    let lost: Vec<_> = results.iter().filter(|(ok, after)| *ok && *after).collect();
    assert!(
        lost.is_empty(),
        "{} append(s) reported success after remove returned — silently lost records",
        lost.len()
    );
}

/// F-2 regression: `SessionIndex::remove`'s `persist_full` rewrite racing
/// a concurrent create's `record_header` append must never lose the
/// appended line (pre-fix the rename could destroy it, making the next
/// open WARN-rebuild). Post-fix both paths hold `lifecycle`, so the
/// reopen assertion below is deterministic.
///
/// Why this fails before the fix: pre-fix `remove` released the index
/// state lock before `persist_full`, and `create` never serialized with
/// `remove` at all, so a `record_header` upsert+append interleaved
/// between the snapshot and the rename had its line destroyed — the
/// reopened store then found index ids != disk ids and logged
/// "index rebuild". (The pre-fix window is narrow, so pre-fix failure is
/// probabilistic; post-fix the outcome is structurally guaranteed.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_create_and_remove_reopen_without_index_rebuild_warn() {
    use std::sync::Arc;
    use tokio::sync::Barrier;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    {
        let store = Arc::new(JsonlSessionStore::open(root.clone()).await.unwrap());
        for round in 0..20 {
            let victim = SessionId::new();
            store.create(ephemeral_meta(victim)).await.unwrap();
            let fresh = SessionId::new();

            let barrier = Arc::new(Barrier::new(3));
            let remover = {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                tokio::spawn(async move {
                    barrier.wait().await;
                    store.remove(&victim).await
                })
            };
            let creator = {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                tokio::spawn(async move {
                    barrier.wait().await;
                    store.create(persistent_meta(fresh)).await
                })
            };
            barrier.wait().await;
            remover
                .await
                .unwrap()
                .unwrap_or_else(|e| panic!("round {round}: remove failed: {e}"));
            creator
                .await
                .unwrap()
                .unwrap_or_else(|e| panic!("round {round}: create failed: {e}"));
        }
        // `store` (and its Arc clones) dropped here: Drop flush_syncs the index.
    }

    // Structural F-2 pin: compare index.jsonl's id set against the
    // session-directory scan — exactly the consistency check `try_load`
    // performs on reopen. This is the primary assertion; the WARN capture
    // below is only secondary, because `install_capture` uses the
    // thread-local `tracing::subscriber::set_default` and, on this
    // multi-thread runtime, the "index rebuild" WARN inside `open` could
    // be emitted on a worker thread with no capture subscriber installed
    // and silently dropped, letting a WARN-only assertion pass vacuously.
    let index_text = std::fs::read_to_string(root.join("index.jsonl")).unwrap();
    let mut index_ids = std::collections::HashSet::new();
    for line in index_text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let sid: SessionId = v["session"].as_str().unwrap().parse().unwrap();
        assert!(
            index_ids.insert(sid),
            "duplicate entry in index.jsonl for session {sid}"
        );
    }
    let mut disk_ids = std::collections::HashSet::new();
    for entry in std::fs::read_dir(&root).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        // Non-session `.jsonl` files (index.jsonl itself) never parse as
        // a SessionId and are skipped, mirroring `scan_session_files`.
        if let Some(sid) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<SessionId>().ok())
        {
            disk_ids.insert(sid);
        }
    }
    assert_eq!(
        index_ids, disk_ids,
        "concurrent create+remove must leave index.jsonl consistent with the session files on disk"
    );

    // Secondary: with a consistent index.jsonl the reopen must not
    // WARN-rebuild. Kept as a belt-and-braces check (see the vacuity
    // caveat above), not as the pin.
    let (log, _guard) = install_capture();
    let _store = JsonlSessionStore::open(root.clone()).await.unwrap();
    assert!(
        !log.contains("index rebuild"),
        "concurrent create+remove must leave index.jsonl consistent with disk, got: {:?}",
        log.entries.lock().unwrap()
    );
}

// ---------------------------------------------------------------------
// remove_file failure handling (review: tombstone wedge). ENOENT from
// remove_file is a successful purge outcome (the file is already gone,
// e.g. deleted externally) and must proceed to index eviction; any other
// io error must roll the removal back — restore the previous handle (or
// drop the tombstone) and clear SessionFile.removed — so the session
// stays usable AND removable rather than wedging behind a permanent
// tombstone while still listed.
// ---------------------------------------------------------------------

/// The session file was deleted externally between the last access and
/// `remove`: ENOENT from `remove_file` is the purge goal already
/// achieved, so remove must succeed and evict the index (pre-fix it
/// returned NotFound and left the session listed yet un-removable —
/// Guard-1 meta hit the tombstone — until restart).
#[tokio::test]
async fn remove_of_an_externally_deleted_file_succeeds_and_evicts_the_index() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();

    let sid = SessionId::new();
    store.create(ephemeral_meta(sid)).await.unwrap();
    store.append(&sid, user_turn("warm")).await.unwrap();

    // An outside actor deletes the file before the purge.
    std::fs::remove_file(root.join(format!("{sid}.jsonl"))).unwrap();

    store.remove(&sid).await.unwrap_or_else(|e| {
        panic!("remove must treat ENOENT from remove_file as success: {e:?}")
    });

    assert!(store.is_removal_tombstoned(&sid).await);
    assert!(matches!(
        store.meta(&sid).await.unwrap_err(),
        StoreError::NotFound { .. }
    ));
    let listed = store
        .list(SessionFilter {
            include_ephemeral: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        listed.iter().all(|m| m.id != sid),
        "externally-deleted session must still be evicted from the index"
    );

    // Reopen: the index eviction persisted, so the reopened store agrees
    // with disk (no WARN-rebuild) and the session stays gone.
    let (log, _guard) = install_capture();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    assert!(
        !log.contains("index rebuild"),
        "expected no rebuild after remove + reopen, got: {:?}",
        log.entries.lock().unwrap()
    );
    assert!(matches!(
        store.meta(&sid).await.unwrap_err(),
        StoreError::NotFound { .. }
    ));
}

/// A `remove_file` failure OTHER than ENOENT (here: EACCES from a
/// read-only session directory — the same permission-based fault
/// injection as `index_tests.rs`) must roll the removal back: the
/// tombstone is retracted, the `removed` flag cleared, and the session
/// stays usable and removable. Pre-fix the `?` early-return left the
/// tombstone published and the flag set over a surviving file + index
/// entry — the session was listed, every data path returned NotFound,
/// and a retry could never get past the tombstone.
#[tokio::test]
async fn remove_file_failure_rolls_back_the_tombstone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();

    let sid = SessionId::new();
    store.create(ephemeral_meta(sid)).await.unwrap();
    store.append(&sid, user_turn("warm")).await.unwrap();

    // Probe first: if unlink still succeeds in a read-only directory
    // (e.g. the tests run as root), this harness cannot inject the fault
    // and the test would be vacuous — restore and skip.
    let probe = root.join("perm-probe");
    std::fs::write(&probe, b"x").unwrap();
    let mut perms = std::fs::metadata(&root).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&root, perms.clone()).unwrap();
    if std::fs::remove_file(&probe).is_ok() {
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&root, perms).unwrap();
        eprintln!("skipping: unlink succeeds in a read-only directory (running as root?)");
        return;
    }

    // unlink requires write permission on the parent directory: this
    // remove_file fails EACCES, exercising the rollback path.
    let err = store.remove(&sid).await.unwrap_err();

    // Restore permissions immediately so tempdir cleanup and the
    // assertions below never depend on the injected fault.
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&root, perms).unwrap();

    assert!(
        matches!(err, StoreError::Io { .. }),
        "expected a plain Io error from the failed remove_file, got: {err:?}"
    );
    assert!(
        !store.is_removal_tombstoned(&sid).await,
        "a failed remove_file must retract the tombstone"
    );
    assert!(
        root.join(format!("{sid}.jsonl")).exists(),
        "a failed remove_file must leave the session file in place"
    );
    let listed = store
        .list(SessionFilter {
            include_ephemeral: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        listed.iter().any(|m| m.id == sid),
        "a rolled-back remove must leave the session listed"
    );

    // Usable: the removed flag was rolled back under the session mutex,
    // so the warm handle accepts appends again.
    store
        .append(&sid, user_turn("after rollback"))
        .await
        .unwrap_or_else(|e| panic!("session must be usable after the rollback: {e:?}"));

    // Removable: with the tombstone retracted, Guard-1 meta no longer
    // hits it and a retry succeeds once the fault is gone.
    store
        .remove(&sid)
        .await
        .unwrap_or_else(|e| panic!("session must be removable after the rollback: {e:?}"));
    assert!(!root.join(format!("{sid}.jsonl")).exists());
    assert!(store.is_removal_tombstoned(&sid).await);
}

/// Deterministic coverage of the `SessionFile::removed` flag path
/// (previously exercised only probabilistically by the 50-appender
/// barrier test): clone the raw handle Arc, remove the session, then
/// drive the append path through the stale Arc — the flag, set by remove
/// under the session mutex, must refuse with NotFound.
#[tokio::test]
async fn stale_handle_arc_append_after_remove_fails_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::open(dir.path().to_path_buf())
        .await
        .unwrap();

    let sid = SessionId::new();
    store.create(ephemeral_meta(sid)).await.unwrap();
    store.append(&sid, user_turn("warm")).await.unwrap();

    // Clone the raw handle Arc BEFORE the removal, the exact position an
    // in-flight append is in when remove publishes its tombstone.
    let stale = store
        .clone_handle_for_test(&sid)
        .await
        .expect("handle must be live before remove");

    store.remove(&sid).await.unwrap();

    let err = store
        .append_via_raw_handle(&sid, stale, user_turn("late"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound { .. }),
        "the removed flag must refuse a stale-Arc append with NotFound, got: {err:?}"
    );
}
