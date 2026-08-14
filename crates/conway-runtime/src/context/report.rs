//! `ContextReport` persistence and historical-turn lookup.
//!
//! Thin wrapper over `conway_session::provenance`'s already-committed
//! `append_context_report`/`load_context_report`/
//! `load_all_context_reports`. This module adds no new file format and no
//! new record kind, and does not redefine `ContextReport`/
//! `ContextReportEntry` -- `conway_core::provenance` is authoritative and
//! `conway_session::provenance` already re-exports those exact types (see
//! that module's own doc). This module exists only to translate
//! `StoreError` into `conway_core::error::RuntimeError` (the only error
//! type this crate's public surface returns) and to add the one behavior
//! `conway-session` does not: a typed "turn out of range" error.
//!
//! ## Reconciliation: `tokenizer`, not `estimator`
//!
//! The an earlier item criterion prose reads "each report carries `estimator:
//! heuristic-chars4`". `conway_core::provenance::ContextReport` has no
//! `estimator` field -- only `tokenizer: String`, whose own doc comment
//! states plainly: " an earlier item asserts this field (there is no separate
//! estimator field)". `context/builder.rs`'s `TOKEN_ESTIMATOR` constant
//! already resolved this identically. This item's tests assert
//! `report.tokenizer == "heuristic-chars4"`; no `estimator` field is added
//! anywhere.
//!
//! ## Reconciliation: out-of-range turn error
//!
//! `conway_core::error::RuntimeError` is `#[non_exhaustive]` and has no
//! dedicated "turn out of range" variant, and `crates/conway-core/src/
//! error.rs` is out of this item's file scope. Following the same
//! "closest fit" convention `tree.rs`'s `already_attached` established for
//! the identical situation (a gap this crate cannot add a variant for),
//! `turn_out_of_range` maps to `RuntimeError::Tool(ToolError::Internal {
//! detail })`, with `detail` naming the valid turn range in text --
//! satisfying the criterion's "typed error naming the valid range" without
//! adding a variant to a crate outside this item's scope. (`subagent.rs`'s
//! `invalid_spec` was ALSO once this same "closest fit" fallback, but a
//! later item added `RuntimeError::InvalidSpec` and moved it off `Internal`
//! -- see that module's own doc; "turn out of range" is a different gap,
//! not addressed by that variant, and stays on this fallback.)

use conway_core::error::{RuntimeError, ToolError};
use conway_core::ids::{AgentId, LogSeq, SessionId};
use conway_core::ports::SessionStore;
use conway_core::provenance::ContextReport;

/// Persists `report` as an ordinary `LogRecord::ContextReportRecord`
/// (`conway_session::provenance::append_context_report`), inheriting
/// that helper's fsync policy, seq assignment, and crash tolerance. Callers
/// (`agent_loop.rs`) must call this AFTER the turn's assistant record has
/// already been durably appended -- this function does not enforce that
/// ordering, the same caller-discipline contract the helper it wraps
/// already documents.
pub async fn persist(
    store: &dyn SessionStore,
    sid: &SessionId,
    report: &ContextReport,
) -> Result<LogSeq, RuntimeError> {
    conway_session::provenance::append_context_report(store, sid, report)
        .await
        .map_err(RuntimeError::Store)
}

/// The report persisted for `agent`'s `turn` in session `sid`, or a typed
/// error naming the valid turn range if `turn` was never persisted.
pub async fn persisted_at_turn(
    store: &dyn SessionStore,
    agent: AgentId,
    sid: &SessionId,
    turn: u32,
) -> Result<ContextReport, RuntimeError> {
    if let Some(report) = conway_session::provenance::load_context_report(store, sid, turn)
        .await
        .map_err(RuntimeError::Store)?
    {
        return Ok(report);
    }

    let all = conway_session::provenance::load_all_context_reports(store, sid)
        .await
        .map_err(RuntimeError::Store)?;
    Err(turn_out_of_range(agent, turn, &all))
}

fn turn_out_of_range(agent: AgentId, turn: u32, reports: &[ContextReport]) -> RuntimeError {
    let detail = match (
        reports.iter().map(|r| r.turn).min(),
        reports.iter().map(|r| r.turn).max(),
    ) {
        (Some(min), Some(max)) => {
            format!("agent {agent}: turn {turn} out of range; valid turns are {min}..={max}")
        }
        _ => format!(
            "agent {agent}: turn {turn} out of range; no turns have been persisted for this agent yet"
        ),
    };
    RuntimeError::Tool(ToolError::Internal { detail })
}
