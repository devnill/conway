//! Integration tests for `provenance::{append_context_report,
//! load_context_report, load_all_context_reports}` (WI-051 criteria),
//! exercised entirely through the public `JsonlSessionStore`/`SessionStore`
//! surface — the report is an ordinary `LogRecord::ContextReportRecord`
//! appended/read via `store.append`/`store.read`, with no store
//! special-casing beyond `kind` matching.

use std::path::PathBuf;

use chrono::{DateTime, Utc};

use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::ids::{AgentId, LogSeq, ModelRef, SeqRange, SessionId};
use conway_core::log::{LogRecord, SessionStatus};
use conway_core::ports::SessionStore;
use conway_core::provenance::Provenance;
use conway_session::provenance::{
    append_context_report, load_all_context_reports, load_context_report, ContextReport,
    ContextReportEntry,
};
use conway_session::{JsonlSessionStore, SessionMeta};

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

fn report(turn: u32, segments: Vec<ContextReportEntry>) -> ContextReport {
    let total_tokens_est = segments.iter().map(|s| s.tokens_est).sum();
    ContextReport {
        agent_id: AgentId::new(),
        turn,
        tokenizer: "heuristic-chars4".into(),
        segments,
        total_tokens_est,
    }
}

fn entry(provenance: Provenance, tokens_est: u32) -> ContextReportEntry {
    ContextReportEntry {
        segment: conway_core::ids::SegmentId::new(),
        provenance,
        tokens_est,
        estimated: true,
    }
}

fn assistant_record(seq: LogSeq) -> LogRecord {
    LogRecord::Assistant {
        seq,
        ts: ts(),
        content: vec![ContentBlock::Text { text: "ok".into() }],
        model: ModelRef {
            backend: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
        },
        route_reason: serde_json::json!({"AliasPrimary": {"alias": "coder"}}),
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        },
        stop: StopReason::EndTurn,
    }
}

async fn open_store(root: &std::path::Path) -> JsonlSessionStore {
    JsonlSessionStore::open(root.to_path_buf()).await.unwrap()
}

// ---------------------------------------------------------------------
// Types: public, Serialize + Deserialize + Clone + Debug + PartialEq.
// ---------------------------------------------------------------------

#[tokio::test]
async fn types_are_public_and_serde_derived() {
    let r = report(1, vec![entry(Provenance::UserPrompt, 42)]);
    let cloned = r.clone();
    let json = serde_json::to_string(&r).unwrap();
    let back: ContextReport = serde_json::from_str(&json).unwrap();
    assert_eq!(r, back);
    assert_eq!(r, cloned);
    assert!(!format!("{r:?}").is_empty());
}

// ---------------------------------------------------------------------
// append_context_report: exactly one line, kind == context_report, seq
// returned.
// ---------------------------------------------------------------------

#[tokio::test]
async fn append_writes_exactly_one_context_report_line() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let sid = store.create(meta_for(SessionId::new())).await.unwrap();

    let r = report(1, vec![entry(Provenance::UserPrompt, 10)]);
    let seq = append_context_report(&store, &sid, &r).await.unwrap();
    assert_eq!(seq, LogSeq(0));

    let records = store.read(&sid, SeqRange::full()).await.unwrap();
    assert_eq!(records.len(), 1);
    match &records[0] {
        LogRecord::ContextReportRecord {
            seq: rec_seq,
            report: rec_report,
            ..
        } => {
            assert_eq!(*rec_seq, LogSeq(0));
            assert_eq!(rec_report, &r);
        }
        other => panic!("expected ContextReportRecord, got {other:?}"),
    }
    assert_eq!(records[0].kind_str(), "context_report");
}

// ---------------------------------------------------------------------
// Round-trip across 5 distinct Provenance variants.
// ---------------------------------------------------------------------

#[tokio::test]
async fn round_trips_five_distinct_provenance_variants() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let sid = store.create(meta_for(SessionId::new())).await.unwrap();

    let segments = vec![
        entry(Provenance::UserPrompt, 5),
        entry(
            Provenance::AgentDef {
                name: "reviewer".into(),
            },
            10,
        ),
        entry(
            Provenance::Skill {
                name: "review".into(),
            },
            15,
        ),
        entry(
            Provenance::ToolRegistry {
                hash: "deadbeef".into(),
            },
            20,
        ),
        entry(
            Provenance::ToolResult {
                call_id: "tc_1".into(),
                tool: conway_core::ids::ToolName::new("read"),
            },
            25,
        ),
    ];
    let r = report(1, segments);

    append_context_report(&store, &sid, &r).await.unwrap();
    let loaded = load_context_report(&store, &sid, 1).await.unwrap();
    assert_eq!(loaded, Some(r));
}

// ---------------------------------------------------------------------
// load_context_report: highest-seq wins for a shared turn; absent turn is
// Ok(None).
// ---------------------------------------------------------------------

#[tokio::test]
async fn load_context_report_absent_turn_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let sid = store.create(meta_for(SessionId::new())).await.unwrap();

    let r = report(1, vec![entry(Provenance::UserPrompt, 1)]);
    append_context_report(&store, &sid, &r).await.unwrap();

    assert_eq!(load_context_report(&store, &sid, 2).await.unwrap(), None);
}

#[tokio::test]
async fn load_context_report_on_session_with_no_reports_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let sid = store.create(meta_for(SessionId::new())).await.unwrap();

    assert_eq!(load_context_report(&store, &sid, 1).await.unwrap(), None);
    assert!(load_all_context_reports(&store, &sid)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn load_context_report_highest_seq_wins_for_shared_turn() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let sid = store.create(meta_for(SessionId::new())).await.unwrap();

    let first = report(1, vec![entry(Provenance::UserPrompt, 1)]);
    let second = report(1, vec![entry(Provenance::UserPrompt, 2)]);
    append_context_report(&store, &sid, &first).await.unwrap();
    append_context_report(&store, &sid, &second).await.unwrap();

    let loaded = load_context_report(&store, &sid, 1).await.unwrap();
    assert_eq!(loaded, Some(second));
}

// ---------------------------------------------------------------------
// load_all_context_reports: ascending seq order.
// ---------------------------------------------------------------------

#[tokio::test]
async fn load_all_context_reports_ascending_seq_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let sid = store.create(meta_for(SessionId::new())).await.unwrap();

    let r1 = report(1, vec![entry(Provenance::UserPrompt, 1)]);
    let r2 = report(2, vec![entry(Provenance::UserPrompt, 2)]);
    let r3 = report(3, vec![entry(Provenance::UserPrompt, 3)]);
    append_context_report(&store, &sid, &r1).await.unwrap();
    append_context_report(&store, &sid, &r2).await.unwrap();
    append_context_report(&store, &sid, &r3).await.unwrap();

    let all = load_all_context_reports(&store, &sid).await.unwrap();
    assert_eq!(all, vec![r1, r2, r3]);
}

// ---------------------------------------------------------------------
// Restart: reports byte-identically recoverable after store reopen.
// ---------------------------------------------------------------------

#[tokio::test]
async fn reports_survive_store_restart() {
    let dir = tempfile::tempdir().unwrap();
    let sid = SessionId::new();
    let r1 = report(1, vec![entry(Provenance::UserPrompt, 1)]);
    let r2 = report(2, vec![entry(Provenance::UserPrompt, 2)]);

    {
        let store = open_store(dir.path()).await;
        store.create(meta_for(sid)).await.unwrap();
        append_context_report(&store, &sid, &r1).await.unwrap();
        append_context_report(&store, &sid, &r2).await.unwrap();
    }

    let path = dir.path().join(format!("{sid}.jsonl"));
    let bytes_before = std::fs::read(&path).unwrap();

    let store = open_store(dir.path()).await;
    let all = load_all_context_reports(&store, &sid).await.unwrap();
    assert_eq!(all, vec![r1, r2]);

    let bytes_after = std::fs::read(&path).unwrap();
    assert_eq!(bytes_before, bytes_after);
}

// ---------------------------------------------------------------------
// Reports interleave in read(Full) as ordinary records, seq-adjacent to
// the surrounding assistant record — no store special-casing beyond
// `kind` matching.
// ---------------------------------------------------------------------

#[tokio::test]
async fn reports_interleave_with_surrounding_records_in_seq_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let sid = store.create(meta_for(SessionId::new())).await.unwrap();

    let assistant_seq = store
        .append(&sid, assistant_record(LogSeq(0)))
        .await
        .unwrap();
    let r = report(1, vec![entry(Provenance::UserPrompt, 1)]);
    let report_seq = append_context_report(&store, &sid, &r).await.unwrap();

    assert_eq!(report_seq, assistant_seq.succ());

    let records = store.read(&sid, SeqRange::full()).await.unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].kind_str(), "assistant");
    assert_eq!(records[1].kind_str(), "context_report");
    assert_eq!(records[0].seq(), Some(assistant_seq));
    assert_eq!(records[1].seq(), Some(report_seq));
}

// ---------------------------------------------------------------------
// A zero-segment report round-trips without error.
// ---------------------------------------------------------------------

#[tokio::test]
async fn zero_segment_report_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let sid = store.create(meta_for(SessionId::new())).await.unwrap();

    let r = report(1, vec![]);
    append_context_report(&store, &sid, &r).await.unwrap();
    let loaded = load_context_report(&store, &sid, 1).await.unwrap();
    assert_eq!(loaded, Some(r));
}
