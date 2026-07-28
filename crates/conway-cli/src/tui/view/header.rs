//! The sticky context header + the floating "jump to bottom" footer (T6).
//!
//! Two small, independently-drawn scroll affordances that answer the
//! "scrolled-back with no idea where I am" report:
//!
//! - **The sticky header** ([`draw`]) is a single plain line -- `session ·
//!   focused agent · model · ctx%` -- pinned above the transcript pane
//!   whenever the transcript actually overflows the viewport
//!   (`view::mod::layout`'s own doc explains the non-recursive
//!   overflow test that decides this without a layout feedback loop).
//!   When content fits on screen, no header row is reserved at all -- the
//!   layout does not shift for a session that never needs to scroll.
//! - **The floating footer** ([`draw_scroll_footer`]) is a small pill drawn
//!   over the BOTTOM ROW of the transcript area while `!state.follow_tail`
//!   (the user has scrolled away from the tail): `↓ N lines above tail --
//!   End to jump to bottom`. It disappears the instant `follow_tail`
//!   re-engages (`End`, or paging back down to the true bottom).
//!
//! **Neither widget is ever part of the transcript's own `Paragraph`**
//! (`view/transcript.rs`'s clean-copy guarantee: no `.block(..)`, no glyph
//! `entry_lines` did not itself emit). Both are drawn as their own,
//! separate `frame.render_widget` calls from `view::draw` -- the header
//! into its own reserved `Rect` above the transcript, the footer as a
//! `Clear` + `Paragraph` OVERLAY on top of the transcript's own last row,
//! the same "modal overlay drawn over transcript content, never folded
//! into its `Span`s" pattern `view/mod.rs`'s permission/`/ask`/intent
//! overlays already use. `entry_lines`/`build_lines` themselves are
//! completely untouched by this module -- the
//! `entry_lines_never_contain_box_drawing_glyphs` and
//! `rendered_buffer_contains_no_box_drawing_glyphs` tests in
//! `transcript.rs` still pass unmodified.
//!
//! **Mouse wheel stays out of scope.** `view/transcript.rs`'s own module
//! doc already explains why crossterm mouse capture is not enabled (it
//! would disable the terminal's native click-drag text selection, which
//! the clean-copy guarantee exists to protect). T6 ships `PageUp`/
//! `PageDown` (existing) plus `End`/`Home` (new, this module's keys) and
//! this floating footer as the keyboard-only, selection-preserving answer
//! to "how do I get back to the bottom" -- not a mouse-wheel workaround.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use conway::AgentId;

use super::status;
use super::theme::Theme;
use crate::tui::state::AppState;

/// The sticky header's fixed height -- always exactly one plain line, no
/// border (mirroring `view/status.rs`'s own single-line, borderless
/// treatment of the bottom status line).
pub const HEADER_HEIGHT: u16 = 1;

/// Renders the sticky context header into `area` -- `view/mod.rs::layout`
/// only ever calls this with a `Some` header `Rect` (reserved only while
/// the transcript overflows), so there is no visibility check here: by the
/// time this runs, the caller has already decided the header belongs on
/// screen.
pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let paragraph = Paragraph::new(header_line(state, theme));
    frame.render_widget(paragraph, area);
}

/// The header's plain-text content: `session <id> [· agent <id>] [· model]
/// · ctx%`, joined with ` · `. `agent <id>` is present only while the
/// transcript is NOT showing the session's own root (mirroring
/// `view/status.rs::hint_spans`'s identical "off-root only" convention for
/// its own `focused: <id>` note, so the common single-agent case stays
/// uncluttered); `model` is present only once the focused agent's first
/// `Event::ModelDecision` has routed (mirrors the status line's `model`
/// field, which is likewise omitted before that point). `ctx%`/raw-tokens
/// reuses `status::ctx_label` directly -- see that function's own doc for
/// why (never a second, drift-prone copy of the percentage formula).
fn header_line(state: &AppState, theme: &Theme) -> Line<'static> {
    let mut parts = vec![format!("session {}", short_id(state.root_agent()))];
    if !state.is_root_focused() {
        parts.push(format!("agent {}", short_id(state.focused_agent)));
    }
    if let Some(model) = state.focused_model.as_deref() {
        parts.push(model.to_string());
    }
    parts.push(status::ctx_label(state));
    Line::from(Span::styled(format!(" {} ", parts.join(" · ")), theme.header))
}

/// An `AgentId`'s first 8 characters -- ULIDs are 26-character base32
/// strings, ASCII-only, so slicing by byte can never land mid-character.
/// Full precision is unnecessary for an at-a-glance header; the full id is
/// still always available via `/agents`.
fn short_id(id: AgentId) -> String {
    id.to_string().chars().take(8).collect()
}

/// Renders the floating "jump to bottom" pill over the bottom row of
/// `transcript_area` while `!state.follow_tail`; a no-op while following
/// (nothing to jump back to) or if `transcript_area` has no rows at all
/// (an extreme small-terminal edge case). `max_scroll` is caller-computed
/// (`view::mod::draw`, via `view::max_scroll`) -- this module has no
/// terminal-width/height of its own to derive the wrapped line count from,
/// the same reason every other scroll-adjacent function in this crate
/// takes `max_scroll` as a parameter rather than recomputing it.
///
/// Drawn as a SEPARATE `Clear` + `Paragraph` overlay directly on the
/// frame -- never folded into `transcript::draw`'s own `Paragraph`, so it
/// can never leak into the transcript's clean-copy text. This is the exact
/// "modal drawn over transcript content" shape `view/mod.rs`'s permission/
/// `/ask`/intent-confirm overlays already use (`Clear`, then a widget, over
/// a sub-`Rect` of the transcript area) -- just one row tall instead of
/// claiming most of the pane.
pub fn draw_scroll_footer(
    frame: &mut Frame,
    transcript_area: Rect,
    state: &AppState,
    theme: &Theme,
    max_scroll: u16,
) {
    if state.follow_tail || transcript_area.height == 0 {
        return;
    }
    let above = state.lines_above_tail(max_scroll);
    let area = Rect {
        x: transcript_area.x,
        y: transcript_area.y + transcript_area.height - 1,
        width: transcript_area.width,
        height: 1,
    };
    let text = footer_text(above, area.width);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(Span::styled(text, theme.scroll_footer)),
        area,
    );
}

/// The footer's text at `width` columns, degrading rather than being
/// clipped mid-word.
///
/// The full form names both the position and the way out. On a narrow
/// terminal that does not fit, a hard truncation would cut the `End` hint
/// off first -- the half that tells the user what to *do* -- leaving a
/// dangling `↓ 8 lines above tai`. So the variants drop information in
/// order of least usefulness: the wordy form, then the terse one, then the
/// bare count. Whatever fits is complete; nothing is ever shown as a
/// fragment.
fn footer_text(above: u16, width: u16) -> String {
    let width = width as usize;
    let candidates = [
        format!(" ↓ {above} lines above tail — End to jump to bottom "),
        format!(" ↓ {above} above — End to jump "),
        format!(" ↓ {above} — End "),
        format!(" ↓{above} "),
    ];
    candidates
        .iter()
        .find(|c| c.chars().count() <= width)
        .cloned()
        // Every candidate is wider than the pane (a terminal only a few
        // columns wide). Show the shortest and let the terminal clip it --
        // there is no shorter honest form left to fall back to.
        .unwrap_or_else(|| candidates[candidates.len() - 1].clone())
}

#[cfg(test)]
mod tests {
    use conway::AgentId as TestAgentId;

    use super::*;
    use crate::tui::state::Entry;

    fn thirty_lines_state() -> AppState {
        let mut state = AppState::new(TestAgentId::new());
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        state
    }

    // ---- header_line content ----

    #[test]
    fn header_line_includes_session_and_ctx_but_not_agent_or_model_by_default() {
        let root = TestAgentId::new();
        let state = AppState::new(root);
        let text = plain(&header_line(&state, &Theme::default()));

        assert!(
            text.contains(&format!("session {}", short_id(root))),
            "{text}"
        );
        assert!(text.contains("ctx"), "{text}");
        assert!(
            !text.contains("agent "),
            "root-focused must not show a redundant `agent <id>` field: {text}"
        );
    }

    #[test]
    fn header_line_shows_the_focused_agent_off_root() {
        let root = TestAgentId::new();
        let mut state = AppState::new(root);
        let child = TestAgentId::new();
        state.focus_agent(child);

        let text = plain(&header_line(&state, &Theme::default()));

        assert!(text.contains(&format!("session {}", short_id(root))), "{text}");
        assert!(text.contains(&format!("agent {}", short_id(child))), "{text}");
    }

    #[test]
    fn header_line_shows_the_model_once_known() {
        let mut state = AppState::new(TestAgentId::new());
        state.focused_model = Some("anthropic/claude-sonnet-4-6".to_string());
        let text = plain(&header_line(&state, &Theme::default()));
        assert!(text.contains("anthropic/claude-sonnet-4-6"), "{text}");
    }

    #[test]
    fn header_line_ctx_matches_the_shared_status_line_helper() {
        let mut state = AppState::new(TestAgentId::new());
        state.focused_model_max_context = Some(200_000);
        state.focused_ctx_tokens = 50_000; // 25%
        let text = plain(&header_line(&state, &Theme::default()));
        assert!(
            text.contains(&status::ctx_label(&state)),
            "the header must reuse status::ctx_label verbatim, not a second \
             copy of the percentage formula: {text}"
        );
        assert!(text.contains("ctx 25%"), "{text}");
    }

    fn plain(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    // ---- draw_scroll_footer ----

    /// The header's visibility rule is the whole reason `layout` needs a
    /// non-recursive overflow test: a short conversation must not pay a row
    /// for it. This exercises the rule end to end through the real
    /// `view::draw`, not the predicate in isolation.
    #[test]
    fn header_is_absent_when_content_fits_and_present_once_it_overflows() {
        use crate::tui::test_support::render_text;

        let root = TestAgentId::new();
        let mut state = AppState::new(root);
        state.transcript.push(Entry::Assistant {
            text: "only line".to_string(),
            model: None,
            summary: None,
            ts: None,
        });

        // 24 rows, one transcript line: nothing to scroll, so no header.
        let fits = render_text(&state, 60, 24);
        assert!(
            !fits.contains(&format!("session {}", short_id(root))),
            "a transcript that fits on screen must not reserve a header row: {fits}"
        );

        // Same viewport, far more content than rows: the header appears.
        for i in 0..80 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        let overflows = render_text(&state, 60, 24);
        assert!(
            overflows.contains(&format!("session {}", short_id(root))),
            "an overflowing transcript must show the sticky header: {overflows}"
        );
    }

    #[test]
    fn footer_is_absent_while_following() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let state = thirty_lines_state();
        assert!(state.follow_tail);

        let backend = TestBackend::new(20, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, 20, 8);
                draw_scroll_footer(f, area, &state, &Theme::default(), 20);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(
            !text.contains("jump to bottom"),
            "the footer must not render while follow_tail is set: {text}"
        );
    }

    #[test]
    fn footer_shows_the_correct_lines_above_tail_count_while_scrolled_up() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut state = thirty_lines_state();
        state.follow_tail = false;
        state.scroll = 12; // max_scroll(20) - 12 = 8 lines above tail.

        // Wide enough for the full form -- `footer_text`'s narrow variants
        // are exercised separately below.
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, 60, 8);
                draw_scroll_footer(f, area, &state, &Theme::default(), 20);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(
            text.contains("8 lines above tail"),
            "the footer must name the live lines-above-tail count: {text}"
        );
        assert!(text.contains("End to jump to bottom"), "{text}");
    }

    /// The footer degrades to a shorter complete form rather than being
    /// clipped mid-word: a truncated ` ↓ 8 lines above tai` loses the `End`
    /// hint, which is the half that tells the user what to do about it.
    #[test]
    fn footer_text_degrades_to_a_complete_shorter_form_when_narrow() {
        let wide = footer_text(8, 60);
        assert!(wide.contains("lines above tail"), "{wide}");
        assert!(wide.contains("End to jump to bottom"), "{wide}");

        for width in [40u16, 20, 12, 6] {
            let text = footer_text(8, width);
            assert!(
                text.chars().count() <= width as usize,
                "width {width} must not overflow: {text:?} ({} chars)",
                text.chars().count()
            );
            assert!(
                text.contains('8'),
                "every form must still name the count: {text:?}"
            );
        }

        // The narrower forms keep the `End` affordance for as long as it
        // fits at all.
        assert!(footer_text(8, 32).contains("End"), "{}", footer_text(8, 32));
    }

    #[test]
    fn footer_is_a_separate_overlay_not_part_of_the_transcript_paragraph() {
        // End to end: through the REAL `view::draw`, the footer text must
        // land on screen while scrolled up, but `transcript::entry_lines`
        // itself never emits it -- proven by the fact that scrolling back
        // to the tail makes it disappear again with no state change to the
        // transcript's own entries (only `follow_tail` changed).
        use crate::tui::test_support::render_text;

        let mut state = thirty_lines_state();
        state.follow_tail = false;
        state.scroll = 0;

        let scrolled_up = render_text(&state, 60, 8);
        assert!(
            scrolled_up.contains("lines above tail"),
            "{scrolled_up}"
        );

        state.follow_tail = true;
        let following = render_text(&state, 60, 8);
        assert!(
            !following.contains("lines above tail"),
            "the footer must disappear once follow_tail re-engages, with no \
             transcript entry mutation at all: {following}"
        );
    }
}
