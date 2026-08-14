//! The below-chat agent-tree panel (criterion 4): shown on demand
//! (toggled by `/agents`, handled in `app.rs` since `commands.rs` is out of
//! this item's file scope) rather than as an always-on side pane. Ordinary
//! subagent lifecycle is ALSO surfaced inline in the conversation stream
//! itself (`transcript.rs`'s `Entry::Agent` handling) -- this panel is for
//! browsing the whole tree at a glance, not the only place activity shows.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use conway::SubagentMode;

use super::theme::Theme;
use crate::tui::state::{AppState, NodeStatus, TreeNode};

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    // Item A2: the visibility filter lives entirely HERE, at draw time --
    // `state.tree` itself is never filtered -- provenance survives -- so
    // finished agents are hidden, not removed. `visible` is the only place the
    // `AgentVisibility` mode takes effect. Row indices (selection, focus)
    // are indices into this filtered list.
    let visible: Vec<&TreeNode> = state.visible_agent_nodes().collect();
    let items: Vec<ListItem> = visible
        .iter()
        .map(|node| {
            let depth = ancestor_depth(state, node.agent_id);
            let indent = "  ".repeat(depth);
            let label = node
                .agent_def
                .clone()
                .unwrap_or_else(|| "agent".to_string());
            let marker = status_marker(node.status);
            // the agent whose conversation the transcript pane
            // currently shows gets an explicit, textual tag -- distinct
            // from `agent_selected`'s own reversed-highlight (the browsing
            // cursor), which ratatui already applies via `ListState`
            // below, and which need not be the same row at all.
            let focus_tag = if node.agent_id == state.focused_agent {
                " (focused)"
            } else {
                ""
            };
            let mut spans = vec![
                Span::raw(indent),
                Span::styled(marker, status_style(node.status, theme)),
                Span::raw(" "),
                Span::raw(label),
            ];
            // Item A2: the recipe label (what context recipe this agent was
            // spawned with), dimmed so it reads as annotation next to the
            // row's own label.
            for part in recipe_parts(node) {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(part, theme.dim));
            }
            spans.push(Span::styled(focus_tag, theme.focused));
            ListItem::new(Line::from(spans))
        })
        .collect();
    let title = format!(
        "agents ({} · ↑/↓ scroll · v filter · esc)",
        state.agent_visibility.label()
    );
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(theme.border_normal),
        )
        // The arrow-selected row. Using a `ListState` (rather than
        // pre-styling one `ListItem`) lets ratatui scroll the selection into
        // view when the tree is taller than the panel.
        .highlight_style(theme.selected);
    let mut list_state = ListState::default();
    if !visible.is_empty() {
        list_state.select(Some(state.agent_selected.min(visible.len() - 1)));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

/// Item A2: the recipe-label parts for a row -- what context recipe this
/// agent was spawned with, composed from the A1 `TreeNode` fields
/// (`kind`/`inherited_upto`/`ephemeral`). A pure function so the
/// label formatting is unit-testable with no terminal. Root/legacy nodes
/// (`kind: None`) get no recipe label; an ephemeral node always carries the
/// `(ephemeral)` marker. ASCII, single-line, copy-paste friendly.
///
/// `pub(crate)` so item A3's `/tree` snapshot renderer (`tui::commands`)
/// composes the SAME label text the panel draws instead of duplicating the
/// logic -- the panel and the hidden alias can never drift apart.
pub(crate) fn recipe_parts(node: &TreeNode) -> Vec<String> {
    let mut parts = Vec::new();
    match node.kind {
        Some(SubagentMode::Fork) => match node.inherited_upto {
            Some(seq) => parts.push(format!("fork @seq {}", seq.0)),
            // A fork always records its fork point; if it is somehow
            // missing, degrade to the bare kind rather than dropping the
            // label entirely.
            None => parts.push("fork".to_string()),
        },
        Some(SubagentMode::Spawn) => match &node.agent_def {
            Some(def) => parts.push(format!("@{def}")),
            None => parts.push("(inherit)".to_string()),
        },
        None => {}
    }
    if node.ephemeral {
        parts.push("(ephemeral)".to_string());
    }
    parts
}

/// V5: one ancestry-chain hop's label for the status line's `lineage`
/// breadcrumb (`view/status.rs`; originally on T6's sticky header, relocated
/// by the item that corrected that requirement miss) -- built from
/// [`recipe_parts`] (the SAME provenance
/// text the panel row shows) so the breadcrumb and the panel can never
/// drift apart. When `recipe_parts` has nothing to say (`kind: None`: the
/// root, or a node seeded out-of-band via `ensure_agent_tracked`, which
/// never saw a spawn event -- V5 acceptance: this must render sensibly, not
/// be mislabeled as a fork or a spawn it never was), falls back to the
/// node's own short id so the hop still names WHO even when it cannot name
/// HOW.
pub(crate) fn hop_label(node: &TreeNode) -> String {
    let parts = recipe_parts(node);
    if parts.is_empty() {
        short_agent_id(node.agent_id)
    } else {
        parts.join(" ")
    }
}

/// An `AgentId`'s first 8 characters -- ULIDs are 26-character base32
/// strings, ASCII-only, so slicing by byte can never land mid-character.
/// `pub(crate)` so `view/status.rs`'s `session`/`lineage` fields (which
/// already showed `agent <id>` this way pre-V5, on T6's original sticky
/// header) use this SAME truncation rule for both the plain `agent <id>`
/// field and the lineage breadcrumb, rather than keeping two copies of an
/// identical one-liner.
pub(crate) fn short_agent_id(id: conway::AgentId) -> String {
    id.to_string().chars().take(8).collect()
}

/// V5: a defensive bound on the ancestry walk (untrusted structure -- a cycle
/// in `parent` should be impossible, but must never hang or overflow if it
/// somehow happened). Generous for any tree this TUI will realistically show.
const MAX_ANCESTOR_CHAIN: usize = 64;

/// The root-first ancestry chain for `agent`, `agent` itself included as the
/// LAST element -- e.g. `[root, child, grandchild]` for `grandchild`. Shared
/// bounded walk ([`MAX_ANCESTOR_CHAIN`]) behind both [`ancestor_depth`]
/// (the panel's own indent rule) and V5's lineage breadcrumb
/// (`view/status.rs`), so there is exactly one cycle-safe tree walk rather
/// than two copies that could disagree. A node missing from `state.tree`
/// entirely (should not happen for anything reachable from `agent`, but
/// `focused_agent` fails open elsewhere too) simply ends the walk there
/// rather than panicking.
pub(crate) fn ancestor_chain(state: &AppState, agent: conway::AgentId) -> Vec<conway::AgentId> {
    let mut chain = vec![agent];
    let mut cursor = agent;
    for _ in 0..MAX_ANCESTOR_CHAIN {
        let Some(node) = state.tree.nodes.iter().find(|n| n.agent_id == cursor) else {
            break;
        };
        match node.parent {
            // `contains` (not a `HashSet`) is fine at this scale -- the
            // ancestry chains this walks are, by definition, shallow trees
            // of live agents, never a large flat collection.
            Some(p) if !chain.contains(&p) => {
                chain.push(p);
                cursor = p;
            }
            // No parent (reached the root), or `p` is already in the
            // chain -- a cycle, which should be impossible but must not
            // hang the walk.
            _ => break,
        }
    }
    chain.reverse();
    chain
}

/// `pub(crate)` so item A3's `/tree` snapshot renderer (`tui::commands`)
/// indents by the same ancestor-depth rule the panel rows use.
pub(crate) fn ancestor_depth(state: &AppState, agent: conway::AgentId) -> usize {
    ancestor_chain(state, agent).len() - 1
}

fn status_marker(status: NodeStatus) -> &'static str {
    match status {
        NodeStatus::Starting => "o",
        NodeStatus::Running => "*",
        NodeStatus::AwaitingPermission => "?",
        NodeStatus::Finished => "v",
        NodeStatus::Failed => "x",
        NodeStatus::Cancelled => "-",
    }
}

fn status_style(status: NodeStatus, theme: &Theme) -> Style {
    // Parity pin (T1 review finding 1): pre-T1 the agent panel used
    // `Style::default()` (unstyled, terminal default fg) for `Starting`,
    // while the transcript's inline `Entry::Agent` line used `Color::Gray`.
    // The two call sites genuinely differed, so delegating `Starting` to
    // `transcript::node_status_style` (which returns `theme.agent_starting`
    // = Gray) would be a re-skin, not a refactor. Special-case `Starting`
    // to unstyled here; the other five statuses match pre-T1 parity and
    // delegate to the shared mapping so the panel and the inline line
    // never drift apart on a color override.
    if matches!(status, NodeStatus::Starting) {
        return Style::default();
    }
    super::transcript::node_status_style(status, theme).1
}

#[cfg(test)]
mod tests {
    use conway::{AgentId, LogSeq, SubagentMode};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::tui::state::{AgentVisibility, TreeNode};

    // ---- T1 review finding 1: the agent panel's `Starting` marker stays
    // unstyled at the default theme (pre-T1 parity pin). Pre-T1 the panel
    // used `Style::default()` for `Starting` while the transcript's inline
    // `Entry::Agent` line used `Color::Gray`; the refactor must not unify
    // them into Gray. ----

    #[test]
    fn panel_starting_status_style_is_unstyled_at_default_theme() {
        let theme = Theme::default();
        assert_eq!(
            status_style(NodeStatus::Starting, &theme),
            Style::default(),
            "the agent panel's Starting marker must stay unstyled (pre-T1 parity), \
             not pick up theme.agent_starting's Gray"
        );
        // The other statuses delegate to the shared per-status mapping and
        // match their pre-T1 colors.
        assert_eq!(
            status_style(NodeStatus::Running, &theme),
            theme.agent_running
        );
        assert_eq!(
            status_style(NodeStatus::Cancelled, &theme),
            theme.agent_cancelled
        );
    }

    fn node(
        agent_id: AgentId,
        parent: Option<AgentId>,
        agent_def: Option<&str>,
        status: NodeStatus,
        kind: Option<SubagentMode>,
        inherited_upto: Option<LogSeq>,
        ephemeral: bool,
    ) -> TreeNode {
        TreeNode {
            agent_id,
            parent,
            agent_def: agent_def.map(str::to_string),
            status,
            kind,
            inherited_upto,
            ephemeral,
        }
    }

    fn rendered(state: &AppState, width: u16, height: u16) -> String {
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

    #[test]
    fn draw_renders_one_row_per_tree_node_without_panicking() {
        let root = AgentId::new();
        let state = AppState::new(root);

        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), &state, &Theme::default()))
            .expect("draw");

        let buffer = terminal.backend().buffer();
        assert!(buffer.content().iter().any(|cell| cell.symbol() != " "));
    }

    /// A2 review (minor): the ActiveOnly filter can hide EVERY row (e.g. an
    /// all-terminal tree). The draw must not panic and must render the
    /// header-only panel (no node labels, no ListState selection).
    #[test]
    fn draw_with_zero_visible_rows_renders_header_only_without_panicking() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        for n in &mut state.tree.nodes {
            n.status = NodeStatus::Finished;
        }
        state.tree.nodes.push(node(
            AgentId::new(),
            Some(root),
            Some("donechild"),
            NodeStatus::Finished,
            None,
            None,
            false,
        ));
        // V5: the default is All, which would show every row here -- force
        // ActiveOnly (every node in this tree is terminal) to exercise the
        // zero-visible-rows case.
        state.agent_visibility = AgentVisibility::ActiveOnly;

        let text = rendered(&state, 80, 10);

        assert!(
            !text.contains("donechild"),
            "a hidden terminal row must not render, got: {text:?}"
        );
        assert!(
            text.contains("agents"),
            "the header must still render on an empty panel, got: {text:?}"
        );
    }

    // an earlier item (finding M1): the arrow-selected agent row renders highlighted.
    #[test]
    fn draw_highlights_the_selected_agent_row() {
        use ratatui::style::Modifier;

        let root = AgentId::new();
        let mut state = AppState::new(root); // starts with the root node
        state.tree.nodes.push(node(
            AgentId::new(),
            Some(root),
            Some("child"),
            NodeStatus::Running,
            None,
            None,
            false,
        ));
        state.agent_selected = 1; // select the child row

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
        assert!(
            any_reversed,
            "the selected agent row must render highlighted (reversed)"
        );
    }

    // the focused agent (distinct from the browsing cursor above)
    // gets its own visible tag in the panel.
    #[test]
    fn draw_tags_the_focused_agent_row() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.tree.nodes.push(node(
            child,
            Some(root),
            Some("child"),
            NodeStatus::Running,
            None,
            None,
            false,
        ));
        state.focus_agent(child);

        let text = rendered(&state, 40, 10);
        assert!(
            text.contains("focused"),
            "expected the focused agent's row to be tagged, got: {text:?}"
        );
    }

    // ---- Item A2: recipe labels (pure `recipe_parts` formatting) ----

    #[test]
    fn recipe_label_fork_shows_the_fork_point_seq() {
        let n = node(
            AgentId::new(),
            None,
            None,
            NodeStatus::Running,
            Some(SubagentMode::Fork),
            Some(LogSeq(42)),
            false,
        );
        assert_eq!(recipe_parts(&n), vec!["fork @seq 42"]);
    }

    #[test]
    fn recipe_label_fork_without_a_seq_degrades_to_bare_fork() {
        let n = node(
            AgentId::new(),
            None,
            None,
            NodeStatus::Running,
            Some(SubagentMode::Fork),
            None,
            false,
        );
        assert_eq!(
            recipe_parts(&n),
            vec!["fork"],
            "a missing inherited_upto must degrade gracefully, not drop the label"
        );
    }

    #[test]
    fn recipe_label_spawn_with_an_agent_def_shows_at_def() {
        let n = node(
            AgentId::new(),
            None,
            Some("reviewer"),
            NodeStatus::Running,
            Some(SubagentMode::Spawn),
            None,
            false,
        );
        assert_eq!(recipe_parts(&n), vec!["@reviewer"]);
    }

    #[test]
    fn recipe_label_spawn_without_an_agent_def_shows_inherit() {
        let n = node(
            AgentId::new(),
            None,
            None,
            NodeStatus::Running,
            Some(SubagentMode::Spawn),
            None,
            false,
        );
        assert_eq!(recipe_parts(&n), vec!["(inherit)"]);
    }

    #[test]
    fn recipe_label_ephemeral_appends_the_marker() {
        let fork = node(
            AgentId::new(),
            None,
            None,
            NodeStatus::Running,
            Some(SubagentMode::Fork),
            Some(LogSeq(7)),
            true,
        );
        assert_eq!(recipe_parts(&fork), vec!["fork @seq 7", "(ephemeral)"]);

        // Even a node with no kind (should-not-happen for an ephemeral one)
        // still carries the marker.
        let kindless = node(
            AgentId::new(),
            None,
            None,
            NodeStatus::Running,
            None,
            None,
            true,
        );
        assert_eq!(recipe_parts(&kindless), vec!["(ephemeral)"]);
    }

    #[test]
    fn recipe_label_plain_root_has_no_recipe() {
        let n = node(
            AgentId::new(),
            None,
            None,
            NodeStatus::Running,
            None,
            None,
            false,
        );
        assert!(
            recipe_parts(&n).is_empty(),
            "a root/legacy node (kind: None) gets no recipe label"
        );
    }

    // ---- Item A2: draw-time visibility filtering + header mode label ----

    /// Root(Starting) + a Running child + a Finished child.
    fn three_node_state() -> (AppState, AgentId, AgentId) {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let live = AgentId::new();
        let done = AgentId::new();
        state.tree.nodes.push(node(
            live,
            Some(root),
            Some("livechild"),
            NodeStatus::Running,
            Some(SubagentMode::Spawn),
            None,
            false,
        ));
        state.tree.nodes.push(node(
            done,
            Some(root),
            Some("donechild"),
            NodeStatus::Finished,
            Some(SubagentMode::Fork),
            Some(LogSeq(3)),
            true,
        ));
        (state, live, done)
    }

    /// V5 acceptance: the panel's default listing is stable -- a Finished
    /// row stays represented under the default (`All`) filter.
    #[test]
    fn draw_under_the_default_filter_still_shows_a_finished_agent() {
        let (state, _live, _done) = three_node_state();
        assert_eq!(state.agent_visibility, AgentVisibility::All);

        let text = rendered(&state, 80, 10);

        assert!(text.contains("livechild"), "live rows must show: {text:?}");
        assert!(
            text.contains("donechild"),
            "a Finished row must still be represented under the default \
             (All) filter: {text:?}"
        );
        // The tree itself is untouched (draw-time filtering only).
        assert_eq!(state.tree.nodes.len(), 3);
    }

    #[test]
    fn draw_under_active_only_hides_terminal_rows() {
        let (mut state, _live, _done) = three_node_state();
        state.agent_visibility = AgentVisibility::ActiveOnly;

        let text = rendered(&state, 80, 10);

        assert!(text.contains("livechild"), "live rows must show: {text:?}");
        assert!(
            !text.contains("donechild"),
            "a Finished row must be hidden under ActiveOnly: {text:?}"
        );
        // The tree itself is untouched (draw-time filtering only).
        assert_eq!(state.tree.nodes.len(), 3);
    }

    #[test]
    fn draw_under_all_shows_terminal_rows_with_their_recipe_labels() {
        let (mut state, _live, _done) = three_node_state();
        state.agent_visibility = AgentVisibility::All;

        let text = rendered(&state, 80, 10);

        assert!(text.contains("livechild"));
        assert!(text.contains("donechild"));
        assert!(
            text.contains("fork @seq 3"),
            "the fork recipe label must render: {text:?}"
        );
        assert!(
            text.contains("(ephemeral)"),
            "the ephemeral marker must render: {text:?}"
        );
        assert!(
            text.contains("@livechild"),
            "the spawn @agent_def recipe must render: {text:?}"
        );
    }

    #[test]
    fn draw_under_finished_only_shows_only_terminal_rows() {
        let (mut state, _live, _done) = three_node_state();
        state.agent_visibility = AgentVisibility::FinishedOnly;

        let text = rendered(&state, 80, 10);

        assert!(text.contains("donechild"));
        assert!(
            !text.contains("livechild"),
            "a Running row must be hidden under FinishedOnly: {text:?}"
        );
    }

    #[test]
    fn draw_header_shows_the_current_filter_mode() {
        let (mut state, _live, _done) = three_node_state();
        for (mode, label) in [
            (AgentVisibility::ActiveOnly, "active"),
            (AgentVisibility::All, "all"),
            (AgentVisibility::FinishedOnly, "finished"),
        ] {
            state.agent_visibility = mode;
            let text = rendered(&state, 80, 10);
            assert!(
                text.contains(label),
                "the header must name the {mode:?} filter as {label:?}: {text:?}"
            );
        }
    }

    #[test]
    fn draw_clamps_the_selection_to_the_filtered_row_count() {
        use ratatui::style::Modifier;

        // Select the LAST raw tree index (the finished child), then hide it
        // under ActiveOnly: the draw must clamp to the filtered rows (root,
        // live) rather than selecting nothing / panicking.
        let (mut state, _live, _done) = three_node_state();
        state.agent_selected = 2;
        state.agent_visibility = AgentVisibility::ActiveOnly;

        let backend = TestBackend::new(80, 10);
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
        assert!(
            any_reversed,
            "the clamped selection (last visible row) must still render highlighted"
        );
    }

    // ---- V5: ancestor_chain (the shared, bounded ancestry walk) ----

    #[test]
    fn ancestor_chain_is_root_first_and_includes_the_queried_agent_last() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        let grandchild = AgentId::new();
        state.tree.nodes.push(node(
            child,
            Some(root),
            None,
            NodeStatus::Running,
            None,
            None,
            false,
        ));
        state.tree.nodes.push(node(
            grandchild,
            Some(child),
            None,
            NodeStatus::Running,
            None,
            None,
            false,
        ));

        assert_eq!(ancestor_chain(&state, root), vec![root]);
        assert_eq!(ancestor_chain(&state, child), vec![root, child]);
        assert_eq!(
            ancestor_chain(&state, grandchild),
            vec![root, child, grandchild]
        );
        assert_eq!(ancestor_depth(&state, grandchild), 2);
    }

    #[test]
    fn ancestor_chain_of_an_untracked_agent_is_just_itself() {
        let root = AgentId::new();
        let state = AppState::new(root);
        let stray = AgentId::new(); // never added to state.tree
        assert_eq!(ancestor_chain(&state, stray), vec![stray]);
    }

    /// A cycle in `parent` should be impossible, but the walk must
    /// never hang or overflow if one somehow existed. Two nodes pointing at
    /// each other as their own "parent".
    #[test]
    fn ancestor_chain_terminates_on_a_cycle_instead_of_hanging() {
        let a = AgentId::new();
        let b = AgentId::new();
        let mut state = AppState::new(a);
        // Overwrite the auto-created root node and add a second node so
        // `a.parent == Some(b)` and `b.parent == Some(a)`.
        state.tree.nodes.clear();
        state.tree.nodes.push(node(
            a,
            Some(b),
            None,
            NodeStatus::Running,
            None,
            None,
            false,
        ));
        state.tree.nodes.push(node(
            b,
            Some(a),
            None,
            NodeStatus::Running,
            None,
            None,
            false,
        ));

        let chain = ancestor_chain(&state, a);
        assert!(
            chain.len() <= MAX_ANCESTOR_CHAIN + 1,
            "a cyclic parent chain must be bounded, got {} entries",
            chain.len()
        );
        assert!(chain.contains(&a));
    }

    /// A deep, non-cyclic chain (below the bound) is walked in full --
    /// distinct from the cycle case above.
    #[test]
    fn ancestor_chain_walks_a_deep_non_cyclic_chain_in_full() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut cursor = root;
        let mut expected = vec![root];
        for _ in 0..10 {
            let next = AgentId::new();
            state.tree.nodes.push(node(
                next,
                Some(cursor),
                None,
                NodeStatus::Running,
                None,
                None,
                false,
            ));
            expected.push(next);
            cursor = next;
        }

        assert_eq!(ancestor_chain(&state, cursor), expected);
    }

    // ---- V5: hop_label (the breadcrumb's per-hop text, reusing recipe_parts) ----

    #[test]
    fn hop_label_uses_the_same_recipe_text_as_the_panel_row() {
        let n = node(
            AgentId::new(),
            None,
            None,
            NodeStatus::Running,
            Some(SubagentMode::Fork),
            Some(LogSeq(9)),
            false,
        );
        assert_eq!(hop_label(&n), "fork @seq 9");
    }

    /// V5 acceptance: a node with `kind: None` (the root, or one seeded
    /// out-of-band via `ensure_agent_tracked`) must render sensibly rather
    /// than being mislabeled as a fork or a spawn it never was.
    #[test]
    fn hop_label_of_a_kindless_node_falls_back_to_its_short_id_not_a_recipe_guess() {
        let id = AgentId::new();
        let n = node(id, None, None, NodeStatus::Running, None, None, false);
        assert_eq!(hop_label(&n), short_agent_id(id));
        assert!(!hop_label(&n).contains("fork"));
        assert!(!hop_label(&n).contains('@'));
    }
}
