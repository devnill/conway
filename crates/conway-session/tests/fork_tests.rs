//! Integration tests for `fork_impl` (criteria): single-line child
//! header, `ForkOrigin` population/normalization, zero-copy, O(1) parent
//! reads, range/existence errors, parent immutability, sibling forks, and
//! fsync-under-all-policies.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use proptest::prelude::*;

use conway_core::error::StoreError;
use conway_core::ids::{AgentId, LogSeq, SeqRange, SessionId};
use conway_core::log::{ForkOrigin, LogRecord, SubagentMode};
use conway_core::ports::SessionStore;
use conway_core::provenance::Provenance;
use conway_session::{FsyncPolicy, JsonlSessionStore, SessionMeta, StoreConfig};

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
        ephemeral: false,
        ask_origin: None,
        root: None,
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

async fn open_store(root: &std::path::Path) -> JsonlSessionStore {
    JsonlSessionStore::open(root.to_path_buf()).await.unwrap()
}

/// Fast store for tests that append many records: `Never` skips the
/// fsync-policy machinery entirely so bulk appends aren't gated on it.
async fn open_fast_store(root: &std::path::Path) -> JsonlSessionStore {
    JsonlSessionStore::open_with(
        root.to_path_buf(),
        StoreConfig {
            fsync: FsyncPolicy::Never,
            ..StoreConfig::default()
        },
    )
    .await
    .unwrap()
}

// ---------------------------------------------------------------------
// single-line child, O(1), zero-copy
// ---------------------------------------------------------------------

/// An earlier review found: M1: prove fork's WRITTEN BYTES decode to the expected
/// ForkOrigin — cold-reopen the store so meta() must parse the header from
/// disk rather than returning the warm in-memory copy.
#[tokio::test]
async fn fork_written_header_decodes_from_disk_after_cold_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let child;
    let parent;
    {
        let store = open_store(dir.path()).await;
        parent = SessionId::new();
        store.create(meta_for(parent)).await.unwrap();
        store.append(&parent, user_turn("one")).await.unwrap();
        store.append(&parent, user_turn("two")).await.unwrap();
        child = store
            .fork(&parent, LogSeq(1), meta_for(SessionId::new()))
            .await
            .unwrap();
    } // store dropped: every handle discarded.

    let cold = open_store(dir.path()).await;
    let meta = cold.meta(&child).await.unwrap();
    let origin = meta.origin.expect("forked child must carry origin");
    assert_eq!(origin.parent, parent);
    assert_eq!(origin.at_seq, LogSeq(1));
    assert_eq!(origin.mode, SubagentMode::Fork);
}

#[tokio::test]
async fn fork_writes_exactly_one_line_for_a_small_and_a_large_parent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = open_fast_store(&root).await;

    for n in [10u64, 10_000u64] {
        let parent = SessionId::new();
        store.create(meta_for(parent)).await.unwrap();
        for _ in 0..n {
            store.append(&parent, user_turn("x")).await.unwrap();
        }

        let child = SessionId::new();
        let returned = store
            .fork(&parent, LogSeq(n), meta_for(child))
            .await
            .unwrap();
        assert_eq!(returned, child);

        let content = tokio::fs::read_to_string(root.join(format!("{child}.jsonl")))
            .await
            .unwrap();
        assert_eq!(
            content.lines().count(),
            1,
            "fork of a {n}-record parent must write exactly 1 line"
        );
    }
}

#[tokio::test]
async fn fork_is_zero_copy_on_a_10_000_record_parent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = open_fast_store(&root).await;

    let parent = SessionId::new();
    store.create(meta_for(parent)).await.unwrap();
    for _ in 0..10_000u64 {
        store.append(&parent, user_turn("x")).await.unwrap();
    }
    let head = store.head(&parent).await.unwrap();

    let child = SessionId::new();
    store.fork(&parent, head, meta_for(child)).await.unwrap();

    let child_path = root.join(format!("{child}.jsonl"));
    let len = tokio::fs::metadata(&child_path).await.unwrap().len();
    assert!(len < 2_000, "child file should be tiny, got {len} bytes");

    let records = store.read(&child, SeqRange::full()).await.unwrap();
    assert!(
        records.is_empty(),
        "a fresh fork must have zero own records"
    );
}

#[tokio::test]
async fn fork_performs_zero_additional_parent_reads_when_the_handle_is_warm() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = open_fast_store(&root).await;

    for n in [10u64, 10_000u64] {
        let parent = SessionId::new();
        store.create(meta_for(parent)).await.unwrap();
        for _ in 0..n {
            store.append(&parent, user_turn("x")).await.unwrap();
        }
        // `create` + `append` never cold-open — the parent handle has been
        // warm (in-memory) the entire time, which is the store's real
        // runtime usage pattern the O(1) contract targets.
        let before = store.lines_scanned();

        let child = SessionId::new();
        store
            .fork(&parent, LogSeq(n), meta_for(child))
            .await
            .unwrap();

        assert_eq!(
            store.lines_scanned(),
            before,
            "fork must read 0 parent lines when the parent handle is already warm (n={n})"
        );
    }
    assert_eq!(
        store.lines_scanned(),
        0,
        "this test never cold-opens any session, so the store-wide scan counter must stay at 0"
    );
}

// ---------------------------------------------------------------------
// ForkOrigin population and normalization
// ---------------------------------------------------------------------

#[tokio::test]
async fn fork_fills_origin_from_arguments_and_defaults_mode_to_fork_when_meta_origin_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let parent = SessionId::new();
    store.create(meta_for(parent)).await.unwrap();
    store.append(&parent, user_turn("a")).await.unwrap();
    store.append(&parent, user_turn("b")).await.unwrap();

    let child = SessionId::new();
    let mut child_meta = meta_for(child);
    child_meta.origin = None;
    store.fork(&parent, LogSeq(1), child_meta).await.unwrap();

    let got = store.meta(&child).await.unwrap();
    assert_eq!(
        got.origin,
        Some(ForkOrigin {
            parent,
            at_seq: LogSeq(1),
            mode: SubagentMode::Fork,
        })
    );
}

#[tokio::test]
async fn fork_preserves_caller_mode_but_normalizes_parent_and_at_seq() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let parent = SessionId::new();
    store.create(meta_for(parent)).await.unwrap();
    store.append(&parent, user_turn("a")).await.unwrap();

    let child = SessionId::new();
    let mut child_meta = meta_for(child);
    // A stale/bogus origin: fork must overwrite `parent`/`at_seq` with its
    // own arguments while preserving the caller-supplied `mode`.
    child_meta.origin = Some(ForkOrigin {
        parent: SessionId::new(),
        at_seq: LogSeq(999),
        mode: SubagentMode::Spawn,
    });
    store.fork(&parent, LogSeq(1), child_meta).await.unwrap();

    let got = store.meta(&child).await.unwrap();
    assert_eq!(
        got.origin,
        Some(ForkOrigin {
            parent,
            at_seq: LogSeq(1),
            mode: SubagentMode::Spawn,
        })
    );
}

// ---------------------------------------------------------------------
// error paths: no file created
// ---------------------------------------------------------------------

#[tokio::test]
async fn fork_at_greater_than_head_returns_seq_out_of_range_and_creates_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = open_store(&root).await;
    let parent = SessionId::new();
    store.create(meta_for(parent)).await.unwrap();
    store.append(&parent, user_turn("a")).await.unwrap();
    let head = store.head(&parent).await.unwrap();

    let child = SessionId::new();
    let requested = LogSeq(head.0 + 1);
    let err = store
        .fork(&parent, requested, meta_for(child))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        StoreError::SeqOutOfRange { requested: r, head: h } if r == requested && h == head
    ));

    assert!(!root.join(format!("{child}.jsonl")).exists());
}

#[tokio::test]
async fn fork_nonexistent_parent_returns_not_found_and_creates_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = open_store(&root).await;
    let parent = SessionId::new(); // never created
    let child = SessionId::new();

    let err = store
        .fork(&parent, LogSeq(0), meta_for(child))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound { session } if session == parent));

    assert!(!root.join(format!("{child}.jsonl")).exists());
}

// ---------------------------------------------------------------------
// siblings
// ---------------------------------------------------------------------

#[tokio::test]
async fn fork_ten_siblings_at_the_same_point_are_distinct_and_parent_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = open_store(&root).await;
    let parent = SessionId::new();
    store.create(meta_for(parent)).await.unwrap();
    for _ in 0..5 {
        store.append(&parent, user_turn("x")).await.unwrap();
    }
    let parent_path = root.join(format!("{parent}.jsonl"));
    let before = tokio::fs::read(&parent_path).await.unwrap();

    let mut children = std::collections::HashSet::new();
    for _ in 0..10 {
        let child = SessionId::new();
        let returned = store
            .fork(&parent, LogSeq(3), meta_for(child))
            .await
            .unwrap();
        assert_eq!(returned, child);
        assert!(children.insert(child), "fork ids must be distinct");
        assert!(root.join(format!("{child}.jsonl")).is_file());
    }
    assert_eq!(
        children.len(),
        10,
        "10 forks must produce 10 distinct files"
    );

    let after = tokio::fs::read(&parent_path).await.unwrap();
    assert_eq!(
        before, after,
        "parent must be byte-identical after 10 sibling forks"
    );
}

// ---------------------------------------------------------------------
// fsync
// ---------------------------------------------------------------------

#[tokio::test]
async fn fork_fsyncs_the_child_header_before_returning_under_all_fsync_policies() {
    for policy in [
        FsyncPolicy::Always,
        FsyncPolicy::Never,
        FsyncPolicy::Interval(std::time::Duration::from_secs(3600)),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonlSessionStore::open_with(
            dir.path().to_path_buf(),
            StoreConfig {
                fsync: policy,
                lru_capacity: 8,
            },
        )
        .await
        .unwrap();
        let parent = SessionId::new();
        store.create(meta_for(parent)).await.unwrap();
        store.append(&parent, user_turn("a")).await.unwrap();

        let before = store.fsync_count();
        let child = SessionId::new();
        store
            .fork(&parent, LogSeq(1), meta_for(child))
            .await
            .unwrap();

        assert!(
            store.fsync_count() > before,
            "fork's header write must fsync regardless of policy {policy:?}"
        );
    }
}

// ---------------------------------------------------------------------
// Property test: parent bytes are unaffected by fork, and the parent's
// pre-fork bytes remain an unchanged prefix even after further appends
// (≥128 cases). The stronger `TranscriptResolver`-level invariant is
// the.
// ---------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn parent_is_immutable_across_fork_and_subsequent_appends(
        n_before in 0usize..30,
        n_after in 0usize..10,
        at_offset in 0usize..=30,
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().to_path_buf();
            let store = JsonlSessionStore::open_with(
                root.clone(),
                StoreConfig {
                    fsync: FsyncPolicy::Never,
                    ..StoreConfig::default()
                },
            )
            .await
            .unwrap();

            let parent = SessionId::new();
            store.create(meta_for(parent)).await.unwrap();
            for _ in 0..n_before {
                store.append(&parent, user_turn("x")).await.unwrap();
            }
            let head = store.head(&parent).await.unwrap();
            let at = LogSeq((at_offset as u64).min(head.0));

            let parent_path = root.join(format!("{parent}.jsonl"));
            let before_fork = tokio::fs::read(&parent_path).await.unwrap();

            let child = SessionId::new();
            store.fork(&parent, at, meta_for(child)).await.unwrap();

            let after_fork = tokio::fs::read(&parent_path).await.unwrap();
            assert_eq!(before_fork, after_fork, "fork must not modify parent bytes");

            for _ in 0..n_after {
                store.append(&parent, user_turn("y")).await.unwrap();
            }

            let after_more_appends = tokio::fs::read(&parent_path).await.unwrap();
            assert!(
                after_more_appends.starts_with(after_fork.as_slice()),
                "appends to the parent after a fork must not rewrite the pre-append prefix"
            );
        });
    }
}
