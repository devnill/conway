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
//! **Board item `01M0XBZNBPXEESX8VNTJDKNG0J`: the hook half is wired here
//! too, now.** Until this item, only the MCP half of what a Claude Code
//! plugin directory can declare was ever appended into the `ConwayBuilder`
//! this module hands `build()` -- `conway_plugin_claude::
//! ClaudeCompatReport::hook_registrations()` (board item
//! `01M0X1FCQ80C9ET97HENXSAW2K`) already produced real, dispatchable
//! `[hooks].rules[]`-shaped registrations, but nothing appended them into a
//! `HooksConfig` before `build()` read one: an operator naming a directory
//! in `[plugins].claude_compat[]` got its MCP servers running and its
//! hooks reported, never dispatching -- the built-but-unreachable defect
//! `DESIGN-plugin-dependencies.md` §1 names as this tree's recurring
//! disease. `install`'s loop below now does both halves, per entry: attach
//! every `.mcp.json` server (unchanged), then append every mapped
//! `hooks/hooks.json` rule's own `HookRegistration` into
//! `builder.config_mut().hooks.rules` via `ConwayBuilder::config_mut`
//! (added by this item -- see that method's own doc for why no narrower
//! seam already existed). `conway_plugin_claude::ClaudeCompatReport::
//! unsupported` is still read separately, by `tui::app::startup` (for the
//! `/plugin` listing's own honesty requirement -- acceptance 5) -- this
//! module's job stays "make a translated declaration real," not
//! "report on everything found."
//!
//! **Guard rail, deliberate: a translated hook's `on_failure` is left at
//! `conway_core::hook::HookOnFailure`'s own default, `Deny`, never set
//! explicitly by this module.** `conway_plugin_claude::HookRegistration`
//! carries no `on_failure` field of its own (that policy is
//! `conway::config::schema::HookEntry`-only, and this crate never depends
//! on `conway` -- see that crate's own module doc), so `to_hook_entry`
//! constructs every appended `conway::config::schema::HookEntry` via
//! `..Default::default()` for exactly that one field. This is the SAME
//! posture every existing `[hooks].rules[]` entry with no explicit
//! `on_failure` already has (board item `01M0X1AH44SNMK5TZ507K30QNP`): this
//! layer must not silently pick a foreign plugin's own failure posture on
//! the operator's behalf, and fail-closed is the one choice that never
//! WIDENS what an outage does. See `install`'s own test,
//! `a_translated_pre_tool_use_hook_carries_on_failure_deny`, which pins it
//! directly against a real translated registration rather than only
//! asserting it in prose.
//!
//! **Guard rail, deliberate: deny-capable hooks are called out, by name, on
//! stderr -- distinct from observation-only ones, and unconditionally.** A
//! translated `pre_tool_use` rule is a real permission consequence of
//! naming a directory in `settings.json`: it can deny a real tool call, the
//! identical authority an operator-authored `[hooks].rules[]` entry already
//! has. `install` reports that distinction itself, via
//! `conway_cli::diag::warn` (unconditional stderr, "reserve this for
//! something an operator would act on" -- that function's own doc) for
//! every `pre_tool_use` registration, and `diag::info` (verbose-only,
//! routine progress) for every other, observation-only one -- never one
//! undifferentiated "hooks registered" line. Both calls happen inside
//! `build_conway`, before the TUI ever puts the terminal into raw/alternate-
//! screen mode (`main.rs`'s own comment on why a stray stderr write after
//! that point lands on top of the drawn UI), so this reaches the operator's
//! real scrollback on every dispatch target, TUI included.
//!
//! **The payload-shape caveat this module does not, and must not, weaken.**
//! `conway_plugin_claude::hooks`'s own module doc states it in full:
//! "dispatches" is not the same claim as "behaves identically to running
//! under real Claude Code" -- a translated hook script still reads
//! `tool_name`/`tool_input` on stdin, while conway's dispatcher sends its
//! own `HookInvocation`/`HookEvent` shape. Wiring dispatch (this item) makes
//! the registration REAL; it does not, and cannot, repair that mismatch --
//! `docs/plugins/claude-compat.md` states the same limitation for the
//! operator, and nothing here claims otherwise.
//!
//! **Trust, stated where the capability is defined**, the same disclosure
//! `subprocess_plugins`/`mcp_plugins` each carry: everything a
//! `[plugins].claude_compat[]` entry's directory declares runs, or is read,
//! with the operator's own privileges and no sandboxing --
//! `conway::config::schema::PluginsConfig::claude_compat`'s own doc has the
//! full disclosure.

use std::sync::Arc;

use conway::config::schema::HookEntry;
use conway::{ConwayBuilder, FacadeError};
use conway_plugin_claude::HookRegistration;
use conway_plugin_mcp::McpPlugin;

use crate::diag;

/// The one conway core event a translated registration can carry that is
/// ever consulted at `PermissionBroker::decide`'s DENY tier -- mirrors
/// `ConwayBuilder::build`'s own `rule.event == "pre_tool_use"` filter
/// (`crates/conway/src/builder.rs`) exactly, so this module's own
/// deny-capable/observation-only split can never drift from what `build()`
/// actually treats as consequential.
const DENY_CAPABLE_EVENT: &str = "pre_tool_use";

/// Converts one translated [`HookRegistration`] into a real, appendable
/// `conway::config::schema::HookEntry` -- field for field, per
/// `HookRegistration`'s own doc ("mirrors `HookEntry`'s five fields
/// exactly, deliberately NOT that literal type"). `on_failure` is left at
/// `HookEntry::default`'s own value (`HookOnFailure::Deny`) -- see this
/// module's own top doc for why that is deliberate, not an oversight.
fn to_hook_entry(registration: HookRegistration) -> HookEntry {
    HookEntry {
        id: registration.id,
        event: registration.event.to_string(),
        match_tool: registration.match_tool,
        command: registration.command,
        timeout_ms: registration.timeout_ms,
        enabled: registration.enabled,
        ..Default::default()
    }
}

/// Reports, on stderr, which of `registrations` -- all already known to
/// belong to `entry_id` -- can deny a real tool call and which are
/// observation-only, per this module's own "distinguish, don't just say
/// 'hooks registered'" guard rail. A true no-op when `registrations` is
/// empty (neither call below ever fires).
fn report_hook_registrations(entry_id: &str, registrations: &[HookRegistration]) {
    let deny_capable: Vec<&str> = registrations
        .iter()
        .filter(|r| r.event == DENY_CAPABLE_EVENT)
        .map(|r| r.id.as_str())
        .collect();
    let observation_only: Vec<&str> = registrations
        .iter()
        .filter(|r| r.event != DENY_CAPABLE_EVENT)
        .map(|r| r.id.as_str())
        .collect();
    if !deny_capable.is_empty() {
        diag::warn(format!(
            "[plugins].claude_compat entry '{entry_id}' registered {} hook(s) that CAN DENY a \
             real tool call ({DENY_CAPABLE_EVENT}): {}",
            deny_capable.len(),
            deny_capable.join(", ")
        ));
    }
    if !observation_only.is_empty() {
        diag::info(format!(
            "[plugins].claude_compat entry '{entry_id}' registered {} observation-only hook(s): \
             {}",
            observation_only.len(),
            observation_only.join(", ")
        ));
    }
}

/// Discovers and attaches every `[plugins].claude_compat[]` entry's own
/// `.mcp.json` server declarations, in list order, then per-server order
/// within a directory -- then appends every mapped `hooks/hooks.json`
/// rule's own [`HookRegistration`] into the SAME builder's `HooksConfig`
/// (this module's own top doc). A discovery failure -- the directory itself
/// missing, a malformed `.claude-plugin/plugin.json`/`.mcp.json`
/// (`conway_plugin_claude::ClaudeCompatError`), or the translated MCP
/// server itself failing discovery (`conway_plugin_mcp::McpPluginError`) --
/// fails the WHOLE call as [`FacadeError::Build`], naming the offending
/// entry's own `id`, mirroring `subprocess_plugins::install`/
/// `mcp_plugins::install`'s own "an unresolvable entry fails the whole
/// build" posture for the same reason: an operator who named a directory in
/// `settings.json` and got nothing for it, silently, is exactly the rung-1
/// lie CONTRIBUTING's declaration rule exists to prevent. Appending
/// translated hook rules never itself fails this call -- `HookRegistration`
/// construction is infallible (`conway_plugin_claude::hooks::
/// HookTranslation::registration`'s own doc); any defect in the RESULT
/// (a duplicate id, an invalid `match`) surfaces later, at `build()`'s own
/// re-validation, exactly like an operator-authored `[hooks].rules[]` entry
/// with the same defect would.
pub async fn install(builder: ConwayBuilder) -> conway::Result<ConwayBuilder> {
    let entries = builder.config().plugins.claude_compat.clone();
    let mut builder = builder;
    for entry in entries {
        let report =
            conway_plugin_claude::discover(&entry.dir).map_err(|err| FacadeError::Build {
                message: format!("[plugins].claude_compat entry '{}': {err}", entry.id),
            })?;
        // Computed BEFORE the `mcp_servers` loop below moves that field out
        // of `report` -- `hook_registrations()` takes `&self`, which a
        // partially-moved `report` could no longer satisfy.
        let registrations = report.hook_registrations();
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

        if !registrations.is_empty() {
            report_hook_registrations(&entry.id, &registrations);
            let rules = registrations.into_iter().map(to_hook_entry);
            builder.config_mut().hooks.rules.extend(rules);
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

    // ---- hook-dispatch wiring (board item `01M0XBZNBPXEESX8VNTJDKNG0J`) ----

    use conway::config::schema::ClaudeCompatPluginEntry;
    use conway_core::hook::HookOnFailure;

    /// Writes `<dir>/hooks/hooks.json` with the given raw JSON contents --
    /// the identical fixture shape `conway_plugin_claude::hooks`'s own tests
    /// use, inlined here rather than shared across crates.
    fn write_hooks_json(dir: &std::path::Path, contents: &str) {
        std::fs::create_dir_all(dir.join("hooks")).unwrap();
        std::fs::write(dir.join("hooks").join("hooks.json"), contents).unwrap();
    }

    fn config_with_claude_compat_entry(dir: &std::path::Path) -> ConwayConfig {
        let mut config = minimal_config();
        config.plugins.claude_compat.push(ClaudeCompatPluginEntry {
            id: "acme-tools".to_string(),
            dir: dir.to_path_buf(),
            timeout_ms: 5_000,
        });
        config
    }

    /// **The headline claim this item exists to prove**: a `PreToolUse`
    /// rule in a directory's own `hooks/hooks.json` is not merely reported
    /// -- it is appended, real and dispatchable, into the SAME builder's
    /// `HooksConfig` `install` hands back, ready for `ConwayBuilder::build`
    /// to read. `crates/conway-cli/tests/hook_runner_wiring.rs` is the
    /// sibling end-to-end proof that an appended `pre_tool_use` rule
    /// actually denies a real tool call through the compiled binary; this
    /// test pins the wiring step that makes that reachable at all.
    #[tokio::test]
    async fn a_mapped_pre_tool_use_hook_is_appended_as_a_dispatchable_rule() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"echo pre"}]}]}}"#,
        );
        let builder = ConwayBuilder::from_parts(config_with_claude_compat_entry(dir.path()));
        let builder = install(builder).await.expect("install must succeed");

        let rules = &builder.config().hooks.rules;
        assert_eq!(rules.len(), 1, "exactly one mapped rule: {rules:?}");
        let rule = &rules[0];
        assert_eq!(rule.event, "pre_tool_use");
        assert_eq!(rule.match_tool.as_deref(), Some("Bash"));
        assert_eq!(rule.command[0], "/bin/sh");
        assert_eq!(rule.command[1], "-c");
        assert!(rule.command[2].contains("echo pre"));
        assert!(rule.enabled);
        assert!(
            rule.id.starts_with("claude_compat:"),
            "a translated rule's id must be namespaced: {}",
            rule.id
        );
    }

    /// **Guard rail, pinned directly**: a translated hook never sets
    /// `on_failure` itself -- it is left at [`HookEntry::default`]'s own
    /// `HookOnFailure::Deny`, the same fail-closed posture every existing
    /// `[hooks].rules[]` entry with no explicit `on_failure` already has.
    /// This module must never silently choose a foreign plugin's own
    /// failure posture on the operator's behalf.
    #[tokio::test]
    async fn a_translated_pre_tool_use_hook_carries_on_failure_deny() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"echo pre"}]}]}}"#,
        );
        let builder = ConwayBuilder::from_parts(config_with_claude_compat_entry(dir.path()));
        let builder = install(builder).await.expect("install must succeed");

        let rules = &builder.config().hooks.rules;
        assert_eq!(rules.len(), 1);
        assert_eq!(
            rules[0].on_failure,
            HookOnFailure::Deny,
            "a translated rule must default to Deny, never widen an operator-unreviewed outage \
             posture"
        );
    }

    /// A mapped, but non-`pre_tool_use`, event (`SessionStart` ->
    /// `session_starting`) is appended exactly like a `pre_tool_use` one --
    /// dispatch wiring does not discriminate by event, only the operator-
    /// visible reporting (`report_hook_registrations`) does.
    #[tokio::test]
    async fn a_mapped_session_starting_hook_is_also_appended() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"echo start"}]}]}}"#,
        );
        let builder = ConwayBuilder::from_parts(config_with_claude_compat_entry(dir.path()));
        let builder = install(builder).await.expect("install must succeed");

        let rules = &builder.config().hooks.rules;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].event, "session_starting");
    }

    /// An `Unmapped` rule (no conway counterpart -- `Stop` here) contributes
    /// no `HookEntry` at all: `hook_registrations()` already filters these
    /// out (they are named in `ClaudeCompatReport::unsupported` instead,
    /// read by a different module, per this file's own top doc).
    #[tokio::test]
    async fn an_unmapped_hook_appends_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo bye"}]}]}}"#,
        );
        let builder = ConwayBuilder::from_parts(config_with_claude_compat_entry(dir.path()));
        let builder = install(builder).await.expect("install must succeed");

        assert!(
            builder.config().hooks.rules.is_empty(),
            "an unmapped event must append nothing: {:?}",
            builder.config().hooks.rules
        );
    }

    /// A directory declaring both a deny-capable (`PreToolUse`) and an
    /// observation-only (`SessionStart`) rule appends BOTH -- proving
    /// `report_hook_registrations`'s deny/observation split (stderr-only) is
    /// purely a reporting distinction, never a filter on what actually gets
    /// wired.
    #[tokio::test]
    async fn a_directory_with_both_deny_capable_and_observation_only_hooks_appends_both() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{
                "PreToolUse":[{"hooks":[{"type":"command","command":"echo pre"}]}],
                "SessionStart":[{"hooks":[{"type":"command","command":"echo start"}]}]
            }}"#,
        );
        let builder = ConwayBuilder::from_parts(config_with_claude_compat_entry(dir.path()));
        let builder = install(builder).await.expect("install must succeed");

        let rules = &builder.config().hooks.rules;
        assert_eq!(
            rules.len(),
            2,
            "both mapped rules must be appended: {rules:?}"
        );
        assert!(rules.iter().any(|r| r.event == "pre_tool_use"));
        assert!(rules.iter().any(|r| r.event == "session_starting"));
    }

    /// Two `[plugins].claude_compat[]` entries, each declaring its own
    /// mapped hook, both land in the SAME `HooksConfig` -- `install`'s loop
    /// accumulates across entries rather than each entry silently
    /// overwriting the last (`Vec::extend`, never a re-assignment).
    #[tokio::test]
    async fn two_claude_compat_entries_accumulate_into_the_same_hooks_config() {
        // Each directory gets its OWN `.claude-plugin/plugin.json` `name` --
        // `HookRegistration::id` is namespaced by that manifest-derived
        // `ClaudeCompatReport::id`, not by the config entry's own `id`
        // (`ClaudeCompatPluginEntry::id`'s own doc: the two are allowed to
        // differ). Naming both here makes the id-namespacing assertion
        // below check something real rather than a random tempdir name.
        let dir_a = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir_a.path().join(".claude-plugin")).unwrap();
        std::fs::write(
            dir_a.path().join(".claude-plugin").join("plugin.json"),
            r#"{"name":"entry-a"}"#,
        )
        .unwrap();
        write_hooks_json(
            dir_a.path(),
            r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"echo a"}]}]}}"#,
        );
        let dir_b = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir_b.path().join(".claude-plugin")).unwrap();
        std::fs::write(
            dir_b.path().join(".claude-plugin").join("plugin.json"),
            r#"{"name":"entry-b"}"#,
        )
        .unwrap();
        write_hooks_json(
            dir_b.path(),
            r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"echo b"}]}]}}"#,
        );

        let mut config = minimal_config();
        config.plugins.claude_compat.push(ClaudeCompatPluginEntry {
            id: "entry-a".to_string(),
            dir: dir_a.path().to_path_buf(),
            timeout_ms: 5_000,
        });
        config.plugins.claude_compat.push(ClaudeCompatPluginEntry {
            id: "entry-b".to_string(),
            dir: dir_b.path().to_path_buf(),
            timeout_ms: 5_000,
        });

        let builder = ConwayBuilder::from_parts(config);
        let builder = install(builder).await.expect("install must succeed");

        let rules = &builder.config().hooks.rules;
        assert_eq!(rules.len(), 2, "one rule from each entry: {rules:?}");
        // Namespaced by each entry's own report id, so the two never
        // collide even though both name the identical Claude Code event.
        assert!(rules.iter().any(|r| r.id.contains("entry-a")));
        assert!(rules.iter().any(|r| r.id.contains("entry-b")));
    }
}
