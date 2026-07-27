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
use crate::tui::state::{Activity, AppState, Entry, NodeStatus, ToolStatus};

/// The streaming-line cursor (T2): a block `▌` (U+258C) appended at RENDER
/// time to the live, in-progress `Entry::Assistant` line while `activity ==
/// Responding`. This is a render-time decoration ONLY -- it is never baked
/// into the stored `Entry::Assistant` text or into [`entry_lines`] output
/// for settled entries (the clean-copy invariant, decision
/// 01KYJFB983G6KH491ZKYYHYM1K, is RELAXED only for the actively-streaming
/// line per decision D-clean-copy). See [`build_paragraph`] for the
/// append-at-render-time mechanism that keeps settled `entry_lines` output
/// unchanged.
const STREAMING_CURSOR: &str = "▌";

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
///
/// T2 streaming cursor: while `state.activity == Responding`, a block `▌`
/// ([`STREAMING_CURSOR`]) `Span` is appended to the LAST `Line` produced by
/// the LAST `Entry::Assistant` in `state.transcript` -- the live,
/// in-progress assistant line. This is a RENDER-TIME decoration: it is
/// added HERE, on the `Line` produced from the entry, NEVER baked into the
/// stored `Entry::Assistant` text or into [`entry_lines`] output for
/// settled entries. The clean-copy invariant (settled `entry_lines` output
/// never contains block/box glyphs) is preserved -- `entry_lines` itself is
/// unchanged; the cursor lives only in this render path, and only while the
/// line is actively streaming. When the turn settles (`TurnFinished` ->
/// `activity` returns to `Idle`), the cursor disappears from the next
/// render because the gate here stops firing.
fn build_paragraph(state: &AppState, theme: &Theme) -> Paragraph<'static> {
    Paragraph::new(build_lines(state, theme)).wrap(Wrap { trim: false })
}

/// The `Vec<Line>` `build_paragraph` wraps into a `Paragraph` -- factored
/// out (T2) so the streaming-cursor behavior is directly unit-testable
/// without a `TestBackend`/`Paragraph` round-trip. The streaming cursor
/// (`STREAMING_CURSOR`) is appended to the last `Line` of the last
/// `Entry::Assistant` ONLY while `state.activity == Responding`; it is
/// never baked into [`entry_lines`] output for settled entries (clean-copy
/// invariant preserved -- see [`STREAMING_CURSOR`]'s doc).
fn build_lines(state: &AppState, theme: &Theme) -> Vec<Line<'static>> {
    let streaming = matches!(state.activity, Activity::Responding);
    // Index of the last `Entry::Assistant` in the transcript, if any -- the
    // streaming cursor is attached to ITS last line only, never to a
    // settled assistant entry that happens to sit before a later
    // tool/agent/notice entry.
    let last_assistant_idx = state
        .transcript
        .iter()
        .rposition(|e| matches!(e, Entry::Assistant { .. }));
    state
        .transcript
        .iter()
        .enumerate()
        .flat_map(|(i, entry)| {
            let mut lines = entry_lines(entry, state.tool_preview_lines, theme);
            if streaming && Some(i) == last_assistant_idx {
                if let Some(last) = lines.last_mut() {
                    // Reuse the assistant body style for the cursor so it
                    // reads as part of the streaming line, not a separate
                    // accent. Uses the `theme.assistant` slot -- never an
                    // inline `Style::default().fg(..)` literal (T1's grep
                    // guard forbids that in view files other than
                    // `theme.rs`).
                    last.spans
                        .push(Span::styled(STREAMING_CURSOR.to_string(), theme.assistant));
                }
            }
            lines
        })
        .collect()
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
pub fn entry_lines(entry: &Entry, tool_cap: u32, theme: &Theme) -> Vec<Line<'static>> {
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
            expanded,
            ..
        } => tool_lines(name, *status, preview, *expanded, tool_cap, theme),
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

/// The tool's `[tag] name -- preview` line(s), with T5 folding. The stored
/// `preview` is NEVER truncated -- the cap is render-time only. While
/// `expanded` is `false`, only the first `cap` physical lines of the
/// preview render, followed by a dim `… (+M lines, Ctrl-E to expand)`
/// affordance (M = total - cap, total being the preview's physical line
/// count). While `expanded` is `true`, the full preview renders. No
/// box-drawing, no `Block` -- the clean-copy invariant (settled tool
/// output) is preserved. A settled tool entry (non-empty preview) ends
/// with a blank line + a dim plain `-` rule as a non-box separator.
///
/// **T4 reuse:** the `expanded` flag + this collapsed/expanded render
/// branch are intentionally generic -- T4's tool-args preview is the same
/// shape: the `expanded` flag, the `cap`-gated collapsed branch, and the
/// `… (+M lines, Ctrl-E to expand)` affordance are reusable. The header
/// here is tool-output-specific (`[{tag}] {name} -- {first}`), so T4 should
/// share the collapsed-branch shape via a sibling function rather than call
/// `tool_lines` directly -- the mechanism is reusable, the function is not.
fn tool_lines(
    name: &str,
    status: ToolStatus,
    preview: &str,
    expanded: bool,
    cap: u32,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let (tag, style) = tool_status_style(status, theme);

    if preview.is_empty() {
        return vec![Line::from(vec![
            Span::styled(format!("[{tag}] "), style),
            Span::raw(name.to_string()),
        ])];
    }

    let all_lines: Vec<&str> = preview.split('\n').collect();
    let total = all_lines.len();
    let first = all_lines.first().copied().unwrap_or_default();

    let mut lines = vec![Line::from(vec![
        Span::styled(format!("[{tag}] "), style),
        Span::raw(name.to_string()),
        Span::raw(format!(" -- {first}")),
    ])];

    let cap = cap.max(1) as usize;
    if expanded || total <= cap {
        // Expanded (or short enough that the cap doesn't bite): emit every
        // remaining physical line of the preview, unprefixed.
        lines.extend(all_lines.iter().skip(1).map(|line| Line::from(line.to_string())));
    } else {
        // Collapsed: the first `cap` lines total (the header line above is
        // line 1, so `cap - 1` continuation lines here) + the dim
        // affordance naming how many lines are hidden.
        let continuation = cap.saturating_sub(1);
        lines.extend(
            all_lines
                .iter()
                .skip(1)
                .take(continuation)
                .map(|line| Line::from(line.to_string())),
        );
        let hidden = total.saturating_sub(cap);
        lines.push(Line::from(Span::styled(
            format!("… (+{hidden} lines, Ctrl-E to expand)"),
            theme.dim,
        )));
    }

    // Clean-copy separator for settled tool output: a blank line + a dim
    // plain `-` rule (non-box). No `Block`, no box-drawing glyph -- the
    // clean-copy invariant is preserved (the `entry_lines_never_contain_
    // box_drawing_glyphs` test covers this).
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("-", theme.dim)));

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
                expanded: false,
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
            for line in entry_lines(entry, 3, &Theme::default()) {
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
        let lines = entry_lines(&Entry::User("hello".to_string()), 3, &Theme::default());
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

    /// T5 no-snap-on-shrink (review finding 2): when a toggle SHRINKS content
    /// so `max_scroll` drops below the stored `scroll`, the render clamp must
    /// seat the viewport at the new `max_scroll` (nearest valid position) --
    /// not a blank viewport, not a stale offset. The toggle itself never
    /// writes `scroll`/`follow_tail`; only the render clamp adjusts.
    #[test]
    fn toggle_shrink_re_clamps_viewport_to_new_bottom_not_blank() {
        let preview = (0..20)
            .map(|i| format!("out line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let expanded_entry = Entry::Tool {
            call_id: "c1".to_string(),
            name: "bash".to_string(),
            status: ToolStatus::Finished { is_error: false },
            preview,
            expanded: true,
        };
        let mut state = AppState::new(AgentId::new());
        state.transcript.push(expanded_entry);
        state.follow_tail = false;
        // Overscroll, mirroring a user who paged to the very bottom while
        // expanded: `scroll` sits at/past the expanded `max_scroll`.
        state.scroll = u16::MAX;

        // Expanded render: clamps to the expanded bottom, shows the last line.
        let expanded_text = rendered_text(&state, 40, 6);
        assert!(
            expanded_text.contains("out line 19"),
            "expanded overscroll must clamp to the expanded bottom: {expanded_text:?}"
        );

        // Toggle: collapse every tool entry. Content shrinks ~17 lines; the
        // stored `scroll` (u16::MAX) is now far above the collapsed `max_scroll`.
        state.toggle_all_tool_entries_expanded();
        // The toggle must NOT have touched scroll/follow_tail -- only the
        // render re-clamps (state-level contract pinned in state.rs tests).
        assert_eq!(state.scroll, u16::MAX, "toggle must not write scroll");
        assert!(!state.follow_tail, "toggle must not engage follow_tail");

        let collapsed_text = rendered_text(&state, 40, 6);
        let blank_rows = collapsed_text.chars().filter(|&c| c != ' ').count();
        assert!(
            blank_rows > 0,
            "shrunken content must re-clamp to the new bottom, not render blank: {collapsed_text:?}"
        );
        // The affordance line is the collapsed tail's last content line; it
        // must be visible, proving the viewport sits at the collapsed
        // `max_scroll` rather than floating in stale overscroll.
        assert!(
            collapsed_text.contains("Ctrl-E to expand"),
            "collapsed re-clamp must show the affordance at the new bottom: {collapsed_text:?}"
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
            3,
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
            3,
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

    // ---- T2: streaming cursor on the live assistant line ----

    /// The streaming cursor ([`STREAMING_CURSOR`]) is a RENDER-TIME
    /// decoration on the live, in-progress `Entry::Assistant` line while
    /// `activity == Responding`. This is the pure half of the test: the
    /// `Line`s produced by [`build_lines`] (the function `build_paragraph`
    /// wraps into a `Paragraph`) end with the cursor glyph while
    /// Responding.
    #[test]
    fn streaming_cursor_present_on_last_assistant_line_while_responding() {
        let mut state = AppState::new(AgentId::new());
        state.transcript.push(Entry::Assistant {
            text: "streaming live".to_string(),
        });
        state.activity = Activity::Responding;

        let lines = build_lines(&state, &Theme::default());
        let last = lines.last().expect("at least one line");
        let text: String = last.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.ends_with(STREAMING_CURSOR),
            "the live assistant line must end with the streaming cursor while Responding, got: {text:?}"
        );
        assert!(
            text.contains("streaming live"),
            "the body text must still be present, got: {text:?}"
        );
    }

    #[test]
    fn streaming_cursor_absent_when_not_responding() {
        let mut state = AppState::new(AgentId::new());
        state.transcript.push(Entry::Assistant {
            text: "settled line".to_string(),
        });

        // Idle: no cursor.
        state.activity = Activity::Idle;
        let lines = build_lines(&state, &Theme::default());
        let last = lines.last().expect("at least one line");
        let text: String = last.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            !text.contains(STREAMING_CURSOR),
            "an idle/ settled assistant line must NOT carry the streaming cursor, got: {text:?}"
        );

        // Also explicitly: Thinking/RunningTool/AwaitingPermission do NOT
        // add the cursor -- only Responding does (the spec: "while activity
        // == Responding"). A Thinking state with an assistant entry on the
        // transcript (e.g. mid-turn before the first TextDelta) must not
        // show the cursor on the PREVIOUS turn's settled assistant line.
        for activity in [
            Activity::Thinking,
            Activity::RunningTool("bash".to_string()),
            Activity::AwaitingPermission,
        ] {
            state.activity = activity.clone();
            let lines = build_lines(&state, &Theme::default());
            let last = lines.last().expect("at least one line");
            let text: String = last.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                !text.contains(STREAMING_CURSOR),
                "a non-Responding activity ({activity:?}) must NOT add the cursor, got: {text:?}"
            );
        }
    }

    /// The cursor is attached ONLY to the LAST `Entry::Assistant`'s last
    /// line -- an earlier assistant entry that sits before a later
    /// assistant entry must not get the cursor (it's not the streaming
    /// tail). And the cursor lands on the last PHYSICAL line of a
    /// multi-line assistant entry.
    #[test]
    fn streaming_cursor_lands_on_the_last_physical_line_of_the_last_assistant_entry() {
        let mut state = AppState::new(AgentId::new());
        state.transcript.push(Entry::Assistant {
            text: "earlier\nmulti\nline".to_string(),
        });
        state.transcript.push(Entry::Assistant {
            text: "live\nsecond".to_string(),
        });
        state.activity = Activity::Responding;

        let lines = build_lines(&state, &Theme::default());
        // The last line of the last assistant entry is "second" + cursor.
        let last = lines.last().expect("at least one line");
        let text: String = last.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.ends_with(STREAMING_CURSOR),
            "the last physical line of the last assistant entry must carry the cursor: {text:?}"
        );
        assert!(text.starts_with("second"));
        // The earlier assistant entry's lines do NOT carry the cursor.
        for line in &lines {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            if text.contains("earlier") || text == "multi" || text == "line" {
                assert!(
                    !text.contains(STREAMING_CURSOR),
                    "a non-tail assistant line must not carry the cursor: {text:?}"
                );
            }
        }
    }

    /// The clean-copy invariant is preserved: `entry_lines` itself NEVER
    /// emits the streaming cursor (or any block glyph) -- the cursor is
    /// added in `build_lines`'s render path only. This is the load-bearing
    /// assertion behind the "render-time only" mechanism.
    #[test]
    fn entry_lines_itself_never_emits_the_streaming_cursor() {
        let entry = Entry::Assistant {
            text: "hello".to_string(),
        };
        for line in entry_lines(&entry, 3, &Theme::default()) {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                !text.contains(STREAMING_CURSOR),
                "entry_lines must not bake the cursor into settled output: {text:?}"
            );
        }
    }

    /// End-to-end: the real `draw` render path adds the cursor to the
    /// streaming line while Responding, and the rendered buffer contains
    /// the cursor glyph on that row.
    #[test]
    fn rendered_buffer_shows_streaming_cursor_while_responding() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut state = AppState::new(AgentId::new());
        state.transcript.push(Entry::Assistant {
            text: "streaming live".to_string(),
        });
        state.activity = Activity::Responding;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), &state, &Theme::default()))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(
            text.contains(STREAMING_CURSOR),
            "the rendered buffer must contain the streaming cursor while Responding: {text}"
        );
        // The streaming line's body is still present.
        assert!(text.contains("streaming live"));
    }

    /// And the inverse end-to-end: an idle/ settled assistant line does
    /// NOT render the cursor through the real `draw` path.
    #[test]
    fn rendered_buffer_has_no_streaming_cursor_when_idle() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut state = AppState::new(AgentId::new());
        state.transcript.push(Entry::Assistant {
            text: "settled line".to_string(),
        });
        // activity stays Idle (the default).

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), &state, &Theme::default()))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(
            !text.contains(STREAMING_CURSOR),
            "an idle assistant line must not render the cursor: {text}"
        );
    }

    // ---- T5: tool output folding + expand ----

    /// Helper: a settled `Entry::Tool` with a `preview` of `n` physical
    /// lines, collapsed (`expanded: false`).
    fn collapsed_tool_with_n_lines(n: usize) -> Entry {
        let preview = (0..n)
            .map(|i| format!("out line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        Entry::Tool {
            call_id: "c1".to_string(),
            name: "bash".to_string(),
            status: ToolStatus::Finished { is_error: false },
            preview,
            expanded: false,
        }
    }

    /// Helper: the total rendered-line count for an entry at a given cap.
    fn rendered_line_count(entry: &Entry, cap: u32) -> usize {
        entry_lines(entry, cap, &Theme::default()).len()
    }

    /// The affordance text the collapsed branch emits, as plain text.
    fn affordance_text(line: &Line) -> Option<String> {
        let text = plain_text(line);
        if text.contains("Ctrl-E to expand") {
            Some(text)
        } else {
            None
        }
    }

    /// Acceptance: a 20-line preview collapsed under a cap of 3 renders
    /// at most N+1 lines (the cap, plus the `… (+M lines, Ctrl-E to
    /// expand)` affordance) -- NOT all 20. The header line is one of the
    /// capped lines, so the total is `cap + 1` (affordance) + 2 (blank +
    /// dim `-` separator) = `cap + 3`.
    #[test]
    fn collapsed_tool_preview_caps_at_n_plus_affordance() {
        let entry = collapsed_tool_with_n_lines(20);
        let cap = 3u32;
        let lines = entry_lines(&entry, cap, &Theme::default());

        // The affordance line is present and names the hidden count:
        // 20 total - 3 shown = 17 hidden.
        let affordance = lines
            .iter()
            .find_map(|l| affordance_text(l))
            .expect("collapsed preview must include the Ctrl-E affordance");
        assert!(
            affordance.contains("+17 lines"),
            "affordance must name the hidden count: {affordance}"
        );

        // No physical "out line {i}" past the cap appears.
        for i in 3..20 {
            let needle = format!("out line {i}");
            assert!(
                !lines.iter().any(|l| plain_text(l).contains(&needle)),
                "collapsed preview must not include line {i}: {:?}",
                lines.iter().map(plain_text).collect::<Vec<_>>()
            );
        }

        // Total line count: cap (header + cap-1 continuation) + 1
        // affordance + 2 separator (blank + `-`) = cap + 3.
        assert_eq!(
            lines.len(),
            cap as usize + 3,
            "collapsed 20-line preview at cap=3 must render cap+3 lines (cap + affordance + blank + `-`): {:?}",
            lines.iter().map(plain_text).collect::<Vec<_>>()
        );
    }

    /// Acceptance: with `expanded: true`, the full 20-line preview renders
    /// -- no affordance, no capping. The total is 1 header + 19
    /// continuation + 2 separator = 22.
    #[test]
    fn expanded_tool_preview_renders_all_lines() {
        let mut entry = collapsed_tool_with_n_lines(20);
        if let Entry::Tool { expanded, .. } = &mut entry {
            *expanded = true;
        }
        let lines = entry_lines(&entry, 3, &Theme::default());

        assert!(
            !lines.iter().any(|l| affordance_text(l).is_some()),
            "expanded preview must not include the collapsed affordance"
        );
        // Every physical line of the preview is present.
        for i in 0..20 {
            let needle = if i == 0 {
                "out line 0".to_string()
            } else {
                format!("out line {i}")
            };
            assert!(
                lines.iter().any(|l| plain_text(l).contains(&needle)),
                "expanded preview must include line {i}: {:?}",
                lines.iter().map(plain_text).collect::<Vec<_>>()
            );
        }
        assert_eq!(
            lines.len(),
            22,
            "expanded 20-line preview must render 1 header + 19 continuation + 2 separator = 22 lines"
        );
    }

    /// A preview with FEWER physical lines than the cap never collapses --
    /// no affordance, just the content + separator.
    #[test]
    fn short_tool_preview_is_not_collapsed() {
        let entry = collapsed_tool_with_n_lines(2);
        let lines = entry_lines(&entry, 3, &Theme::default());
        assert!(
            !lines.iter().any(|l| affordance_text(l).is_some()),
            "a preview shorter than the cap must not show the affordance"
        );
        // 1 header + 1 continuation + 2 separator = 4.
        assert_eq!(lines.len(), 4, "2-line preview at cap=3: {lines:?}");
    }

    /// The cap is honored: at cap=5, a 20-line preview shows 5 content
    /// lines + the affordance naming 15 hidden.
    #[test]
    fn collapsed_tool_preview_honors_a_configured_cap() {
        let entry = collapsed_tool_with_n_lines(20);
        let lines = entry_lines(&entry, 5, &Theme::default());
        let affordance = lines
            .iter()
            .find_map(|l| affordance_text(l))
            .expect("affordance present");
        assert!(
            affordance.contains("+15 lines"),
            "cap=5 -> 20-5=15 hidden: {affordance}"
        );
    }

    /// Clean-copy invariant: no box-drawing glyphs in collapsed OR expanded
    /// tool output (settled). The separator is a plain `-`, never `─` or
    /// `│`.
    #[test]
    fn tool_output_contains_no_box_drawing_glyphs_collapsed_or_expanded() {
        let collapsed = collapsed_tool_with_n_lines(20);
        for line in entry_lines(&collapsed, 3, &Theme::default()) {
            let text = plain_text(&line);
            assert!(
                !text.chars().any(|c| BOX_DRAWING_CHARS.contains(&c)),
                "collapsed tool output leaked a box glyph: {text:?}"
            );
        }

        let mut expanded = collapsed;
        if let Entry::Tool { expanded, .. } = &mut expanded {
            *expanded = true;
        }
        for line in entry_lines(&expanded, 3, &Theme::default()) {
            let text = plain_text(&line);
            assert!(
                !text.chars().any(|c| BOX_DRAWING_CHARS.contains(&c)),
                "expanded tool output leaked a box glyph: {text:?}"
            );
        }
    }

    /// The separator is a dim plain `-` (one character) on its own line,
    /// preceded by a blank line -- never a box-drawing rule.
    #[test]
    fn settled_tool_output_ends_with_a_blank_line_and_a_dim_plain_dash() {
        let entry = collapsed_tool_with_n_lines(2);
        let lines = entry_lines(&entry, 3, &Theme::default());
        // The last line is the `-` rule; the second-to-last is blank.
        let last = plain_text(lines.last().expect("at least the separator"));
        assert_eq!(last, "-", "the separator must be a single plain `-`: {last:?}");
        let second_last = plain_text(&lines[lines.len() - 2]);
        assert_eq!(
            second_last, "",
            "a blank line must precede the `-` separator: {second_last:?}"
        );
        // The `-` rule is styled with `theme.dim`.
        assert_eq!(
            lines.last().unwrap().spans.first().unwrap().style,
            Theme::default().dim,
            "the `-` separator must use theme.dim"
        );
    }

    /// An empty preview (a tool that has not finished, or finished with no
    /// output) does NOT get a separator -- only settled (non-empty preview)
    /// tool output does.
    #[test]
    fn empty_tool_preview_has_no_separator() {
        let entry = Entry::Tool {
            call_id: "c1".to_string(),
            name: "bash".to_string(),
            status: ToolStatus::Proposed,
            preview: String::new(),
            expanded: false,
        };
        let lines = entry_lines(&entry, 3, &Theme::default());
        assert_eq!(lines.len(), 1, "empty preview -> just the header line: {lines:?}");
        assert!(
            !lines.iter().any(|l| plain_text(l) == "-"),
            "no separator for an empty preview"
        );
    }

    /// `cap.max(1)` guard: a cap of 0 (which P-10 prevents at the config
    /// boundary, but the render path must still never divide-by-zero or
    /// emit zero capped lines) degrades to a 1-line cap.
    #[test]
    fn cap_of_zero_degrades_to_one_line() {
        let entry = collapsed_tool_with_n_lines(20);
        let lines = entry_lines(&entry, 0, &Theme::default());
        // cap=1 -> 1 content line + affordance + 2 separator = 4.
        assert_eq!(lines.len(), 4, "cap=0 degrades to cap=1: {lines:?}");
        let affordance = lines
            .iter()
            .find_map(|l| affordance_text(l))
            .expect("affordance present");
        assert!(
            affordance.contains("+19 lines"),
            "cap=1 -> 20-1=19 hidden: {affordance}"
        );
    }

    /// `rendered_line_count` helper sanity: a 20-line collapsed preview at
    /// cap=3 renders exactly cap+3 lines.
    #[test]
    fn rendered_line_count_helper_is_consistent() {
        let entry = collapsed_tool_with_n_lines(20);
        assert_eq!(rendered_line_count(&entry, 3), 6);
        assert_eq!(rendered_line_count(&entry, 5), 8);
    }
}
