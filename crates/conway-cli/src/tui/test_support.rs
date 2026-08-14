//! Test-only render/key-driving harness.
//!
//! Every one of the four TUI runtime bugs this harness exists to catch
//! (see this crate's own history around an earlier item-adjacent items) passed its
//! unit tests -- because those tests only ever drove `AppState`/`status_line`
//! directly and never went through the real render pass (`view::draw`) or
//! the real key router (`input::handle_key`). This module gives a test two
//! small, ergonomic seams onto those SAME production functions, so a bug at
//! either seam can be caught the way it actually manifests: on screen, or
//! from a keypress.
//!
//! Deliberately test-only (`#[cfg(test)]`, wired into `tui/mod.rs` the same
//! way): nothing outside `#[cfg(test)]` code depends on it, and neither
//! [`view::draw`] nor [`input::handle_key`] needed any signature or
//! visibility change to be callable from here -- both were already `pub fn`
//! on a `pub mod`, reachable from any test in this crate.

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::Terminal;

use super::input::{self, Action};
use super::state::AppState;
use super::view::{self, Theme};

/// The terminal size [`press_key`] (and any test that doesn't care about a
/// specific size) renders/scrolls against -- large enough that the agent
/// panel/palette overlays this crate's own tests exercise actually fit.
const DEFAULT_SIZE: (u16, u16) = (80, 24);

/// Renders `state` at `width`x`height` through the REAL `view::draw` render
/// pass (the exact function `app.rs`'s run loop calls every frame) into a
/// `ratatui::backend::TestBackend`, and returns the result as one `String`
/// per terminal row (row 0 first), each exactly `width` columns wide
/// (trailing blanks are kept, not trimmed, so column-position assertions
/// stay meaningful). Does not reimplement layout in any way -- every pixel
/// on screen comes from `view::draw` itself.
pub(crate) fn render(state: &AppState, width: u16, height: u16) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("TestBackend construction cannot fail");
    terminal
        .draw(|f| view::draw(state, f, &Theme::default()))
        .expect("drawing into a TestBackend cannot fail");

    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

/// [`render`], joined into one `\n`-separated string -- handy when a test
/// only needs a `contains(..)` check and does not care which row the text
/// landed on.
pub(crate) fn render_text(state: &AppState, width: u16, height: u16) -> String {
    render(state, width, height).join("\n")
}

/// A plain (no-modifier) key event for `code` -- the common case for a test
/// that just wants to press a key.
pub(crate) fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Feeds `event` through the REAL `input::handle_key`, then applies the
/// resulting [`Action`] to `state` the same way `app.rs`'s run loop applies
/// it for every action that needs only `state` + the render area -- no live
/// `SessionHandle`/`Conway` facade call:
///
/// - `ScrollUp`/`ScrollDown`: `view::transcript_area`/`view::max_scroll` at
///   `area`, then `AppState::scroll_page_up`/`scroll_page_down` -- mirrors
///   `App::page_scroll` exactly (same page-height math, same clamp).
/// - `JumpToTop`/`JumpToTail` (T6): the same `max_scroll` lookup, but
///   `AppState::jump_to_top`/`jump_to_tail` (`Home`/`End`) instead of the
///   page-sized pair above -- mirrors `App::jump_to_top`/the direct
///   `state.jump_to_tail()` call in `app.rs`'s action dispatch.
/// - `PermissionDecision`: `AppState::resolve_current_prompt` -- mirrors the
///   run loop's `Action::PermissionDecision` arm exactly.
///
/// `Submit`/`CtrlC`/`Quit`/`FocusAgent`/`AskFate` are NOT applied here:
/// each of those needs a live facade call in `app.rs` (`self.submit`,
/// `self.handle.cancel`, `self.handle.agent_events`,
/// `commands::apply_ask_fate`) that this harness has no session to make.
/// Returning the unapplied `Action` still lets a test assert the router
/// picked the right one (e.g. `assert_eq!(press(..), Action::FocusAgent(id))`)
/// -- the live half of applying it is covered where that facade call lives,
/// not here.
pub(crate) fn press(state: &mut AppState, event: KeyEvent, area: Rect) -> Action {
    let action = input::handle_key(state, event);
    match &action {
        Action::ScrollUp => apply_page_scroll(state, area, true),
        Action::ScrollDown => apply_page_scroll(state, area, false),
        // V3: mirrors `app.rs`'s `line_scroll` -- one line is just a
        // smaller page, sharing the same clamp/follow-tail rules.
        // V2b: installing the grant needs the live facade, which this
        // terminal-free harness does not have. `press` returns the action
        // unapplied, so a test asserts on the ACTION -- the install/persist
        // half is covered where the facade lives.
        Action::GrantPermissionPattern(_, _)
        | Action::CyclePermissionMode
        | Action::RevokePermissionGrants
        | Action::RevokePermissionPattern(_, _)
        | Action::RevokeStructuredAllowRule(_, _, _)
        | Action::RevokeHookRule(_, _) => {}
        Action::ScrollLineUp => apply_line_scroll(state, area, true),
        Action::ScrollLineDown => apply_line_scroll(state, area, false),
        Action::JumpToTop => {
            let max = view::max_scroll(state, area);
            state.jump_to_top(max);
        }
        Action::JumpToTail => state.jump_to_tail(),
        Action::PermissionDecision(decision) => {
            state.resolve_current_prompt(decision.clone());
        }
        Action::None
        | Action::Submit(_)
        | Action::CtrlC
        | Action::Quit
        | Action::FocusAgent(_)
        | Action::AskFate(_)
        | Action::IntentConfirm(_) => {}
    }
    action
}

/// [`press`] for the common case of a plain (no-modifier) `code` at
/// [`DEFAULT_SIZE`] -- most key-driving tests care about the input-line
/// editing/mode logic, not a particular terminal size.
pub(crate) fn press_key(state: &mut AppState, code: KeyCode) -> Action {
    press(
        state,
        key(code),
        Rect::new(0, 0, DEFAULT_SIZE.0, DEFAULT_SIZE.1),
    )
}

/// `App::page_scroll`'s body, verbatim -- factored out so [`press`] cannot
/// silently drift out of sync with what the app loop actually does for a
/// `PageUp`/`PageDown` keypress.
/// V3: the one-line counterpart of [`apply_page_scroll`], mirroring
/// `app.rs`'s `line_scroll` so a `press`-driven test exercises the same
/// state mutation the live loop performs.
fn apply_line_scroll(state: &mut AppState, area: Rect, up: bool) {
    let max = view::max_scroll(state, area);
    if up {
        state.scroll_page_up(1, max);
    } else {
        state.scroll_page_down(1, max);
    }
}

fn apply_page_scroll(state: &mut AppState, area: Rect, page_up: bool) {
    let transcript_area = view::transcript_area(state, area);
    let max = view::max_scroll(state, area);
    let page = transcript_area.height.saturating_sub(1).max(1);
    if page_up {
        state.scroll_page_up(page, max);
    } else {
        state.scroll_page_down(page, max);
    }
}

#[cfg(test)]
mod tests {
    use conway::AgentId;

    use super::*;
    use crate::tui::state::{Entry, NodeStatus, TreeNode};

    /// Self-test 1/2: a known transcript entry's text lands in the rendered
    /// buffer, at the row the transcript pane's top -- proves [`render`] is
    /// really driving `view::draw` (not returning a placeholder) and that
    /// the returned rows are addressable by index.
    #[test]
    fn render_shows_a_known_transcript_entry_at_the_expected_row() {
        let mut state = AppState::new(AgentId::new());
        state.transcript.push(Entry::Assistant {
            text: "hello from the harness".to_string(),
            model: None,
            summary: None,
            ts: None,
        });

        let rows = render(&state, 80, 24);

        assert_eq!(rows.len(), 24, "one row per terminal line");
        assert_eq!(rows[0].len(), 80, "each row is exactly `width` columns");
        assert!(
            rows[0].contains("hello from the harness"),
            "the transcript's only entry must render at the transcript pane's \
             top row (row 0): {:?}",
            rows[0]
        );
        // The rest of the transcript pane is untouched blank space -- this
        // also incidentally proves `render_text`'s join round-trips the
        // same content `render` returns.
        assert!(render_text(&state, 80, 24).contains("hello from the harness"));
    }

    /// Self-test 2/2: a known keypress produces the expected `Action` AND
    /// (for the state-only actions `press` applies) the expected `AppState`
    /// mutation -- proves [`press`] is really driving `input::handle_key`
    /// (not a stub) and faithfully mirrors `app.rs`'s action application.
    #[test]
    fn press_types_a_character_and_reports_the_expected_action() {
        let mut state = AppState::new(AgentId::new());

        let action = press_key(&mut state, KeyCode::Char('h'));

        assert_eq!(action, Action::None);
        assert_eq!(state.input, "h");
    }

    /// Companion case for the same self-test: a live-facade action
    /// (`FocusAgent`) is reported but deliberately left unapplied --
    /// `state.focused_agent` must be untouched, matching this module's own
    /// documented "stops at returning the Action" contract.
    #[test]
    fn press_reports_focus_agent_without_applying_it() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.tree.nodes.push(TreeNode {
            agent_id: child,
            parent: Some(root),
            agent_def: Some("reviewer".to_string()),
            status: NodeStatus::Running,
            kind: None,
            inherited_upto: None,
            ephemeral: false,
        });
        state.agent_view_open = true;
        state.agent_selected = 1;

        let action = press_key(&mut state, KeyCode::Enter);

        assert_eq!(action, Action::FocusAgent(child));
        assert_eq!(
            state.focused_agent, root,
            "FocusAgent needs a live agent_events() call to apply -- press() \
             must not have switched focus itself"
        );
    }

    /// `press` applying `ScrollUp`/`ScrollDown` the same way `app.rs` does,
    /// end to end: enough transcript lines to overflow a small viewport,
    /// `PageUp` disengages auto-follow and moves `scroll` off the bottom.
    #[test]
    fn press_page_up_disengages_follow_and_scrolls_like_the_app_loop() {
        let mut state = AppState::new(AgentId::new());
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        assert!(state.follow_tail);

        let action = press(&mut state, key(KeyCode::PageUp), Rect::new(0, 0, 20, 10));

        assert_eq!(action, Action::ScrollUp);
        assert!(
            !state.follow_tail,
            "PageUp must disengage auto-follow, exactly as App::page_scroll does"
        );
    }
}
