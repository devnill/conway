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
//! ask: "keep rendering functions small and testable"). T6 added
//! `header.rs` for two scroll affordances: a sticky overlay above the
//! transcript and a floating "jump to bottom" footer pill over its bottom
//! row. A later item corrected T6's sticky overlay: it originally put
//! `session · agent <id>[ via lineage] · model · ctx%` there -- application
//! chrome, not scroll-position-dependent information -- gated on whether the
//! transcript overflowed, which was itself the tell that the content was
//! misfiled (chrome that flickers with scroll position is noise). That
//! content moved to the status line (`view/status.rs`'s `session`/`lineage`
//! fields); `header.rs` now shows only the current turn's own prompt, and
//! only while it has scrolled out of view -- see that module's own doc for
//! the full story and the exact trigger.
//!
//! Neither of `header.rs`'s two widgets reserves a layout row: both are
//! drawn straight onto the frame after `transcript::draw`, so [`layout`]
//! itself no longer needs to predict scroll-driven overflow (a real
//! transcript-vs-reserved-row feedback loop T6 originally had to work
//! around with a fixed-point trick -- gone along with the row it used to
//! reserve).

// `pub(crate)` for item A3: `tui::commands`'s `/tree` snapshot renderer
// reuses `agents::recipe_parts`/`agents::ancestor_depth` so the hidden
// alias can never drift from what the panel draws.
pub(crate) mod agents;
mod header;
mod help;
mod input_box;
// V1: the shared bottom-anchored/content-sized/capped modal primitive, and
// the tree-navigation primitive layered on it. `pub(crate)` (not private)
// so `help.rs` (a sibling) and a future V4 settings module can both reach
// them the same way `agents.rs`'s recipe-label helpers are already shared.
pub(crate) mod menu;
pub(crate) mod modal;
pub mod palette;
// V4: the `/settings` menu, the first real caller of `menu`/`modal` beyond
// `/help`. `pub(crate)` (not private) so `input.rs` can build/navigate the
// SAME tree this module renders (`super::view::settings::build_tree`) --
// mirrors `palette` (`pub mod`) already being reachable from `input.rs` the
// same way.
pub(crate) mod settings;
mod status;
pub mod theme;
mod transcript;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use super::state::{AppState, AskModal, IntentConfirm, Mode};
pub use theme::Theme;

/// The input box's floor height: one row of text plus the two border rows
/// (top/bottom) -- the pre-T8 fixed size, still the minimum a single-line
/// draft ever occupies. [`input_height`] never returns less than this.
const MIN_INPUT_HEIGHT: u16 = 3;
const STATUS_HEIGHT: u16 = 1;
const AGENT_PANEL_HEIGHT: u16 = 8;

/// T8: the input box's height grows with its own content -- one row per
/// line in `state.input` (`\n`-separated, so Alt/Shift-Enter's inserted
/// newlines each earn a row), plus the two border rows -- capped at
/// `area.height / 3` (item spec: "cap growth at `min(content_lines + 2,
/// area.height / 3)`") so a very long paste or multi-line draft can never
/// crowd the transcript/status out entirely. The cap itself is floored at
/// [`MIN_INPUT_HEIGHT`] so a tiny terminal (`area.height / 3 < 3`) never
/// shrinks the input box below one visible line of text.
///
/// [`layout`]'s own constraint list reads THIS value, not a fixed constant --
/// growing the input box shrinks the transcript's available height exactly
/// where `layout` measures it, so the two can never disagree about how many
/// rows the transcript actually has.
fn input_height(state: &AppState, area_height: u16) -> u16 {
    let content_lines = state.input.split('\n').count().max(1) as u16;
    let desired = content_lines.saturating_add(2);
    let cap = (area_height / 3).max(MIN_INPUT_HEIGHT);
    desired.clamp(MIN_INPUT_HEIGHT, cap)
}

pub fn draw(state: &AppState, frame: &mut Frame, theme: &Theme) {
    let area = frame.area();
    let areas = layout(state, area);

    transcript::draw(frame, areas.transcript, state, theme);

    // The sticky prompt overlay (shows the current turn's prompt once it
    // scrolls out of view) and the floating "jump to bottom" footer (shown
    // over the transcript's own bottom row while scrolled up) -- both drawn
    // as their own separate widgets straight onto the frame, AFTER
    // `transcript::draw`, never folded into its `Paragraph` and never
    // claiming a layout row of their own (see `header.rs`'s module doc).
    // `max_scroll`/the effective scroll offset are deliberately recomputed
    // here (not threaded out of `transcript::draw`, which computes its own
    // internally) the same way `app.rs`'s `page_scroll`/`jump_to_top`
    // recompute them fresh outside any render pass -- both are cheap and
    // always derived from the SAME `Paragraph`/`Wrap` parameters
    // `transcript::draw` just rendered with, so neither can ever disagree
    // with what is actually on screen.
    let live_max_scroll = max_scroll(state, area);
    let live_scroll = effective_scroll(state, live_max_scroll);
    header::draw_sticky_prompt(frame, areas.transcript, state, theme, live_scroll);
    header::draw_scroll_footer(frame, areas.transcript, state, theme, live_max_scroll);

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
            state.permission_grant_scope,
            state.modal_scroll,
            theme,
        );
    }

    if let Mode::AskModal(modal) = &state.mode {
        draw_ask_modal(frame, areas.transcript, modal, state.modal_scroll, theme);
    }

    if let Mode::IntentConfirm(card) = &state.mode {
        draw_intent_confirm(frame, areas.transcript, card, state.modal_scroll, theme);
    }

    // T7: the `/help` keybinding overlay is NOT a `Mode` variant (see
    // `AppState::help_open`'s own doc) -- it is gated on `Mode::Normal`
    // here instead, which is exactly how it avoids ever stacking on top of
    // an active permission prompt / `/ask` modal / intent-confirm card:
    // whichever of those three is live already claimed `mode` above (and
    // drew its own overlay over this same `areas.transcript`), so this
    // branch simply never fires while one of them is showing. No separate
    // parking queue is needed -- `state.help_open` is untouched by any of
    // those transitions, so the overlay reappears on its own the moment
    // `mode` returns to `Normal`, with zero extra bookkeeping.
    if state.help_open && matches!(state.mode, Mode::Normal) {
        help::draw(frame, areas.transcript, state.modal_scroll, theme);
    }

    // V4: the `/settings` menu follows the EXACT same gating as `/help`
    // just above (informational, checked against `Mode::Normal`, never a
    // `Mode` variant -- see `AppState::settings_open`'s own doc), and is
    // mutually exclusive with it by construction
    // (`AppState::open_settings`/`open_help` each clear the other), so this
    // branch and the one above it never both fire.
    if state.settings_open && matches!(state.mode, Mode::Normal) {
        settings::draw(frame, areas.transcript, state, theme);
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
    // T8: the input box's height is content-dependent (grows with
    // multi-line drafts, capped at `area.height / 3`) -- computed once here
    // so `show_agents`'s size check and the constraint list itself agree on
    // the SAME value.
    let input_height = input_height(state, area.height);

    // B5: while the /ask modal owns the screen, the /agents panel is NOT
    // visible (user decision, binding) even if it was open when the modal
    // appeared -- `state.agent_view_open` itself is left untouched, so the
    // panel comes back exactly as it was once a fate closes the modal.
    let show_agents = state.agent_view_open
        && !matches!(state.mode, Mode::AskModal(_) | Mode::IntentConfirm(_))
        && area.height > input_height + STATUS_HEIGHT + 3;

    // This item removed T6's sticky-header row reservation entirely: the
    // sticky prompt overlay and the floating scroll footer (`header.rs`) are
    // both drawn straight onto the frame after `transcript::draw`, never
    // claiming a `Constraint` of their own -- so this function no longer
    // needs to predict whether the transcript will overflow (the feedback
    // loop T6's own fixed-point trick used to work around is gone along
    // with the row it was reserving).
    let mut constraints = vec![Constraint::Min(0)];
    if show_agents {
        constraints.push(Constraint::Length(AGENT_PANEL_HEIGHT.min(area.height / 3)));
    }
    constraints.push(Constraint::Length(input_height));
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

/// The transcript's actual rendered scroll offset (wrapped rows from the
/// top) -- the SAME clamp `transcript::draw` applies internally
/// (`follow_tail` pins to `max_scroll`; otherwise `state.scroll` clamped to
/// it). Recomputed here (not threaded out of that render pass) so the
/// sticky-prompt overlay's trigger can never disagree with what is actually
/// on screen -- mirrors why `max_scroll` itself is recomputed fresh rather
/// than read back out of `transcript::draw`.
fn effective_scroll(state: &AppState, max_scroll: u16) -> u16 {
    if state.follow_tail {
        max_scroll
    } else {
        state.scroll.min(max_scroll)
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
/// path line, the `[y]/[a]/[p]/[n]/[Esc]` decision-key hint, the grant-scope
/// line (which `[a]`/`[p]` remember at, cycled by `[s]`), and (V2b) the
/// one-line statement of what `[p]` would grant. Five rows, not four: the
/// offer and scope lines must be reserved even when no pattern is on offer,
/// because a footer that changes height as the operator scrolls would shift
/// the command text under them mid-read. This is the
/// load-bearing invariant behind [`draw_permission_overlay`]'s whole
/// rework -- see that function's own doc.
///
/// HONEST ACCOUNTING, one conditional line this constant does NOT reserve:
/// the `[PageUp/PageDown] scroll command` hint appears only when the command
/// body overflows its cap. With both it and an offer present the footer is
/// 6 lines against these 5, so the bottom-most line (agent path) clips by
/// one. Accepted: the clip order keeps every decision-relevant line (keys,
/// scope, offer) ahead of the informational tool/category and agent-path
/// lines, so what clips is always the least decision-relevant row.
const PERMISSION_FOOTER_ROWS: u16 = 5;

/// The permission prompt: bottom-anchored, content-sized, capped, drawn over
/// the transcript via the shared [`modal`] primitive (V1) -- unmistakably
/// distinct from ordinary transcript output (module notes; also this item's
/// human criterion), never part of the copyable conversation (it replaces
/// transcript content on screen only while a decision is pending, via
/// `Clear`).
///
/// Bug fix (01KYB0F7V65QAMZWWYH8K7DWDC): this used to be a fixed ~6-row box
/// with the ENTIRE `req.rendered` command as line 0 of one unscrolled
/// `Paragraph` -- a long tool-call argument overflowed the box and clipped
/// the tool/category line, the agent path, and the `[y]/[a]/[n]/[Esc]`
/// decision-key hint off-screen. V1 replaces that fix's own interim shape
/// (which over-corrected to "claim nearly the whole transcript area" --
/// exactly the complaint THIS item exists to address) with
/// [`modal::draw_modal_frame`]: the overlay sizes to `req.rendered`'s own
/// wrapped line count, capped at [`modal::DEFAULT_CAP_DENOMINATOR`] of the
/// transcript area, and SCROLLS past the cap instead of either truncating or
/// eating the screen.
/// - The block's interior is split into a scrollable command body and a
///   FIXED-height footer ([`PERMISSION_FOOTER_ROWS`]) below it holding the
///   tool/category line, the agent path, and the decision-key hint -- rows
///   the command `Paragraph` can never grow into, however long
///   `req.rendered` is or however far it's scrolled. This is what keeps the
///   hint on screen, not the command's own wrapping.
/// - `scroll` (`AppState::modal_scroll`, paged by `PageUp`/`PageDown` while
///   `Mode::AwaitingPermission` -- see `input.rs::handle_permission_key`)
///   drives the command body's `Paragraph::scroll`, clamped here
///   ([`modal::clamp_scroll`]) to the command's own wrapped line count so an
///   over-large value just lands on the true bottom, never past real
///   content. `modal_scroll` is shared across every modal-bearing surface
///   (see that field's own doc on `AppState`) -- only one is ever showing at
///   a time, so one field suffices for all of them.
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
    grant_scope: conway::PermissionScope,
    scroll: u16,
    theme: &Theme,
) {
    let agent_path = if req.agent_path.is_empty() {
        "root".to_string()
    } else {
        req.agent_path
            .iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(" -> ")
    };

    let body = Paragraph::new(Line::from(Span::styled(
        req.rendered.clone(),
        theme.emphasized,
    )))
    .wrap(Wrap { trim: false });
    let content_rows = body
        .line_count(modal::body_width(transcript_area))
        .min(u16::MAX as usize) as u16;

    let frame_areas = modal::draw_modal_frame(
        frame,
        transcript_area,
        content_rows,
        PERMISSION_FOOTER_ROWS,
        modal::DEFAULT_CAP_DENOMINATOR,
        " PERMISSION REQUIRED ",
        theme.border_danger,
    );

    let body_max_scroll = modal::body_max_scroll(content_rows, frame_areas.body_area.height);
    let clamped_scroll = modal::clamp_scroll(scroll, body_max_scroll);
    frame.render_widget(body.scroll((clamped_scroll, 0)), frame_areas.body_area);

    // Review fix (01KYB0F7V65QAMZWWYH8K7DWDC): even with the footer's rows
    // reserved first ([`modal::draw_modal_frame`]'s own invariant),
    // `footer_area` can still end up shorter than [`PERMISSION_FOOTER_ROWS`]
    // on a genuinely tiny viewport -- the block's own 2 border rows alone
    // can eat most of a small transcript area, with nothing left to
    // reserve. A `Paragraph` clips top-down, so whichever line is FIRST
    // survives longest as the footer shrinks. The decision-key hint is what
    // the user actually needs to act on the prompt -- it goes FIRST here,
    // ahead of the purely informational tool/category and agent-path lines,
    // so even a 1-row footer still shows it.
    // V2b: `[p]` appears only when a pattern grant is actually on offer.
    // What is offered depends on the call's own `render_kind` (carried on
    // the request from the broker): a shell-shaped rendering gets the
    // narrow two-token prefix, or nothing when it carries shell
    // metacharacters (advertising a key that would do nothing is worse
    // than omitting it); a `Structured` rendering gets the registerable
    // `tool:*` wildcard -- a prefix over a JSON dump is a registration
    // error, so it is never offered.
    let offered = conway::permission_pattern::suggested_rule(
        req.tool.as_str(),
        &req.rendered,
        req.render_kind,
    );
    // The decision keys must all stay legible on a narrow terminal --
    // losing `[Esc] deny with feedback` off the right edge would hide a
    // decision the operator may want. So the keys go on their own line and
    // the scroll hint, which is an affordance rather than a decision, goes
    // on the offer line's tail when there is room.
    let hint = if offered.is_some() {
        "[y] once  [a] always  [p] pattern  [n] deny  [Esc] deny w/ feedback"
    } else {
        "[y] allow once  [a] allow always  [n] deny  [Esc] deny with feedback"
    };
    let mut footer_lines = vec![Line::from(hint)];
    // The scope the remembered-grant keys (`a` and `p`) grant at, stated
    // in words -- the grant's BREADTH along the who-does-it-cover axis,
    // the same way the `[p] grants:` line below states it along the
    // which-calls axis. An operator answering the prompt is answering both
    // at once, so both are on screen before anything is pressed. Second
    // line, right under the decision keys it modifies: it is
    // decision-relevant, not informational, so it must outlast the
    // tool/category and agent-path lines as the footer clips top-down.
    let scope_words = match grant_scope {
        conway::PermissionScope::Session => "this session",
        conway::PermissionScope::Agent => "this agent only",
        conway::PermissionScope::AgentSubtree => "this agent and its subtree",
        // `PermissionScope` is `#[non_exhaustive]`: describe an unknown
        // scope honestly rather than guessing at its breadth.
        _ => "a custom scope",
    };
    footer_lines.push(Line::from(format!(
        "  [a]/[p] remember for: {scope_words}  ([s] cycles)"
    )));
    if body_max_scroll > 0 {
        footer_lines.push(Line::from("  [PageUp/PageDown] scroll command"));
    }
    // The offered grant's BREADTH, stated in words, before the operator
    // presses anything. This is the whole premise of choosing prefixes
    // over regex: a grant you can evaluate by reading it. Placed directly
    // under the key hint so it is not separated from the `[p]` it explains.
    if let Some(rule) = &offered {
        footer_lines.push(Line::from(format!("  [p] grants: {}", rule.describe())));
    }
    footer_lines.push(Line::from(format!(
        "tool: {}  category: {:?}",
        req.tool, req.category
    )));
    footer_lines.push(Line::from(format!("agent path: {agent_path}")));
    let footer = Paragraph::new(footer_lines).wrap(Wrap { trim: true });
    frame.render_widget(footer, frame_areas.footer_area);
}

/// Rows the /ask modal's footer ALWAYS reserves (B5): the fate-key hint,
/// plus one line for the in-modal error shown after a failed fate (blank
/// when there is none, so the hint never jumps vertically when an error
/// appears).
const ASK_MODAL_FOOTER_ROWS: u16 = 2;

/// The `/ask` single-turn modal (B5): bottom-anchored, content-sized,
/// capped, via the shared [`modal`] primitive (V1) -- following
/// [`draw_permission_overlay`]'s precedent (a modal overlay replacing
/// transcript content only while a decision is pending, via `Clear` --
/// never part of the copyable conversation).
///
/// V1 adds scrolling here where B5 originally had none (the answer used to
/// simply clip on a small viewport, on the reasoning that the full answer
/// remains reachable afterward via whichever fate the user picks) -- this
/// item's own acceptance criterion is that every ported surface scrolls
/// past its cap rather than truncating, so a long answer is no longer lost
/// to a small terminal even before the fate is chosen.
///
/// The footer shows the three fate keys -- `[p] pull in · [f] fork ·
/// [esc] discard` -- and, after a FAILED fate, the error that kept the
/// modal open (red). The hint is ordered FIRST within the footer for the
/// same small-viewport reason the permission overlay's doc explains: a
/// `Paragraph` clips top-down, so the line the user needs to act on is the
/// last thing clipped.
fn draw_ask_modal(
    frame: &mut Frame,
    transcript_area: Rect,
    modal_state: &AskModal,
    scroll: u16,
    theme: &Theme,
) {
    let mut body_lines = vec![
        Line::from(Span::styled(
            format!("you asked: {}", modal_state.question),
            theme.emphasized,
        )),
        Line::from(""),
    ];
    body_lines.extend(
        modal_state
            .answer
            .split('\n')
            .map(|line| Line::from(line.to_string())),
    );
    let body = Paragraph::new(body_lines).wrap(Wrap { trim: false });
    let content_rows = body
        .line_count(modal::body_width(transcript_area))
        .min(u16::MAX as usize) as u16;

    let frame_areas = modal::draw_modal_frame(
        frame,
        transcript_area,
        content_rows,
        ASK_MODAL_FOOTER_ROWS,
        modal::DEFAULT_CAP_DENOMINATOR,
        " ASK ",
        theme.border_warning,
    );

    let body_max_scroll = modal::body_max_scroll(content_rows, frame_areas.body_area.height);
    let clamped_scroll = modal::clamp_scroll(scroll, body_max_scroll);
    frame.render_widget(body.scroll((clamped_scroll, 0)), frame_areas.body_area);

    let hint = if body_max_scroll > 0 {
        "[p] pull in  [f] fork  [esc] discard  [PageUp/PageDown] scroll"
    } else {
        "[p] pull in  [f] fork  [esc] discard"
    };
    let error_line = match &modal_state.error {
        Some(err) => Line::from(Span::styled(format!("error: {err}"), theme.error)),
        None => Line::from(""),
    };
    let footer_lines = vec![Line::from(hint), error_line];
    let footer = Paragraph::new(footer_lines).wrap(Wrap { trim: true });
    frame.render_widget(footer, frame_areas.footer_area);
}

/// Rows the intent confirmation card's footer ALWAYS reserves (C2): the
/// choice-key hint -- `[enter] confirm  [e] edit  [esc] manual` -- plus a
/// blank line reserved for symmetry with [`ASK_MODAL_FOOTER_ROWS`] (the
/// card has no in-modal error state: a failed confirm/manual re-enters
/// `commands::execute`, which pushes the failure as a transcript `Notice`
/// and returns `Effect::None`, so the card closes on the failure rather
/// than staying open the way the `/ask` modal does).
const INTENT_CONFIRM_FOOTER_ROWS: u16 = 2;

/// The NL intent confirmation card (C2): bottom-anchored, content-sized,
/// capped, via the shared [`modal`] primitive (V1) -- following
/// [`draw_ask_modal`]'s overlay precedent (a modal overlay replacing
/// transcript content only while a decision is pending, via `Clear` --
/// never part of the copyable conversation). The card shows the classified
/// `recipe` (`fork`/`spawn`), the `agent_def` (or `(inherit)` when `None`),
/// and the `prompt` the classifier produced (or the user's raw text on the
/// verbatim-passthrough path), then forces exactly one choice via the
/// footer: `[enter] confirm  [e] edit  [esc] manual`. V1 adds scrolling
/// here too (a long classified prompt used to simply clip). The hint is
/// ordered FIRST within the footer for the same small-viewport reason the
/// `/ask` modal's doc explains: a `Paragraph` clips top-down, so the line
/// the user needs to act on is the last thing clipped.
fn draw_intent_confirm(
    frame: &mut Frame,
    transcript_area: Rect,
    card: &IntentConfirm,
    scroll: u16,
    theme: &Theme,
) {
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
    let content_rows = body
        .line_count(modal::body_width(transcript_area))
        .min(u16::MAX as usize) as u16;

    let frame_areas = modal::draw_modal_frame(
        frame,
        transcript_area,
        content_rows,
        INTENT_CONFIRM_FOOTER_ROWS,
        modal::DEFAULT_CAP_DENOMINATOR,
        " INTENT ",
        theme.border_accent,
    );

    let body_max_scroll = modal::body_max_scroll(content_rows, frame_areas.body_area.height);
    let clamped_scroll = modal::clamp_scroll(scroll, body_max_scroll);
    frame.render_widget(body.scroll((clamped_scroll, 0)), frame_areas.body_area);

    let hint = if body_max_scroll > 0 {
        "[enter] confirm  [e] edit  [esc] manual  [PageUp/PageDown] scroll"
    } else {
        "[enter] confirm  [e] edit  [esc] manual"
    };
    let footer_lines = vec![Line::from(hint), Line::from("")];
    let footer = Paragraph::new(footer_lines).wrap(Wrap { trim: true });
    frame.render_widget(footer, frame_areas.footer_area);
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
    use crate::tui::test_support::{render, render_text};

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
        terminal
            .draw(|f| draw(&state, f, &Theme::default()))
            .expect("draw");

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
        terminal
            .draw(|f| draw(&state, f, &Theme::default()))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        assert!(buffer.content().iter().any(|cell| cell.symbol() != " "));
    }

    /// End-to-end companion to `view/status.rs`'s width-aware assembly
    /// tests: through the REAL `draw` render pass at a realistic narrow
    /// terminal (40 columns, the width the review measured `hint` losing
    /// ~26 characters at), the status row still names `/help` -- proving
    /// the width budgeting actually reaches the terminal, not just the
    /// pure `status_line_spans` return value.
    #[test]
    fn status_row_keeps_a_hint_pointer_at_a_narrow_forty_column_terminal() {
        let root = AgentId::new();
        let state = AppState::new(root);
        let backend = TestBackend::new(40, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(&state, f, &Theme::default()))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(
            text.contains("/help"),
            "the status line must still point at /help at 40 columns, not \
             silently clip it off screen: {text:?}"
        );
    }

    #[test]
    fn agent_panel_hidden_by_default() {
        let root = AgentId::new();
        let state = AppState::new(root);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(&state, f, &Theme::default()))
            .expect("draw");
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
        terminal
            .draw(|f| draw(&state, f, &Theme::default()))
            .expect("draw");
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
        terminal
            .draw(|f| draw(&state, f, &Theme::default()))
            .expect("draw");
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
        terminal
            .draw(|f| draw(&state, f, &Theme::default()))
            .expect("draw");
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
            // `bash` genuinely declares ShellCommand -- the honest fixture
            // for a shell tool's prompt.
            render_kind: conway::RenderKind::ShellCommand,
        }
    }

    /// A `Structured`-render tool's prompt fixture (e.g. `report` -- its
    /// rendering is a JSON dump, never a shell command, and it declares
    /// `RenderKind::Structured` to say so).
    fn sample_structured_request(rendered: &str) -> PermissionRequest {
        PermissionRequest {
            render_kind: conway::RenderKind::Structured,
            tool: ToolName::new("report"),
            category: ToolCategory::Read,
            ..sample_request(rendered)
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
        // V2b shortened the key labels so every decision -- including
        // deny-with-feedback -- stays legible once `[p]` joined the row.
        assert!(text.contains("[y]"), "{text}");
        assert!(text.contains("[a]"), "{text}");
        assert!(text.contains("[n] deny"), "{text}");
        assert!(
            text.contains("[Esc] deny w/ feedback"),
            "the deny-with-feedback key must not be pushed off the edge: {text}"
        );
        // The offered grant is named, and its breadth is stated in words.
        assert!(text.contains("[p]"), "{text}");
        assert!(
            text.contains("commands starting with"),
            "the prompt must state what [p] would grant BEFORE it is pressed: {text}"
        );

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
        let state = intent_confirm_state(
            conway::SubagentMode::Spawn,
            Some("reviewer"),
            "review the diff carefully",
        );

        let text = render_text(&state, 80, 24);

        assert!(
            text.contains("recipe: spawn"),
            "the classified recipe must render: {text}"
        );
        assert!(
            text.contains("agent_def: reviewer"),
            "the classified agent_def must render: {text}"
        );
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

    // ---- T8: dynamic input-box height ----

    #[test]
    fn single_line_input_keeps_the_pre_t8_fixed_height() {
        let state = AppState::new(AgentId::new());
        assert_eq!(input_height(&state, 24), MIN_INPUT_HEIGHT);
    }

    #[test]
    fn input_height_grows_by_one_row_per_extra_line() {
        let mut state = AppState::new(AgentId::new());
        state.input = "line one\nline two\nline three".to_string();
        // 3 content lines + 2 border rows, well under the height/3 cap at a
        // normal terminal size.
        assert_eq!(input_height(&state, 24), 5);
    }

    #[test]
    fn input_height_is_capped_at_a_third_of_the_terminal_height() {
        let mut state = AppState::new(AgentId::new());
        state.input = "l\n".repeat(30); // 31 content lines -- way over any cap
        let area_height = 24;
        assert_eq!(input_height(&state, area_height), area_height / 3);
    }

    #[test]
    fn growing_the_input_box_shrinks_the_transcript_area_by_the_same_amount() {
        let root = AgentId::new();
        let mut single_line = AppState::new(root);
        single_line.input = "one line".to_string();
        let mut multi_line = AppState::new(root);
        multi_line.input = "one\ntwo\nthree".to_string();

        let area = Rect::new(0, 0, 80, 24);
        let single_areas = layout(&single_line, area);
        let multi_areas = layout(&multi_line, area);

        assert_eq!(single_areas.input.height, MIN_INPUT_HEIGHT);
        assert_eq!(multi_areas.input.height, 5);
        assert_eq!(
            single_areas.transcript.height - multi_areas.transcript.height,
            multi_areas.input.height - single_areas.input.height,
            "every extra row the input box claims must come straight out of \
             the transcript area, not somewhere else in the chrome"
        );
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

    // ---- T7: the /help keybinding overlay ----

    /// Acceptance: the overlay's content includes every binding in the
    /// verified list (enumerated from `input.rs` at HEAD -- see
    /// `view/help.rs`'s own doc for the full rationale), asserted against
    /// the REAL rendered text, not a constant.
    #[test]
    fn help_overlay_shows_every_verified_binding() {
        let mut state = AppState::new(AgentId::new());
        state.open_help();

        // Tall/wide enough that the full grouped list renders unclipped.
        let text = render_text(&state, 100, 60);

        // input & editing
        assert!(text.contains("Enter") && text.contains("submit"), "{text}");
        assert!(text.contains("Alt-Enter"), "{text}");
        assert!(text.contains("Shift-Enter"), "{text}");
        assert!(text.contains("insert a newline"), "{text}");
        assert!(text.contains("Left / Right"), "{text}");
        assert!(text.contains("move the cursor"), "{text}");
        assert!(text.contains("Backspace"), "{text}");
        assert!(text.contains("delete back"), "{text}");
        assert!(text.contains("Ctrl-W"), "{text}");
        assert!(text.contains("delete the previous word"), "{text}");
        assert!(text.contains("Ctrl-D"), "{text}");
        assert!(text.contains("Ctrl-C"), "{text}");
        assert!(text.contains("interrupt"), "{text}");

        // history and navigation
        assert!(text.contains("Up / Down"), "{text}");
        assert!(text.contains("Home / End"), "{text}");
        assert!(text.contains("PageUp / PageDown"), "{text}");
        assert!(text.contains("scroll the transcript by a page"), "{text}");

        // tools and display
        assert!(text.contains("Ctrl-E"), "{text}");
        assert!(text.contains("expand/collapse all tool output"), "{text}");

        // settings menu (V4)
        assert!(text.contains("settings menu"), "{text}");
        assert!(text.contains("toggle a display setting"), "{text}");
        assert!(text.contains("adjust the numeric setting"), "{text}");
        assert!(text.contains("close the settings menu"), "{text}");

        // modal keys
        assert!(text.contains("/ask modal"), "{text}");
        assert!(text.contains("fork"), "{text}");
        assert!(text.contains("pull in"), "{text}");
        assert!(text.contains("discard"), "{text}");
        assert!(text.contains("intent-confirm card"), "{text}");
        assert!(text.contains("confirm"), "{text}");
        assert!(text.contains("edit"), "{text}");
        assert!(text.contains("manual"), "{text}");
        assert!(text.contains("permission prompt"), "{text}");
        assert!(text.contains("allow once"), "{text}");
        assert!(text.contains("allow always"), "{text}");
        assert!(text.contains("deny with feedback"), "{text}");
        assert!(text.contains("scroll the command"), "{text}");

        // agent panel
        assert!(text.contains("visibility filter"), "{text}");
        // Esc's two-step behavior must be described, not just "closes the
        // panel" -- the previous wording documented only half of what the
        // key did, which is how the focus-discarding bug went unnoticed.
        assert!(text.contains("close the panel"), "{text}");
        assert!(
            text.contains("press") && text.contains("again"),
            "the overlay must say a SECOND Esc returns to the root: {text}"
        );
    }

    /// V4 acceptance: `/thinking` and `/timestamps` appear NOWHERE in the
    /// `/help` overlay -- both are removed commands, not keybindings, so
    /// they earn no row any more (unlike before V4, when they were the
    /// overlay's one deliberate "syntactically a command" exception).
    #[test]
    fn help_overlay_never_mentions_the_removed_thinking_and_timestamps_commands() {
        let mut state = AppState::new(AgentId::new());
        state.open_help();

        let text = render_text(&state, 100, 60);
        assert!(!text.contains("/thinking"), "{text}");
        assert!(!text.contains("/timestamps"), "{text}");
    }

    /// Acceptance: `mouse` appears nowhere in the overlay's keybinding rows
    /// (a guard against a future well-meaning re-add) -- `view/help.rs`'s
    /// own `no_binding_row_mentions_mouse` test covers the data directly;
    /// this is the render-level companion proving the same holds for what
    /// actually reaches the screen. The one legitimate `mouse` occurrence is
    /// the freeform note's own prose (which the `Paragraph`'s word-wrap may
    /// split across several on-screen rows) -- so this asserts the word
    /// appears EXACTLY ONCE in the whole rendered text, not zero times.
    #[test]
    fn help_overlay_explains_the_mouse_situation() {
        let mut state = AppState::new(AgentId::new());
        state.open_help();

        let text = render_text(&state, 100, 60).to_lowercase();
        // V3: `mouse` may now appear in the note AND in the Up/Down row,
        // which truthfully says the wheel arrives there via the terminal's
        // alternate-scroll mode. What must never appear is a mouse KEY
        // binding (guarded in `help.rs::no_binding_row_claims_a_mouse_key`).
        assert!(
            text.contains("mouse"),
            "the overlay must still explain the mouse situation: {text}"
        );
        // The stronger guarantee -- that no row claims a mouse KEY -- is
        // asserted structurally against the binding data in
        // `help.rs::no_binding_row_claims_a_mouse_key`, not by substring
        // matching here: this rendered text is line-wrapped, so any
        // substring assertion on it is fragile by construction.
    }

    /// Acceptance: `/help` opening the overlay is a pure `AppState` flip --
    /// no transcript entries.
    #[test]
    fn help_overlay_open_pushes_no_transcript_entries() {
        let mut state = AppState::new(AgentId::new());
        state.open_help();
        assert!(state.transcript.is_empty());
        let text = render_text(&state, 100, 60);
        assert!(text.contains("HELP"), "{text}");
    }

    /// Acceptance: `Esc` closes the overlay, driven through the real key
    /// router (not a direct `close_help()` call).
    #[test]
    fn esc_closes_the_help_overlay_end_to_end() {
        let mut state = AppState::new(AgentId::new());
        state.open_help();
        let text_before = render_text(&state, 100, 60);
        assert!(text_before.contains("HELP"), "{text_before}");

        let action = input::handle_key(&mut state, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, Action::None);

        assert!(!state.help_open);
        let text_after = render_text(&state, 100, 60);
        assert!(!text_after.contains(" HELP "), "{text_after}");
    }

    /// Acceptance: the overlay must not stack on top of an active
    /// permission prompt / `/ask` modal / intent-confirm card -- each is
    /// exercised with `help_open` left `true` in the background (exactly
    /// how it would arrive in practice: help was already open when the
    /// decision-owed surface showed up).
    #[test]
    fn help_overlay_does_not_stack_on_an_active_permission_prompt() {
        let mut state = awaiting_permission("bash: ls");
        state.open_help();

        let text = render_text(&state, 100, 60);

        assert!(
            text.contains("PERMISSION REQUIRED"),
            "the permission overlay must win: {text}"
        );
        assert!(
            !text.contains(" HELP "),
            "the help overlay must not be drawn on top of an active prompt: {text}"
        );
    }

    #[test]
    fn help_overlay_does_not_stack_on_an_active_ask_modal() {
        let mut state = ask_modal_state("q", "a", None);
        state.open_help();

        let text = render_text(&state, 100, 60);

        assert!(
            text.contains("you asked: q"),
            "the ask modal must win: {text}"
        );
        assert!(!text.contains(" HELP "), "{text}");
    }

    #[test]
    fn help_overlay_does_not_stack_on_an_active_intent_confirm_card() {
        let mut state = intent_confirm_state(conway::SubagentMode::Spawn, None, "go");
        state.open_help();

        let text = render_text(&state, 100, 60);

        assert!(
            text.contains("recipe: spawn"),
            "the intent card must win: {text}"
        );
        assert!(!text.contains(" HELP "), "{text}");
    }

    /// Once the permission prompt resolves, `mode` returns to `Normal` and
    /// the overlay -- whose `help_open` flag was never touched by any of
    /// this -- reappears with no further action needed.
    #[test]
    fn help_overlay_reappears_once_the_blocking_prompt_resolves() {
        let mut state = awaiting_permission("bash: ls");
        state.open_help();
        assert!(!render_text(&state, 100, 60).contains(" HELP "));

        state.resolve_current_prompt(PermissionDecision::AllowOnce);

        assert!(matches!(state.mode, Mode::Normal));
        assert!(
            state.help_open,
            "help_open was never touched by the prompt resolving"
        );
        assert!(
            render_text(&state, 100, 60).contains(" HELP "),
            "the overlay must reappear once mode returns to Normal"
        );
    }

    // ---- V4: the `/settings` menu -- same non-stacking guarantee `/help`
    // has, plus mutual exclusion with `/help` itself. ----

    #[test]
    fn settings_menu_does_not_stack_on_an_active_permission_prompt() {
        let mut state = awaiting_permission("bash: ls");
        state.open_settings();

        let text = render_text(&state, 100, 60);

        assert!(
            text.contains("PERMISSION REQUIRED"),
            "the permission overlay must win: {text}"
        );
        assert!(
            !text.contains(" SETTINGS "),
            "the settings menu must not be drawn on top of an active prompt: {text}"
        );
    }

    #[test]
    fn settings_menu_does_not_stack_on_an_active_ask_modal() {
        let mut state = ask_modal_state("q", "a", None);
        state.open_settings();

        let text = render_text(&state, 100, 60);

        assert!(
            text.contains("you asked: q"),
            "the ask modal must win: {text}"
        );
        assert!(!text.contains(" SETTINGS "), "{text}");
    }

    #[test]
    fn settings_menu_does_not_stack_on_an_active_intent_confirm_card() {
        let mut state = intent_confirm_state(conway::SubagentMode::Spawn, None, "go");
        state.open_settings();

        let text = render_text(&state, 100, 60);

        assert!(
            text.contains("recipe: spawn"),
            "the intent card must win: {text}"
        );
        assert!(!text.contains(" SETTINGS "), "{text}");
    }

    #[test]
    fn settings_menu_reappears_once_the_blocking_prompt_resolves() {
        let mut state = awaiting_permission("bash: ls");
        state.open_settings();
        assert!(!render_text(&state, 100, 60).contains(" SETTINGS "));

        state.resolve_current_prompt(PermissionDecision::AllowOnce);

        assert!(matches!(state.mode, Mode::Normal));
        assert!(
            state.settings_open,
            "settings_open was never touched by the prompt resolving"
        );
        assert!(
            render_text(&state, 100, 60).contains(" SETTINGS "),
            "the menu must reappear once mode returns to Normal"
        );
    }

    /// The settings menu and `/help` are mutually exclusive WITH EACH OTHER
    /// (unlike the three decision-owed surfaces above, both of these are
    /// merely informational, so nothing but their own open/close calls keeps
    /// them from stacking on one another).
    #[test]
    fn settings_menu_and_help_never_stack_on_each_other() {
        let mut state = AppState::new(AgentId::new());
        state.open_help();
        state.open_settings();

        let text = render_text(&state, 100, 60);
        assert!(text.contains(" SETTINGS "), "{text}");
        assert!(
            !text.contains(" HELP "),
            "opening settings must close help: {text}"
        );

        state.open_help();
        let text = render_text(&state, 100, 60);
        assert!(text.contains(" HELP "), "{text}");
        assert!(
            !text.contains(" SETTINGS "),
            "opening help must close settings: {text}"
        );
    }

    // ---- V1: the shared modal primitive -- every ported surface is
    // bottom-anchored, content-sized, capped, and the transcript stays
    // visible above it. ----

    /// Finds the row index of the modal's own top border (the row starting
    /// with `┌`) -- the acceptance test below uses this to prove ordinary
    /// transcript rows are still visible ABOVE a short modal, i.e. the
    /// overlay no longer claims "nearly the whole transcript area" the way
    /// the pre-V1 permission overlay's own doc used to describe.
    fn top_border_row(rows: &[String]) -> Option<usize> {
        rows.iter()
            .position(|row| row.trim_start().starts_with('┌'))
    }

    #[test]
    fn a_short_permission_command_leaves_transcript_text_visible_above_it() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.transcript.push(Entry::Assistant {
            text: "TRANSCRIPT_MARKER_ABOVE_THE_MODAL".to_string(),
            model: None,
            summary: None,
            ts: None,
        });
        let (prompt, _rx) = PendingPrompt::new_for_test(sample_request("bash: ls"));
        state.mode = Mode::AwaitingPermission(prompt);

        let rows = render(&state, 80, 24);
        let border_row = top_border_row(&rows)
            .expect("a short command's overlay must still draw a bordered block");

        // The pre-V1 overlay claimed `transcript_area.height - 1` (nearly
        // the whole area) regardless of content -- a short command now
        // sizes to its own content, so the border must land well below
        // row 0, leaving room for the transcript entry pushed above it.
        assert!(
            border_row > 5,
            "a short command's modal must be content-sized, not claim nearly the \
             whole transcript area (border landed at row {border_row}): {rows:?}"
        );
        let above: String = rows[..border_row].join("\n");
        assert!(
            above.contains("TRANSCRIPT_MARKER_ABOVE_THE_MODAL"),
            "ordinary transcript text must remain visible above a short modal: {above}"
        );
    }

    #[test]
    fn ask_modal_and_intent_confirm_also_leave_transcript_text_visible_above_them() {
        for mut state in [
            ask_modal_state("q", "a", None),
            intent_confirm_state(conway::SubagentMode::Spawn, None, "go"),
        ] {
            state.transcript.push(Entry::Assistant {
                text: "TRANSCRIPT_MARKER_ABOVE_THE_MODAL".to_string(),
                model: None,
                summary: None,
                ts: None,
            });
            // Both constructors above build a FRESH state via `AppState::new`
            // and re-offer the modal -- push the marker, then re-open so it
            // lands before the modal in the transcript the same way a real
            // conversation would. `ask_modal_state`/`intent_confirm_state`
            // already set `state.mode`; re-render is enough, no re-open
            // needed since the marker is transcript content, independent of
            // the modal itself.
            let rows = render(&state, 80, 24);
            let border_row = top_border_row(&rows).expect("modal must draw a bordered block");
            assert!(
                border_row > 3,
                "a short modal must not claim nearly the whole transcript area \
                 (border landed at row {border_row}): {rows:?}"
            );
            let above: String = rows[..border_row].join("\n");
            assert!(
                above.contains("TRANSCRIPT_MARKER_ABOVE_THE_MODAL"),
                "ordinary transcript text must remain visible above the modal: {above}"
            );
        }
    }

    /// V4 acceptance: the settings menu is bottom-anchored and content-sized
    /// too, on the same shared primitive -- ordinary transcript text stays
    /// visible above its short tree. The viewport is 40 rows tall rather
    /// than the usual 24 because the default tree is no longer 5 rows: the
    /// permissions group grew allow/deny/prompt review sections (three
    /// sub-group headers plus their honest empty-state rows), so "short"
    /// now means ~15 content rows -- still far from claiming a 40-row
    /// transcript area, which is what this test actually asserts.
    #[test]
    fn settings_menu_is_bottom_anchored_and_content_sized() {
        let mut state = AppState::new(AgentId::new());
        state.transcript.push(Entry::Assistant {
            text: "TRANSCRIPT_MARKER_ABOVE_THE_MODAL".to_string(),
            model: None,
            summary: None,
            ts: None,
        });
        state.open_settings();

        let rows = render(&state, 80, 40);
        let border_row =
            top_border_row(&rows).expect("the settings menu must draw a bordered block");

        assert!(
            border_row > 3,
            "a short tree must not claim nearly the whole transcript area \
             (border landed at row {border_row}): {rows:?}"
        );
        let above: String = rows[..border_row].join("\n");
        assert!(
            above.contains("TRANSCRIPT_MARKER_ABOVE_THE_MODAL"),
            "ordinary transcript text must remain visible above the settings menu: {above}"
        );
    }

    /// Acceptance: a long one grows to the cap and then SCROLLS rather than
    /// truncating -- the permission overlay's own huge-command tests already
    /// prove the hint survives and paging reveals the tail; this test proves
    /// the CAP ITSELF is honored (the overlay does not simply keep growing
    /// with the content the way the pre-V1 "nearly the whole area" shape
    /// did).
    #[test]
    fn a_long_permission_command_caps_its_height_instead_of_filling_the_screen() {
        let huge_rendered = format!("bash({})", "argument-chunk-".repeat(500));
        let state = awaiting_permission(&huge_rendered);

        let rows = render(&state, 80, 24);
        let top = top_border_row(&rows).expect("a huge command's overlay must still draw a border");
        // The modal's OWN bottom border, not the terminal's last row -- the
        // overlay is bottom-anchored to `transcript_area` only, which sits
        // above the (still-visible) input box and status line, not the
        // whole terminal.
        let bottom = rows[top..]
            .iter()
            .position(|row| row.trim_start().starts_with('└'))
            .map(|i| top + i)
            .expect("a huge command's overlay must still draw a closing border");
        let modal_height = bottom - top + 1;

        // `transcript_area.height` at 80x24 is 20 (24 - input(3) - status(1)),
        // and `modal::DEFAULT_CAP_DENOMINATOR` is 2, so the cap is 10 -- a
        // FAR cry from the ~23 rows the pre-V1 "claim nearly the whole area"
        // shape would have used for content this long.
        assert!(
            modal_height <= 10,
            "a capped modal must not grow anywhere near the full transcript \
             area for arbitrarily long content (modal_height={modal_height}): {rows:?}"
        );
        assert!(
            modal_height < 20,
            "sanity: the cap must be meaningfully smaller than the transcript area itself"
        );
    }

    // ---- P-10: every ported modal degrades without panicking on a
    // terminal too small for it. ----

    #[test]
    fn every_ported_modal_survives_a_tiny_terminal_without_panicking() {
        let permission = awaiting_permission("bash: ls");
        let ask = ask_modal_state("q", "a", None);
        let intent = intent_confirm_state(conway::SubagentMode::Spawn, None, "go");
        let mut help = AppState::new(AgentId::new());
        help.open_help();
        let mut settings = AppState::new(AgentId::new());
        settings.open_settings();

        for (name, state) in [
            ("permission", &permission),
            ("ask", &ask),
            ("intent", &intent),
            ("help", &help),
            ("settings", &settings),
        ] {
            for (w, h) in [(80u16, 1u16), (80, 2), (80, 3), (1, 24), (0, 0)] {
                let backend = TestBackend::new(w.max(1), h.max(1));
                let mut terminal = Terminal::new(backend).expect("terminal");
                terminal
                    .draw(|f| draw(state, f, &Theme::default()))
                    .unwrap_or_else(|e| panic!("{name} modal panicked/errored at {w}x{h}: {e}"));
            }
        }
    }

    // ---- V1: the shared `modal_scroll` field generalizes across every
    // modal-bearing surface, and decision-owed surfaces still queue in
    // priority order with it. ----

    #[test]
    fn modal_scroll_resets_when_a_queued_prompt_is_promoted() {
        let mut state = awaiting_permission("first: a long command that was scrolled far");
        state.modal_scroll = 40;

        let (second, _rx) = PendingPrompt::new_for_test(sample_request("second"));
        state.offer_prompt(second);
        // The second prompt queued behind the first -- resolving the first
        // promotes it, and the leftover scroll from the FIRST prompt's
        // overlay must not carry over onto the second's.
        state.resolve_current_prompt(PermissionDecision::AllowOnce);

        assert!(matches!(state.mode, Mode::AwaitingPermission(_)));
        assert_eq!(
            state.modal_scroll, 0,
            "a freshly promoted surface must not inherit the previous surface's scroll position"
        );
    }

    #[test]
    fn modal_scroll_resets_when_an_ask_modal_opens_immediately() {
        let mut state = AppState::new(AgentId::new());
        state.modal_scroll = 40;

        state.offer_ask_modal(crate::tui::state::AskModal {
            question: "q".to_string(),
            child: AgentId::new(),
            answer: "a".to_string(),
            error: None,
        });

        assert!(matches!(state.mode, Mode::AskModal(_)));
        assert_eq!(state.modal_scroll, 0);
    }

    #[test]
    fn modal_scroll_resets_when_help_opens() {
        let mut state = AppState::new(AgentId::new());
        state.modal_scroll = 40;

        state.open_help();

        assert_eq!(state.modal_scroll, 0);
    }
    /// V2b, end-to-end through the WIRED path: a command carrying shell
    /// metacharacters gets no pattern offer at all.
    ///
    /// The engine-level guarantee (a chained command is never authorized
    /// by a prefix grant) is proven in `conway-runtime`'s broker tests.
    /// This asserts the UI half: Conway does not even OFFER a grant it
    /// would then refuse to honor, because an offer that silently does
    /// nothing is worse than no offer.
    #[test]
    fn no_pattern_grant_is_offered_for_a_chained_command() {
        let state = awaiting_permission("git status && rm -rf /");
        let text = render_text(&state, 100, 24);

        // The OFFER markers specifically: the hint's `[p] pattern` key and
        // the `[p] grants:` breadth line. (The scope line's `[a]/[p]`
        // mention is always present -- it describes what the keys remember
        // at, not an offer.)
        assert!(
            !text.contains("[p] pattern") && !text.contains("[p] grants:"),
            "a chained command must not be offered a pattern grant: {text}"
        );
        assert!(
            text.contains("[y]") && text.contains("[n] deny"),
            "the ordinary decisions must still be available: {text}"
        );
        assert!(
            state.offered_permission_rule().is_none(),
            "and the state helper must agree with what was rendered"
        );
    }

    /// The narrow-by-default offer, verified where the operator sees it:
    /// approving `git status --short` must not silently authorize
    /// `git push`.
    #[test]
    fn the_offered_grant_is_the_narrow_two_token_prefix() {
        let state = awaiting_permission("git status --short");

        let rule = state
            .offered_permission_rule()
            .expect("a clean command gets an offer");
        assert_eq!(rule.command_prefix, "git status");
        assert!(!rule.matches("bash", "git push --force"));

        let text = render_text(&state, 100, 24);
        assert!(
            text.contains("git status"),
            "the prompt names the prefix it would grant: {text}"
        );
    }

    /// Axis A, end to end at the OFFER site: a `Structured` tool's
    /// JSON-dump rendering is full of shell metacharacters, but they are
    /// not shell risk -- the prompt must still offer a pattern grant, and
    /// the grant it offers must be the one shape F12's registration check
    /// admits against a `Structured` tool: the `tool:*` wildcard, stated
    /// in words. (Before `suggested_rule` took `render_kind`, this prompt
    /// silently showed no `[p]` at all.)
    #[test]
    fn a_structured_tools_prompt_offers_the_registrable_wildcard() {
        let (prompt, _rx) = PendingPrompt::new_for_test(sample_structured_request(
            r#"report({"summary":"build finished ok"})"#,
        ));
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.mode = Mode::AwaitingPermission(prompt);

        let rule = state
            .offered_permission_rule()
            .expect("a Structured tool must have an honest offer");
        assert_eq!(
            rule.command_prefix, "*",
            "the offer must be the wildcard -- a prefix over a JSON dump is \
             a registration error and must never be offered"
        );

        let text = render_text(&state, 100, 24);
        assert!(
            text.contains("[p] grants: any `report` call"),
            "the prompt must state the wildcard grant in words: {text}"
        );
    }

    /// Axis B at the render site: the prompt states which scope the
    /// remembered-grant keys (`a`/`p`) will grant at, and the words track
    /// the `s`-key cycle -- an operator never grants broader or narrower
    /// than what is on screen.
    #[test]
    fn the_prompt_states_the_grant_scope_and_tracks_the_cycle() {
        let mut state = awaiting_permission("git status --short");

        let session_text = render_text(&state, 100, 24);
        assert!(
            session_text.contains("remember for: this session"),
            "the default scope must be stated: {session_text}"
        );

        state.cycle_permission_grant_scope();
        let agent_text = render_text(&state, 100, 24);
        assert!(
            agent_text.contains("remember for: this agent only"),
            "one `s` press narrows to this agent: {agent_text}"
        );

        state.cycle_permission_grant_scope();
        let subtree_text = render_text(&state, 100, 24);
        assert!(
            subtree_text.contains("remember for: this agent and its subtree"),
            "a second `s` press widens to the subtree: {subtree_text}"
        );

        state.cycle_permission_grant_scope();
        assert_eq!(
            state.permission_grant_scope,
            conway::PermissionScope::Session,
            "a third `s` press wraps back to the session default"
        );
    }
}
