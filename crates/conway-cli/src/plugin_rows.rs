//! The `/plugin` listing's row model, and the pure builders that turn each
//! plugin source's own already-loaded data into it -- lifted out of
//! `tui::view::plugins` (board item `01M1FSDRF20E2EGHCG3RK28DKH`) so a
//! headless caller (`commands::plugin`'s own `list`/`install`/`remove`) can
//! reuse the IDENTICAL row-building logic the TUI's `/plugin` command
//! renders, rather than re-deriving a second, independently-worded listing
//! that can drift from it.
//!
//! **Why this file exists, not just a wider `pub(crate)` on the old one.**
//! `tui::view::plugins` is a rendering module: its top-of-file `use
//! ratatui::...` imports, and everything downstream of [`PluginRow`] there
//! (`build_tree`'s `tui::view::menu::MenuNode` construction, `draw`'s
//! `ratatui::Frame`), are real UI concerns a one-shot subcommand has no
//! business depending on, even indirectly. The row MODEL and the row
//! BUILDERS, though, were always pure: `PluginRow`/`PluginOrigin`/
//! `PluginToggle` are plain data, and every `rows_from_*` function below
//! only ever reads already-resolved config-shaped input (a
//! [`PluginBrowserEntry`] slice, a [`ConfiguredPluginEntry`] slice, ...) and
//! returns a `Vec<PluginRow>` -- no ratatui type appears anywhere in this
//! file, and `cargo tree`/a plain `grep ratatui` over it will keep saying
//! so. Moving exactly that pure slice out is the seam: `tui::view::plugins`
//! keeps every ratatui-touching function (`build_tree`, `plugin_row_node`,
//! `selected_plugin_row`, `draw`, `draw_plugin_detail`, `modal_rect`) and
//! now calls into this module for the rows themselves, and
//! `commands::plugin` calls the identical functions directly, with no TUI
//! module anywhere on its own call stack.
//!
//! **Why `PluginBrowserEntry`/`ConfiguredPluginEntry`/
//! `ClaudeCompatPluginEntry` themselves stay defined in [`crate::tui::
//! state`], imported here rather than moved too.** Each one is a plain data
//! struct with no ratatui dependency of its own (this module's own doc
//! above already establishes that "no ratatui" is the actual bar, not
//! "not defined under `tui/`") -- but `AppState` (also in that module) owns
//! them as real, mutated fields: `plugin_browser`'s `installed` flips on a
//! successful toggle, and every one of them is POPULATED once at `App::new`
//! from a live `Conway`/config read that only the TUI's own startup path
//! performs today. Relocating the type definitions themselves would touch
//! every one of the ~9 files across this crate that already name
//! `crate::tui::state::PluginBrowserEntry` (`startup.rs`, `plugin_toggle.rs`,
//! `claude_compat_plugins.rs`, `tui/commands.rs`, `tui/view/settings.rs`,
//! plus their own tests) for a purely cosmetic win -- this module already
//! proves the functional property that actually matters (a headless caller
//! reaches the row builders with no ratatui in its dependency graph)
//! without that churn. [`crate::commands::plugin`]'s own `list` builds its
//! OWN `Vec<PluginBrowserEntry>` (mirroring `tui::app::startup`'s identical
//! construction from `first_party_plugins::all_bundle_plugins` +
//! `conway.config().plugins.install`) and hands it to
//! [`rows_from_plugin_browser`] below -- the type is shared, only its one
//! populator differs per caller, exactly as intended.

use crate::tui::state::{ClaudeCompatPluginEntry, ConfiguredPluginEntry, PluginBrowserEntry};

/// An open set of plugin sources -- see `tui::view::plugins`' own doc,
/// "The row model", for why this is a label wrapper rather than a closed
/// enum (unchanged by this move).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PluginOrigin(pub(crate) &'static str);

impl PluginOrigin {
    /// `first_party_plugins::all_bundle_plugins` -- compiled into this
    /// binary, selected via `[plugins].install`.
    pub(crate) const COMPILED_IN: PluginOrigin = PluginOrigin("compiled-in");
    /// `[plugins].subprocess[]` -- an operator-named command speaking
    /// conway's own wire protocol.
    pub(crate) const SUBPROCESS: PluginOrigin = PluginOrigin("subprocess");
    /// `[plugins].mcp[]` -- an operator-named command speaking MCP
    /// (JSON-RPC 2.0) as a client.
    pub(crate) const MCP: PluginOrigin = PluginOrigin("mcp");
    /// `[plugins].claude_compat[]` -- a Claude Code plugin directory read
    /// off disk and translated (board item `01M0VR89FB1F3Q4FQ8852K2A5E`).
    pub(crate) const CLAUDE_COMPAT: PluginOrigin = PluginOrigin("claude-compat");

    pub(crate) fn label(self) -> &'static str {
        self.0
    }
}

/// Whether a [`PluginRow`] responds to a toggle at all, and if not, why.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PluginToggle {
    /// A compiled-in plugin's own `[plugins].install` membership --
    /// `installed` is the CURRENT state, mirroring [`PluginBrowserEntry::
    /// installed`].
    Toggleable { installed: bool },
    /// No control exists here for this row -- `reason` names why.
    ReadOnly { reason: &'static str },
}

/// One row of the `/plugin` listing: `(identity, origin, what it can
/// contribute, is it active)` -- see `tui::view::plugins`' own doc, "The
/// row model", for why `origin` is an open set and `contributes` is honest
/// per-kind rather than padded to look uniform.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PluginRow {
    pub(crate) id: String,
    pub(crate) origin: PluginOrigin,
    /// A one-line, honest statement of what this plugin contributes --
    /// verbatim for a compiled-in plugin's own `PluginDescription::summary`
    /// (curated by that plugin's own author), or the closed wire
    /// vocabulary its transport bridges for kinds 2/3.
    pub(crate) contributes: String,
    /// Whether this plugin is currently running in this process. Always
    /// `true` for subprocess/MCP (installed unconditionally, no candidate
    /// set); mirrors [`PluginBrowserEntry::installed`] for compiled-in.
    pub(crate) active: bool,
    pub(crate) toggle: PluginToggle,
    /// The full "you get"/"you lose"/"costs" breakdown, when one exists --
    /// only a compiled-in plugin carries a real [`conway::plugin::
    /// PluginDescription`] to show one from; `None` for subprocess/MCP
    /// rows, whose [`Self::contributes`] is already the whole story.
    pub(crate) description: Option<conway::plugin::PluginDescription>,
}

const READ_ONLY_SUBPROCESS_REASON: &str =
    "installed unconditionally from [plugins].subprocess in settings.json -- \
     no per-entry control here; edit settings.json to remove it";
const READ_ONLY_MCP_REASON: &str = "installed unconditionally from [plugins].mcp in settings.json \
     -- no per-entry control here; edit settings.json to remove it";
/// Board item `01M0VR89FB1F3Q4FQ8852K2A5E`: a claude-compat directory is
/// re-read fresh every startup (read-at-runtime, never written to config)
/// -- there is nothing here TO toggle, the same "no candidate set" shape
/// `READ_ONLY_SUBPROCESS_REASON`/`READ_ONLY_MCP_REASON` already state, for
/// the same reason.
const READ_ONLY_CLAUDE_COMPAT_REASON: &str =
    "read-at-runtime from a directory named in [plugins].claude_compat in settings.json -- \
     no per-entry control here; run `/plugin uninstall <id>` (or edit settings.json directly) \
     to remove it";

/// Subprocess wire vocabulary, cited from `conway_plugin_subprocess::
/// wire`'s own `initialize` point list (`crates/conway-plugin-subprocess/
/// src/wire.rs`): `tool/1`, `permission.policy/1`, `observe/1`,
/// `status.declare/1`. Stated as a compile-time constant, not discovered by
/// spawning the entry -- this listing never spawns anything (out of
/// scope).
const SUBPROCESS_CONTRIBUTES: &str =
    "tools, permission policy, observation, status (the wire points a subprocess plugin may bridge)";

/// MCP bridges tools only -- `conway_plugin_mcp::McpPlugin`'s `Plugin` impl
/// has exactly one non-manifest method, `tools()`; no `commands`,
/// `permission_evaluator`, or hook override.
const MCP_CONTRIBUTES: &str = "tools only";

pub(crate) fn rows_from_plugin_browser(entries: &[PluginBrowserEntry]) -> Vec<PluginRow> {
    entries
        .iter()
        .map(|entry| PluginRow {
            id: entry.id.clone(),
            origin: PluginOrigin::COMPILED_IN,
            contributes: non_empty_or(&entry.description.summary, "(no description)").to_string(),
            active: entry.installed,
            toggle: PluginToggle::Toggleable {
                installed: entry.installed,
            },
            description: Some(entry.description.clone()),
        })
        .collect()
}

pub(crate) fn rows_from_subprocess(entries: &[ConfiguredPluginEntry]) -> Vec<PluginRow> {
    entries
        .iter()
        .map(|entry| PluginRow {
            id: entry.id.clone(),
            origin: PluginOrigin::SUBPROCESS,
            contributes: SUBPROCESS_CONTRIBUTES.to_string(),
            active: true,
            toggle: PluginToggle::ReadOnly {
                reason: READ_ONLY_SUBPROCESS_REASON,
            },
            description: None,
        })
        .collect()
}

pub(crate) fn rows_from_mcp(entries: &[ConfiguredPluginEntry]) -> Vec<PluginRow> {
    entries
        .iter()
        .map(|entry| PluginRow {
            id: entry.id.clone(),
            origin: PluginOrigin::MCP,
            contributes: MCP_CONTRIBUTES.to_string(),
            active: true,
            toggle: PluginToggle::ReadOnly {
                reason: READ_ONLY_MCP_REASON,
            },
            description: None,
        })
        .collect()
}

/// Board item `01M0VR89FB1F3Q4FQ8852K2A5E`: the fourth source. See
/// `tui::view::plugins`' own (pre-move) doc for the full "acceptance 5's
/// naming lives ON THE ROW itself" reasoning -- unchanged by this move.
fn rows_from_claude_compat(entries: &[ClaudeCompatPluginEntry]) -> Vec<PluginRow> {
    entries
        .iter()
        .map(|entry| {
            let total_hooks = entry.mapped_hook_count + entry.unmapped_hook_names.len();
            let mut contributes = format!(
                "{} mcp server(s) translated (tools only)",
                entry.mcp_server_count
            );
            if total_hooks > 0 {
                let observation_only_hooks =
                    entry.mapped_hook_count - entry.deny_capable_hook_count;
                contributes.push_str(&format!(
                    "; hooks {}/{} mapped and dispatching ({} deny-capable, {} observation-only)",
                    entry.mapped_hook_count,
                    total_hooks,
                    entry.deny_capable_hook_count,
                    observation_only_hooks
                ));
            }
            if !entry.unmapped_hook_names.is_empty() {
                contributes.push_str(&format!(
                    "; unmapped hooks: {}",
                    bounded_name_list(&entry.unmapped_hook_names)
                ));
            }
            if !entry.unsupported_names.is_empty() {
                contributes.push_str(&format!(
                    "; not imported: {}",
                    bounded_name_list(&entry.unsupported_names)
                ));
            }
            let you_get = if entry.mcp_server_count == 0 {
                "no .mcp.json server declarations found -- nothing translated".to_string()
            } else {
                format!(
                    "{} mcp server(s) translated into real, running plugins (tools only)",
                    entry.mcp_server_count
                )
            };
            let you_lose =
                if entry.unmapped_hook_names.is_empty() && entry.unsupported_names.is_empty() {
                    "nothing else found in this directory".to_string()
                } else {
                    let mut parts = Vec::new();
                    if !entry.unmapped_hook_names.is_empty() {
                        parts.push(format!(
                            "hook event(s) with no conway counterpart: {}",
                            entry.unmapped_hook_names.join(", ")
                        ));
                    }
                    if !entry.unsupported_names.is_empty() {
                        parts.push(format!(
                            "not imported: {}",
                            entry.unsupported_names.join(", ")
                        ));
                    }
                    parts.join("; ")
                };
            let costs = format!(
                "everything under {} runs/reads with your own privileges, unsandboxed \
                 (same trust footing as [plugins].mcp/[plugins].subprocess)",
                entry.source_dir.display()
            );
            PluginRow {
                id: entry.id.clone(),
                origin: PluginOrigin::CLAUDE_COMPAT,
                contributes,
                active: true,
                toggle: PluginToggle::ReadOnly {
                    reason: READ_ONLY_CLAUDE_COMPAT_REASON,
                },
                description: Some(conway::plugin::PluginDescription {
                    summary: format!(
                        "Claude Code plugin directory: {}",
                        entry.source_dir.display()
                    ),
                    you_get,
                    you_lose,
                    costs,
                }),
            }
        })
        .collect()
}

/// The most names [`rows_from_claude_compat`] prints verbatim on a row
/// before falling back to a "+N more" tail.
const MAX_NAMED_ITEMS: usize = 4;

fn bounded_name_list(names: &[String]) -> String {
    if names.len() <= MAX_NAMED_ITEMS {
        names.join(", ")
    } else {
        format!(
            "{}, +{} more",
            names[..MAX_NAMED_ITEMS].join(", "),
            names.len() - MAX_NAMED_ITEMS
        )
    }
}

/// Every plugin conway can run today, from every source -- the pure builder
/// [`crate::tui::view::plugins::build_tree`]/[`crate::tui::view::plugins::
/// modal_rect`] call against the live `AppState`'s own four source slices.
/// Takes the four sources directly rather than `&AppState` (this module's
/// own top doc explains why: `AppState` is a TUI-owned type this module
/// must not depend on) -- a caller with only ONE source populated (e.g.
/// `commands::plugin::list`, compiled-in only) passes empty slices for the
/// other three, which is exactly what an empty `&[]` already means: no rows
/// from that source.
pub(crate) fn all_plugin_rows(
    plugin_browser: &[PluginBrowserEntry],
    subprocess_plugins: &[ConfiguredPluginEntry],
    mcp_plugins: &[ConfiguredPluginEntry],
    claude_compat_plugins: &[ClaudeCompatPluginEntry],
) -> Vec<PluginRow> {
    let mut rows = Vec::new();
    rows.extend(rows_from_plugin_browser(plugin_browser));
    rows.extend(rows_from_subprocess(subprocess_plugins));
    rows.extend(rows_from_mcp(mcp_plugins));
    rows.extend(rows_from_claude_compat(claude_compat_plugins));
    rows
}

pub(crate) fn non_empty_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() {
        fallback
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiled_in(id: &str, installed: bool, summary: &str) -> PluginBrowserEntry {
        PluginBrowserEntry {
            id: id.to_string(),
            version: "0.9.0".to_string(),
            installed,
            description: conway::plugin::PluginDescription {
                summary: summary.to_string(),
                you_get: "you-get-text".to_string(),
                you_lose: "you-lose-text".to_string(),
                costs: "costs-text".to_string(),
            },
        }
    }

    /// The pure builder itself, exercised with no `AppState` in sight --
    /// this is the property the whole move exists to prove: a caller that
    /// only ever has a `Vec<PluginBrowserEntry>` in hand (never a live
    /// `AppState`) still gets the identical row shape the TUI renders.
    #[test]
    fn rows_from_plugin_browser_alone_needs_no_app_state() {
        let entries = vec![
            compiled_in("conway.memory", true, "notes"),
            compiled_in("conway.skills", false, "skill index"),
        ];
        let rows = rows_from_plugin_browser(&entries);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "conway.memory");
        assert!(rows[0].active);
        assert_eq!(rows[0].toggle, PluginToggle::Toggleable { installed: true });
        assert!(!rows[1].active);
        assert_eq!(rows[1].contributes, "skill index");
    }

    #[test]
    fn all_plugin_rows_with_empty_other_sources_matches_plugin_browser_alone() {
        let entries = vec![compiled_in("conway.memory", true, "notes")];
        let via_all = all_plugin_rows(&entries, &[], &[], &[]);
        let via_direct = rows_from_plugin_browser(&entries);
        assert_eq!(via_all, via_direct);
    }
}
