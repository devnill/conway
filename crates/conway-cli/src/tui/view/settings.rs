//! The `/settings` menu (V4, extended by board item
//! `01M0KARX71A64NTSYTDBVANVPF`'s plugins section): a mostly
//! session-only display-preferences tree drawn on V1's shared
//! [`super::modal`] + [`super::menu`] primitives -- the first real caller
//! of [`super::menu`], which existed only as an exercised-by-its-own-tests
//! primitive before this item (see that module's own doc).
//!
//! ## Session-only, not `settings.json` -- EXCEPT the plugins section
//!
//! Conway's config load (`conway::config::merge::load`) is a five-source
//! layered read; when this doc was first written it had no writer anywhere
//! outside test fixtures, so persisting a runtime toggle meant inventing
//! one, and answering "which LAYER gets written" (default/user/project/
//! env/CLI) had no good default answer. **That question is now answered**
//! (decision `01M0K8BAXJ6THVJAPK0JZ17VV6`: the user layer,
//! `~/.conway/settings.json`, unconditionally, `CONWAY_CONFIG_DIR`-
//! overridable), and `conway::config::writer::set_plugin_installed` is the
//! resulting writer -- see that module's own doc for why it is a targeted
//! text splice rather than a parse-mutate-reserialize round trip. The
//! **plugins** section (below) is therefore the one part of this menu with
//! real persistence behind it; display/tool-output/permissions stay
//! session-only exactly as before, changing `AppState` at runtime only,
//! the way the two slash commands display toggling replaced
//! (`/thinking`, `/timestamps` -- both REMOVED, not aliased, see
//! `commands.rs`'s parser) already did. [`SESSION_NOTE`]/
//! [`PLUGIN_TOGGLE_NOTE`] say so, on separate footer lines, on every
//! render; the one leaf with a real backing config key outside plugins
//! (`tool_preview_lines`) names it inline (see [`build_tree`]'s own doc) --
//! the other two display toggles have no config-key equivalent to point to
//! at all today, so they carry no such annotation.
//!
//! ## Content: a survey, not "every bool on `AppState`"
//!
//! Three settings, deliberately -- everything else on `AppState` is either
//! internal bookkeeping (scroll offsets, in-flight flags, palette state,
//! ...) or already has its own dedicated, better-fitting UI (`v` cycles
//! `/agents`' visibility filter in place; `Ctrl-E` expands/collapses tool
//! output in place). A setting earns a row here only if a user would
//! deliberately reach for it as a persistent-for-the-session DISPLAY
//! preference: `show_reasoning` (was `/thinking`), `show_timestamps` (was
//! `/timestamps`), and `tool_preview_lines` (previously config-only,
//! `[tui.tool_preview_lines]` -- this menu is the first place it becomes
//! reachable at runtime at all).
//!
//! ## Grouping
//!
//! Four top-level [`MenuNode::Group`]s -- "display" (the two booleans),
//! "tool output" (the one numeric setting), "permissions" (the mode,
//! plus allow/deny/prompt/hooks rule review as FOUR SUB-groups -- see
//! [`build_tree`]'s own doc for why they are separate sections, why
//! deny/prompt rows are read-only [`MenuNode::Static`] rows, and why hooks
//! get a fourth section rather than
//! folding into allow), and "plugins" (board item
//! `01M0KARX71A64NTSYTDBVANVPF`, restructured by `01M0RW3CPE8SG3PZ2J8RTK9Y9N`
//! -- see "Plugins: a switch you can see, not a row you have to guess"
//! below) -- rather than one flat group or several separate ones: this is
//! genuinely the shape a further settings category (say, session history)
//! would extend later, not artificial nesting invented only to exercise the
//! primitive.
//!
//! ## Plugins: a switch you can see, not a row you have to guess
//!
//! Board item `01M0RW3CPE8SG3PZ2J8RTK9Y9N`, replacing the shape
//! `01M0KARX71A64NTSYTDBVANVPF` originally shipped (a per-plugin subgroup
//! whose children were the toggle leaf and three READ-ONLY "you get"/"you
//! lose"/"costs" rows, sitting as flat siblings -- nothing in that list told
//! a first-time reader which ONE of the four rows actually did anything).
//! The operator's own complaint named two separate defects, and this fixes
//! both, in two independent moves:
//!
//! - **The switch now LOOKS like a switch.** [`plugin_toggle_leaf`] prefixes
//!   every plugin's toggle label with `[x]`/`[ ]` -- the SAME bracket
//!   visual language [`draw`] (below, in `view/menu.rs`) already uses for a
//!   group's own `[-]`/`[+]` expand marker, extended rather than replaced
//!   with a second symbol invented for this section alone (the module doc
//!   already records why a second toggle KEY was rejected for the same
//!   "don't invent a second idiom" reason -- see [`PLUGIN_TOGGLE_NOTE`]'s
//!   own doc; the same logic applies to a second visual language). The two
//!   boolean leaves in "display" ([`LEAF_SHOW_REASONING`]/
//!   [`LEAF_SHOW_TIMESTAMPS`], via [`bool_label`]) get the SAME prefix, for
//!   the same reason: a checkbox invented only for plugins would BE the
//!   second idiom this is meant to avoid. [`LEAF_PERMISSION_MODE`] (three
//!   values, not two) and every revoke leaf (an action, not a persistent
//!   on/off state) deliberately do NOT get one -- a checkbox on either would
//!   misrepresent what the row means.
//!
//!   ONE ASYMMETRY BETWEEN THE TWO CALL SITES, stated rather than glossed:
//!   [`bool_label`] keeps its trailing `-- on`/`-- off` word beside the box,
//!   so its state is never carried by the glyph alone. The plugin leaf
//!   DROPPED its status word, so its state is carried by the glyph plus the
//!   `(turn off, Enter)` / `(turn on, Enter)` action hint, which names the
//!   state by implying its opposite. Both remain readable without colour --
//!   which is the property that matters -- but they are not the same
//!   construction, and a future change that removes the action hint would
//!   leave the plugin row's state on the glyph alone.
//! - **The info moved out of the list, not deleted.** A plugin's own
//!   subgroup and its three static rows are gone; [`build_tree`] now emits
//!   exactly ONE row per plugin (the toggle leaf), so a person scanning the
//!   plugins list sees only things that respond to `Enter`. The "you get" /
//!   "you lose" / "costs" text -- the operator's own framing, kept literally
//!   (unchanged from the original item) -- reappears in a DETAIL PANEL
//!   [`draw`] renders below the list whenever the cursor sits on a plugin's
//!   own toggle row ([`selected_plugin_detail`]/[`draw_plugin_detail`]),
//!   picked from the three structural options the item's own spec named
//!   (info collapsed behind a child row; a detail/side pane for the
//!   selected item; or the rows kept but visibly demoted) because it is the
//!   one the ORIGINAL item's own mockup already showed (a horizontal rule
//!   then a per-plugin detail block under a flat list) -- reusing an
//!   already-considered shape rather than inventing a fourth. Collapsing
//!   the info behind a child row was rejected because it still leaves a
//!   selectable-but-not-a-switch row in the SAME list the fix exists to
//!   clear of exactly that; demoting the rows in place was rejected because
//!   dim styling alone does not fix "nothing signals which line responds to
//!   `Enter`" -- a demoted row is still a row, still competing with the
//!   switch for the reader's attention as something that MIGHT be
//!   selectable until they try it.
//!
//! This also retires the one collapse-state hazard the original item's own
//! doc used to warn about here: with no more per-plugin [`MenuNode::Group`],
//! there is no more per-plugin entry in `AppState::settings_collapsed_groups`
//! to key correctly in the first place -- the concern is moot, not merely
//! avoided.
//!
//! ## Interaction
//!
//! `Up`/`Down` navigate, `Enter` toggles a boolean leaf or expands/collapses
//! a group (mirrors the `/agents` panel's own Enter-to-activate shape --
//! `menu::MenuState::selected_leaf_id`'s own doc). The one NON-boolean leaf
//! (`tool_preview_lines`) is a `Left`/`Right` STEPPER instead of a third
//! `Enter` behavior -- see [`crate::tui::state::AppState::
//! adjust_tool_preview_lines`]'s own doc for why a continuous +/-1 step
//! fits a small integer range better than cycling a fixed preset list
//! (there is no natural "meaningfully different" preset set here the way,
//! say, a theme picker would have -- every value in `1..=200` is an equally
//! plausible answer to "how many lines before folding", so a stepper lets
//! the user land on exactly the number they want instead of hunting through
//! presets that may straddle it). `Esc` closes.
//!
//! ## Fresh tree every call, not a long-lived stored `MenuState`
//!
//! [`build_tree`] is called fresh by both [`draw`] (below) and
//! `input::handle_settings_key` on every render/keypress, baking the
//! CURRENT `AppState` values into each leaf's label text (`"show reasoning
//! traces -- on"`). A long-lived, mutated-in-place `MenuState` would go
//! stale the instant a toggle changed the very value its own label
//! displays, and `menu::MenuState` deliberately exposes no "relabel this
//! one leaf" mutator (it doesn't know what a leaf's opaque `id` means).
//! Only [`crate::tui::state::AppState::settings_selected`] (the cursor) and
//! [`crate::tui::state::AppState::settings_collapsed_groups`] (which groups
//! are collapsed, keyed by label) persist across calls -- `MenuState::
//! set_selected` (V4 addition to `menu.rs`) restores the cursor onto the
//! freshly built tree.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::menu::{self, MenuNode, MenuState};
use super::modal;
use super::theme::Theme;
use crate::tui::state::AppState;

/// [`MenuNode::Leaf`] ids -- opaque to `menu.rs`, interpreted only here and
/// by `input::handle_settings_key` (the only two places that need to agree
/// on their meaning). `pub(crate)` so `input.rs` can match on them without
/// this module needing to expose its whole tree-building internals.
pub(crate) const LEAF_SHOW_REASONING: &str = "show_reasoning";
pub(crate) const LEAF_SHOW_TIMESTAMPS: &str = "show_timestamps";
pub(crate) const LEAF_TOOL_PREVIEW_LINES: &str = "tool_preview_lines";
/// V2b: cycles `prompt` -> `plan` -> `AUTO-ALLOW` -> `prompt`.
pub(crate) const LEAF_PERMISSION_MODE: &str = "permission_mode";
/// V2b: drops every pattern grant and cached allow-always.
pub(crate) const LEAF_REVOKE_GRANTS: &str = "revoke_grants";
/// Prefix for one grant row's own
/// leaf id, `"{LEAF_REVOKE_GRANT_PREFIX}{index}"` where `index` is the
/// row's position in `state.permission_grants` at the moment this tree was
/// built. `input::activate_settings_selection` resolves that index back
/// into `state.permission_grants` in the SAME call that built this tree, so
/// there is no window in which the index could point at a different grant
/// than the one rendered — see that function's own doc.
pub(crate) const LEAF_REVOKE_GRANT_PREFIX: &str = "revoke_grant:";
/// A2: prefix for one STRUCTURED allow rule's own leaf id,
/// `"{LEAF_REVOKE_STRUCTURED_ALLOW_PREFIX}{index}"` where `index` is the
/// row's position in `state.structured_allow_rules` at the moment this tree
/// was built -- a DISTINCT id space from [`LEAF_REVOKE_GRANT_PREFIX`] so a
/// flat revocation and a structured revocation can never resolve against
/// each other's mirror (the two lists are indexed independently, and
/// `input::activate_settings_selection` resolves each prefix against its
/// OWN mirror in the same call that built this tree, exactly as the flat
/// path does -- see that function's own doc).
pub(crate) const LEAF_REVOKE_STRUCTURED_ALLOW_PREFIX: &str = "revoke_structured_allow:";
/// Prefix for one hook-backed rule's
/// own leaf id, `"{LEAF_REVOKE_HOOK_PREFIX}{index}"` where `index` is the
/// row's position in `state.hook_rules` at the moment this tree was built --
/// a DISTINCT id space from the two prefixes above, resolved against its own
/// mirror the same way `input::activate_settings_selection` already resolves
/// the other two.
pub(crate) const LEAF_REVOKE_HOOK_PREFIX: &str = "revoke_hook:";
/// Board item `01M0KARX71A64NTSYTDBVANVPF`: prefix for one plugin's own
/// toggle leaf id, `"{LEAF_TOGGLE_PLUGIN_PREFIX}{plugin_id}"` -- keyed by
/// the plugin's own manifest id (stable, never an index into a Vec that
/// could reorder) since `state.plugin_browser` is sorted by id fresh on
/// every `build_tree` call, unlike the index-addressed grant/hook prefixes
/// above, which key by POSITION in a list whose order does not change
/// between builds. `input::activate_settings_selection` strips this
/// prefix and looks the plugin id up directly.
pub(crate) const LEAF_TOGGLE_PLUGIN_PREFIX: &str = "toggle_plugin:";

/// The two top-level group labels (see this module's own doc, "Grouping").
/// `pub(crate)` for the same reason the leaf ids are -- `input.rs` and this
/// module must agree on the SAME strings, since [`crate::tui::state::
/// AppState::settings_collapsed_groups`] is keyed by them.
const DISPLAY_GROUP: &str = "display";
const TOOL_OUTPUT_GROUP: &str = "tool output";
const PERMISSIONS_GROUP: &str = "permissions";
/// The permissions group's three sub-section labels. Allow, deny, and
/// prompt compose by DIFFERENT rules (deny beats everything except root
/// containment; prompt narrows below deny; allow grants), so one
/// undifferentiated list would misrepresent the model -- each gets its own
/// collapsible section.
const ALLOW_GROUP: &str = "allow";
const DENY_GROUP: &str = "deny";
const PROMPT_GROUP: &str = "prompt";
/// The fourth review-list sub-section:
/// every hook-backed rule that can currently deny a call (`pre_tool_use`,
/// `prompt_submitted`) -- its own section, not merged into `ALLOW_GROUP`/
/// `DENY_GROUP`/`PROMPT_GROUP`, because a hook rule composes by a DIFFERENT
/// mechanism than a pattern rule (it runs an operator-configured command and
/// answers `Deny`/`no_opinion`, not a `When`/`Then` match) and revoking one
/// only ever drops it for the rest of THIS session (`Conway::
/// revoke_hook_rule`'s own doc) -- unlike a flat/structured allow grant,
/// which can also persist to a file. See [`build_tree`]'s own doc for why
/// it appears here rather than being folded into an existing section.
const HOOKS_GROUP: &str = "hooks";
/// Board item `01M0KARX71A64NTSYTDBVANVPF`: the plugin browser's own
/// top-level group. Deliberately just `"plugins"` -- never carries the
/// live installed/available COUNTS the way the mockup's own header line
/// does, because `group_node`/`AppState::settings_collapsed_groups` key a
/// group's collapsed state by its exact label text; a label that changed
/// text every time a toggle changed the count would silently forget
/// whether the operator had this section collapsed (falls back to the
/// expanded default) on the very next toggle. The counts render instead
/// as the section's own first, non-selectable row -- see [`build_tree`]'s
/// own doc.
const PLUGINS_GROUP: &str = "plugins";

/// The footer's session-only disclosure for the display/tool-output/
/// permissions sections: no writer exists for THEM (still true --
/// `01M0K8BAXJ6THVJAPK0JZ17VV6` settled a writer only for `plugins.install`,
/// not the fuller settings design this item deliberately stays a slice of).
/// Shown on every render, regardless of which row is selected -- kept
/// TRUE unconditionally by staying scoped to exactly what it always meant
/// (display/tool-output/permissions), never widened to also describe
/// plugin toggles, which is a DIFFERENT, now-real persistence story (see
/// [`PLUGIN_TOGGLE_NOTE`]).
const SESSION_NOTE: &str = "display/permission changes apply to this session only";
/// Board item `01M0KARX71A64NTSYTDBVANVPF`, acceptance criterion 4: the
/// footer must state plainly that a plugin toggle applies on next start --
/// paired on the SAME footer line as [`SESSION_NOTE`] so both persistence
/// stories are visible together regardless of which row is selected
/// (this whole menu's own "no row-conditional footer" precedent -- see
/// `SESSION_NOTE`'s own doc history). Deliberately does NOT say "restart
/// to take effect" is achieved via the mockup's literal "space toggles"
/// wording -- this menu's OWN established key is `Enter`, consistent with
/// every other toggleable row in it (booleans, permission mode, grant
/// revocation); inventing a second toggle key for one section alone would
/// be the inconsistency, not a virtue of matching an illustrative mockup
/// literally.
const PLUGIN_TOGGLE_NOTE: &str = "plugin toggles: Enter, written to disk, applied on next restart";

/// Builds the settings tree from the CURRENT `state` (see this module's own
/// doc, "Fresh tree every call") and restores the persisted cursor
/// (`state.settings_selected`) onto it. `pub(crate)` so both [`draw`] and
/// `input::handle_settings_key` build from the exact same function -- the
/// tree the user sees and the tree navigation resolves against can never
/// drift apart.
pub(crate) fn build_tree(state: &AppState) -> MenuState {
    let roots = vec![
        group_node(
            DISPLAY_GROUP,
            state,
            vec![
                MenuNode::leaf(
                    bool_label("show reasoning traces", state.show_reasoning),
                    LEAF_SHOW_REASONING,
                ),
                MenuNode::leaf(
                    bool_label("show timestamps", state.show_timestamps),
                    LEAF_SHOW_TIMESTAMPS,
                ),
            ],
        ),
        group_node(
            TOOL_OUTPUT_GROUP,
            state,
            vec![MenuNode::leaf(
                format!(
                    "tool preview lines -- {} (Left/Right to adjust; \
                     persists via [tui.tool_preview_lines])",
                    state.tool_preview_lines
                ),
                LEAF_TOOL_PREVIEW_LINES,
            )],
        ),
        // each active grant is now a
        // real, selectable leaf row with a destructive action (revoke)
        // rather than inert label text -- the reasoning that made a
        // selectable-but-inert row "a worse lie than an obviously static
        // one" held only until per-rule revocation existed to back it; it
        // now does (`Conway::revoke_permission_pattern`). The row's label
        // is unchanged (`[origin] description`, `describe()`) plus an
        // explicit "(Enter to revoke)" hint, so the destructive action is
        // named before it happens, not just implied by the row becoming
        // selectable.
        //
        // Deny and prompt rules get their own sections BELOW the allow
        // section, and their rows are `MenuNode::Static` -- read-only, so
        // the cursor can never land on one and nothing highlights or
        // answers `Enter`. That is the same "worse lie" reasoning run in
        // the OTHER direction: deny/prompt are deliberately NOT revocable
        // from this menu (a safety rule offering one-keystroke removal is
        // the wrong shape -- `Conway::revoke_permission_pattern`'s own
        // doc), so their rows must not LOOK actionable the way a grant row
        // does. They are shown at all because deny/prompt install from ANY
        // permissions file, trusted or not -- an untrusted checkout can
        // ship one -- and a rule set nobody can inspect is a trap. Flat
        // and structured (F12) rules render alike, `[origin] description`;
        // the structured ones simply come from `Rule::describe()` instead
        // of `PatternRule::describe()`.
        group_node(PERMISSIONS_GROUP, state, {
            let rows = vec![
                MenuNode::leaf(
                    format!("mode -- {} (Enter to cycle)", state.permission_mode.label()),
                    LEAF_PERMISSION_MODE,
                ),
                group_node(ALLOW_GROUP, state, {
                    let mut allow = Vec::new();
                    if state.permission_grants.is_empty() && state.structured_allow_rules.is_empty()
                    {
                        allow.push(MenuNode::static_row("no active grants"));
                    } else {
                        for (i, (rule, origin)) in state.permission_grants.iter().enumerate() {
                            allow.push(MenuNode::leaf(
                                format!(
                                    "granted: [{}] {} (Enter to revoke)",
                                    origin.describe(),
                                    rule.describe()
                                ),
                                format!("{LEAF_REVOKE_GRANT_PREFIX}{i}"),
                            ));
                        }
                        // A2: the structured allow rules the flat
                        // form cannot express render in the SAME allow
                        // section, as the same selectable-leaf shape -- they
                        // are revocable grants exactly like the flat rows,
                        // just addressed through the Rule-identity revoke
                        // (`input::activate_settings_selection`'s
                        // `LEAF_REVOKE_STRUCTURED_ALLOW_PREFIX` arm) because
                        // the flat `(PatternRule, origin)` key cannot name
                        // one. The scope is annotated only when it is NOT
                        // the whole session: every rule the TUI itself
                        // installs is session-scoped, so a constant
                        // "session" annotation would be noise on every row,
                        // while an Agent/AgentSubtree grant (an embedder's)
                        // covers LESS than the row otherwise implies and
                        // must say so.
                        for (i, (rule, origin, scope)) in
                            state.structured_allow_rules.iter().enumerate()
                        {
                            let scope_note = match scope {
                                conway::GrantScope::Session => String::new(),
                                other => format!(" (scope: {})", other.describe()),
                            };
                            allow.push(MenuNode::leaf(
                                format!(
                                    "granted: [{}] {}{scope_note} (Enter to revoke)",
                                    origin.describe(),
                                    rule.describe()
                                ),
                                format!("{LEAF_REVOKE_STRUCTURED_ALLOW_PREFIX}{i}"),
                            ));
                        }
                        allow.push(MenuNode::leaf(
                            "revoke all grants (Enter)".to_string(),
                            LEAF_REVOKE_GRANTS,
                        ));
                    }
                    allow
                }),
                group_node(DENY_GROUP, state, {
                    let mut deny: Vec<MenuNode> = state
                        .permission_denies
                        .iter()
                        .map(|(rule, origin)| {
                            MenuNode::static_row(format!(
                                "[{}] {}",
                                origin.describe(),
                                rule.describe()
                            ))
                        })
                        .collect();
                    deny.extend(state.structured_deny_rules.iter().map(|(rule, origin)| {
                        MenuNode::static_row(format!("[{}] {}", origin.describe(), rule.describe()))
                    }));
                    if deny.is_empty() {
                        deny.push(MenuNode::static_row("no active deny rules"));
                    }
                    deny
                }),
                group_node(PROMPT_GROUP, state, {
                    let mut prompt: Vec<MenuNode> = state
                        .permission_prompts
                        .iter()
                        .map(|(rule, origin)| {
                            MenuNode::static_row(format!(
                                "[{}] {}",
                                origin.describe(),
                                rule.describe()
                            ))
                        })
                        .collect();
                    prompt.extend(state.structured_prompt_rules.iter().map(|(rule, origin)| {
                        MenuNode::static_row(format!("[{}] {}", origin.describe(), rule.describe()))
                    }));
                    if prompt.is_empty() {
                        prompt.push(MenuNode::static_row("no active prompt rules"));
                    }
                    prompt
                }),
                // the fourth
                // review list -- every hook-backed rule that can currently
                // DENY a call, selectable and revocable exactly like an
                // allow-grant row (unlike deny/prompt's read-only rows: a
                // hook rule an operator wants OFF should be one keystroke
                // away, same as an allow grant, not permanently locked the
                // way a safety-rule deny pattern deliberately is -- see
                // `Conway::revoke_hook_rule`'s own doc for why this is
                // always session-scoped, never a file rewrite). Each row
                // names its id, event, matcher, and origin so an operator
                // can tell WHICH rule they are about to turn off.
                group_node(HOOKS_GROUP, state, {
                    if state.hook_rules.is_empty() {
                        vec![MenuNode::static_row("no active hook rules")]
                    } else {
                        state
                            .hook_rules
                            .iter()
                            .enumerate()
                            .map(|(i, rule)| {
                                let matcher = match &rule.match_tool {
                                    Some(pattern) => format!("matching `{pattern}`"),
                                    None => "every call".to_string(),
                                };
                                MenuNode::leaf(
                                    format!(
                                        "hook: [{}] `{}` on `{}`, {matcher} (Enter to revoke)",
                                        rule.origin, rule.id, rule.event
                                    ),
                                    format!("{LEAF_REVOKE_HOOK_PREFIX}{i}"),
                                )
                            })
                            .collect()
                    }
                }),
            ];
            rows
        }),
        // Board item `01M0KARX71A64NTSYTDBVANVPF`, restructured by
        // `01M0RW3CPE8SG3PZ2J8RTK9Y9N`: the plugin browser. First child is a
        // non-selectable header row naming the live installed/available
        // counts (see `PLUGINS_GROUP`'s own doc for why the counts live
        // here rather than in the group's own label); every OTHER child is
        // ONE compiled-in plugin's own toggle leaf -- no nested subgroup and
        // no sibling info rows any more (see this module's own doc,
        // "Plugins: a switch you can see, not a row you have to guess") --
        // sorted by id for a deterministic, scan-stable order across
        // renders regardless of `first_party_plugins::bundle`'s own
        // construction-order.
        group_node(PLUGINS_GROUP, state, {
            let installed_count = state.plugin_browser.iter().filter(|p| p.installed).count();
            let available_count = state.plugin_browser.len() - installed_count;
            let mut rows = vec![MenuNode::static_row(format!(
                "{installed_count} installed \u{b7} {available_count} available"
            ))];
            let mut entries: Vec<&crate::tui::state::PluginBrowserEntry> =
                state.plugin_browser.iter().collect();
            entries.sort_by(|a, b| a.id.cmp(&b.id));
            for entry in entries {
                rows.push(plugin_toggle_leaf(entry));
            }
            rows
        }),
    ];
    let mut menu = MenuState::new(roots);
    menu.set_selected(state.settings_selected);
    menu
}

/// One plugin's own row in the flattened list (board item
/// `01M0KARX71A64NTSYTDBVANVPF`, restructured by
/// `01M0RW3CPE8SG3PZ2J8RTK9Y9N`): a SINGLE selectable leaf, no nested
/// subgroup and no sibling info rows -- the "you get"/"you lose"/"costs"
/// text this used to sit beside now lives in the detail panel
/// [`draw_plugin_detail`] renders for whichever plugin is currently
/// selected (see this module's own doc, "Plugins: a switch you can see,
/// not a row you have to guess"). The label leads with `[x]`/`[ ]` -- the
/// same bracket marker [`view::menu::draw`] already uses for a group's own
/// `[-]`/`[+]` expand state, reused here rather than a symbol invented for
/// this section alone -- followed by the id, version, and one-line summary,
/// and the same `(turn off/on, Enter)` action hint the original shape used.
fn plugin_toggle_leaf(entry: &crate::tui::state::PluginBrowserEntry) -> MenuNode {
    let box_glyph = if entry.installed { "x" } else { " " };
    let action = if entry.installed {
        "turn off"
    } else {
        "turn on"
    };
    let summary = non_empty_or(&entry.description.summary, "(no description)");
    MenuNode::leaf(
        format!(
            "[{box_glyph}] {} \u{b7} v{} -- {summary} ({action}, Enter)",
            entry.id, entry.version
        ),
        format!("{LEAF_TOGGLE_PLUGIN_PREFIX}{}", entry.id),
    )
}

/// If the CURRENT selection is a plugin's own toggle leaf, the entry the
/// detail panel should describe -- resolved by stripping
/// [`LEAF_TOGGLE_PLUGIN_PREFIX`] off `tree`'s own [`MenuState::
/// selected_leaf_id`] and looking it up in `state.plugin_browser` in the
/// SAME call, mirroring `input::activate_settings_selection`'s own
/// resolve-against-the-tree-that-built-this-id pattern (see that fn's own
/// doc) -- the id can never point at a stale entry because both come from
/// the same `state` the caller already has in hand. `None` on a group row,
/// a non-plugin leaf, or an empty tree -- [`draw`] renders no detail panel
/// at all in that case, exactly like browsing "display" or "permissions"
/// today.
fn selected_plugin_detail<'a>(
    tree: &MenuState,
    state: &'a AppState,
) -> Option<&'a crate::tui::state::PluginBrowserEntry> {
    let id = tree.selected_leaf_id()?;
    let plugin_id = id.strip_prefix(LEAF_TOGGLE_PLUGIN_PREFIX)?;
    state
        .plugin_browser
        .iter()
        .find(|entry| entry.id == plugin_id)
}

/// The fixed row budget [`draw`] reserves below the list for
/// [`draw_plugin_detail`]: one row for the top-border divider
/// (`Borders::TOP`, the mockup's own horizontal rule, drawn via a real
/// border rather than a manually repeated dash string so it always spans
/// the modal's actual width), one for the `id \u{b7} vVERSION \u{b7}
/// status` header line, and one each for "you get"/"you lose"/"costs" --
/// mirroring [`FOOTER_ROWS`]'s own "fixed, reserved, may clip on a
/// pathologically long value" posture rather than measuring a wrapped line
/// count the way the list body does (a settings value is operator-authored
/// short prose, not user content of unbounded length).
const DETAIL_ROWS: u16 = 5;

/// Renders the selected plugin's own "you get"/"you lose"/"costs" panel
/// (the operator's own framing, kept literally, per this module's own doc)
/// into `area` -- [`draw`]'s own reserved [`DETAIL_ROWS`]-tall region below
/// the list, present only while the cursor sits on a plugin's toggle row.
fn draw_plugin_detail(
    frame: &mut Frame,
    area: Rect,
    entry: &crate::tui::state::PluginBrowserEntry,
    theme: &Theme,
) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme.dim);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let status = if entry.installed { "on" } else { "off" };
    let you_get = non_empty_or(&entry.description.you_get, "(none given)");
    let you_lose = non_empty_or(&entry.description.you_lose, "(none given)");
    let costs = non_empty_or(&entry.description.costs, "none");
    let lines = vec![
        Line::from(Span::styled(
            format!("{} \u{b7} v{} \u{b7} {status}", entry.id, entry.version),
            theme.emphasized,
        )),
        Line::from(format!("you get   {you_get}")),
        Line::from(format!("you lose  {you_lose}")),
        Line::from(format!("costs     {costs}")),
    ];
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

/// Builds one top-level group node via [`MenuNode::group`] (expanded by
/// default -- the primitive's own convenience constructor) and then patches
/// `expanded` to `false` only when `label` is in `state.
/// settings_collapsed_groups` -- collapsed is the exception, so the
/// convenience constructor's own default covers the common case directly
/// rather than every call site re-deriving `!collapsed.contains(label)`
/// inline.
fn group_node(label: &str, state: &AppState, children: Vec<MenuNode>) -> MenuNode {
    let mut node = MenuNode::group(label, children);
    if state.settings_collapsed_groups.contains(label) {
        if let MenuNode::Group { expanded, .. } = &mut node {
            *expanded = false;
        }
    }
    node
}

/// `01M0RW3CPE8SG3PZ2J8RTK9Y9N`: leads with the same `[x]`/`[ ]` bracket
/// marker [`plugin_toggle_leaf`] uses -- the SAME idiom applied to every
/// boolean leaf in the menu, not one invented for the plugins section alone
/// (this module's own doc, "Plugins: a switch you can see..."). The
/// trailing `-- on`/`-- off` text is UNCHANGED from before this item, kept
/// alongside the box rather than replaced by it, so a value is never
/// conveyed by the box glyph alone.
fn bool_label(name: &str, value: bool) -> String {
    let box_glyph = if value { "x" } else { " " };
    format!(
        "[{box_glyph}] {name} -- {}",
        if value { "on" } else { "off" }
    )
}

/// Rows the settings modal's footer ALWAYS reserves: the key hint, the
/// session-only disclosure, and (board item `01M0KARX71A64NTSYTDBVANVPF`)
/// the plugin-toggle persistence disclosure -- mirroring every other
/// ported surface's "footer rows are fixed, never squeezed by body
/// growth" invariant (`view/modal.rs`'s own doc). Two SEPARATE lines
/// (`SESSION_NOTE`/`PLUGIN_TOGGLE_NOTE`), not one concatenated line: the
/// combined text does not reliably fit one row at a realistic terminal
/// width, and `Wrap { trim: true }`'s own wrap would otherwise silently
/// clip the second half against a footer area sized for one row.
const FOOTER_ROWS: u16 = 3;

/// The settings menu's own cap denominator -- `1`, the same generous cap
/// `/help` uses (`view/help.rs::CAP_DENOMINATOR`'s own doc): this is an
/// INFORMATIONAL surface the user opened on purpose to browse/adjust, not a
/// decision interrupting them, so it can reasonably claim more of the
/// screen than the `2`-denominator decision-owed surfaces.
const CAP_DENOMINATOR: u16 = 1;

/// Draws the `/settings` menu over `transcript_area` via the shared
/// [`modal`] primitive (bottom-anchored, content-sized, capped) with
/// [`menu::draw`] rendering the tree itself into the modal's own
/// `body_area`. Reuses `theme.help_border` for the border rather than
/// adding a new theme slot -- like `/help`, this is an informational
/// overlay, and the two are mutually exclusive (`AppState::open_settings`/
/// `open_help` each close the other), so no two things can ever be on
/// screen at once needing visually distinct borders.
pub fn draw(frame: &mut Frame, transcript_area: Rect, state: &AppState, theme: &Theme) {
    let tree = build_tree(state);
    let content_rows = tree.rows().len().min(u16::MAX as usize) as u16;

    // `01M0RW3CPE8SG3PZ2J8RTK9Y9N`: while the cursor sits on a plugin's own
    // toggle row, reserve extra room below the list for its detail panel
    // (see this module's own doc, "Plugins: a switch you can see..."). The
    // reservation happens BEFORE `modal::draw_modal_frame` (folded into its
    // own `footer_rows` parameter, alongside `FOOTER_ROWS`) so the modal's
    // total height genuinely accounts for it -- the SAME "reserved before
    // the body, never squeezed out" contract `FOOTER_ROWS` itself already
    // has, extended here rather than the detail panel silently overlapping
    // the body or getting clipped by a modal sized without it in mind.
    let detail_entry = selected_plugin_detail(&tree, state);
    let detail_rows = if detail_entry.is_some() {
        DETAIL_ROWS
    } else {
        0
    };

    let frame_areas = modal::draw_modal_frame(
        frame,
        transcript_area,
        content_rows,
        FOOTER_ROWS + detail_rows,
        CAP_DENOMINATOR,
        " SETTINGS ",
        theme.help_border,
    );

    menu::draw(frame, frame_areas.body_area, &tree, theme);

    // Splits the reserved tail into the detail panel (if any) and the
    // always-present footer, in that order -- the footer is what you act on
    // (this module's own `FOOTER_ROWS` doc), so it is the LAST thing
    // squeezed if the reserved tail itself had to shrink on a tiny
    // terminal, never the first.
    let footer_area = if let Some(entry) = &detail_entry {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(FOOTER_ROWS.min(frame_areas.footer_area.height)),
            ])
            .split(frame_areas.footer_area);
        draw_plugin_detail(frame, rows[0], entry, theme);
        rows[1]
    } else {
        frame_areas.footer_area
    };

    let footer_lines = vec![
        Line::from("[Up/Down] move  [Enter] toggle/expand  [Left/Right] adjust  [Esc] close"),
        Line::from(Span::styled(SESSION_NOTE, theme.dim)),
        Line::from(Span::styled(PLUGIN_TOGGLE_NOTE, theme.dim)),
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

    // ---- build_tree: labels reflect the CURRENT AppState values ----

    #[test]
    fn build_tree_labels_reflect_current_boolean_values() {
        let mut state = AppState::new(AgentId::new());
        state.show_reasoning = true;
        state.show_timestamps = false;

        let rows = build_tree(&state).rows();
        let labels: Vec<String> = rows.iter().map(|r| r.label.clone()).collect();

        assert!(
            labels
                .iter()
                .any(|l| l.contains("show reasoning traces") && l.ends_with("on")),
            "{labels:?}"
        );
        assert!(
            labels
                .iter()
                .any(|l| l.contains("show timestamps") && l.ends_with("off")),
            "{labels:?}"
        );

        state.show_reasoning = false;
        let rows_after = build_tree(&state).rows();
        let labels_after: Vec<String> = rows_after.iter().map(|r| r.label.clone()).collect();
        assert!(
            labels_after
                .iter()
                .any(|l| l.contains("show reasoning traces") && l.ends_with("off")),
            "a freshly built tree must reflect the CURRENT value, not a stale one: {labels_after:?}"
        );
    }

    #[test]
    fn build_tree_labels_the_numeric_leaf_with_its_current_value_and_config_key() {
        let mut state = AppState::new(AgentId::new());
        state.tool_preview_lines = 7;

        let rows = build_tree(&state).rows();
        let numeric_label = rows
            .iter()
            .find(|r| r.label.contains("tool preview lines"))
            .expect("the numeric leaf must exist")
            .label
            .clone();

        assert!(numeric_label.contains('7'), "{numeric_label}");
        assert!(
            numeric_label.contains("[tui.tool_preview_lines]"),
            "the one leaf with a real config key must name it: {numeric_label}"
        );
    }

    #[test]
    fn boolean_leaves_do_not_claim_a_config_key_that_does_not_exist() {
        let state = AppState::new(AgentId::new());
        let rows = build_tree(&state).rows();
        for row in &rows {
            if row.label.contains("show reasoning") || row.label.contains("show timestamps") {
                assert!(
                    !row.label.contains("[tui."),
                    "no `[tui.*]` config key backs these two -- the label must not \
                     imply one exists: {}",
                    row.label
                );
            }
        }
    }

    #[test]
    fn build_tree_restores_the_persisted_cursor() {
        let mut state = AppState::new(AgentId::new());
        state.settings_selected = 2;

        let tree = build_tree(&state);
        assert_eq!(tree.selected_index(), 2);
    }

    // ---- draw: bottom-anchored, content-sized (V1's shape) ----

    #[test]
    fn draw_renders_bottom_anchored_and_content_sized() {
        let state = AppState::new(AgentId::new());
        let text = render(&state, 80, 24);
        assert!(text.contains("SETTINGS"), "{text}");
        assert!(text.contains("display"), "{text}");
        assert!(text.contains("tool output"), "{text}");
        assert!(text.contains(SESSION_NOTE), "{text}");
    }

    #[test]
    fn draw_never_panics_on_a_tiny_terminal() {
        let state = AppState::new(AgentId::new());
        for (w, h) in [(80u16, 1u16), (80, 2), (1, 24), (0, 0)] {
            let backend = TestBackend::new(w.max(1), h.max(1));
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| draw(f, f.area(), &state, &Theme::default()))
                .unwrap_or_else(|e| panic!("panicked/errored at {w}x{h}: {e}"));
        }
    }
    /// V2b: the permissions group exposes the mode and, once grants exist,
    /// a review list plus revoke-all.
    #[test]
    fn the_permissions_group_shows_mode_and_grants() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();

        let text = plain_rows(&state);
        assert!(text.contains("permissions"), "{text}");
        assert!(text.contains("mode -- prompt"), "{text}");
        assert!(
            text.contains("no active grants"),
            "an empty grant list says so rather than rendering nothing: {text}"
        );

        state.permission_grants = vec![(
            conway::PatternRule::parse("bash:git status").expect("valid rule"),
            conway::PatternOrigin::Interactive,
        )];
        let text = plain_rows(&state);
        assert!(
            text.contains("granted: [interactive] `bash` commands starting with `git status` (Enter to revoke)"),
            "{text}"
        );
        assert!(
            text.contains("revoke all grants"),
            "revoke-all appears only once there is something to revoke: {text}"
        );
    }

    /// Each grant row is now a real
    /// selectable leaf, addressed by its position in `state.
    /// permission_grants` at build time -- not the inert `""` id the
    /// pre-revocation shape used.
    #[test]
    fn each_grant_row_is_a_selectable_leaf_addressed_by_its_index() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.permission_grants = vec![
            (
                conway::PatternRule::parse("bash:git status").expect("valid rule"),
                conway::PatternOrigin::Interactive,
            ),
            (
                conway::PatternRule::parse("read:*").expect("valid rule"),
                conway::PatternOrigin::File(std::path::PathBuf::from(
                    "/repo/.conway/permissions.json",
                )),
            ),
        ];

        let rows = build_tree(&state).rows();
        let grant_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.label.starts_with("granted:"))
            .collect();
        assert_eq!(grant_rows.len(), 2);
        assert_eq!(
            grant_rows[0].kind,
            menu::MenuRowKind::Leaf {
                id: format!("{LEAF_REVOKE_GRANT_PREFIX}0")
            }
        );
        assert_eq!(
            grant_rows[1].kind,
            menu::MenuRowKind::Leaf {
                id: format!("{LEAF_REVOKE_GRANT_PREFIX}1")
            }
        );
        assert!(grant_rows[1]
            .label
            .contains("/repo/.conway/permissions.json"));
    }

    /// The mode label in the menu tracks `AppState`, so it cannot show a
    /// stale value after a cycle.
    #[test]
    fn the_mode_row_reflects_the_current_permission_mode() {
        let mut state = AppState::new(AgentId::new());
        state.permission_mode = conway::PermissionMode::AutoAllow;
        assert!(plain_rows(&state).contains("mode -- AUTO-ALLOW"));

        state.permission_mode = conway::PermissionMode::Plan;
        assert!(plain_rows(&state).contains("mode -- plan"));
    }

    /// Helper: the menu's visible row labels as one string.
    fn plain_rows(state: &AppState) -> String {
        build_tree(state)
            .rows()
            .iter()
            .map(|r| r.label.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ---- Deny/prompt review sections: visible, read-only, with origin ----

    /// One flat deny + one structured deny + one flat prompt + one
    /// structured prompt, all from the same project file.
    fn populate_deny_and_prompt(state: &mut AppState) {
        let file = || {
            conway::PatternOrigin::File(std::path::PathBuf::from("/repo/.conway/permissions.json"))
        };
        state.permission_denies = vec![(
            conway::PatternRule::parse("bash:curl").expect("valid rule"),
            file(),
        )];
        state.structured_deny_rules = vec![(
            conway::Rule {
                select: conway::Select::Tools(vec!["bash".to_string(), "read".to_string()]),
                when: conway::When::Always,
                then: conway::Then::Deny,
            },
            file(),
        )];
        state.permission_prompts = vec![(
            conway::PatternRule::parse("bash:rm").expect("valid rule"),
            file(),
        )];
        state.structured_prompt_rules = vec![(
            conway::Rule {
                select: conway::Select::Tools(vec!["bash".to_string(), "read".to_string()]),
                when: conway::When::Always,
                then: conway::Then::Prompt,
            },
            file(),
        )];
    }

    /// Every active deny and prompt rule -- flat AND structured -- renders
    /// with its origin, in its own section, as a NON-selectable static row
    /// that promises no `Enter` action.
    #[test]
    fn deny_and_prompt_rules_render_with_their_origins_as_static_rows() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        populate_deny_and_prompt(&mut state);

        let rows = build_tree(&state).rows();
        let text = rows
            .iter()
            .map(|r| r.label.clone())
            .collect::<Vec<_>>()
            .join("\n");

        // The sections exist as their own groups, distinct from allow's.
        for section in ["allow", "deny", "prompt"] {
            assert!(
                rows.iter().any(
                    |r| r.label == section && matches!(r.kind, menu::MenuRowKind::Group { .. })
                ),
                "the {section} section must be its own group: {text}"
            );
        }

        // Flat and structured rules alike, each with its origin's path.
        let expected = [
            "[/repo/.conway/permissions.json] `bash` commands starting with `curl`",
            "[/repo/.conway/permissions.json] [bash, read] (any call)",
            "[/repo/.conway/permissions.json] `bash` commands starting with `rm`",
        ];
        for needle in expected {
            assert!(text.contains(needle), "missing row: {needle}\n{text}");
        }

        // Every deny/prompt entry row is a Static row (never selectable,
        // never highlighted) and names no Enter action -- a read-only row
        // must not LOOK actionable (see `MenuNode::Static`'s own doc).
        let deny_prompt_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.label.contains("/repo/.conway/permissions.json"))
            .collect();
        assert_eq!(deny_prompt_rows.len(), 4, "{text}");
        for row in deny_prompt_rows {
            assert_eq!(
                row.kind,
                menu::MenuRowKind::Static,
                "a deny/prompt row must be read-only: {row:?}"
            );
            assert!(
                !row.label.contains("(Enter"),
                "a read-only row must not promise an action: {}",
                row.label
            );
        }
    }

    /// Empty sections say so honestly, as static rows (the cursor never
    /// lands on the placeholder either).
    #[test]
    fn deny_and_prompt_sections_have_honest_empty_states() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();

        let rows = build_tree(&state).rows();
        for needle in ["no active deny rules", "no active prompt rules"] {
            let row = rows
                .iter()
                .find(|r| r.label == needle)
                .unwrap_or_else(|| panic!("missing empty-state row: {needle}"));
            assert_eq!(row.kind, menu::MenuRowKind::Static, "{row:?}");
        }
    }

    /// Driven through the real navigation primitive: pressing Down across
    /// the whole tree never rests the cursor on a deny/prompt row.
    #[test]
    fn the_cursor_can_never_land_on_a_deny_or_prompt_row() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        populate_deny_and_prompt(&mut state);

        let mut tree = build_tree(&state);
        let row_count = tree.rows().len();
        for _ in 0..row_count {
            let selected = tree.selected_row().expect("nonempty tree");
            assert!(
                !matches!(selected.kind, menu::MenuRowKind::Static),
                "the cursor landed on a read-only row: {selected:?}"
            );
            tree.move_selection(1);
        }
    }

    // ---- Structured allow rows: visible AND revocable (A2) ----

    fn a_structured_allow_rule() -> conway::Rule {
        // Multi-tool + Always: a rule the flat form cannot express (its
        // `to_pattern_rule()` is None), the kind A2 makes revocable.
        conway::Rule {
            select: conway::Select::Tools(vec!["bash".to_string(), "read".to_string()]),
            when: conway::When::Always,
            then: conway::Then::Allow,
        }
    }

    /// A structured allow rule renders in the allow section as a SELECTABLE,
    /// revocable leaf -- `[origin] description (Enter to revoke)`, the same
    /// shape as a flat grant row -- but keyed by its OWN leaf-id prefix, so
    /// its index space can never collide with the flat rows'.
    #[test]
    fn structured_allow_rules_render_as_revocable_leaves_with_their_origins() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.permission_grants = vec![(
            conway::PatternRule::parse("bash:git status").expect("valid rule"),
            conway::PatternOrigin::Interactive,
        )];
        state.structured_allow_rules = vec![(
            a_structured_allow_rule(),
            conway::PatternOrigin::File(std::path::PathBuf::from("/repo/.conway/permissions.json")),
            conway::GrantScope::Session,
        )];

        let rows = build_tree(&state).rows();

        let structured_row = rows
            .iter()
            .find(|r| r.label.contains("[bash, read] (any call)"))
            .expect("the structured allow row must render");
        assert!(structured_row.label.starts_with("granted:"));
        assert!(
            structured_row
                .label
                .contains("/repo/.conway/permissions.json"),
            "the row must name its origin: {}",
            structured_row.label
        );
        assert!(
            structured_row.label.contains("(Enter to revoke)"),
            "a revocable row must name its action: {}",
            structured_row.label
        );
        assert!(
            !structured_row.label.contains("scope:"),
            "a session-wide grant is the default -- annotating it would be noise: {}",
            structured_row.label
        );
        assert_eq!(
            structured_row.kind,
            menu::MenuRowKind::Leaf {
                id: format!("{LEAF_REVOKE_STRUCTURED_ALLOW_PREFIX}0")
            },
            "keyed by the structured prefix, in its own index space"
        );

        // The flat sibling keeps its own prefix and index space -- index 0
        // in EACH list addresses a different row, and the prefixes keep the
        // two resolves apart.
        let flat_row = rows
            .iter()
            .find(|r| r.label.contains("git status"))
            .expect("the flat grant row must render");
        assert_eq!(
            flat_row.kind,
            menu::MenuRowKind::Leaf {
                id: format!("{LEAF_REVOKE_GRANT_PREFIX}0")
            }
        );

        // Both are selectable (never Static), and revoke-all is still there.
        let text = rows
            .iter()
            .map(|r| r.label.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("revoke all grants"), "{text}");
        assert!(!text.contains("no active grants"), "{text}");
    }

    /// A structured allow rule granted at a NARROWER scope than the session
    /// must say so -- the row otherwise implies it covers every agent.
    #[test]
    fn a_structured_allow_rule_with_a_narrower_scope_is_annotated() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.structured_allow_rules = vec![(
            a_structured_allow_rule(),
            conway::PatternOrigin::Interactive,
            conway::GrantScope::Agent(AgentId::new()),
        )];

        let rows = build_tree(&state).rows();
        let row = rows
            .iter()
            .find(|r| r.label.contains("[bash, read] (any call)"))
            .expect("the structured allow row must render");
        assert!(
            row.label.contains("scope: agent"),
            "a non-session scope must be visible: {}",
            row.label
        );
    }

    /// The whole point of `GrantScope::Agent`/`::Subtree` carrying an
    /// `AgentId` (Stage 2b, board items `01KZVYZM7BZRQ54RRB8P814KV9`/
    /// `01KZWRZ4JBAVCRCZ99BFZFF01K`): the review row must name WHICH agent a
    /// per-agent or per-subtree grant covers, not merely that it is
    /// narrower than the session. Two rows granted to two DIFFERENT agents
    /// must render two DIFFERENT labels -- a regression that dropped the
    /// `AgentId` (e.g. `GrantScope` degrading to a bare `PermissionScope`-
    /// shaped enum, or `describe()` returning a constant `"agent"` string)
    /// would collapse both to the same text and fail this test, unlike
    /// `a_structured_allow_rule_with_a_narrower_scope_is_annotated` above,
    /// which only checks the word "agent" appears.
    #[test]
    fn the_structured_allow_scope_annotation_names_which_agent_it_covers() {
        let agent_a = AgentId::new();
        let agent_b = AgentId::new();
        assert_ne!(agent_a, agent_b, "the two granting agents must differ");

        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.structured_allow_rules = vec![
            (
                a_structured_allow_rule(),
                conway::PatternOrigin::Interactive,
                conway::GrantScope::Agent(agent_a),
            ),
            (
                a_structured_allow_rule(),
                conway::PatternOrigin::File(std::path::PathBuf::from(
                    "/repo/.conway/permissions.json",
                )),
                conway::GrantScope::Subtree(agent_b),
            ),
        ];

        let rows = build_tree(&state).rows();
        let labels: Vec<&str> = rows
            .iter()
            .filter(|r| r.label.contains("[bash, read] (any call)"))
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(
            labels.len(),
            2,
            "both structured rows must render: {labels:?}"
        );

        let agent_a_str = agent_a.to_string();
        let agent_b_str = agent_b.to_string();
        assert!(
            labels.iter().any(|l| l.contains(&agent_a_str)),
            "the per-agent row must name agent {agent_a_str}'s own id, not just \
             the word \"agent\": {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains(&agent_b_str)),
            "the per-subtree row must name agent {agent_b_str}'s own id: {labels:?}"
        );
        assert_ne!(
            labels[0], labels[1],
            "two grants to two different agents must render two different \
             rows -- identical rows would mean the AgentId was dropped \
             somewhere on the way from `GrantScope` to the render: {labels:?}"
        );
    }

    /// An allow section holding ONLY structured rules must not render the
    /// "no active grants" empty state (that message would be a lie), and
    /// revoke-all remains available for them.
    #[test]
    fn an_allow_section_with_only_structured_rules_has_no_false_empty_state() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.structured_allow_rules = vec![(
            a_structured_allow_rule(),
            conway::PatternOrigin::Interactive,
            conway::GrantScope::Session,
        )];

        let text = plain_rows(&state);
        assert!(
            !text.contains("no active grants"),
            "a structured rule IS an active grant: {text}"
        );
        assert!(text.contains("[bash, read] (any call)"), "{text}");
        assert!(text.contains("revoke all grants"), "{text}");
    }

    // ---- Hooks review section: visible AND revocable ----

    fn a_hook_rule(id: &str, event: &str, match_tool: Option<&str>) -> conway::HookRuleView {
        conway::HookRuleView {
            id: id.to_string(),
            event: event.to_string(),
            match_tool: match_tool.map(str::to_string),
            origin: "settings.json (merged config)".to_string(),
        }
    }

    /// The hooks section exists, is its own group distinct from allow/deny/
    /// prompt, and an empty list says so honestly rather than rendering
    /// nothing (mirroring the deny/prompt empty states).
    #[test]
    fn the_hooks_group_has_an_honest_empty_state() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();

        let rows = build_tree(&state).rows();
        assert!(
            rows.iter()
                .any(|r| r.label == "hooks" && matches!(r.kind, menu::MenuRowKind::Group { .. })),
            "the hooks section must be its own group: {rows:?}"
        );
        let empty_row = rows
            .iter()
            .find(|r| r.label == "no active hook rules")
            .expect("an empty hook list must say so honestly");
        assert_eq!(empty_row.kind, menu::MenuRowKind::Static);
    }

    /// A `pre_tool_use` rule and a `prompt_submitted` rule -- one with a
    /// matcher, one without -- each render as a SELECTABLE, revocable leaf
    /// naming its id, event, matcher, and origin, addressed by its own
    /// index-keyed leaf id (mirroring the allow section's revocable rows).
    #[test]
    fn hook_rules_render_as_revocable_leaves_naming_id_event_match_and_origin() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.hook_rules = vec![
            a_hook_rule("deny-writes", "pre_tool_use", Some("fs.write")),
            a_hook_rule("deny-prompts", "prompt_submitted", None),
        ];

        let rows = build_tree(&state).rows();
        let hook_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.label.starts_with("hook:"))
            .collect();
        assert_eq!(hook_rows.len(), 2, "{rows:?}");

        assert!(
            hook_rows[0].label.contains("deny-writes")
                && hook_rows[0].label.contains("pre_tool_use")
                && hook_rows[0].label.contains("fs.write")
                && hook_rows[0].label.contains("settings.json (merged config)")
                && hook_rows[0].label.contains("(Enter to revoke)"),
            "{}",
            hook_rows[0].label
        );
        assert_eq!(
            hook_rows[0].kind,
            menu::MenuRowKind::Leaf {
                id: format!("{LEAF_REVOKE_HOOK_PREFIX}0")
            }
        );

        // The second row (no matcher) says so honestly rather than
        // omitting the field or claiming a pattern that isn't there.
        assert!(
            hook_rows[1].label.contains("deny-prompts")
                && hook_rows[1].label.contains("prompt_submitted")
                && hook_rows[1].label.contains("every call"),
            "{}",
            hook_rows[1].label
        );
        assert_eq!(
            hook_rows[1].kind,
            menu::MenuRowKind::Leaf {
                id: format!("{LEAF_REVOKE_HOOK_PREFIX}1")
            }
        );
    }

    /// A hook row is never `MenuNode::Static` -- unlike deny/prompt rows,
    /// a hook rule IS revocable from this menu (`Conway::revoke_hook_rule`'s
    /// own doc: session-scoped, never a file rewrite, so there is no
    /// safety-rule reason to lock it the way a deny PATTERN is).
    #[test]
    fn a_hook_row_is_selectable_not_static() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.hook_rules = vec![a_hook_rule("deny-writes", "pre_tool_use", None)];

        let rows = build_tree(&state).rows();
        let row = rows
            .iter()
            .find(|r| r.label.starts_with("hook:"))
            .expect("the hook row must render");
        assert!(
            !matches!(row.kind, menu::MenuRowKind::Static),
            "a revocable hook row must not be read-only: {row:?}"
        );
    }

    // ---- Plugin browser (board item 01M0KARX71A64NTSYTDBVANVPF) ----

    fn plugin_entry(
        id: &str,
        installed: bool,
        summary: &str,
        you_get: &str,
        you_lose: &str,
        costs: &str,
    ) -> crate::tui::state::PluginBrowserEntry {
        crate::tui::state::PluginBrowserEntry {
            id: id.to_string(),
            version: "0.9.0".to_string(),
            installed,
            description: conway::plugin::PluginDescription {
                summary: summary.to_string(),
                you_get: you_get.to_string(),
                you_lose: you_lose.to_string(),
                costs: costs.to_string(),
            },
        }
    }

    /// The header row states the live installed/available counts, and
    /// each plugin's summary appears as part of its own toggle leaf.
    #[test]
    fn the_plugins_group_shows_counts_and_each_plugins_summary() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.plugin_browser = vec![
            plugin_entry(
                "conway.memory",
                true,
                "notes that survive a restart",
                "3 tools",
                "nothing else",
                "a small read every turn",
            ),
            plugin_entry(
                "conway.trim",
                false,
                "drops old tool results to save room",
                "smaller context",
                "older tool results",
                "none",
            ),
        ];

        let text = plain_rows(&state);
        assert!(text.contains("plugins"), "{text}");
        assert!(text.contains("1 installed"), "{text}");
        assert!(text.contains("1 available"), "{text}");
        assert!(text.contains("notes that survive a restart"), "{text}");
        assert!(
            text.contains("drops old tool results to save room"),
            "{text}"
        );
    }

    /// `01M0RW3CPE8SG3PZ2J8RTK9Y9N`, acceptance criterion 1: a plugin's own
    /// "you get"/"you lose"/"costs" text no longer sits in the main list at
    /// all -- it moved to the detail panel (covered below). A person
    /// scanning the flattened row list sees only the header and one leaf
    /// per plugin, nothing that could be mistaken for a second kind of
    /// selectable-looking row.
    #[test]
    fn the_plugins_list_no_longer_carries_you_get_you_lose_costs_as_rows() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.plugin_browser = vec![plugin_entry(
            "conway.memory",
            true,
            "notes that survive a restart",
            "3 tools \u{b7} /memory",
            "nothing else -- recall falls back to context",
            "a small read at the start of every turn",
        )];

        let text = plain_rows(&state);
        assert!(
            !text.contains("you get")
                && !text.contains("you lose")
                && !text.contains("costs")
                && !text.contains("recall falls back to context"),
            "the info text must not appear in the flattened row list any more \
             -- it belongs in the detail panel, not the switch list: {text}"
        );

        // Exactly one row per plugin (the header aside): the toggle leaf,
        // and nothing else -- no orphaned static children left behind.
        let rows = build_tree(&state).rows();
        let plugin_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.label.contains("conway.memory"))
            .collect();
        assert_eq!(
            plugin_rows.len(),
            1,
            "a plugin must contribute exactly one row to the list: {rows:?}"
        );
    }

    /// `01M0RW3CPE8SG3PZ2J8RTK9Y9N`, acceptance criterion 2: the toggle
    /// leaf's label leads with a visible `[x]`/`[ ]` box, not only the word
    /// "on"/"off" -- the same bracket marker `view/menu.rs::draw` already
    /// uses for a group's own `[-]`/`[+]` expand state.
    #[test]
    fn plugin_toggle_rows_show_a_checkbox_reflecting_installed_state() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.plugin_browser = vec![
            plugin_entry(
                "conway.memory",
                true,
                "notes that survive a restart",
                "3 tools",
                "nothing else",
                "a small read",
            ),
            plugin_entry(
                "conway.trim",
                false,
                "drops old tool results",
                "less context",
                "older results",
                "none",
            ),
        ];

        let rows = build_tree(&state).rows();
        let on_row = rows
            .iter()
            .find(|r| r.label.contains("conway.memory"))
            .expect("the installed plugin's row must render");
        assert!(
            on_row.label.starts_with("[x] "),
            "an installed plugin's row must open with a checked box: {}",
            on_row.label
        );
        assert!(on_row.label.contains("turn off"), "{}", on_row.label);

        let off_row = rows
            .iter()
            .find(|r| r.label.contains("conway.trim"))
            .expect("the uninstalled plugin's row must render");
        assert!(
            off_row.label.starts_with("[ ] "),
            "an uninstalled plugin's row must open with an UNchecked box: {}",
            off_row.label
        );
        assert!(off_row.label.contains("turn on"), "{}", off_row.label);
    }

    /// The toggle leaf is the SELECTABLE row (never `Static`), keyed by
    /// the plugin's own id via `LEAF_TOGGLE_PLUGIN_PREFIX` -- the row
    /// `input::activate_settings_selection` resolves `Action::
    /// TogglePlugin` against.
    #[test]
    fn the_toggle_row_is_selectable_and_keyed_by_the_plugin_id() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.plugin_browser = vec![plugin_entry(
            "conway.memory",
            true,
            "notes that survive a restart",
            "3 tools",
            "nothing else",
            "a small read",
        )];

        let rows = build_tree(&state).rows();
        let toggle_row = rows
            .iter()
            .find(|r| matches!(&r.kind, menu::MenuRowKind::Leaf { id } if id.starts_with(LEAF_TOGGLE_PLUGIN_PREFIX)))
            .expect("the toggle row must render");
        assert_eq!(
            toggle_row.kind,
            menu::MenuRowKind::Leaf {
                id: format!("{LEAF_TOGGLE_PLUGIN_PREFIX}conway.memory")
            }
        );
        assert!(
            toggle_row.label.contains("turn off"),
            "{}",
            toggle_row.label
        );
    }

    /// An off plugin's own toggle row offers "turn on", not "turn off".
    #[test]
    fn an_off_plugins_toggle_row_offers_turn_on() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.plugin_browser = vec![plugin_entry(
            "conway.trim",
            false,
            "drops old tool results",
            "less context",
            "older results",
            "none",
        )];

        let rows = build_tree(&state).rows();
        let toggle_row = rows
            .iter()
            .find(|r| matches!(&r.kind, menu::MenuRowKind::Leaf { id } if id.starts_with(LEAF_TOGGLE_PLUGIN_PREFIX)))
            .expect("the toggle row must render");
        assert!(toggle_row.label.contains("turn on"), "{}", toggle_row.label);
    }

    /// Finds the flattened row INDEX of `plugin_id`'s own toggle leaf --
    /// tests use this to point `state.settings_selected` directly at it
    /// (mirroring `build_tree_restores_the_persisted_cursor`'s own "set the
    /// raw index" precedent above) rather than driving `Up`/`Down` through
    /// `input.rs`, which this module does not depend on.
    fn plugin_toggle_row_index(state: &AppState, plugin_id: &str) -> usize {
        build_tree(state)
            .rows()
            .iter()
            .position(|r| {
                matches!(&r.kind, menu::MenuRowKind::Leaf { id }
                    if id == &format!("{LEAF_TOGGLE_PLUGIN_PREFIX}{plugin_id}"))
            })
            .unwrap_or_else(|| panic!("no toggle row for {plugin_id}"))
    }

    /// `01M0RW3CPE8SG3PZ2J8RTK9Y9N`, acceptance criterion 3: the "you get" /
    /// "you lose" / "costs" text is still reachable -- moved into the
    /// detail panel [`draw`] renders below the list while the cursor sits
    /// on that plugin's own toggle row, the operator's own framing kept
    /// literally (unchanged from the original item).
    #[test]
    fn the_detail_panel_shows_the_selected_plugins_own_info_literally() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.plugin_browser = vec![plugin_entry(
            "conway.memory",
            true,
            "notes that survive a restart",
            "3 tools \u{b7} /memory",
            "nothing else -- recall falls back to context",
            "a small read at the start of every turn",
        )];
        state.settings_selected = plugin_toggle_row_index(&state, "conway.memory");

        let text = render(&state, 120, 40);
        assert!(text.contains("you get"), "{text}");
        assert!(text.contains("3 tools"), "{text}");
        assert!(text.contains("you lose"), "{text}");
        assert!(
            text.contains("nothing else -- recall falls back to context"),
            "{text}"
        );
        assert!(text.contains("costs"), "{text}");
        assert!(
            text.contains("a small read at the start of every turn"),
            "{text}"
        );
    }

    /// The detail panel tracks the SELECTED plugin, not merely "some"
    /// plugin -- selecting a different row shows different info, proving
    /// the panel is a live lookup rather than the first entry rendered
    /// unconditionally.
    #[test]
    fn the_detail_panel_changes_with_the_selected_plugin() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.plugin_browser = vec![
            plugin_entry(
                "conway.memory",
                true,
                "notes that survive a restart",
                "memory-only-you-get",
                "memory-only-you-lose",
                "memory-only-costs",
            ),
            plugin_entry(
                "conway.trim",
                false,
                "drops old tool results",
                "trim-only-you-get",
                "trim-only-you-lose",
                "trim-only-costs",
            ),
        ];

        state.settings_selected = plugin_toggle_row_index(&state, "conway.memory");
        let memory_text = render(&state, 120, 40);
        assert!(memory_text.contains("memory-only-you-get"), "{memory_text}");
        assert!(
            !memory_text.contains("trim-only-you-get"),
            "selecting one plugin must not show the OTHER plugin's info: {memory_text}"
        );

        state.settings_selected = plugin_toggle_row_index(&state, "conway.trim");
        let trim_text = render(&state, 120, 40);
        assert!(trim_text.contains("trim-only-you-get"), "{trim_text}");
        assert!(
            !trim_text.contains("memory-only-you-get"),
            "selecting the other plugin must switch the panel, not append: {trim_text}"
        );
    }

    /// No detail panel renders while browsing a NON-plugin section -- the
    /// panel only appears once the cursor genuinely rests on a plugin's own
    /// toggle row.
    #[test]
    fn no_detail_panel_renders_when_the_selection_is_not_a_plugin_row() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.plugin_browser = vec![plugin_entry(
            "conway.memory",
            true,
            "notes that survive a restart",
            "a-distinctive-you-get-value",
            "nothing else",
            "a small read",
        )];
        // Cursor starts on the very first row ("display", a group) --
        // nowhere near the plugins section.
        assert_eq!(state.settings_selected, 0);

        let text = render(&state, 120, 40);
        assert!(
            !text.contains("a-distinctive-you-get-value"),
            "no plugin's info must leak into the panel area while a different \
             section is selected: {text}"
        );
    }

    /// A plugin with an empty description field renders an honest
    /// fallback, never a blank line that looks like a rendering bug -- now
    /// checked against the DETAIL PANEL, where these fields actually
    /// render (the toggle leaf's own label only carries `summary`).
    #[test]
    fn an_empty_description_field_renders_an_honest_fallback_not_a_blank() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.plugin_browser = vec![plugin_entry(
            "conway.plugin_skeleton",
            false,
            "",
            "",
            "",
            "",
        )];
        state.settings_selected = plugin_toggle_row_index(&state, "conway.plugin_skeleton");

        let list_text = plain_rows(&state);
        assert!(list_text.contains("(no description)"), "{list_text}");

        let rendered = render(&state, 120, 40);
        assert!(rendered.contains("(none given)"), "{rendered}");
        assert!(rendered.contains("none"), "{rendered}");
    }

    /// The footer states plainly that a plugin toggle applies on next
    /// restart -- acceptance criterion 5 (renumbered from the original
    /// item's criterion 4).
    #[test]
    fn the_footer_states_plugin_toggles_apply_on_next_restart() {
        let state = AppState::new(AgentId::new());
        let text = render(&state, 120, 40);
        assert!(text.contains("restart"), "{text}");
        assert!(text.contains(SESSION_NOTE), "{text}");
    }

    /// A tiny terminal with a plugin selected (so the detail panel's own
    /// row reservation is genuinely exercised) still never panics -- the
    /// same property `draw_never_panics_on_a_tiny_terminal` already proves
    /// for the base tree, re-checked here with the detail panel's own
    /// `Layout::split` in the path.
    #[test]
    fn draw_with_a_selected_plugin_never_panics_on_a_tiny_terminal() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.plugin_browser = vec![plugin_entry(
            "conway.memory",
            true,
            "notes that survive a restart",
            "3 tools",
            "nothing else",
            "a small read",
        )];
        state.settings_selected = plugin_toggle_row_index(&state, "conway.memory");

        for (w, h) in [(80u16, 1u16), (80, 2), (1, 24), (0, 0)] {
            let backend = TestBackend::new(w.max(1), h.max(1));
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| draw(f, f.area(), &state, &Theme::default()))
                .unwrap_or_else(|e| panic!("panicked/errored at {w}x{h}: {e}"));
        }
    }
}
