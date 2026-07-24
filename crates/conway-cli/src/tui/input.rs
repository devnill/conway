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

use conway::{AgentId, PermissionDecision, PermissionScope};
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
    /// Board item 01KYASZPVVRCHGTEAN9XS5C6EC: bare arrow `Up`/`Down` (when
    /// neither the palette nor the agent panel is active) scroll ONE line,
    /// not a page -- in alt-screen the terminal reports touchpad/wheel
    /// scroll as arrow keys (WI-127 clean-copy: mouse capture stays off, so
    /// this is how a touchpad swipe actually reaches the app), and routing
    /// those through the page-sized `ScrollUp`/`ScrollDown` made a light
    /// touchpad nudge jump a whole page. `PageUp`/`PageDown` keep emitting
    /// `ScrollUp`/`ScrollDown` unchanged.
    ScrollLineUp,
    ScrollLineDown,
    /// WI-140: switch the transcript pane to `AgentId`'s own conversation.
    /// Emitted by `Enter` on the `/agents` panel's highlighted row (any
    /// row, including the root's own -- focusing the root row is one of
    /// the two documented ways back), and by `Esc` while focused on a
    /// non-root agent (the other way back, per the item's own doc). The
    /// app loop (out of this module's scope) both mutates
    /// `AppState::focus_agent` and re-subscribes `handle.agent_events`.
    FocusAgent(AgentId),
}

/// Routes a keypress based on `state.mode`, mutating `state.input`/`cursor`
/// directly for plain editing and returning an [`Action`] for anything the
/// app loop must act on.
pub fn handle_key(state: &mut AppState, key: KeyEvent) -> Action {
    match &state.mode {
        Mode::AwaitingPermission(_) => handle_permission_key(state, key),
        Mode::Normal => handle_normal_key(state, key),
    }
}

/// The permission overlay's own command-body scroll step (bug fix,
/// 01KYB0F7V65QAMZWWYH8K7DWDC): `PageUp`/`PageDown` doesn't collide with the
/// `y`/`a`/`n`/`Esc` decision keys below, so it's free to drive
/// `AppState::permission_scroll` without touching decision handling at all.
/// A generous fixed step (rather than deriving one from the actual overlay
/// viewport height, which this module has no `Rect` for) -- `view/mod.rs`'s
/// `draw_permission_overlay` clamps the value it's actually handed to the
/// command's own wrapped line count, so an over-large step here can never
/// scroll past real content; it just lands on the true bottom/top.
const PERMISSION_SCROLL_STEP: u16 = 5;

fn handle_permission_key(state: &mut AppState, key: KeyEvent) -> Action {
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
        // Bug fix (01KYB0F7V65QAMZWWYH8K7DWDC): a long command's argument
        // used to clip the decision keys off-screen with no way to see the
        // rest of it. `PageUp`/`PageDown` page the overlay's own command
        // body while the decision keys keep working exactly as above --
        // mutated directly here (like `handle_normal_key`'s plain-editing
        // keys) rather than via a new `Action` variant, since nothing here
        // needs a live facade call.
        KeyCode::PageDown => {
            state.permission_scroll = state
                .permission_scroll
                .saturating_add(PERMISSION_SCROLL_STEP);
            Action::None
        }
        KeyCode::PageUp => {
            state.permission_scroll = state
                .permission_scroll
                .saturating_sub(PERMISSION_SCROLL_STEP);
            Action::None
        }
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
                state.sync_palette_stem();
                return Action::None;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Enter => {
            if state.input.is_empty() {
                // WI-140: with the agent panel open and nothing typed,
                // Enter focuses the highlighted row's agent instead of
                // being a pure no-op (its only prior behavior) -- the
                // input line has nothing else to submit in this state, so
                // this cannot collide with a real prompt/command.
                if state.agent_view_open {
                    if let Some(node) = state.tree.nodes.get(
                        state
                            .agent_selected
                            .min(state.tree.nodes.len().saturating_sub(1)),
                    ) {
                        return Action::FocusAgent(node.agent_id);
                    }
                }
                return Action::None;
            }
            let text = std::mem::take(&mut state.input);
            state.cursor = 0;
            state.clear_palette();
            Action::Submit(text)
        }
        KeyCode::Backspace => {
            if state.cursor > 0 {
                let end = byte_index(&state.input, state.cursor);
                let start = byte_index(&state.input, state.cursor - 1);
                state.input.replace_range(start..end, "");
                state.cursor -= 1;
                state.sync_palette_stem();
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
        // WI-130/bug 3 (01KYAN9XZ6E22NSQ3GS3726XW6): arrows drive the
        // on-demand surfaces. The slash-command palette takes priority when
        // it is showing (the user is composing a command); otherwise the
        // arrows scroll the agent panel when it is open; otherwise -- the
        // lowest-priority, most common case -- they scroll the transcript
        // ONE LINE at a time (01KYASZPVVRCHGTEAN9XS5C6EC; `PageUp`/`PageDown`,
        // below, are the full-page binding). Mouse wheel stays disabled by
        // design (WI-127 clean-copy); this is a keyboard-only fix.
        KeyCode::Up => {
            if palette_navigate(state, -1) {
                Action::None
            } else if state.agent_view_open {
                state.agent_scroll(-1);
                Action::None
            } else {
                Action::ScrollLineUp
            }
        }
        KeyCode::Down => {
            if palette_navigate(state, 1) {
                Action::None
            } else if state.agent_view_open {
                state.agent_scroll(1);
                Action::None
            } else {
                Action::ScrollLineDown
            }
        }
        KeyCode::Esc => {
            // WI-130: Esc closes the agent panel when it is open.
            if state.agent_view_open {
                state.agent_view_open = false;
            }
            // WI-140: Esc is also the other documented way back to the
            // root's own conversation (alongside focusing the root row via
            // Enter, above) -- whether or not the panel was open, so it
            // still works once the panel has already been dismissed. A
            // no-op (no `FocusAgent` emitted) when already on the root, so
            // Esc does not force an unnecessary transcript clear+replay.
            if !state.is_root_focused() {
                return Action::FocusAgent(state.root_agent());
            }
            Action::None
        }
        KeyCode::PageUp => Action::ScrollUp,
        KeyCode::PageDown => Action::ScrollDown,
        KeyCode::Char(c) => {
            let idx = byte_index(&state.input, state.cursor);
            state.input.insert(idx, c);
            state.cursor += 1;
            state.sync_palette_stem();
            Action::None
        }
        _ => Action::None,
    }
}

/// Moves the slash-command palette selection by `delta` and autofills
/// `input` with the newly-highlighted command (WI-130). Returns whether the
/// palette was active and thus consumed the key.
///
/// The candidate list is anchored to [`AppState::palette_source`] (the stem
/// the user typed), NOT to the live `input` -- so autofilling `input` with a
/// whole command on each arrow press does not shrink the list to that one
/// entry; cycling stays over the full set the stem matched. A `None`
/// selection means "not navigating yet": the first `Down` lands on the first
/// match, the first `Up` on the last; further presses wrap.
fn palette_navigate(state: &mut AppState, delta: isize) -> bool {
    let candidates = super::view::palette::matches(state.palette_source());
    let len = candidates.len();
    if len == 0 {
        return false;
    }
    let next = match state.palette_selected {
        None => {
            if delta > 0 {
                0
            } else {
                len - 1
            }
        }
        Some(i) => (i.min(len - 1) as isize + delta).rem_euclid(len as isize) as usize,
    };
    state.palette_selected = Some(next);
    state.input = candidates[next].name.to_string();
    state.cursor = char_count(&state.input);
    true
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
    use ratatui::layout::Rect;

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

    // ---- WI-130: palette arrow navigation + agent-panel scroll ----

    fn type_str(state: &mut AppState, s: &str) {
        for c in s.chars() {
            handle_key(state, key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn palette_down_autofills_successive_matches_without_collapsing() {
        let mut state = AppState::new(AgentId::new());
        type_str(&mut state, "/a"); // matches [/ask, /agents]
                                    // First Down lands on the first match and autofills it.
        handle_key(&mut state, key(KeyCode::Down));
        assert_eq!(state.input, "/ask");
        assert_eq!(state.palette_selected, Some(0));
        // Second Down advances even though `input` is now a full command --
        // the list stayed anchored to the "/a" stem, so it did not collapse.
        handle_key(&mut state, key(KeyCode::Down));
        assert_eq!(state.input, "/agents");
        assert_eq!(state.palette_selected, Some(1));
        // Wraps back to the top.
        handle_key(&mut state, key(KeyCode::Down));
        assert_eq!(state.input, "/ask");
    }

    #[test]
    fn palette_up_from_no_selection_lands_on_the_last_match() {
        let mut state = AppState::new(AgentId::new());
        type_str(&mut state, "/a");
        handle_key(&mut state, key(KeyCode::Up));
        assert_eq!(state.input, "/agents");
        assert_eq!(state.palette_selected, Some(1));
    }

    #[test]
    fn typing_resets_the_palette_highlight() {
        let mut state = AppState::new(AgentId::new());
        type_str(&mut state, "/");
        handle_key(&mut state, key(KeyCode::Down));
        assert!(state.palette_selected.is_some());
        handle_key(&mut state, key(KeyCode::Char('h')));
        assert_eq!(state.palette_selected, None);
    }

    #[test]
    fn esc_closes_the_agent_panel() {
        let mut state = AppState::new(AgentId::new());
        state.agent_view_open = true;
        handle_key(&mut state, key(KeyCode::Esc));
        assert!(!state.agent_view_open);
    }

    // ---- bug 3 (01KYAN9XZ6E22NSQ3GS3726XW6): arrow keys scroll the
    // transcript when neither the palette nor the agent panel is active,
    // preserving priority over both when they are. ----

    // The small viewport `test_support`'s own PageUp test uses to force the
    // 30-line transcript below to overflow -- mirrored here so the Up/Down
    // tests exercise the same overflow condition (at the default 80x24
    // size, 30 short lines fit with no scrolling at all, which would make
    // `scroll > 0` assertions vacuous).
    fn small_viewport() -> Rect {
        Rect::new(0, 0, 20, 10)
    }

    #[test]
    fn up_scrolls_the_transcript_when_neither_palette_nor_panel_is_active() {
        use crate::tui::state::Entry;
        use crate::tui::test_support::press;

        let mut state = AppState::new(AgentId::new());
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
            });
        }
        assert!(state.follow_tail);

        let action = press(&mut state, key(KeyCode::Up), small_viewport());

        assert_eq!(action, Action::ScrollLineUp);
        assert!(
            state.scroll > 0,
            "Up must move `scroll` off the bottom, exactly like PageUp"
        );
        assert!(
            !state.follow_tail,
            "Up must disengage auto-follow, exactly like PageUp"
        );
    }

    #[test]
    fn down_scrolls_back_toward_the_bottom_and_reengages_follow() {
        use crate::tui::state::Entry;
        use crate::tui::test_support::press;

        let mut state = AppState::new(AgentId::new());
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
            });
        }
        // Scroll up first (as the previous test does), so there is
        // somewhere for Down to scroll back down to.
        assert_eq!(
            press(&mut state, key(KeyCode::Up), small_viewport()),
            Action::ScrollLineUp
        );
        assert!(!state.follow_tail);
        let scrolled_up_to = state.scroll;

        let action = press(&mut state, key(KeyCode::Down), small_viewport());

        assert_eq!(action, Action::ScrollLineDown);
        assert!(
            state.scroll > scrolled_up_to,
            "Down must move `scroll` back toward the bottom"
        );
        assert!(
            state.follow_tail,
            "Down must re-engage auto-follow once it reaches the bottom, \
             exactly like PageDown"
        );
    }

    // ---- 01KYASZPVVRCHGTEAN9XS5C6EC: arrows scroll ONE line, PageUp/
    // PageDown keep the full-page step ----

    #[test]
    fn up_moves_scroll_by_exactly_one_line_not_a_page() {
        use crate::tui::state::Entry;
        use crate::tui::test_support::press;

        let mut state = AppState::new(AgentId::new());
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
            });
        }
        // First Up disengages `follow_tail` and establishes a real `scroll`
        // baseline (while following, `scroll` itself is stale -- see
        // `scroll_page_up`'s own doc on why it reads from `max_scroll`
        // instead).
        press(&mut state, key(KeyCode::Up), small_viewport());
        let before = state.scroll;

        let action = press(&mut state, key(KeyCode::Up), small_viewport());

        assert_eq!(action, Action::ScrollLineUp);
        assert_eq!(
            state.scroll,
            before - 1,
            "Up must move `scroll` by exactly one line, not a page"
        );
    }

    #[test]
    fn down_moves_scroll_by_exactly_one_line_not_a_page() {
        use crate::tui::state::Entry;
        use crate::tui::test_support::press;

        let mut state = AppState::new(AgentId::new());
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
            });
        }
        // Scroll up several lines first, so there is room for Down to move
        // without hitting the bottom clamp (which would re-engage
        // `follow_tail` and make the step assertion vacuous).
        for _ in 0..5 {
            press(&mut state, key(KeyCode::Up), small_viewport());
        }
        let before = state.scroll;

        let action = press(&mut state, key(KeyCode::Down), small_viewport());

        assert_eq!(action, Action::ScrollLineDown);
        assert_eq!(
            state.scroll,
            before + 1,
            "Down must move `scroll` by exactly one line, not a page"
        );
    }

    #[test]
    fn page_up_and_page_down_still_move_a_full_page_unlike_the_arrows() {
        use crate::tui::state::Entry;
        use crate::tui::test_support::press;

        let mut state = AppState::new(AgentId::new());
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
            });
        }
        // First PageUp disengages `follow_tail` and establishes a real
        // `scroll` baseline, same reasoning as the arrow test above.
        press(&mut state, key(KeyCode::PageUp), small_viewport());
        let before = state.scroll;

        let up_action = press(&mut state, key(KeyCode::PageUp), small_viewport());
        let after_page_up = state.scroll;

        assert_eq!(up_action, Action::ScrollUp);
        let page_step = before - after_page_up;
        assert!(
            page_step > 1,
            "PageUp's step ({page_step}) must remain a full page, not the \
             one-line arrow step"
        );

        let down_action = press(&mut state, key(KeyCode::PageDown), small_viewport());

        assert_eq!(down_action, Action::ScrollDown);
        assert_eq!(
            state.scroll,
            after_page_up + page_step,
            "PageDown's step must be the same full-page size as PageUp's"
        );
    }

    #[test]
    fn agent_panel_open_keeps_up_down_driving_panel_nav_not_scroll() {
        use crate::tui::state::{Entry, NodeStatus, TreeNode};
        use crate::tui::test_support::press_key;

        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.tree.nodes.push(TreeNode {
            agent_id: child,
            parent: Some(root),
            agent_def: Some("reviewer".to_string()),
            status: NodeStatus::Running,
        });
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
            });
        }
        state.agent_view_open = true;
        assert_eq!(state.agent_selected, 0);

        let action = press_key(&mut state, KeyCode::Down);

        assert_eq!(
            action,
            Action::None,
            "the agent panel must consume Down, not the transcript scroll"
        );
        assert_eq!(state.agent_selected, 1, "panel selection must have moved");
        assert!(
            state.follow_tail,
            "the transcript must be untouched while the panel owns the key"
        );

        let action = press_key(&mut state, KeyCode::Up);

        assert_eq!(action, Action::None);
        assert_eq!(state.agent_selected, 0);
        assert!(state.follow_tail);
    }

    #[test]
    fn palette_composing_keeps_up_down_driving_the_palette_not_scroll() {
        use crate::tui::state::Entry;
        use crate::tui::test_support::press_key;

        let mut state = AppState::new(AgentId::new());
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
            });
        }
        type_str(&mut state, "/a"); // matches [/ask, /agents]; palette active

        let action = press_key(&mut state, KeyCode::Down);

        assert_eq!(
            action,
            Action::None,
            "the palette must consume Down, not the transcript scroll"
        );
        assert_eq!(state.input, "/ask");
        assert!(
            state.follow_tail,
            "the transcript must be untouched while the palette owns the key"
        );

        let action = press_key(&mut state, KeyCode::Up);

        assert_eq!(action, Action::None);
        assert!(state.follow_tail);
    }

    // ---- WI-140: focused-agent switch ----

    #[test]
    fn enter_on_an_empty_input_with_the_agent_panel_open_focuses_the_highlighted_row() {
        use crate::tui::state::{NodeStatus, TreeNode};

        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.tree.nodes.push(TreeNode {
            agent_id: child,
            parent: Some(root),
            agent_def: Some("reviewer".to_string()),
            status: NodeStatus::Running,
        });
        state.agent_view_open = true;
        state.agent_selected = 1; // the child row

        let action = handle_key(&mut state, key(KeyCode::Enter));

        assert_eq!(action, Action::FocusAgent(child));
    }

    #[test]
    fn enter_on_an_empty_input_with_the_panel_closed_is_still_a_noop() {
        let mut state = AppState::new(AgentId::new());
        assert!(!state.agent_view_open);
        assert_eq!(handle_key(&mut state, key(KeyCode::Enter)), Action::None);
    }

    #[test]
    fn esc_while_focused_off_root_returns_focus_to_root() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.focus_agent(child);

        let action = handle_key(&mut state, key(KeyCode::Esc));

        assert_eq!(action, Action::FocusAgent(root));
    }

    #[test]
    fn esc_while_already_on_root_does_not_emit_a_focus_switch() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.is_root_focused());

        let action = handle_key(&mut state, key(KeyCode::Esc));

        assert_eq!(action, Action::None);
    }

    #[test]
    fn esc_closes_the_panel_and_refocuses_root_together() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.focus_agent(child);
        state.agent_view_open = true;

        let action = handle_key(&mut state, key(KeyCode::Esc));

        assert!(!state.agent_view_open);
        assert_eq!(action, Action::FocusAgent(root));
    }

    #[test]
    fn palette_arrows_take_priority_over_the_agent_panel() {
        let mut state = AppState::new(AgentId::new());
        state.agent_view_open = true;
        type_str(&mut state, "/a"); // palette active
        handle_key(&mut state, key(KeyCode::Down));
        // The palette consumed the key: input autofilled, panel selection
        // untouched.
        assert_eq!(state.input, "/ask");
        assert_eq!(state.agent_selected, 0);
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
        let mut state = AppState::new(AgentId::new());
        assert_eq!(
            handle_permission_key(&mut state, key(KeyCode::Char('y'))),
            Action::PermissionDecision(PermissionDecision::AllowOnce)
        );
        assert_eq!(
            handle_permission_key(&mut state, key(KeyCode::Char('a'))),
            Action::PermissionDecision(PermissionDecision::AllowAlways {
                scope: PermissionScope::Session
            })
        );
        assert_eq!(
            handle_permission_key(&mut state, key(KeyCode::Char('n'))),
            Action::PermissionDecision(PermissionDecision::Deny {
                reason: "user denied".to_string()
            })
        );
        assert_eq!(
            handle_permission_key(&mut state, key(KeyCode::Esc)),
            Action::PermissionDecision(PermissionDecision::DenyWithFeedback {
                message: "user declined; try another approach".to_string()
            })
        );
    }

    /// Bug fix companion: `PageDown`/`PageUp` while awaiting a permission
    /// decision must page `AppState::permission_scroll` (for a long
    /// command's overlay, 01KYB0F7V65QAMZWWYH8K7DWDC) instead of falling
    /// through to `Action::None` doing nothing, and must never collide with
    /// the `y`/`a`/`n`/`Esc` decision keys tested above.
    #[test]
    fn permission_mode_page_keys_scroll_instead_of_deciding() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(state.permission_scroll, 0);

        assert_eq!(
            handle_permission_key(&mut state, key(KeyCode::PageDown)),
            Action::None
        );
        assert!(
            state.permission_scroll > 0,
            "PageDown must advance the overlay's own scroll offset"
        );

        let after_down = state.permission_scroll;
        assert_eq!(
            handle_permission_key(&mut state, key(KeyCode::PageUp)),
            Action::None
        );
        assert!(
            state.permission_scroll < after_down,
            "PageUp must step the scroll offset back down"
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
