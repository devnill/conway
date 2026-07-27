//! The TUI's render pass (WI-114; redesigned single-column layout WI-127):
//! a pure function from `&AppState` to a `ratatui::Frame` -- no `AppState`
//! mutation, no I/O, so it can run under a `ratatui::backend::TestBackend`
//! with no real terminal.
//!
//! WI-127 replaced the old always-on two-pane layout (a left agent-tree
//! pane alongside right transcript/input columns) with a single column:
//! conversation stream on top, an optional on-demand agent panel, an input
//! box, and a bottom status line (criterion 1). See `transcript.rs`'s doc
//! for the clean-copy guarantee (criterion 2), `palette.rs` for the
//! live-filtering command palette (criterion 3), and
//! `agents.rs`/`transcript.rs`'s `Entry::Agent` handling for the
//! agent-tree/subagent-activity criterion (criterion 4).
//!
//! Submodules are a directory (not one flat file) so each concern --
//! transcript, input box, status line, palette, agent panel -- stays a
//! small, independently testable pure-rendering unit (module notes' own
//! ask: "keep rendering functions small and testable").

// `pub(crate)` for item A3: `tui::commands`'s `/tree` snapshot renderer
// reuses `agents::recipe_parts`/`agents::ancestor_depth` so the hidden
// alias can never drift from what the panel draws.
pub(crate) mod agents;
mod input_box;
pub mod palette;
mod status;
pub mod theme;
mod transcript;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::state::{AppState, AskModal, IntentConfirm, Mode};
pub use theme::Theme;

const INPUT_HEIGHT: u16 = 3;
const STATUS_HEIGHT: u16 = 1;
const AGENT_PANEL_HEIGHT: u16 = 8;

pub fn draw(state: &AppState, frame: &mut Frame, theme: &Theme) {
    let area = frame.area();
    let areas = layout(state, area);

    transcript::draw(frame, areas.transcript, state, theme);

    if let Some(agents_area) = areas.agents {
        agents::draw(frame, agents_area, state, theme);
    }

    input_box::draw(frame, areas.input, state, theme);
    status::draw(frame, areas.status, state, theme);

    if state.palette_source().starts_with('/') {
        palette::draw_overlay(
            frame,
            areas.input,
            state.palette_source(),
            state.palette_selected,
        );
    }

    if let Mode::AwaitingPermission(pending) = &state.mode {
        draw_permission_overlay(
            frame,
            areas.transcript,
            &pending.request,
            state.permission_scroll,
            theme,
        );
    }

    if let Mode::AskModal(modal) = &state.mode {
        draw_ask_modal(frame, areas.transcript, modal, theme);
    }

    if let Mode::IntentConfirm(card) = &state.mode {
        draw_intent_confirm(frame, areas.transcript, card, theme);
    }
}

/// The frame's row split -- exactly what [`draw`] renders into, factored
/// out so `app.rs` can find the transcript viewport's width/height (for the
/// scroll-clamp math in [`max_scroll`]) without re-deriving this same
/// `Constraint`/`Layout` sequence a second time and risking it drifting out
/// of sync with what is actually on screen.
struct Areas {
    transcript: Rect,
    agents: Option<Rect>,
    input: Rect,
    status: Rect,
}

fn layout(state: &AppState, area: Rect) -> Areas {
    // B5: while the /ask modal owns the screen, the /agents panel is NOT
    // visible (user decision, binding) even if it was open when the modal
    // appeared -- `state.agent_view_open` itself is left untouched, so the
    // panel comes back exactly as it was once a fate closes the modal.
    let show_agents = state.agent_view_open
        && !matches!(state.mode, Mode::AskModal(_) | Mode::IntentConfirm(_))
        && area.height > INPUT_HEIGHT + STATUS_HEIGHT + 3;

    let mut constraints = vec![Constraint::Min(0)];
    if show_agents {
        constraints.push(Constraint::Length(AGENT_PANEL_HEIGHT.min(area.height / 3)));
    }
    constraints.push(Constraint::Length(INPUT_HEIGHT));
    constraints.push(Constraint::Length(STATUS_HEIGHT));

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let mut next = 0;
    let transcript = rows[next];
    next += 1;
    let agents = if show_agents {
        let a = rows[next];
        next += 1;
        Some(a)
    } else {
        None
    };
    let input = rows[next];
    next += 1;
    let status = rows[next];

    Areas {
        transcript,
        agents,
        input,
        status,
    }
}

/// The transcript viewport's on-screen `Rect` for `state`/`area`, exactly as
/// [`draw`] computes it -- `app.rs` needs this (via [`max_scroll`]) to turn
/// a `PageUp`/`PageDown` keypress into a wrapped-line clamp outside of any
/// actual render pass.
pub(crate) fn transcript_area(state: &AppState, area: Rect) -> Rect {
    layout(state, area).transcript
}

/// The scroll-clamp ceiling (total wrapped transcript lines minus the
/// transcript viewport's height) for `state` at `area`'s current size --
/// `app.rs`'s `PageUp`/`PageDown` handling passes this straight into
/// `AppState::scroll_page_up`/`scroll_page_down`. Delegates the actual line
/// count to `transcript::wrapped_line_count`, which wraps with the SAME
/// `Paragraph`/`Wrap` parameters `transcript::draw` renders with, so this
/// can never disagree with what ends up on screen.
pub(crate) fn max_scroll(state: &AppState, area: Rect) -> u16 {
    let transcript = transcript_area(state, area);
    let total = transcript::wrapped_line_count(state, transcript.width);
    total
        .saturating_sub(transcript.height as usize)
        .min(u16::MAX as usize) as u16
}

/// Rows the permission overlay's footer ALWAYS reserves, regardless of how
/// long the command being displayed is: the tool/category line, the agent
/// path line, and the `[y]/[a]/[n]/[Esc]` decision-key hint. This is the
/// load-bearing invariant behind [`draw_permission_overlay`]'s whole
/// rework -- see that function's own doc.
const PERMISSION_FOOTER_ROWS: u16 = 3;

/// The permission prompt: a bordered block over the bottom of the
/// transcript area, unmistakably distinct from ordinary transcript output
/// (module notes; also this item's human criterion). A `Block`/border here
/// is fine -- it is a modal overlay, never part of the copyable
/// conversation (it replaces transcript content on screen only while a
/// decision is pending, via `Clear`).
///
/// Bug fix (01KYB0F7V65QAMZWWYH8K7DWDC): this used to be a fixed ~6-row box
/// with the ENTIRE `req.rendered` command as line 0 of one unscrolled
/// `Paragraph` -- a long tool-call argument overflowed the box and clipped
/// the tool/category line, the agent path, and the `[y]/[a]/[n]/[Esc]`
/// decision-key hint off-screen, so the user could see neither the full
/// command nor how to answer the prompt. Reworked so:
/// - The overlay claims a much larger share of the transcript area (nearly
///   all of it, `transcript_area`-height permitting) instead of a fixed
///   handful of rows, so a long command has real room before scrolling is
///   even needed.
/// - The block's interior is split into a scrollable command body (grows
///   to fill whatever's left) and a FIXED-height footer
///   ([`PERMISSION_FOOTER_ROWS`]) below it holding the tool/category line,
///   the agent path, and the decision-key hint -- rows the command
///   `Paragraph` can never grow into, however long `req.rendered` is or
///   however far it's scrolled. This is what keeps the hint on screen,
///   not the command's own wrapping.
/// - `scroll` (`AppState::permission_scroll`, paged by `PageUp`/`PageDown`
///   while `Mode::AwaitingPermission` -- see `input.rs::handle_permission_key`)
///   drives the command body's `Paragraph::scroll`, clamped here to the
///   command's own wrapped line count so an over-large value (this
///   function's only real validation of it) just lands on the true bottom,
///   never past real content.
/// - Review fix: on a small terminal the footer itself can be forced below
///   [`PERMISSION_FOOTER_ROWS`] (border rows alone can eat most of a tiny
///   transcript area) -- the decision-key hint is ordered FIRST within the
///   footer specifically so it is the LAST thing to get clipped, not the
///   first, as the footer shrinks. See the footer-line construction below
///   for the full reasoning.
fn draw_permission_overlay(
    frame: &mut Frame,
    transcript_area: Rect,
    req: &conway::PermissionRequest,
    scroll: u16,
    theme: &Theme,
) {
    // At minimum: 2 border rows + the pinned footer + one row of command.
    let min_height = (2 + PERMISSION_FOOTER_ROWS + 1).min(transcript_area.height);
    // Claim nearly the whole transcript area (a "larger fraction of the
    // screen" per this item's spec) rather than a fixed handful of rows,
    // leaving just a sliver of ordinary transcript visible above it.
    let height = transcript_area
        .height
        .saturating_sub(1)
        .max(min_height)
        .min(transcript_area.height);
    let area = Rect {
        x: transcript_area.x,
        y: transcript_area.y + transcript_area.height.saturating_sub(height),
        width: transcript_area.width,
        height,
    };

    let agent_path = if req.agent_path.is_empty() {
        "root".to_string()
    } else {
        req.agent_path
            .iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(" -> ")
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" PERMISSION REQUIRED ")
        .border_style(theme.border_danger);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    // Reserve the footer's rows FIRST -- up to `PERMISSION_FOOTER_ROWS`, but
    // never more than the block's interior actually has. The command BODY
    // (below) is what shrinks on a tight viewport -- all the way to zero --
    // never the footer: `Constraint::Length(footer_rows)` is satisfied in
    // full before `Constraint::Min(0)` gets whatever is left over, so a
    // small overlay squeezes the command out long before it can squeeze the
    // footer.
    let footer_rows = PERMISSION_FOOTER_ROWS.min(inner.height);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(footer_rows)])
        .split(inner);
    let body_area = rows[0];
    let footer_area = rows[1];

    let body = Paragraph::new(Line::from(Span::styled(
        req.rendered.clone(),
        theme.emphasized,
    )))
    .wrap(Wrap { trim: false });
    let body_max_scroll = body
        .line_count(body_area.width)
        .saturating_sub(body_area.height as usize)
        .min(u16::MAX as usize) as u16;
    let clamped_scroll = scroll.min(body_max_scroll);
    frame.render_widget(body.scroll((clamped_scroll, 0)), body_area);

    // Review fix (01KYB0F7V65QAMZWWYH8K7DWDC): even with the footer's rows
    // reserved first (above), `footer_area` can still end up shorter than
    // `PERMISSION_FOOTER_ROWS` on a genuinely tiny viewport -- the block's
    // own 2 border rows alone can eat most of a small transcript area, with
    // nothing left to reserve. A `Paragraph` clips top-down, so whichever
    // line is FIRST survives longest as the footer shrinks. The decision-key
    // hint is what the user actually needs to act on the prompt -- it goes
    // FIRST here, ahead of the purely informational tool/category and
    // agent-path lines, so even a 1-row footer still shows it. (Previously
    // the hint was last and was the FIRST thing clipped -- exactly
    // backwards.)
    let hint = if body_max_scroll > 0 {
        "[y] allow once  [a] allow always  [n] deny  [Esc] deny with feedback  \
         [PageUp/PageDown] scroll command"
    } else {
        "[y] allow once  [a] allow always  [n] deny  [Esc] deny with feedback"
    };
    let footer_lines = vec![
        Line::from(hint),
        Line::from(format!("tool: {}  category: {:?}", req.tool, req.category)),
        Line::from(format!("agent path: {agent_path}")),
    ];
    let footer = Paragraph::new(footer_lines).wrap(Wrap { trim: true });
    frame.render_widget(footer, footer_area);
}

/// Rows the /ask modal's footer ALWAYS reserves (B5): the fate-key hint,
/// plus one line for the in-modal error shown after a failed fate (blank
/// when there is none, so the hint never jumps vertically when an error
/// appears).
const ASK_MODAL_FOOTER_ROWS: u16 = 2;

/// The `/ask` single-turn modal (B5): a bordered block over the bottom of
/// the transcript area, following [`draw_permission_overlay`]'s precedent
/// (a modal overlay replacing transcript content only while a decision is
/// pending, via `Clear` -- never part of the copyable conversation).
/// Unlike the permission overlay there is no scrolling: the modal's whole
/// point is the forced choice in the footer, so the answer renders from
/// its top and clips on a small viewport (the full answer always remains
/// reachable afterward via whichever fate the user picks -- pull-in merges
/// it into the parent's transcript; fork keeps the session).
///
/// The footer shows the three fate keys -- `[p] pull in · [f] fork ·
/// [esc] discard` -- and, after a FAILED fate, the error that kept the
/// modal open (red). The hint is ordered FIRST within the footer for the
/// same small-viewport reason the permission overlay's doc explains: a
/// `Paragraph` clips top-down, so the line the user needs to act on is the
/// last thing clipped.
fn draw_ask_modal(frame: &mut Frame, transcript_area: Rect, modal: &AskModal, theme: &Theme) {
    // At minimum: 2 border rows + the pinned footer + one row of body.
    let min_height = (2 + ASK_MODAL_FOOTER_ROWS + 1).min(transcript_area.height);
    let height = transcript_area
        .height
        .saturating_sub(1)
        .max(min_height)
        .min(transcript_area.height);
    let area = Rect {
        x: transcript_area.x,
        y: transcript_area.y + transcript_area.height.saturating_sub(height),
        width: transcript_area.width,
        height,
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" ASK ")
        .border_style(theme.border_warning);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    // Reserve the footer's rows FIRST (same invariant the permission
    // overlay documents): the body shrinks on a tight viewport, never the
    // footer.
    let footer_rows = ASK_MODAL_FOOTER_ROWS.min(inner.height);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(footer_rows)])
        .split(inner);
    let body_area = rows[0];
    let footer_area = rows[1];

    let mut body_lines = vec![
        Line::from(Span::styled(
            format!("you asked: {}", modal.question),
            theme.emphasized,
        )),
        Line::from(""),
    ];
    body_lines.extend(
        modal
            .answer
            .split('\n')
            .map(|line| Line::from(line.to_string())),
    );
    let body = Paragraph::new(body_lines).wrap(Wrap { trim: false });
    frame.render_widget(body, body_area);

    let error_line = match &modal.error {
        Some(err) => Line::from(Span::styled(
            format!("error: {err}"),
            theme.error,
        )),
        None => Line::from(""),
    };
    let footer_lines = vec![
        Line::from("[p] pull in  [f] fork  [esc] discard"),
        error_line,
    ];
    let footer = Paragraph::new(footer_lines).wrap(Wrap { trim: true });
    frame.render_widget(footer, footer_area);
}

/// Rows the intent confirmation card's footer ALWAYS reserves (C2): the
/// choice-key hint -- `[enter] confirm  [e] edit  [esc] manual` -- plus a
/// blank line reserved for symmetry with [`ASK_MODAL_FOOTER_ROWS`] (the
/// card has no in-modal error state: a failed confirm/manual re-enters
/// `commands::execute`, which pushes the failure as a transcript `Notice`
/// and returns `Effect::None`, so the card closes on the failure rather
/// than staying open the way the `/ask` modal does).
const INTENT_CONFIRM_FOOTER_ROWS: u16 = 2;

/// The NL intent confirmation card (C2): a bordered block over the bottom of
/// the transcript area, following [`draw_ask_modal`]'s overlay precedent (a
/// modal overlay replacing transcript content only while a decision is
/// pending, via `Clear` -- never part of the copyable conversation). The
/// card shows the classified `recipe` (`fork`/`spawn`), the `agent_def`
/// (or `(inherit)` when `None`), and the `prompt` the classifier produced
/// (or the user's raw text on the verbatim-passthrough path), then forces
/// exactly one choice via the footer: `[enter] confirm  [e] edit  [esc]
/// manual`. The hint is ordered FIRST within the footer for the same
/// small-viewport reason the `/ask` modal's doc explains: a `Paragraph`
/// clips top-down, so the line the user needs to act on is the last thing
/// clipped.
fn draw_intent_confirm(
    frame: &mut Frame,
    transcript_area: Rect,
    card: &IntentConfirm,
    theme: &Theme,
) {
    // At minimum: 2 border rows + the pinned footer + one row of body.
    let min_height = (2 + INTENT_CONFIRM_FOOTER_ROWS + 1).min(transcript_area.height);
    let height = transcript_area
        .height
        .saturating_sub(1)
        .max(min_height)
        .min(transcript_area.height);
    let area = Rect {
        x: transcript_area.x,
        y: transcript_area.y + transcript_area.height.saturating_sub(height),
        width: transcript_area.width,
        height,
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" INTENT ")
        .border_style(theme.border_accent);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    // Reserve the footer's rows FIRST (same invariant the /ask modal
    // documents): the body shrinks on a tight viewport, never the footer.
    let footer_rows = INTENT_CONFIRM_FOOTER_ROWS.min(inner.height);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(footer_rows)])
        .split(inner);
    let body_area = rows[0];
    let footer_area = rows[1];

    let recipe = match card.intent.recipe {
        conway::SubagentMode::Fork => "fork",
        conway::SubagentMode::Spawn => "spawn",
    };
    let agent_def = card
        .intent
        .agent_def
        .clone()
        .unwrap_or_else(|| "(inherit)".to_string());
    let mut body_lines = vec![
        Line::from(Span::styled(
            format!("recipe: {recipe}    agent_def: {agent_def}"),
            theme.emphasized,
        )),
        Line::from(""),
    ];
    body_lines.extend(
        card.intent
            .prompt
            .split('\n')
            .map(|line| Line::from(line.to_string())),
    );
    let body = Paragraph::new(body_lines).wrap(Wrap { trim: false });
    frame.render_widget(body, body_area);

    let footer_lines = vec![
        Line::from("[enter] confirm  [e] edit  [esc] manual"),
        Line::from(""),
    ];
    let footer = Paragraph::new(footer_lines).wrap(Wrap { trim: true });
    frame.render_widget(footer, footer_area);
}

#[cfg(test)]
mod tests {
    use conway::{AgentId, PermissionDecision, PermissionRequest, ToolCategory, ToolName};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;

    use super::*;
    use crate::tui::gate::PendingPrompt;
    use crate::tui::input::{self, Action};
    use crate::tui::state::Entry;
    use crate::tui::test_support::render_text;

    #[test]
    fn draw_produces_a_non_empty_buffer_and_does_not_mutate_state() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.transcript.push(Entry::Assistant {
            text: "hello".to_string(),
            model: None,
            summary: None,
            ts: None,
        });
        let before = format!("{:?}", state.transcript);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(&state, f, &Theme::default())).expect("draw");

        let buffer = terminal.backend().buffer();
        let non_blank = buffer.content().iter().any(|cell| cell.symbol() != " ");
        assert!(non_blank, "expected the frame to render something");

        let after = format!("{:?}", state.transcript);
        assert_eq!(before, after, "draw must not mutate AppState");
    }

    #[test]
    fn small_terminal_does_not_panic() {
        let root = AgentId::new();
        let state = AppState::new(root);
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(&state, f, &Theme::default())).expect("draw");
        let buffer = terminal.backend().buffer();
        assert!(buffer.content().iter().any(|cell| cell.symbol() != " "));
    }

    #[test]
    fn agent_panel_hidden_by_default() {
        let root = AgentId::new();
        let state = AppState::new(root);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(&state, f, &Theme::default())).expect("draw");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(!text.contains("agents ("));
    }

    #[test]
    fn agent_panel_shown_once_toggled_on() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.toggle_agent_view();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(&state, f, &Theme::default())).expect("draw");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("agents ("));
    }

    #[test]
    fn slash_input_shows_the_command_palette() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.input = "/as".to_string();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(&state, f, &Theme::default())).expect("draw");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("/ask"));
    }

    #[test]
    fn non_slash_input_hides_the_command_palette() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.input = "hello".to_string();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| draw(&state, f, &Theme::default())).expect("draw");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        // The status line's own "/ for commands" hint always contains the
        // word "commands" -- assert against a palette-only string (a
        // command's usage form) instead of that word.
        assert!(!text.contains("/ask <text>"));
    }

    // ---- 01KYB0F7V65QAMZWWYH8K7DWDC: permission overlay always shows the
    // action keys + a long command is viewable ----

    fn sample_request(rendered: &str) -> PermissionRequest {
        PermissionRequest {
            agent_id: AgentId::new(),
            agent_path: Vec::new(),
            tool: ToolName::new("bash"),
            category: ToolCategory::Execute,
            arguments: serde_json::json!({}),
            rendered: rendered.to_string(),
            call_id: "tc_1".to_string(),
        }
    }

    fn awaiting_permission(rendered: &str) -> AppState {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let (prompt, _rx) = PendingPrompt::new_for_test(sample_request(rendered));
        state.mode = Mode::AwaitingPermission(prompt);
        state
    }

    /// Bug's own reproduction: a huge argument used to clip the
    /// tool/category line, the agent path, and the decision-key hint
    /// entirely off-screen. The hint must now ALWAYS be present, however
    /// long `rendered` is.
    #[test]
    fn permission_overlay_never_clips_the_action_key_hint_for_a_huge_command() {
        let huge_rendered = format!("bash({})", "argument-chunk-".repeat(500));
        let state = awaiting_permission(&huge_rendered);

        let text = render_text(&state, 80, 24);

        assert!(
            text.contains("[y] allow once"),
            "the [y] hint must never be clipped off-screen: {text}"
        );
        assert!(
            text.contains("[a] allow always"),
            "the [a] hint must never be clipped off-screen: {text}"
        );
        assert!(
            text.contains("[n] deny"),
            "the [n] hint must never be clipped off-screen: {text}"
        );
        assert!(
            text.contains("[Esc] deny with feedback"),
            "the [Esc] hint must never be clipped off-screen: {text}"
        );
        assert!(
            text.contains("tool: bash"),
            "the tool/category line must never be clipped off-screen: {text}"
        );
    }

    /// Review-fix regression guard: a SMALL viewport (a ~7-row terminal --
    /// input(3) + status(1) leaves a 3-row transcript area, plausible for a
    /// split pane, not a 1-row extreme) used to squeeze the footer below
    /// `PERMISSION_FOOTER_ROWS`, and because the decision-key hint was the
    /// LAST footer line, it was the FIRST thing a `Paragraph`'s top-down
    /// clipping dropped -- exactly when the user most needed to see how to
    /// answer. The hint must survive even here.
    #[test]
    fn permission_overlay_shows_the_action_key_hint_on_a_small_viewport() {
        let huge_rendered = format!("bash({})", "argument-chunk-".repeat(500));
        let state = awaiting_permission(&huge_rendered);

        // 7-row terminal: layout() gives the transcript pane 7 - 3 (input)
        // - 1 (status) = 3 rows, well inside the "3-4 rows, not a 1-row
        // extreme" scenario this fix targets.
        let text = render_text(&state, 80, 7);

        assert!(
            text.contains("[y] allow once"),
            "the [y] hint must survive a small-viewport footer: {text}"
        );
        assert!(
            text.contains("[a] allow always"),
            "the [a] hint must survive a small-viewport footer: {text}"
        );
        assert!(
            text.contains("[n] deny"),
            "the [n] hint must survive a small-viewport footer: {text}"
        );
    }

    /// The command itself must stay viewable: at the top of a long command
    /// the tail is not yet on screen, but paging down (`PageDown`, driven
    /// through the real `input::handle_key` router, exactly as a keypress
    /// would) brings it into view while the hint stays pinned.
    #[test]
    fn permission_overlay_page_down_reveals_the_rest_of_a_long_command() {
        let rendered = format!("bash({} TAIL_MARKER_XYZ)", "word ".repeat(800));
        let mut state = awaiting_permission(&rendered);

        let before = render_text(&state, 80, 24);
        assert!(
            !before.contains("TAIL_MARKER_XYZ"),
            "the tail of a huge command must not already be visible with no scrolling: {before}"
        );
        // Still true at the top: the hint is visible even before any
        // scrolling happens (the main invariant this item fixes).
        assert!(before.contains("[y] allow once"));

        // Page down generously -- `draw_permission_overlay` clamps the
        // scroll to the command's own real wrapped height, so overshooting
        // just lands on the true bottom.
        for _ in 0..40 {
            let action = input::handle_key(
                &mut state,
                KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            );
            assert_eq!(action, Action::None);
        }

        let after = render_text(&state, 80, 24);
        assert!(
            after.contains("TAIL_MARKER_XYZ"),
            "paging down must bring the rest of the command into view: {after}"
        );
        // The hint must STILL be visible after scrolling -- pinned, not
        // part of the scrolled region.
        assert!(after.contains("[y] allow once"));
        assert!(after.contains("[a] allow always"));
        assert!(after.contains("[n] deny"));
        assert!(after.contains("[Esc]"));
    }

    /// A short command must still render normally (no regression from the
    /// rework), and the decision keys must keep working exactly as before.
    #[test]
    fn permission_overlay_short_command_renders_and_decision_keys_still_resolve() {
        let mut state = awaiting_permission("bash: ls");

        let text = render_text(&state, 80, 24);
        assert!(text.contains("bash: ls"));
        assert!(text.contains("[y] allow once"));
        assert!(text.contains("[a] allow always"));
        assert!(text.contains("[n] deny"));
        assert!(text.contains("[Esc] deny with feedback"));

        let action = input::handle_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );
        assert_eq!(
            action,
            Action::PermissionDecision(PermissionDecision::AllowOnce)
        );
        state.resolve_current_prompt(PermissionDecision::AllowOnce);
        assert!(
            matches!(state.mode, Mode::Normal),
            "resolving the decision must return to Mode::Normal"
        );
    }

    // ---- B5: the /ask single-turn modal ----

    fn ask_modal_state(question: &str, answer: &str, error: Option<&str>) -> AppState {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.offer_ask_modal(crate::tui::state::AskModal {
            question: question.to_string(),
            child: AgentId::new(),
            answer: answer.to_string(),
            error: error.map(str::to_string),
        });
        state
    }

    #[test]
    fn ask_modal_renders_the_question_answer_and_fate_keys() {
        let state = ask_modal_state("what is the status?", "all green", None);

        let text = render_text(&state, 80, 24);

        assert!(text.contains("you asked: what is the status?"), "{text}");
        assert!(text.contains("all green"), "{text}");
        assert!(text.contains("[p] pull in"), "{text}");
        assert!(text.contains("[f] fork"), "{text}");
        assert!(text.contains("[esc] discard"), "{text}");
    }

    #[test]
    fn ask_modal_shows_the_error_after_a_failed_fate() {
        let state = ask_modal_state("q", "a", Some("pull_in refused: child is still running"));

        let text = render_text(&state, 80, 24);

        assert!(
            text.contains("pull_in refused: child is still running"),
            "the failed fate's error must show in-modal: {text}"
        );
        // The fate keys are still on screen -- the user still must choose.
        assert!(text.contains("[p] pull in"), "{text}");
    }

    #[test]
    fn ask_modal_hides_the_agents_panel_even_when_it_was_open() {
        let mut state = ask_modal_state("q", "a", None);
        state.agent_view_open = true;

        let text = render_text(&state, 80, 24);

        assert!(
            !text.contains("agents ("),
            "the /agents panel must not be visible while the modal is open: {text}"
        );
        assert!(
            state.agent_view_open,
            "the flag itself is left untouched -- the panel returns once the modal closes"
        );
    }

    // ---- C2: the NL intent confirmation card ----

    fn intent_confirm_state(
        recipe: conway::SubagentMode,
        agent_def: Option<&str>,
        prompt: &str,
    ) -> AppState {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.offer_intent_confirm(crate::tui::state::IntentConfirm {
            intent: conway::AgentIntent {
                recipe,
                agent_def: agent_def.map(str::to_string),
                prompt: prompt.to_string(),
            },
            default_recipe: recipe,
            raw_text: prompt.to_string(),
            parent: root,
        });
        state
    }

    #[test]
    fn intent_confirm_card_renders_recipe_def_prompt_and_footer() {
        let state = intent_confirm_state(conway::SubagentMode::Spawn, Some("reviewer"), "review the diff carefully");

        let text = render_text(&state, 80, 24);

        assert!(text.contains("recipe: spawn"), "the classified recipe must render: {text}");
        assert!(text.contains("agent_def: reviewer"), "the classified agent_def must render: {text}");
        assert!(
            text.contains("review the diff carefully"),
            "the classified prompt must render: {text}"
        );
        assert!(
            text.contains("[enter] confirm  [e] edit  [esc] manual"),
            "the three-choice footer must render: {text}"
        );
    }

    #[test]
    fn intent_confirm_card_shows_inherit_when_no_agent_def() {
        let state = intent_confirm_state(conway::SubagentMode::Fork, None, "go");

        let text = render_text(&state, 80, 24);

        assert!(text.contains("recipe: fork"), "{text}");
        assert!(
            text.contains("agent_def: (inherit)"),
            "None agent_def must render as (inherit): {text}"
        );
        assert!(text.contains("[enter] confirm  [e] edit  [esc] manual"));
    }

    #[test]
    fn intent_confirm_card_hides_the_agents_panel_even_when_it_was_open() {
        let mut state = intent_confirm_state(conway::SubagentMode::Spawn, None, "go");
        state.agent_view_open = true;

        let text = render_text(&state, 80, 24);

        assert!(
            !text.contains("agents ("),
            "the /agents panel must not be visible while the card is open: {text}"
        );
        assert!(
            state.agent_view_open,
            "the flag itself is left untouched -- the panel returns once the card closes"
        );
    }
}
