//! The TUI's central color/style system (T1, the v0.3.0 polish enabler).
//!
//! Before T1, every `view/*.rs` hand-rolled `Style::default().fg(Color::…)`
//! inline at each call site. T1 replaces that with a single [`Theme`] struct
//! holding one named [`Style`] per concern, threaded through `view::draw`
//! and each per-view `draw` fn as `&Theme` (decision D-T1: injected, not
//! re-fetched via a call-site accessor or a global `Lazy`). The view files
//! now read `theme.<name>` instead of building a `Style` inline.
//!
//! ## Configurability
//!
//! The theme is **configurable from the start**: a `[tui.theme]` table in
//! `settings.json` (schema: `conway::config::schema::ThemeConfig`) overlays
//! per-slot `fg`/`bg`/`modifiers` on top of the defaults. [`Theme::from_config`]
//! loads that overlay; [`Theme::default`] is what you get when `[tui.theme]`
//! is absent entirely. The defaults match the exact `(Color, Modifier)` pairs
//! the view files used pre-T1, so an unconfigured TUI renders identically
//! (visual parity is an acceptance criterion for this refactor).
//!
//! ## P-10: config is untrusted input
//!
//! [`Theme::from_config`] never panics on a malformed `[tui.theme]` value: an
//! unparseable color name, an unknown modifier, or an out-of-range hex code
//! is mapped back to the default for that slot (the named style's built-in
//! `Color`/`Modifier`), not the whole theme -- a single typo'd `fg` on one
//! slot does not silently wipe another slot's override. See
//! `docs/crates/conway-cli.md`'s `[tui.theme]` section for the accepted
//! color/modifier spellings.
//!
//! ## New accent styles
//!
//! Two slots have no pre-T1 call site and are defined here for T4 (transcript
//! provenance) to consume later: [`Theme::assistant_marker`] (a distinct
//! accent for the assistant's turn marker) and [`Theme::reasoning`] (dim
//! italic, for reasoning-trace text). Their defaults are picked to fit
//! today's palette, not to change it.
//!
//! ## V7: what each color MEANS
//!
//! T1 assembled the named-style table; V7 is the pass that asks *why* each
//! slot is the color it is, and writes the answer down so the next slot
//! added here has a rule to follow instead of a nearest-neighbor guess. The
//! full rationale (with examples) lives in `docs/crates/conway-cli.md`'s
//! `[tui.theme]` section; the compressed version:
//!
//! - **Red is failure or active danger, never decoration.** `error`,
//!   `tool_failed`, `agent_failed`, `border_danger`, and `fatal_error` are
//!   the only red slots. `fatal_error` (Red+Bold) is V7's one addition to
//!   what red *covers*: it is now the AUTO-ALLOW indicator in the status
//!   line (`view/status.rs`), not just a reserved fatal-runtime-error
//!   accent -- both are "the single highest-severity thing on screen right
//!   now," so sharing the slot is a shared meaning, not a coincidence.
//! - **Yellow is in-progress**, nothing else: `tool_running`,
//!   `agent_running`, `spinner`, `border_warning` (the `/ask` modal is
//!   "waiting on you," the in-progress state from the operator's side).
//! - **Magenta is blocked-on-you**: `tool_awaiting`, `agent_awaiting`.
//!   Distinct from yellow (in-progress on its own) and red (something went
//!   wrong) -- this is "stopped, needs a decision."
//! - **Green is success**, period: `tool_done`, `agent_finished`. V7
//!   stopped reusing it for `help_key` (see below) so a green glyph
//!   anywhere in the TUI means exactly one thing.
//! - **Gray/dim is secondary, and is never a fixed dark color.**
//!   `tool_proposed`/`agent_starting` (pending, not yet meaningful),
//!   `dim`, `timestamp`, `agent_cancelled`, `reasoning`, `status_dim`,
//!   `scroll_footer` all render via `Modifier::DIM` (a relative dimming of
//!   the terminal's own foreground) rather than `Color::DarkGray` (an
//!   absolute dark color that a dark-background terminal can render nearly
//!   indistinguishable from the background -- V7 audited every `DarkGray`
//!   default against this and moved the survivors to `DIM`).
//! - **Conversation text is never colored.** `user`, `assistant` stay
//!   unstyled; `assistant_marker` is the one exception, and it earns it:
//!   the marker names *which model* answered, not how the answer should be
//!   read, so it is provenance metadata sitting next to the text, not the
//!   text itself.
//! - **Chrome that carries no state is bold or dim, never colored.**
//!   `focused`, `emphasized`, `help_key` (V7 dropped its Green -- the
//!   key/description column split needed *distinguishing*, not a status
//!   color) are bold-only; a color on pure layout chrome competes with the
//!   red/yellow/magenta/green vocabulary above for the eye's attention
//!   without adding information.
//! - **Modal borders are colored by how urgent the decision behind them
//!   is**: `border_danger` (red, a tool call you must approve or refuse),
//!   `border_warning` (yellow, the `/ask` modal), `border_accent` (cyan,
//!   the NL intent-confirm card), `help_border` (blue, `/help` -- the one
//!   modal with no decision at all, deliberately the coolest, least urgent
//!   hue of the four).
//!
//! V7 also removed [`Theme`]'s `agent_marker` slot: it had no call site
//! anywhere in `view/*.rs` since the day T4 defined it (grep-verified), so
//! it was a config key that could be set and would silently do nothing --
//! the exact failure mode V6 already ruled out for `spinner_b`/`spinner_c`.
//! See `docs/crates/conway-cli.md` for the full before/after and the
//! `tool_*`/`agent_*` status-family duplication finding V7 chose NOT to act
//! on (and why).

use ratatui::style::{Color, Modifier, Style};

use conway::config::schema::{ThemeConfig, ThemeStyleConfig};

/// The TUI's named style table -- one [`Style`] per concern, threaded
/// through `view::draw` and each per-view `draw` fn as `&Theme`.
///
/// Construct at startup ([`Theme::default`] or [`Theme::from_config`]) and
/// pass by reference into the render pass; do not construct per-frame and
/// do not call a global accessor to fetch one (decision D-T1).
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    /// The user's `you> ` prefix in the transcript. Pre-T1:
    /// `Style::default().add_modifier(Modifier::BOLD)` (no fg).
    pub user: Style,
    /// Assistant text body in the transcript. Pre-T1: unstyled
    /// (`Style::default()`).
    pub assistant: Style,
    /// Accent for the assistant's turn marker (NEW, no pre-T1 call site;
    /// T4 will consume). Default: `Color::Magenta` + `Modifier::BOLD`.
    pub assistant_marker: Style,
    /// Reasoning-trace text (NEW, no pre-T1 call site; T4 will consume).
    /// Default: `Modifier::DIM` + `Modifier::ITALIC` (V7: was
    /// `Color::DarkGray` + `Modifier::ITALIC` -- moved to a relative `DIM`
    /// so the trace stays legible on a dark-background terminal, where a
    /// fixed dark color can render nearly indistinguishable from the
    /// background; see the module doc's "gray/dim" rule).
    pub reasoning: Style,
    /// T4: the `HH:MM ` timestamp prefix prepended to each entry's first
    /// rendered line while `show_timestamps` is on. Default: `Modifier::DIM`
    /// (no fg) -- a quiet annotation that should not compete with the entry
    /// body itself. V7: was `Color::DarkGray`, moved to `DIM` for the same
    /// dark-background legibility reason as [`Theme::reasoning`] above.
    pub timestamp: Style,
    /// Tool-call tag, `ToolStatus::Proposed`. Pre-T1: `Color::Gray`.
    pub tool_proposed: Style,
    /// Tool-call tag, `ToolStatus::AwaitingPermission`. Pre-T1:
    /// `Color::Magenta`.
    pub tool_awaiting: Style,
    /// Tool-call tag, `ToolStatus::Running`. Pre-T1: `Color::Yellow`.
    pub tool_running: Style,
    /// Tool-call tag, `ToolStatus::Finished { is_error: false }`. Pre-T1:
    /// `Color::Green`.
    pub tool_done: Style,
    /// Tool-call tag, `ToolStatus::Finished { is_error: true }`. Pre-T1:
    /// `Color::Red`.
    pub tool_failed: Style,
    /// Agent-tree/transcript marker, `NodeStatus::Starting`. Pre-T1:
    /// `Color::Gray`.
    pub agent_starting: Style,
    /// Agent-tree/transcript marker, `NodeStatus::Running`. Pre-T1:
    /// `Color::Yellow`.
    pub agent_running: Style,
    /// Agent-tree/transcript marker, `NodeStatus::AwaitingPermission`.
    /// Pre-T1: `Color::Magenta`.
    pub agent_awaiting: Style,
    /// Agent-tree/transcript marker, `NodeStatus::Finished`. Pre-T1:
    /// `Color::Green`.
    pub agent_finished: Style,
    /// Agent-tree/transcript marker, `NodeStatus::Failed`. Pre-T1:
    /// `Color::Red`.
    pub agent_failed: Style,
    /// Agent-tree/transcript marker, `NodeStatus::Cancelled`. Default:
    /// `Modifier::DIM` (no fg) -- V7: was `Color::DarkGray`, moved to `DIM`
    /// for the same dark-background legibility reason as
    /// [`Theme::reasoning`]/[`Theme::timestamp`]. A cancelled agent is
    /// terminal, secondary information, same category as those two.
    pub agent_cancelled: Style,
    /// `Entry::Notice` text in the transcript. Pre-T1: `Color::Cyan`.
    pub notice: Style,
    /// Error text in modal overlays. Pre-T1: `Color::Red` (the `/ask`
    /// modal's failed-fate error line).
    pub error: Style,
    /// The highest-alert accent: `Color::Red` + `Modifier::BOLD`. Reserved
    /// (no pre-T1 call site) until V7, which wired it to the status line's
    /// AUTO-ALLOW indicator (`view/status.rs`) -- the operator having
    /// forgotten they are in a mode that auto-approves every tool call is
    /// exactly the failure this accent exists to prevent (see the module
    /// doc's red rule). Still available for a genuine fatal runtime error
    /// (`Event::Error { fatal: true }`) once that path is wired to carry a
    /// style through `Entry::Notice` -- tracked as a follow-up, not done
    /// here (see `docs/crates/conway-cli.md`).
    pub fatal_error: Style,
    /// Dimmed annotation text (agent-tree recipe labels, input-box
    /// placeholder). Pre-T1: `Modifier::DIM` (no fg).
    pub dim: Style,
    /// The `(focused)` tag on the focused agent's row in the agent panel,
    /// and the emphasized body line in modal overlays. Pre-T1:
    /// `Modifier::BOLD` (no fg).
    pub focused: Style,
    /// The arrow-selected row's highlight in the agent panel. Pre-T1:
    /// `Modifier::REVERSED`.
    pub selected: Style,
    /// Emphasized body line in modal overlays (the permission prompt's
    /// command body, the `/ask` modal's `you asked:` line, the intent
    /// card's `recipe:` line). Pre-T1: `Modifier::BOLD` (no fg). Same
    /// default as [`Theme::focused`]; kept as a distinct slot so a user
    /// can recolor one without touching the other.
    pub emphasized: Style,
    /// Default block border (input box, agent panel). Pre-T1:
    /// `Style::default()` (no fg, no modifier).
    pub border_normal: Style,
    /// Warning-level modal border (the `/ask` modal). Pre-T1:
    /// `Color::Yellow` + `Modifier::BOLD`.
    pub border_warning: Style,
    /// Danger-level modal border (the permission prompt). Pre-T1:
    /// `Color::Red` + `Modifier::BOLD`.
    pub border_danger: Style,
    /// Accent modal border (the NL intent confirmation card). Pre-T1:
    /// `Color::Cyan` + `Modifier::BOLD`.
    pub border_accent: Style,
    /// The bottom status line's overall style. Pre-T1:
    /// `Modifier::REVERSED` (no fg).
    pub status_mode: Style,
    /// Dimmed status-line accent (NEW, no pre-T1 call site). Default:
    /// `Modifier::DIM`.
    pub status_dim: Style,
    /// Activity spinner accent. Default: `Color::Yellow`. Styles both the
    /// braille glyph and the activity word, steadily -- V6 removed T2's
    /// `spinner_b`/`spinner_c` pulse palette, since cycling color on every
    /// tick strobed rather than signalled. The advancing frame is the
    /// liveness cue.
    pub spinner: Style,
    /// T6: the sticky context header shown above the transcript pane while
    /// it overflows the viewport (`session · focused agent · model ·
    /// ctx%`). Default: `Modifier::REVERSED` (no fg) -- a persistent bar,
    /// matching how [`Theme::status_mode`] treats the OTHER fixed,
    /// always-legible affordance on screen.
    pub header: Style,
    /// T6: the floating "jump to bottom" footer pill drawn over the bottom
    /// row of the transcript while scrolled up (`!follow_tail`). Default:
    /// `Modifier::DIM` (no fg) -- a quiet annotation, matching
    /// [`Theme::status_dim`]'s treatment of a similar dim affordance hint.
    pub scroll_footer: Style,
    /// T7: the `/help` keybinding overlay's block border. Default:
    /// `Color::Blue` + `Modifier::BOLD` -- distinct from the other three
    /// modal borders ([`Theme::border_danger`] red, [`Theme::border_warning`]
    /// yellow, [`Theme::border_accent`] cyan), since the help overlay is
    /// informational, not a decision the user owes an answer to.
    pub help_border: Style,
    /// T7: the key/chord column in the `/help` overlay's rows (e.g.
    /// `Ctrl-E`, `PageUp/PageDown`), distinguishing it from the plain
    /// description text beside it. Default: `Modifier::BOLD` (no fg) --
    /// V7: was `Color::Green` + `Modifier::BOLD`. The split only needs
    /// distinguishing, not a status color, and green already means
    /// "success" ([`Theme::tool_done`]/[`Theme::agent_finished`]); reusing
    /// it here for plain layout chrome blurred that meaning for no reason
    /// (see the module doc's "chrome" rule).
    pub help_key: Style,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            user: Style::default().add_modifier(Modifier::BOLD),
            assistant: Style::default(),
            assistant_marker: Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
            reasoning: Style::default()
                .add_modifier(Modifier::DIM)
                .add_modifier(Modifier::ITALIC),
            timestamp: Style::default().add_modifier(Modifier::DIM),
            tool_proposed: Style::default().fg(Color::Gray),
            tool_awaiting: Style::default().fg(Color::Magenta),
            tool_running: Style::default().fg(Color::Yellow),
            tool_done: Style::default().fg(Color::Green),
            tool_failed: Style::default().fg(Color::Red),
            agent_starting: Style::default().fg(Color::Gray),
            agent_running: Style::default().fg(Color::Yellow),
            agent_awaiting: Style::default().fg(Color::Magenta),
            agent_finished: Style::default().fg(Color::Green),
            agent_failed: Style::default().fg(Color::Red),
            agent_cancelled: Style::default().add_modifier(Modifier::DIM),
            notice: Style::default().fg(Color::Cyan),
            error: Style::default().fg(Color::Red),
            fatal_error: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            dim: Style::default().add_modifier(Modifier::DIM),
            focused: Style::default().add_modifier(Modifier::BOLD),
            selected: Style::default().add_modifier(Modifier::REVERSED),
            emphasized: Style::default().add_modifier(Modifier::BOLD),
            border_normal: Style::default(),
            border_warning: Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            border_danger: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            border_accent: Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            status_mode: Style::default().add_modifier(Modifier::REVERSED),
            status_dim: Style::default().add_modifier(Modifier::DIM),
            spinner: Style::default().fg(Color::Yellow),
            header: Style::default().add_modifier(Modifier::REVERSED),
            scroll_footer: Style::default().add_modifier(Modifier::DIM),
            help_border: Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
            help_key: Style::default().add_modifier(Modifier::BOLD),
        }
    }
}

impl Theme {
    /// Builds a [`Theme`] from a loaded `[tui.theme]` config table overlaid
    /// on the built-in defaults. Each slot's override is applied
    /// independently; a malformed value (unknown color name, unparseable
    /// hex, unknown modifier) falls back to that slot's default for the
    /// affected channel -- never a panic (P-10). `None` overrides are
    /// no-ops, so an empty `ThemeConfig` yields `Theme::default()`.
    pub fn from_config(config: &ThemeConfig) -> Self {
        let mut theme = Self::default();
        theme.user = overlay(theme.user, config.user.as_ref());
        theme.assistant = overlay(theme.assistant, config.assistant.as_ref());
        theme.assistant_marker =
            overlay(theme.assistant_marker, config.assistant_marker.as_ref());
        theme.reasoning = overlay(theme.reasoning, config.reasoning.as_ref());
        theme.timestamp = overlay(theme.timestamp, config.timestamp.as_ref());
        theme.tool_proposed = overlay(theme.tool_proposed, config.tool_proposed.as_ref());
        theme.tool_awaiting = overlay(theme.tool_awaiting, config.tool_awaiting.as_ref());
        theme.tool_running = overlay(theme.tool_running, config.tool_running.as_ref());
        theme.tool_done = overlay(theme.tool_done, config.tool_done.as_ref());
        theme.tool_failed = overlay(theme.tool_failed, config.tool_failed.as_ref());
        theme.agent_starting = overlay(theme.agent_starting, config.agent_starting.as_ref());
        theme.agent_running = overlay(theme.agent_running, config.agent_running.as_ref());
        theme.agent_awaiting = overlay(theme.agent_awaiting, config.agent_awaiting.as_ref());
        theme.agent_finished = overlay(theme.agent_finished, config.agent_finished.as_ref());
        theme.agent_failed = overlay(theme.agent_failed, config.agent_failed.as_ref());
        theme.agent_cancelled = overlay(theme.agent_cancelled, config.agent_cancelled.as_ref());
        theme.notice = overlay(theme.notice, config.notice.as_ref());
        theme.error = overlay(theme.error, config.error.as_ref());
        theme.fatal_error = overlay(theme.fatal_error, config.fatal_error.as_ref());
        theme.dim = overlay(theme.dim, config.dim.as_ref());
        theme.focused = overlay(theme.focused, config.focused.as_ref());
        theme.selected = overlay(theme.selected, config.selected.as_ref());
        theme.emphasized = overlay(theme.emphasized, config.emphasized.as_ref());
        theme.border_normal = overlay(theme.border_normal, config.border_normal.as_ref());
        theme.border_warning = overlay(theme.border_warning, config.border_warning.as_ref());
        theme.border_danger = overlay(theme.border_danger, config.border_danger.as_ref());
        theme.border_accent = overlay(theme.border_accent, config.border_accent.as_ref());
        theme.status_mode = overlay(theme.status_mode, config.status_mode.as_ref());
        theme.status_dim = overlay(theme.status_dim, config.status_dim.as_ref());
        theme.spinner = overlay(theme.spinner, config.spinner.as_ref());
        theme.header = overlay(theme.header, config.header.as_ref());
        theme.scroll_footer = overlay(theme.scroll_footer, config.scroll_footer.as_ref());
        theme.help_border = overlay(theme.help_border, config.help_border.as_ref());
        theme.help_key = overlay(theme.help_key, config.help_key.as_ref());
        theme
    }
}

/// Applies one slot's `Option<ThemeStyleConfig>` override on top of its
/// `default` style. `fg`/`bg` strings that don't parse to a ratatui `Color`
/// are silently skipped (the default for that channel is kept); each
/// modifier string that doesn't parse to a ratatui `Modifier` is silently
/// skipped too. `None` returns `default` unchanged. Never panics (P-10).
fn overlay(default: Style, cfg: Option<&ThemeStyleConfig>) -> Style {
    let Some(cfg) = cfg else {
        return default;
    };
    let mut out = default;
    if let Some(fg) = cfg.fg.as_deref().and_then(parse_color) {
        out = out.fg(fg);
    }
    if let Some(bg) = cfg.bg.as_deref().and_then(parse_color) {
        out = out.bg(bg);
    }
    for raw in &cfg.modifiers {
        if let Some(modifier) = parse_modifier(raw) {
            out = out.add_modifier(modifier);
        }
    }
    out
}

/// Parses a config-supplied color string into a ratatui `Color`. Accepts the
/// 16 named ratatui colors (case-insensitive, `snake_case` or `kebab-case`),
/// `"reset"`, and `#rrggbb` / `#rgb` hex codes. Returns `None` for anything
/// else (P-10: the caller falls back to the default -- never a panic, no
/// `unwrap`/`expect`/indexing on the config value).
fn parse_color(raw: &str) -> Option<Color> {
    let lower = raw.trim().to_ascii_lowercase();
    match lower.as_str() {
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "gray" | "grey" => Some(Color::Gray),
        "dark_gray" | "dark-gray" | "dark-grey" | "darkgrey" | "darkgray" => {
            Some(Color::DarkGray)
        }
        "light_red" | "light-red" | "lightred" => Some(Color::LightRed),
        "light_green" | "light-green" | "lightgreen" => Some(Color::LightGreen),
        "light_yellow" | "light-yellow" | "lightyellow" => Some(Color::LightYellow),
        "light_blue" | "light-blue" | "lightblue" => Some(Color::LightBlue),
        "light_magenta" | "light-magenta" | "lightmagenta" => Some(Color::LightMagenta),
        "light_cyan" | "light-cyan" | "lightcyan" => Some(Color::LightCyan),
        "white" => Some(Color::White),
        "reset" => Some(Color::Reset),
        _ => parse_hex_color(&lower),
    }
}

/// Parses `#rrggbb` (6 digits) or `#rgb` (3 digits) hex into a ratatui
/// `Color::Rgb`. Returns `None` for any other shape (P-10).
fn parse_hex_color(lower: &str) -> Option<Color> {
    let hex = lower.strip_prefix('#')?;
    if hex.len() != 6 && hex.len() != 3 {
        return None;
    }
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let (r, g, b) = if hex.len() == 6 {
        (
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
        )
    } else {
        (
            u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?,
            u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?,
            u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?,
        )
    };
    Some(Color::Rgb(r, g, b))
}

/// Parses a config-supplied modifier tag into a ratatui `Modifier`.
/// Case-insensitive, `snake_case` or `kebab-case`. Returns `None` for an
/// unknown tag (P-10: the caller skips it -- never a panic).
fn parse_modifier(raw: &str) -> Option<Modifier> {
    let lower = raw.trim().to_ascii_lowercase();
    Some(match lower.as_str() {
        "bold" => Modifier::BOLD,
        "dim" => Modifier::DIM,
        "italic" => Modifier::ITALIC,
        "underlined" => Modifier::UNDERLINED,
        "reversed" => Modifier::REVERSED,
        "slow_blink" | "slow-blink" => Modifier::SLOW_BLINK,
        "rapid_blink" | "rapid-blink" => Modifier::RAPID_BLINK,
        "hidden" => Modifier::HIDDEN,
        "crossed_out" | "crossed-out" => Modifier::CROSSED_OUT,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::config::schema::ThemeStyleConfig;

    /// Helper: build a `ThemeStyleConfig` with just an `fg` override.
    fn fg_only(fg: &str) -> ThemeStyleConfig {
        ThemeStyleConfig {
            fg: Some(fg.to_string()),
            bg: None,
            modifiers: Vec::new(),
        }
    }

    /// Helper: build a `ThemeStyleConfig` with just a modifiers list.
    fn mods_only(mods: &[&str]) -> ThemeStyleConfig {
        ThemeStyleConfig {
            fg: None,
            bg: None,
            modifiers: mods.iter().map(|s| s.to_string()).collect(),
        }
    }

    // ---- visual parity: Theme::default() matches today's exact pairs ----

    #[test]
    fn default_user_is_bold_no_fg() {
        let t = Theme::default();
        assert_eq!(t.user, Style::default().add_modifier(Modifier::BOLD));
    }

    #[test]
    fn default_assistant_is_unstyled() {
        let t = Theme::default();
        assert_eq!(t.assistant, Style::default());
    }

    #[test]
    fn default_tool_tags_match_pre_t1_colors() {
        let t = Theme::default();
        assert_eq!(t.tool_proposed, Style::default().fg(Color::Gray));
        assert_eq!(t.tool_awaiting, Style::default().fg(Color::Magenta));
        assert_eq!(t.tool_running, Style::default().fg(Color::Yellow));
        assert_eq!(t.tool_done, Style::default().fg(Color::Green));
        assert_eq!(t.tool_failed, Style::default().fg(Color::Red));
    }

    #[test]
    fn default_agent_status_colors_match_pre_t1() {
        let t = Theme::default();
        assert_eq!(t.agent_starting, Style::default().fg(Color::Gray));
        assert_eq!(t.agent_running, Style::default().fg(Color::Yellow));
        assert_eq!(t.agent_awaiting, Style::default().fg(Color::Magenta));
        assert_eq!(t.agent_finished, Style::default().fg(Color::Green));
        assert_eq!(t.agent_failed, Style::default().fg(Color::Red));
    }

    /// V7: `agent_cancelled` moved off a fixed `Color::DarkGray` (which can
    /// render nearly indistinguishable from a dark-background terminal) to
    /// a relative `Modifier::DIM`, matching `timestamp`/`reasoning` below.
    #[test]
    fn default_agent_cancelled_is_dim_not_dark_gray() {
        let t = Theme::default();
        assert_eq!(
            t.agent_cancelled,
            Style::default().add_modifier(Modifier::DIM)
        );
    }

    #[test]
    fn default_notice_is_cyan() {
        let t = Theme::default();
        assert_eq!(t.notice, Style::default().fg(Color::Cyan));
    }

    #[test]
    fn default_error_is_red() {
        let t = Theme::default();
        assert_eq!(t.error, Style::default().fg(Color::Red));
    }

    #[test]
    fn default_dim_is_dim_modifier() {
        let t = Theme::default();
        assert_eq!(t.dim, Style::default().add_modifier(Modifier::DIM));
    }

    /// V7: `timestamp` moved off a fixed `Color::DarkGray` to a relative
    /// `Modifier::DIM` -- see the module doc's "gray/dim" rule.
    #[test]
    fn default_timestamp_is_dim_not_dark_gray() {
        let t = Theme::default();
        assert_eq!(t.timestamp, Style::default().add_modifier(Modifier::DIM));
    }

    #[test]
    fn default_focused_and_emphasized_are_bold() {
        let t = Theme::default();
        assert_eq!(t.focused, Style::default().add_modifier(Modifier::BOLD));
        assert_eq!(t.emphasized, Style::default().add_modifier(Modifier::BOLD));
    }

    #[test]
    fn default_selected_is_reversed() {
        let t = Theme::default();
        assert_eq!(t.selected, Style::default().add_modifier(Modifier::REVERSED));
    }

    #[test]
    fn default_borders_match_pre_t1() {
        let t = Theme::default();
        assert_eq!(t.border_normal, Style::default());
        assert_eq!(
            t.border_warning,
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            t.border_danger,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            t.border_accent,
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        );
    }

    #[test]
    fn default_status_mode_is_reversed() {
        let t = Theme::default();
        assert_eq!(t.status_mode, Style::default().add_modifier(Modifier::REVERSED));
    }

    // ---- new accent styles have sensible defaults ----

    #[test]
    fn default_assistant_marker_is_magenta_bold() {
        let t = Theme::default();
        assert_eq!(
            t.assistant_marker,
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)
        );
    }

    /// V7: `reasoning` moved off a fixed `Color::DarkGray` to a relative
    /// `Modifier::DIM` for the same dark-background legibility reason as
    /// `timestamp`/`agent_cancelled` -- see the module doc's "gray/dim"
    /// rule.
    #[test]
    fn default_reasoning_is_dim_italic_not_dark_gray() {
        let t = Theme::default();
        assert_eq!(
            t.reasoning,
            Style::default()
                .add_modifier(Modifier::DIM)
                .add_modifier(Modifier::ITALIC)
        );
    }

    // ---- T6: sticky header + floating scroll footer ----

    #[test]
    fn default_header_is_reversed_no_fg() {
        let t = Theme::default();
        assert_eq!(t.header, Style::default().add_modifier(Modifier::REVERSED));
    }

    #[test]
    fn default_scroll_footer_is_dim_no_fg() {
        let t = Theme::default();
        assert_eq!(t.scroll_footer, Style::default().add_modifier(Modifier::DIM));
    }

    #[test]
    fn header_and_scroll_footer_overrides_apply_independently() {
        let cfg = ThemeConfig {
            header: Some(fg_only("cyan")),
            ..Default::default()
        };
        let t = Theme::from_config(&cfg);
        assert_eq!(
            t.header,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::REVERSED)
        );
        // Untouched slot keeps its default.
        assert_eq!(t.scroll_footer, Style::default().add_modifier(Modifier::DIM));
    }

    #[test]
    fn malformed_header_override_falls_back_to_default() {
        let cfg = ThemeConfig {
            header: Some(fg_only("not-a-color")),
            ..Default::default()
        };
        let t = Theme::from_config(&cfg);
        assert_eq!(t.header, Style::default().add_modifier(Modifier::REVERSED), "P-10: no panic");
    }

    // ---- T7: /help keybinding overlay ----

    #[test]
    fn default_help_border_is_blue_bold() {
        let t = Theme::default();
        assert_eq!(
            t.help_border,
            Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)
        );
    }

    /// V7: `help_key` dropped `Color::Green` -- the key/description column
    /// split only needs distinguishing, and green already means "success"
    /// elsewhere in the palette ([`Theme::tool_done`]/
    /// [`Theme::agent_finished`]); reusing it for plain layout chrome
    /// blurred that meaning. `Modifier::BOLD` alone still distinguishes the
    /// column from the plain description text beside it.
    #[test]
    fn default_help_key_is_bold_not_green() {
        let t = Theme::default();
        assert_eq!(t.help_key, Style::default().add_modifier(Modifier::BOLD));
    }

    #[test]
    fn help_border_and_help_key_overrides_apply_independently() {
        let cfg = ThemeConfig {
            help_border: Some(fg_only("magenta")),
            ..Default::default()
        };
        let t = Theme::from_config(&cfg);
        assert_eq!(
            t.help_border,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD)
        );
        // Untouched slot keeps its default.
        assert_eq!(t.help_key, Style::default().add_modifier(Modifier::BOLD));
    }

    #[test]
    fn malformed_help_border_override_falls_back_to_default() {
        let cfg = ThemeConfig {
            help_border: Some(fg_only("not-a-color")),
            ..Default::default()
        };
        let t = Theme::from_config(&cfg);
        assert_eq!(
            t.help_border,
            Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
            "P-10: no panic"
        );
    }

    // ---- from_config: empty config == default ----

    #[test]
    fn empty_config_yields_default_theme() {
        let t = Theme::from_config(&ThemeConfig::default());
        assert_eq!(t, Theme::default());
    }

    // ---- from_config: an override changes the named slot ----

    #[test]
    fn override_changes_the_named_slot() {
        let cfg = ThemeConfig {
            notice: Some(fg_only("red")),
            ..Default::default()
        };
        let t = Theme::from_config(&cfg);
        assert_eq!(t.notice, Style::default().fg(Color::Red));
        // Untouched slots keep their defaults.
        assert_eq!(t.tool_running, Style::default().fg(Color::Yellow));
    }

    #[test]
    fn override_adds_modifiers_on_top_of_default() {
        let cfg = ThemeConfig {
            tool_running: Some(mods_only(&["bold", "italic"])),
            ..Default::default()
        };
        let t = Theme::from_config(&cfg);
        assert_eq!(
            t.tool_running,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::ITALIC)
        );
    }

    #[test]
    fn override_fg_and_modifiers_together() {
        let cfg = ThemeConfig {
            user: Some(ThemeStyleConfig {
                fg: Some("magenta".to_string()),
                bg: None,
                modifiers: vec!["italic".to_string()],
            }),
            ..Default::default()
        };
        let t = Theme::from_config(&cfg);
        assert_eq!(
            t.user,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::ITALIC)
        );
    }

    #[test]
    fn override_hex_color_parses() {
        let cfg = ThemeConfig {
            notice: Some(fg_only("#ff8800")),
            ..Default::default()
        };
        let t = Theme::from_config(&cfg);
        assert_eq!(t.notice, Style::default().fg(Color::Rgb(0xff, 0x88, 0x00)));
    }

    #[test]
    fn override_short_hex_color_parses() {
        let cfg = ThemeConfig {
            notice: Some(fg_only("#f80")),
            ..Default::default()
        };
        let t = Theme::from_config(&cfg);
        assert_eq!(t.notice, Style::default().fg(Color::Rgb(0xff, 0x88, 0x00)));
    }

    // ---- P-10: malformed overrides fall back to defaults, no panic ----

    #[test]
    fn malformed_fg_falls_back_to_default_for_that_slot() {
        let cfg = ThemeConfig {
            notice: Some(fg_only("not-a-real-color")),
            ..Default::default()
        };
        let t = Theme::from_config(&cfg);
        // The whole slot reverts to its default (Cyan) -- the bad `fg` is
        // dropped, no other channel is touched, no panic.
        assert_eq!(t.notice, Style::default().fg(Color::Cyan));
    }

    #[test]
    fn malformed_hex_falls_back_to_default() {
        let cfg = ThemeConfig {
            notice: Some(fg_only("#zzzzzz")),
            ..Default::default()
        };
        let t = Theme::from_config(&cfg);
        assert_eq!(t.notice, Style::default().fg(Color::Cyan));
    }

    #[test]
    fn malformed_modifier_is_skipped() {
        let cfg = ThemeConfig {
            user: Some(mods_only(&["bold", "not-a-modifier", "italic"])),
            ..Default::default()
        };
        let t = Theme::from_config(&cfg);
        // The default `user` is BOLD; the override adds BOLD + ITALIC, the
        // unknown tag is silently dropped (no panic).
        assert_eq!(
            t.user,
            Style::default()
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::ITALIC)
        );
    }

    #[test]
    fn malformed_override_on_one_slot_does_not_touch_another() {
        let cfg = ThemeConfig {
            notice: Some(fg_only("not-a-real-color")),
            tool_running: Some(fg_only("red")),
            ..Default::default()
        };
        let t = Theme::from_config(&cfg);
        assert_eq!(t.notice, Style::default().fg(Color::Cyan));
        assert_eq!(t.tool_running, Style::default().fg(Color::Red));
    }

    // ---- T1 review finding 2: render-level proof that the `assistant` and
    // `border_normal` slots are wired into the render path (not just
    // resolved onto the `Theme` struct and then ignored). Overrides each
    // slot with a distinct fg, renders through the REAL `view::draw`, and
    // asserts the rendered buffer reflects the override. ----

    fn render_with_theme(
        state: &crate::tui::state::AppState,
        theme: &Theme,
        width: u16,
        height: u16,
    ) -> ratatui::buffer::Buffer {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("TestBackend construction cannot fail");
        terminal
            .draw(|f| crate::tui::view::draw(state, f, theme))
            .expect("drawing into a TestBackend cannot fail");
        terminal.backend().buffer().clone()
    }

    #[test]
    fn assistant_override_reaches_the_rendered_buffer() {
        use crate::tui::state::{AppState, Entry};
        use conway::AgentId;

        let mut state = AppState::new(AgentId::new());
        state.transcript.push(Entry::Assistant {
            text: "hello-assistant".to_string(),
            model: None,
            summary: None,
            ts: None,
        });
        let theme = Theme {
            assistant: Style::default().fg(Color::Red),
            ..Theme::default()
        };

        let buffer = render_with_theme(&state, &theme, 80, 24);

        // The assistant text lands at the transcript pane's top-left (row 0).
        // Find the cell holding the first 'h' and assert its fg is the
        // overridden Red, not the default (which would be `Reset`/unset).
        let cell = &buffer[(0, 0)];
        assert_eq!(
            cell.symbol(),
            "h",
            "expected the assistant text to render at row 0, got {:?}",
            cell.symbol()
        );
        assert_eq!(
            cell.fg,
            Color::Red,
            "theme.assistant override must reach the rendered buffer (T1 finding 2)"
        );
    }

    #[test]
    fn border_normal_override_reaches_the_rendered_buffer() {
        use crate::tui::state::AppState;
        use conway::AgentId;

        let state = AppState::new(AgentId::new());
        let theme = Theme {
            border_normal: Style::default().fg(Color::Green),
            ..Theme::default()
        };

        let buffer = render_with_theme(&state, &theme, 80, 24);

        // The input box is the one always-visible bordered element (the
        // agent panel is hidden by default). Its block border is drawn with
        // `theme.border_normal`; at least one border glyph cell must carry
        // the overridden Green fg.
        const BORDER_GLYPHS: &[char] = &['│', '─', '┌', '┐', '└', '┘'];
        let any_green_border = buffer.content().iter().any(|cell| {
            let sym = cell.symbol();
            BORDER_GLYPHS.contains(&sym.chars().next().unwrap_or(' ')) && cell.fg == Color::Green
        });
        assert!(
            any_green_border,
            "theme.border_normal override must reach the input-box border in the rendered \
             buffer (T1 finding 2): no border glyph carries the overridden Green fg"
        );
    }

    // ---- color/modifier parser direct checks ----

    #[test]
    fn parse_color_accepts_named_snake_and_kebab_case() {
        assert_eq!(parse_color("dark_gray"), Some(Color::DarkGray));
        assert_eq!(parse_color("dark-gray"), Some(Color::DarkGray));
        assert_eq!(parse_color("CYAN"), Some(Color::Cyan));
        assert_eq!(parse_color("  magenta  "), Some(Color::Magenta));
    }

    #[test]
    fn parse_color_rejects_unknown_name() {
        assert_eq!(parse_color("purple"), None);
        assert_eq!(parse_color(""), None);
    }

    #[test]
    fn parse_modifier_accepts_named_snake_and_kebab_case() {
        assert_eq!(parse_modifier("bold"), Some(Modifier::BOLD));
        assert_eq!(parse_modifier("slow_blink"), Some(Modifier::SLOW_BLINK));
        assert_eq!(parse_modifier("slow-blink"), Some(Modifier::SLOW_BLINK));
        assert_eq!(parse_modifier("BOLD"), Some(Modifier::BOLD));
    }

    #[test]
    fn parse_modifier_rejects_unknown_tag() {
        assert_eq!(parse_modifier("sparkly"), None);
        assert_eq!(parse_modifier(""), None);
    }

    // ---- T2: spinner pulse palette ----

    #[test]
    fn malformed_spinner_override_falls_back_to_default() {
        let cfg = ThemeConfig {
            spinner: Some(fg_only("not-a-color")),
            ..Default::default()
        };
        let t = Theme::from_config(&cfg);
        assert_eq!(t.spinner, Style::default().fg(Color::Yellow), "P-10: no panic");
    }

    // ---- T1 acceptance: no inline `Style::default().fg(Color::…)` remains
    // in any view file. Scans each view source via `include_str!` so the
    // check runs at test time without touching the filesystem. `theme.rs`
    // itself is the one place a `Style::default().fg(Color::…)` literal is
    // expected (the defaults), so it is excluded; `palette.rs` is the slash-
    // command palette, not part of T1's refactor scope, but it uses only
    // `.add_modifier(..)` (no `.fg(Color::..)`), so it passes too. ----

    #[test]
    fn no_inline_style_default_fg_color_remains_in_view_files() {
        // `theme.rs` legitimately builds the defaults with
        // `Style::default().fg(Color::…)` -- it is THE place those live now.
        const THEME_RS: &str = include_str!("theme.rs");
        // Every other view file must not reintroduce an inline
        // `Style::default().fg(Color::…)` -- that is the whole point of T1.
        const NEEDLE: &str = "Style::default().fg(Color::";
        for (name, contents) in [
            ("mod.rs", include_str!("mod.rs")),
            ("transcript.rs", include_str!("transcript.rs")),
            ("status.rs", include_str!("status.rs")),
            ("agents.rs", include_str!("agents.rs")),
            ("input_box.rs", include_str!("input_box.rs")),
            ("palette.rs", include_str!("palette.rs")),
            ("header.rs", include_str!("header.rs")),
            ("help.rs", include_str!("help.rs")),
            // V1: the shared modal/menu primitives take a caller-supplied
            // `Style` (the ported surfaces' own `theme.border_*`) rather
            // than building one -- they must stay just as clean of an
            // inline literal as every other view file.
            ("modal.rs", include_str!("modal.rs")),
            ("menu.rs", include_str!("menu.rs")),
            // V4: the `/settings` menu, the first real caller of the two
            // primitives above.
            ("settings.rs", include_str!("settings.rs")),
        ] {
            // `theme.rs` is allowed to contain the needle (the defaults);
            // assert it is the ONLY file that does.
            assert!(
                !contents.contains(NEEDLE),
                "{name} reintroduces an inline `Style::default().fg(Color::…)` -- \
                 use a `Theme` slot instead (T1). The needle is permitted only in \
                 theme.rs, which owns the defaults."
            );
        }
        // Sanity: theme.rs itself must still contain the needle (otherwise
        // the defaults were refactored away and this guard would pass
        // vacuously).
        assert!(
            THEME_RS.contains(NEEDLE),
            "theme.rs must own the `Style::default().fg(Color::…)` defaults -- \
             if it no longer does, this guard is vacuous."
        );
    }
    /// V6: the spinner is a single steady slot. The `spinner_b`/`spinner_c`
    /// pulse-palette slots are gone -- and so are their `[tui.theme]` config
    /// keys, deliberately: a config key that silently does nothing is worse
    /// than no key at all.
    #[test]
    fn spinner_is_one_steady_slot_with_no_pulse_palette() {
        let t = Theme::default();
        assert_eq!(t.spinner, Style::default().fg(Color::Yellow));

        let overridden = Theme::from_config(&ThemeConfig {
            spinner: Some(fg_only("cyan")),
            ..Default::default()
        });
        assert_eq!(overridden.spinner, Style::default().fg(Color::Cyan));
    }

}