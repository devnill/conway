//! WI-097 amendment: the headroom config surface — global default,
//! per-role override, precedence, hard-error validation, and the
//! deterministic "headroom exceeds context" warning.

#[path = "support/mod.rs"]
mod support;

use std::collections::HashMap;

use conway::config::schema::{ConwayConfig, DEFAULT_HEADROOM_TOKENS};
use conway::config::{load, CliOverrides, LoadOptions, WarningCode};
use conway_core::ids::RoleAlias;

/// Disclosed reconciliation (see `schema::DEFAULT_HEADROOM_TOKENS`'s doc
/// comment): the amendment's prose says the built-in default is `16000`.
/// The already-committed `conway_core::capabilities::DEFAULT_HEADROOM_TOKENS`
/// (and `conway-routing`'s `HeadroomPolicy`, which explicitly notes it
/// supersedes an earlier, different literal) is `8_192`. This crate reuses
/// that cross-crate-agreed constant rather than introducing a third,
/// disagreeing default.
#[test]
fn empty_config_default_headroom_matches_the_cross_crate_constant_not_the_amendment_literal() {
    let cfg: ConwayConfig =
        toml::from_str("default_role = \"coder\"\n\n[roles.coder]\nchain = []\n").unwrap();
    assert_eq!(cfg.routing.default_headroom_tokens, DEFAULT_HEADROOM_TOKENS);
    assert_eq!(DEFAULT_HEADROOM_TOKENS, 8_192);
}

#[test]
fn headroom_for_role_override_vs_global_default_vs_unknown_alias() {
    let cfg: ConwayConfig = toml::from_str(
        "default_role = \"coder\"\n\n\
         [roles.coder]\nchain = []\n\n\
         [roles.planner]\nchain = []\nheadroom_tokens = 40000\n\n\
         [roles.fast]\nchain = []\n\n\
         [routing]\ndefault_headroom_tokens = 16000\n",
    )
    .unwrap();

    assert_eq!(cfg.headroom_for(&RoleAlias::new("planner")), 40_000);
    assert_eq!(cfg.headroom_for(&RoleAlias::new("fast")), 16_000);
    assert_eq!(cfg.headroom_for(&RoleAlias::new("nonexistent")), 16_000);
}

#[test]
fn headroom_default_participates_in_the_full_five_source_precedence_chain() {
    let root = support::unique_temp_dir("headroom-precedence");

    let xdg_home = root.join("xdg-home");
    std::fs::create_dir_all(xdg_home.join("conway")).unwrap();
    std::fs::write(
        xdg_home.join("conway").join("conway.toml"),
        "default_role = \"coder\"\n\n[roles.coder]\nchain = []\n\n[routing]\ndefault_headroom_tokens = 20000\n",
    )
    .unwrap();

    let project_dir = root.join("project");
    std::fs::create_dir_all(project_dir.join(".conway")).unwrap();
    std::fs::write(
        project_dir.join(".conway").join("conway.toml"),
        "default_role = \"coder\"\n\n[roles.coder]\nchain = []\n\n[routing]\ndefault_headroom_tokens = 30000\n",
    )
    .unwrap();

    let role = RoleAlias::new("fast");

    let mut xdg_only_env = HashMap::new();
    xdg_only_env.insert(
        "XDG_CONFIG_HOME".to_string(),
        xdg_home.to_string_lossy().to_string(),
    );
    let mut full_env = xdg_only_env.clone();
    full_env.insert(
        "CONWAY_ROUTING__DEFAULT_HEADROOM_TOKENS".to_string(),
        "40000".to_string(),
    );
    let cli = CliOverrides {
        headroom_tokens: Some(50_000),
        ..Default::default()
    };

    let opts =
        |cwd: std::path::PathBuf, env: HashMap<String, String>, c: CliOverrides| LoadOptions {
            cwd,
            explicit_path: None,
            env,
            cli_overrides: c,
            model_metadata_refresh: false,
        };

    // C wins.
    let outcome = load(opts(project_dir.clone(), full_env.clone(), cli.clone())).unwrap();
    assert_eq!(outcome.config.headroom_for(&role), 50_000);

    // remove C -> E.
    let outcome = load(opts(
        project_dir.clone(),
        full_env.clone(),
        CliOverrides::default(),
    ))
    .unwrap();
    assert_eq!(outcome.config.headroom_for(&role), 40_000);

    // remove C, E -> P.
    let outcome = load(opts(
        project_dir.clone(),
        xdg_only_env.clone(),
        CliOverrides::default(),
    ))
    .unwrap();
    assert_eq!(outcome.config.headroom_for(&role), 30_000);

    // remove C, E, P -> X.
    let empty_dir = root.join("empty");
    std::fs::create_dir_all(&empty_dir).unwrap();
    let outcome = load(opts(
        empty_dir.clone(),
        xdg_only_env.clone(),
        CliOverrides::default(),
    ))
    .unwrap();
    assert_eq!(outcome.config.headroom_for(&role), 20_000);

    // remove everything -> D.
    let outcome = load(opts(empty_dir, HashMap::new(), CliOverrides::default())).unwrap();
    assert_eq!(outcome.config.headroom_for(&role), DEFAULT_HEADROOM_TOKENS);
}

#[test]
fn per_role_headroom_from_a_lower_precedence_source_beats_a_higher_sources_global_default() {
    let root = support::unique_temp_dir("headroom-role-vs-global");

    let xdg_home = root.join("xdg-home");
    std::fs::create_dir_all(xdg_home.join("conway")).unwrap();
    std::fs::write(
        xdg_home.join("conway").join("conway.toml"),
        "default_role = \"coder\"\n\n[roles.coder]\nchain = []\n\n[roles.planner]\nchain = []\nheadroom_tokens = 40000\n",
    )
    .unwrap();

    let project_dir = root.join("project");
    std::fs::create_dir_all(project_dir.join(".conway")).unwrap();
    std::fs::write(
        project_dir.join(".conway").join("conway.toml"),
        "default_role = \"coder\"\n\n[roles.coder]\nchain = []\n\n[routing]\ndefault_headroom_tokens = 8000\n",
    )
    .unwrap();

    let mut env = HashMap::new();
    env.insert(
        "XDG_CONFIG_HOME".to_string(),
        xdg_home.to_string_lossy().to_string(),
    );

    let outcome = load(LoadOptions {
        cwd: project_dir,
        explicit_path: None,
        env,
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .unwrap();

    assert_eq!(
        outcome.config.headroom_for(&RoleAlias::new("planner")),
        40_000
    );
    assert_eq!(outcome.config.headroom_for(&RoleAlias::new("fast")), 8_000);
}

#[test]
fn env_var_overrides_a_per_role_headroom() {
    let root = support::unique_temp_dir("headroom-env-role");
    let xdg_home = root.join("xdg-home");
    std::fs::create_dir_all(xdg_home.join("conway")).unwrap();
    std::fs::write(
        xdg_home.join("conway").join("conway.toml"),
        "default_role = \"coder\"\n\n[roles.coder]\nchain = []\n\n[roles.planner]\nchain = []\nheadroom_tokens = 40000\n",
    )
    .unwrap();

    let mut env = HashMap::new();
    env.insert(
        "XDG_CONFIG_HOME".to_string(),
        xdg_home.to_string_lossy().to_string(),
    );
    env.insert(
        "CONWAY_ROLES__PLANNER__HEADROOM_TOKENS".to_string(),
        "30000".to_string(),
    );

    let outcome = load(LoadOptions {
        cwd: root,
        explicit_path: None,
        env,
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .unwrap();

    assert_eq!(
        outcome.config.headroom_for(&RoleAlias::new("planner")),
        30_000
    );
}

#[test]
fn env_var_for_an_unknown_role_alias_is_ignored() {
    let root = support::unique_temp_dir("headroom-env-unknown-role");
    let mut env = HashMap::new();
    env.insert(
        "CONWAY_ROLES__GHOST__HEADROOM_TOKENS".to_string(),
        "999".to_string(),
    );

    let outcome = load(LoadOptions {
        cwd: root,
        explicit_path: None,
        env,
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .expect("an env var naming an unknown role alias must be ignored, not error");
    assert!(!outcome.config.roles.contains_key("ghost"));
}

#[test]
fn zero_global_headroom_is_a_hard_error() {
    let dir = support::unique_temp_dir("headroom-zero-global");
    let result = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(support::fixtures_dir().join("headroom_zero.toml")),
        env: HashMap::new(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    });
    let err = result.unwrap_err().to_string();
    assert!(err.contains("default_headroom_tokens"));
    assert!(err.contains("must be greater than 0"));
}

#[test]
fn zero_per_role_headroom_is_a_hard_error_naming_the_role() {
    let dir = support::unique_temp_dir("headroom-zero-role");
    let path = dir.join("conway.toml");
    std::fs::write(
        &path,
        "default_role = \"coder\"\n\n[roles.coder]\nchain = []\nheadroom_tokens = 0\n",
    )
    .unwrap();

    let result = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(path),
        env: HashMap::new(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    });
    let err = result.unwrap_err().to_string();
    assert!(err.contains("coder"));
    assert!(err.contains("must be greater than 0"));
}

#[test]
fn headroom_role_override_fixture_resolves_as_documented() {
    let dir = support::unique_temp_dir("headroom-role-override-fixture");
    let outcome = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(support::fixtures_dir().join("headroom_role_override.toml")),
        env: HashMap::new(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .unwrap();
    assert_eq!(
        outcome.config.headroom_for(&RoleAlias::new("planner")),
        40_000
    );
    assert_eq!(outcome.config.headroom_for(&RoleAlias::new("fast")), 8_000);
}

#[test]
fn headroom_exceeding_smallest_reachable_context_warns_without_clamping() {
    let fixtures = support::fixtures_dir();
    let outcome = load(LoadOptions {
        cwd: fixtures.clone(),
        explicit_path: Some(fixtures.join("headroom_exceeds_context.toml")),
        env: HashMap::new(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .unwrap();

    assert_eq!(outcome.warnings.len(), 1);
    let warning = &outcome.warnings[0];
    assert_eq!(warning.code, WarningCode::HeadroomExceedsContext);
    assert!(warning.message.contains("coder"));
    assert!(warning.message.contains("200000"));
    assert!(warning.message.contains("anthropic/claude-haiku-4-5"));
    assert!(warning.message.contains("32768"));

    // Not clamped: the configured value survives unmodified.
    assert_eq!(
        outcome.config.headroom_for(&RoleAlias::new("coder")),
        200_000
    );
}

#[test]
fn no_headroom_warning_when_model_metadata_is_absent() {
    let dir = support::unique_temp_dir("headroom-no-metadata");
    let path = dir.join("conway.toml");
    std::fs::write(
        &path,
        "default_role = \"coder\"\n\n\
         [roles.coder]\nchain = [\"anthropic/claude-haiku-4-5\"]\nheadroom_tokens = 200000\n\n\
         [backends.anthropic]\nkind = \"anthropic\"\n",
    )
    .unwrap();

    let outcome = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(path),
        env: HashMap::new(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .unwrap();
    assert!(outcome.warnings.is_empty());
}

#[test]
fn two_offending_roles_produce_deterministically_ordered_warnings() {
    let dir = support::unique_temp_dir("headroom-two-roles");
    let metadata_path = dir.join("models.json");
    std::fs::write(
        &metadata_path,
        r#"{"models":{"anthropic/claude-haiku-4-5":{"max_context_tokens":32768,"tool_calling":"streaming","reasoning":false,"reliability_tier":"verified"}}}"#,
    )
    .unwrap();

    let toml_text = format!(
        "default_role = \"alpha\"\n\n\
         [roles.alpha]\nchain = [\"anthropic/claude-haiku-4-5\"]\nheadroom_tokens = 100000\n\n\
         [roles.zeta]\nchain = [\"anthropic/claude-haiku-4-5\"]\nheadroom_tokens = 100000\n\n\
         [backends.anthropic]\nkind = \"anthropic\"\n\n\
         [models]\nmetadata_path = {:?}\n",
        metadata_path.to_string_lossy()
    );
    let path = dir.join("conway.toml");
    std::fs::write(&path, &toml_text).unwrap();

    let run = || {
        load(LoadOptions {
            cwd: dir.clone(),
            explicit_path: Some(path.clone()),
            env: HashMap::new(),
            cli_overrides: CliOverrides::default(),
            model_metadata_refresh: false,
        })
        .unwrap()
    };

    let first = run();
    let second = run();
    assert_eq!(first.warnings.len(), 2);
    assert!(first.warnings[0].message.contains("alpha"));
    assert!(first.warnings[1].message.contains("zeta"));
    assert_eq!(first.warnings, second.warnings);
}
