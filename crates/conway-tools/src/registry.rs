//! The single registration entry point the facade consumes:
//! [`builtin_plugins`].

use std::sync::Arc;

use conway_core::ports::Plugin;

/// The four built-in plugins, in registration order (not sorted — the
/// sorted-ids assertion belongs to the caller, not this ordering).
///
/// No plugin is privileged: every returned value is a plain `Arc<dyn
/// Plugin>` with no side channel to the runtime. If a future built-in needs
/// a capability, it is added to `ToolCtx` in conway-core, not here.
pub fn builtin_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![
        Arc::new(crate::fs::FsPlugin::new()),
        Arc::new(crate::shell::ShellPlugin::new()),
        Arc::new(crate::subagent::SubagentPlugin::new()),
        Arc::new(crate::report::ReportPlugin::new()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_plugins_with_expected_ids() {
        let mut ids: Vec<String> = builtin_plugins().iter().map(|p| p.manifest().id).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec![
                "conway.fs",
                "conway.report",
                "conway.shell",
                "conway.subagent"
            ]
        );
    }
}
