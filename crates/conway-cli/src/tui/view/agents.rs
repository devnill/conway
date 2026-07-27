//! The below-chat agent-tree panel (WI-127 criterion 4): shown on demand
//! (toggled by `/agents`, handled in `app.rs` since `commands.rs` is out of
//! this item's file scope) rather than as an always-on side pane. Ordinary
//! subagent lifecycle is ALSO surfaced inline in the conversation stream
//! itself (`transcript.rs`'s `Entry::Agent` handling) -- this panel is for
//! browsing the whole tree at a glance, not the only place activity shows.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use conway::SubagentMode;

use crate::tui::state::{AppState, NodeStatus, TreeNode};

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState) {
    // Item A2: the visibility filter lives entirely HERE, at draw time --
    // `state.tree` itself is never filtered (P-2: finished agents are
    // hidden, not removed), so `visible` is the only place the
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
            // WI-140: the agent whose conversation the transcript pane
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
                Span::styled(marker, status_style(node.status)),
                Span::raw(" "),
                Span::raw(label),
            ];
            // Item A2: the recipe label (what context recipe this agent was
            // spawned with), dimmed so it reads as annotation next to the
            // row's own label.
            for part in recipe_parts(node) {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    part,
                    Style::default().add_modifier(Modifier::DIM),
                ));
            }
            spans.push(Span::styled(
                focus_tag,
                Style::default().add_modifier(Modifier::BOLD),
            ));
            ListItem::new(Line::from(spans))
        })
        .collect();
    let title = format!(
        "agents ({} · ↑/↓ scroll · v filter · esc)",
        state.agent_visibility.label()
    );
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        // The arrow-selected row (WI-130). Using a `ListState` (rather than
        // pre-styling one `ListItem`) lets ratatui scroll the selection into
        // view when the tree is taller than the panel.
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut list_state = ListState::default();
    if !visible.is_empty() {
        list_state.select(Some(state.agent_selected.min(visible.len() - 1)));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

/// Item A2: the recipe-label parts for a row -- what context recipe this
/// agent was spawned with, composed from the A1 `TreeNode` fields
/// (`kind`/`inherited_upto`/`ephemeral`). A pure function (GP-04) so the
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

/// `pub(crate)` so item A3's `/tree` snapshot renderer (`tui::commands`)
/// indents by the same ancestor-depth rule the panel rows use.
pub(crate) fn ancestor_depth(state: &AppState, agent: conway::AgentId) -> usize {
    let mut depth = 0;
    let mut cursor = agent;
    loop {
        let Some(node) = state.tree.nodes.iter().find(|n| n.agent_id == cursor) else {
            break;
        };
        match node.parent {
            Some(p) => {
                depth += 1;
                cursor = p;
            }
            None => break,
        }
    }
    depth
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

fn status_style(status: NodeStatus) -> Style {
    match status {
        NodeStatus::Running => Style::default().fg(Color::Yellow),
        NodeStatus::AwaitingPermission => Style::default().fg(Color::Magenta),
        NodeStatus::Finished => Style::default().fg(Color::Green),
        NodeStatus::Failed => Style::default().fg(Color::Red),
        NodeStatus::Cancelled => Style::default().fg(Color::DarkGray),
        NodeStatus::Starting => Style::default(),
    }
}

#[cfg(test)]
mod tests {
    use conway::{AgentId, LogSeq, SubagentMode};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::tui::state::{AgentVisibility, TreeNode};

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
        terminal.draw(|f| draw(f, f.area(), state)).expect("draw");
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
        terminal.draw(|f| draw(f, f.area(), &state)).expect("draw");

        let buffer = terminal.backend().buffer();
        assert!(buffer.content().iter().any(|cell| cell.symbol() != " "));
    }

    /// A2 review (minor): the default ActiveOnly filter can hide EVERY row
    /// (e.g. an all-terminal tree). The draw must not panic and must render
    /// the header-only panel (no node labels, no ListState selection).
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
        // Default filter is ActiveOnly: every node here is terminal.

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

    // WI-134 (finding M1): the arrow-selected agent row renders highlighted.
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
        terminal.draw(|f| draw(f, f.area(), &state)).expect("draw");

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

    // WI-140: the focused agent (distinct from the browsing cursor above)
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

    /// root(Starting) + a Running child + a Finished child.
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

    #[test]
    fn draw_under_the_default_active_filter_hides_terminal_rows() {
        let (state, _live, _done) = three_node_state();
        assert_eq!(state.agent_visibility, AgentVisibility::ActiveOnly);

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
        assert_eq!(state.agent_visibility, AgentVisibility::ActiveOnly);

        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, f.area(), &state)).expect("draw");

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
}
