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

use crate::tui::state::{AppState, NodeStatus};

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState) {
    let items: Vec<ListItem> = state
        .tree
        .nodes
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
            ListItem::new(Line::from(vec![
                Span::raw(indent),
                Span::styled(marker, status_style(node.status)),
                Span::raw(" "),
                Span::raw(label),
                Span::styled(focus_tag, Style::default().add_modifier(Modifier::BOLD)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("agents (↑/↓ scroll · esc to close)"),
        )
        // The arrow-selected row (WI-130). Using a `ListState` (rather than
        // pre-styling one `ListItem`) lets ratatui scroll the selection into
        // view when the tree is taller than the panel.
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut list_state = ListState::default();
    if !state.tree.nodes.is_empty() {
        list_state.select(Some(state.agent_selected.min(state.tree.nodes.len() - 1)));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn ancestor_depth(state: &AppState, agent: conway::AgentId) -> usize {
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
    use conway::AgentId;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;

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

    // WI-134 (finding M1): the arrow-selected agent row renders highlighted.
    #[test]
    fn draw_highlights_the_selected_agent_row() {
        use crate::tui::state::TreeNode;
        use ratatui::style::Modifier;

        let root = AgentId::new();
        let mut state = AppState::new(root); // starts with the root node
        state.tree.nodes.push(TreeNode {
            agent_id: AgentId::new(),
            parent: Some(root),
            agent_def: Some("child".to_string()),
            status: NodeStatus::Running,
        });
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
        use crate::tui::state::TreeNode;

        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.tree.nodes.push(TreeNode {
            agent_id: child,
            parent: Some(root),
            agent_def: Some("child".to_string()),
            status: NodeStatus::Running,
        });
        state.focus_agent(child);

        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, f.area(), &state)).expect("draw");

        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(
            text.contains("focused"),
            "expected the focused agent's row to be tagged, got: {text:?}"
        );
    }
}
