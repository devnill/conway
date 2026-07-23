//! The TUI's render pass (WI-114; redesigned single-column layout WI-127):
//! a pure function from `&AppState` to a `ratatui::Frame` -- no `AppState`
//! mutation, no I/O, so it can run under a `ratatui::backend::TestBackend`
//! with no real terminal.
//!
//! WI-127 replaced the old always-on two-pane layout (a left agent-tree
//! pane alongside right transcript/input columns) with a single column:
//! conversation stream on top, an optional on-demand agent panel, an input
//! box, and a bottom status line (criterion 1). See `transcript.rs`'s doc
//! for the clean-copy guarantee (criterion 2), `palette.rs` for the
//! live-filtering command palette (criterion 3), and
//! `agents.rs`/`transcript.rs`'s `Entry::Agent` handling for the
//! agent-tree/subagent-activity criterion (criterion 4).
//!
//! Submodules are a directory (not one flat file) so each concern --
//! transcript, input box, status line, palette, agent panel -- stays a
//! small, independently testable pure-rendering unit (module notes' own
//! ask: "keep rendering functions small and testable").

mod agents;
mod input_box;
mod palette;
mod status;
mod transcript;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::state::{AppState, Mode};

const INPUT_HEIGHT: u16 = 3;
const STATUS_HEIGHT: u16 = 1;
const AGENT_PANEL_HEIGHT: u16 = 8;

pub fn draw(state: &AppState, frame: &mut Frame) {
    let area = frame.area();
    let show_agents = state.agent_view_open && area.height > INPUT_HEIGHT + STATUS_HEIGHT + 3;

    let mut constraints = vec![Constraint::Min(0)];
    if show_agents {
        constraints.push(Constraint::Length(AGENT_PANEL_HEIGHT.min(area.height / 3)));
    }
    constraints.push(Constraint::Length(INPUT_HEIGHT));
    constraints.push(Constraint::Length(STATUS_HEIGHT));

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let mut next = 0;
    let transcript_area = rows[next];
    next += 1;
    transcript::draw(frame, transcript_area, state);

    if show_agents {
        agents::draw(frame, rows[next], state);
        next += 1;
    }

    let input_area = rows[next];
    next += 1;
    input_box::draw(frame, input_area, state);

    status::draw(frame, rows[next], state);

    if state.input.starts_with('/') {
        palette::draw_overlay(frame, input_area, &state.input);
    }

    if let Mode::AwaitingPermission(pending) = &state.mode {
        draw_permission_overlay(frame, transcript_area, &pending.request);
    }
}

/// The permission prompt: a bordered block over the bottom of the
/// transcript area, unmistakably distinct from ordinary transcript output
/// (module notes; also this item's human criterion). A `Block`/border here
/// is fine -- it is a modal overlay, never part of the copyable
/// conversation (it replaces transcript content on screen only while a
/// decision is pending, via `Clear`).
fn draw_permission_overlay(
    frame: &mut Frame,
    transcript_area: Rect,
    req: &conway::PermissionRequest,
) {
    let height = 6u16.min(transcript_area.height);
    let area = Rect {
        x: transcript_area.x,
        y: transcript_area.y + transcript_area.height.saturating_sub(height),
        width: transcript_area.width,
        height,
    };

    let agent_path = if req.agent_path.is_empty() {
        "root".to_string()
    } else {
        req.agent_path
            .iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(" -> ")
    };

    let lines = vec![
        Line::from(Span::styled(
            req.rendered.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(format!("tool: {}  category: {:?}", req.tool, req.category)),
        Line::from(format!("agent path: {agent_path}")),
        Line::from("[y] allow once  [a] allow always  [n] deny  [Esc] deny with feedback"),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" PERMISSION REQUIRED ")
        .border_style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD));
    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });
    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use conway::AgentId;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::tui::state::Entry;

    #[test]
    fn draw_produces_a_non_empty_buffer_and_does_not_mutate_state() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.transcript.push(Entry::Assistant {
            text: "hello".to_string(),
        });
        let before = format!("{:?}", state.transcript);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(&state, f)).expect("draw");

        let buffer = terminal.backend().buffer();
        let non_blank = buffer.content().iter().any(|cell| cell.symbol() != " ");
        assert!(non_blank, "expected the frame to render something");

        let after = format!("{:?}", state.transcript);
        assert_eq!(before, after, "draw must not mutate AppState");
    }

    #[test]
    fn small_terminal_does_not_panic() {
        let root = AgentId::new();
        let state = AppState::new(root);
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(&state, f)).expect("draw");
        let buffer = terminal.backend().buffer();
        assert!(buffer.content().iter().any(|cell| cell.symbol() != " "));
    }

    #[test]
    fn agent_panel_hidden_by_default() {
        let root = AgentId::new();
        let state = AppState::new(root);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(&state, f)).expect("draw");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(!text.contains("agents ("));
    }

    #[test]
    fn agent_panel_shown_once_toggled_on() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.toggle_agent_view();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(&state, f)).expect("draw");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("agents ("));
    }

    #[test]
    fn slash_input_shows_the_command_palette() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.input = "/as".to_string();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(&state, f)).expect("draw");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("/ask"));
    }

    #[test]
    fn non_slash_input_hides_the_command_palette() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.input = "hello".to_string();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(&state, f)).expect("draw");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        // The status line's own "/ for commands" hint always contains the
        // word "commands" -- assert against a palette-only string (a
        // command's usage form) instead of that word.
        assert!(!text.contains("/ask <text>"));
    }
}
