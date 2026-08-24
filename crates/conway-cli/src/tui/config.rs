//! The TUI's own presentation config: `[tui]` in `settings.json`
//! (`TuiSection`, `ThemeConfig`, `ThemeStyleConfig`, `StatusLineConfig`).
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

use conway::ConwayError;

use crate::cli::Cli;

/// `[tui]` (TUI-only options). Read at `App::new` via [`load`]; the
/// `conway-cli` TUI reads `.theme`/`.status_line`/`.tool_preview_lines`/
/// `.history_size` at startup. `conway::config::ConwayConfig` never names
/// this type -- see this module's own doc.
#[derive(Clone, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TuiSection {
    #[serde(default)]
    pub theme: ThemeConfig,
    /// `[tui.status_line]` (T3): declarative status-line field order +
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
    /// `[tui.tool_preview_lines]` (T5): the cap on collapsed tool-preview
    /// lines in the TUI transcript. A tool entry whose stored `preview` has
    /// more physical lines than this renders the first N lines + a dim
    /// `… (+M lines, Ctrl-E to expand)` affordance while the entry's
    /// `expanded` flag is `false`; the full preview renders while `true`.
    /// The stored preview is never truncated -- the cap is render-time only.
    /// `None` (the default) means the TUI's built-in default of 3. The TUI
    /// clamps a loaded value to `1..=200` with a fallback to 3 on a
    /// missing/out-of-range/bad value (config is untrusted input,
    /// never a panic). `CONWAY_TUI__TOOL_PREVIEW_LINES=10` overrides via
    /// env.
    #[serde(default)]
    pub tool_preview_lines: Option<u32>,
    /// `[tui.history_size]` (T8): the cap on the persisted input-history
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
}

/// `[tui.status_line]`: declarative status-line field order + visibility
/// (T3). The `fields` list is the ordered set of field names the TUI
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
/// `crates/conway-cli/src/tui/view/theme.rs` one-for-one.
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
    /// (e.g. `Ctrl-E`, `PageUp/PageDown`).
    pub help_key: Option<ThemeStyleConfig>,
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
pub fn load(cli: &Cli) -> conway::Result<TuiSection> {
    let options = conway::config::LoadOptions {
        explicit_path: cli.config.clone(),
        ..conway::config::LoadOptions::default()
    };
    load_from_options(options)
}

fn load_from_options(options: conway::config::LoadOptions) -> conway::Result<TuiSection> {
    let mut merged = conway::config::merged_document(&options)?;
    let tui_value = merged.as_object_mut().and_then(|obj| obj.remove("tui"));
    match tui_value {
        Some(value) => serde_json::from_value(value).map_err(|e| ConwayError::Config {
            path: None,
            message: format!("failed to parse [tui]: {e}"),
        }),
        None => Ok(TuiSection::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
            tui.theme.user,
            Some(ThemeStyleConfig {
                fg: Some("cyan".to_string()),
                bg: None,
                modifiers: vec!["bold".to_string()],
            })
        );
        assert_eq!(
            tui.theme.error,
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
}
