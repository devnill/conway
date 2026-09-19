//! Amendment: the headroom config surface — global default,
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
/// (also consumed directly by `conway_core::capabilities::HeadroomPolicy`)
/// is `8_192`. This crate reuses that cross-crate-agreed constant rather
/// than introducing a third, disagreeing default.
#[test]
fn empty_config_default_headroom_matches_the_cross_crate_constant_not_the_amendment_literal() {
    let cfg: ConwayConfig =
        serde_json::from_str(r#"{"default_role":"coder","roles":{"coder":{"chain":[]}}}"#).unwrap();
    assert_eq!(cfg.routing.default_headroom_tokens, DEFAULT_HEADROOM_TOKENS);
    assert_eq!(DEFAULT_HEADROOM_TOKENS, 8_192);
}

#[test]
fn headroom_for_role_override_vs_global_default_vs_unknown_alias() {
    let cfg: ConwayConfig = serde_json::from_str(
        r#"{
            "default_role": "coder",
            "roles": {
                "coder": { "chain": [] },
                "planner": { "chain": [], "headroom_tokens": 40000 },
                "fast": { "chain": [] }
            },
            "routing": { "default_headroom_tokens": 16000 }
        }"#,
    )
    .unwrap();

    assert_eq!(cfg.headroom_for(&RoleAlias::new("planner")), 40_000);
    assert_eq!(cfg.headroom_for(&RoleAlias::new("fast")), 16_000);
    assert_eq!(cfg.headroom_for(&RoleAlias::new("nonexistent")), 16_000);
}

#[test]
fn headroom_default_participates_in_the_full_five_source_precedence_chain() {
    let root = support::unique_temp_dir("headroom-precedence");

    let config_home = root.join("config_dir-home");
    std::fs::create_dir_all(&config_home).unwrap();
    std::fs::write(
        config_home.join("settings.json"),
        r#"{"default_role":"coder","roles":{"coder":{"chain":[]}},"routing":{"default_headroom_tokens":20000}}"#,
    )
    .unwrap();

    let project_dir = root.join("project");
    std::fs::create_dir_all(project_dir.join(".conway")).unwrap();
    std::fs::write(
        project_dir.join(".conway").join("settings.json"),
        r#"{"default_role":"coder","roles":{"coder":{"chain":[]}},"routing":{"default_headroom_tokens":30000}}"#,
    )
    .unwrap();

    let role = RoleAlias::new("fast");

    let mut user_only_env = HashMap::new();
    user_only_env.insert(
        "CONWAY_CONFIG_DIR".to_string(),
        config_home.to_string_lossy().to_string(),
    );
    let mut full_env = user_only_env.clone();
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
        user_only_env.clone(),
        CliOverrides::default(),
    ))
    .unwrap();
    assert_eq!(outcome.config.headroom_for(&role), 30_000);

    // remove C, E, P -> X.
    let empty_dir = root.join("empty");
    std::fs::create_dir_all(&empty_dir).unwrap();
    let outcome = load(opts(
        empty_dir.clone(),
        user_only_env.clone(),
        CliOverrides::default(),
    ))
    .unwrap();
    assert_eq!(outcome.config.headroom_for(&role), 20_000);

    // remove everything -> D. Still an isolated `CONWAY_CONFIG_DIR` (not a
    // bare `HashMap::new()`): the point of this stage is "no source names a
    // value," not "read whatever real settings.json this machine has" (see
    // `support::isolated_env`'s doc comment).
    let outcome = load(opts(
        empty_dir,
        support::isolated_env(),
        CliOverrides::default(),
    ))
    .unwrap();
    assert_eq!(outcome.config.headroom_for(&role), DEFAULT_HEADROOM_TOKENS);
}

#[test]
fn per_role_headroom_from_a_lower_precedence_source_beats_a_higher_sources_global_default() {
    let root = support::unique_temp_dir("headroom-role-vs-global");

    let config_home = root.join("config_dir-home");
    std::fs::create_dir_all(&config_home).unwrap();
    std::fs::write(
        config_home.join("settings.json"),
        r#"{"default_role":"coder","roles":{"coder":{"chain":[]},"planner":{"chain":[],"headroom_tokens":40000}}}"#,
    )
    .unwrap();

    let project_dir = root.join("project");
    std::fs::create_dir_all(project_dir.join(".conway")).unwrap();
    std::fs::write(
        project_dir.join(".conway").join("settings.json"),
        r#"{"default_role":"coder","roles":{"coder":{"chain":[]}},"routing":{"default_headroom_tokens":8000}}"#,
    )
    .unwrap();

    let mut env = HashMap::new();
    env.insert(
        "CONWAY_CONFIG_DIR".to_string(),
        config_home.to_string_lossy().to_string(),
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
    let config_home = root.join("config_dir-home");
    std::fs::create_dir_all(&config_home).unwrap();
    std::fs::write(
        config_home.join("settings.json"),
        r#"{"default_role":"coder","roles":{"coder":{"chain":[]},"planner":{"chain":[],"headroom_tokens":40000}}}"#,
    )
    .unwrap();

    let mut env = HashMap::new();
    env.insert(
        "CONWAY_CONFIG_DIR".to_string(),
        config_home.to_string_lossy().to_string(),
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
    let mut env = support::isolated_env();
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
        explicit_path: Some(support::fixtures_dir().join("headroom_zero.json")),
        env: support::isolated_env(),
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
    let path = dir.join("settings.json");
    std::fs::write(
        &path,
        r#"{"default_role":"coder","roles":{"coder":{"chain":[],"headroom_tokens":0}}}"#,
    )
    .unwrap();

    let result = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(path),
        env: support::isolated_env(),
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
        explicit_path: Some(support::fixtures_dir().join("headroom_role_override.json")),
        env: support::isolated_env(),
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
        explicit_path: Some(fixtures.join("headroom_exceeds_context.json")),
        env: support::isolated_env(),
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

/// New for board item `01M1AVZPTRSWVE33G4DTJY7Q1B`:
/// `WarningCode::HeadroomConsumesLargeFractionOfContext` fires below the
/// literal-exceeds threshold (headroom(8192) < window(32768), so
/// `headroom_exceeding_smallest_reachable_context_warns_without_clamping`'s
/// own check does not fire) once headroom reaches the fraction threshold --
/// exactly conway's own built-in default against a 32768-token window, the
/// number the item's own walked scenario names. Fails against the
/// pre-change code: before this item, `validate`'s step 7 only ever
/// produced `HeadroomExceedsContext`, never this variant, so
/// `outcome.warnings` would be empty here.
///
/// **`headroom_fraction: 0` here, explicitly** -- this same board item's
/// OWN later work (`headroom_fraction`'s finishing pass, this file's
/// `default_headroom_fraction_computes_headroom_adaptively_with_no_config`
/// test below) makes the adaptive fraction conway's actual DEFAULT. Left at
/// that default, THIS config's flat `8192` would itself be adaptively
/// overridden to `max(32768/10, 2048) = 3276` before `validate` ever sees
/// it -- 3276/32768 is ~10%, under the 25% threshold, so the warning this
/// test exists to pin would stop firing and the scenario it walks (an
/// operator who set a flat headroom that happens to consume a large
/// fraction) would no longer be reachable through the default path. Setting
/// `headroom_fraction: 0` opts back out, reproducing that exact reachable
/// case: an operator who explicitly disabled (or predates) the adaptive
/// fraction still gets the "warn, never clamp" protection this test
/// verifies.
#[test]
fn headroom_consuming_a_large_fraction_of_context_warns_without_exceeding_it() {
    let dir = support::unique_temp_dir("headroom-large-fraction");
    let metadata_path = dir.join("models.json");
    std::fs::write(
        &metadata_path,
        r#"{"models":{"anthropic/claude-haiku-4-5":{"max_context_tokens":32768,"tool_calling":"streaming","reasoning":false,"reliability_tier":"verified"}}}"#,
    )
    .unwrap();

    let config_value = serde_json::json!({
        "default_role": "coder",
        "roles": {
            "coder": { "chain": ["anthropic/claude-haiku-4-5"] },
        },
        "routing": { "default_headroom_tokens": 8192, "headroom_fraction": 0 },
        "backends": { "anthropic": { "kind": "anthropic" } },
        "models": { "metadata_path": metadata_path.to_string_lossy() },
    });
    let path = dir.join("settings.json");
    std::fs::write(&path, serde_json::to_vec(&config_value).unwrap()).unwrap();

    let outcome = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(path),
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .unwrap();

    assert_eq!(outcome.warnings.len(), 1);
    let warning = &outcome.warnings[0];
    assert_eq!(
        warning.code,
        WarningCode::HeadroomConsumesLargeFractionOfContext
    );
    assert!(warning.message.contains("coder"));
    assert!(warning.message.contains("8192"));
    assert!(warning.message.contains("32768"));
    assert!(warning.message.contains("25%"));
    assert!(warning.message.contains("anthropic/claude-haiku-4-5"));

    // Not clamped -- same guarantee the literal-exceeds warning makes.
    assert_eq!(outcome.config.headroom_for(&RoleAlias::new("coder")), 8_192);
}

/// Finishing pass on board item `01M1AVZPTRSWVE33G4DTJY7Q1B` (move 2, the
/// operator's 2026-09-01 ruling): `headroom_fraction` landed OPT-IN in a
/// prior commit (`RoutingSection::default().headroom_fraction == None`);
/// this item's job was to make it the actual default. The identical
/// scenario the test immediately above disables (`headroom_fraction: 0`) to
/// stay reachable now happens on its own with NO `[routing]` section at
/// all: a role whose chain resolves a 32768-token window gets
/// `max(32768/10, HEADROOM_FLOOR) = 3276` tokens of headroom, not the flat
/// `8192` `DEFAULT_HEADROOM_TOKENS` used to hand back unconditionally.
/// `3276/32768` is ~10%, under the 25%
/// `HeadroomConsumesLargeFractionOfContext` threshold, so this ALSO proves
/// the adaptive default structurally avoids the exact problem the item's
/// own walked scenario hit (a flat `8192` against a 32768-token window
/// sitting at a dangerous 25%) rather than merely warning about it.
#[test]
fn default_headroom_fraction_computes_headroom_adaptively_with_no_config() {
    let dir = support::unique_temp_dir("headroom-adaptive-default");
    let metadata_path = dir.join("models.json");
    std::fs::write(
        &metadata_path,
        r#"{"models":{"anthropic/claude-haiku-4-5":{"max_context_tokens":32768,"tool_calling":"streaming","reasoning":false,"reliability_tier":"verified"}}}"#,
    )
    .unwrap();

    let config_value = serde_json::json!({
        "default_role": "coder",
        "roles": {
            "coder": { "chain": ["anthropic/claude-haiku-4-5"] },
        },
        "backends": { "anthropic": { "kind": "anthropic" } },
        "models": { "metadata_path": metadata_path.to_string_lossy() },
    });
    let path = dir.join("settings.json");
    std::fs::write(&path, serde_json::to_vec(&config_value).unwrap()).unwrap();

    let outcome = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(path),
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .unwrap();

    assert!(
        outcome.warnings.is_empty(),
        "the adaptive default should not need the large-fraction warning at all: {:?}",
        outcome.warnings
    );
    // Board item `01M2TVEWVMPP69TZ17XSGWEW82` moved WHERE this value is
    // asked for, not what it is. It used to be written back into
    // `roles.coder.headroom_tokens` by `merge::load_impl` and read out of
    // `headroom_for`; that write-back is gone, because parking a derived
    // number in a field only an operator is supposed to write made level 2
    // and level 3 of the precedence ladder indistinguishable downstream.
    assert_eq!(
        outcome.config.headroom_for_model(
            &RoleAlias::new("coder"),
            "anthropic/claude-haiku-4-5",
            Some(32_768)
        ),
        3_276
    );

    // The operator's own document is not rewritten.
    assert_eq!(outcome.config.roles["coder"].headroom_tokens, None);
    // And the ROLE-wide question -- asked with no candidate in hand --
    // answers with the flat default rather than a number that is only true
    // about one particular model.
    assert_eq!(
        outcome.config.headroom_for(&RoleAlias::new("coder")),
        DEFAULT_HEADROOM_TOKENS
    );
}

/// Board item `01M2TVEWVMPP69TZ17XSGWEW82`, acceptance 2: the whole
/// precedence ladder, pinned in one place, in order --
/// operator-per-model > operator-per-role > conway-derived-per-candidate >
/// `routing.default_headroom_tokens`.
///
/// Fails against HEAD twice over: `[routing].models` does not exist there
/// (this document would not deserialize at all), and the derived level was
/// computed once per ROLE from the smallest window that role's chain could
/// reach in metadata -- so `local/big` below would have been handed
/// `local/small`'s number, which is the defect itself.
#[test]
fn headroom_precedence_is_model_then_role_then_derived_then_global_default() {
    let dir = support::unique_temp_dir("headroom-precedence-ladder");
    let metadata_path = dir.join("models.json");
    std::fs::write(
        &metadata_path,
        r#"{"models":{
            "local/small":{"max_context_tokens":32768,"tool_calling":"streaming","reasoning":false,"reliability_tier":"verified"},
            "local/big":{"max_context_tokens":1000000,"tool_calling":"streaming","reasoning":false,"reliability_tier":"verified"},
            "local/pinned":{"max_context_tokens":200000,"tool_calling":"streaming","reasoning":false,"reliability_tier":"verified"}
        }}"#,
    )
    .unwrap();

    let config_value = serde_json::json!({
        "default_role": "coder",
        "roles": {
            // No per-role override: levels 1, 3 and 4 are reachable here.
            "coder": { "chain": ["local/small", "local/big", "local/pinned"] },
            // A per-role override: level 2 must beat level 3 for every
            // candidate of this role, however different their windows.
            "fixed": { "chain": ["local/small", "local/big"], "headroom_tokens": 4_000 },
        },
        "routing": {
            "default_headroom_tokens": 9_999,
            "headroom_fraction": 10,
            // Level 1: the most specific operator statement there is.
            "models": { "local/pinned": { "headroom_tokens": 1_234 } },
        },
        "backends": { "local": { "kind": "openai-compat" } },
        "models": { "metadata_path": metadata_path.to_string_lossy() },
    });
    let path = dir.join("settings.json");
    std::fs::write(&path, serde_json::to_vec(&config_value).unwrap()).unwrap();

    let cfg = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(path),
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .unwrap()
    .config;

    let coder = RoleAlias::new("coder");
    let fixed = RoleAlias::new("fixed");

    // 1. Operator per-model beats everything below it -- including the
    //    20000 this candidate's own 200000-token window would derive.
    assert_eq!(
        cfg.headroom_for_model(&coder, "local/pinned", Some(200_000)),
        1_234
    );
    //    ... and a per-role override too.
    assert_eq!(
        cfg.headroom_for_model(&fixed, "local/pinned", Some(200_000)),
        1_234
    );

    // 2. Operator per-role beats the derived per-candidate value, for
    //    every candidate of that role regardless of window size -- the
    //    "a conway-derived value never beats an operator-written one" half
    //    of the ruling.
    assert_eq!(
        cfg.headroom_for_model(&fixed, "local/small", Some(32_768)),
        4_000
    );
    assert_eq!(
        cfg.headroom_for_model(&fixed, "local/big", Some(1_000_000)),
        4_000
    );

    // 3. Derived per-candidate beats the global default, and is derived
    //    from THIS candidate's window. Two candidates of the SAME role get
    //    two different numbers -- the whole point of the item.
    assert_eq!(
        cfg.headroom_for_model(&coder, "local/small", Some(32_768)),
        3_276
    );
    assert_eq!(
        cfg.headroom_for_model(&coder, "local/big", Some(1_000_000)),
        100_000
    );

    // 4. The global default is the floor of the ladder: reached only when
    //    no operator value applies and no window is known to derive from.
    assert_eq!(cfg.headroom_for_model(&coder, "local/small", None), 9_999);
}

/// Board item `01M2TVEWVMPP69TZ17XSGWEW82`, acceptance 3, and the exact
/// configuration that motivated the item: a chain entry absent from
/// `models.json` was invisible to BOTH the derivation and the warning, so
/// an operator whose small model had been made unreachable was told
/// nothing at all.
///
/// Fails against HEAD: `validate`'s headroom check only ever looked at
/// chain entries it found in metadata, so `local/unknown-small` below
/// produced no warning of any kind.
#[test]
fn a_chain_entry_missing_from_model_metadata_is_reported_by_name() {
    let dir = support::unique_temp_dir("headroom-missing-metadata");
    let metadata_path = dir.join("models.json");
    // Only the 1M model is described. The small one is exactly the
    // `[floor (assumed)]` case from the incident.
    std::fs::write(
        &metadata_path,
        r#"{"models":{"cloud/glm":{"max_context_tokens":1000000,"tool_calling":"streaming","reasoning":false,"reliability_tier":"verified"}}}"#,
    )
    .unwrap();

    let config_value = serde_json::json!({
        "default_role": "coder",
        "roles": {
            "coder": { "chain": ["local/unknown-small", "cloud/glm"] },
        },
        "backends": { "local": { "kind": "openai-compat" }, "cloud": { "kind": "openai-compat" } },
        "models": { "metadata_path": metadata_path.to_string_lossy() },
    });
    let path = dir.join("settings.json");
    std::fs::write(&path, serde_json::to_vec(&config_value).unwrap()).unwrap();

    let outcome = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(path),
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .unwrap();

    let warning = outcome
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::ChainEntryContextWindowUnknown)
        .expect("a chain entry absent from models.json must be reported, not skipped");
    assert!(
        warning.message.contains("local/unknown-small"),
        "the warning must name the offending entry: {}",
        warning.message
    );
    assert!(
        warning.message.contains("coder"),
        "the warning must name the role: {}",
        warning.message
    );
    assert!(
        warning.message.contains("routing.models"),
        "the warning must point at the knob that fixes it: {}",
        warning.message
    );

    // BREAK-THE-GUARD: the KNOWN sibling in the same chain earns no such
    // warning, so this check is not simply firing on every entry.
    assert_eq!(
        outcome
            .warnings
            .iter()
            .filter(|w| w.code == WarningCode::ChainEntryContextWindowUnknown)
            .count(),
        1
    );
}

/// The per-model level is operator policy, so a zero is as wrong there as
/// at the role and global levels -- and is rejected the same way, naming
/// the key.
#[test]
fn zero_per_model_headroom_is_a_hard_error_naming_the_model() {
    let dir = support::unique_temp_dir("headroom-zero-model");
    let path = dir.join("settings.json");
    std::fs::write(
        &path,
        r#"{"default_role":"coder","roles":{"coder":{"chain":[]}},"routing":{"models":{"local/x":{"headroom_tokens":0}}}}"#,
    )
    .unwrap();

    let result = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(path),
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    });
    let err = result.unwrap_err().to_string();
    assert!(err.contains("local/x"), "{err}");
    assert!(err.contains("must be greater than 0"), "{err}");
}

/// The per-role explicit override still wins over the adaptive default --
/// same precedence `headroom_tokens` vs. `headroom_fraction` has always
/// documented, re-pinned now that the fraction applies without any
/// `[routing]` section naming it.
#[test]
fn explicit_per_role_headroom_still_wins_over_the_adaptive_default() {
    let dir = support::unique_temp_dir("headroom-explicit-beats-adaptive-default");
    let metadata_path = dir.join("models.json");
    std::fs::write(
        &metadata_path,
        r#"{"models":{"anthropic/claude-haiku-4-5":{"max_context_tokens":32768,"tool_calling":"streaming","reasoning":false,"reliability_tier":"verified"}}}"#,
    )
    .unwrap();

    let config_value = serde_json::json!({
        "default_role": "coder",
        "roles": {
            "coder": { "chain": ["anthropic/claude-haiku-4-5"], "headroom_tokens": 5_000 },
        },
        "backends": { "anthropic": { "kind": "anthropic" } },
        "models": { "metadata_path": metadata_path.to_string_lossy() },
    });
    let path = dir.join("settings.json");
    std::fs::write(&path, serde_json::to_vec(&config_value).unwrap()).unwrap();

    let outcome = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(path),
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .unwrap();

    assert_eq!(outcome.config.headroom_for(&RoleAlias::new("coder")), 5_000);
}

/// No model metadata reachable for a role's chain at all -- the fixed
/// `DEFAULT_HEADROOM_TOKENS` is the fallback, exactly as the item's ruling
/// states ("the fixed default is the fallback only when no model window is
/// known"), even though the adaptive fraction is on by default.
#[test]
fn adaptive_default_falls_back_to_the_fixed_constant_with_no_model_metadata() {
    let dir = support::unique_temp_dir("headroom-adaptive-default-no-metadata");
    let outcome = load(LoadOptions {
        cwd: dir,
        explicit_path: None,
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .unwrap();

    assert_eq!(
        outcome.config.headroom_for(&RoleAlias::new("anything")),
        DEFAULT_HEADROOM_TOKENS
    );
}

/// Negative control (a check is not established until it has been shown to
/// fail elsewhere, and shown NOT to over-fire here): the same built-in
/// default against a realistically large window (200000) is a low
/// single-digit percentage, well under the 25% threshold, and warns not at
/// all.
#[test]
fn headroom_well_under_the_fraction_threshold_does_not_warn() {
    let dir = support::unique_temp_dir("headroom-below-fraction");
    let metadata_path = dir.join("models.json");
    std::fs::write(
        &metadata_path,
        r#"{"models":{"anthropic/claude-haiku-4-5":{"max_context_tokens":200000,"tool_calling":"streaming","reasoning":false,"reliability_tier":"verified"}}}"#,
    )
    .unwrap();

    let config_value = serde_json::json!({
        "default_role": "coder",
        "roles": {
            "coder": { "chain": ["anthropic/claude-haiku-4-5"] },
        },
        "routing": { "default_headroom_tokens": 8192 },
        "backends": { "anthropic": { "kind": "anthropic" } },
        "models": { "metadata_path": metadata_path.to_string_lossy() },
    });
    let path = dir.join("settings.json");
    std::fs::write(&path, serde_json::to_vec(&config_value).unwrap()).unwrap();

    let outcome = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(path),
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .unwrap();

    assert!(outcome.warnings.is_empty());
}

#[test]
fn no_headroom_warning_when_model_metadata_is_absent() {
    let dir = support::unique_temp_dir("headroom-no-metadata");
    let path = dir.join("settings.json");
    std::fs::write(
        &path,
        r#"{"default_role":"coder","roles":{"coder":{"chain":["anthropic/claude-haiku-4-5"],"headroom_tokens":200000}},"backends":{"anthropic":{"kind":"anthropic"}}}"#,
    )
    .unwrap();

    let outcome = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(path),
        env: support::isolated_env(),
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

    let config_value = serde_json::json!({
        "default_role": "alpha",
        "roles": {
            "alpha": { "chain": ["anthropic/claude-haiku-4-5"], "headroom_tokens": 100000 },
            "zeta": { "chain": ["anthropic/claude-haiku-4-5"], "headroom_tokens": 100000 },
        },
        "backends": { "anthropic": { "kind": "anthropic" } },
        "models": { "metadata_path": metadata_path.to_string_lossy() },
    });
    let path = dir.join("settings.json");
    std::fs::write(&path, serde_json::to_vec(&config_value).unwrap()).unwrap();

    let run = || {
        load(LoadOptions {
            cwd: dir.clone(),
            explicit_path: Some(path.clone()),
            env: support::isolated_env(),
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
