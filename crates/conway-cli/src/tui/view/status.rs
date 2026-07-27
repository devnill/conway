//! The bottom status line (WI-127 criterion 1): a single, always-visible
//! plain line -- no border -- summarizing mode, agent count, and the two
//! on-demand affordances (`/` for the command palette, `/agents` for the
//! agent-tree panel).
//!
//! T2 adds a working indicator to the activity slot. While the focused
//! agent is working, the slot renders a braille spinner glyph plus the
//! activity word plus live elapsed plus the new context tokens added this
//! turn, e.g. `⠋ thinking… 12s · +45 tok`. The glyph and the word both
//! pulse through a small theme palette (`Theme::spinner_palette`) on each
//! 125ms tick. The pulse is element-level (spinner glyph + activity word
//! share one pulse color per frame), NOT per-character TextShimmer (out of
//! scope per the T2 spec).

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::theme::Theme;
use crate::tui::state::{should_animate, Activity, AppState, Mode, SPINNER_FRAMES};

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let line = status_line_spans(state, theme);
    let paragraph = Paragraph::new(line).style(theme.status_mode);
    frame.render_widget(paragraph, area);
}

/// Pure formatting, split out from [`draw`] so it is testable with no
/// `Frame`/terminal at all. Returns the plain-text content of the status
/// line (no styling) -- the styled path ([`status_line_spans`]) is what
/// [`draw`] actually renders, and the two share the same text content via
/// [`flatten`] so they can never drift apart. Test-only: the production
/// render path uses [`status_line_spans`] directly, but this plain-text
/// view stays the most ergonomic seam for the existing `contains(..)`
/// status-line tests.
#[cfg(test)]
pub fn status_line(state: &AppState) -> String {
    let line = status_line_spans(state, &Theme::default());
    flatten(&line)
}

/// Flattens a `Line`'s spans into one plain string -- used by
/// [`status_line`] to keep the plain-text path in lockstep with the styled
/// path without duplicating the formatting logic.
#[cfg(test)]
fn flatten(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Builds the status line as a styled [`Line`] (T2): the leading ` {mode} |
/// {count} {noun} | ` is rendered with the default `status_mode` style
/// (reversed, no fg), the working indicator (spinner glyph + activity word)
/// is rendered in the current pulse color from `Theme::spinner_palette`, the
/// live `elapsed · tokens` tail is rendered dim, and the trailing
/// ` | {tokens} tok | / for commands | {agents_hint}{focus_note} ` is
/// rendered with the default style again. While idle the activity slot is
/// just `idle` (no spinner, no elapsed/tokens) -- the spinner only appears
/// while [`should_animate`] is true.
pub fn status_line_spans(state: &AppState, theme: &Theme) -> Line<'static> {
    let mode = match state.mode {
        Mode::Normal => "ready",
        Mode::AwaitingPermission(_) => "awaiting permission",
        // B5: the /ask modal owns the screen -- the status line says so.
        Mode::AskModal(_) => "ask",
        // C2: the NL intent confirmation card owns the screen.
        Mode::IntentConfirm(_) => "intent",
    };
    let count = state.tree.nodes.len();
    let noun = if count == 1 { "agent" } else { "agents" };
    let agents_hint = if state.agent_view_open {
        "/agents to hide"
    } else {
        "/agents to view"
    };
    // WI-140: name which agent's conversation is currently shown whenever
    // it is not the root -- the root case stays silent (an always-on
    // "focused: root" would be noise for the overwhelmingly common case of
    // never having switched at all).
    let focus_note = if state.is_root_focused() {
        String::new()
    } else {
        format!(" | focused: {}", state.focused_agent)
    };
    // Board item 01KYAGP11FF9YC3G60TWHHKKST: the focused agent's cumulative
    // token spend -- always shown (unlike `focus_note`, which stays silent
    // for the root case), since "what has this conversation cost so far" is
    // useful even while focused on the root.
    let tokens = spent_tokens(&state.focused_agent_usage);

    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::raw(format!(" {mode} | {count} {noun} | ")));

    // T2 working indicator: spinner glyph + activity word in the current
    // pulse color, plus live elapsed + new-segment tokens added this turn
    // (dim, prefixed `+`) while active. While idle the slot is just `idle`
    // (default style).
    if should_animate(&state.activity) {
        let palette = theme.spinner_palette();
        let pulse = palette
            .get(state.spinner_color_idx % palette.len())
            .copied()
            .unwrap_or(theme.spinner);
        let glyph = SPINNER_FRAMES
            .get(state.spinner_frame % SPINNER_FRAMES.len())
            .copied()
            .unwrap_or("");
        let phrase = activity_phrase(&state.activity);
        // Spinner glyph + phrase both in the pulse color -- the subtle
        // element-level contrast shift the T2 spec calls for.
        spans.push(Span::styled(format!("{glyph} {phrase}"), pulse));
        // Live elapsed + new-segment tokens added this turn, dim. `elapsed`
        // is computed against `Instant::now()`; a `None` `turn_started_at`
        // (e.g. an activity set without a `TurnStarted` -- the
        // `app.rs::submit` immediate-`Thinking` path before the event
        // round-trips) shows 0s. The token figure is prefixed with `+` to
        // signal "added this turn" (session-deduped segment deltas -- NOT
        // total context occupancy; the authoritative turn-end token total
        // lands via the turn-end summary, T4) and to visually distinguish it
        // from the cumulative `| {tokens} tok |` slot to the right.
        let elapsed = state
            .turn_started_at
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        spans.push(Span::styled(
            format!(" {elapsed}s · +{} tok", state.turn_running_tokens),
            theme.status_dim,
        ));
    } else {
        spans.push(Span::raw("idle"));
    }

    spans.push(Span::raw(format!(
        " | {tokens} tok | / for commands | {agents_hint}{focus_note} "
    )));
    Line::from(spans)
}

/// A short, human-readable phrase for [`Activity`] (module doc's "primary
/// 'is it working?' signal"). Uses an ellipsis (`…`) for the working states,
/// matching the T2 spec's `⠋ thinking…` shape.
fn activity_phrase(activity: &Activity) -> String {
    match activity {
        Activity::Idle => "idle".to_string(),
        Activity::Thinking => "thinking…".to_string(),
        Activity::Responding => "responding…".to_string(),
        Activity::RunningTool(name) => format!("running {name}…"),
        Activity::AwaitingPermission => "awaiting permission…".to_string(),
    }
}

/// The focused agent's cumulative token spend as one plain integer: every
/// `Usage` field summed (input + output + both cache dimensions +
/// reasoning) -- all of them are tokens the model actually processed for
/// this agent's own turns, not a single privileged subset. Deliberately NOT
/// `ContextReport.total_tokens_est` (that number is context-WINDOW
/// occupancy, a different question -- see `SessionHandle::session_usage`'s
/// own doc).
fn spent_tokens(usage: &conway::Usage) -> u64 {
    u64::from(usage.input_tokens)
        + u64::from(usage.output_tokens)
        + u64::from(usage.cache_read_tokens)
        + u64::from(usage.cache_write_tokens)
        + u64::from(usage.reasoning_tokens)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use conway::AgentId;

    use super::*;

    #[test]
    fn status_line_reports_ready_and_one_agent_by_default() {
        let state = AppState::new(AgentId::new());
        let line = status_line(&state);
        assert!(line.contains("ready"));
        assert!(line.contains("1 agent"));
        assert!(!line.contains("1 agents"));
    }

    #[test]
    fn status_line_reflects_agent_view_toggle() {
        let mut state = AppState::new(AgentId::new());
        assert!(status_line(&state).contains("/agents to view"));
        state.toggle_agent_view();
        assert!(status_line(&state).contains("/agents to hide"));
    }

    // WI-140: the focused agent must be clearly indicated.
    #[test]
    fn status_line_says_nothing_extra_while_focused_on_root() {
        let state = AppState::new(AgentId::new());
        assert!(!status_line(&state).contains("focused"));
    }

    #[test]
    fn status_line_names_the_focused_agent_once_switched_off_root() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.focus_agent(child);
        let line = status_line(&state);
        assert!(line.contains("focused"));
        assert!(line.contains(&child.to_string()));
    }

    // ---- Board item 01KYAGP11FF9YC3G60TWHHKKST: activity indicator + token
    // spend, both scoped to the focused agent. ----

    #[test]
    fn status_line_shows_idle_by_default() {
        let state = AppState::new(AgentId::new());
        assert!(status_line(&state).contains("idle"));
    }

    #[test]
    fn status_line_reflects_every_activity_state() {
        let mut state = AppState::new(AgentId::new());

        state.activity = Activity::Thinking;
        assert!(status_line(&state).contains("thinking"));

        state.activity = Activity::Responding;
        assert!(status_line(&state).contains("responding"));

        state.activity = Activity::RunningTool("bash".to_string());
        let line = status_line(&state);
        assert!(line.contains("running"));
        assert!(line.contains("bash"));

        state.activity = Activity::AwaitingPermission;
        assert!(status_line(&state).contains("awaiting permission"));

        state.activity = Activity::Idle;
        assert!(status_line(&state).contains("idle"));
    }

    #[test]
    fn status_line_reports_zero_tokens_by_default() {
        let state = AppState::new(AgentId::new());
        assert!(status_line(&state).contains("0 tok"));
    }

    #[test]
    fn status_line_reports_the_focused_agents_cumulative_token_spend() {
        let mut state = AppState::new(AgentId::new());
        state.focused_agent_usage = conway::Usage {
            input_tokens: 100,
            output_tokens: 23,
            cache_read_tokens: 2,
            cache_write_tokens: 0,
            reasoning_tokens: 5,
        };
        // 100 + 23 + 2 + 0 + 5
        assert!(status_line(&state).contains("130 tok"));
    }

    #[test]
    fn focusing_a_different_agent_resets_the_token_figure_and_activity() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.activity = Activity::Responding;
        state.focused_agent_usage = conway::Usage {
            input_tokens: 50,
            ..Default::default()
        };

        state.focus_agent(AgentId::new());

        assert_eq!(state.activity, Activity::Idle);
        assert_eq!(state.focused_agent_usage, conway::Usage::default());
        assert!(status_line(&state).contains("0 tok"));
        assert!(status_line(&state).contains("idle"));
    }

    // ---- T2: spinner + elapsed + new-segment tokens added this turn ----

    #[test]
    fn status_line_shows_elapsed_and_running_tokens_while_active() {
        let mut state = AppState::new(AgentId::new());
        state.activity = Activity::Thinking;
        // 12s ago -- the elapsed renderer computes `Instant::now() - turn_started_at`.
        state.turn_started_at = Some(Instant::now() - Duration::from_secs(12));
        state.turn_running_tokens = 320;

        let line = status_line(&state);
        assert!(
            line.contains("12s"),
            "the working indicator must render live elapsed seconds while active: {line}"
        );
        assert!(
            line.contains("+320 tok"),
            "the working indicator must render the new-segment tokens with a `+` prefix while active: {line}"
        );
        assert!(
            line.contains("thinking"),
            "the activity word must still be present: {line}"
        );
        // A spinner glyph from the braille sequence must lead the activity
        // phrase.
        assert!(
            SPINNER_FRAMES.iter().any(|g| line.contains(g)),
            "the spinner glyph must precede the activity phrase: {line}"
        );
    }

    #[test]
    fn status_line_shows_no_elapsed_or_running_tokens_while_idle() {
        let state = AppState::new(AgentId::new());
        let line = status_line(&state);
        // No "Ns ·" elapsed pattern and no spinner glyph while idle.
        assert!(
            !SPINNER_FRAMES.iter().any(|g| line.contains(g)),
            "no spinner glyph while idle: {line}"
        );
        // The idle slot is just `idle` -- no `·` separator from the
        // elapsed/tokens tail, and no `+`-prefixed new-token figure.
        assert!(
            !line.contains(" · "),
            "no elapsed/tokens separator while idle: {line}"
        );
        assert!(
            !line.contains("· +"),
            "no `· +N tok` new-segment figure while idle: {line}"
        );
    }

    #[test]
    fn status_line_pulse_color_picks_from_the_theme_palette() {
        // Drive the styled path directly: with a non-default palette, the
        // spinner+phrase span's fg must come from the palette at the current
        // spinner_color_idx. With spinner_color_idx = 1, the palette[1] is
        // `spinner_b`.
        let mut state = AppState::new(AgentId::new());
        state.activity = Activity::Thinking;
        state.spinner_color_idx = 1;
        state.spinner_frame = 0;

        let theme = Theme {
            spinner_b: ratatui::style::Style::default().fg(ratatui::style::Color::Cyan),
            ..Theme::default()
        };

        let line = status_line_spans(&state, &theme);
        // Find the span whose content starts with the spinner glyph and
        // assert its fg is the palette[1] color (Cyan), not the default
        // Yellow.
        let glyph = SPINNER_FRAMES[0];
        let pulse_span = line
            .spans
            .iter()
            .find(|s| s.content.as_ref().starts_with(glyph))
            .expect("a spinner-leading span must be present while active");
        assert_eq!(
            pulse_span.style.fg,
            Some(ratatui::style::Color::Cyan),
            "the spinner glyph + activity word must use the palette color at spinner_color_idx"
        );
    }

    #[test]
    fn status_line_pulse_color_wraps_past_the_palette_end() {
        // spinner_color_idx past the palette length wraps back to palette[0]
        // (the renderer does `idx % palette.len()`).
        let mut state = AppState::new(AgentId::new());
        state.activity = Activity::Thinking;
        state.spinner_color_idx = 3; // palette len is 3 -> wraps to 0.

        let theme = Theme {
            spinner: ratatui::style::Style::default().fg(ratatui::style::Color::Red),
            ..Theme::default()
        };

        let line = status_line_spans(&state, &theme);
        let glyph = SPINNER_FRAMES[state.spinner_frame % SPINNER_FRAMES.len()];
        let pulse_span = line
            .spans
            .iter()
            .find(|s| s.content.as_ref().starts_with(glyph))
            .expect("a spinner-leading span must be present while active");
        assert_eq!(
            pulse_span.style.fg,
            Some(ratatui::style::Color::Red),
            "spinner_color_idx past the palette end must wrap to palette[0]"
        );
    }
}
