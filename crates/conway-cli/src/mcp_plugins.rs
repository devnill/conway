//! The MCP-over-stdio client plugin tier's install mechanism for the CLI
//! binary (board item `01M03GPNF0KN59FHAEEAEY2JD3`): every `[plugins].mcp[]`
//! entry in `settings.json`, discovered by spawning the named command as a
//! persistent child process, completing the JSON-RPC 2.0 `initialize`
//! handshake, and attaching the resulting plugin via the ordinary
//! `ConwayBuilder::with_plugin`.
//!
//! **A separate choke point from `subprocess_plugins::install`, AND from
//! `first_party_plugins::install`, deliberately.** `first_party_plugins::
//! install` resolves an operator-named id against a CLOSED set of plugin
//! crates this binary links; `subprocess_plugins::install` spawns a command
//! that speaks conway's OWN wire protocol; this module spawns a command that
//! speaks a DIFFERENT wire protocol (JSON-RPC 2.0, MCP). These mechanisms
//! compose independently: `main.rs::build_conway` calls each in order,
//! against the same `ConwayBuilder` -- joined by a FOURTH, board item
//! `01M0VR89FB1F3Q4FQ8852K2A5E`'s `claude_compat_plugins::install`, which
//! reuses this module's own `McpPlugin::discover` path for a translated
//! declaration rather than an operator-authored one. The MCP client is the SAME shape as the
//! subprocess tier (operator names a command in config; the CLI discovers it
//! async before `build()`; attaches via `with_plugin`) -- only the wire
//! protocol and the plugin crate differ.
//!
//! **Why this is async, like `subprocess_plugins::install`.** Discovering an
//! MCP server's own manifest means spawning a real process and awaiting its
//! `initialize`/`tools/list` handshake (`conway_plugin_mcp::McpPlugin::
//! discover`'s own doc: "a plugin needing setup does it in its own
//! constructor, before `ConwayBuilder::with_plugin`, where errors surface to
//! the embedder directly"). This is exactly that constructor call, at
//! exactly that point -- `main.rs`'s `build_conway` is `async fn` for this
//! reason (the subprocess tier already widened it; this tier rides the same
//! `.await`). `first_party_plugins::install` is separately `async fn` too
//! today (board item `01M09V3S2AQYB2VK6MANFRH1JM`, opening the durable
//! memory store) -- a later, unrelated reason; resolving `[plugins].install`
//! itself is still the pure, synchronous id lookup it always was.
//!
//! **Trust, disclosed at the one place this binary actually spawns anything
//! from this config, not only in the schema's own doc.** A `[plugins].mcp[]`
//! entry is code THIS process executes with the operator's own privileges,
//! on the identical footing `[hooks].rules[]` and `[plugins].subprocess[]`
//! already have (`conway_plugin_mcp`'s own crate doc has the full argument).
//! Board item `01KZHVFCN6ZEAXV7K5JHRQN1YB` (a digest-keyed `plugin` trust
//! subject) was reopened once both out-of-process transports shipped and
//! worked to a conclusion: considered and DECLINED, not deferred -- see
//! `docs/plugins/trust-and-security.md` for the full reasoning. This module
//! does not gate spawning on any trust check, exactly as
//! `subprocess_plugins::install` does not. An operator who
//! would not paste an unfamiliar command into `[hooks].rules[]` should not
//! paste one into `[plugins].mcp[]`.

use std::sync::Arc;

use conway::{ConwayBuilder, FacadeError};
use conway_plugin_mcp::{McpPlugin, McpPluginSpec};

/// Discovers and attaches every `[plugins].mcp[]` entry in `builder`'s own
/// config, in list order. A discovery failure (spawn, timeout, handshake
/// refusal, or an invalid `tools/list` answer -- every
/// [`conway_plugin_mcp::McpPluginError`] variant) fails the WHOLE call as
/// [`FacadeError::Build`], naming the offending entry's own `id` -- never
/// silently skipped, matching `subprocess_plugins::install`'s own
/// "an unresolvable entry fails the whole build" posture for the SAME reason:
/// an operator who named an MCP server in `settings.json` and got nothing for
/// it, silently, is exactly the rung-1 lie CONTRIBUTING's declaration rule
/// exists to prevent.
pub async fn install(builder: ConwayBuilder) -> conway::Result<ConwayBuilder> {
    let entries = builder.config().plugins.mcp.clone();
    let mut builder = builder;
    for entry in entries {
        let spec = McpPluginSpec {
            config_id: entry.id.clone(),
            command: entry.command,
            timeout_ms: entry.timeout_ms,
            env: entry.env,
        };
        let plugin = McpPlugin::discover(spec)
            .await
            .map_err(|err| FacadeError::Build {
                message: format!("[plugins].mcp entry '{}': {err}", entry.id),
            })?;
        builder = builder.with_plugin(Arc::new(plugin));
    }
    Ok(builder)
}

#[cfg(test)]
mod tests {
    //! **Wiring-only, exactly like `subprocess_plugins::tests`' own
    //! disclosure.** This module's own liveness is covered against the REAL
    //! compiled binary in `crates/conway-cli/tests/`, and
    //! `McpPlugin::discover`'s own failure-mode coverage lives in
    //! `crates/conway-plugin-mcp/tests/`. What is local, and checkable, HERE
    //! is only that an empty entry list is a true no-op (never spawns
    //! anything, never errors) -- the base case every other behavior in this
    //! file builds on.
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
    async fn an_empty_mcp_list_is_a_true_no_op() {
        let builder = ConwayBuilder::from_parts(minimal_config());
        let result = install(builder).await;
        assert!(
            result.is_ok(),
            "an empty [plugins].mcp list must never fail"
        );
    }
}
