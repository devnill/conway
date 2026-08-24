//! Runs `conway.trim` against a SYNTHETIC multi-turn session log (board item
//! `01M0EMAC4CCDQ8QJYM21RXPKRY`) and answers the item's three questions
//! from the record shapes that answer depends on.
//!
//! `tests/fixtures/synthetic_session.jsonl` is generated content, not a copy
//! of any real session: a 35-assistant-turn log built directly from the
//! `LogRecord`/`ContentBlock`/`ContextReport` types (see the crate's own
//! history for the generator), preserving the three shapes the item's
//! findings depend on --
//!
//!   1. 34 of the 35 `Assistant` records carry `Text`/`Thinking` ALONGSIDE a
//!      `ToolUse` block, so "drop the tool result" can only be expressed as
//!      "drop the call-and-result round-trip together" (finding #1).
//!   2. Every tool-using turn interleaves a `ContextReportRecord` BETWEEN
//!      the `ToolUse` and its answering `ToolResultRecord` -- the exact
//!      shape that caught the curator's turn-counter bug (see the crate's
//!      module doc). Without this shape that regression is unguarded.
//!   3. Per-turn `usage.cache_read_tokens` climbs monotonically (0, 4_000,
//!      8_000, ... 136_000), so the cache-cost assertion below still means
//!      something, even though the numbers are invented rather than a real
//!      backend's own persisted figures.
//!
//! It is loaded into a fresh in-memory [`conway_testkit::FakeStore`] rather
//! than read from any real session directory.
use std::sync::Arc;

use conway::plugin::{CurateCtx, SeqRange, TranscriptResolver};
use conway::{
    Derivation, DivergenceKind, LogRecord, NodeProvenance, NodeStamp, PathNode, RecordRef,
    Selector, SessionId, SessionStore, ValidatedPath,
};
use conway_plugin_trim::TrimOldToolResults;

const FIXTURE: &str = include_str!("fixtures/synthetic_session.jsonl");

/// Parses the fixture, replays it into a fresh [`FakeStore`], and returns
/// the session id plus the full record list (header excluded) in order.
async fn load_fixture() -> (SessionId, Arc<dyn SessionStore>, Vec<LogRecord>) {
    let store: Arc<dyn SessionStore> = Arc::new(conway_testkit::FakeStore::new());
    let mut sid: Option<SessionId> = None;
    for line in FIXTURE.lines().filter(|l| !l.trim().is_empty()) {
        let value: serde_json::Value = serde_json::from_str(line).expect("valid json line");
        if value.get("kind").and_then(|k| k.as_str()) == Some("header") {
            let meta: conway::SessionMeta = serde_json::from_value(value).expect("valid header");
            sid = Some(store.create(meta).await.expect("create"));
            continue;
        }
        let rec: LogRecord = serde_json::from_value(value).expect("valid record");
        store
            .append(sid.as_ref().expect("header seen first"), rec)
            .await
            .expect("append");
    }
    let sid = sid.expect("fixture has a header line");
    let records = store.read(&sid, SeqRange::full()).await.expect("read back");
    (sid, store, records)
}

/// Builds a `ValidatedPath` over the whole session, in log order -- exactly
/// what a non-forked session's default path looks like (`Head` on the first
/// record, `Own` on every one after; DESIGN §2.2). No prefix chain: this
/// fixture's header has `origin: None` (a root session, never forked), so
/// this is an honest reconstruction, not a simplification that hides a real
/// prefix.
fn build_path(sid: SessionId, records: &[LogRecord]) -> ValidatedPath {
    let now = chrono::Utc::now();
    let nodes: Vec<(PathNode, Arc<LogRecord>)> = records
        .iter()
        .enumerate()
        .map(|(i, rec)| {
            let seq = rec.seq().expect("non-header record has a seq");
            let node = PathNode {
                record: RecordRef { session: sid, seq },
                stamp: if i == 0 {
                    NodeStamp::Head
                } else {
                    NodeStamp::Own
                },
                prov: NodeProvenance {
                    selected_by: Selector::DefaultRule,
                    at: now,
                },
            };
            (node, Arc::new(rec.clone()))
        })
        .collect();
    ValidatedPath::default_path(nodes)
}

fn assistant_turn_count(records: &[LogRecord]) -> u32 {
    records
        .iter()
        .filter(|r| matches!(r, LogRecord::Assistant { .. }))
        .count() as u32
}

/// Per-`Assistant`-record `usage.cache_read_tokens`, in path order -- pulled
/// straight from the fixture's own JSON, not recomputed or modeled.
fn synthetic_cache_read_tokens() -> Vec<u64> {
    FIXTURE
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("kind").and_then(|k| k.as_str()) == Some("assistant"))
        .map(|v| {
            v["usage"]["cache_read_tokens"]
                .as_u64()
                .expect("cache_read_tokens present")
        })
        .collect()
}

#[tokio::test]
async fn trims_old_tool_round_trips_on_a_synthetic_session_without_orphaning_anything() {
    let (sid, store, records) = load_fixture().await;
    assert_eq!(records.len(), 104, "fixture record count, header excluded");
    let base = build_path(sid, &records);
    let total_turns = assistant_turn_count(&records);
    assert_eq!(total_turns, 35);

    let curator = TrimOldToolResults::new(5);
    let ctx = CurateCtx {
        agent_id: conway::AgentId::new(),
        session_id: sid,
        turn: total_turns,
        model: None,
        store: store.clone(),
        resolver: Arc::new(TranscriptResolver::new(64)),
    };

    let outcome = {
        use conway::plugin::Curator as _;
        curator.curate(&ctx, &base).await
    };

    let derivation = match outcome {
        conway::plugin::CurateOutcome::Derived(d) => d,
        other => panic!("expected a real derivation over a synthetic session, got {other:?}"),
    };
    let Derivation { path, cost } = derivation;

    // Q1/Q3: omission, never reordering -- and the derivation is coherent
    // (the harness's own three-rule validator ran and did not refuse), so
    // dropping call+result together as one unit never orphaned anything
    // even with a `ContextReportRecord` sitting between every call and its
    // own answering result.
    assert_eq!(cost.divergence_kind, DivergenceKind::Omission);
    assert!(
        path.nodes().count() < records.len(),
        "something was dropped"
    );

    // Q2: the structural promise (first divergence falls early -- the
    // "expensive" direction PHILOSOPHY.md §4 names) cross-referenced against
    // this fixture's own invented-but-plausible cache figures.
    let cache = synthetic_cache_read_tokens();
    // `shared_prefix_nodes` counts leading nodes untouched by the omission;
    // translate that into "how many turns are unambiguously fresh" so it can
    // be read against `cache`'s per-turn index.
    let sp = cost.shared_prefix_nodes as usize;
    let untouched_turns = records[..sp.min(records.len())]
        .iter()
        .filter(|r| matches!(r, LogRecord::Assistant { .. }))
        .count();
    // With `keep_turns = 5` at `turn = 35`, everything before turn 30 is a
    // drop candidate, and the very first record after the initial user turn
    // is already a drop candidate -- so the shared prefix cannot extend past
    // that first round-trip.
    assert!(
        untouched_turns < 5,
        "expected the shared prefix to end early (the expensive direction), got {untouched_turns} untouched turns"
    );
    let voided_cache_tokens = cache[total_turns as usize - 1];
    eprintln!(
        "conway.trim on a synthetic 35-turn session: shared_prefix_nodes={sp} \
         ({untouched_turns} of {total_turns} turns untouched); the NEXT call in \
         this session would have reused {voided_cache_tokens} cached tokens -- \
         all of which a curator running this derivation would have voided, \
         because the divergence falls inside the first {untouched_turns} turns \
         rather than near the tail."
    );
    assert!(
        voided_cache_tokens > 100_000,
        "this fixture's own invented cache figures should show six figures \
         of accumulated cache by the final turn, got {voided_cache_tokens}"
    );
}

#[tokio::test]
async fn a_short_window_still_yields_unchanged_when_nothing_is_old_enough() {
    let (sid, store, records) = load_fixture().await;
    let base = build_path(sid, &records);
    let curator = TrimOldToolResults::new(1000); // window wider than the session
    let ctx = CurateCtx {
        agent_id: conway::AgentId::new(),
        session_id: sid,
        turn: assistant_turn_count(&records),
        model: None,
        store,
        resolver: Arc::new(TranscriptResolver::new(64)),
    };
    use conway::plugin::Curator as _;
    let outcome = curator.curate(&ctx, &base).await;
    assert!(matches!(outcome, conway::plugin::CurateOutcome::Unchanged));
}
