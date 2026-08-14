//! Conway-tools: the built-in `Plugin`/`Tool` implementations for
//! conway-core's plugin ports (architecture "Module: conway-tools").
//!
//! This crate provides no privileged capability: every built-in plugin is a
//! plain `Arc<dyn Plugin>` built from the exact same `Plugin`/`Tool` traits
//! available to third parties, and every runtime interaction goes through
//! `ToolCtx` ports (`events`, `subagents`, `cancel`, `config`). This crate
//! MUST NOT depend on `conway-runtime`, `conway-session`, `conway-routing`,
//! or `conway-plugin-backends` (architecture boundary rule).
//!
//! Four built-in plugins, one per submodule:
//! - [`fs`] — `cd`, `read`, `write`, `edit`, `glob`, `grep` (`FsPlugin`)
//! - [`shell`] — `bash` (`ShellPlugin`)
//! - [`subagent`] — `conway_fork`, `conway_spawn`, `conway_ask`,
//!   `conway_steer`, `conway_await`, `conway_cancel` (`SubagentPlugin`)
//! - [`report`] — `report` (`ReportPlugin`)
//!
//! [`common`] holds the shared helper layer every tool builds on.
//! `process` holds the process-group spawn/kill primitives [`shell`] and
//! [`hook_runner`] both build on -- one implementation, never restated.
//! [`hook_runner`] is `conway_core::ports::HookRunner`'s one-shot exec
//! implementation -- not a
//! `Plugin`/`Tool`, so it is not part of [`builtin_plugins`]; this item
//! wires no event, so nothing constructs or injects it yet.
//! [`builtin_plugins`] is the single registration entry point the facade
//! consumes.

pub mod common;
pub mod fs;
pub mod hook_runner;
mod process;
pub mod report;
pub mod shell;
pub mod subagent;

mod registry;

/// In-crate test doubles (`FakeSubagentHost`, `RecordingEventSink`,
/// `test_ctx`) that let every tool in this crate be unit-tested with zero
/// runtime. Always available to this crate's own `#[cfg(test)]` unit tests;
/// available to external/integration tests (`tests/*.rs`, downstream
/// crates) only when the `testing` feature is enabled, so ordinary
/// (non-test) builds never carry this code.
#[cfg(any(test, feature = "test-fakes"))]
pub mod testing;

pub use registry::builtin_plugins;
