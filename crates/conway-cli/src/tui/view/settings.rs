//! The `/settings` menu (V4): a session-only display-preferences tree drawn
//! on V1's shared [`super::modal`] + [`super::menu`] primitives -- the first
//! real caller of [`super::menu`], which existed only as an
//! exercised-by-its-own-tests primitive before this item (see that module's
//! own doc).
//!
//! ## Session-only, not `settings.json`
//!
//! Conway's config load (`conway::config::merge::load`) is a five-source
//! layered read with no writer anywhere outside test fixtures -- persisting
//! a runtime toggle would mean inventing one, and answering "which LAYER
//! gets written" (default/XDG/project/env/CLI) has no good default answer.
//! That question is out of THIS item's scope by design. This menu changes
//! `AppState` at runtime only, exactly the way the two slash commands it
//! replaces (`/thinking`, `/timestamps` -- both REMOVED, not aliased, see
//! `commands.rs`'s parser) already did; [`SESSION_NOTE`] says so in the
//! footer on every render, and the one leaf with a real backing config key
//! (`tool_preview_lines`) names it inline (see [`build_tree`]'s own doc) --
//! the other two have no config-key equivalent to point to at all today, so
//! they carry no such annotation.
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
//! Three top-level [`MenuNode::Group`]s -- "display" (the two booleans),
//! "tool output" (the one numeric setting), and "permissions" (the mode,
//! plus allow/deny/prompt rule review as three SUB-groups -- see
//! [`build_tree`]'s own doc for why they are separate sections and why
//! deny/prompt rows are read-only [`MenuNode::Static`] rows) -- rather
//! than one flat group or several separate ones: this is genuinely the
//! shape a further settings category (say, session history) would extend
//! later, not artificial nesting invented only to exercise the primitive.
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

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
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
/// Board item 01KYND4WGHSZXW5YQ6ZWHCDDNN: prefix for one grant row's own
/// leaf id, `"{LEAF_REVOKE_GRANT_PREFIX}{index}"` where `index` is the
/// row's position in `state.permission_grants` at the moment this tree was
/// built. `input::activate_settings_selection` resolves that index back
/// into `state.permission_grants` in the SAME call that built this tree, so
/// there is no window in which the index could point at a different grant
/// than the one rendered — see that function's own doc.
pub(crate) const LEAF_REVOKE_GRANT_PREFIX: &str = "revoke_grant:";
/// Board item A2: prefix for one STRUCTURED allow rule's own leaf id,
/// `"{LEAF_REVOKE_STRUCTURED_ALLOW_PREFIX}{index}"` where `index` is the
/// row's position in `state.structured_allow_rules` at the moment this tree
/// was built -- a DISTINCT id space from [`LEAF_REVOKE_GRANT_PREFIX`] so a
/// flat revocation and a structured revocation can never resolve against
/// each other's mirror (the two lists are indexed independently, and
/// `input::activate_settings_selection` resolves each prefix against its
/// OWN mirror in the same call that built this tree, exactly as the flat
/// path does -- see that function's own doc).
pub(crate) const LEAF_REVOKE_STRUCTURED_ALLOW_PREFIX: &str = "revoke_structured_allow:";

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

/// The footer's session-only disclosure (this item's decision record: no
/// `settings.json` writer exists, and inventing one raises a "which layer"
/// question out of scope here). Shown on every render, regardless of which
/// row is selected -- the whole menu is session-only, not just some rows.
const SESSION_NOTE: &str = "changes apply to this session only";

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
        // Board item 01KYND4WGHSZXW5YQ6ZWHCDDNN: each active grant is now a
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
                        // Board item A2: the structured allow rules the flat
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
            ];
            rows
        }),
    ];
    let mut menu = MenuState::new(roots);
    menu.set_selected(state.settings_selected);
    menu
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

fn bool_label(name: &str, value: bool) -> String {
    format!("{name} -- {}", if value { "on" } else { "off" })
}

/// Rows the settings modal's footer ALWAYS reserves: the key hint, plus the
/// session-only disclosure -- mirroring every other ported surface's
/// "footer rows are fixed, never squeezed by body growth" invariant
/// (`view/modal.rs`'s own doc).
const FOOTER_ROWS: u16 = 2;

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

    let frame_areas = modal::draw_modal_frame(
        frame,
        transcript_area,
        content_rows,
        FOOTER_ROWS,
        CAP_DENOMINATOR,
        " SETTINGS ",
        theme.help_border,
    );

    menu::draw(frame, frame_areas.body_area, &tree, theme);

    let footer_lines = vec![
        Line::from("[Up/Down] move  [Enter] toggle/expand  [Left/Right] adjust  [Esc] close"),
        Line::from(Span::styled(SESSION_NOTE, theme.dim)),
    ];
    let footer = Paragraph::new(footer_lines).wrap(Wrap { trim: true });
    frame.render_widget(footer, frame_areas.footer_area);
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

    /// Board item 01KYND4WGHSZXW5YQ6ZWHCDDNN: each grant row is now a real
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

    // ---- Structured allow rows: visible AND revocable (board item A2) ----

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
}
