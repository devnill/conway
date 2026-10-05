//! The TUI's own presentation config: `[tui]` in `settings.json`
//! (`TuiSection`, `ThemeSetting`, `ThemeConfig`, `ThemeStyleConfig`,
//! `StatusLineConfig`).
//!
//! **Stage 2a: moved here from `conway::config::schema`, verbatim in shape.**
//! `conway`, the facade, is what a headless service or IDE embedding conway
//! links -- before this move it still had to parse and validate roughly 34
//! slots of theme and status-line configuration it could never render. This
//! crate is the one reader that actually consumes `[tui]` (`view/theme.rs`
//! builds a ratatui `Theme` from it; `view/status.rs` reads the status-line
//! field order; `app/startup.rs` reads the tool-preview-lines and
//! history-size caps), so the schema now lives where the behavior does.
//!
//! ## How `[tui]` still reaches the CLI
//!
//! `conway::config::ConwayConfig` no longer defines a `tui` field at all --
//! `conway::config::load`/`load_ignoring_user_config` (used by
//! `ConwayBuilder::from_config`/`discover`, `build_conway`'s own choke
//! point) strip a top-level `tui` key out of the merged document before
//! `ConwayConfig`'s `#[serde(deny_unknown_fields)]` deserialize, so an
//! existing `settings.json` carrying `[tui.theme]`/`[tui.status_line]`
//! still loads successfully through the facade (see that function's own
//! doc for why: the alternative is every such file hard-failing to load at
//! all, the opposite of "still works"). The facade records the strip as a
//! `ConfigWarning { code: PresentationConfigIgnored, .. }` -- a caller that
//! is not this crate, and does not separately re-parse `[tui]` itself, is
//! told its value went nowhere rather than being left to wonder why a
//! theme never applied.
//!
//! [`load`] is this crate's own separate read: it calls
//! `conway::config::merge::merged_document` (the same five-source
//! precedence merge `load` performs internally, exposed as raw JSON before
//! that strip) and deserializes the `tui` key back out of it into
//! [`TuiSection`] -- SAME file(s), SAME precedence (default < user config <
//! project < env < CLI's `--config`), SAME `CONWAY_TUI__*` env var mapping,
//! independently `#[serde(deny_unknown_fields)]`-checked against this
//! crate's own schema. A typo inside `[tui.theme]` still fails loudly for
//! the CLI; the facade just does not know to look for one.

use conway::FacadeError;

use crate::cli::Cli;

/// `[tui]` (TUI-only options). Read at `App::new` via [`load`]; the
/// `conway-cli` TUI reads `.theme`/`.status_line`/`.tool_preview_lines`/
/// `.history_size` at startup. `conway::config::ConwayConfig` never names
/// this type -- see this module's own doc.
#[derive(Clone, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TuiSection {
    /// Board item `01M1YVX43MABAVX491HQ5ZCC2M`: either a built-in/custom
    /// PRESET name (the new string shape) or the per-slot override table
    /// this key has always been (unchanged -- see [`ThemeSetting`]'s own
    /// doc for why the field stays named `theme`, and why this is an
    /// `#[serde(untagged)]` enum rather than a second, differently-named
    /// field). `crate::tui::view::theme::Theme::resolve` is what reads
    /// this (and [`Self::theme_overrides`]/[`Self::color`]) into a built
    /// [`crate::tui::view::theme::Theme`] -- `app/startup.rs` calls it
    /// instead of `Theme::from_config` directly.
    #[serde(default)]
    pub theme: ThemeSetting,
    /// Board item `01M1YVX43MABAVX491HQ5ZCC2M`: per-slot overrides applied
    /// ON TOP of whatever [`Self::theme`] resolves to (a built-in preset, a
    /// custom theme file, or -- the legacy shape -- [`Self::theme`] itself
    /// already being the override table). The spec's own worked example
    /// (`tui.theme = "light"` plus `tui.theme_overrides.<slot>`) is this
    /// field; see [`ThemeSetting`]'s own doc for the full shape decision.
    /// Same schema as the object form of [`Self::theme`] -- one override
    /// schema, whether it arrives inline as the legacy `tui.theme` object
    /// or here, alongside a preset name.
    #[serde(default)]
    pub theme_overrides: ThemeConfig,
    /// Board item `01M1YVX43MABAVX491HQ5ZCC2M`: `Some(false)` renders
    /// without color (fg/bg), same as the `NO_COLOR` environment variable
    /// (per no-color.org: present and non-empty) -- either disables it.
    /// `None` (the default) and `Some(true)` both mean "on", mirroring
    /// [`Self::tool_preview_lines`]'s own `Option<u32>` "`None` means the
    /// built-in default" shape rather than `bool`'s own `#[derive(Default)]`
    /// (which would default to `false`, the wrong way round for a feature
    /// that must stay on unless something turns it off). See
    /// `crate::tui::view::theme::Theme::resolve`/`Theme::into_no_color` for
    /// what "without color" keeps (modifiers that carry meaning on their
    /// own, like `REVERSED`/`BOLD`, stay; only `fg`/`bg` are stripped).
    #[serde(default)]
    pub color: Option<bool>,
    /// `[tui.status_line]`: declarative status-line field order +
    /// visibility. `fields` is the ordered list of field names to render; a
    /// field absent from the list is hidden, and the list's order is the
    /// render order. Unknown names are silently dropped at render time
    /// (config is untrusted input, never a panic). Default = the
    /// Lean line
    /// `["session","lineage","mode","model","ctx","tokens","activity","hint"]`.
    /// See `docs/interactive.md`'s "The status line" section for the
    /// full field list.
    #[serde(default)]
    pub status_line: StatusLineConfig,
    /// `[tui.status_line_command]` (board item
    /// `01M0X500861X9035QJEA82F94K`): an operator-configured command whose
    /// stdout becomes a status-line entry, via `conway.statusline` -- the
    /// migration home for a Claude Code `statusLine.type`/
    /// `statusLine.command` pair, which [`StatusLineConfig`]'s own closed
    /// ten-name vocabulary cannot express by design.
    ///
    /// **It lives HERE, and not in `conway`'s `[plugins]` section, because
    /// an architecture invariant says so and caught it.** It shipped in
    /// `conway::config::schema::PluginsConfig` first, and
    /// `crates/conway/tests/architecture_invariants.rs`'s T7 rejected it:
    /// the facade's config schema must not name a presentation-shaped
    /// type, so a headless host never parses terminal vocabulary it can
    /// never render. A status line is presentation by definition. This
    /// module is exactly where Stage 2a put every other such slot.
    ///
    /// **Off by construction when [`StatusLineCommandConfig::command`] is
    /// empty** (its own [`Default`]):
    /// `crates/conway-cli/src/statusline_plugin.rs::install` does not
    /// attach the plugin at all in that case, so `conway.statusline`
    /// linked into the binary with nothing written here is a true no-op --
    /// zero process spawns, zero contributions.
    ///
    /// **Trust, stated here because this is the field that grants the
    /// capability.** The command is code THIS process executes with the
    /// operator's own privileges, on the identical footing
    /// `[hooks].rules[].command` and `[plugins].subprocess[].command`
    /// already have: no sandboxing, no digest check, the operator's own
    /// review of what they typed is the only control point.
    /// `docs/plugins/statusline.md` states this plainly for an operator
    /// deciding whether to configure it.
    #[serde(default)]
    pub status_line_command: StatusLineCommandConfig,
    /// `[tui.tool_preview_lines]`: the cap on collapsed tool-preview
    /// lines in the TUI transcript. A tool entry whose stored `preview` has
    /// more physical lines than this renders the first N lines + a dim
    /// `… (+M lines, Ctrl-O to expand)` affordance while the entry's
    /// `expanded` flag is `false`; the full preview renders while `true`.
    /// The stored preview is never truncated -- the cap is render-time only.
    /// `None` (the default) means the TUI's built-in default of 3. The TUI
    /// clamps a loaded value to `1..=200` with a fallback to 3 on a
    /// missing/out-of-range/bad value (config is untrusted input,
    /// never a panic). `CONWAY_TUI__TOOL_PREVIEW_LINES=10` overrides via
    /// env.
    #[serde(default)]
    pub tool_preview_lines: Option<u32>,
    /// `[tui.history_size]`: the cap on the persisted input-history
    /// FIFO (`~/.conway/history`, or `$CONWAY_CONFIG_DIR/history` when
    /// set -- see `conway::config::discovery::history_file_path`). Loaded at
    /// startup and appended to on every submit; oldest entries are evicted
    /// once the cap is exceeded. `None` (the default) means the TUI's
    /// built-in default of 500. The TUI clamps a loaded value to
    /// `1..=100_000` with a fallback to 500 on a missing/out-of-range/bad
    /// value (config is untrusted input, never a panic).
    /// `CONWAY_TUI__HISTORY_SIZE=1000` overrides via env.
    #[serde(default)]
    pub history_size: Option<u32>,
    /// `[tui.busy_input]` (board item `01M1YVHKTQVXJRDSRYT3TCRXFX`,
    /// "Typing while the agent works"): what submitting a message while the
    /// focused agent's turn is still running does. `Steer` (the default --
    /// board item `01M44PK089DF2M9TM3C4P5CKMZ`) delivers it at the running
    /// turn's own next tool-loop step via the existing steer primitive;
    /// `Queue` withholds the message until the turn boundary instead;
    /// `Interrupt` (board item
    /// `01M3XGPGT5W7GABVTC7F2NA0C9`) aborts the focused agent's current
    /// turn, waits for it to report idle, then sends the message into the
    /// same live agent. See [`crate::tui::state::BusyInputMode`]'s own doc
    /// for the full semantics and `docs/interactive.md`'s "Typing while the
    /// agent works" section for the operator-facing description.
    ///
    /// This is the one display-preference setting (of the `/settings`
    /// menu's "display" group) with a real backing config key -- loaded
    /// here as the session's STARTING value; `/settings` changes it for
    /// the rest of the session only (same "session-only" posture
    /// `view/settings.rs`'s own module doc states for `show_reasoning`/
    /// `show_timestamps`, and the same shape `tool_preview_lines` already
    /// established: config-seeded, session-adjustable, never written
    /// back).
    #[serde(default)]
    pub busy_input: BusyInputMode,
    /// `[tui.editor_mode]` (board item `01M1YVJNS575YN5DCQG9BKZR4E`, opt-in
    /// vim prompt editing): `emacs` (the default -- today's readline-shaped
    /// keymap, unchanged) or `vim`, which layers INSERT/NORMAL modal editing
    /// on top of the SAME input widget -- see [`crate::tui::input::vim`]'s
    /// own module doc for the supported subset. Seeded here as the
    /// session's STARTING value; the `/settings` menu's "display" group
    /// toggles it for the rest of THIS session only, the same session-only
    /// posture [`Self::busy_input`] already has.
    #[serde(default)]
    pub editor_mode: EditorMode,
}

/// `[tui.editor_mode]`'s two values -- see [`TuiSection::editor_mode`]'s own
/// doc. `vim` never rebinds a single entry of [`crate::tui::keybindings::
/// ACTIONS`] -- it is a modal layer the input widget consults BEFORE its
/// ordinary text-editing fallback, not a second keymap (see
/// `crate::tui::input::vim`'s own module doc for the full mechanism and
/// precedence).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorMode {
    #[default]
    Emacs,
    Vim,
}

/// `[tui.busy_input]`'s three modes -- see [`TuiSection::busy_input`]'s own
/// doc for what each does and [`crate::tui::state::BusyInputMode`] for the
/// re-exported session-side type this deserializes into
/// (`AppState::busy_input`). Kept here, alongside every other presentation
/// config type this module owns (module doc: "this crate is the one reader
/// that actually consumes `[tui]`"), with `state::busy_input` re-exporting
/// it rather than defining a second, independent copy.
///
/// **`Interrupt` existed here through review round 2 of board item
/// `01M1YVHKTQVXJRDSRYT3TCRXFX`, was removed by orchestrator ruling because
/// this runtime's only cancellation primitive at the time
/// (`CancelMode::Immediate`, the same one `Ctrl-C` used) was UNCONDITIONALLY
/// TERMINAL for a kept-alive agent -- there was no "cancel this reply, keep
/// the session" outcome for `interrupt` to have delivered the new message
/// into -- and is reinstated here by board item
/// `01M3XGPGT5W7GABVTC7F2NA0C9`, which built exactly that primitive**
/// (`SessionHandle::abort_turn` -- `conway_runtime::tree::AgentTree::
/// abort_turn`'s own doc has the full mechanism). `Interrupt` now aborts the
/// FOCUSED agent's current turn (via `App::handle_busy_input_interrupt`),
/// waits until it reports `awaiting_prompt`, then sends the held message
/// into the same live agent -- never ending the session, exactly the
/// guarantee that was missing before. A `settings.json` written during the
/// window this value was removed (`"interrupt"`, silently normalized to
/// `"queue"` with a startup warning) now simply loads as `Interrupt` again,
/// like any other valid value -- no special-casing left.
///
/// **`Steer` is the default (board item `01M44PK089DF2M9TM3C4P5CKMZ`,
/// operator ruling), not `Queue`.** DOGFOOD 4 (finding 15,
/// `.conway/dogfood/round4-20261004/notes.md`) found `queue`'s own failure
/// mode worse than `steer`'s: a message typed while the agent was busy sat
/// silently in `AppState::held_prompts` until the NEXT turn boundary, with
/// no way for the operator to tell, from the transcript alone, whether it
/// had been seen at all -- indistinguishable, on screen, from a message
/// that was simply lost. `steer` delivers into the mailbox the instant it
/// is submitted (`conway_runtime::mailbox`'s own doc: "landing at `target`'s
/// next turn boundary", already durable the moment `SessionHandle::steer`
/// returns), and (closing the gap that same finding named) a steer that
/// arrives after the running turn's own last delivery opportunity now
/// starts a fresh turn on its own rather than sitting unconsumed until the
/// operator's next message -- see `conway_runtime::agent_loop`'s own
/// `ResumeGate` doc for the mechanism. A `settings.json` that never named
/// `[tui.busy_input]` at all (the overwhelming majority, since this key
/// predates this ruling) now starts a session in `steer`, not `queue` --
/// this is a deliberate behavior change, not a migration concern: there is
/// no stored value to migrate away from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusyInputMode {
    Queue,
    #[default]
    Steer,
    Interrupt,
}

/// `[tui.status_line]`: declarative status-line field order + visibility.
/// The `fields` list is the ordered set of field names the TUI
/// renders, left to right; a field not in the list is hidden, and the list
/// order is the render order. Unknown names are dropped at render time
/// (config is untrusted input: never a panic). Defaults to the Lean line
/// `["session","lineage","mode","model","ctx","tokens","activity","hint"]`.
///
/// Available field names (see `docs/interactive.md`): `session`,
/// `lineage`, `mode`, `model`, `ctx`, `tokens`, `activity`, `hint`, `git`,
/// `cwd`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StatusLineConfig {
    /// Ordered field names to render. Default = Lean line.
    pub fields: Vec<String>,
}

impl Default for StatusLineConfig {
    fn default() -> Self {
        Self {
            fields: vec![
                "session".to_string(),
                "lineage".to_string(),
                "mode".to_string(),
                "model".to_string(),
                "ctx".to_string(),
                "tokens".to_string(),
                "activity".to_string(),
                "hint".to_string(),
            ],
        }
    }
}

/// `[tui.theme]`'s value shape (board item `01M1YVX43MABAVX491HQ5ZCC2M`):
/// either a plain string naming a PRESET -- one of `Theme`'s six built-ins
/// (`"system"`, `"dark"`, `"light"`, `"solarized_dark"`, `"gruvbox"`,
/// `"nord"`) or a custom theme's name (a file at
/// `<config dir>/themes/<name>.json`, same shape as [`ThemeConfig`] --
/// `crate::tui::view::theme::Theme::resolve` tries built-ins first, then
/// that file) -- or [`ThemeConfig`] directly, the per-slot override table
/// this key has ALWAYS been.
///
/// **Shape decision, and why: `#[serde(untagged)]` on the SAME key, not a
/// second field.** The spec's own two worked alternatives were `tui.theme =
/// "light"` plus a sibling `tui.theme_overrides.<slot>`, or `tui.theme.preset
/// = "light"` alongside slot keys nested one level deeper. This picks the
/// FIRST shape, but keeps it anchored on `tui.theme` itself staying the
/// well-known key name (not renamed to, say, `tui.theme_name`): every
/// existing `settings.json` with `"theme": {"user": {...}, ...}` (an
/// object) keeps parsing EXACTLY as [`ThemeConfig`] today, byte for byte,
/// because `untagged` tries [`ThemeSetting::Preset`] (a bare JSON string)
/// first and falls through to [`ThemeSetting::Overrides`] (any JSON
/// object) -- an existing config's shape never changes, so "keep existing
/// per-slot configs working unchanged" holds by construction, not by a
/// migration step. The second shape (`tui.theme.preset`) was rejected
/// because it would have forced TODAY's per-slot keys down one more
/// nesting level (`tui.theme.theme.user`, or a parallel `tui.theme.slots.*`
/// -- either way, a breaking rename for every existing config), the exact
/// thing the first shape's `theme_overrides` sibling avoids.
///
/// `CONWAY_TUI__THEME=dark` (item 3's own required env override) falls out
/// of this for free: `conway::config::merge`'s env layer writes a bare JSON
/// string at `tui.theme`, which `untagged` resolves to
/// [`ThemeSetting::Preset`] exactly like a `settings.json` string would --
/// no special-cased env handling needed, the SAME layered precedence (env
/// over file) `CONWAY_TUI__TOOL_PREVIEW_LINES` and friends already have.
/// Setting it alongside an existing object-shaped `tui.theme` in a
/// LOWER-precedence source is not a composition this needs to resolve: env
/// always wins outright at that leaf path, same as any other scalar
/// `CONWAY_TUI__*` override of an object-shaped key would.
///
/// **`#[serde(untagged)]` on [`Serialize`](serde::Serialize) only -- NOT on
/// [`Deserialize`](serde::Deserialize), which this type implements by hand
/// below.** Serde's derived untagged `Deserialize` buffers the input and
/// tries each variant in turn, discarding every variant's own error on
/// failure and reporting only a generic "data did not match any variant"
/// once all have failed -- which would have silently swallowed
/// `a_typo_inside_tui_theme_is_a_surfaced_parse_error`'s own requirement
/// that a typo'd slot name inside the object shape still names itself in
/// the error. The hand-written impl below dispatches on the JSON value's
/// own shape FIRST (string vs. object) rather than trying both blindly, so
/// the object arm's `serde_json::from_value::<ThemeConfig>` failure -- with
/// `#[serde(deny_unknown_fields)]`'s own specific, field-naming message --
/// passes straight through untouched.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub enum ThemeSetting {
    /// A preset or custom-theme NAME -- resolved against `Theme`'s six
    /// built-ins first, then `<config dir>/themes/<name>.json`, by
    /// `crate::tui::view::theme::Theme::resolve`. An unresolvable name
    /// falls back to the `"system"` default there (config is untrusted
    /// input, never a panic) with a non-fatal startup notice naming it --
    /// never surfaced here, since this type has no way to perform that
    /// file-system lookup itself (it has no `env`/config-dir access, only
    /// the raw deserialized string).
    Preset(String),
    /// The per-slot override table -- unchanged from before this board
    /// item; see [`ThemeConfig`]'s own doc.
    Overrides(ThemeConfig),
}

impl<'de> serde::Deserialize<'de> for ThemeSetting {
    /// See this type's own doc for why this is hand-written rather than
    /// `#[derive(Deserialize)]` + `#[serde(untagged)]`.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::String(name) => Ok(ThemeSetting::Preset(name)),
            other => serde_json::from_value(other)
                .map(ThemeSetting::Overrides)
                .map_err(serde::de::Error::custom),
        }
    }
}

impl Default for ThemeSetting {
    /// The ordinary unconfigured case (`[tui.theme]` absent entirely):
    /// an empty override table, resolving to the `"system"` preset with no
    /// overrides -- bit-for-bit what `[tui]` absent always rendered before
    /// this board item.
    fn default() -> Self {
        ThemeSetting::Overrides(ThemeConfig::default())
    }
}

/// `[tui.theme]`: a per-named-style override table. Each entry is an
/// `Option<ThemeStyleConfig>` -- `None` (the default for every slot) means
/// "use the TUI's built-in default for this named style"; `Some` overlays
/// `fg`/`bg`/`modifiers` on top of the default. The TUI resolves the
/// strings to ratatui `Color`/`Modifier` values and maps any unparseable
/// or out-of-range value back to the default for that slot (config
/// is untrusted input, never a panic). Every field is `Option` so a user
/// can override just one named style without restating the rest.
///
/// Field names match the `Theme` slot names in
/// `crates/conway-cli/src/tui/view/theme.rs` one-for-one. Also the exact
/// shape a custom theme file (`<config dir>/themes/<name>.json`) uses --
/// see [`ThemeSetting`]'s own doc.
#[derive(Clone, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ThemeConfig {
    pub user: Option<ThemeStyleConfig>,
    pub assistant: Option<ThemeStyleConfig>,
    pub assistant_marker: Option<ThemeStyleConfig>,
    pub reasoning: Option<ThemeStyleConfig>,
    /// T4: the `HH:MM ` timestamp prefix prepended to each entry's first
    /// rendered line while `show_timestamps` is on.
    pub timestamp: Option<ThemeStyleConfig>,
    pub tool_proposed: Option<ThemeStyleConfig>,
    pub tool_awaiting: Option<ThemeStyleConfig>,
    pub tool_running: Option<ThemeStyleConfig>,
    pub tool_done: Option<ThemeStyleConfig>,
    pub tool_failed: Option<ThemeStyleConfig>,
    pub agent_starting: Option<ThemeStyleConfig>,
    pub agent_running: Option<ThemeStyleConfig>,
    pub agent_awaiting: Option<ThemeStyleConfig>,
    pub agent_finished: Option<ThemeStyleConfig>,
    pub agent_failed: Option<ThemeStyleConfig>,
    pub agent_cancelled: Option<ThemeStyleConfig>,
    pub notice: Option<ThemeStyleConfig>,
    pub error: Option<ThemeStyleConfig>,
    pub fatal_error: Option<ThemeStyleConfig>,
    pub dim: Option<ThemeStyleConfig>,
    pub focused: Option<ThemeStyleConfig>,
    pub selected: Option<ThemeStyleConfig>,
    pub emphasized: Option<ThemeStyleConfig>,
    pub border_normal: Option<ThemeStyleConfig>,
    pub border_warning: Option<ThemeStyleConfig>,
    pub border_danger: Option<ThemeStyleConfig>,
    pub border_accent: Option<ThemeStyleConfig>,
    pub status_mode: Option<ThemeStyleConfig>,
    pub status_dim: Option<ThemeStyleConfig>,
    pub spinner: Option<ThemeStyleConfig>,
    /// T6: the sticky context header shown above the transcript while it
    /// overflows the viewport (`session · focused agent · model · ctx%`).
    pub header: Option<ThemeStyleConfig>,
    /// T6: the floating "jump to bottom" footer pill shown over the bottom
    /// row of the transcript while scrolled up (`!follow_tail`).
    pub scroll_footer: Option<ThemeStyleConfig>,
    /// T7: the `/help` keybinding overlay's block border.
    pub help_border: Option<ThemeStyleConfig>,
    /// T7: the key/chord column in the `/help` keybinding overlay's rows
    /// (e.g. `Ctrl-O`, `PageUp/PageDown`).
    pub help_key: Option<ThemeStyleConfig>,
    /// Board item 01M1YVEJB6GAPST5YZET4KZZE2: an added line in a rendered
    /// `edit`/`write` diff (the permission prompt, the settled transcript
    /// entry, `/diff`).
    pub diff_add: Option<ThemeStyleConfig>,
    /// Board item 01M1YVEJB6GAPST5YZET4KZZE2: a removed line in a rendered
    /// `edit`/`write` diff.
    pub diff_del: Option<ThemeStyleConfig>,
}

/// One `[tui.theme.<name>]` entry: foreground/background color names plus a
/// modifier tag list. All fields are optional -- a `None`/empty field means
/// "leave the named style's default for that channel untouched". The TUI
/// parses `fg`/`bg` as ratatui color names (`"cyan"`, `"dark_gray"`,
/// `"#ff00ff"`, ...) and `modifiers` as ratatui modifier names
/// (`"bold"`, `"dim"`, `"italic"`, `"reversed"`, ...); any unrecognized
/// value falls back to the default -- config is untrusted input -- never a panic.
#[derive(Clone, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ThemeStyleConfig {
    pub fg: Option<String>,
    pub bg: Option<String>,
    #[serde(default)]
    pub modifiers: Vec<String>,
}

/// Loads `[tui]` from the SAME layered `settings.json`
/// discovery/precedence/env sources `build_conway` uses to build the
/// `Conway` this session runs against -- `cli.config`, when set, is the
/// same explicit path `ConwayBuilder::from_config` would read; `cwd`/`env`
/// mirror `conway::config::LoadOptions::default()`, the same defaults
/// `ConwayBuilder::discover` uses. Returns [`TuiSection::default`] when
/// `[tui]` is absent entirely -- an ordinary, unconfigured TUI. A `[tui]`
/// block present but malformed against THIS schema (an unknown key, a
/// wrong-shaped value) is a surfaced, named parse error -- the CLI keeps
/// `#[serde(deny_unknown_fields)]`'s typo protection for its own
/// presentation config even though the facade no longer can.
///
/// **Board item `01M3XGPGT5W7GABVTC7F2NA0C9`:** this function used to
/// normalize a removed `busy_input: "interrupt"` value to `"queue"` with a
/// startup warning (see git history for the mechanism, if needed) --
/// `Interrupt` is a real [`BusyInputMode`] value again, so a `settings.json`
/// naming it now just loads, like any other valid value.
pub fn load(cli: &Cli) -> conway::Result<TuiSection> {
    let options = conway::config::LoadOptions {
        explicit_path: cli.config.clone(),
        ..conway::config::LoadOptions::default()
    };
    load_from_options(options)
}

fn load_from_options(options: conway::config::LoadOptions) -> conway::Result<TuiSection> {
    // Board item `01M3TJQGJHFFPWE2YYN60WN1XB` (security review): trust-gated,
    // not plain `merged_document` -- an untrusted project `settings.json`
    // must contribute NOTHING, including a `[tui.theme]` block that could
    // otherwise hide the very notice/status-line marker that names it (by
    // styling `theme.error` -- the channel both render through --
    // `{"modifiers":["hidden"]}`). See `conway::config::merge::
    // load_trust_gated`'s own doc for why this needs the SAME gate
    // `ConwayBuilder::discover` applies, not a second, independent opinion.
    let mut merged = conway::config::merged_document_trust_gated(&options)?;
    let tui_value = merged.as_object_mut().and_then(|obj| obj.remove("tui"));
    let tui_value = match tui_value {
        Some(value) => value,
        None => return Ok(TuiSection::default()),
    };
    serde_json::from_value(tui_value).map_err(|e| FacadeError::Config {
        path: None,
        message: format!("failed to parse [tui]: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Test helper: every fixture in this module writes `[tui.theme]` as
    /// the legacy per-slot OBJECT shape, never the new preset-string shape
    /// -- so every one of them resolves to [`ThemeSetting::Overrides`].
    /// Panics (with the actual value) if that ever stops being true, so a
    /// future edit that changes a fixture's shape fails loudly here rather
    /// than via a confusing downstream assertion mismatch.
    fn theme_overrides(tui: &TuiSection) -> &ThemeConfig {
        match &tui.theme {
            ThemeSetting::Overrides(cfg) => cfg,
            other => panic!("expected the legacy object shape, got {other:?}"),
        }
    }

    /// A `settings.json` with no `[tui]` key at all loads to every
    /// built-in default -- the ordinary, unconfigured case.
    #[test]
    fn absent_tui_section_loads_to_defaults() {
        let cwd_dir = tempfile::tempdir().expect("tempdir");
        let user_config_dir = tempfile::tempdir().expect("tempdir");
        let path = cwd_dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"default_role": "coder", "roles": {"coder": {"chain": []}}}"#,
        )
        .expect("write settings.json");

        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            user_config_dir.path().to_string_lossy().to_string(),
        );
        let options = conway::config::LoadOptions {
            cwd: cwd_dir.path().to_path_buf(),
            explicit_path: Some(path),
            env,
            cli_overrides: conway::config::CliOverrides::default(),
            model_metadata_refresh: false,
        };
        let tui = load_from_options(options).expect("load must succeed");
        assert_eq!(tui, TuiSection::default());
    }

    /// A real `settings.json` carrying a full `[tui.theme]` block loads
    /// end to end into this crate's own [`TuiSection`] -- the ANCHOR half
    /// of the board item's verification requirement that lives in this
    /// crate: a real config file, a real theme block, reaching this
    /// crate's own schema (not merely a unit test of the facade's parser).
    #[test]
    fn a_real_settings_json_with_a_full_theme_block_loads_into_tui_section() {
        let cwd_dir = tempfile::tempdir().expect("tempdir");
        let user_config_dir = tempfile::tempdir().expect("tempdir");
        let path = cwd_dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
                "default_role": "coder",
                "roles": {"coder": {"chain": []}},
                "tui": {
                    "theme": {
                        "user": {"fg": "cyan", "modifiers": ["bold"]},
                        "error": {"fg": "red", "bg": "black"}
                    },
                    "status_line": {"fields": ["session", "hint"]},
                    "tool_preview_lines": 7,
                    "history_size": 42
                }
            }"#,
        )
        .expect("write settings.json");

        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            user_config_dir.path().to_string_lossy().to_string(),
        );
        let options = conway::config::LoadOptions {
            cwd: cwd_dir.path().to_path_buf(),
            explicit_path: Some(path),
            env,
            cli_overrides: conway::config::CliOverrides::default(),
            model_metadata_refresh: false,
        };
        let tui = load_from_options(options).expect("load must succeed");

        assert_eq!(
            theme_overrides(&tui).user,
            Some(ThemeStyleConfig {
                fg: Some("cyan".to_string()),
                bg: None,
                modifiers: vec!["bold".to_string()],
            })
        );
        assert_eq!(
            theme_overrides(&tui).error,
            Some(ThemeStyleConfig {
                fg: Some("red".to_string()),
                bg: Some("black".to_string()),
                modifiers: vec![],
            })
        );
        assert_eq!(
            tui.status_line.fields,
            vec!["session".to_string(), "hint".to_string()]
        );
        assert_eq!(tui.tool_preview_lines, Some(7));
        assert_eq!(tui.history_size, Some(42));
    }

    /// **Security review finding, closed (board item
    /// `01M3TJQGJHFFPWE2YYN60WN1XB`)**: an untrusted, WALK-DISCOVERED
    /// project `settings.json`'s `[tui]` section contributes NOTHING --
    /// not a theme override that could otherwise hide the very notice/
    /// status-line marker naming the file as ignored
    /// (`{"error":{"modifiers":["hidden"]}}`), and not anything else in
    /// `[tui]` either. Deliberately does NOT use `explicit_path` (every
    /// other test in this module does, and `explicit_path` bypasses the
    /// trust gate entirely, by design -- the same `--config <path>`
    /// carve-out `conway::config::trust`'s own doc describes): this test's
    /// fixture is reached by the ancestor walk from `cwd`, the only shape
    /// the gate ever fires for. **Fails against a version that reads
    /// `[tui]` via plain `conway::config::merged_document`.**
    #[test]
    fn an_untrusted_project_tui_section_is_ignored_until_trusted() {
        let project_root = tempfile::tempdir().expect("tempdir");
        let user_config_dir = tempfile::tempdir().expect("tempdir");
        let conf_dir = project_root.path().join(".conway");
        std::fs::create_dir_all(&conf_dir).expect("mkdir .conway");
        let settings_path = conf_dir.join("settings.json");
        std::fs::write(
            &settings_path,
            r#"{
                "tui": {
                    "theme": {"error": {"fg": "red", "modifiers": ["hidden"]}},
                    "tool_preview_lines": 999
                }
            }"#,
        )
        .expect("write settings.json");

        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            user_config_dir.path().to_string_lossy().to_string(),
        );
        let options = conway::config::LoadOptions {
            cwd: project_root.path().to_path_buf(),
            explicit_path: None,
            env: env.clone(),
            cli_overrides: conway::config::CliOverrides::default(),
            model_metadata_refresh: false,
        };

        // Before consent: the project's own [tui] section is invisible.
        let tui = load_from_options(options.clone()).expect("load must still succeed");
        assert_eq!(
            tui,
            TuiSection::default(),
            "an untrusted project's [tui] section must contribute nothing, \
             including the theme override that could hide its own notice"
        );

        // After consent: the SAME walk now applies it.
        conway::config::trust::TrustStore::trust_settings(&env, &settings_path)
            .expect("trust_settings succeeds");
        let tui = load_from_options(options).expect("load must succeed");
        assert_eq!(tui.tool_preview_lines, Some(999));
        assert_eq!(
            theme_overrides(&tui).error,
            Some(ThemeStyleConfig {
                fg: Some("red".to_string()),
                bg: None,
                modifiers: vec!["hidden".to_string()],
            }),
            "once trusted, the project's own [tui] section applies exactly \
             as any other trusted project config would"
        );
    }

    /// A typo'd key inside `[tui.theme]` still fails loudly for this
    /// crate's own schema -- `conway::config::load` accepting the rest of
    /// the document (Stage 2a's accepted-and-ignored-with-a-warning
    /// choice) does not mean the CLI stops catching its own typos.
    #[test]
    fn a_typo_inside_tui_theme_is_a_surfaced_parse_error() {
        let cwd_dir = tempfile::tempdir().expect("tempdir");
        let user_config_dir = tempfile::tempdir().expect("tempdir");
        let path = cwd_dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
                "default_role": "coder",
                "roles": {"coder": {"chain": []}},
                "tui": {"theme": {"usre": {"fg": "cyan"}}}
            }"#,
        )
        .expect("write settings.json");

        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            user_config_dir.path().to_string_lossy().to_string(),
        );
        let options = conway::config::LoadOptions {
            cwd: cwd_dir.path().to_path_buf(),
            explicit_path: Some(path),
            env,
            cli_overrides: conway::config::CliOverrides::default(),
            model_metadata_refresh: false,
        };
        let err = load_from_options(options).expect_err("a typo'd key must be rejected");
        assert!(
            err.to_string().contains("usre"),
            "error must name the unrecognized field: {err}"
        );
    }

    /// Board item `01M3XGPGT5W7GABVTC7F2NA0C9`: `tui.busy_input =
    /// "interrupt"` is a real, supported value again (see
    /// [`BusyInputMode`]'s own doc for the history) -- it must load exactly
    /// like any other valid value, not fail the whole `[tui]` parse the way
    /// `a_typo_inside_tui_theme_is_a_surfaced_parse_error` (just above)
    /// proves an ordinary malformed value does, and not fall back to
    /// `queue` the way it briefly did while this primitive did not exist
    /// yet.
    #[test]
    fn busy_input_interrupt_loads_as_a_real_value_not_a_fallback() {
        let cwd_dir = tempfile::tempdir().expect("tempdir");
        let user_config_dir = tempfile::tempdir().expect("tempdir");
        let path = cwd_dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
                "default_role": "coder",
                "roles": {"coder": {"chain": []}},
                "tui": {"busy_input": "interrupt"}
            }"#,
        )
        .expect("write settings.json");

        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            user_config_dir.path().to_string_lossy().to_string(),
        );
        let options = conway::config::LoadOptions {
            cwd: cwd_dir.path().to_path_buf(),
            explicit_path: Some(path),
            env,
            cli_overrides: conway::config::CliOverrides::default(),
            model_metadata_refresh: false,
        };
        let tui = load_from_options(options).expect("a supported value must load");

        assert_eq!(tui.busy_input, BusyInputMode::Interrupt);
    }

    /// Another ordinary, still-supported `busy_input` value, for contrast
    /// with `Interrupt` immediately above.
    #[test]
    fn an_ordinary_busy_input_value_loads() {
        let cwd_dir = tempfile::tempdir().expect("tempdir");
        let user_config_dir = tempfile::tempdir().expect("tempdir");
        let path = cwd_dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
                "default_role": "coder",
                "roles": {"coder": {"chain": []}},
                "tui": {"busy_input": "steer"}
            }"#,
        )
        .expect("write settings.json");

        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            user_config_dir.path().to_string_lossy().to_string(),
        );
        let options = conway::config::LoadOptions {
            cwd: cwd_dir.path().to_path_buf(),
            explicit_path: Some(path),
            env,
            cli_overrides: conway::config::CliOverrides::default(),
            model_metadata_refresh: false,
        };
        let tui = load_from_options(options).expect("load must succeed");

        assert_eq!(tui.busy_input, BusyInputMode::Steer);
    }

    /// Board item `01M44PK089DF2M9TM3C4P5CKMZ` (operator ruling): a
    /// `settings.json` that never names `[tui.busy_input]` at all now starts
    /// a session in `steer`, not `queue` -- `#[serde(default)]` on
    /// [`TuiSection::busy_input`] falls through to [`BusyInputMode::
    /// default()`], and this proves it is wired to the NEW default, not the
    /// pre-ruling one (a stale `#[default] Queue` left on the enum would
    /// pass `an_ordinary_busy_input_value_loads`/`busy_input_interrupt_
    /// loads_as_a_real_value_not_a_fallback` above -- neither names a
    /// missing key -- without ever catching this regression).
    #[test]
    fn busy_input_defaults_to_steer_when_the_key_is_absent() {
        let cwd_dir = tempfile::tempdir().expect("tempdir");
        let user_config_dir = tempfile::tempdir().expect("tempdir");
        let path = cwd_dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
                "default_role": "coder",
                "roles": {"coder": {"chain": []}}
            }"#,
        )
        .expect("write settings.json");

        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            user_config_dir.path().to_string_lossy().to_string(),
        );
        let options = conway::config::LoadOptions {
            cwd: cwd_dir.path().to_path_buf(),
            explicit_path: Some(path),
            env,
            cli_overrides: conway::config::CliOverrides::default(),
            model_metadata_refresh: false,
        };
        let tui = load_from_options(options).expect("load must succeed with no `[tui]` at all");

        assert_eq!(tui.busy_input, BusyInputMode::Steer);
    }

    /// Board item `01M1YVJNS575YN5DCQG9BKZR4E`: `tui.editor_mode = "vim"`
    /// loads as a real, supported value -- mirrors `an_ordinary_busy_input_
    /// value_loads` immediately above. A missing key defaults to `emacs`
    /// (`EditorMode::default()`), covered by `TuiSection::default()` having
    /// no dedicated test of its own, the same way every other `#[serde(default)]`
    /// slot in this struct is only exercised indirectly.
    #[test]
    fn editor_mode_vim_loads() {
        let cwd_dir = tempfile::tempdir().expect("tempdir");
        let user_config_dir = tempfile::tempdir().expect("tempdir");
        let path = cwd_dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
                "default_role": "coder",
                "roles": {"coder": {"chain": []}},
                "tui": {"editor_mode": "vim"}
            }"#,
        )
        .expect("write settings.json");

        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            user_config_dir.path().to_string_lossy().to_string(),
        );
        let options = conway::config::LoadOptions {
            cwd: cwd_dir.path().to_path_buf(),
            explicit_path: Some(path),
            env,
            cli_overrides: conway::config::CliOverrides::default(),
            model_metadata_refresh: false,
        };
        let tui = load_from_options(options).expect("a supported value must load");

        assert_eq!(tui.editor_mode, EditorMode::Vim);
    }

    /// Board item `01M1YVX43MABAVX491HQ5ZCC2M`: `tui.theme = "dark"` (the
    /// NEW string shape) loads as [`ThemeSetting::Preset`], not
    /// [`ThemeSetting::Overrides`] -- the `#[serde(untagged)]` enum's first
    /// variant wins for a bare JSON string.
    #[test]
    fn theme_preset_string_loads_as_a_preset_name() {
        let cwd_dir = tempfile::tempdir().expect("tempdir");
        let user_config_dir = tempfile::tempdir().expect("tempdir");
        let path = cwd_dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
                "default_role": "coder",
                "roles": {"coder": {"chain": []}},
                "tui": {"theme": "dark"}
            }"#,
        )
        .expect("write settings.json");

        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            user_config_dir.path().to_string_lossy().to_string(),
        );
        let options = conway::config::LoadOptions {
            cwd: cwd_dir.path().to_path_buf(),
            explicit_path: Some(path),
            env,
            cli_overrides: conway::config::CliOverrides::default(),
            model_metadata_refresh: false,
        };
        let tui = load_from_options(options).expect("a preset-name string must load");

        assert_eq!(tui.theme, ThemeSetting::Preset("dark".to_string()));
    }

    /// Board item `01M1YVX43MABAVX491HQ5ZCC2M`: the spec's own worked
    /// example -- `tui.theme = "light"` plus `tui.theme_overrides.<slot>` --
    /// loads both halves, independently of each other.
    #[test]
    fn theme_preset_plus_theme_overrides_both_load() {
        let cwd_dir = tempfile::tempdir().expect("tempdir");
        let user_config_dir = tempfile::tempdir().expect("tempdir");
        let path = cwd_dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
                "default_role": "coder",
                "roles": {"coder": {"chain": []}},
                "tui": {
                    "theme": "light",
                    "theme_overrides": {"notice": {"fg": "magenta"}}
                }
            }"#,
        )
        .expect("write settings.json");

        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            user_config_dir.path().to_string_lossy().to_string(),
        );
        let options = conway::config::LoadOptions {
            cwd: cwd_dir.path().to_path_buf(),
            explicit_path: Some(path),
            env,
            cli_overrides: conway::config::CliOverrides::default(),
            model_metadata_refresh: false,
        };
        let tui = load_from_options(options).expect("preset + overrides must load");

        assert_eq!(tui.theme, ThemeSetting::Preset("light".to_string()));
        assert_eq!(
            tui.theme_overrides.notice,
            Some(ThemeStyleConfig {
                fg: Some("magenta".to_string()),
                bg: None,
                modifiers: vec![],
            })
        );
    }

    /// Board item `01M1YVX43MABAVX491HQ5ZCC2M`: `tui.color = false` loads
    /// as `Some(false)`; the key's absence loads as `None`, the "on unless
    /// told otherwise" default -- see [`TuiSection::color`]'s own doc for
    /// why this is `Option<bool>`, not plain `bool`.
    #[test]
    fn tui_color_false_loads_and_absence_defaults_to_none() {
        let cwd_dir = tempfile::tempdir().expect("tempdir");
        let user_config_dir = tempfile::tempdir().expect("tempdir");
        let path = cwd_dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
                "default_role": "coder",
                "roles": {"coder": {"chain": []}},
                "tui": {"color": false}
            }"#,
        )
        .expect("write settings.json");

        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            user_config_dir.path().to_string_lossy().to_string(),
        );
        let options = conway::config::LoadOptions {
            cwd: cwd_dir.path().to_path_buf(),
            explicit_path: Some(path),
            env,
            cli_overrides: conway::config::CliOverrides::default(),
            model_metadata_refresh: false,
        };
        let tui = load_from_options(options).expect("tui.color = false must load");
        assert_eq!(tui.color, Some(false));
        assert_eq!(TuiSection::default().color, None);
    }

    /// Board item `01M1YVX43MABAVX491HQ5ZCC2M`: `CONWAY_TUI__THEME`
    /// reaches `tui.theme` as a [`ThemeSetting::Preset`] through the SAME
    /// env layer `CONWAY_TUI__STATUS_LINE__FIELDS` already proves (test
    /// immediately below) -- no special-cased env handling needed, see
    /// [`ThemeSetting`]'s own doc.
    #[test]
    fn conway_tui_theme_env_var_sets_a_preset_name() {
        let cwd_dir = tempfile::tempdir().expect("tempdir");
        let user_config_dir = tempfile::tempdir().expect("tempdir");
        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            user_config_dir.path().to_string_lossy().to_string(),
        );
        env.insert("CONWAY_TUI__THEME".to_string(), "nord".to_string());
        let options = conway::config::LoadOptions {
            cwd: cwd_dir.path().to_path_buf(),
            explicit_path: None,
            env,
            cli_overrides: conway::config::CliOverrides::default(),
            model_metadata_refresh: false,
        };
        let tui = load_from_options(options).expect("load must succeed");
        assert_eq!(tui.theme, ThemeSetting::Preset("nord".to_string()));
    }

    /// The `CONWAY_TUI__STATUS_LINE__FIELDS` env override reaches this
    /// crate's own `TuiSection` through the SAME layered merge the facade
    /// uses for every other section (`conway::config::merge`'s
    /// `ARRAY_LEAF_KEYS` comma-split path) -- proving the env-var reach
    /// survived the move, not just the file-based path exercised above.
    #[test]
    fn env_var_override_reaches_tui_section_through_the_same_layered_merge() {
        let cwd_dir = tempfile::tempdir().expect("tempdir");
        let user_config_dir = tempfile::tempdir().expect("tempdir");
        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            user_config_dir.path().to_string_lossy().to_string(),
        );
        env.insert(
            "CONWAY_TUI__STATUS_LINE__FIELDS".to_string(),
            "session,hint".to_string(),
        );
        let options = conway::config::LoadOptions {
            cwd: cwd_dir.path().to_path_buf(),
            explicit_path: None,
            env,
            cli_overrides: conway::config::CliOverrides::default(),
            model_metadata_refresh: false,
        };
        let tui = load_from_options(options).expect("load must succeed");
        assert_eq!(
            tui.status_line.fields,
            vec!["session".to_string(), "hint".to_string()],
            "CONWAY_TUI__STATUS_LINE__FIELDS must still reach conway-cli's own TuiSection \
             after Stage 2a moved the type here"
        );
    }

    /// **Mechanical guard, board item A5.4.** `docs/plugins/statusline.md`'s
    /// own worked example shipped with a key this schema had already moved
    /// out from under it (`[plugins].statusline`, when the real section had
    /// been `[tui.status_line_command]` since Stage 2a/T7) -- a reader who
    /// copy-pasted it got a config that refused to load, and nothing caught
    /// it because the example was prose, checked by no one but a reader who
    /// happened to try it. This test is what replaces "checked by no one":
    /// it extracts the doc's OWN fenced JSON block, byte for byte, at test
    /// time (`include_str!`, not a second copy retyped here that could
    /// drift from the page independently) and deserializes its `tui` key
    /// through THIS crate's real [`TuiSection`] -- the identical
    /// `#[serde(deny_unknown_fields)]` schema [`load`] parses `[tui]`
    /// through at startup. A future edit that reintroduces the wrong key,
    /// or any other field this schema does not recognize, fails this test
    /// the moment it lands, not merely a reader's own copy-paste attempt
    /// weeks later.
    ///
    /// Locates the block by its fence markers rather than assuming a line
    /// number, so a doc edit that moves the example (without changing its
    /// content) does not spuriously break this guard -- and asserts there is
    /// EXACTLY one ```json fence in the page, so a second example added
    /// later is caught here too rather than silently extracting the wrong
    /// one.
    #[test]
    fn the_statusline_doc_example_loads_through_the_real_tui_schema() {
        let doc = include_str!("../../../../docs/plugins/statusline.md");
        let fences: Vec<&str> =
            doc.match_indices("```json")
                .map(|(i, _)| i)
                .fold(Vec::new(), |mut acc, start| {
                    let after_open = start + "```json".len();
                    let end = doc[after_open..]
                        .find("```")
                        .map(|rel| after_open + rel)
                        .unwrap_or_else(|| {
                            panic!(
                                "unterminated ```json fence at byte offset {start} in \
                                 docs/plugins/statusline.md"
                            )
                        });
                    acc.push(doc[after_open..end].trim());
                    acc
                });
        assert_eq!(
            fences.len(),
            1,
            "expected exactly one ```json fence in docs/plugins/statusline.md (this test \
             extracts the first as THE worked example) -- found {}: {fences:?}",
            fences.len()
        );

        let example: serde_json::Value = serde_json::from_str(fences[0]).unwrap_or_else(|e| {
            panic!(
                "docs/plugins/statusline.md's example is not even valid JSON: {e}\n{}",
                fences[0]
            )
        });
        let tui_value = example.get("tui").cloned().expect(
            "the doc's example must be wrapped in a top-level \"tui\" key -- a bare \
                     [plugins] key (this page's own past mistake) would silently miss this \
                     assertion entirely rather than fail it, so this checks the WRAPPER too",
        );
        let tui: TuiSection = serde_json::from_value(tui_value).unwrap_or_else(|e| {
            panic!(
                "docs/plugins/statusline.md's example must deserialize through the real \
                 TuiSection schema (the same one `crate::tui::config::load` parses `[tui]` \
                 through) -- it does not: {e}"
            )
        });
        assert_eq!(
            tui.status_line_command.command,
            vec![
                "git".to_string(),
                "branch".to_string(),
                "--show-current".to_string()
            ],
            "the doc's example must round-trip its own command argv exactly"
        );
        assert_eq!(tui.status_line_command.key, "branch");
        assert_eq!(tui.status_line_command.refresh_interval_ms, 5000);
        assert_eq!(tui.status_line_command.timeout_ms, 2000);
    }
}

/// `[tui.status_line_command]`: the single operator-configured
/// status-line command -- see [`TuiSection::status_line_command`]'s own
/// doc for the reachability and trust disclosure this field carries, and
/// why it is a singular struct rather than a `Vec`.
///
/// **Deliberately mirrors `conway::config::schema::HookEntry`/`SubprocessPluginEntry`'s own
/// shape** (`command` as an argv vector, a `timeout_ms` sharing the same
/// default) rather than inventing a fourth "run this command" config
/// vocabulary -- an operator who has already configured a `[hooks].rules[]`
/// or `[plugins].subprocess[]` entry recognizes this shape on sight.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StatusLineCommandConfig {
    /// The command to run, argv-shaped (program, then its arguments) --
    /// never a single shell string, the same shape and reasoning
    /// `conway::config::schema::HookEntry::command`'s own doc gives. An operator migrating a
    /// Claude Code `statusLine.command` shell one-liner wraps it
    /// explicitly, e.g. `["sh", "-c", "git branch --show-current"]` --
    /// `docs/plugins/statusline.md` states this conversion. **Empty (the
    /// default) means "off"** -- see [`TuiSection::status_line_command`]'s own
    /// doc for what that turns off.
    pub command: Vec<String>,
    /// The `PluginStatusContribution::key` this command's result is filed
    /// under -- distinguishes this plugin's row from another
    /// status-contributing plugin's on the rendered status line. Defaults
    /// to `"statusline"` (`conway_plugin_statusline::DEFAULT_KEY`).
    #[serde(default = "default_statusline_key")]
    pub key: String,
    /// Milliseconds between the end of one run and the start of the next.
    /// Floored at `conway_plugin_statusline::MIN_REFRESH_INTERVAL_MS`
    /// (1000ms) by the plugin itself regardless of what is written here --
    /// this crate carries the wire value verbatim and does not enforce
    /// the floor (the same "this crate carries the wire shape and does
    /// nothing else with it" division of labor `[plugins].subprocess`'s
    /// own doc states), so a value below the floor round-trips through
    /// config faithfully but never actually produces a faster cadence.
    /// Defaults to `conway_plugin_statusline::DEFAULT_REFRESH_INTERVAL_MS`
    /// (5000ms).
    #[serde(default = "default_statusline_refresh_interval_ms")]
    pub refresh_interval_ms: u64,
    /// Milliseconds a single run is allowed before it is treated as failed.
    /// Defaults to `conway_plugin_statusline::DEFAULT_TIMEOUT_MS` (2000ms)
    /// -- deliberately shorter than `default_hook_timeout_ms`'s 5000ms;
    /// see that constant's own doc and `conway_plugin_statusline::
    /// DEFAULT_TIMEOUT_MS`'s own doc for why a background probe with no
    /// caller waiting on it wants a tighter ceiling than a one-shot,
    /// operator-initiated callout does.
    #[serde(default = "default_statusline_timeout_ms")]
    pub timeout_ms: u64,
}

impl Default for StatusLineCommandConfig {
    fn default() -> Self {
        Self {
            command: Vec::new(),
            key: default_statusline_key(),
            refresh_interval_ms: default_statusline_refresh_interval_ms(),
            timeout_ms: default_statusline_timeout_ms(),
        }
    }
}

/// This module does not depend on `conway_plugin_statusline` (it carries
/// the wire shape only, the same division of labor every other section in
/// this file already has), so the three defaults below are plain literals
/// kept IN SYNC BY VALUE, not by a shared constant, with that crate's own
/// `DEFAULT_KEY`/
/// `DEFAULT_REFRESH_INTERVAL_MS`/`DEFAULT_TIMEOUT_MS`. `crates/conway-cli/
/// src/statusline_plugin.rs::install` is the one place both sides meet (it
/// depends on both crates), so a future drift between these literals and
/// the plugin crate's own constants would still resolve to
/// WHATEVER-THIS-CONFIG-SAYS at that one conversion site -- never silently
/// disagree at runtime -- but is worth re-checking if either default ever
/// changes.
fn default_statusline_key() -> String {
    "statusline".to_string()
}

fn default_statusline_refresh_interval_ms() -> u64 {
    5000
}

fn default_statusline_timeout_ms() -> u64 {
    2000
}
