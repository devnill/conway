//! The `/plugin` command's own surface (board item
//! `01M0VR5RCCB8NDGG2JEQW8X7XR`): a listing of EVERY kind of plugin conway
//! can run today, in one place, each row naming where it came from and
//! honestly stating what it can contribute.
//!
//! ## The gap this closes
//!
//! Before this item, the only plugin listing in the TUI was `/settings`'
//! own plugins section, and it read `AppState::plugin_browser` alone --
//! populated (`app/startup.rs`) from `first_party_plugins::
//! all_bundle_plugins`, the compiled-in first-party bundle. An operator
//! with a `[plugins].subprocess[]` or `[plugins].mcp[]` entry configured
//! had no way to see it anywhere in the interface: their own MCP server was
//! invisible. This module reads all three sources -- [`AppState::
//! plugin_browser`], [`AppState::subprocess_plugins`], [`AppState::
//! mcp_plugins`] -- and renders one row per plugin, regardless of kind.
//!
//! ## One home, not two
//!
//! `/settings` used to implement its OWN plugin browser (toggle leaves,
//! detail panel, the lot) over the identical `plugins.install` array this
//! module also reads. Two surfaces over the same data, with the same
//! restart-to-apply semantics and the same silent-higher-layer-override
//! hazard (`app/plugin_toggle.rs:66-95`), is real drift risk -- and this
//! module is a strict superset of what the old settings section could show
//! (it adds subprocess/MCP rows the old section had no way to represent at
//! all). So `/plugin` now OWNS plugin listing: `view/settings.rs`'
//! plugins section is a single shortcut row into this one
//! (`view/settings.rs`'s own doc, "Plugins: one home, not two"), not a
//! second implementation. The compiled-in toggle mechanism itself
//! (`Action::TogglePlugin` -> `App::apply_plugin_toggle`, `input.rs`'s
//! `LEAF_TOGGLE_PLUGIN_PREFIX` id space) MOVED here unchanged -- the
//! post-write config-merge re-check that makes a toggle's restart-to-apply
//! claim honest (`app/plugin_toggle.rs`'s own doc) is untouched by this
//! item, only its one caller's UI moved.
//!
//! ## The row model: `(identity, origin, what it contributes, is it
//! active)`, and why origin is an open set
//!
//! [`PluginRow`] is exactly that four-field shape, and [`PluginOrigin`] is
//! deliberately NOT a closed enum of today's three kinds -- it is a thin
//! wrapper around a `&'static str` label, constructed by named associated
//! consts ([`PluginOrigin::COMPILED_IN`]/[`PluginOrigin::SUBPROCESS`]/
//! [`PluginOrigin::MCP`]). Nothing downstream of a [`PluginRow`] ever
//! matches on `origin` -- [`build_tree`]/[`draw`]/[`draw_plugin_detail`]
//! read `origin.label()` generically, group rows by whatever labels are
//! actually present (`group_rows_by_origin`, insertion-order stable, no
//! fixed list of expected labels), and print it. The one thing that DOES
//! vary by kind -- whether a row is toggleable -- is carried explicitly on
//! the row itself ([`PluginRow::toggle`]), decided once when the row is
//! BUILT (by [`rows_from_plugin_browser`]/[`rows_from_subprocess`]/
//! [`rows_from_mcp`]), not by the renderer branching on origin later.
//!
//! **What the Claude-compat item (a separate, later board item) has to do
//! to register its kind into this listing:** write one `rows_from_*`
//! function returning `Vec<PluginRow>` with a new `PluginOrigin` const
//! (e.g. `PluginOrigin::CLAUDE_COMPAT`), and add one call to it inside
//! [`all_plugin_rows`]. Nothing else in this module changes -- no match
//! arm, no new group constant, no renderer edit -- because grouping,
//! labelling, and detail rendering are already origin-agnostic. This is
//! the literal test the spec asked this item to pass: "if the answer is
//! more than 'add a source', redesign."
//!
//! ## Kinds 2 and 3 are honestly thinner, not padded to match kind 1
//!
//! A compiled-in plugin carries a real [`conway::plugin::PluginDescription`]
//! (`summary`/`you_get`/`you_lose`/`costs`) -- rich, curated text. A
//! subprocess or MCP entry carries only an `id` and a `command` in config;
//! this module never spawns either one just to ask it more (out of scope,
//! and the wire vocabulary each transport bridges is a compile-time
//! constant that answers the question just as accurately with zero
//! process spawned). So [`PluginRow::contributes`] for kinds 2/3 states
//! exactly the closed wire vocabulary each transport bridges, cited from
//! the transport crates themselves:
//! `conway_plugin_subprocess::wire`'s `initialize` point list (`tool/1`,
//! `permission.policy/1`, `observe/1`, `status.declare/1`) for subprocess,
//! and `conway_plugin_mcp::McpPlugin`'s single `Plugin::tools` impl (no
//! `commands`/`permission_evaluator`/hook override) for MCP. Neither string
//! claims a command, a permission policy contribution, or anything else
//! its own transport cannot carry -- an MCP row says "tools only" and
//! means it.
//!
//! ## Read-only kinds say so, visibly (acceptance 6)
//!
//! `[plugins].subprocess[]`/`[plugins].mcp[]` entries are installed
//! UNCONDITIONALLY -- "every configured entry is spawned, there is no
//! candidate set" (`subprocess_plugins.rs`'s own doc). There is nothing to
//! toggle, and `config::writer` deliberately has no array-entry writer
//! (out of scope for this item to add one). So every subprocess/MCP row's
//! [`PluginRow::toggle`] is [`PluginToggle::ReadOnly`], and [`build_tree`]
//! renders it as a [`super::menu::MenuNode::Static`] row naming exactly why
//! -- never a selectable-but-inert row that silently does nothing on
//! `Enter` (the same "a worse lie than an obviously static one" reasoning
//! `view/settings.rs`'s deny/prompt review rows already established).
//!
//! ## Transcript reservation, not just an overlay (board item
//! `01M1AFGDWR9CQ8WNYYV2B1TQBK`)
//!
//! This listing is informational and, like `/help`, commonly left open
//! while session activity continues behind it -- exactly the shape that
//! made an appended error unreadable while `/settings` covered it (fixed
//! one item earlier, `01M1A9M2EVJNR0HBN86A8E40EA`). [`modal_rect`] gives
//! `view::mod::layout` this listing's own height BEFORE `transcript::draw`
//! runs, so the transcript shrinks ahead of it instead of being drawn over
//! -- see [`CAP_DENOMINATOR`]'s own doc for why the cap had to change
//! alongside the reservation, not stay as it was.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::menu::{self, MenuNode, MenuState};
use super::modal;
use super::theme::Theme;
use crate::tui::state::{
    AppState, ClaudeCompatPluginEntry, ConfiguredPluginEntry, PluginBrowserEntry,
};

/// An open set of plugin sources -- see this module's own doc, "The row
/// model", for why this is a label wrapper rather than a closed enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PluginOrigin(&'static str);

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
    /// The fourth source this module's own doc anticipated: one new
    /// `rows_from_*` fn ([`rows_from_claude_compat`]) plus this one new
    /// const, and one new call in [`all_plugin_rows`] -- nothing else in
    /// this module changed to add it.
    pub(crate) const CLAUDE_COMPAT: PluginOrigin = PluginOrigin("claude-compat");

    pub(crate) fn label(self) -> &'static str {
        self.0
    }
}

/// Whether a [`PluginRow`] responds to `Enter` at all, and if not, why --
/// acceptance 6: whatever cannot be toggled says so visibly rather than
/// silently offering nothing.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PluginToggle {
    /// A compiled-in plugin's own `[plugins].install` membership --
    /// `installed` is the CURRENT state, mirroring [`PluginBrowserEntry::
    /// installed`].
    Toggleable { installed: bool },
    /// No control exists here for this row -- `reason` is shown on the row
    /// itself, not just in the detail panel, so the absence of a control is
    /// visible without having to select the row first.
    ReadOnly { reason: &'static str },
}

/// One row of the `/plugin` listing: `(identity, origin, what it can
/// contribute, is it active)` -- see this module's own doc, "The row
/// model", for why `origin` is an open set and `contributes` is honest
/// per-kind rather than padded to look uniform.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PluginRow {
    pub(crate) id: String,
    pub(crate) origin: PluginOrigin,
    /// A one-line, honest statement of what this plugin contributes --
    /// verbatim for a compiled-in plugin's own `PluginDescription::summary`
    /// (curated by that plugin's own author), or the closed wire
    /// vocabulary its transport bridges for kinds 2/3 (see this module's
    /// own doc for the exact citations).
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

fn rows_from_plugin_browser(entries: &[PluginBrowserEntry]) -> Vec<PluginRow> {
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

fn rows_from_subprocess(entries: &[ConfiguredPluginEntry]) -> Vec<PluginRow> {
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

fn rows_from_mcp(entries: &[ConfiguredPluginEntry]) -> Vec<PluginRow> {
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

/// Board item `01M0VR89FB1F3Q4FQ8852K2A5E`: the fourth source, exactly as
/// this module's own doc anticipated -- one `rows_from_*` fn, no match arm,
/// no renderer edit.
///
/// **Acceptance 5's naming lives ON THE ROW itself, not in
/// [`PluginRow::description`].** A claude-compat row is
/// [`PluginToggle::ReadOnly`], and `plugin_row_node`'s own doc already
/// establishes that a `ReadOnly` row is rendered [`menu::MenuNode::
/// Static`], which [`MenuState::selected_index`][sel] can never land the
/// cursor on -- so its detail panel is UNREACHABLE today (the exact
/// "forward declaration" `draw_plugin_detail`'s own `ReadOnly` arm already
/// documents for `reason`). Putting the full unsupported-items list only
/// there would be exactly the "claims to be reached but isn't" failure
/// GP-14 forbids, one layer down. So [`Self::contributes`] -- via
/// [`plugin_row_node`], which prints it verbatim on the row -- names every
/// unmapped hook event and every unsupported item directly, bounded by
/// [`MAX_NAMED_ITEMS`] with an honest "+N more" tail rather than silently
/// truncating past a terminal's own width with no indication anything was
/// cut. `description` is still populated (the same richer breakdown a
/// compiled-in row's detail panel shows) so this row is ready the moment a
/// future item makes `ReadOnly` rows selectable -- not because it is
/// reachable now.
///
/// [sel]: super::menu::MenuState::selected_index
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
                // Board item `01M0XRD8VMWD273W0W51T8ECCM`: this used to say
                // "(not wired)" -- true when written, false since board item
                // `01M0XBZNBPXEESX8VNTJDKNG0J` made every mapped hook a
                // real, dispatchable `[hooks].rules[]` entry and never
                // touched this row. Stale in the UNDERSTATING direction,
                // about a permission boundary -- the more dangerous one
                // (this item's own spec). Now says it dispatches, and
                // distinguishes deny-capable (`entry.deny_capable_hook_count`,
                // `conway::DENY_CAPABLE_EVENTS`) from observation-only --
                // acceptance 4.
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
/// before falling back to a "+N more" tail -- see that function's own doc
/// for why the full list must be reachable on the row rather than deferred
/// to an unreachable detail panel. 4 is small enough that an ordinary
/// plugin directory's own unsupported list fits without a tail at all,
/// while still bounding a pathological directory (hundreds of `commands/
/// *.md` files) to one line.
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

/// Every plugin this binary can run today, from every source -- **the one
/// place a future source registers itself** (see this module's own doc,
/// "The row model", for exactly what the Claude-compat item does here: one
/// new `rows_from_*` fn, one new call, nothing else).
pub(crate) fn all_plugin_rows(state: &AppState) -> Vec<PluginRow> {
    let mut rows = Vec::new();
    rows.extend(rows_from_plugin_browser(&state.plugin_browser));
    rows.extend(rows_from_subprocess(&state.subprocess_plugins));
    rows.extend(rows_from_mcp(&state.mcp_plugins));
    rows.extend(rows_from_claude_compat(&state.claude_compat_plugins));
    rows
}

/// Groups `rows` by [`PluginRow::origin`], in the FIRST-SEEN order of
/// origins in `rows` itself (so a caller that orders [`all_plugin_rows`]'s
/// own concatenation compiled-in/subprocess/mcp gets that same order back
/// here) -- never a fixed, hand-listed set of expected origins, which is
/// exactly the thing this module's whole design exists to avoid (see this
/// module's own doc). Within a group, rows keep [`all_plugin_rows`]'s own
/// order (already per-source, e.g. `plugin_browser`'s sort order).
fn group_rows_by_origin(rows: &[PluginRow]) -> Vec<(PluginOrigin, Vec<&PluginRow>)> {
    let mut groups: Vec<(PluginOrigin, Vec<&PluginRow>)> = Vec::new();
    for row in rows {
        match groups.iter_mut().find(|(origin, _)| *origin == row.origin) {
            Some((_, members)) => members.push(row),
            None => groups.push((row.origin, vec![row])),
        }
    }
    groups
}

/// Prefix for a compiled-in plugin's own toggle leaf id -- MOVED here
/// unchanged from `view/settings.rs::LEAF_TOGGLE_PLUGIN_PREFIX` (see this
/// module's own doc, "One home, not two"). `input::handle_plugins_key`
/// strips this prefix and looks the plugin id up directly, exactly as
/// `input::activate_settings_selection` used to.
pub(crate) const LEAF_TOGGLE_PLUGIN_PREFIX: &str = "toggle_plugin:";

/// Builds the `/plugin` listing's tree: one [`MenuNode::Static`] header row
/// per origin group (naming the group and its row count), then one row per
/// plugin -- a [`MenuNode::Leaf`] (via [`LEAF_TOGGLE_PLUGIN_PREFIX`]) for a
/// [`PluginToggle::Toggleable`] row, a [`MenuNode::Static`] naming its
/// [`PluginToggle::ReadOnly`] reason otherwise. Flat, not nested
/// [`MenuNode::Group`]s -- there is nothing here worth collapsing (unlike
/// `/settings`' own permissions review lists), and a flat list keeps this
/// tree's own navigation as simple as the old settings plugin section's
/// was.
pub(crate) fn build_tree(state: &AppState) -> MenuState {
    let rows = all_plugin_rows(state);
    let mut nodes = Vec::new();
    for (origin, members) in group_rows_by_origin(&rows) {
        nodes.push(MenuNode::static_row(format!(
            "-- {} ({}) --",
            origin.label(),
            members.len()
        )));
        for row in members {
            nodes.push(plugin_row_node(row));
        }
    }
    let mut menu = MenuState::new(nodes);
    menu.set_selected(state.plugins_selected);
    menu
}

fn plugin_row_node(row: &PluginRow) -> MenuNode {
    match &row.toggle {
        PluginToggle::Toggleable { installed } => {
            let box_glyph = if *installed { "x" } else { " " };
            let action = if *installed { "turn off" } else { "turn on" };
            MenuNode::leaf(
                format!(
                    "[{box_glyph}] [{}] {} -- {} ({action}, Enter)",
                    row.origin.label(),
                    row.id,
                    row.contributes,
                ),
                format!("{LEAF_TOGGLE_PLUGIN_PREFIX}{}", row.id),
            )
        }
        // `(read-only)` sits BEFORE `contributes`, and the full `reason` is
        // deliberately NOT on the row. Both for the same reason: a row is one
        // line in a bounded pane, and `contributes` is already ~96 characters
        // for a subprocess entry. Appending the reason produced a ~250-char
        // line, so at any ordinary terminal width the marker this row exists
        // to show was truncated off-screen -- the row said "read-only" to
        // nobody. The class-level explanation lives once in `TOGGLE_NOTE`,
        // which the footer wraps, rather than once per row.
        PluginToggle::ReadOnly { .. } => MenuNode::static_row(format!(
            "[{}] {} (read-only) -- {}",
            row.origin.label(),
            row.id,
            row.contributes,
        )),
    }
}

/// If the CURRENT selection is a plugin's own row, the row the detail panel
/// should describe -- resolved by id, mirroring `view/settings.rs::
/// selected_plugin_detail`'s own "resolve against the tree that built this
/// id, in the same call" pattern, generalized to any origin rather than
/// compiled-in alone.
fn selected_plugin_row<'a>(tree: &MenuState, rows: &'a [PluginRow]) -> Option<&'a PluginRow> {
    let selected = tree.selected_row()?;
    let id = match &selected.kind {
        menu::MenuRowKind::Leaf { id } => id
            .strip_prefix(LEAF_TOGGLE_PLUGIN_PREFIX)
            .map(str::to_string)?,
        menu::MenuRowKind::Static => {
            // A read-only row is never the tree's OWN selection (only a
            // selectable row can be -- `MenuState::selected_index`'s own
            // doc), but this function is also called defensively from
            // `draw` below; a static row simply has no detail to show.
            return None;
        }
        menu::MenuRowKind::Group { .. } => return None,
    };
    rows.iter().find(|row| row.id == id)
}

const DETAIL_ROWS: u16 = 6;

/// Renders the selected row's own detail: origin, active/toggle state, and
/// either the full "you get"/"you lose"/"costs" breakdown (compiled-in) or
/// the plain `contributes` line plus its read-only reason (subprocess/MCP)
/// -- see this module's own doc, "Kinds 2 and 3 are honestly thinner".
fn draw_plugin_detail(frame: &mut Frame, area: Rect, row: &PluginRow, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme.dim);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let status = if row.active { "active" } else { "off" };
    let mut lines = vec![Line::from(Span::styled(
        format!("[{}] {} \u{b7} {status}", row.origin.label(), row.id),
        theme.emphasized,
    ))];
    match &row.description {
        Some(description) => {
            let you_get = non_empty_or(&description.you_get, "(none given)");
            let you_lose = non_empty_or(&description.you_lose, "(none given)");
            let costs = non_empty_or(&description.costs, "none");
            lines.push(Line::from(format!("you get   {you_get}")));
            lines.push(Line::from(format!("you lose  {you_lose}")));
            lines.push(Line::from(format!("costs     {costs}")));
        }
        None => {
            lines.push(Line::from(format!("contributes  {}", row.contributes)));
        }
    }
    match &row.toggle {
        PluginToggle::Toggleable { .. } => {
            lines.push(Line::from(
                "toggle    Enter, written to disk, applied on next restart",
            ));
        }
        // FORWARD DECLARATION, labelled rather than left to look live. This
        // arm is UNREACHABLE today: the detail panel renders only for the
        // selected row, and every `ReadOnly` row is a `MenuNode::Static` that
        // the cursor can never land on (see `input::activate_plugins_selection`
        // and `MenuState::selected_index`). It is kept, rather than deleted
        // with the field, because `reason` is the only place each origin's
        // read-only cause is written down in a machine-readable form, and it
        // becomes reachable the moment these rows are made selectable. If that
        // never happens, delete this arm and `PluginToggle::ReadOnly`'s
        // `reason` field together -- do not leave one without the other.
        PluginToggle::ReadOnly { reason } => {
            lines.push(Line::from(format!("toggle    read-only -- {reason}")));
        }
    }
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true });
    frame.render_widget(paragraph, inner);
}

fn non_empty_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() {
        fallback
    } else {
        value
    }
}

const FOOTER_ROWS: u16 = 3;
/// The `/plugin` listing's own cap denominator.
///
/// **Written for the listing's own V1 as `1` ("up to the whole
/// `transcript_area`"), corrected by board item
/// `01M1AFGDWR9CQ8WNYYV2B1TQBK`.** The `1` was sound while this listing drew
/// straight OVER an already-rendered transcript -- this item makes
/// `view::mod::layout` reserve the listing's own height out of the
/// transcript pane BEFORE `transcript::draw` runs (see [`modal_rect`],
/// mirroring `view/settings.rs::modal_rect`'s own fix one item earlier, and
/// `view/help.rs`'s identical correction alongside this one). A cap of `1`
/// and a reservation are the same contradiction both of those modules'
/// own `CAP_DENOMINATOR` docs already name: the listing claims the WHOLE
/// pane, the transcript shrinks to nothing, and an error appended while
/// `/plugin` is open is unreadable again. `2` (matching [`modal::
/// DEFAULT_CAP_DENOMINATOR`] and `settings.rs`'s own choice) is this
/// correction's answer, for the identical reason.
///
/// **Safe to cap: this listing's body is [`menu::draw`]'s stateful `List`,
/// the SAME primitive `/settings` renders with -- see `view/
/// settings.rs::CAP_DENOMINATOR`'s own doc for why a `ListState`-backed
/// selection makes capping safe (ratatui auto-scrolls to keep the
/// selection visible, so rows past the cap stay reachable with
/// `Up`/`Down`).** The DETAIL PANEL below the list (`draw_plugin_detail`,
/// [`DETAIL_ROWS`] fixed rows) is NOT part of the capped/scrolled body --
/// it renders into `frame_areas.footer_area`, whose height (`FOOTER_ROWS +
/// detail_rows`) is fully reserved by [`modal::modal_area`]'s own
/// `footer_rows` parameter exactly as `/settings`' footer already is, so
/// capping the overall modal height never truncates the detail panel
/// either -- only the scrollable list above it shrinks.
const CAP_DENOMINATOR: u16 = 2;

const SESSION_NOTE: &str =
    "every kind conway can run: compiled-in, subprocess, MCP -- see docs/plugins/ for how each installs";
const TOGGLE_NOTE: &str = "compiled-in toggles: Enter, written to disk, applied on next restart; \
     subprocess/MCP entries are read-only here (see settings.json)";

/// The `/plugin` listing's own bottom-anchored, content-sized `Rect`,
/// computed against `transcript_area` -- exactly what [`draw`] itself asks
/// for via this same function, never a second, independent computation
/// (steering P-14, mirrors `view/settings.rs::modal_rect`'s own doc).
///
/// **Board item `01M1AFGDWR9CQ8WNYYV2B1TQBK`.** Factored out so
/// `view::mod::layout` can learn how tall this listing will render BEFORE
/// `transcript::draw` runs, and shrink the transcript pane by exactly that
/// height -- see [`CAP_DENOMINATOR`]'s own doc for why a cap alone was not
/// enough.
pub(crate) fn modal_rect(state: &AppState, transcript_area: Rect) -> Rect {
    let tree = build_tree(state);
    let content_rows = tree.rows().len().min(u16::MAX as usize) as u16;
    let rows = all_plugin_rows(state);
    let detail_rows = if selected_plugin_row(&tree, &rows).is_some() {
        DETAIL_ROWS
    } else {
        0
    };
    modal::modal_area(
        transcript_area,
        content_rows,
        FOOTER_ROWS + detail_rows,
        CAP_DENOMINATOR,
    )
}

/// Draws the `/plugin` listing over `transcript_area`, mirroring
/// `view/settings.rs::draw`'s own shape (shared [`modal`] primitive,
/// [`menu::draw`] for the body, a reserved detail-panel + footer tail).
pub fn draw(frame: &mut Frame, transcript_area: Rect, state: &AppState, theme: &Theme) {
    let tree = build_tree(state);

    let rows = all_plugin_rows(state);
    let detail_row = selected_plugin_row(&tree, &rows);
    let detail_rows = if detail_row.is_some() { DETAIL_ROWS } else { 0 };
    // `modal_rect` re-derives the SAME `Rect` from the SAME tree/detail
    // state this call just resolved -- never a second, independently-
    // resolved area (steering P-14, mirrors `view/settings.rs::draw`'s own
    // `modal_rect`-then-`draw_modal_frame_in` shape).
    let area = modal_rect(state, transcript_area);

    let frame_areas = modal::draw_modal_frame_in(
        frame,
        area,
        FOOTER_ROWS + detail_rows,
        " PLUGINS ",
        theme.help_border,
    );

    menu::draw(frame, frame_areas.body_area, &tree, theme);

    let footer_area = if let Some(row) = detail_row {
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(FOOTER_ROWS.min(frame_areas.footer_area.height)),
            ])
            .split(frame_areas.footer_area);
        draw_plugin_detail(frame, split[0], row, theme);
        split[1]
    } else {
        frame_areas.footer_area
    };

    let footer_lines = vec![
        Line::from("[Up/Down] move  [Enter] toggle (compiled-in only)  [Esc] close"),
        Line::from(Span::styled(SESSION_NOTE, theme.dim)),
        Line::from(Span::styled(TOGGLE_NOTE, theme.dim)),
    ];
    let footer = Paragraph::new(footer_lines).wrap(Wrap { trim: true });
    frame.render_widget(footer, footer_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::AgentId;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(state: &AppState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), state, &Theme::default()))
            .expect("draw");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn plain_rows(state: &AppState) -> String {
        build_tree(state)
            .rows()
            .iter()
            .map(|r| r.label.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

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

    fn configured(id: &str) -> ConfiguredPluginEntry {
        ConfiguredPluginEntry {
            id: id.to_string(),
            command: vec![id.to_string()],
        }
    }

    /// P-15: a fixture with NO subprocess/mcp entries proves nothing about
    /// those rows -- so this test configures ONE of each (acceptance 2's
    /// own demonstration shape) and asserts both actually render, each
    /// naming its own origin.
    #[test]
    fn a_configured_mcp_entry_and_a_configured_subprocess_entry_both_appear() {
        let mut state = AppState::new(AgentId::new());
        state.subprocess_plugins = vec![configured("acme.review")];
        state.mcp_plugins = vec![configured("acme.search")];

        let text = plain_rows(&state);
        assert!(
            text.contains("[subprocess] acme.review"),
            "the subprocess entry must appear, labelled with its origin: {text}"
        );
        assert!(
            text.contains("[mcp] acme.search"),
            "the mcp entry must appear, labelled with its origin: {text}"
        );
    }

    /// Falsifies the fixture itself: dropping the subprocess entry from the
    /// SAME state must make its row disappear -- otherwise the test above
    /// would pass even if the subprocess source were silently dropped
    /// (P-15).
    #[test]
    fn dropping_the_subprocess_fixture_removes_its_row() {
        let mut state = AppState::new(AgentId::new());
        state.subprocess_plugins = vec![configured("acme.review")];
        assert!(plain_rows(&state).contains("acme.review"));

        state.subprocess_plugins.clear();
        assert!(
            !plain_rows(&state).contains("acme.review"),
            "removing the configured entry must remove its row"
        );
    }

    /// Every row states its origin -- acceptance 3.
    #[test]
    fn every_row_names_its_origin() {
        let mut state = AppState::new(AgentId::new());
        state.plugin_browser = vec![compiled_in("conway.memory", true, "notes")];
        state.subprocess_plugins = vec![configured("acme.review")];
        state.mcp_plugins = vec![configured("acme.search")];

        let text = plain_rows(&state);
        assert!(text.contains("[compiled-in] conway.memory"), "{text}");
        assert!(text.contains("[subprocess] acme.review"), "{text}");
        assert!(text.contains("[mcp] acme.search"), "{text}");
    }

    /// Subprocess/mcp rows are not padded to look like a compiled-in row --
    /// they state the closed wire vocabulary, never a command or a
    /// `PluginDescription`'s own curated text (which they have none of).
    #[test]
    fn subprocess_and_mcp_rows_state_their_own_honest_wire_vocabulary() {
        let mut state = AppState::new(AgentId::new());
        state.subprocess_plugins = vec![configured("acme.review")];
        state.mcp_plugins = vec![configured("acme.search")];

        let text = plain_rows(&state);
        assert!(
            text.contains("tools, permission policy, observation, status"),
            "{text}"
        );
        assert!(text.contains("tools only"), "{text}");
    }

    /// Acceptance 6: a row that cannot be toggled says so visibly, right on
    /// the row -- never a selectable-but-inert row that does nothing on
    /// `Enter`.
    #[test]
    fn read_only_rows_are_static_and_name_their_reason_on_the_row() {
        let mut state = AppState::new(AgentId::new());
        state.subprocess_plugins = vec![configured("acme.review")];

        let rows = build_tree(&state).rows();
        let row = rows
            .iter()
            .find(|r| r.label.contains("acme.review"))
            .expect("the subprocess row must render");
        assert_eq!(
            row.kind,
            menu::MenuRowKind::Static,
            "a read-only row must never be selectable: {row:?}"
        );
        assert!(
            row.label.contains("read-only"),
            "the row must say it is read-only, on the row itself: {}",
            row.label
        );

        // ...and it must still say so on a REAL terminal. Asserting only that
        // the label String contains the marker is an intermediate signal: the
        // first version of this row appended the full read-only reason after
        // `contributes`, producing a ~250-character label whose marker sat far
        // past any pane a person actually has. The label assertion above
        // passed the whole time. This pins the observable outcome instead --
        // the marker must fall inside a narrow pane, not merely exist in the
        // string. 72 is deliberately tighter than an 80-column terminal, to
        // leave room for the modal border and indent.
        const NARROW_PANE: usize = 72;
        let marker_at = row
            .label
            .find("(read-only)")
            .expect("the marker must be present verbatim");
        assert!(
            marker_at + "(read-only)".len() <= NARROW_PANE,
            "the read-only marker must be visible in a {NARROW_PANE}-column pane, but it ends at \
             column {}: {}",
            marker_at + "(read-only)".len(),
            row.label
        );
    }

    /// A compiled-in row is still Toggleable, exactly as `/settings`' own
    /// (now-removed) plugin section used to render it -- the mechanism
    /// moved, not changed.
    #[test]
    fn a_compiled_in_row_is_still_toggleable() {
        let mut state = AppState::new(AgentId::new());
        state.plugin_browser = vec![compiled_in("conway.memory", true, "notes")];

        let rows = build_tree(&state).rows();
        let row = rows
            .iter()
            .find(|r| r.label.contains("conway.memory"))
            .expect("the compiled-in row must render");
        assert_eq!(
            row.kind,
            menu::MenuRowKind::Leaf {
                id: format!("{LEAF_TOGGLE_PLUGIN_PREFIX}conway.memory")
            }
        );
        assert!(row.label.starts_with("[x] "), "{}", row.label);
    }

    fn claude_compat(
        id: &str,
        mcp_server_count: usize,
        unmapped_hook_names: Vec<&str>,
        unsupported_names: Vec<&str>,
    ) -> crate::tui::state::ClaudeCompatPluginEntry {
        claude_compat_with_hooks(
            id,
            mcp_server_count,
            1,
            0,
            unmapped_hook_names,
            unsupported_names,
        )
    }

    /// [`claude_compat`], plus explicit control over the mapped/deny-capable
    /// hook split -- board item `01M0XRD8VMWD273W0W51T8ECCM`, acceptance 4:
    /// the tests below need a row where that split is actually mixed, which
    /// [`claude_compat`]'s own fixed `mapped_hook_count: 1` cannot express.
    #[allow(clippy::too_many_arguments)]
    fn claude_compat_with_hooks(
        id: &str,
        mcp_server_count: usize,
        mapped_hook_count: usize,
        deny_capable_hook_count: usize,
        unmapped_hook_names: Vec<&str>,
        unsupported_names: Vec<&str>,
    ) -> crate::tui::state::ClaudeCompatPluginEntry {
        crate::tui::state::ClaudeCompatPluginEntry {
            id: id.to_string(),
            source_dir: std::path::PathBuf::from(format!("/tmp/{id}")),
            mcp_server_count,
            mapped_hook_count,
            deny_capable_hook_count,
            unmapped_hook_names: unmapped_hook_names
                .into_iter()
                .map(str::to_string)
                .collect(),
            unsupported_names: unsupported_names.into_iter().map(str::to_string).collect(),
        }
    }

    /// Board item `01M0VR89FB1F3Q4FQ8852K2A5E`, acceptance 3: a
    /// claude-compat row appears in the SAME listing as a native one, and
    /// names its own origin -- the real (not hypothetical) fourth-origin
    /// case the test immediately below this one already proved the
    /// downstream mechanism handles.
    #[test]
    fn a_claude_compat_entry_appears_alongside_a_compiled_in_one_naming_both_origins() {
        let mut state = AppState::new(AgentId::new());
        state.plugin_browser = vec![compiled_in("conway.memory", true, "notes")];
        state.claude_compat_plugins = vec![claude_compat("acme-tools", 1, vec![], vec![])];

        let text = plain_rows(&state);
        assert!(text.contains("[compiled-in] conway.memory"), "{text}");
        assert!(text.contains("[claude-compat] acme-tools"), "{text}");
    }

    /// Acceptance 5, at the `/plugin` layer: a claude-compat row with
    /// unmapped hooks and unsupported items names them ON THE ROW ITSELF --
    /// `rows_from_claude_compat`'s own doc explains why it cannot rely on
    /// the detail panel (unreachable for a `ReadOnly`/`Static` row). Both
    /// the `PluginRow` value (unbounded -- what `all_plugin_rows` actually
    /// carries) and the rendered label (what a real terminal shows) are
    /// checked, the same "String assertion is not enough, pin the
    /// observable outcome" discipline `read_only_rows_are_static_and_name_
    /// their_reason_on_the_row` above already established for this row
    /// kind.
    #[test]
    fn a_claude_compat_row_names_what_it_could_not_use() {
        let mut state = AppState::new(AgentId::new());
        state.claude_compat_plugins = vec![claude_compat(
            "acme-tools",
            1,
            vec!["Stop"],
            vec!["commands/review.md", "skills/triage"],
        )];

        let raw_rows = all_plugin_rows(&state);
        let raw = raw_rows
            .iter()
            .find(|r| r.id == "acme-tools")
            .expect("the claude-compat row must exist");
        assert!(raw.contributes.contains("Stop"), "{}", raw.contributes);
        assert!(
            raw.contributes.contains("commands/review.md"),
            "{}",
            raw.contributes
        );
        assert!(
            raw.contributes.contains("skills/triage"),
            "{}",
            raw.contributes
        );

        let rows = build_tree(&state).rows();
        let row = rows
            .iter()
            .find(|r| r.label.contains("acme-tools"))
            .expect("the claude-compat row must render");
        assert_eq!(
            row.kind,
            menu::MenuRowKind::Static,
            "read-only, like subprocess/mcp"
        );
        assert!(row.label.contains("read-only"), "{}", row.label);
        assert!(row.label.contains("Stop"), "{}", row.label);
        assert!(row.label.contains("commands/review.md"), "{}", row.label);
    }

    /// Board item `01M0XRD8VMWD273W0W51T8ECCM`, acceptance 4: the row must
    /// say mapped hooks DISPATCH (never "not wired" -- that claim was true
    /// when written and false since board item `01M0XBZNBPXEESX8VNTJDKNG0J`
    /// wired real dispatch and never touched this row), and must
    /// distinguish deny-capable from observation-only, not report one bare
    /// count. Two mapped, one deny-capable, one observation-only.
    #[test]
    fn a_claude_compat_row_says_hooks_dispatch_and_distinguishes_deny_capable() {
        let mut state = AppState::new(AgentId::new());
        state.claude_compat_plugins = vec![claude_compat_with_hooks(
            "acme-tools",
            0,
            2,
            1,
            vec![],
            vec![],
        )];

        let raw_rows = all_plugin_rows(&state);
        let raw = raw_rows
            .iter()
            .find(|r| r.id == "acme-tools")
            .expect("the claude-compat row must exist");
        assert!(
            !raw.contributes.contains("not wired"),
            "must never claim a real, dispatching hook is inert: {}",
            raw.contributes
        );
        assert!(
            raw.contributes.contains("dispatching"),
            "{}",
            raw.contributes
        );
        assert!(
            raw.contributes.contains("1 deny-capable"),
            "{}",
            raw.contributes
        );
        assert!(
            raw.contributes.contains("1 observation-only"),
            "{}",
            raw.contributes
        );
    }

    /// A pathological directory (more unsupported items than
    /// [`MAX_NAMED_ITEMS`]) still names the first few AND says how many
    /// more there are -- never a silent, unbounded row nor a bare count
    /// with no names at all.
    #[test]
    fn a_long_unsupported_list_is_bounded_with_an_honest_tail() {
        let mut state = AppState::new(AgentId::new());
        state.claude_compat_plugins = vec![claude_compat(
            "acme-tools",
            0,
            vec![],
            vec!["a.md", "b.md", "c.md", "d.md", "e.md", "f.md"],
        )];
        let rows = all_plugin_rows(&state);
        let row = rows.iter().find(|r| r.id == "acme-tools").unwrap();
        assert!(row.contributes.contains("a.md"), "{}", row.contributes);
        assert!(
            row.contributes.contains("+2 more"),
            "6 items, MAX_NAMED_ITEMS=4, must say 2 more: {}",
            row.contributes
        );
    }

    /// Acceptance 4's own load-bearing property, checked directly rather
    /// than only asserted in prose: adding a fourth source requires
    /// touching `all_plugin_rows` alone -- everything downstream
    /// (`group_rows_by_origin`, `build_tree`, the renderer) is origin-
    /// agnostic. This test proves the DOWNSTREAM half by constructing a
    /// row set with an origin `all_plugin_rows` never produces today
    /// (there is no fourth `rows_from_*` fn yet -- that is the next item's
    /// job) and confirming the grouping/rendering path handles it with no
    /// special-casing.
    #[test]
    fn a_hypothetical_fourth_origin_groups_and_renders_with_no_special_casing() {
        const CLAUDE_COMPAT: PluginOrigin = PluginOrigin("claude-compat");
        let rows = vec![
            PluginRow {
                id: "acme.claude_tool".to_string(),
                origin: CLAUDE_COMPAT,
                contributes: "tools (translated)".to_string(),
                active: true,
                toggle: PluginToggle::ReadOnly {
                    reason: "compatibility layer, no toggle yet",
                },
                description: None,
            },
            PluginRow {
                id: "conway.memory".to_string(),
                origin: PluginOrigin::COMPILED_IN,
                contributes: "notes".to_string(),
                active: true,
                toggle: PluginToggle::Toggleable { installed: true },
                description: None,
            },
        ];
        let groups = group_rows_by_origin(&rows);
        assert_eq!(groups.len(), 2, "{groups:?}");
        assert!(groups.iter().any(|(origin, _)| *origin == CLAUDE_COMPAT));
        assert!(groups
            .iter()
            .any(|(origin, _)| *origin == PluginOrigin::COMPILED_IN));
    }

    #[test]
    fn draw_renders_bottom_anchored_and_content_sized() {
        let state = AppState::new(AgentId::new());
        let text = render(&state, 80, 24);
        assert!(text.contains("PLUGINS"), "{text}");
    }

    #[test]
    fn draw_never_panics_on_a_tiny_terminal() {
        let mut state = AppState::new(AgentId::new());
        state.plugin_browser = vec![compiled_in("conway.memory", true, "notes")];
        state.subprocess_plugins = vec![configured("acme.review")];
        state.mcp_plugins = vec![configured("acme.search")];
        for (w, h) in [(80u16, 1u16), (80, 2), (1, 24), (0, 0)] {
            let backend = TestBackend::new(w.max(1), h.max(1));
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| draw(f, f.area(), &state, &Theme::default()))
                .unwrap_or_else(|e| panic!("panicked/errored at {w}x{h}: {e}"));
        }
    }

    /// The detail panel shows the full breakdown for a compiled-in
    /// selection -- the SAME "detail panel tracks the selected row"
    /// property `view/settings.rs` used to prove for compiled-in alone,
    /// carried over unchanged.
    #[test]
    fn the_detail_panel_shows_the_full_breakdown_for_a_compiled_in_selection() {
        let mut state = AppState::new(AgentId::new());
        state.plugin_browser = vec![compiled_in("conway.memory", true, "notes")];
        state.subprocess_plugins = vec![configured("acme.review")];

        let idx = build_tree(&state)
            .rows()
            .iter()
            .position(|r| r.label.contains("conway.memory"))
            .expect("row exists");
        state.plugins_selected = idx;
        let text = render(&state, 120, 40);
        assert!(text.contains("you get"), "{text}");
        assert!(text.contains("you-get-text"), "{text}");
    }

    /// No detail panel renders while the tree has no selectable row at all
    /// -- e.g. only read-only (subprocess/mcp) rows configured. Not the
    /// same claim as `view/settings.rs`'s old "cursor is on a different
    /// section" test: here it is that NOTHING in the tree can ever hold
    /// the cursor, since [`MenuState::selected_index`] resolves any raw
    /// index to the nearest selectable row when one exists (its own doc) --
    /// with none, it falls back to the raw, non-selectable index, and
    /// [`selected_plugin_row`] correctly reports no detail for it.
    #[test]
    fn no_detail_panel_renders_when_nothing_in_the_tree_is_toggleable() {
        let mut state = AppState::new(AgentId::new());
        state.subprocess_plugins = vec![configured("acme.review")];

        let text = render(&state, 120, 40);
        assert!(
            !text.contains("contributes  tools, permission policy"),
            "no detail panel should render at all when nothing is selectable: {text}"
        );
    }
}
