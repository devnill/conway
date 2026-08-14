//! Per-turn context provenance persistence (architecture, Internal Design
//! Notes: "provenance survives process restart", decision 9).
//!
//! implements `append_context_report`, `load_context_report`, and
//! `load_all_context_reports` on top of the ordinary
//! `store.append`/`store.read` path — the report is persisted as
//! `LogRecord::ContextReportRecord`, an ordinary record with
//! `kind == "context_report"`, so it inherits fsync policy, seq assignment,
//! and crash tolerance from an earlier item with no new file format.
//!
//! ## Type re-export, not redefinition
//!
//! The an earlier item spec text sketches `ContextReport`/`ContextSegmentEntry` as
//! new types to be defined in this module (`{ turn, segments }` +
//! `{ segment, provenance, tokens_est }`). `conway-core` (authoritative;
//! complete) already defines `ContextReport`/`ContextReportEntry` in
//! `conway_core::provenance`, and `LogRecord::ContextReportRecord` (see
//! `crates/conway-core/src/log.rs`) already embeds
//! `conway_core::provenance::ContextReport` by that exact type. Defining a
//! second, differently-shaped type of the same name here would make it
//! impossible for `append_context_report` to construct a
//! `LogRecord` from its input, and would collide with the re-export `pub
//! use provenance::ContextReport;` `lib.rs` is specified to perform. This
//! module therefore re-exports the authoritative `conway-core` types
//! instead of redefining them. The criterion "`ContextReport`/
//! `ContextSegmentEntry` are public, `Serialize + Deserialize + Clone +
//! Debug + PartialEq`" is satisfied by the re-exported types (`conway-core`
//! derives all five on both).
//!
//! ## Store parameter: generic over `SessionStore`, not `JsonlSessionStore`
//!
//! `append_context_report`/`load_context_report`/`load_all_context_reports`
//! take `&S where S: SessionStore + ?Sized`, the same pattern
//! `TranscriptResolver::resolve` uses — object-safe, so a `&dyn
//! SessionStore` satisfies the bound too, and `&JsonlSessionStore` costs
//! callers no extra syntax.

use chrono::Utc;

use conway_core::error::StoreError;
use conway_core::ids::{LogSeq, SeqRange, SessionId};
use conway_core::log::LogRecord;
use conway_core::ports::SessionStore;

pub use conway_core::provenance::{ContextReport, ContextReportEntry};

/// Appends `report` as an ordinary `LogRecord::ContextReportRecord` through
/// the same `store.append` path every other record uses — this function
/// adds no new file format and no new durability rule, inheriting seq
/// assignment, fsync policy, and crash tolerance from an earlier item. It exists as
/// a typed convenience so callers do not hand-build the record.
///
/// Callers append the report *after* the turn's assistant record (
/// spec) so a truncated trailing line can lose a report without losing the
/// turn it describes — this function does not enforce that ordering, it is
/// a caller discipline.
pub async fn append_context_report<S>(
    store: &S,
    sid: &SessionId,
    report: &ContextReport,
) -> Result<LogSeq, StoreError>
where
    S: SessionStore + ?Sized,
{
    let rec = LogRecord::ContextReportRecord {
        seq: LogSeq::ZERO, // overwritten by `append`; the store is the seq authority.
        ts: Utc::now(),
        report: report.clone(),
    };
    store.append(sid, rec).await
}

/// The report for `turn`, or `Ok(None)` if no report was ever appended for
/// it. If multiple reports share a turn, the highest-seq one wins (`read`
/// returns ascending seq order, so this is simply the last match).
pub async fn load_context_report<S>(
    store: &S,
    sid: &SessionId,
    turn: u32,
) -> Result<Option<ContextReport>, StoreError>
where
    S: SessionStore + ?Sized,
{
    let reports = load_all_context_reports(store, sid).await?;
    Ok(reports.into_iter().rfind(|r| r.turn == turn))
}

/// Every context report persisted for `sid`, in ascending seq order. A
/// linear scan over the full transcript, filtering on `kind ==
/// "context_report"` — the only interpretation of record contents this
/// module performs; `segments`, `provenance`, and `tokens_est` stay opaque
/// payload. Acceptable cost: reports are read on demand by inspection
/// APIs, never on the agent-loop hot path.
pub async fn load_all_context_reports<S>(
    store: &S,
    sid: &SessionId,
) -> Result<Vec<ContextReport>, StoreError>
where
    S: SessionStore + ?Sized,
{
    let records = store.read(sid, SeqRange::full()).await?;
    Ok(records
        .into_iter()
        .filter_map(|rec| match rec {
            LogRecord::ContextReportRecord { report, .. } => Some(report),
            _ => None,
        })
        .collect())
}
