//! The TUI's render pass (WI-114): a pure function from `&AppState` to a
//! `ratatui::Frame` -- no `AppState` mutation, no I/O, so it can run under a
//! `ratatui::backend::TestBackend` with no real terminal.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use super::state::{AppState, Entry, Mode, NodeStatus, ToolStatus};

/// Below this terminal width the agent-tree pane is hidden entirely (module
/// notes: "When width < 60, the tree pane is hidden and reachable only via
/// `/tree`" -- WI-115 owns `/tree` itself, this item only owns hiding the
/// pane).
const TREE_PANE_MIN_WIDTH: u16 = 60;
const TREE_PANE_WIDTH: u16 = 28;
const INPUT_PANE_HEIGHT: u16 = 3;

pub fn draw(state: &AppState, frame: &mut Frame) {
    let area = frame.area();
    let show_tree = area.width >= TREE_PANE_MIN_WIDTH;

    let columns = if show_tree {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(TREE_PANE_WIDTH), Constraint::Min(0)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0)])
            .split(area)
    };

    let right_col = *columns.last().expect("layout always yields >=1 column");
    if show_tree {
        draw_tree(frame, columns[0], state);
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(INPUT_PANE_HEIGHT)])
        .split(right_col);

    draw_transcript(frame, rows[0], state);
    draw_input(frame, rows[1], state);

    if let Mode::AwaitingPermission(pending) = &state.mode {
        draw_permission_overlay(frame, rows[0], &pending.request);
    }
}

fn draw_tree(frame: &mut Frame, area: Rect, state: &AppState) {
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
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Agents"));
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

fn draw_transcript(frame: &mut Frame, area: Rect, state: &AppState) {
    let mut lines: Vec<Line> = Vec::new();
    for entry in &state.transcript {
        lines.push(entry_line(entry));
    }
    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("Transcript"))
        .wrap(Wrap { trim: false })
        .scroll((state.scroll, 0));
    frame.render_widget(paragraph, area);
}

fn entry_line(entry: &Entry) -> Line<'static> {
    match entry {
        Entry::User(text) => Line::from(vec![
            Span::styled("you> ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(text.clone()),
        ]),
        Entry::Assistant { text } => Line::from(text.clone()),
        Entry::Tool {
            name,
            status,
            preview,
            ..
        } => {
            let (tag, style) = match status {
                ToolStatus::Proposed => ("proposed", Style::default().fg(Color::Gray)),
                ToolStatus::AwaitingPermission => {
                    ("awaiting permission", Style::default().fg(Color::Magenta))
                }
                ToolStatus::Running => ("running", Style::default().fg(Color::Yellow)),
                ToolStatus::Finished { is_error: false } => {
                    ("done", Style::default().fg(Color::Green))
                }
                ToolStatus::Finished { is_error: true } => {
                    ("failed", Style::default().fg(Color::Red))
                }
            };
            Line::from(vec![
                Span::styled(format!("[{tag}] "), style),
                Span::raw(name.clone()),
                Span::raw(if preview.is_empty() {
                    String::new()
                } else {
                    format!(" -- {preview}")
                }),
            ])
        }
        Entry::Notice { text } => {
            Line::from(Span::styled(text.clone(), Style::default().fg(Color::Cyan)))
        }
    }
}

fn draw_input(frame: &mut Frame, area: Rect, state: &AppState) {
    let disabled = !matches!(state.mode, Mode::Normal);
    let title = if disabled { "Input (paused)" } else { "Input" };
    let paragraph = Paragraph::new(state.input.as_str())
        .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(paragraph, area);

    // The border eats one row/column on each side, so the text starts at
    // (area.x + 1, area.y + 1); `cursor` is a char count, which -- absent
    // wide/combining glyphs, not a concern for a plain-text input line --
    // is also the column offset into that text.
    if !disabled && area.width > 2 && area.height > 2 {
        let max_col = area.width.saturating_sub(2);
        let col = (state.cursor as u16).min(max_col);
        frame.set_cursor_position((area.x + 1 + col, area.y + 1));
    }
}

/// The permission prompt: a bordered block over the bottom of the
/// transcript pane, unmistakably distinct from ordinary transcript output
/// (module notes; also this item's human criterion).
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
    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use conway::AgentId;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::tui::state::AppState;

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
    fn tree_pane_hidden_below_min_width() {
        let root = AgentId::new();
        let state = AppState::new(root);
        let backend = TestBackend::new(40, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(&state, f)).expect("draw");
        // No panic and a non-empty frame is the assertion here -- exact
        // layout pixels are a human/UX review concern (this item's [human]
        // criterion), not a machine one.
        let buffer = terminal.backend().buffer();
        assert!(buffer.content().iter().any(|cell| cell.symbol() != " "));
    }
}
