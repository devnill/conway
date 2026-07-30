//! Crash-recovery integration tests for `JsonlSessionStore` (WI-047):
//! truncate-and-warn on a damaged trailing line, `Corrupt` (never
//! truncated) on a damaged header, and that a post-recovery `append`
//! continues at `last_complete_seq + 1`.
//!
//! WARN capture approach: `tracing-test` is not a dependency of this crate,
//! so these tests implement a minimal `tracing::Subscriber` (using only the
//! `tracing` crate, already a direct dependency) that records event
//! messages into a shared buffer, installed for the test's duration via
//! `tracing::subscriber::set_default`. This relies on the default
//! `#[tokio::test]` current-thread runtime keeping the whole test on one
//! OS thread, since `set_default` is thread-local.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use conway_core::error::StoreError;
use conway_core::ids::{AgentId, LogSeq, SeqRange, SessionId};
use conway_core::log::LogRecord;
use conway_core::ports::SessionStore;
use conway_core::provenance::Provenance;
use conway_session::{JsonlSessionStore, SessionMeta, SessionStatus};

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
        root: None,
    }
}

fn user_turn(text: &str) -> LogRecord {
    LogRecord::UserTurn {
        seq: LogSeq(0),
        ts: ts(),
        text: text.into(),
        prov: Provenance::UserPrompt,
    }
}

// ---------------------------------------------------------------------
// Minimal tracing WARN capture (no tracing-subscriber dependency).
// ---------------------------------------------------------------------

#[derive(Clone, Default)]
struct CaptureLog {
    entries: Arc<Mutex<Vec<String>>>,
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
// Truncated trailing record line
// ---------------------------------------------------------------------

#[tokio::test]
async fn truncated_trailing_line_is_repaired_warned_and_readable() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();

    {
        let store = JsonlSessionStore::open(root.clone()).await.unwrap();
        store.create(meta_for(sid)).await.unwrap();
        for _ in 0..3 {
            store.append(&sid, user_turn("ok")).await.unwrap();
        }
        // `store` drops here, releasing its file handle before the raw
        // corruption write below.
    }

    let path = root.join(format!("{sid}.jsonl"));
    let clean_len = std::fs::metadata(&path).unwrap().len();

    // Simulate a crash mid-write: a syntactically incomplete JSON object,
    // no trailing `\n`.
    let torn = br#"{"kind":"user_turn","seq":3,"ts":"2026-07-20T00:00:00Z","text":"in"#;
    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(torn).unwrap();
    }
    assert!(std::fs::metadata(&path).unwrap().len() > clean_len);

    let (log, _guard) = install_capture();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    let records = store.read(&sid, SeqRange::full()).await.unwrap();

    assert_eq!(records.len(), 3, "the 3 complete records survive");
    assert!(
        log.contains("truncated trailing line"),
        "expected a WARN containing \"truncated trailing line\", got: {:?}",
        log.entries.lock().unwrap()
    );

    let repaired_len = std::fs::metadata(&path).unwrap().len();
    assert_eq!(
        repaired_len, clean_len,
        "the file must be truncated back to the end of the last complete line"
    );

    // Post-recovery append continues at last_complete_seq + 1 (== 3).
    let seq = store
        .append(&sid, user_turn("post-recovery"))
        .await
        .unwrap();
    assert_eq!(seq, LogSeq(3));

    let records = store.read(&sid, SeqRange::full()).await.unwrap();
    assert_eq!(records.len(), 4, "file re-reads cleanly after repair");
    for (i, r) in records.iter().enumerate() {
        assert_eq!(r.seq(), Some(LogSeq(i as u64)));
    }
}

#[tokio::test]
async fn trailing_line_with_newline_but_malformed_json_is_also_truncated_and_warned() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();

    {
        let store = JsonlSessionStore::open(root.clone()).await.unwrap();
        store.create(meta_for(sid)).await.unwrap();
        store.append(&sid, user_turn("ok")).await.unwrap();
    }

    let path = root.join(format!("{sid}.jsonl"));
    let clean_len = std::fs::metadata(&path).unwrap().len();

    // Fully newline-terminated, but not valid JSON.
    let garbage_line = b"not valid json at all\n";
    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(garbage_line).unwrap();
    }

    let (log, _guard) = install_capture();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    let records = store.read(&sid, SeqRange::full()).await.unwrap();

    assert_eq!(records.len(), 1);
    assert!(log.contains("truncated trailing line"));
    assert_eq!(std::fs::metadata(&path).unwrap().len(), clean_len);
}

// ---------------------------------------------------------------------
// Corrupted header: Corrupt, never truncated
// ---------------------------------------------------------------------

#[tokio::test]
async fn corrupted_header_returns_corrupt_and_is_not_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();
    let path = root.join(format!("{sid}.jsonl"));

    // Incomplete JSON, no trailing newline: a header write that crashed
    // mid-flush.
    let garbage = br#"{"kind":"header","session":""#.to_vec();
    std::fs::write(&path, &garbage).unwrap();

    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    let err = store.read(&sid, SeqRange::full()).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Corrupt { session, line: 0, .. } if session == sid),
        "expected Corrupt at line 0, got: {err:?}"
    );

    let meta_err = store.meta(&sid).await.unwrap_err();
    assert!(matches!(meta_err, StoreError::Corrupt { line: 0, .. }));

    let after = std::fs::read(&path).unwrap();
    assert_eq!(after, garbage, "a corrupted header must never be truncated");
}
