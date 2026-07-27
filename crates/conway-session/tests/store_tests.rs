//! Integration tests for `JsonlSessionStore` (WI-047 criteria): file
//! layout, `create`/`append`/`read`/`head`/`meta`, fsync policy, and
//! per-session-lock concurrency. Crash-recovery behavior lives in
//! `tests/recovery_tests.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};

use conway_core::agent::{AgentResult, ResultStatus};
use conway_core::error::StoreError;
use conway_core::ids::{AgentId, LogSeq, SeqRange, SessionId};
use conway_core::log::LogRecord;
use conway_core::ports::SessionStore;
use conway_core::provenance::Provenance;
use conway_session::{FsyncPolicy, JsonlSessionStore, SessionMeta, SessionStatus, StoreConfig};

fn ts() -> DateTime<Utc> {
    "2026-07-20T00:00:00Z".parse().unwrap()
}

fn meta_for(id: SessionId) -> SessionMeta {
    SessionMeta {
        id,
        agent_id: AgentId::new(),
        origin: None,
        agent_def: None,
        role: None,
        created: ts(),
        cwd: PathBuf::from("/tmp/project"),
        labels: vec![],
        status: SessionStatus::Active,
        ephemeral: false,
        ask_origin: None,
    }
}

fn user_turn(text: &str) -> LogRecord {
    LogRecord::UserTurn {
        seq: LogSeq(0), // overwritten by `append`; the store is the seq authority.
        ts: ts(),
        text: text.into(),
        prov: Provenance::UserPrompt,
    }
}

fn agent_result_record() -> LogRecord {
    LogRecord::AgentResultRecord {
        seq: LogSeq(0),
        ts: ts(),
        result: AgentResult::new(
            AgentId::new(),
            SessionId::new(),
            ResultStatus::Completed,
            "done",
        ),
    }
}

async fn open_store(root: &std::path::Path) -> JsonlSessionStore {
    JsonlSessionStore::open(root.to_path_buf()).await.unwrap()
}

// ---------------------------------------------------------------------
// open
// ---------------------------------------------------------------------

/// WI-047 review S1 regression: under `Interval`, an idle session's tail
/// write is synced by the background flusher within ~the interval — it must
/// not wait for the next append.
#[tokio::test]
async fn interval_policy_flusher_syncs_idle_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::open_with(
        dir.path().to_path_buf(),
        StoreConfig {
            fsync: FsyncPolicy::Interval(std::time::Duration::from_millis(50)),
            ..StoreConfig::default()
        },
    )
    .await
    .unwrap();

    let sid = SessionId::new();
    store.create(meta_for(sid)).await.unwrap();
    store.append(&sid, user_turn("tail write")).await.unwrap();
    let after_append = store.fsync_count();

    // No further appends: the flusher alone must sync the dirty handle.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        store.fsync_count() > after_append,
        "background flusher must sync idle dirty sessions (count stayed at {after_append})"
    );
}

#[tokio::test]
async fn open_creates_root_recursively() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("nested").join("sessions");
    assert!(!root.exists());
    let _store = JsonlSessionStore::open(root.clone()).await.unwrap();
    assert!(root.is_dir());
}

#[tokio::test]
async fn open_does_not_modify_existing_session_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = open_store(&root).await;
    let sid = SessionId::new();
    store.create(meta_for(sid)).await.unwrap();
    store.append(&sid, user_turn("a")).await.unwrap();
    drop(store);

    let path = root.join(format!("{sid}.jsonl"));
    let before = tokio::fs::read(&path).await.unwrap();

    let _reopened = JsonlSessionStore::open(root.clone()).await.unwrap();

    let after = tokio::fs::read(&path).await.unwrap();
    assert_eq!(
        before, after,
        "opening an existing root must not touch session files"
    );
}

// ---------------------------------------------------------------------
// create
// ---------------------------------------------------------------------

#[tokio::test]
async fn create_writes_exactly_one_header_line_and_head_is_zero() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = open_store(&root).await;
    let sid = SessionId::new();

    let returned = store.create(meta_for(sid)).await.unwrap();
    assert_eq!(returned, sid);

    let path = root.join(format!("{sid}.jsonl"));
    let content = tokio::fs::read_to_string(&path).await.unwrap();
    assert_eq!(
        content.lines().count(),
        1,
        "create must write exactly one line"
    );

    assert_eq!(store.head(&sid).await.unwrap(), LogSeq(0));
}

#[tokio::test]
async fn create_duplicate_id_returns_already_exists() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let sid = SessionId::new();
    store.create(meta_for(sid)).await.unwrap();

    let err = store.create(meta_for(sid)).await.unwrap_err();
    assert!(matches!(err, StoreError::AlreadyExists { session } if session == sid));
}

// ---------------------------------------------------------------------
// append / read / head
// ---------------------------------------------------------------------

#[tokio::test]
async fn append_assigns_sequential_seqs_and_correct_line_count() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = open_store(&root).await;
    let sid = SessionId::new();
    store.create(meta_for(sid)).await.unwrap();

    for i in 0..5u64 {
        let seq = store.append(&sid, user_turn("x")).await.unwrap();
        assert_eq!(seq, LogSeq(i));
    }

    let path = root.join(format!("{sid}.jsonl"));
    let content = tokio::fs::read_to_string(&path).await.unwrap();
    assert_eq!(content.lines().count(), 6, "header + 5 records");
    assert_eq!(store.head(&sid).await.unwrap(), LogSeq(5));
}

#[tokio::test]
async fn read_full_excludes_header_and_returns_seq_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let sid = SessionId::new();
    store.create(meta_for(sid)).await.unwrap();
    for _ in 0..4 {
        store.append(&sid, user_turn("x")).await.unwrap();
    }

    let records = store.read(&sid, SeqRange::full()).await.unwrap();
    assert_eq!(records.len(), 4);
    for (i, r) in records.iter().enumerate() {
        assert_eq!(r.seq(), Some(LogSeq(i as u64)));
    }
}

#[tokio::test]
async fn read_sub_range_returns_exact_slice() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let sid = SessionId::new();
    store.create(meta_for(sid)).await.unwrap();
    for _ in 0..6 {
        store.append(&sid, user_turn("x")).await.unwrap();
    }

    let records = store
        .read(&sid, SeqRange::new(LogSeq(2), Some(LogSeq(5))))
        .await
        .unwrap();
    let seqs: Vec<LogSeq> = records.iter().map(|r| r.seq().unwrap()).collect();
    assert_eq!(seqs, vec![LogSeq(2), LogSeq(3), LogSeq(4)]);
}

#[tokio::test]
async fn read_range_beyond_head_returns_available_subset_without_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let sid = SessionId::new();
    store.create(meta_for(sid)).await.unwrap();
    for _ in 0..3 {
        store.append(&sid, user_turn("x")).await.unwrap();
    }

    let partial = store
        .read(&sid, SeqRange::new(LogSeq(2), Some(LogSeq(10))))
        .await
        .unwrap();
    assert_eq!(partial.len(), 1);
    assert_eq!(partial[0].seq(), Some(LogSeq(2)));

    let empty = store
        .read(&sid, SeqRange::new(LogSeq(10), Some(LogSeq(20))))
        .await
        .unwrap();
    assert!(empty.is_empty());
}

// ---------------------------------------------------------------------
// meta
// ---------------------------------------------------------------------

#[tokio::test]
async fn meta_returns_header_and_missing_file_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let sid = SessionId::new();
    let m = meta_for(sid);
    store.create(m.clone()).await.unwrap();

    let got = store.meta(&sid).await.unwrap();
    assert_eq!(got, m);

    let missing = SessionId::new();
    let err = store.meta(&missing).await.unwrap_err();
    assert!(matches!(err, StoreError::NotFound { session } if session == missing));
}

#[tokio::test]
async fn meta_cold_path_reads_header_after_reopening_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = open_store(&root).await;
    let sid = SessionId::new();
    let m = meta_for(sid);
    store.create(m.clone()).await.unwrap();
    for _ in 0..10 {
        store.append(&sid, user_turn("x")).await.unwrap();
    }
    drop(store);

    // A fresh store has no cached handle for `sid`: this exercises meta()'s
    // cold path (line-0-only read), not the in-memory fast path.
    let fresh = JsonlSessionStore::open(root).await.unwrap();
    let got = fresh.meta(&sid).await.unwrap();
    assert_eq!(got, m);
}

// ---------------------------------------------------------------------
// fsync policy
// ---------------------------------------------------------------------

#[tokio::test]
async fn fsync_always_syncs_every_append() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::open_with(
        dir.path().to_path_buf(),
        StoreConfig {
            fsync: FsyncPolicy::Always,
            lru_capacity: 8,
        },
    )
    .await
    .unwrap();
    let sid = SessionId::new();
    store.create(meta_for(sid)).await.unwrap();

    let baseline = store.fsync_count();
    for _ in 0..5 {
        store.append(&sid, user_turn("x")).await.unwrap();
    }
    assert_eq!(store.fsync_count() - baseline, 5);
}

#[tokio::test]
async fn fsync_never_syncs_zero_appends() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::open_with(
        dir.path().to_path_buf(),
        StoreConfig {
            fsync: FsyncPolicy::Never,
            lru_capacity: 8,
        },
    )
    .await
    .unwrap();
    let sid = SessionId::new();
    store.create(meta_for(sid)).await.unwrap();

    let baseline = store.fsync_count();
    for _ in 0..4 {
        store.append(&sid, user_turn("x")).await.unwrap();
    }
    store.append(&sid, agent_result_record()).await.unwrap();
    assert_eq!(
        store.fsync_count() - baseline,
        0,
        "Never must not fsync any appended record, agent_result included"
    );
}

#[tokio::test]
async fn fsync_interval_syncs_header_and_agent_result_immediately() {
    let dir = tempfile::tempdir().unwrap();
    // A long interval so the elapsed-time path never fires during the test.
    let store = JsonlSessionStore::open_with(
        dir.path().to_path_buf(),
        StoreConfig {
            fsync: FsyncPolicy::Interval(Duration::from_secs(3600)),
            lru_capacity: 8,
        },
    )
    .await
    .unwrap();
    let sid = SessionId::new();

    let before_create = store.fsync_count();
    store.create(meta_for(sid)).await.unwrap();
    assert_eq!(
        store.fsync_count() - before_create,
        1,
        "header write must always fsync"
    );

    let before_user_turn = store.fsync_count();
    store.append(&sid, user_turn("x")).await.unwrap();
    assert_eq!(
        store.fsync_count() - before_user_turn,
        0,
        "an ordinary record before the interval elapses must not fsync"
    );

    let before_agent_result = store.fsync_count();
    store.append(&sid, agent_result_record()).await.unwrap();
    assert!(
        store.fsync_count() - before_agent_result >= 1,
        "agent_result must fsync immediately under Interval"
    );
}

// ---------------------------------------------------------------------
// concurrency: per-session locks, no store-wide lock
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn ten_distinct_sessions_append_concurrently_lock_free() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(open_store(dir.path()).await);

    let mut sids = Vec::new();
    for _ in 0..10 {
        let sid = SessionId::new();
        store.create(meta_for(sid)).await.unwrap();
        sids.push(sid);
    }

    let mut tasks = Vec::new();
    for &sid in &sids {
        let store = Arc::clone(&store);
        tasks.push(tokio::spawn(async move {
            store.append(&sid, user_turn("x")).await.unwrap()
        }));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap(), LogSeq(0));
    }

    for i in 0..sids.len() {
        for j in (i + 1)..sids.len() {
            assert!(
                store.distinct_handles(&sids[i], &sids[j]).await,
                "sessions {i} and {j} must not share a write handle"
            );
        }
    }

    for &sid in &sids {
        let records = store.read(&sid, SeqRange::full()).await.unwrap();
        assert_eq!(records.len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn hundred_concurrent_appends_to_one_session_produce_contiguous_seqs() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(open_store(dir.path()).await);
    let sid = SessionId::new();
    store.create(meta_for(sid)).await.unwrap();

    let mut tasks = Vec::new();
    for _ in 0..100 {
        let store = Arc::clone(&store);
        tasks.push(tokio::spawn(async move {
            store.append(&sid, user_turn("x")).await.unwrap()
        }));
    }
    let mut seqs: Vec<u64> = Vec::new();
    for t in tasks {
        seqs.push(t.await.unwrap().0);
    }
    seqs.sort_unstable();
    assert_eq!(
        seqs,
        (0..100u64).collect::<Vec<u64>>(),
        "no duplicates, no gaps"
    );

    let records = store.read(&sid, SeqRange::full()).await.unwrap();
    assert_eq!(records.len(), 100, "no partial/interleaved lines");
    for (i, r) in records.iter().enumerate() {
        assert_eq!(r.seq(), Some(LogSeq(i as u64)));
    }
}

// ---------------------------------------------------------------------
// append-only byte-prefix invariant
// ---------------------------------------------------------------------

#[tokio::test]
async fn append_never_rewrites_existing_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = open_store(&root).await;
    let sid = SessionId::new();
    store.create(meta_for(sid)).await.unwrap();

    let path = root.join(format!("{sid}.jsonl"));
    let mut prev = tokio::fs::read(&path).await.unwrap();
    for i in 0..5 {
        store.append(&sid, user_turn("x")).await.unwrap();
        let now = tokio::fs::read(&path).await.unwrap();
        assert!(
            now.starts_with(&prev),
            "append must only extend the file (iteration {i})"
        );
        prev = now;
    }
}

// ---------------------------------------------------------------------
// Cross-process liveness sidecar (S1 follow-up to B5's sweep).
// ---------------------------------------------------------------------

/// The sidecar is NOT a session file: `SessionIndex`'s directory scan filters
/// by `.jsonl` extension, so `.conway-live` never appears in `list`/`children`
/// and never gets mistaken for a session. Sanity check that touching the
/// marker does not pollute the session listing.
#[tokio::test]
async fn live_marker_sidecar_is_not_listed_as_a_session() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = open_store(&root).await;
    let sid = SessionId::new();
    store.create(meta_for(sid)).await.unwrap();

    store.touch_live_owner(1234).await.unwrap();
    assert!(root.join(".conway-live").exists(), "marker sidecar written");

    let listed = store
        .list(conway_core::log::SessionFilter::default())
        .await
        .unwrap();
    assert_eq!(
        listed.len(),
        1,
        "the sidecar must not appear in list(); only the real session"
    );
    assert_eq!(listed[0].id, sid);
}

/// Round-trip: `touch` writes `{pid, heartbeat=now}`, `live_owner` reads it
/// back, `clear` removes it (absence reads back as `None`). A second `touch`
/// refreshes the heartbeat and may change the pid.
#[tokio::test]
async fn live_marker_round_trip_touch_read_clear() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = open_store(&root).await;

    // No marker initially.
    assert_eq!(
        store.live_owner().await.unwrap(),
        None,
        "a fresh store has no live owner"
    );

    store.touch_live_owner(42).await.unwrap();
    let owner = store.live_owner().await.unwrap().expect("marker present");
    assert_eq!(owner.pid, 42);
    let first_beat = owner.heartbeat;

    // A second touch refreshes the heartbeat (and a different pid).
    store.touch_live_owner(43).await.unwrap();
    let owner = store.live_owner().await.unwrap().expect("marker still present");
    assert_eq!(owner.pid, 43);
    assert!(
        owner.heartbeat >= first_beat,
        "heartbeat must advance on a refresh"
    );

    // Clear removes it; absence reads back as None (not an error).
    store.clear_live_owner().await.unwrap();
    assert_eq!(
        store.live_owner().await.unwrap(),
        None,
        "clear_live_owner removes the marker"
    );

    // Clearing an already-absent marker is Ok (idempotent).
    store.clear_live_owner().await.unwrap();
}

/// A corrupt or half-written sidecar decodes to `None`, not an error: "I
/// can't tell whether anyone is alive" is read as "nobody is" (reap residue,
/// the cold-start behavior), so a botched marker never wedges the sweep.
#[tokio::test]
async fn live_marker_corrupt_file_decodes_to_none() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = open_store(&root).await;

    tokio::fs::write(root.join(".conway-live"), b"not valid json")
        .await
        .unwrap();

    assert_eq!(
        store.live_owner().await.unwrap(),
        None,
        "a corrupt marker is read as no live owner, not surfaced as an error"
    );
}

/// `touch_live_owner` is crash-atomic (tmp + fsync + rename): a leftover tmp
/// file from a failed prior touch does not break a subsequent touch, and no
/// stray temp file leaks into the session listing.
#[tokio::test]
async fn live_marker_touch_is_atomic_and_overwrites_stale_tmp() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = open_store(&root).await;

    // Simulate a crashed prior touch: a stale tmp file left behind.
    tokio::fs::write(root.join(".conway-live.tmp"), b"garbage")
        .await
        .unwrap();

    store.touch_live_owner(7).await.unwrap();
    let owner = store.live_owner().await.unwrap().expect("marker written");
    assert_eq!(owner.pid, 7);
    // The tmp is gone (renamed over) and did not leak into the listing.
    assert!(!root.join(".conway-live.tmp").exists());
    assert_eq!(
        store
            .list(conway_core::log::SessionFilter::default())
            .await
            .unwrap()
            .len(),
        0,
        "no session files in this store; the marker/tmp must not be listed"
    );
}
