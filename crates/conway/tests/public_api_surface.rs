//! Locks down the crate's curated re-export list (criteria).
//!
//! The `use conway::{...};` statement below names every item `lib.rs`
//! re-exports as of this work item. Removing any name from `lib.rs` is a
//! compile failure here, not a silent surface shrink.

use conway::{
    AgentDef, AgentId, AgentResult, AgentTreeSnapshot, AttemptFailure, Backend, BreakerKind,
    BreakerSnapshot, BreakerState, Budget, CapabilitySummary, ContextHook, ContextReport,
    ConwayError, EntryOutcome, Envelope, Event, ExplainEntry, ExplainReport, HealthRegistry,
    LogRecord, LogSeq, ModelRef, PermissionDecision, PermissionDecisionKind, PermissionGate,
    PermissionRequest, PermissionScope, Plugin, Provenance, Result, ResultStatus, RoleAlias,
    Router, RoutingReason, SessionFilter, SessionId, SessionMeta, SessionStore, SubagentMode, Tool,
    ToolCategory, ToolName,
};

/// Every re-exported *type* must be nameable at this path. The function is
/// never called: the compiler type-checking its signature is the assertion.
#[allow(dead_code, clippy::too_many_arguments)]
fn assert_types_nameable(
    _: Option<AgentId>,
    _: Option<SessionId>,
    _: Option<LogRecord>,
    _: Option<LogSeq>,
    _: Option<SessionMeta>,
    _: Option<SessionFilter>,
    _: Option<AgentResult>,
    _: Option<ResultStatus>,
    _: Option<Event>,
    _: Option<Envelope>,
    _: Option<Budget>,
    _: Option<AgentDef>,
    _: Option<RoleAlias>,
    _: Option<ModelRef>,
    _: Option<ContextReport>,
    _: Option<AgentTreeSnapshot>,
    _: Option<Provenance>,
    _: Option<ConwayError>,
    _: Option<PermissionDecision>,
    _: Option<PermissionDecisionKind>,
    _: Option<PermissionRequest>,
    _: Option<PermissionScope>,
    _: Option<ToolCategory>,
    _: Option<ToolName>,
    _: Option<SubagentMode>,
    _: Option<RoutingReason>,
    _: Option<BreakerKind>,
    _: Option<BreakerState>,
    _: Option<ExplainReport>,
    _: Option<ExplainEntry>,
    _: Option<EntryOutcome>,
    _: Option<CapabilitySummary>,
    _: Option<BreakerSnapshot>,
    _: Option<AttemptFailure>,
) {
}

/// Every re-exported port *trait* must be nameable and usable as a trait
/// object at this path.
#[allow(dead_code, clippy::too_many_arguments)]
fn assert_traits_object_safe(
    _: &dyn Backend,
    _: &dyn Plugin,
    _: &dyn Tool,
    _: &dyn PermissionGate,
    _: &dyn SessionStore,
    _: &dyn Router,
    _: &dyn HealthRegistry,
    _: &dyn ContextHook,
) {
}

/// `conway::Result<T>` must alias `std::result::Result<T, ConwayError>`.
#[allow(dead_code)]
fn assert_result_alias() -> Result<()> {
    Ok(())
}

#[test]
fn public_api_surface_present() {
    // The assertion is that this file compiles: every name in the `use`
    // statement above resolved, and the signatures above type-checked.
}

/// Grep-based guard on the "no `conway_runtime` internals re-exported"
/// criterion: at most one `pub use conway_runtime::` line anywhere under
/// `crates/conway/src/**`.
#[test]
fn at_most_one_runtime_reexport() {
    let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let count = count_runtime_reexports(&src_dir);
    assert!(
        count <= 1,
        "expected at most one `pub use conway_runtime::` line under crates/conway/src, found {count}"
    );
}

fn count_runtime_reexports(dir: &std::path::Path) -> usize {
    let mut count = 0;
    for entry in std::fs::read_dir(dir).expect("read src dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            count += count_runtime_reexports(&path);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let contents = std::fs::read_to_string(&path).expect("read rs file");
            count += contents
                .lines()
                .filter(|line| line.trim_start().starts_with("pub use conway_runtime::"))
                .count();
        }
    }
    count
}
