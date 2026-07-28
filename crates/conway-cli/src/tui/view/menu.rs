//! A selectable, keyboard-navigable **tree** primitive (V1), layered on the
//! shared modal ([`super::modal`]) rather than deciding its own
//! overlay/`Rect` shape -- V4's settings tree draws its content into
//! whatever `body_area` [`super::modal::draw_modal_frame`] hands it, the
//! same way every OTHER ported surface in this item does.
//!
//! **Not wired to anything yet** (item spec: "Do not wire it to anything
//! yet -- V1 provides the primitive; V4 fills it"). This module owns the
//! data structure ([`MenuNode`]), the navigation state machine
//! ([`MenuState`]), and the render fn ([`draw`]); nothing in `app.rs` or
//! `input.rs` constructs a [`MenuState`] yet. It is still fully exercised by
//! this module's own tests (below) so V4 can build directly on it without
//! first having to fix a half-finished primitive.
//!
//! ## Nested groups, not a flat list
//!
//! A flat list (what [`super::agents::draw`] already is) is not enough for
//! a settings tree, which has genuinely nested sections. [`MenuNode`] is
//! either a [`MenuNode::Leaf`] (a selectable value, opaque `id` the caller
//! interprets) or a [`MenuNode::Group`] (a label plus child nodes,
//! independently collapsible). [`MenuState`] flattens the CURRENTLY VISIBLE
//! rows (a collapsed group hides its children, mirroring how
//! `view/agents.rs`'s draw-time visibility filter hides rows without
//! mutating the underlying tree) fresh on every navigation/render call --
//! the tree itself is never mutated except for a group's own `expanded`
//! flag, so there is no separate "flattened cache" that could drift out of
//! sync with the tree.
//!
//! ## How this slots into V3's arrow-key priority chain
//!
//! `input.rs::handle_normal_key`'s `Up`/`Down` arms resolve, in order:
//! palette -> agent panel -> multi-line draft interior -> bare transcript
//! line-scroll (V3, `a562550`). None of THIS module's future key-handling
//! joins that chain at all -- exactly like `/help` (T7), a settings surface
//! built on this primitive is **informational**, not decision-owed (see
//! `AppState::help_open`'s own doc on that distinction), so it belongs
//! alongside `/help`'s own top-level check in `input::handle_key` --
//! `if state.settings_open && matches!(state.mode, Mode::Normal) { return
//! handle_settings_key(state, key) }` checked BEFORE the `mode` match, the
//! same way `state.help_open` already is. While such a flag is set, its own
//! handler owns Up/Down completely (calling [`MenuState::move_selection`])
//! and returns before `handle_normal_key`'s chain is ever reached, so wheel-
//! scroll (which arrives as bare `Up`/`Down` via a terminal's alternate-
//! scroll mode -- V3's own module doc) is never swallowed while no such
//! surface is open, and is legitimately, deliberately claimed by the menu
//! -- never permanently -- while one is, the exact same trade the agent
//! panel already makes for its own arrows. V4 is expected to add that flag
//! and its handler; this module supplies the [`MenuState`] navigation calls
//! it would need, already tested standalone.

//! `MenuNode`/`MenuState`/`draw` are not constructed anywhere outside this
//! module's own tests yet -- V4 (the settings tree) is later work that
//! supplies the actual content and wires a key handler onto
//! [`MenuState::move_selection`]/[`MenuState::toggle_group_at_selection`].
//! The `#[allow(dead_code)]` below is scoped to this file for exactly that
//! reason (mirroring `conway-backends/src/http.rs`'s own precedent for the
//! same "primitive lands before its first caller" shape) and should be
//! revisited once V4 lands.
#![allow(dead_code)]

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};
use ratatui::Frame;

use super::theme::Theme;

/// One node of a selectable settings/menu tree.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuNode {
    /// A selectable value. `id` is opaque to this module -- V4 interprets it
    /// however its own settings schema needs (a dotted config path, an enum
    /// tag, ...); [`MenuState`] never inspects it beyond returning it.
    Leaf { label: String, id: String },
    /// A collapsible group of child nodes. `expanded` starts `true` (a
    /// settings tree is more useful to browse fully open than to have to
    /// expand every section first) and is flipped by
    /// [`MenuState::toggle_group_at_selection`].
    Group {
        label: String,
        children: Vec<MenuNode>,
        expanded: bool,
    },
}

impl MenuNode {
    /// Convenience constructor for a leaf row.
    pub fn leaf(label: impl Into<String>, id: impl Into<String>) -> Self {
        MenuNode::Leaf {
            label: label.into(),
            id: id.into(),
        }
    }

    /// Convenience constructor for a group, expanded by default.
    pub fn group(label: impl Into<String>, children: Vec<MenuNode>) -> Self {
        MenuNode::Group {
            label: label.into(),
            children,
            expanded: true,
        }
    }
}

/// One row of the CURRENTLY VISIBLE, flattened tree -- what [`MenuState::rows`]
/// returns and [`draw`] renders one [`ListItem`] per. `path` is the sequence
/// of child indices from the roots down to this node, used internally by
/// [`MenuState`] to look the node back up for mutation (toggling a group);
/// callers of [`MenuState::rows`] normally only need `depth`/`label`/`kind`.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuRow {
    path: Vec<usize>,
    pub depth: usize,
    pub label: String,
    pub kind: MenuRowKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MenuRowKind {
    Leaf { id: String },
    Group { expanded: bool },
}

/// The tree's navigation state: which roots exist, and which flattened row
/// is currently selected (the arrow-navigated cursor, mirroring
/// `AppState::agent_selected`'s own shape for the `/agents` panel).
#[derive(Debug, Clone, PartialEq)]
pub struct MenuState {
    roots: Vec<MenuNode>,
    selected: usize,
}

impl MenuState {
    pub fn new(roots: Vec<MenuNode>) -> Self {
        Self { roots, selected: 0 }
    }

    /// The currently visible, flattened rows -- a collapsed group's children
    /// are omitted entirely (not rendered as disabled/grayed rows), the same
    /// "hidden, not merely styled differently" shape
    /// `AgentVisibility::shows` already uses for the `/agents` panel.
    pub fn rows(&self) -> Vec<MenuRow> {
        let mut out = Vec::new();
        flatten(&self.roots, 0, &mut Vec::new(), &mut out);
        out
    }

    /// The selected row's index, clamped to the CURRENT flattened row count
    /// (never past the end, even right after a collapse shortened the
    /// list -- mirrors `AppState::clamp_agent_selected`'s own re-clamp
    /// shape).
    pub fn selected_index(&self) -> usize {
        let n = self.rows().len();
        if n == 0 {
            0
        } else {
            self.selected.min(n - 1)
        }
    }

    /// Moves the selection by `delta` rows, clamped at both ends (no wrap --
    /// mirrors `AppState::agent_scroll`'s own browsing-list shape).
    pub fn move_selection(&mut self, delta: isize) {
        let n = self.rows().len();
        if n == 0 {
            return;
        }
        let max = (n - 1) as isize;
        let cur = self.selected_index() as isize;
        self.selected = (cur + delta).clamp(0, max) as usize;
    }

    /// The currently selected row, if any (`None` only when the tree is
    /// entirely empty).
    pub fn selected_row(&self) -> Option<MenuRow> {
        self.rows().into_iter().nth(self.selected_index())
    }

    /// Flips the selected row's `expanded` flag if it is a group; a no-op on
    /// a leaf row (there is nothing to expand/collapse) or an empty tree.
    /// Re-clamps the selection afterward the same way
    /// [`super::super::state::AppState::cycle_agent_visibility`] re-clamps
    /// `agent_selected` after a filter change shortens the row list --
    /// collapsing a group whose OWN row was selected never leaves the
    /// cursor dangling past the new, shorter list.
    pub fn toggle_group_at_selection(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if !matches!(row.kind, MenuRowKind::Group { .. }) {
            return;
        }
        if let Some(MenuNode::Group { expanded, .. }) = node_at_mut(&mut self.roots, &row.path) {
            *expanded = !*expanded;
        }
        self.selected = self.selected_index();
    }

    /// The selected LEAF's `id`, or `None` when the selection is on a group
    /// row (or the tree is empty) -- the caller's "activate" action (an
    /// `Enter` key, say) checks this first and falls back to
    /// [`Self::toggle_group_at_selection`] when it's `None` and the row is a
    /// group, mirroring how `input.rs`'s `Enter` on the `/agents` panel
    /// resolves through the filtered row list rather than assuming a shape.
    pub fn selected_leaf_id(&self) -> Option<String> {
        match self.selected_row()?.kind {
            MenuRowKind::Leaf { id } => Some(id),
            MenuRowKind::Group { .. } => None,
        }
    }
}

fn flatten(nodes: &[MenuNode], depth: usize, path: &mut Vec<usize>, out: &mut Vec<MenuRow>) {
    for (i, node) in nodes.iter().enumerate() {
        path.push(i);
        match node {
            MenuNode::Leaf { label, id } => {
                out.push(MenuRow {
                    path: path.clone(),
                    depth,
                    label: label.clone(),
                    kind: MenuRowKind::Leaf { id: id.clone() },
                });
            }
            MenuNode::Group {
                label,
                children,
                expanded,
            } => {
                out.push(MenuRow {
                    path: path.clone(),
                    depth,
                    label: label.clone(),
                    kind: MenuRowKind::Group {
                        expanded: *expanded,
                    },
                });
                if *expanded {
                    flatten(children, depth + 1, path, out);
                }
            }
        }
        path.pop();
    }
}

/// Looks up the node at `path` (a sequence of child indices from the roots)
/// for mutation. `None` if the path is stale (should not happen in practice
/// -- [`MenuState::rows`] and this lookup always walk the SAME `roots`
/// between calls with no mutation in between other than through
/// [`MenuState`]'s own methods), handled defensively rather than assumed
/// (P-10: no `unwrap`/indexing panic on a path that could in principle be
/// stale).
fn node_at_mut<'a>(nodes: &'a mut [MenuNode], path: &[usize]) -> Option<&'a mut MenuNode> {
    let (first, rest) = path.split_first()?;
    let node = nodes.get_mut(*first)?;
    if rest.is_empty() {
        Some(node)
    } else {
        match node {
            MenuNode::Group { children, .. } => node_at_mut(children, rest),
            MenuNode::Leaf { .. } => None,
        }
    }
}

/// Renders `menu`'s currently visible rows into `area` (typically a modal's
/// own `body_area`, per this module's own doc), indented by depth, with a
/// group's `expanded` state shown via a `[-]`/`[+]` marker (ASCII, matching
/// `view/agents.rs`'s own status-marker convention rather than a Unicode
/// glyph) and the selected row highlighted via `theme.selected` -- the SAME
/// highlight style the `/agents` panel's arrow-selected row uses, so a
/// settings tree built on this primitive looks like it belongs next to the
/// other on-demand surfaces rather than inventing its own accent.
pub fn draw(frame: &mut Frame, area: Rect, menu: &MenuState, theme: &Theme) {
    let rows = menu.rows();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| {
            let indent = "  ".repeat(row.depth);
            let marker = match &row.kind {
                MenuRowKind::Group { expanded: true } => "[-] ",
                MenuRowKind::Group { expanded: false } => "[+] ",
                MenuRowKind::Leaf { .. } => "    ",
            };
            let style = match &row.kind {
                MenuRowKind::Group { .. } => theme.emphasized,
                MenuRowKind::Leaf { .. } => theme.dim,
            };
            ListItem::new(Line::from(vec![
                Span::raw(indent),
                Span::raw(marker),
                Span::styled(row.label.clone(), style),
            ]))
        })
        .collect();

    let list = List::new(items).highlight_style(theme.selected);
    let mut list_state = ListState::default();
    if !rows.is_empty() {
        list_state.select(Some(menu.selected_index()));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tree() -> Vec<MenuNode> {
        vec![
            MenuNode::group(
                "display",
                vec![
                    MenuNode::leaf("theme", "display.theme"),
                    MenuNode::group(
                        "status line",
                        vec![
                            MenuNode::leaf("model", "display.status_line.model"),
                            MenuNode::leaf("ctx", "display.status_line.ctx"),
                        ],
                    ),
                ],
            ),
            MenuNode::leaf("history size", "history_size"),
        ]
    }

    // ---- flattening: nested groups render as a tree, not a flat list ----

    #[test]
    fn rows_walks_nested_groups_in_document_order_with_correct_depth() {
        let state = MenuState::new(sample_tree());
        let rows = state.rows();

        let labels_and_depths: Vec<(String, usize)> =
            rows.iter().map(|r| (r.label.clone(), r.depth)).collect();
        assert_eq!(
            labels_and_depths,
            vec![
                ("display".to_string(), 0),
                ("theme".to_string(), 1),
                ("status line".to_string(), 1),
                ("model".to_string(), 2),
                ("ctx".to_string(), 2),
                ("history size".to_string(), 0),
            ],
            "nested groups must flatten in document order with depth reflecting nesting"
        );
    }

    #[test]
    fn collapsing_a_group_hides_its_children_but_not_the_group_row_itself() {
        let mut state = MenuState::new(sample_tree());
        // Select "display" (row 0) and collapse it.
        state.toggle_group_at_selection();

        let labels: Vec<String> = state.rows().iter().map(|r| r.label.clone()).collect();
        assert_eq!(
            labels,
            vec!["display".to_string(), "history size".to_string()],
            "a collapsed group's children must be hidden, but the group's own \
             row must still render"
        );
    }

    #[test]
    fn expanding_a_collapsed_group_restores_its_children() {
        let mut state = MenuState::new(sample_tree());
        state.toggle_group_at_selection(); // collapse
        state.toggle_group_at_selection(); // expand again (involution)

        assert_eq!(state.rows().len(), 6, "re-expanding must restore every child row");
    }

    #[test]
    fn toggling_a_leaf_row_is_a_noop() {
        let mut state = MenuState::new(sample_tree());
        state.move_selection(1); // now on "theme", a leaf
        let before = state.rows();

        state.toggle_group_at_selection();

        assert_eq!(state.rows(), before, "a leaf row has nothing to toggle");
    }

    // ---- navigation: clamps at both ends, no wrap ----

    #[test]
    fn move_selection_clamps_at_the_last_row() {
        let mut state = MenuState::new(sample_tree());
        for _ in 0..20 {
            state.move_selection(1);
        }
        assert_eq!(state.selected_index(), 5, "must clamp at the last visible row, not wrap");
    }

    #[test]
    fn move_selection_clamps_at_the_first_row() {
        let mut state = MenuState::new(sample_tree());
        state.move_selection(3);
        for _ in 0..20 {
            state.move_selection(-1);
        }
        assert_eq!(state.selected_index(), 0, "must clamp at the first row, not go negative");
    }

    #[test]
    fn collapsing_the_selected_groups_parent_reclamps_the_selection() {
        let mut state = MenuState::new(sample_tree());
        // Select "ctx" (the last leaf under "status line", row 4).
        state.move_selection(4);
        assert_eq!(state.selected_row().unwrap().label, "ctx");

        // Move back up to "display" (row 0) and collapse it -- the row list
        // shrinks from 6 to 2.
        state.move_selection(-4);
        state.toggle_group_at_selection();

        assert_eq!(state.rows().len(), 2);
        assert!(
            state.selected_index() < state.rows().len(),
            "the selection must never point past the new, shorter row list"
        );
    }

    // ---- selection resolves through the tree, mirroring the /agents panel's
    // filtered-row-lookup precedent ----

    #[test]
    fn selected_leaf_id_returns_none_while_a_group_row_is_selected() {
        let state = MenuState::new(sample_tree());
        assert_eq!(state.selected_row().unwrap().label, "display");
        assert_eq!(state.selected_leaf_id(), None);
    }

    #[test]
    fn selected_leaf_id_returns_the_opaque_id_for_a_leaf_row() {
        let mut state = MenuState::new(sample_tree());
        state.move_selection(1); // "theme"
        assert_eq!(state.selected_leaf_id(), Some("display.theme".to_string()));
    }

    #[test]
    fn selected_leaf_id_resolves_a_nested_leaf_correctly() {
        let mut state = MenuState::new(sample_tree());
        state.move_selection(3); // "model", nested two levels deep
        assert_eq!(state.selected_row().unwrap().label, "model");
        assert_eq!(
            state.selected_leaf_id(),
            Some("display.status_line.model".to_string())
        );
    }

    // ---- P-10: an empty tree never panics ----

    #[test]
    fn an_empty_tree_never_panics_on_navigation_or_lookup() {
        let mut state = MenuState::new(Vec::new());
        state.move_selection(1);
        state.move_selection(-1);
        state.toggle_group_at_selection();
        assert_eq!(state.selected_index(), 0);
        assert_eq!(state.selected_row(), None);
        assert_eq!(state.selected_leaf_id(), None);
    }

    // ---- rendering: composes with the shared modal primitive without
    // panicking, and highlights the selected row ----

    #[test]
    fn draw_renders_every_visible_row_without_panicking() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let state = MenuState::new(sample_tree());
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), &state, &Theme::default()))
            .expect("draw");

        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("display"));
        assert!(text.contains("theme"));
        assert!(text.contains("history size"));
    }

    #[test]
    fn draw_highlights_the_selected_row() {
        use ratatui::backend::TestBackend;
        use ratatui::style::Modifier;
        use ratatui::Terminal;

        let mut state = MenuState::new(sample_tree());
        state.move_selection(1);

        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), &state, &Theme::default()))
            .expect("draw");

        let any_reversed = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|c| c.modifier.contains(Modifier::REVERSED));
        assert!(any_reversed, "the selected row must render highlighted");
    }

    /// Proves this primitive genuinely composes with the shared modal
    /// (`super::modal`) rather than needing its own overlay math -- exactly
    /// the shape V4 is expected to use: a modal frame's own `body_area` is
    /// what [`draw`] renders into.
    #[test]
    fn draw_composes_with_the_shared_modal_frame() {
        use ratatui::backend::TestBackend;
        use ratatui::style::Style;
        use ratatui::Terminal;

        let state = MenuState::new(sample_tree());
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                let transcript_area = f.area();
                let frame = super::super::modal::draw_modal_frame(
                    f,
                    transcript_area,
                    state.rows().len() as u16,
                    1,
                    super::super::modal::DEFAULT_CAP_DENOMINATOR,
                    " SETTINGS ",
                    Style::default(),
                );
                draw(f, frame.body_area, &state, &Theme::default());
            })
            .expect("draw");

        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("SETTINGS"));
        assert!(text.contains("display"));
    }
}
