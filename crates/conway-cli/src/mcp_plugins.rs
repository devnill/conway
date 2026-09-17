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

use conway::config::schema::McpPluginEntry;
use conway::config::{ConfigWarning, WarningCode};
use conway::{ConwayBuilder, FacadeError};
use conway_plugin_mcp::{McpPlugin, McpPluginSpec};

use crate::diag;

/// Builds the [`McpPluginSpec`] `McpPlugin::discover` needs from one
/// `[plugins].mcp[]` entry -- pulled out so [`install`] and
/// [`install_tolerant`] share exactly one place that maps config fields
/// onto discovery parameters, rather than the two copies that would drift
/// the moment a new field's default logic changed in only one of them.
fn spec_for(entry: &McpPluginEntry) -> McpPluginSpec {
    McpPluginSpec {
        config_id: entry.id.clone(),
        command: entry.command.clone(),
        timeout_ms: entry.timeout_ms,
        // The opening handshake gets its own, larger budget: an
        // operator-authored server may also need to start before it can
        // answer. `timeout_ms` stays the per-call deadline it has always
        // been, so an operator who tuned it sees no change in what it
        // governs.
        startup_timeout_ms: conway_plugin_mcp::DEFAULT_STARTUP_TIMEOUT_MS,
        // The FIRST ordinary round trip after the handshake gets its own
        // budget too, operator-tunable like `timeout_ms` -- board item
        // `01M1YQ3MJQSCQTMVAZ3GCSTB8P`.
        first_call_timeout_ms: entry.first_call_timeout_ms,
        env: entry.env.clone(),
    }
}

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
///
/// **The posture every TURN-TAKING dispatch target still gets** (the TUI,
/// one-shot `-p`, and any `Command::External` -- `main.rs`'s own
/// `command_tolerates_mcp_startup_failures` function has the full list of
/// what does NOT get this posture). A session that might actually call a
/// tool must still refuse to start rather than silently omit one an
/// operator declared -- see [`install_tolerant`]'s own doc for the sibling
/// function that widens past this for introspection-only targets, and
/// board item `01M2PJCT90G2010KGCJ4YSFREM` for the reasoning that produced
/// the split.
///
/// **Deliberately UNCHANGED by board item `01M1AMSDE035HAG23TE6XPEF9R`,
/// disclosed here so a reader of that item does not assume symmetry that
/// was not actually shipped.** That item made the SIBLING install path,
/// `claude_compat_plugins::install`, degrade-and-announce instead of
/// hard-failing when a TRANSLATED `.mcp.json` server's own `McpPlugin::
/// discover` call fails -- see that function's own doc for the full
/// ruling (an MCP server contributes tools only, so a dead one narrows
/// capability rather than silently dropping a permission rule; P-13 does
/// not apply to a tool-only surface). The identical argument could be made
/// for an operator-AUTHORED `[plugins].mcp[]` entry here -- same wire
/// protocol, same "tools only" contribution shape -- but extending the
/// ruling to every dispatch target unconditionally is a SEPARATE, wider
/// change that item chose not to make on its own account, and board item
/// `01M2PJCT90G2010KGCJ4YSFREM` still does not make it unconditionally --
/// see [`install_tolerant`]'s own doc for exactly how far it goes instead.
/// This function's own hard-fail posture stays exactly as it was.
pub async fn install(builder: ConwayBuilder) -> conway::Result<ConwayBuilder> {
    let entries = builder.config().plugins.mcp.clone();
    let mut builder = builder;
    for entry in entries {
        let spec = spec_for(&entry);
        let plugin = McpPlugin::discover(spec)
            .await
            .map_err(|err| FacadeError::Build {
                message: format!("[plugins].mcp entry '{}': {err}", entry.id),
            })?;
        builder = builder.with_plugin(Arc::new(plugin));
    }
    Ok(builder)
}

/// The introspection-only counterpart to [`install`] -- board item
/// `01M2PJCT90G2010KGCJ4YSFREM`. A crashing MCP plugin used to fail the
/// WHOLE build via [`install`]'s own hard-fail posture, which meant a
/// command that never starts an agent or calls a tool -- `routes explain`,
/// `sessions`, `tools list`, `plugin list`, `plugin install`, `plugin
/// remove` -- refused with the same `[plugins].mcp entry '...': ... session
/// died ...` error a turn-taking invocation would legitimately produce. The
/// operator most likely to reach for `routes explain` is the one debugging
/// why their session will not start -- exactly who that choke point blocked.
///
/// **Degrades exactly like `claude_compat_plugins::install`'s own MCP-server
/// handling** (board item `01M1AMSDE035HAG23TE6XPEF9R`, that function's own
/// doc has the full argument for why a dead MCP server narrows capability
/// rather than dropping a permission rule, so P-13 does not forbid
/// degrading here): a failing entry is skipped, reported on the SAME two
/// channels that function already uses -- unconditional `diag::warn`
/// (GP-14: an operator reading a degraded run must be told which plugin
/// failed and what is missing) and a [`ConfigWarning`] with
/// [`WarningCode::McpServerFailed`] pushed via [`ConwayBuilder::
/// with_warning`], which reaches `Conway::warnings()` -- rendered, already,
/// by `main.rs`'s own non-interactive warning loop for every dispatch
/// target this function is ever called for (none of them is the TUI) --
/// and every OTHER entry, successful or not, is still attempted; one dead
/// server never stops discovery of its siblings.
///
/// **This function's caller decides whether it, or [`install`], runs for a
/// given invocation -- never both, never a runtime toggle inside this
/// module.** `main.rs::command_tolerates_mcp_startup_failures` is that
/// predicate; deliberately a SEPARATE test from `command_needs_provider`
/// even though the two happen to admit the identical command set today
/// (`sessions`/`routes`/`tools list`/`plugin install`/`plugin remove`/
/// `plugin list`) -- one asks "does this need a working model," the other
/// asks "may this tolerate a missing plugin," and conflating them would
/// make a future change to either quietly redefine the other's set. See
/// that predicate's own doc for the full argument.
///
/// Never fails: an entry that cannot be discovered is reported and skipped,
/// not propagated, so this returns the (possibly-narrowed) `builder`
/// outright rather than a `conway::Result`.
pub async fn install_tolerant(builder: ConwayBuilder) -> ConwayBuilder {
    let entries = builder.config().plugins.mcp.clone();
    let mut builder = builder;
    for entry in entries {
        let id = entry.id.clone();
        let spec = spec_for(&entry);
        match McpPlugin::discover(spec).await {
            Ok(plugin) => builder = builder.with_plugin(Arc::new(plugin)),
            Err(err) => {
                let message = format!(
                    "[plugins].mcp entry '{id}': {err} -- starting WITHOUT this server's tools \
                     (this command never proposes a tool call). Fix or remove the entry in \
                     settings.json's [plugins].mcp list, or run `conway routes explain <role>` \
                     again once it is healthy."
                );
                diag::warn(&message);
                builder = builder.with_warning(ConfigWarning {
                    code: WarningCode::McpServerFailed,
                    message,
                });
            }
        }
    }
    builder
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

    /// [`install_tolerant`]'s own base case, mirroring [`an_empty_mcp_list_
    /// is_a_true_no_op`] exactly: nothing to discover, nothing degraded,
    /// `builder` comes back untouched.
    #[tokio::test]
    async fn an_empty_mcp_list_is_a_true_no_op_for_install_tolerant() {
        let builder = ConwayBuilder::from_parts(minimal_config());
        let builder = install_tolerant(builder).await;
        assert!(
            builder.warnings().is_empty(),
            "an empty [plugins].mcp list must never produce a warning"
        );
    }

    /// **The headline claim board item `01M2PJCT90G2010KGCJ4YSFREM` exists
    /// to prove.** An entry naming a command that cannot even be spawned
    /// (the `ALWAYS_DIE_SERVER`/nonexistent-command shape the board item's
    /// own traced mechanism names) degrades under [`install_tolerant`]
    /// instead of returning an `Err` the way [`install`] still would for
    /// the identical entry -- proven directly against this module's own
    /// function, not only end-to-end through the compiled binary (that
    /// coverage lives in `crates/conway-cli/tests/`).
    #[tokio::test]
    async fn a_dead_mcp_entry_degrades_under_install_tolerant_instead_of_failing() {
        let mut config = minimal_config();
        config.plugins.mcp.push(McpPluginEntry {
            id: "acme-mcp".to_string(),
            command: vec!["/does/not/exist/at/all".to_string()],
            ..Default::default()
        });
        let builder = ConwayBuilder::from_parts(config);
        let builder = install_tolerant(builder).await;

        assert!(
            builder.plugins().is_empty(),
            "the dead entry's own plugin must never attach: {}",
            builder.plugins().len()
        );
        let warning = builder
            .warnings()
            .iter()
            .find(|w| w.code == conway::config::WarningCode::McpServerFailed)
            .expect("the degraded entry must be reported on Conway::warnings()");
        assert!(
            warning.message.contains("acme-mcp"),
            "the failing entry's own id must be named: {}",
            warning.message
        );
    }

    /// The SAME entry, run through [`install`] instead, must still fail the
    /// whole call -- pinning that the two functions genuinely differ in
    /// posture rather than [`install_tolerant`] having quietly become the
    /// only behavior left. This is [`install`]'s own pre-existing contract,
    /// re-asserted here as the direct contrast to the test immediately
    /// above.
    #[tokio::test]
    async fn the_same_dead_entry_still_fails_the_whole_call_under_install() {
        let mut config = minimal_config();
        config.plugins.mcp.push(McpPluginEntry {
            id: "acme-mcp".to_string(),
            command: vec!["/does/not/exist/at/all".to_string()],
            ..Default::default()
        });
        let builder = ConwayBuilder::from_parts(config);
        let err = match install(builder).await {
            Ok(_) => panic!("a dead MCP entry must still fail install()'s own hard-fail posture"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("acme-mcp"),
            "the failing entry's own id must be named: {err}"
        );
    }

    /// Two independently-dead entries both degrade under [`install_tolerant`]
    /// -- one entry's failure never masks or stops discovery of the next,
    /// mirroring `claude_compat_plugins::install`'s own
    /// `two_independently_dead_mcp_servers_in_the_same_directory_both_
    /// degrade` proof for the sibling install path.
    #[tokio::test]
    async fn two_independently_dead_mcp_entries_both_degrade() {
        let mut config = minimal_config();
        config.plugins.mcp.push(McpPluginEntry {
            id: "acme-mcp-a".to_string(),
            command: vec!["/does/not/exist/at/all".to_string()],
            ..Default::default()
        });
        config.plugins.mcp.push(McpPluginEntry {
            id: "acme-mcp-b".to_string(),
            command: vec!["/also/does/not/exist".to_string()],
            ..Default::default()
        });
        let builder = ConwayBuilder::from_parts(config);
        let builder = install_tolerant(builder).await;

        assert_eq!(
            builder
                .warnings()
                .iter()
                .filter(|w| w.code == conway::config::WarningCode::McpServerFailed)
                .count(),
            2,
            "both entries' own failures must be reported individually: {:?}",
            builder.warnings()
        );
    }
}
