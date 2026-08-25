//! The Claude Code plugin directory compatibility tier's install mechanism
//! for the CLI binary (board item `01M0VR89FB1F3Q4FQ8852K2A5E`): every
//! `[plugins].claude_compat[]` entry in `settings.json` names a directory
//! already on the operator's own machine; this module reads it
//! (`conway_plugin_claude::discover`, no network access anywhere in that
//! call) and attaches every `.mcp.json` server declaration it translated,
//! through the exact same `conway_plugin_mcp::McpPlugin::discover` ->
//! `ConwayBuilder::with_plugin` path `mcp_plugins::install` already uses
//! for an operator-authored `[plugins].mcp[]` entry.
//!
//! **A fourth, sibling choke point** -- `first_party_plugins`'s closed
//! candidate set, `subprocess_plugins`'s conway-wire host, `mcp_plugins`'s
//! JSON-RPC client, and this module's own directory-read translation layer
//! all resolve independently from the same `ConwayBuilder`, in
//! `main.rs::build_conway`.
//!
//! **Only the MCP half of what a Claude Code plugin directory can declare
//! is wired here.** `conway_plugin_claude::ClaudeCompatReport::hooks`/
//! `unsupported` are read separately, by `tui::app::startup` (for the
//! `/plugin` listing's own honesty requirement -- acceptance 5), never
//! here: this module's only job is making a translated MCP declaration a
//! real, running plugin, mirroring `mcp_plugins::install`'s own narrow
//! scope exactly.
//!
//! **Trust, stated where the capability is defined**, the same disclosure
//! `subprocess_plugins`/`mcp_plugins` each carry: everything a
//! `[plugins].claude_compat[]` entry's directory declares runs, or is read,
//! with the operator's own privileges and no sandboxing --
//! `conway::config::schema::PluginsConfig::claude_compat`'s own doc has the
//! full disclosure.

use std::sync::Arc;

use conway::{ConwayBuilder, FacadeError};
use conway_plugin_mcp::McpPlugin;

/// Discovers and attaches every `[plugins].claude_compat[]` entry's own
/// `.mcp.json` server declarations, in list order, then per-server order
/// within a directory. A discovery failure -- the directory itself missing,
/// a malformed `.claude-plugin/plugin.json`/`.mcp.json`
/// (`conway_plugin_claude::ClaudeCompatError`), or the translated MCP
/// server itself failing discovery (`conway_plugin_mcp::McpPluginError`) --
/// fails the WHOLE call as [`FacadeError::Build`], naming the offending
/// entry's own `id`, mirroring `subprocess_plugins::install`/
/// `mcp_plugins::install`'s own "an unresolvable entry fails the whole
/// build" posture for the same reason: an operator who named a directory in
/// `settings.json` and got nothing for it, silently, is exactly the rung-1
/// lie CONTRIBUTING's declaration rule exists to prevent.
pub async fn install(builder: ConwayBuilder) -> conway::Result<ConwayBuilder> {
    let entries = builder.config().plugins.claude_compat.clone();
    let mut builder = builder;
    for entry in entries {
        let report =
            conway_plugin_claude::discover(&entry.dir).map_err(|err| FacadeError::Build {
                message: format!("[plugins].claude_compat entry '{}': {err}", entry.id),
            })?;
        for server in report.mcp_servers {
            let server_name = server.name.clone();
            let spec = server.into_spec(entry.timeout_ms);
            let plugin = McpPlugin::discover(spec)
                .await
                .map_err(|err| FacadeError::Build {
                    message: format!(
                        "[plugins].claude_compat entry '{}': mcp server '{server_name}': {err}",
                        entry.id
                    ),
                })?;
            builder = builder.with_plugin(Arc::new(plugin));
        }
    }
    Ok(builder)
}

#[cfg(test)]
mod tests {
    //! **Wiring-only, exactly like `subprocess_plugins`/`mcp_plugins`'s own
    //! disclosure.** `conway_plugin_claude`'s own translation logic is
    //! covered by its own crate's test suite; what is local and checkable
    //! HERE is only that an empty entry list is a true no-op, and that a
    //! directory naming an entry which fails to discover fails the whole
    //! build, naming the entry -- P-13, checked directly rather than only
    //! asserted in prose.
    use super::*;
    use conway::config::schema::ConwayConfig;

    fn minimal_config() -> ConwayConfig {
        use std::collections::BTreeMap;

        use conway::config::schema::{
            AgentsConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
            PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig,
            ToolsConfig,
        };
        use conway_core::ids::RoleAlias;

        let mut roles = BTreeMap::new();
        roles.insert(
            "default".to_string(),
            RoleEntry {
                chain: vec![],
                headroom_tokens: None,
                ..Default::default()
            },
        );
        ConwayConfig {
            default_role: RoleAlias::new("default"),
            cwd: std::path::PathBuf::from("."),
            session: SessionConfig::default(),
            limits: LimitsConfig::default(),
            permissions: PermissionsConfig::default(),
            backends: BTreeMap::new(),
            routing: RoutingSection::default(),
            roles,
            health: HealthSection::default(),
            agents: AgentsConfig::default(),
            models: ModelsConfig::default(),
            tools: ToolsConfig::default(),
            plugins: PluginsConfig::default(),
            hooks: HooksConfig::default(),
        }
    }

    #[tokio::test]
    async fn an_empty_claude_compat_list_is_a_true_no_op() {
        let builder = ConwayBuilder::from_parts(minimal_config());
        let result = install(builder).await;
        assert!(
            result.is_ok(),
            "an empty [plugins].claude_compat list must never fail"
        );
    }

    #[tokio::test]
    async fn a_nonexistent_directory_fails_the_whole_build_naming_the_entry() {
        use conway::config::schema::ClaudeCompatPluginEntry;

        let mut config = minimal_config();
        config.plugins.claude_compat.push(ClaudeCompatPluginEntry {
            id: "acme-tools".to_string(),
            dir: std::path::PathBuf::from("/does/not/exist/at/all"),
            timeout_ms: 5_000,
        });
        let builder = ConwayBuilder::from_parts(config);
        // `ConwayBuilder` does not implement `Debug`, so `expect_err`/
        // `unwrap_err` (both bound on `T: Debug`) are unavailable here --
        // matched explicitly instead, mirroring `conway/tests/builder.rs`'s
        // own `expect_build_err` helper for the identical reason.
        let err = match install(builder).await {
            Ok(_) => panic!("a nonexistent claude_compat directory must fail the whole build"),
            Err(err) => err,
        };
        let message = err.to_string();
        assert!(
            message.contains("acme-tools"),
            "the failing entry's own id must be named: {message}"
        );
    }
}
