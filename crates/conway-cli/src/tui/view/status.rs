//! The bottom status line (WI-127 criterion 1): a single, always-visible
//! plain line -- no border -- summarizing mode, agent count, and the two
//! on-demand affordances (`/` for the command palette, `/agents` for the
//! agent-tree panel).

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::tui::state::{AppState, Mode};

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState) {
    let paragraph = Paragraph::new(Line::from(status_line(state)))
        .style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_widget(paragraph, area);
}

/// Pure formatting, split out from [`draw`] so it is testable with no
/// `Frame`/terminal at all.
pub fn status_line(state: &AppState) -> String {
    let mode = match state.mode {
        Mode::Normal => "ready",
        Mode::AwaitingPermission(_) => "awaiting permission",
    };
    let count = state.tree.nodes.len();
    let noun = if count == 1 { "agent" } else { "agents" };
    let agents_hint = if state.agent_view_open {
        "/agents to hide"
    } else {
        "/agents to view"
    };
    format!(" {mode} | {count} {noun} | / for commands | {agents_hint} ")
}

#[cfg(test)]
mod tests {
    use conway::AgentId;

    use super::*;

    #[test]
    fn status_line_reports_ready_and_one_agent_by_default() {
        let state = AppState::new(AgentId::new());
        let line = status_line(&state);
        assert!(line.contains("ready"));
        assert!(line.contains("1 agent"));
        assert!(!line.contains("1 agents"));
    }

    #[test]
    fn status_line_reflects_agent_view_toggle() {
        let mut state = AppState::new(AgentId::new());
        assert!(status_line(&state).contains("/agents to view"));
        state.toggle_agent_view();
        assert!(status_line(&state).contains("/agents to hide"));
    }
}
