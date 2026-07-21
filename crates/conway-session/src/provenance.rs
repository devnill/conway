//! Per-turn context provenance persistence (architecture, Internal Design
//! Notes: "provenance survives process restart", decision 9; GP-10).
//!
//! This file is a skeleton only. WI-051 implements `append_context_report`,
//! `load_context_report`, and `load_all_context_reports` on top of the
//! ordinary `store.append`/`store.read` path — the report is persisted as
//! `LogRecord::ContextReportRecord`, an ordinary record with
//! `kind == "context_report"`, so it inherits fsync policy, seq assignment,
//! and crash tolerance from WI-047 with no new file format.
//!
//! ## Type re-export, not redefinition
//!
//! The WI-046 spec text sketches `ContextReport`/`ContextSegmentEntry` as
//! new types to be defined in this module (`{ turn, segments }` +
//! `{ segment, provenance, tokens_est }`). `conway-core` (authoritative;
//! complete) already defines `ContextReport`/`ContextReportEntry` in
//! `conway_core::provenance`, and `LogRecord::ContextReportRecord` (see
//! `crates/conway-core/src/log.rs`) already embeds
//! `conway_core::provenance::ContextReport` by that exact type. Defining a
//! second, differently-shaped type of the same name here would make it
//! impossible for `append_context_report` (WI-051) to construct a
//! `LogRecord` from its input, and would collide with the re-export `pub
//! use provenance::ContextReport;` `lib.rs` is specified to perform. This
//! module therefore re-exports the authoritative `conway-core` types
//! instead of redefining them. The criterion "`ContextReport`/
//! `ContextSegmentEntry` are public, `Serialize + Deserialize + Clone +
//! Debug + PartialEq`" is satisfied by the re-exported types (`conway-core`
//! derives all five on both).

pub use conway_core::provenance::{ContextReport, ContextReportEntry};
