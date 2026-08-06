//! WI-097: round-trip, five-source precedence, env-var mapping, discovery,
//! and fail-loud schema validation (unknown keys, unknown role alias).

#[path = "support/mod.rs"]
mod support;

use std::collections::HashMap;

use conway::config::schema::{ConwayConfig, PermissionMode};
use conway::config::{load, CliOverrides, LoadOptions};
use conway_core::ids::RoleAlias;

const FULL_SCHEMA_JSON: &str = r#"
{
  "default_role": "coder",
  "cwd": ".",
  "session": {
    "root": ".conway/sessions",
    "fsync": "interval",
    "fsync_interval_ms": 200
  },
  "limits": {
    "max_steps": 40,
    "max_tokens": 0,
    "deadline_secs": 0,
    "max_parallel_tools": 4
  },
  "permissions": {
    "mode": "prompt",
    "allowed_tools": [],
    "denied_tools": []
  },
  "backends": {
    "anthropic": {
      "kind": "anthropic",
      "api_key": "",
      "api_key_env": "ANTHROPIC_API_KEY",
      "base_url": ""
    },
    "local": {
      "kind": "openai-compat",
      "dialect": "ollama",
      "base_url": "http://localhost:11434/v1",
      "api_key_env": "",
      "stream_tools": false
    }
  },
  "routing": {
    "default_headroom_tokens": 8192
  },
  "roles": {
    "coder": {
      "chain": ["local/qwen3-coder-80b", "anthropic/claude-sonnet-4-6"]
    },
    "planner": {
      "chain": ["anthropic/claude-sonnet-4-6"],
      "headroom_tokens": 40000
    }
  },
  "health": {
    "transport_failures_to_open": 3,
    "open_duration_secs": 30,
    "probe_interval_secs": 15,
    "probe_timeout_secs": 2,
    "probe_failures_to_open": 3
  },
  "agents": {
    "dir": ".conway/agents"
  },
  "models": {
    "metadata_path": ".conway/models.json",
    "probe_on_startup": false
  }
}
"#;

#[test]
fn conway_config_round_trips_the_full_documented_schema() {
    let cfg: ConwayConfig = serde_json::from_str(FULL_SCHEMA_JSON).expect("full schema must parse");
    let reserialized = serde_json::to_string(&cfg).expect("must serialize");
    let cfg2: ConwayConfig = serde_json::from_str(&reserialized).expect("reserialized must parse");
    assert_eq!(cfg, cfg2);
    assert_eq!(cfg.roles["coder"].chain.len(), 2);
    assert_eq!(cfg.backends["local"].stream_tools, Some(false));
}

/// Precedence test: default < XDG < project < env < CLI, proven for
/// `default_role`, `limits.max_steps`, and `permissions.mode` across all
/// five sources, and for `backends.<id>.base_url` across the four sources
/// that have a documented path (see the module-level disclosure below for
/// why CLI is excluded for that one key).
///
/// Reconciliation disclosed in the WI-097 Self-Check: `CliOverrides`' field
/// list (fixed by the amendment's implementation notes) has no per-backend
/// override, so there is no CLI path to `backends.<id>.base_url`. That key
/// is therefore proven across default/XDG/project/env only.
#[test]
fn five_source_precedence_across_representative_keys() {
    let root = support::unique_temp_dir("precedence");

    let xdg_home = root.join("xdg-home");
    std::fs::create_dir_all(xdg_home.join("conway")).unwrap();
    std::fs::write(
        xdg_home.join("conway").join("settings.json"),
        r#"
{
  "default_role": "role-x",
  "roles": {
    "role-x": { "chain": [] },
    "role-p": { "chain": [] },
    "role-e": { "chain": [] },
    "role-c": { "chain": [] }
  },
  "backends": {
    "anthropic": { "kind": "anthropic", "base_url": "https://xdg.example.com" }
  },
  "limits": { "max_steps": 11 },
  "permissions": { "mode": "deny" }
}
"#,
    )
    .unwrap();

    let project_dir = root.join("project");
    std::fs::create_dir_all(project_dir.join(".conway")).unwrap();
    std::fs::write(
        project_dir.join(".conway").join("settings.json"),
        r#"
{
  "default_role": "role-p",
  "backends": {
    "anthropic": { "base_url": "https://project.example.com" }
  },
  "limits": { "max_steps": 22 },
  "permissions": { "mode": "prompt" }
}
"#,
    )
    .unwrap();

    let mut xdg_only_env = HashMap::new();
    xdg_only_env.insert(
        "XDG_CONFIG_HOME".to_string(),
        xdg_home.to_string_lossy().to_string(),
    );

    let mut full_env = xdg_only_env.clone();
    full_env.insert("CONWAY_DEFAULT_ROLE".to_string(), "role-e".to_string());
    full_env.insert(
        "CONWAY_BACKENDS__ANTHROPIC__BASE_URL".to_string(),
        "https://env.example.com".to_string(),
    );
    full_env.insert("CONWAY_LIMITS__MAX_STEPS".to_string(), "33".to_string());
    full_env.insert("CONWAY_PERMISSIONS__MODE".to_string(), "deny".to_string());

    let full_cli = CliOverrides {
        default_role: Some(RoleAlias::new("role-c")),
        max_steps: Some(44),
        permission_mode: Some("prompt".to_string()),
        ..Default::default()
    };

    let opts = |env: HashMap<String, String>, cli: CliOverrides| LoadOptions {
        cwd: project_dir.clone(),
        explicit_path: None,
        env,
        cli_overrides: cli,
        model_metadata_refresh: false,
    };

    // Stage 1: all five present -> CLI wins.
    let outcome = load(opts(full_env.clone(), full_cli.clone())).unwrap();
    assert_eq!(outcome.config.default_role.as_str(), "role-c");
    assert_eq!(outcome.config.limits.max_steps, 44);
    assert_eq!(outcome.config.permissions.mode, PermissionMode::Prompt);

    // Stage 2: remove CLI -> env wins.
    let outcome = load(opts(full_env.clone(), CliOverrides::default())).unwrap();
    assert_eq!(outcome.config.default_role.as_str(), "role-e");
    assert_eq!(outcome.config.limits.max_steps, 33);
    assert_eq!(outcome.config.permissions.mode, PermissionMode::Deny);
    assert_eq!(
        outcome.config.backends["anthropic"].base_url,
        "https://env.example.com"
    );

    // Stage 3: remove CLI + env -> project wins.
    let outcome = load(opts(xdg_only_env.clone(), CliOverrides::default())).unwrap();
    assert_eq!(outcome.config.default_role.as_str(), "role-p");
    assert_eq!(outcome.config.limits.max_steps, 22);
    assert_eq!(outcome.config.permissions.mode, PermissionMode::Prompt);
    assert_eq!(
        outcome.config.backends["anthropic"].base_url,
        "https://project.example.com"
    );

    // Stage 4: remove CLI + env + project -> XDG wins.
    let empty_dir = root.join("empty-project");
    std::fs::create_dir_all(&empty_dir).unwrap();
    let outcome = load(LoadOptions {
        cwd: empty_dir.clone(),
        ..opts(xdg_only_env.clone(), CliOverrides::default())
    })
    .unwrap();
    assert_eq!(outcome.config.default_role.as_str(), "role-x");
    assert_eq!(outcome.config.limits.max_steps, 11);
    assert_eq!(outcome.config.permissions.mode, PermissionMode::Deny);
    assert_eq!(
        outcome.config.backends["anthropic"].base_url,
        "https://xdg.example.com"
    );

    // Stage 5: remove everything -> baked-in defaults.
    let outcome = load(LoadOptions {
        cwd: empty_dir,
        ..opts(HashMap::new(), CliOverrides::default())
    })
    .unwrap();
    assert_eq!(outcome.config.default_role.as_str(), "coder");
    assert_eq!(outcome.config.limits.max_steps, 40);
    assert_eq!(outcome.config.permissions.mode, PermissionMode::Prompt);
    assert!(!outcome.config.backends.contains_key("anthropic"));
}

#[test]
fn env_var_mapping_reads_known_vars_and_ignores_unknown_ones() {
    let mut env = HashMap::new();
    env.insert("CONWAY_DEFAULT_ROLE".to_string(), "coder".to_string());
    env.insert(
        "CONWAY_BACKENDS__ANTHROPIC__API_KEY".to_string(),
        "sk-ant-api03-abc".to_string(),
    );
    env.insert("CONWAY_LIMITS__MAX_STEPS".to_string(), "77".to_string());
    // Unknown top-level segment and an unrelated unprefixed var: both
    // ignored without error.
    env.insert(
        "CONWAY_TOTALLY_UNKNOWN_VAR".to_string(),
        "should-be-ignored".to_string(),
    );
    env.insert("SOME_OTHER_APP_VAR".to_string(), "irrelevant".to_string());

    let dir = support::unique_temp_dir("env-mapping");
    let outcome = load(LoadOptions {
        cwd: dir,
        explicit_path: None,
        env,
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .expect("unknown CONWAY_* vars must not cause an error");

    assert_eq!(outcome.config.default_role.as_str(), "coder");
    assert_eq!(
        outcome.config.backends["anthropic"].api_key,
        "sk-ant-api03-abc"
    );
    assert_eq!(outcome.config.limits.max_steps, 77);
}

#[test]
fn load_discovers_the_nearest_project_config_via_parent_walk() {
    let root = support::unique_temp_dir("discover-load");
    let nested = root.join("a").join("b");
    std::fs::create_dir_all(&nested).unwrap();
    let conf_dir = root.join("a").join(".conway");
    std::fs::create_dir_all(&conf_dir).unwrap();
    std::fs::write(
        conf_dir.join("settings.json"),
        r#"{"default_role":"coder","roles":{"coder":{"chain":[]}},"limits":{"max_steps":5}}"#,
    )
    .unwrap();

    let outcome = load(LoadOptions {
        cwd: nested,
        explicit_path: None,
        env: HashMap::new(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .unwrap();
    assert_eq!(outcome.config.limits.max_steps, 5);
}

#[test]
fn unknown_role_alias_in_default_role_names_the_alias_and_defined_roles() {
    let dir = support::unique_temp_dir("bad-role");
    let result = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(support::fixtures_dir().join("bad_role.json")),
        env: HashMap::new(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    });
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("ghost"),
        "error must name the unknown alias: {err}"
    );
    assert!(
        err.contains("coder"),
        "error must list defined roles: {err}"
    );
}

#[test]
fn typo_d_key_is_rejected_by_deny_unknown_fields() {
    let dir = support::unique_temp_dir("unknown-key");
    let result = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(support::fixtures_dir().join("unknown_key.json")),
        env: HashMap::new(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    });
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("max_step"),
        "error must name the typo'd key: {err}"
    );
}

#[test]
fn typo_d_health_key_is_rejected_by_deny_unknown_fields() {
    let dir = support::unique_temp_dir("unknown-health-key");
    let result = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(support::fixtures_dir().join("unknown_health_key.json")),
        env: HashMap::new(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    });
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("transport_failures_to_opne"),
        "error must name the typo'd [health] key: {err}"
    );
}

/// Sibling of `typo_d_key_is_rejected_by_deny_unknown_fields` and
/// `typo_d_health_key_is_rejected_by_deny_unknown_fields` for `RoleEntry`'s
/// six capability-floor fields (`tool_calling`/`structured_output`/
/// `parallel_tool_calls`/`reasoning`/`min_reliability`/`min_context`):
/// they are structurally protected today by `RoleEntry`'s own
/// `#[serde(deny_unknown_fields, default)]`, but nothing pinned that
/// against a refactor that drops the annotation -- this project has direct
/// precedent for exactly that silent-typo failure mode (a misspelled
/// `builtin_plugins` id was silently ignored, cf98357).
#[test]
fn typo_d_role_capability_key_is_rejected_by_deny_unknown_fields() {
    let dir = support::unique_temp_dir("unknown-role-capability-key");
    let result = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(support::fixtures_dir().join("unknown_role_capability_key.json")),
        env: HashMap::new(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    });
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("min_reliabilty"),
        "error must name the typo'd [roles.<alias>] key: {err}"
    );
}
