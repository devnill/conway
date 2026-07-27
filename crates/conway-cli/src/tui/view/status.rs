//! The bottom status line (WI-127 criterion 1): a single, always-visible
//! plain line -- no border -- summarizing mode, agent count, and the two
//! on-demand affordances (`/` for the command palette, `/agents` for the
//! agent-tree panel).

use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::theme::Theme;
use crate::tui::state::{Activity, AppState, Mode};

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let paragraph = Paragraph::new(Line::from(status_line(state))).style(theme.status_mode);
    frame.render_widget(paragraph, area);
}

/// Pure formatting, split out from [`draw`] so it is testable with no
/// `Frame`/terminal at all.
pub fn status_line(state: &AppState) -> String {
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
    // Board item 01KYAGP11FF9YC3G60TWHHKKST: the primary "is it working?"
    // signal, plus the focused agent's cumulative token spend -- both
    // always shown (unlike `focus_note`, which stays silent for the
    // overwhelmingly common root-focused case), since "is it working right
    // now" and "what has this conversation cost so far" are useful even
    // while focused on the root.
    let activity = activity_phrase(&state.activity);
    let tokens = spent_tokens(&state.focused_agent_usage);
    format!(
        " {mode} | {count} {noun} | {activity} | {tokens} tok | / for commands | {agents_hint}{focus_note} "
    )
}

/// A short, human-readable phrase for [`Activity`] (module doc's "primary
/// 'is it working?' signal").
fn activity_phrase(activity: &Activity) -> String {
    match activity {
        Activity::Idle => "idle".to_string(),
        Activity::Thinking => "thinking...".to_string(),
        Activity::Responding => "responding...".to_string(),
        Activity::RunningTool(name) => format!("running {name}..."),
        Activity::AwaitingPermission => "awaiting permission...".to_string(),
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
}
