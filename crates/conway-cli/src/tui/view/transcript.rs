//! The conversation stream (WI-127 criteria 1 & 2): a plain, borderless
//! `Paragraph` of [`entry_lines`] output.
//!
//! **Clean-copy guarantee (criterion 2):** [`draw`] renders `Paragraph::new`
//! with no `.block(..)` at all -- no `Borders`, no title, nothing that puts
//! a box-drawing glyph anywhere in this area. Every cell the terminal ever
//! paints here comes straight from an [`entry_lines`] `Span`'s text content,
//! so selecting/copying this region copies exactly that plain text (styling
//! such as color/bold/dim is not part of a terminal's copied text at all).
//! [`entry_lines`] itself never emits a box-drawing character (see this
//! module's own `entry_lines_never_contain_box_drawing_glyphs` test) --
//! together these two facts are the whole guarantee. Do not wrap this
//! area's `Paragraph` in a `Block`/`Borders` in any future change; if a
//! visual divider is ever needed, use blank-line spacing or styling, never
//! a border/rule glyph.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::tui::state::{AppState, Entry, NodeStatus, ToolStatus};

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState) {
    let lines: Vec<Line> = state.transcript.iter().flat_map(entry_lines).collect();
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((state.scroll, 0));
    frame.render_widget(paragraph, area);
}

/// Renders one transcript [`Entry`] into its plain-text line(s) -- most
/// entries are exactly one line; [`Entry::EphemeralAsk`] is two (question,
/// reply). A free function (not inlined into `draw`) so it is directly
/// unit-testable against a `TestBackend`-free `Line`/`Span`.
pub fn entry_lines(entry: &Entry) -> Vec<Line<'static>> {
    match entry {
        Entry::User(text) => vec![Line::from(vec![
            Span::styled("you> ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(text.clone()),
        ])],
        Entry::Assistant { text } => vec![Line::from(text.clone())],
        Entry::Tool {
            name,
            status,
            preview,
            ..
        } => vec![tool_line(name, *status, preview)],
        Entry::Agent { label, status, .. } => vec![agent_line(label, *status)],
        Entry::EphemeralAsk {
            question, reply, ..
        } => ephemeral_ask_lines(question, reply.as_deref()),
        Entry::Notice { text } => {
            vec![Line::from(Span::styled(
                text.clone(),
                Style::default().fg(Color::Cyan),
            ))]
        }
    }
}

fn tool_line(name: &str, status: ToolStatus, preview: &str) -> Line<'static> {
    let (tag, style) = match status {
        ToolStatus::Proposed => ("proposed", Style::default().fg(Color::Gray)),
        ToolStatus::AwaitingPermission => {
            ("awaiting permission", Style::default().fg(Color::Magenta))
        }
        ToolStatus::Running => ("running", Style::default().fg(Color::Yellow)),
        ToolStatus::Finished { is_error: false } => ("done", Style::default().fg(Color::Green)),
        ToolStatus::Finished { is_error: true } => ("failed", Style::default().fg(Color::Red)),
    };
    Line::from(vec![
        Span::styled(format!("[{tag}] "), style),
        Span::raw(name.to_string()),
        Span::raw(if preview.is_empty() {
            String::new()
        } else {
            format!(" -- {preview}")
        }),
    ])
}

/// Subagent lifecycle, inline in the stream (WI-127 criterion 4).
fn agent_line(label: &str, status: NodeStatus) -> Line<'static> {
    let (tag, style) = match status {
        NodeStatus::Starting => ("starting", Style::default().fg(Color::Gray)),
        NodeStatus::Running => ("running", Style::default().fg(Color::Yellow)),
        NodeStatus::AwaitingPermission => {
            ("awaiting permission", Style::default().fg(Color::Magenta))
        }
        NodeStatus::Finished => ("done", Style::default().fg(Color::Green)),
        NodeStatus::Failed => ("failed", Style::default().fg(Color::Red)),
        NodeStatus::Cancelled => ("cancelled", Style::default().fg(Color::DarkGray)),
    };
    Line::from(vec![
        Span::styled(format!("[agent {tag}] "), style),
        Span::raw(label.to_string()),
    ])
}

/// `/ask` renders as a dimmed, clearly-ephemeral aside (WI-127 criterion 5):
/// a `[ephemeral ...]` text tag on every line (not just a dim style, which
/// some terminals render subtly enough to miss) plus `Modifier::DIM`.
fn ephemeral_ask_lines(question: &str, reply: Option<&str>) -> Vec<Line<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let reply_text = reply
        .map(str::to_string)
        .unwrap_or_else(|| "...".to_string());
    vec![
        Line::from(Span::styled(format!("[ephemeral ask] {question}"), dim)),
        Line::from(Span::styled(format!("[ephemeral reply] {reply_text}"), dim)),
    ]
}

#[cfg(test)]
mod tests {
    use conway::AgentId;

    use super::*;

    const BOX_DRAWING_CHARS: &[char] = &[
        '│', '─', '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼', '║', '═', '╔', '╗', '╚', '╝', '╠',
        '╣', '╦', '╩', '╬',
    ];

    fn plain_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The machine-checkable half of criterion 2: no `Entry` variant's
    /// rendered text ever contains a box-drawing glyph. Combined with
    /// `draw`'s own lack of a `Block`/`Borders` (this module's doc comment),
    /// this is the whole clean-copy guarantee.
    #[test]
    fn entry_lines_never_contain_box_drawing_glyphs() {
        let entries = vec![
            Entry::User("hi there".to_string()),
            Entry::Assistant {
                text: "hello back".to_string(),
            },
            Entry::Tool {
                call_id: "c1".to_string(),
                name: "bash".to_string(),
                status: ToolStatus::Finished { is_error: false },
                preview: "ok".to_string(),
            },
            Entry::Agent {
                agent_id: AgentId::new(),
                label: "reviewer".to_string(),
                status: NodeStatus::Running,
            },
            Entry::EphemeralAsk {
                id: 0,
                question: "what now?".to_string(),
                reply: Some("do this".to_string()),
            },
            Entry::EphemeralAsk {
                id: 1,
                question: "pending?".to_string(),
                reply: None,
            },
            Entry::Notice {
                text: "a notice".to_string(),
            },
        ];

        for entry in &entries {
            for line in entry_lines(entry) {
                let text = plain_text(&line);
                assert!(
                    !text.chars().any(|c| BOX_DRAWING_CHARS.contains(&c)),
                    "box-drawing chrome leaked into clean-copy text: {text:?}"
                );
            }
        }
    }

    #[test]
    fn ephemeral_ask_renders_two_lines_tagged_ephemeral() {
        let lines = entry_lines(&Entry::EphemeralAsk {
            id: 0,
            question: "q".to_string(),
            reply: Some("r".to_string()),
        });
        assert_eq!(lines.len(), 2);
        assert!(plain_text(&lines[0]).contains("[ephemeral ask] q"));
        assert!(plain_text(&lines[1]).contains("[ephemeral reply] r"));
    }

    #[test]
    fn ephemeral_ask_pending_shows_a_placeholder_not_a_panic() {
        let lines = entry_lines(&Entry::EphemeralAsk {
            id: 0,
            question: "q".to_string(),
            reply: None,
        });
        assert!(plain_text(&lines[1]).contains("[ephemeral reply]"));
    }

    #[test]
    fn user_entry_keeps_the_you_prefix() {
        let lines = entry_lines(&Entry::User("hello".to_string()));
        assert_eq!(lines.len(), 1);
        assert!(plain_text(&lines[0]).starts_with("you> hello"));
    }

    #[test]
    fn draw_does_not_mutate_state() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.transcript.push(Entry::Assistant {
            text: "hello".to_string(),
        });
        let before = format!("{:?}", state.transcript);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, f.area(), &state)).expect("draw");

        assert_eq!(format!("{:?}", state.transcript), before);
    }

    /// End-to-end companion to `entry_lines_never_contain_box_drawing_glyphs`:
    /// even if a future change wrapped this area's `Paragraph` in a `Block`,
    /// this test would catch the resulting border glyphs in the rendered
    /// buffer itself, not just in the pure `entry_lines` output.
    #[test]
    fn rendered_buffer_contains_no_box_drawing_glyphs() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.transcript.push(Entry::User("hi".to_string()));
        state.transcript.push(Entry::Assistant {
            text: "hello".to_string(),
        });

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, f.area(), &state)).expect("draw");

        let buffer = terminal.backend().buffer();
        for cell in buffer.content() {
            let symbol = cell.symbol();
            assert!(
                !BOX_DRAWING_CHARS.contains(&symbol.chars().next().unwrap_or(' ')),
                "box-drawing glyph found in transcript buffer: {symbol:?}"
            );
        }
    }
}
