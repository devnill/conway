//! The transcript pane's own scroll position: stick-to-bottom auto-follow,
//! `PageUp`/`PageDown`, and the T6 `Home`/`End` jump keys. See
//! [`AppState::follow_tail`]'s own doc for the auto-follow contract this
//! module implements.

use super::*;

impl AppState {
    /// `PageUp`: scrolls the transcript up by `page` (wrapped) lines and
    /// disengages auto-follow -- the user is now reviewing history, so new
    /// output must not yank the view back down (the transcript-scrolling
    /// item's own criterion: "scrolled-up state is not yanked to the bottom
    /// by new output"). Starts from `max_scroll` (i.e. the bottom) when
    /// `follow_tail` was still on, since that IS the view's current
    /// position even though `scroll` itself hasn't been tracking it.
    /// `max_scroll` is caller-computed (`app.rs`, via `view::max_scroll`) --
    /// this struct has no terminal width/height of its own to derive the
    /// wrapped line count from.
    pub fn scroll_page_up(&mut self, page: u16, max_scroll: u16) {
        let from = if self.follow_tail {
            max_scroll
        } else {
            self.scroll.min(max_scroll)
        };
        self.scroll = from.saturating_sub(page);
        self.follow_tail = false;
    }

    /// `PageDown`: scrolls the transcript down by `page` lines, clamped to
    /// `max_scroll` (never blank overscroll past the true bottom).
    /// Re-engages auto-follow once the view lands back on the bottom (the
    /// item's own criterion: "returning to bottom re-enables follow").
    pub fn scroll_page_down(&mut self, page: u16, max_scroll: u16) {
        let from = if self.follow_tail {
            max_scroll
        } else {
            self.scroll.min(max_scroll)
        };
        self.scroll = from.saturating_add(page).min(max_scroll);
        self.follow_tail = self.scroll >= max_scroll;
    }

    /// `End` (T6): snaps the transcript straight to its own tail --
    /// re-engages [`Self::follow_tail`] and resets the stored [`Self::scroll`]
    /// to 0. The stored `scroll` value is meaningless once `follow_tail` is
    /// set (the next render draws from `max_scroll` instead -- see
    /// `follow_tail`'s own doc), so 0 here is just the same tidy reset
    /// [`Self::new`] itself starts from, not a value anything actually reads
    /// while following. The complement to [`Self::jump_to_top`].
    pub fn jump_to_tail(&mut self) {
        self.follow_tail = true;
        self.scroll = 0;
    }

    /// `Home` (T6): jumps the transcript straight to its own TOP --
    /// disengages `follow_tail` (reviewing history, same as
    /// [`Self::scroll_page_up`]) and seats `scroll` at 0, the oldest wrapped
    /// line. [`Self::scroll`]'s own doc is explicit that it counts "wrapped
    /// lines from the top", and
    /// `transcript::tests::explicit_scroll_shows_history_when_not_following`
    /// pins `scroll == 0` as showing the OLDEST entry (`scroll == max_scroll`
    /// is the tail) -- so 0, not `max_scroll`, is what "the top" means in
    /// this codebase's established scroll direction. Takes `max_scroll` for
    /// call-site symmetry with `Self::scroll_page_up`/`Self::scroll_page_down`
    /// (`app.rs`'s caller already has it in hand from `view::max_scroll` --
    /// mirroring how it drives every other terminal-size-derived scroll
    /// mutation) even though jumping to the top needs no clamping of its own:
    /// 0 is always a valid offset regardless of `max_scroll`.
    pub fn jump_to_top(&mut self, max_scroll: u16) {
        let _ = max_scroll;
        self.follow_tail = false;
        self.scroll = 0;
    }

    /// T6: how many wrapped lines currently sit BELOW the bottom of the
    /// viewport (i.e. between the current scroll position and the tail) --
    /// the count the floating "jump to bottom" footer names (`↓ N lines
    /// above tail`). Always 0 while [`Self::follow_tail`] is set (the
    /// viewport already IS at the tail); while scrolled up, it is
    /// `max_scroll` minus the same clamped effective scroll
    /// `view/transcript.rs::draw` renders from, so this can never disagree
    /// with what is actually on screen.
    pub fn lines_above_tail(&self, max_scroll: u16) -> u16 {
        if self.follow_tail {
            0
        } else {
            max_scroll.saturating_sub(self.scroll.min(max_scroll))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_follows_the_tail_by_default() {
        let state = AppState::new(AgentId::new());
        assert!(state.follow_tail);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn scroll_page_up_from_following_starts_at_the_bottom_and_disengages_follow() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.follow_tail);

        state.scroll_page_up(5, 20);

        assert_eq!(
            state.scroll, 15,
            "must step up FROM the bottom (max_scroll)"
        );
        assert!(!state.follow_tail);
    }

    #[test]
    fn scroll_page_up_clamps_at_the_top() {
        let mut state = AppState::new(AgentId::new());
        state.scroll = 3;
        state.follow_tail = false;

        state.scroll_page_up(10, 20);

        assert_eq!(state.scroll, 0, "must not go negative / wrap");
        assert!(!state.follow_tail);
    }

    #[test]
    fn scroll_page_down_clamps_at_the_bottom_and_reengages_follow() {
        let mut state = AppState::new(AgentId::new());
        state.scroll = 15;
        state.follow_tail = false;

        state.scroll_page_down(10, 20);

        assert_eq!(state.scroll, 20, "must not overscroll past max_scroll");
        assert!(
            state.follow_tail,
            "landing back on the bottom must re-engage auto-follow"
        );
    }

    #[test]
    fn scroll_page_down_short_of_the_bottom_leaves_follow_disengaged() {
        let mut state = AppState::new(AgentId::new());
        state.scroll = 0;
        state.follow_tail = false;

        state.scroll_page_down(5, 20);

        assert_eq!(state.scroll, 5);
        assert!(
            !state.follow_tail,
            "must not re-engage follow until actually back at the bottom"
        );
    }

    #[test]
    fn scroll_page_up_then_down_round_trips_back_to_the_bottom() {
        let mut state = AppState::new(AgentId::new());
        state.scroll_page_up(4, 20); // 20 -> 16, follow off
        assert_eq!(state.scroll, 16);
        assert!(!state.follow_tail);

        state.scroll_page_down(4, 20); // 16 -> 20, follow re-engages
        assert_eq!(state.scroll, 20);
        assert!(state.follow_tail);
    }

    #[test]
    fn scroll_page_down_while_already_following_is_a_pinned_noop() {
        // If content grows (raising max_scroll) between renders, PageDown
        // while still following must not push `scroll` past the NEW bottom.
        let mut state = AppState::new(AgentId::new());
        assert!(state.follow_tail);

        state.scroll_page_down(3, 20);

        assert_eq!(state.scroll, 20);
        assert!(state.follow_tail);
    }

    #[test]
    fn jump_to_tail_reengages_follow_and_resets_scroll() {
        let mut state = AppState::new(AgentId::new());
        state.follow_tail = false;
        state.scroll = 7;

        state.jump_to_tail();

        assert!(state.follow_tail, "End must re-engage follow_tail");
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn jump_to_tail_from_already_following_is_a_noop_in_effect() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.follow_tail);

        state.jump_to_tail();

        assert!(state.follow_tail);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn jump_to_top_disengages_follow_and_seats_scroll_at_zero() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.follow_tail);

        state.jump_to_top(20);

        assert!(
            !state.follow_tail,
            "Home must disengage follow_tail -- the user is reviewing history"
        );
        assert_eq!(
            state.scroll, 0,
            "Home must land on the transcript's own top: scroll == 0 is the \
             oldest wrapped line in this codebase's scroll direction (see \
             transcript::tests::explicit_scroll_shows_history_when_not_following)"
        );
    }

    #[test]
    fn jump_to_top_from_mid_scroll_still_lands_at_zero() {
        let mut state = AppState::new(AgentId::new());
        state.follow_tail = false;
        state.scroll = 12;

        state.jump_to_top(20);

        assert!(!state.follow_tail);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn lines_above_tail_is_zero_while_following() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.follow_tail);
        assert_eq!(state.lines_above_tail(20), 0);

        // Even a nonzero stale `scroll` is irrelevant while following.
        state.scroll = 5;
        assert_eq!(state.lines_above_tail(20), 0);
    }

    #[test]
    fn lines_above_tail_counts_from_the_current_scroll_to_max() {
        let mut state = AppState::new(AgentId::new());
        state.follow_tail = false;
        state.scroll = 12;

        assert_eq!(state.lines_above_tail(20), 8);
    }

    #[test]
    fn lines_above_tail_is_zero_at_the_true_bottom_even_if_follow_is_off() {
        let mut state = AppState::new(AgentId::new());
        state.follow_tail = false;
        state.scroll = 20;

        assert_eq!(state.lines_above_tail(20), 0);
    }

    #[test]
    fn lines_above_tail_clamps_an_overscrolled_value() {
        let mut state = AppState::new(AgentId::new());
        state.follow_tail = false;
        state.scroll = u16::MAX;

        assert_eq!(
            state.lines_above_tail(20),
            0,
            "an overscrolled `scroll` must clamp, not underflow/wrap"
        );
    }
}
