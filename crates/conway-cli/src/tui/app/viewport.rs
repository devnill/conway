//! Terminal-size-derived transcript scrolling: `App`'s own thin wrappers
//! around `AppState`'s pure scroll math (`scroll_page_up`/`scroll_page_down`/
//! `jump_to_top`), supplying the `max_scroll`/page inputs those methods need
//! but don't have access to themselves. Extracted out of `app.rs` verbatim
//! (this item, board); [`super::run`]'s own key-handling match is the sole
//! production caller.

use ratatui::backend::Backend;
use ratatui::Terminal;

use super::App;
use crate::tui::view;

impl App {
    /// `PageUp`/`PageDown`: steps the transcript by ~one viewport page
    /// (`view::transcript_area`'s height, minus one line so the last row of
    /// the previous page stays in view for context -- floored at 1 so even
    /// a tiny terminal still moves). Delegates the actual scroll math to
    /// `AppState::scroll_page_up`/`scroll_page_down` (auto-follow
    /// disengage/re-engage, clamping) -- this method's only job is
    /// supplying the terminal-size-derived `max_scroll`/page inputs those
    /// pure methods need but don't have access to themselves.
    /// V3: a one-line transcript scroll, for bare `Up`/`Down` (which is
    /// what a terminal's alternate-scroll mode turns a wheel event into).
    /// Delegates to the same `scroll_page_up`/`scroll_page_down` state
    /// mutations with a page of 1, so the clamping and follow-tail
    /// re-engagement rules are literally the same code as the page-sized
    /// scroll -- one line is just a smaller page.
    pub(super) fn line_scroll<B: Backend>(
        &mut self,
        terminal: &Terminal<B>,
        up: bool,
    ) -> conway::Result<()>
    where
        // See `run`'s doc on the same bound: ratatui 0.30 widened
        // `Backend::Error` beyond the fixed `io::Error` it was in 0.29.
        B::Error: Into<std::io::Error>,
    {
        let size = terminal
            .size()
            .map_err(|e| conway::ConwayError::Io(e.into()))?;
        let area = ratatui::layout::Rect::new(0, 0, size.width, size.height);
        let max = view::max_scroll(&self.state, area);
        if up {
            self.state.scroll_page_up(1, max);
        } else {
            self.state.scroll_page_down(1, max);
        }
        Ok(())
    }

    pub(super) fn page_scroll<B: Backend>(
        &mut self,
        terminal: &Terminal<B>,
        page_up: bool,
    ) -> conway::Result<()>
    where
        // See `run`'s doc on the same bound: ratatui 0.30 widened
        // `Backend::Error` beyond the fixed `io::Error` it was in 0.29.
        B::Error: Into<std::io::Error>,
    {
        let size = terminal
            .size()
            .map_err(|e| conway::ConwayError::Io(e.into()))?;
        let area = ratatui::layout::Rect::new(0, 0, size.width, size.height);
        let transcript_area = view::transcript_area(&self.state, area);
        let max = view::max_scroll(&self.state, area);
        let page = transcript_area.height.saturating_sub(1).max(1);
        if page_up {
            self.state.scroll_page_up(page, max);
        } else {
            self.state.scroll_page_down(page, max);
        }
        Ok(())
    }

    /// `Home` (T6): jumps the transcript straight to its own top. Delegates
    /// the actual mutation to `AppState::jump_to_top`, mirroring how
    /// `page_scroll` delegates to `scroll_page_up`/`scroll_page_down` --
    /// this method's only job is the terminal-size-derived `max_scroll`
    /// that pure method's signature takes (for call-site symmetry with the
    /// page-scroll pair; see `AppState::jump_to_top`'s own doc on why the
    /// value itself goes unused). `End`'s `Action::JumpToTail` needs no
    /// terminal size at all, so it calls `AppState::jump_to_tail` directly
    /// from the action-dispatch match instead of routing through a method
    /// here.
    pub(super) fn jump_to_top<B: Backend>(&mut self, terminal: &Terminal<B>) -> conway::Result<()>
    where
        // See `run`'s doc on the same bound: ratatui 0.30 widened
        // `Backend::Error` beyond the fixed `io::Error` it was in 0.29.
        B::Error: Into<std::io::Error>,
    {
        let size = terminal
            .size()
            .map_err(|e| conway::ConwayError::Io(e.into()))?;
        let area = ratatui::layout::Rect::new(0, 0, size.width, size.height);
        let max = view::max_scroll(&self.state, area);
        self.state.jump_to_top(max);
        Ok(())
    }
}
