//! Preset plugin and permission-config registration helpers (WI-098).
//!
//! Contains no logic beyond delegation and defaults — no plugin registered
//! here is privileged over one an embedder supplies via
//! `ConwayBuilder::with_plugin` (GP-03).

use crate::config::schema::{PermissionMode, PermissionsConfig};

/// The built-in plugin set (`conway-tools`' `fs`, `shell`, `subagent`, and
/// `report` plugins), unchanged.
///
/// Gated on the `builtin-tools` feature: with it disabled, the crate has no
/// `conway-tools` dependency and this function does not exist, rather than
/// existing and returning an empty vector — a caller checking for built-in
/// tools at compile time gets a compile error, not a silent no-op.
#[cfg(feature = "builtin-tools")]
pub fn builtin_plugins() -> Vec<std::sync::Arc<dyn conway_core::ports::Plugin>> {
    conway_tools::builtin_plugins()
}

/// The recommended `[permissions]` config for one-shot (`-p`) invocations:
/// allow-list mode with an empty allow list, i.e. every tool call is denied
/// with feedback unless the embedder populates `allowed_tools` itself.
///
/// Allow-list mode is used (rather than `deny`) because it is the only mode
/// that never blocks on a prompt: one-shot mode has no interactive channel
/// to prompt through, and `AllowListGate` never returns `AllowAlways`, so
/// this preset is safe to use unattended.
pub fn default_permissions_for_one_shot() -> PermissionsConfig {
    PermissionsConfig {
        mode: PermissionMode::Allowlist,
        allowed_tools: Vec::new(),
        denied_tools: Vec::new(),
    }
}
