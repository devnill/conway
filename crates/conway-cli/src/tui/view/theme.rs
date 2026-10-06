//! The TUI's central color/style system.
//!
//! Every `view/*.rs` file used to hand-roll `Style::default().fg(Color::…)`
//! inline at each call site. This module replaces that with a single [`Theme`] struct
//! holding one named [`Style`] per concern, threaded through `view::draw`
//! and each per-view `draw` fn as `&Theme` (decision D-T1: injected, not
//! re-fetched via a call-site accessor or a global `Lazy`). The view files
//! now read `theme.<name>` instead of building a `Style` inline.
//!
//! ## Configurability
//!
//! The theme is **configurable from the start**: a `[tui.theme]` table in
//! `settings.json` (schema: `crate::tui::config::ThemeConfig`) overlays
//! per-slot `fg`/`bg`/`modifiers` on top of the defaults. [`Theme::from_config`]
//! loads that overlay; [`Theme::default`] is what you get when `[tui.theme]`
//! is absent entirely. The defaults match the exact `(Color, Modifier)` pairs
//! the view files used pre-T1, so an unconfigured TUI renders identically
//! (visual parity is an acceptance criterion for this refactor).
//!
//! ## Config is untrusted input
//!
//! [`Theme::from_config`] never panics on a malformed `[tui.theme]` value: an
//! unparseable color name, an unknown modifier, or an out-of-range hex code
//! is mapped back to the default for that slot (the named style's built-in
//! `Color`/`Modifier`), not the whole theme -- a single typo'd `fg` on one
//! slot does not silently wipe another slot's override.
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
//! added here has a rule to follow instead of a nearest-neighbor guess:
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
//!
//! ## Presets, custom themes, and no-color (board item `01M1YVX43MABAVX491HQ5ZCC2M`)
//!
//! Before board item `01M1YVX43MABAVX491HQ5ZCC2M`, the only way to get
//! anything other than the plain ANSI-16 [`Theme::default`] was to restate
//! every slot you cared about by hand. [`ThemePreset`] adds six named,
//! built-in color schemes
//! ([`Theme::from_preset`]); [`Theme::resolve`] is the new top-level entry
//! point `app/startup.rs` calls instead of [`Theme::from_config`] directly
//! -- it composes a preset (or a custom theme file, same per-slot shape as
//! `[tui.theme]`'s object form, loaded from `<config dir>/themes/<name>.json`)
//! with `[tui.theme_overrides]`/the legacy `[tui.theme]` object shape, then
//! applies `NO_COLOR`/`tui.color = false` last via [`Theme::into_no_color`].
//! See `crate::tui::config::ThemeSetting`'s own doc for the config shape
//! decision and [`Theme::resolve`]'s own doc for the full composition
//! order.
//!
//! **No-color keeps modifiers, strips colors, with one exception.**
//! [`Theme::into_no_color`] clears every slot's `fg`/`bg` but keeps every
//! modifier (`BOLD`/`DIM`/`ITALIC`/`REVERSED`/...) -- `selected`
//! (`Modifier::REVERSED`, the cursor) never had a color to lose, so it
//! stays exactly as visible as ever. The one exception is
//! [`Theme::fatal_error`] (the status line's `AUTO-ALLOW` indicator, this
//! module's own highest-alert accent -- see the V7 note above): stripped
//! of its `fg`, it would render IDENTICAL to [`Theme::emphasized`] (the
//! `plan` rung's own style, `BOLD` with no color either), silently
//! reopening the exact "operator can't tell AUTO-ALLOW from plan" failure
//! this accent exists to prevent. `into_no_color` adds `Modifier::UNDERLINED`
//! to `fatal_error` alone to keep that one safety-critical distinction
//! alive without color.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use ratatui::style::{Color, Modifier, Style};

use crate::tui::config::{ThemeConfig, ThemeSetting, ThemeStyleConfig, TuiSection};

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
    /// here.
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
    /// `Ctrl-O`, `PageUp/PageDown`), distinguishing it from the plain
    /// description text beside it. Default: `Modifier::BOLD` (no fg) --
    /// V7: was `Color::Green` + `Modifier::BOLD`. The split only needs
    /// distinguishing, not a status color, and green already means
    /// "success" ([`Theme::tool_done`]/[`Theme::agent_finished`]); reusing
    /// it here for plain layout chrome blurred that meaning for no reason
    /// (see the module doc's "chrome" rule).
    pub help_key: Style,
    /// Board item 01M1YVEJB6GAPST5YZET4KZZE2: an added line in a rendered
    /// `edit`/`write` diff (the permission prompt, the settled transcript
    /// entry, `/diff`). Default: `Color::Green` -- the SAME meaning
    /// [`Theme::tool_done`]/[`Theme::agent_finished`] already give green
    /// (the module doc's "green is success" rule): an added line is what
    /// the call is asking to make true, read the same way a finished tool
    /// call's own tag reads.
    pub diff_add: Style,
    /// Board item 01M1YVEJB6GAPST5YZET4KZZE2: a removed line in a rendered
    /// `edit`/`write` diff. Default: `Color::Red` -- pairs with
    /// [`Theme::diff_add`]'s green the same way a real `diff -u`'s red/green
    /// does, and stays inside the module doc's "red is failure or active
    /// danger" only loosely -- here it means "going away", not a failure,
    /// but red/green as a removed/added pair is universal enough (Claude
    /// Code, Cursor, OpenCode, and Antigravity all use exactly this
    /// pairing for their own diff surfaces) that inventing a different
    /// color for it would cost legibility for no gain.
    pub diff_del: Style,
    /// Board item `01M1YVX43MABAVX491HQ5ZCC2M`: whether this `Theme` was
    /// built WITH color -- `true` for every preset/`from_config` build
    /// ([`Theme::default`]/[`Theme::from_preset`]/[`Theme::from_config`]
    /// all set it `true`); `false` only after [`Theme::into_no_color`],
    /// which [`Theme::resolve`] applies when `NO_COLOR` is present at all
    /// (no-color.org: presence alone disables color, regardless of value)
    /// or `tui.color = false`. Not reachable through [`Theme::overlay`]/
    /// `[tui.theme]`'s per-slot table at all -- there is no `color_enabled`
    /// slot in [`ThemeConfig`] for an override to name. Read by
    /// [`Theme::security_notice_style`] so that style degrades the SAME
    /// way under `NO_COLOR` as every other slot does, without `[tui.theme]`
    /// gaining a new way to reach it (see that method's own doc).
    pub color_enabled: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            user: Style::default().add_modifier(Modifier::BOLD),
            assistant: Style::default(),
            assistant_marker: Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
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
            border_warning: Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            border_danger: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            border_accent: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            status_mode: Style::default().add_modifier(Modifier::REVERSED),
            status_dim: Style::default().add_modifier(Modifier::DIM),
            spinner: Style::default().fg(Color::Yellow),
            header: Style::default().add_modifier(Modifier::REVERSED),
            scroll_footer: Style::default().add_modifier(Modifier::DIM),
            help_border: Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            help_key: Style::default().add_modifier(Modifier::BOLD),
            diff_add: Style::default().fg(Color::Green),
            diff_del: Style::default().fg(Color::Red),
            color_enabled: true,
        }
    }
}

impl Theme {
    /// Builds a [`Theme`] from a loaded `[tui.theme]` config table overlaid
    /// on the built-in defaults -- equivalent to `Theme::default().overlay(config)`.
    /// Each slot's override is applied independently; a malformed value
    /// (unknown color name, unparseable hex, unknown modifier) falls back
    /// to that slot's default for the affected channel -- never a panic on
    /// untrusted config. `None` overrides are no-ops, so an empty
    /// `ThemeConfig` yields `Theme::default()`.
    pub fn from_config(config: &ThemeConfig) -> Self {
        Self::default().overlay(config)
    }

    /// Applies `config`'s per-slot overrides on top of `self`, returning a
    /// new `Theme` -- the general form [`Theme::from_config`] is a
    /// shorthand for (`Theme::default().overlay(config)`). Board item
    /// `01M1YVX43MABAVX491HQ5ZCC2M`'s [`Theme::resolve`] is the other
    /// caller: it overlays onto a PRESET's own built theme rather than the
    /// plain default, so `[tui.theme_overrides]`/the legacy `[tui.theme]`
    /// object shape compose on top of whichever preset (or custom theme
    /// file) is active. Same malformed-value-falls-back-to-the-CURRENT-
    /// slot's-value posture as `from_config` -- never a panic.
    pub fn overlay(&self, config: &ThemeConfig) -> Self {
        let mut theme = self.clone();
        theme.user = overlay_style(theme.user, config.user.as_ref());
        theme.assistant = overlay_style(theme.assistant, config.assistant.as_ref());
        theme.assistant_marker =
            overlay_style(theme.assistant_marker, config.assistant_marker.as_ref());
        theme.reasoning = overlay_style(theme.reasoning, config.reasoning.as_ref());
        theme.timestamp = overlay_style(theme.timestamp, config.timestamp.as_ref());
        theme.tool_proposed = overlay_style(theme.tool_proposed, config.tool_proposed.as_ref());
        theme.tool_awaiting = overlay_style(theme.tool_awaiting, config.tool_awaiting.as_ref());
        theme.tool_running = overlay_style(theme.tool_running, config.tool_running.as_ref());
        theme.tool_done = overlay_style(theme.tool_done, config.tool_done.as_ref());
        theme.tool_failed = overlay_style(theme.tool_failed, config.tool_failed.as_ref());
        theme.agent_starting = overlay_style(theme.agent_starting, config.agent_starting.as_ref());
        theme.agent_running = overlay_style(theme.agent_running, config.agent_running.as_ref());
        theme.agent_awaiting = overlay_style(theme.agent_awaiting, config.agent_awaiting.as_ref());
        theme.agent_finished = overlay_style(theme.agent_finished, config.agent_finished.as_ref());
        theme.agent_failed = overlay_style(theme.agent_failed, config.agent_failed.as_ref());
        theme.agent_cancelled =
            overlay_style(theme.agent_cancelled, config.agent_cancelled.as_ref());
        theme.notice = overlay_style(theme.notice, config.notice.as_ref());
        theme.error = overlay_style(theme.error, config.error.as_ref());
        theme.fatal_error = overlay_style(theme.fatal_error, config.fatal_error.as_ref());
        theme.dim = overlay_style(theme.dim, config.dim.as_ref());
        theme.focused = overlay_style(theme.focused, config.focused.as_ref());
        theme.selected = overlay_style(theme.selected, config.selected.as_ref());
        theme.emphasized = overlay_style(theme.emphasized, config.emphasized.as_ref());
        theme.border_normal = overlay_style(theme.border_normal, config.border_normal.as_ref());
        theme.border_warning = overlay_style(theme.border_warning, config.border_warning.as_ref());
        theme.border_danger = overlay_style(theme.border_danger, config.border_danger.as_ref());
        theme.border_accent = overlay_style(theme.border_accent, config.border_accent.as_ref());
        theme.status_mode = overlay_style(theme.status_mode, config.status_mode.as_ref());
        theme.status_dim = overlay_style(theme.status_dim, config.status_dim.as_ref());
        theme.spinner = overlay_style(theme.spinner, config.spinner.as_ref());
        theme.header = overlay_style(theme.header, config.header.as_ref());
        theme.scroll_footer = overlay_style(theme.scroll_footer, config.scroll_footer.as_ref());
        theme.help_border = overlay_style(theme.help_border, config.help_border.as_ref());
        theme.help_key = overlay_style(theme.help_key, config.help_key.as_ref());
        theme.diff_add = overlay_style(theme.diff_add, config.diff_add.as_ref());
        theme.diff_del = overlay_style(theme.diff_del, config.diff_del.as_ref());
        theme
    }

    /// Board item `01M3TJQGJHFFPWE2YYN60WN1XB` (security review): the one
    /// style NO `[tui.theme]` override can ever reach -- deliberately a
    /// method with no OTHER input than `self.color_enabled`, not a
    /// `Theme` field. Every field on `Theme` is, by construction, something
    /// [`Theme::overlay`]/[`Theme::from_config`] applies a `ThemeStyleConfig`
    /// onto; a field here would just be `theme.error` under a different
    /// name, reachable by the identical `{"error":{"modifiers":["hidden"]}}`
    /// an untrusted project `settings.json` could otherwise use to hide its
    /// own ignore-notice. Bypassing `Theme`'s own fields entirely is what
    /// makes that unreachable: nothing in `overlay`/`overlay_style` ever
    /// touches this method's return value, from ANY `[tui.theme]`/
    /// `[tui.theme_overrides]` source, trusted or not. Used by `tui::state::
    /// transcript::Entry::SecurityNotice`'s own render arm and
    /// `tui::view::status`'s persistent `project config ignored` marker --
    /// see each one's own doc for why.
    ///
    /// **`self.color_enabled` IS read here (board item
    /// `01M1YVX43MABAVX491HQ5ZCC2M`), and that is a DIFFERENT exposure than
    /// the one the paragraph above forecloses.** `NO_COLOR`/`tui.color =
    /// false` are a global, operator/environment-level accessibility
    /// toggle, never a per-slot `[tui.theme]` value -- there is no
    /// `color_enabled` key in [`ThemeConfig`] for an untrusted project to
    /// set, and stripping color never makes text invisible the way the
    /// `"hidden"` modifier this method exists to resist does. Honoring
    /// `NO_COLOR` here is the same "every slot renders the same way under
    /// no-color" guarantee [`Theme::into_no_color`] gives every other slot,
    /// extended to this one deliberately-unreachable style too.
    ///
    /// Still satisfies this module's own T1 convention (`no_inline_style_
    /// default_fg_color_remains_in_view_files`, below): the inline
    /// `Style::default().fg(Color::…)` construction lives HERE, in
    /// `theme.rs`, the one file that owns style construction -- callers
    /// elsewhere get a `Style` back, they never build one themselves.
    pub fn security_notice_style(&self) -> Style {
        let style = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
        if self.color_enabled {
            style
        } else {
            strip_color(style)
        }
    }

    /// Board item `01M1YVX43MABAVX491HQ5ZCC2M`: one of [`ThemePreset`]'s
    /// six built-ins, built directly (never through [`Theme::from_config`]
    /// -- a preset is not a `[tui.theme]` override table). `System` is
    /// `Theme::default()` verbatim, so every existing visual-parity test
    /// above stays true of it unchanged. See [`ThemePreset`]'s own doc for
    /// what each preset assumes about the terminal's background and why.
    pub fn from_preset(preset: ThemePreset) -> Self {
        if preset == ThemePreset::System {
            return Theme::default();
        }
        let p = palette_for(preset);
        let dim = dim_style(p.dim_base);
        Theme {
            user: Style::default().add_modifier(Modifier::BOLD),
            assistant: Style::default(),
            assistant_marker: Style::default().fg(p.blocked).add_modifier(Modifier::BOLD),
            reasoning: dim.add_modifier(Modifier::ITALIC),
            timestamp: dim,
            tool_proposed: Style::default().fg(p.secondary),
            tool_awaiting: Style::default().fg(p.blocked),
            tool_running: Style::default().fg(p.warning),
            tool_done: Style::default().fg(p.success),
            tool_failed: Style::default().fg(p.danger),
            agent_starting: Style::default().fg(p.secondary),
            agent_running: Style::default().fg(p.warning),
            agent_awaiting: Style::default().fg(p.blocked),
            agent_finished: Style::default().fg(p.success),
            agent_failed: Style::default().fg(p.danger),
            agent_cancelled: dim,
            notice: Style::default().fg(p.info),
            error: Style::default().fg(p.danger),
            fatal_error: Style::default().fg(p.danger).add_modifier(Modifier::BOLD),
            dim,
            focused: Style::default().add_modifier(Modifier::BOLD),
            selected: Style::default().add_modifier(Modifier::REVERSED),
            emphasized: Style::default().add_modifier(Modifier::BOLD),
            border_normal: Style::default(),
            border_warning: Style::default().fg(p.warning).add_modifier(Modifier::BOLD),
            border_danger: Style::default().fg(p.danger).add_modifier(Modifier::BOLD),
            border_accent: Style::default().fg(p.info).add_modifier(Modifier::BOLD),
            status_mode: Style::default().add_modifier(Modifier::REVERSED),
            status_dim: dim,
            spinner: Style::default().fg(p.warning),
            header: Style::default().add_modifier(Modifier::REVERSED),
            scroll_footer: dim,
            help_border: Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
            help_key: Style::default().add_modifier(Modifier::BOLD),
            diff_add: Style::default().fg(p.success),
            diff_del: Style::default().fg(p.danger),
            color_enabled: true,
        }
    }

    /// Board item `01M1YVX43MABAVX491HQ5ZCC2M`: `NO_COLOR`/`tui.color =
    /// false` -- strips every slot's `fg`/`bg`, keeping every modifier (a
    /// modifier carries meaning on its own: `REVERSED` IS the selection
    /// highlight, `BOLD` IS the emphasis, independent of color). The one
    /// exception is [`Theme::fatal_error`] -- see this module's own doc,
    /// "No-color keeps modifiers, strips colors, with one exception," for
    /// why it alone gains `Modifier::UNDERLINED` here.
    pub fn into_no_color(self) -> Theme {
        Theme {
            user: strip_color(self.user),
            assistant: strip_color(self.assistant),
            assistant_marker: strip_color(self.assistant_marker),
            reasoning: strip_color(self.reasoning),
            timestamp: strip_color(self.timestamp),
            tool_proposed: strip_color(self.tool_proposed),
            tool_awaiting: strip_color(self.tool_awaiting),
            tool_running: strip_color(self.tool_running),
            tool_done: strip_color(self.tool_done),
            tool_failed: strip_color(self.tool_failed),
            agent_starting: strip_color(self.agent_starting),
            agent_running: strip_color(self.agent_running),
            agent_awaiting: strip_color(self.agent_awaiting),
            agent_finished: strip_color(self.agent_finished),
            agent_failed: strip_color(self.agent_failed),
            agent_cancelled: strip_color(self.agent_cancelled),
            notice: strip_color(self.notice),
            error: strip_color(self.error),
            fatal_error: strip_color(self.fatal_error).add_modifier(Modifier::UNDERLINED),
            dim: strip_color(self.dim),
            focused: strip_color(self.focused),
            selected: strip_color(self.selected),
            emphasized: strip_color(self.emphasized),
            border_normal: strip_color(self.border_normal),
            border_warning: strip_color(self.border_warning),
            border_danger: strip_color(self.border_danger),
            border_accent: strip_color(self.border_accent),
            status_mode: strip_color(self.status_mode),
            status_dim: strip_color(self.status_dim),
            spinner: strip_color(self.spinner),
            header: strip_color(self.header),
            scroll_footer: strip_color(self.scroll_footer),
            help_border: strip_color(self.help_border),
            help_key: strip_color(self.help_key),
            diff_add: strip_color(self.diff_add),
            diff_del: strip_color(self.diff_del),
            color_enabled: false,
        }
    }

    /// Board item `01M1YVX43MABAVX491HQ5ZCC2M`: the ONE place preset
    /// selection, custom-theme-file loading, per-slot overrides (both the
    /// legacy `[tui.theme]` object shape and the new `[tui.theme_overrides]`
    /// sibling), and `NO_COLOR`/`tui.color` color-disabling all compose.
    /// `app/startup.rs` calls this instead of [`Theme::from_config`]
    /// directly.
    ///
    /// **Composition order:** (1) resolve `tui.theme` to a base `Theme` --
    /// a built-in [`ThemePreset`] by name, a custom theme file loaded from
    /// `<config dir>/themes/<name>.json` by the SAME name, or (the legacy
    /// shape) [`Theme::default`] when `tui.theme` is already the per-slot
    /// object; (2) if `tui.theme` WAS that legacy object, overlay it; (3)
    /// overlay `tui.theme_overrides` (applies in both cases -- a preset's
    /// own per-slot top-up, or a second pass over the legacy shape, which
    /// is a harmless no-op for every existing config since nothing wrote
    /// `theme_overrides` before this board item); (4) strip color
    /// entirely if `NO_COLOR` is present at all (no-color.org: presence
    /// alone disables color, regardless of value -- even `NO_COLOR=""`)
    /// or `tui.color == Some(false)`.
    ///
    /// Returns the built theme and an optional load warning -- an unknown
    /// preset/custom-theme name, or a custom theme file that exists but
    /// fails to parse -- for the caller to surface via `Entry::Error {
    /// fatal: false }`, mirroring `crate::tui::keybindings::Keymap::load`'s
    /// own "exists but malformed fails loudly, missing/unresolvable
    /// degrades quietly to a default" posture. Never a startup failure --
    /// config, including the environment, is always untrusted input here.
    pub fn resolve(tui: &TuiSection, env: &HashMap<String, String>) -> (Theme, Option<String>) {
        let (preset_name, legacy_overrides): (String, Option<&ThemeConfig>) = match &tui.theme {
            ThemeSetting::Preset(name) => (name.clone(), None),
            ThemeSetting::Overrides(cfg) => ("system".to_string(), Some(cfg)),
        };

        let (mut theme, warning) = Self::resolve_by_name(&preset_name, env);
        if let Some(cfg) = legacy_overrides {
            theme = theme.overlay(cfg);
        }
        theme = theme.overlay(&tui.theme_overrides);

        // no-color.org: presence alone disables color -- `NO_COLOR=""` (set,
        // but empty) still disables it, exactly like `NO_COLOR=1`.
        let no_color_env = env.contains_key("NO_COLOR");
        if no_color_env || tui.color == Some(false) {
            theme = theme.into_no_color();
        }
        (theme, warning)
    }

    /// The base-theme-by-name half of [`Self::resolve`] (a built-in
    /// [`ThemePreset`] by name, tried first, falling back to a custom theme
    /// file loaded from `<config dir>/themes/<name>.json`), factored out so
    /// [`Self::resolve_named`] -- the `/settings → display → theme` cycle's
    /// own entry point -- can share it exactly rather than re-deriving the
    /// same unknown-name/malformed-file warning text a second time.
    fn resolve_by_name(name: &str, env: &HashMap<String, String>) -> (Theme, Option<String>) {
        if let Some(preset) = ThemePreset::parse(name) {
            (Theme::from_preset(preset), None)
        } else {
            match load_custom_theme_file(env, name) {
                Ok(Some(cfg)) => (Theme::from_config(&cfg), None),
                Ok(None) => (
                    Theme::default(),
                    Some(format!(
                        "[tui.theme] names unknown preset or theme {name:?} \
                         (expected one of {} or a file at <config dir>/themes/{name}.json) \
                         -- using the \"system\" default",
                        ThemePreset::ALL
                            .iter()
                            .map(|p| p.name())
                            .collect::<Vec<_>>()
                            .join("/"),
                    )),
                ),
                Err(reason) => (
                    Theme::default(),
                    Some(format!(
                        "custom theme file for {name:?} failed to load ({reason}) -- \
                         using the \"system\" default"
                    )),
                ),
            }
        }
    }

    /// Board item `01M1YVX43MABAVX491HQ5ZCC2M`, follow-up: the `/settings →
    /// display → theme` row's own entry point (`app/run.rs`'s
    /// `Action::CycleThemePreset` arm) -- rebuilds a `Theme` for `name`
    /// (already resolved by [`next_theme_name`]) the same way [`Self::
    /// resolve`] does for the one `[tui.theme]` names at startup, MINUS the
    /// legacy object-shape overlay (there is no second, independent
    /// `[tui.theme]` object to re-apply once the operator has cycled away
    /// from whatever `[tui.theme]` named at startup -- `overrides` is
    /// `App::theme_overrides`, seeded once from `[tui.theme_overrides]` and
    /// otherwise unchanged by cycling, so a `theme_overrides` top-up
    /// configured at startup still applies to every preset cycled through,
    /// exactly as it would if the operator had set `tui.theme` to that name
    /// directly). `force_no_color` is the session's fixed `NO_COLOR`/
    /// `tui.color` verdict (`App` reads it once, from the theme `App::new`
    /// already resolved, rather than re-reading the environment on every
    /// cycle) -- cycling changes the active NAME, never whether color itself
    /// is allowed.
    pub fn resolve_named(
        name: &str,
        overrides: &ThemeConfig,
        force_no_color: bool,
        env: &HashMap<String, String>,
    ) -> (Theme, Option<String>) {
        let (mut theme, warning) = Self::resolve_by_name(name, env);
        theme = theme.overlay(overrides);
        if force_no_color {
            theme = theme.into_no_color();
        }
        (theme, warning)
    }
}

/// Board item `01M1YVX43MABAVX491HQ5ZCC2M`: clears `fg`/`bg`, keeping
/// every modifier -- the one shared primitive [`Theme::into_no_color`]/
/// [`Theme::security_notice_style`] both apply per-slot. Never touches
/// `add_modifier`/`sub_modifier`, so `BOLD`/`DIM`/`ITALIC`/`REVERSED`
/// survive untouched.
fn strip_color(style: Style) -> Style {
    Style {
        fg: None,
        bg: None,
        ..style
    }
}

/// One built-in color scheme `[tui.theme]` can select by name (the string
/// shape) -- see [`Theme::from_preset`] for how each maps onto every named
/// style, and `crate::tui::config::ThemeSetting` for the config shape that
/// carries this name (or a custom theme file's name instead --
/// [`Theme::resolve`] is where the two are told apart: a name matching one
/// of these six wins; anything else is tried as a custom theme file).
///
/// **`System` is the default** (`[tui.theme]` absent, or set to the
/// literal string `"system"`): it is [`Theme::default`] verbatim -- the
/// plain ANSI-16 color names this module already used before this board
/// item, so an unconfigured TUI still renders identically and still lets
/// the TERMINAL's own palette decide light vs. dark, exactly as it always
/// has. `Dark`/`Light` assume a literal dark/light terminal background and
/// pick concrete RGB accents calibrated for legibility against it
/// (`System` cannot do this -- an ANSI name's actual RGB is whatever the
/// terminal emulator's own palette maps it to, which is the entire point
/// of letting the terminal decide). The remaining three are well-known
/// third-party color schemes, picked for name recognition over a
/// from-scratch palette: `SolarizedDark` (Solarized's dark variant),
/// `Gruvbox` (its dark variant, using its own "bright" accent set for
/// contrast against a dark background), `Nord`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemePreset {
    System,
    Dark,
    Light,
    SolarizedDark,
    Gruvbox,
    Nord,
}

impl ThemePreset {
    /// Every built-in preset, in [`Self::next`]'s own cycle order.
    pub const ALL: [ThemePreset; 6] = [
        ThemePreset::System,
        ThemePreset::Dark,
        ThemePreset::Light,
        ThemePreset::SolarizedDark,
        ThemePreset::Gruvbox,
        ThemePreset::Nord,
    ];

    /// The config-facing name `[tui.theme]`/`CONWAY_TUI__THEME` matches
    /// against, exact and lowercase -- `snake_case`, matching every other
    /// string-valued config vocabulary in this crate (`EditorMode`'s
    /// `"emacs"`/`"vim"`, `BusyInputMode`'s `"queue"`/`"steer"`/
    /// `"interrupt"`).
    pub fn name(self) -> &'static str {
        match self {
            ThemePreset::System => "system",
            ThemePreset::Dark => "dark",
            ThemePreset::Light => "light",
            ThemePreset::SolarizedDark => "solarized_dark",
            ThemePreset::Gruvbox => "gruvbox",
            ThemePreset::Nord => "nord",
        }
    }

    /// The built-in preset named `raw`, exact (case-sensitive, matching
    /// [`Self::name`]'s own lowercase `snake_case`) -- `None` for anything
    /// else, including a custom theme's own name ([`Theme::resolve`] tries
    /// this FIRST and falls back to a custom theme file only when it
    /// returns `None`, never a panic either way).
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == raw)
    }

    /// The next preset in [`Self::ALL`]'s own order, wrapping --
    /// `System -> Dark -> Light -> SolarizedDark -> Gruvbox -> Nord ->
    /// System`. Pure and total (every variant has a successor), so cycling
    /// can never get stuck or panic regardless of which preset is current.
    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|p| *p == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }
}

/// The handful of semantic colors [`Theme::from_preset`] maps onto every
/// named style, for ONE preset -- keeps each preset's definition to "name
/// its danger/warning/blocked/success/info/accent/secondary color plus its
/// dim-annotation base" instead of restating all ~30 slot assignments per
/// preset. Matches this module's own V7 "what each color MEANS" rule
/// (module-level doc, above): `danger` is every red slot, `warning` every
/// yellow, `blocked` every magenta, `success` every green, `info` every
/// cyan (`notice`/`border_accent`), `accent` is `help_border`'s own blue,
/// `secondary` is the gray `tool_proposed`/`agent_starting` pending-state
/// color.
struct Palette {
    danger: Color,
    warning: Color,
    blocked: Color,
    success: Color,
    info: Color,
    accent: Color,
    secondary: Color,
    dim_base: DimBase,
}

/// The base style for every "secondary/annotation" slot (`dim`/
/// `timestamp`/`agent_cancelled`/`status_dim`/`scroll_footer`, and
/// `reasoning` with `Modifier::ITALIC` added on top) -- see
/// [`dim_style`]'s own doc for how each resolves to a concrete [`Style`].
#[derive(Clone, Copy)]
enum DimBase {
    /// `System`/dark-background presets: a bare `Modifier::DIM`, matching
    /// this module's own V7 "never a fixed dark color" rule (module-level
    /// doc) -- a RELATIVE dimming of the terminal's own foreground stays
    /// legible against a dark background.
    Modifier,
    /// `Light`: an explicit mid-gray `fg` instead. `Modifier::DIM`'s
    /// relative dimming is calibrated for a dark background (V7's own
    /// rule, immediately above) and is not guaranteed legible against a
    /// LIGHT one -- a light terminal's own foreground is already dark, and
    /// "dim" rendering of an already-dark color can land anywhere from
    /// "slightly lighter" to "unreadably close to white," entirely at the
    /// terminal emulator's own discretion. An explicit color sidesteps
    /// that uncertainty the same way every other named-color slot already
    /// does.
    Fg(Color),
}

fn dim_style(base: DimBase) -> Style {
    match base {
        DimBase::Modifier => Style::default().add_modifier(Modifier::DIM),
        DimBase::Fg(color) => Style::default().fg(color),
    }
}

/// [`Theme::from_preset`]'s own palette table -- one [`Palette`] per
/// [`ThemePreset`] other than `System` (which shortcuts to
/// [`Theme::default`] before ever calling this). RGB values are each
/// preset's own well-known accent set (Solarized/Gruvbox/Nord each
/// publish one); `Dark`/`Light` are hand-picked for legibility against an
/// assumed dark/light terminal background -- see [`ThemePreset`]'s own
/// doc.
fn palette_for(preset: ThemePreset) -> Palette {
    match preset {
        ThemePreset::System => unreachable!(
            "Theme::from_preset returns Theme::default() for System before calling this"
        ),
        ThemePreset::Dark => Palette {
            danger: Color::Rgb(0xe0, 0x6c, 0x75),
            warning: Color::Rgb(0xe5, 0xc0, 0x7b),
            blocked: Color::Rgb(0xc6, 0x78, 0xdd),
            success: Color::Rgb(0x98, 0xc3, 0x79),
            info: Color::Rgb(0x56, 0xb6, 0xc2),
            accent: Color::Rgb(0x61, 0xaf, 0xef),
            secondary: Color::Rgb(0x9d, 0xa5, 0xb4),
            dim_base: DimBase::Modifier,
        },
        ThemePreset::Light => Palette {
            danger: Color::Rgb(0xc4, 0x1a, 0x1a),
            warning: Color::Rgb(0x9a, 0x67, 0x00),
            blocked: Color::Rgb(0x8f, 0x3f, 0x8f),
            success: Color::Rgb(0x1e, 0x7d, 0x32),
            info: Color::Rgb(0x0b, 0x72, 0x85),
            accent: Color::Rgb(0x1a, 0x56, 0xb0),
            secondary: Color::Rgb(0x5a, 0x5a, 0x5a),
            dim_base: DimBase::Fg(Color::Rgb(0x6e, 0x6e, 0x6e)),
        },
        ThemePreset::SolarizedDark => Palette {
            danger: Color::Rgb(0xdc, 0x32, 0x2f),
            warning: Color::Rgb(0xb5, 0x89, 0x00),
            blocked: Color::Rgb(0xd3, 0x36, 0x82),
            success: Color::Rgb(0x85, 0x99, 0x00),
            info: Color::Rgb(0x2a, 0xa1, 0x98),
            accent: Color::Rgb(0x26, 0x8b, 0xd2),
            secondary: Color::Rgb(0x58, 0x6e, 0x75),
            dim_base: DimBase::Modifier,
        },
        ThemePreset::Gruvbox => Palette {
            danger: Color::Rgb(0xfb, 0x49, 0x34),
            warning: Color::Rgb(0xfa, 0xbd, 0x2f),
            blocked: Color::Rgb(0xd3, 0x86, 0x9b),
            success: Color::Rgb(0xb8, 0xbb, 0x26),
            info: Color::Rgb(0x8e, 0xc0, 0x7c),
            accent: Color::Rgb(0x83, 0xa5, 0x98),
            secondary: Color::Rgb(0xa8, 0x99, 0x84),
            dim_base: DimBase::Modifier,
        },
        ThemePreset::Nord => Palette {
            danger: Color::Rgb(0xbf, 0x61, 0x6a),
            warning: Color::Rgb(0xeb, 0xcb, 0x8b),
            blocked: Color::Rgb(0xb4, 0x8e, 0xad),
            success: Color::Rgb(0xa3, 0xbe, 0x8c),
            info: Color::Rgb(0x88, 0xc0, 0xd0),
            accent: Color::Rgb(0x81, 0xa1, 0xc1),
            secondary: Color::Rgb(0x4c, 0x56, 0x6a),
            dim_base: DimBase::Modifier,
        },
    }
}

/// Board item `01M1YVX43MABAVX491HQ5ZCC2M`: `<config dir>/themes/<name>.json`
/// -- the custom-theme file path for `name` (same directory
/// `conway::config::discovery::user_config_path` resolves `settings.json`
/// into, so `CONWAY_CONFIG_DIR` relocates it exactly as it relocates
/// `history`/`keybindings.json` -- see those two resolvers' own doc for the
/// identical shape). `None` when that function is (no resolvable home
/// directory and `CONWAY_CONFIG_DIR` unset) -- **and also when `name` is not
/// a safe plain file stem** (see `is_safe_theme_name`'s own doc).
/// `tui.theme` is settable from a project's own (lower-trust)
/// `settings.json`, and `name` reaches this function unvalidated from
/// there, so a name like `"../../x"` must never escape `<config
/// dir>/themes/` -- callers (`load_custom_theme_file`) already treat `None`
/// as "fall back to the system default, with a non-fatal warning," the
/// exact same path an unresolvable/unknown name already took, so an unsafe
/// name needs no new error variant of its own. Belt-and-suspenders: even
/// after validating `name` makes a traversal unreachable by construction,
/// the resolved path's own parent is checked against `dir` before this
/// function returns it, so a future change to how the two are joined can
/// never silently reopen the escape this validation closes today.
pub fn custom_theme_path(env: &HashMap<String, String>, name: &str) -> Option<PathBuf> {
    if !is_safe_theme_name(name) {
        return None;
    }
    let dir = themes_dir(env)?;
    let candidate = dir.join(format!("{name}.json"));
    if candidate.parent() != Some(dir.as_path()) {
        return None;
    }
    Some(candidate)
}

/// Whether `name` is safe to join onto `<config dir>/themes/` as
/// `<name>.json` without escaping that directory: non-empty, no NUL byte,
/// no path separator of either kind (`/` -- the one this platform's `Path`
/// component splitting already honors -- and `\\`, checked literally too,
/// since a `tui.theme` value naming a backslash-separated escape must be
/// refused on every platform this binary runs on, not only the one where
/// `Path` treats it as a separator), not `.`/`..`, not absolute, and --
/// as a final defense-in-depth pass over the literal checks above, not a
/// replacement for them, since a literal check is easier to read and audit
/// than reasoning about every platform's own component-splitting rules --
/// resolves to exactly one [`Component::Normal`] when parsed as a `Path`.
fn is_safe_theme_name(name: &str) -> bool {
    if name.is_empty() || name.contains('\0') || name.contains('/') || name.contains('\\') {
        return false;
    }
    if name == "." || name == ".." {
        return false;
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return false;
    }
    let components: Vec<Component> = path.components().collect();
    matches!(components.as_slice(), [Component::Normal(_)])
}

/// The `<config dir>/themes/` directory itself -- the same directory
/// [`custom_theme_path`] joins `<name>.json` onto, factored out so
/// [`custom_theme_names`] can list it without restating the
/// `user_config_path` resolution.
fn themes_dir(env: &HashMap<String, String>) -> Option<PathBuf> {
    conway::config::discovery::user_config_path(env)
        .and_then(|settings| settings.parent().map(|dir| dir.join("themes")))
}

/// Board item `01M1YVX43MABAVX491HQ5ZCC2M`, follow-up: every custom theme
/// name currently sitting in `<config dir>/themes/` (a `*.json` file's own
/// stem), sorted alphabetically -- [`next_theme_name`]'s own source for the
/// `/settings → display → theme` cycle's tail end, after
/// [`ThemePreset::ALL`]'s six built-ins. A name already claimed by a
/// built-in preset is skipped: [`Theme::resolve`]/[`Theme::resolve_named`]
/// both try [`ThemePreset::parse`] FIRST (see that function's own doc), so a
/// same-named file could never be reached by that name anyway, and listing
/// it again would render a duplicate, unreachable row in the cycle. Returns
/// an empty list, never an error, when the directory does not exist or
/// is not readable -- a missing `themes/` directory is the ordinary case,
/// not a misconfiguration (config is untrusted input, never a panic).
pub fn custom_theme_names(env: &HashMap<String, String>) -> Vec<String> {
    let Some(dir) = themes_dir(env) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                return None;
            }
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(|stem| stem.to_string())
        })
        .filter(|name| ThemePreset::parse(name).is_none())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Board item `01M1YVX43MABAVX491HQ5ZCC2M`, follow-up: the `/settings →
/// display → theme` row's own cycle order -- every [`ThemePreset::ALL`]
/// name, in that fixed order, followed by every [`custom_theme_names`]
/// result (alphabetical, re-read fresh on every call, so a theme file added
/// or removed mid-session takes effect the next time the row cycles) --
/// wrapping. `current` is matched by exact name; a name no longer in either
/// list (its custom file was deleted mid-session, say) falls back to index
/// 0 (`"system"`), the same way [`ThemePreset::next`]'s own `unwrap_or(0)`
/// degrades for a stale value rather than panicking. Always returns a name
/// (at minimum `ThemePreset::ALL`'s six built-ins), so the cycle can never
/// get stuck or run out of names to offer.
pub fn next_theme_name(current: &str, env: &HashMap<String, String>) -> String {
    let mut names: Vec<String> = ThemePreset::ALL
        .iter()
        .map(|p| p.name().to_string())
        .collect();
    names.extend(custom_theme_names(env));
    let idx = names.iter().position(|n| n == current).unwrap_or(0);
    names[(idx + 1) % names.len()].clone()
}

/// Loads `name`'s custom theme file (the per-slot [`ThemeConfig`] shape --
/// one schema for "override some slots," whether it arrived inline in
/// `settings.json` or as its own file). `Ok(None)` means the file does not
/// exist (or [`custom_theme_path`] itself returned `None`) -- NOT an
/// error, the caller's cue to fall back to the `"system"` default
/// ([`Theme::resolve`]). `Err` only for a file that EXISTS but is not
/// valid JSON or fails this crate's own `#[serde(deny_unknown_fields)]`
/// schema -- the same "exists but malformed IS an error" posture
/// `crate::tui::keybindings::Keymap::load` already established.
fn load_custom_theme_file(
    env: &HashMap<String, String>,
    name: &str,
) -> Result<Option<ThemeConfig>, String> {
    let Some(path) = custom_theme_path(env, name) else {
        return Ok(None);
    };
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    serde_json::from_str(&contents)
        .map(Some)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Applies one slot's `Option<ThemeStyleConfig>` override on top of its
/// `default` style. `fg`/`bg` strings that don't parse to a ratatui `Color`
/// are silently skipped (the default for that channel is kept); each
/// modifier string that doesn't parse to a ratatui `Modifier` is silently
/// skipped too. `None` returns `default` unchanged. Never panics on untrusted config.
fn overlay_style(default: Style, cfg: Option<&ThemeStyleConfig>) -> Style {
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
/// else (the caller falls back to the default -- never a panic, no
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
        "dark_gray" | "dark-gray" | "dark-grey" | "darkgrey" | "darkgray" => Some(Color::DarkGray),
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
/// `Color::Rgb`. Returns `None` for any other shape, rather than panicking on
/// untrusted config.
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
/// unknown tag (the caller skips it -- never a panic).
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
    use crate::tui::config::ThemeStyleConfig;

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
        assert_eq!(
            t.selected,
            Style::default().add_modifier(Modifier::REVERSED)
        );
    }

    #[test]
    fn default_borders_match_pre_t1() {
        let t = Theme::default();
        assert_eq!(t.border_normal, Style::default());
        assert_eq!(
            t.border_warning,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            t.border_danger,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            t.border_accent,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        );
    }

    #[test]
    fn default_status_mode_is_reversed() {
        let t = Theme::default();
        assert_eq!(
            t.status_mode,
            Style::default().add_modifier(Modifier::REVERSED)
        );
    }

    // ---- new accent styles have sensible defaults ----

    #[test]
    fn default_assistant_marker_is_magenta_bold() {
        let t = Theme::default();
        assert_eq!(
            t.assistant_marker,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD)
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
        assert_eq!(
            t.scroll_footer,
            Style::default().add_modifier(Modifier::DIM)
        );
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
        assert_eq!(
            t.scroll_footer,
            Style::default().add_modifier(Modifier::DIM)
        );
    }

    #[test]
    fn malformed_header_override_falls_back_to_default() {
        let cfg = ThemeConfig {
            header: Some(fg_only("not-a-color")),
            ..Default::default()
        };
        let t = Theme::from_config(&cfg);
        assert_eq!(
            t.header,
            Style::default().add_modifier(Modifier::REVERSED),
            "untrusted config must not panic"
        );
    }

    // ---- T7: /help keybinding overlay ----

    #[test]
    fn default_help_border_is_blue_bold() {
        let t = Theme::default();
        assert_eq!(
            t.help_border,
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD)
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

    /// Board item 01M1YVEJB6GAPST5YZET4KZZE2: `diff_add`/`diff_del` default
    /// to plain `Color::Green`/`Color::Red` -- the same red/green pairing
    /// every mainstream diff-showing tool uses (see [`Theme::diff_del`]'s
    /// own doc).
    #[test]
    fn default_diff_add_is_green_and_diff_del_is_red() {
        let t = Theme::default();
        assert_eq!(t.diff_add, Style::default().fg(Color::Green));
        assert_eq!(t.diff_del, Style::default().fg(Color::Red));
    }

    #[test]
    fn diff_add_and_diff_del_overrides_apply_independently() {
        let cfg = ThemeConfig {
            diff_add: Some(fg_only("light_green")),
            ..Default::default()
        };
        let t = Theme::from_config(&cfg);
        assert_eq!(t.diff_add, Style::default().fg(Color::LightGreen));
        // Untouched slot keeps its default.
        assert_eq!(t.diff_del, Style::default().fg(Color::Red));
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
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            "untrusted config must not panic"
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

    // ---- malformed overrides fall back to defaults, no panic ----

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
        assert_eq!(
            t.spinner,
            Style::default().fg(Color::Yellow),
            "untrusted config must not panic"
        );
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

    // ---- board item 01M1YVX43MABAVX491HQ5ZCC2M: presets ----

    #[test]
    fn theme_preset_name_round_trips_through_parse() {
        for preset in ThemePreset::ALL {
            assert_eq!(ThemePreset::parse(preset.name()), Some(preset));
        }
        assert_eq!(ThemePreset::parse("not-a-real-preset"), None);
        assert_eq!(ThemePreset::parse(""), None);
    }

    #[test]
    fn theme_preset_next_cycles_through_all_six_and_wraps() {
        let mut seen = std::collections::HashSet::new();
        let mut current = ThemePreset::System;
        for _ in 0..ThemePreset::ALL.len() {
            seen.insert(current);
            current = current.next();
        }
        assert_eq!(
            seen.len(),
            ThemePreset::ALL.len(),
            "every preset visited exactly once"
        );
        assert_eq!(
            current,
            ThemePreset::System,
            "the cycle wraps back to the start"
        );
    }

    /// Board item `01M1YVX43MABAVX491HQ5ZCC2M`, follow-up: with no custom
    /// theme files present, `next_theme_name` cycles through exactly the
    /// six built-ins, in `ThemePreset::ALL`'s own order, and wraps -- the
    /// `/settings` row's own cycle, pinned independently of
    /// `ThemePreset::next` (which this delegates to for the built-in-only
    /// case, but is never called directly by the row).
    #[test]
    fn next_theme_name_cycles_through_every_preset_and_wraps_with_no_custom_themes() {
        let env = HashMap::new();
        let mut seen = Vec::new();
        let mut current = "system".to_string();
        for _ in 0..ThemePreset::ALL.len() {
            seen.push(current.clone());
            current = next_theme_name(&current, &env);
        }
        let expected: Vec<String> = ThemePreset::ALL
            .iter()
            .map(|p| p.name().to_string())
            .collect();
        assert_eq!(seen, expected);
        assert_eq!(current, "system", "the cycle wraps back to the start");
    }

    /// A custom theme file in `<config dir>/themes/` extends the cycle past
    /// the six built-ins, alphabetically, before wrapping back to `system`.
    #[test]
    fn next_theme_name_includes_custom_theme_files_after_every_preset() {
        let config_dir = tempfile::tempdir().expect("tempdir");
        let themes_dir = config_dir.path().join("themes");
        std::fs::create_dir_all(&themes_dir).expect("mkdir themes");
        std::fs::write(themes_dir.join("my-custom.json"), "{}").expect("write custom theme file");
        std::fs::write(themes_dir.join("another.json"), "{}").expect("write custom theme file");

        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            config_dir.path().to_string_lossy().to_string(),
        );

        // Last built-in (`nord`) advances into the custom names, alpha
        // order ("another" before "my-custom"), then wraps back to
        // "system" only after BOTH have been visited.
        assert_eq!(next_theme_name("nord", &env), "another");
        assert_eq!(next_theme_name("another", &env), "my-custom");
        assert_eq!(next_theme_name("my-custom", &env), "system");
    }

    /// A custom file that happens to share a built-in's own name is never
    /// listed a second time -- `ThemePreset::parse` already intercepts that
    /// name first (`Theme::resolve`'s own doc), so a duplicate row would be
    /// dead weight the cycle could never actually reach by that name.
    #[test]
    fn custom_theme_names_skips_a_name_already_claimed_by_a_builtin_preset() {
        let config_dir = tempfile::tempdir().expect("tempdir");
        let themes_dir = config_dir.path().join("themes");
        std::fs::create_dir_all(&themes_dir).expect("mkdir themes");
        std::fs::write(themes_dir.join("dark.json"), "{}").expect("write shadowed file");
        std::fs::write(themes_dir.join("my-custom.json"), "{}").expect("write custom theme file");

        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            config_dir.path().to_string_lossy().to_string(),
        );

        assert_eq!(custom_theme_names(&env), vec!["my-custom".to_string()]);
    }

    /// With no `<config dir>/themes/` directory at all (the ordinary case),
    /// `custom_theme_names` degrades to empty rather than erroring.
    #[test]
    fn custom_theme_names_is_empty_when_the_themes_directory_does_not_exist() {
        let config_dir = tempfile::tempdir().expect("tempdir");
        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            config_dir.path().to_string_lossy().to_string(),
        );
        assert_eq!(custom_theme_names(&env), Vec::<String>::new());
    }

    /// Board item `01M1YVX43MABAVX491HQ5ZCC2M`, follow-up:
    /// `Theme::resolve_named` -- the cycle's own theme-builder -- overlays
    /// `theme_overrides` on top of whatever name it is building (the
    /// `App::theme_overrides` field's own contract: a `[tui.theme_overrides]`
    /// top-up must survive every cycle, not just the theme `[tui.theme]`
    /// named at startup).
    #[test]
    fn resolve_named_overlays_theme_overrides_on_top_of_the_named_preset() {
        let overrides = ThemeConfig {
            notice: Some(fg_only("white")),
            ..Default::default()
        };
        let (theme, warning) = Theme::resolve_named("dark", &overrides, false, &HashMap::new());
        assert_eq!(warning, None);
        assert_eq!(theme.notice, Style::default().fg(Color::White));
        let preset_only = Theme::from_preset(ThemePreset::Dark);
        assert_eq!(theme.tool_running, preset_only.tool_running);
    }

    /// `force_no_color` strips color from EVERY preset `resolve_named`
    /// builds, exactly as `Theme::resolve`'s own `NO_COLOR`/`tui.color`
    /// handling does -- the `/settings` row's own "the cycle still changes
    /// the name but colour stays off" contract (`docs/interactive.md`).
    #[test]
    fn resolve_named_strips_color_when_forced() {
        let (theme, _) =
            Theme::resolve_named("dark", &ThemeConfig::default(), true, &HashMap::new());
        assert!(!theme.color_enabled);
        assert_eq!(theme.notice.fg, None);
    }

    /// End to end: composing `next_theme_name` with `Theme::resolve_named`
    /// exactly the way `Action::CycleThemePreset`'s `app/run.rs` arm does
    /// actually changes a rendered slot's color, not just the active name
    /// -- the `/settings` row's own "cycling advances through presets ...
    /// the rendered theme changes" acceptance.
    #[test]
    fn cycling_the_resolved_name_changes_a_rendered_slots_color() {
        let (system, _) =
            Theme::resolve_named("system", &ThemeConfig::default(), false, &HashMap::new());
        let next_name = next_theme_name("system", &HashMap::new());
        let (next, _) =
            Theme::resolve_named(&next_name, &ThemeConfig::default(), false, &HashMap::new());

        assert_ne!(next_name, "system", "the cycle must actually advance");
        assert_ne!(
            system.notice, next.notice,
            "cycling to {next_name:?} must actually change a rendered slot"
        );
    }

    #[test]
    fn system_preset_is_default_verbatim() {
        assert_eq!(Theme::from_preset(ThemePreset::System), Theme::default());
    }

    /// Every non-`System` preset actually changes the palette (otherwise a
    /// preset would be indistinguishable from `system`, defeating the
    /// point of naming it).
    #[test]
    fn every_non_system_preset_differs_from_the_default() {
        for preset in ThemePreset::ALL {
            if preset == ThemePreset::System {
                continue;
            }
            let t = Theme::from_preset(preset);
            assert_ne!(
                t,
                Theme::default(),
                "{preset:?} must render differently than the system default"
            );
        }
    }

    /// V7's "what each color MEANS" grouping (module doc, above) holds for
    /// every preset, not just the ANSI default: slots sharing one semantic
    /// color (e.g. every "success" slot) share one concrete `Color`, for
    /// every built-in.
    #[test]
    fn every_preset_keeps_v7_semantic_color_grouping() {
        for preset in ThemePreset::ALL {
            let t = Theme::from_preset(preset);
            assert_eq!(
                t.tool_done.fg, t.agent_finished.fg,
                "{preset:?}: tool_done/agent_finished must share one success color"
            );
            assert_eq!(
                t.tool_done.fg, t.diff_add.fg,
                "{preset:?}: diff_add must share tool_done's success color"
            );
            assert_eq!(
                t.tool_failed.fg, t.agent_failed.fg,
                "{preset:?}: tool_failed/agent_failed must share one danger color"
            );
            assert_eq!(
                t.tool_failed.fg, t.error.fg,
                "{preset:?}: error must share tool_failed's danger color"
            );
            assert_eq!(
                t.fatal_error.fg, t.border_danger.fg,
                "{preset:?}: fatal_error/border_danger must share one danger color"
            );
            assert_eq!(
                t.tool_awaiting.fg, t.agent_awaiting.fg,
                "{preset:?}: tool_awaiting/agent_awaiting must share one blocked color"
            );
            assert_eq!(
                t.tool_running.fg, t.agent_running.fg,
                "{preset:?}: tool_running/agent_running must share one warning color"
            );
            assert_eq!(
                t.tool_running.fg, t.spinner.fg,
                "{preset:?}: spinner must share tool_running's warning color"
            );
        }
    }

    /// "Conversation text is never colored" and "chrome with no state is
    /// bold or dim, never colored" (module doc, above) hold for every
    /// preset -- these slots have no `fg` in `Theme::default()` and must
    /// not grow one just because a colorful preset is active.
    #[test]
    fn every_preset_keeps_conversation_text_and_plain_chrome_uncolored() {
        for preset in ThemePreset::ALL {
            let t = Theme::from_preset(preset);
            assert_eq!(t.user.fg, None, "{preset:?}: user must stay uncolored");
            assert_eq!(
                t.assistant.fg, None,
                "{preset:?}: assistant must stay uncolored"
            );
            assert_eq!(
                t.border_normal.fg, None,
                "{preset:?}: border_normal must stay uncolored"
            );
            assert_eq!(
                t.focused.fg, None,
                "{preset:?}: focused must stay uncolored"
            );
            assert_eq!(
                t.selected.fg, None,
                "{preset:?}: selected must stay uncolored"
            );
            assert_eq!(
                t.status_mode.fg, None,
                "{preset:?}: status_mode must stay uncolored"
            );
        }
    }

    /// CONSTRAINTS: the permission-mode field's warning color stays
    /// distinguishable from the status line's other two rungs in EVERY
    /// preset -- "distinct from the normal status style" (the spec's own
    /// second option). `fatal_error` (`AUTO-ALLOW`) must carry a real
    /// color no other status-line style carries, and `status_mode`/
    /// `emphasized` (the healthy/`plan` rungs -- `view/status.rs`'s own
    /// `mode_ladder` doc) must stay exactly as colorless as they always
    /// were.
    #[test]
    fn fatal_error_is_distinguishable_from_other_status_styles_in_every_preset() {
        for preset in ThemePreset::ALL {
            let t = Theme::from_preset(preset);
            assert!(
                t.fatal_error.fg.is_some(),
                "{preset:?}: fatal_error must carry a concrete color"
            );
            assert_eq!(
                t.status_mode.fg, None,
                "{preset:?}: the healthy status rung must carry no color of its own"
            );
            assert_eq!(
                t.emphasized.fg, None,
                "{preset:?}: the plan rung must carry no color of its own"
            );
            assert_ne!(
                t.fatal_error, t.status_mode,
                "{preset:?}: AUTO-ALLOW must not render identically to the healthy rung"
            );
            assert_ne!(
                t.fatal_error, t.emphasized,
                "{preset:?}: AUTO-ALLOW must not render identically to the plan rung"
            );
        }
    }

    // ---- Theme::resolve ----

    fn tui_section_with_theme(theme: ThemeSetting) -> crate::tui::config::TuiSection {
        crate::tui::config::TuiSection {
            theme,
            ..Default::default()
        }
    }

    #[test]
    fn resolve_with_no_config_yields_the_system_default_and_no_warning() {
        let (theme, warning) =
            Theme::resolve(&crate::tui::config::TuiSection::default(), &HashMap::new());
        assert_eq!(theme, Theme::default());
        assert_eq!(warning, None);
    }

    #[test]
    fn resolve_a_known_preset_name_matches_from_preset_directly() {
        let tui = tui_section_with_theme(ThemeSetting::Preset("dark".to_string()));
        let (theme, warning) = Theme::resolve(&tui, &HashMap::new());
        assert_eq!(theme, Theme::from_preset(ThemePreset::Dark));
        assert_eq!(warning, None);
    }

    #[test]
    fn resolve_an_unknown_name_falls_back_to_system_with_a_named_warning() {
        let tui = tui_section_with_theme(ThemeSetting::Preset("not-a-real-theme".to_string()));
        let (theme, warning) = Theme::resolve(&tui, &HashMap::new());
        assert_eq!(theme, Theme::default());
        let warning = warning.expect("an unresolvable name must warn, not silently default");
        assert!(
            warning.contains("not-a-real-theme"),
            "the warning must name the unresolved value: {warning}"
        );
    }

    /// P-15: a custom theme file loads from a temp config dir (the per-slot
    /// shape, item 2's own requirement), without touching the real
    /// operator home directory -- `CONWAY_CONFIG_DIR` throughout.
    #[test]
    fn resolve_loads_a_custom_theme_file_from_a_temp_config_dir() {
        let config_dir = tempfile::tempdir().expect("tempdir");
        let themes_dir = config_dir.path().join("themes");
        std::fs::create_dir_all(&themes_dir).expect("mkdir themes");
        std::fs::write(
            themes_dir.join("my-custom.json"),
            r#"{"notice": {"fg": "magenta", "modifiers": ["bold"]}}"#,
        )
        .expect("write custom theme file");

        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            config_dir.path().to_string_lossy().to_string(),
        );
        let tui = tui_section_with_theme(ThemeSetting::Preset("my-custom".to_string()));
        let (theme, warning) = Theme::resolve(&tui, &env);

        assert_eq!(
            warning, None,
            "a custom theme file that parses must not warn"
        );
        assert_eq!(
            theme.notice,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD)
        );
        // Untouched slots keep the system default -- a custom theme file
        // is an override table, same as `[tui.theme]`'s object shape,
        // never a full replacement.
        assert_eq!(theme.tool_running, Style::default().fg(Color::Yellow));
    }

    #[test]
    fn resolve_a_custom_theme_file_that_is_not_valid_json_warns_and_falls_back() {
        let config_dir = tempfile::tempdir().expect("tempdir");
        let themes_dir = config_dir.path().join("themes");
        std::fs::create_dir_all(&themes_dir).expect("mkdir themes");
        std::fs::write(themes_dir.join("broken.json"), "{ not json").expect("write broken file");

        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            config_dir.path().to_string_lossy().to_string(),
        );
        let tui = tui_section_with_theme(ThemeSetting::Preset("broken".to_string()));
        let (theme, warning) = Theme::resolve(&tui, &env);

        assert_eq!(theme, Theme::default());
        assert!(
            warning
                .expect("a malformed custom theme file must warn")
                .contains("broken"),
            "the warning must name the broken file"
        );
    }

    /// Board item (post-merge review fix): `tui.theme` is settable from a
    /// project's own, lower-trust `settings.json`, so a traversal name must
    /// be refused rather than joined onto `<config dir>/themes/` raw --
    /// `custom_theme_path` is the one seam, exercised directly (not through
    /// `Theme::resolve`, which this module's own
    /// `resolve_a_name_that_escapes_the_themes_directory_falls_back_with_a_warning`
    /// covers end to end).
    #[test]
    fn custom_theme_path_refuses_every_traversal_or_malformed_name() {
        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            "/tmp/does-not-need-to-exist-for-this-check".to_string(),
        );
        for name in [
            "../escape",
            "../../etc/passwd",
            "..",
            ".",
            "",
            "a/b",
            "a\\b",
            "/etc/passwd",
        ] {
            assert!(
                custom_theme_path(&env, name).is_none(),
                "{name:?} must be refused, not resolved to a path"
            );
        }
        // A NUL byte: not expressible in a `&str` literal directly, built
        // from a byte vec instead.
        let nul_name = String::from_utf8(vec![b'a', 0u8, b'b']).expect("valid utf8 with a NUL");
        assert!(custom_theme_path(&env, &nul_name).is_none());

        // An ordinary, safe name still resolves, proving the checks above
        // reject by SHAPE, not by refusing every name outright.
        assert!(custom_theme_path(&env, "my-custom-theme").is_some());
    }

    /// The end-to-end proof: a `tui.theme` value shaped like a traversal,
    /// with a real file actually sitting at the escaped location, still
    /// never loads that file -- it degrades to the SAME non-fatal "unknown
    /// theme" warning path an ordinary unresolvable name already takes
    /// (`resolve_an_unknown_name_falls_back_to_system_with_a_named_warning`,
    /// nearby), never a silent read of a file outside `<config
    /// dir>/themes/`.
    #[test]
    fn resolve_a_name_that_escapes_the_themes_directory_falls_back_with_a_warning() {
        let config_dir = tempfile::tempdir().expect("tempdir");
        let themes_dir = config_dir.path().join("themes");
        std::fs::create_dir_all(&themes_dir).expect("mkdir themes");
        // Sitting OUTSIDE `themes/`, one level up -- exactly where
        // `"../escape"` would resolve if `custom_theme_path` joined it
        // unvalidated.
        std::fs::write(
            config_dir.path().join("escape.json"),
            r#"{"notice": {"fg": "magenta"}}"#,
        )
        .expect("write the file sitting at the escape target");

        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            config_dir.path().to_string_lossy().to_string(),
        );
        let tui = tui_section_with_theme(ThemeSetting::Preset("../escape".to_string()));
        let (theme, warning) = Theme::resolve(&tui, &env);

        assert_eq!(
            theme,
            Theme::default(),
            "a traversal name must never load the file sitting at its escape target"
        );
        let warning = warning.expect("a refused traversal name must warn, not silently default");
        assert!(
            warning.contains("../escape"),
            "the warning must name the refused value: {warning}"
        );
    }

    #[test]
    fn resolve_layers_theme_overrides_on_top_of_a_preset() {
        let mut tui = tui_section_with_theme(ThemeSetting::Preset("dark".to_string()));
        tui.theme_overrides = ThemeConfig {
            notice: Some(fg_only("white")),
            ..Default::default()
        };
        let (theme, warning) = Theme::resolve(&tui, &HashMap::new());
        assert_eq!(warning, None);
        assert_eq!(theme.notice, Style::default().fg(Color::White));
        // Every other slot still matches the preset untouched.
        let preset_only = Theme::from_preset(ThemePreset::Dark);
        assert_eq!(theme.tool_running, preset_only.tool_running);
    }

    /// The legacy object shape of `[tui.theme]` -- `tui.theme = {"notice":
    /// ...}` rather than a preset string -- still resolves bit-for-bit
    /// identically to `Theme::from_config` (backward compatibility: "keep
    /// existing per-slot configs working unchanged").
    #[test]
    fn resolve_the_legacy_object_shape_matches_from_config_exactly() {
        let cfg = ThemeConfig {
            notice: Some(fg_only("red")),
            ..Default::default()
        };
        let tui = tui_section_with_theme(ThemeSetting::Overrides(cfg.clone()));
        let (theme, warning) = Theme::resolve(&tui, &HashMap::new());
        assert_eq!(warning, None);
        assert_eq!(theme, Theme::from_config(&cfg));
    }

    #[test]
    fn resolve_honors_tui_color_false() {
        let tui = crate::tui::config::TuiSection {
            color: Some(false),
            ..Default::default()
        };
        let (theme, _) = Theme::resolve(&tui, &HashMap::new());
        assert!(!theme.color_enabled);
        assert_eq!(theme.notice.fg, None);
    }

    /// no-color.org: `NO_COLOR` disables color when it is merely PRESENT,
    /// regardless of its value -- an EMPTY `NO_COLOR` (set, but `""`) must
    /// disable color exactly the same as `NO_COLOR=1` does.
    #[test]
    fn resolve_honors_no_color_env_on_presence_alone() {
        let mut env = HashMap::new();
        env.insert("NO_COLOR".to_string(), "1".to_string());
        let (theme, _) = Theme::resolve(&crate::tui::config::TuiSection::default(), &env);
        assert!(!theme.color_enabled);
        assert_eq!(theme.notice.fg, None);

        let mut empty_env = HashMap::new();
        empty_env.insert("NO_COLOR".to_string(), String::new());
        let (theme, _) = Theme::resolve(&crate::tui::config::TuiSection::default(), &empty_env);
        assert!(
            !theme.color_enabled,
            "an empty-but-present NO_COLOR must still disable color (no-color.org: presence \
             alone is the signal)"
        );
        assert_eq!(theme.notice.fg, None);
    }

    // ---- Theme::into_no_color ----

    #[test]
    fn into_no_color_strips_every_slots_fg_and_bg() {
        let no_color = Theme::default().into_no_color();
        assert!(!no_color.color_enabled);
        for (name, style) in [
            ("user", no_color.user),
            ("assistant", no_color.assistant),
            ("assistant_marker", no_color.assistant_marker),
            ("reasoning", no_color.reasoning),
            ("timestamp", no_color.timestamp),
            ("tool_proposed", no_color.tool_proposed),
            ("tool_awaiting", no_color.tool_awaiting),
            ("tool_running", no_color.tool_running),
            ("tool_done", no_color.tool_done),
            ("tool_failed", no_color.tool_failed),
            ("agent_starting", no_color.agent_starting),
            ("agent_running", no_color.agent_running),
            ("agent_awaiting", no_color.agent_awaiting),
            ("agent_finished", no_color.agent_finished),
            ("agent_failed", no_color.agent_failed),
            ("agent_cancelled", no_color.agent_cancelled),
            ("notice", no_color.notice),
            ("error", no_color.error),
            ("fatal_error", no_color.fatal_error),
            ("dim", no_color.dim),
            ("focused", no_color.focused),
            ("selected", no_color.selected),
            ("emphasized", no_color.emphasized),
            ("border_normal", no_color.border_normal),
            ("border_warning", no_color.border_warning),
            ("border_danger", no_color.border_danger),
            ("border_accent", no_color.border_accent),
            ("status_mode", no_color.status_mode),
            ("status_dim", no_color.status_dim),
            ("spinner", no_color.spinner),
            ("header", no_color.header),
            ("scroll_footer", no_color.scroll_footer),
            ("help_border", no_color.help_border),
            ("help_key", no_color.help_key),
            ("diff_add", no_color.diff_add),
            ("diff_del", no_color.diff_del),
        ] {
            assert_eq!(style.fg, None, "{name}.fg must be stripped");
            assert_eq!(style.bg, None, "{name}.bg must be stripped");
        }
    }

    /// "Modifiers that carry meaning on their own stay" -- the selection
    /// highlight (`REVERSED`) and plain emphasis (`BOLD`) survive
    /// `into_no_color` untouched.
    #[test]
    fn into_no_color_keeps_meaningful_modifiers() {
        let no_color = Theme::default().into_no_color();
        assert!(no_color.selected.add_modifier.contains(Modifier::REVERSED));
        assert!(no_color
            .status_mode
            .add_modifier
            .contains(Modifier::REVERSED));
        assert!(no_color.focused.add_modifier.contains(Modifier::BOLD));
        assert!(no_color.user.add_modifier.contains(Modifier::BOLD));
    }

    /// See this module's own doc, "No-color keeps modifiers, strips
    /// colors, with one exception": `fatal_error` alone gains `UNDERLINED`
    /// so it stays visually distinct from `emphasized` (both are otherwise
    /// bare `BOLD` once color is stripped).
    #[test]
    fn into_no_color_keeps_fatal_error_distinguishable_from_emphasized() {
        let no_color = Theme::default().into_no_color();
        assert_ne!(
            no_color.fatal_error, no_color.emphasized,
            "AUTO-ALLOW must not collapse onto the plan rung's bare BOLD under no-color"
        );
        assert!(no_color
            .fatal_error
            .add_modifier
            .contains(Modifier::UNDERLINED));
        assert!(no_color.fatal_error.add_modifier.contains(Modifier::BOLD));
    }

    /// Board item `01M1YVX43MABAVX491HQ5ZCC2M`'s own P-15 requirement:
    /// render a REAL frame (through the real `view::draw`, not a hand-built
    /// style) with a `NO_COLOR` theme and assert NOT ONE cell anywhere in
    /// the buffer carries a non-`Reset` fg/bg -- several entry kinds that
    /// DO carry color under the default theme (a notice, a non-fatal
    /// error, a fatal error, an `AUTO-ALLOW` status line) are deliberately
    /// present, so this fails against a version that forgets to strip any
    /// one of them.
    #[test]
    fn no_color_theme_renders_with_no_color_sgr_anywhere_in_the_frame() {
        use crate::tui::state::{AppState, Entry};
        use conway::AgentId;

        let mut state = AppState::new(AgentId::new());
        state.transcript.push(Entry::Notice {
            text: "a routine notice".to_string(),
        });
        state.transcript.push(Entry::Error {
            text: "a non-fatal error".to_string(),
            fatal: false,
        });
        state.transcript.push(Entry::Error {
            text: "a fatal error".to_string(),
            fatal: true,
        });
        state.transcript.push(Entry::SecurityNotice {
            text: "project config ignored: /repo/.conway/settings.json".to_string(),
        });
        state.permission_mode = conway::PermissionMode::AutoAllow;

        let no_color_theme = Theme::default().into_no_color();
        let buffer = render_with_theme(&state, &no_color_theme, 100, 30);

        for (i, cell) in buffer.content().iter().enumerate() {
            assert_eq!(
                cell.fg,
                Color::Reset,
                "cell {i} ({:?}) carries a non-Reset fg under NO_COLOR",
                cell.symbol()
            );
            assert_eq!(
                cell.bg,
                Color::Reset,
                "cell {i} ({:?}) carries a non-Reset bg under NO_COLOR",
                cell.symbol()
            );
        }
    }
}
