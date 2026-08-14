//! Tool dispatch: [`registry::PluginRegistry`] compiles the
//! injected plugin set once; [`runner::ToolRunner`] owns per-call
//! resolution, schema validation, permission gating, bounded concurrent
//! execution, cancellation, truncation enforcement, and event emission.

pub mod registry;
pub mod runner;

pub use registry::PluginRegistry;
pub use runner::{ToolBatchCtx, ToolOutcome, ToolRunner};
