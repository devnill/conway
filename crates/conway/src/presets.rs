//! Preset plugin and permission-config registration helpers.
//!
//! Contains no logic beyond delegation and defaults — no plugin registered
//! here is privileged over one an embedder supplies via
//! `ConwayBuilder::with_plugin` -- the one extension mechanism.

use crate::config::schema::{PermissionMode, PermissionsConfig};

/// The full built-in plugin CANDIDATE set (`conway-tools`' `fs`, `shell`,
/// `subagent`, and `report` plugins), unchanged.
///
/// **Not every candidate returned here necessarily ends up registered.**
/// `ConwayBuilder::build` filters this list through a `PluginSelection`
/// (bash ships on by default and cannot be declined) before
/// installing anything -- by default every candidate except `conway.shell`
/// (bash), which requires a deliberate opt-in (see
/// `crate::config::schema::ToolsConfig`'s doc and `ConwayBuilder::
/// with_builtin_plugins`). This function itself still returns all four,
/// unfiltered: it is the raw candidate source the builder filters, not the
/// filtering policy.
///
/// Gated on the `builtin-tools` feature: with it disabled, the crate has no
/// `conway-tools` dependency and this function does not exist, rather than
/// existing and returning an empty vector — a caller checking for built-in
/// tools at compile time gets a compile error, not a silent no-op.
#[cfg(feature = "builtin-tools")]
pub fn builtin_plugins() -> Vec<std::sync::Arc<dyn conway_core::ports::Plugin>> {
    conway_tools::builtin_plugins()
}

/// Every built-in plugin id an operator may legitimately name in
/// `tools.builtin_plugins`, derived from the candidates themselves rather
/// than restated. A second hand-maintained list would drift the day
/// a built-in is added or renamed, and the drift would be silent: a valid
/// id rejected as unknown, or a stale one accepted and then never matched.
///
/// Used by config validation to reject a typo instead of letting it
/// silently disable a tool the operator believes they enabled.
///
/// Returns owned `String`s because the ids come from each plugin's own
/// `PluginManifest`, which is constructed per call.
#[cfg(feature = "builtin-tools")]
pub fn builtin_plugin_ids() -> Vec<String> {
    builtin_plugins()
        .iter()
        .map(|p| p.manifest().id.clone())
        .collect()
}

/// The recommended `[permissions]` config for one-shot (`-p`) invocations:
/// allow-list mode with an empty allow list, i.e. every tool call is denied
/// with feedback unless the embedder populates `allowed_tools` itself.
///
/// Allow-list mode is used (rather than `deny`) because it is the only mode
/// that never blocks on a prompt: one-shot mode has no interactive channel
/// to prompt through, and `AllowListGate` never returns `AllowAlways`, so
/// this preset is safe to use unattended.
///
/// **Builds successfully.** This exact combination (`mode = "allowlist"`,
/// empty `allowed_tools`) was once rejected unconditionally by
/// `config::merge::validate`'s check 3, which made this preset dead on
/// arrival for every caller (board item 01M01EM4QSB204FZSANJB3XH78). Check 3
/// is now scoped to configs a human could have hand-typed into a settings
/// file (`config::load`/`load_ignoring_user_config`'s own call site) -- see that
/// check's own comment in `crate::config::merge` for the full reasoning --
/// and no longer runs on `ConwayBuilder::build`'s re-validation step, so a
/// config carrying this preset unmodified now builds. Proven, not just
/// asserted: `tests/preset_one_shot_permissions_build.rs` builds a `Conway`
/// from this exact value and drives a turn through it.
pub fn default_permissions_for_one_shot() -> PermissionsConfig {
    PermissionsConfig {
        mode: PermissionMode::Allowlist,
        allowed_tools: Vec::new(),
        denied_tools: Vec::new(),
    }
}
