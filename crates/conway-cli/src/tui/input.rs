//! Key handling (WI-114): translates a `crossterm::event::KeyEvent` into an
//! [`Action`] the app loop (`app.rs`) carries out. Pure with respect to the
//! input line itself (`AppState::input`/`AppState::cursor` are mutated here
//! directly, since that's local editing state with no async effect), but
//! never calls `SessionHandle`/`Conway` -- every side-effecting action is
//! returned to the caller instead.
//!
//! `AppState::cursor` is a *char* index into `input`, not a byte offset --
//! [`byte_index`] converts to the corresponding byte offset (always on a
//! char boundary, since it walks `char_indices`) right before any `String`
//! mutation, so multi-byte UTF-8 input can never split a character.

use conway::{PermissionDecision, PermissionScope};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::state::{AppState, Mode};

/// What a keypress means for the app loop to carry out.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Nothing to do beyond whatever local `AppState` edit already happened.
    None,
    /// The input line was submitted (`Enter`) -- `SessionHandle::prompt`
    /// text, or (if it starts with `/`) a slash command's raw input. Which
    /// one it is is not this module's concern (module notes: "the dispatch
    /// hook is defined here, handlers land in WI-115") -- the app loop
    /// branches on the leading `/`.
    Submit(String),
    /// A permission prompt was answered.
    PermissionDecision(PermissionDecision),
    /// `Ctrl-C`: cancel the running turn (first press) or exit 130 (second
    /// press within 2s) -- the app loop owns the timing, this just signals
    /// "a Ctrl-C happened".
    CtrlC,
    /// `Ctrl-D` on an empty input line: exit 0.
    Quit,
    ScrollUp,
    ScrollDown,
}

/// Routes a keypress based on `state.mode`, mutating `state.input`/`cursor`
/// directly for plain editing and returning an [`Action`] for anything the
/// app loop must act on.
pub fn handle_key(state: &mut AppState, key: KeyEvent) -> Action {
    match &state.mode {
        Mode::AwaitingPermission(_) => handle_permission_key(key),
        Mode::Normal => handle_normal_key(state, key),
    }
}

fn handle_permission_key(key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            Action::PermissionDecision(PermissionDecision::AllowOnce)
        }
        KeyCode::Char('a') | KeyCode::Char('A') => {
            Action::PermissionDecision(PermissionDecision::AllowAlways {
                scope: PermissionScope::Session,
            })
        }
        KeyCode::Char('n') | KeyCode::Char('N') => {
            Action::PermissionDecision(PermissionDecision::Deny {
                reason: "user denied".to_string(),
            })
        }
        KeyCode::Esc => Action::PermissionDecision(PermissionDecision::DenyWithFeedback {
            message: "user declined; try another approach".to_string(),
        }),
        _ => Action::None,
    }
}

fn handle_normal_key(state: &mut AppState, key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('C') => return Action::CtrlC,
            KeyCode::Char('d') | KeyCode::Char('D') if state.input.is_empty() => {
                return Action::Quit
            }
            KeyCode::Char('w') | KeyCode::Char('W') => {
                delete_word_before_cursor(state);
                return Action::None;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Enter => {
            if state.input.is_empty() {
                return Action::None;
            }
            let text = std::mem::take(&mut state.input);
            state.cursor = 0;
            Action::Submit(text)
        }
        KeyCode::Backspace => {
            if state.cursor > 0 {
                let end = byte_index(&state.input, state.cursor);
                let start = byte_index(&state.input, state.cursor - 1);
                state.input.replace_range(start..end, "");
                state.cursor -= 1;
            }
            Action::None
        }
        KeyCode::Left => {
            state.cursor = state.cursor.saturating_sub(1);
            Action::None
        }
        KeyCode::Right => {
            state.cursor = (state.cursor + 1).min(char_count(&state.input));
            Action::None
        }
        KeyCode::Home => {
            state.cursor = 0;
            Action::None
        }
        KeyCode::End => {
            state.cursor = char_count(&state.input);
            Action::None
        }
        KeyCode::PageUp => Action::ScrollUp,
        KeyCode::PageDown => Action::ScrollDown,
        KeyCode::Char(c) => {
            let idx = byte_index(&state.input, state.cursor);
            state.input.insert(idx, c);
            state.cursor += 1;
            Action::None
        }
        _ => Action::None,
    }
}

fn char_count(s: &str) -> usize {
    s.chars().count()
}

/// The byte offset of the `char_idx`-th char in `s` (always a char
/// boundary), or `s.len()` if `char_idx` is at or past the end.
fn byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

/// Deletes the word immediately before the cursor: trailing whitespace
/// first, then the run of non-whitespace before that (conventional `Ctrl-W`
/// behavior), moving the cursor to the start of what was deleted.
fn delete_word_before_cursor(state: &mut AppState) {
    if state.cursor == 0 {
        return;
    }
    let chars: Vec<char> = state.input.chars().collect();
    let mut start = state.cursor;
    while start > 0 && chars[start - 1].is_whitespace() {
        start -= 1;
    }
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    let start_byte = byte_index(&state.input, start);
    let end_byte = byte_index(&state.input, state.cursor);
    state.input.replace_range(start_byte..end_byte, "");
    state.cursor = start;
}

#[cfg(test)]
mod tests {
    use conway::AgentId;

    use super::*;
    use crate::tui::state::AppState;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn typing_appends_to_input() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Char('h'))),
            Action::None
        );
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Char('i'))),
            Action::None
        );
        assert_eq!(state.input, "hi");
    }

    #[test]
    fn enter_submits_and_clears_input() {
        let mut state = AppState::new(AgentId::new());
        state.input = "hello".to_string();
        let action = handle_key(&mut state, key(KeyCode::Enter));
        assert_eq!(action, Action::Submit("hello".to_string()));
        assert!(state.input.is_empty());
    }

    #[test]
    fn enter_on_empty_input_is_a_noop() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(handle_key(&mut state, key(KeyCode::Enter)), Action::None);
    }

    #[test]
    fn ctrl_d_on_empty_input_quits() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(
            handle_key(&mut state, ctrl_key(KeyCode::Char('d'))),
            Action::Quit
        );
    }

    #[test]
    fn ctrl_d_with_text_present_does_not_quit() {
        let mut state = AppState::new(AgentId::new());
        state.input = "x".to_string();
        assert_eq!(
            handle_key(&mut state, ctrl_key(KeyCode::Char('d'))),
            Action::None
        );
    }

    #[test]
    fn ctrl_c_signals_regardless_of_input() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(
            handle_key(&mut state, ctrl_key(KeyCode::Char('c'))),
            Action::CtrlC
        );
    }

    #[test]
    fn permission_mode_keys_map_to_decisions() {
        // Exercised directly (mode-independent of how `Mode::AwaitingPermission`
        // gets populated -- that wiring is `app.rs`'s job, via `AppState::offer_prompt`).
        assert_eq!(
            handle_permission_key(key(KeyCode::Char('y'))),
            Action::PermissionDecision(PermissionDecision::AllowOnce)
        );
        assert_eq!(
            handle_permission_key(key(KeyCode::Char('a'))),
            Action::PermissionDecision(PermissionDecision::AllowAlways {
                scope: PermissionScope::Session
            })
        );
        assert_eq!(
            handle_permission_key(key(KeyCode::Char('n'))),
            Action::PermissionDecision(PermissionDecision::Deny {
                reason: "user denied".to_string()
            })
        );
        assert_eq!(
            handle_permission_key(key(KeyCode::Esc)),
            Action::PermissionDecision(PermissionDecision::DenyWithFeedback {
                message: "user declined; try another approach".to_string()
            })
        );
    }

    #[test]
    fn handle_key_routes_by_mode() {
        let mut state = AppState::new(AgentId::new());
        assert!(matches!(state.mode, Mode::Normal));
        // In `Normal` mode, `y` is ordinary text input, not a decision.
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Char('y'))),
            Action::None
        );
        assert_eq!(state.input, "y");
    }

    #[test]
    fn left_and_right_move_cursor_clamped_at_bounds() {
        let mut state = AppState::new(AgentId::new());
        state.input = "hi".to_string();
        state.cursor = 2;

        // Clamped at the left edge.
        handle_key(&mut state, key(KeyCode::Left));
        assert_eq!(state.cursor, 1);
        handle_key(&mut state, key(KeyCode::Left));
        assert_eq!(state.cursor, 0);
        handle_key(&mut state, key(KeyCode::Left));
        assert_eq!(state.cursor, 0);

        // Clamped at the right edge.
        handle_key(&mut state, key(KeyCode::Right));
        handle_key(&mut state, key(KeyCode::Right));
        assert_eq!(state.cursor, 2);
        handle_key(&mut state, key(KeyCode::Right));
        assert_eq!(state.cursor, 2);
    }

    #[test]
    fn home_and_end_jump_cursor_to_bounds() {
        let mut state = AppState::new(AgentId::new());
        state.input = "hello".to_string();
        state.cursor = 2;

        handle_key(&mut state, key(KeyCode::Home));
        assert_eq!(state.cursor, 0);

        handle_key(&mut state, key(KeyCode::End));
        assert_eq!(state.cursor, 5);
    }

    #[test]
    fn char_inserts_at_cursor_not_just_appends() {
        let mut state = AppState::new(AgentId::new());
        state.input = "helo".to_string();
        state.cursor = 3; // between 'l' and 'o'

        handle_key(&mut state, key(KeyCode::Char('l')));

        assert_eq!(state.input, "hello");
        assert_eq!(state.cursor, 4);
    }

    #[test]
    fn backspace_deletes_char_before_cursor_mid_string() {
        let mut state = AppState::new(AgentId::new());
        state.input = "hxello".to_string();
        state.cursor = 2; // right after the stray 'x'

        handle_key(&mut state, key(KeyCode::Backspace));

        assert_eq!(state.input, "hello");
        assert_eq!(state.cursor, 1);
    }

    #[test]
    fn backspace_at_start_of_input_is_a_noop() {
        let mut state = AppState::new(AgentId::new());
        state.input = "hi".to_string();
        state.cursor = 0;

        handle_key(&mut state, key(KeyCode::Backspace));

        assert_eq!(state.input, "hi");
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn ctrl_w_deletes_word_before_cursor() {
        let mut state = AppState::new(AgentId::new());
        state.input = "foo bar baz".to_string();
        state.cursor = 11; // end of string

        handle_key(&mut state, ctrl_key(KeyCode::Char('w')));

        assert_eq!(state.input, "foo bar ");
        assert_eq!(state.cursor, 8);
    }

    #[test]
    fn ctrl_w_from_mid_word_deletes_only_that_word_fragment() {
        let mut state = AppState::new(AgentId::new());
        state.input = "foo bar".to_string();
        state.cursor = 6; // between 'a' and 'r' of "bar"

        handle_key(&mut state, ctrl_key(KeyCode::Char('w')));

        assert_eq!(state.input, "foo r");
        assert_eq!(state.cursor, 4);
    }

    #[test]
    fn ctrl_w_skips_trailing_whitespace_before_deleting_word() {
        let mut state = AppState::new(AgentId::new());
        state.input = "foo bar   ".to_string();
        state.cursor = 10; // end of string, after trailing spaces

        handle_key(&mut state, ctrl_key(KeyCode::Char('w')));

        assert_eq!(state.input, "foo ");
        assert_eq!(state.cursor, 4);
    }

    #[test]
    fn cursor_stays_in_bounds_across_multi_byte_utf8_chars() {
        let mut state = AppState::new(AgentId::new());
        // "héllo": 'é' is a 2-byte UTF-8 char but a single `char`/cursor step.
        state.input = "héllo".to_string();
        state.cursor = 5; // end, in char units

        handle_key(&mut state, key(KeyCode::Left));
        handle_key(&mut state, key(KeyCode::Left));
        handle_key(&mut state, key(KeyCode::Left));
        assert_eq!(state.cursor, 2); // just after 'é'

        handle_key(&mut state, key(KeyCode::Backspace));
        assert_eq!(state.input, "hllo");
        assert_eq!(state.cursor, 1);
    }
}
