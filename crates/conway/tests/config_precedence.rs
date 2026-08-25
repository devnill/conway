//! Round-trip, five-source precedence, env-var mapping, discovery,
//! and fail-loud schema validation (unknown keys, unknown role alias).

#[path = "support/mod.rs"]
mod support;

use std::collections::HashMap;

use conway::config::discovery;
use conway::config::schema::{ConwayConfig, PermissionMode};
use conway::config::{load, load_ignoring_user_config, CliOverrides, LoadOptions};
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
    "half_open_successes_to_close": 1
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

/// Precedence test: default < user < project < env < CLI, proven for
/// `default_role`, `limits.max_steps`, and `permissions.mode` across all
/// five sources, and for `backends.<id>.base_url` across the four sources
/// that have a documented path (see the module-level disclosure below for
/// why CLI is excluded for that one key).
///
/// Reconciliation disclosed here: `CliOverrides`' field
/// list (fixed by the amendment's implementation notes) has no per-backend
/// override, so there is no CLI path to `backends.<id>.base_url`. That key
/// is therefore proven across default/user/project/env only.
#[test]
fn five_source_precedence_across_representative_keys() {
    let root = support::unique_temp_dir("precedence");

    let config_home = root.join("config_dir-home");
    std::fs::create_dir_all(&config_home).unwrap();
    std::fs::write(
        config_home.join("settings.json"),
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
    "anthropic": { "kind": "anthropic", "base_url": "https://config_dir.example.com" }
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

    let mut user_only_env = HashMap::new();
    user_only_env.insert(
        "CONWAY_CONFIG_DIR".to_string(),
        config_home.to_string_lossy().to_string(),
    );

    let mut full_env = user_only_env.clone();
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
    let outcome = load(opts(user_only_env.clone(), CliOverrides::default())).unwrap();
    assert_eq!(outcome.config.default_role.as_str(), "role-p");
    assert_eq!(outcome.config.limits.max_steps, 22);
    assert_eq!(outcome.config.permissions.mode, PermissionMode::Prompt);
    assert_eq!(
        outcome.config.backends["anthropic"].base_url,
        "https://project.example.com"
    );

    // Stage 4: remove CLI + env + project -> user config wins.
    let empty_dir = root.join("empty-project");
    std::fs::create_dir_all(&empty_dir).unwrap();
    let outcome = load(LoadOptions {
        cwd: empty_dir.clone(),
        ..opts(user_only_env.clone(), CliOverrides::default())
    })
    .unwrap();
    assert_eq!(outcome.config.default_role.as_str(), "role-x");
    assert_eq!(outcome.config.limits.max_steps, 11);
    assert_eq!(outcome.config.permissions.mode, PermissionMode::Deny);
    assert_eq!(
        outcome.config.backends["anthropic"].base_url,
        "https://config_dir.example.com"
    );

    // Stage 5: remove everything -> baked-in defaults. Still an isolated
    // `CONWAY_CONFIG_DIR` (not a bare `HashMap::new()`): "remove everything"
    // means no source names a value, not "read whatever real settings.json
    // this machine has" (see `support::isolated_env`'s doc comment).
    let outcome = load(LoadOptions {
        cwd: empty_dir,
        ..opts(support::isolated_env(), CliOverrides::default())
    })
    .unwrap();
    assert_eq!(outcome.config.default_role.as_str(), "default");
    assert_eq!(outcome.config.limits.max_steps, 40);
    assert_eq!(outcome.config.permissions.mode, PermissionMode::Prompt);
    assert!(!outcome.config.backends.contains_key("anthropic"));
}

#[test]
fn env_var_mapping_reads_known_vars_and_ignores_unknown_ones() {
    let mut env = support::isolated_env();
    // "default" (not an arbitrary alias): with no project/user-config source, the
    // merged `[roles]` table is only the baked-in default's own
    // `roles.default` (`config::merge::default_document`) -- naming any
    // other alias here would fail the "`default_role` exists in `[roles]`"
    // validation check for reasons unrelated to what this test proves.
    env.insert("CONWAY_DEFAULT_ROLE".to_string(), "default".to_string());
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

    assert_eq!(outcome.config.default_role.as_str(), "default");
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
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .unwrap();
    assert_eq!(outcome.config.limits.max_steps, 5);
}

/// Board item `01M0VV6CVSZM4XH8J4G6EBV5E3`, the "one file, two roles"
/// collision, proven in-process (a compiled-binary sibling,
/// `crates/conway-cli/tests/config_isolation_binary.rs`, covers the
/// specific case that actually cost the operator two live provider calls --
/// `~/.conway/settings.json` reached from a `cwd` under the REAL home
/// directory, which this in-process test cannot construct without mutating
/// this test PROCESS's real `HOME`, unsafe under parallel test execution).
/// This test instead constructs the identical collision SHAPE fully
/// deterministically: `CONWAY_CONFIG_DIR` is pointed at an ancestor
/// directory's own `.conway/` subdirectory, so `user_config_path(env)`
/// (`$CONWAY_CONFIG_DIR/settings.json`) and `discover`'s own upward walk
/// from a nested `cwd` beneath that ancestor land on the EXACT SAME file --
/// the literal "resolved user config" collision the item's own Option 3
/// text names -- with no dependence on this machine's real home directory
/// at all.
///
/// **Uses [`load_ignoring_user_config`], not [`load`] -- load-bearing, not
/// a stylistic choice.** With the *user* layer read (`load`), the colliding
/// file's content reaches the merged config via that layer regardless of
/// whether `discover` also (redundantly) returns it as the *project* layer
/// -- same file, same bytes, so a correct exclusion is completely
/// unobservable in the merged result (this is precisely the "harmless
/// dedup" this module's own doc describes for the CONWAY_CONFIG_DIR-unset
/// case). `load_ignoring_user_config` removes that channel entirely: the ONLY
/// way the colliding file's `session.root` can reach `outcome.config` here
/// is through the *project* layer, so this is a clean pre/post-fix
/// discriminator -- before the fix, `discover`'s old, un-excluded walk
/// still finds and returns it; after the fix, `project_discovery_exclusions`
/// makes `discover` skip it, and NO layer supplies `session.root` at all
/// (project skipped, user layer not read by this function at all).
#[test]
fn load_ignoring_user_config_excludes_a_project_candidate_matching_the_resolved_user_config() {
    let root = support::unique_temp_dir("collision-session-root");
    // Stands in for the ancestor directory whose OWN `.conway/` this test
    // also points `CONWAY_CONFIG_DIR` at -- the shape that makes the two
    // resolutions collide.
    let ancestor = root.join("ancestor");
    let conf_dir = ancestor.join(".conway");
    std::fs::create_dir_all(&conf_dir).unwrap();
    let poisoned_root = ancestor.join("POISONED-SESSIONS");
    std::fs::write(
        conf_dir.join("settings.json"),
        format!(
            r#"{{"session":{{"root":{:?}}}}}"#,
            poisoned_root.to_string_lossy()
        ),
    )
    .unwrap();

    let nested_cwd = ancestor.join("work").join("project");
    std::fs::create_dir_all(&nested_cwd).unwrap();

    let mut env = HashMap::new();
    env.insert(
        "CONWAY_CONFIG_DIR".to_string(),
        conf_dir.to_string_lossy().to_string(),
    );

    // Sanity: `user_config_path` really does resolve to the exact file
    // `discover` would otherwise find as a project candidate -- this
    // assertion is what makes the rest of this test a collision test rather
    // than an ordinary discovery test.
    assert_eq!(
        discovery::user_config_path(&env).unwrap(),
        conf_dir.join("settings.json")
    );

    let outcome = load_ignoring_user_config(LoadOptions {
        cwd: nested_cwd.clone(),
        explicit_path: None,
        env: env.clone(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .unwrap();

    assert_ne!(
        outcome.config.session.root,
        Some(poisoned_root),
        "the colliding file must not win as a *project* layer when it is \
         the identical file already excluded as (what would be) the *user* \
         layer -- with the user layer not even read by \
         load_ignoring_user_config, nothing should have supplied session.root \
         from a config file at all"
    );
    // `load_impl`'s own central-default resolution runs unconditionally
    // (regardless of `IncludeUserLayer`) whenever no layer set
    // `session.root` -- with the collision excluded and no OTHER project
    // config between `nested_cwd` and the ancestor, that is exactly what
    // happens here, so `session.root` lands on the SAME central default
    // `discovery::session_root` computes directly (which itself is based on
    // `user_config_path(env)`'s directory, i.e. `CONWAY_CONFIG_DIR` -- not
    // on any file's content).
    let expected_default = discovery::session_root(&nested_cwd, None, &env);
    assert_eq!(
        outcome.config.session.root,
        Some(expected_default),
        "with the collision excluded, session.root should fall through to \
         the central default resolved against CONWAY_CONFIG_DIR, not the \
         poisoned file content"
    );
}

#[test]
fn unknown_role_alias_in_default_role_names_the_alias_and_defined_roles() {
    let dir = support::unique_temp_dir("bad-role");
    let result = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(support::fixtures_dir().join("bad_role.json")),
        env: support::isolated_env(),
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
        env: support::isolated_env(),
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
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    });
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("transport_failures_to_opne"),
        "error must name the typo'd [health] key: {err}"
    );
}

/// **Breaking-change coverage (a later item, "retire
/// the health prober"): a `settings.json` naming a removed `[health].probe_*`
/// key fails loudly, naming the key, rather than loading silently.**
/// `probe_enabled`/`probe_interval_secs`/`probe_timeout_secs`/
/// `probe_failures_to_open` used to configure a periodic health prober and
/// the independent `Probe` breaker it fed; both were retired because the
/// prober had no production call site and the Transport breaker alone
/// already handles recovery. `HealthSection` keeps
/// `#[serde(deny_unknown_fields)]`, so a config that previously loaded
/// (silently accepting these keys) now fails to load at all -- the same
/// mechanism `typo_d_health_key_is_rejected_by_deny_unknown_fields` above
/// proves for a genuine typo.
#[test]
fn removed_health_probe_key_is_rejected_by_deny_unknown_fields() {
    let dir = support::unique_temp_dir("removed-health-probe-key");
    let result = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(support::fixtures_dir().join("removed_health_probe_key.json")),
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    });
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("probe_enabled"),
        "error must name the removed [health] key: {err}"
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
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    });
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("min_reliabilty"),
        "error must name the typo'd [roles.<alias>] key: {err}"
    );
}
