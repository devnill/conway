//! The input box: the one bordered element besides the on-demand agent
//! panel/command palette. A border here is fine -- WI-127 criterion 2's
//! clean-copy guarantee is specifically about the conversation stream
//! (`transcript.rs`), which this is not.

use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::theme::Theme;
use crate::tui::state::{AppState, Mode};

const PLACEHOLDER: &str = "Type a message, or / for commands";

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let disabled = !matches!(state.mode, Mode::Normal);
    let title = if disabled { "input (paused)" } else { "input" };

    let paragraph = if state.input.is_empty() && !disabled {
        Paragraph::new(PLACEHOLDER)
            .style(theme.dim)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(theme.border_normal),
            )
    } else {
        Paragraph::new(state.input.as_str()).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(theme.border_normal),
        )
    };
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

#[cfg(test)]
mod tests {
    use conway::AgentId;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;

    #[test]
    fn draw_does_not_panic_and_paints_something() {
        let mut state = AppState::new(AgentId::new());
        state.input = "hello".to_string();
        state.cursor = 5;

        let backend = TestBackend::new(40, 5);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, f.area(), &state, &Theme::default())).expect("draw");

        let buffer = terminal.backend().buffer();
        assert!(buffer.content().iter().any(|cell| cell.symbol() != " "));
    }

    #[test]
    fn empty_input_shows_the_placeholder() {
        let state = AppState::new(AgentId::new());

        let backend = TestBackend::new(60, 5);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, f.area(), &state, &Theme::default())).expect("draw");

        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Type a message"));
    }
}
