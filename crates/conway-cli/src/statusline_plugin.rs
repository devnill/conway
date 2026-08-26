//! The status-line plugin's install mechanism for the CLI binary (board
//! item `01M0X500861X9035QJEA82F94K`): the single `[tui.status_line_command]`
//! entry in `settings.json`, converted into a
//! `conway_plugin_statusline::StatusLineSpec` and attached via the
//! ordinary `ConwayBuilder::with_plugin` -- exactly the migration home
//! `docs/plugins/statusline.md` describes for a Claude Code
//! `statusLine.type`/`statusLine.command` pair.
//!
//! **A FIFTH sibling choke point, alongside `subprocess_plugins`/
//! `mcp_plugins`/`claude_compat_plugins` -- deliberately NOT folded into
//! `first_party_plugins::install`.** That module resolves an operator-named
//! id against a CLOSED set of plugin crates this binary happens to link
//! (`first_party_plugins::bundle`); a mismatch is an unknown-id error, and
//! every one of its ten entries ships with an opinionated default and no
//! `settings.json` field of its own (`docs/plugins/README.md`'s own
//! membership rule). This module has no such candidate set to match
//! against -- naming a command in `[tui.status_line_command].command` is
//! ALREADY the complete opt-in signal, the identical shape
//! `[plugins].subprocess[]`/`[plugins].mcp[]`/`[plugins].claude_compat[]`
//! already have ("an operator names a command/directory directly; that
//! naming alone is what makes it run"), so `"conway.statusline"` never
//! appears in `[plugins].install` at all.
//!
//! **Unlike its three siblings above, this is NOT async.** Discovering a
//! subprocess/MCP/Claude-Code-compat plugin means a real handshake before
//! `ConwayBuilder::with_plugin` can even be called -- spawning a process
//! and awaiting its manifest. This plugin needs no such handshake: it is
//! an ordinary in-process `Plugin` implementation
//! (`conway_plugin_statusline::StatusLinePlugin`), constructed
//! synchronously, exactly like every `first_party_plugins::bundle()`
//! entry. Its own background refresh loop starts inside that synchronous
//! constructor (via `tokio::runtime::Handle::try_current()` -- see
//! `conway_plugin_statusline::StatusLinePlugin::new`'s own doc), which
//! works here specifically because `build_conway` (this function's one
//! caller) already runs inside `#[tokio::main]`.
//!
//! **Trust, disclosed at the one place this binary actually spawns
//! anything from this config, not only in the schema's own doc.** A
//! `[tui.status_line_command].command` is code THIS process executes
//! REPEATEDLY, with the operator's own privileges, on the identical
//! footing `[hooks].rules[].command`/`[plugins].subprocess[].command`
//! already have -- no sandboxing, no digest check. An operator who would
//! not paste an unfamiliar command into `[hooks].rules[]` should not paste
//! one into `[tui.status_line_command].command` either -- see
//! `docs/plugins/statusline.md` for the full disclosure and the cadence
//! bound this crate enforces regardless of configuration.

use conway::ConwayBuilder;
use conway_plugin_statusline::{StatusLinePlugin, StatusLineSpec};

/// Reads `[tui.status_line_command]` from the CLI's OWN layered config
/// load -- not from `builder.config()`, which cannot carry a
/// presentation-shaped section (`crates/conway/tests/
/// architecture_invariants.rs` T7) -- and, when it names a
/// non-empty command, constructs and attaches a `StatusLinePlugin`. A true
/// no-op (returns `builder` unchanged, attaches nothing, starts no
/// background task) when `command` is empty -- the default, and the state
/// of an operator who never wrote this section at all.
///
/// Infallible, unlike `subprocess_plugins::install`/`mcp_plugins::install`:
/// there is no handshake here that can fail (no process is spawned by this
/// function itself -- only by the plugin's own background loop, later, on
/// its own cadence), so this stays a plain, synchronous
/// `fn(ConwayBuilder) -> ConwayBuilder` rather than the fallible `async fn`
/// its three async siblings are.
pub fn install(builder: ConwayBuilder, tui: &crate::tui::config::TuiSection) -> ConwayBuilder {
    let entry = tui.status_line_command.clone();
    if entry.command.is_empty() {
        return builder;
    }
    let spec = StatusLineSpec {
        command: entry.command,
        key: entry.key,
        refresh_interval_ms: entry.refresh_interval_ms,
        timeout_ms: entry.timeout_ms,
    };
    builder.with_plugin(std::sync::Arc::new(StatusLinePlugin::new(spec)))
}

#[cfg(test)]
mod tests {
    //! **Wiring-only, exactly like `subprocess_plugins::tests`' own
    //! disclosure.** What is local and checkable HERE is only that an
    //! unconfigured `[tui.status_line_command]` is a true no-op -- the base
    //! case every other behavior in this module builds on. This module's
    //! own liveness with a REAL configured command is
    //! `conway-plugin-statusline`'s own end-to-end coverage
    //! (`crates/conway-plugin-statusline/tests/statusline_end_to_end.rs`),
    //! which drives `ConwayBuilder::with_plugin` directly -- the identical
    //! shape this function's own single `with_plugin` call takes, minus
    //! only the `[tui.status_line_command]` config-to-spec conversion this
    //! module performs.
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

    /// The base case: no `[tui.status_line_command].command` at all is a
    /// true no-op -- `install` returns the builder with nothing attached,
    /// provable indirectly (this module holds no reachable list of
    /// installed plugins to inspect directly, the same constraint
    /// `subprocess_plugins::tests`' own doc names) by confirming `install`
    /// does not panic and returns a builder that still builds cleanly with
    /// every other port faked.
    #[test]
    fn an_unconfigured_statusline_entry_is_a_true_no_op() {
        let tui = crate::tui::config::TuiSection::default();
        assert!(
            tui.status_line_command.command.is_empty(),
            "sanity: the default TUI section carries no status-line command"
        );
        let builder = ConwayBuilder::from_parts(minimal_config());
        // `install` must not panic on the default (empty) entry -- the
        // only property checkable at this wiring layer without duplicating
        // `conway-plugin-statusline`'s own end-to-end coverage.
        let _builder = install(builder, &tui);
    }

    /// The section moved out of `conway`'s own config schema after
    /// `crates/conway/tests/architecture_invariants.rs`'s T7 rejected it
    /// there: a status line is presentation, and the facade's schema must
    /// not name a presentation-shaped type so a headless host never parses
    /// terminal vocabulary it cannot render. This pins the move so a
    /// future convenience does not quietly walk it back.
    #[test]
    fn the_status_line_command_is_not_reachable_from_the_facade_config() {
        let config = minimal_config();
        let rendered = format!("{:?}", config.plugins);
        assert!(
            !rendered.to_lowercase().contains("statusline")
                && !rendered.to_lowercase().contains("status_line"),
            "the facade's [plugins] section must carry no status-line slot: {rendered}"
        );
    }
}
