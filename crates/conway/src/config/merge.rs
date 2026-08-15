//! Five-source precedence merge (default < XDG < project < env < CLI),
//! `CONWAY_*` environment mapping, and `ConwayConfig` semantic validation
//! (the headroom checks and structural consistency checks).
//!
//! [`load_ignoring_xdg`] is the one seam that opts out of a source entirely
//! — see its own doc for why XDG
//! alone, not `env` too, and why it is a sibling function rather than a new
//! `LoadOptions` field.
//!
//! Merge happens on `serde_json::Value` (tables union by key, arrays and
//! scalars replace wholesale), and only the final merged document is
//! deserialized into [`ConwayConfig`] — this is what makes
//! `#[serde(deny_unknown_fields)]` a meaningful fail-loud check on the
//! *result* of layering five sources, rather than on each source
//! individually (a source may legitimately omit almost everything).
//!
//! **One named exception (Stage 2a):** a top-level `tui` key is stripped
//! out of the merged document before that deserialize, rather than tripping
//! `deny_unknown_fields`, because `[tui]` is `conway-cli`'s presentation
//! config (`TuiSection` and its siblings no longer live in this schema at
//! all) and an existing `settings.json` naming it must still load
//! successfully. [`load`]/[`load_ignoring_xdg`] record the strip as a
//! [`crate::config::ConfigWarning`] rather than dropping it with no trace;
//! [`merged_document`] is the escape hatch a caller that DOES understand
//! `[tui]` (`conway-cli`) uses to read it back out of the same layered
//! document.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use conway_core::event_name::validate_event_name;
use conway_core::ids::{ModelRef, RoleAlias};
use conway_runtime::hook_dispatch::EVENTS_WITHOUT_TOOL_NAME;
use serde_json::{Map, Value};

use crate::config::model_metadata::ModelMetadata;
use crate::config::schema::ConwayConfig;
use crate::config::{discovery, model_metadata, ConfigWarning, LoadOutcome, WarningCode};
use crate::error::{ConwayError, Result};

/// The five-source `load` input. `env` stands in for the process
/// environment: `load` with a default `LoadOptions` reads `std::env::vars()`
/// into it, but tests construct their own map so precedence tests never
/// mutate real process env (and stay parallel-safe).
#[derive(Debug, Clone)]
pub struct LoadOptions {
    pub cwd: PathBuf,
    pub explicit_path: Option<PathBuf>,
    pub env: HashMap<String, String>,
    pub cli_overrides: CliOverrides,
    /// The only supported default is `false`: `load` performs no network
    /// I/O regardless of this flag's value (there is currently no code path
    /// that reads it — it exists so a future opt-in refresh call site has
    /// somewhere to receive caller intent without changing this struct's
    /// shape).
    pub model_metadata_refresh: bool,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            explicit_path: None,
            env: std::env::vars().collect(),
            cli_overrides: CliOverrides::default(),
            model_metadata_refresh: false,
        }
    }
}

/// An embedder-facing override struct: [`load`]'s fifth (highest-precedence)
/// layer, and [`apply_cli`]'s re-application point. Fully wired and tested
/// *as a library API* — an embedder that constructs one and passes it to
/// `LoadOptions::cli_overrides` or `ConwayBuilder::with_cli_overrides` gets
/// exactly the precedence and validation this module promises.
///
/// **What it is not (corrected):** despite its name and this
/// struct's original doc comment, `conway-cli` does not construct or pass
/// one of these. `grep -rn "with_cli_overrides" crates/` finds exactly three
/// hits: this struct's definition, and two test files
/// (`crates/conway-cli/tests/oneshot_ask.rs`,
/// `crates/conway-cli/tests/continuity.rs`) — zero production call sites.
/// `conway-cli`'s actual flag-to-config wiring is separate, bespoke code in
/// that crate, not this struct. The previous wording ("mirrored here (not
/// in `conway-cli`) so the library is the source of truth") read as
/// a claim that CLI flag values flow through this exact struct in
/// production; they do not, and the next person to add a field here on the
/// strength of that claim would reasonably expect it to reach a real `conway`
/// invocation when it would not. Whether/how to reconcile the bespoke
/// `conway-cli` wiring with this struct is the open architectural question
/// filed as — not decided here.
///
/// Reconciliation disclosed here: this field list is
/// exactly the amendment's enumerated set. It has no per-backend override
/// (e.g. no `backends.<id>.base_url` field) — the precedence-test criterion
/// names `backends.<id>.base_url` as one of the keys to prove all five
/// sources against, but no CLI path exists for that leaf under this
/// documented shape. `tests/config_precedence.rs` covers that key across
/// default/XDG/project/env (four sources) and notes the gap rather than
/// inventing an undocumented field here.
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub default_role: Option<RoleAlias>,
    pub permission_mode: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    pub denied_tools: Option<Vec<String>>,
    pub max_steps: Option<u32>,
    pub session_root: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    /// Amendment addition: sets `routing.default_headroom_tokens`. There is
    /// no CLI form for a per-role override — a CLI-supplied value is a
    /// session-wide floor.
    ///
    /// Not a mode-reachability violation despite no `conway-cli` flag
    /// setting it through this struct (see the struct doc comment):
    /// headroom is independently reachable today via `settings.json`
    /// (`[routing].default_headroom_tokens`, `[roles.<alias>].headroom_tokens`)
    /// and the `CONWAY_ROUTING__DEFAULT_HEADROOM_TOKENS` /
    /// `CONWAY_ROLES__<ALIAS>__HEADROOM_TOKENS` env vars. Left in place
    /// rather than removed (
    ///): unlike the deleted `model` field —
    /// which had no `ConwayConfig` key to land on and was skipped by this
    /// struct's own `cli_overrides_to_value` — this field IS translated
    /// into the merge document (`routing.default_headroom_tokens`) and is
    /// exercised end to end by `tests/config_headroom.rs`. Its only gap is
    /// that no `conway-cli` flag ever constructs a `CliOverrides` with it
    /// set, which is now true of every field here: `conway-cli` wires its
    /// own flags directly rather than through this struct (see the struct
    /// doc comment), so this field's reachability from a real CLI
    /// invocation is exactly as good, and exactly as absent, as its seven
    /// remaining siblings'.
    pub headroom_tokens: Option<u32>,
}

/// The full five-source load: default < XDG < project < env < CLI.
pub fn load(options: LoadOptions) -> Result<LoadOutcome> {
    load_impl(options, IncludeXdgLayer::Yes)
}

/// Identical to [`load`], except the XDG/user layer
/// (`$XDG_CONFIG_HOME/conway/settings.json`, or `~/.conway/settings.json`)
/// is never read — the merge becomes `default < project < env < CLI`, four
/// sources instead of five.
///
/// [`load`] reads the XDG layer unconditionally, *before* the
/// `explicit_path`/discovered project layer, regardless of whether
/// `options.explicit_path` is set — so a caller who wants isolation (a test
/// fixture, or an embedder that wants to use its own configuration rather
/// than whatever is in the invoking user's home directory) has had no way
/// to get it. This function is that seam. See
/// [`crate::builder::ConwayBuilder::from_config_only`] for the builder-level
/// entry point most callers want instead of calling this directly.
///
/// **A sibling function, not a new `LoadOptions` field, deliberately:**
/// `LoadOptions` is constructed via full struct-literal syntax (naming
/// every field, no `..LoadOptions::default()` base) at call sites in other
/// crates across this workspace (`crates/conway-cli/src/tui/view/status.rs`,
/// `crates/conway-thirdparty-backend/src/lib.rs`) — outside this seam's own
/// file lane. Growing `LoadOptions`'s field set would silently break every
/// one of those at compile time for a capability they have no reason to
/// opt into; a same-shaped sibling function costs them nothing.
///
/// **XDG only, not `env` too — decided, not left implicit:** `CONWAY_*`
/// environment variables differ from the XDG layer in the one way that
/// matters here — they are how a *caller* (CI, a container entrypoint, an
/// embedder's own process supervisor) explicitly hands *this* invocation
/// its credentials and overrides, at the moment `options.env` is
/// constructed and passed in. The XDG layer, by contrast, is a *file on
/// disk*, written independently of any particular invocation and
/// discovered by walking the filesystem rather than supplied by the
/// caller — exactly the ambient "invoking user's home directory" state this
/// function exists to bypass. Suppressing `env` here would break the
/// CI/embedder credential-passing use case this seam serves, for no
/// isolation benefit: a caller that also wants an env-free load already has
/// the tool for that — pass a hand-built (possibly empty) `env` map, the
/// same mechanism every hermetic test in this workspace already uses (see
/// `crates/conway/tests/support/mod.rs::isolated_env`).
pub fn load_ignoring_xdg(options: LoadOptions) -> Result<LoadOutcome> {
    load_impl(options, IncludeXdgLayer::No)
}

/// Whether [`load_impl`] reads the XDG/user layer — a private, two-variant
/// enum rather than a bare `bool` so `load`/`load_ignoring_xdg`'s own call
/// sites stay self-documenting at the call site, not `load_impl(options,
/// true)`/`load_impl(options, false)` with no indication of which way
/// `true` goes.
enum IncludeXdgLayer {
    Yes,
    No,
}

/// The fully layered document (the same five-source precedence [`load`]
/// uses -- default < XDG < project < env < CLI), as raw JSON, BEFORE the
/// final `ConwayConfig` deserialize.
///
/// **The one sanctioned escape hatch for a section this facade's schema
/// deliberately does not define.** Stage 2a moved `TuiSection`/
/// `ThemeConfig`/`StatusLineConfig`/`ThemeStyleConfig` out of
/// `ConwayConfig` entirely -- `[tui]` is `conway-cli`'s presentation
/// config, and a headless host linking only this facade has no business
/// parsing or validating a theme it can never render. `load`/
/// `load_ignoring_xdg` strip a top-level `tui` key out of the merged
/// document before deserializing (see their own doc for why: otherwise
/// EVERY existing `settings.json` with a `[tui.theme]`/`[tui.status_line]`
/// block would hard-fail to load through this crate at all), so `[tui]`'s
/// actual value is not reachable through [`LoadOutcome`] at all any more.
/// `conway-cli` calls this function directly instead, to read `[tui]`'s
/// raw value back out of the SAME layered document and deserialize it into
/// its own, locally-owned `TuiSection` (`crates/conway-cli/src/tui/
/// config.rs`) -- the one caller today.
///
/// Every other caller should prefer [`load`]/[`load_ignoring_xdg`]
/// instead: this bypasses `ConwayConfig`'s `#[serde(deny_unknown_fields)]`
/// validation entirely, so a typo anywhere in the document is not caught
/// here.
pub fn merged_document(options: &LoadOptions) -> Result<Value> {
    merged_document_impl(options, IncludeXdgLayer::Yes)
}

fn merged_document_impl(options: &LoadOptions, include_xdg: IncludeXdgLayer) -> Result<Value> {
    let mut merged = default_document();

    if matches!(include_xdg, IncludeXdgLayer::Yes) {
        if let Some(path) = discovery::xdg_config_path(&options.env) {
            if let Some(layer) = read_json_layer(&path)? {
                merge_values(&mut merged, layer);
            }
        }
    }

    let project_path = options
        .explicit_path
        .clone()
        .or_else(|| discovery::discover(&options.cwd));
    if let Some(path) = project_path {
        if let Some(layer) = read_json_layer(&path)? {
            merge_values(&mut merged, layer);
        }
    }

    let env_layer = env_to_value(&options.env, &merged);
    merge_values(&mut merged, env_layer);

    let cli_layer = cli_overrides_to_value(&options.cli_overrides);
    merge_values(&mut merged, cli_layer);

    Ok(merged)
}

fn load_impl(options: LoadOptions, include_xdg: IncludeXdgLayer) -> Result<LoadOutcome> {
    let mut merged = merged_document_impl(&options, include_xdg)?;

    // `[tui]` (or a `CONWAY_TUI__*` env var) is a presentation-only
    // section this facade deliberately does not define a type for any
    // more (Stage 2a; see `merged_document`'s own doc). Extracted and
    // DROPPED here, rather than handed to `ConwayConfig`'s
    // `#[serde(deny_unknown_fields)]` deserialize below, which would
    // otherwise hard-fail loading any EXISTING settings.json that
    // configures a TUI theme or status line -- `conway-cli` genuinely
    // still needs those to load successfully (it re-reads `[tui]` itself
    // via `merged_document`, see that function's own doc). Silently
    // dropping it with no trace at all would be the worst option (board
    // item's own framing): a `ConfigWarning` is pushed onto this outcome
    // instead, so a caller that does NOT separately re-parse `[tui]`
    // itself is told its presence was seen and ignored, not left to
    // wonder why nothing happened.
    let had_tui = merged
        .as_object_mut()
        .is_some_and(|obj| obj.remove("tui").is_some());

    let config: ConwayConfig = serde_json::from_value(merged).map_err(|e| ConwayError::Config {
        path: None,
        message: format!("failed to parse merged configuration: {e}"),
    })?;

    let metadata_path = resolve_metadata_path(&config, &options.cwd);
    let metadata = model_metadata::load(&metadata_path)?;

    let mut warnings = validate(&config, &metadata, &options.env)?;
    if had_tui {
        warnings.push(ConfigWarning {
            code: WarningCode::PresentationConfigIgnored,
            message: "a [tui] section (or a CONWAY_TUI__* environment variable) is present, \
                      but conway's own config schema no longer defines [tui] -- it is \
                      conway-cli's presentation config (theme/status-line/tool-preview-lines/\
                      history-size), not the facade's, as of Stage 2a. This load accepted the \
                      rest of the document and discarded [tui] entirely; a caller that is not \
                      conway-cli (which re-reads [tui] itself through a separate, un-stripped \
                      merge) will not find its value anywhere."
                .to_string(),
        });
    }

    Ok(LoadOutcome { config, warnings })
}

/// Re-applies `cli_overrides` to an already-loaded config (used by
/// `ConwayBuilder::build`, so an
/// invalid override is caught even when `from_parts`/`from_config` bypassed
/// `load`'s own CLI layer). Re-runs every validation check EXCEPT check 3
/// (`permissions.mode = "allowlist"` requiring non-empty `allowed_tools`) --
/// see that check's own comment in `validate_impl` for why this call site
/// deliberately diverges from `validate`'s strict default. Every other check
/// still runs unconditionally: this is a targeted exception for one check,
/// not a general relaxation.
pub fn apply_cli(config: &ConwayConfig, cli: &CliOverrides) -> Result<ConwayConfig> {
    let mut value = serde_json::to_value(config).map_err(|e| ConwayError::Config {
        path: None,
        message: format!("failed to serialize config for CLI override merge: {e}"),
    })?;
    merge_values(&mut value, cli_overrides_to_value(cli));
    let merged: ConwayConfig = serde_json::from_value(value).map_err(|e| ConwayError::Config {
        path: None,
        message: format!("failed to parse config after CLI override merge: {e}"),
    })?;

    let metadata_path = resolve_metadata_path(&merged, &merged.cwd);
    let metadata = model_metadata::load(&metadata_path).unwrap_or_else(|_| ModelMetadata::empty());
    validate_impl(
        &merged,
        &metadata,
        &HashMap::new(),
        AllowlistEmptyCheck::Skip,
    )?;

    Ok(merged)
}

fn resolve_metadata_path(config: &ConwayConfig, cwd: &std::path::Path) -> PathBuf {
    if config.models.metadata_path.is_absolute() {
        config.models.metadata_path.clone()
    } else {
        cwd.join(&config.models.metadata_path)
    }
}

/// The built-in, lowest-precedence layer. Matches the documented config
/// defaults exactly (the implementation notes / the headroom
/// amendment), except `routing.default_headroom_tokens`: see
/// `schema::DEFAULT_HEADROOM_TOKENS`'s doc comment for why `8_192` (not the
/// amendment's literal `16000`) is used.
///
/// `roles.coder` (an empty chain, no headroom override) is baked in so that
/// `default_role = "coder"` — itself a baked-in default — passes the
/// "`default_role` exists in `[roles]`" validation check even when every
/// other source is absent (the bare-defaults / "D wins" precedence-test
/// stage). An empty chain trivially satisfies the chain-format/backend-
/// existence check too.
fn default_document() -> Value {
    serde_json::json!({
        "default_role": "coder",
        "cwd": ".",
        "session": {
            "root": ".conway/sessions",
            "fsync": "interval",
            "fsync_interval_ms": 200,
        },
        "limits": {
            "max_steps": 40,
            "max_tokens": 0,
            "deadline_secs": 0,
            "max_parallel_tools": 4,
        },
        "permissions": {
            "mode": "prompt",
            "allowed_tools": [],
            "denied_tools": [],
        },
        "backends": {},
        "routing": {
            "default_headroom_tokens": crate::config::schema::DEFAULT_HEADROOM_TOKENS,
        },
        "roles": {
            "coder": { "chain": [], "headroom_tokens": null },
        },
        "health": {
            "transport_failures_to_open": 3,
            "open_duration_secs": 30,
        },
        "agents": {
            "dir": ".conway/agents",
        },
        "models": {
            "metadata_path": ".conway/models.json",
            "probe_on_startup": false,
        },
    })
}

/// Deep merge: `Object`+`Object` unions by key (recursing); anything else
/// (including array-vs-array) replaces `base` wholesale with `overlay`.
fn merge_values(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Object(base_map), Value::Object(overlay_map)) => {
            for (key, value) in overlay_map {
                match base_map.get_mut(&key) {
                    Some(existing) => merge_values(existing, value),
                    None => {
                        base_map.insert(key, value);
                    }
                }
            }
        }
        (base_slot, overlay_value) => {
            *base_slot = overlay_value;
        }
    }
}

fn read_json_layer(path: &std::path::Path) -> Result<Option<Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let value: Value = serde_json::from_str(&text).map_err(|e| ConwayError::Config {
                path: Some(path.to_path_buf()),
                message: format!("failed to parse JSON at {}: {e}", path.display()),
            })?;
            Ok(Some(value))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ConwayError::Config {
            path: Some(path.to_path_buf()),
            message: format!("failed to read {}: {e}", path.display()),
        }),
    }
}

/// The top-level `ConwayConfig` field names a `CONWAY_*` env var's first
/// segment must match; anything else is silently ignored ("unknown
/// `CONWAY_*` vars are ignored without error").
const KNOWN_TOP_LEVEL_KEYS: &[&str] = &[
    "default_role",
    "cwd",
    "session",
    "limits",
    "permissions",
    "backends",
    "routing",
    "roles",
    "health",
    "agents",
    "models",
    "tui",
];

/// Array-valued leaf keys, so their env values are comma-split rather than
/// parsed as a single scalar.
const ARRAY_LEAF_KEYS: &[&str] = &["allowed_tools", "denied_tools", "fields"];

/// Builds the env-derived merge layer. `CONWAY_` prefix, `__` as the table
/// separator, uppercase with single `_` preserved within a segment.
/// `known_roles` (the roles table already merged from lower-precedence
/// sources) gates per-role env vars: `CONWAY_ROLES__<ALIAS>__HEADROOM_TOKENS`
/// is applied only when `<ALIAS>` case-insensitively matches an existing
/// role; otherwise it is ignored, per the amendment's "unknown alias is
/// ignored" rule.
fn env_to_value(env: &HashMap<String, String>, merged_so_far: &Value) -> Value {
    let known_roles: BTreeSet<String> = merged_so_far
        .get("roles")
        .and_then(Value::as_object)
        .map(|roles| roles.keys().map(|k| k.to_lowercase()).collect())
        .unwrap_or_default();

    let mut root = Map::new();
    for (key, raw_value) in env {
        let Some(rest) = key.strip_prefix("CONWAY_") else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let segments: Vec<String> = rest.split("__").map(|s| s.to_lowercase()).collect();
        let Some(top) = segments.first() else {
            continue;
        };
        if !KNOWN_TOP_LEVEL_KEYS.contains(&top.as_str()) {
            continue;
        }
        if top == "roles" {
            // Expect exactly `roles.<alias>.<field>`; only `headroom_tokens`
            // is env-settable per role, and only for a known alias.
            if segments.len() != 3 || segments[2] != "headroom_tokens" {
                continue;
            }
            if !known_roles.contains(&segments[1]) {
                continue;
            }
        }

        let leaf_key = segments.last().unwrap().clone();
        let value = if ARRAY_LEAF_KEYS.contains(&leaf_key.as_str()) {
            Value::Array(
                raw_value
                    .split(',')
                    .map(|s| Value::String(s.trim().to_string()))
                    .collect(),
            )
        } else {
            parse_env_scalar(raw_value)
        };

        set_path(&mut root, &segments, value);
    }
    Value::Object(root)
}

fn parse_env_scalar(raw: &str) -> Value {
    if let Ok(i) = raw.parse::<i64>() {
        return Value::from(i);
    }
    if let Ok(f) = raw.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    match raw {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => Value::String(raw.to_string()),
    }
}

/// Inserts `value` into `root` at the nested path named by `segments`,
/// creating intermediate objects as needed.
fn set_path(root: &mut Map<String, Value>, segments: &[String], value: Value) {
    if segments.len() == 1 {
        root.insert(segments[0].clone(), value);
        return;
    }
    let entry = root
        .entry(segments[0].clone())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(map) = entry {
        set_path(map, &segments[1..], value);
    }
}

fn cli_overrides_to_value(cli: &CliOverrides) -> Value {
    let mut root = Map::new();
    if let Some(role) = &cli.default_role {
        root.insert(
            "default_role".to_string(),
            Value::String(role.as_str().to_string()),
        );
    }
    if let Some(cwd) = &cli.cwd {
        root.insert(
            "cwd".to_string(),
            Value::String(cwd.to_string_lossy().to_string()),
        );
    }
    if let Some(mode) = &cli.permission_mode {
        set_path(
            &mut root,
            &["permissions".to_string(), "mode".to_string()],
            Value::String(mode.clone()),
        );
    }
    if let Some(allowed) = &cli.allowed_tools {
        set_path(
            &mut root,
            &["permissions".to_string(), "allowed_tools".to_string()],
            Value::Array(allowed.iter().cloned().map(Value::String).collect()),
        );
    }
    if let Some(denied) = &cli.denied_tools {
        set_path(
            &mut root,
            &["permissions".to_string(), "denied_tools".to_string()],
            Value::Array(denied.iter().cloned().map(Value::String).collect()),
        );
    }
    if let Some(max_steps) = cli.max_steps {
        set_path(
            &mut root,
            &["limits".to_string(), "max_steps".to_string()],
            Value::from(max_steps),
        );
    }
    if let Some(session_root) = &cli.session_root {
        set_path(
            &mut root,
            &["session".to_string(), "root".to_string()],
            Value::String(session_root.to_string_lossy().to_string()),
        );
    }
    if let Some(headroom) = cli.headroom_tokens {
        set_path(
            &mut root,
            &["routing".to_string(), "default_headroom_tokens".to_string()],
            Value::from(headroom),
        );
    }
    Value::Object(root)
}

/// Runs every validation step in the documented order, failing on the
/// first hard error and returning accumulated warnings from the last step.
///
/// This is the STRICT entry point (check 3 included, see that check's own
/// doc for why): the right choice for a config a human might have typed by
/// hand, notably `load_impl`'s own call site (behind [`load`]/
/// [`load_ignoring_xdg`]). [`apply_cli`] deliberately calls
/// `validate_impl` with check 3 skipped instead of this function -- see
/// its own doc.
///
/// Note: conway does not validate the *shape* of an API key. Whether a
/// credential is a metered API key, a coding-plan subscription key, or a
/// token for some Anthropic-compatible third-party endpoint is not
/// conway's business to adjudicate -- the provider answers that, and its
/// answer is more accurate than a prefix match here.
pub fn validate(
    config: &ConwayConfig,
    metadata: &ModelMetadata,
    env: &HashMap<String, String>,
) -> Result<Vec<ConfigWarning>> {
    validate_impl(config, metadata, env, AllowlistEmptyCheck::Enforce)
}

/// Whether [`validate_impl`] treats `permissions.mode = "allowlist"` paired
/// with an empty `allowed_tools` as a hard error (check 3) -- a private,
/// two-variant enum rather than a bare `bool` for the same self-documenting
/// reason [`IncludeXdgLayer`] is one, not `validate_impl(config, metadata,
/// env, true)` with no indication of which way `true` goes.
///
/// See check 3's own comment, at its call site below, for why the two
/// [`validate_impl`] call sites ([`validate`] and [`apply_cli`]) disagree.
enum AllowlistEmptyCheck {
    Enforce,
    Skip,
}

fn validate_impl(
    config: &ConwayConfig,
    metadata: &ModelMetadata,
    // Retained in the signature (this function's public wrapper, `validate`,
    // is `pub`) though no current check consults it: the removed key-shape
    // check was its only reader. A future validation that legitimately needs
    // the injected environment has it available without a breaking signature
    // change.
    _env: &HashMap<String, String>,
    allowlist_empty_check: AllowlistEmptyCheck,
) -> Result<Vec<ConfigWarning>> {
    // 1. default_role exists in [roles].
    if !config.roles.contains_key(config.default_role.as_str()) {
        let mut known: Vec<&str> = config.roles.keys().map(String::as_str).collect();
        known.sort_unstable();
        return Err(ConwayError::Config {
            path: None,
            message: format!(
                "default_role '{}' is not defined in [roles]; defined roles: [{}]",
                config.default_role,
                known.join(", ")
            ),
        });
    }

    // 2. Every ModelRef in every chain has the form <backend_id>/<model>
    //    and <backend_id> exists in [backends].
    for (name, entry) in &config.roles {
        for raw in &entry.chain {
            let model_ref: ModelRef = raw.parse().map_err(|_| ConwayError::Config {
                path: None,
                message: format!(
                    "role '{name}': chain entry '{raw}' is not a valid 'backend/model' reference"
                ),
            })?;
            if !config.backends.contains_key(model_ref.backend.as_str()) {
                return Err(ConwayError::Config {
                    path: None,
                    message: format!(
                        "role '{name}': chain entry '{raw}' names unknown backend '{}'",
                        model_ref.backend
                    ),
                });
            }
        }
    }

    // 3. permissions.mode = "allowlist" requires non-empty allowed_tools --
    //    but only when the config being checked is one a human could have
    //    typed by hand into a settings file, i.e. only in `validate`'s own
    //    STRICT entry point (`AllowlistEmptyCheck::Enforce`, pinned by
    //    `config_validation.rs::allowlist_mode_with_empty_allowed_tools_is_
    //    rejected`, which loads the offending value through `config::load`
    //    -- a JSON FILE -- not through this module's own struct
    //    constructors). There, `allowlist` with nothing on it is
    //    overwhelmingly more likely to be an operator who forgot to list
    //    the tools they meant to allow than a deliberate "deny everything"
    //    -- allow-list mode reads as additive, so an empty list reads as
    //    "I haven't filled this in yet", not "I want zero tools". The typo
    //    is exactly the failure mode this check exists to catch before it
    //    reaches a live session.
    //
    //    `apply_cli` -- `ConwayBuilder::build`'s own re-validation step,
    //    called for EVERY build regardless of how the config was
    //    assembled -- passes `AllowlistEmptyCheck::Skip` instead, because by
    //    the time a config reaches it, the empty-allowlist-as-typo concern
    //    above no longer applies: either the config came from
    //    `config::load`/`load_ignoring_xdg` and already passed THIS check
    //    once (`load_impl`'s own `validate` call), or it was assembled
    //    programmatically via `ConwayBuilder::from_parts`/`CliOverrides` --
    //    an embedder writing Rust, not a human hand-editing a settings file
    //    -- where an explicit empty `allowed_tools` is exactly as legible a
    //    "deny everything" statement as `permissions.mode = "deny"` itself,
    //    and is precisely what `presets::default_permissions_for_one_shot`
    //    ships as its own deliberate, documented value (see that function's
    //    own doc comment). Skipping here does not weaken the file-typo
    //    protection above: nothing reaches `apply_cli` without either having
    //    gone through the strict check already, or never having been a file
    //    at all.
    if matches!(allowlist_empty_check, AllowlistEmptyCheck::Enforce)
        && matches!(
            config.permissions.mode,
            crate::config::schema::PermissionMode::Allowlist
        )
        && config.permissions.allowed_tools.is_empty()
    {
        return Err(ConwayError::Config {
            path: None,
            message: "permissions.mode = \"allowlist\" requires a non-empty allowed_tools list"
                .to_string(),
        });
    }

    // 4. fsync = "interval" requires fsync_interval_ms > 0.
    if matches!(
        config.session.fsync,
        crate::config::schema::FsyncMode::Interval
    ) && config.session.fsync_interval_ms == 0
    {
        return Err(ConwayError::Config {
            path: None,
            message: "session.fsync = \"interval\" requires fsync_interval_ms > 0".to_string(),
        });
    }

    // 5. api_key and api_key_env are not both non-empty for the same
    //    backend.
    for (id, backend) in &config.backends {
        if !backend.api_key.is_empty() && !backend.api_key_env.is_empty() {
            return Err(ConwayError::Config {
                path: None,
                message: format!(
                    "backend '{id}': api_key and api_key_env are mutually exclusive but both are set"
                ),
            });
        }
    }

    // 6. Hard error: headroom values must be > 0 (global and every present
    //    per-role override).
    if config.routing.default_headroom_tokens == 0 {
        return Err(ConwayError::Config {
            path: None,
            message: "routing.default_headroom_tokens must be greater than 0".to_string(),
        });
    }
    let mut role_names: Vec<&String> = config.roles.keys().collect();
    role_names.sort_unstable();
    for name in &role_names {
        if config.roles[*name].headroom_tokens == Some(0) {
            return Err(ConwayError::Config {
                path: None,
                message: format!("roles.{name}.headroom_tokens must be greater than 0"),
            });
        }
    }

    // 7. Warning only: headroom >= smallest reachable model context.
    let mut warnings = Vec::new();
    if !metadata.models.is_empty() {
        let mut seen = BTreeSet::new();
        for name in &role_names {
            let entry = &config.roles[*name];
            let role_alias = RoleAlias::new((*name).clone());
            let headroom = config.headroom_for(&role_alias);

            let mut smallest: Option<(&str, u32)> = None;
            for raw in &entry.chain {
                if let Some(model_meta) = metadata.models.get(raw.as_str()) {
                    if smallest.is_none_or(|(_, m)| model_meta.max_context_tokens < m) {
                        smallest = Some((raw.as_str(), model_meta.max_context_tokens));
                    }
                }
            }

            if let Some((model_ref, max_context)) = smallest {
                if headroom >= max_context {
                    let subject = if entry.headroom_tokens.is_some() {
                        format!("headroom for role '{name}'")
                    } else {
                        "routing.default_headroom_tokens".to_string()
                    };
                    let message = format!(
                        "{subject} is {headroom} tokens, which is not less than the smallest \
                         context window in its chain ({model_ref} = {max_context} tokens); every \
                         request routed to that model will be rejected by the context-window gate"
                    );
                    if seen.insert(message.clone()) {
                        warnings.push(ConfigWarning {
                            code: WarningCode::HeadroomExceedsContext,
                            message,
                        });
                    }
                }
            }
        }
    }

    // 8. Hard error: every id in tools.builtin_plugins names a real built-in.
    //
    // A hard error rather than a warning, for the same reason check 1 hard-
    // errors on an undefined default_role: the candidate set is closed and
    // known at compile time, so an unrecognized id is unambiguously a typo,
    // never a forward reference. And the failure it prevents is the one
    // This ranks WORST -- user-facing configuration that silently does
    // nothing. `builtin_plugins` is how an operator turns `bash` back on;
    // `"conway.shel"` would leave it off with no signal, and the operator
    // would believe they had enabled it. Silence there is indistinguishable
    // from success, which is precisely the shape this project keeps having
    // to walk back.
    //
    // Note this list is NOT the extension point: a third-party plugin is
    // installed with `ConwayBuilder::with_plugin` and is never filtered by
    // this selection, so naming one here is also a mistake worth catching.
    //
    // Gated on `builtin-tools` alongside the candidate source it validates
    // against: without that feature there are no built-ins to name, and a
    // check with nothing to check against would either reject every id or
    // accept every id -- both worse than not running.
    #[cfg(feature = "builtin-tools")]
    {
        let known = crate::presets::builtin_plugin_ids();
        let mut unknown: Vec<&str> = config
            .tools
            .builtin_plugins
            .iter()
            .map(String::as_str)
            .filter(|id| !known.iter().any(|k| k == id))
            .collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            unknown.dedup();
            let mut known_sorted = known.clone();
            known_sorted.sort();
            return Err(ConwayError::Config {
                path: None,
                message: format!(
                    "tools.builtin_plugins names unknown built-in plugin(s): [{}]; known \
                     built-ins: [{}]. A third-party plugin is installed with \
                     ConwayBuilder::with_plugin and is not listed here.",
                    unknown.join(", "),
                    known_sorted.join(", ")
                ),
            });
        }
    }

    // 9. Every [hooks].rules[] entry has a non-empty `id`, and every `id` is
    //    unique across the file. Enforced here (a semantic check on the
    //    parsed value), not by serde, matching how every other "required in
    //    practice" invariant in this function is enforced -- see check 3's
    //    own precedent (`permissions.mode = "allowlist"` requiring
    //    non-empty `allowed_tools`). `id` is load-bearing for the later
    //    operator-visibility item that lists hook rules individually and
    //    revokes one by name (`schema::HookEntry::id`'s own doc comment);
    //    an empty or duplicate id there would make that lookup ambiguous or
    //    silently target the wrong rule.
    //
    // Note: `[hooks]` itself only parses and validates today -- see
    // `schema::HooksConfig`'s own per-event reachability disclosure. This check runs
    // regardless of whether any rule is ever dispatched, exactly like every
    // other structural check in this function runs on config that may
    // never be exercised at runtime.
    {
        let mut seen_ids: BTreeSet<&str> = BTreeSet::new();
        for rule in &config.hooks.rules {
            if rule.id.is_empty() {
                return Err(ConwayError::Config {
                    path: None,
                    message: "hooks.rules[]: every rule must have a non-empty \"id\"".to_string(),
                });
            }
            if !seen_ids.insert(rule.id.as_str()) {
                return Err(ConwayError::Config {
                    path: None,
                    message: format!(
                        "hooks.rules[]: duplicate id '{}' -- every rule's id must be unique",
                        rule.id
                    ),
                });
            }
        }
    }

    // 10. A rule's `match` only
    //     applies to an `event` whose payload actually names a tool --
    //     `"pre_tool_use"`/`"post_tool_use"`. `EVENTS_WITHOUT_TOOL_NAME`
    //     names every OTHER event `conway-runtime` dispatches on this
    //     item's own list (`session_starting`, `child_spawned`,
    //     `request_assembled`, `child_reported`, `prompt_submitted`);
    //     `"pre_tool_use"` itself is added here rather than to that shared
    //     list because `conway-runtime`'s `hook_dispatch` module does not
    //     dispatch it (`crate::permission::PermissionBroker` does) -- see
    //     `EVENTS_WITHOUT_TOOL_NAME`'s own doc.
    //
    //     This is a SURFACED, TYPED error naming the rule's `id` -- per this
    //     item's own ACCEPTANCE, "an error, not silence": a `match` an
    //     operator wrote in good faith that can never fire (because the
    //     event it is paired with never carries a tool name) must not
    //     silently parse into a rule that quietly does nothing extra,
    //     exactly the class of defect check 9's own comment names.
    {
        const PRE_TOOL_USE: &str = "pre_tool_use";
        for rule in &config.hooks.rules {
            if rule.match_tool.is_none() {
                continue;
            }
            if rule.event == PRE_TOOL_USE
                || rule.event == conway_runtime::hook_dispatch::POST_TOOL_USE
            {
                continue;
            }
            if EVENTS_WITHOUT_TOOL_NAME.contains(&rule.event.as_str()) {
                return Err(ConwayError::Config {
                    path: None,
                    message: format!(
                        "hooks.rules[]: rule '{}' sets \"match\" on event \"{}\", which carries \
                         no tool name -- \"match\" only applies to \"pre_tool_use\"/\
                         \"post_tool_use\"",
                        rule.id, rule.event
                    ),
                });
            }
        }
    }

    // 11. Every [hooks].rules[] `event` is a WELL-FORMED name -- bare
    //     (core-shaped) or `plugin_id.event_name` -- per
    //     `conway_core::event_name::validate_event_name`'s subscriber-side
    //     rule (`declaring_plugin: None`).
    //     this closes the FOLLOW-UP `schema::HookEntry::event`'s own doc
    //     comment used to name ("a sibling is deciding that
    //     rule" -- this is that item).
    //
    //     This checks SHAPE only, never membership: whether a well-formed
    //     namespaced `event` names an event some INSTALLED plugin actually
    //     declares needs the resolved plugin set, which this function has
    //     no access to -- that check is `ConwayBuilder::build`'s own (see
    //     `schema::HooksConfig`'s reachability doc for the tolerant
    //     "unknown -- never dispatched, not an error" rule that applies
    //     there, identical to a typo'd core event name today).
    {
        for rule in &config.hooks.rules {
            if let Err(reason) = validate_event_name(&rule.event, None) {
                return Err(ConwayError::Config {
                    path: None,
                    message: format!("hooks.rules[]: rule '{}': {reason}", rule.id),
                });
            }
        }
    }

    Ok(warnings)
}
