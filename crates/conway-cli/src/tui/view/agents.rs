//! The below-chat agent-tree panel (WI-127 criterion 4): shown on demand
//! (toggled by `/agents`, handled in `app.rs` since `commands.rs` is out of
//! this item's file scope) rather than as an always-on side pane. Ordinary
//! subagent lifecycle is ALSO surfaced inline in the conversation stream
//! itself (`transcript.rs`'s `Entry::Agent` handling) -- this panel is for
//! browsing the whole tree at a glance, not the only place activity shows.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};
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
            ListItem::new(Line::from(vec![
                Span::raw(indent),
                Span::styled(marker, status_style(node.status)),
                Span::raw(" "),
                Span::raw(label),
            ]))
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("agents (/agents to hide)"),
    );
    frame.render_widget(list, area);
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
}
