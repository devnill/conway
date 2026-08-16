//! The subprocess plugin tier's install mechanism for the CLI binary
//! (board item `01KZY8PATND84AKY0J376E3DWV`): every `[plugins].subprocess[]`
//! entry in `settings.json`, discovered by spawning the named command and
//! attached via the ordinary `ConwayBuilder::with_plugin`.
//!
//! **A separate choke point from `first_party_plugins::install`,
//! deliberately.** That module resolves an operator-named id against a
//! CLOSED set of plugin crates this binary happens to link
//! (`first_party_plugins::bundle`); a mismatch is an unknown-id error. This
//! module has no such candidate set -- there is nothing to look up, every
//! `[plugins].subprocess[]` entry names a command directly and IS spawned,
//! by construction, the identical relationship `[hooks].rules[].command`
//! already has to a hook rule. The two mechanisms compose independently:
//! `main.rs::build_conway` calls both, in either order, against the same
//! `ConwayBuilder`.
//!
//! **Why this is async, unlike `first_party_plugins::install`.** Resolving
//! `[plugins].install` is a pure, synchronous id lookup against an
//! in-memory `Vec`; discovering a subprocess plugin's own manifest means
//! spawning a real process and awaiting its `tool.spec/1` answer
//! (`conway_plugin_subprocess::SubprocessPlugin::discover`'s own doc: "a
//! plugin needing setup does it in its own constructor, before
//! `ConwayBuilder::with_plugin`, where errors surface to the embedder
//! directly"). This is exactly that constructor call, at exactly that
//! point -- `main.rs`'s `build_conway` is `async fn` for this reason alone
//! (see that function's own doc for the disclosed widening this causes).
//!
//! **Trust, disclosed at the one place this binary actually spawns
//! anything from this config, not only in the schema's own doc.** A
//! `[plugins].subprocess[]` entry is code THIS process executes with the
//! operator's own privileges, on the identical footing `[hooks].rules[]`
//! already has (`conway_plugin_subprocess`'s own crate doc has the full
//! argument). Board item `01KZHVFCN6ZEAXV7K5JHRQN1YB` (a digest-keyed
//! `plugin` trust subject) is under a STANDING OPERATOR DEFERRAL and is
//! NOT built here or anywhere in this item -- this module does not gate
//! spawning on any trust check, exactly as `ProcessHookRunner` does not
//! gate a hook's command on one either. An operator who would not paste an
//! unfamiliar command into `[hooks].rules[]` should not paste one into
//! `[plugins].subprocess[]`.

use std::sync::Arc;

use conway::config::schema::SubprocessTransport as ConfigTransport;
use conway::{ConwayBuilder, ConwayError};
use conway_plugin_subprocess::{
    SubprocessPlugin, SubprocessPluginSpec, SubprocessTransport as PluginTransport,
};

/// Discovers and attaches every `[plugins].subprocess[]` entry in
/// `builder`'s own config, in list order. A discovery failure (spawn,
/// timeout, nonzero exit, or an invalid/unparseable manifest -- every
/// [`conway_plugin_subprocess::SubprocessPluginError`] variant) fails the
/// WHOLE call as [`ConwayError::Build`], naming the offending entry's own
/// `id` -- never silently skipped, matching this crate's own
/// `first_party_plugins::install`'s "an unresolvable entry fails the whole
/// build" posture for the SAME reason: an operator who named a plugin in
/// `settings.json` and got nothing for it, silently, is exactly the
/// rung-1 lie CONTRIBUTING's declaration rule exists to prevent.
pub async fn install(builder: ConwayBuilder) -> conway::Result<ConwayBuilder> {
    let entries = builder.config().plugins.subprocess.clone();
    let mut builder = builder;
    for entry in entries {
        let transport = match entry.transport {
            ConfigTransport::OneShot => PluginTransport::OneShot,
            ConfigTransport::Persistent => PluginTransport::Persistent,
        };
        let spec = SubprocessPluginSpec {
            config_id: entry.id.clone(),
            command: entry.command,
            timeout_ms: entry.timeout_ms,
            transport,
        };
        let plugin = SubprocessPlugin::discover(spec)
            .await
            .map_err(|err| ConwayError::Build {
                message: format!("[plugins].subprocess entry '{}': {err}", entry.id),
            })?;
        builder = builder.with_plugin(Arc::new(plugin));
    }
    Ok(builder)
}

#[cfg(test)]
mod tests {
    //! **Wiring-only, exactly like `first_party_plugins::tests`' own
    //! disclosure** ("Constructing a `ConwayBuilder` here would need a
    //! stub config solely to re-check what the integration suite already
    //! proves against the real binary"). This module's own liveness is
    //! covered against the REAL compiled binary in
    //! `crates/conway-cli/tests/subprocess_plugins.rs`, and
    //! `SubprocessPlugin::discover`'s own failure-mode coverage lives in
    //! `crates/conway-plugin-subprocess/tests/mechanism.rs`. What is
    //! local, and checkable, HERE is only that an empty entry list is a
    //! true no-op (never spawns anything, never errors) -- the base case
    //! every other behavior in this file builds on.
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
    async fn an_empty_subprocess_list_is_a_true_no_op() {
        let builder = ConwayBuilder::from_parts(minimal_config());
        let result = install(builder).await;
        assert!(
            result.is_ok(),
            "an empty [plugins].subprocess list must never fail"
        );
    }
}
