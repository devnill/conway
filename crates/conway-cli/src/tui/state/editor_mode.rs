//! Board item `01M1YVJNS575YN5DCQG9BKZR4E` ("opt-in vim editing mode for
//! the TUI prompt line"): [`AppState::editor_mode`] (seeded from
//! `[tui.editor_mode]`, `crate::tui::config::EditorMode`) selects whether
//! `input.rs`'s own fixed chain is the whole story (`emacs`, unchanged) or
//! also consults `crate::tui::input::vim` first (`vim`) -- see that
//! module's own doc for the full mechanism. [`AppState::vim`]
//! (`crate::tui::input::vim::VimState`) is the engine's own transient
//! session state -- mode, pending operator/count, the undo stack, the
//! unnamed register -- RESET on `/new`/`/resume` exactly like every other
//! piece of the OLD session's own in-progress editing would be (see
//! `AppState::reset_for_new_session`'s own doc), while [`AppState::
//! editor_mode`] itself is CARRIED, the same session-lifetime-preference
//! shape [`AppState::busy_input`] already has.

use super::*;

pub use crate::tui::config::EditorMode;

impl AppState {
    /// The `/settings` menu's "display" group row for [`Self::editor_mode`]
    /// (`Enter` on `LEAF_EDITOR_MODE`, `view/settings.rs`): flips `emacs
    /// <-> vim`, mirroring [`Self::cycle_busy_input`]'s own "pure `AppState`
    /// flip, no `Action` round trip" shape (session-only, no broker/config
    /// write). Also resets [`Self::vim`] back to its own fresh default --
    /// a partially-typed vim command (a pending operator, an open Visual
    /// selection, ...) has no meaning once the layer it belonged to is
    /// switched off, and stale undo/register state from a PREVIOUS stretch
    /// of vim editing earlier in the same session is exactly the kind of
    /// "looks idle but silently has memory" surprise this crate's own
    /// steering warns against -- flipping the mode always starts the
    /// engine clean.
    pub fn toggle_editor_mode(&mut self) {
        self.editor_mode = match self.editor_mode {
            EditorMode::Emacs => EditorMode::Vim,
            EditorMode::Vim => EditorMode::Emacs,
        };
        self.vim = crate::tui::input::vim::VimState::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::AgentId;

    #[test]
    fn toggle_editor_mode_flips_emacs_and_vim() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(state.editor_mode, EditorMode::Emacs);

        state.toggle_editor_mode();
        assert_eq!(state.editor_mode, EditorMode::Vim);

        state.toggle_editor_mode();
        assert_eq!(state.editor_mode, EditorMode::Emacs);
    }

    #[test]
    fn toggle_editor_mode_resets_stale_vim_engine_state() {
        let mut state = AppState::new(AgentId::new());
        state.toggle_editor_mode(); // emacs -> vim
        state.input = "hello".to_string();
        state.cursor = 5;
        // Drive the engine into a non-default state (Visual mode) directly
        // through the real key router, the same production entry point a
        // keypress uses -- never a hand-built `VimState` value.
        crate::tui::input::handle_key(
            &mut state,
            ratatui::crossterm::event::KeyEvent::new(
                ratatui::crossterm::event::KeyCode::Esc,
                ratatui::crossterm::event::KeyModifiers::NONE,
            ),
        );
        crate::tui::input::handle_key(
            &mut state,
            ratatui::crossterm::event::KeyEvent::new(
                ratatui::crossterm::event::KeyCode::Char('v'),
                ratatui::crossterm::event::KeyModifiers::NONE,
            ),
        );

        state.toggle_editor_mode(); // vim -> emacs
        state.toggle_editor_mode(); // emacs -> vim again

        assert_eq!(
            state.vim.mode_label(),
            "INSERT",
            "a mode flip must never carry a stale Visual/pending-operator state forward"
        );
    }
}
