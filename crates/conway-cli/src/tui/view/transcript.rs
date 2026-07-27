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
//!
//! **Mouse-wheel decision (TUI transcript scrolling item):** crossterm mouse
//! capture is NOT enabled. Enabling it would let `MouseEventKind::ScrollUp`/
//! `ScrollDown` drive this pane, but crossterm mouse capture also disables
//! the terminal's own native click-drag text selection (a captured terminal
//! routes every mouse event to the app instead of the emulator), which is
//! exactly the mechanism the clean-copy guarantee above exists to keep
//! working (WI-127 criterion 2: the user selects/copies with the mouse).
//! Trading that away for wheel support -- even with a documented
//! modifier-drag escape hatch -- regresses a already-shipped, tested
//! invariant for a nice-to-have. Auto-follow + clamp + keyboard scroll
//! (`PageUp`/`PageDown`, this module's [`draw`] + `AppState::scroll_page_up`/
//! `scroll_page_down`) is the required core per this item's own spec and
//! ships without that trade-off; mouse wheel is left undone.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use super::theme::Theme;
use crate::tui::state::{AppState, Entry, NodeStatus, ToolStatus};

/// Renders the transcript, auto-following its own bottom while
/// `state.follow_tail` is set (the growing-conversation criterion: newest
/// output must stay visible with no manual scrolling) and clamping the
/// effective scroll offset to `[0, max_scroll]` otherwise (the
/// no-blank-overscroll criterion) -- both computed fresh on every render
/// from the SAME `Paragraph`/`Wrap` this function renders with, so the
/// clamp ceiling can never disagree with what is actually on screen.
pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let paragraph = build_paragraph(state, theme);
    let max = max_scroll(&paragraph, area.width, area.height);
    let effective_scroll = if state.follow_tail {
        max
    } else {
        state.scroll.min(max)
    };
    frame.render_widget(paragraph.scroll((effective_scroll, 0)), area);
}

/// The un-scrolled `Paragraph` `draw` renders from -- factored out so
/// [`wrapped_line_count`] (used by `app.rs`, via `view::max_scroll`, to
/// clamp `PageUp`/`PageDown` outside of a render pass) measures the exact
/// same wrapping this module actually draws with.
fn build_paragraph(state: &AppState, theme: &Theme) -> Paragraph<'static> {
    let lines: Vec<Line> = state
        .transcript
        .iter()
        .flat_map(|entry| entry_lines(entry, theme))
        .collect();
    Paragraph::new(lines).wrap(Wrap { trim: false })
}

/// The wrapped-line clamp ceiling for a `width`x`height` viewport: total
/// wrapped lines minus the viewport height, floored at 0 (a transcript
/// shorter than the viewport has nothing to scroll).
fn max_scroll(paragraph: &Paragraph<'static>, width: u16, height: u16) -> u16 {
    let total = paragraph.line_count(width);
    total.saturating_sub(height as usize).min(u16::MAX as usize) as u16
}

/// `state`'s transcript, wrapped to `width`, in lines -- the piece of
/// [`draw`]'s clamp math `app.rs` needs to reproduce for `PageUp`/`PageDown`
/// (see `view::max_scroll`), since it runs outside of any `Frame`/render
/// pass and so can't just read `draw`'s locals.
pub(super) fn wrapped_line_count(state: &AppState, width: u16) -> usize {
    build_paragraph(state, &Theme::default()).line_count(width)
}

/// Renders one transcript [`Entry`] into its plain-text line(s) -- every
/// entry kind is exactly one line except multi-line text bodies, which
/// split into one [`Line`] per physical line (see [`split_lines`]). A free
/// function (not inlined into `draw`) so it is directly unit-testable
/// against a `TestBackend`-free `Line`/`Span`. The `theme` parameter drives
/// every color/modifier on the emitted `Span`s (T1); the text content is
/// identical to a pre-T1 build at the default theme (visual parity).
pub fn entry_lines(entry: &Entry, theme: &Theme) -> Vec<Line<'static>> {
    match entry {
        Entry::User(text) => {
            let prefix = theme.user;
            text.split('\n')
                .enumerate()
                .map(|(i, line)| {
                    // Only the first physical line carries the "you> "
                    // prefix -- continuation lines of a multi-line message
                    // line up under the text, not re-prefixed.
                    if i == 0 {
                        Line::from(vec![
                            Span::styled("you> ", prefix),
                            Span::raw(line.to_string()),
                        ])
                    } else {
                        Line::from(line.to_string())
                    }
                })
                .collect()
        }
        Entry::Assistant { text } => split_lines(text, theme),
        Entry::Tool {
            name,
            status,
            preview,
            ..
        } => tool_lines(name, *status, preview, theme),
        Entry::Agent { label, status, .. } => vec![agent_line(label, *status, theme)],
        Entry::Notice { text } => text
            .split('\n')
            .map(|line| Line::from(Span::styled(line.to_string(), theme.notice)))
            .collect(),
    }
}

/// Splits `text` on `\n` into one [`Line`] per physical line, each styled
/// with `theme.assistant` -- ratatui does NOT interpret embedded newlines
/// within a single `Line`, so any multi-line entry text must be split
/// before construction or it collapses onto one row (the bug this function
/// exists to fix). T1: the per-line style is the `assistant` theme slot
/// (default `Style::default()`, preserving pre-T1 parity).
fn split_lines(text: &str, theme: &Theme) -> Vec<Line<'static>> {
    text.split('\n')
        .map(|line| Line::from(Span::styled(line.to_string(), theme.assistant)))
        .collect()
}

/// The tool's `[tag] name -- preview` line(s): a `preview` containing `\n`
/// (e.g. a multi-line command output) splits onto its own continuation
/// lines, unprefixed, following the same pattern as `Entry::User` above.
fn tool_lines(
    name: &str,
    status: ToolStatus,
    preview: &str,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let (tag, style) = tool_status_style(status, theme);

    if preview.is_empty() {
        return vec![Line::from(vec![
            Span::styled(format!("[{tag}] "), style),
            Span::raw(name.to_string()),
        ])];
    }

    let mut preview_lines = preview.split('\n');
    let first = preview_lines.next().unwrap_or_default();
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("[{tag}] "), style),
        Span::raw(name.to_string()),
        Span::raw(format!(" -- {first}")),
    ])];
    lines.extend(preview_lines.map(|line| Line::from(line.to_string())));
    lines
}

/// Maps a [`ToolStatus`] to its `[tag]` label and the theme slot for the
/// tag's color. Kept as a free function so the per-status -> style mapping
/// is testable on its own and so `tool_lines` reads as plain formatting.
fn tool_status_style(status: ToolStatus, theme: &Theme) -> (&'static str, Style) {
    match status {
        ToolStatus::Proposed => ("proposed", theme.tool_proposed),
        ToolStatus::AwaitingPermission => ("awaiting permission", theme.tool_awaiting),
        ToolStatus::Running => ("running", theme.tool_running),
        ToolStatus::Finished { is_error: false } => ("done", theme.tool_done),
        ToolStatus::Finished { is_error: true } => ("failed", theme.tool_failed),
    }
}

/// Subagent lifecycle, inline in the stream (WI-127 criterion 4).
fn agent_line(label: &str, status: NodeStatus, theme: &Theme) -> Line<'static> {
    let (tag, style) = node_status_style(status, theme);
    Line::from(vec![
        Span::styled(format!("[agent {tag}] "), style),
        Span::raw(label.to_string()),
    ])
}

/// Maps a [`NodeStatus`] to its `[agent <tag>]` label and the theme slot
/// for the tag's color. Shared between the transcript's inline
/// `Entry::Agent` line and the agent panel's status marker so the two
/// never drift apart on a color override. Kept `pub(super)` so the agent
/// panel (`agents.rs`) can reuse it for its own status marker coloring.
pub(super) fn node_status_style(status: NodeStatus, theme: &Theme) -> (&'static str, Style) {
    match status {
        NodeStatus::Starting => ("starting", theme.agent_starting),
        NodeStatus::Running => ("running", theme.agent_running),
        NodeStatus::AwaitingPermission => {
            ("awaiting permission", theme.agent_awaiting)
        }
        NodeStatus::Finished => ("done", theme.agent_finished),
        NodeStatus::Failed => ("failed", theme.agent_failed),
        NodeStatus::Cancelled => ("cancelled", theme.agent_cancelled),
    }
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
            Entry::Notice {
                text: "a notice".to_string(),
            },
        ];

        for entry in &entries {
            for line in entry_lines(entry, &Theme::default()) {
                let text = plain_text(&line);
                assert!(
                    !text.chars().any(|c| BOX_DRAWING_CHARS.contains(&c)),
                    "box-drawing chrome leaked into clean-copy text: {text:?}"
                );
            }
        }
    }

    #[test]
    fn user_entry_keeps_the_you_prefix() {
        let lines = entry_lines(&Entry::User("hello".to_string()), &Theme::default());
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
        terminal.draw(|f| draw(f, f.area(), &state, &Theme::default())).expect("draw");

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
        terminal.draw(|f| draw(f, f.area(), &state, &Theme::default())).expect("draw");

        let buffer = terminal.backend().buffer();
        for cell in buffer.content() {
            let symbol = cell.symbol();
            assert!(
                !BOX_DRAWING_CHARS.contains(&symbol.chars().next().unwrap_or(' ')),
                "box-drawing glyph found in transcript buffer: {symbol:?}"
            );
        }
    }

    // ---- transcript scrolling: auto-follow + clamp ----

    /// 20 one-line `Entry::Assistant`s tagged `line N` -- small enough that
    /// each fits on its own wrapped line at the test widths used below, so
    /// which ones are visible directly reflects the scroll offset.
    fn numbered_lines_state(n: usize) -> AppState {
        let mut state = AppState::new(AgentId::new());
        for i in 0..n {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
            });
        }
        state
    }

    fn rendered_text(state: &AppState, width: u16, height: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(f, f.area(), state, &Theme::default())).expect("draw");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    /// The core auto-follow criterion: a transcript taller than the
    /// viewport shows its TAIL (latest lines) while following, with no
    /// manual scrolling.
    #[test]
    fn auto_follow_shows_the_tail_of_a_transcript_taller_than_the_viewport() {
        let state = numbered_lines_state(20);
        assert!(state.follow_tail, "AppState::new must default to following");

        let text = rendered_text(&state, 20, 5);

        assert!(
            text.contains("line 19"),
            "the newest line must be visible while following: {text:?}"
        );
        assert!(
            !text.contains("line 0 "),
            "the oldest line must have scrolled off while following: {text:?}"
        );
    }

    /// Scrolling up (disengaging follow) reviews history from the requested
    /// offset instead of always snapping back to the tail.
    #[test]
    fn explicit_scroll_shows_history_when_not_following() {
        let mut state = numbered_lines_state(20);
        state.follow_tail = false;
        state.scroll = 0;

        let text = rendered_text(&state, 20, 5);

        assert!(
            text.contains("line 0 "),
            "scrolled to the top must show the oldest line: {text:?}"
        );
        assert!(
            !text.contains("line 19"),
            "scrolled to the top must not also show the newest line: {text:?}"
        );
    }

    /// The no-blank-overscroll criterion: an out-of-range `scroll` (e.g. a
    /// resize that shrank `max_scroll` out from under a stored offset) is
    /// clamped at render time to the true bottom, never past it.
    #[test]
    fn scroll_past_the_bottom_is_clamped_not_left_blank() {
        let mut state = numbered_lines_state(20);
        state.follow_tail = false;
        state.scroll = u16::MAX;

        let text = rendered_text(&state, 20, 5);

        assert!(
            text.contains("line 19"),
            "an overscrolled offset must clamp to the true bottom: {text:?}"
        );
        let blank_rows = text.chars().filter(|&c| c != ' ').count();
        assert!(
            blank_rows > 0,
            "clamped scroll must not render an all-blank viewport: {text:?}"
        );
    }

    // ---- embedded-newline splitting (the bug this item fixes) ----

    /// The bug itself: `entry_lines` must build one `Line` PER physical
    /// line of a `\n`-containing `Entry::Assistant` text, not one `Line`
    /// whose text happens to contain `\n` -- ratatui never interprets an
    /// embedded newline within a single `Line`, so the old
    /// `vec![Line::from(text.clone())]` collapsed every physical line onto
    /// one row.
    #[test]
    fn assistant_entry_splits_embedded_newlines_into_separate_lines() {
        let lines = entry_lines(
            &Entry::Assistant {
                text: "line one\nline two\nline three".to_string(),
            },
            &Theme::default(),
        );

        assert_eq!(
            lines.len(),
            3,
            "a 3-physical-line text must produce 3 `Line`s, not 1: {lines:?}"
        );
        assert_eq!(plain_text(&lines[0]), "line one");
        assert_eq!(plain_text(&lines[1]), "line two");
        assert_eq!(plain_text(&lines[2]), "line three");
    }

    /// Same fix, `Entry::Notice` -- the styled-Cyan variant shares the old
    /// one-`Line`-per-string pattern and needed the identical split.
    #[test]
    fn notice_entry_splits_embedded_newlines_into_separate_lines() {
        let lines = entry_lines(
            &Entry::Notice {
                text: "notice one\nnotice two".to_string(),
            },
            &Theme::default(),
        );

        assert_eq!(lines.len(), 2, "expected 2 `Line`s: {lines:?}");
        assert_eq!(plain_text(&lines[0]), "notice one");
        assert_eq!(plain_text(&lines[1]), "notice two");
    }

    /// End-to-end companion, through the REAL render pass into an actual
    /// terminal buffer (`crate::tui::test_support::render`): a multi-line
    /// `Entry::Assistant` must land on three SEPARATE rows of the rendered
    /// buffer, not one row containing a raw `\n` byte (which a
    /// `TestBackend` cell can never hold anyway -- proving this end-to-end
    /// is what actually would have caught the shipped bug, since the
    /// release build's visible symptom was on-screen collapse, not just an
    /// `entry_lines` return value).
    #[test]
    fn multiline_assistant_entry_renders_on_three_separate_rows() {
        use crate::tui::test_support;

        let mut state = AppState::new(AgentId::new());
        state.transcript.push(Entry::Assistant {
            text: "line one\nline two\nline three".to_string(),
        });

        let rows = test_support::render(&state, 80, 24);

        let row_one = rows.iter().position(|r| r.contains("line one"));
        let row_two = rows.iter().position(|r| r.contains("line two"));
        let row_three = rows.iter().position(|r| r.contains("line three"));

        assert!(
            row_one.is_some() && row_two.is_some() && row_three.is_some(),
            "all three physical lines must appear somewhere in the rendered \
             buffer: {rows:?}"
        );
        assert_ne!(
            row_one, row_two,
            "'line one' and 'line two' must render on DIFFERENT rows, not \
             collapse onto one: {rows:?}"
        );
        assert_ne!(
            row_two, row_three,
            "'line two' and 'line three' must render on DIFFERENT rows, not \
             collapse onto one: {rows:?}"
        );
        assert_eq!(
            row_one.unwrap() + 1,
            row_two.unwrap(),
            "'line two' must be the row immediately after 'line one' \
             (no blank/collapsed rows between them): {rows:?}"
        );
        assert_eq!(
            row_two.unwrap() + 1,
            row_three.unwrap(),
            "'line three' must be the row immediately after 'line two': {rows:?}"
        );
    }

    /// Same end-to-end assertion for `Entry::Notice`.
    #[test]
    fn multiline_notice_entry_renders_on_separate_rows() {
        use crate::tui::test_support;

        let mut state = AppState::new(AgentId::new());
        state.transcript.push(Entry::Notice {
            text: "notice one\nnotice two".to_string(),
        });

        let rows = test_support::render(&state, 80, 24);

        let row_one = rows.iter().position(|r| r.contains("notice one"));
        let row_two = rows.iter().position(|r| r.contains("notice two"));

        assert!(
            row_one.is_some() && row_two.is_some(),
            "both physical lines must appear in the rendered buffer: {rows:?}"
        );
        assert_eq!(
            row_one.unwrap() + 1,
            row_two.unwrap(),
            "'notice two' must be the row immediately after 'notice one': {rows:?}"
        );
    }
}
