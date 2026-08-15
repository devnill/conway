//! The input line's own local composer state: the cursor position
//! ([`AppState::cursor_line_col`]), the slash-command palette's anchor
//! ([`AppState::palette_source`]), and T8's persisted input history
//! ([`AppState::push_history`], [`AppState::history_recall_prev`]/
//! [`AppState::history_recall_next`]).

use super::*;

impl AppState {
    /// The text the slash-command palette's match list is anchored to
    ///: the stem the user last *typed* when set, else the raw
    /// `input`. Arrow navigation autofills `input` with a whole command but
    /// leaves the stem alone, so cycling the list does not collapse it; the
    /// `input` fallback keeps the palette visible for callers/tests that set
    /// `input` directly without going through key handling.
    pub fn palette_source(&self) -> &str {
        if self.palette_stem.is_empty() {
            &self.input
        } else {
            &self.palette_stem
        }
    }

    /// Re-anchors the palette to whatever the user just typed and clears the
    /// arrow highlight. Called after every edit to `input` in
    /// `input.rs`, so typing always re-filters live from the new text.
    pub fn sync_palette_stem(&mut self) {
        self.palette_stem = self.input.clone();
        self.palette_selected = None;
    }

    /// Closes the palette navigation state: called when `input` is
    /// submitted, so a fresh line starts with no stem and no highlight.
    pub fn clear_palette(&mut self) {
        self.palette_stem.clear();
        self.palette_selected = None;
    }

    /// The cursor's (line, column) position within [`Self::input`], both
    /// char indices -- `line` counts `\n` characters before the cursor,
    /// `column` is the cursor's offset from that line's own start (T8:
    /// multi-line input, Alt/Shift-Enter). Used by `view/input_box.rs` to
    /// place the on-screen cursor and by `input.rs`'s `Up`/`Down`
    /// vertical-cursor-movement gating.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let mut line = 0;
        let mut col = 0;
        for c in self.input.chars().take(self.cursor) {
            if c == '\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    /// Records a just-submitted line into [`Self::history`] (T8): pushed to
    /// the back (newest), then the front is evicted until the deque is back
    /// within [`Self::history_cap`] -- the circular-buffer behavior the
    /// item spec asks for. Always resets browsing state
    /// (`Self::history_index`/`Self::history_draft`) so the NEXT `Up`
    /// starts a fresh recall from the newest entry, not wherever a previous
    /// (now-stale) browse left off. `App::submit` calls this before
    /// dispatching the text, then persists `history` to disk (best-effort --
    /// this method itself does no I/O, so it can never fail a submit).
    pub fn push_history(&mut self, text: String) {
        self.history_index = None;
        self.history_draft.clear();
        if self.history_cap == 0 {
            self.history.clear();
            return;
        }
        self.history.push_back(text);
        while self.history.len() > self.history_cap {
            self.history.pop_front();
        }
    }

    /// `Up` while composing (T8): recalls the previous (older) history
    /// entry into `input`, saving whatever was already typed as
    /// `Self::history_draft` the FIRST time this starts browsing (`Up`
    /// from `history_index == None`) so [`Self::history_recall_next`] can
    /// restore it later. Returns whether it fired -- `false` (no mutation)
    /// when `history` is empty, letting the caller's `Up` fall through to
    /// whatever it would otherwise do. Repeated calls walk toward the
    /// oldest entry and simply stop there (still consuming the key,
    /// returning `true`) rather than wrapping.
    pub fn history_recall_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        match self.history_index {
            None => {
                self.history_draft = self.input.clone();
                self.set_input_from_history(self.history.len() - 1);
                true
            }
            Some(0) => true,
            Some(i) => {
                self.set_input_from_history(i - 1);
                true
            }
        }
    }

    /// `Down`'s counterpart (T8): recalls the next (newer) history entry,
    /// or -- once `Down` walks past the newest entry -- restores whatever
    /// unsent draft [`Self::history_recall_prev`] saved when browsing
    /// started, and stops browsing (`history_index` back to `None`).
    /// Returns whether it fired -- `false` when not currently browsing
    /// (`history_index` is already `None`), letting the caller's `Down`
    /// fall through, exactly mirroring [`Self::history_recall_prev`]'s
    /// empty-history case.
    pub fn history_recall_next(&mut self) -> bool {
        match self.history_index {
            None => false,
            Some(i) if i + 1 < self.history.len() => {
                self.set_input_from_history(i + 1);
                true
            }
            Some(_) => {
                self.history_index = None;
                self.input = std::mem::take(&mut self.history_draft);
                self.cursor = self.input.chars().count();
                true
            }
        }
    }

    /// Shared by [`Self::history_recall_prev`]/[`Self::history_recall_next`]:
    /// loads `history[idx]` into `input`, moves `history_index` to `idx`,
    /// and puts the cursor at the recalled line's end -- so the recalled
    /// prompt is immediately editable inline (item spec) starting from
    /// where you'd naturally continue typing.
    fn set_input_from_history(&mut self, idx: usize) {
        self.history_index = Some(idx);
        self.input = self.history[idx].clone();
        self.cursor = self.input.chars().count();
    }
}

/// T8: [`AppState::new`]'s default [`AppState::history_cap`] -- overridden
/// at `App::new` by [`clamp_history_size`] against the loaded
/// `[tui.history_size]` config.
pub const DEFAULT_HISTORY_SIZE: usize = 500;

/// T8: clamps a loaded `[tui.history_size]` config value into a safe
/// history-cap, the same shape as [`clamp_tool_preview_lines`]: config is
/// untrusted input, so never a panic and no `unwrap`/`expect`/indexing on
/// the value. `None` -> [`DEFAULT_HISTORY_SIZE`]. A value in
/// `1..=100_000` is kept as-is (converted to `usize`, infallible on every
/// platform this project targets). Any other value (`0`, `> 100_000`) falls
/// back to the default -- `0` is technically a valid cap
/// ([`AppState::push_history`] handles it), but silently keeping NO history
/// is more likely a typo than an intent, so it is treated the same as an
/// out-of-range value here, matching `clamp_tool_preview_lines`'s own
/// zero-falls-back-to-default precedent.
pub fn clamp_history_size(n: Option<u32>) -> usize {
    n.and_then(|v| {
        if (1..=100_000).contains(&v) {
            Some(v as usize)
        } else {
            None
        }
    })
    .unwrap_or(DEFAULT_HISTORY_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_source_prefers_the_stem_over_autofilled_input() {
        let mut state = AppState::new(AgentId::new());
        // No stem yet: the source mirrors `input` (covers direct-set callers).
        state.input = "/x".to_string();
        assert_eq!(state.palette_source(), "/x");
        // The user "typed" /a; an arrow then autofills `input` to a whole
        // command. The source stays the stem so the match list does not
        // collapse to the single autofilled entry.
        state.input = "/a".to_string();
        state.sync_palette_stem();
        state.input = "/agents".to_string();
        assert_eq!(state.palette_source(), "/a");
        // Submitting clears the stem; the source falls back to `input`.
        state.clear_palette();
        assert_eq!(state.palette_selected, None);
        assert_eq!(state.palette_source(), "/agents");
    }

    #[test]
    fn new_state_defaults_history_cap_to_500() {
        let state = AppState::new(AgentId::new());
        assert_eq!(state.history_cap, DEFAULT_HISTORY_SIZE);
        assert!(state.history.is_empty());
    }

    #[test]
    fn push_history_appends_newest_at_the_back() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("first".to_string());
        state.push_history("second".to_string());
        assert_eq!(
            state.history,
            std::collections::VecDeque::from(vec!["first".to_string(), "second".to_string()])
        );
    }

    #[test]
    fn push_history_evicts_the_oldest_once_the_cap_is_exceeded() {
        let mut state = AppState::new(AgentId::new());
        state.history_cap = 2;
        state.push_history("a".to_string());
        state.push_history("b".to_string());
        state.push_history("c".to_string());
        assert_eq!(
            state.history,
            std::collections::VecDeque::from(vec!["b".to_string(), "c".to_string()]),
            "the oldest entry ('a') must be evicted once the 2-entry cap is exceeded"
        );
    }

    #[test]
    fn push_history_with_a_zero_cap_keeps_no_history() {
        let mut state = AppState::new(AgentId::new());
        state.history_cap = 0;
        state.push_history("a".to_string());
        assert!(state.history.is_empty());
    }

    #[test]
    fn history_recall_prev_on_empty_history_does_not_fire() {
        let mut state = AppState::new(AgentId::new());
        state.input = "typing".to_string();
        assert!(!state.history_recall_prev());
        assert_eq!(
            state.input, "typing",
            "an empty history must not touch input"
        );
    }

    #[test]
    fn up_then_up_walks_from_newest_toward_oldest() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("older".to_string());
        state.push_history("newest".to_string());

        assert!(state.history_recall_prev());
        assert_eq!(state.input, "newest");
        assert!(state.history_recall_prev());
        assert_eq!(state.input, "older");
        // At the oldest entry: further `Up` still "fires" (consumes the
        // key) but stops moving.
        assert!(state.history_recall_prev());
        assert_eq!(state.input, "older");
    }

    #[test]
    fn down_after_up_walks_back_toward_the_newest() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("older".to_string());
        state.push_history("newest".to_string());

        state.history_recall_prev(); // -> "newest"
        state.history_recall_prev(); // -> "older"
        assert!(state.history_recall_next());
        assert_eq!(state.input, "newest");
    }

    #[test]
    fn down_past_the_newest_entry_restores_the_in_progress_draft() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("older".to_string());
        state.push_history("newest".to_string());
        state.input = "unsent draft".to_string();
        state.cursor = state.input.chars().count();

        assert!(state.history_recall_prev());
        assert_eq!(state.input, "newest");

        assert!(state.history_recall_next());
        assert_eq!(
            state.input, "unsent draft",
            "Down past the newest entry must restore the pre-recall draft"
        );
        assert_eq!(state.cursor, "unsent draft".chars().count());
    }

    #[test]
    fn history_recall_next_while_not_browsing_does_not_fire() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("only".to_string());
        state.input = "typing".to_string();

        assert!(!state.history_recall_next());
        assert_eq!(state.input, "typing");
    }

    #[test]
    fn a_recalled_prompt_is_editable_inline_before_resubmit() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("hello world".to_string());

        assert!(state.history_recall_prev());
        assert_eq!(state.input, "hello world");
        assert_eq!(state.cursor, "hello world".chars().count());

        // The recalled text is ordinary `input`/`cursor` state -- editing it
        // (simulated directly here; `input.rs`'s key handlers do the same
        // mutation for a real keypress) just works, with no special
        // "recalled" mode to escape first.
        state.input.push_str("!!!");
        state.cursor = state.input.chars().count();
        assert_eq!(state.input, "hello world!!!");
    }

    #[test]
    fn push_history_resets_browsing_state_for_the_next_recall() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("first".to_string());
        state.history_recall_prev();
        assert_eq!(state.input, "first");

        // Submitting resets browsing -- the next `Up` starts fresh from the
        // newest entry again, not wherever the previous browse left off.
        state.push_history("second".to_string());
        assert!(state.history_recall_prev());
        assert_eq!(state.input, "second");
    }

    #[test]
    fn clamp_history_size_none_falls_back_to_default() {
        assert_eq!(clamp_history_size(None), DEFAULT_HISTORY_SIZE);
    }

    #[test]
    fn clamp_history_size_in_range_value_is_kept() {
        assert_eq!(clamp_history_size(Some(1)), 1);
        assert_eq!(clamp_history_size(Some(500)), 500);
        assert_eq!(clamp_history_size(Some(100_000)), 100_000);
    }

    #[test]
    fn clamp_history_size_zero_falls_back_to_default() {
        assert_eq!(clamp_history_size(Some(0)), DEFAULT_HISTORY_SIZE);
    }

    #[test]
    fn clamp_history_size_above_max_falls_back_to_default() {
        assert_eq!(clamp_history_size(Some(100_001)), DEFAULT_HISTORY_SIZE);
        assert_eq!(clamp_history_size(Some(u32::MAX)), DEFAULT_HISTORY_SIZE);
    }

    #[test]
    fn cursor_line_col_on_a_single_line() {
        let mut state = AppState::new(AgentId::new());
        state.input = "hello".to_string();
        state.cursor = 3;
        assert_eq!(state.cursor_line_col(), (0, 3));
    }

    #[test]
    fn cursor_line_col_after_embedded_newlines() {
        let mut state = AppState::new(AgentId::new());
        state.input = "abc\ndef\ngh".to_string();
        // Cursor at the very end (char index 10): line 2 ("gh"), column 2.
        state.cursor = state.input.chars().count();
        assert_eq!(state.cursor_line_col(), (2, 2));

        // Cursor right after the first newline (char index 4): line 1
        // ("def"), column 0.
        state.cursor = 4;
        assert_eq!(state.cursor_line_col(), (1, 0));
    }
}
