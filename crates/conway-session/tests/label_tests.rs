//! Integration tests for `SessionStore::add_label`/`remove_label` (board
//! item `01M1WVKVSDXHB68J66VZ9HE8B3`): the guarded, idempotent,
//! crash-atomic header rewrite that gives `SessionMeta.labels`/
//! `SessionFilter::label` a write path reachable after session creation.
//! Mirrors `promote_tests.rs`'s own coverage shape (both share
//! `JsonlSessionStore::rewrite_header_locked`): the on-disk header rewrite
//! with VERBATIM record-byte preservation, the index upsert +
//! `persist_full`, warm- and cold-handle paths, reopen persistence, and the
//! idempotency contract that deliberately does NOT mirror `set_ephemeral`'s
//! guard-refusal shape.

use std::path::PathBuf;

use chrono::{DateTime, Utc};

use conway_core::error::StoreError;
use conway_core::ids::{AgentId, SessionId};
use conway_core::log::LogRecord;
use conway_core::ports::SessionStore;
use conway_core::provenance::Provenance;
use conway_session::{JsonlSessionStore, SessionFilter, SessionMeta};

fn ts() -> DateTime<Utc> {
    "2026-07-20T00:00:00Z".parse().unwrap()
}

fn meta(id: SessionId) -> SessionMeta {
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
        plugin_config: conway_core::ports::PluginConfig::default(),
    }
}

fn turn(seq: u64, text: &str) -> LogRecord {
    LogRecord::UserTurn {
        seq: conway_core::ids::LogSeq(seq),
        ts: ts(),
        text: text.into(),
        prov: Provenance::UserPrompt,
    }
}

fn raw_session_bytes(root: &std::path::Path, sid: SessionId) -> Vec<u8> {
    std::fs::read(root.join(format!("{sid}.jsonl"))).unwrap()
}

fn record_bytes(raw: &[u8]) -> &[u8] {
    let nl = raw.iter().position(|b| *b == b'\n').unwrap() + 1;
    &raw[nl..]
}

// ---------------------------------------------------------------------
// Happy path: add_label is durable, index-correct, record-preserving,
// filterable via `list`, and survives reopen.
// ---------------------------------------------------------------------

#[tokio::test]
async fn add_label_flips_header_on_disk_and_index_preserves_records_and_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();

    let records_before;
    {
        let store = JsonlSessionStore::open(root.clone()).await.unwrap();
        store.create(meta(sid)).await.unwrap();
        // Append two records first, so the handle is WARM and the rewrite
        // must preserve real record bytes.
        store.append(&sid, turn(0, "question")).await.unwrap();
        store.append(&sid, turn(1, "answer")).await.unwrap();
        records_before = raw_session_bytes(&root, sid);

        store.add_label(&sid, "important").await.unwrap();

        // In-memory meta reflects the add immediately.
        let m = store.meta(&sid).await.unwrap();
        assert_eq!(m.labels, vec!["important".to_string()]);

        // `SessionFilter::label` now surfaces the session via `list`.
        let listing = store
            .list(SessionFilter {
                label: Some("important".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            listing.iter().any(|m| m.id == sid),
            "label filter must find the newly-labeled session, got: {listing:?}"
        );

        // The on-disk header carries the label; record bytes are VERBATIM.
        let raw_after = raw_session_bytes(&root, sid);
        let header_line = String::from_utf8(
            raw_after[..raw_after.iter().position(|b| *b == b'\n').unwrap()].to_vec(),
        )
        .unwrap();
        assert!(
            header_line.contains("\"important\""),
            "line 0 must carry the new label, got: {header_line}"
        );
        assert_eq!(
            record_bytes(&raw_after),
            record_bytes(&records_before),
            "record bytes must be preserved verbatim across the rewrite"
        );
        assert!(
            !root.join(format!("{sid}.header.tmp")).exists(),
            "the temp file must be consumed by the rename"
        );

        // The on-disk index was rewritten too (persist_full).
        let index_raw = std::fs::read_to_string(root.join("index.jsonl")).unwrap();
        let line = index_raw
            .lines()
            .find(|l| l.contains(&sid.to_string()))
            .expect("index must contain the session");
        assert!(
            line.contains("\"important\""),
            "index.jsonl must project the new label, got: {line}"
        );

        // Post-rewrite append through the SAME warm handle must land in the
        // renamed file, not a detached inode.
        store.append(&sid, turn(2, "post-label turn")).await.unwrap();
        let head = store.head(&sid).await.unwrap();
        assert_eq!(head.0, 3);
    }

    // Reopen: the persisted header (not any in-memory state) drives meta.
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    let m = store.meta(&sid).await.unwrap();
    assert_eq!(
        m.labels,
        vec!["important".to_string()],
        "a reopened store must show the persisted label"
    );
    let records = store
        .read(&sid, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    assert_eq!(
        records.len(),
        3,
        "all records must survive the rewrite, got: {records:?}"
    );
}

/// The COLD-handle path: `add_label` as the very first access to the
/// session (no prior `append`/`meta` warming the handle).
#[tokio::test]
async fn add_label_as_first_access_cold_opens_and_labels() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();

    {
        let store = JsonlSessionStore::open(root.clone()).await.unwrap();
        store.create(meta(sid)).await.unwrap();
        store.append(&sid, turn(0, "q")).await.unwrap();
    }

    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    store.add_label(&sid, "cold").await.unwrap();
    assert_eq!(store.meta(&sid).await.unwrap().labels, vec!["cold".to_string()]);
    let records = store
        .read(&sid, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
}

// ---------------------------------------------------------------------
// remove_label
// ---------------------------------------------------------------------

#[tokio::test]
async fn remove_label_removes_the_match_and_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();

    {
        let store = JsonlSessionStore::open(root.clone()).await.unwrap();
        store.create(meta(sid)).await.unwrap();
        store.add_label(&sid, "a").await.unwrap();
        store.add_label(&sid, "b").await.unwrap();
        assert_eq!(
            store.meta(&sid).await.unwrap().labels,
            vec!["a".to_string(), "b".to_string()]
        );

        store.remove_label(&sid, "a").await.unwrap();
        let m = store.meta(&sid).await.unwrap();
        assert_eq!(
            m.labels,
            vec!["b".to_string()],
            "only the removed label should be gone; the other must survive"
        );

        let listing = store
            .list(SessionFilter {
                label: Some("a".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            !listing.iter().any(|m| m.id == sid),
            "the removed label must no longer match via `list`"
        );
    }

    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    let m = store.meta(&sid).await.unwrap();
    assert_eq!(
        m.labels,
        vec!["b".to_string()],
        "the removal must persist across reopen"
    );
}

// ---------------------------------------------------------------------
// Idempotency -- deliberately NOT `set_ephemeral`'s guard-refusal shape.
// ---------------------------------------------------------------------

#[tokio::test]
async fn add_label_twice_is_idempotent_not_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();

    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    store.create(meta(sid)).await.unwrap();
    store.add_label(&sid, "x").await.unwrap();
    store
        .add_label(&sid, "x")
        .await
        .expect("re-adding an already-present label must succeed, not refuse");
    assert_eq!(
        store.meta(&sid).await.unwrap().labels,
        vec!["x".to_string()],
        "the label must not be duplicated"
    );
}

#[tokio::test]
async fn remove_label_never_attached_is_idempotent_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();

    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    store.create(meta(sid)).await.unwrap();
    store
        .remove_label(&sid, "never-set")
        .await
        .expect("removing an absent label must succeed, not error -- unlike NamesStore::unset");
}

// ---------------------------------------------------------------------
// Unknown session
// ---------------------------------------------------------------------

#[tokio::test]
async fn add_label_unknown_session_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::open(dir.path().to_path_buf())
        .await
        .unwrap();
    let sid = SessionId::new();
    let err = store.add_label(&sid, "x").await.unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound { session } if session == sid),
        "unknown session must be NotFound, got: {err:?}"
    );
}

#[tokio::test]
async fn remove_label_unknown_session_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::open(dir.path().to_path_buf())
        .await
        .unwrap();
    let sid = SessionId::new();
    let err = store.remove_label(&sid, "x").await.unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound { session } if session == sid),
        "unknown session must be NotFound, got: {err:?}"
    );
}
