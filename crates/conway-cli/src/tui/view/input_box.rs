//! The input box: the one bordered element besides the on-demand agent
//! panel/command palette. A border here is fine -- earlier work criterion 2's
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
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::theme::Theme;
use crate::tui::state::{AppState, Mode};

const PLACEHOLDER: &str = "Type a message, or / for commands";

/// Board item `01M1YVHKTQVXJRDSRYT3TCRXFX`: the queued-strip's own first-
/// line budget inside the box's border title -- long enough to be useful,
/// short enough that it cannot crowd out the `input`/`shell`/`shell →
/// model` chrome word it is appended to. Mirrors `view/transcript.rs`'s own
/// small, per-module truncation helper rather than sharing one (that
/// module's own doc: "a handful of near-identical small per-module
/// time-formatting helpers rather than one shared util with drifting
/// callers").
const QUEUED_STRIP_PREVIEW_CHARS: usize = 40;

fn truncate_with_ellipsis(text: &str, budget: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= budget {
        return text.to_string();
    }
    let truncated: String = text.chars().take(budget.saturating_sub(1)).collect();
    format!("{truncated}…")
}

/// Board item `01M1YVFRPH0DCE8N0DR5BS5BRT`, review round 1 (SIGNIFICANT
/// finding 2, "prose starting with `!` is executed silently"): the box's
/// own title/border change the INSTANT the draft starts with `!`, before
/// `Enter` is ever pressed -- visible proof the operator is about to run a
/// command, not send a message, mirroring Claude Code's own bash-mode
/// indicator (the ruling the review cites). `!>` (the to-model form,
/// `tui::app::shell_cmd::Bang::Run { to_model: true, .. }`'s own parse)
/// gets its own distinct title so the two forms are never confused with
/// each other either.
///
/// Checked on the RAW `state.input` text, byte-for-byte the same prefix
/// `tui::app::shell_cmd::parse_bang` itself checks at submit time -- this
/// is a preview of that exact parse, not a second, independently-tuned
/// one, so the indicator can never show "shell" for a draft that would
/// actually submit as an ordinary prompt (a leading `\!`, the escape the
/// same finding adds -- see `App::submit`'s own escape check, `tui/app.rs`,
/// for where that backslash is stripped -- is NOT a `!` prefix by this
/// check either, since `"\\!...".starts_with('!')` is false: the indicator
/// and the escape agree by construction, not by two call sites kept in
/// step by hand).
///
/// `disabled` (a modal/form surface covering the input, including a live
/// permission prompt) wins outright over the shell-mode chrome below --
/// mirrors the pre-existing "input (paused)" precedence over anything else
/// this box could show. It does NOT win over the queued-strip summary
/// ([`queued_strip_title`]): board item `01M44PK089DF2M9TM3C4P5CKMZ` found a
/// message queued or steered while a permission prompt is up read as lost --
/// the title bailed out to the bare `"input (paused)"` word before the
/// strip ever got a chance to append its own `— N queued: ...`/`— N steer:
/// ...` summary, even though the underlying queue was intact the whole
/// time. Appending the SAME summary here that the enabled branch below
/// already shows keeps "paused" honest about what else is true right now.
fn input_box_chrome(state: &AppState, disabled: bool, theme: &Theme) -> (String, Style) {
    if disabled {
        return (
            queued_strip_title(state, "input (paused)"),
            theme.border_normal,
        );
    }
    let (base, style) = if state.input.starts_with("!>") {
        ("shell → model", theme.border_accent)
    } else if state.input.starts_with('!') {
        ("shell", theme.border_accent)
    } else {
        ("input", theme.border_normal)
    };
    let base = vim_mode_suffix(state, base);
    (queued_strip_title(state, &base), style)
}

/// Board item `01M1YVJNS575YN5DCQG9BKZR4E`: the vim mode indicator the
/// spec asks for in "the input tray" -- appended to the SAME border-title
/// string the shell-mode/paused chrome above already computes, so showing
/// it needs no layout change, mirroring how the queued strip
/// ([`queued_strip_title`]) already piggybacks on this one title string.
/// A no-op (returns `base` verbatim) whenever [`AppState::editor_mode`]
/// is `emacs` -- the indicator only exists for the mode it describes.
fn vim_mode_suffix(state: &AppState, base: &str) -> String {
    if !matches!(state.editor_mode, crate::tui::config::EditorMode::Vim) {
        return base.to_string();
    }
    match &state.vim.mode {
        crate::tui::input::vim::VimMode::Search { query } => format!("{base} [/{query}]"),
        _ => format!("{base} [{}]", state.vim.mode_label()),
    }
}

/// Board item `01M1YVHKTQVXJRDSRYT3TCRXFX`: the queued strip itself --
/// appended to the box's own border title (the line directly framing the
/// input) rather than a second bordered element, so no layout change is
/// needed to show it.
///
/// Review round 1, CRITICAL fix stated as a choice: `AppState::
/// queued_strip_summary` returns `(focused_count, first_line,
/// first_is_steer, other_count)`, scoped to the FOCUSED agent -- a
/// DIFFERENT, non-focused agent's own queue is folded into `other_count`
/// alone, shown as a bare number with no text (`state::busy_input`'s own
/// module doc states the reasoning: naming a background agent's
/// in-progress draft in the currently-focused conversation would leak a
/// different agent's business into this one). `(0, None, false, 0)`
/// (nothing queued anywhere, the overwhelmingly common case) leaves `base`
/// untouched.
///
/// Board item `01M44PK089DF2M9TM3C4P5CKMZ`: the word itself is `"steer"`,
/// not `"queued"`, when `first_is_steer` says the NEXT message to show is
/// already-sent-but-pending rather than withheld -- see `Entry::
/// QueuedUser`'s own doc for why the distinction matters to an operator.
fn queued_strip_title(state: &AppState, base: &str) -> String {
    let (focused_count, first_line, first_is_steer, other_count) = state.queued_strip_summary();
    if focused_count == 0 && other_count == 0 {
        return base.to_string();
    }
    let mut title = if focused_count > 0 {
        let first_line = first_line.unwrap_or_default();
        let first_line = first_line.lines().next().unwrap_or(first_line);
        let word = if first_is_steer { "steer" } else { "queued" };
        format!(
            "{base} — {focused_count} {word}: {}",
            truncate_with_ellipsis(first_line, QUEUED_STRIP_PREVIEW_CHARS)
        )
    } else {
        base.to_string()
    };
    if other_count > 0 {
        if focused_count > 0 {
            title.push_str(&format!(" (+{other_count} for other agents)"));
        } else {
            title.push_str(&format!(" — {other_count} queued (other agents)"));
        }
    }
    title
}

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let disabled = !matches!(state.mode, Mode::Normal);
    let (title, border_style) = input_box_chrome(state, disabled, theme);

    if state.input.is_empty() && !disabled {
        let paragraph = Paragraph::new(PLACEHOLDER).style(theme.dim).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(border_style),
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
            .border_style(border_style),
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

    /// Board item `01M1YVJNS575YN5DCQG9BKZR4E`, acceptance: "the tray shows
    /// the mode."
    #[test]
    fn vim_mode_shows_in_the_border_title() {
        let mut state = AppState::new(AgentId::new());
        state.editor_mode = crate::tui::config::EditorMode::Vim;
        state.input = "hello".to_string();
        state.cursor = 0;
        crate::tui::input::handle_key(
            &mut state,
            ratatui::crossterm::event::KeyEvent::new(
                ratatui::crossterm::event::KeyCode::Esc,
                ratatui::crossterm::event::KeyModifiers::NONE,
            ),
        );

        let backend = TestBackend::new(40, 5);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), &state, &Theme::default()))
            .expect("draw");

        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            text.contains("NORMAL"),
            "the input tray must show the vim mode: {text}"
        );
    }

    /// The emacs default shows no mode indicator at all -- it has no mode
    /// to show.
    #[test]
    fn emacs_mode_shows_no_mode_indicator() {
        let mut state = AppState::new(AgentId::new());
        state.input = "hello".to_string();
        state.cursor = 5;

        let (title, _) = input_box_chrome(&state, false, &Theme::default());
        assert_eq!(title, "input");
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

    /// The bug fixed here: a long single line used to CLAMP the
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

    /// Review round 1, SIGNIFICANT finding 2: the title becomes `shell`
    /// the instant the draft starts with `!`, before `Enter` is ever
    /// pressed.
    #[test]
    fn a_leading_bang_shows_the_shell_mode_title() {
        let mut state = AppState::new(AgentId::new());
        state.input = "!git status".to_string();
        state.cursor = state.input.chars().count();

        let backend = TestBackend::new(60, 5);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), &state, &Theme::default()))
            .expect("draw");

        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("shell"), "{text:?}");
        assert!(!text.contains("shell → model"), "{text:?}");
    }

    /// Sibling of the test above: `!>` (the to-model form) gets its own
    /// distinct title, `shell → model`, never confused with the bare `!`
    /// form's plain `shell`.
    #[test]
    fn a_leading_bang_arrow_shows_the_shell_to_model_title() {
        let mut state = AppState::new(AgentId::new());
        state.input = "!> echo hi".to_string();
        state.cursor = state.input.chars().count();

        let backend = TestBackend::new(60, 5);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), &state, &Theme::default()))
            .expect("draw");

        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("shell → model"), "{text:?}");
    }

    /// A leading `\!` is the escape (finding 2's own other half) -- it must
    /// NOT trigger the shell-mode title, since it never dispatches as a
    /// `!` command (`App::submit`'s own escape check, `tui/app.rs`).
    #[test]
    fn an_escaped_bang_does_not_show_the_shell_mode_title() {
        let mut state = AppState::new(AgentId::new());
        state.input = "\\!important".to_string();
        state.cursor = state.input.chars().count();

        let backend = TestBackend::new(60, 5);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), &state, &Theme::default()))
            .expect("draw");

        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!text.contains("shell"), "{text:?}");
    }

    /// Board item `01M1YVHKTQVXJRDSRYT3TCRXFX`, acceptance 1: "two messages
    /// typed during a turn show '2 queued'".
    #[test]
    fn two_queued_messages_show_the_count_and_the_next_messages_first_line() {
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        state.queue_prompt(agent, "fix the bug in the parser".to_string());
        state.queue_prompt(agent, "also run the tests".to_string());

        let backend = TestBackend::new(80, 5);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), &state, &Theme::default()))
            .expect("draw");

        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("2 queued"), "{text:?}");
        assert!(
            text.contains("fix the bug in the parser"),
            "the strip must show the NEXT message to send: {text:?}"
        );
    }

    /// Review round 1, CRITICAL fix: a message queued for a DIFFERENT,
    /// non-focused agent shows as a bare count only -- never its text.
    #[test]
    fn a_queued_message_for_another_agent_shows_only_a_bare_count() {
        let a = AgentId::new();
        let b = AgentId::new();
        let mut state = AppState::new(a);
        state.queue_prompt(a, "private draft for a".to_string());
        state.focus_agent(b);

        let backend = TestBackend::new(80, 5);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), &state, &Theme::default()))
            .expect("draw");

        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("1 queued"), "{text:?}");
        assert!(text.contains("other agents"), "{text:?}");
        assert!(
            !text.contains("private draft"),
            "a non-focused agent's own queued text must never leak into the strip: {text:?}"
        );
    }

    /// With nothing queued, the title carries no queued-strip suffix at
    /// all -- the common, unaffected case.
    #[test]
    fn no_queued_messages_shows_no_strip() {
        let state = AppState::new(AgentId::new());

        let backend = TestBackend::new(60, 5);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), &state, &Theme::default()))
            .expect("draw");

        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!text.contains("queued"), "{text:?}");
    }

    #[test]
    fn truncate_with_ellipsis_leaves_short_text_untouched() {
        assert_eq!(truncate_with_ellipsis("short", 40), "short");
    }

    #[test]
    fn truncate_with_ellipsis_shortens_long_text() {
        let long = "x".repeat(100);
        let truncated = truncate_with_ellipsis(&long, 10);
        assert_eq!(truncated.chars().count(), 10);
        assert!(truncated.ends_with('…'));
    }
}
