//! The input box: the one bordered element besides the on-demand agent
//! panel/command palette. A border here is fine -- an earlier item criterion 2's
//! clean-copy guarantee is specifically about the conversation stream
//! (`transcript.rs`), which this is not.
//!
//! T8: the box can hold a multi-line draft (`state.input` gains embedded
//! `\n` characters from Alt/Shift-Enter -- see `input.rs`), and its own
//! `Rect` grows with that content (`view/mod.rs::input_height`, capped at
//! `area.height / 3`). Two scroll axes follow from that:
//!
//! - **Vertical** -- once the draft has more lines than the box has rows
//!   (only possible once content exceeds the height cap), the visible
//!   window scrolls so the cursor's own line always stays on screen.
//! - **Horizontal** -- a single long line (no `\n` at all, or the cursor's
//!   own line within a multi-line draft) can still be wider than the box.
//!   Previously this CLAMPED the rendered cursor column to `area.width - 2`
//!   with no corresponding change to what was drawn -- the cursor visually
//!   froze at the right edge while the text kept extending off-screen
//!   invisibly. Now the cursor's own row scrolls horizontally so the
//!   character at the cursor is always the one rendered at (or the one
//!   pushing against) the right edge; every OTHER row renders from its own
//!   column 0 and is simply clipped if it overflows the width -- only the
//!   row you are actively editing needs to track the cursor.

use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::theme::Theme;
use crate::tui::state::{AppState, Mode};

const PLACEHOLDER: &str = "Type a message, or / for commands";

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let disabled = !matches!(state.mode, Mode::Normal);
    let title = if disabled { "input (paused)" } else { "input" };

    if state.input.is_empty() && !disabled {
        let paragraph = Paragraph::new(PLACEHOLDER).style(theme.dim).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(theme.border_normal),
        );
        frame.render_widget(paragraph, area);
        return;
    }

    let rows = area.height.saturating_sub(2) as usize;
    let cols = area.width.saturating_sub(2) as usize;
    let (lines, cursor) = visible_window(state, rows, cols);

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(theme.border_normal),
    );
    frame.render_widget(paragraph, area);

    // The border eats one row/column on each side, so the text's own
    // (0, 0) lands at (area.x + 1, area.y + 1).
    if !disabled && area.width > 2 && area.height > 2 {
        if let Some((col, row)) = cursor {
            frame.set_cursor_position((area.x + 1 + col, area.y + 1 + row));
        }
    }
}

/// Builds the box's rendered lines plus the cursor's on-screen `(col, row)`
/// position relative to the text area's own top-left, from `state.input`/
/// `state.cursor` and the interior `rows`x`cols` the box has to show it in.
///
/// Vertical scroll: `row_offset` is chosen so the cursor's own line is
/// always the LAST visible row once the draft has more lines than `rows`
/// (never scrolls further than needed -- a draft that still fits needs no
/// offset at all). Horizontal scroll: only the cursor's own line gets a
/// `col_offset` (chosen the same way, against `cols`); every other line
/// renders from its own start and is naturally clipped by `Line`'s width if
/// it overflows -- no OTHER line can contain the cursor, so no other line
/// needs to track it.
fn visible_window(
    state: &AppState,
    rows: usize,
    cols: usize,
) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    let input_lines: Vec<&str> = state.input.split('\n').collect();
    let (cursor_line, cursor_col) = state.cursor_line_col();

    let row_offset = if rows == 0 {
        0
    } else {
        cursor_line.saturating_sub(rows - 1)
    };

    let mut out = Vec::new();
    let mut cursor_pos = None;

    for (i, line) in input_lines
        .iter()
        .enumerate()
        .skip(row_offset)
        .take(rows.max(1))
    {
        let is_cursor_line = i == cursor_line;
        let col_offset = if is_cursor_line && cols > 0 {
            cursor_col.saturating_sub(cols - 1)
        } else {
            0
        };
        let visible: String = line.chars().skip(col_offset).take(cols.max(1)).collect();
        if is_cursor_line {
            let screen_row = (i - row_offset) as u16;
            let screen_col = (cursor_col - col_offset) as u16;
            cursor_pos = Some((screen_col, screen_row));
        }
        out.push(Line::from(visible));
    }

    (out, cursor_pos)
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
        terminal
            .draw(|f| draw(f, f.area(), &state, &Theme::default()))
            .expect("draw");

        let buffer = terminal.backend().buffer();
        assert!(buffer.content().iter().any(|cell| cell.symbol() != " "));
    }

    #[test]
    fn empty_input_shows_the_placeholder() {
        let state = AppState::new(AgentId::new());

        let backend = TestBackend::new(60, 5);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), &state, &Theme::default()))
            .expect("draw");

        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Type a message"));
    }

    fn row_text(terminal: &Terminal<TestBackend>, y: u16, width: u16) -> String {
        let buffer = terminal.backend().buffer();
        (0..width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    }

    /// The bug this item fixes: a long single line used to CLAMP the
    /// rendered cursor column to `area.width - 2` with the text itself
    /// never scrolling -- the row always showed the HEAD of the string
    /// (columns 0.. from the left), with the cursor frozen at the right
    /// edge regardless of where it truly was. Now the row scrolls so its
    /// TAIL (the text right up to the true cursor position) is what's on
    /// screen.
    #[test]
    fn long_single_line_input_scrolls_horizontally_to_keep_the_cursor_visible() {
        let mut state = AppState::new(AgentId::new());
        // A distinguishable head marker and tail marker either side of 190
        // filler characters (200 chars total) -- proves the rendered row is
        // a SCROLLED WINDOW ending at the true cursor position, not the old
        // bug's unscrolled head-of-string with a merely mis-clamped cursor
        // column (visually indistinguishable from correct scrolling if the
        // content were uniform).
        let input = format!("HEADMARKER{}TAILMARKEND", "x".repeat(179));
        assert_eq!(input.chars().count(), 200);
        state.input = input.clone();
        state.cursor = input.chars().count();

        let width = 40;
        let height = 3;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), &state, &Theme::default()))
            .expect("draw");

        let row = row_text(&terminal, 1, width);
        assert!(
            row.contains("TAILMARKEND"),
            "the visible row must show the TAIL of the input: {row:?}"
        );
        assert!(
            !row.contains("HEADMARKER"),
            "the head of the 200-char input must have scrolled off-screen: {row:?}"
        );

        let pos = terminal
            .get_cursor_position()
            .expect("the cursor must be shown, not left unset");
        assert_eq!(pos.y, 1);
        assert_eq!(
            pos.x,
            width - 2,
            "the cursor must land on the last interior column, not clamped \
             mid-string with the text left unscrolled"
        );
    }

    #[test]
    fn multi_line_input_renders_each_line_and_places_the_cursor_on_the_right_row() {
        let mut state = AppState::new(AgentId::new());
        state.input = "first\nsecond\nthird".to_string();
        state.cursor = state.input.chars().count(); // end of "third"

        let width = 40;
        let height = 5; // 3 interior rows -- fits all 3 lines
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), &state, &Theme::default()))
            .expect("draw");

        assert!(row_text(&terminal, 1, width).contains("first"));
        assert!(row_text(&terminal, 2, width).contains("second"));
        assert!(row_text(&terminal, 3, width).contains("third"));

        let pos = terminal.get_cursor_position().expect("cursor must be set");
        assert_eq!(pos.y, 3, "cursor row must be the third interior row");
        assert_eq!(pos.x, 1 + 5, "cursor col must sit right after 'third'");
    }

    #[test]
    fn multi_line_input_scrolls_vertically_to_keep_the_cursor_line_visible() {
        let mut state = AppState::new(AgentId::new());
        state.input = "one\ntwo\nthree\nfour\nfive".to_string();
        state.cursor = state.input.chars().count(); // end of "five"

        let width = 40;
        let height = 4; // 2 interior rows -- fewer than the 5 content lines
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), &state, &Theme::default()))
            .expect("draw");

        let text = row_text(&terminal, 1, width) + &row_text(&terminal, 2, width);
        assert!(
            text.contains("five"),
            "the cursor's own line must always be visible: {text:?}"
        );
        assert!(
            !text.contains("one"),
            "the scroll must have moved past the earliest lines: {text:?}"
        );
    }
}
