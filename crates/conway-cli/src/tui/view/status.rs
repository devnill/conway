//! The bottom status line (T3): a single, always-visible plain line -- no
//! border -- summarizing the focused agent's turn at a glance. The line is
//! an ordered, configurable set of fields driven by `[tui.status_line]` in
//! `settings.json` (schema: `conway::config::schema::StatusLineConfig`).
//! Each field renders only when it is both listed in the configured `fields`
//! order AND has data to show (e.g. `git` is omitted when not in a repo,
//! `model` is omitted before the first `ModelDecision`).
//!
//! Default Lean line: `mode | model | ctx | tokens | activity | hint`.
//!
//! - `mode` -- `ready`/`awaiting permission`/`ask`/`intent` (the TUI's
//!   current top-level mode).
//! - `model` -- the focused agent's serving model display name from
//!   `Event::ModelDecision` (e.g. `anthropic/claude-sonnet-4-6`); omitted
//!   before the first turn routes.
//! - `ctx` -- context-window occupancy: `ctx 42%` when the focused model's
//!   max context is known from `[models.metadata_path]`, else the raw
//!   cumulative token estimate `ctx 12.3k`. The numerator is the cumulative
//!   sum of `Event::ContextSegmentAdded { tokens_est }` on the focused
//!   agent's stream (session-wide, NOT per-turn).
//! - `tokens` -- the focused agent's cumulative token spend as
//!   `<total> tok (<n%> cached)`, where `total` is the sum of every
//!   `Usage` field (input + output + both cache dimensions + reasoning)
//!   and `n%` is the cache hit rate `cache_read / (input + cache_read +
//!   cache_write)`. The parenthetical is omitted when its denominator is 0
//!   (no cache activity yet) -- the field then reads `<total> tok`.
//! - `activity` -- T2's working indicator: a braille spinner glyph plus the
//!   activity word plus live elapsed plus new-segment tokens added this
//!   turn, e.g. `⠋ thinking… 12s · +45 tok`, pulsing through
//!   `Theme::spinner_palette` on each 125ms tick. While idle: just `idle`.
//! - `hint` -- a persistent keybinding/affordance hint (T7 will reconcile):
//!   `Enter submit · Ctrl-E expand · ↑↓ history · PgUp/PgDn · /help · /agents to {view|hide}`,
//!   plus `focused: <id>` when the transcript is focused on a non-root
//!   agent.
//! - `git` -- the current `git rev-parse --abbrev-ref HEAD` branch, read
//!   once at startup; omitted when not in a git repo.
//! - `cwd` -- the session's working directory; omitted when unset.
//!
//! The whole line uses `theme.status_mode` (reversed) as its base style;
//! the activity spinner/phrase pulse and the dim `hint` field overlay
//! their own styles on top.

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

/// One orderable status-line field (T3). The configured `fields` list
/// (from `[tui.status_line]`) is parsed into this enum at render time;
/// unknown names are dropped (P-10: never a panic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusLineField {
    Mode,
    Model,
    Ctx,
    Tokens,
    Activity,
    Hint,
    Git,
    Cwd,
}

impl StatusLineField {
    /// Parses one configured field name. Unknown names return `None`
    /// (P-10: the caller drops them silently -- never a panic).
    fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "mode" => Some(Self::Mode),
            "model" => Some(Self::Model),
            "ctx" => Some(Self::Ctx),
            "tokens" => Some(Self::Tokens),
            "activity" => Some(Self::Activity),
            "hint" => Some(Self::Hint),
            "git" => Some(Self::Git),
            "cwd" => Some(Self::Cwd),
            _ => None,
        }
    }
}

/// Resolves the configured `fields` list into an ordered, validated
/// `Vec<StatusLineField>`, dropping unknown names (P-10). Falls back to
/// the default Lean order when the configured list is empty (an empty
/// `fields = []` would otherwise render a blank line -- treat that as
/// "user wanted defaults" rather than "user wanted nothing").
fn resolve_fields(config: &conway::config::schema::StatusLineConfig) -> Vec<StatusLineField> {
    let parsed: Vec<StatusLineField> = config
        .fields
        .iter()
        .filter_map(|name| StatusLineField::parse(name))
        .collect();
    if parsed.is_empty() {
        // Empty / all-unknown config: fall back to the Lean order rather
        // than rendering a blank line (P-10: bad input never produces a
        // broken UI -- it falls back to defaults).
        resolve_fields(&conway::config::schema::StatusLineConfig::default())
    } else {
        parsed
    }
}

/// Builds the status line as a styled [`Line`] (T3): an ordered,
/// configurable field set joined by ` | `, each field rendered only when
/// present+enabled, all under the `theme.status_mode` base style. The
/// `activity` field (T2) overlays its spinner pulse color and dim
/// elapsed/tokens tail; the `hint` field overlays `theme.status_dim`.
pub fn status_line_spans(state: &AppState, theme: &Theme) -> Line<'static> {
    let fields = resolve_fields(&state.status_line_config);
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::raw(" "));
    let mut first = true;
    for field in fields {
        let field_spans = render_field(field, state, theme);
        if field_spans.is_empty() {
            continue;
        }
        if !first {
            spans.push(Span::raw(" | "));
        }
        first = false;
        spans.extend(field_spans);
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// Renders one field's spans. Returns an empty `Vec` when the field is
/// absent (no data to show) so the caller skips it without leaving a
/// dangling separator.
fn render_field(
    field: StatusLineField,
    state: &AppState,
    theme: &Theme,
) -> Vec<Span<'static>> {
    match field {
        StatusLineField::Mode => vec![Span::raw(mode_label(&state.mode))],
        StatusLineField::Model => state
            .focused_model
            .as_deref()
            .map(|name| vec![Span::raw(name.to_string())])
            .unwrap_or_default(),
        StatusLineField::Ctx => vec![Span::raw(ctx_label(state))],
        StatusLineField::Tokens => vec![Span::raw(tokens_label(state))],
        StatusLineField::Activity => activity_spans(state, theme),
        StatusLineField::Hint => hint_spans(state, theme),
        StatusLineField::Git => state
            .git_branch
            .as_deref()
            .map(|b| vec![Span::raw(b.to_string())])
            .unwrap_or_default(),
        StatusLineField::Cwd => state
            .cwd_display
            .as_deref()
            .map(|c| vec![Span::raw(c.to_string())])
            .unwrap_or_default(),
    }
}

/// The `mode` field's text.
fn mode_label(mode: &Mode) -> String {
    match mode {
        Mode::Normal => "ready".to_string(),
        Mode::AwaitingPermission(_) => "awaiting permission".to_string(),
        // B5: the /ask modal owns the screen -- the status line says so.
        Mode::AskModal(_) => "ask".to_string(),
        // C2: the NL intent confirmation card owns the screen.
        Mode::IntentConfirm(_) => "intent".to_string(),
    }
}

/// The `ctx` field's text: `ctx 42%` when the focused model's max context
/// is known, else `ctx 12.3k` (raw tokens, compact-suffixed). Guards
/// divide-by-zero on the max.
///
/// The `pct.min(100)` cap below is a DELIBERATE lossy clamp, not just noise
/// avoidance: `focused_ctx_tokens` is a segment-id-deduped estimate, and an
/// estimate that exceeds the declared `max_context_tokens` (headroom,
/// rounding, a metadata file that under-declares the real window) is shown
/// as `ctx 100%` rather than `ctx 137%` so the status line never looks like
/// a bug to the user. The tradeoff is that this CAN hide a genuine overshoot
/// -- an agent whose context really has grown past its declared max still
/// reads `ctx 100%`, not `ctx 137%`. That is accepted here: the authoritative
/// token total lands via the turn-end summary (T4), and a proper re-fetch of
/// the runtime's true context total on focus is tracked as a separate
/// follow-up board item. No behavior change vs. the original cap -- only the
/// intent is now documented.
///
/// `pub(super)` (T6): the sticky context header (`view/header.rs`) shows the
/// same `ctx%`/raw-tokens figure and reuses this function directly rather
/// than recomputing the percentage formula a second time, so the header and
/// the status line's `ctx` field can never drift apart on the cap/fallback
/// logic.
pub(super) fn ctx_label(state: &AppState) -> String {
    match state.focused_model_max_context {
        Some(max) if max > 0 => {
            let pct = (state.focused_ctx_tokens * 100) / u64::from(max);
            // Deliberate lossy clamp -- see the doc comment above.
            let pct = pct.min(100);
            format!("ctx {pct}%")
        }
        _ => format!("ctx {}", compact_tokens(state.focused_ctx_tokens)),
    }
}

/// The `tokens` field's text: `<total> tok (<n%> cached)` when the cache
/// denominator is non-zero, else `<total> tok`. `total` is the sum of
/// every `Usage` field (input + output + both cache dimensions +
/// reasoning); the cache hit rate is `cache_read / (input + cache_read +
/// cache_write)`.
fn tokens_label(state: &AppState) -> String {
    let usage = &state.focused_agent_usage;
    let total = spent_tokens(usage);
    let denom = u64::from(usage.input_tokens)
        + u64::from(usage.cache_read_tokens)
        + u64::from(usage.cache_write_tokens);
    if denom == 0 || usage.cache_read_tokens == 0 {
        return format!("{total} tok");
    }
    let pct = (u64::from(usage.cache_read_tokens) * 100) / denom;
    format!("{total} tok ({pct}% cached)")
}

/// The `activity` field's spans (T2): spinner glyph + activity word in the
/// current pulse color, plus live elapsed + new-segment tokens added this
/// turn (dim) while active; just `idle` while idle.
fn activity_spans(state: &AppState, theme: &Theme) -> Vec<Span<'static>> {
    if !should_animate(&state.activity) {
        return vec![Span::raw("idle")];
    }
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
    let elapsed = state
        .turn_started_at
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0);
    vec![
        Span::styled(format!("{glyph} {phrase}"), pulse),
        Span::styled(
            format!(" {elapsed}s · +{} tok", state.turn_running_tokens),
            theme.status_dim,
        ),
    ]
}

/// The `hint` field's spans (T3): a persistent keybinding/affordance hint,
/// rendered dim. Includes the `/agents` toggle affordance and, when the
/// transcript is focused on a non-root agent, a `focused: <id>` note.
fn hint_spans(state: &AppState, theme: &Theme) -> Vec<Span<'static>> {
    let agents_hint = if state.agent_view_open {
        "/agents to hide"
    } else {
        "/agents to view"
    };
    let mut hint = format!(
        "Enter submit · Ctrl-E expand · ↑↓ history · PgUp/PgDn · /help · /thinking · /timestamps · {agents_hint}"
    );
    // WI-140: name which agent's conversation is currently shown whenever
    // it is not the root -- the root case stays silent (an always-on
    // "focused: root" would be noise for the overwhelmingly common case).
    if !state.is_root_focused() {
        hint.push_str(&format!(" · focused: {}", state.focused_agent));
    }
    vec![Span::styled(hint, theme.status_dim)]
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

/// Compact token-count formatting for the `ctx` field's raw-tokens
/// fallback (unknown max context): `< 1000` renders as-is, `>= 1000`
/// renders as `{k}.{tenths}k` (e.g. `12345` -> `12.3k`). Keeps the field
/// short for very large context windows.
fn compact_tokens(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    let k = n / 1000;
    let tenths = (n % 1000) / 100;
    format!("{k}.{tenths}k")
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use conway::config::schema::StatusLineConfig;
    use conway::AgentId;

    use super::*;

    fn cfg(fields: &[&str]) -> StatusLineConfig {
        StatusLineConfig {
            fields: fields.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn status_line_reports_ready_by_default() {
        let state = AppState::new(AgentId::new());
        let line = status_line(&state);
        assert!(line.contains("ready"));
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
        assert!(!status_line(&state).contains("focused:"));
    }

    #[test]
    fn status_line_names_the_focused_agent_once_switched_off_root() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.focus_agent(child);
        let line = status_line(&state);
        assert!(line.contains("focused:"));
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
        // The idle activity slot is just `idle` -- no `+`-prefixed
        // new-token figure and no `<Ns> ·` elapsed prefix. (The hint field
        // itself uses ` · ` as a separator between affordances, so we
        // assert against the activity-specific `· +` pattern instead of
        // the bare ` · ` the T2 test used before the hint grew `·`.)
        assert!(
            !line.contains("· +"),
            "no `· +N tok` new-segment figure while idle: {line}"
        );
        // No `Ns ·` elapsed prefix: a `\d+s ·` pattern only the activity
        // field's elapsed tail produces.
        assert!(
            !line.contains("s · +"),
            "no `Ns · +N tok` elapsed/tail while idle: {line}"
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

    // ---- T3: ordered configurable field set ----

    #[test]
    fn default_field_order_is_mode_model_ctx_tokens_activity_hint() {
        // The default Lean line: mode | model | ctx | tokens | activity | hint.
        // `model` is omitted before the first ModelDecision, so the default
        // state's line is `ready | ctx 0% | 0 tok | idle | <hint>`.
        let state = AppState::new(AgentId::new());
        let line = status_line(&state);
        let ready = line.find("ready").unwrap();
        let ctx = line.find("ctx").unwrap();
        let tok = line.find("0 tok").unwrap();
        let idle = line.find("idle").unwrap();
        let hint = line.find("Ctrl-E").unwrap();
        // `model` is omitted (no ModelDecision yet) -- assert it's absent.
        assert!(!line.contains("anthropic/"));
        // Order: ready < ctx < tok < idle < hint.
        assert!(ready < ctx, "mode precedes ctx: {line}");
        assert!(ctx < tok, "ctx precedes tokens: {line}");
        assert!(tok < idle, "tokens precedes activity: {line}");
        assert!(idle < hint, "activity precedes hint: {line}");
    }

    #[test]
    fn each_enabled_field_renders_in_configured_order() {
        // Reverse the order and add git/cwd -- every present field must
        // appear, in the configured order.
        let mut state = AppState::new(AgentId::new());
        state.focused_model = Some("anthropic/claude-sonnet-4-6".to_string());
        state.focused_model_max_context = Some(200_000);
        state.focused_ctx_tokens = 50_000; // 25%
        state.git_branch = Some("main".to_string());
        state.cwd_display = Some("/home/user/conway".to_string());
        state.status_line_config = cfg(&["cwd", "git", "hint", "activity", "tokens", "ctx", "model", "mode"]);

        let line = status_line(&state);
        let cwd = line.find("/home/user/conway").unwrap();
        let git = line.find("main").unwrap();
        let hint = line.find("Ctrl-E").unwrap();
        let idle = line.find("idle").unwrap();
        let tok = line.find("0 tok").unwrap();
        let ctx = line.find("ctx 25%").unwrap();
        let model = line.find("anthropic/claude-sonnet-4-6").unwrap();
        let mode = line.find("ready").unwrap();
        assert!(cwd < git, "{line}");
        assert!(git < hint, "{line}");
        assert!(hint < idle, "{line}");
        assert!(idle < tok, "{line}");
        assert!(tok < ctx, "{line}");
        assert!(ctx < model, "{line}");
        assert!(model < mode, "{line}");
    }

    #[test]
    fn disabled_field_is_omitted() {
        // Drop `tokens` from the configured fields -- it must NOT render.
        let mut state = AppState::new(AgentId::new());
        state.focused_agent_usage = conway::Usage {
            input_tokens: 100,
            ..Default::default()
        };
        state.status_line_config = cfg(&["mode", "activity"]);
        let line = status_line(&state);
        assert!(line.contains("ready"));
        assert!(line.contains("idle"));
        assert!(
            !line.contains("100 tok"),
            "a disabled field must not render: {line}"
        );
        assert!(
            !line.contains("ctx"),
            "a disabled field must not render: {line}"
        );
    }

    #[test]
    fn missing_git_field_is_omitted_gracefully() {
        // No git branch set -> `git` field is omitted even when configured.
        let mut state = AppState::new(AgentId::new());
        state.status_line_config = cfg(&["mode", "git", "activity"]);
        let line = status_line(&state);
        assert!(line.contains("ready"));
        assert!(line.contains("idle"));
        // No dangling separator where `git` would have been: the line has
        // exactly one ` | ` (between mode and activity).
        assert_eq!(
            line.matches(" | ").count(),
            1,
            "missing field must not leave a dangling separator: {line}"
        );
    }

    #[test]
    fn missing_model_field_is_omitted_before_first_model_decision() {
        let mut state = AppState::new(AgentId::new());
        state.status_line_config = cfg(&["mode", "model", "activity"]);
        let line = status_line(&state);
        assert!(line.contains("ready"));
        assert!(line.contains("idle"));
        assert!(
            !line.contains("anthropic/"),
            "model must be omitted before the first ModelDecision: {line}"
        );
        assert_eq!(
            line.matches(" | ").count(),
            1,
            "omitted model must not leave a dangling separator: {line}"
        );
    }

    #[test]
    fn tokens_field_with_cache_data_renders_cache_percentage() {
        let mut state = AppState::new(AgentId::new());
        state.focused_agent_usage = conway::Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 300,
            cache_write_tokens: 100,
            reasoning_tokens: 0,
        };
        // total = 100 + 50 + 300 + 100 + 0 = 550
        // cache% = 300 / (100 + 300 + 100) = 300 / 500 = 60%
        let line = status_line(&state);
        assert!(
            line.contains("550 tok (60% cached)"),
            "tokens field must render total + cache%%: {line}"
        );
    }

    #[test]
    fn tokens_field_without_cache_data_renders_bare_total() {
        let mut state = AppState::new(AgentId::new());
        state.focused_agent_usage = conway::Usage {
            input_tokens: 100,
            output_tokens: 23,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 5,
        };
        // No cache denominator -> bare total, no parenthetical.
        let line = status_line(&state);
        assert!(
            line.contains("128 tok"),
            "tokens field must render bare total: {line}"
        );
        assert!(
            !line.contains("cached"),
            "no `cached` parenthetical without cache activity: {line}"
        );
    }

    #[test]
    fn tokens_field_cache_only_write_still_no_parenthetical() {
        // cache_write but no cache_read -> cache_read is 0, so the
        // parenthetical is suppressed (no hits to report a rate from).
        let mut state = AppState::new(AgentId::new());
        state.focused_agent_usage = conway::Usage {
            input_tokens: 100,
            cache_write_tokens: 200,
            ..Default::default()
        };
        let line = status_line(&state);
        assert!(line.contains("300 tok"), "total renders: {line}");
        assert!(
            !line.contains("cached"),
            "no `cached` parenthetical when cache_read is 0: {line}"
        );
    }

    #[test]
    fn ctx_field_renders_percentage_when_max_known() {
        let mut state = AppState::new(AgentId::new());
        state.focused_model_max_context = Some(200_000);
        state.focused_ctx_tokens = 50_000; // 25%
        let line = status_line(&state);
        assert!(line.contains("ctx 25%"), "{line}");
    }

    #[test]
    fn ctx_field_renders_raw_tokens_when_max_unknown() {
        let mut state = AppState::new(AgentId::new());
        state.focused_ctx_tokens = 12_345;
        let line = status_line(&state);
        assert!(
            line.contains("ctx 12.3k"),
            "raw-tokens fallback must compact-format: {line}"
        );
        assert!(!line.contains("ctx 12.3k%"), "{line}");
    }

    #[test]
    fn ctx_field_caps_at_100_percent_when_estimate_exceeds_max() {
        let mut state = AppState::new(AgentId::new());
        state.focused_model_max_context = Some(1000);
        state.focused_ctx_tokens = 5_000; // would be 500%
        let line = status_line(&state);
        assert!(
            line.contains("ctx 100%"),
            "ctx%% must cap at 100, not show 500%%: {line}"
        );
    }

    #[test]
    fn ctx_field_small_count_renders_without_suffix() {
        let mut state = AppState::new(AgentId::new());
        state.focused_ctx_tokens = 750;
        // Drop the `tokens` field so the only `tok`/`k`-bearing text is the
        // ctx field itself -- the hint and tokens field both contain `k`.
        state.status_line_config = cfg(&["mode", "ctx", "activity"]);
        let line = status_line(&state);
        assert!(line.contains("ctx 750"), "{line}");
        assert!(
            !line.contains("ctx 750k"),
            "small counts must not get a `k` suffix: {line}"
        );
    }

    #[test]
    fn override_reorders_and_hides_fields() {
        // A custom order that drops `activity` and `hint`, putting `tokens`
        // first.
        let mut state = AppState::new(AgentId::new());
        state.focused_agent_usage = conway::Usage {
            input_tokens: 10,
            ..Default::default()
        };
        state.status_line_config = cfg(&["tokens", "mode", "ctx"]);
        let line = status_line(&state);
        let tok = line.find("10 tok").unwrap();
        let mode = line.find("ready").unwrap();
        let ctx = line.find("ctx").unwrap();
        assert!(tok < mode, "{line}");
        assert!(mode < ctx, "{line}");
        assert!(!line.contains("idle"), "activity dropped: {line}");
        assert!(!line.contains("Ctrl-E"), "hint dropped: {line}");
    }

    #[test]
    fn unknown_field_names_are_dropped_silently() {
        // P-10: unknown names never panic; the known ones still render.
        let mut state = AppState::new(AgentId::new());
        state.status_line_config = cfg(&["mode", "bogus", "nonsense", "activity"]);
        let line = status_line(&state);
        assert!(line.contains("ready"));
        assert!(line.contains("idle"));
        assert!(!line.contains("bogus"));
    }

    #[test]
    fn empty_fields_falls_back_to_default_order() {
        // P-10: an empty `fields` list falls back to the Lean order rather
        // than rendering a blank line.
        let mut state = AppState::new(AgentId::new());
        state.status_line_config = cfg(&[]);
        let line = status_line(&state);
        assert!(line.contains("ready"));
        assert!(line.contains("idle"));
        assert!(line.contains("Ctrl-E"));
    }

    #[test]
    fn hint_field_includes_keybinding_hint() {
        let state = AppState::new(AgentId::new());
        let line = status_line(&state);
        assert!(line.contains("Enter submit"), "{line}");
        assert!(line.contains("Ctrl-E expand"), "{line}");
        assert!(line.contains("/help"), "{line}");
        assert!(line.contains("↑↓ history"), "{line}");
        assert!(line.contains("PgUp/PgDn"), "{line}");
        // T4: the new toggles are surfaced in the hint.
        assert!(line.contains("/thinking"), "{line}");
        assert!(line.contains("/timestamps"), "{line}");
    }

    #[test]
    fn git_and_cwd_fields_render_when_set() {
        let mut state = AppState::new(AgentId::new());
        state.git_branch = Some("feature-branch".to_string());
        state.cwd_display = Some("/Users/dan/conway".to_string());
        state.status_line_config = cfg(&["mode", "git", "cwd", "activity"]);
        let line = status_line(&state);
        assert!(line.contains("feature-branch"), "{line}");
        assert!(line.contains("/Users/dan/conway"), "{line}");
    }
}