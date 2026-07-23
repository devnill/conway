//! WI-097: mandatory Anthropic subscription OAuth-token (`sk-ant-oat*`)
//! rejection, regardless of backend kind and of which source produced the
//! value.

#[path = "support/mod.rs"]
mod support;

use std::collections::HashMap;

use conway::config::merge::validate;
use conway::config::model_metadata::ModelMetadata;
use conway::config::schema::{BackendEntry, BackendKind, ConwayConfig};
use conway::config::{load, CliOverrides, LoadOptions};

const REQUIRED_SUBSTRINGS: [&str; 4] = [
    "sk-ant-oat",
    "Anthropic subscription OAuth",
    "Terms of Service",
    "not supported",
];

fn assert_oauth_message(message: &str) {
    for needle in REQUIRED_SUBSTRINGS {
        assert!(
            message.contains(needle),
            "expected {needle:?} in OAuth rejection message: {message:?}"
        );
    }
}

fn minimal_valid_config() -> ConwayConfig {
    serde_json::from_str(
        r#"{"default_role":"coder","roles":{"coder":{"chain":[]}},"backends":{"anthropic":{"kind":"anthropic"}}}"#,
    )
    .unwrap()
}

#[test]
fn oauth_token_supplied_via_file_is_rejected() {
    let dir = support::unique_temp_dir("oauth-file");
    let result = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(support::fixtures_dir().join("oauth_token.json")),
        env: HashMap::new(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    });
    let err = result.expect_err("an sk-ant-oat* api_key in a file must be rejected");
    assert_oauth_message(&err.to_string());
}

#[test]
fn oauth_token_supplied_via_env_is_rejected() {
    let dir = support::unique_temp_dir("oauth-env");
    let mut env = HashMap::new();
    env.insert(
        "CONWAY_BACKENDS__ANTHROPIC__API_KEY".to_string(),
        "sk-ant-oat01-fromenv".to_string(),
    );
    let result = load(LoadOptions {
        cwd: dir,
        explicit_path: None,
        env,
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    });
    let err = result.expect_err("an sk-ant-oat* api_key from a CONWAY_* env var must be rejected");
    assert_oauth_message(&err.to_string());
}

/// Reconciliation disclosed in the WI-097 Self-Check: `CliOverrides`' field
/// list (fixed by the amendment's implementation notes: `default_role`,
/// `model`, `permission_mode`, `allowed_tools`, `denied_tools`,
/// `max_steps`, `session_root`, `cwd`, `headroom_tokens`) has no
/// per-backend `api_key` override, so `load` has no literal "CLI sets
/// `backends.<id>.api_key`" path to exercise. `merge::validate` is the
/// single, provenance-agnostic enforcement point every layer (file, env,
/// and — were a CLI field ever added — CLI) funnels through: it rejects
/// whatever the final merged `ConwayConfig` contains, independent of which
/// layer set it. This test proves that by constructing a `ConwayConfig` as
/// a CLI-applied override would produce one and validating it directly,
/// rather than inventing an undocumented `CliOverrides` field.
#[test]
fn oauth_token_present_in_the_final_config_is_rejected_independent_of_source() {
    let mut cfg = minimal_valid_config();
    cfg.backends.get_mut("anthropic").unwrap().api_key = "sk-ant-oat01-fromcli".to_string();

    let err = validate(&cfg, &ModelMetadata::empty(), &HashMap::new())
        .expect_err("an sk-ant-oat* api_key must be rejected regardless of provenance");
    assert_oauth_message(&err.to_string());
}

#[test]
fn oauth_rejection_applies_regardless_of_backend_kind() {
    let mut cfg = minimal_valid_config();
    cfg.backends.insert(
        "local".to_string(),
        BackendEntry {
            kind: BackendKind::OpenaiCompat,
            api_key: "sk-ant-oat01-anykind".to_string(),
            ..Default::default()
        },
    );

    let err = validate(&cfg, &ModelMetadata::empty(), &HashMap::new())
        .expect_err("OAuth rejection must not depend on backend kind");
    assert_oauth_message(&err.to_string());
}

#[test]
fn oauth_rejection_also_catches_the_api_key_env_indirection() {
    // api_key itself is empty, but api_key_env names a var whose (injected)
    // value is an OAuth token — validate must resolve the indirection.
    let mut cfg = minimal_valid_config();
    {
        let backend = cfg.backends.get_mut("anthropic").unwrap();
        backend.api_key = String::new();
        backend.api_key_env = "MY_ANTHROPIC_KEY".to_string();
    }
    let mut env = HashMap::new();
    env.insert(
        "MY_ANTHROPIC_KEY".to_string(),
        "sk-ant-oat01-viaindirection".to_string(),
    );

    let err = validate(&cfg, &ModelMetadata::empty(), &env)
        .expect_err("a resolved api_key_env value starting with sk-ant-oat must be rejected");
    assert_oauth_message(&err.to_string());
}

#[test]
fn a_normal_api_key_is_accepted() {
    let mut cfg = minimal_valid_config();
    cfg.backends.get_mut("anthropic").unwrap().api_key = "sk-ant-api03-perfectlyfine".to_string();
    assert!(validate(&cfg, &ModelMetadata::empty(), &HashMap::new()).is_ok());
}
