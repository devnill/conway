//! Five-source precedence merge (default < XDG < project < env < CLI),
//! `CONWAY_*` environment mapping, and `ConwayConfig` semantic validation
//! (including mandatory `sk-ant-oat*` rejection and the headroom checks).
//!
//! Merge happens on `serde_json::Value` (tables union by key, arrays and
//! scalars replace wholesale), and only the final merged document is
//! deserialized into [`ConwayConfig`] — this is what makes
//! `#[serde(deny_unknown_fields)]` a meaningful fail-loud check on the
//! *result* of layering five sources, rather than on each source
//! individually (a source may legitimately omit almost everything).

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use conway_core::ids::{ModelRef, RoleAlias};
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

/// The subset of config keys the CLI exposes, mirrored here (not in
/// `conway-cli`) so the library is the source of truth (C-03).
///
/// Reconciliation disclosed in the WI-097 Self-Check: this field list is
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
    pub model: Option<ModelRef>,
    pub permission_mode: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    pub denied_tools: Option<Vec<String>>,
    pub max_steps: Option<u32>,
    pub session_root: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    /// Amendment addition: sets `routing.default_headroom_tokens`. There is
    /// no CLI form for a per-role override — a CLI-supplied value is a
    /// session-wide floor.
    pub headroom_tokens: Option<u32>,
}

/// The full five-source load: default < XDG < project < env < CLI.
pub fn load(options: LoadOptions) -> Result<LoadOutcome> {
    let mut merged = default_document();

    if let Some(path) = discovery::xdg_config_path(&options.env) {
        if let Some(layer) = read_json_layer(&path)? {
            merge_values(&mut merged, layer);
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

    let config: ConwayConfig = serde_json::from_value(merged).map_err(|e| ConwayError::Config {
        path: None,
        message: format!("failed to parse merged configuration: {e}"),
    })?;

    let metadata_path = resolve_metadata_path(&config, &options.cwd);
    let metadata = model_metadata::load(&metadata_path)?;

    let warnings = validate(&config, &metadata, &options.env)?;

    Ok(LoadOutcome { config, warnings })
}

/// Re-applies `cli_overrides` to an already-loaded config (used by
/// `ConwayBuilder::build`, WI-100, so a CLI-supplied `sk-ant-oat*` key or an
/// invalid override is caught even when `from_parts`/`from_config` bypassed
/// `load`'s own CLI layer). Re-runs full validation.
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
    validate(&merged, &metadata, &HashMap::new())?;

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
/// defaults exactly (WI-097's Implementation Notes / the headroom
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
            "probe_interval_secs": 15,
            "probe_timeout_secs": 2,
            "probe_failures_to_open": 3,
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
];

/// Array-valued leaf keys, so their env values are comma-split rather than
/// parsed as a single scalar.
const ARRAY_LEAF_KEYS: &[&str] = &["allowed_tools", "denied_tools"];

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
    // `cli.model` is a routing pin consumed downstream by
    // `ConwayBuilder`/`SessionSpec` (WI-100), not a `ConwayConfig` field —
    // deliberately not merged here.
    Value::Object(root)
}

const OAUTH_TOKEN_PREFIX: &str = "sk-ant-oat";

fn oauth_error(backend_id: &str, source: &str) -> ConwayError {
    ConwayError::Config {
        path: None,
        message: format!(
            "Anthropic subscription OAuth tokens (sk-ant-oat*) are not supported: using a Claude \
             subscription token through a third-party harness is prohibited by Anthropic's Terms \
             of Service and has been technically blocked since February 2026. Use an API key \
             (sk-ant-api*) from console.anthropic.com instead. (backend: {backend_id}, source: {source})"
        ),
    }
}

/// Runs every validation step in the documented order, failing on the
/// first hard error (steps 1-7) and returning accumulated warnings from
/// step 8.
pub fn validate(
    config: &ConwayConfig,
    metadata: &ModelMetadata,
    env: &HashMap<String, String>,
) -> Result<Vec<ConfigWarning>> {
    // 1. OAuth token rejection — direct `api_key`, and the value named by
    //    `api_key_env` when it resolves in `env` (the injected
    //    environment). Applies regardless of which layer (file, `CONWAY_*`
    //    env, or a hypothetical CLI layer) produced the offending value:
    //    this function only sees the final merged `ConwayConfig`.
    for (id, backend) in &config.backends {
        if backend.api_key.starts_with(OAUTH_TOKEN_PREFIX) {
            return Err(oauth_error(id, "api_key"));
        }
        if !backend.api_key_env.is_empty() {
            if let Some(resolved) = env.get(&backend.api_key_env) {
                if resolved.starts_with(OAUTH_TOKEN_PREFIX) {
                    return Err(oauth_error(id, "api_key_env"));
                }
            }
        }
    }

    // 2. default_role exists in [roles].
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

    // 3. Every ModelRef in every chain has the form <backend_id>/<model>
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

    // 4. permissions.mode = "allowlist" requires non-empty allowed_tools.
    if matches!(
        config.permissions.mode,
        crate::config::schema::PermissionMode::Allowlist
    ) && config.permissions.allowed_tools.is_empty()
    {
        return Err(ConwayError::Config {
            path: None,
            message: "permissions.mode = \"allowlist\" requires a non-empty allowed_tools list"
                .to_string(),
        });
    }

    // 5. fsync = "interval" requires fsync_interval_ms > 0.
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

    // 6. api_key and api_key_env are not both non-empty for the same
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

    // 7. Hard error: headroom values must be > 0 (global and every present
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

    // 8. Warning only: headroom >= smallest reachable model context.
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

    Ok(warnings)
}
