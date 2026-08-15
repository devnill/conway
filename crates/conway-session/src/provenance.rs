//! `append_context_report` / `load_context_report` / `load_all_context_reports`
//! re-export.
//!
//! These helpers moved to `conway_core::provenance` (board item
//! 01KZVYVTVWRH20R6VJ6G3SWTJ6, "Stage 1a"): they are pure logic over the
//! `SessionStore` *port*, not over `JsonlSessionStore` specifically, so they
//! belong beside `ContextReport`/`ContextReportEntry` in the contract crate
//! rather than in this one adapter. This module re-exports all five names
//! unchanged so existing callers of `conway_session::provenance::*` keep
//! compiling without edits.

pub use conway_core::provenance::{
    append_context_report, load_all_context_reports, load_context_report, ContextReport,
    ContextReportEntry,
};
