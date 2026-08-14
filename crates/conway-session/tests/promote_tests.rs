//! Integration tests for `SessionStore::set_ephemeral` (B3): the guarded,
//! one-way ephemeral→persistent header flip behind the facade's promote.
//! Covers the guard matrix (demotion refused; non-ephemeral no-op refused;
//! unknown session `NotFound`), the crash-atomic on-disk header rewrite
//! with VERBATIM record-byte preservation (), the index upsert +
//! `persist_full` (both the in-memory `list` view and the on-disk
//! `index.jsonl`), warm- and cold-handle paths, reopen persistence, and
//! the purge interplay (a promoted session is no longer removable).

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

fn meta(id: SessionId, ephemeral: bool) -> SessionMeta {
    SessionMeta {
        id,
        agent_id: AgentId::new(),
        origin: None,
        agent_def: None,
        role: None,
        created: ts(),
        cwd: PathBuf::from("/tmp/project"),
        labels: vec![],
        ephemeral,
        ask_origin: None,
        root: None,
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

/// Raw file bytes for `sid` under `root`.
fn raw_session_bytes(root: &std::path::Path, sid: SessionId) -> Vec<u8> {
    std::fs::read(root.join(format!("{sid}.jsonl"))).unwrap()
}

/// Everything after line 0 (the record bytes, verbatim).
fn record_bytes(raw: &[u8]) -> &[u8] {
    let nl = raw.iter().position(|b| *b == b'\n').unwrap() + 1;
    &raw[nl..]
}

// ---------------------------------------------------------------------
// Happy path: the flip is durable, index-correct, record-preserving, and
// survives reopen — and it closes the purge path.
// ---------------------------------------------------------------------

#[tokio::test]
async fn set_ephemeral_flips_header_on_disk_and_index_preserves_records_and_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();

    let records_before;
    {
        let store = JsonlSessionStore::open(root.clone()).await.unwrap();
        store.create(meta(sid, true)).await.unwrap();
        // Append two records first, so the handle is WARM and the rewrite
        // must preserve real record bytes.
        store
            .append(&sid, turn(0, "scratch question"))
            .await
            .unwrap();
        store.append(&sid, turn(1, "scratch answer")).await.unwrap();
        records_before = raw_session_bytes(&root, sid);

        store.set_ephemeral(&sid, false).await.unwrap();

        // In-memory meta reflects the flip immediately.
        let m = store.meta(&sid).await.unwrap();
        assert!(!m.ephemeral, "warm-handle meta must show ephemeral: false");

        // The default (exclude-ephemeral) listing now surfaces the session
        // — the in-memory index upsert landed.
        let listing = store.list(SessionFilter::default()).await.unwrap();
        assert!(
            listing.iter().any(|m| m.id == sid && !m.ephemeral),
            "default listing must include the promoted session, got: {listing:?}"
        );

        // The on-disk header flipped; the record bytes are VERBATIM
        // (: promotion rewrites nothing except the flag).
        let raw_after = raw_session_bytes(&root, sid);
        let header_line = String::from_utf8(
            raw_after[..raw_after.iter().position(|b| *b == b'\n').unwrap()].to_vec(),
        )
        .unwrap();
        assert!(
            header_line.contains("\"ephemeral\":false"),
            "line 0 must carry the flipped flag, got: {header_line}"
        );
        assert_eq!(
            record_bytes(&raw_after),
            record_bytes(&records_before),
            "record bytes must be preserved verbatim across the rewrite"
        );
        // No stray temp file remains after a successful rewrite.
        assert!(
            !root.join(format!("{sid}.promote.tmp")).exists(),
            "the temp file must be consumed by the rename"
        );

        // The on-disk index was rewritten too (persist_full), not just the
        // in-memory one.
        let index_raw = std::fs::read_to_string(root.join("index.jsonl")).unwrap();
        let line = index_raw
            .lines()
            .find(|l| l.contains(&sid.to_string()))
            .expect("index must contain the session");
        assert!(
            line.contains("\"ephemeral\":false"),
            "index.jsonl must project the flipped flag, got: {line}"
        );

        // The flip also closes the purge path: a promoted session is no
        // longer ephemeral, so `remove`'s Guard 1 refuses.
        let err = store.remove(&sid).await.unwrap_err();
        assert!(
            matches!(err, StoreError::NotRemovable { session, .. } if session == sid),
            "a promoted session must no longer be removable, got: {err:?}"
        );

        // The post-rename handle swap: an append through THIS SAME store
        // instance (warm handle) must land in the renamed file, not the
        // detached old inode — without the `sf.file` swap this record would
        // be silently lost while `append` reported success.
        store
            .append(&sid, turn(2, "post-promote turn"))
            .await
            .unwrap();
        let head = store.head(&sid).await.unwrap();
        assert_eq!(head.0, 3);
        let raw_final = raw_session_bytes(&root, sid);
        assert!(
            String::from_utf8_lossy(&raw_final).contains("post-promote turn"),
            "a post-promote append must be durable in the renamed file"
        );

        // Store drops here; reopen below proves durability.
    }

    // Reopen: the persisted header (not any in-memory state) drives meta.
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    let m = store.meta(&sid).await.unwrap();
    assert!(
        !m.ephemeral,
        "a reopened store must show the persisted non-ephemeral meta"
    );
    let listing = store.list(SessionFilter::default()).await.unwrap();
    assert!(
        listing.iter().any(|m| m.id == sid),
        "the reopened store's default listing must include the promoted session"
    );
    // Records survived the rewrite intact and readable — all three,
    // including the one appended through the swapped handle.
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

/// The COLD-handle path: `set_ephemeral` as the very first access to the
/// session (no prior `append`/`meta` warming the handle) must cold-open,
/// flip, and leave the session fully usable.
#[tokio::test]
async fn set_ephemeral_as_first_access_cold_opens_and_flips() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();

    {
        let store = JsonlSessionStore::open(root.clone()).await.unwrap();
        store.create(meta(sid, true)).await.unwrap();
        store.append(&sid, turn(0, "q")).await.unwrap();
    }

    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    // First access to `sid` in this store instance is the flip itself.
    store.set_ephemeral(&sid, false).await.unwrap();
    assert!(!store.meta(&sid).await.unwrap().ephemeral);
    let records = store
        .read(&sid, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
}

// ---------------------------------------------------------------------
// Guard matrix
// ---------------------------------------------------------------------

#[tokio::test]
async fn set_ephemeral_refuses_demotion_and_leaves_the_header_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();

    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    store.create(meta(sid, true)).await.unwrap();
    let before = raw_session_bytes(&root, sid);

    let err = store.set_ephemeral(&sid, true).await.unwrap_err();
    assert!(
        matches!(err, StoreError::NotPromotable { session, .. } if session == sid),
        "false -> true must be refused as NotPromotable, got: {err:?}"
    );
    assert_eq!(
        raw_session_bytes(&root, sid),
        before,
        "a refused demotion must not touch the file"
    );
    assert!(store.meta(&sid).await.unwrap().ephemeral);
}

#[tokio::test]
async fn set_ephemeral_refuses_a_non_ephemeral_noop() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();

    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    store.create(meta(sid, false)).await.unwrap();

    let err = store.set_ephemeral(&sid, false).await.unwrap_err();
    assert!(
        matches!(err, StoreError::NotPromotable { session, .. } if session == sid),
        "a no-op on a non-ephemeral session must be refused, got: {err:?}"
    );
}

#[tokio::test]
async fn set_ephemeral_unknown_session_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::open(dir.path().to_path_buf())
        .await
        .unwrap();
    let sid = SessionId::new();
    let err = store.set_ephemeral(&sid, false).await.unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound { session } if session == sid),
        "unknown session must be NotFound, got: {err:?}"
    );
    // Demotion is refused BEFORE existence is even checked (Guard 0 is
    // purely request-shaped), so it reports NotPromotable, not NotFound.
    let err = store.set_ephemeral(&sid, true).await.unwrap_err();
    assert!(
        matches!(err, StoreError::NotPromotable { .. }),
        "demotion must be refused regardless of existence, got: {err:?}"
    );
}

/// A double promote: the second flip hits Guard 1 (the session is no
/// longer ephemeral) — the store must not treat it as a silent success.
#[tokio::test]
async fn set_ephemeral_twice_refuses_the_second_flip() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();

    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    store.create(meta(sid, true)).await.unwrap();
    store.set_ephemeral(&sid, false).await.unwrap();
    let err = store.set_ephemeral(&sid, false).await.unwrap_err();
    assert!(
        matches!(err, StoreError::NotPromotable { session, .. } if session == sid),
        "a double promote must be refused, got: {err:?}"
    );
}
