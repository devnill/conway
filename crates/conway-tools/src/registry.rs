//! The single registration entry point the facade consumes:
//! [`builtin_plugins`].
//!
//! At this stage (WI-061) the four built-in plugins do not exist yet
//! (`FsPlugin` lands in WI-062/063, `ShellPlugin` in WI-064, `ReportPlugin`
//! in WI-065, `SubagentPlugin` in WI-066); `builtin_plugins` returns an
//! empty vec until WI-067 assembles and populates it.

use std::sync::Arc;

use conway_core::ports::Plugin;

/// No plugin is privileged: every returned value is a plain `Arc<dyn
/// Plugin>` with no side channel to the runtime.
pub fn builtin_plugins() -> Vec<Arc<dyn Plugin>> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_at_this_stage() {
        assert!(builtin_plugins().is_empty());
    }
}
