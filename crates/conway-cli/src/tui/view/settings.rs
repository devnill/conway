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
//! Two [`MenuNode::Group`]s -- "display" (the two booleans) and "tool
//! output" (the one numeric setting) -- rather than one flat group or three
//! separate ones: this is genuinely the shape a THIRD settings category
//! (say, session history) would extend later, not artificial nesting
//! invented only to exercise the primitive.
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

/// The two top-level group labels (see this module's own doc, "Grouping").
/// `pub(crate)` for the same reason the leaf ids are -- `input.rs` and this
/// module must agree on the SAME strings, since [`crate::tui::state::
/// AppState::settings_collapsed_groups`] is keyed by them.
const DISPLAY_GROUP: &str = "display";
const TOOL_OUTPUT_GROUP: &str = "tool output";
const PERMISSIONS_GROUP: &str = "permissions";

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
        // V2b. The grant list is rendered as non-selectable label text
        // rather than as leaves: per-rule revocation is not implemented,
        // so a selectable row that did nothing on Enter would be a worse
        // lie than a plainly inert one. Revoke-all is the shipped floor.
        group_node(PERMISSIONS_GROUP, state, {
            let mut rows = vec![MenuNode::leaf(
                format!(
                    "mode -- {} (Enter to cycle)",
                    state.permission_mode.label()
                ),
                LEAF_PERMISSION_MODE,
            )];
            if state.permission_grants.is_empty() {
                rows.push(MenuNode::leaf("no active grants".to_string(), ""));
            } else {
                for grant in &state.permission_grants {
                    rows.push(MenuNode::leaf(format!("granted: {grant}"), ""));
                }
                rows.push(MenuNode::leaf(
                    "revoke all grants (Enter)".to_string(),
                    LEAF_REVOKE_GRANTS,
                ));
            }
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

        state.permission_grants =
            vec!["`bash` commands starting with `git status`".to_string()];
        let text = plain_rows(&state);
        assert!(text.contains("granted: `bash` commands starting with `git status`"), "{text}");
        assert!(
            text.contains("revoke all grants"),
            "revoke-all appears only once there is something to revoke: {text}"
        );
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

}
